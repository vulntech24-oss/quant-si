//! Hosted models with tool calling (`docs/integrations/ai-providers.md`,
//! "Tool calling and web search").
//!
//! One provider-neutral transcript ([`Turn`]) is rendered for each API:
//!
//! - OpenAI and xAI, Responses API: the model's output items are sent back
//!   unchanged, followed by `function_call_output` items.
//! - Gemini, `generateContent`: the model's content is sent back unchanged
//!   (thought signatures survive), followed by a user turn of
//!   `functionResponse` parts.
//!
//! Web research is a separate call with only the provider's built-in search
//! tool (`web_search`, or `googleSearch` on Gemini). Its answer and cited
//! sources come back as data.

use std::time::Duration;

use async_trait::async_trait;
use qd_ai_providers::Provider;
use serde::Serialize;
use serde_json::{Value, json};
use thiserror::Error;

/// Why a model call failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LlmError {
    /// The call timed out.
    #[error("the model call timed out")]
    Timeout,
    /// The provider refused or failed.
    #[error("model call failed: {0}")]
    Failed(String),
    /// The answer had an unexpected shape.
    #[error("unexpected model answer: {0}")]
    Invalid(String),
}

/// A function the model may call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ToolSpec {
    /// Name.
    pub name: String,
    /// When to use it.
    pub description: String,
    /// JSON Schema of the arguments (an object).
    pub parameters: Value,
}

/// A call the model asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    /// Call id, echoed with the result.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Arguments.
    pub arguments: Value,
}

/// The result of one call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolResult {
    /// The call id.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Result (JSON).
    pub output: Value,
}

/// One turn of the transcript.
#[derive(Clone, Debug, PartialEq)]
pub enum Turn {
    /// Text from QuantDesk (the task).
    User(String),
    /// What the model returned, in the provider's own shape.
    Model(Value),
    /// Results of the model's calls.
    Results(Vec<ToolResult>),
}

/// What the model returned in one step.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelReply {
    /// Text, if any.
    pub text: String,
    /// Calls requested (none means the model is done).
    pub calls: Vec<ToolCall>,
    /// The raw turn to send back.
    pub raw: Value,
}

/// A cited source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Source {
    /// Title.
    pub title: String,
    /// URL.
    pub url: String,
}

/// The result of a web research call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Research {
    /// Answer text (untrusted web-derived content).
    pub answer: String,
    /// Cited sources.
    pub sources: Vec<Source>,
    /// Queries the provider ran.
    pub queries: Vec<String>,
}

/// A model that can call tools and research the web.
#[async_trait]
pub trait ChatModel: Send + Sync {
    /// `provider:model`.
    fn name(&self) -> &str;
    /// One step of the conversation.
    async fn step(
        &self,
        system: &str,
        transcript: &[Turn],
        tools: &[ToolSpec],
    ) -> Result<ModelReply, LlmError>;
    /// Researches a question on the web with the provider's search tool.
    async fn research(&self, query: &str) -> Result<Research, LlmError>;
}

const RESEARCH_INSTRUCTIONS: &str = "You are a financial research assistant \
for Indian markets (NSE, BSE, MCX). Search the web and answer the question \
factually and concisely: latest news, results, corporate actions, guidance, \
sector and macro context, with dates. Say when information is old or \
uncertain. Do not give trading instructions.";

/// The request body for one step.
#[must_use]
pub fn step_body(
    provider: Provider,
    model: &str,
    system: &str,
    transcript: &[Turn],
    tools: &[ToolSpec],
) -> Value {
    match provider {
        Provider::OpenAi | Provider::Xai => {
            let mut input = vec![json!({"role": "system", "content": system})];
            for turn in transcript {
                match turn {
                    Turn::User(text) => input.push(json!({"role": "user", "content": text})),
                    Turn::Model(raw) => {
                        if let Some(items) = raw.as_array() {
                            input.extend(items.iter().cloned());
                        }
                    }
                    Turn::Results(results) => {
                        input.extend(results.iter().map(|r| {
                            json!({
                                "type": "function_call_output",
                                "call_id": r.id,
                                "output": r.output.to_string(),
                            })
                        }));
                    }
                }
            }
            let tools: Vec<Value> = tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                        "strict": false,
                    })
                })
                .collect();
            json!({
                "model": model,
                "input": input,
                "tools": tools,
                "tool_choice": "auto",
                "parallel_tool_calls": true,
            })
        }
        Provider::Gemini => {
            let mut contents = Vec::new();
            for turn in transcript {
                match turn {
                    Turn::User(text) => {
                        contents.push(json!({"role": "user", "parts": [{"text": text}]}));
                    }
                    Turn::Model(raw) => contents.push(raw.clone()),
                    Turn::Results(results) => {
                        let parts: Vec<Value> = results
                            .iter()
                            .map(|r| {
                                json!({"functionResponse": {
                                    "name": r.name,
                                    "id": r.id,
                                    "response": {"result": r.output},
                                }})
                            })
                            .collect();
                        contents.push(json!({"role": "user", "parts": parts}));
                    }
                }
            }
            let declarations: Vec<Value> = tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "parametersJsonSchema": t.parameters,
                    })
                })
                .collect();
            json!({
                "systemInstruction": {"parts": [{"text": system}]},
                "contents": contents,
                "tools": [{"functionDeclarations": declarations}],
                "toolConfig": {"functionCallingConfig": {"mode": "AUTO"}},
            })
        }
    }
}

