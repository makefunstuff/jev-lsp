//! The decision wire: questions in, values out, no prose.
//!
//! A decision is not a chat. There are no messages and there is no answer to read — the model
//! is handed a *state* and a numbered set of questions, and returns one value per question with
//! a probability. That is the whole protocol, and it is why a decision costs a few dozen tokens
//! where a review costs thousands.
//!
//! Two wires speak it, and the only difference between them is the path: a System One server
//! (the default) and an OpenRouter-compatible gateway. The request body is identical for both.
//!
//! Everything here is pure except [`DecisionClient::decide`]: parsing and body building are
//! testable without a socket, which is where the mistakes are — a probability read as a level, a
//! distribution mistaken for a label, a missing answer invented as `false`.

use crate::config::DecisionTierConfig;
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::time::Duration;

/// Which server answers. The body is the same; only the path differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wire {
    SystemOne,
    OpenRouter,
}

impl Wire {
    /// The path appended to the configured base URL.
    pub fn path(self) -> &'static str {
        match self {
            Wire::SystemOne => "/systemone",
            Wire::OpenRouter => "/alpha/decisions",
        }
    }
}

/// The shape of the value a question wants back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuestionKind {
    /// A probability that a statement is true.
    Noul,
    /// One label out of a described set.
    Choice,
    /// A level, described as an ordered list.
    Score,
}

impl QuestionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            QuestionKind::Noul => "noul",
            QuestionKind::Choice => "choice",
            QuestionKind::Score => "score",
        }
    }
}

