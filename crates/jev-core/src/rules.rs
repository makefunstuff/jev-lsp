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
//! **One document, two spellings.** A rule file is `schema: jev.rules/1` and a list of rules, and
//! it is read whether it is written as JSON (`.json`) or as YAML (`.yaml`, `.yml`). The extension
//! picks the parser and nothing else: both spellings deserialise into the same `RuleFile`, so the
//! merged set, the hash, the lints and the findings cannot tell which one a rule was authored in,
//! and a repository can move a file from one spelling to the other without changing what a pass
//! does. YAML is the spelling for a *hand*: prose does not need `\n` escapes, and a regex is
//! written once rather than escaped twice (see GUIDE §4). JSON remains the interchange — the
//! shipped set is embedded as JSON, and `jev rules compile` emits it.
//!
//! **Two sources, one set.** A rule comes either from `.jev/rules/*.{json,yaml,yml}` — the
//! repository's own, `RuleSource::Repository` — or from the set shipped inside the binary
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
///
/// One flat directory: the repository's rule files, one rule or many per file, as JSON
/// (`.json`) or YAML (`.yaml`, `.yml`).
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_matches: Option<usize>,
    },
    /// The pattern the file is expected to contain but does not (a licence header, a module
    /// declaration). One candidate, at the head of the file.
    Absent {
        pattern: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criteria: Option<serde_json::Value>,
    /// The labels an answer may pick to say *why*. Advisory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasons: Option<serde_json::Value>,
    /// A candidate is only reported when the decision's probability clears this. Default 0.5:
    /// a coin flip is not a finding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    #[serde(default)]
    pub applies_to: Vec<String>,
    pub inspection: Inspection,
    pub judgement: Judgement,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verb_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
    /// Where this rule was read from. Never part of the file format and never part of the
    /// rules' hash: a rule's *text* is what a conclusion was taken against, and the two
    /// sources cannot both contribute the same `id` (see `merge`).
    #[serde(skip)]
    pub source: RuleSource,
}

/// What one rule file holds, in either spelling.
///
/// `Serialize` is here for `jev rules compile`, which reads a `.yaml` file through this struct
/// and writes the `.json` document back out: one deserialiser and one serialiser for the format,
/// so a compiled file is a file [`parse`] reads again. An optional field that is absent stays
/// absent on the way out (`skip_serializing_if`), so the emitted document has the shape a rule
/// file is written in — the same keys a hand-authored `.json` carries, no `null` padding —
/// and a diff against that file is the rule's change rather than the converter's.
#[derive(Debug, Serialize, Deserialize)]
struct RuleFile {
    #[serde(default)]
    schema: Option<String>,
    #[serde(default)]
    rules: Vec<Rule>,
}

/// The rule-file spellings the loader reads, chosen by the file's extension.
///
/// The extension picks the parser and nothing else. It is not a check on the contents — a
/// `.yaml` file holding JSON is valid YAML, because YAML is a JSON superset, and this parser
/// reads it as the document it is — and a file with no rule-file extension is not a rule file at
/// all, so a README left in `.jev/rules/` is ignored rather than reported as broken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Json,
    Yaml,
}

impl Format {
    /// The format a path's extension names, or `None` when the loader does not read it.
    fn of(path: &Path) -> Option<Format> {
        match path.extension().and_then(|e| e.to_str()) {
            Some("json") => Some(Format::Json),
            Some("yaml") | Some("yml") => Some(Format::Yaml),
            _ => None,
        }
    }

    /// One document, or the parser's own account of what is wrong with it.
    ///
    /// The two parsers have two error types and one message: what the caller reports is
    /// "is not a rules document" plus the parser's line, which is what a reader needs from a
    /// hand-edited file.
    fn parse(self, text: &str) -> Result<RuleFile, String> {
        match self {
            Format::Json => serde_json::from_str(text).map_err(|e| e.to_string()),
            Format::Yaml => serde_yaml::from_str(text).map_err(|e| e.to_string()),
        }
    }
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

/// Read the rules for `root`: `.jev/rules/*.{json,yaml,yml}`, plus the shipped set when
/// `defaults`.
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

/// Read one directory of rule files, in path order, in whichever spelling each one is written.
///
/// Every file whose extension is `json`, `yaml` or `yml` is read; anything else in the directory
/// is not a rule file and is left alone. Both spellings mix freely in one directory — a rule set
/// is one set whatever the extension of each file — and the order is the sorted path, so which
/// rule a pass runs first does not depend on the filesystem.
fn load_dir(dir: &Path, source: RuleSource) -> RuleSet {
    let mut paths: Vec<(PathBuf, Format)> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter_map(|path| Format::of(&path).map(|format| (path, format)))
            .collect(),
        // No rules directory is the normal case in a repository that has written none yet.
        Err(_) => return RuleSet::default(),
    };
    // Path order, and only path order: the extension must not become a tiebreaker between two
    // files whose paths merely start alike.
    paths.sort_by(|a, b| a.0.cmp(&b.0));

