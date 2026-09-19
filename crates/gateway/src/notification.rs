//! Notification subsystem for alerts and announcements (Spec §11, §14.6, P4-4).
//!
//! Supports dispatching alerts across:
//! - Webhook (HTTP POST with JSON payload & optional auth header)
//! - Telegram (Telegram Bot API sendMessage)
//! - Email (Transactional HTTP webhook / mailer API)
//!
//! Event triggers:
//! - `CardIssued`: New card created/issued
//! - `BalanceLow`: Card remaining credits under threshold
//! - `ProviderFault`: Upstream model provider errors / failover triggered
//! - `MaintenanceAnnouncement`: System maintenance broadcast

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Notification event domain payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum NotificationEvent {
    CardIssued {
        card_id: String,
        card_code_masked: String,
        initial_credits: i64,
        plan_name: String,
        ts: u64,
    },
    BalanceLow {
        card_id: String,
        current_credits: i64,
        threshold_credits: i64,
        ts: u64,
    },
    ProviderFault {
        provider_id: String,
        error_class: String,
        status_code: Option<u16>,
        failover_target: Option<String>,
        ts: u64,
    },
    MaintenanceAnnouncement {
        id: String,
        title: String,
        content: String,
        effective_time: u64,
    },
}

impl NotificationEvent {
    pub fn timestamp(&self) -> u64 {
        match self {
            Self::CardIssued { ts, .. } => *ts,
            Self::BalanceLow { ts, .. } => *ts,
            Self::ProviderFault { ts, .. } => *ts,
            Self::MaintenanceAnnouncement { effective_time, .. } => *effective_time,
        }
    }
}

/// Notification destination channel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "channel_type", rename_all = "snake_case")]
pub enum NotificationChannel {
    Webhook {
        url: String,
        secret_header: Option<String>,
        secret_token: Option<String>,
    },
    Telegram {
        bot_token: String,
        chat_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        custom_endpoint: Option<String>,
    },
    Email {
        endpoint_url: String,
        recipient: String,
        api_key: Option<String>,
    },
}

/// Notification dispatcher managing channels, formatting, and delivery.
#[derive(Debug, Clone, Default)]
pub struct NotificationDispatcher {
    channels: Arc<RwLock<Vec<NotificationChannel>>>,
    history: Arc<RwLock<VecDeque<NotificationEvent>>>,
    max_history: usize,
}

impl NotificationDispatcher {
    pub fn new() -> Self {
        Self {
            channels: Arc::new(RwLock::new(Vec::new())),
            history: Arc::new(RwLock::new(VecDeque::new())),
            max_history: 100,
        }
    }

    pub fn with_channel(self, channel: NotificationChannel) -> Self {
        self.add_channel(channel);
        self
    }

    pub fn add_channel(&self, channel: NotificationChannel) {
        if let Ok(mut chans) = self.channels.write() {
            chans.push(channel);
        }
    }

    pub fn list_channels(&self) -> Vec<NotificationChannel> {
        self.channels.read().map(|c| c.clone()).unwrap_or_default()
    }

    pub fn record_event(&self, event: NotificationEvent) {
        if let Ok(mut hist) = self.history.write() {
            if hist.len() >= self.max_history {
                hist.pop_front();
            }
            hist.push_back(event);
        }
    }

