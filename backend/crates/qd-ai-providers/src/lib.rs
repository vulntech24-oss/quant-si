//! Advisors backed by hosted models (ADR 0014,
//! `docs/integrations/ai-providers.md`): OpenAI and xAI through the
//! Responses API, Google Gemini through `generateContent`. Each call asks
//! for JSON that matches one schema, and the answer goes through the same
//! validation as every advisor's ([`qd_ai::advisor::finalize`]).
//!
//! Advisory only (INV-04): an advisor reads the decision packet, which holds
//! no equity, quantities or secrets, and returns commentary. The API key is
//! given as a string and only ever sent in the provider's auth header.

use std::time::Duration;

use async_trait::async_trait;
use qd_ai::advisor::{AdviceDraft, Advisor, AdvisorError, AdvisorInput};
use qd_app::journal::AiStance;
use rust_decimal::Decimal;
use serde_json::{Value, json};

/// Version of the prompt and schema; part of every advisor's name.
pub const PROMPT_VERSION: &str = "v1";

const INSTRUCTIONS: &str = "You review one proposed trade for the owner of a \
small systematic trading desk. You are advisory only: you cannot place, size \
or change anything. The packet holds the post-risk decision, the plan \
(entry, stop, target), its economics net of costs, outcome probabilities \
from historical evidence and the strongest argument against it. Judge \
whether the plan is sound: evidence strength, reward-to-risk after costs, \
stop placement, regime fit. Be concrete and brief. Answer with JSON only: \
stance (agree, caution, disagree or abstain), confidence between 0 and 1, \
a summary of at most 600 characters, and up to 5 short flags.";

/// A hosted model provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    /// OpenAI Responses API.
    OpenAi,
    /// xAI Responses API.
    Xai,
    /// Google Gemini `generateContent`.
    Gemini,
}

impl Provider {
    /// Short name.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Xai => "xai",
            Self::Gemini => "gemini",
        }
    }

    /// The production API base.
    #[must_use]
    pub const fn default_base(self) -> &'static str {
        match self {
            Self::OpenAi => "https://api.openai.com/v1",
            Self::Xai => "https://api.x.ai/v1",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta",
        }
    }
}

/// The JSON schema every provider must answer with.
#[must_use]
pub fn advice_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "stance": {"type": "string", "enum": ["agree", "caution", "disagree", "abstain"]},
            "confidence": {"type": "number"},
            "summary": {"type": "string"},
            "flags": {"type": "array", "items": {"type": "string"}},
        },
        "required": ["stance", "confidence", "summary", "flags"],
        "additionalProperties": false,
    })
}

/// The request body for a provider.
pub fn request_body(
    provider: Provider,
    model: &str,
    input: &AdvisorInput,
) -> Result<Value, AdvisorError> {
    let packet = serde_json::to_string(input).map_err(|e| AdvisorError::Failed(e.to_string()))?;
    Ok(match provider {
        Provider::OpenAi | Provider::Xai => json!({
            "model": model,
            "input": [
                {"role": "system", "content": INSTRUCTIONS},
                {"role": "user", "content": packet},
            ],
            "text": {"format": {
                "type": "json_schema",
                "name": "trade_advice",
                "schema": advice_schema(),
                "strict": true,
            }},
        }),
        Provider::Gemini => json!({
            "systemInstruction": {"parts": [{"text": INSTRUCTIONS}]},
            "contents": [{"role": "user", "parts": [{"text": packet}]}],
            "generationConfig": {
                "responseMimeType": "application/json",
                "responseJsonSchema": advice_schema(),
            },
        }),
    })
}

/// The model's JSON text from a provider response.
pub fn output_text(provider: Provider, body: &Value) -> Result<String, AdvisorError> {
    match provider {
        Provider::OpenAi | Provider::Xai => {
            let parts = body
                .get("output")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
                .flat_map(|item| {
                    item.get("content")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                });
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("output_text") => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            return Ok(text.to_owned());
                        }
                    }
                    Some("refusal") => {
                        let why = part.get("refusal").and_then(Value::as_str).unwrap_or("");
                        return Err(AdvisorError::Invalid(format!("the model refused: {why}")));
                    }
                    _ => {}
                }
            }
            Err(AdvisorError::Invalid("no output text".to_owned()))
        }
        Provider::Gemini => body
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                let reason = body
                    .pointer("/candidates/0/finishReason")
                    .or_else(|| body.pointer("/promptFeedback/blockReason"))
                    .and_then(Value::as_str)
                    .unwrap_or("no candidate");
                AdvisorError::Invalid(format!("no output text: {reason}"))
            }),
    }
}

