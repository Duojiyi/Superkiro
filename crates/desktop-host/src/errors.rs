use serde_json::{json, Value};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const STAGES: &[&str] = &[
    "unknown",
    "preflight",
    "launch-prepare",
    "authenticate",
    "close",
    "apply",
    "launch",
    "restore",
    "unbind",
    "native",
];
const CODES: &[&str] = &[
    "SK-KIRO-001",
    "SK-AUTH-001",
    "SK-AUTH-002",
    "SK-AUTH-003",
    "SK-BIND-001",
    "SK-BIND-002",
    "SK-BIND-003",
    "SK-AUTH-004",
    "SK-NET-001",
    "SK-NET-002",
    "SK-NET-003",
    "SK-CONNECT-001",
    "SK-CONNECT-002",
    "SK-CONNECT-003",
    "SK-CONNECT-004",
    "SK-CONNECT-005",
    "SK-CONNECT-006",
    "SK-CONNECT-007",
    "SK-CONNECT-008",
    "SK-RESTORE-001",
    "SK-RESTORE-002",
    "SK-RESTORE-003",
    "SK-BIND-004",
    "SK-LOCAL-001",
    "SK-LOCAL-002",
    "SK-UNKNOWN-001",
    "SK-LOCAL-003",
];
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn classify(raw: &str, path: &str, method: &str) -> Value {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let lower = raw.to_ascii_lowercase();
    let stage = STAGES
        .iter()
        .copied()
        .find(|s| raw.starts_with(&format!("[connection:{s}]")))
        .unwrap_or(match path.split('?').next().unwrap_or("") {
            "native" => "native",
            "/api/activate" => "preflight",
            "/api/restore" => "restore",
            "/api/unbind" => "unbind",
            "/api/launch" => "launch",
            _ => "unknown",
        });
    let auth = raw.split("[auth:").nth(1).and_then(|s| s.split(']').next());
    let code =
        if lower.contains("remote unbind succeeded") && lower.contains("local cleanup failed") {
            "SK-BIND-004"
        } else if lower.contains("operation in progress") {
            "SK-LOCAL-002"
        } else if lower.contains("reinstall kiro") {
            // Kiro's bundle is still modified and nothing is left to restore it from:
            // retrying cannot help, reinstalling Kiro replaces the file.
            "SK-RESTORE-002"
        } else if lower.contains("settings.json has a syntax error") {
            // Kiro's settings.json has a typo no edit can safely read past. Nothing was
            // changed; the customer fixes that line and retries.
            "SK-RESTORE-003"
        } else if lower.contains("kiro") && lower.contains("is unsupported; upgrade to") {
            "SK-KIRO-001"
        } else if let Some(code) = match auth {
            Some("invalid-card") => Some("SK-AUTH-001"),
            Some("expired") => Some("SK-AUTH-002"),
            Some("access-denied") => Some("SK-AUTH-003"),
            Some("device-binding") => Some("SK-BIND-001"),
            Some("rebind-cooldown") => Some("SK-BIND-002"),
            Some("rebind-limit") => Some("SK-BIND-003"),
            Some("throttled" | "locked-out") => Some("SK-AUTH-004"),
            _ => None,
        } {
            code
        } else if path == "native"
            && matches!(
                method,
                "get_remembered_card"
                    | "set_remembered_card"
                    | "clear_remembered_card"
                    | "credential_get"
                    | "credential_set"
                    | "credential_delete"
            )
            && !lower.contains("unauthorized webview")
        {
            "SK-LOCAL-003"
        } else if lower.contains("invalid card") {
            "SK-AUTH-001"
        } else if lower.contains("invalid or expired") {
            "SK-UNKNOWN-001"
        } else if lower.contains("expired") {
            "SK-AUTH-002"
        // An editor that will not close is neither a network timeout nor an
        // uncertain write: nothing was modified, and the user can act on it by
        // saving and quitting Kiro. Left to the generic timeout rule below it
        // became SK-NET-001, whose POST outcome is "unknown" — which wedges the
        // client into "last write unconfirmed" and refuses every later retry.
        //
        // This must cover every producer, not just the restore route: activate
        // reports "[connection:close] Timed out ..." (desktop.rs), restore and
        // unbind report "Cannot stop Kiro for restore: ..." (backend.rs), and
        // unbind's own precondition reports "Kiro IDE is currently running ...".
        // Kiro would not close and still has a window on screen, most likely its save
        // prompt. Distinct from other close failures because it is the one case where
        // ending Kiro, with the user's explicit say-so, can help.
        } else if lower.contains("profile other than default") {
            // Only the Default profile is configured; the user must switch windows to it.
            "SK-CONNECT-007"
        } else if (lower.contains("kiro update is waiting to install")
            || lower.contains("installation changed after it was checked"))
            && !lower.contains("recovery record retained")
        {
            // Kiro is updating itself, or just did, under the takeover. Nothing was left
            // changed; letting the update finish and trying again is all it takes.
            "SK-CONNECT-008"
        } else if lower.contains("another windows session") {
            // The same user's Kiro in another session: this client can neither ask it to
            // close nor end it, so neither saving here nor forcing helps.
            "SK-CONNECT-006"
        } else if lower.contains("kiro is still open") {
            "SK-CONNECT-005"
        } else if stage == "close"
            || lower.contains("cannot stop kiro")
            || lower.contains("kiro ide is currently running")
        {
            "SK-CONNECT-002"
        } else if lower.contains("timeout") || lower.contains("timed out") {
            "SK-NET-001"
        } else if lower.contains("certificate") || lower.contains("tls") {
            "SK-NET-003"
        } else if lower.contains("permission")
            || lower.contains("access denied")
            || lower.contains("os error 5")
        {
            "SK-LOCAL-001"
        } else if lower.contains("authentication rejected: 429") || lower.contains("http 429") {
            "SK-AUTH-004"
        } else if lower.contains("authentication rejected: 401") || lower.contains("http 401") {
            "SK-AUTH-001"
        } else if lower.contains("authentication rejected: 403")
            || lower.contains("http 403")
            || lower.contains("unauthorized webview")
        {
            "SK-AUTH-003"
        } else if lower.contains("network")
            || lower.contains("connection refused")
            || lower.contains("dns")
        {
            "SK-NET-002"
        } else {
            match stage {
                "preflight" => "SK-CONNECT-001",
                "close" => "SK-CONNECT-002",
                "apply" => "SK-CONNECT-003",
                "launch" | "launch-prepare" => "SK-CONNECT-004",
                "restore" => "SK-RESTORE-001",
                _ => "SK-UNKNOWN-001",
            }
        };
    let retry = raw
        .split("[retry-after:")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|v| *v <= 86400);
    let outcome = match code {
        "SK-BIND-004" => "partial",
        "SK-LOCAL-002" => "unknown",
        "SK-NET-001" if method == "POST" => "unknown",
        _ => "failed",
    };
    // Where settings.json has to be fixed: two numbers, never any text from the file.
    let (line, column) = if code == "SK-RESTORE-003" {
        (
            number_after(raw, "at line "),
            number_after(raw, ", column "),
        )
    } else {
        (None, None)
    };
    json!({"code":code,"feedback_id":format!("sk-{:x}-{:x}-{:x}", now.as_nanos(), std::process::id(), SEQUENCE.fetch_add(1, Ordering::Relaxed)),"stage":stage,"outcome":outcome,"retry_after_seconds":retry,"line":line,"column":column,"occurred_at":now.as_secs().to_string()})
}

