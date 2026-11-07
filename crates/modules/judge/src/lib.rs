//! Judge (docs/13-blocks.md §3.2): Judge, Classifier, Extractor and Rubric. Each node
//! builds a JSON schema, asks its engine for an answer that fits it, checks the answer,
//! retries, and routes the value it judged. The engine is the built-in LLM one over the
//! `llm.endpoint@1` wired into `model`, unless a `judge.engine@1` is wired into `engine`.
//!
//! Every value is judged on a thread of its own, so the bus that delivered it goes on at
//! once. What the node emits from there starts a new chain on the bus (depth 0).

pub mod engine;
pub mod schema;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use regulus_contract_llm_endpoint::{LlmEndpoint, DESCRIBE};
use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, Manifest, Node, NodeCall, NodeEvent};
use serde::Deserialize;
use serde_json::{json, Value};

pub use engine::{Ask, Engine, LlmEngine, LlmSettings, Mode, WiredEngine, ASK, ENGINE_CAPABILITY};

const MANIFEST: &str = include_str!("../regulus-module.toml");
/// The input of the built-in engine's endpoint.
const MODEL_PORT: &str = "model";
/// The input of a plugged-in engine.
const ENGINE_PORT: &str = "engine";
/// The rubric's input of deterministic results.
const GATE_PORT: &str = "gate";
const ERROR_PORT: &str = "error";
const FEEDBACK_PORT: &str = "feedback";
const OUT_PORT: &str = "out";
const SCORE_PORT: &str = "score";
const RESULTS_PORT: &str = "results";
const PASS: &str = "pass";
const FAIL: &str = "fail";
/// The classifier's label when none fits.
const OTHER: &str = "other";

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ModuleError> {
    serde_json::from_value(params).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

/// A node, its execution and the manifest that gives its fields their defaults.
struct Job {
    manifest: Arc<Manifest>,
    ctx: ModuleCtx,
    node: Node,
    me: NodeRef,
}

impl Job {
    fn setting(&self, field: &str) -> Option<&Value> {
        self.manifest.setting(&self.node, field)
    }

    fn text(&self, field: &str) -> Option<String> {
        self.setting(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    }

    fn number(&self, field: &str) -> Option<f64> {
        self.setting(field).and_then(Value::as_f64)
    }

    /// A comma separated list.
    fn list(&self, field: &str) -> Vec<String> {
        self.text(field)
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    }

    fn error(&self, message: String) -> String {
        format!("{} `{}`: {message}", self.node.node_type, self.node.id)
    }

    /// The plugged-in engine if one is wired, otherwise the LLM one.
    fn engine(&self) -> Result<Box<dyn Engine>, String> {
        if let Some(endpoint) = self.ctx.resolve(&self.me, ENGINE_PORT).into_iter().next() {
            return Ok(Box::new(WiredEngine {
                ctx: self.ctx.clone(),
                endpoint,
            }));
        }
        let endpoint = self
            .ctx
            .resolve(&self.me, MODEL_PORT)
            .into_iter()
            .next()
            .ok_or("no engine: wire a provider into `model` or an engine into `engine`")?;
        let described = self
            .ctx
            .call(&endpoint, DESCRIBE, Value::Null)
            .map_err(|e| format!("its provider: {e}"))?;
        let described: LlmEndpoint =
            serde_json::from_value(described).map_err(|e| format!("its provider: {e}"))?;
        let mode = Mode::parse(&self.text("mode").unwrap_or_default())?;
        let settings = LlmSettings {
            model: self.text("model"),
            mode,
            temperature: self.number("temperature").unwrap_or_default(),
            max_tokens: self.number("max_tokens").map_or(0, |n| n as u64),
            timeout: Duration::from_millis(self.number("timeout_ms").map_or(0, |n| n as u64)),
        };
        Ok(Box::new(LlmEngine::new(described, settings)?))
    }

    fn retries(&self) -> u32 {
        self.number("retries").map_or(0, |n| n.max(0.0) as u32)
    }

    /// An answer that fits `ask.schema`, asking again with the reason while attempts
    /// remain.
    fn ask(&self, engine: &dyn Engine, mut ask: Ask) -> Result<Value, String> {
        let attempts = self.retries() + 1;
        let mut last = String::new();
        for _ in 0..attempts {
            match engine
                .ask(&ask)
                .and_then(|answer| schema::check(&answer, &ask.schema).map(|()| answer))
            {
                Ok(answer) => return Ok(answer),
                Err(error) => {
                    ask.prompt = format!(
                        "{}\n\nYour previous answer was rejected: {error}\nAnswer again, \
                         only with JSON that fits the schema.",
                        ask.prompt
                    );
                    last = error;
                }
            }
        }
        Err(format!("no valid answer in {attempts} attempt(s): {last}"))
    }

    fn system(&self) -> String {
        self.text("system").unwrap_or_default()
    }

    fn emit(&self, port: &str, value: Value) {
        if self.node.port(port).is_none() {
            return;
        }
        if let Err(error) = self.ctx.emit(&self.me, port, value) {
            eprintln!("regulus: judge: {}", self.error(error.to_string()));
        }
    }
}

/// The text a value is judged as: itself if text, its JSON otherwise.
fn as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn enum_schema(key: &str, values: &[String], with_feedback: bool) -> Value {
    let mut properties = json!({ key: { "type": "string", "enum": values } });
    let mut required = vec![key.to_string()];
    if with_feedback {
        properties[FEEDBACK_PORT] = json!({ "type": "string" });
        required.push(FEEDBACK_PORT.into());
    }
    json!({ "type": "object", "properties": properties, "required": required,
            "additionalProperties": false })
}

/// What a value turns into: values to emit, in order.
type Emits = Vec<(String, Value)>;

fn judge(job: &Job, input: &Value) -> Result<Emits, String> {
    let verdicts = job.list("verdicts");
    if verdicts.is_empty() {
        return Err("`verdicts` is empty".into());
    }
    if let Some(reserved) = verdicts
        .iter()
        .find(|v| *v == FEEDBACK_PORT || *v == ERROR_PORT)
    {
        return Err(format!(
            "`{reserved}` is an output of its own, not a verdict"
        ));
    }
    let ask = Ask {
        system: job.system(),
        prompt: format!(
            "Criteria:\n{}\n\nVerdicts: {}\n\nInput:\n{}",
            job.text("criteria").unwrap_or_default(),
            verdicts.join(" | "),
            as_text(input)
        ),
        schema_name: "verdict".into(),
        schema: enum_schema("verdict", &verdicts, true),
    };
    let answer = job.ask(job.engine()?.as_ref(), ask)?;
    let verdict = answer["verdict"].as_str().unwrap_or_default().to_string();
    Ok(vec![
        (FEEDBACK_PORT.into(), answer[FEEDBACK_PORT].clone()),
        (verdict, input.clone()),
    ])
}

fn classify(job: &Job, input: &Value) -> Result<Emits, String> {
    let mut labels = job.list("labels");
    if labels.is_empty() {
        return Err("`labels` is empty".into());
    }
    if labels.iter().any(|l| l == ERROR_PORT) {
        return Err(format!(
            "`{ERROR_PORT}` is an output of its own, not a label"
        ));
    }
    if !labels.iter().any(|l| l == OTHER) {
        labels.push(OTHER.into());
    }
    let ask = Ask {
        system: job.system(),
        prompt: format!(
            "Labels: {}\n\n{}\n\nInput:\n{}",
            labels.join(" | "),
            job.text("instructions").unwrap_or_default(),
            as_text(input)
        ),
        schema_name: "label".into(),
        schema: enum_schema("label", &labels, false),
    };
    let answer = job.ask(job.engine()?.as_ref(), ask)?;
    let label = answer["label"].as_str().unwrap_or(OTHER).to_string();
    Ok(vec![(label, input.clone())])
}

fn extract(job: &Job, input: &Value) -> Result<Emits, String> {
    let schema = job
        .setting("schema")
        .filter(|s| s.get("type") == Some(&json!("object")))
        .cloned()
        .ok_or("`schema` must be a JSON schema whose root is `{ \"type\": \"object\" }`")?;
    let ask = Ask {
        system: job.system(),
        prompt: format!(
            "{}\n\nInput:\n{}",
            job.text("instructions").unwrap_or_default(),
            as_text(input)
        ),
        schema_name: "extract".into(),
        schema,
    };
    let answer = job.ask(job.engine()?.as_ref(), ask)?;
    Ok(vec![(OUT_PORT.into(), answer)])
}

/// One criterion of a rubric: a leaf has `criteria` or `gate`, a parent `children`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Criterion {
    pub id: String,
    #[serde(default)]
    pub weight: Option<f64>,
    #[serde(default)]
    pub criteria: Option<String>,
    #[serde(default)]
    pub gate: Option<String>,
    #[serde(default)]
    pub children: Vec<Criterion>,
}

/// How one leaf went.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LeafResult {
    pub id: String,
    pub pass: bool,
    /// `engine` or `gate`.
    pub by: String,
    pub feedback: String,
}

