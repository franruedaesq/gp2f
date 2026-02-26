//! LLM Provider abstraction and implementations.
//!
//! Implements Phase 8 requirements 3–5: a [`LlmProvider`] trait with
//! concrete implementations for OpenAI, Anthropic, and Groq, plus a
//! zero-dependency [`MockProvider`] used in tests.
//!
//! ## Secrets
//!
//! API keys are loaded from environment variables at startup.  In production
//! a secrets-rotation sidecar (AWS Secrets Manager / HashiCorp Vault) injects
//! them as environment variables before the pod starts.
//!
//! ## Compliance logging
//!
//! All LLM calls emit a tracing span with `prompt_hash` (BLAKE3 of the raw
//! prompt) and `provider`.  The raw prompt is **never** written to any log,
//! metric, or distributed trace.
//!
//! ## Retry policy
//!
//! Rate-limit (HTTP 429) and transient (5xx) errors are retried up to 3 times
//! with exponential back-off.  If the LLM returns invalid JSON or a response
//! with no tool call the result is `Ok(LlmResponse { tool_call: None, … })`;
//! the caller is responsible for treating this as a rejected proposal.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tool_gating::ToolDescriptor;

// ── types ─────────────────────────────────────────────────────────────────────

/// A single message in the LLM conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: String,
    pub content: String,
}

/// Request sent to an LLM provider.
#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub messages: Vec<LlmMessage>,
    /// Tools visible to the LLM (already filtered by [`ToolGatingService`]).
    pub tools: Vec<ToolDescriptor>,
    /// Must be 0 for deterministic tool-call selection.
    pub temperature: f32,
    /// Maximum response tokens.
    pub max_tokens: u32,
}

/// A tool call chosen by the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// The ephemeral `tool_req_*` id chosen by the LLM.
    pub tool_id: String,
    /// Arguments as a JSON object.
    pub arguments: Value,
}

/// Response from an LLM provider.
#[derive(Debug, Clone)]
pub struct LlmResponse {
    /// The tool call chosen by the LLM, if any.
    pub tool_call: Option<ToolCall>,
    /// BLAKE3 hex digest of the raw prompt (for compliance logging).
    pub prompt_hash: String,
    /// Provider name for telemetry.
    pub provider: String,
}

/// Errors returned by [`LlmProvider::complete`].
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("rate limited by provider; retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },

    #[error("transient provider error: {0}")]
    Transient(String),

    #[error("invalid response from provider: {0}")]
    InvalidResponse(String),

    #[error("provider not configured: {0}")]
    NotConfigured(String),

    #[error("HTTP request error: {0}")]
    Request(String),
}

// ── trait ─────────────────────────────────────────────────────────────────────

/// Abstraction over LLM providers.
///
/// Implementors: [`MockProvider`], [`OpenAiProvider`], [`AnthropicProvider`],
/// [`GroqProvider`].
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Call the LLM with `request` and return a structured response.
    async fn complete(&self, request: &LlmRequest) -> Result<LlmResponse, LlmError>;

    /// Return the provider name used in telemetry spans.
    fn name(&self) -> &'static str;
}

// ── mock provider ─────────────────────────────────────────────────────────────

/// No-op provider for unit tests and environments without an API key.
///
/// Returns the first tool in the allowed list as the chosen call (or `None`
/// when the list is empty).
pub struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    async fn complete(&self, request: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let tool_call = request.tools.first().map(|t| ToolCall {
            tool_id: t.tool_id.clone(),
            arguments: serde_json::json!({}),
        });
        Ok(LlmResponse {
            tool_call,
            prompt_hash: prompt_hash(request),
            provider: "mock".into(),
        })
    }

    fn name(&self) -> &'static str {
        "mock"
    }
}

// ── OpenAI provider ───────────────────────────────────────────────────────────

/// OpenAI provider.  Reads `OPENAI_API_KEY` and optionally `OPENAI_MODEL`
/// from the environment.
pub struct OpenAiProvider {
    api_key: String,
    model: String,
}

impl OpenAiProvider {
    /// Create from environment variables.  Returns `None` when `OPENAI_API_KEY`
    /// is not set.
    pub fn from_env() -> Option<Self> {
        std::env::var("OPENAI_API_KEY").ok().map(|key| Self {
            api_key: key,
            model: std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o".into()),
        })
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn complete(&self, request: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let phash = prompt_hash(request);
        tracing::info!(prompt_hash = %phash, provider = "openai", "LLM call");

        let tools_json = openai_tools_json(&request.tools);
        let body = serde_json::json!({
            "model": self.model,
            "messages": request.messages,
            "tools": tools_json,
            "tool_choice": "auto",
            "temperature": request.temperature,
            "max_tokens": request.max_tokens,
        });

        let resp = send_with_retry(
            "https://api.openai.com/v1/chat/completions",
            &self.api_key,
            &body,
        )
        .await?;

        Ok(LlmResponse {
            tool_call: parse_openai_tool_call(&resp)?,
            prompt_hash: phash,
            provider: "openai".into(),
        })
    }

    fn name(&self) -> &'static str {
        "openai"
    }
}

