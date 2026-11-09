//! One completion against an `llm.endpoint@1`: OpenAI chat completions or Anthropic
//! messages. Adapted from the judge's engine (`crates/modules/judge/src/engine.rs`),
//! without schemas: the model's text comes back as it is.

use std::time::Duration;

use regulus_contract_llm_endpoint::{protocol, LlmEndpoint};
use serde_json::{json, Value};

/// The `anthropic-version` header the Messages API requires: part of the protocol, not a
/// setting.
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Most characters of a server's answer kept in an error.
const MAX_ERROR_CHARS: usize = 300;
/// Query parameters whose values never show in an error.
const SECRET_QUERY: &[&str] = &["key", "token", "api_key", "apikey", "access_token"];
/// Statuses that may pass when asked again.
const RETRYABLE: &[u16] = &[408, 429];

/// One request.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub system: String,
    pub prompt: String,
    pub max_tokens: u64,
    pub temperature: f64,
}

/// What the model answered.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub text: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

/// Why there is no answer, and whether asking again may help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub message: String,
    pub retry: bool,
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

pub struct Client {
    endpoint: LlmEndpoint,
    pub model: String,
    agent: ureq::Agent,
}

impl Client {
    /// `model` empty: the endpoint's first.
    pub fn new(endpoint: LlmEndpoint, model: &str, agent: ureq::Agent) -> Result<Self, String> {
        if endpoint.protocol != protocol::OPENAI && endpoint.protocol != protocol::ANTHROPIC {
            return Err(format!(
                "inference speaks `{}` and `{}`, not `{}`",
                protocol::OPENAI,
                protocol::ANTHROPIC,
                endpoint.protocol
            ));
        }
        let model = Some(model.trim())
            .filter(|m| !m.is_empty())
            .map(String::from)
            .or_else(|| endpoint.models.first().cloned())
            .ok_or("no model: set `model` on the node or on its provider")?;
        Ok(Self {
            endpoint,
            model,
            agent,
        })
    }

    pub fn protocol(&self) -> &str {
        &self.endpoint.protocol
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
        let transient = |text: String| Failure {
            message: clean(text),
            retry: true,
        };
        let mut request = self.agent.post(url);
        for (name, value) in &self.endpoint.headers {
            request = request.header(name, value);
        }
        for (name, value) in extra {
            request = request.header(*name, *value);
        }
        let mut response = request
            .send_json(body)
            .map_err(|e| transient(format!("{url}: {e}")))?;
        let status = response.status();
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|e| transient(format!("{url}: {e}")))?;
        if !status.is_success() {
            let code = status.as_u16();
            return Err(Failure {
                message: clean(format!("{url} answered {status}: {text}")),
                retry: code >= 500 || RETRYABLE.contains(&code),
            });
        }
        serde_json::from_str(&text).map_err(|e| Failure {
            message: clean(format!("{url}: not JSON: {e}")),
            retry: false,
        })
    }

    pub fn complete(&self, request: &Request) -> Result<Answer, Failure> {
        let no_text = |answer: &Value| Failure {
            message: redact(&format!("no text in {answer}"), &self.secrets()),
            retry: false,
        };
        if self.endpoint.protocol == protocol::ANTHROPIC {
            let mut body = json!({
                "model": self.model,
                "max_tokens": request.max_tokens,
                "temperature": request.temperature,
                "messages": [{ "role": "user", "content": request.prompt }],
            });
            if !request.system.is_empty() {
                body["system"] = json!(request.system);
            }
            let version = [("anthropic-version", ANTHROPIC_VERSION)];
            let answer = self.post(&self.url("v1/messages"), &body, &version)?;
            let texts: Vec<&str> = answer["content"]
                .as_array()
                .ok_or_else(|| no_text(&answer))?
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect();
            if texts.is_empty() {
                return Err(no_text(&answer));
            }
            return Ok(Answer {
                text: texts.join(""),
                input_tokens: answer["usage"]["input_tokens"].as_u64(),
                output_tokens: answer["usage"]["output_tokens"].as_u64(),
            });
        }
        let mut messages = Vec::new();
        if !request.system.is_empty() {
            messages.push(json!({ "role": "system", "content": request.system }));
        }
        messages.push(json!({ "role": "user", "content": request.prompt }));
        let body = json!({
            "model": self.model,
            "messages": messages,
            "temperature": request.temperature,
            "max_tokens": request.max_tokens,
        });
        let answer = self.post(&self.url("chat/completions"), &body, &[])?;
        let text = answer["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| no_text(&answer))?;
        Ok(Answer {
            text: text.to_string(),
            input_tokens: answer["usage"]["prompt_tokens"].as_u64(),
            output_tokens: answer["usage"]["completion_tokens"].as_u64(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_hides_secrets_and_secret_query_values() {
        let clean = redact(
            "http://h/v1?key=abc123&x=1 answered 401: Bearer sk-live-1",
            &["Bearer sk-live-1", "sk-live-1"],
        );
        assert!(
            !clean.contains("abc123") && !clean.contains("sk-live-1"),
            "{clean}"
        );
        assert_eq!(
            redact(&"y".repeat(1000), &[]).chars().count(),
            MAX_ERROR_CHARS + 1
        );
    }

    #[test]
    fn unknown_protocols_and_missing_models_are_refused() {
        let mut endpoint = LlmEndpoint {
            protocol: "bedrock".into(),
            base_url: "http://127.0.0.1:1".into(),
            ..LlmEndpoint::default()
        };
        let agent = || agent(Duration::from_secs(1));
        assert!(Client::new(endpoint.clone(), "", agent())
            .err()
            .unwrap()
            .contains("bedrock"));
        endpoint.protocol = protocol::OPENAI.into();
        assert!(Client::new(endpoint.clone(), " ", agent())
            .err()
            .unwrap()
            .contains("no model"));
        endpoint.models = vec!["m1".into()];
        assert_eq!(Client::new(endpoint, "", agent()).unwrap().model, "m1");
    }
}
