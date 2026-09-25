//! Each customer request and the upstream model's reply, kept for 24 hours so a problem
//! can be traced back to what was sent and what came back.
//!
//! Stored encrypted under the master key (for this purpose only), compressed and
//! size-bounded; deleted after 24 hours, and the oldest first once the archive is full.
//! The deploy tool leaves this folder out of every copy it makes of the data, so none of
//! it outlives its 24 hours in a backup. Administrators read one request at a time, and
//! each read is logged without its content.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};

/// How long a request and its reply are kept.
pub const RETENTION_SECS: u64 = 24 * 3600;
/// The largest request kept, as JSON before compression; the oldest history goes first.
const REQUEST_LIMIT: usize = 4 * 1024 * 1024;
/// The most of each part of a reply (its text, its reasoning, each call's arguments) kept.
pub const REPLY_PART_LIMIT: usize = 1024 * 1024;
/// The most the archive takes on disk; beyond it the oldest requests go first.
const TOTAL_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
/// A string longer than this under a `bytes` key is image data, not text.
const IMAGE_BYTES_MIN: usize = 1024;
/// What the master key seals here, and nothing else.
const PURPOSE: &[u8] = b"superkiro-request-archive/1";

static ARCHIVE: OnceLock<RequestArchive> = OnceLock::new();

/// Makes `archive` the one this process keeps requests in. Only the first call counts.
pub fn install(archive: RequestArchive) {
    let _ = ARCHIVE.set(archive);
}

/// The archive this process keeps requests in, when it keeps any.
pub fn active() -> Option<&'static RequestArchive> {
    ARCHIVE.get()
}

pub struct RequestArchive {
    dir: PathBuf,
    kek: billing::MasterKek,
    total_limit: u64,
    /// Requests kept by this process whose reply is still to come: key -> when received.
    open: Mutex<HashMap<String, u64>>,
}

