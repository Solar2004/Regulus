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

pub trait Engine {
    /// The answer, parsed as JSON. Whether it fits the schema is the caller's business.
    fn ask(&self, ask: &Ask) -> Result<Value, String>;
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

/// A refusal of the request itself (4xx), as opposed to a failure on the way.
enum Failure {
    Refused(String),
    Other(String),
}

impl Failure {
    fn text(self) -> String {
        match self {
            Failure::Refused(text) | Failure::Other(text) => text,
        }
    }
}

impl LlmEngine {
    pub fn new(endpoint: LlmEndpoint, settings: LlmSettings) -> Result<Self, String> {
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
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(settings.timeout))
            .http_status_as_error(false)
            .build();
        Ok(Self {
            endpoint,
            model,
            settings,
            agent: ureq::Agent::new_with_config(config),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.endpoint.base_url.trim_end_matches('/'))
    }

    fn post(&self, url: &str, body: &Value, extra: &[(&str, &str)]) -> Result<Value, Failure> {
        let mut request = self.agent.post(url);
        for (name, value) in &self.endpoint.headers {
            request = request.header(name, value);
        }
        for (name, value) in extra {
            request = request.header(*name, *value);
        }
        let mut response = request
            .send_json(body)
            .map_err(|e| Failure::Other(format!("{url}: {e}")))?;
        let status = response.status();
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|e| Failure::Other(format!("{url}: {e}")))?;
        if status.is_client_error() {
            return Err(Failure::Refused(format!("{url} answered {status}: {text}")));
        }
        if !status.is_success() {
            return Err(Failure::Other(format!("{url} answered {status}: {text}")));
        }
        serde_json::from_str(&text).map_err(|e| Failure::Other(format!("{url}: not JSON: {e}")))
    }

    fn openai(&self, ask: &Ask, mode: Mode) -> Result<Value, Failure> {
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
        let answer = self.post(&self.url("chat/completions"), &body, &[])?;
        let message = &answer["choices"][0]["message"];
        let text = match mode {
            Mode::Tool => message["tool_calls"][0]["function"]["arguments"].as_str(),
            _ => message["content"].as_str(),
        }
        .ok_or_else(|| Failure::Other(format!("no answer in {answer}")))?;
        parse_answer(text).map_err(Failure::Other)
    }

    fn anthropic(&self, ask: &Ask) -> Result<Value, Failure> {
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
        let answer = self.post(&self.url("v1/messages"), &body, &version)?;
        answer["content"]
            .as_array()
            .and_then(|blocks| blocks.iter().find(|b| b["type"] == "tool_use"))
            .map(|block| block["input"].clone())
            .ok_or_else(|| Failure::Other(format!("no tool_use in {answer}")))
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
    serde_json::from_str(inner.trim()).map_err(|e| format!("the answer is not JSON ({e}): {text}"))
}

impl Engine for LlmEngine {
    fn ask(&self, ask: &Ask) -> Result<Value, String> {
        if self.endpoint.protocol == protocol::ANTHROPIC {
            return self.anthropic(ask).map_err(Failure::text);
        }
        match self.settings.mode {
            Mode::Auto => match self.openai(ask, Mode::Schema) {
                Err(Failure::Refused(_)) => self.openai(ask, Mode::Tool).map_err(Failure::text),
                other => other.map_err(Failure::text),
            },
            mode => self.openai(ask, mode).map_err(Failure::text),
        }
    }
}

/// An engine behind a "uses" wire: whatever node serves [`ASK`].
pub struct WiredEngine {
    pub ctx: ModuleCtx,
    pub endpoint: Endpoint,
}

impl Engine for WiredEngine {
    fn ask(&self, ask: &Ask) -> Result<Value, String> {
        let args = serde_json::to_value(ask).map_err(|e| e.to_string())?;
        self.ctx
            .call(&self.endpoint, ASK, args)
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(LlmEngine::new(endpoint.clone(), settings.clone())
            .err()
            .unwrap()
            .contains("bedrock"));
        endpoint.protocol = protocol::OPENAI.into();
        assert!(LlmEngine::new(endpoint, settings)
            .err()
            .unwrap()
            .contains("no model"));
    }
}