/// What a question's answer amounts to.
#[derive(Debug, Clone, PartialEq)]
pub enum DecisionValue {
    Bool(bool),
    Choice(String),
    Level(u32),
    /// No answer. Reported as this rather than guessed at.
    None,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecisionQuestion {
    pub id: String,
    pub kind: QuestionKind,
    pub instructions: String,
    /// `null` means the question carries none, and the field is then omitted from the body.
    pub criteria: Value,
    pub reasons: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecisionAnswer {
    pub id: String,
    pub value: DecisionValue,
    /// The probability of the answer, when the wire gave one.
    pub probability: Option<f64>,
    /// The label the answer chose out of the question's `reasons`, when it gave one.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecisionRequest {
    pub state: String,
    pub questions: Vec<DecisionQuestion>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecisionResponse {
    pub answers: Vec<DecisionAnswer>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl DecisionResponse {
    /// The answer to one question.
    ///
    /// A question the response does not mention comes back as [`DecisionValue::None`] with no
    /// probability. It is never guessed at: a `false` the model never said is indistinguishable
    /// from a `false` it did, and the caller must be able to tell the two apart.
    pub fn answer(&self, id: &str) -> DecisionAnswer {
        self.answers
            .iter()
            .find(|a| a.id == id)
            .cloned()
            .unwrap_or(DecisionAnswer {
                id: id.to_string(),
                value: DecisionValue::None,
                probability: None,
                reason: None,
            })
    }
}

/// A response that arrived but cannot be understood.
///
/// Separate from a transport failure on purpose: "the endpoint said something I cannot parse" is
/// a contract violation and "the endpoint did not answer" is a model failure, and the caller
/// reports them with different codes.
#[derive(Debug)]
pub struct BadAnswer {
    /// The question it was about, when the problem is scoped to one.
    pub id: Option<String>,
    pub why: String,
}

impl std::fmt::Display for BadAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.id {
            Some(id) => write!(f, "answer {id:?}: {}", self.why),
            None => write!(f, "{}", self.why),
        }
    }
}

impl std::error::Error for BadAnswer {}

fn bad(id: Option<&str>, why: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(BadAnswer {
        id: id.map(str::to_string),
        why: why.into(),
    })
}

/// One decision round trip. Implementations must be safe to share across threads.
pub trait DecisionBackend: Send + Sync {
    fn decide(&self, cfg: &DecisionTierConfig, req: &DecisionRequest) -> Result<DecisionResponse>;
}

pub struct DecisionClient {
    agent: ureq::Agent,
}

impl Default for DecisionClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DecisionClient {
    pub fn new() -> DecisionClient {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .build();
        DecisionClient {
            agent: ureq::Agent::new_with_config(config),
        }
    }

    /// Join `base_url` and the wire's path without doubling or dropping a slash.
    pub fn endpoint(wire: Wire, base_url: &str) -> String {
        let base = base_url.trim_end_matches('/');
        if base.ends_with(wire.path()) {
            base.to_string()
        } else {
            format!("{base}{}", wire.path())
        }
    }

    /// The request body. One shape for both wires, because the wire *is* the path here.
    pub fn build_body(_wire: Wire, cfg: &DecisionTierConfig, req: &DecisionRequest) -> Value {
        let mut questions = Map::new();
        for q in &req.questions {
            let mut entry = json!({
                "type": q.kind.as_str(),
                "instructions": q.instructions,
            });
            // Omitted when null: a `criteria: null` is not the same statement as "no criteria"
            // to a server that validates the field's shape, and noul criteria are optional.
            if !q.criteria.is_null() {
                entry["criteria"] = q.criteria.clone();
            }
            if let Some(reasons) = &q.reasons {
                entry["reasons"] = reasons.clone();
            }
            questions.insert(q.id.clone(), entry);
        }
        json!({
            "model": cfg.model,
            "state": req.state,
            "questions": Value::Object(questions),
        })
    }

    /// Read a response. Every problem is an error naming what failed; nothing is guessed.
    pub fn parse(raw: &Value) -> Result<DecisionResponse> {
        let answers = raw
            .get("answers")
            .and_then(|a| a.as_object())
            .ok_or_else(|| bad(None, "the response carries no `answers` object"))?;
        let mut parsed = Vec::with_capacity(answers.len());
        for (id, value) in answers {
            parsed.push(parse_answer(id, value)?);
        }
        // Usage is optional; absent means zero tokens, which is what a server that does not
        // count them is saying.
        let usage = raw.get("usage");
        let count = |k: &str| {
            usage
                .and_then(|u| u.get(k))
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        };
        Ok(DecisionResponse {
            answers: parsed,
            input_tokens: count("input_tokens"),
            output_tokens: count("output_tokens"),
        })
    }
}

/// The highest-probability entry of a labelled distribution.
fn argmax_label(dist: &Map<String, Value>) -> Option<(String, f64)> {
    dist.iter()
        .filter_map(|(k, v)| v.as_f64().map(|p| (k.clone(), p)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

/// The highest-probability level of a distribution keyed by level index as a string.
fn argmax_level(dist: &Map<String, Value>) -> Option<(u32, f64)> {
    dist.iter()
        .filter_map(|(k, v)| {
            let level = k.parse::<u32>().ok()?;
            v.as_f64().map(|p| (level, p))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

fn parse_answer(id: &str, raw: &Value) -> Result<DecisionAnswer> {
    let obj = raw
        .as_object()
        .ok_or_else(|| bad(Some(id), "the answer is not an object"))?;
    // Kind detection: the answer's own `type` when it carries one, and otherwise the value key
    // it contains. The reference server omits `type` on some routes, so inferring is not a
    // fallback for a broken answer — it is the other shape the wire legitimately has.
    let kind = match obj.get("type").and_then(|v| v.as_str()) {
        Some("noul") => QuestionKind::Noul,
        Some("choice") => QuestionKind::Choice,
        Some("score") => QuestionKind::Score,
        Some(other) => return Err(bad(Some(id), format!("unknown answer type {other:?}"))),
        None if obj.contains_key("noul") => QuestionKind::Noul,
        None if obj.contains_key("choice") => QuestionKind::Choice,
        None if obj.contains_key("score") => QuestionKind::Score,
        None => {
            return Err(bad(
                Some(id),
                "the answer names none of `noul`, `choice` or `score`",
            ))
        }
    };

    let reason = obj
        .get("reason")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let confidence = obj.get("confidence").and_then(|v| v.as_f64());

    let (value, probability) = match kind {
        QuestionKind::Noul => {
            let p = obj
                .get("noul")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| bad(Some(id), "a noul answer needs a numeric `noul`"))?;
            // The wire gives the probability that the statement is true; the value is that
            // claim, and the probability travels beside it so a caller can apply its own floor.
            (DecisionValue::Bool(p > 0.5), Some(p))
        }
        QuestionKind::Choice => match obj.get("choice") {
            Some(Value::String(label)) => (DecisionValue::Choice(label.clone()), confidence),
            // A distribution instead of a label: the answer is its argmax, and its probability
            // is that entry's, not the request-wide confidence.
            Some(Value::Object(dist)) => {
                let (label, p) = argmax_label(dist).ok_or_else(|| {
                    bad(Some(id), "a choice distribution carries no numeric entry")
                })?;
                (DecisionValue::Choice(label), Some(p))
            }
            _ => {
                return Err(bad(
                    Some(id),
                    "a choice answer needs a label or a distribution",
                ))
            }
        },
        QuestionKind::Score => {
            // The distribution first: `score` may be a *fractional expected level* rather than
            // an index, and rounding 1.25 would name a level the model did not choose. The
            // argmax of the distribution is the level it did choose.
            match obj
                .get("probabilities")
                .and_then(|v| v.as_object())
                .and_then(argmax_level)
            {
                Some((level, p)) => (DecisionValue::Level(level), Some(p)),
                None => {
                    let s = obj
                        .get("score")
                        .and_then(|v| v.as_f64())
                        .ok_or_else(|| {
                            bad(
                                Some(id),
                                "a score answer needs a numeric `score` or a probabilities object",
                            )
                        })?;
                    (
                        DecisionValue::Level(s.round().max(0.0) as u32),
                        confidence,
                    )
                }
            }
        }
    };

    Ok(DecisionAnswer {
        id: id.to_string(),
        value,
        probability,
        reason,
    })
}

impl DecisionBackend for DecisionClient {
    fn decide(&self, cfg: &DecisionTierConfig, req: &DecisionRequest) -> Result<DecisionResponse> {
        let url = Self::endpoint(cfg.wire, &cfg.base_url);
        let body = Self::build_body(cfg.wire, cfg, req);

        let mut call = self
            .agent
            .post(&url)
            .config()
            .timeout_global(Some(Duration::from_millis(cfg.timeout_ms.max(1))))
            .build()
            .header("content-type", "application/json");
        // Only when the variable is named *and* set: an unset key is not an empty credential.
        if let Some(env) = &cfg.api_key_env {
            if let Ok(key) = std::env::var(env) {
                if !key.is_empty() {
                    call = call.header("authorization", &format!("Bearer {key}"));
                }
            }
        }

        let mut response = call
            .send_json(&body)
            .with_context(|| format!("POST {url}"))?;
        let raw: Value = response
            .body_mut()
            .read_json()
            .with_context(|| format!("decoding the decision response from {url}"))?;
        Self::parse(&raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Think;

    fn cfg() -> DecisionTierConfig {
        DecisionTierConfig {
            base_url: "https://api.typesafe.ai/v1".into(),
            model: "jev-latest".into(),
            api_key_env: None,
            ..Default::default()
        }
    }

    fn question(id: &str, kind: QuestionKind, criteria: Value, reasons: Option<Value>) -> DecisionQuestion {
        DecisionQuestion {
            id: id.to_string(),
            kind,
            instructions: format!("decide {id}"),
            criteria,
            reasons,
        }
    }

    #[test]
    fn the_two_wires_join_the_base_and_the_path_without_doubling_a_slash() {
        assert_eq!(
            DecisionClient::endpoint(Wire::SystemOne, "https://api.typesafe.ai/v1"),
            "https://api.typesafe.ai/v1/systemone"
        );
        assert_eq!(
            DecisionClient::endpoint(Wire::SystemOne, "https://api.typesafe.ai/v1/"),
            "https://api.typesafe.ai/v1/systemone"
        );
        assert_eq!(
            DecisionClient::endpoint(Wire::OpenRouter, "https://or.example/api"),
            "https://or.example/api/alpha/decisions"
        );
        assert_eq!(
            DecisionClient::endpoint(Wire::SystemOne, "http://127.0.0.1:8009/v1/systemone"),
            "http://127.0.0.1:8009/v1/systemone"
        );
    }

    #[test]
    fn both_wires_carry_the_same_body_for_a_noul_question_with_criteria() {
        let req = DecisionRequest {
            state: "1: fn a() {".into(),
            questions: vec![question(
                "r#1",
                QuestionKind::Noul,
                json!({"true": "reachable", "false": "test only"}),
                None,
            )],
        };
        for wire in [Wire::SystemOne, Wire::OpenRouter] {
            let body = DecisionClient::build_body(wire, &cfg(), &req);
            assert_eq!(body["model"], "jev-latest");
            assert_eq!(body["state"], "1: fn a() {");
            assert_eq!(body["questions"]["r#1"]["type"], "noul");
            assert_eq!(body["questions"]["r#1"]["instructions"], "decide r#1");
            assert_eq!(body["questions"]["r#1"]["criteria"]["true"], "reachable");
            assert!(
                body["questions"]["r#1"].get("reasons").is_none(),
                "reasons are omitted when the question carries none"
            );
        }
    }

    #[test]
    fn a_choice_question_carries_its_label_map_and_a_score_its_ordered_list() {
        let req = DecisionRequest {
            state: "s".into(),
            questions: vec![
                question(
                    "c",
                    QuestionKind::Choice,
                    json!({"returns": "it returns a value", "throws": "it throws"}),
                    Some(json!({"returns": "a value came back"})),
                ),
                question(
                    "s",
                    QuestionKind::Score,
                    json!(["nothing", "a little", "a lot"]),
                    None,
                ),
            ],
        };
        let body = DecisionClient::build_body(Wire::SystemOne, &cfg(), &req);
        assert_eq!(body["questions"]["c"]["type"], "choice");
        assert_eq!(body["questions"]["c"]["criteria"]["returns"], "it returns a value");
        assert_eq!(body["questions"]["c"]["reasons"]["returns"], "a value came back");
        assert_eq!(body["questions"]["s"]["type"], "score");
        assert!(
            body["questions"]["s"]["criteria"].is_array(),
            "a score's criteria is an ordered array of level descriptions"
        );
    }

    #[test]
    fn a_null_criteria_is_omitted_rather_than_sent_as_null() {
        let req = DecisionRequest {
            state: "s".into(),
            questions: vec![question("q", QuestionKind::Noul, Value::Null, None)],
        };
        let body = DecisionClient::build_body(Wire::SystemOne, &cfg(), &req);
        assert!(body["questions"]["q"].get("criteria").is_none());
        assert!(body["questions"]["q"].get("reasons").is_none());
    }

    #[test]
    fn noul_above_and_below_the_midpoint() {
        let above = DecisionClient::parse(&json!({
            "answers": {"a": {"type": "noul", "noul": 0.92}}
        }))
        .unwrap();
        let a = above.answer("a");
        assert_eq!(a.value, DecisionValue::Bool(true));
        assert_eq!(a.probability, Some(0.92));

        let below = DecisionClient::parse(&json!({
            "answers": {"a": {"type": "noul", "noul": 0.31, "reason": "test_only"}}
        }))
        .unwrap();
        let b = below.answer("a");
        assert_eq!(b.value, DecisionValue::Bool(false));
        assert_eq!(b.probability, Some(0.31));
        assert_eq!(b.reason.as_deref(), Some("test_only"));
    }

    #[test]
    fn choice_as_a_label_keeps_the_confidence_that_came_with_it() {
        let r = DecisionClient::parse(&json!({
            "answers": {"c": {"type": "choice", "choice": "returns", "confidence": 0.83,
                              "probabilities": {"returns": 0.83, "throws": 0.17}}}
        }))
        .unwrap();
        let a = r.answer("c");
        assert_eq!(a.value, DecisionValue::Choice("returns".into()));
        assert_eq!(a.probability, Some(0.83));
    }

    #[test]
    fn choice_as_a_distribution_becomes_its_argmax_with_that_entrys_probability() {
        let r = DecisionClient::parse(&json!({
            "answers": {"c": {"type": "choice", "confidence": 0.4,
                              "choice": {"throws": 0.2, "returns": 0.7, "loops": 0.1}}}
        }))
        .unwrap();
        let a = r.answer("c");
        assert_eq!(a.value, DecisionValue::Choice("returns".into()));
        assert_eq!(a.probability, Some(0.7), "the argmax entry's probability, not confidence");
    }

    #[test]
    fn a_fractional_score_with_a_probabilities_dict_takes_the_argmax_level() {
        // `score` is an expected level here, not an index: rounding 1.25 would name level 1,
        // which is not the level with the most probability behind it.
        let r = DecisionClient::parse(&json!({
            "answers": {"s": {"type": "score", "score": 1.25, "confidence": 0.88,
                              "legend": {"0": "nothing", "1": "a little", "2": "a lot"},
                              "probabilities": {"0": 0.0, "1": 0.75, "2": 0.25}}}
        }))
        .unwrap();
        let a = r.answer("s");
        assert_eq!(a.value, DecisionValue::Level(1));
        assert_eq!(a.probability, Some(0.75));
    }

    #[test]
    fn a_score_with_only_a_number_is_rounded_to_a_level() {
        let r = DecisionClient::parse(&json!({
            "answers": {"s": {"type": "score", "score": 2.4, "confidence": 0.6}}
        }))
        .unwrap();
        let a = r.answer("s");
        assert_eq!(a.value, DecisionValue::Level(2));
        assert_eq!(a.probability, Some(0.6));
    }

    #[test]
    fn the_kind_is_inferred_when_the_answer_carries_no_type() {
        let r = DecisionClient::parse(&json!({
            "answers": {
                "a": {"noul": 0.9},
                "b": {"choice": "x"},
                "c": {"score": 1.0},
            }
        }))
        .unwrap();
        assert_eq!(r.answer("a").value, DecisionValue::Bool(true));
        assert_eq!(r.answer("b").value, DecisionValue::Choice("x".into()));
        assert_eq!(r.answer("c").value, DecisionValue::Level(1));
    }

    #[test]
    fn a_question_absent_from_the_response_is_reported_never_guessed() {
        let r = DecisionClient::parse(&json!({
            "answers": {"a": {"type": "noul", "noul": 0.9}}
        }))
        .unwrap();
        let missing = r.answer("not-asked");
        assert_eq!(missing.id, "not-asked");
        assert_eq!(missing.value, DecisionValue::None);
        assert_eq!(missing.probability, None);
    }

    #[test]
    fn a_malformed_answer_is_an_error_naming_the_question() {
        for bad_raw in [
            json!({"answers": {"q": {"type": "noul"}}}),
            json!({"answers": {"q": {"type": "noul", "noul": "yes"}}}),
            json!({"answers": {"q": {"type": "verdict", "verdict": 1}}}),
            json!({"answers": {"q": {"type": "choice"}}}),
            json!({"answers": {"q": {"type": "score"}}}),
            json!({"answers": {"q": {"nothing": 1}}}),
            json!({"answers": {"q": 7}}),
            json!({"answers": []}),
            json!({}),
        ] {
            let e = DecisionClient::parse(&bad_raw).unwrap_err();
            assert!(
                e.downcast_ref::<BadAnswer>().is_some(),
                "a malformed answer is a contract violation, not a model failure: {e}"
            );
        }
        let named = DecisionClient::parse(&json!({"answers": {"q": {"noul": "yes"}}}))
            .unwrap_err()
            .to_string();
        assert!(named.contains("\"q\""), "the error names the question: {named}");
    }

    #[test]
    fn usage_absent_is_zero_and_present_is_kept() {
        let none = DecisionClient::parse(&json!({"answers": {}})).unwrap();
        assert_eq!((none.input_tokens, none.output_tokens), (0, 0));
        let some = DecisionClient::parse(&json!({
            "answers": {},
            "usage": {"input_tokens": 120, "output_tokens": 3}
        }))
        .unwrap();
        assert_eq!((some.input_tokens, some.output_tokens), (120, 3));
    }

    #[test]
    fn a_tier_default_is_off_and_cheap() {
        let d = DecisionTierConfig::default();
        assert_eq!(d.wire, Wire::SystemOne);
        assert_eq!(d.think, Think::Off);
        assert!(d.max_tokens <= 64, "a decision generates no prose");
        assert!(d.timeout_ms <= 5000, "a decision that takes longer than this is not saving anyone");
    }

    #[test]
    fn an_unreachable_endpoint_is_an_error_not_a_panic() {
        let backend = DecisionClient::new();
        let req = DecisionRequest {
            state: "s".into(),
            questions: vec![question("q", QuestionKind::Noul, Value::Null, None)],
        };
        let cfg = DecisionTierConfig {
            base_url: "http://127.0.0.1:1/v1".into(),
            timeout_ms: 200,
            ..cfg()
        };
        assert!(backend.decide(&cfg, &req).is_err());
    }
}
