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
    /// Prefix and suffix for a fill-in-the-middle call, when the tier's endpoint is one.
    ///
    /// Carried separately from the prompt rather than parsed back out of it: a FIM endpoint
    /// wants the two halves as its own fields, and reading them back out of a rendered prompt
    /// would be this client undoing its own work.
    pub fim: Option<(String, String)>,
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

    /// The same round trip, reporting the answer as it arrives.
    ///
    /// The default waits for the whole answer and reports it once, so a backend that cannot
    /// stream is still a backend. `OpenAiCompat` overrides it with `stream: true`.
    fn chat_stream(
        &self,
        cfg: &TierConfig,
        req: &ChatRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<ChatResponse> {
        let answer = self.chat(cfg, req)?;
        on_delta(&answer.text);
        Ok(answer)
    }
}

/// Accumulates a streaming completion.
///
/// Pure on purpose: everything that decides what an SSE line *means* is testable without a
/// socket, which is where the mistakes are — a keep-alive taken for content, `[DONE]` parsed
/// as JSON, usage dropped because the chunk that carries it has no `choices`.
///
/// llama.cpp, vLLM and OpenAI all send `data: {json}` per event and end with `data: [DONE]`;
/// comment lines (`: ping`) and blank lines are keep-alives.
#[derive(Default)]
pub struct Stream {
    text: String,
    reasoning: String,
    prompt_tokens: u64,
    completion_tokens: u64,
    finish_reason: Option<String>,
}

