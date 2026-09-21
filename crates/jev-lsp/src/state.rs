//! Shared server state. Locks are held for short, non-async critical sections only.

use jev_core::budget::Budget;
use jev_core::cache::Cache;
use jev_core::config::Config;
use jev_core::decision::DecisionBackend;
use jev_core::document::Document;
use jev_core::model::Backend;
use jev_core::rules::{self, RuleSet};
use parking_lot::{Mutex, RwLock};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// One document's analysis slot. At most one run per document at a time, and a request
/// that arrives during a run is *remembered*, never dropped.
#[derive(Debug, Default, Clone, Copy)]
struct AnalysisSlot {
    running: bool,
    pending: bool,
}

/// One document's *rules* slot.
///
/// The same shape and the same promise as the analysis slot — at most one run per document, a
/// request that arrives during a run is remembered rather than dropped — with one difference:
/// the pending count is bounded. A document that changes on every keystroke can ask for a rules
/// pass faster than a decision can answer, and an unbounded queue would be a way to spend a
/// session's whole budget on one file. Past the cap, further requests coalesce into the pending
/// passes instead of adding work.
#[derive(Debug, Default, Clone, Copy)]
struct RulesSlot {
    running: bool,
    pending: u32,
}

/// How many follow-up rules passes one document may have queued behind the running one.
const MAX_PENDING_RULES: u32 = 4;

/// What the last rules pass did, for `jev.status`. Zeros before the first pass.
#[derive(Debug, Clone, Default)]
pub struct RulesStats {
    pub loaded: usize,
    pub hash: String,
    pub last_pass_ms: u64,
    pub candidates: usize,
    pub calls: u64,
    /// How many problems `rules::lint` found in the rules document itself, as of that pass.
    /// Reported, never enforced: a rule that fails lint still runs (`rules::lint`).
    pub lint: usize,
}

/// How long the answer to "which files has git seen change" is reused.
///
/// One pass covers up to `rules.max_files_per_pass` documents and must not shell out once per
/// document. Short on purpose: a save that lands a moment later still has to see itself.
const CHANGED_TTL: Duration = Duration::from_millis(2000);

/// What a caller got when asking for an analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// This caller owns the run and must call [`AppState::finish_analysis`] when done.
    Run,
    /// A run is already in flight; it has been told to run again afterwards.
    Queued,
}

