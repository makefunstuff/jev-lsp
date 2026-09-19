//! Practical gates: the only things that limit support, and every one of them is stated
//! rather than silent (docs/LANGUAGE.md §5).
//!
//! None of these is about language. Support is unconditional (PROTOCOL.md N10).

/// Why a document is not being analysed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    Binary,
    OverSize { bytes: usize, limit: u64 },
    Ignored { pattern: String },
}

impl Skip {
    pub fn code(&self) -> &'static str {
        match self {
            Skip::Binary => "binary",
            Skip::OverSize { .. } => "over_size",
            Skip::Ignored { .. } => "ignored",
        }
    }

    pub fn reason(&self) -> String {
        match self {
            Skip::Binary => "the file looks binary".to_string(),
            Skip::OverSize { bytes, limit } => {
                format!("{bytes} bytes exceeds the {limit} byte analysis limit")
            }
            Skip::Ignored { pattern } => format!("path matches the ignore pattern {pattern:?}"),
        }
    }
}

const BINARY_SNIFF_BYTES: usize = 8192;

/// A NUL byte in the first 8 KiB is the conventional and cheap binary signal.
pub fn is_binary(text: &str) -> bool {
    let head = &text.as_bytes()[..text.len().min(BINARY_SNIFF_BYTES)];
    head.contains(&0)
}

pub fn too_large(text: &str, max_bytes: u64) -> bool {
    max_bytes > 0 && text.len() as u64 > max_bytes
}

/// Minimal glob matcher: `**` spans path segments, `*` and `?` stay within one.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let seg: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match_segments(&pat, &seg)
}

fn match_segments(pat: &[&str], seg: &[&str]) -> bool {
    match pat.split_first() {
        None => seg.is_empty(),
        Some((&"**", rest)) => {
            // `**` matches zero or more segments.
            (0..=seg.len()).any(|skip| match_segments(rest, &seg[skip..]))
        }
        Some((&p, rest)) => match seg.split_first() {
            Some((&s, srest)) => match_segment(p, s) && match_segments(rest, srest),
            None => false,
        },
    }
}

fn match_segment(pat: &str, seg: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let s: Vec<char> = seg.chars().collect();
    glob_here(&p, &s)
}

fn glob_here(p: &[char], s: &[char]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some(('*', rest)) => {
            // `*` matches zero or more characters within the segment.
            (0..=s.len()).any(|skip| glob_here(rest, &s[skip..]))
        }
        Some(('?', rest)) => match s.split_first() {
            Some((_, srest)) => glob_here(rest, srest),
            None => false,
        },
        Some((&c, rest)) => match s.split_first() {
            Some((&sc, srest)) if sc == c => glob_here(rest, srest),
            _ => false,
        },
    }
}

/// A path as a rule's `applies_to` should see it: relative to the workspace root.
///
/// `applies_to` patterns are written the way a repository names its own files —
/// `crates/**/*.rs` — while a document path is absolute, so matching the absolute path compares
/// `crates` against `Users` and matches nothing. That failure is silent, which is the worst kind
/// here: the pass reports `no_rules`, and "no rule applied" reads exactly like "you have no
/// rules". Falls back to the path as given when it is not under the root, so a file outside the
/// workspace cannot be matched by a repository-relative pattern by accident.
pub fn relative_to<'a>(path: &'a str, root: &str) -> &'a str {
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return path;
    }
    match path.strip_prefix(root) {
        // The remainder has to be *inside* the root, which means a path-segment boundary:
        // `strip_prefix` is a byte comparison, so `/Users/x/repo-other/a.rs` would otherwise
        // come back as `-other/a.rs` and be matched as if it lived in the repository.
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            rest.strip_prefix('/').unwrap_or(rest)
        }
        _ => path,
    }
}

/// The first ignore pattern that matches, if any.
pub fn ignored_by(path: &str, patterns: &[String]) -> Option<String> {
    patterns
        .iter()
        .find(|p| glob_match(p, path))
        .cloned()
}

