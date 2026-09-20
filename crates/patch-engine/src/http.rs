//! Application-scoped CA trust; public HTTPS verification remains enabled.
pub(crate) fn add_ca(
    builder: reqwest::ClientBuilder,
    pem: &[u8],
) -> Result<reqwest::ClientBuilder, String> {
    let certificates = reqwest::Certificate::from_pem_bundle(pem).map_err(|e| e.to_string())?;
    if certificates.is_empty() {
        return Err("Configured CA file contains no certificates".into());
    }
    Ok(certificates.into_iter().fold(builder, |b, certificate| {
        b.add_root_certificate(certificate)
    }))
}

pub(crate) fn client_builder() -> Result<reqwest::ClientBuilder, String> {
    client_builder_with_ca(
        std::env::var_os("KIRO_GATEWAY_CA_CERT")
            .as_deref()
            .map(std::path::Path::new),
    )
}

pub(crate) fn client_builder_with_ca(
    path: Option<&std::path::Path>,
) -> Result<reqwest::ClientBuilder, String> {
    let builder = reqwest::Client::builder();
    if let Some(path) = path {
        let pem = std::fs::read(path)
            .map_err(|e| format!("Cannot read configured KIRO_GATEWAY_CA_CERT: {e}"))?;
        return add_ca(builder, &pem).map_err(|e| format!("Invalid KIRO_GATEWAY_CA_CERT: {e}"));
    }
    Ok(builder)
}

#[cfg(test)]
mod tests {
    #[test]
    fn deployment_ca_is_valid_and_invalid_ca_is_rejected() {
        assert!(super::add_ca(
            reqwest::Client::builder(),
            include_bytes!("../../../deploy/server-ca.pem")
        )
        .unwrap()
        .build()
        .is_ok());
        assert!(super::add_ca(reqwest::Client::builder(), b"not a certificate").is_err());
    }
}

#[cfg(test)]
mod initialization_tests {
    #[test]
    fn invalid_ca_configuration_is_an_error_without_fallback_or_panic() {
        let missing = std::env::temp_dir().join(format!(
            "kiro-missing-ca-{}-{}.pem",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let error = super::client_builder_with_ca(Some(&missing)).unwrap_err();
        assert!(error.contains("Cannot read configured KIRO_GATEWAY_CA_CERT"));
        let invalid = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/http.rs");
        let error = super::client_builder_with_ca(Some(&invalid)).unwrap_err();
        assert!(error.contains("Invalid KIRO_GATEWAY_CA_CERT"));
        assert!(super::client_builder_with_ca(None).is_ok());
    }
}

/// Bound decoded bytes while streaming; Content-Length is only an early rejection.
pub(crate) async fn bounded_json(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<serde_json::Value, String> {
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err("Cloud response exceeds byte limit".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Cannot read cloud response")?
    {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err("Cloud response exceeds byte limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid cloud response".into())
}

#[cfg(test)]
mod response_limit_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn response(wire: Vec<u8>) -> reqwest::Response {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await;
            let _ = socket.write_all(&wire).await;
        });
        reqwest::Client::new()
            .get(format!("http://{addr}"))
            .send()
            .await
            .unwrap()
    }
    #[tokio::test]
    async fn usage_body_limit_checks_stream_not_only_content_length() {
        let normal = response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".to_vec()).await;
        assert_eq!(
            bounded_json(normal, 2).await.unwrap(),
            serde_json::json!({})
        );
        let declared =
            response(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n".to_vec()).await;
        assert!(bounded_json(declared, 32)
            .await
            .unwrap_err()
            .contains("byte limit"));
        let mut wire = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        for _ in 0..8 {
            wire.extend_from_slice(b"10\r\n0123456789abcdef\r\n");
        }
        wire.extend_from_slice(b"0\r\n\r\n");
        assert!(bounded_json(response(wire).await, 64)
            .await
            .unwrap_err()
            .contains("byte limit"));
    }
}
