//! One-click provider configuration importer (Spec §14.3, P4-6).
//!
//! Supports importing provider settings and models from popular LLM clients:
//! - Cherry Studio (`cherry-studio-*.json` exports and provider arrays)
//! - CC Switch (Cursor / Claude / OpenAI provider configurations)
//! - Generic open format (with auto-detection)

use crate::facade::models::{ModelInfo, TokenLimits};
use billing::provider::{Provider, ProviderFormat, ProviderKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ImportError {
    #[error("Failed to parse configuration JSON: {0}")]
    Json(String),

    #[error("Unrecognized or unsupported provider configuration schema")]
    UnknownSchema,

    #[error("No valid providers found in configuration payload")]
    EmptyProviders,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFormat {
    #[default]
    Auto,
    CherryStudio,
    CcSwitch,
    Generic,
}

/// Normalized provider representation extracted from third-party client configurations.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedProvider {
    pub id: String,
    pub name: String,
    pub format: ProviderFormat,
    pub base_url: String,
    pub api_key: String,
    pub models: Vec<String>,
    pub enabled: bool,
}

impl std::fmt::Debug for ImportedProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportedProvider")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("format", &self.format)
            .field("base_url", &self.base_url)
            .field("api_key", &"[redacted]")
            .field("models", &self.models)
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl ImportedProvider {
    /// Convert imported provider into billing domain entities: `(Provider, ProviderKey)`.
    pub fn to_provider_and_key(&self) -> (Provider, ProviderKey) {
        let mut provider = Provider::new(&self.id, &self.name, self.format, &self.base_url);
        provider.enabled = self.enabled;
        let mut key = ProviderKey::new(format!("{}-key-1", self.id), &self.id, &self.api_key);
        key.allowed_models = Some(self.models.clone());
        (provider, key)
    }

    /// Generate default virtual ModelInfo entries for exposed models.
    pub fn to_model_infos(&self) -> Vec<ModelInfo> {
        self.models
            .iter()
            .map(|model_id| {
                let lower = model_id.to_lowercase();
                let supports_reasoning = lower.contains("reasoner")
                    || lower.contains("r1")
                    || lower.contains("o1")
                    || lower.contains("o3")
                    || lower.contains("thinking");
                let is_text_only = lower.contains("deepseek-v3")
                    || lower.contains("deepseek-chat")
                    || lower.contains("text-")
                    || lower.contains("deepseek-reasoner");
                let supports_vision = !is_text_only;

                ModelInfo {
                    model_id: model_id.clone(),
                    model_name: Some(model_id.clone()),
                    description: Some(format!("Imported from {}", self.name)),
                    token_limits: Some(TokenLimits {
                        max_input_tokens: 128_000,
                        max_output_tokens: 16_384,
                    }),
                    supports_reasoning,
                    supports_vision,
                    default_effort_level: if supports_reasoning {
                        Some("medium".to_string())
                    } else {
                        None
                    },
                }
            })
            .collect()
    }
}

/// Helper to sanitize string identifiers into clean slug IDs.
fn slugify_id(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() {
        "imported-provider".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Helper to extract string from json with multiple key aliases.
fn get_str_alias<'a>(val: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    for k in keys {
        if let Some(s) = val.get(*k).and_then(|v| v.as_str()) {
            if !s.trim().is_empty() {
                return Some(s.trim());
            }
        }
    }
    None
}

/// Parse Cherry Studio format.
/// Cherry Studio exports:
/// - `{ "version": 1, "providers": [ ... ] }`
/// - or directly `[ { "id": "...", "name": "...", "apiType": "...", "baseUrl": "...", "apiKey": "...", "models": [...] } ]`
pub fn parse_cherry_studio(val: &serde_json::Value) -> Result<Vec<ImportedProvider>, ImportError> {
    let provider_list = if let Some(arr) = val.as_array() {
        arr
    } else if let Some(arr) = val.get("providers").and_then(|p| p.as_array()) {
        arr
    } else if let Some(obj) = val.get("providers").and_then(|p| p.as_object()) {
        // sometimes object map
        return parse_provider_object_map(obj, true);
    } else {
        return Err(ImportError::UnknownSchema);
    };

    let mut result = Vec::new();
    for p in provider_list {
        let name =
            get_str_alias(p, &["name", "providerName", "title"]).unwrap_or("Cherry Provider");
        let id = get_str_alias(p, &["id", "provider_id", "providerId", "slug"])
            .map(|s| s.to_string())
            .unwrap_or_else(|| slugify_id(name));

        let base_url = match get_str_alias(p, &["baseUrl", "base_url", "apiUrl", "url"]) {
            Some(u) => u.trim_end_matches('/').to_string(),
            None => continue,
        };

        let api_key = match get_str_alias(p, &["apiKey", "api_key", "key", "token"]) {
            Some(k) => k.to_string(),
            None => continue,
        };

        let api_type = get_str_alias(p, &["apiType", "type", "format"]).unwrap_or("openai");
        let format = if api_type.eq_ignore_ascii_case("anthropic")
            || api_type.eq_ignore_ascii_case("claude")
        {
            ProviderFormat::Anthropic
        } else {
            ProviderFormat::OpenAi
        };

        let enabled = p.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);

        // Extract models
        let mut models = Vec::new();
        if let Some(model_arr) = p.get("models").and_then(|m| m.as_array()) {
            for m in model_arr {
                if let Some(mid) = m.as_str() {
                    models.push(mid.to_string());
                } else if let Some(mid) = m.get("id").and_then(|v| v.as_str()) {
                    models.push(mid.to_string());
                } else if let Some(mname) = m.get("name").and_then(|v| v.as_str()) {
                    models.push(mname.to_string());
                }
            }
        }

        result.push(ImportedProvider {
            id,
            name: name.to_string(),
            format,
            base_url,
            api_key,
            models,
            enabled,
        });
    }

    if result.is_empty() {
        Err(ImportError::EmptyProviders)
    } else {
        Ok(result)
    }
}

