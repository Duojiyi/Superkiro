use patch_engine::{desktop::DesktopSession, detect_kiro, MemoryGuard, SnapshotManager};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub struct Host {
    pub operation: tokio::sync::Mutex<()>,
    operation_state: std::sync::Mutex<Value>,
    preferences: PathBuf,
    maintenance: std::sync::Mutex<Value>,
    _instance: patch_engine::SingleInstanceLock,
}
impl Host {
    pub fn new(config: PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        std::fs::create_dir_all(&config)?;
        Ok(Self {
            operation: tokio::sync::Mutex::new(()),
            operation_state: std::sync::Mutex::new(load_operation(&config)),
            maintenance: std::sync::Mutex::new(
                json!({"enabled":true,"mode":if cfg!(windows) {"automatic"} else {"monitor-only"},"threshold_mb":2500,"cooldown_seconds":300,"last_sample_mb":null,"last_error":null,"last_trim":null}),
            ),
            preferences: config.join("preferences.json"),
            _instance: patch_engine::SingleInstanceLock::acquire(Some(&config.join("host.lock")))?,
        })
    }
    pub fn operation_status(&self) -> Result<Value, String> {
        self.operation_state
            .lock()
            .map(|s| s.clone())
            .map_err(|_| "Operation state unavailable".into())
    }
    pub fn begin_operation(&self, path: &str) -> Result<(), String> {
        let mut state = self
            .operation_state
            .lock()
            .map_err(|_| "Operation state unavailable")?;
        let path = path.split('?').next().filter(|path| {
            matches!(
                *path,
                "/api/activate"
                    | "/api/restore"
                    | "/api/unbind"
                    | "/api/launch"
                    | "/api/memory/trim"
            )
        });
        *state = json!({"id":state["id"].as_u64().unwrap_or(0) + 1,"state":"running","path":path,"error":null});
        Ok(())
    }
    pub fn finish_operation(&self, result: &Result<Value, String>) {
        if let Ok(mut state) = self.operation_state.lock() {
            state["state"] = json!(if result.is_ok() {
                "succeeded"
            } else {
                "failed"
            });
            // Only fixed labels/status numbers survive. Never persist raw remote errors.
            let details = operation_error(result.as_ref().err().map(String::as_str));
            for key in ["stage", "code", "http_status"] {
                state[key] = details[key].clone();
            }
            state["error"] = if result.is_err() {
                json!("Operation failed; inspect status before recovery")
            } else {
                Value::Null
            };
            state["finished_at"] = json!(SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs());
            let file = self.preferences.with_file_name("last-operation.json");
            let temporary = file.with_extension("tmp");
            let saved = std::fs::write(&temporary, state.to_string())
                .and_then(|()| std::fs::rename(&temporary, &file));
            state["diagnostic_saved"] = json!(saved.is_ok());
        }
    }
    fn install_path(&self) -> Result<Option<PathBuf>, String> {
        match std::fs::read(&self.preferences) {
            Ok(bytes) => {
                let data: Value = serde_json::from_slice(&bytes)
                    .map_err(|_| "Invalid installation preferences")?;
                Ok(data["install_path"].as_str().map(PathBuf::from))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err("Cannot read installation preferences".into()),
        }
    }
    pub fn set_install_path(&self, path: &Path) -> Result<Value, String> {
        if recovery_pending() {
            return Err("Restore Kiro before changing installation".into());
        }
        patch_engine::inspect_installation_dir(path).map_err(|e| e.to_string())?;
        let path = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
        self.save_install_path(&path)
    }
    fn save_install_path(&self, path: &Path) -> Result<Value, String> {
        let temporary = self.preferences.with_extension("tmp");
        std::fs::write(&temporary, json!({"install_path":path}).to_string())
            .map_err(|e| e.to_string())?;
        std::fs::rename(temporary, &self.preferences).map_err(|e| e.to_string())?;
        Ok(json!({"success":true,"path":path}))
    }
}
const CONNECTION_STAGES: &[&str] = &[
    "preflight",
    "launch-prepare",
    "authenticate",
    "close",
    "apply",
    "launch",
];
const ERROR_CODES: &[&str] = &[
    "timeout",
    "tls",
    "auth-rejected",
    "network",
    "permission",
    "unknown",
];
fn operation_error(error: Option<&str>) -> Value {
    let Some(error) = error else {
        return json!({});
    };
    let stage = CONNECTION_STAGES
        .iter()
        .find(|stage| error.starts_with(&format!("[connection:{stage}]")))
        .copied()
        .unwrap_or("unknown");
    let lower = error.to_ascii_lowercase();
    let http_status = error
        .split("Authentication rejected: ")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|s| (100..600).contains(s));
    let code = if lower.contains("timeout") || lower.contains("timed out") {
        "timeout"
    } else if lower.contains("certificate") || lower.contains("tls") {
        "tls"
    } else if http_status.is_some() {
        "auth-rejected"
    } else if lower.contains("network") {
        "network"
    } else if lower.contains("permission") || lower.contains("access denied") {
        "permission"
    } else {
        "unknown"
    };
    json!({"stage":stage,"code":code,"http_status":http_status})
}
fn load_operation(config: &Path) -> Value {
    let idle = json!({"id":0,"state":"idle","path":null,"error":null});
    let Ok(bytes) = std::fs::read(config.join("last-operation.json")) else {
        return idle;
    };
    let Ok(saved) = serde_json::from_slice::<Value>(&bytes) else {
        return idle;
    };
    if !matches!(saved["state"].as_str(), Some("failed" | "succeeded")) {
        return idle;
    }
    let mut clean = json!({"id":saved["id"].as_u64().unwrap_or(0), "state":saved["state"], "path":null, "error":null});
    clean["finished_at"] = json!(saved["finished_at"].as_u64());
    if saved["state"] == "failed" {
        clean["error"] = json!("Operation failed; inspect status before recovery");
        clean["stage"] = json!(saved["stage"]
            .as_str()
            .filter(|v| CONNECTION_STAGES.contains(v))
            .unwrap_or("unknown"));
        clean["code"] = json!(saved["code"]
            .as_str()
            .filter(|v| ERROR_CODES.contains(v))
            .unwrap_or("unknown"));
        clean["http_status"] = json!(saved["http_status"]
            .as_u64()
            .filter(|v| (100..600).contains(v)));
    }
    clean
}
/// The owner is detached by the IPC caller. Dropping a webview waiter must never
/// drop this future while its blocking worker can still mutate local files.
pub async fn run_operation(
    host: &Host,
    path: &str,
    tracked: bool,
    work: impl std::future::Future<Output = Result<Value, String>>,
) -> Result<Value, String> {
    let _guard = host
        .operation
        .try_lock()
        .map_err(|_| "Operation in progress; query /api/operation before retrying".to_string())?;
    if tracked {
        host.begin_operation(path)?;
    }
    let result = work.await;
    if tracked {
        host.finish_operation(&result);
    }
    result
}

