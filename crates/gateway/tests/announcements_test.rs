//! Public announcements must use the shared billing store without user/admin credentials.
use axum::{
    body::{to_bytes, Body},
    http::{Method, Request, StatusCode},
};
use billing::{
    observability::{Announcement, AnnouncementLevel},
    BillingEngine,
};
use gateway::facade::FacadeRegistry;
use serde_json::{json, Value};
use tower::ServiceExt;

fn setup() -> (BillingEngine, axum::Router) {
    let billing = BillingEngine::default();
    let mut registry = FacadeRegistry::new();
    registry.register_portal_facades(billing.clone(), None);
    let auth = gateway::auth::AuthState::new("test-announcements-auth-secret-32bytes");
    (billing, registry.into_router_with_auth(auth))
}

#[tokio::test]
async fn anonymous_announcements_are_active_display_fields_only() {
    let (billing, app) = setup();
    for (id, level) in [
        ("a", AnnouncementLevel::Info),
        ("b", AnnouncementLevel::Warning),
        ("c", AnnouncementLevel::Critical),
    ] {
        billing.add_announcement(Announcement::new(id, "Title", "Content", level, 42));
    }
    let mut disabled =
        Announcement::new("disabled", "Hidden", "Hidden", AnnouncementLevel::Info, 0);
    disabled.enabled = false;
    billing.add_announcement(disabled);
    billing.add_announcement(
        Announcement::new("expired", "Hidden", "Hidden", AnnouncementLevel::Info, 0).with_expiry(1),
    );
    billing.add_announcement(
        Announcement::new("future", "Title", "Content", AnnouncementLevel::Info, 42)
            .with_expiry(u64::MAX),
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/announcements")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let value: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 2);
    assert_eq!(value["success"], true);
    let rows = value["announcements"].as_array().unwrap();
    assert_eq!(rows.len(), 4);
    for (id, level) in [
        ("a", "info"),
        ("b", "warning"),
        ("c", "critical"),
        ("future", "info"),
    ] {
        let row = rows.iter().find(|r| r["id"] == id).unwrap();
        assert_eq!(
            row,
            &json!({"id":id,"level":level,"title":"Title","content":"Content",
            "created_at":42,"expires_at":if id == "future" { json!(u64::MAX) } else { Value::Null }})
        );
    }
}

#[tokio::test]
async fn empty_is_success_and_writes_are_not_allowed() {
    let (_, app) = setup();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/announcements")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let value: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(value, json!({"success":true,"announcements":[]}));
    for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/api/v1/announcements")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}

const ADMIN_KEY: &str = "test-announcements-admin-key-32chars!!";

/// The public announcements and the admin API over one billing state, with groups A and B,
/// and the sign-in the public route honours.
fn setup_with_admin() -> (BillingEngine, gateway::auth::AuthState, axum::Router) {
    let billing = BillingEngine::default();
    billing.upsert_group(billing::Group::pro_plus("group-a", "A"));
    billing.upsert_group(billing::Group::pro_plus("group-b", "B"));
    let mut registry = FacadeRegistry::new();
    registry.register_portal_facades(billing.clone(), None);
    registry.register_admin_facades(billing.clone(), ADMIN_KEY.to_string());
    let auth = gateway::auth::AuthState::new("test-announcements-auth-secret-32bytes");
    (billing, auth.clone(), registry.into_router_with_auth(auth))
}

/// A request's status and JSON body; `admin` sends the admin key, `bearer` a card's token.
async fn call(
    app: &axum::Router,
    method: Method,
    uri: &str,
    admin: bool,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if admin {
        request = request.header("x-admin-key", ADMIN_KEY);
    }
    if let Some(token) = bearer {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let body = body.map_or_else(Body::empty, |body| Body::from(body.to_string()));
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

async fn publish(app: &axum::Router, body: Value) -> (StatusCode, Value) {
    let uri = "/api/v1/admin/announcements";
    call(app, Method::POST, uri, true, None, Some(body)).await
}

async fn edit(app: &axum::Router, body: Value) -> (StatusCode, Value) {
    let uri = "/api/v1/admin/announcements/edit";
    call(app, Method::POST, uri, true, None, Some(body)).await
}

/// The titles a client is shown, signed in with `bearer` or not, in order.
async fn client_titles(app: &axum::Router, bearer: Option<&str>) -> Vec<String> {
    let uri = "/api/v1/announcements";
    let (status, body) = call(app, Method::GET, uri, false, bearer, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut titles: Vec<String> = body["announcements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["title"].as_str().unwrap().to_string())
        .collect();
    titles.sort();
    titles
}

/// The console's list, and with all=true, ended and withdrawn ones too.
async fn admin_list(app: &axum::Router, all: bool) -> Vec<Value> {
    let uri = if all {
        "/api/v1/admin/announcements?all=true"
    } else {
        "/api/v1/admin/announcements"
    };
    let (status, body) = call(app, Method::GET, uri, true, None, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["announcements"].as_array().unwrap().clone()
}

#[tokio::test]
async fn an_announcement_is_shown_from_its_start_to_its_end() {
    let (billing, _, app) = setup_with_admin();
    let now = gateway::now_secs();
    let (status, open) = publish(&app, json!({"title": "Open", "content": "Now"})).await;
    assert_eq!(status, StatusCode::OK, "{open}");
    let open = &open["announcement"];
    assert_eq!(open["status"], "active");
    assert_eq!(open["starts_at"], open["created_at"]);
    assert_eq!(open["expires_at"], Value::Null);
    assert_eq!(
        (&open["audience"], &open["edits"]),
        (&json!([]), &json!([]))
    );
    // A maintenance notice: shown from an hour ahead until it is over.
    let (status, later) = publish(
        &app,
        json!({"title": "Later", "content": "Maintenance", "level": "warning",
               "startsAtSecs": now + 3_600, "endsAtSecs": now + 7_200}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{later}");
    let later = &later["announcement"];
    assert_eq!(later["status"], "scheduled");
    assert_eq!(later["starts_at"], now + 3_600);
    assert_eq!(later["expires_at"], now + 7_200);
    // The older days shortcut counts from the start; a start already past is now.
    let (_, shortcut) = publish(
        &app,
        json!({"title": "Shortcut", "content": "x", "startsAtSecs": now + 3_600, "ttlSecs": 600}),
    )
    .await;
    assert_eq!(shortcut["announcement"]["expires_at"], now + 4_200);
    let (_, past) = publish(
        &app,
        json!({"title": "Past", "content": "x", "startsAtSecs": now - 100, "ttlSecs": 600}),
    )
    .await;
    let past = &past["announcement"];
    assert_eq!(past["status"], "active");
    let start = past["starts_at"].as_u64().unwrap();
    assert!(start >= now);
    assert_eq!(past["expires_at"], start + 600);
    assert_eq!(client_titles(&app, None).await, ["Open", "Past"]);

    let after_start = "endsAtSecs must be after the start and within 3650 days";
    for (body, message) in [
        (
            json!({"title": "t", "content": "c", "startsAtSecs": now + 100, "endsAtSecs": now + 100}),
            after_start,
        ),
        (
            json!({"title": "t", "content": "c", "endsAtSecs": now + 3651 * 86_400}),
            after_start,
        ),
        (
            json!({"title": "t", "content": "c", "startsAtSecs": now + 3651 * 86_400}),
            "startsAtSecs must be within 3650 days",
        ),
        (
            json!({"title": "t", "content": "c", "endsAtSecs": now + 900, "ttlSecs": 600}),
            "Give at most one of endsAtSecs and ttlSecs",
        ),
        (
            json!({"title": "t", "content": "c", "audience": ["group-a", "nope"]}),
            "Unknown group in audience: nope",
        ),
    ] {
        assert_eq!(
            publish(&app, body).await,
            (
                StatusCode::BAD_REQUEST,
                json!({"success": false, "error": message})
            )
        );
    }

    // The console lists what is scheduled or shown, newest first; ended and withdrawn ones
    // only when it asks for all.
    billing.add_announcement(
        Announcement::new("ended", "Ended", "x", AnnouncementLevel::Info, 10).with_expiry(20),
    );
    let (status, _) = call(
        &app,
        Method::POST,
        "/api/v1/admin/announcements/withdraw",
        true,
        None,
        Some(json!({"id": open["id"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let listed: Vec<Value> = admin_list(&app, false)
        .await
        .iter()
        .map(|a| a["title"].clone())
        .collect();
    assert_eq!(listed.len(), 3, "{listed:?}");
    assert!(!listed.contains(&json!("Open")) && !listed.contains(&json!("Ended")));
    let all = admin_list(&app, true).await;
    let status_of = |title: &str| {
        all.iter()
            .find(|a| a["title"] == title)
            .map(|a| a["status"].clone())
            .unwrap()
    };
    assert_eq!(
        [status_of("Open"), status_of("Ended"), status_of("Later")],
        [json!("withdrawn"), json!("ended"), json!("scheduled")]
    );
    let created: Vec<u64> = all
        .iter()
        .map(|a| a["created_at"].as_u64().unwrap())
        .collect();
    assert!(
        created.windows(2).all(|pair| pair[0] >= pair[1]),
        "{created:?}"
    );
}

#[tokio::test]
async fn an_announcement_is_edited_in_place_and_each_edit_is_kept() {
    let (billing, _, app) = setup_with_admin();
    let now = gateway::now_secs();
    let (_, created) = publish(
        &app,
        json!({"title": "Maintenance 09-28", "content": "02:00", "startsAtSecs": now + 3_600,
               "endsAtSecs": now + 7_200}),
    )
    .await;
    let id = created["announcement"]["id"].clone();
    // The console sends every field back; only those that differ are changes.
    let (status, body) = edit(
        &app,
        json!({"id": id, "title": "Maintenance 09-29", "endsAtSecs": now + 10_800,
               "startsAtSecs": now + 3_600, "content": "02:00"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let edited = &body["announcement"];
    assert_eq!(edited["title"], "Maintenance 09-29");
    assert_eq!(edited["expires_at"], now + 10_800);
    assert_eq!(edited["starts_at"], now + 3_600);
    let edits = edited["edits"].as_array().unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0]["operator"], "admin");
    assert_eq!(edits[0]["changed"], json!(["title", "expires_at"]));
    assert!(edits[0]["at_secs"].as_u64().unwrap() >= now);
    // The same values again change nothing and are not recorded.
    let (status, again) = edit(&app, json!({"id": id, "title": "Maintenance 09-29"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["announcement"]["edits"].as_array().unwrap().len(), 1);
    // Shown from now, until withdrawn, to group A only.
    let (status, body) = edit(
        &app,
        json!({"id": id, "startsAtSecs": now - 60, "endsAtSecs": null, "audience": ["group-a"],
               "level": "critical"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let edited = &body["announcement"];
    assert_eq!(edited["status"], "active");
    assert_eq!(edited["expires_at"], Value::Null);
    assert!(edited["starts_at"].as_u64().unwrap() >= now);
    assert_eq!(edited["audience"], json!(["group-a"]));
    assert_eq!(
        edited["edits"][1]["changed"],
        json!(["level", "starts_at", "expires_at", "audience"])
    );
    // Kept across a restart.
    let restored = BillingEngine::new();
    restored.import_snapshot(billing.export_snapshot());
    let kept = restored.list_announcements();
    assert_eq!(kept[0].edits.len(), 2);
    assert_eq!(kept[0].audience, ["group-a"]);

    let before = billing.list_announcements();
    for (body, status, message) in [
        (
            json!({"id": id, "startsAtSecs": now + 600, "endsAtSecs": now + 300}),
            StatusCode::BAD_REQUEST,
            "endsAtSecs must be after the start and within 3650 days",
        ),
        (
            json!({"id": id, "endsAtSecs": now + 600, "ttlSecs": 600}),
            StatusCode::BAD_REQUEST,
            "Give at most one of endsAtSecs and ttlSecs",
        ),
        (
            json!({"id": id, "level": "loud"}),
            StatusCode::BAD_REQUEST,
            "level must be info, warning or critical",
        ),
        (
            json!({"id": id, "title": " "}),
            StatusCode::BAD_REQUEST,
            "title must be 1 to 256 characters and content 1 to 20000",
        ),
        (
            json!({"id": id, "audience": ["nope"]}),
            StatusCode::BAD_REQUEST,
            "Unknown group in audience: nope",
        ),
        (
            json!({"id": "ann-missing", "title": "x"}),
            StatusCode::NOT_FOUND,
            "announcement not found",
        ),
    ] {
        assert_eq!(
            edit(&app, body).await,
            (status, json!({"success": false, "error": message}))
        );
    }
    let (status, body) = edit(&app, json!({"id": id, "endsAt": now + 600})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .starts_with("Invalid request body"));
    assert_eq!(billing.list_announcements(), before);

    // A withdrawn one stays withdrawn.
    billing.withdraw_announcement(id.as_str().unwrap()).unwrap();
    assert_eq!(
        edit(&app, json!({"id": id, "title": "Back"})).await,
        (
            StatusCode::CONFLICT,
            json!({"success": false, "error": "A withdrawn announcement cannot be edited"})
        )
    );
    // Only the administrator edits.
    let (status, _) = call(
        &app,
        Method::POST,
        "/api/v1/admin/announcements/edit",
        false,
        None,
        Some(json!({"id": id, "title": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_announcement_for_some_groups_reaches_only_their_signed_in_cards() {
    let (_, auth, app) = setup_with_admin();
    for (title, audience) in [
        ("Everyone", json!([])),
        ("For A", json!(["group-a"])),
        ("For B", json!(["group-b"])),
        ("For A and B", json!(["group-a", "group-b", "group-a"])),
    ] {
        let (status, body) = publish(
            &app,
            json!({"title": title, "content": "x", "audience": audience}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let token = |card: &str, group: &str| {
        auth.upsert_card(gateway::auth::CardRecord {
            card_id: card.into(),
            group_id: group.into(),
            current_token_version: 1,
            is_active: true,
        });
        auth.issue_token(card, group, 1, 3_600).unwrap()
    };
    let (a, b) = (token("card-a", "group-a"), token("card-b", "group-b"));
    assert_eq!(client_titles(&app, None).await, ["Everyone"]);
    assert_eq!(
        client_titles(&app, Some(&a)).await,
        ["Everyone", "For A", "For A and B"]
    );
    assert_eq!(
        client_titles(&app, Some(&b)).await,
        ["Everyone", "For A and B", "For B"]
    );
    // A token that does not verify is no sign-in, and no refusal either.
    assert_eq!(client_titles(&app, Some("not-a-token")).await, ["Everyone"]);
    auth.set_card_active("card-a", false);
    assert_eq!(client_titles(&app, Some(&a)).await, ["Everyone"]);
    // The public rows still carry display fields only.
    let uri = "/api/v1/announcements";
    let (_, body) = call(&app, Method::GET, uri, false, Some(&b), None).await;
    for row in body["announcements"].as_array().unwrap() {
        let mut keys: Vec<&String> = row.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "content",
                "created_at",
                "expires_at",
                "id",
                "level",
                "title"
            ]
        );
    }
    let listed = admin_list(&app, false).await;
    let both = listed.iter().find(|a| a["title"] == "For A and B").unwrap();
    assert_eq!(both["audience"], json!(["group-a", "group-b"]));
}