/// Parse CC Switch format.
/// CC Switch exports:
/// - `{ "providers": [ { "name": "...", "api_type": "...", "base_url": "...", "api_key": "...", "models": [...] } ] }`
/// - `{ "providers": { "deepseek": { ... } } }`
/// - or direct array `[ { "name": "...", "url": "...", "key": "...", "format": "..." } ]`
pub fn parse_cc_switch(val: &serde_json::Value) -> Result<Vec<ImportedProvider>, ImportError> {
    if let Some(obj) = val.get("providers").and_then(|p| p.as_object()) {
        return parse_provider_object_map(obj, false);
    }

    let provider_list = if let Some(arr) = val.as_array() {
        arr
    } else if let Some(arr) = val.get("providers").and_then(|p| p.as_array()) {
        arr
    } else {
        return Err(ImportError::UnknownSchema);
    };

    let mut result = Vec::new();
    for p in provider_list {
        let name = get_str_alias(p, &["name", "label", "title"]).unwrap_or("CC Provider");
        let id = get_str_alias(p, &["id", "provider_id", "providerId", "slug"])
            .map(|s| s.to_string())
            .unwrap_or_else(|| slugify_id(name));

        let base_url = match get_str_alias(p, &["base_url", "url", "endpoint", "baseUrl"]) {
            Some(u) => u.trim_end_matches('/').to_string(),
            None => continue,
        };

        let api_key = match get_str_alias(p, &["api_key", "key", "token", "apiKey"]) {
            Some(k) => k.to_string(),
            None => continue,
        };

        let api_type =
            get_str_alias(p, &["api_type", "type", "format", "protocol"]).unwrap_or("openai");
        let format = if api_type.eq_ignore_ascii_case("anthropic")
            || api_type.eq_ignore_ascii_case("claude")
        {
            ProviderFormat::Anthropic
        } else {
            ProviderFormat::OpenAi
        };

        let enabled = p.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);

        let mut models = Vec::new();
        if let Some(m_arr) = p.get("models").and_then(|m| m.as_array()) {
            for m in m_arr {
                if let Some(s) = m.as_str() {
                    models.push(s.to_string());
                } else if let Some(s) = m.get("id").and_then(|v| v.as_str()) {
                    models.push(s.to_string());
                } else if let Some(s) = m.get("name").and_then(|v| v.as_str()) {
                    models.push(s.to_string());
                }
            }
        }

        result.push(ImportedProvider {
            id,
            name: name.to_string(),
            format,
            base_url,
            api_key,
            models,
            enabled,
        });
    }

    if result.is_empty() {
        Err(ImportError::EmptyProviders)
    } else {
        Ok(result)
    }
}