/// Scores a tree: a leaf 1 or 0 as `leaf` decides, a parent the weighted mean of its
/// children. Leaves are judged one at a time, depth first.
pub fn score(
    criterion: &Criterion,
    leaf: &mut dyn FnMut(&Criterion) -> Result<LeafResult, String>,
    results: &mut Vec<LeafResult>,
) -> Result<f64, String> {
    if criterion.children.is_empty() {
        if criterion.criteria.is_some() == criterion.gate.is_some() {
            return Err(format!(
                "leaf `{}` needs exactly one of `criteria` and `gate`",
                criterion.id
            ));
        }
        let result = leaf(criterion)?;
        let value = if result.pass { 1.0 } else { 0.0 };
        results.push(result);
        return Ok(value);
    }
    let (mut total, mut weights) = (0.0, 0.0);
    for child in &criterion.children {
        let weight = child.weight.unwrap_or(1.0);
        if weight < 0.0 {
            return Err(format!("`{}` has a negative weight", child.id));
        }
        total += weight * score(child, leaf, results)?;
        weights += weight;
    }
    Ok(if weights > 0.0 { total / weights } else { 0.0 })
}

/// What reached `gate`, by name: `pass`, or `exit_code` (0 passes).
fn gate_result(value: &Value) -> Option<(String, bool)> {
    let name = value.get("name")?.as_str()?.to_string();
    let pass = match (value.get("pass"), value.get("exit_code")) {
        (Some(Value::Bool(pass)), _) => *pass,
        (_, Some(code)) => code.as_i64() == Some(0),
        _ => return None,
    };
    Some((name, pass))
}

