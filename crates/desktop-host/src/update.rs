//! In-place updates of this client from its own download server, signed offline.
//!
//! The server's `/downloads/releases.json` names the current release for each platform.
//! A release installs only when it is newer than this build and its entry carries a
//! signature, by one of `UPDATE_KEYS`, over its target, version, size and hash: whoever
//! controls the server or the network can withhold an update, never substitute one.
//!
//! Installing never touches the client the customer starts until the new version has
//! proven itself. The verified download is staged beside it and started from there, and
//! the old version exits. Once the new version's window is up and talking to the host it
//! confirms: it moves the old version aside and itself into its place. Until then the
//! customer's own shortcut still starts the old version, whatever happens to the new one:
//! a crash, a window that never loads, an antivirus quarantining it, or the customer
//! starting the client again meanwhile. A trial that ends without confirming is counted,
//! and after two such trials that release's hash is refused, so a broken release cannot
//! restart the client again and again.
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// Public halves of the offline release keys (deploy/update_signing.py). A list, so a new
/// key can ship in a release, trusted alongside the old, before the old one is retired.
const UPDATE_KEYS: &[&str] = &["346633520d5a0d37dbf8cc09028724118a84c7eb14181596e2a8439a699263eb"];

/// Trusted only by debug builds, for local end-to-end tests (the key seeded with byte 42 in
/// .review-scratch/update_demo.py). A release build never trusts it, and the publishers
/// refuse any binary that carries it.
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

/// The identifier in tauri.conf.json, which names the local data directory.
const IDENTIFIER: &str = "app.superkiro.desktop";
const MANIFEST_LIMIT: usize = 256 * 1024;
const ARTIFACT_LIMIT: u64 = 512 * 1024 * 1024;
/// Set for the new version by the one it replaces: that process's id, and the marker the
/// new version writes to say it has started.
const HANDOFF_PID: &str = "SUPERKIRO_UPDATE_HANDOFF";
const HANDOFF_MARKER: &str = "SUPERKIRO_UPDATE_MARKER";
/// Set for the customer's own version when a trial that never came up steps aside for it:
/// the trial's process id, to wait for before taking the single instance.
const FALLBACK_PID: &str = "SUPERKIRO_UPDATE_FALLBACK";
/// How long the replaced process waits for the new one to show it started.
const START_LIMIT: Duration = Duration::from_secs(30);
/// How long a new process waits for the one before it to exit.
const PREDECESSOR_LIMIT: Duration = Duration::from_secs(60);
/// How long a trial may take to bring its window up before it steps aside.
const CONFIRM_LIMIT: Duration = Duration::from_secs(180);
/// Bounds on the download: no answer, a stalled line, and the whole transfer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const READ_TIMEOUT: Duration = Duration::from_secs(60);
const DOWNLOAD_LIMIT: Duration = Duration::from_secs(30 * 60);
/// Trials of one release that may end without confirming before its hash is refused.
const FAILURES_BEFORE_REJECT: u64 = 2;
/// How many refused hashes are remembered; enough to outlast a few bad releases.
const REJECTED_LIMIT: usize = 24;
/// A partial download older than this belongs to a release long superseded.
const PART_LIFETIME: Duration = Duration::from_secs(3 * 24 * 3600);

/// This process is a trial of a newly installed version, not yet confirmed.
struct Trial {
    pending: Pending,
    /// Held until confirmation: while it is held, no other process judges this trial.
    lock: Mutex<Option<fs::File>>,
}

static TRIAL: OnceLock<Trial> = OnceLock::new();
/// Where the customer's client is, once a confirmed trial has moved itself there.
static CANONICAL: OnceLock<PathBuf> = OnceLock::new();
static CONFIRMED: AtomicBool = AtomicBool::new(false);
/// The installer's lock, held until this process exits once a trial has been started.
static INSTALLER_LOCK: Mutex<Option<fs::File>> = Mutex::new(None);
static INSTALLING: AtomicBool = AtomicBool::new(false);
static CANCELLED: AtomicBool = AtomicBool::new(false);