/// Bound for a line or column number carried to the page.
const MAX_POSITION: u64 = 10_000_000;

/// The positive number written right after `label` in `raw`.
fn number_after(raw: &str, label: &str) -> Option<u64> {
    let digits: String = raw
        .split(label)
        .nth(1)?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits
        .parse()
        .ok()
        .filter(|value| (1..=MAX_POSITION).contains(value))
}

/// Rebuild only the public schema; never trust persisted strings or extra fields.
pub fn validated(value: &Value) -> Option<Value> {
    let code = value["code"].as_str().filter(|s| CODES.contains(s))?;
    let stage = value["stage"].as_str().filter(|s| STAGES.contains(s))?;
    let outcome = value["outcome"]
        .as_str()
        .filter(|s| ["failed", "unknown", "partial"].contains(s))?;
    let id = value["feedback_id"].as_str()?;
    let parts: Vec<_> = id.strip_prefix("sk-")?.split('-').collect();
    if id.len() > 80
        || parts.len() != 3
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return None;
    }
    let occurred = value["occurred_at"].as_str()?;
    if occurred.is_empty()
        || !occurred.bytes().all(|b| b.is_ascii_digit())
        || occurred.parse::<u64>().is_err()
    {
        return None;
    }
    let retry = &value["retry_after_seconds"];
    if !retry.is_null() && retry.as_u64().is_none_or(|v| v > 86400) {
        return None;
    }
    let (line, column) = (&value["line"], &value["column"]);
    for position in [line, column] {
        if !position.is_null()
            && position
                .as_u64()
                .is_none_or(|v| !(1..=MAX_POSITION).contains(&v))
        {
            return None;
        }
    }
    Some(
        json!({"code":code,"stage":stage,"outcome":outcome,"feedback_id":id,"occurred_at":occurred,"retry_after_seconds":retry,"line":line,"column":column}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classification_and_privacy() {
        let cases = [
            ("Kiro 0.1 is unsupported; upgrade to 0.2", "SK-KIRO-001"),
            ("[auth:invalid-card]", "SK-AUTH-001"),
            ("[auth:expired]", "SK-AUTH-002"),
            ("[auth:access-denied]", "SK-AUTH-003"),
            ("[auth:device-binding]", "SK-BIND-001"),
            ("[auth:rebind-cooldown]", "SK-BIND-002"),
            ("[auth:rebind-limit]", "SK-BIND-003"),
            ("[auth:throttled]", "SK-AUTH-004"),
            ("[auth:locked-out]", "SK-AUTH-004"),
            (
                "Cannot stop Kiro for restore: Timed out waiting for Kiro process to terminate",
                "SK-CONNECT-002",
            ),
            (
                "Cannot stop Kiro for restore: Cannot safely determine Kiro process state",
                "SK-CONNECT-002",
            ),
            ("Cannot stop Kiro for restore: Kiro is still open; it may be asking whether to save changes", "SK-CONNECT-005"),
            ("[connection:close] Kiro is still open; it may be asking whether to save changes", "SK-CONNECT-005"),
            ("Cannot stop Kiro for restore: Kiro is open in another Windows session of this user; close it there and retry", "SK-CONNECT-006"),
            ("[connection:close] Kiro is open in another Windows session of this user; close it there and retry", "SK-CONNECT-006"),
            ("[connection:preflight] Kiro has windows on a profile other than Default; takeover configures only the Default profile", "SK-CONNECT-007"),
            ("[connection:preflight] A Kiro update is waiting to install; nothing was changed. Open Kiro once so it can finish, then try again", "SK-CONNECT-008"),
            ("[connection:apply] Login succeeded but takeover failed: Kiro's installation changed after it was checked, likely an update installed as Kiro closed; nothing was changed. Open Kiro once, then try again; your own Kiro sign-in was put back", "SK-CONNECT-008"),
            // Something is left for a restore, so this is not the case where nothing changed.
            ("[connection:apply] Login succeeded but takeover failed: Kiro's installation changed after it was checked; putting your own Kiro sign-in back failed (denied); recovery record retained", "SK-CONNECT-003"),
            ("Cannot stop Kiro for restore: Kiro is running as administrator and cannot be closed from here; close it yourself and retry", "SK-CONNECT-002"),
            ("timed out", "SK-NET-001"),
            ("network", "SK-NET-002"),
            ("TLS certificate", "SK-NET-003"),
            ("[connection:preflight]", "SK-CONNECT-001"),
            ("[connection:close]", "SK-CONNECT-002"),
            ("[connection:apply]", "SK-CONNECT-003"),
            ("[connection:launch]", "SK-CONNECT-004"),
            (
                "Remote unbind succeeded; local cleanup failed",
                "SK-BIND-004",
            ),
            ("permission denied", "SK-LOCAL-001"),
            ("Operation in progress", "SK-LOCAL-002"),
            ("oops", "SK-UNKNOWN-001"),
        ];
        for (raw, code) in cases {
            let value = classify(
                &format!("{raw} secret-card https://private.invalid C:\\secret"),
                "",
                "POST",
            );
            assert_eq!(value["code"], code, "{raw}");
            assert!(!value.to_string().contains("secret"));
            assert!(!value.to_string().contains("private.invalid"));
            assert_eq!(validated(&value), Some(value));
        }
        assert_eq!(
            classify("oops", "/api/restore", "POST")["code"],
            "SK-RESTORE-001"
        );
        assert_eq!(classify("timeout", "", "POST")["outcome"], "unknown");
        assert_eq!(classify("timeout", "", "GET")["outcome"], "failed");
        assert_eq!(
            classify("Remote unbind succeeded; local cleanup failed", "", "POST")["outcome"],
            "partial"
        );
        assert_eq!(
            classify("Operation in progress", "", "POST")["outcome"],
            "unknown"
        );
        assert_eq!(
            classify("[auth:rebind-cooldown] [retry-after:42]", "", "POST")["retry_after_seconds"],
            42
        );
        assert!(classify("[retry-after:86401]", "", "GET")["retry_after_seconds"].is_null());
    }
    #[test]
    fn every_code_has_a_desktop_message() {
        let messages = include_str!("../../../apps/desktop-ui/src/errors.ts");
        for code in CODES {
            assert!(messages.contains(&format!("'{code}':")), "{code}");
        }
    }
    #[test]
    fn classification_boundaries() {
        assert_eq!(
            classify("[connection:launch-prepare] failed", "", "POST")["code"],
            "SK-CONNECT-004"
        );
        assert_eq!(
            classify("Kiro MacBundleNameUnsupported", "", "POST")["code"],
            "SK-UNKNOWN-001"
        );
        assert_eq!(
            classify("Card authorization invalid or expired", "", "GET")["code"],
            "SK-UNKNOWN-001"
        );
        // Card verification says why a card cannot be used.
        for (raw, code) in [
            (
                "[auth:invalid-card] Card verification HTTP 400",
                "SK-AUTH-001",
            ),
            ("[auth:expired] Card has expired", "SK-AUTH-002"),
            (
                "[auth:access-denied] Card is frozen, banned or voided",
                "SK-AUTH-003",
            ),
            (
                "[auth:throttled] [retry-after:30] Card verification HTTP 429",
                "SK-AUTH-004",
            ),
        ] {
            let value = classify(raw, "/api/verify-card", "POST");
            assert_eq!(value["code"], code, "{raw}");
            assert_eq!(value["outcome"], "failed", "{raw}");
        }
        let reinstall = classify(
            "Kiro's extension is still modified and its backup is gone; reinstall Kiro to replace it",
            "/api/restore",
            "POST",
        );
        assert_eq!(reinstall["code"], "SK-RESTORE-002");
        assert_eq!(reinstall["outcome"], "failed");
        assert_eq!(validated(&reinstall), Some(reinstall.clone()));
        // The same guidance when a restore finds the patch's backup gone.
        assert_eq!(
            classify(
                "Kiro's extension is still modified and its backup is gone or no longer matches; reinstall Kiro to replace it, then restore again",
                "/api/restore",
                "POST",
            )["code"],
            "SK-RESTORE-002"
        );
        // A settings.json the customer has to fix first says where, and nothing else.
        let syntax = classify(
            "Settings error: settings.json has a syntax error at line 12, column 5; fix that line and retry",
            "/api/restore",
            "POST",
        );
        assert_eq!(syntax["code"], "SK-RESTORE-003");
        assert_eq!(syntax["outcome"], "failed");
        assert_eq!(
            (&syntax["line"], &syntax["column"]),
            (&json!(12), &json!(5))
        );
        assert_eq!(validated(&syntax), Some(syntax.clone()));
        for bad in [json!(0), json!("12"), json!(-1), json!(MAX_POSITION + 1)] {
            let mut forged = syntax.clone();
            forged["line"] = bad;
            assert!(validated(&forged).is_none());
        }
        // The same file refusing a takeover, or the cleanup of lost records.
        for (raw, path) in [
            ("[connection:preflight] Settings error: settings.json has a syntax error at line 3, column 1; fix that line and retry", "/api/activate"),
            ("settings.json has a syntax error at line 3, column 1; fix that line and retry", "/api/restore"),
        ] {
            let value = classify(raw, path, "POST");
            assert_eq!(value["code"], "SK-RESTORE-003", "{raw}");
            assert_eq!(value["line"], 3, "{raw}");
        }
        // Positions travel only with that code.
        let other = classify(
            "Settings error: oops at line 3, column 4",
            "/api/restore",
            "POST",
        );
        assert!(other["line"].is_null() && other["column"].is_null());
        assert_eq!(
            classify("credential network timeout", "", "POST")["code"],
            "SK-NET-001"
        );
        assert_eq!(
            classify("credential HTTP 403", "", "GET")["code"],
            "SK-AUTH-003"
        );
        for method in [
            "get_remembered_card",
            "set_remembered_card",
            "clear_remembered_card",
            "credential_get",
            "credential_set",
            "credential_delete",
        ] {
            let value = classify("keyring failed /private", "native", method);
            assert_eq!(value["code"], "SK-LOCAL-003");
            assert_eq!(value["stage"], "native");
        }
        assert_eq!(
            classify("credential failure", "native", "screen")["code"],
            "SK-UNKNOWN-001"
        );
    }
    #[test]
    fn reload_rejects_untrusted_fields_and_ids_are_unique() {
        let value = classify("oops", "", "GET");
        assert_ne!(
            value["feedback_id"],
            classify("oops", "", "GET")["feedback_id"]
        );
        for key in [
            "code",
            "stage",
            "outcome",
            "feedback_id",
            "occurred_at",
            "retry_after_seconds",
            "line",
            "column",
        ] {
            let mut bad = value.clone();
            bad[key] = json!("secret");
            assert!(validated(&bad).is_none(), "{key}");
        }
        let mut extra = value.clone();
        extra["raw"] = json!("secret");
        assert_eq!(validated(&extra), Some(value));
    }
}

#[cfg(test)]
mod stop_failure_tests {
    use super::*;
    /// A restore that could not close Kiro must stay actionable and must not be
    /// reported as an uncertain write. `SK-NET-001` carries outcome "unknown" on
    /// POST, and the client treats that as "the host may have written something",
    /// locking out every further attempt behind "last write unconfirmed" — which
    /// is what turned one failed restore into a permanently stuck client.
    #[test]
    fn a_kiro_that_will_not_close_is_actionable_not_an_uncertain_write() {
        for raw in [
            "Cannot stop Kiro for restore: Timed out waiting for Kiro process to terminate",
            "Cannot stop Kiro for restore: Cannot safely determine Kiro process state",
        ] {
            let payload = classify(raw, "/api/restore", "POST");
            assert_eq!(payload["code"], "SK-CONNECT-002", "{raw}");
            assert_eq!(payload["outcome"], "failed", "{raw}");
        }
        // The generic timeout rule still applies to everything else.
        assert_eq!(
            classify("credential network timeout", "/api/restore", "POST")["code"],
            "SK-NET-001"
        );
    }
}