// Low-frequency working-set maintenance only. Never kills processes or deletes caches.
pub fn start_maintenance(host: std::sync::Arc<Host>) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_attempt: Option<std::time::Instant> = None;
        loop {
            interval.tick().await;
            let Ok(_operation) = host.operation.try_lock() else {
                continue;
            };
            let sample =
                match tauri::async_runtime::spawn_blocking(MemoryGuard::sample_memory).await {
                    Ok(sample) => sample,
                    Err(_) => {
                        if let Ok(mut state) = host.maintenance.lock() {
                            state["last_error"] = json!("Memory sampling failed");
                            state["last_sample_mb"] = Value::Null;
                        }
                        continue;
                    }
                };
            if let Ok(mut state) = host.maintenance.lock() {
                state["last_error"] = json!(sample.error);
                state["last_sample_mb"] = if sample.error.is_none() {
                    json!(sample.total_memory_mb)
                } else {
                    Value::Null
                };
            }
            if sample.error.is_some()
                || !should_auto_trim(
                    cfg!(windows),
                    sample.total_memory_mb,
                    sample.processes.len(),
                    last_attempt.map(|t| t.elapsed().as_secs()),
                )
            {
                continue;
            }
            last_attempt = Some(std::time::Instant::now());
            let pids: Vec<_> = sample.processes.iter().map(|p| p.pid).collect();
            let result = tauri::async_runtime::spawn_blocking(move || {
                MemoryGuard::trim_working_set(Some(&pids))
            })
            .await;
            if let Ok(mut state) = host.maintenance.lock() {
                match result {
                    Ok(result) if result.error.is_none() => {
                        state["last_trim"] = json!(result);
                        state["last_error"] = if result.failed_count > 0 {
                            json!("Some working sets could not be trimmed")
                        } else {
                            Value::Null
                        };
                    }
                    Ok(result) => {
                        state["last_error"] = json!(result.error);
                        state["last_trim"] = Value::Null;
                    }
                    Err(_) => {
                        state["last_error"] = json!("Working set maintenance failed");
                        state["last_trim"] = Value::Null;
                    }
                }
            }
        }
    });
}
fn should_auto_trim(
    windows: bool,
    total_mb: u64,
    process_count: usize,
    elapsed: Option<u64>,
) -> bool {
    windows && process_count > 0 && total_mb > 2500 && elapsed.is_none_or(|seconds| seconds >= 300)
}

pub fn recovery_pending() -> bool {
    SnapshotManager::default().has_active_snapshot()
        || DesktopSession::system()
            .map(|s| s.recovery_pending())
            .unwrap_or(true)
}
fn select_fields(value: &Value, keys: &[&str]) -> Value {
    Value::Object(
        keys.iter()
            .filter_map(|key| value.get(*key).map(|v| ((*key).to_string(), v.clone())))
            .collect(),
    )
}
fn select_rows(value: &Value, keys: &[&str]) -> Value {
    value
        .as_array()
        .map(|rows| {
            Value::Array(
                rows.iter()
                    .take(90)
                    .map(|v| select_fields(v, keys))
                    .collect(),
            )
        })
        .unwrap_or(Value::Null)
}
fn gateway(value: Option<&str>) -> Result<String, String> {
    let default = std::env::var("KIRO_GATEWAY_URL").unwrap_or_else(|_| "https://kiro.rent".into());
    patch_engine::patch::validate_gateway_url(value.filter(|s| !s.is_empty()).unwrap_or(&default))
        .map_err(|e| e.to_string())
}
pub fn external_allowed(url: &str) -> Result<bool, String> {
    Ok(url == "https://kiro.dev/downloads/" || url == format!("{}/", gateway(None)?))
}
pub fn validate_card(card: &str) -> Result<(), String> {
    if card.trim().is_empty() || card.chars().count() > 256 {
        Err("Invalid card key".into())
    } else {
        Ok(())
    }
}
fn validate_authorization(value: &Value, now: f64) -> Result<(), String> {
    let number = |key: &str| {
        value[key]
            .as_f64()
            .is_some_and(|n| n.is_finite() && n >= 0.0)
    };
    if value["success"] != true
        || !matches!(value["status"].as_str(), Some("active" | "unactivated"))
        || value["isExpired"] != false
        || !["remainingPoints", "totalPoints", "maxDevices"]
            .iter()
            .all(|k| number(k))
        || !value["boundDevices"].is_array()
        || !(value["validUntil"].is_null()
            || value["validUntil"]
                .as_f64()
                .is_some_and(|n| n > now && n <= 8640000000000.0))
    {
        return Err("Card authorization invalid or expired".into());
    }
    Ok(())
}
fn gateway_certificates() -> Result<Vec<reqwest::Certificate>, String> {
    let Some(path) = std::env::var_os("KIRO_GATEWAY_CA_CERT") else {
        return Ok(vec![]);
    };
    let pem =
        std::fs::read(path).map_err(|_| "TLS configuration: cannot read KIRO_GATEWAY_CA_CERT")?;
    let certificates = reqwest::Certificate::from_pem_bundle(&pem)
        .map_err(|_| "TLS configuration: invalid CA bundle")?;
    if certificates.is_empty() {
        return Err("TLS configuration: CA bundle contains no certificates".into());
    }
    Ok(certificates)
}
fn gateway_client(gateway: &str) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20));
    let url = reqwest::Url::parse(gateway).map_err(|_| "Invalid gateway URL")?;
    if matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")) {
        builder = builder.no_proxy();
    }
    for certificate in gateway_certificates()? {
        builder = builder.add_root_certificate(certificate);
    }
    builder
        .build()
        .map_err(|_| "Cannot initialize TLS client".into())
}