/// Whether this run is a newly installed version on its first start.
pub fn was_updated() -> bool {
    TRIAL.get().is_some()
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
    let mut response = tokio::time::timeout(
        REQUEST_TIMEOUT,
        client
            .get(format!("{origin}/downloads/releases.json"))
            .send(),
    )
    .await
    .map_err(|_| "[update:download] The update server did not answer".to_string())?
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

/// The release this client should install now, if any. A release whose trials never
/// confirmed often enough (its hash is refused in `state`) is skipped.
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

// ---- persistent state: a staged trial, trials that failed, and refused hashes ----

/// The update state, in the client's own local data directory: the directory Tauri's
/// `app_local_data_dir` resolves, computed the same way so the check before the app is
/// built and the host agree on it.
pub fn state_path() -> Option<PathBuf> {
    Some(
        dirs::data_local_dir()?
            .join(IDENTIFIER)
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

/// Counts a trial of `sha256` that ended without confirming; refuses the hash at the limit.
fn record_failure(state: &mut Value, sha256: &str) {
    if !state["failures"].is_object() {
        state["failures"] = json!({});
    }
    let count = state["failures"][sha256].as_u64().unwrap_or(0) + 1;
    if count >= FAILURES_BEFORE_REJECT {
        if let Some(failures) = state["failures"].as_object_mut() {
            failures.remove(sha256);
        }
        reject_hash(state, sha256);
    } else {
        state["failures"][sha256] = json!(count);
    }
}

fn clear_failures(state: &mut Value, sha256: &str) {
    // Through get_mut: indexing a missing key would write a null into the state.
    if let Some(failures) = state.get_mut("failures").and_then(Value::as_object_mut) {
        failures.remove(sha256);
    }
}

/// A staged version started as a trial: what it is, where it runs from, and where the
/// customer's own client is.
#[derive(Debug, Clone, PartialEq)]
struct Pending {
    version: String,
    sha256: String,
    /// The staged executable (Windows) or app bundle (macOS).
    staged: PathBuf,
    /// What the trial runs: the staged executable.
    executable: PathBuf,
    /// The customer's client: the executable or bundle the shortcut starts.
    current: PathBuf,
    /// The executable to start for the customer's client.
    current_executable: PathBuf,
}

fn pending_of(state: &Value) -> Option<Pending> {
    let p = state.get("pending")?;
    let path = |key: &str| p[key].as_str().map(PathBuf::from);
    Some(Pending {
        version: p["version"].as_str()?.to_string(),
        sha256: p["sha256"].as_str()?.to_string(),
        staged: path("staged")?,
        executable: path("executable")?,
        current: path("current")?,
        current_executable: path("current_executable")?,
    })
}

fn set_pending(state: &mut Value, pending: Option<&Pending>) {
    match pending {
        Some(p) => {
            state["pending"] = json!({
                "version": p.version,
                "sha256": p.sha256,
                "staged": p.staged.to_string_lossy(),
                "executable": p.executable.to_string_lossy(),
                "current": p.current.to_string_lossy(),
                "current_executable": p.current_executable.to_string_lossy(),
            })
        }
        None => {
            if let Some(object) = state.as_object_mut() {
                object.remove("pending");
            }
        }
    }
}

// ---- where the customer's client is ----

/// The executable inside the customer's client: itself (Windows), or the bundle's binary.
fn executable_in(current: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        current.join("Contents/MacOS/Superkiro")
    } else {
        current.to_path_buf()
    }
}

/// The customer's client (the executable or app bundle their shortcut starts) and the
/// executable to start for it. A trial runs from its staged copy, but this is still the
/// customer's; after confirming, the trial is at that path itself.
fn client_location() -> Result<(PathBuf, PathBuf), String> {
    if let Some(current) = CANONICAL.get() {
        return Ok((current.clone(), executable_in(current)));
    }
    if let Some(trial) = TRIAL.get() {
        return Ok((
            trial.pending.current.clone(),
            trial.pending.current_executable.clone(),
        ));
    }
    let executable = std::env::current_exe()
        .map_err(|_| "[update:replace] Cannot locate the running client".to_string())?;
    if cfg!(target_os = "macos") {
        let bundle = executable
            .ancestors()
            .find(|path| path.extension().is_some_and(|ext| ext == "app"))
            .ok_or("[update:replace] The client is not inside an app bundle")?
            .to_path_buf();
        Ok((bundle, executable))
    } else {
        Ok((executable.clone(), executable))
    }
}

/// The customer's client: where downloads and staged versions go, beside it.
pub fn client_path() -> Result<PathBuf, String> {
    client_location().map(|(current, _)| current)
}

/// A hidden name beside `current` for what an update keeps there.
fn beside(current: &Path, suffix: &str) -> PathBuf {
    let name = current.file_name().unwrap_or_default().to_string_lossy();
    current.with_file_name(format!(".{name}.{suffix}"))
}

// ---- the download: resuming an interrupted one, verified whole ----

/// Where a download of `release` is kept, beside the client so the verified file can be
/// staged by a rename on one volume. Named by hash, so another release's bytes are never
/// continued; kept between attempts and restarts so an interrupted download resumes.
fn download_part(current: &Path, release: &Release) -> PathBuf {
    beside(current, &format!("{}.part", &hex(&release.sha256)[..16]))
}

/// What to do with a part file of `existing` bytes for a release of `size`.
#[derive(Debug, PartialEq)]
enum Resume {
    Fresh,
    From(u64),
    Complete,
}

fn resume_plan(existing: u64, size: u64) -> Resume {
    match existing {
        0 => Resume::Fresh,
        n if n == size => Resume::Complete,
        n if n < size => Resume::From(n),
        // Longer than the signed size: it cannot be these bytes. Start over.
        _ => Resume::Fresh,
    }
}

/// The first byte a `206 Partial Content` starts at, as its `Content-Range` says.
fn content_range_start(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let value = headers.get(reqwest::header::CONTENT_RANGE)?.to_str().ok()?;
    value
        .strip_prefix("bytes ")?
        .split('-')
        .next()?
        .trim()
        .parse()
        .ok()
}

fn file_sha256(path: &Path) -> std::io::Result<[u8; 32]> {
    use std::io::Read;
    let mut file = fs::File::open(path)?;
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        context.update(&buffer[..read]);
    }
    let mut digest = [0u8; 32];
    digest.copy_from_slice(context.finish().as_ref());
    Ok(digest)
}

/// Downloads `release` beside the client at `current`, resuming bytes already there, and
/// returns the file once it is exactly the signed bytes. `progress` hears (received,
/// total, resumed). A cancel, a stall or the overall deadline stops it; what arrived stays
/// for the next attempt.
pub async fn download(
    client: &reqwest::Client,
    origin: &str,
    release: &Release,
    current: &Path,
    mut progress: impl FnMut(u64, u64, bool),
) -> Result<PathBuf, String> {
    use std::io::Write;
    let failed =
        |e: reqwest::Error| format!("[update:download] {}", crate::backend::network_error(e));
    let part = download_part(current, release);
    let existing = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    let mut have = match resume_plan(existing, release.size) {
        Resume::Complete => release.size,
        Resume::From(n) => n,
        Resume::Fresh => {
            let _ = fs::remove_file(&part);
            0
        }
    };
    let resumed = have > 0;
    progress(have, release.size, resumed);

    if have < release.size {
        let mut request = client.get(format!("{origin}{}", release.url));
        if have > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
        }
        let mut response = tokio::time::timeout(REQUEST_TIMEOUT, request.send())
            .await
            .map_err(|_| "[update:download] The update server did not answer".to_string())?
            .map_err(failed)?;
        let status = response.status();
        // A 206 continues what we have, from where the server says it starts (a proxy may
        // answer from elsewhere); a 200 ignores the range, so the file starts over.
        let continues = have > 0
            && status == reqwest::StatusCode::PARTIAL_CONTENT
            && content_range_start(response.headers()) == Some(have);
        if !continues && status != reqwest::StatusCode::OK {
            let _ = fs::remove_file(&part);
            return Err(format!("[update:download] Update HTTP {}", status.as_u16()));
        }
        let opened = if continues {
            fs::OpenOptions::new().append(true).open(&part)
        } else {
            have = 0;
            fs::File::create(&part)
        };
        // Writing beside the client is the first write the update makes; a folder this user
        // cannot write (Program Files) stops it here, the client untouched.
        let mut file =
            opened.map_err(|e| format!("[update:replace] Cannot write beside the client: {e}"))?;
        let deadline = Instant::now() + DOWNLOAD_LIMIT;
        loop {
            if CANCELLED.load(Ordering::Relaxed) {
                return Err("[update:download] The update was cancelled".into());
            }
            if Instant::now() >= deadline {
                return Err("[update:download] The update download took too long".into());
            }
            // A stalled line fails in a minute; a slow one may take as long as it needs.
            let chunk = tokio::time::timeout(READ_TIMEOUT, response.chunk())
                .await
                .map_err(|_| "[update:download] Update download timed out".to_string())?
                .map_err(failed)?;
            let Some(chunk) = chunk else { break };
            if chunk.len() as u64 > release.size - have {
                let _ = fs::remove_file(&part);
                return Err("[update:verify] The update is larger than was signed".into());
            }
            file.write_all(&chunk)
                .map_err(|e| format!("[update:replace] Cannot write beside the client: {e}"))?;
            have += chunk.len() as u64;
            progress(have, release.size, resumed);
        }
        file.sync_all()
            .map_err(|e| format!("[update:replace] Cannot write beside the client: {e}"))?;
    }

    let size = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if size != release.size {
        // Short: the connection dropped. It stays for the next attempt to continue.
        return Err("[update:download] The update download was cut short".into());
    }
    match file_sha256(&part) {
        Ok(digest) if digest == release.sha256 => Ok(part),
        _ => {
            // Not the signed bytes: discard, so a corrupt resume cannot wedge every attempt.
            let _ = fs::remove_file(&part);
            Err("[update:verify] The update does not match its signed hash".into())
        }
    }
}

