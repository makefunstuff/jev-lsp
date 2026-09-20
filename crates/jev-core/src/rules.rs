//! The `jev.rules/1` document: the conventions a repository states, and how each is checked.
//!
//! A rule is a plain-English convention plus an *inspection* that finds the places it might be
//! about. The inspection is deliberately dumb and local — a regex, or the absence of one — and
//! it does not decide anything: it only names candidates. Whether a candidate is a violation is
//! the judgement question, answered by the decision tier (see `crate::inspections` and
//! `Engine::inspect`).
//!
//! That split is the whole point. A regex can find every `.unwrap()` in a file and cannot tell a
//! test helper from a request handler; a model can tell them apart and cannot be trusted to scan
//! a file. Each does the half it is good at.
//!
//! **A real regex engine, not a hand-rolled matcher.** `regex` is a dependency here rather than
//! twenty lines of character matching because `lint` has to be able to say "this rule's pattern
//! does not compile" — a repository's rules are written by hand and a broken one must be
//! reported, not silently matched-never-with — and because a hand-rolled engine that is
//! *nearly* right finds *nearly* the right lines, which is a bug farm nobody can debug. Each
//! rule's pattern is compiled once per pass, in `inspections::candidates`, never per line.
//!
//! **Two sources, one set.** A rule comes either from `.jev/rules/*.json` — the repository's
//! own, `RuleSource::Repository` — or from the set shipped inside the binary
//! (`default_rules/<group>/*.json`, embedded by `build.rs`, `RuleSource::Builtin`). [`load`]
//! merges them with the repository winning: a file the user wrote always shadows a shipped rule
//! that claims the same `id`. Which source a rule came from travels with it onto every finding
//! (`crate::types::RuleSource`), because a finding you cannot trace to a file you can open is
//! one you cannot calibrate or turn off.
//!
//! The shipped set is *not* a fallback in the sense §12 refuses: nothing generative runs, the
//! rules the pass runs are always data, and a repository that turns the shipped ones off with
//! `rules.defaults = false` is back to the behaviour a repository with no rule files had before
//! — `no_rules`, said out loud. What changed is that "no rule files of your own" and "nothing to
//! inspect with" are no longer the same statement.

use crate::types::{RuleSource, Verb};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The schema string every rule file must carry.
pub const SCHEMA: &str = "jev.rules/1";

/// Where rules live, relative to the workspace root.
pub const DIR: &str = ".jev/rules";

/// Where the shipped set lives in the source tree, relative to `crates/jev-core`.
///
/// Not read at run time — `build.rs` embeds it — but it is the name a skipped shipped file is
/// reported under, so a reader can find the file this build baked in.
pub const DEFAULT_DIR: &str = "default_rules";

/// The shipped rule files, embedded at build time: `(path under `default_rules/`, contents)`.
///
/// Empty until a rule file is written into `default_rules/`, and empty is a working build —
/// the whole set is authored as ordinary rule files, and a build that needed all of them at
/// once would make every rule a build-breaking dependency of every other.
///
/// The list is generated (`build.rs`) rather than written here, so adding a shipped rule is
/// adding a file. Nothing has to be registered, and neither authoring session has to touch a
/// shared index the other is editing.
pub fn builtin_files() -> &'static [(&'static str, &'static str)] {
    // `include!` of the generated slice of `("name", include_str!(…))` pairs.
    include!(concat!(env!("OUT_DIR"), "/default_rules.rs"))
}

/// A rule's title becomes a finding's label, which is clipped at 60 characters
/// (`findings::MAX_LABEL`) — a longer title is silently cut in half on screen, so it is an
/// error here instead.
pub const MAX_TITLE: usize = 60;

/// How likely a rule's judgement has to be before a candidate becomes a finding, when the rule
/// does not say. A coin flip is not a finding.
pub const DEFAULT_MIN_PROBABILITY: f64 = 0.5;

