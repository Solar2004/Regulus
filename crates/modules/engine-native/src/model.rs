//! The model side of the loop: the conversation in a neutral form, and one request to an
//! `llm.endpoint@1` in the protocol it speaks (OpenAI Chat Completions or Anthropic
//! Messages), tools included, turned back into the same neutral answer.

use std::time::Duration;

use regulus_contract_agent_engine::Usage;
use regulus_contract_llm_endpoint::{protocol, LlmEndpoint};
use serde_json::{json, Map, Value};

/// The `anthropic-version` header the Messages API requires: part of the protocol, not a
/// setting.
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Most characters of a server's answer kept in an error.
const MAX_ERROR_CHARS: usize = 300;

/// The protocols the loop speaks.
pub const PROTOCOLS: &[&str] = &[protocol::OPENAI, protocol::ANTHROPIC];

/// A tool call the model asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// The arguments; a string when the model sent something that is not JSON.
    pub args: Value,
}

/// One message of the conversation, whatever the protocol.
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    User(String),
    Assistant {
        text: String,
        calls: Vec<ToolCall>,
    },
    Tool {
        id: String,
        content: String,
        error: bool,
    },
}

/// A tool as the model is told about it.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// What one request asks.
pub struct Request<'a> {
    pub system: &'a str,
    pub messages: &'a [Msg],
    pub tools: &'a [ToolDef],
}

/// What the model answered.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reply {
    pub text: String,
    pub calls: Vec<ToolCall>,
    pub usage: Option<Usage>,
}

/// How to ask: everything but the conversation.
#[derive(Debug, Clone)]
pub struct Client {
    pub endpoint: LlmEndpoint,
    pub model: String,
    /// `None`: not sent.
    pub temperature: Option<f64>,
    pub max_tokens: u64,
    pub timeout: Duration,
}

