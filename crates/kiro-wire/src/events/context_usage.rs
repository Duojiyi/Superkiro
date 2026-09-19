//! `contextUsageEvent` model.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsageEvent {
    #[serde(default)]
    pub context_usage_percentage: f64,
}

impl ContextUsageEvent {
    pub fn new(context_usage_percentage: f64) -> Self {
        Self {
            context_usage_percentage,
        }
    }
}