async fn verify_card(gateway: &str, card: &str) -> Result<Value, String> {
    validate_card(card)?;
    let response = gateway_client(gateway)?
        .post(format!("{gateway}/api/v1/portal/query"))
        .json(&json!({"card":card.trim()}))
        .send()
        .await
        .map_err(|_| "Card verification network failure")?;
    if !response.status().is_success() {
        return Err(format!(
            "Card verification HTTP {}",
            response.status().as_u16()
        ));
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Invalid card response")?
    {
        if bytes.len() + chunk.len() > 65536 {
            return Err("Card response too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid card response")?;
    validate_authorization(
        &value,
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "Invalid system clock")?
            .as_secs_f64(),
    )?;
    Ok(
        json!({"success":true,"authorization":select_fields(&value, &["virtualPlanName","remainingPoints","totalPoints","validUntil","isExpired","status","maxDevices"]),"gateway_url":gateway}),
    )
}
// Dispatch only passes the host-configured gateway, never IPC input.
async fn fetch_announcements(gateway: &str) -> Result<Value, String> {
    const MAX_BYTES: usize = 1024 * 1024;
    let mut response = gateway_client(gateway)?
        .get(format!(
            "{}/api/v1/announcements",
            gateway.trim_end_matches('/')
        ))
        .send()
        .await
        .map_err(|_| "Announcements network failure")?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(format!("Announcements HTTP {}", response.status().as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|size| size > MAX_BYTES as u64)
    {
        return Err("Announcements response too large".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Invalid announcements response")?
    {
        if chunk.len() > MAX_BYTES.saturating_sub(bytes.len()) {
            return Err("Announcements response too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value = serde_json::from_slice(&bytes).map_err(|_| "Invalid announcements response")?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "Invalid system clock")?
        .as_secs();
    announcements_output(value, now)
}

fn announcements_output(value: Value, now: u64) -> Result<Value, String> {
    let invalid = "Invalid announcements response";
    if value["success"] != true {
        return Err(invalid.into());
    }
    let rows = value["announcements"].as_array().ok_or(invalid)?;
    let mut announcements = Vec::new();
    for row in rows {
        for (key, max) in [("id", 256), ("title", 256), ("content", 20_000)] {
            let text = row[key].as_str().ok_or(invalid)?;
            if text.trim().is_empty() || text.chars().count() > max {
                return Err(invalid.into());
            }
        }
        if !matches!(row["level"].as_str(), Some("info" | "warning" | "critical")) {
            return Err(invalid.into());
        }
        row.get("created_at")
            .and_then(Value::as_u64)
            .ok_or(invalid)?;
        let expires = row.get("expires_at").ok_or(invalid)?;
        if !expires.is_null() {
            let expires = expires.as_u64().ok_or(invalid)?;
            if expires <= now {
                continue;
            }
        }
        announcements.push(select_fields(
            row,
            &[
                "id",
                "level",
                "title",
                "content",
                "expires_at",
                "created_at",
            ],
        ));
    }
    Ok(json!({"success":true,"announcements":announcements}))
}

fn usage_output(usage: Value) -> Value {
    // Server-authoritative balance includes reservations. Preserve null/missing;
    // never reconstruct it from the display-only usage limit and consumed total.
    let mut safe_usage = select_fields(
        &usage,
        &[
            "virtualPlanName",
            "validUntil",
            "isExpired",
            "availableCredits",
        ],
    );
    if usage.get("usageBreakdownList").is_some() {
        safe_usage["usageBreakdownList"] = select_rows(
            &usage["usageBreakdownList"],
            &[
                "dimensionType",
                "currentUsageWithPrecision",
                "usageLimitWithPrecision",
            ],
        );
    }
    let mut settled = if usage["settledUsage"].is_object() {
        select_fields(
            &usage["settledUsage"],
            &[
                "totalTokens",
                "todayPoints",
                "todayTokens",
                "timezone",
                "windowStart",
                "windowEnd",
            ],
        )
    } else {
        Value::Null
    };
    if settled.is_object() {
        if usage["settledUsage"].get("daily").is_some() {
            settled["daily"] = select_rows(
                &usage["settledUsage"]["daily"],
                &["date", "points", "tokens"],
            );
        }
        if usage["settledUsage"].get("models").is_some() {
            settled["models"] = select_rows(
                &usage["settledUsage"]["models"],
                &["name", "points", "tokens"],
            );
        }
    }
    json!({"success":true,"settledUsage":settled,"usage":safe_usage})
}

fn unbind_gateway(session_gateway: Option<&str>, body: &Value) -> Result<String, String> {
    gateway(session_gateway.or(body["gateway_url"].as_str()))
}

fn confirmed_restore_stop(
    body: &Value,
    pending: bool,
    stop: impl FnOnce() -> Result<(), patch_engine::process::ProcessError>,
) -> Result<(), String> {
    if body.get("close_kiro_confirmed").and_then(Value::as_bool) != Some(true) {
        return Err(
            "Explicit close_kiro_confirmed: true is required to close Kiro and restore".into(),
        );
    }
    if pending {
        stop().map_err(|e| format!("Cannot stop Kiro for restore: {e}"))?;
    }
    Ok(())
}

pub async fn dispatch(host: &Host, path: &str, method: &str, body: Value) -> Result<Value, String> {
    if matches!(
        path.split('?').next(),
        Some("/api/doctor" | "/api/verify-card" | "/api/activate" | "/api/unbind")
    ) {
        gateway_certificates()?;
    }
    let route = path.split('?').next().unwrap_or(path);
    match (method, route) {
        ("GET", "/api/operation") => host.operation_status(),
        ("GET", "/api/announcements") => fetch_announcements(&gateway(None)?).await,
        ("GET", "/api/status") => {
            let custom = host.install_path()?;
            let install = detect_kiro(custom.as_deref()).ok();
            let session = DesktopSession::system().ok();
            let token_readable = patch_engine::default_token_path()
                .ok()
                .is_some_and(|path| patch_engine::TokenStorage::at(path).load().is_ok());
            let gateway = gateway(None)?;
            Ok(
                json!({"process_state":patch_engine::detect_kiro_process_state().to_string(),
                "kiro_installed":install.is_some(),"kiro_install_path":install.as_ref().map(|i| &i.install_dir),
                "kiro_version":install.as_ref().map(|i| &i.version),"has_snapshot":SnapshotManager::default().has_active_snapshot(),
                "recovery_pending":recovery_pending(),"authenticated":token_readable && session.as_ref().is_some_and(|s| s.authenticated()),"gateway_url":session.as_ref().and_then(|s| s.gateway()),"suggested_gateway_url":gateway,
                "portal_url":format!("{gateway}/"),"platform":if cfg!(windows) {"win32"} else if cfg!(target_os="macos") {"darwin"} else {"linux"},
                "model_service_available":null,"tray_available":true,"memory_maintenance":host.maintenance.lock().map_err(|_| "Maintenance state unavailable")?.clone(),"app_version":env!("SUPERKIRO_BUILD_VERSION")}),
            )
        }
        ("GET", "/api/usage") => Ok(usage_output(DesktopSession::system()?.usage().await?)),
        ("GET", "/api/doctor") => {
            let url = reqwest::Url::parse(&format!("http://localhost{path}"))
                .map_err(|_| "Invalid diagnostic request")?;
            let query = url
                .query_pairs()
                .find(|(k, _)| k == "gateway_url")
                .map(|(_, v)| v.into_owned());
            let custom = host.install_path()?;
            let report = patch_engine::Doctor::default()
                .diagnose(&gateway(query.as_deref())?, custom.as_deref())
                .await;
            Ok(
                json!({"overall_status":report.overall_status,"items":report.items.iter().map(|item| json!({"name":item.name,"level":item.level})).collect::<Vec<_>>(),"kiro_version":report.kiro_version,"is_running":report.is_running,"gateway_reachable":report.gateway_reachable,"can_one_click_fix":report.can_one_click_fix}),
            )
        }
        ("GET", "/api/memory/sample") => {
            let sample = MemoryGuard::sample_memory();
            if let Some(error) = sample.error.as_ref() {
                return Err(error.clone());
            }
            serde_json::to_value(sample).map_err(|e| e.to_string())
        }
        ("POST", "/api/heartbeat") => Ok(json!({"status":"alive"})),
        ("POST", "/api/verify-card") => {
            verify_card(
                &gateway(body["gateway_url"].as_str())?,
                body["card_key"].as_str().unwrap_or(""),
            )
            .await
        }
        ("POST", "/api/activate") => {
            if recovery_pending() {
                return Err("Restore pending Kiro recovery state before activating".into());
            }
            let card = body["card_key"].as_str().unwrap_or("");
            validate_card(card)?;
            let custom = host.install_path()?;
            let install = detect_kiro(custom.as_deref()).map_err(|e| e.to_string())?;
            DesktopSession::system()?
                .activate_and_launch(
                    &install,
                    &gateway(body["gateway_url"].as_str())?,
                    card.trim(),
                    body["close_kiro_confirmed"] == true,
                )
                .await?;
            Ok(json!({"success":true}))
        }
        ("POST", "/api/restore") => {
            confirmed_restore_stop(
                &body,
                recovery_pending(),
                patch_engine::process::stop_kiro_for_restore,
            )?;
            DesktopSession::system()?.restore_and_logout(&SnapshotManager::default())?;
            Ok(json!({"success":true}))
        }
        ("POST", "/api/unbind") => {
            let card = body["card_key"].as_str().unwrap_or("");
            validate_card(card)?;
            let session = DesktopSession::system()?;
            let target = unbind_gateway(session.gateway().as_deref(), &body)?;
            confirmed_restore_stop(
                &body,
                recovery_pending(),
                patch_engine::process::stop_kiro_for_restore,
            )?;
            session
                .unbind_with_gateway(&SnapshotManager::default(), card.trim(), &target)
                .await?;
            Ok(json!({"success":true}))
        }
        ("POST", "/api/launch") => {
            let custom = host.install_path()?;
            DesktopSession::system()?
                .launch(&detect_kiro(custom.as_deref()).map_err(|e| e.to_string())?)?;
            Ok(json!({"success":true}))
        }
        ("POST", "/api/memory/trim") => {
            if !cfg!(windows) {
                return Err("Working set trimming requires Windows".into());
            }
            let sample = MemoryGuard::sample_memory();
            if let Some(error) = sample.error {
                return Err(error);
            }
            if sample.processes.is_empty() {
                return Err("No Kiro processes to trim".into());
            }
            let pids: Vec<_> = sample.processes.iter().map(|p| p.pid).collect();
            let result = MemoryGuard::trim_working_set(Some(&pids));
            if let Some(error) = result.error.as_ref() {
                return Err(error.clone());
            }
            serde_json::to_value(result).map_err(|e| e.to_string())
        }
        _ => Err("Unknown API route or method".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restore_without_local_changes_never_stops_official_kiro() {
        confirmed_restore_stop(&json!({"close_kiro_confirmed":true}), false, || {
            panic!("verification-only sessions must not close Kiro")
        })
        .unwrap();
    }
    #[test]
    fn rejects_unsafe_gateways() {
        for value in [
            "http://example.com",
            "https://user:pass@example.com",
            "https://example.com/?secret=x",
            "https://example.com/#x",
        ] {
            assert!(gateway(Some(value)).is_err());
        }
        assert!(gateway(Some("http://127.0.0.1:44040")).is_ok());
    }
    #[test]
    fn validates_card_without_coercion() {
        assert!(validate_card(" ").is_err());
        assert!(validate_card(&"x".repeat(257)).is_err());
        assert!(validate_card("abc").is_ok());
    }
    #[test]
    fn preserves_usage_without_fake_metrics() {
        let source = json!({"settledUsage":{"totalTokens":42},"virtualPlanName":"PRO"});
        let result = usage_output(source.clone());
        assert_eq!(
            result["usage"]["virtualPlanName"],
            source["virtualPlanName"]
        );
        assert!(result["usage"].get("settledUsage").is_none());
        assert_eq!(result["settledUsage"]["totalTokens"], 42);
        assert!(usage_output(json!({}))["settledUsage"].is_null());
    }
    #[test]
    fn rejects_expired_or_incomplete_authorization() {
        let mut value = json!({"success":true,"status":"active","isExpired":false,"remainingPoints":1,"totalPoints":2,"maxDevices":1,"boundDevices":[],"validUntil":200});
        assert!(validate_authorization(&value, 100.0).is_ok());
        assert!(validate_authorization(&value, 201.0).is_err());
        value["remainingPoints"] = json!("1");
        assert!(validate_authorization(&value, 100.0).is_err());
    }
    #[test]
    fn external_urls_are_allowlisted() {
        assert!(external_allowed("https://kiro.dev/downloads/").unwrap());
        assert!(!external_allowed("file:///C:/Windows").unwrap());
        assert!(!external_allowed("https://evil.example").unwrap());
    }
    #[tokio::test]
    async fn unknown_routes_and_wrong_verbs_do_not_mutate() {
        let dir = std::env::temp_dir().join(format!("superkiro-host-test-{}", std::process::id()));
        let host = Host::new(dir.clone()).unwrap();
        for (method, path) in [
            ("GET", "/api/activate"),
            ("POST", "/api/status"),
            ("POST", "/api/unknown"),
        ] {
            assert!(dispatch(&host, path, method, json!({})).await.is_err());
        }
        assert!(!host.preferences.exists());
        drop(host);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn verification_is_read_only_and_returns_authorization() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![];
            let mut buffer = [0u8; 1024];
            loop {
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|part| part == b"\r\n\r\n") {
                    break;
                }
            }
            assert!(String::from_utf8_lossy(&request).starts_with("POST /api/v1/portal/query "));
            let body = json!({"success":true,"status":"unactivated","isExpired":false,"remainingPoints":5,"totalPoints":5,"maxDevices":1,"boundDevices":[],"validUntil":null}).to_string();
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body);
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let result = verify_card(&format!("http://{address}"), "test-card")
            .await
            .unwrap();
        assert_eq!(result["authorization"]["remainingPoints"], 5);
        assert_eq!(result["success"], true);
        server.await.unwrap();
    }
    #[test]
    fn automatic_maintenance_requires_threshold_and_cooldown() {
        assert!(should_auto_trim(true, 2501, 1, None));
        assert!(should_auto_trim(true, 2501, 1, Some(300)));
        assert!(!should_auto_trim(true, 2500, 1, None));
        assert!(!should_auto_trim(true, 9000, 1, Some(299)));
        assert!(!should_auto_trim(true, 9000, 0, None));
        assert!(!should_auto_trim(false, 9000, 1, None));
    }

    #[test]
    fn repeated_install_path_save_replaces_existing_preferences() {
        let root = std::env::temp_dir().join(format!("superkiro-save-test-{}", std::process::id()));
        let host = Host::new(root.clone()).unwrap();
        let first = root.join("first");
        let second = root.join("second");
        host.save_install_path(&first).unwrap();
        assert_eq!(host.install_path().unwrap(), Some(first));
        host.save_install_path(&second).unwrap();
        assert_eq!(host.install_path().unwrap(), Some(second));
        assert!(!host.preferences.with_extension("tmp").exists());
        drop(host);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn usage_drops_unrecognized_and_secret_fields_at_every_level() {
        let result = usage_output(
            json!({"accessToken":"secret", "virtualPlanName":"PRO", "usageBreakdownList":[{"dimensionType":"CREDIT","currentUsageWithPrecision":2,"usageLimitWithPrecision":10,"token":"secret"}],"settledUsage":{"totalTokens":12,"internal":"secret","daily":[{"date":"2026-09-19","points":1,"tokens":12,"credential":"secret"}],"models":[{"name":"model","tokens":12,"points":1,"apiKey":"secret"}]}}),
        );
        assert!(!result.to_string().contains("secret"));
        assert_eq!(result["settledUsage"]["daily"][0]["tokens"], 12);
        assert_eq!(
            result["usage"]["usageBreakdownList"][0]["currentUsageWithPrecision"],
            2
        );
    }
    #[test]
    fn usage_preserves_server_available_credits_including_null_and_zero() {
        // Limit 100 - used 20 would overstate spendable credit by reserved 30.
        for available in [json!(50), json!(0), Value::Null] {
            let result = usage_output(json!({
                "availableCredits": available,
                "usageBreakdownList": [{
                    "dimensionType": "CREDIT",
                    "currentUsageWithPrecision": 20,
                    "usageLimitWithPrecision": 100
                }]
            }));
            assert_eq!(result["usage"]["availableCredits"], available);
        }
        assert!(usage_output(json!({}))["usage"]
            .get("availableCredits")
            .is_none());
    }
    #[test]
    fn usage_preserves_aggregation_timezone_and_window() {
        let result = usage_output(
            json!({"settledUsage":{"timezone":"UTC","windowStart":100,"windowEnd":200,"internalToken":"secret"}}),
        );
        assert_eq!(result["settledUsage"]["timezone"], "UTC");
        assert_eq!(result["settledUsage"]["windowStart"], 100);
        assert_eq!(result["settledUsage"]["windowEnd"], 200);
        assert!(!result.to_string().contains("secret"));
    }
}

#[cfg(test)]
mod local_preflight {
    #[test]
    #[ignore = "Explicit local read-only Kiro installation check"]
    fn installed_kiro_connection_preflight() {
        let installation = patch_engine::detect_kiro(None).expect("installation detection");
        let extension = installation
            .agent_extension_dir
            .as_ref()
            .expect("agent extension");
        let patcher =
            patch_engine::ExtensionPatcher::new(extension.join("dist").join("extension.js"));
        patch_engine::SnapshotManager::default()
            .validate_takeover(
                &patch_engine::SettingsManager::default(),
                Some(&patcher),
                "https://kiro.rent",
            )
            .expect("read-only patch/settings preflight");
        let _command =
            patch_engine::process::prepare_kiro_launch(&installation, "https://kiro.rent", &[])
                .expect("launch preparation without spawning");
    }
}

#[cfg(test)]
mod operation_tests {
    use super::*;

    #[tokio::test]
    async fn heartbeat_dispatch_remains_alive_while_operation_lock_is_held() {
        let root = std::env::temp_dir().join(format!(
            "host-heartbeat-lock-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let host = Host::new(root.clone()).unwrap();
        let guard = host.operation.lock().await;
        host.begin_operation("/api/activate").unwrap();
        let before = host.operation_status().unwrap();
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            dispatch(&host, "/api/heartbeat", "POST", Value::Null),
        )
        .await
        .expect("heartbeat must not wait for the operation lock")
        .unwrap();
        assert_eq!(result, json!({"status":"alive"}));
        assert_eq!(host.operation_status().unwrap(), before);
        assert!(host.operation.try_lock().is_err());
        drop(guard);
        drop(host);
        std::fs::remove_dir(root).unwrap();
    }

    #[tokio::test]
    async fn timeout_keeps_writer_exclusive_but_status_queryable_then_allows_recovery() {
        let root =
            std::env::temp_dir().join(format!("host-operation-fixture-{}", std::process::id()));
        let host = std::sync::Arc::new(Host::new(root.clone()).unwrap());
        for fails in [false, true] {
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
            let worker = host.clone();
            let before = host.operation_status().unwrap()["id"].as_u64().unwrap();
            let mut task = tokio::spawn(async move {
                run_operation(&worker, "/api/activate", true, async move {
                    started_tx.send(()).unwrap();
                    finish_rx.await.unwrap();
                    if fails {
                        Err("synthetic failure".into())
                    } else {
                        Ok(json!({"success":true}))
                    }
                })
                .await
            });
            started_rx.await.unwrap();
            assert!(tokio::time::timeout(Duration::from_millis(10), &mut task)
                .await
                .is_err());
            let status = tokio::time::timeout(
                Duration::from_millis(100),
                dispatch(&host, "/api/operation", "GET", Value::Null),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(status["state"], "running");
            assert_eq!(status["id"], before + 1);
            assert!(run_operation(&host, "/api/restore", true, async {
                panic!("overlapping writer ran")
            })
            .await
            .is_err());
            finish_tx.send(()).unwrap();
            assert_eq!(task.await.unwrap().is_err(), fails);
            assert_eq!(
                host.operation_status().unwrap()["state"],
                if fails { "failed" } else { "succeeded" }
            );
            let terminal = host.operation_status().unwrap();
            run_operation(&host, "/api/heartbeat", false, async { Ok(Value::Null) })
                .await
                .unwrap();
            assert_eq!(host.operation_status().unwrap(), terminal);
            run_operation(&host, "/api/restore", true, async {
                Ok(json!({"success":true}))
            })
            .await
            .unwrap();
            assert_eq!(host.operation_status().unwrap()["id"], before + 2);
        }
        drop(host);
        std::fs::remove_file(root.join("last-operation.json")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    #[test]
    fn operation_diagnostics_are_redacted_and_survive_restart() {
        let root = std::env::temp_dir().join(format!("host-diagnostics-{}", std::process::id()));
        let host = Host::new(root.clone()).unwrap();
        host.begin_operation("/api/activate?secret-card").unwrap();
        host.finish_operation(&Err("[connection:authenticate] Authentication rejected: 403 - secret-card https://private.invalid".into()));
        let saved = std::fs::read_to_string(root.join("last-operation.json")).unwrap();
        assert!(!saved.contains("secret-card"));
        assert!(!saved.contains("private.invalid"));
        drop(host);
        let host = Host::new(root.clone()).unwrap();
        let state = host.operation_status().unwrap();
        assert_eq!(state["stage"], "authenticate");
        assert_eq!(state["code"], "auth-rejected");
        assert_eq!(state["http_status"], 403);
        assert_eq!(state["state"], "failed");
        assert_eq!(
            operation_error(Some("[connection:apply] permission denied /secret"))["code"],
            "permission"
        );
        assert_eq!(
            operation_error(Some("[connection:authenticate] TLS certificate invalid"))["code"],
            "tls"
        );
        assert_eq!(
            operation_error(Some("network timed out"))["code"],
            "timeout"
        );
        assert_eq!(
            operation_error(Some("sensitive unknown error"))["stage"],
            "unknown"
        );
        drop(host);
        std::fs::write(
            root.join("last-operation.json"),
            r#"{"state":"failed","stage":"secret","code":"secret","http_status":999}"#,
        )
        .unwrap();
        let clean = load_operation(&root);
        assert_eq!(clean["stage"], "unknown");
        assert_eq!(clean["code"], "unknown");
        assert!(clean["http_status"].is_null());
        std::fs::remove_file(root.join("last-operation.json")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    #[test]
    fn usage_rows_are_bounded_without_changing_totals() {
        let rows: Vec<_> = (0..1000)
            .map(|i| json!({"date":i,"name":i,"tokens":1}))
            .collect();
        let result =
            usage_output(json!({"settledUsage":{"totalTokens":1000,"daily":rows,"models":rows}}));
        assert_eq!(result["settledUsage"]["totalTokens"], 1000);
        assert_eq!(
            result["settledUsage"]["daily"].as_array().unwrap().len(),
            90
        );
        assert_eq!(
            result["settledUsage"]["models"].as_array().unwrap().len(),
            90
        );
    }
}

#[cfg(test)]
mod announcement_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn row() -> Value {
        json!({"id":"ann-1","level":"warning","title":"Title","content":"Content","expires_at":null,"created_at":42})
    }
    fn envelope(row: Value) -> Value {
        json!({"success":true,"announcements":[row]})
    }

    #[test]
    fn display_fields_levels_and_expiry_boundary() {
        for level in ["info", "warning", "critical"] {
            let mut item = row();
            item["level"] = json!(level);
            let expected = item.clone();
            item["private"] = json!("must-not-leak");
            assert_eq!(
                announcements_output(envelope(item), 100).unwrap(),
                envelope(expected)
            );
        }
        for expires in [99, 100, 101] {
            let mut item = row();
            item["expires_at"] = json!(expires);
            let result = announcements_output(envelope(item), 100).unwrap();
            assert_eq!(
                result["announcements"].as_array().unwrap().len(),
                usize::from(expires > 100)
            );
        }
        assert_eq!(
            announcements_output(json!({"success":true,"announcements":[]}), 0).unwrap()
                ["announcements"],
            json!([])
        );
    }

    #[test]
    fn timestamps_are_preserved_and_legacy_expiry_is_not_a_fallback() {
        for created_at in [0, 42, u64::MAX] {
            let mut item = row();
            item["created_at"] = json!(created_at);
            item["expires_at"] = json!(u64::MAX);
            let expected = item.clone();
            item["expires"] = json!("legacy-field-must-not-leak");
            assert_eq!(
                announcements_output(envelope(item), 100).unwrap(),
                envelope(expected)
            );
        }
        let mut item = row();
        item.as_object_mut().unwrap().remove("expires_at");
        item["expires"] = Value::Null;
        assert!(announcements_output(envelope(item), 0).is_err());
    }

    #[test]
    fn rejects_invalid_types_missing_fields_and_lengths() {
        for key in [
            "id",
            "title",
            "content",
            "level",
            "expires_at",
            "created_at",
        ] {
            let mut item = row();
            item.as_object_mut().unwrap().remove(key);
            assert!(announcements_output(envelope(item), 0).is_err());
        }
        for (key, bad) in [
            ("id", json!(1)),
            ("title", Value::Null),
            ("content", json!([])),
            ("level", json!("urgent")),
            ("expires_at", json!("123")),
            ("expires_at", json!(-1)),
            ("expires_at", json!(1.5)),
            ("expires_at", json!(true)),
            ("created_at", Value::Null),
            ("created_at", json!("123")),
            ("created_at", json!(-1)),
            ("created_at", json!(1.5)),
            ("created_at", json!(true)),
        ] {
            let mut item = row();
            item[key] = bad;
            assert!(announcements_output(envelope(item), 0).is_err());
        }
        for (key, max) in [("id", 256), ("title", 256), ("content", 20_000)] {
            for text in [String::new(), "  ".into(), "文".repeat(max + 1)] {
                let mut item = row();
                item[key] = json!(text);
                assert!(announcements_output(envelope(item), 0).is_err());
            }
            let mut item = row();
            item[key] = json!("文".repeat(max));
            assert!(announcements_output(envelope(item), 0).is_ok());
        }
        for value in [
            Value::Null,
            json!({}),
            json!({"success":"true","announcements":[]}),
            json!({"success":false,"announcements":[]}),
            json!({"success":true,"announcements":{}}),
        ] {
            assert!(announcements_output(value, 0).is_err());
        }
    }

    async fn fetch_wire(wire: Vec<u8>) -> (Result<Value, String>, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 1024];
                let n = socket.read(&mut bytes).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&bytes[..n]);
                if request.windows(4).any(|v| v == b"\r\n\r\n") {
                    break;
                }
            }
            let _ = socket.write_all(&wire).await;
            String::from_utf8(request).unwrap()
        });
        let result = fetch_announcements(&format!("http://{address}")).await;
        (result, server.await.unwrap())
    }

    #[tokio::test]
    async fn fetches_real_endpoint_without_credentials() {
        let body = envelope(row()).to_string();
        let (result, request) = fetch_wire(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .into_bytes(),
        )
        .await;
        assert_eq!(result.unwrap(), envelope(row()));
        assert!(request.starts_with("GET /api/v1/announcements HTTP/1.1\r\n"));
        assert!(!request.to_lowercase().contains("authorization:"));
        assert!(!request.to_lowercase().contains("cookie:"));
    }

    #[tokio::test]
    async fn rejects_redirects_errors_invalid_json_and_oversized_bodies() {
        for (wire, expected) in [
            ("HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/secret\r\nContent-Length: 0\r\n\r\n", "Announcements HTTP 302"),
            ("HTTP/1.1 403 Forbidden\r\nContent-Length: 6\r\n\r\nsecret", "Announcements HTTP 403"),
            ("HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nsecret", "Invalid announcements response"),
            ("HTTP/1.1 200 OK\r\nContent-Length: 1048577\r\n\r\n", "Announcements response too large"),
        ] {
            assert_eq!(fetch_wire(wire.as_bytes().to_vec()).await.0.unwrap_err(), expected);
        }
        let body = "x".repeat(1024 * 1024 + 1);
        let wire = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n",
            body.len(),
            body
        );
        assert_eq!(
            fetch_wire(wire.into_bytes()).await.0.unwrap_err(),
            "Announcements response too large"
        );
    }
}

#[cfg(test)]
mod restore_confirmation_tests {
    use super::*;

    #[test]
    fn only_explicit_boolean_confirmation_can_stop_kiro() {
        for body in [
            Value::Null,
            json!({}),
            json!({"close_kiro_confirmed":false}),
            json!({"close_kiro_confirmed":"true"}),
            json!({"close_kiro_confirmed":1}),
            json!({"close_kiro_confirmed":null}),
        ] {
            assert!(confirmed_restore_stop(&body, true, || panic!(
                "must not stop without consent"
            ))
            .is_err());
        }
        let called = std::cell::Cell::new(false);
        confirmed_restore_stop(&json!({"close_kiro_confirmed":true}), true, || {
            called.set(true);
            Ok(())
        })
        .unwrap();
        assert!(called.get());
    }

    #[test]
    fn failed_stop_prevents_restore() {
        let result = confirmed_restore_stop(&json!({"close_kiro_confirmed":true}), true, || {
            Err(patch_engine::process::ProcessError::UnknownState)
        })
        .map(|_| -> Result<(), String> { panic!("must not restore after failed stop") });
        assert!(result.unwrap_err().contains("Cannot stop Kiro for restore"));
    }

    #[tokio::test]
    async fn both_routes_reject_unconfirmed_requests_before_system_side_effects() {
        let root =
            std::env::temp_dir().join(format!("host-restore-consent-{}", std::process::id()));
        let host = Host::new(root.clone()).unwrap();
        for route in ["/api/restore", "/api/unbind"] {
            for confirmation in [Value::Null, json!(false), json!("true"), json!(1)] {
                let error = dispatch(
                    &host,
                    route,
                    "POST",
                    json!({
                        "card_key":"TEST-CARD-12345678", "close_kiro_confirmed":confirmation
                    }),
                )
                .await
                .unwrap_err();
                assert!(error.contains("close_kiro_confirmed"), "{route}: {error}");
            }
        }
        drop(host);
        std::fs::remove_dir(root).unwrap();
    }
}

#[cfg(test)]
mod client_contract_tests {
    use super::*;
    #[test]
    fn unbind_gateway_honors_explicit_target_and_saved_session() {
        assert_eq!(
            unbind_gateway(None, &json!({"gateway_url":"https://a.example"})).unwrap(),
            "https://a.example"
        );
        for supplied in [
            "https://b.example",
            "http://unsafe.example",
            "https://user:secret@a.example",
        ] {
            assert_eq!(
                unbind_gateway(Some("https://a.example"), &json!({"gateway_url":supplied}))
                    .unwrap(),
                "https://a.example"
            );
        }
        for supplied in [
            "http://unsafe.example",
            "https://user:secret@a.example",
            "https://a.example?secret=1",
        ] {
            assert!(unbind_gateway(None, &json!({"gateway_url":supplied})).is_err());
        }
    }
}