    let mut set = RuleSet::default();
    for (path, format) in paths {
        let shown = path.display().to_string();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                set.skipped.push((shown, format!("cannot be read: {e}")));
                continue;
            }
        };
        match parse(&text, source, format) {
            Ok(rules) => set.rules.extend(rules),
            Err(reason) => set.skipped.push((shown, reason)),
        }
    }
    set
}

/// Read embedded files that are already in memory, named as they are in the source tree.
///
/// The shipped set is embedded from `default_rules/<group>/*.json` (`build.rs`), so its names
/// end in `.json` and it parses as JSON. The extension still decides, rather than this function
/// assuming JSON: if the shipped tree ever holds a `.yaml` file it is read as YAML here with no
/// second code path, and a name carrying no rule-file extension (the shape a fixture with a
/// plain name has) keeps the JSON reading it has always had.
fn load_named(files: &[(&str, &str)], source: RuleSource) -> RuleSet {
    let mut set = RuleSet::default();
    for (name, text) in files {
        // The path a reader can open in this repository, not the build's temporary copy.
        let shown = format!("{DEFAULT_DIR}/{name}");
        let format = Format::of(Path::new(name)).unwrap_or(Format::Json);
        match parse(text, source, format) {
            Ok(rules) => set.rules.extend(rules),
            Err(reason) => set.skipped.push((shown, reason)),
        }
    }
    set
}

/// One rules document's rules, or the reason it was skipped.
fn parse(text: &str, source: RuleSource, format: Format) -> Result<Vec<Rule>, String> {
    let file = format
        .parse(text)
        .map_err(|e| format!("is not a rules document: {e}"))?;
    Ok(valid(file, source)?.rules)
}

/// One parsed document, with the schema checked and every rule stamped with its source.
///
/// The single place the `schema` string is checked and the single place a rule is told where it
/// came from, so the pass and `compile` ([`compile`]) cannot disagree about what a rule file is:
/// the loader reports the reason a file is skipped, and the converter refuses the same file with
/// the same sentence.
fn valid(mut file: RuleFile, source: RuleSource) -> Result<RuleFile, String> {
    match file.schema.as_deref() {
        Some(SCHEMA) => {
            for rule in &mut file.rules {
                rule.source = source;
            }
            Ok(file)
        }
        Some(other) => Err(format!("schema is {other:?}, not {SCHEMA:?}")),
        None => Err(format!("carries no `schema`; expected {SCHEMA:?}")),
    }
}

