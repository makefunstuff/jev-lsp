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
//! `jev inspect` must produce the same findings from the same rules, and the only way to
//! guarantee that is for there to be one implementation of it.

use crate::config::RulesConfig;
use crate::contract::{RawAnchor, RawFinding, RawFindings};
use crate::decision::{DecisionQuestion, DecisionRequest, DecisionResponse, DecisionValue, QuestionKind};
use crate::findings::{self, FindingBuild};
use crate::gates;
use crate::lang::Profile;
use crate::rules::{self, Inspection, Rule, RuleSet};
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

/// The skip a pass owes a document when it has nothing to run, if that is the case.
///
/// This is the case the ambient demotion creates: rules are the ambient path, and a repository
/// that has written none gets no ambient findings — the chat review does not step in to fill the
/// gap. What it must not be is *silence*: "no findings" and "nothing was inspected" look
/// identical from a sign column, and only one of them is a bug.
///
/// It lives here, beside the pass it describes, so the language server and `jev inspect` cannot
/// drift: one implementation, called by both shells. A front end that reports nothing while the
/// other reports the reason is a parity failure, and this function exists because that happened.
pub fn nothing_to_run(
    rules: &RuleSet,
    considered: usize,
    doc_path: &str,
    root: &str,
) -> Option<(String, String)> {
    let dir = std::path::Path::new(root).join(rules::DIR).display().to_string();
    if rules.rules.is_empty() {
        return Some((
            "no_rules".to_string(),
            format!("no rules loaded from {dir} — the ambient pass has nothing to run"),
        ));
    }
    if considered == 0 {
        return Some((
            "no_rules".to_string(),
            format!(
                "no rule applies to {doc_path} ({} loaded from {dir}) — the ambient pass has nothing to run",
                rules.rules.len()
            ),
        ));
    }
    None
}

/// The request the whole candidate set is asked in. One call, one document.
pub fn request(path: &str, text: &str, asked: &[Asked], cfg: &RulesConfig) -> DecisionRequest {
    DecisionRequest {
        state: state(path, text, asked, cfg),
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
        });
    }
    findings::build(text, &raw, profile, max_findings)
}

/// What the decision is shown for one document.
///
/// Three parts, in the order they are read: the file head, numbered from zero so the numbers
/// match the candidate ids; the candidates, each naming the rule and the line it came from; and
/// the rules themselves, so the judgement has its own prose and its criteria in front of it
/// rather than a paraphrase of them.
fn state(doc_path: &str, text: &str, asked: &[Asked], cfg: &RulesConfig) -> String {
    let mut out = String::new();
    out.push_str(&format!("FILE {doc_path}\n"));
    out.push_str("line numbers below are 0-based, as in the candidate list\n");
    for (i, line) in text.lines().take(cfg.max_state_lines).enumerate() {
        out.push_str(&format!("{i}: {line}\n"));
    }

    out.push_str("\nCANDIDATES\n");
    for a in asked {
        out.push_str(&format!(
            "{} {} line {}: {}\n",
            a.id(),
            a.rule.id,
            a.candidate.line,
            a.candidate.needle
        ));
    }

    out.push_str("\nRULES\n");
    let mut seen: Vec<&str> = Vec::new();
    for a in asked {
        if seen.contains(&a.rule.id.as_str()) {
            continue;
        }
        seen.push(&a.rule.id);
        out.push_str(&format!("[{}] {}\n", a.rule.id, a.rule.text));
        out.push_str(&format!("  question: {}\n", a.rule.judgement.question));
        if let Some(criteria) = &a.rule.judgement.criteria {
            out.push_str(&format!("  criteria: {criteria}\n"));
        }
    }
    truncate_state(&out, cfg.max_state_bytes)
}

/// Cut the state at a line boundary, and say so in it.
///
/// Half a line is worse than no line: a model shown `x.unwr` may read a violation that is not
/// there. The note matters too — a state that stops mid-file without a word reads like a whole
/// file that happens to end early.
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
    fn nothing_to_run_names_the_two_ways_a_pass_can_have_nothing_to_do() {
        let empty = RuleSet::default();
        let none = nothing_to_run(&empty, 0, "/w/a.md", "/w").unwrap();
        assert_eq!(none.0, "no_rules");
        assert!(none.1.contains("no rules loaded from /w/.jev/rules"), "{}", none.1);
        assert!(none.1.contains("nothing to run"), "{}", none.1);

        let set = RuleSet {
            rules: vec![rule(Inspection::Regex {
                pattern: "x".into(),
                max_matches: None,
            })],
            ..Default::default()
        };
        let unclaimed = nothing_to_run(&set, 0, "/w/a.md", "/w").unwrap();
        assert_eq!(unclaimed.0, "no_rules", "the same code, a different sentence");
        assert!(unclaimed.1.contains("no rule applies to /w/a.md"), "{}", unclaimed.1);
        assert!(unclaimed.1.contains("nothing to run"), "{}", unclaimed.1);
        assert!(
            nothing_to_run(&set, 1, "/w/a.md", "/w").is_none(),
            "a rule that claims the file means there is something to run"
        );
    }

    #[test]
    fn a_blank_line_has_no_needle_and_is_left_to_be_rejected() {
        let text = "fn a() {}\n\n   \n";
        let found = candidates(&regex(r"^\s*$", None), text, 10);
        assert_eq!(found.len(), 3, "every blank line matches");
        assert!(found.iter().any(|c| c.needle.is_empty()));
    }
}
