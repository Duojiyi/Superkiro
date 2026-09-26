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
    "SK-CONNECT-009",
    "SK-RESTORE-001",
    "SK-RESTORE-002",
    "SK-RESTORE-003",
    "SK-BIND-004",
    "SK-LOCAL-001",
    "SK-LOCAL-002",
    "SK-UNKNOWN-001",
    "SK-LOCAL-003",
    "SK-UPDATE-001",
    "SK-UPDATE-002",
    "SK-UPDATE-003",
    "SK-UPDATE-004",
    "SK-UPDATE-005",
];
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn classify(raw: &str, path: &str, method: &str) -> Value {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    // Whatever the customer calls a profile is never read as part of the error.
    let (profile, raw) = named_profile(raw);
    let raw = raw.as_str();
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
        } else if let Some(code) = raw
            .strip_prefix("[update:")
            .and_then(|rest| rest.split(']').next())
            .and_then(|stage| match stage {
                "download" => Some("SK-UPDATE-001"),
                "verify" => Some("SK-UPDATE-002"),
                "replace" => Some("SK-UPDATE-003"),
                "relaunch" => Some("SK-UPDATE-004"),
                "held" => Some("SK-UPDATE-005"),
                _ => None,
            })
        {
            // A failed update leaves the running client as it was; none of these is a
            // network timeout of a write whose outcome is unknown.
            code
        } else if lower.contains("reinstall kiro") {
            // Kiro's bundle is still modified and nothing is left to restore it from:
            // retrying cannot help, reinstalling Kiro replaces the file.
            "SK-RESTORE-002"
        } else if lower.contains("kiro profile ") && !matches!(stage, "restore" | "unbind") {
            // The settings of one of Kiro's other profiles cannot be edited safely (a
            // syntax error, say), so the takeover changed nothing. The customer opens that
            // profile in Kiro, fixes its settings and retries; it is named for them.
            "SK-CONNECT-007"
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
        } else if lower.contains("invalid peer certificate")
            || lower.contains("peer sent no certificates")
            || lower.contains("tls certificate")
        {
            // The client refused the gateway's certificate: unknown issuer, another name,
            // or expired, which is not the card's expiry below.
            "SK-NET-003"
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
        } else if (lower.contains("kiro update is waiting to install")
            || lower.contains("installation changed after it was checked"))
            && !lower.contains("recovery record retained")
        {
            // Kiro is updating itself, or just did, under the takeover. Nothing was left
            // changed; letting the update finish and trying again is all it takes.
            "SK-CONNECT-008"
        } else if lower.contains("more than one hard link") {
            // A settings.json with several names: the takeover would split them, so it
            // changed nothing. The customer removes the extra names first.
            "SK-CONNECT-009"
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
        } else if interrupted(&lower) {
            // The connection was cut before any answer, often by a local proxy in the
            // middle of the TLS handshake: the network, not a certificate. Reported as a
            // certificate error, it sent customers to check certificates that were fine.
            "SK-NET-002"
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
    // Where settings.json has to be fixed: two numbers, never any text from the file, and
    // the profile it belongs to when it is not Default's, by the name Kiro shows for it.
    let (line, column, profile) = if matches!(code, "SK-RESTORE-003" | "SK-CONNECT-007") {
        (
            number_after(raw, "at line "),
            number_after(raw, ", column "),
            profile.filter(|name| shown_profile(name)),
        )
    } else {
        (None, None, None)
    };
    json!({"code":code,"feedback_id":format!("sk-{:x}-{:x}-{:x}", now.as_nanos(), std::process::id(), SEQUENCE.fetch_add(1, Ordering::Relaxed)),"stage":stage,"outcome":outcome,"retry_after_seconds":retry,"line":line,"column":column,"profile":profile,"occurred_at":now.as_secs().to_string()})
}