    pub fn list_history(&self) -> Vec<NotificationEvent> {
        self.history
            .read()
            .map(|h| h.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Format Telegram Markdown text for an event.
    pub fn format_telegram(event: &NotificationEvent) -> String {
        match event {
            NotificationEvent::CardIssued {
                card_id,
                card_code_masked,
                initial_credits,
                plan_name,
                ..
            } => {
                format!(
                    "🎫 *[Kiro BYOK] 卡密发放通知*\n\n- 卡号: `{}`\n- 卡密: `{}`\n- 额度: `{} 积分`\n- 套餐: *{}*",
                    card_id, card_code_masked, initial_credits, plan_name
                )
            }
            NotificationEvent::BalanceLow {
                card_id,
                current_credits,
                threshold_credits,
                ..
            } => {
                format!(
                    "⚠️ *[Kiro BYOK] 余额不足预警*\n\n- 卡号: `{}`\n- 当前余额: `{}` 积分\n- 预警阈值: `{}` 积分\n请及时续费充值以避免服务中断。",
                    card_id, current_credits, threshold_credits
                )
            }
            NotificationEvent::ProviderFault {
                provider_id,
                error_class,
                status_code,
                failover_target,
                ..
            } => {
                let status_str = status_code
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "N/A".to_string());
                let failover_str = failover_target.as_deref().unwrap_or("无可用备用节点");
                format!(
                    "🚨 *[Kiro BYOK] 供应商故障告警*\n\n- 供应商: `{}`\n- 状态码: `{}`\n- 错误类型: `{}`\n- 自动转移: `{}`",
                    provider_id, status_str, error_class, failover_str
                )
            }
            NotificationEvent::MaintenanceAnnouncement { title, content, .. } => {
                format!("📢 *[Kiro BYOK 维护公告] {}*\n\n{}", title, content)
            }
        }
    }

    /// Format email subject and body for an event.
    pub fn format_email(event: &NotificationEvent) -> (String, String) {
        match event {
            NotificationEvent::CardIssued {
                card_id,
                card_code_masked,
                initial_credits,
                plan_name,
                ..
            } => (
                "[Kiro BYOK] 您的新卡密已生成".to_string(),
                format!(
                    "尊敬的用户：\n\n您的卡密已成功发放：\n- 卡号：{}\n- 卡密：{}\n- 初始额度：{} 积分\n- 关联套餐：{}\n\n请在客户端卡密登录框中激活使用。",
                    card_id, card_code_masked, initial_credits, plan_name
                ),
            ),
            NotificationEvent::BalanceLow {
                card_id,
                current_credits,
                threshold_credits,
                ..
            } => (
                "[Kiro BYOK] 余额预警提示".to_string(),
                format!(
                    "尊敬的用户：\n\n您的卡密 {} 当前剩余额度仅剩 {} 积分（已低于预警阈值 {} 积分）。\n为了保证您的 Coding 对话与补全不中断，建议尽快充值。",
                    card_id, current_credits, threshold_credits
                ),
            ),
            NotificationEvent::ProviderFault {
                provider_id,
                error_class,
                failover_target,
                ..
            } => (
                format!("[Kiro BYOK 监控] 供应商 {} 异常", provider_id),
                format!(
                    "系统检测到供应商 {} 发生故障（{}），网关已尝试自动故障转移至：{}。",
                    provider_id,
                    error_class,
                    failover_target.as_deref().unwrap_or("无")
                ),
            ),
            NotificationEvent::MaintenanceAnnouncement { title, content, .. } => (
                format!("[Kiro BYOK 公告] {}", title),
                content.clone(),
            ),
        }
    }

    /// Dispatch an event to all configured channels asynchronously.
    pub async fn dispatch(
        &self,
        client: &reqwest::Client,
        event: &NotificationEvent,
    ) -> Vec<Result<String, String>> {
        self.record_event(event.clone());
        let channels = self.list_channels();

        let futures = channels.into_iter().map(|chan| {
            let event = event.clone();
            async move {
                match chan {
                    NotificationChannel::Webhook {
                        url,
                        secret_header,
                        secret_token,
                    } => {
                        let mut req = client.post(&url).json(&event);
                        if let (Some(h), Some(tok)) = (secret_header, secret_token) {
                            req = req.header(h, tok);
                        }
                        match req.send().await {
                            Ok(res) if res.status().is_success() => {
                                Ok(format!("Webhook sent to {url} [status: {}]", res.status()))
                            }
                            Ok(res) => {
                                Err(format!("Webhook to {url} returned status {}", res.status()))
                            }
                            Err(e) => Err(format!("Webhook to {url} failed: {e}")),
                        }
                    }
                    NotificationChannel::Telegram {
                        bot_token,
                        chat_id,
                        custom_endpoint,
                    } => {
                        let base = custom_endpoint
                            .as_deref()
                            .unwrap_or("https://api.telegram.org");
                        let tg_url =
                            format!("{}/bot{bot_token}/sendMessage", base.trim_end_matches('/'));
                        let text = Self::format_telegram(&event);
                        let body = serde_json::json!({
                            "chat_id": chat_id,
                            "text": text,
                            "parse_mode": "Markdown"
                        });
                        match client.post(&tg_url).json(&body).send().await {
                            Ok(res) if res.status().is_success() => {
                                Ok(format!("Telegram sent to chat_id {chat_id}"))
                            }
                            Ok(res) => Err(format!("Telegram returned status {}", res.status())),
                            Err(e) => Err(format!("Telegram send failed: {e}")),
                        }
                    }
                    NotificationChannel::Email {
                        endpoint_url,
                        recipient,
                        api_key,
                    } => {
                        let (subject, body) = Self::format_email(&event);
                        let mut req = client.post(&endpoint_url).json(&serde_json::json!({
                            "to": recipient,
                            "subject": subject,
                            "body": body
                        }));
                        if let Some(key) = api_key {
                            req = req.header("Authorization", format!("Bearer {key}"));
                        }
                        match req.send().await {
                            Ok(res) if res.status().is_success() => {
                                Ok(format!("Email sent to {recipient} via {endpoint_url}"))
                            }
                            Ok(res) => {
                                Err(format!("Email endpoint returned status {}", res.status()))
                            }
                            Err(e) => Err(format!("Email send failed: {e}")),
                        }
                    }
                }
            }
        });

        let wrapped_futures = futures.map(|fut| async move {
            match tokio::time::timeout(Duration::from_secs(10), fut).await {
                Ok(res) => res,
                Err(_) => Err("Timeout exceeded".to_string()),
            }
        });

        futures_util::future::join_all(wrapped_futures).await
    }
}

/// Helper function to create current timestamp.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
