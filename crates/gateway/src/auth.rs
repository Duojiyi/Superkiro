//! Authentication middleware and JWT validation for Kiro BYOK.
//!
//! Spec §4.1, P0-4 (DECISIONS.md #5).
//! Features:
//! - Validates Bearer JWT with HMAC-SHA256 signature.
//! - Extracts caller identity: `card_id`, `group_id`, `token_version`.
//! - Instant token revocation via `token_version` comparison against in-memory/cache store.
//! - Rejects missing, malformed, expired, invalid signature, or revoked tokens with structured AWS JSON 401.

use crate::facade::error_response;
use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::Response,
};
use billing::card::CardStatus;
use billing::engine::BillingEngine;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// Error types for authentication failures.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("Missing Authorization header or Bearer prefix")]
    MissingToken,

    #[error("Malformed Authorization token format")]
    MalformedToken,

    #[error("Invalid token signature or payload: {0}")]
    InvalidSignature(String),

    #[error("Token has expired")]
    Expired,

    #[error("Card {0} not found")]
    CardNotFound(String),

    #[error("Card {0} is inactive or frozen")]
    CardInactive(String),

    #[error("Token version mismatch: token v{token_version} != card v{card_version} (revoked)")]
    TokenRevoked {
        token_version: u64,
        card_version: u64,
    },

    #[error("Token tenant does not match the live card tenant")]
    TenantMismatch,
}

impl AuthError {
    /// Convert internal auth error to HTTP status code and AWS exception type.
    pub fn to_aws_error(&self) -> (StatusCode, &'static str, String) {
        match self {
            Self::MissingToken => (
                StatusCode::UNAUTHORIZED,
                "MissingAuthenticationTokenException",
                "Missing or malformed Authorization header".to_string(),
            ),
            Self::MalformedToken => (
                StatusCode::UNAUTHORIZED,
                "UnrecognizedClientException",
                "Invalid token format".to_string(),
            ),
            Self::InvalidSignature(_) => (
                StatusCode::UNAUTHORIZED,
                "UnrecognizedClientException",
                "Invalid authentication credentials".to_string(),
            ),
            Self::Expired => (
                StatusCode::UNAUTHORIZED,
                "ExpiredTokenException",
                "The security token included in the request is expired".to_string(),
            ),
            Self::CardNotFound(_)
            | Self::CardInactive(_)
            | Self::TokenRevoked { .. }
            | Self::TenantMismatch => (
                StatusCode::UNAUTHORIZED,
                "AccessDeniedException",
                "Invalid authentication credentials".to_string(),
            ),
        }
    }
}

/// JWT claims structure matching Spec §4.1.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthClaims {
    pub card_id: String,
    pub group_id: String,
    pub token_version: u64,
    pub exp: u64,
    pub iat: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AccessClaims {
    card_id: String,
    group_id: String,
    token_version: u64,
    exp: u64,
    iat: u64,
    #[serde(default = "default_access_kind")]
    kind: String,
    #[serde(default)]
    iss: String,
    #[serde(default)]
    aud: String,
}