impl Stream {
    /// Feed one line of the body. Returns the text it added, which is what a caller forwards.
    pub fn push_line(&mut self, line: &str) -> Option<String> {
        let payload = line.trim_end_matches(['\r', '\n']).strip_prefix("data:")?.trim();
        if payload.is_empty() || payload == "[DONE]" {
            return None;
        }
        let chunk: serde_json::Value = serde_json::from_str(payload).ok()?;
        if let Some(usage) = chunk.get("usage").and_then(|u| u.as_object()) {
            let n = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
            self.prompt_tokens = n("prompt_tokens");
            self.completion_tokens = n("completion_tokens");
        }
        let choice = chunk.get("choices").and_then(|c| c.get(0))?;
        if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
            self.finish_reason = Some(reason.to_string());
        }
        let delta = choice.get("delta")?;
        if let Some(thought) = delta.get("reasoning_content").and_then(|c| c.as_str()) {
            self.reasoning.push_str(thought);
        }
        let text = delta.get("content").and_then(|c| c.as_str())?;
        if text.is_empty() {
            return None;
        }
        self.text.push_str(text);
        Some(text.to_string())
    }

    /// The answer, judged by the same function that judges a buffered one: what an empty
    /// answer means must not fork between the streaming and non-streaming paths.
    pub fn into_response(self) -> Result<ChatResponse> {
        OpenAiCompat::parse_response(&json!({
            "choices": [{
                "message": { "content": self.text, "reasoning_content": self.reasoning },
                "finish_reason": self.finish_reason,
            }],
            "usage": {
                "prompt_tokens": self.prompt_tokens,
                "completion_tokens": self.completion_tokens,
            },
        }))
    }
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
    /// One fill-in-the-middle call. Never streamed: a completion has no prose to stream and
    /// the client shows it all at once.
    fn infill(&self, cfg: &TierConfig, req: &ChatRequest) -> Result<ChatResponse> {
        let url = cfg.base_url.trim_end_matches('/').to_string();
        let body = Self::build_infill_body(cfg, req).ok_or_else(|| {
            anyhow!("the fim tier points at a fill-in-the-middle endpoint but the request carries no prefix and suffix")
        })?;
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
        Self::parse_infill_response(&raw)
    }

    pub fn endpoint(base_url: &str) -> String {
        let base = base_url.trim_end_matches('/');
        if base.ends_with("/chat/completions") {
            base.to_string()
        } else {
            format!("{base}/chat/completions")
        }
    }

    /// Is this tier's endpoint a fill-in-the-middle endpoint rather than a chat one?
    ///
    /// The URL is the switch. `llama.cpp` serves FIM at `/infill` and chat at
    /// `/v1/chat/completions`, and a client that has to be told which one it is talking to will
    /// eventually be told wrong; the path already says it.
    pub fn is_infill(base_url: &str) -> bool {
        base_url.trim_end_matches('/').ends_with("/infill")
    }

    /// The body a fill-in-the-middle endpoint expects.
    ///
    /// Not OpenAI-compatible and not pretending to be: `llama.cpp` takes the two halves as
    /// `input_prefix` and `input_suffix` and answers with `content`. Measured against the local
    /// server: the model's own template applies, so the answer is 7 tokens where the chat shape
    /// spends a persona and a paragraph of instructions first.
    pub fn build_infill_body(cfg: &TierConfig, req: &ChatRequest) -> Option<serde_json::Value> {
        let (prefix, suffix) = req.fim.as_ref()?;
        Some(json!({
            // Named even though a single-model server ignores it: a *router* — one llama.cpp
            // serving several presets, which is how this project's own machines run — needs it
            // to know which one to fill with. Verified tolerant on the single-model server and
            // required by the router.
            "model": cfg.model,
            "input_prefix": prefix,
            "input_suffix": suffix,
            "n_predict": req.max_tokens,
            "temperature": req.temperature,
        }))
    }

    /// `llama.cpp`'s answer to a FIM request.
    pub fn parse_infill_response(raw: &serde_json::Value) -> Result<ChatResponse> {
        let text = raw
            .get("content")
            .and_then(|c| c.as_str())
            .ok_or_else(|| anyhow!("a fill-in-the-middle response has no content"))?
            .to_string();
        let n = |k: &str| raw.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        Ok(ChatResponse {
            text,
            prompt_tokens: n("tokens_evaluated"),
            completion_tokens: n("tokens_predicted"),
            finish_reason: raw.get("stop").and_then(|s| s.as_bool()).map(|s| {
                if s { "stop".to_string() } else { "length".to_string() }
            }),
            had_reasoning: false,
        })
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

    /// The same request, asking the server to stream. `include_usage` is ignored by servers
    /// that do not know it, and honoured by the ones that do — llama.cpp and vLLM both send
    /// the token counts in a final chunk with an empty `choices` array when asked.
    pub fn build_stream_body(cfg: &TierConfig, req: &ChatRequest) -> serde_json::Value {
        let mut body = Self::build_body(cfg, req);
        body["stream"] = json!(true);
        body["stream_options"] = json!({ "include_usage": true });
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
        if Self::is_infill(&cfg.base_url) {
            return self.infill(cfg, req);
        }
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

    fn chat_stream(
        &self,
        cfg: &TierConfig,
        req: &ChatRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<ChatResponse> {
        if Self::is_infill(&cfg.base_url) {
            // A FIM endpoint has nothing to stream; ask it once and report the answer whole.
            let answer = self.infill(cfg, req)?;
            on_delta(&answer.text);
            return Ok(answer);
        }
        let url = Self::endpoint(&cfg.base_url);
        let body = Self::build_stream_body(cfg, req);

        let mut call = self
            .agent
            .post(&url)
            .config()
            .timeout_global(Some(Duration::from_millis(cfg.timeout_ms.max(1))))
            .build()
            .header("content-type", "application/json")
            .header("accept", "text/event-stream");
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
        let mut stream = Stream::default();
        let mut raw_body = String::new();
        let mut saw_event = false;
        {
            use std::io::BufRead as _;
            let reader = std::io::BufReader::new(response.body_mut().as_reader());
            for line in reader.lines() {
                let line = line.with_context(|| format!("reading the stream from {url}"))?;
                if line.trim_start().starts_with("data:") {
                    saw_event = true;
                }
                raw_body.push_str(&line);
                raw_body.push('\n');
                if let Some(delta) = stream.push_line(&line) {
                    on_delta(&delta);
                }
            }
        }
        if !saw_event {
            // A server that ignores `stream` answers with one JSON object, and a proxy that
            // strips the header can do the same. Fall back to reading it whole: otherwise a
            // working endpoint is reported as an empty answer, which is the least useful
            // thing to say.
            let raw: serde_json::Value = serde_json::from_str(&raw_body)
                .with_context(|| format!("decoding the response from {url}"))?;
            let answer = Self::parse_response(&raw)?;
            on_delta(&answer.text);
            return Ok(answer);
        }
        stream.into_response()
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
            fim: None,
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
            fim: None,
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

    /// One SSE event, as llama.cpp and OpenAI both send it.
    fn delta(text: &str) -> String {
        json!({"choices": [{"index": 0, "delta": {"content": text}}]}).to_string()
    }

    #[test]
    fn a_stream_line_yields_the_text_it_adds() {
        let mut s = Stream::default();
        assert_eq!(s.push_line(&format!("data: {}", delta("Hel"))).as_deref(), Some("Hel"));
        assert_eq!(s.push_line(&format!("data: {}", delta("lo"))).as_deref(), Some("lo"));
        let out = s.into_response().expect("a complete answer");
        assert_eq!(out.text, "Hello");
    }

    #[test]
    fn keepalives_and_the_sentinel_are_not_content() {
        let mut s = Stream::default();
        for line in [
            "",
            "\r",
            ": ping",
            ": keep-alive",
            "event: message",
            "data: ",
            "data: [DONE]",
            "data: not json at all",
            &format!("data: {}", delta("")),
        ] {
            assert_eq!(s.push_line(line), None, "line {line:?} was taken for content");
        }
        assert!(s.into_response().is_err(), "nothing was said, so there is no answer");
    }

    #[test]
    fn usage_arrives_in_a_chunk_with_no_choices_and_is_kept() {
        let mut s = Stream::default();
        s.push_line(&format!("data: {}", delta("42")));
        s.push_line("data: {\"choices\": [], \"usage\": {\"prompt_tokens\": 120, \"completion_tokens\": 7}}");
        let out = s.into_response().expect("a complete answer");
        assert_eq!(out.text, "42");
        assert_eq!(out.prompt_tokens, 120);
        assert_eq!(out.completion_tokens, 7);
    }

    #[test]
    fn a_finish_reason_is_carried_through() {
        let mut s = Stream::default();
        s.push_line(&format!("data: {}", delta("x")));
        s.push_line("data: {\"choices\": [{\"delta\": {}, \"finish_reason\": \"stop\"}]}");
        assert_eq!(s.into_response().unwrap().finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn a_streamed_answer_with_only_reasoning_is_refused_like_a_buffered_one() {
        let mut s = Stream::default();
        assert_eq!(s.push_line("data: {\"choices\": [{\"delta\": {\"reasoning_content\": \"thinking\"}}]}"), None);
        let err = s.into_response().unwrap_err().to_string();
        assert!(
            err.contains("only reasoning"),
            "the diagnosis a buffered answer gets must survive streaming: {err}"
        );
    }

    #[test]
    fn an_infill_endpoint_is_recognised_by_its_path() {
        assert!(OpenAiCompat::is_infill("http://127.0.0.1:37313/infill"));
        assert!(OpenAiCompat::is_infill("http://127.0.0.1:37313/infill/"));
        assert!(!OpenAiCompat::is_infill("http://127.0.0.1:37313/v1"));
        assert!(!OpenAiCompat::is_infill("http://127.0.0.1:37313/v1/chat/completions"));
    }

    #[test]
    fn a_fim_request_is_the_two_halves_and_no_persona() {
        let req = ChatRequest {
            system: "unused by a FIM endpoint".into(),
            user: "also unused".into(),
            temperature: 0.0,
            max_tokens: 64,
            json: false,
            think: Think::Off,
            fim: Some(("def add(a, b):\n    return ".into(), "\n\ndef sub(a, b):".into())),
        };
        let body = OpenAiCompat::build_infill_body(&tier(), &req).expect("a FIM body");
        assert_eq!(
            body["model"], "m",
            "a router needs to be told which preset to fill with"
        );
        assert_eq!(body["input_prefix"], "def add(a, b):\n    return ");
        assert_eq!(body["input_suffix"], "\n\ndef sub(a, b):");
        assert_eq!(body["n_predict"], 64);
        assert!(
            body.get("messages").is_none(),
            "a fill-in-the-middle endpoint takes no chat messages: {body}"
        );
    }

    #[test]
    fn a_chat_request_has_no_fim_body() {
        let req = ChatRequest {
            system: "s".into(),
            user: "u".into(),
            temperature: 0.0,
            max_tokens: 8,
            json: false,
            think: Think::Off,
            fim: None,
        };
        assert!(OpenAiCompat::build_infill_body(&tier(), &req).is_none());
    }

    #[test]
    fn an_infill_answer_is_content_and_its_token_counts() {
        let raw = json!({"content": " a + b", "tokens_predicted": 7, "tokens_evaluated": 27, "stop": true});
        let out = OpenAiCompat::parse_infill_response(&raw).expect("an answer");
        assert_eq!(out.text, " a + b");
        assert_eq!(out.prompt_tokens, 27);
        assert_eq!(out.completion_tokens, 7);
        assert_eq!(out.finish_reason.as_deref(), Some("stop"));
        assert!(
            OpenAiCompat::parse_infill_response(&json!({"tokens_predicted": 1})).is_err(),
            "an answer with no content is an error, not an empty completion"
        );
    }

    #[test]
    fn the_stream_body_asks_for_streaming_and_usage() {
        let req = ChatRequest {
            system: "s".into(),
            user: "u".into(),
            temperature: 0.0,
            max_tokens: 8,
            json: false,
            think: Think::Off,
            fim: None,
        };
        let body = OpenAiCompat::build_stream_body(&tier(), &req);
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        // everything the buffered body carries still has to be there
        assert_eq!(body["model"], "m");
        assert_eq!(body["messages"][1]["content"], "u");
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
            fim: None,
        };
        let cfg = TierConfig {
            timeout_ms: 200,
            ..tier()
        };
        assert!(backend.chat(&cfg, &req).is_err());
    }
}
