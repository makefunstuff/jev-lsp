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
