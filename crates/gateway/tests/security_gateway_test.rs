static PROXY_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::get,
    Router,
};
use gateway::security::{
    BruteForceConfig, BruteForceError, BruteForceProtector, ContentGuardrailConfig,
    DualRouterBuilder, GuardrailError,
};
use tower::ServiceExt;

#[test]
fn test_login_brute_force_progressive_lockout() {
    let config = BruteForceConfig {
        max_failures: 3,
        window_secs: 60,
        lockout_secs: 300,
    };
    let protector = BruteForceProtector::new(config);
    let ip = "192.168.1.100:device-abc";
    let now = 1_000;

    // Failure 1
    assert!(protector.record_failure(ip, now).is_ok());
    assert!(protector.check_lockout(ip, now).is_ok());

    // Failure 2
    assert!(protector.record_failure(ip, now + 5).is_ok());
    assert!(protector.check_lockout(ip, now + 5).is_ok());

    // Failure 3 -> Threshold reached! Locked out!
    let res = protector.record_failure(ip, now + 10);
    assert_eq!(
        res,
        Err(BruteForceError::LockedOut {
            remaining_secs: 300
        })
    );

    // During lockout period (now + 50s, 260s remaining)
    let check = protector.check_lockout(ip, now + 50);
    assert_eq!(
        check,
        Err(BruteForceError::LockedOut {
            remaining_secs: 260
        })
    );

    // Further attempts during lockout are blocked immediately
    let attempt_during_lock = protector.record_failure(ip, now + 60);
    assert_eq!(
        attempt_during_lock,
        Err(BruteForceError::LockedOut {
            remaining_secs: 250
        })
    );

    // After lockout expires (now + 311s)
    assert!(protector.check_lockout(ip, now + 311).is_ok());

    // Successful login resets state
    protector.record_success(ip);
    assert!(protector.check_lockout(ip, now + 312).is_ok());
}

#[test]
fn test_login_brute_force_window_expiry_resets_counter() {
    let config = BruteForceConfig {
        max_failures: 3,
        window_secs: 30,
        lockout_secs: 300,
    };
    let protector = BruteForceProtector::new(config);
    let ip = "10.0.0.5";

    // Failure 1 at t=100
    assert!(protector.record_failure(ip, 100).is_ok());
    // Failure 2 at t=110 (within 30s window)
    assert!(protector.record_failure(ip, 110).is_ok());

    // Failure 3 at t=150 (40s after window started at 100 -> window expired!)
    // Resets count to 1 instead of locking out
    assert!(protector.record_failure(ip, 150).is_ok());
    assert!(protector.check_lockout(ip, 150).is_ok());
}

#[test]
fn test_content_guardrail_prompt_and_image_limits() {
    let guardrail = ContentGuardrailConfig {
        max_body_bytes: 10 * 1024 * 1024,
        max_images_per_request: 5,
        max_image_bytes: 400 * 1024, // 400 KB
        max_prompt_chars: 10_000,
    };

    // Valid payload: 5000 chars, 2 images (100KB, 200KB)
    assert!(guardrail
        .validate_payload(5_000, &[100_000, 200_000])
        .is_ok());

    // Prompt too long (> 10,000)
    let err_prompt = guardrail.validate_payload(10_001, &[]);
    assert_eq!(
        err_prompt,
        Err(GuardrailError::PromptTooLong {
            actual: 10_001,
            max: 10_000
        })
    );

    // Too many images (> 5)
    let six_images = vec![50_000; 6];
    let err_images = guardrail.validate_payload(1_000, &six_images);
    assert_eq!(
        err_images,
        Err(GuardrailError::TooManyImages { actual: 6, max: 5 })
    );

    // Image too large (> 400KB)
    let large_image = vec![450 * 1024];
    let err_img_size = guardrail.validate_payload(1_000, &large_image);
    assert_eq!(
        err_img_size,
        Err(GuardrailError::ImageTooLarge {
            actual: 450 * 1024,
            max: 400 * 1024
        })
    );
}

