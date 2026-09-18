//! Shared server state. Locks are held for short, non-async critical sections only.

use meta_core::budget::Budget;
use meta_core::cache::Cache;
use meta_core::config::Config;
use meta_core::document::Document;
use meta_core::model::Backend;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// What the client knows about one version of a document (PROTOCOL §3.4.3, §6.1).
///
/// One entry, one version guard: declarations for the surfaces that enumerate (`codeLens`,
/// `inlayHint`) and standing context for the path that cannot assemble it per request — the
/// completion, which fires on a timer while the user types.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KnownDocument {
    pub version: i32,
    #[serde(default)]
    pub definitions: Vec<ClientDefinition>,
    #[serde(default)]
    pub context: Vec<meta_core::context::Provided>,
}

/// A declaration the *client* found with its own parser (PROTOCOL §3.4.3).
///
/// The client has the parser and this side does not, by design (`LANGUAGE.md` §4). The plugin
/// sends what treesitter found, version-stamped, and the server uses it in place of its own
/// structural scan. Definitions whose version no longer matches the document are ignored, so a
/// stale set means the structural answer rather than a lens pointing at the wrong line.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClientDefinition {
    pub start_line: u32,
    pub end_line: u32,
    #[serde(default)]
    pub name: Option<String>,
}

/// The last artifact produced for a scope.
///
/// Hover reads this and never calls a model. A hover that waits ten seconds is one nobody
/// uses, and an explanation the user already asked for is worth showing again — on the symbol
/// they are pointing at, with no key to remember.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredArtifact {
    pub content_hash: String,
    pub markdown: String,
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
    /// Plans, oldest first. A `Vec` rather than a map because the bound is 32 and eviction has
    /// to take the *oldest*: a plan a user is reading should not disappear while a newer one
    /// arrives, and a map's iteration order cannot promise that.
    plans: Mutex<Vec<meta_core::types::Plan>>,
    /// Explanations and answers, oldest first, so hover can repeat one for free.
    artifacts: Mutex<Vec<(String, StoredArtifact)>>,
    /// What the client sent, by uri, with the version it describes.
    known: Mutex<std::collections::HashMap<String, KnownDocument>>,
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
    /// Set once a client has asked for inlay hints at least once.
    ///
    /// `workspace/inlayHint/refresh` is broadcast by the client to *every* attached server, and
    /// an inlay-hint request carries no document version — so a refresh sent for hints nobody
    /// displays is a chance for an unrelated server to answer with positions from before the
    /// edit. Nothing is refreshed until something has been asked for.
    hints_asked: AtomicBool,
    /// Set once the client's `meta` section has been read.
    ///
    /// A buffer saved during startup can be analysed before the first
    /// `workspace/configuration` round trip finishes, and the analysis would then run against
    /// the built-in defaults — a client that configured an endpoint and watched the server
    /// call somewhere else. Model work waits for this instead of being dropped.
    config_ready: AtomicBool,
    config_notify: tokio::sync::Notify,
    pub backend: Arc<dyn Backend>,
    root: RwLock<Option<String>>,
}