fn default_access_kind() -> String {
    "access".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RefreshClaims {
    card_id: String,
    group_id: String,
    token_version: u64,
    #[serde(default = "default_refresh_version")]
    refresh_version: u64,
    exp: u64,
    iat: u64,
    jti: String,
    kind: String,
    #[serde(default)]
    iss: String,
    #[serde(default)]
    aud: String,
}

fn default_refresh_version() -> u64 {
    1
}

/// In-memory card record for authentication and version checking.
#[derive(Debug, Clone)]
pub struct CardRecord {
    pub card_id: String,
    pub group_id: String,
    pub current_token_version: u64,
    pub is_active: bool,
}

/// Shared authentication state.
#[derive(Clone)]
pub struct AuthState {
    pub secret: String,
    cards: Arc<RwLock<HashMap<String, CardRecord>>>,
    billing: Option<BillingEngine>,
    used_refresh_tokens: Arc<RwLock<HashMap<String, u64>>>,
    refresh_versions: Arc<RwLock<HashMap<String, u64>>>,
    refresh_nonce: Arc<AtomicU64>,
    /// `new()` is retained for legacy/unit-test stores. Production state is
    /// connected through `with_billing()` and requires issuer/audience claims.
    require_claim_context: bool,
}

impl AuthState {
    /// Create new AuthState with the specified signing secret.
    pub fn new(secret: &str) -> Self {
        Self {
            secret: secret.to_string(),
            cards: Arc::new(RwLock::new(HashMap::new())),
            billing: None,
            used_refresh_tokens: Arc::new(RwLock::new(HashMap::new())),
            refresh_versions: Arc::new(RwLock::new(HashMap::new())),
            refresh_nonce: Arc::new(AtomicU64::new(1)),
            require_claim_context: false,
        }
    }

    /// Create new AuthState connected directly to the central BillingEngine.
    pub fn with_billing(secret: &str, billing: BillingEngine) -> Self {
        Self {
            secret: secret.to_string(),
            cards: Arc::new(RwLock::new(HashMap::new())),
            billing: Some(billing),
            used_refresh_tokens: Arc::new(RwLock::new(HashMap::new())),
            refresh_versions: Arc::new(RwLock::new(HashMap::new())),
            refresh_nonce: Arc::new(AtomicU64::new(1)),
            require_claim_context: true,
        }
    }

    /// Create default state with a pre-configured developer card key.
    pub fn with_default_dev_card() -> Self {
        let state = Self::new("kiro-byok-dev-jwt-secret-key-32bytes!!");
        state.upsert_card(CardRecord {
            card_id: "card-dev-001".to_string(),
            group_id: "group-pro-plus".to_string(),
            current_token_version: 1,
            is_active: true,
        });
        state
    }

    /// Upsert or update a card record (in-memory mock store).
    pub fn upsert_card(&self, card: CardRecord) {
        let mut w = self.cards.write().unwrap();
        self.refresh_versions
            .write()
            .unwrap()
            .entry(card.card_id.clone())
            .or_insert(1);
        w.insert(card.card_id.clone(), card);
    }

    /// Revoke all active tokens for a card by incrementing its `token_version`.
    pub fn revoke_card(&self, card_id: &str) -> Option<u64> {
        if let Some(ref b) = self.billing {
            b.revoke_tokens(card_id).ok()
        } else {
            let mut w = self.cards.write().unwrap();
            if let Some(card) = w.get_mut(card_id) {
                card.current_token_version += 1;
                Some(card.current_token_version)
            } else {
                None
            }
        }
    }

    /// Freeze or deactivate a card.
    pub fn set_card_active(&self, card_id: &str, active: bool) -> bool {
        if let Some(ref b) = self.billing {
            if active {
                b.unfreeze_card(card_id).is_ok()
            } else {
                b.freeze_card(card_id, "admin toggle").is_ok()
            }
        } else {
            let mut w = self.cards.write().unwrap();
            if let Some(card) = w.get_mut(card_id) {
                card.is_active = active;
                true
            } else {
                false
            }
        }
    }

    /// Issue a new Bearer JWT for a given card based on its current live state.
    pub fn issue_token_for_card(&self, card_id: &str, ttl_secs: u64) -> Result<String, AuthError> {
        if let Some(ref b) = self.billing {
            let card = b
                .get_card(card_id)
                .ok_or_else(|| AuthError::CardNotFound(card_id.to_string()))?;
            card.check_device_policy()
                .map_err(|e| AuthError::CardInactive(e.to_string()))?;
            if card.status != CardStatus::Active {
                return Err(AuthError::CardInactive(format!(
                    "Card {} status is {:?}",
                    card_id, card.status
                )));
            }
            self.issue_token(card_id, &card.group_id, card.token_version, ttl_secs)
        } else {
            let r = self.cards.read().unwrap();
            let card = r
                .get(card_id)
                .ok_or_else(|| AuthError::CardNotFound(card_id.to_string()))?;
            if !card.is_active {
                return Err(AuthError::CardInactive(card_id.to_string()));
            }
            self.issue_token(
                card_id,
                &card.group_id,
                card.current_token_version,
                ttl_secs,
            )
        }
    }

    /// Issue a new Bearer JWT for a given card with explicit parameters.
    pub fn issue_token(
        &self,
        card_id: &str,
        group_id: &str,
        token_version: u64,
        ttl_secs: u64,
    ) -> Result<String, AuthError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let claims = AccessClaims {
            card_id: card_id.to_string(),
            group_id: group_id.to_string(),
            token_version,
            exp: now.saturating_add(ttl_secs),
            iat: now,
            kind: "access".to_string(),
            iss: "kiro-byok".to_string(),
            aud: "kiro-gateway".to_string(),
        };

        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(self.secret.as_bytes()),
        )
        .map_err(|e| AuthError::InvalidSignature(e.to_string()))
    }

    /// Issue a refresh-only token. It is deliberately a different signed schema
    /// from access JWTs so an access token cannot be replayed at `/refreshToken`.
    pub fn issue_refresh_token(
        &self,
        card_id: &str,
        group_id: &str,
        token_version: u64,
        refresh_version: u64,
        ttl_secs: u64,
    ) -> Result<String, AuthError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| AuthError::InvalidSignature(e.to_string()))?
            .as_secs();
        let claims = RefreshClaims {
            card_id: card_id.to_string(),
            group_id: group_id.to_string(),
            token_version,
            refresh_version,
            exp: now.saturating_add(ttl_secs),
            iat: now,
            jti: format!(
                "rt-{}-{}",
                now,
                self.refresh_nonce.fetch_add(1, Ordering::Relaxed)
            ),
            kind: "refresh".to_string(),
            iss: "kiro-byok".to_string(),
            aud: "kiro-gateway".to_string(),
        };
        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(self.secret.as_bytes()),
        )
        .map_err(|e| AuthError::InvalidSignature(e.to_string()))
    }

    /// Validate and atomically consume a refresh token, returning a rotated pair.
    pub fn rotate_refresh_token(
        &self,
        raw_token: &str,
        access_ttl_secs: u64,
        refresh_ttl_secs: u64,
    ) -> Result<(AuthClaims, String, String), AuthError> {
        let mut validation = Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.validate_exp = true;
        validation.validate_aud = self.require_claim_context;
        if self.require_claim_context {
            validation.set_issuer(&["kiro-byok"]);
            validation.set_audience(&["kiro-gateway"]);
        }
        let token_data = decode::<RefreshClaims>(
            raw_token,
            &DecodingKey::from_secret(self.secret.as_bytes()),
            &validation,
        )
        .map_err(|e| match e.kind() {
            jsonwebtoken::errors::ErrorKind::ExpiredSignature => AuthError::Expired,
            _ => AuthError::InvalidSignature(e.to_string()),
        })?;
        let refresh = token_data.claims;
        if refresh.kind != "refresh" {
            return Err(AuthError::MalformedToken);
        }
        let claims = self.verify_token_for_card(&refresh.card_id, refresh.token_version)?;
        if claims.group_id != refresh.group_id {
            return Err(AuthError::InvalidSignature(
                "refresh token tenant mismatch".to_string(),
            ));
        }
        // Refresh tokens are rotated per token JTI below. Do not compare or
        // increment a card-wide version here: that would invalidate another
        // device's still-valid refresh token. token_version remains the card-wide
        // emergency revocation mechanism.
        {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let mut used = self
                .used_refresh_tokens
                .write()
                .map_err(|_| AuthError::InvalidSignature("refresh store poisoned".to_string()))?;
            used.retain(|_, exp| *exp > now);
            if used.contains_key(&refresh.jti) {
                return Err(AuthError::InvalidSignature(
                    "refresh token already used".to_string(),
                ));
            }
            used.insert(refresh.jti.clone(), refresh.exp);
        }
        let new_claims = claims;
        let new_refresh_version = refresh.refresh_version;
        let access = self.issue_token(
            &new_claims.card_id,
            &new_claims.group_id,
            new_claims.token_version,
            access_ttl_secs,
        )?;
        let rotated = self.issue_refresh_token(
            &new_claims.card_id,
            &new_claims.group_id,
            new_claims.token_version,
            new_refresh_version,
            refresh_ttl_secs,
        )?;
        Ok((new_claims, access, rotated))
    }

    fn verify_token_for_card(
        &self,
        card_id: &str,
        token_version: u64,
    ) -> Result<AuthClaims, AuthError> {
        let card = if let Some(ref billing) = self.billing {
            billing
                .get_card(card_id)
                .ok_or_else(|| AuthError::CardNotFound(card_id.to_string()))?
        } else {
            let cards = self.cards.read().unwrap();
            let record = cards
                .get(card_id)
                .ok_or_else(|| AuthError::CardNotFound(card_id.to_string()))?;
            if !record.is_active {
                return Err(AuthError::CardInactive(card_id.to_string()));
            }
            if record.current_token_version != token_version {
                return Err(AuthError::TokenRevoked {
                    token_version,
                    card_version: record.current_token_version,
                });
            }
            return Ok(AuthClaims {
                card_id: card_id.to_string(),
                group_id: record.group_id.clone(),
                token_version,
                exp: u64::MAX,
                iat: 0,
            });
        };
        card.check_device_policy()
            .map_err(|e| AuthError::CardInactive(e.to_string()))?;
        if card.status != CardStatus::Active {
            return Err(AuthError::CardInactive(card_id.to_string()));
        }
        if let Some(valid_until) = card.valid_until {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if now >= valid_until {
                return Err(AuthError::CardInactive(card_id.to_string()));
            }
        }
        if card.token_version != token_version {
            return Err(AuthError::TokenRevoked {
                token_version,
                card_version: card.token_version,
            });
        }
        Ok(AuthClaims {
            card_id: card.id,
            group_id: card.group_id,
            token_version,
            exp: u64::MAX,
            iat: 0,
        })
    }

    /// Verify a raw Bearer token string.
    pub fn verify_token(&self, token_str: &str) -> Result<AuthClaims, AuthError> {
        let mut validation = Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.validate_exp = true;
        validation.validate_aud = self.require_claim_context;
        if self.require_claim_context {
            validation.set_issuer(&["kiro-byok"]);
            validation.set_audience(&["kiro-gateway"]);
        }

        let token_data = decode::<AccessClaims>(
            token_str,
            &DecodingKey::from_secret(self.secret.as_bytes()),
            &validation,
        )
        .map_err(|e| match e.kind() {
            jsonwebtoken::errors::ErrorKind::ExpiredSignature => AuthError::Expired,
            _ => AuthError::InvalidSignature(e.to_string()),
        })?;

        let signed_claims = token_data.claims;
        if signed_claims.kind != "access" {
            return Err(AuthError::MalformedToken);
        }
        let claims = AuthClaims {
            card_id: signed_claims.card_id,
            group_id: signed_claims.group_id,
            token_version: signed_claims.token_version,
            exp: signed_claims.exp,
            iat: signed_claims.iat,
        };

        // Verify token_version against current card state (Instant revocation check)
        if let Some(ref b) = self.billing {
            let card = b
                .get_card(&claims.card_id)
                .ok_or_else(|| AuthError::CardNotFound(claims.card_id.clone()))?;

            card.check_device_policy()
                .map_err(|e| AuthError::CardInactive(e.to_string()))?;
            // Status check
            match card.status {
                CardStatus::Frozen | CardStatus::Banned | CardStatus::Voided => {
                    return Err(AuthError::CardInactive(format!(
                        "Card {} is {:?}",
                        claims.card_id, card.status
                    )));
                }
                CardStatus::Expired => {
                    return Err(AuthError::CardInactive(format!(
                        "Card {} is expired",
                        claims.card_id
                    )));
                }
                CardStatus::Unactivated => {
                    return Err(AuthError::CardInactive(format!(
                        "Card {} is unactivated",
                        claims.card_id
                    )));
                }
                CardStatus::Active => {
                    if let Some(valid_until) = card.valid_until {
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_secs();
                        if now >= valid_until {
                            return Err(AuthError::CardInactive(format!(
                                "Card {} is expired",
                                claims.card_id
                            )));
                        }
                    }
                }
            }

            // Token version check (Instant Revocation - Spec §4.1, §7)
            if claims.token_version != card.token_version {
                return Err(AuthError::TokenRevoked {
                    token_version: claims.token_version,
                    card_version: card.token_version,
                });
            }
            if claims.group_id != card.group_id {
                return Err(AuthError::TenantMismatch);
            }
        } else {
            let r = self.cards.read().unwrap();
            if let Some(card) = r.get(&claims.card_id) {
                if !card.is_active {
                    return Err(AuthError::CardInactive(claims.card_id));
                }
                if claims.token_version != card.current_token_version {
                    return Err(AuthError::TokenRevoked {
                        token_version: claims.token_version,
                        card_version: card.current_token_version,
                    });
                }
                if claims.group_id != card.group_id {
                    return Err(AuthError::TenantMismatch);
                }
            } else {
                return Err(AuthError::CardNotFound(claims.card_id));
            }
        }

        Ok(claims)
    }
}

/// Axum middleware for authenticating requests.
/// Validates Authorization: Bearer <token>, extracts `AuthClaims`,
/// and places `AuthClaims` into request extensions.
pub async fn auth_middleware(
    State(auth): State<AuthState>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok());

    let token = match auth_header {
        Some(h) if h.starts_with("Bearer ") => &h[7..],
        Some(_) => {
            let (status, err_type, msg) = AuthError::MalformedToken.to_aws_error();
            return error_response(status, err_type, &msg);
        }
        None => {
            let (status, err_type, msg) = AuthError::MissingToken.to_aws_error();
            return error_response(status, err_type, &msg);
        }
    };

    match auth.verify_token(token) {
        Ok(claims) => {
            req.extensions_mut().insert(claims);
            next.run(req).await
        }
        Err(err) => {
            let (status, err_type, msg) = err.to_aws_error();
            error_response(status, err_type, &msg)
        }
    }
}
