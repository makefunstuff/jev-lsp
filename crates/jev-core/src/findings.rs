//! Turning the review tier's anchored findings into positioned diagnostics.
//!
//! Same principle as edits: the model quotes text, the server computes where it is. A
//! finding whose quote cannot be located unambiguously is dropped, never guessed at.

use crate::contract::{parse_severity, RawFindings};
use crate::lang::Profile;
use crate::scope::line_len;
use crate::types::{Finding, Severity, Verb};
use sha2::{Digest, Sha256};

/// Findings plus a count of the ones that were discarded for being unusable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingBuild {
    pub findings: Vec<Finding>,
    pub rejected: usize,
}

const MAX_LABEL: usize = 60;
const MAX_DETAIL: usize = 300;

fn clip(s: &str, max: usize) -> String {
    let cleaned = s.replace(['\n', '\r'], " ");
    let trimmed = cleaned.trim();
    let mut out: String = trimmed.chars().take(max).collect();
    if trimmed.chars().count() > max {
        out.push('…');
    }
    out
}

/// Stable across analyses of unchanged content, which is what makes dismissal work.
fn finding_id(label: &str, detail: &str, line: u32, col: u32) -> String {
    let mut h = Sha256::new();
    h.update(label.as_bytes());
    h.update(b"\x00");
    h.update(detail.as_bytes());
    h.update(line.to_le_bytes());
    h.update(col.to_le_bytes());
    let d = h.finalize();
    d.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// The one place the visible finding set is finalised.
///
/// `max_findings` is the floor: every surface (diagnostics, code lens, inlay hints) reads the
/// result of this function, so the same file cannot report different counts depending on where
/// you look. Warnings outrank information for the budget; within a severity the line order the
/// analysis produced is kept.
pub fn build(
    text: &str,
    raw: &RawFindings,
    _profile: &Profile,
    max_findings: usize,
) -> FindingBuild {
    let mut findings = Vec::new();
    let mut rejected = 0usize;

    for r in &raw.findings {
        let needle = r.anchor.needle.as_str();
        if needle.is_empty() {
            rejected += 1;
            continue;
        }
        let matches: Vec<usize> = text.match_indices(needle).map(|(i, _)| i).collect();
        if matches.len() != 1 {
            rejected += 1;
            continue;
        }
        let offset = matches[0];
        let line = text[..offset].bytes().filter(|b| *b == b'\n').count() as u32;
        let line_start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let start_col = (offset - line_start) as u32;
        let needle_first_line = needle.split('\n').next().unwrap_or(needle);
        let end_col = (start_col + needle_first_line.chars().count() as u32)
            .min(line_len(text, line))
            .max(start_col);

        let label = clip(&r.label, MAX_LABEL);
        if label.is_empty() {
            rejected += 1;
            continue;
        }
        let detail = clip(r.detail.as_deref().unwrap_or(""), MAX_DETAIL);
        let verb_hint = r
            .verb_hint
            .as_deref()
            .and_then(Verb::parse)
            .unwrap_or(Verb::Fix);

        findings.push(Finding {
            id: finding_id(&label, &detail, line, start_col),
            line,
            start_col,
            end_col,
            severity: parse_severity(r.severity.as_deref()),
            label,
            detail,
            verb_hint,
        });
    }

    findings.sort_by_key(|f| (f.line, f.start_col));
    findings.dedup_by(|a, b| a.id == b.id);
    findings.sort_by_key(|f| match f.severity {
        Severity::Warning => 0u8,
        Severity::Information => 1u8,
    });
    findings.truncate(max_findings);

    FindingBuild { findings, rejected }
}

/// One finding as every Result-shaped surface prints it.
///
/// Shared rather than written twice: the CLI and the language server answer the same question
/// about the same document, and a field that drifts between them is a parity bug nobody notices
/// until a client reads the one that is wrong.
pub fn finding_json(f: &Finding) -> serde_json::Value {
    serde_json::json!({
        "id": f.id,
        "line": f.line,
        "start_col": f.start_col,
        "end_col": f.end_col,
        "severity": match f.severity {
            Severity::Warning => "warning",
            Severity::Information => "information",
        },
        "label": f.label,
        "detail": f.detail,
        "verb": f.verb_hint.as_str(),
    })
}

/// The payload both front ends wrap into their `jev.inspect` result.
///
/// The counts are part of it: a pass that examined nothing and a pass that examined twenty lines
/// and agreed with them are different answers.
///
/// `skipped` is a list of `{code, detail}`:
///
/// * a rule file that could not be read — `code` is its path, `detail` the reason;
/// * `"unchanged"` — the document git reports as untouched, `detail` its path;
/// * `"no_rules"` — the pass had nothing to run, `detail` the sentence a user needs to read.
///
/// One field order for all three, so a consumer reads a code and a sentence, never a shape it
/// has to guess at.
pub fn inspect_fields(
    findings: &[Finding],
    considered: usize,
    candidates: usize,
    skipped: &[(String, String)],
) -> serde_json::Value {
    serde_json::json!({
        "findings": findings.iter().map(finding_json).collect::<Vec<_>>(),
        "considered": considered,
        "candidates": candidates,
        "skipped": skipped
            .iter()
            .map(|(code, detail)| serde_json::json!({"code": code, "detail": detail}))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::parse_findings;
    use crate::lang::profile;
    use crate::types::Severity;

    const DOC: &str = "fn a() {\n    let f = File::open(p)?;\n}\n";

    fn one(src: &str, text: &str) -> FindingBuild {
        build(text, &parse_findings(src).unwrap(), &profile("rust"), usize::MAX)
    }

    /// The floor keeps warnings and drops the rest, and it is the only place that decides.
    #[test]
    fn the_cap_keeps_warnings_over_information_and_truncates() {
        let src = r#"{"findings":[
            {"anchor":{"match":"File::open(p)"},"label":"info","severity":"information"},
            {"anchor":{"match":"fn a()"},"label":"warn","severity":"warning"},
            {"anchor":{"match":"let f"},"label":"warn2","severity":"warning"}]}"#;
        let raw = parse_findings(src).unwrap();
        let b = build(DOC, &raw, &profile("rust"), 2);
        assert_eq!(b.findings.len(), 2);
        assert_eq!(b.findings[0].label, "warn");
        assert_eq!(b.findings[1].label, "warn2");
        assert!(b.findings.iter().all(|f| f.severity == Severity::Warning));
        // Within a severity the analysis' line order survives the cap.
        assert!(b.findings[0].line <= b.findings[1].line);
        let none = build(DOC, &raw, &profile("rust"), 0);
        assert!(none.findings.is_empty());
    }

    #[test]
    fn a_located_anchor_becomes_a_positioned_finding() {
        let b = one(
            r#"{"findings":[{"anchor":{"match":"File::open(p)"},"label":"unchecked","detail":"d"}]}"#,
            DOC,
        );
        assert_eq!(b.rejected, 0);
        assert_eq!(b.findings.len(), 1);
        let f = &b.findings[0];
        assert_eq!(f.line, 1);
        assert_eq!(f.start_col, 12);
        assert_eq!(f.end_col, 12 + "File::open(p)".len() as u32);
        assert_eq!(f.severity, Severity::Warning);
        assert_eq!(f.verb_hint, Verb::Fix);
    }

    #[test]
    fn unlocatable_and_ambiguous_anchors_are_dropped_not_guessed() {
        let b = one(r#"{"findings":[{"anchor":{"match":"nope"},"label":"x"}]}"#, DOC);
        assert_eq!((b.findings.len(), b.rejected), (0, 1));
        let doc = "a b\na b\n";
        let b2 = one(r#"{"findings":[{"anchor":{"match":"a b"},"label":"x"}]}"#, doc);
        assert_eq!((b2.findings.len(), b2.rejected), (0, 1));
    }

    #[test]
    fn severity_is_never_error_and_defaults_to_warning() {
        let b = one(
            r#"{"findings":[{"anchor":{"match":"File::open(p)"},"label":"a","severity":"error"}]}"#,
            DOC,
        );
        assert_eq!(b.findings[0].severity, Severity::Warning);
    }

    #[test]
    fn ids_are_stable_for_unchanged_content_and_shift_with_position() {
        let src = r#"{"findings":[{"anchor":{"match":"File::open(p)"},"label":"l","detail":"d"}]}"#;
        let a = one(src, DOC).findings[0].id.clone();
        let b = one(src, DOC).findings[0].id.clone();
        assert_eq!(a, b, "dismissal depends on this");
        let moved = one(src, "\n\n\nfn a() {\n    let f = File::open(p)?;\n}\n")
            .findings[0]
            .id
            .clone();
        assert_ne!(a, moved);
    }

    #[test]
    fn long_labels_and_details_are_clipped() {
        let long_label = "x".repeat(200);
        let src = format!(
            r#"{{"findings":[{{"anchor":{{"match":"File::open(p)"}},"label":"{long_label}","detail":"y"}}]}}"#
        );
        let b = one(&src, DOC);
        assert!(b.findings[0].label.chars().count() <= MAX_LABEL + 1);
    }

    #[test]
    fn a_blank_label_is_rejected() {
        let b = one(
            r#"{"findings":[{"anchor":{"match":"File::open(p)"},"label":"   "}]}"#,
            DOC,
        );
        assert_eq!((b.findings.len(), b.rejected), (0, 1));
    }

    #[test]
    fn findings_are_sorted_and_deduplicated() {
        let src = r#"{"findings":[
            {"anchor":{"match":"File::open(p)"},"label":"b","detail":"d"},
            {"anchor":{"match":"File::open(p)"},"label":"b","detail":"d"},
            {"anchor":{"match":"fn a()"},"label":"a","detail":"d"}]}"#;
        let b = one(src, DOC);
        assert_eq!(b.findings.len(), 2);
        assert!(b.findings[0].line <= b.findings[1].line);
    }

    #[test]
    fn the_shared_result_shape_names_every_finding_field_and_every_count() {
        let b = one(
            r#"{"findings":[{"anchor":{"match":"File::open(p)"},"label":"l","detail":"d"}]}"#,
            DOC,
        );
        let body = inspect_fields(
            &b.findings,
            3,
            7,
            &[("a.json".to_string(), "is not a rules document".to_string())],
        );
        assert_eq!(body["considered"], 3);
        assert_eq!(body["candidates"], 7);
        assert_eq!(body["skipped"][0]["code"], "a.json");
        assert_eq!(body["skipped"][0]["detail"], "is not a rules document");
        let f = &body["findings"][0];
        assert_eq!(f["label"], "l");
        assert_eq!(f["severity"], "warning");
        assert_eq!(f["verb"], "fix");
        assert_eq!(f["line"], 1);
        assert!(f["id"].as_str().is_some_and(|s| !s.is_empty()));
    }
}
