//! Turning a rule's inspection into the lines a judgement can be asked about — and the
//! judgement's answer back into findings.
//!
//! This is the cheap half of a rules pass and it is *only* cheap: no model is called
//! here, no decision is made, and nothing in the inspection half knows what a violation is. A
//! candidate says "this line matched", never "this line is wrong".
//!
//! The needle a candidate carries is the text `findings::build` will anchor the finding on, and
//! `build` drops an anchor that occurs zero or more than one time. So the needle has to be
//! uniquely locatable, and the work of making it so happens here (see [`needle_at`]) rather than
//! being discovered as a silently-dropped finding later.
//!
//! [`select`], [`request`] and [`resolve`] are the whole pass, and they live here rather than in
//! a front end because both front ends run it: the language server's ambient pass and
//! `jev inspect` must produce the same findings from the same inputs, and the only way to
//! guarantee that is for there to be one implementation of it. The definitions the *client*
//! sent (PROTOCOL §3.4.3) are one of those inputs: [`request`] is shown the declaration a
//! candidate is inside when the client knows it, and `jev inspect`, which has no client, is
//! shown the neighbourhood instead. Two different states, so `cache::rules_key` takes the
//! definitions' own digest and neither conclusion answers for the other.

use crate::config::RulesConfig;
use crate::contract::{RawAnchor, RawFinding, RawFindings};
use crate::decision::{DecisionQuestion, DecisionRequest, DecisionResponse, DecisionValue, QuestionKind};
use crate::findings::{self, FindingBuild};
use crate::gates;
use crate::lang::Profile;
use crate::rules::{self, Inspection, Rule, RuleSet};
use crate::types::LineRange;
use serde_json::Value;

/// A place a rule is about: a 0-based line and the text that locates it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub line: u32,
    /// What `findings::build` anchors on. May still be ambiguous if the file defeats every
    /// attempt to disambiguate it, in which case `build` rejects it and the count is reported.
    pub needle: String,
}

/// How many whole lines a needle may be extended to before it is given up on.
const MAX_SPAN: usize = 5;

/// The candidates one rule finds in one document, capped at `max`.
///
/// `max` is `rules.max_candidates_per_rule`: a regex that matches every line of a generated file
/// must not turn into a thousand questions.
pub fn candidates(rule: &Rule, text: &str, max: usize) -> Vec<Candidate> {
    // Compiled once per pass, never per line: a pattern is a rule's, not a line's.
    let Ok(re) = regex::Regex::new(rule.inspection.pattern()) else {
        // Lint reports this. A pattern that does not compile finds nothing rather than
        // matching everything, which is the safe direction for a *check*.
        return Vec::new();
    };
    let lines: Vec<&str> = text.split('\n').collect();
    match &rule.inspection {
        Inspection::Regex { max_matches, .. } => {
            // A threshold rule: the point is repetition, so nothing is reported until the file
            // holds more matches than the rule allows.
            let total = re.find_iter(text).count();
            if max_matches.is_some_and(|allowed| total <= allowed) {
                return Vec::new();
            }
            lines
                .iter()
                .enumerate()
                .filter(|(_, line)| re.is_match(line))
                .take(max)
                .map(|(i, _)| Candidate {
                    line: i as u32,
                    needle: needle_at(&lines, i, text),
                })
                .collect()
        }
        Inspection::Absent { .. } => {
            if re.is_match(text) {
                return Vec::new();
            }
            // Where a missing pattern is reported: the first line that does not carry it. With
            // the pattern absent from the whole document that is the head of the file, which is
            // where a licence header or a module declaration belongs.
            match lines.iter().position(|line| !re.is_match(line)) {
                Some(i) if max > 0 => vec![Candidate {
                    line: i as u32,
                    needle: needle_at(&lines, i, text),
                }],
                _ => Vec::new(),
            }
        }
    }
}

/// One candidate, with the rule that found it.
#[derive(Debug, Clone, PartialEq)]
pub struct Asked<'a> {
    pub rule: &'a Rule,
    pub candidate: Candidate,
}

impl Asked<'_> {
    /// The id the question is asked under: the rule, and the 0-based line.
    ///
    /// Composed rather than opaque so the same candidate in the same file is always the same
    /// question — and so the state that lists the candidates and the response that answers them
    /// can be read against each other by eye when something goes wrong.
    pub fn id(&self) -> String {
        format!("{}#{}", self.rule.id, self.candidate.line)
    }
}

/// The rules that claim `path`, and the candidates they found in `text`.
///
/// Returns how many rules survived `applies_to`. That is a different number from how many rules
/// exist, and the difference is the whole reason `applies_to` is part of the schema: a rule for
/// `**/*.rs` must never see a Python file.
pub fn select<'a>(
    rules: &'a [Rule],
    path: &str,
    text: &str,
    max_per_rule: usize,
) -> (usize, Vec<Asked<'a>>) {
    let applicable: Vec<&'a Rule> = rules
        .iter()
        .filter(|r| r.applies_to.iter().any(|pattern| gates::glob_match(pattern, path)))
        .collect();
    let considered = applicable.len();
    let mut asked = Vec::new();
    for rule in applicable {
        for candidate in candidates(rule, text, max_per_rule) {
            asked.push(Asked { rule, candidate });
        }
    }
    (considered, asked)
}