/// Parses the advice JSON into a draft (bounds are applied by `finalize`).
pub fn parse_advice(text: &str) -> Result<AdviceDraft, AdvisorError> {
    let v: Value = serde_json::from_str(text.trim())
        .map_err(|e| AdvisorError::Invalid(format!("not JSON: {e}")))?;
    let stance = match v.get("stance").and_then(Value::as_str) {
        Some("agree") => AiStance::Agree,
        Some("caution") => AiStance::Caution,
        Some("disagree") => AiStance::Disagree,
        Some("abstain") => AiStance::Abstain,
        other => return Err(AdvisorError::Invalid(format!("unknown stance {other:?}"))),
    };
    let confidence = match v.get("confidence") {
        Some(Value::Number(n)) => n.to_string().parse::<Decimal>().ok(),
        _ => None,
    }
    .ok_or_else(|| AdvisorError::Invalid("confidence is not a number".to_owned()))?;
    let summary = v
        .get("summary")
        .and_then(Value::as_str)
        .ok_or_else(|| AdvisorError::Invalid("no summary".to_owned()))?
        .to_owned();
    let flags = v
        .get("flags")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(AdviceDraft {
        stance,
        confidence,
        summary,
        flags,
    })
}

/// An API key. Its `Debug` output is redacted.
#[derive(Clone)]
struct ApiKey(String);

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(***)")
    }
}

/// An advisor backed by one provider and model.
#[derive(Clone, Debug)]
pub struct ProviderAdvisor {
    provider: Provider,
    model: String,
    base: String,
    name: String,
    key: ApiKey,
    http: reqwest::Client,
}

impl ProviderAdvisor {
    /// An advisor for `model` at `base` (use [`Provider::default_base`]).
    pub fn new(
        provider: Provider,
        model: &str,
        base: &str,
        api_key: String,
        timeout: Duration,
    ) -> Result<Self, AdvisorError> {
        if model.trim().is_empty() || api_key.trim().is_empty() {
            return Err(AdvisorError::Failed(
                "model and API key are required".to_owned(),
            ));
        }
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| AdvisorError::Failed(e.to_string()))?;
        Ok(Self {
            provider,
            model: model.trim().to_owned(),
            base: base.trim_end_matches('/').to_owned(),
            name: format!("{}:{}:{PROMPT_VERSION}", provider.code(), model.trim()),
            key: ApiKey(api_key.trim().to_owned()),
            http,
        })
    }
}

#[async_trait]
impl Advisor for ProviderAdvisor {
    fn name(&self) -> &str {
        &self.name
    }

    async fn advise(&self, input: &AdvisorInput) -> Result<AdviceDraft, AdvisorError> {
        let body = request_body(self.provider, &self.model, input)?;
        let request = match self.provider {
            Provider::OpenAi | Provider::Xai => self
                .http
                .post(format!("{}/responses", self.base))
                .bearer_auth(&self.key.0),
            Provider::Gemini => self
                .http
                .post(format!(
                    "{}/models/{}:generateContent",
                    self.base, self.model
                ))
                .header("x-goog-api-key", &self.key.0),
        };
        let response = request.json(&body).send().await.map_err(|e| {
            if e.is_timeout() {
                AdvisorError::Timeout
            } else {
                AdvisorError::Failed(e.without_url().to_string())
            }
        })?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|e| AdvisorError::Failed(format!("HTTP {status}: {}", e.without_url())))?;
        if !status.is_success() {
            let message = body
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("no message");
            return Err(AdvisorError::Failed(format!("HTTP {status}: {message}")));
        }
        parse_advice(&output_text(self.provider, &body)?)
    }
}