/// Helper to parse object map `{ "deepseek": { "url": "...", "key": "..." } }`.
fn parse_provider_object_map(
    obj: &serde_json::Map<String, serde_json::Value>,
    _is_cherry: bool,
) -> Result<Vec<ImportedProvider>, ImportError> {
    let mut result = Vec::new();
    for (k, v) in obj {
        let name = get_str_alias(v, &["name", "title"]).unwrap_or(k.as_str());
        let id = slugify_id(k);

        let base_url = match get_str_alias(v, &["baseUrl", "base_url", "url", "apiUrl", "endpoint"])
        {
            Some(u) => u.trim_end_matches('/').to_string(),
            None => continue,
        };

        let api_key = match get_str_alias(v, &["apiKey", "api_key", "key", "token"]) {
            Some(key) => key.to_string(),
            None => continue,
        };

        let type_str =
            get_str_alias(v, &["apiType", "type", "format", "api_type"]).unwrap_or("openai");
        let format = if type_str.eq_ignore_ascii_case("anthropic")
            || type_str.eq_ignore_ascii_case("claude")
        {
            ProviderFormat::Anthropic
        } else {
            ProviderFormat::OpenAi
        };

        let enabled = v.get("enabled").and_then(|b| b.as_bool()).unwrap_or(true);

        let mut models = Vec::new();
        if let Some(m_arr) = v.get("models").and_then(|m| m.as_array()) {
            for m in m_arr {
                if let Some(s) = m.as_str() {
                    models.push(s.to_string());
                } else if let Some(s) = m.get("id").and_then(|i| i.as_str()) {
                    models.push(s.to_string());
                } else if let Some(s) = m.get("name").and_then(|i| i.as_str()) {
                    models.push(s.to_string());
                }
            }
        }

        result.push(ImportedProvider {
            id,
            name: name.to_string(),
            format,
            base_url,
            api_key,
            models,
            enabled,
        });
    }

    if result.is_empty() {
        Err(ImportError::EmptyProviders)
    } else {
        Ok(result)
    }
}

/// Auto-detect source configuration format and extract imported providers.
pub fn parse_auto(val: &serde_json::Value) -> Result<Vec<ImportedProvider>, ImportError> {
    // 1. Try Cherry Studio if contains characteristic keys
    if val.get("version").is_some() || val.to_string().contains("apiType") {
        if let Ok(providers) = parse_cherry_studio(val) {
            return Ok(providers);
        }
    }

    // 2. Try CC Switch
    if let Ok(providers) = parse_cc_switch(val) {
        return Ok(providers);
    }

    // 3. Fallback retry Cherry Studio
    if let Ok(providers) = parse_cherry_studio(val) {
        return Ok(providers);
    }

    Err(ImportError::UnknownSchema)
}

/// Top-level import function supporting format selection and auto-detection.
pub fn import_providers(
    format: SourceFormat,
    raw_content: &str,
) -> Result<Vec<ImportedProvider>, ImportError> {
    let val: serde_json::Value =
        serde_json::from_str(raw_content).map_err(|e| ImportError::Json(e.to_string()))?;

    match format {
        SourceFormat::CherryStudio => parse_cherry_studio(&val),
        SourceFormat::CcSwitch => parse_cc_switch(&val),
        SourceFormat::Auto | SourceFormat::Generic => parse_auto(&val),
    }
}
