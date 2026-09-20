fn main() {
    println!("cargo:rerun-if-env-changed=SUPERKIRO_BUILD_REVISION");
    let version = std::env::var("CARGO_PKG_VERSION").expect("Cargo supplies package version");
    let version = match std::env::var("SUPERKIRO_BUILD_REVISION") {
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
    tauri_build::build()
}