/// A snapshot taken before an applied edit, so `jev.revert` can restore it.
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
    pub context: Vec<jev_core::context::Provided>,
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
    /// The same, for the rules pass, which has its own concurrency and its own trigger.
    rules_slots: Mutex<HashMap<String, RulesSlot>>,
    /// The content a pull has already started a rules pass for, by document.
    ///
    /// A pass that *skips* — an unchanged file, a path no rule claims — caches nothing, because
    /// there is no conclusion to cache. Without this mark a pull would start another pass for the
    /// same content, whose refresh brings the client back for another, and so on: measured at
    /// 136,292 pulls in 30 seconds before it was added. Marked per content, so an edit (a new
    /// hash) is asked about again.
    pull_passes: Mutex<HashMap<String, String>>,
    /// Rule sets that have been read, by root. The hash is what makes the entry reusable: the
    /// files are re-read on every pass (an edit must be noticed) and the cached set answers when
    /// nothing changed.
    rule_sets: Mutex<HashMap<String, Arc<RuleSet>>>,
    /// What the last rules pass did, for `jev.status`.
    rules_stats: Mutex<RulesStats>,
    /// The last answer to "which files has git seen change", with the root it was about.
    changed: Mutex<Option<(String, Instant, Result<Arc<HashSet<String>>, String>)>>,
    /// Why that question could not be answered, if it could not. Drained once by the caller that
    /// can say so to the user.
    changed_note: Mutex<Option<String>>,
    /// Plans the user is looking at. Session state, deliberately not persisted: a plan is a
    /// continuation handle, not a source of truth (PROTOCOL.md N9).
    /// Plans, oldest first. A `Vec` rather than a map because the bound is 32 and eviction has
    /// to take the *oldest*: a plan a user is reading should not disappear while a newer one
    /// arrives, and a map's iteration order cannot promise that.
    plans: Mutex<Vec<jev_core::types::Plan>>,
    /// Explanations and answers, oldest first, so hover can repeat one for free.
    artifacts: Mutex<Vec<(String, StoredArtifact)>>,
    /// What the client sent, by uri, with the version it describes.
    known: Mutex<std::collections::HashMap<String, KnownDocument>>,
    /// What each applied edit replaced, for `jev.revert`.
    applied: Mutex<HashMap<String, AppliedEdit>>,
    /// The text each server-applied edit was predicted to produce, checked against what
    /// actually arrives on the next sync (PROTOCOL.md §8).
    predictions: Mutex<HashMap<String, String>>,
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
    /// Set once the client's `jev` section has been read.
    ///
    /// A buffer saved during startup can be analysed before the first
    /// `workspace/configuration` round trip finishes, and the analysis would then run against
    /// the built-in defaults — a client that configured an endpoint and watched the server
    /// call somewhere else. Model work waits for this instead of being dropped.
    config_ready: AtomicBool,
    config_notify: tokio::sync::Notify,
    pub backend: Arc<dyn Backend>,
    /// The decision tier: the ambient path's only model client. Injected for the same reason the
    /// chat backend is — the whole rules pass is exercisable without a socket.
    pub decision: Arc<dyn DecisionBackend>,
    /// The shipped rule set this server merges under the repository's own (PROTOCOL.md §9).
    ///
    /// Injected rather than read from the crate's `builtin_files()` at the call site, for the
    /// same reason the two backends are: the pass a server runs and the pass a test runs are
    /// then the same pass over a substitutable input. `main` passes the embedded set.
    builtin: &'static [(&'static str, &'static str)],
    root: RwLock<Option<String>>,
}

