//! Content-hash keyed conclusion cache.
//!
//! Never a source of truth (PROTOCOL.md N9): entries are immutable and evictable, and a
//! miss is always recoverable by recomputation. Keys embed the content hash, so an entry
//! for changed content simply never hits — invalidation is free.

use crate::config::Config;
use crate::types::{Finding, LineRange, Proposal};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
pub struct Conclusion {
    pub findings: Vec<Finding>,
    pub edit: Option<Proposal>,
    pub artifact: Option<String>,
    /// Which pass produced the findings: `"rules"` or `"review"`.
    ///
    /// Carried rather than inferred from the key because both passes write the same document
    /// slot — a client reading a finding is entitled to know whether a convention check or a
    /// model wrote it, and `data.source` on the diagnostic is where it says so (PROTOCOL §9).
    pub source: Option<String>,
}

struct State {
    map: HashMap<String, Arc<Conclusion>>,
    order: VecDeque<String>,
    cap: usize,
    hits: u64,
    misses: u64,
}

pub struct Cache {
    inner: Mutex<State>,
}

impl Cache {
    pub fn new(cap: usize) -> Cache {
        Cache {
            inner: Mutex::new(State {
                map: HashMap::new(),
                order: VecDeque::new(),
                cap: cap.max(1),
                hits: 0,
                misses: 0,
            }),
        }
    }

    pub fn get(&self, key: &str) -> Option<Arc<Conclusion>> {
        let mut s = self.inner.lock();
        match s.map.get(key).cloned() {
            Some(v) => {
                s.hits += 1;
                Some(v)
            }
            None => {
                s.misses += 1;
                None
            }
        }
    }

    pub fn put(&self, key: &str, value: Conclusion) {
        let mut s = self.inner.lock();
        if s.map.insert(key.to_string(), Arc::new(value)).is_none() {
            s.order.push_back(key.to_string());
        }
        while s.order.len() > s.cap {
            if let Some(old) = s.order.pop_front() {
                s.map.remove(&old);
            }
        }
    }