/// How the candidates for a rule are found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Inspection {
    /// Every line the pattern matches.
    Regex {
        pattern: String,
        /// Only report when the file holds *more* than this many matches; `Some(0)` means
        /// "any match at all". `None` is the same as `Some(0)`.
        #[serde(default)]
        max_matches: Option<usize>,
    },
    /// The pattern the file is expected to contain but does not (a licence header, a module
    /// declaration). One candidate, at the head of the file.
    Absent {
        pattern: String,
        #[serde(default)]
        max_matches: Option<usize>,
    },
}

impl Inspection {
    pub fn pattern(&self) -> &str {
        match self {
            Inspection::Regex { pattern, .. } | Inspection::Absent { pattern, .. } => pattern,
        }
    }

    pub fn max_matches(&self) -> Option<usize> {
        match self {
            Inspection::Regex { max_matches, .. } | Inspection::Absent { max_matches, .. } => {
                *max_matches
            }
        }
    }
}

/// The question the decision tier answers about each candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Judgement {
    pub question: String,
    /// Passed through to the wire unchanged: an object for `choice`, an ordered array for
    /// `score`, `{"true": …, "false": …}` for `noul`.
    #[serde(default)]
    pub criteria: Option<serde_json::Value>,
    /// The labels an answer may pick to say *why*. Advisory.
    #[serde(default)]
    pub reasons: Option<serde_json::Value>,
    /// A candidate is only reported when the decision's probability clears this. Default 0.5:
    /// a coin flip is not a finding.
    #[serde(default)]
    pub min_probability: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    /// Becomes `Finding.label`. Deterministic text the repository wrote, never model output.
    pub title: String,
    /// The rule, in prose. Becomes part of `Finding.detail`.
    pub text: String,
    /// `information` or `warning`; `error` is reserved (PROTOCOL §9) and maps to `warning`.
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub applies_to: Vec<String>,
    pub inspection: Inspection,
    pub judgement: Judgement,
    #[serde(default)]
    pub verb_hint: Option<String>,
    #[serde(default)]
    pub docs: Option<String>,
    /// Where this rule was read from. Never part of the file format and never part of the
    /// rules' hash: a rule's *text* is what a conclusion was taken against, and the two
    /// sources cannot both contribute the same `id` (see `merge`).
    #[serde(skip)]
    pub source: RuleSource,
}

/// What one rule file holds.
#[derive(Debug, Deserialize)]
struct RuleFile {
    #[serde(default)]
    schema: Option<String>,
    #[serde(default)]
    rules: Vec<Rule>,
}

/// The rules that loaded, the files that did not, and a hash of the result.
#[derive(Debug, Clone, Default)]
pub struct RuleSet {
    pub rules: Vec<Rule>,
    /// `(path, reason)` for every file that was skipped. Never fatal.
    pub skipped: Vec<(String, String)>,
    /// sha256 over the loaded rules, so editing a rule invalidates every cached conclusion
    /// taken under the old text.
    pub hash: String,
    /// How many shipped rule files this load was offered, whether or not it merged them.
    ///
    /// One number, because a pass with nothing to run owes two different sentences: the shipped
    /// set does not exist in this build, or it exists and this repository switched it off
    /// (`rules.defaults`). "No rules" and "no rules *allowed*" are not the same answer.
    pub shipped: usize,
}

impl RuleSet {
    /// `(the repository's own rules, the shipped ones)` in this set.
    ///
    /// The two counts, not the total, are what a pass has to be able to report: "no rule
    /// applies to this file" means something different depending on whether the file was
    /// looked at with rules the reader can open.
    pub fn counts(&self) -> (usize, usize) {
        let builtin = self
            .rules
            .iter()
            .filter(|r| r.source == RuleSource::Builtin)
            .count();
        (self.rules.len() - builtin, builtin)
    }
}