type GateKey = (String, String);
/// The last result per gate name a node has seen.
type Gates = HashMap<String, bool>;
/// What one kind of node does with a value.
type Run = fn(&Job, &Value, &Gates) -> Result<Emits, String>;

fn rubric(job: &Job, input: &Value, gates: &Gates) -> Result<Emits, String> {
    let tree: Criterion = job
        .setting("rubric")
        .filter(|r| r.as_object().is_some_and(|o| !o.is_empty()))
        .cloned()
        .map(serde_json::from_value)
        .ok_or("`rubric` is empty")?
        .map_err(|e| format!("`rubric`: {e}"))?;
    let mut engine: Option<Box<dyn Engine>> = None;
    let system = job.system();
    let verdicts = [PASS.to_string(), FAIL.to_string()];
    let mut leaf = |c: &Criterion| -> Result<LeafResult, String> {
        if let Some(gate) = &c.gate {
            let pass = gates.get(gate).copied();
            return Ok(LeafResult {
                id: c.id.clone(),
                pass: pass == Some(true),
                by: "gate".into(),
                feedback: match pass {
                    None => format!("no result on `{GATE_PORT}` named `{gate}` yet"),
                    Some(_) => format!("gate `{gate}`"),
                },
            });
        }
        if engine.is_none() {
            engine = Some(job.engine()?);
        }
        let ask = Ask {
            system: system.clone(),
            prompt: format!(
                "Criterion:\n{}\n\nInput:\n{}",
                c.criteria.as_deref().unwrap_or_default(),
                as_text(input)
            ),
            schema_name: "verdict".into(),
            schema: enum_schema("verdict", &verdicts, true),
        };
        let answer = job.ask(engine.as_deref().expect("set above"), ask)?;
        Ok(LeafResult {
            id: c.id.clone(),
            pass: answer["verdict"] == PASS,
            by: "engine".into(),
            feedback: as_text(&answer[FEEDBACK_PORT]),
        })
    };
    let mut results = Vec::new();
    let total = score(&tree, &mut leaf, &mut results)?;
    let threshold = job.number("threshold").unwrap_or(1.0);
    let route = if total >= threshold { PASS } else { FAIL };
    let results = serde_json::to_value(results).map_err(|e| e.to_string())?;
    Ok(vec![
        (SCORE_PORT.into(), json!(total)),
        (RESULTS_PORT.into(), results),
        (route.into(), input.clone()),
    ])
}

