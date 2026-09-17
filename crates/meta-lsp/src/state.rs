//! Shared server state. Locks are held for short, non-async critical sections only.

use meta_core::budget::Budget;
use meta_core::cache::Cache;
use meta_core::config::Config;
use meta_core::document::Document;
use meta_core::model::Backend;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::sync::Arc;

/// One document's analysis slot. At most one run per document at a time, and a request
/// that arrives during a run is *remembered*, never dropped.
#[derive(Debug, Default, Clone, Copy)]
struct AnalysisSlot {
    running: bool,
    pending: bool,
}

/// What a caller got when asking for an analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// This caller owns the run and must call [`AppState::finish_analysis`] when done.
    Run,
    /// A run is already in flight; it has been told to run again afterwards.
    Queued,
}

/// A snapshot taken before an applied edit, so `meta.revert` can restore it.
#[derive(Debug, Clone)]
pub struct AppliedEdit {
    pub uri: String,
    pub before: String,
}

pub struct AppState {
    docs: RwLock<HashMap<String, Document>>,
    /// Per-document generation counter, so a slow analysis cannot publish over a newer one.
    generations: Mutex<HashMap<String, u64>>,
    /// Per-document analysis slots. Opening the menu repeatedly must not stack up calls
    /// (PROTOCOL.md §5, backpressure), and a request that cannot start yet must not vanish.
    analysis: Mutex<HashMap<String, AnalysisSlot>>,
    /// Plans the user is looking at. Session state, deliberately not persisted: a plan is a
    /// continuation handle, not a source of truth (PROTOCOL.md N9).
    plans: Mutex<HashMap<String, meta_core::types::Plan>>,
    /// What each applied edit replaced, for `meta.revert`.
    applied: Mutex<HashMap<String, AppliedEdit>>,
    /// The text each server-applied edit was predicted to produce, checked against what
    /// actually arrives on the next sync (PROTOCOL.md §8).
    predictions: Mutex<HashMap<String, String>>,
    /// A separate window for the `fim` tier: completions fire on typing and must not starve
    /// the work the user explicitly asked for.
    pub fim: crate::inline::FimLimiter,
    pub cache: Cache,
    pub budget: Budget,
    config: RwLock<Config>,
    pub backend: Arc<dyn Backend>,
    root: RwLock<Option<String>>,
}

impl AppState {
    pub fn new(backend: Arc<dyn Backend>, config: Config) -> Arc<AppState> {
        Arc::new(AppState {
            docs: RwLock::new(HashMap::new()),
            generations: Mutex::new(HashMap::new()),
            analysis: Mutex::new(HashMap::new()),
            plans: Mutex::new(HashMap::new()),
            applied: Mutex::new(HashMap::new()),
            predictions: Mutex::new(HashMap::new()),
            fim: crate::inline::FimLimiter::new(),
            cache: Cache::new(512),
            budget: Budget::new(2),
            config: RwLock::new(config),
            backend,
            root: RwLock::new(None),
        })
    }

    pub fn config(&self) -> Config {
        self.config.read().clone()
    }

    /// Apply a `workspace/configuration` payload over the current settings. The
    /// environment is re-applied afterwards, so a client with no opinion about model
    /// endpoints cannot undo what the shell set.
    pub fn merge_config(&self, value: Option<&serde_json::Value>) {
        let current = self.config.read().clone();
        let mut next = current.merged_with(value);
        next.apply_env_overrides();
        *self.config.write() = next;
    }

    pub fn doc(&self, uri: &str) -> Option<Document> {
        self.docs.read().get(uri).cloned()
    }

    pub fn put_doc(&self, doc: Document) {
        self.docs.write().insert(doc.uri.clone(), doc);
    }

    pub fn remove_doc(&self, uri: &str) -> Option<Document> {
        self.generations.lock().remove(uri);
        self.docs.write().remove(uri)
    }

    pub fn doc_count(&self) -> usize {
        self.docs.read().len()
    }

    pub fn all_docs(&self) -> Vec<Document> {
        self.docs.read().values().cloned().collect()
    }

    /// Bump and read the generation for a document.
    pub fn bump_generation(&self, uri: &str) -> u64 {
        let mut g = self.generations.lock();
        let entry = g.entry(uri.to_string()).or_insert(0);
        *entry += 1;
        *entry
    }

    pub fn generation(&self, uri: &str) -> u64 {
        self.generations.lock().get(uri).copied().unwrap_or(0)
    }

    /// Claim this document's analysis slot.
    pub fn claim_analysis(&self, uri: &str) -> Claim {
        let mut slots = self.analysis.lock();
        let entry = slots.entry(uri.to_string()).or_default();
        if entry.running {
            entry.pending = true;
            Claim::Queued
        } else {
            entry.running = true;
            Claim::Run
        }
    }

    /// Remember a plan for the session, keyed by its id.
    pub fn put_plan(&self, plan: meta_core::types::Plan) {
        let mut plans = self.plans.lock();
        // Keep the map from growing without bound over a long session.
        if plans.len() >= 32 {
            if let Some(oldest) = plans.keys().next().cloned() {
                plans.remove(&oldest);
            }
        }
        plans.insert(plan.id.clone(), plan);
    }

    pub fn plan(&self, id: &str) -> Option<meta_core::types::Plan> {
        self.plans.lock().get(id).cloned()
    }

