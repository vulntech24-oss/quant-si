//! The catalog of secrets the owner can enter in the web UI (ADR 0013).
//!
//! Only names listed here can be stored. Values are encrypted at rest,
//! never returned to a browser, and read only by server-side adapters.

use serde::Serialize;

/// One known secret.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SecretSpec {
    /// Stable name.
    pub name: &'static str,
    /// Provider.
    pub provider: &'static str,
    /// Label.
    pub label: &'static str,
    /// Help for the owner.
    pub help: &'static str,
}

/// Every secret the system knows.
pub const SECRETS: &[SecretSpec] = &[
    SecretSpec {
        name: "kite_api_key",
        provider: "Zerodha Kite Connect",
        label: "API key",
        help: "From the Kite Connect developer console (your app's api_key).",
    },
    SecretSpec {
        name: "kite_api_secret",
        provider: "Zerodha Kite Connect",
        label: "API secret",
        help: "From the Kite Connect developer console. Used to exchange the daily login token.",
    },
    SecretSpec {
        name: "kite_access_token",
        provider: "Zerodha Kite Connect",
        label: "Access token (daily)",
        help: "Set by \"Login with Zerodha\" each day (tokens expire at 06:00 IST). You can also paste one.",
    },
    SecretSpec {
        name: "openai_api_key",
        provider: "OpenAI",
        label: "API key",
        help: "For the OpenAI advisor (Settings → Advisory AI). Advisory only (INV-04).",
    },
    SecretSpec {
        name: "gemini_api_key",
        provider: "Google Gemini",
        label: "API key",
        help: "For the Gemini advisor (Settings → Advisory AI). Advisory only (INV-04).",
    },
    SecretSpec {
        name: "xai_api_key",
        provider: "xAI",
        label: "API key",
        help: "For the xAI advisor (Settings → Advisory AI). Advisory only (INV-04).",
    },
    SecretSpec {
        name: "crypto_api_key",
        provider: "Crypto exchange",
        label: "API key",
        help: "For the crypto venue once it is chosen and its adapter exists. Use a key without withdrawal rights.",
    },
    SecretSpec {
        name: "crypto_api_secret",
        provider: "Crypto exchange",
        label: "API secret",
        help: "For the crypto venue once it is chosen and its adapter exists.",
    },
    SecretSpec {
        name: "telegram_bot_token",
        provider: "Telegram",
        label: "Bot token",
        help: "From @BotFather. Alerts and daily summaries go to the chat id in Settings → Notifications.",
    },
];

/// The spec for a name, if it is in the catalog.
#[must_use]
pub fn spec(name: &str) -> Option<&'static SecretSpec> {
    SECRETS.iter().find(|s| s.name == name)
}

/// Longest value accepted.
pub const MAX_SECRET_LEN: usize = 4096;

/// Checks a value before it is stored: not empty, not too long, no control
/// characters (catches pasted newlines and terminal escapes).
pub fn check_value(value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err("the value is empty".to_owned());
    }
    if value.len() > MAX_SECRET_LEN {
        return Err(format!("the value is longer than {MAX_SECRET_LEN} bytes"));
    }
    if value.chars().any(char::is_control) {
        return Err("the value contains control characters (a pasted newline?)".to_owned());
    }
    Ok(())
}