// ── Anthropic provider ────────────────────────────────────────────────────────

/// Anthropic provider.  Reads `ANTHROPIC_API_KEY` and optionally
/// `ANTHROPIC_MODEL` from the environment.
pub struct AnthropicProvider {
    api_key: String,
    model: String,
}

impl AnthropicProvider {
    /// Create from environment variables.  Returns `None` when
    /// `ANTHROPIC_API_KEY` is not set.
    pub fn from_env() -> Option<Self> {
        std::env::var("ANTHROPIC_API_KEY").ok().map(|key| Self {
            api_key: key,
            model: std::env::var("ANTHROPIC_MODEL")
                .unwrap_or_else(|_| "claude-3-5-sonnet-20241022".into()),
        })
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn complete(&self, request: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let phash = prompt_hash(request);
        tracing::info!(prompt_hash = %phash, provider = "anthropic", "LLM call");

        let tools_json: Vec<Value> = request
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.tool_id,
                    "description": t.description,
                    "input_schema": t.parameters.0,
                })
            })
            .collect();

        let system = request
            .messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.clone())
            .unwrap_or_default();

        let messages: Vec<&LlmMessage> = request
            .messages
            .iter()
            .filter(|m| m.role != "system")
            .collect();

        let body = serde_json::json!({
            "model": self.model,
            "system": system,
            "messages": messages,
            "tools": tools_json,
            "max_tokens": request.max_tokens,
        });

        let resp = send_anthropic_with_retry(
            "https://api.anthropic.com/v1/messages",
            &self.api_key,
            &body,
        )
        .await?;

        Ok(LlmResponse {
            tool_call: parse_anthropic_tool_call(&resp)?,
            prompt_hash: phash,
            provider: "anthropic".into(),
        })
    }

    fn name(&self) -> &'static str {
        "anthropic"
    }
}

// ── Groq provider ─────────────────────────────────────────────────────────────

/// Groq provider (OpenAI-compatible API).  Reads `GROQ_API_KEY` and
/// optionally `GROQ_MODEL` from the environment.
pub struct GroqProvider {
    api_key: String,
    model: String,
}

impl GroqProvider {
    /// Create from environment variables.  Returns `None` when `GROQ_API_KEY`
    /// is not set.
    pub fn from_env() -> Option<Self> {
        std::env::var("GROQ_API_KEY").ok().map(|key| Self {
            api_key: key,
            model: std::env::var("GROQ_MODEL").unwrap_or_else(|_| "llama-3.3-70b-versatile".into()),
        })
    }
}

#[async_trait]
impl LlmProvider for GroqProvider {
    async fn complete(&self, request: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let phash = prompt_hash(request);
        tracing::info!(prompt_hash = %phash, provider = "groq", "LLM call");

        let tools_json = openai_tools_json(&request.tools);
        let body = serde_json::json!({
            "model": self.model,
            "messages": request.messages,
            "tools": tools_json,
            "tool_choice": "auto",
            "temperature": request.temperature,
            "max_tokens": request.max_tokens,
        });

        let resp = send_with_retry(
            "https://api.groq.com/openai/v1/chat/completions",
            &self.api_key,
            &body,
        )
        .await?;

        Ok(LlmResponse {
            tool_call: parse_openai_tool_call(&resp)?,
            prompt_hash: phash,
            provider: "groq".into(),
        })
    }

    fn name(&self) -> &'static str {
        "groq"
    }
}

// ── factory ───────────────────────────────────────────────────────────────────

/// Build the active LLM provider from environment variables.
///
/// Priority: OpenAI → Anthropic → Groq → Mock.
/// Falls back to [`MockProvider`] when no API key is configured.
pub fn build_provider() -> Box<dyn LlmProvider> {
    if let Some(p) = OpenAiProvider::from_env() {
        tracing::info!(provider = "openai", "LLM provider configured");
        return Box::new(p);
    }
    if let Some(p) = AnthropicProvider::from_env() {
        tracing::info!(provider = "anthropic", "LLM provider configured");
        return Box::new(p);
    }
    if let Some(p) = GroqProvider::from_env() {
        tracing::info!(provider = "groq", "LLM provider configured");
        return Box::new(p);
    }
    tracing::info!("No LLM API key configured; using mock provider");
    Box::new(MockProvider)
}