// ---- staging and starting the trial, the customer's client untouched ----

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

/// Makes the verified download at `verified` a runnable version beside the client, without
/// touching the client itself.
fn stage(
    verified: &Path,
    current: &Path,
    current_executable: &Path,
    release: &Release,
) -> Result<Pending, String> {
    let replace =
        |e: std::io::Error| format!("[update:replace] Cannot write beside the client: {e}");
    #[cfg(not(target_os = "macos"))]
    let (staged, executable) = {
        // In a hidden folder under the client's own file name, so the trial shows in Task
        // Manager as the client does, and moves out to take its place.
        let work = beside(current, &format!("{}.new", release.version));
        let _ = remove_path(&work);
        fs::create_dir(&work).map_err(replace)?;
        let staged = work.join(current.file_name().unwrap_or_default());
        if let Err(error) = retry(|| fs::rename(verified, &staged)) {
            let _ = fs::remove_dir_all(&work);
            return Err(replace(error));
        }
        (staged.clone(), staged)
    };
    #[cfg(target_os = "macos")]
    let (staged, executable) = {
        let work = beside(current, &format!("{}.new", release.version));
        let _ = remove_path(&work);
        fs::create_dir(&work).map_err(replace)?;
        let unpacked = Command::new("/usr/bin/tar")
            .arg("-xzf")
            .arg(verified)
            .arg("-C")
            .arg(&work)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        let _ = fs::remove_file(verified);
        let bundle = work.join("Superkiro.app");
        let executable = bundle.join("Contents/MacOS/Superkiro");
        if !unpacked || !executable.is_file() {
            let _ = fs::remove_dir_all(&work);
            return Err("[update:verify] The update archive does not hold the app".into());
        }
        (bundle, executable)
    };
    Ok(Pending {
        version: release.version.clone(),
        sha256: hex(&release.sha256),
        staged,
        executable,
        current: current.to_path_buf(),
        current_executable: current_executable.to_path_buf(),
    })
}

