//! Context assembly (docs/MODEL.md §3).
//!
//! Deterministic, ordered, and bounded. The model never chooses what it sees; the slices
//! are fixed by the verb and the scope, and the assembled text is hashed into the cache
//! key, so a hit means the model would have received byte-identical input.

use crate::document::Document;
use crate::scope::Resolved;
use crate::types::Finding;

/// Lines of context shown immediately before and after the scope.
pub const AROUND_LINES: u32 = 5;

/// How much *project* context the client may contribute, enforced here rather than trusted.
///
/// The client decides what it can see — it is the side with a parser, the other language
/// servers, and the list of buffers the user has touched — and the server decides how much of
/// it reaches the prompt. These bounds are the same discipline `AROUND_LINES` follows: the
/// model never chooses what it sees, and neither does an over-eager client.
pub const MAX_PROVIDED_DOCS: usize = 4;
pub const MAX_PROVIDED_LINES: usize = 40;

/// A document the client sent with the request (`PROTOCOL.md` §6.1).
///
/// `text` travels in the request rather than being read from disk: no index, no watcher, and
/// the server hashes exactly what it was given.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Provided {
    /// What this is, in the client's words: `imports`, `reference`, `sibling`, `test`.
    #[serde(default)]
    pub kind: String,
    pub uri: String,
    #[serde(default)]
    pub start_line: u32,
    #[serde(default)]
    pub end_line: u32,
    pub text: String,
}

/// The order kinds are rendered in, so the same request always produces the same prompt.
fn kind_rank(kind: &str) -> u8 {
    match kind {
        "imports" => 0,
        "reference" => 1,
        "test" => 2,
        "sibling" => 3,
        _ => 4,
    }
}

/// Trim to the bounds, keeping the client's order within a kind.
fn bounded(provided: &[Provided]) -> Vec<Provided> {
    let mut kept: Vec<Provided> = provided
        .iter()
        .take(MAX_PROVIDED_DOCS * 4)
        .map(|p| {
            let lines: Vec<&str> = p.text.lines().collect();
            let text = if lines.len() > MAX_PROVIDED_LINES {
                lines[..MAX_PROVIDED_LINES].join("\n")
            } else {
                p.text.clone()
            };
            Provided {
                text,
                ..p.clone()
            }
        })
        .collect();
    kept.sort_by(|a, b| {
        kind_rank(&a.kind)
            .cmp(&kind_rank(&b.kind))
            .then_with(|| a.uri.cmp(&b.uri))
            .then_with(|| a.start_line.cmp(&b.start_line))
    });
    kept.truncate(MAX_PROVIDED_DOCS);
    kept
}

/// A digest of the provided context, for the cache key.
///
/// Without this the cache would answer a request about one project state with an answer
/// computed from another, which is the failure mode that makes caching a lie.
pub fn provided_digest(provided: &[Provided]) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    for p in bounded(provided) {
        hasher.update(p.kind.as_bytes());
        hasher.update([0]);
        hasher.update(p.uri.as_bytes());
        hasher.update([0]);
        hasher.update(p.start_line.to_le_bytes());
        hasher.update(p.end_line.to_le_bytes());
        hasher.update(p.text.as_bytes());
        hasher.update([0xff]);
    }
    format!("{:x}", hasher.finalize())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub path: String,
    pub language: String,
    pub flavour: String,
    pub scope_kind: String,
    pub scope_name: Option<String>,
    pub scope_start: u32,
    pub scope_end: u32,
    pub code: String,
    pub around: Option<String>,
    pub findings: Vec<Finding>,
    /// What the client could see that this side cannot (PROTOCOL §6.1), bounded and ordered.
    pub provided: Vec<Provided>,
    /// The scope could not be shown whole and was narrowed to one statement.
    pub truncated: bool,
}

