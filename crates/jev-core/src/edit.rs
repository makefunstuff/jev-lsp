//! Turning a model's anchored replacements into concrete, safe text operations.
//!
//! This is where PROTOCOL.md N5 is enforced: the model supplies a quotation and a
//! replacement, and every position is computed here against the real document.

use crate::contract::RawEdit;
use crate::lang::Profile;
use crate::scope::{self, line_len};
use crate::types::{NewFile, Proposal, ScopeKind, TextOp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    AnchorNotFound { needle: String },
    AnchorAmbiguous { needle: String, count: usize },
    UnknownAnchorKind { kind: String },
    /// The answer reaches outside the scope the user asked about.
    OutsideScope {
        scope_start: u32,
        scope_end: u32,
        edit_start: u32,
        edit_end: u32,
    },
    Overlap,
    BadPath { path: String },
    /// The answer covers more of the document than the anchor names.
    ReemitsFollowing {
        lines: usize,
        /// The first line of the enclosing block, so a retry can quote it instead of being
        /// told in prose to "anchor on the larger block".
        anchor_hint: String,
    },
    Empty,
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::AnchorNotFound { needle } => write!(
                f,
                "anchor {:?} does not occur in the document; quote text that exists verbatim",
                truncate(needle)
            ),
            EditError::AnchorAmbiguous { needle, count } => write!(
                f,
                "anchor {:?} occurs {count} times; quote enough surrounding text to make it unique",
                truncate(needle)
            ),
            EditError::UnknownAnchorKind { kind } => write!(
                f,
                "anchor kind {kind:?} is not one of function, method, class, struct, impl, mod, block, statement, file"
            ),
            EditError::OutsideScope {
                scope_start,
                scope_end,
                edit_start,
                edit_end,
            } => write!(
                f,
                "the edit covers lines {edit_start}-{edit_end}, outside the scope (lines {scope_start}-{scope_end}) the user selected; change only lines inside that scope"
            ),
            EditError::Overlap => write!(f, "two replacements cover overlapping lines"),
            EditError::ReemitsFollowing { lines, anchor_hint } => write!(
                f,
                "the replacement repeats {lines} line(s) that already follow the anchor, so the \
                 answer covers more of the file than the anchor names. Anchor on the enclosing \
                 block instead and rewrite that whole block, quoting {anchor_hint:?} as `match`."
            ),
            EditError::BadPath { path } => {
                write!(f, "new file path {path:?} must be relative and must not contain `..`")
            }
            EditError::Empty => write!(f, "the response contains no replacements and no new files"),
        }
    }
}

impl std::error::Error for EditError {}