/// What removing a staged version removes: the file, or on macOS the folder it came in.
fn staged_root(pending: &Pending) -> PathBuf {
    pending
        .staged
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| pending.staged.clone())
}

/// Starts a trial of the staged version and waits until it signals it is up.
fn start_trial(pending: &Pending, marker: &Path) -> Result<(), String> {
    let _ = fs::remove_file(marker);
    let spawned = Command::new(&pending.executable)
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
        Err("[update:relaunch] The updated client did not start; this version carries on".into())
    }
}

/// Stops more than one install at a time, for as long as it is held.
pub struct InstallGuard;

impl Drop for InstallGuard {
    fn drop(&mut self) {
        INSTALLING.store(false, Ordering::SeqCst);
    }
}

pub fn begin_install() -> Result<InstallGuard, String> {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("Operation in progress".into());
    }
    CANCELLED.store(false, Ordering::SeqCst);
    Ok(InstallGuard)
}

/// Stops a download in progress; what arrived stays for the next attempt.
pub fn cancel() {
    CANCELLED.store(true, Ordering::SeqCst);
}

/// Installs `release` from its verified download: stages it beside the customer's client
/// and starts it as a trial. The client the customer starts stays exactly as it was until
/// the trial confirms. On success the caller exits. Runs off the async runtime (it blocks).
pub fn install(state: &Path, release: &Release, verified: &Path) -> Result<(), String> {
    if CANCELLED.load(Ordering::SeqCst) {
        return Err("[update:download] The update was cancelled".into());
    }
    // Held from here until this process exits: while a trial is starting, no other start of
    // the client judges it as one that never came up.
    let lock = open_lock(&lock_path(state)).map_err(|_| "Operation in progress".to_string())?;
    let (current, current_executable) = client_location()?;
    let pending = stage(verified, &current, &current_executable, release)?;
    let mut value = read_state(state);
    set_pending(&mut value, Some(&pending));
    if let Err(error) = write_state(state, &value) {
        let _ = remove_path(&staged_root(&pending));
        return Err(format!("[update:replace] {error}"));
    }
    if let Err(error) = start_trial(&pending, &marker_path(state)) {
        // It could not even start: these bytes are counted against, and nothing else changes.
        let _ = remove_path(&staged_root(&pending));
        let mut value = read_state(state);
        set_pending(&mut value, None);
        record_failure(&mut value, &pending.sha256);
        let _ = write_state(state, &value);
        return Err(error);
    }
    *INSTALLER_LOCK.lock().unwrap_or_else(|e| e.into_inner()) = Some(lock);
    Ok(())
}

// ---- the trial: waiting for the old version, confirming, or stepping aside ----