/// Read the rules for `root`: `.jev/rules/*.json`, plus the shipped set when `defaults`.
///
/// A file that cannot be read, cannot be parsed, or does not carry `schema: "jev.rules/1"` is
/// skipped with a stated reason and the rest still load — the same treatment `Config` gives a
/// malformed settings payload, and for the same reason: a bad file in someone's repository must
/// not take the server down, and must not be silent either.
///
/// `builtin` is the shipped set ([`builtin_files`] in production, a fixture in a test). It is a
/// parameter rather than a global read of `builtin_files()` so that the pass a language server
/// runs and the pass these tests run are the same pass over a substitutable input.
pub fn load(root: &Path, defaults: bool, builtin: &[(&str, &str)]) -> RuleSet {
    let mut set = load_dir(&root.join(DIR), RuleSource::Repository);
    set.shipped = builtin.len();
    if defaults {
        let shipped = load_named(builtin, RuleSource::Builtin);
        set.skipped.extend(shipped.skipped);
        set.rules = merge(shipped.rules, set.rules);
    }
    set.hash = hash_of(&set.rules);
    set
}

/// The repository's own rules shadow the shipped ones that claim the same `id`.
///
/// A shipped rule is a default, not an override: when the repository has written a file for the
/// same `id`, that file is what runs, and the shipped rule does not also run beside it — two
/// rules with one id asking two questions would report the same line twice under one label, and
/// the reader could not tell which of them they had calibrated. The shadowed shipped rule is
/// dropped, not merged, and not reported as a skip: nothing was skipped, it was superseded.
///
/// Within *one* source, duplicate ids are kept, both run, and `lint` reports the duplicate —
/// the loader's behaviour before there were two sources, unchanged. Precedence is a rule about
/// sources, never a rule about which of two files in the same directory wins.
fn merge(shipped: Vec<Rule>, repo: Vec<Rule>) -> Vec<Rule> {
    let claimed: Vec<&str> = repo.iter().map(|r| r.id.as_str()).collect();
    let mut rules: Vec<Rule> = shipped
        .into_iter()
        .filter(|r| !claimed.contains(&r.id.as_str()))
        .collect();
    // The repository's own rules last: in a state or a lint list they read as the additions
    // they are, and the shipped set is the base they were written against.
    rules.extend(repo);
    rules
}

/// Read one directory of `<name>.json` files, in path order.
fn load_dir(dir: &Path, source: RuleSource) -> RuleSet {
    let mut paths: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect(),
        // No rules directory is the normal case in a repository that has written none yet.
        Err(_) => return RuleSet::default(),
    };
    paths.sort();

    let mut set = RuleSet::default();
    for path in paths {
        let shown = path.display().to_string();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                set.skipped.push((shown, format!("cannot be read: {e}")));
                continue;
            }
        };
        match parse(&text, source) {
            Ok(rules) => set.rules.extend(rules),
            Err(reason) => set.skipped.push((shown, reason)),
        }
    }
    set
}

/// Read embedded files that are already in memory, named as they are in the source tree.
fn load_named(files: &[(&str, &str)], source: RuleSource) -> RuleSet {
    let mut set = RuleSet::default();
    for (name, text) in files {
        // The path a reader can open in this repository, not the build's temporary copy.
        let shown = format!("{DEFAULT_DIR}/{name}");
        match parse(text, source) {
            Ok(rules) => set.rules.extend(rules),
            Err(reason) => set.skipped.push((shown, reason)),
        }
    }
    set
}

/// One rules document's rules, or the reason it was skipped.
fn parse(text: &str, source: RuleSource) -> Result<Vec<Rule>, String> {
    let file: RuleFile =
        serde_json::from_str(text).map_err(|e| format!("is not a rules document: {e}"))?;
    match file.schema.as_deref() {
        Some(SCHEMA) => Ok(file
            .rules
            .into_iter()
            .map(|mut rule| {
                rule.source = source;
                rule
            })
            .collect()),
        Some(other) => Err(format!("schema is {other:?}, not {SCHEMA:?}")),
        None => Err(format!("carries no `schema`; expected {SCHEMA:?}")),
    }
}