// ── HTTP helpers ──────────────────────────────────────────────────────────────

/// Send a request to an OpenAI-compatible endpoint with exponential back-off.
///
/// Retries up to 3 times on rate-limit (HTTP 429) or transient (5xx) errors.
async fn send_with_retry(url: &str, api_key: &str, body: &Value) -> Result<Value, LlmError> {
    let client = reqwest::Client::new();
    let mut delay_secs = 1u64;

    for attempt in 0..3u32 {
        let resp = client
            .post(url)
            .bearer_auth(api_key)
            .json(body)
            .send()
            .await
            .map_err(|e| LlmError::Request(e.to_string()))?;

        let status = resp.status();

        if status.as_u16() == 429 {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok())
                .unwrap_or(delay_secs);
            if attempt == 2 {
                return Err(LlmError::RateLimited {
                    retry_after_secs: retry_after,
                });
            }
            tokio::time::sleep(std::time::Duration::from_secs(retry_after)).await;
            delay_secs *= 2;
            continue;
        }

        if !status.is_success() {
            let msg = resp.text().await.unwrap_or_default();
            if attempt == 2 {
                return Err(LlmError::Transient(format!("HTTP {status}: {msg}")));
            }
            tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
            delay_secs *= 2;
            continue;
        }

        return resp
            .json::<Value>()
            .await
            .map_err(|e| LlmError::InvalidResponse(e.to_string()));
    }

    Err(LlmError::Transient("max retries exceeded".into()))
}

/// Send a request to the Anthropic messages API with exponential back-off.
async fn send_anthropic_with_retry(
    url: &str,
    api_key: &str,
    body: &Value,
) -> Result<Value, LlmError> {
    let client = reqwest::Client::new();
    let mut delay_secs = 1u64;

    for attempt in 0..3u32 {
        let resp = client
            .post(url)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .json(body)
            .send()
            .await
            .map_err(|e| LlmError::Request(e.to_string()))?;

        let status = resp.status();

        if status.as_u16() == 429 {
            if attempt == 2 {
                return Err(LlmError::RateLimited {
                    retry_after_secs: delay_secs,
                });
            }
            tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
            delay_secs *= 2;
            continue;
        }

        if !status.is_success() {
            let msg = resp.text().await.unwrap_or_default();
            if attempt == 2 {
                return Err(LlmError::Transient(format!("HTTP {status}: {msg}")));
            }
            tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
            delay_secs *= 2;
            continue;
        }

        return resp
            .json::<Value>()
            .await
            .map_err(|e| LlmError::InvalidResponse(e.to_string()));
    }

    Err(LlmError::Transient("max retries exceeded".into()))
}

/// Build an OpenAI-format tools array from [`ToolDescriptor`] list.
fn openai_tools_json(tools: &[ToolDescriptor]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.tool_id,
                    "description": t.description,
                    "parameters": t.parameters.0,
                }
            })
        })
        .collect()
}

/// Parse a tool call from an OpenAI-format response.
fn parse_openai_tool_call(response: &Value) -> Result<Option<ToolCall>, LlmError> {
    let choices = response["choices"]
        .as_array()
        .ok_or_else(|| LlmError::InvalidResponse("no 'choices' field in response".into()))?;

    let choice = choices
        .first()
        .ok_or_else(|| LlmError::InvalidResponse("empty choices array".into()))?;

    let tool_calls = &choice["message"]["tool_calls"];
    if tool_calls.is_null() || tool_calls.as_array().map(|a| a.is_empty()).unwrap_or(true) {
        return Ok(None);
    }

    let tc = &tool_calls[0];
    let tool_id = tc["function"]["name"]
        .as_str()
        .ok_or_else(|| LlmError::InvalidResponse("tool call missing 'name'".into()))?
        .to_owned();

    let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
    let arguments = serde_json::from_str(args_str)
        .map_err(|e| LlmError::InvalidResponse(format!("invalid tool arguments JSON: {e}")))?;

    Ok(Some(ToolCall { tool_id, arguments }))
}

