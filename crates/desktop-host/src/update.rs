//! In-place updates of this client from its own download server, signed offline.
//!
//! The server's `/downloads/releases.json` names the current release for each platform.
//! A release installs only when it is newer than this build and its entry carries a
//! signature, by one of `UPDATE_KEYS`, over its target, version, size and hash: whoever
//! controls the server or the network can withhold an update, never substitute one.
//!
//! Installing swaps the new bytes into place, then starts them beside the old version kept
//! aside. The new version proves itself: once its window is up it confirms, and only then
//! is the old version deleted. A new version that never confirms - it crashed, its window
//! never loaded, an antivirus quarantined it - is rolled back at the next start, and its
//! hash is refused so the client cannot be pushed into the same broken release again.
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Public halves of the offline release keys (deploy/update_signing.py). A list, so a new
/// key can ship in a release, trusted alongside the old, before the old one is retired.
const UPDATE_KEYS: &[&str] = &["346633520d5a0d37dbf8cc09028724118a84c7eb14181596e2a8439a699263eb"];

/// Trusted only by debug builds, for local end-to-end tests (the key seeded with byte 42 in
/// .review-scratch/update_demo.py). A release build never trusts it, so a manifest signed
/// with it cannot update customers.
#[cfg(debug_assertions)]
const TEST_UPDATE_KEY: &str = "197f6b23e16c8532c6abc838facd5ea789be0c76b2920334039bfa8b3d368d61";

fn trusted_keys() -> Vec<&'static str> {
    let mut keys = UPDATE_KEYS.to_vec();
    #[cfg(debug_assertions)]
    keys.push(TEST_UPDATE_KEY);
    keys
}

/// The release this build is. The publishers refuse a binary unless it carries the marker
/// of the version it is published as: a client that believed itself older than the release
/// it just installed would install it again at every start. A reachable `static` keeps the
/// whole marker in the binary through `/OPT:REF,ICF` and dead-stripping, one copy only.
static RELEASE_MARKER: &str = concat!("superkiro-release:", env!("SUPERKIRO_RELEASE_VERSION"), ";");

const MANIFEST_LIMIT: usize = 256 * 1024;
const ARTIFACT_LIMIT: u64 = 512 * 1024 * 1024;
/// Set for the new process by the one it replaces: that process's id, and where it should
/// write the marker that tells the replaced process it has started.
const HANDOFF_PID: &str = "SUPERKIRO_UPDATE_HANDOFF";
const HANDOFF_MARKER: &str = "SUPERKIRO_UPDATE_MARKER";
/// How long the replaced process waits for the new one to show it started.
const START_LIMIT: Duration = Duration::from_secs(30);
/// How long the new process waits for the replaced one to exit before it takes the lock.
const PREDECESSOR_LIMIT: Duration = Duration::from_secs(60);
/// How many rejected hashes are remembered; enough to outlast a few bad releases.
const REJECTED_LIMIT: usize = 24;

static UPDATED: AtomicBool = AtomicBool::new(false);

/// Whether this run is a freshly updated, confirmed build.
pub fn was_updated() -> bool {
    UPDATED.load(Ordering::Relaxed)
}

/// This build's release version; None for builds that never update themselves.
pub fn release_version() -> Option<&'static str> {
    RELEASE_MARKER
        .strip_prefix("superkiro-release:")?
        .strip_suffix(';')
        .filter(|version| !version.is_empty())
}

/// This build's release target, as the manifest names it.
pub fn target() -> Option<(&'static str, &'static str)> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some(("windows", "x64"))
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(("macos", "arm64"))
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some(("macos", "x64"))
    } else {
        None
    }
}

/// One to four dot-separated numbers ("2026.09.25", "2026.09.25.1").
fn version_parts(version: &str) -> Option<[u64; 4]> {
    let mut parts = [0; 4];
    for (count, part) in version.split('.').enumerate() {
        if count == 4
            || part.is_empty()
            || part.len() > 9
            || !part.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        parts[count] = part.parse().ok()?;
    }
    Some(parts)
}

/// Whether `candidate` is a later release than `current`; missing parts count as 0.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    matches!((version_parts(candidate), version_parts(current)), (Some(a), Some(b)) if a > b)
}

/// What a release's signature covers (deploy/update_signing.py builds the same bytes).
pub fn signed_message(
    platform: &str,
    arch: &str,
    version: &str,
    sha256: &str,
    size: u64,
) -> String {
    format!(
        "superkiro-update/1\nplatform={platform}\narch={arch}\nversion={version}\nsha256={sha256}\nsize={size}"
    )
}

fn hex_bytes<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut bytes = [0; N];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(bytes)
}

fn signed_by(message: &[u8], signature: &[u8; 64], keys: &[&str]) -> bool {
    keys.iter()
        .filter_map(|key| hex_bytes::<32>(key))
        .any(|key| {
            ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
                .verify(message, signature)
                .is_ok()
        })
}