fn slice_lines(text: &str, start: u32, end: u32) -> String {
    text.split('\n')
        .skip(start as usize)
        .take((end.saturating_sub(start) + 1) as usize)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build the context for one scope. `max_findings` bounds the diagnostics slice.
pub fn build(
    doc: &Document,
    scope: &Resolved,
    findings: &[Finding],
    max_findings: usize,
) -> Context {
    build_with(doc, scope, findings, max_findings, &[])
}

/// The same, with what the client sent.
pub fn build_with(
    doc: &Document,
    scope: &Resolved,
    findings: &[Finding],
    max_findings: usize,
    provided: &[Provided],
) -> Context {
    let start = scope.range.start_line;
    let end = scope.range.end_line;
    let code = slice_lines(&doc.text, start, end);

    let total_lines = doc.line_count();
    let around = if start == 0 && end + 1 >= total_lines {
        None
    } else {
        let before_start = start.saturating_sub(AROUND_LINES);
        let after_end = (end + AROUND_LINES).min(total_lines.saturating_sub(1));
        let mut parts = Vec::new();
        if before_start < start {
            parts.push(format!(
                "preceding lines {}..{}:\n{}",
                before_start + 1,
                start,
                slice_lines(&doc.text, before_start, start - 1)
            ));
        }
        if after_end > end {
            parts.push(format!(
                "following lines {}..{}:\n{}",
                end + 2,
                after_end + 1,
                slice_lines(&doc.text, end + 1, after_end)
            ));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n"))
        }
    };

    let in_scope: Vec<Finding> = findings
        .iter()
        .filter(|f| f.line >= start && f.line <= end)
        .take(max_findings)
        .cloned()
        .collect();

    Context {
        path: doc.path.clone(),
        language: doc.language.name.clone(),
        flavour: doc.language.prompt.clone(),
        scope_kind: scope.kind.as_str().to_string(),
        scope_name: scope.name.clone(),
        scope_start: start,
        scope_end: end,
        code,
        around,
        findings: in_scope,
        provided: bounded(provided),
        truncated: scope.truncated,
    }
}

/// The rendered block shared by every prompt, so a cache hit means identical input.
pub fn render_block(ctx: &Context) -> String {
    let mut out = String::new();
    out.push_str(&format!("FILE: {}\n", ctx.path));
    out.push_str(&format!(
        "LANGUAGE: {} (prompt flavour: {})\n",
        ctx.language, ctx.flavour
    ));
    out.push_str(&format!(
        "SCOPE: {} {}\n",
        ctx.scope_kind,
        ctx.scope_name.as_deref().unwrap_or("(unnamed)")
    ));
    out.push_str(&format!(
        "SCOPE LINES: {}..{}\n",
        ctx.scope_start + 1,
        ctx.scope_end + 1
    ));
    if !ctx.provided.is_empty() {
        out.push_str("\nPROJECT CONTEXT (provided by the editor, not chosen by you):\n");
        for p in &ctx.provided {
            out.push_str(&format!(
                "  ({kind}) {uri} lines {start}..{end}:\n{text}\n",
                kind = if p.kind.is_empty() { "context" } else { &p.kind },
                uri = p.uri,
                start = p.start_line + 1,
                end = p.end_line + 1,
                text = p.text,
            ));
        }
    }
    if ctx.truncated {
        out.push_str(
            "NOTE: the enclosing declaration is larger than the configured limit, so only \
             one statement is shown and only that statement may be changed.\n",
        );
    }
    if !ctx.findings.is_empty() {
        out.push_str("KNOWN FINDINGS IN SCOPE:\n");
        for f in &ctx.findings {
            out.push_str(&format!(
                "- line {}: {} ({})\n",
                f.line + 1,
                f.label,
                f.detail
            ));
        }
    }
    out.push_str("\nCODE:\n");
    out.push_str(&ctx.code);
    out.push('\n');
    if let Some(around) = &ctx.around {
        out.push_str("\nCONTEXT (do not modify):\n");
        out.push_str(around);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provided(kind: &str, uri: &str, lines: usize) -> Provided {
        Provided {
            kind: kind.to_string(),
            uri: uri.to_string(),
            start_line: 0,
            end_line: lines.saturating_sub(1) as u32,
            text: (0..lines).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"),
        }
    }

    #[test]
    fn provided_context_is_bounded_and_ordered() {
        let many: Vec<Provided> = (0..10)
            .map(|i| provided("sibling", &format!("file:///{i}.rs"), 5))
            .collect();
        let kept = bounded(&many);
        assert_eq!(kept.len(), MAX_PROVIDED_DOCS, "a client cannot flood the prompt");
        assert_eq!(kept[0].uri, "file:///0.rs", "the client's order within a kind");

        let long = bounded(&[provided("imports", "file:///a.py", 500)]);
        assert_eq!(
            long[0].text.lines().count(),
            MAX_PROVIDED_LINES,
            "and one document cannot either"
        );

        // Kinds are rendered in a fixed order whatever order they arrived in.
        let mixed = bounded(&[
            provided("sibling", "file:///s.rs", 2),
            provided("imports", "file:///i.py", 2),
            provided("reference", "file:///r.py", 2),
            provided("test", "file:///t.py", 2),
        ]);
        let kinds: Vec<&str> = mixed.iter().map(|p| p.kind.as_str()).collect();
        assert_eq!(kinds, vec!["imports", "reference", "test", "sibling"]);
    }

    #[test]
    fn the_same_context_digests_the_same_and_a_different_one_does_not() {
        let a = vec![provided("imports", "file:///i.py", 3)];
        let same = vec![provided("imports", "file:///i.py", 3)];
        let other_text = vec![Provided {
            text: "different".to_string(),
            ..provided("imports", "file:///i.py", 3)
        }];
        assert_eq!(provided_digest(&a), provided_digest(&same));
        assert_ne!(
            provided_digest(&a),
            provided_digest(&other_text),
            "the text is part of what was asked"
        );
        assert_eq!(
            provided_digest(&[]),
            provided_digest(&[]),
            "no context is a stable answer too"
        );
    }

    use super::*;
    use crate::lang::Language;
    use crate::scope::{self};
    use crate::types::{ScopeKind, Severity, Verb};

    fn doc(text: &str) -> Document {
        Document::new("file:///tmp/a.rs", 3, text.to_string(), Some("rust"))
    }

    fn finding(line: u32) -> Finding {
        Finding {
            id: "f".into(),
            line,
            start_col: 0,
            end_col: 1,
            severity: Severity::Warning,
            label: "unchecked".into(),
            detail: "detail".into(),
            verb_hint: Verb::Fix,
        }
    }

    #[test]
    fn the_scope_slice_is_exactly_the_scope() {
        let text = "fn a() {\n    x();\n}\nfn b() {\n    y();\n}\n";
        let d = doc(text);
        let s = scope::resolve(text, 1, &crate::lang::profile("rust"), None, 400);
        let ctx = build(&d, &s, &[], 5);
        assert_eq!(ctx.code, "fn a() {\n    x();\n}");
        assert_eq!(ctx.scope_kind, "function");
    }

    #[test]
    fn findings_are_restricted_to_the_scope_and_bounded() {
        let text = "fn a() {\n    x();\n}\nfn b() {\n    y();\n}\n";
        let d = doc(text);
        let s = scope::resolve(text, 1, &crate::lang::profile("rust"), None, 400);
        let ctx = build(&d, &s, &[finding(1), finding(4), finding(1)], 2);
        assert!(ctx.findings.iter().all(|f| f.line <= 2), "outsiders dropped");
        assert!(ctx.findings.len() <= 2, "bounded");
    }

    #[test]
    fn surrounding_context_is_offered_but_labelled_as_not_to_be_modified() {
        let mut text = String::new();
        for i in 0..20 {
            text.push_str(&format!("line{i}\n"));
        }
        let d = doc(&text);
        let s = scope::resolve(&text, 10, &crate::lang::profile("unknown"), Some(crate::types::LineRange { start_line: 10, end_line: 10 }), 400);
        let ctx = build(&d, &s, &[], 5);
        let around = ctx.around.as_ref().expect("surrounding context present");
        assert!(around.contains("preceding lines"));
        assert!(around.contains("following lines"));
        let rendered = render_block(&ctx);
        assert!(rendered.contains("do not modify"));
    }

    #[test]
    fn a_whole_file_scope_has_no_surrounding_context() {
        let text = "a\nb\n";
        let d = doc(text);
        let s = scope::resolve(text, 0, &crate::lang::profile("unknown"), None, 400);
        assert_eq!(s.kind, ScopeKind::File);
        let ctx = build(&d, &s, &[], 5);
        assert!(ctx.around.is_none());
        assert_eq!(ctx.code, "a\nb\n");
    }

    #[test]
    fn the_rendered_block_is_deterministic_and_carries_its_axes() {
        let text = "fn a() {\n    x();\n}\n";
        let d = doc(text);
        let s = scope::resolve(text, 1, &crate::lang::profile("rust"), None, 400);
        let ctx = build(&d, &s, &[finding(1)], 5);
        let a = render_block(&ctx);
        let b = render_block(&ctx);
        assert_eq!(a, b, "identical inputs render identically");
        assert!(a.contains("FILE: /tmp/a.rs"));
        assert!(a.contains("LANGUAGE: rust"));
        assert!(a.contains("KNOWN FINDINGS IN SCOPE:"));
    }

    #[test]
    fn truncation_is_disclosed_to_the_model() {
        let d = doc("fn a() {\n    x();\n}\n");
        let mut s = scope::resolve("fn a() {\n    x();\n}\n", 1, &crate::lang::profile("rust"), None, 400);
        s.truncated = true;
        let ctx = build(&d, &s, &[], 5);
        assert!(render_block(&ctx).contains("larger than the configured limit"));
        assert_eq!(ctx.language, "rust");
        assert_eq!(Language::of(Some("rust"), "", "").name, "rust");
    }
}