/// Parse a tool call from an Anthropic-format response.
fn parse_anthropic_tool_call(response: &Value) -> Result<Option<ToolCall>, LlmError> {
    let content = response["content"].as_array().ok_or_else(|| {
        LlmError::InvalidResponse("no 'content' field in Anthropic response".into())
    })?;

    for block in content {
        if block["type"] == "tool_use" {
            let tool_id = block["name"]
                .as_str()
                .ok_or_else(|| LlmError::InvalidResponse("tool_use block missing 'name'".into()))?
                .to_owned();
            let arguments = block["input"].clone();
            return Ok(Some(ToolCall { tool_id, arguments }));
        }
    }

    Ok(None)
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// BLAKE3 hex digest of the full prompt (for compliance logging).
///
/// The hash is included in every tracing span; the raw prompt is not.
pub fn prompt_hash(request: &LlmRequest) -> String {
    let combined: String = request
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    blake3::hash(combined.as_bytes()).to_hex().to_string()
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_gating::{JsonSchema, ToolDescriptor};

    fn dummy_tools() -> Vec<ToolDescriptor> {
        vec![ToolDescriptor {
            tool_id: "tool_req_test_abc123".into(),
            description: "A test tool".into(),
            parameters: JsonSchema(serde_json::json!({ "type": "object" })),
        }]
    }

    fn dummy_request(tools: Vec<ToolDescriptor>) -> LlmRequest {
        LlmRequest {
            messages: vec![LlmMessage {
                role: "user".into(),
                content: "what next?".into(),
            }],
            tools,
            temperature: 0.0,
            max_tokens: 512,
        }
    }

    #[tokio::test]
    async fn mock_provider_returns_first_tool() {
        let provider = MockProvider;
        let req = dummy_request(dummy_tools());
        let resp = provider.complete(&req).await.unwrap();
        let tc = resp.tool_call.unwrap();
        assert_eq!(tc.tool_id, "tool_req_test_abc123");
        assert_eq!(resp.provider, "mock");
    }

    #[tokio::test]
    async fn mock_provider_returns_none_when_no_tools() {
        let provider = MockProvider;
        let req = dummy_request(vec![]);
        let resp = provider.complete(&req).await.unwrap();
        assert!(resp.tool_call.is_none());
    }

    #[test]
    fn mock_provider_name_is_mock() {
        assert_eq!(MockProvider.name(), "mock");
    }

    #[test]
    fn prompt_hash_is_deterministic() {
        let req = dummy_request(dummy_tools());
        assert_eq!(prompt_hash(&req), prompt_hash(&req));
    }

    #[test]
    fn prompt_hash_differs_for_different_content() {
        let req1 = dummy_request(dummy_tools());
        let mut req2 = dummy_request(dummy_tools());
        req2.messages[0].content = "different prompt".into();
        assert_ne!(prompt_hash(&req1), prompt_hash(&req2));
    }

    #[test]
    fn parse_openai_tool_call_extracts_tool() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "function": {
                            "name": "tool_req_test_abc123",
                            "arguments": "{\"key\": \"value\"}"
                        }
                    }]
                }
            }]
        });
        let tc = parse_openai_tool_call(&response).unwrap().unwrap();
        assert_eq!(tc.tool_id, "tool_req_test_abc123");
        assert_eq!(tc.arguments["key"], "value");
    }

    #[test]
    fn parse_openai_tool_call_returns_none_when_no_tools() {
        let response = serde_json::json!({
            "choices": [{ "message": { "tool_calls": null } }]
        });
        let tc = parse_openai_tool_call(&response).unwrap();
        assert!(tc.is_none());
    }

    #[test]
    fn parse_openai_tool_call_errors_on_missing_choices() {
        let response = serde_json::json!({ "not_choices": [] });
        assert!(parse_openai_tool_call(&response).is_err());
    }

    #[test]
    fn parse_anthropic_tool_call_extracts_tool_use_block() {
        let response = serde_json::json!({
            "content": [
                { "type": "text", "text": "Let me help you." },
                {
                    "type": "tool_use",
                    "name": "tool_req_test_abc123",
                    "input": { "vibe_intent": "confused" }
                }
            ]
        });
        let tc = parse_anthropic_tool_call(&response).unwrap().unwrap();
        assert_eq!(tc.tool_id, "tool_req_test_abc123");
        assert_eq!(tc.arguments["vibe_intent"], "confused");
    }

    #[test]
    fn parse_anthropic_tool_call_returns_none_when_no_tool_use() {
        let response = serde_json::json!({
            "content": [{ "type": "text", "text": "No tool needed." }]
        });
        let tc = parse_anthropic_tool_call(&response).unwrap();
        assert!(tc.is_none());
    }

    #[test]
    fn build_provider_returns_mock_when_no_key_set() {
        // Remove all LLM keys from the environment for this test.
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("GROQ_API_KEY");
        let p = build_provider();
        assert_eq!(p.name(), "mock");
    }
}