#[derive(Default)]
struct Shared {
    /// Values each node is judging now, by execution and node.
    in_flight: Mutex<HashMap<GateKey, usize>>,
    /// The last result per gate name, by execution and node.
    gates: Mutex<HashMap<GateKey, Gates>>,
}

/// Takes one slot of a node's `max_in_flight` and gives it back when dropped.
struct Slot {
    shared: Arc<Shared>,
    key: GateKey,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut in_flight = self
            .shared
            .in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(count) = in_flight.get_mut(&self.key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                in_flight.remove(&self.key);
            }
        }
    }
}

pub struct Judge {
    manifest: Arc<Manifest>,
    ctx: ModuleCtx,
    shared: Arc<Shared>,
}

impl Judge {
    pub fn new() -> Self {
        Self {
            manifest: Arc::new(parse_manifest(MANIFEST).expect("judge manifest is valid")),
            ctx: ModuleCtx::default(),
            shared: Arc::default(),
        }
    }

    fn event(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let event: NodeEvent = parse(call.args)?;
        let key = (call.execution_id.clone(), call.node.id.clone());
        if event.port == GATE_PORT {
            let (name, pass) = gate_result(&event.value).ok_or_else(|| {
                ModuleError::InvalidParams(format!(
                    "`{GATE_PORT}` takes {{ name, pass }} or {{ name, exit_code }}, not {}",
                    event.value
                ))
            })?;
            let mut gates = self.shared.gates.lock().unwrap_or_else(|e| e.into_inner());
            gates.entry(key).or_default().insert(name, pass);
            return Ok(Value::Null);
        }
        let job = Job {
            manifest: self.manifest.clone(),
            ctx: self.ctx.clone(),
            me: NodeRef {
                execution_id: call.execution_id,
                node_id: call.node.id.clone(),
            },
            node: call.node,
        };
        let run: Run = match job.node.node_type.as_str() {
            "judge" => |job, input, _| judge(job, input),
            "classifier" => |job, input, _| classify(job, input),
            "extractor" => |job, input, _| extract(job, input),
            "rubric" => rubric,
            other => {
                return Err(ModuleError::InvalidParams(format!(
                    "`{other}` is not a node of this module"
                )))
            }
        };
        let max = job
            .number("max_in_flight")
            .map_or(1, |n| n.max(1.0) as usize);
        {
            let mut in_flight = self
                .shared
                .in_flight
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let count = in_flight.entry(key.clone()).or_default();
            if *count >= max {
                return Err(ModuleError::Failed(
                    job.error(format!("already judging {max} values (`max_in_flight`)")),
                ));
            }
            *count += 1;
        }
        let slot = Slot {
            shared: self.shared.clone(),
            key: key.clone(),
        };
        let shared = self.shared.clone();
        let input = event.value;
        std::thread::Builder::new()
            .name("regulus-judge".into())
            .spawn(move || {
                let _slot = slot;
                let gates = shared
                    .gates
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&key)
                    .cloned()
                    .unwrap_or_default();
                match run(&job, &input, &gates) {
                    Ok(emits) => {
                        for (port, value) in emits {
                            job.emit(&port, value);
                        }
                    }
                    Err(error) => {
                        let error = job.error(error);
                        eprintln!("regulus: judge: {error}");
                        job.emit(ERROR_PORT, json!(error));
                    }
                }
            })
            .map_err(|e| ModuleError::Failed(format!("cannot start a judge thread: {e}")))?;
        Ok(json!({ "judging": true }))
    }

    fn stop(&self, call: NodeCall) {
        let key = (call.execution_id, call.node.id);
        let mut gates = self.shared.gates.lock().unwrap_or_else(|e| e.into_inner());
        gates.remove(&key);
    }
}