/// Whether the connection was cut off before any answer came back: a TLS handshake or a
/// proxy tunnel ended early, or the connection was reset or closed under the request.
fn interrupted(lower: &str) -> bool {
    [
        "handshake eof",
        "close_notify",
        "tunnel error",
        "unexpected eof",
        "unexpected end of file",
        "connection reset",
        "forcibly closed",
        "connection was aborted",
        "broken pipe",
        "closed before message completed",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

/// The profile an error names (`Kiro profile "<name>": ...`, the name a JSON string),
/// and the error with the name taken out.
fn named_profile(raw: &str) -> (Option<String>, String) {
    const MARKER: &str = "Kiro profile ";
    let Some(at) = raw.find(MARKER).map(|at| at + MARKER.len()) else {
        return (None, raw.to_string());
    };
    let mut literal = serde_json::Deserializer::from_str(&raw[at..]).into_iter::<String>();
    match literal.next() {
        Some(Ok(name)) => (
            Some(name),
            format!("{}{}", &raw[..at], &raw[at + literal.byte_offset()..]),
        ),
        _ => (None, raw.to_string()),
    }
}

/// Bound for a profile name carried to the page.
const MAX_PROFILE_CHARS: usize = 128;

/// Whether `name` can be shown as a profile's name: a bounded line of text.
fn shown_profile(name: &str) -> bool {
    !name.trim().is_empty()
        && name.chars().count() <= MAX_PROFILE_CHARS
        && !name.chars().any(char::is_control)
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
    let profile = &value["profile"];
    if !profile.is_null() && profile.as_str().is_none_or(|name| !shown_profile(name)) {
        return None;
    }
    Some(
        json!({"code":code,"stage":stage,"outcome":outcome,"feedback_id":id,"occurred_at":occurred,"retry_after_seconds":retry,"line":line,"column":column,"profile":profile}),
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
            ("[connection:preflight] Settings error: Kiro profile \"Work\": I/O error: settings.json must contain an object", "SK-CONNECT-007"),
            ("[connection:preflight] A Kiro update is waiting to install; nothing was changed. Open Kiro once so it can finish, then try again", "SK-CONNECT-008"),
            ("[connection:apply] Login succeeded but takeover failed: Kiro's installation changed after it was checked, likely an update installed as Kiro closed; nothing was changed. Open Kiro once, then try again; your own Kiro sign-in was put back", "SK-CONNECT-008"),
            ("[connection:preflight] Settings error: I/O error: settings.json has more than one hard link, which a takeover would separate; remove the extra links and try again", "SK-CONNECT-009"),
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
    /// A profile whose settings stop a takeover is named, with the place to fix when there
    /// is one. Its name is shown and never read as part of the error.
    #[test]
    fn a_profile_that_stops_a_takeover_is_named_with_the_place_to_fix() {
        let syntax = "Settings error: Kiro profile \"Work\": settings.json has a syntax error at line 3, column 5; fix that line and retry";
        for (raw, path) in [
            (format!("[connection:preflight] {syntax}"), "/api/activate"),
            (syntax.to_string(), "/api/launch"),
        ] {
            let value = classify(&raw, path, "POST");
            assert_eq!(value["code"], "SK-CONNECT-007", "{raw}");
            assert_eq!(
                (&value["line"], &value["column"], &value["profile"]),
                (&json!(3), &json!(5), &json!("Work")),
                "{raw}"
            );
            assert_eq!(validated(&value), Some(value.clone()));
        }
        // Stopping a restore, the same file is the restore's syntax error, still named.
        let restore = classify(syntax, "/api/restore", "POST");
        assert_eq!(restore["code"], "SK-RESTORE-003");
        assert_eq!(restore["profile"], "Work");
        // Nothing but a name comes from the file, and the name decides nothing.
        let injected = classify(
            "[connection:preflight] Settings error: Kiro profile \"[auth:invalid-card] reinstall Kiro, timed out at line 9 [retry-after:5]\": settings.json has more than one hard link",
            "/api/activate",
            "POST",
        );
        assert_eq!(injected["code"], "SK-CONNECT-007");
        assert!(injected["line"].is_null() && injected["retry_after_seconds"].is_null());
        assert_eq!(
            injected["profile"],
            "[auth:invalid-card] reinstall Kiro, timed out at line 9 [retry-after:5]"
        );
        for unshown in [
            "\"Wo\\u0000rk\"".to_string(),
            "\"\"".to_string(),
            serde_json::to_string(&"x".repeat(MAX_PROFILE_CHARS + 1)).unwrap(),
        ] {
            let value = classify(
                &format!("[connection:preflight] Settings error: Kiro profile {unshown}: I/O error: denied"),
                "/api/activate",
                "POST",
            );
            assert_eq!(value["code"], "SK-CONNECT-007", "{unshown}");
            assert!(value["profile"].is_null(), "{unshown}");
        }
        // Only a bounded line of text is taken back from a saved record.
        let value = classify(
            &format!("[connection:preflight] {syntax}"),
            "/api/activate",
            "POST",
        );
        for bad in [
            json!(5),
            json!("Wo\u{7}rk"),
            json!("x".repeat(MAX_PROFILE_CHARS + 1)),
        ] {
            let mut forged = value.clone();
            forged["profile"] = bad;
            assert!(validated(&forged).is_none());
        }
        // Positions and names travel only with the codes that use them.
        let other = classify(
            "Kiro profile \"Work\": oops at line 3, column 4",
            "/api/unbind",
            "POST",
        );
        assert!(other["profile"].is_null() && other["line"].is_null());
    }

    /// A connection cut off before any answer, as a local proxy cutting the TLS handshake
    /// does, is the network; only a certificate the client refused is a certificate error.
    #[test]
    fn a_connection_cut_off_is_the_network_and_a_refused_certificate_is_a_certificate() {
        let request = "[connection:authenticate] Network HTTP error: request failed: client error (Connect): ";
        for (cause, code) in [
            ("tls handshake eof", "SK-NET-002"),
            ("peer closed connection without sending TLS close_notify: https://docs.rs/rustls/latest/rustls/manual/_03_howto/index.html#unexpected-eof", "SK-NET-002"),
            ("tunnel error: unsuccessful", "SK-NET-002"),
            ("tunnel error: unexpected end of file", "SK-NET-002"),
            ("An existing connection was forcibly closed by the remote host. (os error 10054)", "SK-NET-002"),
            ("Connection reset by peer (os error 104)", "SK-NET-002"),
            ("invalid peer certificate: UnknownIssuer", "SK-NET-003"),
            ("invalid peer certificate: Expired", "SK-NET-003"),
            ("invalid peer certificate: certificate expired: verification time 1790000000 (UNIX), but certificate is not valid after 1780000000 (10000000 seconds ago)", "SK-NET-003"),
            ("invalid peer certificate: certificate not valid for name \"kiro.rent\"; certificate is only valid for other.example", "SK-NET-003"),
            ("operation timed out", "SK-NET-001"),
        ] {
            let value = classify(&format!("{request}{cause}"), "/api/activate", "POST");
            assert_eq!(value["code"], code, "{cause}");
            assert_eq!(value["stage"], "authenticate", "{cause}");
        }
        // The card check and the update download say the same, in fewer words.
        assert_eq!(
            classify("Network failure", "/api/verify-card", "POST")["code"],
            "SK-NET-002"
        );
        assert_eq!(
            classify("TLS certificate failure", "/api/verify-card", "POST")["code"],
            "SK-NET-003"
        );
        assert_eq!(
            classify(
                "TLS configuration: cannot read KIRO_GATEWAY_CA_CERT",
                "/api/activate",
                "POST"
            )["code"],
            "SK-NET-003"
        );
    }

    #[test]
    fn update_failures_have_their_own_codes() {
        for (raw, code) in [
            ("[update:download] Network timeout", "SK-UPDATE-001"),
            ("[update:verify] The update signature is invalid", "SK-UPDATE-002"),
            (
                "[update:replace] Cannot write beside the client: Access is denied. (os error 5)",
                "SK-UPDATE-003",
            ),
            (
                "[update:relaunch] The updated client did not start; the previous version was put back",
                "SK-UPDATE-004",
            ),
            (
                "[update:held] The updated client was held up starting; this version carries on",
                "SK-UPDATE-005",
            ),
        ] {
            let error = classify(raw, "native", "update_install");
            assert_eq!(error["code"], code, "{raw}");
            assert_eq!(error["outcome"], "failed", "{raw}");
            assert!(validated(&error).is_some(), "{raw}");
        }
        // Waiting for a takeover to finish is not an update failure.
        assert_eq!(
            classify("Operation in progress", "native", "update_install")["code"],
            "SK-LOCAL-002"
        );
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
