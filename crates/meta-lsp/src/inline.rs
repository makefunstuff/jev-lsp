//! Inline completion: the wire types and the rate limiter.
//!
//! `textDocument/inlineCompletion` is 3.18 draft, so `lsp-types` 0.94 has no types for it and
//! `tower-lsp` has no handler method. Both are supplied here and registered as a custom
//! method; the capability is injected by `advertised`.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tower_lsp::lsp_types::{Position, Range, TextDocumentIdentifier};

#[derive(Debug, Deserialize)]
pub struct InlineParams {
    #[serde(rename = "textDocument")]
    pub text_document: TextDocumentIdentifier,
    pub position: Position,
    #[serde(default)]
    pub context: Option<InlineContext>,
}

#[derive(Debug, Deserialize)]
pub struct InlineContext {
    #[serde(rename = "triggerKind", default)]
    pub trigger_kind: Option<u32>,
}

#[derive(Debug, Serialize, Default)]
pub struct InlineList {
    pub items: Vec<InlineItem>,
}

#[derive(Debug, Serialize)]
pub struct InlineItem {
    #[serde(rename = "insertText")]
    pub insert_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
}

/// A separate window for the `fim` tier.
///
/// Inline completion fires on typing, so it is the one thing here that can genuinely run away
/// with the budget. It gets its own counter rather than sharing the global one: a burst of
/// completions must not starve the explicit work the user asked for, and vice versa
/// (PROTOCOL.md §5, `inline_completion.max_calls_per_min`).
pub struct FimLimiter {
    calls: Mutex<VecDeque<Instant>>,
}

impl Default for FimLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl FimLimiter {
    pub fn new() -> FimLimiter {
        FimLimiter {
            calls: Mutex::new(VecDeque::new()),
        }
    }

    /// True, and records the call, when the last minute has room.
    pub fn try_acquire(&self, max_per_min: u32) -> bool {
        self.try_acquire_at(Instant::now(), max_per_min)
    }

    fn try_acquire_at(&self, now: Instant, max_per_min: u32) -> bool {
        let mut calls = self.calls.lock();
        let window = Duration::from_secs(60);
        while calls.front().is_some_and(|t| now.duration_since(*t) >= window) {
            calls.pop_front();
        }
        if calls.len() as u32 >= max_per_min {
            return false;
        }
        calls.push_back(now);
        true
    }

    pub fn calls_last_minute(&self) -> u32 {
        let mut calls = self.calls.lock();
        let now = Instant::now();
        while calls
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            calls.pop_front();
        }
        calls.len() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fim_window_is_separate_and_slides() {
        let limiter = FimLimiter::new();
        let t0 = Instant::now();
        assert!(limiter.try_acquire_at(t0, 2));
        assert!(limiter.try_acquire_at(t0, 2));
        assert!(!limiter.try_acquire_at(t0, 2), "the third in a minute is refused");
        assert!(
            limiter.try_acquire_at(t0 + Duration::from_secs(61), 2),
            "and the window slides"
        );
    }

    #[test]
    fn a_limit_of_zero_refuses_everything() {
        let limiter = FimLimiter::new();
        assert!(!limiter.try_acquire(0));
    }

    #[test]
    fn the_count_reflects_what_was_spent() {
        let limiter = FimLimiter::new();
        assert_eq!(limiter.calls_last_minute(), 0);
        limiter.try_acquire(5);
        limiter.try_acquire(5);
        assert_eq!(limiter.calls_last_minute(), 2);
    }

    #[test]
    fn a_response_serialises_the_way_the_spec_spells_it() {
        let list = InlineList {
            items: vec![InlineItem {
                insert_text: "return 1".to_string(),
                range: None,
            }],
        };
        let json = serde_json::to_value(&list).unwrap();
        assert_eq!(json["items"][0]["insertText"], "return 1");
        assert!(json["items"][0].get("range").is_none(), "absent, not null");
    }

    #[test]
    fn a_request_parses_the_draft_shape_nvim_sends() {
        let params: InlineParams = serde_json::from_str(
            r#"{"textDocument":{"uri":"file:///a.rs"},"position":{"line":1,"character":4},
                "context":{"triggerKind":2}}"#,
        )
        .unwrap();
        assert_eq!(params.position.line, 1);
        assert_eq!(params.context.unwrap().trigger_kind, Some(2));
    }
}