/// Everything a pass owes a reader about the rule set it was run with, in the one list both
/// front ends send as `skipped` (PROTOCOL §6).
///
/// Three kinds, one implementation:
///
/// * `("lint", <message>)` — everything wrong with the rules *document* (`rules::lint`). A rule
///   whose pattern does not compile finds no candidates and is inert in silence otherwise, which
///   is the failure mode lint exists for.
/// * `("default_rules", <sentence>)` — the pass is running on the shipped set because this
///   repository has written no rules of its own. Without this, a fresh install and a broken one
///   read the same: findings from a file nobody can open, with nothing saying where they came
///   from or how to switch them off.
/// * `("no_rules", <sentence>)` — the pass had nothing to run: no rules at all, or none that
///   claims this document. This is the case the ambient demotion creates, and what it must not
///   be is *silence*: "no findings" and "nothing was inspected" look identical from a sign
///   column, and only one of them is a bug.
///
/// It lives here, beside the pass it describes, so the language server and `jev inspect` cannot
/// drift: one implementation, called by both shells. A front end that reports nothing while the
/// other reports the reason is a parity failure, and this function exists because that happened.
pub fn pass_notes(
    rules: &RuleSet,
    considered: usize,
    doc_path: &str,
    root: &str,
) -> Vec<(String, String)> {
    let mut notes: Vec<(String, String)> = rules::lint(rules)
        .into_iter()
        .map(|message| ("lint".to_string(), message))
        .collect();

    let dir = std::path::Path::new(root).join(rules::DIR).display().to_string();
    let (repo, shipped) = rules.counts();

    if rules.rules.is_empty() {
        // "There is nothing to run" has two causes now, and they send a reader in opposite
        // directions: this build ships no rules at all, or it ships them and this repository
        // turned them off. Saying the first when the second is true would send someone looking
        // for a missing file that is right there.
        notes.push((
            "no_rules".to_string(),
            if rules.shipped > 0 {
                format!(
                    "no rules loaded from {dir}, and the {} shipped rule file(s) are switched off (rules.defaults = false) — the ambient pass has nothing to run",
                    rules.shipped
                )
            } else {
                format!(
                    "no rules loaded from {dir} and none shipped in this build — the ambient pass has nothing to run"
                )
            },
        ));
        return notes;
    }

    // The rule files this repository does *not* have, said out loud. A pass that ran is never
    // silent about where its rules came from.
    if repo == 0 && shipped > 0 {
        notes.push((
            "default_rules".to_string(),
            format!(
                "{shipped} shipped rule(s) are carrying this pass; {dir} holds none of this \
                 repository's own — `jev rules init` writes them out to read and edit, and \
                 `rules.defaults = false` turns them off"
            ),
        ));
    }

    if considered == 0 {
        notes.push((
            "no_rules".to_string(),
            format!(
                "no rule applies to {doc_path} ({repo} from {dir}, {shipped} shipped) — the ambient pass has nothing to run"
            ),
        ));
    }
    notes
}

/// The request the whole candidate set is asked in. One call, one document.
///
/// `defs` is what the client sent for this document version (PROTOCOL §3.4.3), or empty when
/// nothing did — `jev inspect` has no client, and a client whose parser has none for the
/// language sends nothing. It decides the shape of each window in the state and nothing else,
/// which is why it is an argument here rather than a read of a global: a pass with no
/// definitions must be reproducible from the files alone.
pub fn request(
    path: &str,
    text: &str,
    asked: &[Asked],
    cfg: &RulesConfig,
    defs: &[LineRange],
) -> DecisionRequest {
    DecisionRequest {
        state: state(path, text, asked, cfg, defs),
        questions: asked
            .iter()
            .map(|a| DecisionQuestion {
                id: a.id(),
                kind: QuestionKind::Noul,
                instructions: a.rule.judgement.question.clone(),
                // `noul` criteria are `{"true": …, "false": …}` when the rule states them, and
                // omitted entirely when it does not.
                criteria: a
                    .rule
                    .judgement
                    .criteria
                    .clone()
                    .unwrap_or(Value::Null),
                reasons: a.rule.judgement.reasons.clone(),
            })
            .collect(),
    }
}

/// What a response earns.
///
/// A `true` that clears the rule's own probability floor becomes a raw finding, and the whole set
/// then goes through [`findings::build`] — the *same* function the chat review's findings pass
/// through. That is what makes a rule finding behave like any other on every surface: ids,
/// dismissal, ordering and the noise cap all follow from it, and none of them is reimplemented
/// here.
pub fn resolve(
    text: &str,
    asked: &[Asked],
    response: &DecisionResponse,
    profile: &Profile,
    max_findings: usize,
) -> FindingBuild {
    let mut raw = RawFindings {
        findings: Vec::new(),
    };
    for a in asked {
        let answer = response.answer(&a.id());
        // A `false`, or an answer nobody gave, publishes nothing. `DecisionValue::None` is
        // deliberately not the same as `Bool(false)`: one is a denial, the other is silence.
        if answer.value != DecisionValue::Bool(true) {
            continue;
        }
        let Some(probability) = answer.probability else {
            continue;
        };
        let floor = a
            .rule
            .judgement
            .min_probability
            .unwrap_or(rules::DEFAULT_MIN_PROBABILITY);
        if probability < floor {
            continue;
        }
        let mut detail = a.rule.text.clone();
        detail.push_str(" — ");
        if let Some(reason) = &answer.reason {
            detail.push_str(reason);
            detail.push(' ');
        }
        detail.push_str(&format!("(p={probability:.2})"));
        raw.findings.push(RawFinding {
            anchor: RawAnchor {
                kind: None,
                name: None,
                needle: a.candidate.needle.clone(),
            },
            severity: a.rule.severity.clone(),
            label: a.rule.title.clone(),
            detail: Some(detail),
            verb_hint: a.rule.verb_hint.clone(),
            // Which rule set this came from rides onto the finding: a shipped default and a
            // rule the repository wrote are turned off in different places, and a reader who
            // cannot tell which they are looking at can do neither.
            rule_source: Some(a.rule.source),
        });
    }
    findings::build(text, &raw, profile, max_findings)
}

/// The lines a candidate is shown with when no declaration covers it: a few tens either side,
/// snapped out to the enclosing blank-line-separated block when that block is small enough to
/// be the statement's own, which is what a reader would have looked at.
///
/// Not `max_state_lines`: this is one candidate's neighbourhood, and it is the *budget* that
/// decides how many of these the state can hold.
const WINDOW_CONTEXT: usize = 20;

/// The most lines one window may span before it stops being a window and becomes the head
/// again. A declaration the client sent can cover half the file; showing the half the candidate
/// is in, and the head of the declaration when the candidate sits near it, is the point.
const MAX_WINDOW_LINES: usize = 80;

/// One place the state shows, and the candidates it is shown for.
///
/// A window is the unit the state is built from: windows that overlap are merged into one, each
/// is measured against the budget on its own, and a window the budget cannot hold is trimmed —
/// or, when its candidates are too far apart for one window, split between them. The candidate
/// lines are the floor of every one of those operations.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Window {
    /// 0-based, inclusive, into `text.split('\n')` — the same indices the candidate ids use.
    start: usize,
    end: usize,
    /// The candidate lines inside it, ascending. Never dropped, by trimming or by splitting.
    keep: Vec<usize>,
}

impl Window {
    fn lines(&self) -> usize {
        self.end - self.start + 1
    }