impl AppState {
    pub fn new(backend: Arc<dyn Backend>, config: Config) -> Arc<AppState> {
        Arc::new(AppState {
            docs: RwLock::new(HashMap::new()),
            generations: Mutex::new(HashMap::new()),
            analysis: Mutex::new(HashMap::new()),
            plans: Mutex::new(Vec::new()),
            known: Mutex::new(std::collections::HashMap::new()),
            artifacts: Mutex::new(Vec::new()),
            applied: Mutex::new(HashMap::new()),
            predictions: Mutex::new(HashMap::new()),
            fim: crate::inline::FimLimiter::new(),
            cache: Cache::new(512),
            budget: Budget::new(2),
            config: RwLock::new(config),
            hints_asked: AtomicBool::new(false),
            config_ready: AtomicBool::new(false),
            config_notify: tokio::sync::Notify::new(),
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
    /// Returns the parse error when the payload could not be applied, so the caller can say
    /// so rather than leaving the server quietly on its defaults.
    pub fn merge_config(&self, value: Option<&serde_json::Value>) -> Option<String> {
        let current = self.config.read().clone();
        let mut next = match current.try_merged_with(value) {
            Ok(next) => next,
            Err(e) => return Some(e),
        };
        next.apply_env_overrides();
        *self.config.write() = next;
        self.mark_config_ready();
        None
    }

    /// True once a client has asked this server for inlay hints.
    pub fn hints_are_wanted(&self) -> bool {
        self.hints_asked.load(Ordering::Acquire)
    }

    /// Note that hints were asked for, so refreshing them is worth doing.
    pub fn mark_hints_asked(&self) {
        self.hints_asked.store(true, Ordering::Release);
    }

    /// True once the client's settings have been read at least once.
    pub fn config_is_ready(&self) -> bool {
        self.config_ready.load(Ordering::Acquire)
    }

    /// Wait for the first configuration read, so model work is not started against defaults.
    /// Bounded: a client that never answers must not stall the server forever.
    pub async fn await_config(&self, limit: std::time::Duration) {
        if self.config_is_ready() {
            return;
        }
        // Subscribe *before* the second check. `notified()` only registers when it is first
        // polled, so a configuration landing between the check above and that first poll would
        // miss the wake and stall for the whole limit: five seconds of delay on a save that
        // should have been analysed the moment it was written.
        let notified = self.config_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.config_is_ready() {
            return;
        }
        let _ = tokio::time::timeout(limit, notified).await;
    }

    /// The client has been asked for its settings and has answered — or cannot answer at all.
    /// Either way there is nothing left to wait for.
    ///
    /// Called on failure too: a `meta` block that will not merge is reported as an error, and
    /// leaving the gate shut would make every model call afterwards wait out the limit. The
    /// defaults are the truth from that moment; they were going to be anyway.
    pub fn mark_config_ready(&self) {
        self.config_ready.store(true, Ordering::Release);
        self.config_notify.notify_waiters();
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
        // Keep the list from growing without bound over a long session, dropping the oldest:
        // what a user is still reading is the newest.
        while plans.len() >= 32 {
            plans.remove(0);
        }
        plans.push(plan);
    }

    /// Key for a scope: the document, and the lines the artifact covers.
    ///
    /// `uri` can contain `|`, so the parts are split from the right.
    fn artifact_key(uri: &str, start: u32, end: u32) -> String {
        format!("{uri}|{start}|{end}")
    }

    fn split_artifact_key(key: &str) -> Option<(String, u32, u32)> {
        let mut parts = key.rsplitn(3, '|');
        let end: u32 = parts.next()?.parse().ok()?;
        let start: u32 = parts.next()?.parse().ok()?;
        Some((parts.next()?.to_string(), start, end))
    }

    /// Remember an artifact, replacing whatever was there for that exact scope.
    pub fn note_artifact(&self, uri: &str, start: u32, end: u32, content_hash: &str, markdown: &str) {
        let key = Self::artifact_key(uri, start, end);
        let mut artifacts = self.artifacts.lock();
        artifacts.retain(|(k, _)| k != &key);
        while artifacts.len() >= 64 {
            artifacts.remove(0);
        }
        artifacts.push((
            key,
            StoredArtifact {
                content_hash: content_hash.to_string(),
                markdown: markdown.to_string(),
            },
        ));
    }

    /// The newest artifact that *covers* a line, while it still describes this content.
    ///
    /// Covering, not equal: an explanation was asked for about a scope, and hovering anywhere
    /// inside that scope is the same question. Requiring the two sides to agree on an exact
    /// extent would make hover work only when a parser and a scan resolve a declaration the
    /// same way, which is not the same thing at all. The content hash still has to match — a
    /// stale explanation shown against lines it was not written about is worse than none.
    pub fn artifact_covering(&self, uri: &str, line: u32, content_hash: &str) -> Option<(u32, u32, String)> {
        self.artifacts.lock().iter().rev().find_map(|(key, a)| {
            let (stored_uri, start, end) = Self::split_artifact_key(key)?;
            if stored_uri == uri && a.content_hash == content_hash && start <= line && line <= end {
                Some((start, end, a.markdown.clone()))
            } else {
                None
            }
        })
    }

    /// Record what the client sent about one document version.
    pub fn put_known(&self, uri: &str, known: KnownDocument) {
        self.known.lock().insert(uri.to_string(), known);
    }

    /// Everything the client sent, but only for the version it sent it for: an answer about a
    /// document that has moved on is worse than no answer, because a lens would point at
    /// whatever now occupies those lines.
    fn known_for(&self, uri: &str, version: i32) -> Option<KnownDocument> {
        self.known
            .lock()
            .get(uri)
            .filter(|k| k.version == version)
            .cloned()
    }

    pub fn definitions(&self, uri: &str, version: i32) -> Option<Vec<ClientDefinition>> {
        self.known_for(uri, version).map(|k| k.definitions)
    }

    /// The standing context for a document, for paths that cannot assemble it per request.
    pub fn standing_context(&self, uri: &str, version: i32) -> Vec<meta_core::context::Provided> {
        self.known_for(uri, version).map(|k| k.context).unwrap_or_default()
    }

    pub fn plan(&self, id: &str) -> Option<meta_core::types::Plan> {
        self.plans
            .lock()
            .iter()
            .find(|p| p.id == id)
            .cloned()
    }

    pub fn mark_step(&self, plan_id: &str, n: u32, status: meta_core::types::StepStatus) {
        if let Some(plan) = self
            .plans
            .lock()
            .iter_mut()
            .find(|p| p.id == plan_id)
        {
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

    /// A plan with nothing in it: the store keys, counts and evicts by identity alone.
    fn plan_with(id: &str) -> meta_core::types::Plan {
        meta_core::types::Plan {
            id: id.to_string(),
            goal: "g".into(),
            language: "rust".into(),
            steps: Vec::new(),
            usage: meta_core::types::Usage {
                model: "m".into(),
                tier: "reason".into(),
                tokens_in: 0,
                tokens_out: 0,
                ms: 0,
                changes: None,
                files: None,
            },
        }
    }

    #[test]
    fn client_definitions_are_used_only_for_the_version_they_describe() {
        let s = AppState::new(Arc::new(Null), Config::default());
        let defs = || {
            vec![ClientDefinition {
                start_line: 4,
                end_line: 8,
                name: Some("f".into()),
            }]
        };
        assert!(s.definitions("file:///a.c", 1).is_none(), "nothing sent, nothing to use");

        s.put_known(
            "file:///a.c",
            KnownDocument {
                version: 1,
                definitions: defs(),
                context: Vec::new(),
            },
        );
        assert!(s.definitions("file:///a.c", 1).is_some(), "the version it describes");
        assert!(
            s.definitions("file:///a.c", 2).is_none(),
            "a document that moved on gets the structural scan, not a stale set of lines"
        );

        s.put_known(
            "file:///a.c",
            KnownDocument {
                version: 2,
                definitions: defs(),
                context: Vec::new(),
            },
        );
        assert!(s.definitions("file:///a.c", 2).is_some(), "and the next push replaces it");
        assert!(s.definitions("file:///b.c", 2).is_none(), "per document, not global");
    }

    #[test]
    fn the_oldest_plan_is_the_one_evicted() {
        let s = AppState::new(Arc::new(Null), Config::default());
        for i in 0..40 {
            s.put_plan(plan_with(&format!("plan-{i}")));
        }
        assert!(
            s.plan("plan-0").is_none(),
            "the first plan put in is gone once the bound is reached"
        );
        assert!(
            s.plan("plan-39").is_some(),
            "and the newest is the one still there"
        );
        assert!(s.plan("plan-8").is_some(), "nothing newer than the bound was evicted");
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
