//! Turning a plan response into a validated plan artifact (PROTOCOL.md §7).
//!
//! Same discipline as edits and findings: the model names work in terms of text, the server
//! resolves that text to a position in the document, and a step whose target cannot be
//! located is dropped rather than guessed at. A plan is deliberately free of edits — steps
//! are applied later, against whatever the file contains then.

use crate::contract::RawPlan;
use crate::types::{Plan, PlanStep, PlanTarget, StepStatus, Usage, Verb};
use sha2::{Digest, Sha256};

/// A plan plus a count of the steps that were discarded for naming something unlocatable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanBuild {
    pub plan: Plan,
    pub rejected: usize,
}

fn line_of_offset(text: &str, offset: usize) -> u32 {
    text[..offset].bytes().filter(|b| *b == b'\n').count() as u32
}

/// Locate a literal quote. Returns `None` when it is absent or ambiguous, which is the same
/// standard the edit path applies.
fn locate(text: &str, needle: &str) -> Option<u32> {
    let mut hits = text.match_indices(needle);
    let (offset, _) = hits.next()?;
    if hits.next().is_some() {
        return None;
    }
    Some(line_of_offset(text, offset))
}

fn plan_id(goal: &str, uri: &str, steps: &[PlanStep]) -> String {
    let mut h = Sha256::new();
    h.update(goal.as_bytes());
    h.update(b"\x00");
    h.update(uri.as_bytes());
    for s in steps {
        h.update(b"\x00");
        h.update(s.title.as_bytes());
        h.update(s.verb.as_str().as_bytes());
    }
    let d = h.finalize();
    d.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// Build a plan from a response, resolving every step's anchors against the document.
pub fn build(
    uri: &str,
    version: i32,
    goal: &str,
    language: &str,
    text: &str,
    raw: &RawPlan,
    usage: Usage,
) -> PlanBuild {
    let mut steps = Vec::new();
    let mut rejected = 0usize;

    for r in &raw.steps {
        let title = r.title.trim().to_string();
        if title.is_empty() {
            rejected += 1;
            continue;
        }
        let mut targets = Vec::new();
        for anchor in &r.anchors {
            if anchor.needle.trim().is_empty() {
                continue;
            }
            match locate(text, &anchor.needle) {
                Some(line) => targets.push(PlanTarget {
                    uri: uri.to_string(),
                    version,
                    line,
                    match_text: anchor.needle.clone(),
                }),
                None => rejected += 1,
            }
        }
        if targets.is_empty() {
            // A step with no locatable target cannot be applied, so it is not a step.
            rejected += 1;
            continue;
        }
        steps.push(PlanStep {
            n: steps.len() as u32 + 1,
            title,
            rationale: r.rationale.clone().unwrap_or_default(),
            verb: r.verb.as_deref().and_then(Verb::parse).unwrap_or(Verb::Rewrite),
            targets,
            status: StepStatus::Proposed,
        });
    }

    let id = plan_id(goal, uri, &steps);
    PlanBuild {
        plan: Plan {
            id,
            // The user's goal is authoritative. A model restatement is a clarification at
            // best and drift at worst, and the artifact is what the user will read back.
            goal: goal.to_string(),
            language: language.to_string(),
            steps,
            usage,
        },
        rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::parse_plan;

    const DOC: &str = "def load(path):\n    f = open(path)\n    return f\n";

    fn build_from(src: &str) -> PlanBuild {
        let raw = parse_plan(src).unwrap();
        build(
            "file:///tmp/a.py",
            7,
            "make it fail loudly",
            "python",
            DOC,
            &raw,
            Usage::default(),
        )
    }

    #[test]
    fn steps_are_numbered_and_their_anchors_resolved() {
        let b = build_from(
            r#"{"goal":"g","steps":[
                {"title":"guard the open","rationale":"r","verb":"harden","anchors":[{"match":"    f = open(path)"}]},
                {"title":"add a test","verb":"test","anchors":[{"match":"def load(path):"}]}]}"#,
        );
        assert_eq!(b.rejected, 0);
        assert_eq!(b.plan.steps.len(), 2);
        assert_eq!(b.plan.steps[0].n, 1);
        assert_eq!(b.plan.steps[1].n, 2);
        assert_eq!(b.plan.steps[0].verb, Verb::Harden);
        assert_eq!(b.plan.steps[0].targets[0].line, 1);
        assert_eq!(b.plan.steps[1].targets[0].line, 0);
        assert!(b.plan.steps.iter().all(|s| s.status == StepStatus::Proposed));
    }

    #[test]
    fn a_step_whose_target_cannot_be_located_is_dropped_not_guessed() {
        let b = build_from(
            r#"{"steps":[
                {"title":"real","verb":"harden","anchors":[{"match":"    f = open(path)"}]},
                {"title":"imaginary","verb":"docs","anchors":[{"match":"nowhere in this file"}]}]}"#,
        );
        assert_eq!(b.plan.steps.len(), 1);
        assert_eq!(b.plan.steps[0].title, "real");
        assert!(b.rejected >= 1);
    }

    #[test]
    fn an_ambiguous_anchor_is_not_a_target() {
        let doc = "x\ny\nx\n";
        let raw = parse_plan(r#"{"steps":[{"title":"t","verb":"harden","anchors":[{"match":"x"}]}]}"#).unwrap();
        let b = build("file:///a", 1, "g", "unknown", doc, &raw, Usage::default());
        assert!(b.plan.steps.is_empty(), "an ambiguous quote locates nothing");
        assert_eq!(b.rejected, 2, "one ambiguous anchor plus the empty step");
    }

    #[test]
    fn a_step_with_no_usable_anchor_is_not_a_step() {
        let b = build_from(r#"{"steps":[{"title":"vague","verb":"harden","anchors":[]}]}"#);
        assert!(b.plan.steps.is_empty());
        assert_eq!(b.rejected, 1);
    }

    #[test]
    fn an_unknown_verb_falls_back_instead_of_failing_the_plan() {
        let b = build_from(
            r#"{"steps":[{"title":"t","verb":"teleport","anchors":[{"match":"def load(path):"}]}]}"#,
        );
        assert_eq!(b.plan.steps[0].verb, Verb::Rewrite);
    }

    #[test]
    fn ids_are_stable_for_the_same_plan_and_move_with_it() {
        let src = r#"{"goal":"g","steps":[{"title":"a","verb":"harden","anchors":[{"match":"def load(path):"}]}]}"#;
        let a = build_from(src).plan.id;
        let b = build_from(src).plan.id;
        assert_eq!(a, b, "a plan the user is looking at must keep its id");
        let other = build_from(
            r#"{"goal":"g","steps":[{"title":"different","verb":"harden","anchors":[{"match":"def load(path):"}]}]}"#,
        )
        .plan
        .id;
        assert_ne!(a, other);
    }

    #[test]
    fn the_goal_is_the_users_not_the_models_restatement() {
        // A restatement is a clarification at best and drift at worst; the artifact is what
        // the user reads back, so it must say what they asked for.
        let b = build_from(
            r#"{"goal":"a different goal the model preferred","steps":[{"title":"t","verb":"harden","anchors":[{"match":"def load(path):"}]}]}"#,
        );
        assert_eq!(b.plan.goal, "make it fail loudly");
    }

    #[test]
    fn nothing_is_lost_when_every_step_is_good() {
        let b = build_from(
            r#"{"steps":[{"title":"a","verb":"harden","anchors":[{"match":"def load(path):"}]}]}"#,
        );
        assert_eq!(b.rejected, 0);
        assert_eq!(b.plan.language, "python");
    }
}
