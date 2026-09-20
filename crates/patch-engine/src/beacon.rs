//! Version negotiation handshake and client health beacon report (Spec §10, §14.5).
//!
//! Features:
//! - Handshakes with gateway on startup: reports Kiro version, client version, OS, and patch status.
//! - Receives compatibility confirmation, minimum required version, and broadcast announcements.
//! - Sends lightweight periodic health beacons (`HealthBeacon`).

use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BeaconError {
    #[error("HTTP client initialization failed: {0}")]
    Initialization(String),

    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Server rejected negotiation: {status} - {message}")]
    Rejected { status: u16, message: String },

    #[allow(dead_code)] // ponytail: reserved for client forced upgrade
    #[error("Client version {0} is deprecated, please update to at least {1}")]
    UpgradeRequired(String, String),
}

/// Request sent by client to negotiate version compatibility with gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientNegotiateRequest {
    pub client_version: String,
    pub kiro_version: String,
    pub os: String,
    pub arch: String,
    pub patch_status: String,
}

use crate::patch::PatchRecipe;
use crate::preferences::UiOverrides;

/// Server response for version negotiation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientNegotiateResponse {
    pub supported: bool,
    pub server_version: String,
    pub min_client_version: String,
    pub recommended_patch_version: String,
    #[serde(default)]
    pub announcements: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch_recipe: Option<PatchRecipe>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_overrides: Option<UiOverrides>,
}

/// Lightweight periodic health beacon sent to gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthBeacon {
    pub client_version: String,
    pub device_id: String,
    pub is_kiro_running: bool,
    pub timestamp: u64,
}

/// Client for performing version negotiation and sending health beacons.
#[derive(Debug, Clone)]
pub struct BeaconClient {
    http: Result<Client, String>,
}

impl Default for BeaconClient {
    fn default() -> Self {
        Self {
            http: crate::http::client_builder().and_then(|builder| {
                builder
                    .timeout(Duration::from_secs(5))
                    .build()
                    .map_err(|e| e.to_string())
            }),
        }
    }
}

impl BeaconClient {
    pub fn new() -> Self {
        Self::default()
    }

    fn http(&self) -> Result<&Client, BeaconError> {
        self.http
            .as_ref()
            .map_err(|e| BeaconError::Initialization(e.clone()))
    }

    /// Perform version negotiation handshake with gateway.
    pub async fn negotiate(
        &self,
        gateway_url: &str,
        req: &ClientNegotiateRequest,
    ) -> Result<ClientNegotiateResponse, BeaconError> {
        let url = format!("{}/client/negotiate", gateway_url.trim_end_matches('/'));

        let resp = self.http()?.post(&url).json(req).send().await?;

        let status = resp.status();
        if !status.is_success() {
            let err_text = resp.text().await.unwrap_or_default();
            return Err(BeaconError::Rejected {
                status: status.as_u16(),
                message: err_text,
            });
        }

        let negotiate_resp: ClientNegotiateResponse = resp.json().await?;
        Ok(negotiate_resp)
    }

    /// Send periodic health beacon to gateway.
    pub async fn send_beacon(
        &self,
        gateway_url: &str,
        beacon: &HealthBeacon,
    ) -> Result<bool, BeaconError> {
        let url = format!("{}/client/beacon", gateway_url.trim_end_matches('/'));
        let resp = self.http()?.post(&url).json(beacon).send().await?;
        Ok(resp.status().is_success())
    }
}

#[cfg(test)]
mod initialization_tests {
    use super::*;

    #[tokio::test]
    async fn initialization_failure_is_returned_by_both_requests() {
        let client = BeaconClient {
            http: Err("invalid CA".into()),
        };
        let req = ClientNegotiateRequest {
            client_version: "test".into(),
            kiro_version: "test".into(),
            os: "test".into(),
            arch: "test".into(),
            patch_status: "test".into(),
        };
        assert!(matches!(
            client.negotiate("https://example.com", &req).await,
            Err(BeaconError::Initialization(_))
        ));
        let beacon = HealthBeacon {
            client_version: "test".into(),
            device_id: "test".into(),
            is_kiro_running: false,
            timestamp: 0,
        };
        assert!(matches!(
            client.send_beacon("https://example.com", &beacon).await,
            Err(BeaconError::Initialization(_))
        ));
    }
}
