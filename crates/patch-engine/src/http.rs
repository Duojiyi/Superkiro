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

pub(crate) fn client_builder() -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder();
    if let Some(path) = std::env::var_os("KIRO_GATEWAY_CA_CERT") {
        let pem = std::fs::read(path).expect("Cannot read configured KIRO_GATEWAY_CA_CERT");
        return add_ca(builder, &pem).expect("Invalid KIRO_GATEWAY_CA_CERT");
    }
    builder
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