/// What came back for a request.
#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchivedReply {
    /// `success`, `client_aborted` or `error`.
    pub status: String,
    pub error: Option<String>,
    pub provider_id: String,
    pub target_model: String,
    pub stop_reason: Option<String>,
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<ArchivedToolCall>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub ttft_ms: Option<u32>,
    pub tokens_per_second: Option<f64>,
    /// Some of the reply was longer than is kept.
    pub truncated: bool,
}

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchivedToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Appends `text` to `part`, up to what is kept of a reply part. True when some was cut.
pub fn append_capped(part: &mut String, text: &str) -> bool {
    let room = REPLY_PART_LIMIT.saturating_sub(part.len());
    if text.len() <= room {
        part.push_str(text);
        return false;
    }
    let mut cut = room;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    part.push_str(&text[..cut]);
    true
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// A file-name key for a request: no card or request id on disk, only a digest of them.
fn key_of(invocation_id: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, invocation_id.as_bytes());
    digest.as_ref()[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// When a file's request arrived, from the name it was written under.
fn received_at_of(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    let (stamp, _) = name.split_once('-')?;
    if stamp.len() != 10 {
        return None;
    }
    stamp.parse().ok()
}

impl RequestArchive {
    pub fn open(dir: impl Into<PathBuf>, kek: billing::MasterKek) -> std::io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            dir,
            kek,
            total_limit: TOTAL_LIMIT,
            open: Mutex::new(HashMap::new()),
        })
    }

    #[cfg(test)]
    fn with_total_limit(mut self, limit: u64) -> Self {
        self.total_limit = limit;
        self
    }

    fn path(&self, received_at: u64, key: &str, kind: &str) -> PathBuf {
        self.dir.join(format!("{received_at:010}-{key}.{kind}"))
    }

    fn write(&self, path: &Path, record: &Value) -> Result<(), String> {
        let json = serde_json::to_vec(record).map_err(|e| e.to_string())?;
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&json).map_err(|e| e.to_string())?;
        let packed = encoder.finish().map_err(|e| e.to_string())?;
        let sealed = self
            .kek
            .seal_bytes(PURPOSE, &packed)
            .map_err(|e| e.to_string())?;
        let mut temporary = path.as_os_str().to_owned();
        temporary.push(".tmp");
        let temporary = PathBuf::from(temporary);
        fs::write(&temporary, sealed).map_err(|e| e.to_string())?;
        fs::rename(&temporary, path).map_err(|e| e.to_string())
    }

    fn read_file(&self, path: &Path) -> Option<Value> {
        let sealed = fs::read(path).ok()?;
        let packed = self.kek.open_bytes(PURPOSE, &sealed).ok()?;
        let mut json = Vec::new();
        flate2::read::DeflateDecoder::new(&packed[..])
            .take(64 * 1024 * 1024)
            .read_to_end(&mut json)
            .ok()?;
        serde_json::from_slice(&json).ok()
    }

    /// Keeps a customer's request as it arrived, images reduced to their size. The write
    /// runs off the request's thread; the reply finds its request by the time it ends.
    pub fn keep_request(
        &'static self,
        invocation_id: &str,
        card_id: &str,
        model: &str,
        body: bytes::Bytes,
    ) {
        let received_at = now_secs();
        let key = key_of(invocation_id);
        self.open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone(), received_at);
        let record = json!({
            "invocationId": invocation_id,
            "cardId": card_id,
            "model": model,
            "receivedAt": received_at,
        });
        let path = self.path(received_at, &key, "req");
        tokio::task::spawn_blocking(move || {
            let (request, notes) = prepare_request(&body);
            let mut record = record;
            record["request"] = request;
            record["notes"] = notes;
            if let Err(error) = self.write(&path, &record) {
                eprintln!("[archive] request not kept: {error}");
            }
        });
    }

    /// Keeps what came back for a request this process kept; nothing for any other.
    pub fn keep_reply(&'static self, invocation_id: &str, reply: ArchivedReply) {
        let key = key_of(invocation_id);
        let Some(received_at) = self
            .open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key)
        else {
            return;
        };
        let path = self.path(received_at, &key, "res");
        tokio::task::spawn_blocking(move || {
            let record = json!({"completedAt": now_secs(), "reply": reply});
            if let Err(error) = self.write(&path, &record) {
                eprintln!("[archive] reply not kept: {error}");
            }
        });
    }

    /// A request and its reply, while they are kept.
    pub fn read(&self, invocation_id: &str, now: u64) -> Option<Value> {
        let key = key_of(invocation_id);
        let suffix = format!("-{key}.req");
        let request_path = fs::read_dir(&self.dir)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(&suffix))
            })?;
        let received_at = received_at_of(&request_path)?;
        if now >= received_at.saturating_add(RETENTION_SECS) {
            return None;
        }
        let mut record = self.read_file(&request_path)?;
        // Another request whose digest shares these bytes is not this one.
        if record["invocationId"] != invocation_id {
            return None;
        }
        record["reply"] = self
            .read_file(&request_path.with_extension("res"))
            .map_or(Value::Null, |value| value["reply"].clone());
        record["expiresAt"] = json!(received_at + RETENTION_SECS);
        Some(record)
    }

    /// Deletes what is 24 hours old, then the oldest until the archive fits its limit.
    pub fn prune(&self, now: u64) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let mut kept = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(received_at) = received_at_of(&path) else {
                continue;
            };
            if now >= received_at.saturating_add(RETENTION_SECS) {
                let _ = fs::remove_file(&path);
                continue;
            }
            let size = entry.metadata().map_or(0, |metadata| metadata.len());
            kept.push((received_at, path, size));
        }
        let mut total: u64 = kept.iter().map(|(_, _, size)| size).sum();
        kept.sort();
        for (_, path, size) in kept {
            if total <= self.total_limit {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(size);
            }
        }
        self.open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, received_at| now < received_at.saturating_add(RETENTION_SECS));
    }
}