/// Evaluate every gate for one document.
/// Whether a document is worth analysing at all.
///
/// The file bound is **bytes**; the scope bound is separate and per declaration
/// (`scope::resolve`). Passing the scope limit here as well refused every file longer than it —
/// four hundred lines, an ordinary module — and refused it as `over_size`, which reads like a
/// size problem rather than a length one. A long file is analysed; a long *declaration* inside
/// it degrades to a statement, which is that bound doing its own job.
pub fn evaluate(
    text: &str,
    path: &str,
    max_file_bytes: u64,
    patterns: &[String],
) -> Option<Skip> {
    if is_binary(text) {
        return Some(Skip::Binary);
    }
    if too_large(text, max_file_bytes) {
        return Some(Skip::OverSize {
            bytes: text.len(),
            limit: max_file_bytes,
        });
    }
    if let Some(pattern) = ignored_by(path, patterns) {
        return Some(Skip::Ignored { pattern });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_is_detected_only_from_a_nul_byte() {
        assert!(is_binary("abc\0def"));
        assert!(!is_binary("just text with unicode é ü 🌍"));
        // A NUL past the sniff window is deliberately not scanned: cost matters.
        let far = format!("{}{}", "a".repeat(BINARY_SNIFF_BYTES), "\0");
        assert!(!is_binary(&far));
    }

    #[test]
    fn size_gate_respects_zero_as_unlimited() {
        assert!(too_large("abcdef", 3));
        assert!(!too_large("abc", 3));
        assert!(!too_large("abc", 0));
    }

    #[test]
    fn globs_match_the_way_ignore_patterns_do() {
        assert!(glob_match("**/node_modules/**", "web/node_modules/pkg/index.js"));
        assert!(glob_match("**/node_modules/**", "/web/node_modules/pkg/index.js"));
        assert!(glob_match("**/*.min.js", "a/b/app.min.js"));
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "src/main.rs"), "a single * stays in one segment");
        assert!(glob_match("src/**/*.rs", "src/a/b/c.rs"));
        assert!(!glob_match("**/*.min.js", "app.js"));
        assert!(glob_match("target", "target"));
        assert!(!glob_match("target", "target/x"));
    }

    #[test]
    fn a_repository_relative_pattern_matches_a_nested_path_under_its_root() {
        let path = "/Users/x/repo/crates/jev-core/src/rules.rs";
        let relative = relative_to(path, "/Users/x/repo");
        assert_eq!(relative, "crates/jev-core/src/rules.rs");
        // The form every rule author writes first.
        assert!(glob_match("crates/**/*.rs", relative));
        // And the form that only works *because* the leading `**` spans zero segments — the
        // one that was silently needed before this, so it must keep working.
        assert!(glob_match("**/crates/**/*.rs", relative));
        assert!(glob_match("crates/jev-core/src/*.rs", relative));
        assert!(!glob_match("src/**/*.rs", relative), "a different tree is still a different tree");
        // A trailing slash on the root is not a different root.
        assert_eq!(relative_to(path, "/Users/x/repo/"), "crates/jev-core/src/rules.rs");
    }

    #[test]
    fn with_no_root_the_absolute_path_is_what_a_pattern_sees() {
        // The documented fallback: without a root there is no relative context to offer, so a
        // repository-relative pattern does not match — and saying so is better than pretending.
        let path = "/Users/x/repo/crates/a.rs";
        assert_eq!(relative_to(path, ""), path);
        assert!(!glob_match("crates/**/*.rs", relative_to(path, "")));
        assert!(glob_match("**/crates/**/*.rs", relative_to(path, "")));
    }

    #[test]
    fn a_file_outside_the_root_is_not_matched_by_a_relative_pattern() {
        assert_eq!(relative_to("/tmp/scratch/a.rs", "/Users/x/repo"), "/tmp/scratch/a.rs");
        assert!(!glob_match("crates/**/*.rs", relative_to("/tmp/scratch/a.rs", "/Users/x/repo")));
        // A sibling directory whose name starts with the root's name is not *inside* it.
        assert_eq!(
            relative_to("/Users/x/repo-other/a.rs", "/Users/x/repo"),
            "/Users/x/repo-other/a.rs"
        );
    }

    #[test]
    fn the_first_matching_pattern_is_reported() {
        let pats = vec!["**/node_modules/**".to_string(), "**/*.min.js".to_string()];
        assert_eq!(
            ignored_by("a/node_modules/b.js", &pats).as_deref(),
            Some("**/node_modules/**")
        );
        assert_eq!(ignored_by("a/b.js", &pats), None);
    }

    #[test]
    fn evaluate_reports_the_first_gate_that_trips() {
        let pats = vec!["**/vendor/**".to_string()];
        assert_eq!(evaluate("ok", "/a/b.rs", 1000, &pats), None);
        assert_eq!(evaluate("a\0b", "/a/b.rs", 1000, &pats), Some(Skip::Binary));
        assert!(matches!(
            evaluate(&"x".repeat(50), "/a/b.rs", 10, &pats),
            Some(Skip::OverSize { .. })
        ));
        assert!(matches!(
            evaluate("ok", "/a/vendor/b.rs", 1000, &pats),
            Some(Skip::Ignored { .. })
        ));
        // Length is not a refusal: four hundred lines is an ordinary module, and refusing it
        // here would leave every large file with no findings, no lenses and no hints.
        assert!(evaluate(&"a\n".repeat(1000), "/a/b.rs", 100_000, &pats).is_none());
    }

    #[test]
    fn skip_reasons_are_human_readable_and_coded() {
        let s = Skip::OverSize { bytes: 20, limit: 10 };
        assert_eq!(s.code(), "over_size");
        assert!(s.reason().contains("20 bytes"));
        assert_eq!(Skip::Binary.code(), "binary");
        assert_eq!(Skip::Ignored { pattern: "x".into() }.code(), "ignored");
    }
}