/// Called at the very start of `main`, before the single-instance plugin or the app lock.
/// A trial waits for the version it replaces to exit and takes the update lock; a normal
/// start judges a trial that ended without confirming.
pub fn startup(state: Option<&Path>) {
    let predecessor = std::env::var(HANDOFF_PID).ok().and_then(|p| p.parse().ok());
    let marker = std::env::var_os(HANDOFF_MARKER).map(PathBuf::from);
    let fallback = std::env::var(FALLBACK_PID)
        .ok()
        .and_then(|p| p.parse().ok());
    // Kiro and anything else this client starts must not inherit these.
    std::env::remove_var(HANDOFF_PID);
    std::env::remove_var(HANDOFF_MARKER);
    std::env::remove_var(FALLBACK_PID);
    if let Some(pid) = fallback {
        // A trial stepping aside for this client: let it end before taking its place.
        ExitWaiter::open(pid).wait(PREDECESSOR_LIMIT);
    }
    let (Some(state), Some(current)) = (state, release_version()) else {
        // Builds that never update themselves leave update state alone.
        if let Some(pid) = predecessor {
            wait_for_predecessor(pid, marker.as_deref());
        }
        return;
    };
    if let Some(pid) = predecessor {
        let pending = pending_of(&read_state(state)).filter(|p| p.version == current);
        wait_for_predecessor(pid, marker.as_deref());
        if let Some(pending) = pending {
            // The installer held the lock until it exited; it is ours from now until we confirm.
            let lock = acquire_lock_within(&lock_path(state), Duration::from_secs(30));
            if TRIAL
                .set(Trial {
                    pending,
                    lock: Mutex::new(lock),
                })
                .is_ok()
            {
                start_watchdog(state.to_path_buf());
            }
        }
        return;
    }
    judge_ended_trial(state, current);
}

/// A trial that ended without confirming is counted against its bytes, and the record of
/// it cleared; the customer's client, never touched, simply carries on. While the update
/// lock is held a trial (or its installer) is live, and nothing is judged.
fn judge_ended_trial(state: &Path, current: &str) {
    if pending_of(&read_state(state)).is_none() {
        return;
    }
    let Ok(_lock) = open_lock(&lock_path(state)) else {
        return;
    };
    let mut value = read_state(state);
    let Some(pending) = pending_of(&value) else {
        return;
    };
    if pending.version != current {
        record_failure(&mut value, &pending.sha256);
        let _ = remove_path(&staged_root(&pending));
    }
    // Being the pending version ourselves, the trial moved itself into place and only its
    // record was left: nothing failed.
    set_pending(&mut value, None);
    let _ = write_state(state, &value);
}

