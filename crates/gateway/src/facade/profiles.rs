//! Profiles handler.
//!
//! Spec §4.2, P0-5 (mgmt-schema.md §2.2).

use super::virtualization::VirtualizationStore;
use super::{json_response, BoxFuture, FacadeHandler, Response};
use crate::auth::AuthClaims;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProfileInfo {
    pub arn: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ListAvailableProfilesResponse {
    pub profiles: Vec<ProfileInfo>,
    pub next_token: Option<String>,
}

/// Handler for `POST /ListAvailableProfiles`
#[derive(Clone, Default)]
pub struct ListAvailableProfilesHandler {
    store: VirtualizationStore,
}

impl ListAvailableProfilesHandler {
    pub fn new(store: VirtualizationStore) -> Self {
        Self { store }
    }
}

impl FacadeHandler for ListAvailableProfilesHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/ListAvailableProfiles"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let claims = req.extensions().get::<AuthClaims>();
            let group = self.store.get_group(claims.map(|c| c.group_id.as_str()));

            // P0-5 Minimum Required Set: Must return >=1 profile with valid ARN structure
            // to satisfy Kiro ProfileArnGuard and prevent UI popup error.
            let resp = ListAvailableProfilesResponse {
                profiles: vec![ProfileInfo {
                    arn: group.profile_arn,
                    profile_name: Some("KiroProfile-us-east-1".to_string()),
                }],
                next_token: None,
            };
            json_response(StatusCode::OK, &resp)
        })
    }
}