/// The request to keep, and what was left out of it.
fn prepare_request(body: &[u8]) -> (Value, Value) {
    let Ok(mut request) = serde_json::from_slice::<Value>(body) else {
        let preview = String::from_utf8_lossy(&body[..body.len().min(64 * 1024)]).into_owned();
        return (json!({ "unparsed": preview }), json!({ "unparsed": true }));
    };
    let omitted_images = strip_images(&mut request);
    let mut omitted_history = 0usize;
    let mut size = serde_json::to_vec(&request).map_or(0, |json| json.len());
    if size > REQUEST_LIMIT {
        if let Some(history) = request
            .pointer_mut("/conversationState/history")
            .and_then(Value::as_array_mut)
        {
            let sizes: Vec<usize> = history
                .iter()
                .map(|entry| serde_json::to_vec(entry).map_or(0, |json| json.len() + 1))
                .collect();
            while size > REQUEST_LIMIT && omitted_history < sizes.len() {
                size = size.saturating_sub(sizes[omitted_history]);
                omitted_history += 1;
            }
            history.drain(..omitted_history);
        }
    }
    let truncated = size > REQUEST_LIMIT;
    if truncated {
        request = json!({ "tooLarge": true });
    }
    (
        request,
        json!({
            "omittedImages": omitted_images,
            "omittedHistoryEntries": omitted_history,
            "truncated": truncated,
        }),
    )
}

/// Replaces image data with its size; returns how many images were reduced.
fn strip_images(value: &mut Value) -> usize {
    match value {
        Value::Object(map) => {
            let mut count = 0;
            for (key, item) in map.iter_mut() {
                match item {
                    Value::String(text) if key == "bytes" && text.len() > IMAGE_BYTES_MIN => {
                        *item = json!(format!("[图片已省略：{} KB]", text.len() * 3 / 4 / 1024));
                        count += 1;
                    }
                    other => count += strip_images(other),
                }
            }
            count
        }
        Value::Array(items) => items.iter_mut().map(strip_images).sum(),
        _ => 0,
    }
}

