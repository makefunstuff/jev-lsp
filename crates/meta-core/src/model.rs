//! The model client. Synchronous by design: one HTTP implementation, no async here.

use crate::config::{TierConfig, Think};
use anyhow::{anyhow, Context as _, Result};
use serde::Serialize;
use serde_json::json;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub system: String,
    pub user: String,
    pub temperature: f32,
    pub max_tokens: u32,
    /// Ask the server for a JSON object rather than prose.
    pub json: bool,
    pub think: Think,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatResponse {
    pub text: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// `stop`, `length`, … — needed to explain the common reasoning-model failure.
    pub finish_reason: Option<String>,
    /// The model emitted reasoning but no answer, which is not the same as an empty string.
    pub had_reasoning: bool,
}

impl ChatResponse {
    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }
}

/// One chat round trip. Implementations must be safe to share across threads.
pub trait Backend: Send + Sync {
    fn chat(&self, cfg: &TierConfig, req: &ChatRequest) -> Result<ChatResponse>;
}

#[derive(Serialize)]
struct WireMessage {
    role: &'static str,
    content: String,
}

pub struct OpenAiCompat {
    agent: ureq::Agent,
}

impl Default for OpenAiCompat {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAiCompat {
    pub fn new() -> OpenAiCompat {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .build();
        OpenAiCompat {
            agent: ureq::Agent::new_with_config(config),
        }
    }

    /// Join `base_url` and `/chat/completions` without doubling or dropping a slash.
    pub fn endpoint(base_url: &str) -> String {
        let base = base_url.trim_end_matches('/');
        if base.ends_with("/chat/completions") {
            base.to_string()
        } else {
            format!("{base}/chat/completions")
        }
    }

    pub fn build_body(cfg: &TierConfig, req: &ChatRequest) -> serde_json::Value {
        let messages = vec![
            WireMessage {
                role: "system",
                content: req.system.clone(),
            },
            WireMessage {
                role: "user",
                content: req.user.clone(),
            },
        ];
        let mut body = json!({
            "model": cfg.model,
            "messages": messages,
            "temperature": req.temperature,
            "max_tokens": req.max_tokens,
            "stream": false,
        });
        if req.json {
            body["response_format"] = json!({"type": "json_object"});
        }
        match req.think {
            // `off` is expressed the way llama.cpp and vLLM both understand.
            Think::Off => body["chat_template_kwargs"] = json!({"enable_thinking": false}),
            level => {
                if let Some(effort) = level.as_effort() {
                    body["reasoning_effort"] = json!(effort);
                }
            }
        }
        body
    }