/// A stable digest of the rules, taken over their JSON.
///
/// The field order is the struct's, so the same rules always hash the same; a changed rule — or
/// a reordered one — hashes differently, which is what invalidates a cache entry.
///
/// The hash is taken over the *merged* set, so the shipped rules are in it: a conclusion taken
/// with one version of the shipped set must never be served under the next (`cache::rules_key`).
pub fn hash_of(rules: &[Rule]) -> String {
    let bytes = serde_json::to_vec(rules).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(&bytes);
    let d = h.finalize();
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// What `jev rules init` did, file by file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitReport {
    /// The directory the set was materialised into, as it was named.
    pub dir: String,
    /// Files written: newly created, or replaced because `--force` said so.
    pub written: Vec<String>,
    /// Files that already held exactly the shipped rule, and were left untouched.
    pub unchanged: Vec<String>,
    /// Files that already existed with different content and were **not** touched. A report
    /// with any of these is a refusal: the user's file won, and the exit code says so.
    pub refused: Vec<String>,
}

impl InitReport {
    /// True when something the user wrote was left alone rather than overwritten.
    pub fn is_refusal(&self) -> bool {
        !self.refused.is_empty()
    }
}

/// Write the shipped set into `dir` (PROTOCOL.md §11, `jev rules init`).
///
/// A rule the user cannot read is a rule they cannot calibrate, so the shipped set is
/// materialisable: one file per shipped file, in the format `.jev/rules/*.json` already uses, so
/// that editing one is editing a rule file and the repository's own copy then shadows the
/// shipped rule it came from (`merge`).
///
/// **Idempotent and non-clobbering.** A file that already exists is read, and left alone unless
/// `--force`: identical means nothing to do, different means the user's file wins and the report
/// names it. A second run over a directory this function filled writes nothing and changes no
/// bytes. Nothing outside `dir` is written, and nothing is ever deleted — including on a
/// refusal, which is reported rather than forced.
///
/// `files` is the shipped set (a fixture in a test); the grouping in the source tree
/// (`default_rules/code/…`, `default_rules/prose/…`) is a filing convention, not a directory the
/// loader reads, so each file is written under its base name — the flat shape `DIR` is.
pub fn materialise(
    files: &[(&str, &str)],
    dir: &Path,
    force: bool,
) -> Result<InitReport, String> {
    if dir.exists() && !dir.is_dir() {
        return Err(format!(
            "{} is not a directory, so no rule can be written into it",
            dir.display()
        ));
    }
    let plan = plan(files)?;

    std::fs::create_dir_all(dir)
        .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let mut report = InitReport {
        dir: dir.display().to_string(),
        ..Default::default()
    };
    for (name, text) in plan {
        let path = dir.join(&name);
        let existing = std::fs::read(&path).ok();
        match existing {
            Some(bytes) if bytes == text.as_bytes() && !force => report.unchanged.push(name),
            Some(_) if !force => report.refused.push(name),
            _ => {
                std::fs::write(&path, text)
                    .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
                report.written.push(name);
            }
        }
    }
    Ok(report)
}

/// The shipped set as `(name written, contents)`, sorted by name and checked for collisions.
///
/// Checked *before* anything is created, because a set that cannot be written whole must not be
/// written at all: half the shipped rules in a directory, with no sign of which half, is worse
/// than a refusal that says so.
fn plan<'a>(files: &'a [(&'a str, &'a str)]) -> Result<Vec<(String, &'a str)>, String> {
    let mut plan: Vec<(String, &str)> = Vec::new();
    for (name, text) in files {
        let Some(base) = Path::new(name).file_name().and_then(|n| n.to_str()) else {
            return Err(format!("the shipped file {name:?} has no file name to write"));
        };
        plan.push((base.to_string(), text));
    }
    plan.sort_by(|a, b| a.0.cmp(&b.0));
    // Two shipped files flattening to one name would silently become one file, with one rule
    // set quietly missing from the directory it was materialised into.
    if let Some(pair) = plan.windows(2).find(|w| w[0].0 == w[1].0) {
        return Err(format!(
            "two shipped rule files are named {:?}; one would be written over the other",
            pair[0].0
        ));
    }
    Ok(plan)
}