fn parse_arguments(raw: &Value) -> Value {
    match raw {
        Value::String(s) => serde_json::from_str(s).unwrap_or_else(|_| json!({"_raw": s})),
        other => other.clone(),
    }
}

/// Parses one step's response.
pub fn parse_step(provider: Provider, body: &Value) -> Result<ModelReply, LlmError> {
    match provider {
        Provider::OpenAi | Provider::Xai => {
            let items = body
                .get("output")
                .and_then(Value::as_array)
                .ok_or_else(|| LlmError::Invalid("no output".to_owned()))?;
            let mut text = String::new();
            let mut calls = Vec::new();
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("function_call") => calls.push(ToolCall {
                        id: item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        name: item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        arguments: parse_arguments(item.get("arguments").unwrap_or(&Value::Null)),
                    }),
                    Some("message") => {
                        for part in item
                            .get("content")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                        {
                            match part.get("type").and_then(Value::as_str) {
                                Some("output_text") => {
                                    text.push_str(
                                        part.get("text").and_then(Value::as_str).unwrap_or(""),
                                    );
                                }
                                Some("refusal") => {
                                    let why =
                                        part.get("refusal").and_then(Value::as_str).unwrap_or("");
                                    text.push_str(&format!("[refused: {why}]"));
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(ModelReply {
                text,
                calls,
                raw: Value::Array(items.clone()),
            })
        }
        Provider::Gemini => {
            let content = body.pointer("/candidates/0/content").ok_or_else(|| {
                let reason = body
                    .pointer("/candidates/0/finishReason")
                    .or_else(|| body.pointer("/promptFeedback/blockReason"))
                    .and_then(Value::as_str)
                    .unwrap_or("no candidate");
                LlmError::Invalid(format!("no content: {reason}"))
            })?;
            let mut text = String::new();
            let mut calls = Vec::new();
            for (n, part) in content
                .get("parts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                if let Some(call) = part.get("functionCall") {
                    let name = call
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let id = call
                        .get("id")
                        .and_then(Value::as_str)
                        .map_or_else(|| format!("call-{n}-{name}"), str::to_owned);
                    calls.push(ToolCall {
                        id,
                        name,
                        arguments: call.get("args").cloned().unwrap_or_else(|| json!({})),
                    });
                } else if part.get("thought").and_then(Value::as_bool) != Some(true) {
                    if let Some(t) = part.get("text").and_then(Value::as_str) {
                        text.push_str(t);
                    }
                }
            }
            let mut raw = content.clone();
            if raw.get("role").is_none() {
                if let Some(obj) = raw.as_object_mut() {
                    obj.insert("role".to_owned(), json!("model"));
                }
            }
            Ok(ModelReply { text, calls, raw })
        }
    }
}

/// The request body for a web research call.
#[must_use]
pub fn research_body(provider: Provider, model: &str, query: &str) -> Value {
    match provider {
        Provider::OpenAi | Provider::Xai => json!({
            "model": model,
            "input": [
                {"role": "system", "content": RESEARCH_INSTRUCTIONS},
                {"role": "user", "content": query},
            ],
            "tools": [{"type": "web_search"}],
            "store": false,
        }),
        Provider::Gemini => json!({
            "systemInstruction": {"parts": [{"text": RESEARCH_INSTRUCTIONS}]},
            "contents": [{"role": "user", "parts": [{"text": query}]}],
            "tools": [{"googleSearch": {}}],
        }),
    }
}

/// Parses a web research response.
pub fn parse_research(provider: Provider, body: &Value) -> Result<Research, LlmError> {
    let mut sources: Vec<Source> = Vec::new();
    let mut queries = Vec::new();
    let mut push = |title: &str, url: &str| {
        if !url.is_empty() && !sources.iter().any(|s| s.url == url) {
            sources.push(Source {
                title: title.to_owned(),
                url: url.to_owned(),
            });
        }
    };
    match provider {
        Provider::OpenAi | Provider::Xai => {
            let mut answer = String::new();
            for item in body
                .get("output")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                match item.get("type").and_then(Value::as_str) {
                    Some("web_search_call") => {
                        let action = item.get("action");
                        if let Some(q) = action.and_then(|a| a.get("query")).and_then(Value::as_str)
                        {
                            queries.push(q.to_owned());
                        }
                        for q in action
                            .and_then(|a| a.get("queries"))
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                        {
                            queries.push(q.to_owned());
                        }
                    }
                    Some("message") => {
                        for part in item
                            .get("content")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                        {
                            if let Some(t) = part.get("text").and_then(Value::as_str) {
                                answer.push_str(t);
                            }
                            for a in part
                                .get("annotations")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                            {
                                if a.get("type").and_then(Value::as_str) == Some("url_citation") {
                                    push(
                                        a.get("title").and_then(Value::as_str).unwrap_or(""),
                                        a.get("url").and_then(Value::as_str).unwrap_or(""),
                                    );
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            if answer.is_empty() {
                return Err(LlmError::Invalid("no research answer".to_owned()));
            }
            Ok(Research {
                answer,
                sources,
                queries,
            })
        }
        Provider::Gemini => {
            let answer: String = body
                .pointer("/candidates/0/content/parts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|p| p.get("thought").and_then(Value::as_bool) != Some(true))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect();
            if answer.is_empty() {
                return Err(LlmError::Invalid("no research answer".to_owned()));
            }
            let meta = body.pointer("/candidates/0/groundingMetadata");
            for chunk in meta
                .and_then(|m| m.get("groundingChunks"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(web) = chunk.get("web") {
                    push(
                        web.get("title").and_then(Value::as_str).unwrap_or(""),
                        web.get("uri").and_then(Value::as_str).unwrap_or(""),
                    );
                }
            }
            queries.extend(
                meta.and_then(|m| m.get("webSearchQueries"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned),
            );
            Ok(Research {
                answer,
                sources,
                queries,
            })
        }
    }
}

/// An API key; its `Debug` output is redacted.
#[derive(Clone)]
struct ApiKey(String);

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(***)")
    }
}

/// A hosted model.
#[derive(Clone, Debug)]
pub struct ProviderModel {
    provider: Provider,
    model: String,
    base: String,
    name: String,
    key: ApiKey,
    http: reqwest::Client,
}

impl ProviderModel {
    /// A model at `base` (use [`Provider::default_base`]).
    pub fn new(
        provider: Provider,
        model: &str,
        base: &str,
        api_key: String,
        timeout: Duration,
    ) -> Result<Self, LlmError> {
        if model.trim().is_empty() || api_key.trim().is_empty() {
            return Err(LlmError::Failed(
                "model and API key are required".to_owned(),
            ));
        }
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| LlmError::Failed(e.to_string()))?;
        Ok(Self {
            provider,
            model: model.trim().to_owned(),
            base: base.trim_end_matches('/').to_owned(),
            name: format!("{}:{}", provider.code(), model.trim()),
            key: ApiKey(api_key.trim().to_owned()),
            http,
        })
    }

    /// The provider.
    #[must_use]
    pub const fn provider(&self) -> Provider {
        self.provider
    }

    async fn post(&self, body: &Value) -> Result<Value, LlmError> {
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
        let response = request.json(body).send().await.map_err(|e| {
            if e.is_timeout() {
                LlmError::Timeout
            } else {
                LlmError::Failed(e.without_url().to_string())
            }
        })?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|e| LlmError::Failed(format!("HTTP {status}: {}", e.without_url())))?;
        if !status.is_success() {
            let message = body
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("no message");
            return Err(LlmError::Failed(format!("HTTP {status}: {message}")));
        }
        Ok(body)
    }
}

#[async_trait]
impl ChatModel for ProviderModel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn step(
        &self,
        system: &str,
        transcript: &[Turn],
        tools: &[ToolSpec],
    ) -> Result<ModelReply, LlmError> {
        let body = step_body(self.provider, &self.model, system, transcript, tools);
        parse_step(self.provider, &self.post(&body).await?)
    }

    async fn research(&self, query: &str) -> Result<Research, LlmError> {
        let body = research_body(self.provider, &self.model, query);
        parse_research(self.provider, &self.post(&body).await?)
    }
}