/// A trial whose window is not up in time steps aside: it releases the update lock, starts
/// the customer's own client, which judges it, and exits.
fn start_watchdog(state: PathBuf) {
    std::thread::spawn(move || {
        let deadline = Instant::now() + CONFIRM_LIMIT;
        while Instant::now() < deadline {
            if CONFIRMED.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        step_aside(&state);
    });
}

fn step_aside(state: &Path) {
    let Some(trial) = TRIAL.get() else { return };
    if CONFIRMED.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = state;
    drop(trial.lock.lock().unwrap_or_else(|e| e.into_inner()).take());
    let _ = Command::new(&trial.pending.current_executable)
        .env(FALLBACK_PID, std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    std::process::exit(0);
}

/// Called by a trial once its window is up and talking to the host. It moves the customer's
/// client aside and itself into its place; the version kept aside is removed. A no-op for
/// anything but an unconfirmed trial.
pub fn confirm(state: &Path) {
    let Some(trial) = TRIAL.get() else { return };
    if CONFIRMED.swap(true, Ordering::SeqCst) {
        return;
    }
    let moved = move_into_place(&trial.pending);
    let mut value = read_state(state);
    if pending_of(&value).as_ref() == Some(&trial.pending) {
        set_pending(&mut value, None);
    }
    if moved {
        clear_failures(&mut value, &trial.pending.sha256);
        let _ = CANONICAL.set(trial.pending.current.clone());
    } else {
        // It runs, but cannot take the client's place (the folder refused the rename): the
        // customer's client stays the old version, and repeated tries are counted.
        record_failure(&mut value, &trial.pending.sha256);
    }
    let _ = write_state(state, &value);
    drop(trial.lock.lock().unwrap_or_else(|e| e.into_inner()).take());
}

/// Moves the customer's client aside and the staged version into its place, putting the
/// client back if the second step fails. A running executable or bundle may be renamed.
fn move_into_place(pending: &Pending) -> bool {
    let aside = beside(&pending.current, &format!("{}.old", std::process::id()));
    let _ = remove_path(&aside);
    if retry(|| fs::rename(&pending.current, &aside)).is_err() {
        return false;
    }
    if retry(|| fs::rename(&pending.staged, &pending.current)).is_err() {
        let _ = retry(|| fs::rename(&aside, &pending.current));
        return false;
    }
    let _ = remove_path(&aside);
    let _ = fs::remove_dir_all(staged_root(pending));
    true
}

/// Waits for the process this one replaced to exit and, on Windows, for its WebView2 helper
/// to release the shared user-data folder, before the caller takes the single instance.
fn wait_for_predecessor(pid: u32, marker: Option<&Path>) {
    // Opened before the predecessor may end, so a reused id cannot be waited on.
    let waiter = ExitWaiter::open(pid);
    if let Some(marker) = marker {
        // Tells the predecessor we started; never follows a link a prior run left.
        let _ = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(marker)
            .and_then(|mut file| std::io::Write::write_all(&mut file, b"ready"));
    }
    waiter.wait(PREDECESSOR_LIMIT);
    #[cfg(windows)]
    {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline
            && patch_engine::has_child_process(pid, "msedgewebview2.exe")
        {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

// ---- the update lock and the start marker ----

fn lock_path(state: &Path) -> PathBuf {
    state.with_file_name("update.lock")
}

/// An exclusive lock on `path`, held as long as the file stays open. The operating system
/// releases it when the process ends, however it ends.
fn open_lock(path: &Path) -> std::io::Result<fs::File> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    file.try_lock()
        .map_err(|_| std::io::Error::other("the update lock is held"))?;
    Ok(file)
}

fn acquire_lock_within(path: &Path, limit: Duration) -> Option<fs::File> {
    let deadline = Instant::now() + limit;
    loop {
        if let Ok(file) = open_lock(path) {
            return Some(file);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The marker a trial writes to tell the process that started it that it is up: in the
/// update directory, not a shared temp folder, and unique to this start.
fn marker_path(state: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    state.with_file_name(format!(".update-ready-{}-{nanos:x}", std::process::id()))
}

// ---- what updates leave beside the client ----

/// Removes what updates left beside the customer's client: versions kept aside, staged
/// versions no trial needs, and partial downloads long superseded. Skipped while a trial or
/// an install is live. Best effort; whatever is locked goes at a later start.
pub fn remove_leftovers(state: &Path) {
    if TRIAL.get().is_some() && !CONFIRMED.load(Ordering::SeqCst) {
        return;
    }
    let Ok(_lock) = open_lock(&lock_path(state)) else {
        return;
    };
    let Ok((current, _)) = client_location() else {
        return;
    };
    let keep: Vec<PathBuf> = pending_of(&read_state(state))
        .map(|pending| vec![pending.staged.clone(), staged_root(&pending)])
        .unwrap_or_default();
    remove_leftovers_of(&current, &keep, SystemTime::now());
}

fn remove_leftovers_of(current: &Path, keep: &[PathBuf], now: SystemTime) {
    let (Some(dir), Some(name)) = (current.parent(), current.file_name()) else {
        return;
    };
    let prefix = format!(".{}.", name.to_string_lossy());
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let own = std::process::id().to_string();
    for entry in entries.flatten() {
        let path = entry.path();
        if keep.contains(&path) {
            continue;
        }
        let file = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = file.strip_prefix(&prefix) else {
            continue;
        };
        let Some((middle, kind)) = rest.rsplit_once('.') else {
            continue;
        };
        let ours = match kind {
            // A download, kept to resume; gone once long superseded.
            "part" => {
                middle.len() == 16
                    && middle.bytes().all(|b| b.is_ascii_hexdigit())
                    && entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|modified| now.duration_since(modified).ok())
                        .is_some_and(|age| age > PART_LIFETIME)
            }
            // Kept aside by a process (its id), or staged (a version); never this process's.
            "old" | "new" | "exe" => {
                !middle.is_empty()
                    && middle != own
                    && middle.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            }
            _ => false,
        };
        if ours {
            let _ = remove_path(&path);
        }
    }
}

// ---- waiting for another process to end ----

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
    /// Never a version a release build could be: tests stay true whatever CI builds as.
    const OTHER: &str = "9999.1.1";

    fn signed(entry: &mut Value, seed: &[u8; 32]) -> String {
        use ring::signature::KeyPair;
        let pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(seed).unwrap();
        let message = signed_message(
            entry["platform"].as_str().unwrap(),
            entry["arch"].as_str().unwrap(),
            entry["version"].as_str().unwrap(),
            entry["sha256"].as_str().unwrap(),
            entry["size"].as_u64().unwrap(),
        );
        entry["updateSignature"] = json!(hex(pair.sign(message.as_bytes()).as_ref()));
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

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("update-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn release_of(body: &[u8], version: &str) -> Release {
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(ring::digest::digest(&ring::digest::SHA256, body).as_ref());
        Release {
            version: version.into(),
            url: "/downloads/app".into(),
            sha256,
            size: body.len() as u64,
            mandatory: true,
        }
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

    #[test]
    fn the_state_lives_where_tauri_keeps_local_data() {
        let config = include_str!("../tauri.conf.json");
        assert!(config.contains(&format!("\"identifier\": \"{IDENTIFIER}\"")));
        let path = state_path().unwrap();
        assert!(path.ends_with(Path::new(IDENTIFIER).join("update-state.json")));
    }

    #[test]
    fn a_download_resumes_from_what_is_already_on_disk() {
        let size = 13_156_864;
        assert_eq!(resume_plan(0, size), Resume::Fresh);
        assert_eq!(resume_plan(4096, size), Resume::From(4096));
        assert_eq!(resume_plan(size, size), Resume::Complete);
        // Longer than the signed size cannot be these bytes: start over.
        assert_eq!(resume_plan(size + 1, size), Resume::Fresh);
    }

    /// A minimal HTTP server: the first plain GET declares the full length but sends only
    /// half and closes (an interrupted download); a ranged GET serves 206 with the rest.
    fn serve_with_one_break(listener: std::net::TcpListener, body: Vec<u8>) {
        use std::io::{Read, Write};
        let mut first = true;
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buffer = [0u8; 2048];
            let mut request = Vec::new();
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&buffer[..n]),
                }
            }
            let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
            let range = text.lines().find_map(|line| {
                line.strip_prefix("range: bytes=")
                    .and_then(|r| r.strip_suffix('-'))
                    .and_then(|n| n.parse::<usize>().ok())
            });
            let (head, payload): (String, &[u8]) = match range {
                Some(start) => (
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                        body.len() - start, start, body.len() - 1, body.len()
                    ),
                    &body[start..],
                ),
                None if first => {
                    first = false;
                    (
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()),
                        &body[..body.len() / 2],
                    )
                }
                None => (
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()),
                    &body[..],
                ),
            };
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(payload);
        }
    }

    #[tokio::test]
    async fn an_interrupted_download_resumes_over_http_and_verifies_whole() {
        let dir = scratch("resume-http");
        let client_file = dir.join("Superkiro.exe");
        let body: Vec<u8> = (0..120_000u32).map(|i| (i % 251) as u8).collect();
        let release = release_of(&body, OTHER);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let served = body.clone();
        std::thread::spawn(move || serve_with_one_break(listener, served));

        let client = reqwest::Client::new();
        // The first attempt is cut short; the part file beside the client keeps what arrived.
        let first = download(&client, &origin, &release, &client_file, |_, _, _| {}).await;
        assert!(first.is_err(), "{first:?}");
        let part = download_part(&client_file, &release);
        let kept = part.metadata().unwrap().len();
        assert!(
            kept > 0 && kept < body.len() as u64,
            "kept {kept} of {}",
            body.len()
        );

        // The next attempt resumes where it broke and returns the whole, verified file.
        let mut first_event = None;
        let verified = download(
            &client,
            &origin,
            &release,
            &client_file,
            |received, _, resumed| {
                first_event.get_or_insert((received, resumed));
            },
        )
        .await
        .unwrap();
        assert_eq!(
            first_event,
            Some((kept, true)),
            "resumed from the break point"
        );
        assert_eq!(fs::read(&verified).unwrap(), body);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_trial_that_fails_twice_is_refused() {
        let mut value = json!({});
        record_failure(&mut value, "abc");
        assert!(rejected_hashes(&value).is_empty());
        assert_eq!(value["failures"]["abc"], 1);
        record_failure(&mut value, "abc");
        assert_eq!(rejected_hashes(&value), vec!["abc".to_string()]);
        assert!(value["failures"].get("abc").is_none());
        // A success in between clears the count.
        record_failure(&mut value, "def");
        clear_failures(&mut value, "def");
        record_failure(&mut value, "def");
        assert!(!rejected_hashes(&value).contains(&"def".to_string()));
    }

    #[test]
    fn rejected_hashes_are_bounded_newest_kept() {
        let mut value = json!({});
        for i in 0..(REJECTED_LIMIT + 5) {
            reject_hash(&mut value, &format!("{i:064x}"));
        }
        let kept = rejected_hashes(&value);
        assert_eq!(kept.len(), REJECTED_LIMIT);
        assert!(kept.contains(&format!("{:064x}", REJECTED_LIMIT + 4)));
        assert!(!kept.contains(&format!("{:064x}", 0)));
    }

    #[cfg(not(target_os = "macos"))]
    fn staged_beside(dir: &Path, old: &[u8], new: &[u8]) -> Pending {
        let client = dir.join("Superkiro.exe");
        fs::write(&client, old).unwrap();
        let release = release_of(new, OTHER);
        let verified = download_part(&client, &release);
        fs::write(&verified, new).unwrap();
        stage(&verified, &client, &client, &release).unwrap()
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn staging_never_touches_the_customers_client() {
        let dir = scratch("stage");
        let pending = staged_beside(&dir, b"old", b"new");
        assert_eq!(fs::read(&pending.current).unwrap(), b"old");
        assert_eq!(fs::read(&pending.staged).unwrap(), b"new");
        assert_eq!(pending.executable, pending.staged);
        assert_eq!(pending.version, OTHER);
        // The trial runs under the client's own file name, so it looks like the client.
        assert_eq!(pending.staged.file_name(), pending.current.file_name());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn clearing_failures_writes_nothing_when_there_are_none() {
        let mut value = json!({});
        clear_failures(&mut value, "abc");
        assert_eq!(value, json!({}));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn confirming_moves_the_new_version_into_place() {
        let dir = scratch("confirm");
        let pending = staged_beside(&dir, b"old", b"new");
        assert!(move_into_place(&pending));
        assert_eq!(fs::read(&pending.current).unwrap(), b"new");
        assert!(!pending.staged.exists());
        let left: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(left.len(), 1, "{left:?}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_failed_move_leaves_the_customers_client_in_place() {
        let dir = scratch("move-fail");
        let pending = staged_beside(&dir, b"old", b"new");
        // The staged copy vanished (an antivirus took it): the client must be put back.
        fs::remove_file(&pending.staged).unwrap();
        assert!(!move_into_place(&pending));
        assert_eq!(fs::read(&pending.current).unwrap(), b"old");
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn an_ended_trial_is_counted_and_the_client_carries_on() {
        let dir = scratch("judge");
        let state = dir.join("update-state.json");
        let pending = staged_beside(&dir, b"old", b"new");
        let mut value = json!({});
        set_pending(&mut value, Some(&pending));
        write_state(&state, &value).unwrap();

        // A live trial holds the lock: nothing is judged.
        let held = open_lock(&lock_path(&state)).unwrap();
        judge_ended_trial(&state, "2026.09.22");
        assert!(pending_of(&read_state(&state)).is_some());
        drop(held);

        // The trial is gone without confirming: counted, its staged copy removed, the
        // customer's client untouched.
        judge_ended_trial(&state, "2026.09.22");
        let value = read_state(&state);
        assert!(pending_of(&value).is_none());
        assert_eq!(value["failures"][&pending.sha256], 1);
        assert!(!pending.staged.exists());
        assert_eq!(fs::read(&pending.current).unwrap(), b"old");
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_new_version_finding_its_own_record_counts_nothing() {
        let dir = scratch("judge-self");
        let state = dir.join("update-state.json");
        let pending = staged_beside(&dir, b"old", b"new");
        let mut value = json!({});
        set_pending(&mut value, Some(&pending));
        write_state(&state, &value).unwrap();
        judge_ended_trial(&state, OTHER);
        let value = read_state(&state);
        assert!(pending_of(&value).is_none());
        assert!(value["failures"].get(&pending.sha256).is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn leftovers_are_removed_and_nothing_else() {
        let dir = scratch("leftovers");
        let client = dir.join("Superkiro.exe");
        fs::write(&client, b"client").unwrap();
        let keep = [
            "Superkiro.exe.old",
            ".Superkiro.exe.x.old",
            "other.txt",
            ".Superkiro.exe.12.log",
            ".Superkiro.exe.0123456789abcdef.part",
            ".Superkiro.exe.2026.09.26.new",
        ];
        for name in keep {
            fs::write(dir.join(name), b"keep").unwrap();
        }
        for name in [
            ".Superkiro.exe.12.old",
            ".Superkiro.exe.34.new",
            ".Superkiro.exe.2026.09.24.new",
        ] {
            fs::write(dir.join(name), b"gone").unwrap();
        }
        fs::create_dir(dir.join(".Superkiro.exe.56.old")).unwrap();
        let pending_staged = dir.join(".Superkiro.exe.2026.09.26.new");
        // Three days on, the kept part file is a superseded download; judged a week later.
        let later = SystemTime::now() + PART_LIFETIME / 2;
        remove_leftovers_of(&client, std::slice::from_ref(&pending_staged), later);
        let mut left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let mut expected: Vec<String> = keep.iter().map(|s| s.to_string()).collect();
        expected.push("Superkiro.exe".into());
        expected.sort();
        assert_eq!(left, expected);
        // Once old enough, the part file goes too.
        remove_leftovers_of(
            &client,
            &[pending_staged],
            SystemTime::now() + PART_LIFETIME * 2,
        );
        assert!(!dir.join(".Superkiro.exe.0123456789abcdef.part").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_dev_build_has_no_release_version() {
        if env!("SUPERKIRO_RELEASE_VERSION").is_empty() {
            assert_eq!(release_version(), None);
        } else {
            assert_eq!(release_version(), Some(env!("SUPERKIRO_RELEASE_VERSION")));
        }
    }
}
