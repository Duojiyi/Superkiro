//! Integration tests for Notification Channels (Spec §11, §14.6, P4-4).

use gateway::notification::{
    now_secs, NotificationChannel, NotificationDispatcher, NotificationEvent,
};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn test_notification_event_formatting() {
    let ts = now_secs();

    // 1. CardIssued
    let card_event = NotificationEvent::CardIssued {
        card_id: "card-999".to_string(),
        card_code_masked: "kiro-***-abcd".to_string(),
        initial_credits: 50_000,
        plan_name: "KIRO PRO+".to_string(),
        ts,
    };
    let tg_card = NotificationDispatcher::format_telegram(&card_event);
    assert!(tg_card.contains("卡密发放通知"));
    assert!(tg_card.contains("card-999"));
    assert!(tg_card.contains("KIRO PRO+"));

    let (email_sub, email_body) = NotificationDispatcher::format_email(&card_event);
    assert!(email_sub.contains("卡密已生成"));
    assert!(email_body.contains("50000 积分"));

    // 2. BalanceLow
    let balance_event = NotificationEvent::BalanceLow {
        card_id: "card-888".to_string(),
        current_credits: 1_200,
        threshold_credits: 5_000,
        ts,
    };
    let tg_bal = NotificationDispatcher::format_telegram(&balance_event);
    assert!(tg_bal.contains("余额不足预警"));
    assert!(tg_bal.contains("1200"));
    assert!(tg_bal.contains("积分"));

    // 3. ProviderFault
    let fault_event = NotificationEvent::ProviderFault {
        provider_id: "anthropic-primary".to_string(),
        error_class: "OverloadedError".to_string(),
        status_code: Some(529),
        failover_target: Some("anthropic-secondary".to_string()),
        ts,
    };
    let tg_fault = NotificationDispatcher::format_telegram(&fault_event);
    assert!(tg_fault.contains("供应商故障告警"));
    assert!(tg_fault.contains("anthropic-primary"));
    assert!(tg_fault.contains("529"));
    assert!(tg_fault.contains("anthropic-secondary"));

    // 4. MaintenanceAnnouncement
    let maint_event = NotificationEvent::MaintenanceAnnouncement {
        id: "maint-1".to_string(),
        title: "Database Index Migration".to_string(),
        content: "Gateway will undergo 5 minutes of scheduled read-only maintenance tonight."
            .to_string(),
        effective_time: ts,
    };
    let tg_maint = NotificationDispatcher::format_telegram(&maint_event);
    assert!(tg_maint.contains("维护公告"));
    assert!(tg_maint.contains("Database Index Migration"));
}

#[tokio::test]
async fn test_notification_multi_channel_dispatch() {
    let mock_server = MockServer::start().await;

    // 1. Setup Webhook mock
    Mock::given(method("POST"))
        .and(path("/webhook"))
        .and(header("X-Webhook-Secret", "my-secret-token"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock_server)
        .await;

    // 2. Setup Telegram mock
    Mock::given(method("POST"))
        .and(path("/bot-token-xyz/sendMessage"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock_server)
        .await;

    // 3. Setup Email mock
    Mock::given(method("POST"))
        .and(path("/send-email"))
        .and(header("Authorization", "Bearer sendgrid-token"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock_server)
        .await;

    let webhook_chan = NotificationChannel::Webhook {
        url: format!("{}/webhook", mock_server.uri()),
        secret_header: Some("X-Webhook-Secret".to_string()),
        secret_token: Some("my-secret-token".to_string()),
    };

    let tg_chan = NotificationChannel::Telegram {
        bot_token: "-token-xyz".to_string(),
        chat_id: "12345678".to_string(),
        custom_endpoint: Some(mock_server.uri()),
    };

    let email_chan = NotificationChannel::Email {
        endpoint_url: format!("{}/send-email", mock_server.uri()),
        recipient: "developer@example.com".to_string(),
        api_key: Some("sendgrid-token".to_string()),
    };

    let dispatcher = NotificationDispatcher::new()
        .with_channel(webhook_chan)
        .with_channel(tg_chan)
        .with_channel(email_chan);

    let client = reqwest::Client::new();
    let event = NotificationEvent::BalanceLow {
        card_id: "card-test-01".to_string(),
        current_credits: 800,
        threshold_credits: 2_000,
        ts: now_secs(),
    };

    let results = dispatcher.dispatch(&client, &event).await;
    assert_eq!(results.len(), 3);
    for r in results {
        assert!(r.is_ok());
    }

    // Verify history recording
    let history = dispatcher.list_history();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0], event);
}

#[test]
fn test_notification_history_capping() {
    let dispatcher = NotificationDispatcher::new();
    for i in 0..150 {
        dispatcher.record_event(NotificationEvent::BalanceLow {
            card_id: format!("card-{}", i),
            current_credits: i as i64,
            threshold_credits: 100,
            ts: now_secs(),
        });
    }

    let history = dispatcher.list_history();
    assert_eq!(history.len(), 100); // capped at max_history (100)
    if let NotificationEvent::BalanceLow { card_id, .. } = &history[99] {
        assert_eq!(card_id, "card-149");
    } else {
        panic!("Wrong event variant");
    }
}