/// Decodes `%XX` escapes in a query value (a request id's `:` arrives as `%3A`).
pub fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Like `key_of`, for log lines that name a request without naming its card.
pub fn log_key(invocation_id: &str) -> String {
    key_of(invocation_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("archive-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn archive(name: &str) -> RequestArchive {
        RequestArchive::open(
            scratch(name),
            billing::MasterKek::generate_random().unwrap(),
        )
        .unwrap()
    }

    fn request_body(history: usize, filler: usize) -> Vec<u8> {
        let image = "A".repeat(40_000);
        serde_json::to_vec(&json!({
            "conversationState": {
                "currentMessage": {"userInputMessage": {
                    "content": "请修复登录按钮的点击问题",
                    "images": [{"format": "png", "source": {"bytes": image}}],
                }},
                "history": (0..history).map(|i| json!({
                    "userInputMessage": {"content": format!("第 {i} 轮：{}", "x".repeat(filler))}
                })).collect::<Vec<_>>(),
            }
        }))
        .unwrap()
    }

    fn keep_now(archive: &RequestArchive, invocation: &str, body: &[u8], at: u64) {
        let key = key_of(invocation);
        let (request, notes) = prepare_request(body);
        let record = json!({"invocationId": invocation, "cardId": "card-1", "model": "claude-opus-5",
            "receivedAt": at, "request": request, "notes": notes});
        archive
            .write(&archive.path(at, &key, "req"), &record)
            .unwrap();
    }

    #[test]
    fn a_request_and_its_reply_read_back_images_reduced_and_nothing_readable_on_disk() {
        let archive = archive("roundtrip");
        let now = now_secs();
        keep_now(&archive, "card-1:inv-1", &request_body(3, 10), now);
        let key = key_of("card-1:inv-1");
        let reply = ArchivedReply {
            status: "success".into(),
            text: "已修复：给按钮加上了 onClick".into(),
            tool_calls: vec![ArchivedToolCall {
                id: "t1".into(),
                name: "fsWrite".into(),
                arguments: "{\"path\":\"a.ts\"}".into(),
            }],
            output_tokens: 42,
            ttft_ms: Some(812),
            ..Default::default()
        };
        archive
            .write(
                &archive.path(now, &key, "res"),
                &json!({"completedAt": now, "reply": reply}),
            )
            .unwrap();

        let record = archive.read("card-1:inv-1", now + 60).unwrap();
        assert_eq!(record["model"], "claude-opus-5");
        assert_eq!(
            record["request"]["conversationState"]["currentMessage"]["userInputMessage"]["content"],
            "请修复登录按钮的点击问题"
        );
        let image = &record["request"]["conversationState"]["currentMessage"]["userInputMessage"]
            ["images"][0]["source"]["bytes"];
        assert_eq!(image, "[图片已省略：29 KB]");
        assert_eq!(record["notes"]["omittedImages"], 1);
        assert_eq!(record["reply"]["text"], "已修复：给按钮加上了 onClick");
        assert_eq!(record["reply"]["toolCalls"][0]["name"], "fsWrite");
        assert_eq!(record["reply"]["ttftMs"], 812);
        assert_eq!(record["expiresAt"], now + RETENTION_SECS);
        // On disk: no card id, no request id, no content in the clear.
        for entry in fs::read_dir(&archive.dir).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(
                !name.contains("card-1") && !name.contains("inv-1"),
                "{name}"
            );
            let bytes = fs::read(entry.path()).unwrap();
            let clear = String::from_utf8_lossy(&bytes);
            assert!(
                !clear.contains("登录") && !clear.contains("onClick") && !clear.contains("card-1")
            );
        }
        // Another request is not found, nor this one read as another's.
        assert!(archive.read("card-1:inv-2", now).is_none());
        fs::remove_dir_all(&archive.dir).unwrap();
    }

    #[test]
    fn requests_go_after_24_hours_and_the_oldest_go_first_when_full() {
        let archive = archive("prune").with_total_limit(1);
        let now = now_secs();
        keep_now(
            &archive,
            "card:old",
            &request_body(1, 10),
            now - RETENTION_SECS,
        );
        keep_now(&archive, "card:a", &request_body(1, 10), now - 20);
        keep_now(&archive, "card:b", &request_body(1, 10), now - 10);
        // A day old is gone even before the archive is pruned.
        assert!(archive.read("card:old", now).is_none());
        assert!(archive.read("card:a", now).is_some());
        let size_of = |invocation: &str| {
            fs::read_dir(&archive.dir)
                .unwrap()
                .flatten()
                .find(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .contains(&key_of(invocation))
                })
                .map(|e| e.metadata().unwrap().len())
                .unwrap()
        };
        let newest = size_of("card:b");
        let archive = archive.with_total_limit(newest);
        archive.prune(now);
        assert!(archive.read("card:old", now).is_none());
        assert!(
            archive.read("card:a", now).is_none(),
            "the oldest goes first"
        );
        assert!(archive.read("card:b", now).is_some());
        let names: Vec<String> = fs::read_dir(&archive.dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        fs::remove_dir_all(&archive.dir).unwrap();
    }

    #[test]
    fn a_huge_request_keeps_its_latest_history_within_the_limit() {
        // Forty turns of 200 KB: 8 MB, twice what is kept.
        let body = request_body(40, 200_000);
        let (request, notes) = prepare_request(&body);
        let kept = serde_json::to_vec(&request).unwrap().len();
        assert!(kept <= REQUEST_LIMIT, "{kept}");
        let omitted = notes["omittedHistoryEntries"].as_u64().unwrap();
        assert!(omitted > 0 && omitted < 40, "{omitted}");
        let history = request["conversationState"]["history"].as_array().unwrap();
        assert_eq!(history.len() as u64, 40 - omitted);
        // The latest turns stay, the oldest go.
        assert!(history.last().unwrap()["userInputMessage"]["content"]
            .as_str()
            .unwrap()
            .starts_with("第 39 轮"));
        assert_eq!(notes["truncated"], false);
        // Not JSON at all: a readable preview, marked as such.
        let (unparsed, notes) = prepare_request(b"not json");
        assert_eq!(unparsed["unparsed"], "not json");
        assert_eq!(notes["unparsed"], true);
    }

    #[test]
    fn reply_parts_are_capped_on_a_character_boundary() {
        let mut part = "a".repeat(REPLY_PART_LIMIT - 1);
        assert!(append_capped(&mut part, "好"));
        assert_eq!(part.len(), REPLY_PART_LIMIT - 1);
        let mut part = String::new();
        assert!(!append_capped(&mut part, "fine"));
        assert_eq!(part, "fine");
    }

    #[test]
    fn query_values_are_decoded() {
        assert_eq!(
            percent_decode("card-1%3Ainv-9").as_deref(),
            Some("card-1:inv-9")
        );
        assert_eq!(percent_decode("plain").as_deref(), Some("plain"));
        assert!(percent_decode("bad%zz").is_none());
        assert!(percent_decode("cut%3").is_none());
    }
}
