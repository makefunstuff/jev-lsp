//! Scope resolution: what "this" means for a cursor position or an anchor.
//!
//! The chain is structural, not syntactic — no parser is required and none is assumed
//! (docs/LANGUAGE.md §4). A scope is always produced; the only question is quality, which
//! is reported as `ScopeSource`.
//!
//! Safety rules encoded here, because a wrong extent produces a destructive edit:
//!
//!  * a brace-scanned block is only accepted if it closes on a line that actually contains
//!    `}` — otherwise the scan was fooled by a brace inside a string or comment;
//!  * if the enclosing block is larger than `max_lines`, the scope degrades to the
//!    innermost single statement rather than rewriting something the model cannot be shown.

use crate::lang::Profile;
use crate::types::{LineRange, ScopeKind, ScopeSource};

/// A resolved scope, with its provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub range: LineRange,
    pub kind: ScopeKind,
    pub source: ScopeSource,
    pub name: Option<String>,
    /// The enclosing declaration was larger than the configured cap.
    pub truncated: bool,
}

/// Words that can stand between the left margin and the keyword that opens a declaration.
///
/// Found by measuring: on a real 249-line Lua file this list was missing `local`, and five of
/// the twelve top-level functions were invisible — no lens, no hint, nothing. The list is a
/// guess by construction, which is why a *parser* answers this question when the client has
/// one (`TS_SCOPE_NODES` in the plugin); this is what serves clients that do not.
const MODIFIERS: &[&str] = &[
    "pub", "async", "unsafe", "extern", "export", "default", "static", "public", "private",
    "protected", "internal", "final", "abstract", "inline", "const", "mut", "override",
    "local", "declare", "virtual", "sealed", "partial", "global", "nonlocal",
];

fn indent_width(line: &str) -> usize {
    let mut w = 0;
    for c in line.chars() {
        match c {
            ' ' => w += 1,
            '\t' => w += 4,
            _ => break,
        }
    }
    w
}

/// Remove string literals and trailing line comments so brace counting is not fooled.
fn strip_code(line: &str, profile: &Profile) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut quote: Option<char> = None;
    let comment = profile.line_comment;
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == '\\' {
                chars.next();
                continue;
            }
            if c == q {
                quote = None;
            }
            out.push(' ');
            continue;
        }
        if c == '"' || c == '\'' || c == '`' {
            quote = Some(c);
            out.push(' ');
            continue;
        }
        if let Some(lc) = comment {
            if c == lc.chars().next().unwrap_or('#') {
                // Only a comment when no other char of the introducer precedes it.
                break;
            }
        }
        out.push(c);
    }
    out
}

fn brace_delta(stripped: &str) -> i64 {
    let mut d = 0i64;
    for c in stripped.chars() {
        match c {
            '{' => d += 1,
            '}' => d -= 1,
            _ => {}
        }
    }
    d
}

fn classify(keyword: &str) -> ScopeKind {
    match keyword {
        "fn" | "func" | "function" | "def" | "sub" => ScopeKind::Function,
        "impl" => ScopeKind::Impl,
        "class" | "struct" | "enum" | "trait" | "interface" | "object" | "type" | "record"
        | "union" | "typedef" => ScopeKind::Class,
        "mod" | "module" | "package" | "namespace" => ScopeKind::Module,
        _ => ScopeKind::Statement,
    }
}

fn trim_ident(tok: &str) -> Option<String> {
    let name: String = tok
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.' || *c == ':')
        .collect();
    let name = name.trim_end_matches(['.', ':']).to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Parse a declaration line: the kind and the declared name, if the bare line carries one
/// without indentation.
fn parse_decl(line: &str, profile: &Profile) -> Option<(ScopeKind, Option<String>)> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with(profile.line_comment.unwrap_or("\u{0}")) {
        return None;
    }
    // A declaration must be at the left margin of its own line, not a nested expression.
    let toks: Vec<&str> = trimmed.split_whitespace().collect();
    let mut idx = 0;
    while idx < toks.len() && MODIFIERS.contains(&toks[idx]) {
        idx += 1;
    }
    let kw = *toks.get(idx)?;
    if !profile.decl_keywords.contains(&kw) {
        return None;
    }
    let name = toks.get(idx + 1).and_then(|t| trim_ident(t));
    Some((classify(kw), name))
}

