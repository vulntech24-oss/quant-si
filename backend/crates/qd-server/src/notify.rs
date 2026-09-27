//! Telegram notifications (ADR 0014, `docs/integrations/telegram.md`).
//!
//! `POST https://api.telegram.org/bot<token>/sendMessage` with `chat_id`
//! and `text`, plain text only. The bot token is a stored secret
//! (`telegram_bot_token`); it is part of the URL, so errors are reported
//! without the URL. The chat id and what is sent come from Settings →
//! Notifications, read on every message.

use std::sync::Arc;

use async_trait::async_trait;
use qd_app::monitor::{Alert, Severity};
use qd_app::ports::{Notifier, StoreError};
use serde_json::{Value, json};

use crate::runtime::Runtime;

/// The production Bot API.
pub const TELEGRAM_BASE: &str = "https://api.telegram.org";

/// Telegram's limit on one message.
const MAX_TEXT_CHARS: usize = 4096;

/// Sends messages through the owner's Telegram bot.
pub struct TelegramNotifier {
    runtime: Arc<Runtime>,
    base: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for TelegramNotifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramNotifier")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl TelegramNotifier {
    /// A notifier for `base` (use [`TELEGRAM_BASE`]).
    pub fn new(runtime: Arc<Runtime>, base: &str) -> Result<Self, StoreError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| StoreError(e.to_string()))?;
        Ok(Self {
            runtime,
            base: base.trim_end_matches('/').to_owned(),
            http,
        })
    }

    /// Sends when notifications are on (alerts, summaries); a no-op otherwise.
    pub async fn notify_if_enabled(&self, text: &str) {
        match self.runtime.effective().await {
            Ok(e) if e.notifications.enabled => {
                if let Err(err) = self.notify(text).await {
                    tracing::warn!(error = %err, "notification not sent");
                }
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(error = %err, "notification settings unreadable"),
        }
    }

    /// Forwards a raised alert at or above the configured severity.
    pub async fn alert(&self, alert: &Alert) {
        let Ok(e) = self.runtime.effective().await else {
            return;
        };
        let wanted = match e.notifications.min_severity.as_str() {
            "warning" => true,
            _ => alert.severity == Severity::Critical,
        };
        if e.notifications.enabled && wanted {
            let level = match alert.severity {
                Severity::Critical => "CRITICAL",
                Severity::Warning => "Warning",
            };
            if let Err(err) = self
                .notify(&format!("QuantDesk {level}: {}", alert.message))
                .await
            {
                tracing::warn!(error = %err, "alert notification not sent");
            }
        }
    }
}

#[async_trait]
impl Notifier for TelegramNotifier {
    /// Sends one message with the stored token and the configured chat id,
    /// whether or not notifications are switched on (the test button).
    async fn notify(&self, text: &str) -> Result<(), StoreError> {
        let e = self.runtime.effective().await?;
        let chat = e.notifications.telegram_chat_id;
        if chat.is_empty() {
            return Err(StoreError(
                "no Telegram chat id (Settings → Notifications)".to_owned(),
            ));
        }
        let token = self
            .runtime
            .secret("telegram_bot_token")
            .await
            .ok_or_else(|| {
                StoreError("the Telegram bot token is not set (Settings → API keys)".to_owned())
            })?;
        let text: String = text.chars().take(MAX_TEXT_CHARS).collect();
        let response = self
            .http
            .post(format!("{}/bot{token}/sendMessage", self.base))
            .json(&json!({ "chat_id": chat, "text": text }))
            .send()
            .await
            .map_err(|e| StoreError(format!("Telegram unreachable: {}", e.without_url())))?;
        let body: Value = response
            .json()
            .await
            .map_err(|e| StoreError(format!("Telegram answer unreadable: {}", e.without_url())))?;
        if body.get("ok") == Some(&Value::Bool(true)) {
            Ok(())
        } else {
            let why = body
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("no description");
            Err(StoreError(format!("Telegram refused the message: {why}")))
        }
    }
}
