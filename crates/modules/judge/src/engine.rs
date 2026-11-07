//! What answers a judge: an engine that takes a question and a JSON schema and returns
//! JSON. The built-in one asks an LLM through an `llm.endpoint@1`; a node wired into
//! `engine` (`judge.engine@1`) replaces it without touching this crate.

use std::time::Duration;

use regulus_contract_llm_endpoint::{protocol, LlmEndpoint};
use regulus_core::{Endpoint, ModuleCtx};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The capability of a pluggable engine: its node serves [`ASK`].
pub const ENGINE_CAPABILITY: &str = "judge.engine@1";
/// `NodeCall` with an [`Ask`] in `args` → the JSON answer, which the judge checks
/// against `schema` itself.
pub const ASK: &str = "judge.ask";

/// One question to an engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ask {
    pub system: String,
    pub prompt: String,
    /// A short identifier for the schema (`verdict`, `label`…): a function name for
    /// servers that need one.
    pub schema_name: String,
    /// The JSON schema the answer must fit.
    pub schema: Value,
}

/// Why an engine gave no answer, and whether asking again may help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// It answered, but not with JSON. Asked again, told so.
    NotJson(String),
    /// A failure on the way (network, timeout, a 5xx). Asked again as it was.
    Transient(String),
    /// Asking again will not help (a 401, 403, 429, a bad request, a wrong setting).
    Fatal(String),
}

impl EngineError {
    pub fn message(&self) -> &str {
        match self {
            Self::NotJson(m) | Self::Transient(m) | Self::Fatal(m) => m,
        }
    }
}

pub trait Engine {
    /// The answer, parsed as JSON. Whether it fits the schema is the caller's business.
    fn ask(&self, ask: &Ask) -> Result<Value, EngineError>;
}

/// How the built-in engine asks for structured output on the openai protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `response_format` json_schema, then a forced tool call if the server refuses it.
    Auto,
    Schema,
    Tool,
}

impl Mode {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "auto" => Ok(Self::Auto),
            "schema" => Ok(Self::Schema),
            "tool" => Ok(Self::Tool),
            other => Err(format!("`mode` is auto, schema or tool, not `{other}`")),
        }
    }
}

/// Settings of the built-in engine, from the node's fields.
#[derive(Debug, Clone)]
pub struct LlmSettings {
    pub model: Option<String>,
    pub mode: Mode,
    pub temperature: f64,
    pub max_tokens: u64,
    pub timeout: Duration,
}

/// The `anthropic-version` header the Messages API requires: part of the protocol, not a
/// setting.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Asks an OpenAI-compatible (`openai`) or Anthropic (`anthropic`) endpoint directly.
pub struct LlmEngine {
    endpoint: LlmEndpoint,
    model: String,
    settings: LlmSettings,
    agent: ureq::Agent,
}

/// Statuses after which `auto` retries with a forced tool call: the server did not take
/// `response_format`. Others (401, 403, 429…) say nothing about the format.
const FORMAT_REFUSED: &[u16] = &[400, 404, 415, 422];
/// Most characters of a server's answer kept in an error.
const MAX_ERROR_CHARS: usize = 300;
/// Query parameters whose values never show in an error.
const SECRET_QUERY: &[&str] = &["key", "token", "api_key", "apikey", "access_token"];

/// A failed request: the HTTP status if there was one.
struct Failure {
    status: Option<u16>,
    message: String,
}

impl Failure {
    fn other(message: String) -> Self {
        Self {
            status: None,
            message,
        }
    }

    fn into_error(self) -> EngineError {
        match self.status {
            Some(status) if (400..500).contains(&status) => EngineError::Fatal(self.message),
            _ => EngineError::Transient(self.message),
        }
    }
}

/// `text` cut to [`MAX_ERROR_CHARS`], with every value of `secrets` and of the secret
/// query parameters replaced by `***`.
pub fn redact(text: &str, secrets: &[&str]) -> String {
    let mut out: String = text.chars().take(MAX_ERROR_CHARS).collect();
    if text.chars().count() > MAX_ERROR_CHARS {
        out.push('…');
    }
    for secret in secrets.iter().filter(|s| s.len() >= 4) {
        out = out.replace(secret, "***");
    }
    let mut redacted = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(eq) = rest.find('=') {
        let name_start = rest[..eq]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .map_or(0, |i| i + 1);
        let name = rest[name_start..eq].to_ascii_lowercase();
        redacted.push_str(&rest[..=eq]);
        rest = &rest[eq + 1..];
        if SECRET_QUERY.contains(&name.as_str()) {
            let end = rest
                .find(|c: char| c == '&' || c == '"' || c.is_whitespace())
                .unwrap_or(rest.len());
            redacted.push_str("***");
            rest = &rest[end..];
        }
    }
    redacted.push_str(rest);
    redacted
}