    /// How many bytes this window adds to the state, without building it. Must stay equal to
    /// [`Window::render`]'s length, which `a_window_measures_what_it_renders` pins.
    fn bytes(&self, lines: &[&str]) -> usize {
        let mut n = 10 + digits(self.start) + digits(self.end);
        for i in self.start..=self.end {
            n += digits(i) + 2 + lines[i].len() + 1;
        }
        n
    }

    /// The window as the state writes it. The marker is what makes a gap between windows
    /// readable: without it the numbers jump and the file reads as if it were edited.
    fn render(&self, lines: &[&str]) -> String {
        let mut out = format!("[lines {}-{}]\n", self.start, self.end);
        for i in self.start..=self.end {
            out.push_str(&format!("{i}: {}\n", lines[i]));
        }
        out
    }

    /// Whether a line can be dropped from an end without dropping a candidate.
    fn can_trim(&self) -> bool {
        let last = *self.keep.last().expect("a window holds a candidate");
        self.start < self.keep[0] || self.end > last
    }

    /// Drop one line from whichever end is farther from the candidates, so what is left stays
    /// centred on them.
    fn trim_one(&mut self) {
        if !self.can_trim() {
            return;
        }
        let left = self.keep[0] - self.start;
        let right = self.end - *self.keep.last().expect("a window holds a candidate");
        if left >= right && left > 0 {
            self.start += 1;
        } else {
            self.end -= 1;
        }
    }

    /// Cut at the widest gap between the candidates this window holds, so each side can be
    /// trimmed on its own. `None` when there is only one candidate to keep.
    ///
    /// This is the alternative to sending no neighbourhood at all: a window whose candidates
    /// are 100 lines apart is two neighbourhoods, and two neighbourhoods fit a budget one
    /// window cannot.
    fn split_widest(&self) -> Option<(Window, Window)> {
        if self.keep.len() < 2 {
            return None;
        }
        let (mut at, mut widest) = (1usize, 0usize);
        for i in 1..self.keep.len() {
            let gap = self.keep[i] - self.keep[i - 1];
            if gap > widest {
                widest = gap;
                at = i;
            }
        }
        let cut = (self.keep[at - 1] + self.keep[at]) / 2;
        Some((
            Window {
                start: self.start,
                end: cut,
                keep: self.keep[..at].to_vec(),
            },
            Window {
                start: cut + 1,
                end: self.end,
                keep: self.keep[at..].to_vec(),
            },
        ))
    }
}

/// Decimal digits, without the `String` `format!` would build to count them.
fn digits(mut n: usize) -> usize {
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

/// Which budget a [`fit`] pass is spending.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Metric {
    Lines,
    Bytes,
}

fn measure(w: &Window, lines: &[&str], metric: Metric) -> usize {
    match metric {
        Metric::Lines => w.lines(),
        Metric::Bytes => w.bytes(lines),
    }
}

/// Trim and split the windows until they fit `budget`.
///
/// Water-filling, then splitting: a window over its share of the budget loses a line, always
/// from the end farther from its candidates; a window that cannot lose a line without losing a
/// candidate, while the total is still over, is split at the widest gap between the candidates
/// it holds. The loop stops when the total fits, or when every window is down to the candidate
/// lines it exists to show — a budget too small for those is not met by showing less than the
/// thing being judged, and `truncate_state` is the bound of last resort.
fn fit(windows: &mut Vec<Window>, lines: &[&str], budget: usize, metric: Metric) {
    loop {
        let total: usize = windows.iter().map(|w| measure(w, lines, metric)).sum();
        if total <= budget || windows.is_empty() {
            return;
        }
        let share = budget / windows.len();
        let over = windows
            .iter()
            .enumerate()
            .filter(|(_, w)| w.can_trim() && measure(w, lines, metric) > share)
            .max_by_key(|(_, w)| measure(w, lines, metric))
            .map(|(i, _)| i);
        if let Some(i) = over {
            windows[i].trim_one();
            continue;
        }
        let splittable = windows
            .iter()
            .enumerate()
            .filter(|(_, w)| w.keep.len() > 1)
            .max_by_key(|(_, w)| measure(w, lines, metric))
            .map(|(i, _)| i);
        match splittable.and_then(|i| windows[i].split_widest().map(|(a, b)| (i, a, b))) {
            Some((i, a, b)) => {
                windows[i] = a;
                windows.insert(i + 1, b);
            }
            None => return,
        }
    }
}

/// The smallest declaration the client sent that contains `line`, by span and then by the
/// later start (the innermost of two identical spans).
fn enclosing(defs: &[LineRange], line: u32) -> Option<LineRange> {
    defs.iter()
        .filter(|d| d.start_line <= line && line <= d.end_line)
        .min_by_key(|d| {
            (
                d.end_line.saturating_sub(d.start_line),
                std::cmp::Reverse(d.start_line),
            )
        })
        .copied()
}

/// The lines `line` is shown with: the declaration the client sent when it sent one that
/// contains it, otherwise [`neighbourhood`].
///
/// A declaration longer than [`MAX_WINDOW_LINES`] is shown around the candidate — its head when
/// the candidate is near the head, which is where the signature that names the thing is, and a
/// centred span otherwise. Showing the whole of a 600-line declaration is the head problem
/// wearing a different hat: the budget would cut it, and the cut would land wherever the bytes
/// ran out rather than where the question is.
fn window_for(lines: &[&str], line: usize, defs: &[LineRange]) -> (usize, usize) {
    let last = lines.len().saturating_sub(1);
    if let Some(d) = enclosing(defs, line as u32) {
        let start = d.start_line as usize;
        let end = (d.end_line as usize).min(last);
        if start <= line && line <= end {
            if end - start + 1 <= MAX_WINDOW_LINES {
                return (start, end);
            }
            let mut from = if line < start + MAX_WINDOW_LINES {
                start
            } else {
                line.saturating_sub(MAX_WINDOW_LINES / 2)
            };
            from = from.max(start).min(end + 1 - MAX_WINDOW_LINES);
            return (from, (from + MAX_WINDOW_LINES - 1).min(end));
        }
    }
    neighbourhood(lines, line)
}

/// A few tens of lines either side of `line`, snapped out to the blank-line-separated block it
/// sits in when that block is at most the size of the window itself — a statement is read with
/// the block it belongs to, and a block that big is one.
fn neighbourhood(lines: &[&str], line: usize) -> (usize, usize) {
    let last = lines.len().saturating_sub(1);
    let lo = line.saturating_sub(WINDOW_CONTEXT);
    let hi = (line + WINDOW_CONTEXT).min(last);
    let (bs, be) = block_bounds(lines, line);
    if be - bs + 1 <= 2 * WINDOW_CONTEXT + 1 {
        (lo.min(bs), hi.max(be))
    } else {
        (lo, hi)
    }
}

