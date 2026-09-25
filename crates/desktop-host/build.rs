fn main() {
    println!("cargo:rerun-if-env-changed=SUPERKIRO_BUILD_REVISION");
    println!("cargo:rerun-if-env-changed=SUPERKIRO_RELEASE_VERSION");
    // A release build carries the version it is published as: what the client shows, and
    // what the releases it may update to are compared with. Other builds never update.
    let release = std::env::var("SUPERKIRO_RELEASE_VERSION").unwrap_or_default();
    if !release.is_empty() {
        let parts: Vec<&str> = release.split('.').collect();
        assert!(
            (1..=4).contains(&parts.len())
                && parts
                    .iter()
                    .all(|p| (1..=9).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit())),
            "release version must be one to four dot-separated numbers, e.g. 2026.09.25"
        );
    }
    let version = std::env::var("CARGO_PKG_VERSION").expect("Cargo supplies package version");
    let version = match std::env::var("SUPERKIRO_BUILD_REVISION") {
        _ if !release.is_empty() => release.clone(),
        Ok(revision) => {
            assert!(
                revision.len() == 40 && revision.bytes().all(|b| b.is_ascii_hexdigit()),
                "build revision must be a full Git SHA"
            );
            format!("{version}-preview.{}", &revision[..12])
        }
        Err(_) => version,
    };
    println!("cargo:rustc-env=SUPERKIRO_BUILD_VERSION={version}");
    println!("cargo:rustc-env=SUPERKIRO_RELEASE_VERSION={release}");
    tauri_build::build()
}