    pub fn parse_response(raw: &serde_json::Value) -> Result<ChatResponse> {
        let choice = raw
            .get("choices")
            .and_then(|c| c.get(0))
            .ok_or_else(|| anyhow!("response has no choices"))?;
        let message = choice.get("message").unwrap_or(choice);
        let finish_reason = choice
            .get("finish_reason")
            .and_then(|f| f.as_str())
            .map(|s| s.to_string());
        let had_reasoning = message
            .get("reasoning_content")
            .and_then(|c| c.as_str())
            .is_some_and(|s| !s.trim().is_empty());

        let text = message
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("");

        if text.trim().is_empty() {
            // A reasoning model that spends its whole budget thinking returns an empty
            // answer, and the cause is not obvious from a bare "no content". Say which
            // it was, because the fix differs: raise the ceiling, or the model is wrong.
            let reason = finish_reason.as_deref().unwrap_or("unknown");
            return Err(if reason == "length" {
                anyhow!(
                    "the model used its entire token budget before answering \
                     (finish_reason=length, {completion} completion tokens). Raise \
                     models.<tier>.max_tokens; reasoning models need room for reasoning \
                     plus the answer.",
                    completion = raw
                        .get("usage")
                        .and_then(|u| u.get("completion_tokens"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0)
                )
            } else if had_reasoning {
                anyhow!(
                    "the model returned only reasoning and no answer \
                     (finish_reason={reason}). Raise models.<tier>.max_tokens."
                )
            } else {
                anyhow!("response has no message content (finish_reason={reason})")
            });
        }

        let usage = raw.get("usage");
        let tokens = |k: &str| usage.and_then(|u| u.get(k)).and_then(|v| v.as_u64()).unwrap_or(0);
        Ok(ChatResponse {
            text: text.to_string(),
            prompt_tokens: tokens("prompt_tokens"),
            completion_tokens: tokens("completion_tokens"),
            finish_reason,
            had_reasoning,
        })
    }
}

impl Backend for OpenAiCompat {
    fn chat(&self, cfg: &TierConfig, req: &ChatRequest) -> Result<ChatResponse> {
        let url = Self::endpoint(&cfg.base_url);
        let body = Self::build_body(cfg, req);

        let mut call = self
            .agent
            .post(&url)
            .config()
            .timeout_global(Some(Duration::from_millis(cfg.timeout_ms.max(1))))
            .build()
            .header("content-type", "application/json");
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
        let raw: serde_json::Value = response
            .body_mut()
            .read_json()
            .with_context(|| format!("decoding the response from {url}"))?;
        Self::parse_response(&raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tier() -> TierConfig {
        TierConfig {
            base_url: "http://127.0.0.1:9/v1".into(),
            model: "m".into(),
            api_key_env: None,
            timeout_ms: 10,
            max_tokens: 128,
            temperature: 0.0,
            think: Think::Off,
            fim_tokens: None,
        }
    }

    #[test]
    fn endpoint_join_is_idempotent() {
        assert_eq!(OpenAiCompat::endpoint("http://h/v1"), "http://h/v1/chat/completions");
        assert_eq!(OpenAiCompat::endpoint("http://h/v1/"), "http://h/v1/chat/completions");
        assert_eq!(
            OpenAiCompat::endpoint("http://h/v1/chat/completions"),
            "http://h/v1/chat/completions"
        );
    }

    #[test]
    fn body_carries_model_messages_and_thinking_control() {
        let req = ChatRequest {
            system: "sys".into(),
            user: "usr".into(),
            temperature: 0.0,
            max_tokens: 64,
            json: true,
            think: Think::Off,
        };
        let body = OpenAiCompat::build_body(&tier(), &req);
        assert_eq!(body["model"], "m");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "sys");
        assert_eq!(body["messages"][1]["content"], "usr");
        assert_eq!(body["stream"], false);
        assert_eq!(body["response_format"]["type"], "json_object");
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn a_think_level_sends_effort_and_no_template_kwargs() {
        let req = ChatRequest {
            system: "s".into(),
            user: "u".into(),
            temperature: 0.0,
            max_tokens: 8,
            json: false,
            think: Think::Medium,
        };
        let body = OpenAiCompat::build_body(&tier(), &req);
        assert_eq!(body["reasoning_effort"], "medium");
        assert!(body.get("chat_template_kwargs").is_none());
        assert!(body.get("response_format").is_none());
    }

    #[test]
    fn responses_parse_with_and_without_usage() {
        let full = json!({
            "choices": [{"message": {"content": "hello"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 4}
        });
        let r = OpenAiCompat::parse_response(&full).unwrap();
        assert_eq!(r.text, "hello");
        assert_eq!(r.total_tokens(), 7);
        assert_eq!(r.finish_reason.as_deref(), Some("stop"));
        assert!(!r.had_reasoning);

        let minimal = json!({"choices": [{"message": {"content": "x"}}]});
        let r2 = OpenAiCompat::parse_response(&minimal).unwrap();
        assert_eq!(r2.total_tokens(), 0);
        assert_eq!(r2.finish_reason, None);
    }

    #[test]
    fn a_reasoning_model_that_ran_out_of_budget_says_so() {
        // The real failure seen against deepseek-flash through the auth gateway: reasoning
        // consumed the whole ceiling, so `content` came back empty. The old error said
        // only "empty content", which points at the wrong thing.
        let starved = json!({
            "choices": [{
                "message": {"content": "", "reasoning_content": "thinking, and thinking…"},
                "finish_reason": "length"
            }],
            "usage": {"prompt_tokens": 900, "completion_tokens": 1024}
        });
        let e = OpenAiCompat::parse_response(&starved).unwrap_err().to_string();
        assert!(e.contains("finish_reason=length"), "{e}");
        assert!(e.contains("max_tokens"), "the fix must be named: {e}");

        let reasoning_only = json!({
            "choices": [{
                "message": {"content": "", "reasoning_content": "…"},
                "finish_reason": "stop"
            }]
        });
        let e2 = OpenAiCompat::parse_response(&reasoning_only).unwrap_err().to_string();
        assert!(e2.contains("only reasoning"), "{e2}");

        let blank = json!({"choices": [{"message": {"content": "   "}}]});
        let e3 = OpenAiCompat::parse_response(&blank).unwrap_err().to_string();
        assert!(e3.contains("no message content"), "{e3}");
    }

    #[test]
    fn reasoning_alongside_an_answer_is_accepted() {
        // Normal for a thinking model: both fields populated. The answer is what counts.
        let both = json!({
            "choices": [{
                "message": {"content": "{\"findings\":[]}", "reasoning_content": "let me look…"},
                "finish_reason": "stop"
            }]
        });
        let r = OpenAiCompat::parse_response(&both).unwrap();
        assert_eq!(r.text, "{\"findings\":[]}");
        assert!(r.had_reasoning);
    }

    #[test]
    fn degenerate_responses_are_errors_not_panics() {
        for bad in [
            json!({}),
            json!({"choices": []}),
            json!({"choices": [{"message": {"content": null}}]}),
            json!({"choices": [{"message": {"content": "   "}}]}),
            json!({"choices": [{"message": {"reasoning_content": "thinking..."}}]}),
        ] {
            assert!(OpenAiCompat::parse_response(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_unreachable_endpoint_is_an_error_not_a_hang() {
        let backend = OpenAiCompat::new();
        let req = ChatRequest {
            system: "s".into(),
            user: "u".into(),
            temperature: 0.0,
            max_tokens: 8,
            json: false,
            think: Think::Off,
        };
        let cfg = TierConfig {
            timeout_ms: 200,
            ..tier()
        };
        assert!(backend.chat(&cfg, &req).is_err());
    }
}