/// A release this build may install, as its signed manifest entry describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    /// A path on the update server: `/downloads/<name>`.
    pub url: String,
    pub sha256: [u8; 32],
    pub size: u64,
    pub mandatory: bool,
}

/// The release in `manifest` this build should install: None when none is newer.
pub fn select(
    manifest: &Value,
    current: &str,
    (platform, arch): (&str, &str),
    keys: &[&str],
) -> Result<Option<Release>, String> {
    let invalid = "[update:verify] Invalid update manifest";
    let releases = manifest["releases"].as_array().ok_or(invalid)?;
    let Some(entry) = releases
        .iter()
        .find(|r| r["platform"] == platform && r["arch"] == arch)
    else {
        return Ok(None);
    };
    let version = entry["version"]
        .as_str()
        .filter(|version| version_parts(version).is_some())
        .ok_or(invalid)?;
    if !is_newer(version, current) {
        return Ok(None);
    }
    // Same origin only, and nothing a path could be built from.
    let url = entry["url"]
        .as_str()
        .filter(|url| {
            url.strip_prefix("/downloads/").is_some_and(|name| {
                (1..=200).contains(&name.len())
                    && !name.starts_with('.')
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
        })
        .ok_or(invalid)?;
    let digest = entry["sha256"].as_str().ok_or(invalid)?;
    let sha256 = hex_bytes::<32>(digest).ok_or(invalid)?;
    let size = entry["size"]
        .as_u64()
        .filter(|size| (1..=ARTIFACT_LIMIT).contains(size))
        .ok_or(invalid)?;
    let signature = entry["updateSignature"]
        .as_str()
        .and_then(hex_bytes::<64>)
        .ok_or("[update:verify] The update is not signed")?;
    let message = signed_message(platform, arch, version, digest, size);
    if !signed_by(message.as_bytes(), &signature, keys) {
        return Err("[update:verify] The update signature is invalid".into());
    }
    Ok(Some(Release {
        version: version.into(),
        url: url.into(),
        sha256,
        size,
        mandatory: entry["mandatory"] == true,
    }))
}

/// Where updates come from: the server this client already talks to. A debug build can be
/// pointed elsewhere, to try an update end to end against a local copy.
pub fn origin(gateway: Result<String, String>) -> Result<String, String> {
    #[cfg(debug_assertions)]
    if let Ok(url) = std::env::var("SUPERKIRO_UPDATE_URL") {
        return patch_engine::patch::validate_gateway_url(&url).map_err(|e| e.to_string());
    }
    gateway
}

async fn manifest(client: &reqwest::Client, origin: &str) -> Result<Value, String> {
    let mut response = client
        .get(format!("{origin}/downloads/releases.json"))
        .send()
        .await
        .map_err(|e| format!("[update:download] {}", crate::backend::network_error(e)))?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(format!(
            "[update:download] Update manifest HTTP {}",
            response.status().as_u16()
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "[update:download] Update manifest interrupted")?
    {
        if chunk.len() > MANIFEST_LIMIT.saturating_sub(bytes.len()) {
            return Err("[update:verify] Update manifest too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "[update:verify] Invalid update manifest".into())
}

/// The release this client should install now, if any. A release whose bytes were rolled
/// back before (its hash is in `state`) is skipped, so a broken forced update cannot loop.
pub async fn available(
    client: &reqwest::Client,
    origin: &str,
    state: &Path,
) -> Result<Option<Release>, String> {
    let (Some(current), Some(target)) = (release_version(), target()) else {
        return Ok(None);
    };
    let release = select(
        &manifest(client, origin).await?,
        current,
        target,
        &trusted_keys(),
    )?;
    let rejected = rejected_hashes(&read_state(state));
    Ok(release.filter(|release| !rejected.contains(&hex(&release.sha256))))
}

/// What the page shows: whether this build updates itself, and what it would install.
pub async fn check(client: &reqwest::Client, origin: &str, state: &Path) -> Result<Value, String> {
    let updated = was_updated();
    let Some(current) = release_version().filter(|_| target().is_some()) else {
        return Ok(json!({"state": "disabled", "updated": updated}));
    };
    Ok(match available(client, origin, state).await? {
        Some(release) => json!({
            "state": "available",
            "current": current,
            "version": release.version,
            "size": release.size,
            "mandatory": release.mandatory,
            "updated": updated,
        }),
        None => json!({"state": "current", "current": current, "updated": updated}),
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The release's bytes, exactly as signed. `progress` hears (received, total).
pub async fn download(
    client: &reqwest::Client,
    origin: &str,
    release: &Release,
    mut progress: impl FnMut(u64, u64),
) -> Result<Vec<u8>, String> {
    let failed =
        |e: reqwest::Error| format!("[update:download] {}", crate::backend::network_error(e));
    let mut response = client
        .get(format!("{origin}{}", release.url))
        .send()
        .await
        .map_err(failed)?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(format!(
            "[update:download] Update HTTP {}",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length != release.size)
    {
        return Err("[update:verify] The update is not the size that was signed".into());
    }
    let mut bytes = Vec::with_capacity(release.size as usize);
    loop {
        // A stalled line fails in a minute; a slow one may take as long as it needs.
        let chunk = tokio::time::timeout(Duration::from_secs(60), response.chunk())
            .await
            .map_err(|_| "[update:download] Update download timed out".to_string())?
            .map_err(failed)?;
        let Some(chunk) = chunk else { break };
        if chunk.len() as u64 > release.size - bytes.len() as u64 {
            return Err("[update:verify] The update is larger than was signed".into());
        }
        bytes.extend_from_slice(&chunk);
        progress(bytes.len() as u64, release.size);
    }
    if bytes.len() as u64 != release.size {
        return Err("[update:download] The update download was cut short".into());
    }
    if ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref() != release.sha256 {
        return Err("[update:verify] The update does not match its signed hash".into());
    }
    Ok(bytes)
}

// ---- persistent state: a pending, unconfirmed boot and the hashes that failed ----

/// The client's own state directory (Tauri's `app_local_data_dir`), computed without the
/// Tauri path resolver so the rollback decision can run before the app is built. `Host`
/// keeps its update state in the same file, so both sides agree on where it is.
pub fn state_path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
    };
    Some(
        base?
            .join("app.superkiro.desktop")
            .join("update-state.json"),
    )
}

fn read_state(state: &Path) -> Value {
    fs::read(state)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn write_state(state: &Path, value: &Value) -> Result<(), String> {
    if let Some(dir) = state.parent() {
        fs::create_dir_all(dir).map_err(|_| "Cannot write update state")?;
    }
    let temp = state.with_extension("json.next");
    fs::write(&temp, value.to_string()).map_err(|_| "Cannot write update state")?;
    fs::rename(&temp, state).map_err(|_| "Cannot write update state".into())
}

fn rejected_hashes(state: &Value) -> Vec<String> {
    state["rejected"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn reject_hash(state: &mut Value, sha256: &str) {
    let mut rejected = rejected_hashes(state);
    rejected.retain(|hash| hash != sha256);
    rejected.push(sha256.to_string());
    let start = rejected.len().saturating_sub(REJECTED_LIMIT);
    state["rejected"] = json!(rejected[start..]);
}

/// A staged version waiting to prove it runs, and how to put the old one back. Rollback and
/// confirm act on `current` and `previous`; the new executable's own path is recorded in the
/// state for diagnostics but not needed here.
struct Pending {
    previous: PathBuf,
    current: PathBuf,
    version: String,
    sha256: String,
}

fn pending_of(state: &Value) -> Option<Pending> {
    let p = state.get("pending")?;
    Some(Pending {
        previous: PathBuf::from(p["previous"].as_str()?),
        current: PathBuf::from(p["current"].as_str()?),
        version: p["version"].as_str()?.to_string(),
        sha256: p["sha256"].as_str()?.to_string(),
    })
}

fn set_pending(state: &mut Value, pending: Option<&Staged>, version: &str, sha256: &str) {
    match pending {
        Some(staged) => {
            state["pending"] = json!({
                "previous": staged.previous.to_string_lossy(),
                "current": staged.current.to_string_lossy(),
                "executable": staged.executable.to_string_lossy(),
                "version": version,
                "sha256": sha256,
            })
        }
        None => {
            if let Some(object) = state.as_object_mut() {
                object.remove("pending");
            }
        }
    }
}

// ---- staging the new bytes on disk ----

/// The running client, replaced on disk by a new version, and how to put it back.
#[derive(Debug)]
pub struct Staged {
    /// Where the client lives: the executable (Windows) or the app bundle (macOS).
    current: PathBuf,
    /// The previous version, moved aside.
    previous: PathBuf,
    /// What to start: the new executable.
    executable: PathBuf,
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Renames on Windows fail for a moment while an antivirus scans a new file.
fn retry(mut action: impl FnMut() -> std::io::Result<()>) -> std::io::Result<()> {
    let mut wait = Duration::from_millis(100);
    for _ in 0..8 {
        match action() {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                std::thread::sleep(wait);
                wait = (wait * 2).min(Duration::from_secs(1));
            }
            result => return result,
        }
    }
    action()
}

/// Puts the previous version back where the client runs. Idempotent: whatever step already
/// happened is treated as done, and success is decided by the client being in place, not by
/// which rename ran - an antivirus that removed the new bytes must not strand the customer.
fn restore_previous(current: &Path, previous: &Path) -> Result<(), String> {
    if previous.exists() {
        if current.exists() {
            let aside = leftover_name(current, "new");
            let _ = remove_path(&aside);
            let _ = retry(|| fs::rename(current, &aside));
            let _ = remove_path(&aside);
        }
        let _ = retry(|| fs::rename(previous, current));
    }
    if current.exists() {
        Ok(())
    } else {
        Err("[update:relaunch] The previous version could not be put back".into())
    }
}

/// Moves `current` aside to `previous` and `next` into its place; puts it back on failure.
fn swap(current: &Path, next: &Path, previous: &Path) -> Result<(), String> {
    let _ = remove_path(previous);
    // A running executable may be renamed, though not deleted or overwritten.
    retry(|| fs::rename(current, previous))
        .map_err(|e| format!("[update:replace] Cannot move the running client aside: {e}"))?;
    if let Err(error) = retry(|| fs::rename(next, current)) {
        let _ = retry(|| fs::rename(previous, current));
        return Err(format!(
            "[update:replace] Cannot put the new client in place: {error}"
        ));
    }
    Ok(())
}

/// The names an update leaves beside `current` for a later start to remove.
fn leftover_name(current: &Path, suffix: &str) -> PathBuf {
    let name = current.file_name().unwrap_or_default().to_string_lossy();
    // Hidden, and unique to this process: an older leftover may still be locked.
    current.with_file_name(format!(".{name}.{}.{suffix}", std::process::id()))
}

/// Replaces the running client on disk with `bytes`, a verified release.
fn stage(bytes: &[u8]) -> Result<Staged, String> {
    let executable = std::env::current_exe()
        .map_err(|_| "[update:replace] Cannot locate the running client".to_string())?;
    stage_at(&executable, bytes)
}

#[cfg(not(target_os = "macos"))]
fn stage_at(executable: &Path, bytes: &[u8]) -> Result<Staged, String> {
    let next = leftover_name(executable, "new");
    let previous = leftover_name(executable, "old");
    // Written beside the client, so the final step is a rename on one volume. Failing here,
    // in a folder this user may not write (Program Files), leaves the client untouched.
    let write = || -> std::io::Result<()> {
        use std::io::Write;
        let mut file = fs::File::create(&next)?;
        file.write_all(bytes)?;
        file.sync_all()
    };
    if let Err(error) = write() {
        let _ = fs::remove_file(&next);
        return Err(format!(
            "[update:replace] Cannot write beside the client: {error}"
        ));
    }
    if let Err(error) = swap(executable, &next, &previous) {
        let _ = fs::remove_file(&next);
        return Err(error);
    }
    Ok(Staged {
        current: executable.to_path_buf(),
        previous,
        executable: executable.to_path_buf(),
    })
}

#[cfg(target_os = "macos")]
fn stage_at(executable: &Path, bytes: &[u8]) -> Result<Staged, String> {
    let bundle = executable
        .ancestors()
        .find(|path| path.extension().is_some_and(|ext| ext == "app"))
        .ok_or("[update:replace] The client is not inside an app bundle")?
        .to_path_buf();
    let work = leftover_name(&bundle, "new");
    let previous = leftover_name(&bundle, "old");
    let unpack = || -> Result<PathBuf, String> {
        fs::create_dir(&work)
            .map_err(|e| format!("[update:replace] Cannot write beside the app: {e}"))?;
        let archive = work.join("update.tar.gz");
        fs::write(&archive, bytes)
            .map_err(|e| format!("[update:replace] Cannot write beside the app: {e}"))?;
        let unpacked = Command::new("/usr/bin/tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&work)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        let fresh = work.join("Superkiro.app");
        if !unpacked || !fresh.join("Contents/MacOS/Superkiro").is_file() {
            return Err("[update:verify] The update archive does not hold the app".into());
        }
        Ok(fresh)
    };
    let result = unpack().and_then(|fresh| swap(&bundle, &fresh, &previous));
    let _ = fs::remove_dir_all(&work);
    result?;
    let relative = executable
        .strip_prefix(&bundle)
        .unwrap_or(Path::new("Contents/MacOS/Superkiro"));
    Ok(Staged {
        executable: bundle.join(relative),
        current: bundle,
        previous,
    })
}

// ---- install, relaunch, and the trial boot ----

fn marker_path(state: &Path) -> PathBuf {
    // In the app-local state dir, not a world-writable temp; a nonce, and created fresh.
    let nonce = format!(
        "{}-{:x}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    );
    state.with_file_name(format!(".update-ready-{nonce}"))
}

/// Replaces this client with `release`'s verified `bytes` and starts the new version. On
/// success the caller exits; the new version waits for it to go, boots, and confirms. On
/// failure nothing is left changed. Runs off the async runtime (it blocks).
pub fn install(state: &Path, bytes: &[u8], release: &Release) -> Result<(), String> {
    let staged = stage(bytes)?;
    let sha256 = hex(&release.sha256);
    let mut value = read_state(state);
    set_pending(&mut value, Some(&staged), &release.version, &sha256);
    if let Err(error) = write_state(state, &value) {
        let _ = restore_previous(&staged.current, &staged.previous);
        return Err(format!("[update:replace] {error}"));
    }
    let marker = marker_path(state);
    match relaunch(&staged, &marker) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = restore_previous(&staged.current, &staged.previous);
            let mut value = read_state(state);
            set_pending(&mut value, None, "", "");
            let _ = write_state(state, &value);
            Err(error)
        }
    }
}

/// Starts the new version and waits until it signals it is up. No file changes: the caller
/// rolls the swap back if this fails.
fn relaunch(staged: &Staged, marker: &Path) -> Result<(), String> {
    let _ = fs::remove_file(marker);
    let spawned = Command::new(&staged.executable)
        .env(HANDOFF_PID, std::process::id().to_string())
        .env(HANDOFF_MARKER, marker)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let started = match spawned {
        Ok(mut child) => {
            let deadline = Instant::now() + START_LIMIT;
            loop {
                if marker.exists() {
                    break true;
                }
                if !matches!(child.try_wait(), Ok(None)) || Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Err(_) => false,
    };
    let _ = fs::remove_file(marker);
    if started {
        Ok(())
    } else {
        Err(
            "[update:relaunch] The updated client did not start; the previous version was put back"
                .into(),
        )
    }
}

/// What [`startup`] tells `main` to do.
pub enum Startup {
    /// Carry on building the app.
    Continue,
    /// A rollback was launched; this process must exit at once, before it takes the lock.
    Exit,
}

/// Called at the very start of `main`, before the single-instance plugin or the app lock.
/// Handles the two sides of an update: the new version's first boot (wait for the version
/// it replaced to exit, then carry on and confirm once up), and a new version that never
/// confirmed (roll back to the version kept aside and hand off to it).
pub fn startup(state: Option<&Path>) -> Startup {
    let predecessor = std::env::var(HANDOFF_PID).ok().and_then(|p| p.parse().ok());
    let marker = std::env::var_os(HANDOFF_MARKER).map(PathBuf::from);
    // Kiro and anything else this client starts must not inherit these.
    std::env::remove_var(HANDOFF_PID);
    std::env::remove_var(HANDOFF_MARKER);

    if let Some(state) = state {
        let mut value = read_state(state);
        if let Some(pending) = pending_of(&value) {
            if release_version() == Some(pending.version.as_str()) {
                if let Some(pid) = predecessor {
                    // Our first boot. Prove ourselves: wait for the old version, then run.
                    // The UI calls confirm() once it is up, which deletes the old version.
                    wait_for_predecessor(pid, marker.as_deref());
                    UPDATED.store(true, Ordering::Relaxed);
                    return Startup::Continue;
                }
                // Started again without ever confirming: crashed, or the window never came
                // up. Put the old version back, refuse these bytes, and hand off to it.
                let _ = restore_previous(&pending.current, &pending.previous);
                reject_hash(&mut value, &pending.sha256);
                set_pending(&mut value, None, "", "");
                let _ = write_state(state, &value);
                let _ = spawn_detached(&pending.current);
                return Startup::Exit;
            }
            // We are not the pending version: a rollback already completed, or a stale
            // record. Drop it so a later, real update is not mistaken for this one.
            set_pending(&mut value, None, "", "");
            let _ = write_state(state, &value);
        }
    }
    if let Some(pid) = predecessor {
        wait_for_predecessor(pid, marker.as_deref());
    }
    Startup::Continue
}

/// Confirms the update once the client is up and its window is talking to the host: the old
/// version is proven unneeded and removed. A no-op when no boot is pending.
pub fn confirm(state: &Path) {
    let mut value = read_state(state);
    if let Some(pending) = pending_of(&value) {
        if release_version() == Some(pending.version.as_str()) {
            let _ = remove_path(&pending.previous);
            set_pending(&mut value, None, "", "");
            let _ = write_state(state, &value);
        }
    }
}

fn spawn_detached(executable: &Path) -> std::io::Result<()> {
    Command::new(executable)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Waits for the process this one replaced to exit and, on Windows, for its WebView2 helper
/// to release the shared user-data folder, before the caller takes the single instance.
fn wait_for_predecessor(pid: u32, marker: Option<&Path>) {
    // Opened before the predecessor may end, so a reused id cannot be waited on.
    let waiter = ExitWaiter::open(pid);
    if let Some(marker) = marker {
        // Tells the predecessor we started; do not follow a symlink a prior run left.
        let _ = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(marker)
            .and_then(|mut file| std::io::Write::write_all(&mut file, b"ready"));
    }
    waiter.wait(PREDECESSOR_LIMIT);
    #[cfg(windows)]
    {
        // The predecessor's WebView2 keeps the user-data folder locked for a moment after it
        // exits; creating our window against it would fail. Give it up to ten seconds.
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline
            && patch_engine::has_child_process(pid, "msedgewebview2.exe")
        {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Removes what an update left beside the client: an old version already confirmed gone, a
/// partial copy. Skips a version still kept for a pending boot. Best effort.
pub fn remove_leftovers(state: &Path) {
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let current = if cfg!(target_os = "macos") {
        match executable
            .ancestors()
            .find(|path| path.extension().is_some_and(|ext| ext == "app"))
        {
            Some(bundle) => bundle.to_path_buf(),
            None => return,
        }
    } else {
        executable
    };
    let keep = pending_of(&read_state(state)).map(|pending| pending.previous);
    remove_leftovers_of(&current, keep.as_deref());
}

fn remove_leftovers_of(current: &Path, keep: Option<&Path>) {
    let (Some(dir), Some(name)) = (current.parent(), current.file_name()) else {
        return;
    };
    let prefix = format!(".{}.", name.to_string_lossy());
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if keep.is_some_and(|keep| keep == entry.path()) {
            continue;
        }
        let file = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = file.strip_prefix(&prefix) else {
            continue;
        };
        // Only our own names: ".<name>.<pid>.old" and ".<name>.<pid>.new".
        let ours = rest.rsplit_once('.').is_some_and(|(pid, kind)| {
            matches!(kind, "old" | "new")
                && !pid.is_empty()
                && pid.bytes().all(|b| b.is_ascii_digit())
                && pid != std::process::id().to_string()
        });
        if ours {
            let _ = remove_path(&entry.path());
        }
    }
}

struct ExitWaiter(
    #[cfg(windows)] *mut std::ffi::c_void,
    #[cfg(not(windows))] u32,
);

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
    fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
}

impl ExitWaiter {
    #[cfg(windows)]
    fn open(pid: u32) -> Self {
        const SYNCHRONIZE: u32 = 0x0010_0000;
        Self(unsafe { OpenProcess(SYNCHRONIZE, 0, pid) })
    }

    #[cfg(not(windows))]
    fn open(pid: u32) -> Self {
        Self(pid)
    }

    /// Until the process has fully ended, handles and locks released, or `limit` passes.
    #[cfg(windows)]
    fn wait(self, limit: Duration) {
        if !self.0.is_null() {
            unsafe {
                WaitForSingleObject(self.0, limit.as_millis().min(u32::MAX as u128) as u32);
                CloseHandle(self.0);
            }
        }
    }

    #[cfg(not(windows))]
    fn wait(self, limit: Duration) {
        // A zombie answers signal 0, so wait for the child to be reaped (reparented away
        // from us) rather than for the pid to disappear.
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline && unsafe { libc::kill(self.0 as libc::pid_t, 0) } == 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SEED: [u8; 32] = [7; 32];

    fn signed(entry: &mut Value, seed: &[u8; 32]) -> String {
        let pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(seed).unwrap();
        let message = signed_message(
            entry["platform"].as_str().unwrap(),
            entry["arch"].as_str().unwrap(),
            entry["version"].as_str().unwrap(),
            entry["sha256"].as_str().unwrap(),
            entry["size"].as_u64().unwrap(),
        );
        entry["updateSignature"] = json!(hex(pair.sign(message.as_bytes()).as_ref()));
        use ring::signature::KeyPair;
        hex(pair.public_key().as_ref())
    }

    fn entry(version: &str) -> Value {
        json!({
            "version": version, "platform": "windows", "arch": "x64", "size": 3,
            "sha256": hex(ring::digest::digest(&ring::digest::SHA256, b"new").as_ref()),
            "url": "/downloads/Superkiro-2026.09.25-Windows.exe", "signature": "unsigned",
            "mandatory": true,
        })
    }

    fn pick(entry: &Value, key: &str) -> Result<Option<Release>, String> {
        select(
            &json!({"releases": [entry]}),
            "2026.09.22",
            ("windows", "x64"),
            &[key],
        )
    }

    /// Signed by deploy/update_signing.py (key seeded with bytes 0..32): the tool and the
    /// client agree on the signed bytes and the signature scheme.
    #[test]
    fn a_signature_from_the_release_tool_verifies() {
        let key = "03a107bff3ce10be1d70dd18e74bc09967e4d6309ba50d5f1ddc8664125531b8";
        let signature = hex_bytes::<64>("f84745f6c16400e43005103671c623c3a4da85c62f91750f4259de11a8046cc4b921045a78210c21a190881a3a9f3bc743b2c632b33223ac8426f2c07bbb4d0f").unwrap();
        let message = signed_message("windows", "x64", "2026.09.25", &"a".repeat(64), 13156864);
        assert!(signed_by(message.as_bytes(), &signature, &[key]));
        let other = signed_message("windows", "x64", "2026.09.26", &"a".repeat(64), 13156864);
        assert!(!signed_by(other.as_bytes(), &signature, &[key]));
        assert!(!signed_by(message.as_bytes(), &signature, UPDATE_KEYS));
    }

    #[test]
    fn versions_compare_numerically_part_by_part() {
        assert!(is_newer("2026.09.25", "2026.09.22"));
        assert!(is_newer("2026.10.01", "2026.09.30"));
        assert!(is_newer("2026.09.25.1", "2026.09.25"));
        assert!(is_newer("10", "9"));
        assert!(!is_newer("2026.09.22", "2026.09.22"));
        assert!(!is_newer("2026.9.22", "2026.09.22"));
        assert!(!is_newer("2026.09.21", "2026.09.22"));
        for bad in ["", "1..2", "1.2.3.4.5", "v1", "1.-2", "1.2a", "1234567890"] {
            assert!(!is_newer(bad, "0") && !is_newer("9", bad), "{bad}");
        }
    }

    #[test]
    fn a_newer_signed_release_is_selected() {
        let mut release = entry("2026.09.25");
        let key = signed(&mut release, &TEST_SEED);
        let selected = pick(&release, &key).unwrap().unwrap();
        assert_eq!(selected.version, "2026.09.25");
        assert_eq!(selected.url, "/downloads/Superkiro-2026.09.25-Windows.exe");
        assert_eq!(selected.size, 3);
        assert!(selected.mandatory);
        release["mandatory"] = json!("yes");
        assert!(!pick(&release, &key).unwrap().unwrap().mandatory);
    }

    #[test]
    fn nothing_newer_or_for_this_target_is_not_an_update() {
        let mut same = entry("2026.09.22");
        let key = signed(&mut same, &TEST_SEED);
        assert_eq!(pick(&same, &key), Ok(None));
        let mut older = entry("2026.09.01");
        signed(&mut older, &TEST_SEED);
        assert_eq!(pick(&older, &key), Ok(None));
        let mut mac = entry("2026.09.25");
        mac["platform"] = json!("macos");
        signed(&mut mac, &TEST_SEED);
        assert_eq!(pick(&mac, &key), Ok(None));
    }

    #[test]
    fn anything_not_signed_by_a_trusted_key_is_refused() {
        let mut release = entry("2026.09.25");
        let trusted = signed(&mut release, &TEST_SEED);
        let mut forged = release.clone();
        signed(&mut forged, &[9; 32]);
        assert!(pick(&forged, &trusted)
            .unwrap_err()
            .contains("signature is invalid"));
        // Every signed field is covered: version (a downgrade or replay), size, hash, target.
        for (field, value) in [
            ("version", json!("2026.09.26")),
            ("size", json!(4)),
            ("sha256", json!("0".repeat(64))),
        ] {
            let mut altered = release.clone();
            altered[field] = value;
            assert!(pick(&altered, &trusted).is_err(), "{field}");
        }
        let mut unsigned = release.clone();
        unsigned.as_object_mut().unwrap().remove("updateSignature");
        assert!(pick(&unsigned, &trusted)
            .unwrap_err()
            .contains("not signed"));
        // The unsigned mandatory flag decides nothing about authenticity.
        let mut optional = release.clone();
        optional["mandatory"] = json!(false);
        assert!(pick(&optional, &trusted).unwrap().is_some());
    }

    #[test]
    fn only_a_plain_same_origin_download_path_is_accepted() {
        for url in [
            "https://elsewhere.example/Superkiro.exe",
            "//elsewhere.example/Superkiro.exe",
            "/downloads/../admin",
            "/downloads/sub/Superkiro.exe",
            "/downloads/.hidden",
            "/downloads/",
            "/other/Superkiro.exe",
            "/downloads/Superkiro.exe?x=1",
        ] {
            let mut release = entry("2026.09.25");
            release["url"] = json!(url);
            let key = signed(&mut release, &TEST_SEED);
            assert!(pick(&release, &key).is_err(), "{url}");
        }
    }

    #[test]
    fn malformed_manifests_are_errors_not_updates() {
        let mut release = entry("2026.09.25");
        let key = signed(&mut release, &TEST_SEED);
        for (field, value) in [
            ("size", json!(0)),
            ("size", json!(ARTIFACT_LIMIT + 1)),
            ("sha256", json!("ABC")),
            ("version", json!("latest")),
        ] {
            let mut broken = release.clone();
            broken[field] = value;
            assert!(pick(&broken, &key).is_err(), "{field}");
        }
        assert!(select(&json!({}), "1", ("windows", "x64"), &[&key]).is_err());
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("update-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_rejected_hash_is_remembered_and_not_offered_again() {
        let dir = scratch("rejected");
        let state = dir.join("update-state.json");
        let mut value = read_state(&state);
        for i in 0..(REJECTED_LIMIT + 5) {
            reject_hash(&mut value, &format!("{i:064x}"));
        }
        write_state(&state, &value).unwrap();
        let kept = rejected_hashes(&read_state(&state));
        assert_eq!(kept.len(), REJECTED_LIMIT);
        // The newest are kept; the oldest fall off.
        assert!(kept.contains(&format!("{:064x}", REJECTED_LIMIT + 4)));
        assert!(!kept.contains(&format!("{:064x}", 0)));
        // Re-rejecting an existing hash moves it to newest without growing the list.
        let last = kept.last().unwrap().clone();
        reject_hash(&mut value, &last);
        assert_eq!(rejected_hashes(&value).len(), REJECTED_LIMIT);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn staging_swaps_the_executable_and_restore_puts_it_back() {
        let dir = scratch("stage");
        let client = dir.join("Superkiro-2026.09.22-Windows.exe");
        fs::write(&client, b"old").unwrap();
        let staged = stage_at(&client, b"new").unwrap();
        assert_eq!(fs::read(&client).unwrap(), b"new");
        assert_eq!(fs::read(&staged.previous).unwrap(), b"old");
        assert_eq!(staged.executable, client);
        restore_previous(&staged.current, &staged.previous).unwrap();
        assert_eq!(fs::read(&client).unwrap(), b"old");
        assert!(!staged.previous.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn restore_is_idempotent_and_survives_a_lost_new_file() {
        let dir = scratch("restore");
        let client = dir.join("Superkiro.exe");
        fs::write(&client, b"old").unwrap();
        let staged = stage_at(&client, b"new").unwrap();
        // An antivirus removed the new bytes: restore must still put the old ones back.
        fs::remove_file(&client).unwrap();
        restore_previous(&staged.current, &staged.previous).unwrap();
        assert_eq!(fs::read(&client).unwrap(), b"old");
        // Running it again changes nothing and still reports the client in place.
        restore_previous(&staged.current, &staged.previous).unwrap();
        assert_eq!(fs::read(&client).unwrap(), b"old");
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_folder_that_cannot_be_written_leaves_the_client_untouched() {
        let dir = scratch("missing");
        let client = dir.join("absent").join("Superkiro.exe");
        let error = stage_at(&client, b"new").unwrap_err();
        assert!(error.starts_with("[update:replace]"), "{error}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn leftovers_are_removed_except_a_pending_previous_and_this_process() {
        let dir = scratch("leftovers");
        let client = dir.join("Superkiro.exe");
        fs::write(&client, b"client").unwrap();
        let keep = [
            "Superkiro.exe.old",
            ".Superkiro.exe.x.old",
            "other.txt",
            ".Superkiro.exe.12.log",
        ];
        for name in keep {
            fs::write(dir.join(name), b"keep").unwrap();
        }
        let pending_previous = dir.join(".Superkiro.exe.99.old");
        fs::write(&pending_previous, b"still-needed").unwrap();
        fs::write(dir.join(".Superkiro.exe.12.old"), b"old").unwrap();
        fs::write(dir.join(".Superkiro.exe.34.new"), b"partial").unwrap();
        fs::create_dir(dir.join(".Superkiro.exe.56.old")).unwrap();
        remove_leftovers_of(&client, Some(&pending_previous));
        let mut left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let mut expected: Vec<String> = keep.iter().map(|s| s.to_string()).collect();
        expected.extend(["Superkiro.exe".into(), ".Superkiro.exe.99.old".into()]);
        expected.sort();
        assert_eq!(left, expected);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn an_unconfirmed_restart_rolls_back_refuses_the_bytes_and_steps_aside() {
        // The pending version equals this test binary's (none): stand in by writing state
        // and calling the pieces startup() runs, without spawning anything.
        let dir = scratch("rollback");
        let state = dir.join("update-state.json");
        let client = dir.join("Superkiro.exe");
        fs::write(&client, b"old").unwrap();
        let staged = stage_at(&client, b"new").unwrap();
        let mut value = read_state(&state);
        set_pending(&mut value, Some(&staged), "2026.09.25", "deadbeef");
        write_state(&state, &value).unwrap();

        // What the rollback branch does when a boot was never confirmed.
        let pending = pending_of(&read_state(&state)).unwrap();
        restore_previous(&pending.current, &pending.previous).unwrap();
        reject_hash(&mut value, &pending.sha256);
        set_pending(&mut value, None, "", "");
        write_state(&state, &value).unwrap();

        assert_eq!(fs::read(&client).unwrap(), b"old");
        let reloaded = read_state(&state);
        assert!(reloaded.get("pending").is_none());
        assert!(rejected_hashes(&reloaded).contains(&"deadbeef".to_string()));
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn confirm_removes_the_old_version_only_for_the_matching_pending() {
        let dir = scratch("confirm");
        let state = dir.join("update-state.json");
        let client = dir.join("Superkiro.exe");
        fs::write(&client, b"new").unwrap();
        let previous = dir.join(".Superkiro.exe.1.old");
        fs::write(&previous, b"old").unwrap();
        let staged = Staged {
            current: client.clone(),
            previous: previous.clone(),
            executable: client.clone(),
        };
        let mut value = read_state(&state);
        // A pending for another version (this test build's release_version() is None) is
        // left alone: confirm() must not touch a boot that is not ours.
        set_pending(&mut value, Some(&staged), "2026.09.25", "deadbeef");
        write_state(&state, &value).unwrap();
        confirm(&state);
        assert!(previous.exists());
        assert!(read_state(&state).get("pending").is_some());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_dev_build_has_no_release_version() {
        // Tests are built without SUPERKIRO_RELEASE_VERSION.
        if env!("SUPERKIRO_RELEASE_VERSION").is_empty() {
            assert_eq!(release_version(), None);
        } else {
            assert_eq!(release_version(), Some(env!("SUPERKIRO_RELEASE_VERSION")));
        }
    }
}