#[tokio::test]
async fn test_dual_listener_router_isolation() {
    // 1. Kiro public router: rejects any /admin route
    let raw_kiro_router =
        Router::new().route("/generateAssistantResponse", get(|| async { "kiro-ok" }));
    let kiro_app = DualRouterBuilder::build_kiro_router(raw_kiro_router);

    // Calling normal Kiro endpoint -> OK (200)
    let req_kiro = Request::builder()
        .uri("/generateAssistantResponse")
        .body(Body::empty())
        .unwrap();
    let resp_kiro = kiro_app.clone().oneshot(req_kiro).await.unwrap();
    assert_eq!(resp_kiro.status(), StatusCode::OK);

    // Calling /admin on Kiro port -> Blocked (404 NOT FOUND)
    let req_admin_leak = Request::builder()
        .uri("/admin/cards")
        .body(Body::empty())
        .unwrap();
    let resp_admin_leak = kiro_app.oneshot(req_admin_leak).await.unwrap();
    assert_eq!(resp_admin_leak.status(), StatusCode::NOT_FOUND);

    // 2. Admin internal router: rejects /generateAssistantResponse
    let raw_admin_router = Router::new().route("/admin/cards", get(|| async { "admin-ok" }));
    let admin_app = DualRouterBuilder::build_admin_router(raw_admin_router);

    // Calling admin route -> OK
    let req_admin = Request::builder()
        .uri("/admin/cards")
        .body(Body::empty())
        .unwrap();
    let resp_admin = admin_app.clone().oneshot(req_admin).await.unwrap();
    assert_eq!(resp_admin.status(), StatusCode::OK);

    // Calling Kiro conversation endpoint on Admin port -> Blocked (404 NOT FOUND)
    let req_kiro_leak = Request::builder()
        .method("POST")
        .uri("/generateAssistantResponse")
        .body(Body::empty())
        .unwrap();
    let resp_kiro_leak = admin_app.oneshot(req_kiro_leak).await.unwrap();
    assert_eq!(resp_kiro_leak.status(), StatusCode::NOT_FOUND);
}

#[test]
fn test_untrusted_peer_ignores_spoofed_forwarded_headers() {
    let _guard = PROXY_ENV_LOCK.lock().unwrap();
    // 1. Untrusted peer address (e.g. public client or untrusted network)
    let mut extensions = axum::http::Extensions::new();
    let untrusted_peer: std::net::SocketAddr = "198.51.100.23:45678".parse().unwrap();
    extensions.insert(axum::extract::ConnectInfo(untrusted_peer));

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "x-forwarded-for",
        "203.0.113.195, 10.0.0.1".parse().unwrap(),
    );
    headers.insert("x-real-ip", "203.0.113.195".parse().unwrap());

    // With TRUSTED_PROXY_HEADERS not enabled or peer outside trusted CIDRs, headers are ignored
    std::env::remove_var("TRUSTED_PROXY_HEADERS");
    std::env::remove_var("TRUSTED_PROXY_CIDRS");

    let client_ip = gateway::security::client_ip_parts(&extensions, &headers);
    assert_eq!(
        client_ip, "198.51.100.23",
        "Untrusted peer must resolve to socket IP, ignoring spoofed headers"
    );
}

#[test]
fn test_trusted_proxy_header_resolution_and_tampering() {
    let _guard = PROXY_ENV_LOCK.lock().unwrap();
    std::env::set_var("TRUSTED_PROXY_HEADERS", "true");
    std::env::set_var(
        "TRUSTED_PROXY_CIDRS",
        "172.16.0.0/12,127.0.0.1/32,10.0.0.0/8",
    );

    // Validate trusted proxy configuration succeeds
    assert!(gateway::security::validate_trusted_proxy_config().is_ok());

    // 1. Connection coming through trusted Docker bridge proxy (172.18.0.2)
    let mut extensions = axum::http::Extensions::new();
    let proxy_peer: std::net::SocketAddr = "172.18.0.2:19820".parse().unwrap();
    extensions.insert(axum::extract::ConnectInfo(proxy_peer));

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "x-forwarded-for",
        "203.0.113.88, 172.18.0.10".parse().unwrap(),
    );

    let client_ip = gateway::security::client_ip_parts(&extensions, &headers);
    // Traverses backward from trusted hops: 172.18.0.10 is trusted proxy, 203.0.113.88 is external client
    assert_eq!(client_ip, "203.0.113.88");

    // Clean up
    std::env::remove_var("TRUSTED_PROXY_HEADERS");
    std::env::remove_var("TRUSTED_PROXY_CIDRS");
}

#[test]
fn test_corrupted_ledger_rejection_at_startup() {
    let temp_dir = std::env::temp_dir().join(format!("kiro_corrupted_test_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp_dir);
    let bad_state_file = temp_dir.join("billing_state.json");

    // Write corrupted JSON
    std::fs::write(
        &bad_state_file,
        b"{\"schema_version\": 1, \"cards\": INVALID_SYNTAX",
    )
    .unwrap();

    let billing = billing::engine::BillingEngine::new();
    billing.set_persistence_path(&bad_state_file);

    let load_res = billing.load_from_file(&bad_state_file);
    assert!(
        load_res.is_err(),
        "Corrupted snapshot file must fail to load"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}