/// Everything wrong with a rule set, in words a person can act on.
///
/// Reported, never enforced: a rule that fails lint still runs, because refusing to inspect
/// because a title is two characters too long would be worse than the long title.
pub fn lint(set: &RuleSet) -> Vec<String> {
    let mut messages = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for rule in &set.rules {
        if seen.contains(&rule.id.as_str()) {
            messages.push(format!("duplicate rule id {:?}", rule.id));
        } else {
            seen.push(&rule.id);
        }
        if rule.title.trim().is_empty() {
            messages.push(format!("rule {:?} has an empty title", rule.id));
        } else if rule.title.chars().count() > MAX_TITLE {
            messages.push(format!(
                "rule {:?} has a {} character title; findings clip labels at {MAX_TITLE}",
                rule.id,
                rule.title.chars().count()
            ));
        }
        if rule.applies_to.is_empty() {
            messages.push(format!(
                "rule {:?} has an empty `applies_to`; it would never run",
                rule.id
            ));
        }
        if let Some(hint) = &rule.verb_hint {
            if Verb::parse(hint).is_none() {
                messages.push(format!(
                    "rule {:?} names the verb hint {hint:?}, which is not a verb",
                    rule.id
                ));
            }
        }
        if let Err(e) = regex::Regex::new(rule.inspection.pattern()) {
            messages.push(format!(
                "rule {:?} has an uncompilable regex {:?}: {e}",
                rule.id,
                rule.inspection.pattern()
            ));
        }
        if let Some(p) = rule.judgement.min_probability {
            if !(0.0..=1.0).contains(&p) {
                messages.push(format!(
                    "rule {:?} has min_probability {p}, which is outside 0.0..=1.0",
                    rule.id
                ));
            }
        }
        if matches!(rule.inspection, Inspection::Absent { .. }) && rule.inspection.max_matches().is_some()
        {
            messages.push(format!(
                "rule {:?} sets `max_matches` on an `absent` inspection, where it means nothing",
                rule.id
            ));
        }
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("jev-rules-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join(DIR)).unwrap();
        path
    }

    fn write(root: &Path, name: &str, body: &str) {
        std::fs::write(root.join(DIR).join(name), body).unwrap();
    }

    /// A one-rule document, named, for a shipped-set fixture.
    fn shipped(id: &str, pattern: &str) -> String {
        format!(
            r#"{{
                "schema": "jev.rules/1",
                "rules": [{{
                    "id": "{id}",
                    "title": "Shipped: {id}",
                    "text": "A convention this build ships.",
                    "severity": "warning",
                    "applies_to": ["**/*.rs"],
                    "inspection": {{"kind": "regex", "pattern": "{pattern}"}},
                    "judgement": {{"question": "Is it a violation?", "min_probability": 0.75}}
                }}]
            }}"#
        )
    }

    const GOOD: &str = r#"{
        "schema": "jev.rules/1",
        "rules": [{
            "id": "no-unwrap-in-handlers",
            "title": "Unwrap in a request handler",
            "text": "A handler must not unwrap; return the error instead.",
            "severity": "warning",
            "applies_to": ["**/*.rs"],
            "inspection": {"kind": "regex", "pattern": "\\.unwrap\\(\\)", "max_matches": 0},
            "judgement": {"question": "Is this unwrap reachable from a request handler?",
                          "criteria": {"true": "a request can reach it", "false": "test code"},
                          "reasons": {"reachable": "a request can reach it"},
                          "min_probability": 0.75},
            "verb_hint": "fix",
            "docs": "why this rule exists"
        }]
    }"#;

    fn one_rule() -> Rule {
        serde_json::from_str::<RuleFile>(GOOD).unwrap().rules.remove(0)
    }

    #[test]
    fn a_malformed_file_is_skipped_with_a_reason_and_the_rest_still_load() {
        let root = dir("skip");
        write(&root, "a-broken.json", "{not json");
        write(&root, "b-wrong-schema.json", r#"{"schema": "jev.rules/9", "rules": []}"#);
        write(&root, "c-no-schema.json", r#"{"rules": []}"#);
        write(&root, "d-good.json", GOOD);

        let set = load(&root, false, &[]);
        assert_eq!(set.rules.len(), 1, "the good file loaded");
        assert_eq!(set.rules[0].id, "no-unwrap-in-handlers");
        assert_eq!(set.skipped.len(), 3, "{:?}", set.skipped);
        assert!(set.skipped[0].0.ends_with("a-broken.json"));
        assert!(set.skipped[0].1.contains("not a rules document"), "{:?}", set.skipped[0]);
        assert!(set.skipped[1].1.contains("jev.rules/9"), "{:?}", set.skipped[1]);
        assert!(set.skipped[2].1.contains("schema"), "{:?}", set.skipped[2]);
        assert!(!set.hash.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn no_rules_directory_is_an_empty_set_not_an_error() {
        let root = dir("missing");
        let set = load(&root, false, &[]);
        assert!(set.rules.is_empty());
        assert!(set.skipped.is_empty());
        assert_eq!(set.hash, hash_of(&[]));
        // And with nothing shipped either, the defaults setting changes nothing at all: this is
        // the shape a repository without rule files has always had.
        let with_defaults = load(&root, true, &[]);
        assert!(with_defaults.rules.is_empty());
        assert_eq!(with_defaults.hash, hash_of(&[]));
        assert_eq!(with_defaults.counts(), (0, 0));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn files_load_in_path_order() {
        let root = dir("order");
        write(&root, "b.json", &GOOD.replace("no-unwrap-in-handlers", "second"));
        write(&root, "a.json", &GOOD.replace("no-unwrap-in-handlers", "first"));
        let set = load(&root, false, &[]);
        assert_eq!(
            set.rules.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_shipped_set_loads_beside_the_repositorys_own_rules() {
        // A repository that has written no rules of its own still has a set to run: this is the
        // merge, and the source each rule is stamped with.
        let root = dir("shipped");
        let shipped_a = shipped("shipped-only", r"TODO");
        let shipped_b = shipped("shared-id", r"open\\(");
        let files: Vec<(&str, &str)> = vec![
            ("code/shipped-only.json", shipped_a.as_str()),
            ("prose/shared-id.json", shipped_b.as_str()),
        ];

        let set = load(&root, true, &files);
        assert!(set.skipped.is_empty(), "{:?}", set.skipped);
        assert_eq!(set.counts(), (0, 2), "nothing of the repository's own");
        assert!(set.rules.iter().all(|r| r.source == RuleSource::Builtin));

        // The repository's file wins the id it shares, and the shipped rule beside it does not
        // also run: one id, one rule, the one a reader can open.
        write(&root, "mine.json", &GOOD.replace("no-unwrap-in-handlers", "shared-id"));
        let set = load(&root, true, &files);
        assert_eq!(set.counts(), (1, 1));
        assert_eq!(
            set.rules
                .iter()
                .map(|r| (r.id.as_str(), r.source))
                .collect::<Vec<_>>(),
            vec![
                ("shipped-only", RuleSource::Builtin),
                ("shared-id", RuleSource::Repository),
            ]
        );
        let shared = set.rules.iter().find(|r| r.id == "shared-id").unwrap();
        assert_eq!(
            shared.title, "Unwrap in a request handler",
            "the repository's text is what runs, not the shipped rule's"
        );

        // With the shipped set switched off, the same tree holds the repository's one rule and
        // nothing else — and the hash follows, because the merged set is what is hashed.
        let off = load(&root, false, &files);
        assert_eq!(off.counts(), (1, 0));
        assert_ne!(off.hash, set.hash, "the shipped rules are in the hash");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn duplicate_ids_within_one_source_are_both_kept_and_linted() {
        // Precedence is a rule about *sources*. Two files in one directory claiming one id is
        // still the loader's old behaviour: both run and `lint` says so.
        let root = dir("dupes");
        write(&root, "a.json", &GOOD.replace("no-unwrap-in-handlers", "twice"));
        write(&root, "b.json", &GOOD.replace("no-unwrap-in-handlers", "twice"));
        let set = load(&root, false, &[]);
        assert_eq!(set.rules.len(), 2);
        assert!(lint(&set).join("\n").contains("duplicate rule id"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_malformed_shipped_file_is_reported_under_the_name_a_reader_can_open() {
        let root = dir("shipped-broken");
        let files: Vec<(&str, &str)> = vec![
            ("code/broken.json", "{not json"),
            ("code/good.json", "{\"schema\": \"jev.rules/1\", \"rules\": []}"),
        ];
        let set = load(&root, true, &files);
        assert_eq!(set.rules.len(), 0, "the good file held no rules, and that is not an error");
        assert_eq!(set.skipped.len(), 1, "{:?}", set.skipped);
        assert_eq!(set.skipped[0].0, "default_rules/code/broken.json");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn materialise_writes_the_shipped_set_once_and_never_clobbers() {
        let root = std::env::temp_dir().join(format!("jev-rules-init-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let target = root.join(DIR);
        let a = shipped("shipped-only", r"TODO");
        let b = shipped("another", r"open\\(");
        let files: Vec<(&str, &str)> =
            vec![("code/a.json", a.as_str()), ("prose/z.json", b.as_str())];

        let first = materialise(&files, &target, false).unwrap();
        assert_eq!(first.written, vec!["a.json".to_string(), "z.json".to_string()]);
        assert!(!first.is_refusal());
        assert_eq!(std::fs::read(target.join("a.json")).unwrap(), a.as_bytes());
        // Nothing outside the target: the shipped tree's group directories are not recreated.
        let beside: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(beside, vec![".jev".to_string()]);

        // A user edits one of them, the way the command exists for.
        std::fs::write(target.join("a.json"), "{ the user's own rule }").unwrap();
        let second = materialise(&files, &target, false).unwrap();
        assert!(second.written.is_empty());
        assert_eq!(second.unchanged, vec!["z.json".to_string()]);
        assert_eq!(second.refused, vec!["a.json".to_string()]);
        assert!(second.is_refusal());
        assert_eq!(
            std::fs::read(target.join("a.json")).unwrap(),
            b"{ the user's own rule }",
            "a refusal is a refusal: the user's bytes are untouched"
        );

        // `--force` is the only thing that replaces them, and a run after it changes nothing
        // again.
        let forced = materialise(&files, &target, true).unwrap();
        assert_eq!(forced.written, vec!["a.json".to_string(), "z.json".to_string()]);
        assert!(!forced.is_refusal());
        assert_eq!(std::fs::read(target.join("a.json")).unwrap(), a.as_bytes());
        let again = materialise(&files, &target, false).unwrap();
        assert!(again.written.is_empty() && again.refused.is_empty());
        assert_eq!(again.unchanged, vec!["a.json".to_string(), "z.json".to_string()]);

        // And what was written is what the loader reads back.
        let set = load(&root, false, &[]);
        assert_eq!(set.counts(), (2, 0));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_shipped_set_that_cannot_be_written_whole_is_not_written_at_all() {
        let root = std::env::temp_dir().join(format!("jev-rules-collide-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let target = root.join(DIR);
        let files = [("code/a.json", "{}"), ("prose/a.json", "{}")];
        let err = materialise(&files, &target, false).unwrap_err();
        assert!(err.contains("a.json"), "{err}");
        assert!(!target.exists(), "a refusal writes nothing: {err}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_shipped_set_this_binary_carries_is_whole() {
        // The only test that reads the real embedded set. It is vacuous while `default_rules/`
        // is empty — the rule files are written by hand, in parallel with this code — and it is
        // what fails the build's tests the day a shipped rule does not parse, does not lint
        // clean, or cannot be materialised.
        let files = builtin_files();
        let set = load_named(files, RuleSource::Builtin);
        assert!(set.skipped.is_empty(), "{:?}", set.skipped);
        assert_eq!(lint(&set), Vec::<String>::new());
        assert!(plan(files).is_ok(), "{:?}", plan(files));
        if !files.is_empty() {
            assert!(
                !set.rules.is_empty(),
                "{} shipped file(s) hold no rules at all",
                files.len()
            );
        }
    }

    #[test]
    fn the_hash_follows_the_rules() {
        let rule = one_rule();
        let other = {
            let mut r = rule.clone();
            r.title = "something else".to_string();
            r
        };
        assert_eq!(hash_of(&[rule.clone()]), hash_of(&[rule.clone()]));
        assert_ne!(
            hash_of(&[rule.clone()]),
            hash_of(&[other]),
            "a rule edit must invalidate a conclusion taken under the old text"
        );
    }

    #[test]
    fn lint_is_quiet_about_a_good_rule_and_names_every_problem_it_sees() {
        let good = RuleSet {
            rules: vec![one_rule()],
            ..Default::default()
        };
        assert!(lint(&good).is_empty(), "{:?}", lint(&good));

        let mut broken = one_rule();
        broken.title = "x".repeat(MAX_TITLE + 1);
        broken.applies_to.clear();
        broken.verb_hint = Some("polish".into());
        broken.inspection = Inspection::Regex {
            pattern: "(".into(),
            max_matches: Some(2),
        };
        broken.judgement.min_probability = Some(1.5);
        let messages = lint(&RuleSet {
            rules: vec![broken.clone(), broken],
            ..Default::default()
        });
        let joined = messages.join("\n");
        for want in [
            "duplicate rule id",
            "character title",
            "empty `applies_to`",
            "not a verb",
            "uncompilable regex",
            "outside 0.0..=1.0",
        ] {
            assert!(joined.contains(want), "{want} missing from:\n{joined}");
        }

        let empty_title = RuleSet {
            rules: vec![Rule {
                title: "   ".into(),
                ..one_rule()
            }],
            ..Default::default()
        };
        assert!(lint(&empty_title).join("\n").contains("empty title"));

        let absent_with_max = RuleSet {
            rules: vec![Rule {
                inspection: Inspection::Absent {
                    pattern: "// Copyright".into(),
                    max_matches: Some(1),
                },
                ..one_rule()
            }],
            ..Default::default()
        };
        assert!(lint(&absent_with_max)
            .join("\n")
            .contains("`max_matches` on an `absent`"));
    }

    #[test]
    fn a_rule_deserialises_the_documented_shape() {
        let rule = one_rule();
        assert_eq!(rule.severity.as_deref(), Some("warning"));
        assert_eq!(rule.applies_to, vec!["**/*.rs".to_string()]);
        assert_eq!(rule.judgement.min_probability, Some(0.75));
        assert_eq!(rule.verb_hint.as_deref(), Some("fix"));
        match &rule.inspection {
            Inspection::Regex { pattern, max_matches } => {
                assert_eq!(pattern, "\\.unwrap\\(\\)");
                assert_eq!(*max_matches, Some(0));
            }
            other => panic!("expected a regex inspection, got {other:?}"),
        }
    }
}
