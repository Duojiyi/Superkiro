use super::*;
use axum::{
    body::Body, extract::ConnectInfo, http::Request as HttpRequest, middleware, routing::any,
    Router,
};
use tower::ServiceExt;

fn browser() -> BrowserAuth {
    BrowserAuth::new(
        "https://admin.test".into(),
        bcrypt::hash("fixture-password", 4).unwrap(),
    )
    .unwrap()
}
fn app(browser: BrowserAuth) -> Router {
    let auth = AdminAuthState::new("fixture-signing-key");
    Router::new()
        .route("/api/v1/admin/session", any(|| async { StatusCode::OK }))
        .layer(middleware::from_fn(move |req, next| {
            let auth = auth.clone();
            let browser = browser.clone();
            async move { handle(&auth, &browser, req, next).await }
        }))
}
fn request(ip: &str, content_type: bool, body: &str) -> HttpRequest<Body> {
    let mut req = HttpRequest::builder()
        .method("POST")
        .uri("/api/v1/admin/session")
        .header("origin", "https://admin.test");
    if content_type {
        req = req.header("content-type", "application/json");
    }
    let mut req = req.body(Body::from(body.to_owned())).unwrap();
    req.extensions_mut().insert(ConnectInfo(
        format!("{ip}:1234")
            .parse::<std::net::SocketAddr>()
            .unwrap(),
    ));
    req
}
const VALID: &str = r#"{"username":"admin","password":"fixture-password"}"#;
const WRONG: &str = r#"{"username":"admin","password":"wrong"}"#;

#[tokio::test]
async fn malformed_requests_do_not_block_another_source() {
    for (content_type, body, expected) in [
        (false, VALID.to_string(), StatusCode::UNSUPPORTED_MEDIA_TYPE),
        (true, "{".to_string(), StatusCode::BAD_REQUEST),
        (true, "x".repeat(4097), StatusCode::BAD_REQUEST),
        (
            true,
            r#"{"username":"admin","password":""}"#.to_string(),
            StatusCode::UNAUTHORIZED,
        ),
        (
            true,
            r#"{"username":"admin","password":null}"#.to_string(),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let app = app(browser());
        for _ in 0..10 {
            assert_eq!(
                app.clone()
                    .oneshot(request("198.51.100.10", content_type, &body))
                    .await
                    .unwrap()
                    .status(),
                expected
            );
        }
        let limited = app
            .clone()
            .oneshot(request("198.51.100.10", true, VALID))
            .await
            .unwrap();
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(limited.headers()[header::RETRY_AFTER], "60");
        let response = app
            .oneshot(request("198.51.100.11", true, VALID))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key(header::SET_COOKIE));
    }
}