impl Client {
    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.endpoint.base_url.trim_end_matches('/'))
    }

    /// The header values an error must never show.
    fn secrets(&self) -> Vec<&str> {
        self.endpoint
            .headers
            .iter()
            .filter(|(name, _)| {
                let name = name.to_ascii_lowercase();
                name == "authorization" || name.contains("key") || name.contains("token")
            })
            .flat_map(|(_, value)| [value.as_str(), value.trim_start_matches("Bearer ")])
            .filter(|s| s.len() >= 4)
            .collect()
    }

    fn clean(&self, text: &str) -> String {
        let mut out: String = text.chars().take(MAX_ERROR_CHARS).collect();
        if text.chars().count() > MAX_ERROR_CHARS {
            out.push('…');
        }
        for secret in self.secrets() {
            out = out.replace(secret, "***");
        }
        out
    }

    fn post(&self, url: &str, body: &Value, extra: &[(&str, &str)]) -> Result<Value, String> {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .http_status_as_error(false)
            .build();
        let agent = ureq::Agent::new_with_config(config);
        let mut request = agent.post(url);
        for (name, value) in &self.endpoint.headers {
            request = request.header(name, value);
        }
        for (name, value) in extra {
            request = request.header(*name, *value);
        }
        let mut response = request
            .send_json(body)
            .map_err(|e| self.clean(&format!("{url}: {e}")))?;
        let status = response.status();
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|e| self.clean(&format!("{url}: {e}")))?;
        if !status.is_success() {
            return Err(self.clean(&format!("{url} answered {status}: {text}")));
        }
        serde_json::from_str(&text).map_err(|e| self.clean(&format!("{url}: not JSON: {e}")))
    }

    /// Asks the model once.
    pub fn ask(&self, request: &Request<'_>) -> Result<Reply, String> {
        match self.endpoint.protocol.as_str() {
            protocol::OPENAI => self.openai(request),
            protocol::ANTHROPIC => self.anthropic(request),
            other => Err(format!(
                "the native engine speaks {}, not `{other}`",
                PROTOCOLS.join(", ")
            )),
        }
    }

    fn openai(&self, request: &Request<'_>) -> Result<Reply, String> {
        let mut messages = Vec::new();
        if !request.system.is_empty() {
            messages.push(json!({ "role": "system", "content": request.system }));
        }
        for msg in request.messages {
            messages.push(match msg {
                Msg::User(text) => json!({ "role": "user", "content": text }),
                Msg::Assistant { text, calls } => {
                    let mut out = json!({ "role": "assistant", "content": text });
                    if !calls.is_empty() {
                        out["tool_calls"] = calls
                            .iter()
                            .map(|call| {
                                let arguments = match &call.args {
                                    Value::String(raw) => raw.clone(),
                                    args => args.to_string(),
                                };
                                json!({ "id": call.id, "type": "function",
                                        "function": { "name": call.name, "arguments": arguments } })
                            })
                            .collect();
                    }
                    out
                }
                Msg::Tool { id, content, .. } => {
                    json!({ "role": "tool", "tool_call_id": id, "content": content })
                }
            });
        }
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "max_tokens": self.max_tokens,
        });
        if let Some(temperature) = self.temperature {
            body["temperature"] = json!(temperature);
        }
        if !request.tools.is_empty() {
            body["tools"] = request
                .tools
                .iter()
                .map(|tool| {
                    json!({ "type": "function", "function": {
                        "name": tool.name, "description": tool.description,
                        "parameters": tool.schema } })
                })
                .collect();
        }
        let answer = self.post(&self.url("chat/completions"), &body, &[])?;
        let message = &answer["choices"][0]["message"];
        if message.is_null() {
            return Err(self.clean(&format!("no message in {answer}")));
        }
        let calls = message["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(i, call)| {
                let raw = call["function"]["arguments"].as_str().unwrap_or("{}");
                let args = if raw.trim().is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.into()))
                };
                ToolCall {
                    id: call["id"]
                        .as_str()
                        .map_or_else(|| format!("call-{}", i + 1), String::from),
                    name: call["function"]["name"].as_str().unwrap_or("").to_string(),
                    args,
                }
            })
            .collect();
        let usage = answer
            .get("usage")
            .filter(|u| u.is_object())
            .map(|u| Usage {
                input_tokens: u["prompt_tokens"].as_u64().unwrap_or(0),
                output_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
            });
        Ok(Reply {
            text: message["content"].as_str().unwrap_or("").to_string(),
            calls,
            usage,
        })
    }

    fn anthropic(&self, request: &Request<'_>) -> Result<Reply, String> {
        // Anthropic wants turns that alternate: tool results are user content, and
        // consecutive user content goes in one message.
        let mut messages: Vec<Value> = Vec::new();
        let mut push = |role: &str, block: Value| match messages.last_mut() {
            Some(last) if last["role"] == role => {
                if let Some(content) = last["content"].as_array_mut() {
                    content.push(block);
                }
            }
            _ => messages.push(json!({ "role": role, "content": [block] })),
        };
        for msg in request.messages {
            match msg {
                Msg::User(text) => push("user", json!({ "type": "text", "text": text })),
                Msg::Assistant { text, calls } => {
                    if !text.is_empty() {
                        push("assistant", json!({ "type": "text", "text": text }));
                    }
                    for call in calls {
                        let input = match &call.args {
                            Value::Object(_) => call.args.clone(),
                            _ => Value::Object(Map::new()),
                        };
                        push(
                            "assistant",
                            json!({ "type": "tool_use", "id": call.id, "name": call.name,
                                    "input": input }),
                        );
                    }
                }
                Msg::Tool { id, content, error } => push(
                    "user",
                    json!({ "type": "tool_result", "tool_use_id": id, "content": content,
                            "is_error": error }),
                ),
            }
        }
        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": messages,
        });
        if !request.system.is_empty() {
            body["system"] = json!(request.system);
        }
        if let Some(temperature) = self.temperature {
            body["temperature"] = json!(temperature);
        }
        if !request.tools.is_empty() {
            body["tools"] = request
                .tools
                .iter()
                .map(|tool| {
                    json!({ "name": tool.name, "description": tool.description,
                            "input_schema": tool.schema })
                })
                .collect();
        }
        let version = [("anthropic-version", ANTHROPIC_VERSION)];
        let answer = self.post(&self.url("v1/messages"), &body, &version)?;
        let blocks = answer["content"]
            .as_array()
            .ok_or_else(|| self.clean(&format!("no content in {answer}")))?;
        let mut reply = Reply::default();
        for block in blocks {
            match block["type"].as_str() {
                Some("text") => reply.text.push_str(block["text"].as_str().unwrap_or("")),
                Some("tool_use") => reply.calls.push(ToolCall {
                    id: block["id"].as_str().unwrap_or("").to_string(),
                    name: block["name"].as_str().unwrap_or("").to_string(),
                    args: block["input"].clone(),
                }),
                _ => {}
            }
        }
        reply.usage = answer
            .get("usage")
            .filter(|u| u.is_object())
            .map(|u| Usage {
                input_tokens: u["input_tokens"].as_u64().unwrap_or(0),
                output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
            });
        Ok(reply)
    }
}

/// A JSON Schema for a tool's arguments: the tool's own if it is an object schema, else
/// one that takes any object.
pub fn schema(input: &Value) -> Value {
    match input {
        Value::Object(map) if map.get("type").and_then(Value::as_str) == Some("object") => {
            input.clone()
        }
        Value::Object(map) if !map.is_empty() && !map.contains_key("type") => {
            let mut schema = map.clone();
            schema.insert("type".into(), json!("object"));
            Value::Object(schema)
        }
        _ => json!({ "type": "object" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_are_always_objects() {
        assert_eq!(schema(&Value::Null), json!({ "type": "object" }));
        let own = json!({ "type": "object", "properties": { "q": { "type": "string" } } });
        assert_eq!(schema(&own), own);
        assert_eq!(
            schema(&json!({ "properties": {} })),
            json!({ "type": "object", "properties": {} })
        );
    }
}
