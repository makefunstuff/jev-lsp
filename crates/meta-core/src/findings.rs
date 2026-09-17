//! Turning the review tier's anchored findings into positioned diagnostics.
//!
//! Same principle as edits: the model quotes text, the server computes where it is. A
//! finding whose quote cannot be located unambiguously is dropped, never guessed at.

use crate::contract::{parse_severity, RawFindings};
use crate::lang::Profile;
use crate::scope::line_len;
use crate::types::{Finding, Verb};
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

pub fn build(text: &str, raw: &RawFindings, _profile: &Profile) -> FindingBuild {
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

    FindingBuild { findings, rejected }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::parse_findings;
    use crate::lang::profile;
    use crate::types::Severity;

    const DOC: &str = "fn a() {\n    let f = File::open(p)?;\n}\n";

    fn one(src: &str, text: &str) -> FindingBuild {
        build(text, &parse_findings(src).unwrap(), &profile("rust"))
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
}
