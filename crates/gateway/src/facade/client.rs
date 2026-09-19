//! Client version negotiation and health beacon endpoints (Spec §10, §14.5).

use super::{json_response, BoxFuture, FacadeHandler, Response};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use serde::{Deserialize, Serialize};

use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PatchRecipe {
    pub recipe_id: String,
    pub marker: String,
    pub needle: String,
    pub replacement: String,
    #[serde(default)]
    pub extra_envs: HashMap<String, String>,
}

impl Default for PatchRecipe {
    fn default() -> Self {
        Self {
            recipe_id: "v1-standard".to_string(),
            marker: "/* @patched-kiro-byok v1 */".to_string(),
            needle: "https://runtime.${t}.kiro.dev".to_string(),
            replacement: "http://127.0.0.1:44040/runtime".to_string(),
            extra_envs: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UiOverrides {
    #[serde(default = "default_brand_name")]
    pub brand_name: String,
    #[serde(default)]
    pub brand_subtitle: Option<String>,
    #[serde(default = "default_credit_label")]
    pub credit_label: String,
    #[serde(default)]
    pub announcements: Vec<String>,
    #[serde(default)]
    pub model_rename_rules: HashMap<String, String>,
    #[serde(default)]
    pub hidden_models: Vec<String>,
}

fn default_brand_name() -> String {
    "KIRO 极速接管中心".to_string()
}

fn default_credit_label() -> String {
    "算力积分".to_string()
}

fn configured_brand_name() -> String {
    std::env::var("BRAND_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(default_brand_name)
}

fn configured_credit_label() -> String {
    std::env::var("CREDIT_LABEL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(default_credit_label)
}

impl Default for UiOverrides {
    fn default() -> Self {
        Self {
            brand_name: default_brand_name(),
            brand_subtitle: Some("专业企业级加速专线".to_string()),
            credit_label: default_credit_label(),
            announcements: vec![],
            model_rename_rules: HashMap::new(),
            hidden_models: vec![],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WhiteLabelConfig {
    pub app_name: String,
    pub app_slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_uri: Option<String>,
    pub primary_color: String,
    pub accent_color: String,
    pub theme_mode: String,
    pub official_website: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub support_contact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copyright: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_css: Option<String>,
}

impl Default for WhiteLabelConfig {
    fn default() -> Self {
        Self {
            app_name: "KIRO 极速接管中心".to_string(),
            app_slug: "kiro-byok".to_string(),
            subtitle: Some("专业企业级 AI 研发加速专线".to_string()),
            icon_uri: Some("asset://icons/brand-logo.svg".to_string()),
            primary_color: "#6366F1".to_string(),
            accent_color: "#4F46E5".to_string(),
            theme_mode: "auto".to_string(),
            official_website: "https://byok.kiro.dev".to_string(),
            docs_url: Some("https://byok.kiro.dev/docs".to_string()),
            support_contact: Some("support@kiro.dev".to_string()),
            copyright: Some("© 2026 Kiro BYOK Studio".to_string()),
            custom_css: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientNegotiateResponse {
    pub supported: bool,
    pub server_version: String,
    pub min_client_version: String,
    pub recommended_patch_version: String,
    pub announcements: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch_recipe: Option<PatchRecipe>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_overrides: Option<UiOverrides>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub white_label: Option<WhiteLabelConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BeaconResponse {
    pub status: String,
}

/// Handler for `POST /client/negotiate`
pub struct ClientNegotiateHandler;

impl FacadeHandler for ClientNegotiateHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/client/negotiate"
    }

    fn handle<'a>(&'a self, _req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let ui = UiOverrides {
                brand_name: configured_brand_name(),
                credit_label: configured_credit_label(),
                ..UiOverrides::default()
            };
            let white_label = WhiteLabelConfig {
                app_name: ui.brand_name.clone(),
                ..WhiteLabelConfig::default()
            };
            let resp = ClientNegotiateResponse {
                supported: true,
                server_version: env!("CARGO_PKG_VERSION").to_string(),
                min_client_version: "0.1.0".to_string(),
                recommended_patch_version: "v1".to_string(),
                announcements: vec![],
                patch_recipe: Some(PatchRecipe::default()),
                ui_overrides: Some(ui),
                white_label: Some(white_label),
            };
            json_response(StatusCode::OK, &resp)
        })
    }
}

/// Handler for `POST /client/beacon`
pub struct ClientBeaconHandler;

impl FacadeHandler for ClientBeaconHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/client/beacon"
    }

    fn handle<'a>(&'a self, _req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let resp = BeaconResponse {
                status: "ok".to_string(),
            };
            json_response(StatusCode::OK, &resp)
        })
    }
}

/// Handler for `GET /client/brand` (Spec §14.6, P4-8)
pub struct ClientBrandHandler {
    pub config: WhiteLabelConfig,
}

impl Default for ClientBrandHandler {
    fn default() -> Self {
        Self {
            config: WhiteLabelConfig {
                app_name: configured_brand_name(),
                ..WhiteLabelConfig::default()
            },
        }
    }
}

impl FacadeHandler for ClientBrandHandler {
    fn method(&self) -> Method {
        Method::GET
    }

    fn path(&self) -> &'static str {
        "/client/brand"
    }

    fn handle<'a>(&'a self, _req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let body = serde_json::json!({
                "status": "ok",
                "brand": self.config,
            });
            json_response(StatusCode::OK, &body)
        })
    }
}