/// An HTTP agent with the node's timeout; one per node, reused for every value.
pub fn agent(timeout: Duration) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build();
    ureq::Agent::new_with_config(config)
}

impl LlmEngine {
    pub fn new(
        endpoint: LlmEndpoint,
        settings: LlmSettings,
        agent: ureq::Agent,
    ) -> Result<Self, String> {
        if endpoint.protocol != protocol::OPENAI && endpoint.protocol != protocol::ANTHROPIC {
            return Err(format!(
                "the LLM engine speaks `{}` and `{}`, not `{}`",
                protocol::OPENAI,
                protocol::ANTHROPIC,
                endpoint.protocol
            ));
        }
        let model = settings
            .model
            .clone()
            .or_else(|| endpoint.models.first().cloned())
            .ok_or("no model: set `model` on the node or on its provider")?;
        Ok(Self {
            endpoint,
            model,
            settings,
            agent,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.endpoint.base_url.trim_end_matches('/'))
    }

    /// The header values an error must never show: the credential, whole and without
    /// its `Bearer ` prefix.
    fn secrets(&self) -> Vec<&str> {
        self.endpoint
            .headers
            .iter()
            .filter(|(name, _)| {
                let name = name.to_ascii_lowercase();
                name == "authorization" || name.contains("key") || name.contains("token")
            })
            .flat_map(|(_, value)| [value.as_str(), value.trim_start_matches("Bearer ")])
            .collect()
    }

    fn post(&self, url: &str, body: &Value, extra: &[(&str, &str)]) -> Result<Value, Failure> {
        let secrets = self.secrets();
        let clean = |text: String| redact(&text, &secrets);
        let mut request = self.agent.post(url);
        for (name, value) in &self.endpoint.headers {
            request = request.header(name, value);
        }
        for (name, value) in extra {
            request = request.header(*name, *value);
        }
        let mut response = request
            .send_json(body)
            .map_err(|e| Failure::other(clean(format!("{url}: {e}"))))?;
        let status = response.status();
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|e| Failure::other(clean(format!("{url}: {e}"))))?;
        if !status.is_success() {
            return Err(Failure {
                status: Some(status.as_u16()),
                message: clean(format!("{url} answered {status}: {text}")),
            });
        }
        serde_json::from_str(&text)
            .map_err(|e| Failure::other(clean(format!("{url}: not JSON: {e}"))))
    }

    /// The answer, or the error with the HTTP status that caused it, if any.
    fn openai(&self, ask: &Ask, mode: Mode) -> Result<Value, (Option<u16>, EngineError)> {
        let mut body = json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": ask.system },
                { "role": "user", "content": ask.prompt },
            ],
            "temperature": self.settings.temperature,
            "max_tokens": self.settings.max_tokens,
        });
        if mode == Mode::Tool {
            body["tools"] = json!([{ "type": "function", "function": {
                "name": ask.schema_name, "parameters": ask.schema } }]);
            body["tool_choice"] =
                json!({ "type": "function", "function": { "name": ask.schema_name } });
        } else {
            body["response_format"] = json!({ "type": "json_schema", "json_schema": {
                "name": ask.schema_name, "strict": true, "schema": ask.schema } });
        }
        let answer = self
            .post(&self.url("chat/completions"), &body, &[])
            .map_err(|failure| (failure.status, failure.into_error()))?;
        let message = &answer["choices"][0]["message"];
        let text = match mode {
            Mode::Tool => message["tool_calls"][0]["function"]["arguments"].as_str(),
            _ => message["content"].as_str(),
        }
        .ok_or_else(|| {
            let error = redact(&format!("no answer in {answer}"), &[]);
            (None, EngineError::NotJson(error))
        })?;
        parse_answer(text).map_err(|e| (None, EngineError::NotJson(e)))
    }

    fn anthropic(&self, ask: &Ask) -> Result<Value, EngineError> {
        let body = json!({
            "model": self.model,
            "max_tokens": self.settings.max_tokens,
            "temperature": self.settings.temperature,
            "system": ask.system,
            "messages": [{ "role": "user", "content": ask.prompt }],
            "tools": [{ "name": ask.schema_name, "input_schema": ask.schema }],
            "tool_choice": { "type": "tool", "name": ask.schema_name },
        });
        let version = [("anthropic-version", ANTHROPIC_VERSION)];
        let answer = self
            .post(&self.url("v1/messages"), &body, &version)
            .map_err(Failure::into_error)?;
        answer["content"]
            .as_array()
            .and_then(|blocks| blocks.iter().find(|b| b["type"] == "tool_use"))
            .map(|block| block["input"].clone())
            .ok_or_else(|| EngineError::NotJson(redact(&format!("no tool_use in {answer}"), &[])))
    }
}