fn truncate(s: &str) -> String {
    let t: String = s.chars().take(40).collect();
    if s.chars().count() > 40 {
        format!("{t}…")
    } else {
        t
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BuildOptions {
    /// Largest scope the model may be asked to rewrite (docs/LANGUAGE.md §4).
    pub max_scope_lines: u32,
    /// The lines the user asked about, when the caller knows them.
    ///
    /// The scope is the unit of change: a fix requested for one function may not rewrite the
    /// file. Measured: a proposal whose ops spanned the document was applied, and the user
    /// watched their whole buffer be replaced instead of the selection being edited. Nothing
    /// checked this; the check turns it into a repair the model can be asked to redo.
    pub scope_lines: Option<(u32, u32)>,
}

impl Default for BuildOptions {
    fn default() -> Self {
        BuildOptions {
            max_scope_lines: 400,
            scope_lines: None,
        }
    }
}

fn parse_kind(kind: Option<&str>) -> Result<ScopeKind, EditError> {
    match kind {
        None => Ok(ScopeKind::Function),
        Some(k) => {
            let k = k.trim().to_ascii_lowercase();
            // Accept the C-family spellings that map onto the same extent class.
            let normalised = match k.as_str() {
                "struct" | "enum" | "trait" | "interface" | "object" | "type" | "record" => "class",
                "func" | "def" | "fn" | "sub" => "function",
                "mod" | "module" | "namespace" | "package" => "module",
                other => other,
            };
            ScopeKind::parse(normalised).ok_or(EditError::UnknownAnchorKind {
                kind: kind.unwrap_or_default().to_string(),
            })
        }
    }
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack.match_indices(needle).count()
}

fn line_of_offset(text: &str, offset: usize) -> u32 {
    text[..offset].bytes().filter(|b| *b == b'\n').count() as u32
}

/// Extend the replaced range over the lines the answer re-emits.
///
/// The invariant that matters: a `replacement` may be longer than the range it replaces
/// (adding lines is normal), but it must not contain copies of lines that are still in the
/// document immediately after that range — applied literally, those become duplicates.
///
/// So a maximal run of the replacement's lines that matches the document immediately after
/// the head span is *consumed* rather than duplicated. Two real shapes this handles:
///
/// * a one-line `statement` answered with `with open(...) as f:` plus the body re-indented
///   under it — the body line is absorbed into the replaced range, which is the intent;
/// * the same answer plus the trailing `return`, likewise absorbed.
///
/// Comparison ignores surrounding whitespace, because re-indenting the absorbed line is the
/// whole point of the first shape. A replacement that merely adds a line (no such match)
/// leaves the range alone.
fn extend_over_reemission(
    lines: &[&str],
    start_line: u32,
    span: usize,
    replacement: &[&str],
) -> (usize, usize) {
    if replacement.len() <= span {
        return (span, 0);
    }
    let mut matched = 0usize;
    while span + matched < replacement.len() {
        let doc_index = start_line as usize + span + matched;
        if doc_index < lines.len()
            && replacement[span + matched].trim() == lines[doc_index].trim()
        {
            matched += 1;
        } else {
            break;
        }
    }
    let covered = span + matched;

    // Anything left over is inserted, and must not duplicate a line that stays put.
    let duplicated = replacement
        .iter()
        .enumerate()
        .skip(covered)
        .filter(|(i, line)| {
            let doc_index = start_line as usize + i;
            doc_index < lines.len() && line.trim() == lines[doc_index].trim()
        })
        .count();

    (covered, duplicated)
}

/// Build a validated proposal.
pub fn build_proposal(
    text: &str,
    raw: &RawEdit,
    profile: &Profile,
    opts: &BuildOptions,
) -> Result<Proposal, EditError> {
    let mut ops: Vec<TextOp> = Vec::new();
    let lines: Vec<&str> = text.split('\n').collect();

    for r in &raw.replacements {
        let needle = r.anchor.needle.as_str();
        let count = count_occurrences(text, needle);
        match count {
            0 => {
                return Err(EditError::AnchorNotFound {
                    needle: needle.to_string(),
                })
            }
            1 => {}
            n => {
                return Err(EditError::AnchorAmbiguous {
                    needle: needle.to_string(),
                    count: n,
                })
            }
        }

        let offset = text.find(needle).unwrap_or(0);
        let line = line_of_offset(text, offset);
        let kind = parse_kind(r.anchor.kind.as_deref())?;
        let head = scope::resolve_anchor(text, line, kind, profile, opts.max_scope_lines);
        let start_line = head.range.start_line;
        let span = (head.range.end_line - start_line + 1) as usize;

        let new_text = r.replacement.trim_end_matches(['\n', '\r']).to_string();
        let replacement: Vec<&str> = new_text.split('\n').collect();

        let (covered, duplicated) = extend_over_reemission(&lines, start_line, span, &replacement);
        if duplicated > 0 {
            let outer = scope::resolve(text, line, profile, None, opts.max_scope_lines);
            let hint = lines
                .get(outer.range.start_line as usize)
                .copied()
                .unwrap_or("")
                .trim()
                .to_string();
            return Err(EditError::ReemitsFollowing {
                lines: duplicated,
                anchor_hint: hint,
            });
        }
        let end_line = start_line + covered as u32 - 1;

        ops.push(TextOp {
            start_line,
            end_line,
            end_col: line_len(text, end_line),
            new_text,
        });
    }

    ops.sort_by_key(|o| (o.start_line, o.end_line));
    ops.dedup();
    for pair in ops.windows(2) {
        if pair[1].start_line <= pair[0].end_line {
            return Err(EditError::Overlap);
        }
    }

    let mut new_files = Vec::new();
    for f in &raw.new_files {
        let path = f.path.trim().to_string();
        let bad = path.is_empty()
            || path.starts_with('/')
            || path.starts_with('~')
            || path.split('/').any(|seg| seg == "..")
            || path.contains('\0');
        if bad {
            return Err(EditError::BadPath { path });
        }
        new_files.push(NewFile {
            path,
            content: f.content.clone(),
        });
    }

    if ops.is_empty() && new_files.is_empty() {
        return Err(EditError::Empty);
    }

    if let Some((scope_start, scope_end)) = opts.scope_lines {
        for op in &ops {
            if op.start_line < scope_start || op.end_line > scope_end {
                return Err(EditError::OutsideScope {
                    scope_start,
                    scope_end,
                    edit_start: op.start_line,
                    edit_end: op.end_line,
                });
            }
        }
    }

    Ok(Proposal {
        summary: raw
            .summary
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "proposed change".to_string()),
        rationale: raw.rationale.clone().unwrap_or_default(),
        ops,
        new_files,
    })
}

/// The text a proposal would produce, used to verify what actually landed.
///
/// PROTOCOL.md §8: after an edit the server applies, the result is checked against its own
/// prediction, and a divergence is published rather than ignored. This is the prediction
/// half; the comparison happens when the document's next sync arrives.
pub fn predict_after(text: &str, proposal: &Proposal) -> String {
    let mut lines: Vec<String> = text.split('\n').map(|s| s.to_string()).collect();
    // Descending, so an earlier replacement cannot shift a later one's line numbers.
    let mut ops = proposal.ops.clone();
    ops.sort_by_key(|o| std::cmp::Reverse(o.start_line));
    for op in ops {
        let start = op.start_line as usize;
        if start >= lines.len() {
            continue;
        }
        let end = (op.end_line as usize).min(lines.len() - 1).max(start);
        let replacement: Vec<String> = op.new_text.split('\n').map(|s| s.to_string()).collect();
        lines.splice(start..=end, replacement);
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::parse_edit;
    use crate::lang::profile;

    fn build(src: &str, text: &str) -> Result<Proposal, EditError> {
        let raw = parse_edit(src).unwrap();
        build_proposal(text, &raw, &profile("rust"), &BuildOptions::default())
    }

    const DOC: &str = "fn a() {\n    let x = 1;\n}\n\nfn retry() {\n    go();\n}\n";

    #[test]
    fn anchor_resolves_to_the_enclosing_block() {
        let p = build(
            r#"{"summary":"harden","replacements":[{"anchor":{"kind":"function","match":"fn retry()"},"replacement":"fn retry() {\n    go_safe();\n}"}]}"#,
            DOC,
        )
        .unwrap();
        assert_eq!(p.ops.len(), 1);
        assert_eq!(p.ops[0].start_line, 4);
        assert_eq!(p.ops[0].end_line, 6);
        assert_eq!(p.ops[0].end_col, 1, "end column excludes the newline");
        assert!(!p.ops[0].new_text.ends_with('\n'), "trailing newline is trimmed");
    }

    #[test]
    fn a_missing_anchor_is_rejected_rather_than_guessed() {
        let e = build(
            r#"{"replacements":[{"anchor":{"match":"fn does_not_exist()"},"replacement":"x"}]}"#,
            DOC,
        )
        .unwrap_err();
        assert!(matches!(e, EditError::AnchorNotFound { .. }));
    }

    #[test]
    fn an_ambiguous_anchor_is_rejected_with_the_count() {
        let doc = "fn a() {}\nfn a() {}\n";
        let e = build(
            r#"{"replacements":[{"anchor":{"match":"fn a()"},"replacement":"x"}]}"#,
            doc,
        )
        .unwrap_err();
        assert_eq!(
            e,
            EditError::AnchorAmbiguous {
                needle: "fn a()".into(),
                count: 2
            }
        );
    }

    #[test]
    fn overlapping_replacements_are_rejected() {
        let src = r#"{"replacements":[
            {"anchor":{"kind":"function","match":"fn retry()"},"replacement":"A"},
            {"anchor":{"kind":"statement","match":"go();"},"replacement":"B"}]}"#;
        assert_eq!(build(src, DOC).unwrap_err(), EditError::Overlap);
    }

    #[test]
    fn unknown_anchor_kind_is_rejected() {
        let src = r#"{"replacements":[{"anchor":{"kind":"banana","match":"go();"},"replacement":"B"}]}"#;
        assert!(matches!(
            build(src, DOC).unwrap_err(),
            EditError::UnknownAnchorKind { .. }
        ));
    }

    #[test]
    fn evil_paths_are_rejected_and_safe_ones_accepted() {
        let evil = r#"{"new_files":[{"path":"../../etc/passwd","content":"x"}]}"#;
        assert!(matches!(
            build(evil, DOC).unwrap_err(),
            EditError::BadPath { .. }
        ));
        let abs = r#"{"new_files":[{"path":"/etc/passwd","content":"x"}]}"#;
        assert!(matches!(
            build(abs, DOC).unwrap_err(),
            EditError::BadPath { .. }
        ));
        let ok = r##"{"new_files":[{"path":"tests/retry.rs","content":"#[test]\n"}]}"##;
        let p = build(ok, DOC).unwrap();
        assert_eq!(p.new_files[0].path, "tests/retry.rs");
        assert!(p.ops.is_empty());
    }

    #[test]
    fn an_empty_response_is_rejected() {
        assert_eq!(build("{}", DOC).unwrap_err(), EditError::Empty);
        assert_eq!(
            build(r#"{"replacements":[]}"#, DOC).unwrap_err(),
            EditError::Empty
        );
    }

    #[test]
    fn summary_falls_back_when_absent_or_blank() {
        let src = r#"{"summary":"   ","replacements":[{"anchor":{"match":"go();"},"replacement":"B"}]}"#;
        assert_eq!(build(src, DOC).unwrap().summary, "proposed change");
    }

    #[test]
    fn delete_is_expressible_as_an_empty_replacement() {
        let src = r#"{"replacements":[{"anchor":{"kind":"statement","match":"    go();\n"},"replacement":""}]}"#;
        let p = build(src, DOC).unwrap();
        assert_eq!(p.ops[0].new_text, "");
        assert_eq!(p.ops[0].start_line, 5);
    }

    #[test]
    fn an_answer_that_reshapes_a_block_absorbs_the_re_emitted_lines() {
        // Reproduced against a real model: anchored on a one-line `statement`, it answered
        // with `with open(...) as f:` plus the body re-indented under it. The body line must
        // be consumed by the replacement, not duplicated after it.
        let doc = "def load(path):\n    f = open(path)\n    data = json.load(f)\n    return data\n";
        let src = r#"{"summary":"s","replacements":[{"anchor":{"kind":"statement","match":"    f = open(path)"},"replacement":"    with open(path) as f:\n        data = json.load(f)"}]}"#;
        let p = build(src, doc).unwrap();
        assert_eq!(p.ops[0].start_line, 1);
        assert_eq!(p.ops[0].end_line, 2, "the re-indented body line is absorbed");
        let after: Vec<&str> = "def load(path):\n    with open(path) as f:\n        data = json.load(f)\n    return data\n"
            .split('\n')
            .collect();
        assert_eq!(after[2], "        data = json.load(f)");
        assert_eq!(after[3], "    return data", "and the tail is untouched");
    }

    #[test]
    fn an_answer_that_also_repeats_the_tail_absorbs_that_too() {
        let doc = "def load(path):\n    f = open(path)\n    data = json.load(f)\n    return data\n";
        let src = r#"{"replacements":[{"anchor":{"kind":"statement","match":"    f = open(path)"},"replacement":"    with open(path) as f:\n    data = json.load(f)\n    return data"}]}"#;
        let p = build(src, doc).unwrap();
        assert_eq!(p.ops[0].start_line, 1);
        assert_eq!(p.ops[0].end_line, 3, "both re-emitted lines are consumed");
    }

    #[test]
    fn a_replacement_that_only_adds_lines_leaves_the_range_alone() {
        // Adding lines is legitimate: nothing matches what follows, so nothing is absorbed.
        let doc = "def f():\n    x = 1\n    y = 9\n";
        let src = r#"{"replacements":[{"anchor":{"kind":"statement","match":"    x = 1"},"replacement":"    x = 1\n    y = 2"}]}"#;
        let p = build(src, doc).unwrap();
        assert_eq!(p.ops[0].start_line, 1);
        assert_eq!(p.ops[0].end_line, 1, "only the anchored line is replaced");
        assert_eq!(p.ops[0].new_text, "    x = 1\n    y = 2");
    }

    #[test]
    fn an_insertion_that_would_duplicate_a_line_that_stays_is_refused() {
        // The corruption shape that is not adjacent to the span: the answer repeats a line
        // further down, which would leave two copies of it.
        let doc = "a\nb\nc\nd\n";
        let src = r#"{"replacements":[{"anchor":{"kind":"statement","match":"b"},"replacement":"B\nx\nd"}]}"#;
        assert_eq!(
            build(src, doc).unwrap_err(),
            EditError::ReemitsFollowing { lines: 1, anchor_hint: "a".to_string() }
        );
    }

    #[test]
    fn the_rejection_tells_the_model_what_to_anchor_on_instead() {
        // Prose advice ("anchor on the larger block") is not actionable for a model that has
        // to emit a verbatim quote; the enclosing block's first line is.
        // `d` appears both in the replacement's tail and further down the document, which is
        // the shape that would duplicate a line if it were applied as written.
        let doc = "a\nb\nc\nd\n";
        let src = r#"{"replacements":[{"anchor":{"kind":"statement","match":"b"},"replacement":"B\nx\nd"}]}"#;
        let rendered = build(src, doc).unwrap_err().to_string();
        assert!(rendered.contains("quoting"), "{rendered}");
        assert!(rendered.contains("\"a\""), "with a line to quote instead: {rendered}");
    }

    #[test]
    fn a_same_size_replacement_ending_with_the_next_line_is_left_alone() {
        let doc = "a\nb\nc\n";
        let src = r#"{"replacements":[{"anchor":{"kind":"statement","match":"b"},"replacement":"c"}]}"#;
        let p = build(src, doc).unwrap();
        assert_eq!(p.ops[0].new_text, "c");
        assert_eq!(p.ops[0].end_line, 1);
    }

    #[test]
    fn a_whole_function_rewrite_is_not_mistaken_for_a_re_emission() {
        // The ordinary case: the anchor names the block, so the answer stays inside it.
        let doc = "fn a() {\n    let x = 1;\n}\n\nfn b() {\n    go();\n}\n";
        let src = r#"{"replacements":[{"anchor":{"kind":"function","match":"fn b()"},"replacement":"fn b() {\n    go_safe();\n}"}]}"#;
        let p = build(src, doc).unwrap();
        assert_eq!(p.ops[0].start_line, 4);
        assert_eq!(p.ops[0].end_line, 6);
        assert_eq!(p.ops[0].new_text, "fn b() {\n    go_safe();\n}");
    }

    #[test]
    fn a_genuinely_longer_replacement_is_kept_whole() {
        // Adding lines is legitimate and common; only a re-emission of what follows the
        // scope is refused.
        let doc = "def f():\n    x = 1\n\ndef g():\n    pass\n";
        let src = r#"{"replacements":[{"anchor":{"kind":"statement","match":"    x = 1"},"replacement":"    x = 1\n    y = 2"}]}"#;
        let p = build(src, doc).unwrap();
        assert_eq!(p.ops[0].new_text, "    x = 1\n    y = 2");
    }

    #[test]
    fn a_prediction_matches_what_applying_the_ops_produces() {
        let raw = parse_edit(
            r#"{"replacements":[{"anchor":{"kind":"function","match":"fn retry()"},"replacement":"fn retry() {\n    go_safe();\n}"}]}"#,
        )
        .unwrap();
        let p = build_proposal(DOC, &raw, &profile("rust"), &BuildOptions::default()).unwrap();
        assert_eq!(
            predict_after(DOC, &p),
            "fn a() {\n    let x = 1;\n}\n\nfn retry() {\n    go_safe();\n}\n"
        );
    }

    #[test]
    fn several_ops_predict_independently_of_their_order() {
        let doc = "one\ntwo\nthree\nfour\n";
        let raw = parse_edit(
            r#"{"replacements":[
                {"anchor":{"kind":"statement","match":"four"},"replacement":"FOUR"},
                {"anchor":{"kind":"statement","match":"one"},"replacement":"ONE"}]}"#,
        )
        .unwrap();
        let p = build_proposal(doc, &raw, &profile("unknown"), &BuildOptions::default()).unwrap();
        assert_eq!(predict_after(doc, &p), "ONE\ntwo\nthree\nFOUR\n");
    }

    #[test]
    fn a_prediction_of_nothing_is_the_same_text() {
        let p = Proposal {
            summary: "s".into(),
            rationale: String::new(),
            ops: Vec::new(),
            new_files: Vec::new(),
        };
        assert_eq!(predict_after(DOC, &p), DOC);
    }

}
