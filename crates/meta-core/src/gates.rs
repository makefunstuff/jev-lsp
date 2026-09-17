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
    TooManyLines { lines: u32, limit: u32 },
}

impl Skip {
    pub fn code(&self) -> &'static str {
        match self {
            Skip::Binary => "binary",
            Skip::OverSize { .. } => "over_size",
            Skip::Ignored { .. } => "ignored",
            Skip::TooManyLines { .. } => "over_size",
        }
    }

    pub fn reason(&self) -> String {
        match self {
            Skip::Binary => "the file looks binary".to_string(),
            Skip::OverSize { bytes, limit } => {
                format!("{bytes} bytes exceeds the {limit} byte analysis limit")
            }
            Skip::Ignored { pattern } => format!("path matches the ignore pattern {pattern:?}"),
            Skip::TooManyLines { lines, limit } => {
                format!("{lines} lines exceeds the {limit} line scope limit")
            }
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

/// The first ignore pattern that matches, if any.
pub fn ignored_by(path: &str, patterns: &[String]) -> Option<String> {
    patterns
        .iter()
        .find(|p| glob_match(p, path))
        .cloned()
}

/// Evaluate every gate for one document.
pub fn evaluate(
    text: &str,
    path: &str,
    max_file_bytes: u64,
    max_lines: u32,
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
    let lines = text.split('\n').count() as u32;
    if max_lines > 0 && lines > max_lines {
        return Some(Skip::TooManyLines {
            lines,
            limit: max_lines,
        });
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
        assert_eq!(evaluate("ok", "/a/b.rs", 1000, 100, &pats), None);
        assert_eq!(evaluate("a\0b", "/a/b.rs", 1000, 100, &pats), Some(Skip::Binary));
        assert!(matches!(
            evaluate(&"x".repeat(50), "/a/b.rs", 10, 100, &pats),
            Some(Skip::OverSize { .. })
        ));
        assert!(matches!(
            evaluate("ok", "/a/vendor/b.rs", 1000, 100, &pats),
            Some(Skip::Ignored { .. })
        ));
        assert!(matches!(
            evaluate("a\nb\nc", "/a/b.rs", 1000, 2, &pats),
            Some(Skip::TooManyLines { .. })
        ));
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