/// A model's text as JSON, tolerating a ```json fence around it.
pub fn parse_answer(text: &str) -> Result<Value, String> {
    let trimmed = text.trim();
    let inner = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|rest| rest.strip_suffix("```"))
        .unwrap_or(trimmed);
    serde_json::from_str(inner.trim())
        .map_err(|e| redact(&format!("the answer is not JSON ({e}): {text}"), &[]))
}

impl Engine for LlmEngine {
    fn ask(&self, ask: &Ask) -> Result<Value, EngineError> {
        if self.endpoint.protocol == protocol::ANTHROPIC {
            return self.anthropic(ask);
        }
        match self.settings.mode {
            // Only a refused format (`FORMAT_REFUSED`) falls back to a tool call; a 401,
            // 403 or 429 is surfaced as it is.
            Mode::Auto => match self.openai(ask, Mode::Schema) {
                Err((Some(status), _)) if FORMAT_REFUSED.contains(&status) => {
                    self.openai(ask, Mode::Tool).map_err(|(_, e)| e)
                }
                other => other.map_err(|(_, e)| e),
            },
            mode => self.openai(ask, mode).map_err(|(_, e)| e),
        }
    }
}

/// An engine behind a "uses" wire: whatever node serves [`ASK`].
pub struct WiredEngine {
    pub ctx: ModuleCtx,
    pub endpoint: Endpoint,
}

impl Engine for WiredEngine {
    fn ask(&self, ask: &Ask) -> Result<Value, EngineError> {
        let args = serde_json::to_value(ask).map_err(|e| EngineError::Fatal(e.to_string()))?;
        self.ctx
            .call(&self.endpoint, ASK, args)
            .map_err(|e| EngineError::Transient(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_hides_secrets_and_secret_query_values_and_cuts_long_text() {
        let text = "http://h/v1?key=abc123&x=1&token=t0k answered 401: Bearer sk-live-1 sk-live-1";
        let clean = redact(text, &["Bearer sk-live-1", "sk-live-1"]);
        assert!(
            !clean.contains("abc123") && !clean.contains("t0k"),
            "{clean}"
        );
        assert!(!clean.contains("sk-live-1"), "{clean}");
        assert!(clean.contains("key=***&x=1&token=***"), "{clean}");
        let long = redact(&"y".repeat(1000), &[]);
        assert_eq!(long.chars().count(), MAX_ERROR_CHARS + 1);
    }

    #[test]
    fn answers_may_come_fenced() {
        assert_eq!(
            parse_answer("```json\n{\"a\":1}\n```").unwrap(),
            json!({ "a": 1 })
        );
        assert_eq!(parse_answer(" {\"a\":1} ").unwrap(), json!({ "a": 1 }));
        assert!(parse_answer("pass").is_err());
    }

    #[test]
    fn unknown_protocols_and_missing_models_are_refused() {
        let settings = LlmSettings {
            model: None,
            mode: Mode::Auto,
            temperature: 0.0,
            max_tokens: 10,
            timeout: Duration::from_secs(1),
        };
        let mut endpoint = LlmEndpoint {
            protocol: "bedrock".into(),
            base_url: "http://127.0.0.1:1".into(),
            ..LlmEndpoint::default()
        };
        assert!(
            LlmEngine::new(endpoint.clone(), settings.clone(), agent(Duration::ZERO))
                .err()
                .unwrap()
                .contains("bedrock")
        );
        endpoint.protocol = protocol::OPENAI.into();
        assert!(LlmEngine::new(endpoint, settings, agent(Duration::ZERO))
            .err()
            .unwrap()
            .contains("no model"));
    }
}