/// The maximal run of non-blank lines containing `line`. A blank line is its own block, which
/// the union in [`neighbourhood`] then pads out.
fn block_bounds(lines: &[&str], line: usize) -> (usize, usize) {
    if lines.get(line).is_none_or(|l| l.trim().is_empty()) {
        return (line, line);
    }
    let mut start = line;
    while start > 0 && !lines[start - 1].trim().is_empty() {
        start -= 1;
    }
    let mut end = line;
    while end + 1 < lines.len() && !lines[end + 1].trim().is_empty() {
        end += 1;
    }
    (start, end)
}

/// One window per candidate, merged where they overlap.
///
/// The merge is what stops a document with thirteen candidates in one region from sending that
/// region thirteen times, with thirteen copies of the marker around it; the line numbers are
/// absolute, so the merged window still says exactly where each candidate is.
fn windows_for(lines: &[&str], asked: &[Asked], defs: &[LineRange]) -> Vec<Window> {
    let mut places: Vec<(usize, usize, usize)> = Vec::new();
    for a in asked {
        let line = a.candidate.line as usize;
        if line >= lines.len() {
            // A candidate cannot name a line the document does not have; if one ever does, the
            // state is not the place to find out.
            continue;
        }
        let (start, end) = window_for(lines, line, defs);
        places.push((start, end, line));
    }
    places.sort_unstable();

    let mut windows: Vec<Window> = Vec::new();
    for (start, end, line) in places {
        match windows.last_mut() {
            Some(w) if start <= w.end + 1 => {
                w.end = w.end.max(end);
                if !w.keep.contains(&line) {
                    w.keep.push(line);
                    w.keep.sort_unstable();
                }
            }
            _ => windows.push(Window {
                start,
                end,
                keep: vec![line],
            }),
        }
    }
    windows
}

/// What the decision is shown for one document.
///
/// Three parts, in the order they are read: the lines around each candidate, numbered from zero
/// and absolute so the numbers match the candidate ids; the candidates, each naming the rule and
/// the line it came from; and the rules themselves, so the judgement has its own prose and its
/// criteria in front of it rather than a paraphrase of them.
///
/// **Around each candidate, not from the top.** The state used to be the file head — the first
/// `max_state_lines` lines — while candidates are found anywhere in the document, so a candidate
/// at line 2,000 of a 2,800-line file was asked about with 200 lines of a file it was not in:
/// thirteen of this repository's own thirteen candidates on `crates/jev-lsp/src/server.rs` lay
/// outside the window they were shown in, and every floor measured on that state was measured
/// with the line invisible. It is also the reason a whole class of rules cannot be written here:
/// "an abstraction with one use site", "a hand-rolled JSON parser" — each needs a fact that is
/// not on the candidate's line and was not in the head either, and how many call sites a
/// declaration has is a property of the *declaration*, which is the thing the client already
/// knows and already sends (§3.4.3).
fn state(
    doc_path: &str,
    text: &str,
    asked: &[Asked],
    cfg: &RulesConfig,
    defs: &[LineRange],
) -> String {
    // The same split the candidate lines were counted with, so the state's numbers and the
    // candidate ids can never drift apart.
    let lines: Vec<&str> = text.split('\n').collect();
    let mut windows = windows_for(&lines, asked, defs);

    let mut out = String::new();
    out.push_str(&format!("FILE {doc_path}\n"));
    out.push_str(
        "line numbers below are 0-based and absolute, as in the candidate list; \
         only the lines around each candidate are shown\n",
    );

    let mut tail = String::from("\nCANDIDATES\n");
    for a in asked {
        tail.push_str(&format!(
            "{} {} line {}: {}\n",
            a.id(),
            a.rule.id,
            a.candidate.line,
            a.candidate.needle
        ));
    }

    tail.push_str("\nRULES\n");
    let mut seen: Vec<&str> = Vec::new();
    for a in asked {
        if seen.contains(&a.rule.id.as_str()) {
            continue;
        }
        seen.push(&a.rule.id);
        tail.push_str(&format!("[{}] {}\n", a.rule.id, a.rule.text));
        tail.push_str(&format!("  question: {}\n", a.rule.judgement.question));
        if let Some(criteria) = &a.rule.judgement.criteria {
            tail.push_str(&format!("  criteria: {criteria}\n"));
        }
    }

    // The budget is spent on the windows, never on the tail. The tail is what the judgement
    // reads the windows *against* — the rule's question and criteria, and the list that ties
    // each window to a question id — so cutting it would leave answers with nothing to answer;
    // and cutting the windows at the end, as a single tail cut does, would drop the code for
    // whichever candidates happened to be last. Both budgets therefore shrink the windows
    // around the candidates until they fit, and `truncate_state` stays as the bound for the one
    // case that cannot be met that way: a budget smaller than the candidate lines themselves.
    //
    // `max_state_bytes == 0` keeps its long-standing meaning here — no truncation at all —
    // rather than becoming "no room for anything". `max_state_lines == 0` is the floor, not an
    // empty state: it shows the candidate lines and nothing around them. Under the head, zero
    // showed nothing at all *and* the candidates were still asked about.
    let fixed = out.len() + tail.len();
    fit(&mut windows, &lines, cfg.max_state_lines, Metric::Lines);
    let room = if cfg.max_state_bytes == 0 {
        usize::MAX
    } else {
        cfg.max_state_bytes.saturating_sub(fixed)
    };
    fit(&mut windows, &lines, room, Metric::Bytes);

    for w in &windows {
        out.push_str(&w.render(&lines));
    }
    out.push_str(&tail);
    truncate_state(&out, cfg.max_state_bytes)
}

/// Cut the state at a line boundary, and say so in it.
///
/// Half a line is worse than no line: a model shown `x.unwr` may read a violation that is not
/// there. The note matters too — a state that stops mid-file without a word reads like a whole
/// file that happens to end early.
///
/// This is the bound of last resort now, not the mechanism: the windows are fitted to both
/// budgets before they are written, so reaching this means the candidate lines and the rules'
/// own prose did not fit on their own. It cuts the rules, at the end, in that case.
fn truncate_state(state: &str, max: usize) -> String {
    if max == 0 || state.len() <= max {
        return state.to_string();
    }
    let mut out = String::new();
    for line in state.split_inclusive('\n') {
        if out.len() + line.len() > max {
            break;
        }
        out.push_str(line);
    }
    out.push_str(&format!("\n[truncated at {max} bytes]\n"));
    out
}

