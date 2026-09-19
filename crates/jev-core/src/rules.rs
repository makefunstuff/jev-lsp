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

use crate::types::Verb;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The schema string every rule file must carry.
pub const SCHEMA: &str = "jev.rules/1";

/// Where rules live, relative to the workspace root.
pub const DIR: &str = ".jev/rules";

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
}

/// Read `.jev/rules/*.json` under `root`, in path order.
///
/// A file that cannot be read, cannot be parsed, or does not carry `schema: "jev.rules/1"` is
/// skipped with a stated reason and the rest still load — the same treatment `Config` gives a
/// malformed settings payload, and for the same reason: a bad file in someone's repository must
/// not take the server down, and must not be silent either.
pub fn load(root: &Path) -> RuleSet {
    let dir = root.join(DIR);
    let mut paths: Vec<PathBuf> = match std::fs::read_dir(&dir) {
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
        let file: RuleFile = match serde_json::from_str(&text) {
            Ok(file) => file,
            Err(e) => {
                set.skipped.push((shown, format!("is not a rules document: {e}")));
                continue;
            }
        };
        match file.schema.as_deref() {
            Some(SCHEMA) => set.rules.extend(file.rules),
            Some(other) => set.skipped.push((
                shown,
                format!("schema is {other:?}, not {SCHEMA:?}"),
            )),
            None => set.skipped.push((shown, format!("carries no `schema`; expected {SCHEMA:?}"))),
        }
    }
    set.hash = hash_of(&set.rules);
    set
}

/// A stable digest of the rules, taken over their JSON.
///
/// The field order is the struct's, so the same rules always hash the same; a changed rule — or
/// a reordered one — hashes differently, which is what invalidates a cache entry.
pub fn hash_of(rules: &[Rule]) -> String {
    let bytes = serde_json::to_vec(rules).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(&bytes);
    let d = h.finalize();
    d.iter().map(|b| format!("{b:02x}")).collect()
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

        let set = load(&root);
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
        let set = load(&root);
        assert!(set.rules.is_empty());
        assert!(set.skipped.is_empty());
        assert_eq!(set.hash, hash_of(&[]));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn files_load_in_path_order() {
        let root = dir("order");
        write(&root, "b.json", &GOOD.replace("no-unwrap-in-handlers", "second"));
        write(&root, "a.json", &GOOD.replace("no-unwrap-in-handlers", "first"));
        let set = load(&root);
        assert_eq!(
            set.rules.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        std::fs::remove_dir_all(&root).ok();
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