impl Default for Judge {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Judge {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        match name {
            method::NODE_EVENT => self.event(parse(params)?),
            method::NODE_STOP => {
                self.stop(parse(params)?);
                Ok(Value::Null)
            }
            method::NODE_START => Ok(Value::Null),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_declares_the_four_nodes_with_both_engine_inputs() {
        let judge = Judge::new();
        for kind in ["judge", "classifier", "extractor", "rubric"] {
            let node = judge.manifest().node_type(kind).unwrap();
            let port = |id: &str| node.ports.iter().find(|p| p.id == id).unwrap();
            assert_eq!(port(MODEL_PORT).capability, "llm.endpoint@1", "{kind}");
            assert_eq!(port(ENGINE_PORT).capability, ENGINE_CAPABILITY, "{kind}");
            assert!(!port(MODEL_PORT).required && !port(ENGINE_PORT).required);
            assert!(node.field("system").unwrap().default.is_some(), "{kind}");
        }
    }

    fn tree(source: Value) -> Criterion {
        serde_json::from_value(source).unwrap()
    }

    #[test]
    fn a_rubric_scores_the_weighted_mean_of_its_leaves() {
        let rubric = tree(json!({ "id": "root", "children": [
            { "id": "tests", "weight": 2, "gate": "tests" },
            { "id": "style", "children": [
                { "id": "names", "criteria": "clear names" },
                { "id": "docs", "criteria": "documented" },
            ]},
        ]}));
        let mut leaf = |c: &Criterion| {
            Ok(LeafResult {
                id: c.id.clone(),
                pass: c.id != "docs",
                by: "test".into(),
                feedback: String::new(),
            })
        };
        let mut results = Vec::new();
        let total = score(&rubric, &mut leaf, &mut results).unwrap();
        // (2 * 1 + 1 * 0.5) / 3
        assert!((total - 2.5 / 3.0).abs() < 1e-9, "{total}");
        assert_eq!(results.len(), 3);
        let bad = tree(json!({ "id": "x", "criteria": "a", "gate": "b" }));
        assert!(score(&bad, &mut leaf, &mut Vec::new()).is_err());
    }

    #[test]
    fn gate_results_take_pass_or_an_exit_code() {
        assert_eq!(
            gate_result(&json!({ "name": "t", "pass": false })),
            Some(("t".into(), false))
        );
        assert_eq!(
            gate_result(&json!({ "name": "t", "exit_code": 0 })),
            Some(("t".into(), true))
        );
        assert_eq!(gate_result(&json!({ "pass": true })), None);
    }
}