    pub fn mark_step(&self, plan_id: &str, n: u32, status: meta_core::types::StepStatus) {
        if let Some(plan) = self.plans.lock().get_mut(plan_id) {
            if let Some(step) = plan.steps.iter_mut().find(|s| s.n == n) {
                step.status = status;
            }
        }
    }

    pub fn plan_count(&self) -> usize {
        self.plans.lock().len()
    }

    /// Record what an applied edit replaced, so it can be reverted.
    pub fn record_applied(&self, edit_id: String, record: AppliedEdit) {
        let mut applied = self.applied.lock();
        if applied.len() >= 64 {
            if let Some(oldest) = applied.keys().next().cloned() {
                applied.remove(&oldest);
            }
        }
        applied.insert(edit_id, record);
    }

    pub fn take_applied(&self, edit_id: &str) -> Option<AppliedEdit> {
        self.applied.lock().remove(edit_id)
    }

    /// Remember what a server-applied edit should produce.
    pub fn remember_prediction(&self, uri: &str, text: String) {
        self.predictions.lock().insert(uri.to_string(), text);
    }

    /// Take the outstanding prediction for a document, if any. Taken rather than read: a
    /// prediction is checked once.
    pub fn take_prediction(&self, uri: &str) -> Option<String> {
        self.predictions.lock().remove(uri)
    }

    /// Release the slot. True means a request arrived while we ran and wants another pass.
    pub fn finish_analysis(&self, uri: &str) -> bool {
        let mut slots = self.analysis.lock();
        match slots.get_mut(uri) {
            Some(slot) if slot.pending => {
                slot.pending = false;
                true
            }
            Some(slot) => {
                slot.running = false;
                false
            }
            None => false,
        }
    }

    pub fn analyses_running(&self) -> usize {
        self.analysis.lock().values().filter(|s| s.running).count()
    }

    pub fn is_analyzing(&self) -> bool {
        self.analyses_running() > 0
    }

    pub fn set_root(&self, root: Option<String>) {
        *self.root.write() = root;
    }

    pub fn root(&self) -> Option<String> {
        self.root.read().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meta_core::model::{ChatRequest, ChatResponse};
    use meta_core::config::TierConfig;
    use anyhow::Result;

    struct Null;
    impl Backend for Null {
        fn chat(&self, _c: &TierConfig, _r: &ChatRequest) -> Result<ChatResponse> {
            Ok(ChatResponse {
                text: "{}".into(),
                prompt_tokens: 0,
                completion_tokens: 0,
                finish_reason: Some("stop".into()),
                had_reasoning: false,
            })
        }
    }

    fn state() -> Arc<AppState> {
        AppState::new(Arc::new(Null), Config::default())
    }

    #[test]
    fn documents_are_stored_and_removed() {
        let s = state();
        assert!(s.doc("file:///a").is_none());
        s.put_doc(Document::new("file:///a", 1, "x".into(), Some("rust")));
        assert_eq!(s.doc("file:///a").unwrap().version, 1);
        assert_eq!(s.doc_count(), 1);
        assert!(s.remove_doc("file:///a").is_some());
        assert_eq!(s.doc_count(), 0);
    }

    #[test]
    fn generations_increase_per_document() {
        let s = state();
        assert_eq!(s.generation("file:///a"), 0);
        assert_eq!(s.bump_generation("file:///a"), 1);
        assert_eq!(s.bump_generation("file:///a"), 2);
        assert_eq!(s.bump_generation("file:///b"), 1);
        assert_eq!(s.generation("file:///a"), 2);
    }

    #[test]
    fn config_merges_over_the_defaults() {
        let s = state();
        s.merge_config(Some(&serde_json::json!({"enabled": false})));
        assert!(!s.config().enabled);
        s.merge_config(None);
        assert!(!s.config().enabled, "a missing payload keeps what we had");
    }

    #[test]
    fn root_is_remembered() {
        let s = state();
        assert!(s.root().is_none());
        s.set_root(Some("/tmp/w".into()));
        assert_eq!(s.root().as_deref(), Some("/tmp/w"));
    }

    #[test]
    fn a_second_request_during_a_run_is_queued_not_dropped() {
        let s = state();
        assert_eq!(s.claim_analysis("file:///a"), Claim::Run);
        assert_eq!(s.claim_analysis("file:///a"), Claim::Queued);
        assert_eq!(s.claim_analysis("file:///a"), Claim::Queued);
        assert_eq!(s.analyses_running(), 1);
        // The queued requests turn into exactly one follow-up pass.
        assert!(s.finish_analysis("file:///a"), "a follow-up was requested");
        assert_eq!(s.analyses_running(), 1, "still owned by the running task");
        assert!(!s.finish_analysis("file:///a"), "no further follow-up");
        assert_eq!(s.analyses_running(), 0);
    }

    #[test]
    fn documents_do_not_block_each_other() {
        let s = state();
        assert_eq!(s.claim_analysis("file:///a"), Claim::Run);
        assert_eq!(
            s.claim_analysis("file:///b"),
            Claim::Run,
            "one document's analysis must not starve another"
        );
        assert_eq!(s.analyses_running(), 2);
        s.finish_analysis("file:///a");
        s.finish_analysis("file:///b");
        assert!(!s.is_analyzing());
    }

    #[test]
    fn finishing_an_unknown_document_is_harmless() {
        let s = state();
        assert!(!s.finish_analysis("file:///never"));
        assert_eq!(s.analyses_running(), 0);
    }
}
