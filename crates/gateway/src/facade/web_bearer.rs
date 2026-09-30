//! Kiro's web-account services (KiroWebBearerService), which the client's takeover points
//! at the gateway: cloud config and cloud sessions.
//!
//! Kiro 1.1 always has both on. Left at app.kiro.dev they received the gateway's bearer
//! token, the first turn of every session waited for the cloud config (up to a minute),
//! and a failed fetch showed "Couldn't fetch your cloud config." in the chat. Kiro treats
//! a cloud config that is not enabled for the account as nothing to show, and an empty
//! list of cloud sessions as none, so the gateway answers exactly that.
//!
//! The protocol is Smithy RPC v2 CBOR: `POST /service/KiroWebBearerService/operation/<Op>`,
//! bodies in CBOR, an error named by the `__type` of its body whatever the status (Kiro's
//! client reads the name after the `#` and finds the error class by it).

use super::{BoxFuture, FacadeHandler, Response};
use axum::{
    body::Body,
    http::{header, HeaderValue, Method, Request, StatusCode},
    response::IntoResponse,
};

/// The Smithy namespace of the service's errors.
const NAMESPACE: &str = "com.amazon.kirowebportalservice.bearer";

/// Every operation of the service Kiro 1.1 sends, with the path it sends it to.
pub const OPERATIONS: &[(&str, &str)] = &[
    (
        "GetConfigManifest",
        "/service/KiroWebBearerService/operation/GetConfigManifest",
    ),
    (
        "GetConfigContents",
        "/service/KiroWebBearerService/operation/GetConfigContents",
    ),
    (
        "ListSpaces",
        "/service/KiroWebBearerService/operation/ListSpaces",
    ),
    (
        "CreateSpace",
        "/service/KiroWebBearerService/operation/CreateSpace",
    ),
    (
        "DeleteSpace",
        "/service/KiroWebBearerService/operation/DeleteSpace",
    ),
    (
        "GetSpace",
        "/service/KiroWebBearerService/operation/GetSpace",
    ),
    (
        "GetSessionStatus",
        "/service/KiroWebBearerService/operation/GetSessionStatus",
    ),
    (
        "LoadSession",
        "/service/KiroWebBearerService/operation/LoadSession",
    ),
    (
        "RespondToPermission",
        "/service/KiroWebBearerService/operation/RespondToPermission",
    ),
    (
        "SendAcpMessage",
        "/service/KiroWebBearerService/operation/SendAcpMessage",
    ),
    (
        "StreamSendMessage",
        "/service/KiroWebBearerService/operation/StreamSendMessage",
    ),
    (
        "ListAvailableProviders",
        "/service/KiroWebBearerService/operation/ListAvailableProviders",
    ),
    (
        "ListProviderResources",
        "/service/KiroWebBearerService/operation/ListProviderResources",
    ),
    (
        "CheckProviderSetup",
        "/service/KiroWebBearerService/operation/CheckProviderSetup",
    ),
];

/// Where the service's paths start: the gateway answers them without a sign-in, as they
/// carry nothing of the account.
pub const PATH_PREFIX: &str = "/service/KiroWebBearerService/operation/";

/// One of the service's operations.
pub struct WebBearerHandler {
    operation: &'static str,
    path: &'static str,
}

impl WebBearerHandler {
    /// A handler for each operation.
    pub fn all() -> Vec<Self> {
        OPERATIONS
            .iter()
            .map(|&(operation, path)| Self { operation, path })
            .collect()
    }
}

impl FacadeHandler for WebBearerHandler {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        self.path
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            super::discard_body(req.into_body(), 1024 * 1024).await;
            answer(self.operation)
        })
    }
}

/// What the gateway answers `operation`.
fn answer(operation: &str) -> Response {
    match operation {
        // Kiro drops what it synced before and shows nothing.
        "GetConfigManifest" | "GetConfigContents" => error(
            StatusCode::FORBIDDEN,
            "CloudConfigNotEnabledException",
            "Cloud config is not enabled for this account",
        ),
        // No cloud sessions, no connected source providers.
        "ListSpaces" => ok(&Cbor::Map(vec![("spaces", Cbor::Array(Vec::new()))])),
        "ListAvailableProviders" => ok(&Cbor::Map(vec![("providers", Cbor::Array(Vec::new()))])),
        "ListProviderResources" => ok(&Cbor::Map(vec![("resources", Cbor::Array(Vec::new()))])),
        // Only ever sent when the customer opens or starts one.
        _ => error(
            StatusCode::FORBIDDEN,
            "ForbiddenException",
            "本服务不提供 Kiro 云端会话。",
        ),
    }
}

fn ok(body: &Cbor) -> Response {
    cbor_response(StatusCode::OK, body)
}

fn error(status: StatusCode, name: &str, message: &str) -> Response {
    let shape = format!("{NAMESPACE}#{name}");
    cbor_response(
        status,
        &Cbor::Map(vec![
            ("__type", Cbor::Text(&shape)),
            ("message", Cbor::Text(message)),
        ]),
    )
}

fn cbor_response(status: StatusCode, body: &Cbor) -> Response {
    let mut bytes = Vec::new();
    body.encode(&mut bytes);
    let mut response = (status, bytes).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/cbor"),
    );
    headers.insert("smithy-protocol", HeaderValue::from_static("rpc-v2-cbor"));
    response
}

/// The CBOR (RFC 8949) the answers take: text, arrays and maps with text keys.
enum Cbor<'a> {
    Text(&'a str),
    Array(Vec<Cbor<'a>>),
    Map(Vec<(&'a str, Cbor<'a>)>),
}

impl Cbor<'_> {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Cbor::Text(text) => {
                head(out, 3, text.len() as u64);
                out.extend_from_slice(text.as_bytes());
            }
            Cbor::Array(items) => {
                head(out, 4, items.len() as u64);
                for item in items {
                    item.encode(out);
                }
            }
            Cbor::Map(entries) => {
                head(out, 5, entries.len() as u64);
                for (key, value) in entries {
                    Cbor::Text(key).encode(out);
                    value.encode(out);
                }
            }
        }
    }
}

/// A data item's head: its major type and its length, in the shortest form.
fn head(out: &mut Vec<u8>, major: u8, length: u64) {
    let major = major << 5;
    match length {
        0..=23 => out.push(major | length as u8),
        24..=0xff => out.extend_from_slice(&[major | 24, length as u8]),
        0x100..=0xffff => {
            out.push(major | 25);
            out.extend_from_slice(&(length as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(major | 26);
            out.extend_from_slice(&(length as u32).to_be_bytes());
        }
        _ => {
            out.push(major | 27);
            out.extend_from_slice(&length.to_be_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heads_take_the_shortest_form() {
        let mut out = Vec::new();
        Cbor::Map(vec![("spaces", Cbor::Array(Vec::new()))]).encode(&mut out);
        assert_eq!(out, b"\xa1\x66spaces\x80");
        for (length, expected) in [
            (23u64, vec![0x77]),
            (24, vec![0x78, 24]),
            (300, vec![0x79, 0x01, 0x2c]),
            (70_000, vec![0x7a, 0x00, 0x01, 0x11, 0x70]),
        ] {
            let mut out = Vec::new();
            head(&mut out, 3, length);
            assert_eq!(out, expected, "{length}");
        }
    }
}