    /// Drop every entry. Used by `:Jev recompute`.
    pub fn clear(&self) {
        let mut s = self.inner.lock();
        s.map.clear();
        s.order.clear();
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// (hits, misses, entries)
    pub fn stats(&self) -> (u64, u64, usize) {
        let s = self.inner.lock();
        (s.hits, s.misses, s.map.len())
    }
}

/// Findings are per document, per language: one analysis covers every scope in it.
///
/// The language is part of the key because it is part of the *question*: the review prompt is
/// built from the resolved language's flavour (`context::build` writes
/// `LANGUAGE: <name> (prompt flavour: <flavour>)` into it), and the language comes from the
/// client's `languageId` or the path's extension — neither of which the content hash knows.
/// Two documents with identical bytes and different names are two different reviews, and one
/// must never be answered with the other's findings.
///
/// `max_findings` is the third input for the same reason: what is stored is the *capped* set
/// (`findings::build`), so serving it under a different cap answers a different question.
pub fn findings_key(content_hash: &str, language: &str, max_findings: usize) -> String {
    format!("findings|{content_hash}|{language}|{max_findings}")
}

/// Findings from the rules pass, keyed by **every** input the conclusion is a function of.
///
/// The rules axis is why this key exists at all: editing a rule changes what it finds, so a
/// conclusion taken under the old text must not be served for the new one. `rule_hash` is the
/// hash of the *merged* set — the repository's rules and the shipped ones together
/// (`rules::load`) — so a change to either invalidates every conclusion taken under the old
/// pair, and `rules.defaults` needs no entry of its own here: turning the shipped set off
/// changes the set the hash is taken over.
///
/// The *path* is the second half of the same argument, and it is not decoration: a rule's
/// `applies_to` is matched against the path, so two files with identical bytes but different
/// extensions are asked different questions and can answer differently. Without the path in the
/// key, a `.py` file whose bytes match an already-inspected `.rs` file would be served the `.rs`
/// file's findings — a violation reported in a file the rule never claimed.
///
/// The decide tier is the third: this conclusion *is* that tier's answer, so a changed classifier
/// was asked the same words and answered them differently. It is the same failure [`op_key`]'s
/// docstring records as measured on the chat axis, and the rules pass has it in the same shape.
/// The `wire` rides along with the endpoint because it selects the *path* the request goes to (the
/// service's own route against a provider's chat route): same host, different question.
///
/// The bounds are the fourth, and they are inputs for the same reason.
/// `max_candidates_per_rule` decides how many candidates become questions, `max_state_lines`
/// and `max_state_bytes` decide how much of the file the decision is shown
/// (`inspections::select`, `inspections::request`), and `noise.max_visible_findings` is the cap
/// the stored set was *truncated* to (`findings::build`, called by `inspections::resolve`).
/// Narrow any of them and identical bytes under identical rules can answer differently.
///
/// That last one was missing while [`findings_key`] carried it with a comment saying why it must
/// be here — the same conclusion, stored under two keys, one of which forgot an input. The
/// symptom is the one this key exists to prevent: widen `noise.max_visible_findings` and the pass
/// answers from the cache with the old cap's findings, so the setting looks like it does nothing.
///
/// **The definitions are the fifth, and they are the newer half of the same defect.**
/// `inspections::state` is a window around each candidate, and the window is the enclosing
/// declaration when the client sent one (PROTOCOL §3.4.3, `inspections::window_for`). Same
/// bytes, same rules, same path, same bounds — and a client that sent declarations asks a
/// different question from one that sent none, and from one that sent different declarations
/// (the plugin re-parses on edit; a client whose parser disagrees resolves a different
/// declaration). Without [`definitions_digest`] in the key, the second session's conclusion
/// answers the first's question, and the whole window rule becomes a source of wrong findings
/// rather than better ones. `&[]` — no client, as `jev inspect` has none — is a value, not an
/// absence: it is the digest of the empty set and it keys the neighbourhood window.
///
/// `rules.enabled` and `max_files_per_pass` are deliberately absent: the first is not an input to
/// an answer that was produced (a disabled pass produces none, and re-enabling asks the same
/// question again), and the second bounds a *pass*, not a document.
pub fn rules_key(
    content_hash: &str,
    rule_hash: &str,
    path: &str,
    defs_digest: &str,
    cfg: &Config,
) -> String {
    let decide = &cfg.models.decide;
    let bounds = &cfg.rules;
    format!(
        "rules|{content_hash}|{rule_hash}|{path}|{defs_digest}|{:?}|{}|{}|{}|{}|{}|{}",
        decide.wire,
        decide.base_url,
        decide.model,
        bounds.max_candidates_per_rule,
        bounds.max_state_lines,
        bounds.max_state_bytes,
        cfg.noise.max_visible_findings
    )
}

/// What the client's definitions do to the question, for [`rules_key`].
///
/// `inspections::window_for` reads a set of line ranges and nothing else about them — the name a
/// client attaches is never read — so the digest is taken over `(start, end)` pairs in sorted
/// order, which is the canonical form of that set and not of the wire message. Two clients whose
/// declarations differ only in the order treesitter happened to emit share a key, because they
/// ask the same question; two whose declarations differ in any range do not, because they do
/// not.
pub fn definitions_digest(defs: &[LineRange]) -> String {
    if defs.is_empty() {
        return String::new();
    }
    let mut ranges: Vec<(u32, u32)> = defs.iter().map(|d| (d.start_line, d.end_line)).collect();
    ranges.sort_unstable();
    let mut h = Sha256::new();
    for (start, end) in ranges {
        h.update(start.to_le_bytes());
        h.update(end.to_le_bytes());
    }
    format!("{:x}", h.finalize())
}

/// A generated edit or artifact, keyed by **every** input the answer is a function of.
///
/// One axis per way the question can differ, because a key that omits one serves an answer to a
/// question nobody asked:
///
/// * `verb` and `prompt_version` — the prompt itself, and its revision (PROTOCOL.md §5 gate 1);
/// * `model` — without it the cache serves one model's answer for another's question, which is
///   not hypothetical: switching a tier's endpoint mid-session returned a cached empty
///   completion from the previous model, in 261 ms, and looked exactly like the new endpoint
///   failing;
/// * `language` — the prompt carries the resolved language's flavour, and the language is not a
///   function of the content (see [`findings_key`]);
/// * `content_hash`, `start_line`, `end_line` — the material, and the scope it was asked about;
/// * `context_digest` — what the editor sent. Empty when nothing was provided; when it is not,
///   a request whose project context differs is a different request.
pub fn op_key(
    verb: &str,
    prompt_version: &str,
    model: &str,
    language: &str,
    content_hash: &str,
    start_line: u32,
    end_line: u32,
    context_digest: &str,
) -> String {
    format!(
        "op|{verb}|{prompt_version}|{model}|{language}|{content_hash}|{start_line}|{end_line}|{context_digest}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::Wire;

    fn conclusion(tag: &str) -> Conclusion {
        Conclusion {
            artifact: Some(tag.to_string()),
            ..Default::default()
        }
    }

    fn op(verb: &str, prompt_version: &str, language: &str) -> String {
        op_key(verb, prompt_version, "m", language, "h", 1, 2, "")
    }

    /// Two versions of one shipped rule: everything a "the shipped set changed" test needs, and
    /// nothing else. The pattern differs, so the rule's text does.
    const SHIPPED_A: &str = r#"{"schema":"jev.rules/1","rules":[{"id":"shipped-one",
        "title":"A shipped rule","text":"Do not leave a TODO.","applies_to":["**/*.rs"],
        "inspection":{"kind":"regex","pattern":"TODO"},
        "judgement":{"question":"Is this a violation?"}}]}"#;
    const SHIPPED_B: &str = r#"{"schema":"jev.rules/1","rules":[{"id":"shipped-one",
        "title":"A shipped rule","text":"Do not leave a FIXME.","applies_to":["**/*.rs"],
        "inspection":{"kind":"regex","pattern":"FIXME"},
        "judgement":{"question":"Is this a violation?"}}]}"#;

    #[test]
    fn keys_separate_the_axes_that_matter() {
        assert_ne!(findings_key("h1", "rust", 5), findings_key("h2", "rust", 5));
        assert_ne!(op("harden", "1", "rust"), op_key("harden", "1", "m", "rust", "h", 1, 3, ""));
        assert_ne!(op("harden", "1", "rust"), op("rewrite", "1", "rust"));
        assert_ne!(
            op("harden", "1", "rust"),
            op("harden", "2", "rust"),
            "a prompt revision must not hit an older conclusion"
        );
        assert_eq!(op("harden", "1", "rust"), op("harden", "1", "rust"));
    }

    #[test]
    fn the_model_is_part_of_the_key() {
        let a = op_key("completion", "v1", "model-a", "rust", "hash", 1, 2, "");
        let b = op_key("completion", "v1", "model-b", "rust", "hash", 1, 2, "");
        assert_ne!(
            a, b,
            "two models asked the same question must not share an answer"
        );
    }

    #[test]
    fn the_language_is_part_of_both_keys() {
        // The prompt carries the resolved language's flavour, and the language is not a function
        // of the content: a `.rs` and a `.py` file holding the same bytes are two different
        // questions, and the second must not be answered with the first's conclusion.
        assert_ne!(
            findings_key("same-bytes", "rust", 5),
            findings_key("same-bytes", "python", 5),
            "the review prompt depends on the language"
        );
        assert_eq!(findings_key("same-bytes", "rust", 5), findings_key("same-bytes", "rust", 5),
                   "and the same document analysed twice must still hit");
        assert_ne!(
            findings_key("same-bytes", "rust", 5),
            findings_key("same-bytes", "rust", 20),
            "the stored set is capped, so the cap is part of the question"
        );
        assert_ne!(
            op("harden", "1", "rust"),
            op("harden", "1", "python"),
            "so does an edit's"
        );
    }

    #[test]
    fn the_rules_key_separates_content_rules_and_path() {
        let cfg = Config::default();
        let base = rules_key("h", "r", "/w/a.rs", "", &cfg);
        assert_eq!(base, rules_key("h", "r", "/w/a.rs", "", &cfg));
        assert_ne!(base, rules_key("h2", "r", "/w/a.rs", "", &cfg), "content");
        assert_ne!(
            base,
            rules_key("h", "r2", "/w/a.rs", "", &cfg),
            "a rule edit must not serve the old conclusion"
        );
        // Two files with the same bytes but different names are asked different questions: a
        // rule's `applies_to` is matched against the path.
        assert_ne!(
            base,
            rules_key("h", "r", "/w/a.py", "", &cfg),
            "`applies_to` makes the answer depend on the path"
        );
    }

    #[test]
    fn the_rules_key_names_the_classifier_and_the_bounds() {
        let cfg = Config::default();
        let key = |cfg: &Config| rules_key("h", "r", "/w/a.rs", "", cfg);
        assert_eq!(key(&cfg), key(&cfg), "the same inputs are the same key");

        // The tier that answered. A different model, endpoint or wire asked the same words of a
        // different classifier, and this conclusion *is* its answer.
        let mut model = cfg.clone();
        model.models.decide.model = "another-model".into();
        assert_ne!(key(&cfg), key(&model), "another model answered the question");

        let mut wire = cfg.clone();
        wire.models.decide.wire = Wire::OpenRouter;
        assert_ne!(key(&cfg), key(&wire), "the wire selects the path");

        let mut endpoint = cfg.clone();
        endpoint.models.decide.base_url = "https://elsewhere.example/v1".into();
        assert_ne!(key(&cfg), key(&endpoint), "another endpoint is another classifier");

        // The bounds the questions were built under.
        let mut candidates = cfg.clone();
        candidates.rules.max_candidates_per_rule -= 1;
        assert_ne!(
            key(&cfg),
            key(&candidates),
            "the candidate cap decides how many questions there were"
        );
        let mut lines = cfg.clone();
        lines.rules.max_state_lines -= 1;
        assert_ne!(key(&cfg), key(&lines), "how much of the file the decision read");
        let mut bytes = cfg.clone();
        bytes.rules.max_state_bytes -= 1;
        assert_ne!(key(&cfg), key(&bytes), "and its byte budget");
        // The cap the stored set was truncated to. This one was missing, so widening the
        // setting served the old cap's findings out of the cache and the setting looked inert.
        let mut cap = cfg.clone();
        cap.noise.max_visible_findings += 1;
        assert_ne!(
            key(&cfg),
            key(&cap),
            "the stored findings are capped, so the cap is part of the question"
        );

        // Two that are not inputs to this conclusion, and must not cost a hit: the switch (a
        // disabled pass produces no conclusion to serve) and the file budget (a property of a
        // pass, not of a document).
        let mut switched = cfg.clone();
        switched.rules.enabled = false;
        switched.rules.max_files_per_pass += 1;
        assert_eq!(
            key(&cfg),
            key(&switched),
            "neither changes what the answer to this question is"
        );
    }

    #[test]
    fn changing_the_shipped_set_changes_the_key() {
        // The shipped set is an input to every rules conclusion, and it enters through the rule
        // hash the key carries: a conclusion taken under one shipped set must never answer for
        // the next. The whole chain is exercised — shipped content → `load` → merged hash →
        // key → cache — because the failure this guards against is exactly a link in it that
        // forgets the set is there.
        let root = std::env::temp_dir().join(format!("jev-cache-shipped-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let cfg = Config::default();
        let path = "/w/a.rs";

        let one = crate::rules::load(&root, true, &[("code/a.json", SHIPPED_A)]);
        let same = crate::rules::load(&root, true, &[("code/a.json", SHIPPED_A)]);
        let changed = crate::rules::load(&root, true, &[("code/a.json", SHIPPED_B)]);
        assert_eq!(one.hash, same.hash, "the same shipped set is the same rules");
        assert_ne!(one.hash, changed.hash, "so is not");

        let cache = Cache::new(4);
        cache.put(
            &rules_key("h", &one.hash, path, "", &cfg),
            conclusion("from the shipped set"),
        );
        assert!(
            cache.get(&rules_key("h", &same.hash, path, "", &cfg)).is_some(),
            "the same document under the same shipped set still hits"
        );
        assert!(
            cache.get(&rules_key("h", &changed.hash, path, "", &cfg)).is_none(),
            "a conclusion taken before the shipped rule changed must not answer for it"
        );

        // And with the shipped set off it is a third key again: what is hashed is the merged
        // set, so `rules.defaults` is an input without needing an entry of its own.
        let off = crate::rules::load(&root, false, &[("code/a.json", SHIPPED_A)]);
        assert!(
            cache.get(&rules_key("h", &off.hash, path, "", &cfg)).is_none(),
            "the shipped rules the hash is taken over are absent, so this is another set"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_conclusion_is_not_served_for_another_classifier_or_another_view() {
        // The key is the whole mechanism: two runs that differ only in the tier, or only in what
        // the decision was shown, must not share a conclusion. This is `op_key`'s measured
        // failure on the chat axis, asserted on the rules axis.
        let cache = Cache::new(8);
        let path = "/w/a.rs";
        let mut cfg = Config::default();
        cache.put(&rules_key("h", "r", path, "", &cfg), conclusion("from the first tier"));
        assert!(
            cache.get(&rules_key("h", "r", path, "", &cfg)).is_some(),
            "the same question hits"
        );

        cfg.models.decide.model = "another-model".into();
        assert!(
            cache.get(&rules_key("h", "r", path, "", &cfg)).is_none(),
            "the stored answer came from a different model"
        );

        cfg.models.decide.model = Config::default().models.decide.model;
        cfg.rules.max_state_lines += 1;
        assert!(
            cache.get(&rules_key("h", "r", path, "", &cfg)).is_none(),
            "and it was given a different view of the file"
        );
    }

    #[test]
    fn the_rules_key_names_the_definitions_the_window_was_built_from() {
        let cfg = Config::default();
        let a = LineRange {
            start_line: 1,
            end_line: 2,
        };
        let b = LineRange {
            start_line: 9,
            end_line: 10,
        };
        let none = definitions_digest(&[]);
        let one = definitions_digest(&[a]);
        let moved = definitions_digest(&[LineRange {
            start_line: 1,
            end_line: 3,
        }]);

        assert_eq!(none, "", "no client is a value, not a missing input");
        assert_ne!(none, one, "a declaration the client sent changes the window");
        assert_ne!(
            one, moved,
            "a declaration that ends one line later is another window"
        );
        // `inspections::window_for` reads a set — the smallest enclosing range — so the order
        // treesitter happened to emit them in is not part of the question.
        assert_eq!(
            definitions_digest(&[a, b]),
            definitions_digest(&[b, a]),
            "the same set in another order is the same question"
        );

        // And through the key, which is the whole point: same bytes, same rules, same path,
        // same bounds, different definitions — a different question, and the second must not be
        // answered from the first's conclusion.
        let base = rules_key("h", "r", "/w/a.rs", &none, &cfg);
        assert_ne!(base, rules_key("h", "r", "/w/a.rs", &one, &cfg));
        assert_ne!(base, rules_key("h", "r", "/w/a.rs", &moved, &cfg));

        let cache = Cache::new(4);
        cache.put(&base, conclusion("no client, the neighbourhood window"));
        assert!(
            cache
                .get(&rules_key("h", "r", "/w/a.rs", &one, &cfg))
                .is_none(),
            "a session whose client sent a declaration was asked about a different state"
        );
        assert!(cache
            .get(&rules_key("h", "r", "/w/a.rs", &definitions_digest(&[]), &cfg))
            .is_some());
    }

    #[test]
    fn stored_values_are_returned_and_misses_counted() {
        let c = Cache::new(4);
        assert!(c.get("k").is_none());
        c.put("k", conclusion("v"));
        assert_eq!(c.get("k").unwrap().artifact.as_deref(), Some("v"));
        let (hits, misses, len) = c.stats();
        assert_eq!((hits, misses, len), (1, 1, 1));
    }

    #[test]
    fn eviction_is_bounded_and_oldest_first() {
        let c = Cache::new(2);
        c.put("a", conclusion("a"));
        c.put("b", conclusion("b"));
        c.put("c", conclusion("c"));
        assert!(c.get("a").is_none(), "oldest evicted");
        assert!(c.get("b").is_some());
        assert!(c.get("c").is_some());
        assert!(c.len() <= 2);
    }

    #[test]
    fn rewriting_a_key_does_not_duplicate_it() {
        let c = Cache::new(4);
        c.put("k", conclusion("one"));
        c.put("k", conclusion("two"));
        assert_eq!(c.len(), 1);
        assert_eq!(c.get("k").unwrap().artifact.as_deref(), Some("two"));
    }

    #[test]
    fn clear_empties_the_cache() {
        let c = Cache::new(4);
        c.put("k", conclusion("v"));
        c.clear();
        assert!(c.is_empty());
        assert!(c.get("k").is_none());
    }

    #[test]
    fn a_zero_capacity_cache_still_works() {
        let c = Cache::new(0);
        c.put("k", conclusion("v"));
        assert!(c.get("k").is_none() || c.len() == 1);
    }
}
