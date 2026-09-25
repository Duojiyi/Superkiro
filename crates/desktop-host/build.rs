/// A semantic version, MAJOR.MINOR.PATCH, without leading zeros: what Windows and macOS
/// accept as an application version, and what clients compare part by part.
fn semantic(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            (1..=9).contains(&p.len())
                && p.bytes().all(|b| b.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'))
        })
}

fn main() {
    println!("cargo:rerun-if-env-changed=SUPERKIRO_BUILD_REVISION");
    println!("cargo:rerun-if-env-changed=SUPERKIRO_RELEASE_VERSION");
    println!("cargo:rerun-if-changed=tauri.conf.json");
    // The product version, set once in tauri.conf.json: what the file properties (Windows)
    // and Info.plist (macOS) show.
    let config = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").expect("Cargo"))
        .join("tauri.conf.json");
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config).expect("tauri.conf.json"))
            .expect("tauri.conf.json is JSON");
    let product = config["version"]
        .as_str()
        .filter(|version| semantic(version))
        .expect("tauri.conf.json names the product version, e.g. 0.1.1")
        .to_string();
    // A release build carries the version it is published as: what the client shows, and
    // what the releases it may update to are compared with. Other builds never update.
    let release = std::env::var("SUPERKIRO_RELEASE_VERSION").unwrap_or_default();
    if !release.is_empty() {
        assert!(
            semantic(&release),
            "release version must be MAJOR.MINOR.PATCH, e.g. 0.1.1"
        );
        // What is published is built for release: the version it shows, the one the system
        // shows for it and the one it compares updates with are then one and the same.
        if std::env::var("PROFILE").as_deref() == Ok("release") {
            assert_eq!(
                release, product,
                "release version must be the version in tauri.conf.json; set it there first"
            );
        }
    }
    let version = product;
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