/// The innermost declaration enclosing `from`, searching upward.
fn find_decl_start(
    lines: &[&str],
    from: usize,
    profile: &Profile,
) -> Option<(usize, ScopeKind, Option<String>)> {
    let mut i = from;
    loop {
        let stripped = strip_code(lines[i], profile);
        if let Some((kind, name)) = parse_decl(&stripped, profile) {
            return Some((i, kind, name));
        }
        if lines[i].trim().is_empty() {
            // A blank line ends the search: the cursor is not inside a declaration.
            return None;
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

/// Brace-delimited extent, accepted only when the closing line really closes a block.
fn find_brace_end(lines: &[&str], start: usize, profile: &Profile) -> Option<usize> {
    let mut balance = 0i64;
    let mut opened = false;
    for (offset, line) in lines[start..].iter().enumerate() {
        let stripped = strip_code(line, profile);
        let d = brace_delta(&stripped);
        if d > 0 {
            opened = true;
        }
        balance += d;
        if opened && balance <= 0 {
            let idx = start + offset;
            if lines[idx].contains('}') {
                return Some(idx);
            }
            return None;
        }
    }
    None
}

/// A block ends on its last non-blank line; trailing blank lines belong to neither block.
fn trim_blank_end(lines: &[&str], start: usize, end: usize) -> usize {
    let mut e = end;
    while e > start && lines[e].trim().is_empty() {
        e -= 1;
    }
    e
}

/// Indentation-delimited extent, for languages without braces.
fn find_indent_end(lines: &[&str], start: usize) -> usize {
    let base = indent_width(lines[start]);
    if lines[start].trim_end().ends_with(':') {
        // A block opener: everything more indented than the header belongs to it.
        for i in start + 1..lines.len() {
            if lines[i].trim().is_empty() {
                continue;
            }
            if indent_width(lines[i]) <= base {
                return trim_blank_end(lines, start, i.saturating_sub(1));
            }
        }
    }
    for i in start + 1..lines.len() {
        if lines[i].trim().is_empty() {
            continue;
        }
        if indent_width(lines[i]) <= base {
            return trim_blank_end(lines, start, i.saturating_sub(1));
        }
    }
    lines.len().saturating_sub(1)
}

fn clamp_lines(text: &str) -> Vec<&str> {
    let v: Vec<&str> = text.split('\n').collect();
    if v.is_empty() {
        vec![""]
    } else {
        v
    }
}

/// Every declaration at the left margin, in document order, with the extent the same rules
/// give a cursor placed inside it.
///
/// `resolve` answers "what is under this cursor". An editor affordance needs the converse: a
/// list to hang one annotation per declaration on, which is what a code lens is. Two
/// deliberate narrowings:
///
///  * only declarations starting at the left margin are returned — a nested one belongs to
///    its parent, and a lens per nested function is noise, not information;
///  * a declaration whose extent cannot be established is skipped rather than guessed, and
///    one longer than `max_lines` is skipped because every caller anchors work the server
///    would refuse anyway. An affordance that cannot do anything is a lie.
pub fn blocks(text: &str, profile: &Profile, max_lines: u32) -> Vec<Resolved> {
    let lines = clamp_lines(text);
    let max = max_lines as usize;
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        if indent_width(lines[i]) == 0 {
            let stripped = strip_code(lines[i], profile);
            if let Some((kind, name)) = parse_decl(&stripped, profile) {
                let end = if profile.braces {
                    find_brace_end(&lines, i, profile).unwrap_or_else(|| find_indent_end(&lines, i))
                } else {
                    find_indent_end(&lines, i)
                };
                let end = end.max(i);
                if end - i + 1 <= max {
                    out.push(Resolved {
                        range: LineRange {
                            start_line: i as u32,
                            end_line: end as u32,
                        },
                        kind,
                        source: ScopeSource::Structural,
                        name,
                        truncated: false,
                    });
                }
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Resolve the scope around a cursor line.
pub fn resolve(
    text: &str,
    cursor_line: u32,
    profile: &Profile,
    explicit: Option<LineRange>,
    max_lines: u32,
) -> Resolved {
    let lines = clamp_lines(text);
    let last = lines.len().saturating_sub(1);

    if let Some(r) = explicit {
        let start = (r.start_line as usize).min(last);
        let end = (r.end_line as usize).min(last).max(start);
        return Resolved {
            range: LineRange {
                start_line: start as u32,
                end_line: end as u32,
            },
            kind: ScopeKind::Selection,
            source: ScopeSource::Explicit,
            name: None,
            truncated: false,
        };
    }

    let cursor = (cursor_line as usize).min(last);
    let max = max_lines as usize;

    if let Some((start, kind, name)) = find_decl_start(&lines, cursor, profile) {
        let end = if profile.braces {
            find_brace_end(&lines, start, profile).unwrap_or_else(|| find_indent_end(&lines, start))
        } else {
            find_indent_end(&lines, start)
        };
        let end = end.max(start);
        if end - start + 1 <= max {
            return Resolved {
                range: LineRange {
                    start_line: start as u32,
                    end_line: end as u32,
                },
                kind,
                source: ScopeSource::Structural,
                name,
                truncated: false,
            };
        }
        // Too large to show the model whole. Degrade to the statement, never to a
        // partial rewrite of a block we could not display.
        return Resolved {
            range: LineRange {
                start_line: cursor as u32,
                end_line: cursor as u32,
            },
            kind: ScopeKind::Statement,
            source: ScopeSource::Structural,
            name,
            truncated: true,
        };
    }

    if lines.len() <= max {
        return Resolved {
            range: LineRange {
                start_line: 0,
                end_line: last as u32,
            },
            kind: ScopeKind::File,
            source: ScopeSource::WholeFile,
            name: None,
            truncated: false,
        };
    }

    Resolved {
        range: LineRange {
            start_line: cursor as u32,
            end_line: cursor as u32,
        },
        kind: ScopeKind::Statement,
        source: ScopeSource::Structural,
        name: None,
        truncated: false,
    }
}

/// Resolve the extent of an anchor found at `anchor_line`.
///
/// `kind = Statement` stays on one line; anything else grows to the enclosing block.
pub fn resolve_anchor(
    text: &str,
    anchor_line: u32,
    kind: ScopeKind,
    profile: &Profile,
    max_lines: u32,
) -> Resolved {
    if kind == ScopeKind::Statement {
        return resolve(text, anchor_line, profile, Some(LineRange {
            start_line: anchor_line,
            end_line: anchor_line,
        }), max_lines);
    }
    if kind == ScopeKind::File {
        let lines = clamp_lines(text);
        return Resolved {
            range: LineRange {
                start_line: 0,
                end_line: lines.len().saturating_sub(1) as u32,
            },
            kind: ScopeKind::File,
            source: ScopeSource::WholeFile,
            name: None,
            truncated: false,
        };
    }
    resolve(text, anchor_line, profile, None, max_lines)
}

/// Byte offset of the start of a line, for building LSP positions.
pub fn line_offsets(text: &str) -> Vec<usize> {
    let mut offsets = vec![0usize];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            offsets.push(i + 1);
        }
    }
    offsets
}

/// Byte length of a line, excluding the newline.
pub fn line_len(text: &str, line: u32) -> u32 {
    let offsets = line_offsets(text);
    let idx = line as usize;
    if idx + 1 < offsets.len() {
        (offsets[idx + 1] - offsets[idx] - 1) as u32
    } else if idx < offsets.len() {
        (text.len() - offsets[idx]) as u32
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang;

    const PY: &str = "import json\n\n\ndef alpha(x):\n    def inner(y):\n        return y\n    return inner(x)\n\n\ndef beta(x):\n    return x\n";

    #[test]
    fn blocks_enumerate_top_level_declarations_in_order() {
        let p = lang::profile("python");
        let found = blocks(PY, &p, 200);
        let names: Vec<_> = found.iter().map(|b| b.name.clone().unwrap_or_default()).collect();
        assert_eq!(names, vec!["alpha", "beta"], "nested declarations belong to their parent");
        assert_eq!(found[0].range.start_line, 3);
        assert_eq!(found[0].range.end_line, 6, "alpha ends after its nested def");
        assert_eq!(found[1].range.start_line, 9);
    }

    #[test]
    fn blocks_skip_a_declaration_longer_than_the_cap() {
        let mut text = String::from("def big():\n");
        for i in 0..40 {
            text.push_str(&format!("    x{i} = 1\n"));
        }
        text.push_str("\ndef small():\n    return 1\n");
        let p = lang::profile("python");
        assert!(blocks(&text, &p, 200).iter().any(|b| b.name.as_deref() == Some("big")));
        let capped = blocks(&text, &p, 10);
        assert!(
            capped.iter().all(|b| b.name.as_deref() != Some("big")),
            "an affordance the server would refuse is not offered"
        );
        assert!(capped.iter().any(|b| b.name.as_deref() == Some("small")));
    }

    #[test]
    fn blocks_are_empty_when_nothing_is_declared() {
        let p = lang::profile("python");
        assert!(blocks("x = 1\ny = 2\n", &p, 200).is_empty());
        assert!(blocks("", &p, 200).is_empty());
    }

    #[test]
    fn a_modifier_before_the_keyword_does_not_hide_a_declaration() {
        // The case that was measured: `local function` was invisible, so five of twelve
        // declarations in a real file had no lens and no hint.
        let p = lang::profile("lua");
        let text = "local function one()\n  return 1\nend\n\nfunction two()\n  return 2\nend\n";
        let found = blocks(text, &p, 200);
        let names: Vec<_> = found.iter().map(|b| b.name.clone().unwrap_or_default()).collect();
        assert_eq!(names, vec!["one", "two"], "both declarations are seen");
    }

    #[test]
    fn blocks_handle_brace_languages() {
        let p = lang::profile("rust");
        let text = "impl Thing {\n    fn a(&self) {}\n}\n\nfn standalone() {\n    let x = 1;\n}\n";
        let found = blocks(text, &p, 200);
        let names: Vec<_> = found.iter().map(|b| b.name.clone().unwrap_or_default()).collect();
        assert_eq!(names, vec!["Thing", "standalone"]);
        assert_eq!(found[0].range.end_line, 2);
        assert_eq!(found[1].range.end_line, 6);
    }

    use crate::lang::profile;

    fn rust() -> Profile {
        profile("rust")
    }

    #[test]
    fn brace_block_is_found_from_inside() {
        let text = "fn a() {\n    let x = 1;\n}\n\nfn b() {\n    inner();\n}\n";
        let r = resolve(text, 5, &rust(), None, 400);
        assert_eq!(r.range.start_line, 4);
        assert_eq!(r.range.end_line, 6);
        assert_eq!(r.kind, ScopeKind::Function);
        assert_eq!(r.name.as_deref(), Some("b"));
        assert_eq!(r.source, ScopeSource::Structural);
    }

    #[test]
    fn innermost_declaration_wins() {
        let text = "fn outer() {\n    fn inner() {\n        work();\n    }\n}\n";
        let r = resolve(text, 2, &rust(), None, 400);
        assert_eq!(r.range.start_line, 1);
        assert_eq!(r.range.end_line, 3);
    }

    #[test]
    fn a_brace_inside_a_string_does_not_end_the_block() {
        let text = "fn a() {\n    let s = \"}\";\n    more();\n}\n";
        let r = resolve(text, 2, &rust(), None, 400);
        assert_eq!(r.range.start_line, 0);
        assert_eq!(r.range.end_line, 3);
    }

    #[test]
    fn a_brace_inside_a_comment_does_not_end_the_block() {
        let text = "fn a() {\n    // closing }\n    more();\n}\n";
        let r = resolve(text, 2, &rust(), None, 400);
        assert_eq!(r.range.end_line, 3);
    }

    #[test]
    fn python_blocks_are_indentation_delimited() {
        let py = profile("python");
        let text = "def a():\n    x = 1\n    return x\n\ndef b():\n    return 2\n";
        let r = resolve(text, 1, &py, None, 400);
        assert_eq!(r.range.start_line, 0);
        assert_eq!(r.range.end_line, 2);
        assert_eq!(r.kind, ScopeKind::Function);
        assert_eq!(r.name.as_deref(), Some("a"));
    }

    #[test]
    fn an_oversized_block_degrades_to_the_statement() {
        let mut text = String::from("fn big() {\n");
        for i in 0..30 {
            text.push_str(&format!("    let v{i} = {i};\n"));
        }
        text.push_str("}\n");
        let r = resolve(&text, 10, &rust(), None, 10);
        assert!(r.truncated);
        assert_eq!(r.kind, ScopeKind::Statement);
        assert_eq!(r.range.start_line, 10);
        assert_eq!(r.range.end_line, 10);
    }

    #[test]
    fn unknown_language_without_declarations_falls_back_to_the_file() {
        let unknown = profile("unknown");
        let text = "one\ntwo\nthree\n";
        let r = resolve(text, 1, &unknown, None, 400);
        assert_eq!(r.kind, ScopeKind::File);
        assert_eq!(r.source, ScopeSource::WholeFile);
        assert_eq!(r.range.end_line, 3);
    }

    #[test]
    fn explicit_range_is_used_verbatim() {
        let r = resolve("a\nb\nc\n", 0, &rust(), Some(LineRange { start_line: 1, end_line: 1 }), 400);
        assert_eq!(r.kind, ScopeKind::Selection);
        assert_eq!(r.source, ScopeSource::Explicit);
        assert_eq!(r.range.start_line, 1);
    }

    #[test]
    fn empty_document_does_not_panic() {
        let r = resolve("", 0, &rust(), None, 400);
        assert_eq!(r.range.start_line, 0);
        assert_eq!(r.range.end_line, 0);
        let r2 = resolve("", 99, &rust(), None, 400);
        assert_eq!(r2.range.end_line, 0);
    }

    #[test]
    fn line_helpers_agree_with_the_text() {
        let text = "ab\ncde\nf";
        assert_eq!(line_offsets(text), vec![0, 3, 7]);
        assert_eq!(line_len(text, 0), 2);
        assert_eq!(line_len(text, 1), 3);
        assert_eq!(line_len(text, 2), 1);
    }

    #[test]
    fn anchor_statement_stays_on_its_line_and_file_covers_everything() {
        let text = "fn a() {\n    x();\n}\n";
        let s = resolve_anchor(text, 1, ScopeKind::Statement, &rust(), 400);
        assert_eq!(s.range.start_line, 1);
        assert_eq!(s.range.end_line, 1);
        let f = resolve_anchor(text, 1, ScopeKind::File, &rust(), 400);
        assert_eq!(f.range.start_line, 0);
        assert_eq!(f.range.end_line, 3);
    }
}