#[tokio::test]
async fn wrong_password_budget_is_per_source_and_forwarded_headers_cannot_spoof_it() {
    let app = app(browser());
    for n in 0..10 {
        let mut req = request("198.51.100.20", true, WRONG);
        req.headers_mut()
            .insert("x-forwarded-for", format!("203.0.113.{n}").parse().unwrap());
        req.headers_mut()
            .insert("x-real-ip", format!("203.0.113.{n}").parse().unwrap());
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        app.clone()
            .oneshot(request("198.51.100.20", true, VALID))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        app.oneshot(request("198.51.100.21", true, VALID))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn malformed_requests_never_use_password_worker_budget() {
    let browser = browser();
    let permits: Vec<_> = (0..MAX_PASSWORD_CHECKS)
        .map(|_| browser.password_checks.clone().try_acquire_owned().unwrap())
        .collect();
    let app = app(browser.clone());
    assert_eq!(
        app.clone()
            .oneshot(request("198.51.100.30", false, VALID))
            .await
            .unwrap()
            .status(),
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    assert_eq!(
        app.clone()
            .oneshot(request("198.51.100.31", true, "{"))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.clone()
            .oneshot(request("198.51.100.32", true, VALID))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(permits);
    assert_eq!(
        app.oneshot(request("198.51.100.33", true, VALID))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        browser.password_checks.available_permits(),
        MAX_PASSWORD_CHECKS
    );
}

#[test]
fn source_storage_is_bounded_and_live_lockouts_survive_churn() {
    let browser = browser();
    for _ in 0..10 {
        assert!(browser.reserve_attempt("locked"));
    }
    for n in 1..MAX_LOGIN_SOURCES {
        assert!(browser.reserve_attempt(&format!("source-{n}")));
    }
    assert!(!browser.reserve_attempt("overflow"));
    assert!(!browser.reserve_attempt("locked"));
    assert_eq!(browser.attempts.lock().unwrap().len(), MAX_LOGIN_SOURCES);
    {
        let mut budgets = browser.attempts.lock().unwrap();
        for (source, (start, _)) in budgets.iter_mut() {
            if source != "locked" {
                *start = Instant::now() - Duration::from_secs(61);
            }
        }
    }
    assert!(browser.reserve_attempt("new-source"));
    assert!(!browser.reserve_attempt("locked"));
    assert_eq!(browser.attempts.lock().unwrap().len(), 2);
    browser
        .attempts
        .lock()
        .unwrap()
        .get_mut("locked")
        .unwrap()
        .0 = Instant::now() - Duration::from_secs(61);
    assert!(browser.reserve_attempt("locked"));
}

#[test]
fn concurrent_source_reservations_cannot_exceed_ten() {
    let browser = browser();
    let threads: Vec<_> = (0..32)
        .map(|_| {
            let browser = browser.clone();
            std::thread::spawn(move || browser.reserve_attempt("same-source"))
        })
        .collect();
    assert_eq!(
        threads
            .into_iter()
            .map(|t| t.join().unwrap())
            .filter(|accepted| *accepted)
            .count(),
        10
    );
}

// A child process isolates trusted-proxy environment from other tests.
#[test]
fn trusted_proxy_sources_are_isolated() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "facade::admin_login::audit_login_security::trusted_proxy_child",
            "--nocapture",
        ])
        .env("ADMIN_LOGIN_PROXY_FIXTURE", "1")
        .env("TRUSTED_PROXY_HEADERS", "true")
        .env("TRUSTED_PROXY_CIDRS", "127.0.0.1/32")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn trusted_proxy_child() {
    if std::env::var("ADMIN_LOGIN_PROXY_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let app = app(browser());
    for n in 0..10 {
        let mut req = request("127.0.0.1", true, WRONG);
        // Spoofed leftmost values cannot change the rightmost untrusted hop.
        req.headers_mut().insert(
            "x-forwarded-for",
            format!("203.0.113.{n}, 198.51.100.40, 127.0.0.1")
                .parse()
                .unwrap(),
        );
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
    for (client, expected) in [
        ("198.51.100.40", StatusCode::TOO_MANY_REQUESTS),
        ("198.51.100.41", StatusCode::OK),
    ] {
        let mut req = request("127.0.0.1", true, VALID);
        req.headers_mut()
            .insert("x-forwarded-for", client.parse().unwrap());
        assert_eq!(app.clone().oneshot(req).await.unwrap().status(), expected);
    }
}

// A flood of new sources fills the source table with live budgets. A source that logged
// in before has a lane of its own, so the operator is not shut out with the strangers.
#[tokio::test]
async fn a_source_that_logged_in_before_keeps_a_lane_when_strangers_fill_the_table() {
    let browser = browser();
    let app = app(browser.clone());
    let operator = "198.51.100.40";
    let status = |ip: &'static str| {
        let app = app.clone();
        async move {
            app.oneshot(request(ip, true, VALID))
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(status(operator).await, StatusCode::OK);
    // Some time later, any budget the operator had in the shared table has lapsed, and
    // strangers hold every slot of it.
    if let Some(budget) = browser.attempts.lock().unwrap().get_mut(operator) {
        budget.0 = Instant::now() - Duration::from_secs(61);
    }
    for n in 0..MAX_LOGIN_SOURCES {
        browser.reserve_attempt(&format!("stranger-{n}"));
    }
    assert_eq!(browser.attempts.lock().unwrap().len(), MAX_LOGIN_SOURCES);

    assert_eq!(status("198.51.100.41").await, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(status(operator).await, StatusCode::OK);
}

// Strangers keeping every password check busy make logins fail with 429. A source that
// logged in before still gets the reserved check.
#[tokio::test]
async fn a_source_that_logged_in_before_keeps_a_password_check_when_strangers_hold_them_all() {
    let browser = browser();
    let app = app(browser.clone());
    let operator = "198.51.100.50";
    assert_eq!(
        app.clone()
            .oneshot(request(operator, true, VALID))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let held: Vec<_> = (0..MAX_PASSWORD_CHECKS)
        .map(|_| browser.password_checks.clone().try_acquire_owned().unwrap())
        .collect();

    assert_eq!(
        app.clone()
            .oneshot(request("198.51.100.51", true, VALID))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        app.oneshot(request(operator, true, VALID))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    drop(held);
}

// One IPv6 subscriber usually holds a whole /64; spread across it, it still has one
// budget, while another /64 is unaffected.
#[tokio::test]
async fn ipv6_addresses_in_one_64_share_one_budget() {
    let app = app(browser());
    for n in 0..10 {
        let ip = format!("[2001:db8:1:2::{:x}]", n + 1);
        assert_eq!(
            app.clone()
                .oneshot(request(&ip, true, WRONG))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        app.clone()
            .oneshot(request("[2001:db8:1:2::ff]", true, VALID))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        app.oneshot(request("[2001:db8:1:3::1]", true, VALID))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[test]
fn login_sources_are_ipv4_addresses_or_ipv6_64s() {
    assert_eq!(login_source("198.51.100.7"), "198.51.100.7");
    assert_eq!(login_source("::ffff:198.51.100.7"), "198.51.100.7");
    assert_eq!(login_source("2001:db8:1:2:3:4:5:6"), "2001:db8:1:2::/64");
    assert_eq!(login_source("unknown"), "unknown");
}