impl AppState {
    pub fn new(
        backend: Arc<dyn Backend>,
        decision: Arc<dyn DecisionBackend>,
        config: Config,
        builtin: &'static [(&'static str, &'static str)],
    ) -> Arc<AppState> {
        Arc::new(AppState {
            docs: RwLock::new(HashMap::new()),
            generations: Mutex::new(HashMap::new()),
            analysis: Mutex::new(HashMap::new()),
            rules_slots: Mutex::new(HashMap::new()),
            pull_passes: Mutex::new(HashMap::new()),
            rule_sets: Mutex::new(HashMap::new()),
            rules_stats: Mutex::new(RulesStats::default()),
            changed: Mutex::new(None),
            changed_note: Mutex::new(None),
            plans: Mutex::new(Vec::new()),
            known: Mutex::new(std::collections::HashMap::new()),
            artifacts: Mutex::new(Vec::new()),
            applied: Mutex::new(HashMap::new()),
            predictions: Mutex::new(HashMap::new()),
            cache: Cache::new(512),
            budget: Budget::new(2),
            config: RwLock::new(config),
            hints_asked: AtomicBool::new(false),
            config_ready: AtomicBool::new(false),
            config_notify: tokio::sync::Notify::new(),
            backend,
            decision,
            builtin,
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
    /// Called on failure too: a `jev` block that will not merge is reported as an error, and
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
    pub fn put_plan(&self, plan: jev_core::types::Plan) {
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

    pub fn plan(&self, id: &str) -> Option<jev_core::types::Plan> {
        self.plans
            .lock()
            .iter()
            .find(|p| p.id == id)
            .cloned()
    }

    pub fn mark_step(&self, plan_id: &str, n: u32, status: jev_core::types::StepStatus) {
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

    /// Record that a pull started a pass for this content. False when it already had.
    pub fn mark_pull_pass(&self, uri: &str, hash: &str) -> bool {
        let mut marks = self.pull_passes.lock();
        if marks.get(uri).is_some_and(|seen| seen == hash) {
            return false;
        }
        marks.insert(uri.to_string(), hash.to_string());
        true
    }

    /// Forget them, for `jev.recompute`: the point of a recompute is to ask again.
    pub fn forget_pull_passes(&self) {
        self.pull_passes.lock().clear();
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

    // ---- the rules pass -----------------------------------------------------

    /// The rules for a workspace root: `.jev/rules/*.{json,yaml,yml}`, plus the shipped set when
    /// `rules.defaults` is on.
    ///
    /// The files are read on every call, because a rule the user just edited has to take effect
    /// on the next save; what the cache buys is the identity, not the I/O — when the hash is
    /// unchanged the previous `Arc` is handed back, so the parsed set is shared rather than
    /// rebuilt for every document in a pass. The hash covers the merged set, so flipping
    /// `rules.defaults` between passes is a different hash and re-parses rather than serving the
    /// previous mixture.
    ///
    /// `defaults` is passed in rather than read here so the caller's already-resolved config is
    /// the one that applies: a pass must not read one setting from the config it was given and
    /// another from the state's current one.
    pub fn rule_set(&self, root: &Path, defaults: bool) -> Arc<RuleSet> {
        let key = root.display().to_string();
        let loaded = Arc::new(rules::load(root, defaults, self.builtin));
        let mut cache = self.rule_sets.lock();
        if let Some(existing) = cache.get(&key) {
            if existing.hash == loaded.hash {
                return existing.clone();
            }
        }
        cache.insert(key, loaded.clone());
        loaded
    }

    /// Claim this document's rules slot.
    pub fn claim_rules(&self, uri: &str) -> Claim {
        let mut slots = self.rules_slots.lock();
        let entry = slots.entry(uri.to_string()).or_default();
        if entry.running {
            entry.pending = (entry.pending + 1).min(MAX_PENDING_RULES);
            Claim::Queued
        } else {
            entry.running = true;
            Claim::Run
        }
    }

    /// Release the slot. True means a request arrived while we ran and wants another pass.
    pub fn finish_rules(&self, uri: &str) -> bool {
        let mut slots = self.rules_slots.lock();
        match slots.get_mut(uri) {
            Some(slot) if slot.pending > 0 => {
                slot.pending -= 1;
                true
            }
            Some(slot) => {
                slot.running = false;
                false
            }
            None => false,
        }
    }

    pub fn rules_running(&self) -> usize {
        self.rules_slots.lock().values().filter(|s| s.running).count()
    }

    /// Record what a pass did, so `jev.status` can report it.
    pub fn note_rules_pass(
        &self,
        loaded: usize,
        hash: &str,
        ms: u64,
        candidates: usize,
        calls: u64,
        lint: usize,
    ) {
        let mut stats = self.rules_stats.lock();
        stats.loaded = loaded;
        stats.hash = hash.to_string();
        stats.last_pass_ms = ms;
        stats.candidates = candidates;
        stats.calls += calls;
        stats.lint = lint;
    }

    pub fn rules_stats(&self) -> RulesStats {
        self.rules_stats.lock().clone()
    }

    // ---- which files git saw change -----------------------------------------

    /// The paths git reports as changed, absolute, or the reason that question could not be
    /// answered.
    ///
    /// `Err` means "ask git" failed: a directory that is not a repository, a repository with no
    /// commits yet, or no git at all. The caller treats that as *every* document being changed,
    /// because a rules pass that never runs in a non-repository is a feature that silently does
    /// nothing, and one that runs is merely a little more expensive than it had to be.
    ///
    /// Cached for [`CHANGED_TTL`]: one pass covers up to `rules.max_files_per_pass` documents
    /// and shelling out per document is how a cheap pass stops being cheap.
    pub fn changed_paths(&self, root: Option<&str>) -> Result<Arc<HashSet<String>>, String> {
        let Some(root) = root else {
            return Err("there is no workspace root to ask git about".to_string());
        };
        {
            let slot = self.changed.lock();
            if let Some((asked, at, value)) = slot.as_ref() {
                if asked == root && at.elapsed() < CHANGED_TTL {
                    return value.clone();
                }
            }
        }
        let value = jev_core::changed::changed_paths(root).map(Arc::new);
        if let Err(why) = &value {
            *self.changed_note.lock() = Some(why.clone());
        }
        *self.changed.lock() = Some((root.to_string(), Instant::now(), value.clone()));
        value
    }

    /// Why the last `changed_paths` could not be answered, taken once so it is said once.
    pub fn take_changed_note(&self) -> Option<String> {
        self.changed_note.lock().take()
    }

    /// Forget the cached changed-set answer.
    ///
    /// The cache exists so that one pass over eight documents shells out once, and for that it
    /// only has to survive the pass. Across passes it is a hazard: a file that was clean when
    /// the question was asked, and is not clean any more, comes back "unchanged" — and the pass
    /// that saves exist for is exactly the pass that gets skipped. A save says so, and the
    /// question is asked again.
    pub fn forget_changed(&self) {
        *self.changed.lock() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jev_core::model::{ChatRequest, ChatResponse};
    use jev_core::config::TierConfig;
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

    /// A decision backend that answers nothing: the store keys, counts and evicts by identity.
    struct NoDecision;
    impl DecisionBackend for NoDecision {
        fn decide(
            &self,
            _c: &jev_core::config::DecisionTierConfig,
            _r: &jev_core::decision::DecisionRequest,
        ) -> Result<jev_core::decision::DecisionResponse> {
            Ok(jev_core::decision::DecisionResponse {
                answers: Vec::new(),
                input_tokens: 0,
                output_tokens: 0,
            })
        }
    }

    fn state() -> Arc<AppState> {
        AppState::new(Arc::new(Null), Arc::new(NoDecision), Config::default(), &[])
    }

    /// A plan with nothing in it: the store keys, counts and evicts by identity alone.
    fn plan_with(id: &str) -> jev_core::types::Plan {
        jev_core::types::Plan {
            id: id.to_string(),
            goal: "g".into(),
            language: "rust".into(),
            steps: Vec::new(),
            usage: jev_core::types::Usage {
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
        let s = AppState::new(Arc::new(Null), Arc::new(NoDecision), Config::default(), &[]);
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
        let s = AppState::new(Arc::new(Null), Arc::new(NoDecision), Config::default(), &[]);
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

    #[test]
    fn rules_runs_coalesce_and_the_pending_count_is_bounded() {
        let s = state();
        assert_eq!(s.claim_rules("file:///a"), Claim::Run);
        // Twenty requests during the run do not become twenty passes: the queue saturates and
        // the rest coalesce into the pending ones.
        for _ in 0..20 {
            assert_eq!(s.claim_rules("file:///a"), Claim::Queued);
        }
        assert_eq!(s.rules_running(), 1);
        let mut follow_ups = 0u32;
        while s.finish_rules("file:///a") {
            follow_ups += 1;
            assert!(follow_ups <= MAX_PENDING_RULES, "the queue is bounded");
        }
        assert_eq!(follow_ups, MAX_PENDING_RULES);
        assert_eq!(s.rules_running(), 0, "and the slot is released at the end");
        // A separate document is not blocked by this one.
        assert_eq!(s.claim_rules("file:///b"), Claim::Run);
        assert_eq!(s.rules_running(), 1);
    }

    #[test]
    fn a_pass_is_recorded_for_status_and_starts_at_zero() {
        let s = state();
        let before = s.rules_stats();
        assert_eq!((before.loaded, before.last_pass_ms, before.candidates, before.calls), (0, 0, 0, 0));
        assert_eq!(before.lint, 0, "nothing is reported for rules nobody has read yet");
        s.note_rules_pass(3, "abc", 12, 5, 1, 2);
        let after = s.rules_stats();
        assert_eq!((after.loaded, after.hash.as_str(), after.candidates), (3, "abc", 5));
        assert_eq!(after.last_pass_ms, 12);
        assert_eq!(after.calls, 1);
        assert_eq!(after.lint, 2);
    }

    #[test]
    fn without_a_root_every_document_counts_as_changed() {
        // Not a failure of the pass: "I cannot tell" and "nothing changed" must not be the same
        // answer, or the ambient pass silently does nothing outside a repository.
        let s = state();
        assert!(s.changed_paths(None).is_err());
        assert!(s.take_changed_note().is_none(), "no root is not a git failure");
    }
}