/// One rule file as a `jev.rules/1` JSON document, whatever spelling it was written in.
///
/// `jev rules compile` (§11) is for interchange, not for running: the loader already reads a
/// `.yaml` file where it stands, so nothing needs this to inspect with. It exists so a rule
/// authored in YAML can be handed to anything that speaks JSON — reviewed as a diff, checked
/// by a tool, or kept as a generated `.json` file — and so a conversion has one implementation
/// rather than a script per author.
///
/// It **validates while it converts**: the document goes through the same [`Format::parse`] and
/// the same schema check the pass uses, so a file this refuses (an unknown extension, a document
/// that does not parse, a schema that is not `jev.rules/1`) is one the pass would have skipped,
/// for the same reason. The returned value is the JSON document itself, so a caller can print it
/// compact or write it pretty without decoding a string again.
pub fn compile(path: &Path) -> Result<serde_json::Value, String> {
    let Some(format) = Format::of(path) else {
        return Err(format!(
            "{} is not a rule file; expected a .json, .yaml or .yml extension",
            path.display()
        ));
    };
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{} cannot be read: {e}", path.display()))?;
    let file = format
        .parse(&text)
        .map_err(|e| format!("{} is not a rules document: {e}", path.display()))?;
    let file = valid(file, RuleSource::Repository)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(serde_json::to_value(file).expect("a serde_json::Value always serializes"))
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
/// shipped rule it came from (`merge`). The name keeps the shipped file's extension — the
/// shipped set is embedded as JSON, so these are `.json` — and the written file is read back
/// whichever spelling it has.
///
/// **Each file is written under its group and its name** (`default_rules/prose/lists-end-in-etc.json`
/// → `prose-lists-end-in-etc.json`). The loader reads one flat directory (`DIR`), so the group
/// cannot survive as a directory; carrying it in the file name is what keeps two groups from
/// colliding. The groups are authored in parallel by people who cannot see each other's file
/// names, and a naming constraint they would have to agree on is a constraint that will be
/// broken — the group is information the shipped tree already has, so it is the thing that
/// disambiguates. It also puts the provenance on disk, which is the same fact `rule_source`
/// reports about a finding (PROTOCOL §9).
///
/// **Idempotent and non-clobbering.** A file that already exists is read, and left alone unless
/// `--force`: identical means nothing to do, different means the user's file wins and the report
/// names it. A second run over a directory this function filled writes nothing and changes no
/// bytes. Nothing outside `dir` is written, and nothing is ever deleted — including on a
/// refusal, which is reported rather than forced.
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

/// The name a shipped file is written under: its group, then its own name.
///
/// `code/no-unwrap.json` → `code-no-unwrap.json`; a file sitting directly in `default_rules/`
/// has no group and keeps its name. The group is the first component, so every file under one
/// group shares a prefix — which is what makes the collision check below a check *within* a
/// group, the only kind of duplicate that can still happen.
fn written_name(name: &str) -> Result<String, String> {
    let path = Path::new(name);
    let Some(base) = path.file_name().and_then(|n| n.to_str()) else {
        return Err(format!("the shipped file {name:?} has no file name to write"));
    };
    let mut parts = path.components();
    let group = match (parts.next(), parts.next()) {
        (Some(std::path::Component::Normal(group)), Some(_)) => group.to_str(),
        _ => None,
    };
    Ok(match group {
        Some(group) => format!("{group}-{base}"),
        None => base.to_string(),
    })
}

/// The shipped set as `(name written, contents)`, sorted by name and checked for collisions.
///
/// Checked *before* anything is created, because a set that cannot be written whole must not be
/// written at all: half the shipped rules in a directory, with no sign of which half, is worse
/// than a refusal that says so.
///
/// The check is **within a group**, because a name already carries its group: two files under
/// `code/` that share a basename are one mistake worth failing on (`code-a.json` twice), and
/// `code/a.json` against `prose/a.json` is not a collision at all — it is two groups, and the
/// refusal that used to fire on it would have been a naming constraint between sessions that
/// cannot see each other's files.
fn plan<'a>(files: &'a [(&'a str, &'a str)]) -> Result<Vec<(String, &'a str)>, String> {
    let mut plan: Vec<(String, &str)> = Vec::new();
    for (name, text) in files {
        plan.push((written_name(name)?, text));
    }
    plan.sort_by(|a, b| a.0.cmp(&b.0));
    // Two files of one name would silently become one file, with one rule set quietly missing
    // from the directory it was materialised into.
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

    /// `GOOD` written in the other spelling: same document, same rule, same fields.
    ///
    /// Every field `GOOD` sets is set here too — `max_matches`, `criteria`, `reasons`,
    /// `min_probability`, `docs` — because the point of the twin is that a move between the
    /// spellings is not a move between schemas. The two conventions it shows are the two a YAML
    /// author has to know: a regex in single quotes is literal, and `criteria` keys are quoted
    /// so they are the strings `"true"`/`"false"` and not YAML booleans.
    const YAML_GOOD: &str = r#"
schema: jev.rules/1
rules:
  - id: no-unwrap-in-handlers
    title: Unwrap in a request handler
    text: A handler must not unwrap; return the error instead.
    severity: warning
    applies_to:
      - "**/*.rs"
    inspection:
      kind: regex
      pattern: '\.unwrap\(\)'
      max_matches: 0
    judgement:
      question: Is this unwrap reachable from a request handler?
      criteria:
        "true": a request can reach it
        "false": test code
      reasons:
        reachable: a request can reach it
      min_probability: 0.75
    verb_hint: fix
    docs: why this rule exists
"#;

    fn one_rule() -> Rule {
        serde_json::from_str::<RuleFile>(GOOD).unwrap().rules.remove(0)
    }

    #[test]
    fn a_yaml_file_loads_as_the_rule_its_json_twin_holds() {
        // The whole claim of the second spelling: a repository can move one file from `.json`
        // to `.yaml` and the pass runs the same rule. Compared field by field through the
        // struct, so a field the YAML spelling quietly dropped shows up here.
        let root = dir("yaml-twin");
        write(&root, "a.json", GOOD);
        write(&root, "b.yaml", YAML_GOOD);
        write(&root, "c.yml", YAML_GOOD);

        let set = load(&root, false, &[]);
        assert!(set.skipped.is_empty(), "{:?}", set.skipped);
        let loaded: Vec<&str> = set.rules.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(loaded, vec!["no-unwrap-in-handlers"; 3], "{:?}", set.skipped);
        let json = one_rule();
        for rule in &set.rules {
            assert_eq!(rule, &json, "a YAML spelling changed the rule");
        }
        // And the fields a rule is written *for*, checked directly rather than through `==`, so
        // a failure says which one the YAML lost.
        let yaml = &set.rules[1];
        assert_eq!(yaml.title, "Unwrap in a request handler");
        assert_eq!(yaml.text, "A handler must not unwrap; return the error instead.");
        assert_eq!(yaml.applies_to, vec!["**/*.rs".to_string()]);
        assert_eq!(yaml.severity.as_deref(), Some("warning"));
        assert_eq!(yaml.verb_hint.as_deref(), Some("fix"));
        assert_eq!(yaml.docs.as_deref(), Some("why this rule exists"));
        assert_eq!(yaml.judgement.min_probability, Some(0.75));
        assert_eq!(
            yaml.judgement.criteria,
            Some(serde_json::json!({"true": "a request can reach it", "false": "test code"}))
        );
        assert_eq!(
            yaml.judgement.reasons,
            Some(serde_json::json!({"reachable": "a request can reach it"}))
        );
        assert_eq!(
            yaml.inspection,
            Inspection::Regex {
                pattern: "\\.unwrap\\(\\)".to_string(),
                max_matches: Some(0),
            }
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn broken_yaml_is_skipped_with_a_reason_and_the_rest_still_load() {
        let root = dir("yaml-broken");
        // An unclosed quote: unparsable, and the reason names the line.
        write(&root, "a-broken.yaml", "schema: jev.rules/1\nrules:\n  - id: 'oops\n");
        write(&root, "b-wrong-schema.yml", "schema: jev.rules/9\nrules: []\n");
        write(&root, "c-no-schema.yaml", "rules: []\n");
        // YAML is a JSON superset, so JSON pasted into a `.yaml` file is not a mistake and is
        // not reported as one: it parses as the document it is.
        write(
            &root,
            "d-json-inside-yaml.yaml",
            &GOOD.replace("no-unwrap-in-handlers", "json-inside-yaml"),
        );
        write(&root, "e-good.yaml", YAML_GOOD);

        let set = load(&root, false, &[]);
        assert_eq!(set.skipped.len(), 3, "{:?}", set.skipped);
        assert_eq!(
            set.rules.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["json-inside-yaml", "no-unwrap-in-handlers"]
        );
        assert!(set.skipped[0].0.ends_with("a-broken.yaml"));
        assert!(set.skipped[0].1.contains("not a rules document"), "{:?}", set.skipped[0]);
        assert!(
            set.skipped[0].1.contains("line 3"),
            "the reason names the line: {:?}",
            set.skipped[0]
        );
        assert!(set.skipped[1].0.ends_with("b-wrong-schema.yml"));
        assert!(set.skipped[1].1.contains("jev.rules/9"), "{:?}", set.skipped[1]);
        assert!(set.skipped[2].1.contains("schema"), "{:?}", set.skipped[2]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn json_and_yaml_load_together_in_path_order() {
        // One set, two spellings, one order: the sorted path, so the extension is not a
        // tiebreaker and the two files run in the order a reader sees them.
        let root = dir("mixed");
        write(&root, "b.yaml", &YAML_GOOD.replace("no-unwrap-in-handlers", "second"));
        write(&root, "c.yml", &YAML_GOOD.replace("no-unwrap-in-handlers", "third"));
        write(&root, "a.json", &GOOD.replace("no-unwrap-in-handlers", "first"));

        let set = load(&root, false, &[]);
        assert!(set.skipped.is_empty(), "{:?}", set.skipped);
        assert_eq!(
            set.rules.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["first", "second", "third"]
        );
        // The hash of the mixed set is the hash of the same rules, so moving a file between
        // spellings does not invalidate a cached conclusion.
        let same: Vec<Rule> = ["first", "second", "third"]
            .iter()
            .map(|id| {
                let mut rule = one_rule();
                rule.id = (*id).to_string();
                rule
            })
            .collect();
        assert_eq!(set.hash, hash_of(&same));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn one_yaml_file_holds_many_rules() {
        // A rule file is a document, not a rule: one `.yaml` file can hold the whole of a
        // repository's policy, and the loader reads it as the rules it holds, in file order.
        let root = dir("yaml-many");
        write(
            &root,
            "policy.yaml",
            r#"
schema: jev.rules/1
rules:
  - id: first
    title: First rule
    text: The first convention.
    applies_to: ["**/*.rs"]
    inspection: {kind: regex, pattern: 'TODO'}
    judgement: {question: Is it a violation?}
  - id: second
    title: Second rule
    text: The second convention.
    applies_to: ["**/*.py"]
    inspection:
      kind: absent
      pattern: '# Copyright'
    judgement:
      question: Is the header missing?
      min_probability: 0.9
"#,
        );
        let set = load(&root, false, &[]);
        assert!(set.skipped.is_empty(), "{:?}", set.skipped);
        assert_eq!(
            set.rules.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        assert_eq!(set.rules[1].applies_to, vec!["**/*.py".to_string()]);
        assert_eq!(
            set.rules[1].inspection,
            Inspection::Absent {
                pattern: "# Copyright".to_string(),
                max_matches: None,
            }
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_file_that_is_not_a_rule_file_is_left_alone() {
        // `.jev/rules/` is a directory a person reads. A README, or an editor's backup, is not
        // a broken rule and is not reported as one.
        let root = dir("not-rules");
        write(&root, "README.md", "# notes\n");
        write(&root, "a.json.bak", GOOD);
        write(&root, "a.yaml", YAML_GOOD);
        let set = load(&root, false, &[]);
        assert_eq!(set.rules.len(), 1);
        assert!(set.skipped.is_empty(), "{:?}", set.skipped);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn yaml_and_json_are_the_same_rule() {
        // The golden: the rule this repository ships for its own `.unwrap()` convention, in
        // both spellings, read from the tree rather than from a fixture. `include_str!` means
        // the test fails at the compiler if either file is deleted, and the assertions below
        // are the no-unwrap fields in full — the `applies_to` glob, the pattern, the question,
        // both criteria, the floor and the verb hint.
        let json_text = include_str!("../../../.jev/rules/no-unwrap-outside-tests.json");
        let yaml_text = include_str!("../../../docs/research/examples/no-unwrap-outside-tests.yaml");

        let json = parse(json_text, RuleSource::Repository, Format::Json).unwrap();
        let yaml = parse(yaml_text, RuleSource::Repository, Format::Yaml).unwrap();
        assert_eq!(json.len(), 1);
        assert_eq!(
            yaml, json,
            "the YAML example must be the JSON rule, not a rule that resembles it"
        );

        let rule = &yaml[0];
        assert_eq!(rule.id, "no-unwrap-outside-tests");
        assert_eq!(rule.title, "Unwrap outside tests");
        assert_eq!(rule.applies_to, vec!["**/crates/**/*.rs".to_string()]);
        assert_eq!(
            rule.inspection,
            Inspection::Regex {
                pattern: "\\.unwrap\\(\\)".to_string(),
                max_matches: None,
            }
        );
        assert_eq!(rule.judgement.min_probability, Some(0.85));
        assert_eq!(rule.verb_hint.as_deref(), Some("fix"));
        assert!(
            rule.text.contains("— handle the error"),
            "the prose survived the spelling change intact: {}",
            rule.text
        );
        assert!(
            rule.judgement.question.contains("rather than inside a test module"),
            "the folded question is one line: {}",
            rule.judgement.question
        );
        assert_eq!(
            rule.judgement.criteria,
            Some(serde_json::json!({
                "true": "code that ships can reach it and the value is not guaranteed",
                "false": "it is in a test module, quoted in a doc comment, or behind an invariant the surrounding lines state",
            }))
        );

        // And the interchange direction, on the same two files: `jev rules compile` of the YAML
        // emits the document in the tree, and compiling the JSON file is that file again. Both
        // compared as JSON values, so this is the *document* being equal, not just the rules it
        // deserialises to.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let checked_in: serde_json::Value = serde_json::from_str(json_text).unwrap();
        assert_eq!(
            compile(
                &root.join("docs/research/examples/no-unwrap-outside-tests.yaml")
            )
            .unwrap(),
            checked_in,
            "compiling the YAML example must produce the JSON rule that is in the tree"
        );
        assert_eq!(
            compile(&root.join(".jev/rules/no-unwrap-outside-tests.json")).unwrap(),
            checked_in,
            "compiling a JSON rule file is that file: no key added, no `null` padding"
        );
    }

    #[test]
    fn compile_emits_json_the_loader_reads_back() {
        let source = dir("compile");
        write(&source, "a.yaml", YAML_GOOD);
        let path = source.join(DIR).join("a.yaml");

        let document = compile(&path).unwrap();
        assert_eq!(document["schema"], "jev.rules/1");
        assert_eq!(document["rules"][0]["id"], "no-unwrap-in-handlers");

        // What `compile` writes is what `load` reads: the emitted JSON, on its own, is a rule
        // file the pass runs — into a directory of its own, so what is loaded is the compiled
        // document and not the YAML it came from.
        let target = dir("compile-out");
        let written = target.join(DIR).join("a.json");
        std::fs::write(&written, serde_json::to_string_pretty(&document).unwrap()).unwrap();
        let set = load(&target, false, &[]);
        assert!(set.skipped.is_empty(), "{:?}", set.skipped);
        assert_eq!(set.rules.len(), 1, "compiling to JSON must not change the rule");
        assert_eq!(set.rules[0], one_rule());

        // A file the pass would skip is refused here too, with the reason.
        std::fs::write(source.join(DIR).join("bad.yaml"), "schema: jev.rules/9\nrules: []\n")
            .unwrap();
        let e = compile(&source.join(DIR).join("bad.yaml")).unwrap_err();
        assert!(e.contains("jev.rules/9"), "{e}");
        let e = compile(&source.join(DIR).join("notes.md")).unwrap_err();
        assert!(e.contains("not a rule file"), "{e}");
        std::fs::remove_dir_all(&source).ok();
        std::fs::remove_dir_all(&target).ok();
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
        assert_eq!(
            first.written,
            vec!["code-a.json".to_string(), "prose-z.json".to_string()],
            "each file is named for its group, so two groups cannot collide"
        );
        assert!(!first.is_refusal());
        assert_eq!(std::fs::read(target.join("code-a.json")).unwrap(), a.as_bytes());
        // Nothing outside the target: the shipped tree's group directories are not recreated.
        let beside: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(beside, vec![".jev".to_string()]);

        // A user edits one of them, the way the command exists for.
        std::fs::write(target.join("code-a.json"), "{ the user's own rule }").unwrap();
        let second = materialise(&files, &target, false).unwrap();
        assert!(second.written.is_empty());
        assert_eq!(second.unchanged, vec!["prose-z.json".to_string()]);
        assert_eq!(second.refused, vec!["code-a.json".to_string()]);
        assert!(second.is_refusal());
        assert_eq!(
            std::fs::read(target.join("code-a.json")).unwrap(),
            b"{ the user's own rule }",
            "a refusal is a refusal: the user's bytes are untouched"
        );

        // `--force` is the only thing that replaces them, and a run after it changes nothing
        // again.
        let forced = materialise(&files, &target, true).unwrap();
        assert_eq!(
            forced.written,
            vec!["code-a.json".to_string(), "prose-z.json".to_string()]
        );
        assert!(!forced.is_refusal());
        assert_eq!(std::fs::read(target.join("code-a.json")).unwrap(), a.as_bytes());
        let again = materialise(&files, &target, false).unwrap();
        assert!(again.written.is_empty() && again.refused.is_empty());
        assert_eq!(
            again.unchanged,
            vec!["code-a.json".to_string(), "prose-z.json".to_string()]
        );

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
        // One group, one name: the mistake that is left, and it is still refused.
        let collide = [("code/a.json", "{}"), ("code/b/a.json", "{}")];
        let err = materialise(&collide, &target, false).unwrap_err();
        assert!(err.contains("code-a.json"), "{err}");
        assert!(!target.exists(), "a refusal writes nothing: {err}");

        // Two groups, one basename: **not** a collision. The groups are authored in parallel by
        // people who cannot see each other's file names, and the written name carries the group
        // precisely so this needs no coordination.
        let both = [("code/a.json", "{\"code\":true}"), ("prose/a.json", "{\"prose\":true}")];
        let report = materialise(&both, &target, false).unwrap();
        assert_eq!(report.written, vec!["code-a.json", "prose-a.json"]);
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