/// The smallest span of whole lines starting at `line` that occurs exactly once in `text`.
///
/// The needle starts as the line from its first non-whitespace character to its end, and grows a
/// whole line at a time while it is ambiguous. Leading indentation is dropped so the anchor does
/// not depend on the indent being repeated, and trailing whitespace is dropped so a stray space
/// cannot make two otherwise identical lines look different.
fn needle_at(lines: &[&str], line: usize, text: &str) -> String {
    let mut end = line;
    loop {
        let needle = span(lines, line, end);
        if needle.is_empty() {
            // A blank line has nothing to anchor on. `findings::build` rejects an empty
            // needle; returning it as-is is how that rejection gets counted rather than
            // guessed around.
            return needle;
        }
        if text.match_indices(&needle).count() == 1 || end + 1 >= lines.len() || end - line + 1 >= MAX_SPAN
        {
            return needle;
        }
        end += 1;
    }
}

/// Lines `start..=end`, trimmed to start at the first non-whitespace character of the first line
/// and to end at the last non-whitespace character of the last.
fn span(lines: &[&str], start: usize, end: usize) -> String {
    let mut joined = String::new();
    for (i, line) in lines[start..=end].iter().enumerate() {
        if i == 0 {
            joined.push_str(line.trim_start());
        } else {
            joined.push('\n');
            joined.push_str(line);
        }
    }
    joined.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{Inspection, Judgement, Rule};

    fn rule(inspection: Inspection) -> Rule {
        Rule {
            id: "r".into(),
            title: "t".into(),
            text: "text".into(),
            severity: None,
            applies_to: vec!["**/*.rs".into()],
            inspection,
            judgement: Judgement {
                question: "q".into(),
                criteria: None,
                reasons: None,
                min_probability: None,
            },
            verb_hint: None,
            docs: None,
            source: crate::types::RuleSource::Repository,
        }
    }

    fn regex(pattern: &str, max_matches: Option<usize>) -> Rule {
        rule(Inspection::Regex {
            pattern: pattern.into(),
            max_matches,
        })
    }

    const SRC: &str = "fn a() {\n    x.unwrap();\n    y.unwrap();\n}\n";

    #[test]
    fn a_candidate_per_matching_line_with_a_locatable_needle() {
        let found = candidates(&regex(r"\.unwrap\(\)", None), SRC, 10);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].line, 1);
        assert_eq!(found[0].needle, "x.unwrap();");
        assert_eq!(found[1].line, 2);
        assert_eq!(found[1].needle, "y.unwrap();");
        // Both needles locate exactly once, which is what makes them usable anchors.
        for c in &found {
            assert_eq!(SRC.match_indices(&c.needle).count(), 1, "{:?}", c);
        }
    }

    #[test]
    fn the_cap_bounds_what_one_rule_may_ask() {
        let many = "x.unwrap();\n".repeat(50);
        let found = candidates(&regex(r"\.unwrap\(\)", None), &many, 3);
        assert_eq!(found.len(), 3, "the cap is what stops a generated file becoming 50 questions");
        assert_eq!(found[0].line, 0);
    }

    #[test]
    fn max_matches_reports_only_when_the_count_exceeds_it() {
        // `Some(2)` and exactly two matches: at the allowance, so nothing.
        let two = "x.unwrap();\ny.unwrap();\n";
        assert!(candidates(&regex(r"\.unwrap\(\)", Some(2)), two, 10).is_empty());
        // One over the allowance: reported.
        let three = "x.unwrap();\ny.unwrap();\nz.unwrap();\n";
        assert_eq!(candidates(&regex(r"\.unwrap\(\)", Some(2)), three, 10).len(), 3);
        // `Some(0)` means "any match".
        assert_eq!(candidates(&regex(r"\.unwrap\(\)", Some(0)), SRC, 10).len(), 2);
        assert!(candidates(&regex(r"\.unwrap\(\)", Some(0)), "fn a() {}\n", 10).is_empty());
    }

    #[test]
    fn absent_reports_the_head_only_when_the_pattern_is_missing() {
        let with = "// Copyright 2026\nfn a() {}\n";
        let without = "fn a() {}\n";
        let r = rule(Inspection::Absent {
            pattern: "// Copyright".into(),
            max_matches: None,
        });
        assert!(candidates(&r, with, 10).is_empty(), "present, so nothing to report");
        let found = candidates(&r, without, 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 0);
        assert_eq!(found[0].needle, "fn a() {}");
    }

    #[test]
    fn absent_yields_nothing_when_the_cap_is_zero() {
        let r = rule(Inspection::Absent {
            pattern: "// Copyright".into(),
            max_matches: None,
        });
        assert!(candidates(&r, "fn a() {}\n", 0).is_empty());
    }

    #[test]
    fn a_duplicated_line_grows_its_needle_until_it_locates_once() {
        // Two identical `x.unwrap();` lines: the single line locates twice, so the needle grows
        // to cover the following line and becomes unique.
        let text = "x.unwrap();\ny.unwrap();\nx.unwrap();\n";
        let found = candidates(&regex(r"\.unwrap\(\)", None), text, 10);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].needle, "x.unwrap();\ny.unwrap();");
        assert_eq!(text.match_indices(&found[0].needle).count(), 1);
        // The last one has nothing to grow into: it is returned as it is, and `build` rejects
        // it, which is reported rather than silently dropped.
        assert_eq!(found[2].line, 2);
        assert_eq!(found[2].needle, "x.unwrap();");
        assert_eq!(text.match_indices(&found[2].needle).count(), 2);
        // The middle line's own text is already unique.
        assert_eq!(found[1].needle, "y.unwrap();");
        assert_eq!(text.match_indices(&found[1].needle).count(), 1);
    }

    #[test]
    fn an_uncompilable_pattern_finds_nothing_rather_than_everything() {
        assert!(candidates(&regex("(", None), SRC, 10).is_empty());
    }

    #[test]
    fn pass_notes_name_the_three_ways_a_pass_reports_itself() {
        // Nothing at all: no files, and this build ships none either.
        let empty = RuleSet::default();
        let none = pass_notes(&empty, 0, "/w/a.md", "/w");
        assert_eq!(none.len(), 1, "{none:?}");
        assert_eq!(none[0].0, "no_rules");
        assert!(none[0].1.contains("no rules loaded from /w/.jev/rules"), "{}", none[0].1);
        assert!(none[0].1.contains("none shipped in this build"), "{}", none[0].1);
        assert!(none[0].1.contains("nothing to run"), "{}", none[0].1);

        // The shipped set exists and was switched off: a different sentence, because the reader
        // has a file to find rather than nothing to find.
        let off = RuleSet {
            shipped: 3,
            ..Default::default()
        };
        let switched = pass_notes(&off, 0, "/w/a.md", "/w");
        assert_eq!(switched[0].0, "no_rules");
        assert!(switched[0].1.contains("switched off"), "{}", switched[0].1);
        assert!(!switched[0].1.contains("none shipped"), "{}", switched[0].1);

        // Rules exist and none claims the file: the same code, a different sentence.
        let set = RuleSet {
            rules: vec![rule(Inspection::Regex {
                pattern: "x".into(),
                max_matches: None,
            })],
            ..Default::default()
        };
        let unclaimed = pass_notes(&set, 0, "/w/a.md", "/w");
        assert_eq!(unclaimed.len(), 1, "{unclaimed:?}");
        assert_eq!(unclaimed[0].0, "no_rules", "the same code, a different sentence");
        assert!(unclaimed[0].1.contains("no rule applies to /w/a.md"), "{}", unclaimed[0].1);
        assert!(unclaimed[0].1.contains("nothing to run"), "{}", unclaimed[0].1);
        assert!(
            pass_notes(&set, 1, "/w/a.md", "/w").is_empty(),
            "a rule that claims the file means there is something to run"
        );

        // The shipped set is carrying the pass: the reader is told where the rules came from,
        // and how to get them onto disk. This is the note that makes a fresh install and a
        // broken one different answers.
        let shipped_only = RuleSet {
            rules: vec![Rule {
                source: crate::types::RuleSource::Builtin,
                ..rule(Inspection::Regex {
                    pattern: "x".into(),
                    max_matches: None,
                })
            }],
            shipped: 1,
            ..Default::default()
        };
        let carried = pass_notes(&shipped_only, 1, "/w/a.rs", "/w");
        assert_eq!(carried.len(), 1, "{carried:?}");
        assert_eq!(carried[0].0, "default_rules");
        assert!(carried[0].1.contains("1 shipped rule(s)"), "{}", carried[0].1);
        assert!(carried[0].1.contains("jev rules init"), "{}", carried[0].1);
        assert!(!carried[0].1.contains("no rule applies"), "{}", carried[0].1);

        // A repository that has written rules of its own is not told about the shipped ones.
        assert!(pass_notes(&set, 1, "/w/a.rs", "/w").is_empty(), "{:?}", set.counts());
    }

    #[test]
    fn a_repository_with_no_rule_files_gets_its_findings_from_the_shipped_set() {
        // End to end through the rules pass, over the shipped-but-absent-from-disk input: no
        // `.jev/rules/` anywhere, so the only rules that can run are the shipped ones.
        let root = std::env::temp_dir().join(format!("jev-inspections-shipped-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let shipped: &[(&str, &str)] = &[(
            "code/shipped.json",
            r#"{"schema":"jev.rules/1","rules":[{"id":"shipped","title":"Shipped rule",
                "text":"A shipped convention.","severity":"warning","applies_to":["**/*.rs"],
                "inspection":{"kind":"regex","pattern":"\\.unwrap\\(\\)"},
                "judgement":{"question":"Is it a violation?","criteria":{"true":"yes","false":"no"},
                              "min_probability":0.75},
                "verb_hint":"fix"}]}"#,
        )];

        let cfg = crate::config::Config::default();
        let set = crate::rules::load(&root, cfg.rules.defaults, shipped);
        let (considered, asked) = select(&set.rules, "src/a.rs", SRC, 8);
        assert_eq!((considered, asked.len()), (1, 2), "{:?}", set.skipped);
        assert!(pass_notes(&set, considered, "/w/src/a.rs", "/w")
            .iter()
            .any(|(c, _)| c == "default_rules"));

        let request = request("/w/src/a.rs", SRC, &asked, &cfg.rules, &[]);
        assert_eq!(request.questions.len(), 2, "one question per candidate");
        let response = crate::decision::DecisionResponse {
            answers: ["shipped#1", "shipped#2"]
                .iter()
                .map(|id| crate::decision::DecisionAnswer {
                    id: id.to_string(),
                    value: crate::decision::DecisionValue::Bool(true),
                    probability: Some(0.9),
                    reason: None,
                })
                .collect(),
            input_tokens: 0,
            output_tokens: 0,
        };
        let built = resolve(
            SRC,
            &asked,
            &response,
            &crate::lang::profile("rust"),
            cfg.noise.max_visible_findings,
        );
        assert_eq!(built.findings.len(), 2, "{:?}", built.findings);
        assert!(built
            .findings
            .iter()
            .all(|f| f.rule_source == Some(crate::types::RuleSource::Builtin)));

        // And the setting, through the config path a client's `workspace/configuration` payload
        // takes, is the only thing between that and nothing at all.
        let off = cfg.merged_with(Some(&serde_json::json!({"rules": {"defaults": false}})));
        let set = crate::rules::load(&root, off.rules.defaults, shipped);
        assert!(set.rules.is_empty());
        let (considered, asked) = select(&set.rules, "src/a.rs", SRC, 8);
        assert_eq!((considered, asked.len()), (0, 0));
        assert_eq!(
            pass_notes(&set, 0, "/w/src/a.rs", "/w")
                .iter()
                .map(|(c, _)| c.as_str())
                .collect::<Vec<_>>(),
            vec!["no_rules"]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    // ---- what the decision is shown -----------------------------------------
    //
    // The state used to be the file head. Every test below fails on some way of going back to
    // that, or on a window rule that loses the thing a window is for.

    /// A document whose candidate lines read `x.unwrap(); // {n}` and whose every other line
    /// reads `let v{n} = {n};`, so a state can be searched for a line by number.
    fn padded(at: &[usize], total: usize) -> String {
        let mut out = String::new();
        for i in 0..total {
            if at.contains(&i) {
                out.push_str(&format!("    x.unwrap(); // {i}\n"));
            } else {
                out.push_str(&format!("let v{i} = {i};\n"));
            }
        }
        out
    }

    /// The state's numbered source lines, counted the way the state writes them: `<n>: <line>`.
    /// A candidate's row (`id rule line N: needle`) and the rules' prose (`  question: …`) do
    /// not count, which is what makes this a count of the *code* the decision was shown.
    fn numbered(state: &str) -> Vec<&str> {
        state
            .lines()
            .filter(|l| {
                l.split_once(": ")
                    .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            })
            .collect()
    }

    /// The assertion the whole change is for: a candidate the pass asks about is a line the
    /// state shows, at the number its id names.
    fn assert_every_candidate_is_shown(state: &str, asked: &[Asked], lines: &[&str]) {
        let shown: Vec<&str> = state.lines().collect();
        for a in asked {
            let line = a.candidate.line as usize;
            let want = format!("{line}: {}", lines[line]);
            assert!(
                shown.contains(&want.as_str()),
                "candidate {} was asked about a line the state does not show\n{state}",
                a.id()
            );
        }
    }

    #[test]
    fn a_candidate_near_the_head_and_one_at_line_2000_are_both_in_the_state() {
        let text = padded(&[2, 2000], 2400);
        let lines: Vec<&str> = text.split('\n').collect();
        let rule = regex(r"\.unwrap\(\)", None);
        let (_, asked) = select(std::slice::from_ref(&rule), "/w/a.rs", &text, 8);
        assert_eq!(asked.len(), 2);
        let cfg = RulesConfig::default();

        let req = request("/w/a.rs", &text, &asked, &cfg, &[]);
        assert_every_candidate_is_shown(&req.state, &asked, &lines);
        // And it is not the head: line 1,200 is nowhere near either candidate, in a file whose
        // head would have carried it. This is the assertion that fails if the window rule is ever the head again.
        assert!(
            !req.state.contains("1200: let v1200 = 1200;"),
            "the state is showing the head again:\n{}",
            req.state
        );
        assert!(
            numbered(&req.state).len() <= cfg.max_state_lines,
            "{} lines of code, budget {}",
            numbered(&req.state).len(),
            cfg.max_state_lines
        );
    }

    #[test]
    fn the_real_document_the_review_measured_shows_every_candidate() {
        // `crates/jev-lsp/src/server.rs` — 2,805 lines and thirteen candidates from this
        // repository's own rules, from line 243 to line 2518. Under the head, none of the
        // thirteen was inside the state it was asked about; this asserts the pair the review
        // used, not a fixture that copies its shape.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let Ok(text) = std::fs::read_to_string(root.join("crates/jev-lsp/src/server.rs")) else {
            // A packaged crate has no `crates/jev-lsp` beside it. The workspace is what this is
            // about.
            return;
        };
        let lines: Vec<&str> = text.split('\n').collect();
        let cfg = crate::config::Config::default();
        let set = rules::load(&root, true, crate::rules::builtin_files());
        let (_, asked) = select(
            &set.rules,
            "crates/jev-lsp/src/server.rs",
            &text,
            cfg.rules.max_candidates_per_rule,
        );
        assert!(
            asked.len() >= 5,
            "this test is about a document its own rules have things to say about: {}",
            asked.len()
        );
        let req = request(
            "crates/jev-lsp/src/server.rs",
            &text,
            &asked,
            &cfg.rules,
            &[],
        );
        assert_every_candidate_is_shown(&req.state, &asked, &lines);
        // The point of the measurement: at least one candidate is past the line the head would
        // have stopped at, so a head-shaped state could not have shown them all.
        assert!(
            asked
                .iter()
                .any(|a| a.candidate.line >= cfg.rules.max_state_lines as u32),
            "every candidate is inside the head — this document no longer demonstrates anything"
        );
    }

    #[test]
    fn a_declaration_the_client_sent_is_the_window_the_candidate_is_read_in() {
        let text = padded(&[10], 40);
        let lines: Vec<&str> = text.split('\n').collect();
        let rule = regex(r"\.unwrap\(\)", None);
        let (_, asked) = select(std::slice::from_ref(&rule), "/w/a.rs", &text, 8);
        let cfg = RulesConfig::default();
        let defs = [LineRange {
            start_line: 6,
            end_line: 14,
        }];

        let with = request("/w/a.rs", &text, &asked, &cfg, &defs);
        assert!(
            with.state.contains("[lines 6-14]"),
            "the declaration is the window:\n{}",
            with.state
        );
        assert_every_candidate_is_shown(&with.state, &asked, &lines);
        assert!(
            !with.state.contains("5: let v5 = 5;") && !with.state.contains("0: let v0 = 0;"),
            "nothing outside the declaration is shown:\n{}",
            with.state
        );

        // The client sent nothing — `jev inspect`, or a client with no parser for the language.
        // Then the window is the neighbourhood, which reaches wider than the declaration did.
        let without = request("/w/a.rs", &text, &asked, &cfg, &[]);
        assert!(without.state.contains("0: let v0 = 0;"), "{}", without.state);
        assert_every_candidate_is_shown(&without.state, &asked, &lines);
        assert_ne!(
            with.state, without.state,
            "the definitions are an input to the state, which is why they are in the key"
        );

        // The *smallest* enclosing declaration wins when the client sent several.
        let nested = [
            LineRange {
                start_line: 0,
                end_line: 39,
            },
            LineRange {
                start_line: 6,
                end_line: 14,
            },
        ];
        let inner = request("/w/a.rs", &text, &asked, &cfg, &nested);
        assert_eq!(inner.state, with.state, "the innermost declaration is the window");
    }

    #[test]
    fn candidates_that_share_context_send_it_once() {
        let text = padded(&[100, 103, 106], 300);
        let rule = regex(r"\.unwrap\(\)", None);
        let (_, asked) = select(std::slice::from_ref(&rule), "/w/a.rs", &text, 8);
        let req = request("/w/a.rs", &text, &asked, &RulesConfig::default(), &[]);
        assert_eq!(
            req.state.matches("101: let v101 = 101;").count(),
            1,
            "shared context, sent once:\n{}",
            req.state
        );
        assert_eq!(
            req.state.matches("[lines ").count(),
            1,
            "three candidates inside one region are one window:\n{}",
            req.state
        );
    }

    #[test]
    fn line_numbers_stay_absolute_across_the_gap_between_windows() {
        let text = padded(&[10, 900], 1200);
        let lines: Vec<&str> = text.split('\n').collect();
        let rule = regex(r"\.unwrap\(\)", None);
        let (_, asked) = select(std::slice::from_ref(&rule), "/w/a.rs", &text, 8);
        let req = request("/w/a.rs", &text, &asked, &RulesConfig::default(), &[]);
        assert!(
            req.state.contains("[lines 880-920]"),
            "the second window names its own place in the file:\n{}",
            req.state
        );
        assert!(
            req.state.contains(&format!("900: {}", lines[900])),
            "and counts from the file's zero, not from its own:\n{}",
            req.state
        );
        // The block after the gap starts at its own number: a state that renumbered each window
        // from zero would open the second one at `0:`, and searching the whole state for a
        // string like `0: let v880` is not the question — `880: let v880 = 880;` contains it.
        let after = req
            .state
            .split("[lines 880-920]\n")
            .nth(1)
            .expect("the second window is in the state");
        assert!(
            after.starts_with("880: "),
            "the numbering restarted at the second window:\n{after}"
        );
    }

    #[test]
    fn the_line_budget_bounds_what_is_shown_when_the_windows_exceed_it() {
        let at: Vec<usize> = (0..8).map(|i| 100 + i * 60).collect();
        let text = padded(&at, 1200);
        let rule = regex(r"\.unwrap\(\)", None);
        let (_, asked) = select(std::slice::from_ref(&rule), "/w/a.rs", &text, 8);
        assert_eq!(asked.len(), 8);
        let lines: Vec<&str> = text.split('\n').collect();

        let cfg = RulesConfig::default();
        let req = request("/w/a.rs", &text, &asked, &cfg, &[]);
        assert!(
            numbered(&req.state).len() <= cfg.max_state_lines,
            "{} lines of code against a budget of {}:\n{}",
            numbered(&req.state).len(),
            cfg.max_state_lines,
            req.state
        );
        // The budget is met by showing less *around* each candidate, never by dropping one.
        assert_every_candidate_is_shown(&req.state, &asked, &lines);
    }

    #[test]
    fn the_byte_budget_bounds_a_merged_set_without_dropping_a_candidate() {
        // Eight neighbourhoods are more than 2,000 bytes of a document this size, and two of
        // them are close enough to merge. Both budgets shrink the windows; neither drops a
        // candidate.
        let at = [100usize, 134];
        let text = padded(&at, 400);
        let lines: Vec<&str> = text.split('\n').collect();
        let rule = regex(r"\.unwrap\(\)", None);
        let (_, asked) = select(std::slice::from_ref(&rule), "/w/a.rs", &text, 8);
        assert_eq!(asked.len(), 2, "two candidates, 34 lines apart");

        let cfg = RulesConfig {
            max_state_bytes: 900,
            ..RulesConfig::default()
        };
        let req = request("/w/a.rs", &text, &asked, &cfg, &[]);
        assert!(
            req.state.len() <= cfg.max_state_bytes,
            "{} bytes against a budget of {}:\n{}",
            req.state.len(),
            cfg.max_state_bytes,
            req.state
        );
        assert_every_candidate_is_shown(&req.state, &asked, &lines);
        // The two candidates are one window when there is room and two when there is not: the
        // widest gap between them is where it is cut, so each keeps its own neighbourhood.
        assert!(
            req.state.matches("[lines ").count() >= 2,
            "a merged window too large to fit was not split:\n{}",
            req.state
        );

        // Room for it, and the merge stands.
        let roomy = RulesConfig::default();
        let merged = request("/w/a.rs", &text, &asked, &roomy, &[]);
        assert_eq!(
            merged.state.matches("[lines ").count(),
            1,
            "with room, the shared context is sent once:\n{}",
            merged.state
        );
    }

    #[test]
    fn a_window_measures_exactly_what_it_renders() {
        let text = padded(&[3, 9], 40);
        let lines: Vec<&str> = text.split('\n').collect();
        for w in [
            Window {
                start: 0,
                end: 0,
                keep: vec![0],
            },
            Window {
                start: 2,
                end: 12,
                keep: vec![3, 9],
            },
            Window {
                start: 38,
                end: 39,
                keep: vec![39],
            },
        ] {
            assert_eq!(w.bytes(&lines), w.render(&lines).len(), "{w:?}");
        }
        // The digit arithmetic has to hold across a decade boundary, which is where a
        // hand-counted marker length goes wrong.
        let big = padded(&[1000], 1100);
        let big_lines: Vec<&str> = big.split('\n').collect();
        let w = Window {
            start: 995,
            end: 1005,
            keep: vec![1000],
        };
        assert_eq!(w.bytes(&big_lines), w.render(&big_lines).len());
    }

    #[test]
    fn a_window_covers_its_candidate_and_grows_outward_not_inward() {
        let lines: Vec<&str> = (0..100).map(|_| "x").collect();
        let mut w = Window {
            start: 40,
            end: 60,
            keep: vec![50],
        };
        assert_eq!(w.lines(), 21);
        w.trim_one();
        assert_eq!((w.start, w.end), (41, 60), "the longer side gives way first");
        w.trim_one();
        assert_eq!((w.start, w.end), (41, 59));
        // Down to the candidate itself and no further: a window's floor is the line the
        // question is about.
        while w.can_trim() {
            w.trim_one();
        }
        assert_eq!((w.start, w.end), (50, 50));
        assert_eq!(w.render(&lines), "[lines 50-50]\n50: x\n");
    }

    #[test]
    fn a_split_keeps_every_candidate_and_orders_the_gap_between_them() {
        let w = Window {
            start: 80,
            end: 160,
            keep: vec![100, 140],
        };
        let (left, right) = w.split_widest().expect("two candidates, one gap");
        assert_eq!((left.start, left.end), (80, 120));
        assert_eq!((right.start, right.end), (121, 160));
        assert_eq!(left.keep, vec![100]);
        assert_eq!(right.keep, vec![140]);
        assert!(Window {
            start: 0,
            end: 10,
            keep: vec![5]
        }
        .split_widest()
        .is_none());
    }

    #[test]
    fn a_blank_line_has_no_needle_and_is_left_to_be_rejected() {
        let text = "fn a() {}\n\n   \n";
        let found = candidates(&regex(r"^\s*$", None), text, 10);
        assert_eq!(found.len(), 3, "every blank line matches");
        assert!(found.iter().any(|c| c.needle.is_empty()));
    }
}
