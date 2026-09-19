//! Documents and the content-hash keying that everything else depends on.
//!
//! The client owns text (PROTOCOL.md N9). These are mirrors, keyed by content hash so that
//! two editors holding the same content at different versions share one cache entry, and a
//! reverted file hits its own earlier entry.

use crate::lang::Language;
use sha2::{Digest, Sha256};

/// Stable content identity. Used in cache keys and staleness checks.
pub fn content_hash(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    let d = h.finalize();
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Turn a `file://` URI into a filesystem path. Never panics on odd input.
pub fn path_from_uri(uri: &str) -> String {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let decoded = percent_decode(raw);
    match decoded.find('/') {
        // file://host/path is not a shape we produce, but tolerate it.
        Some(0) => decoded,
        Some(_) => decoded[decoded.find('/').unwrap_or(0)..].to_string(),
        None => decoded,
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug, Clone)]
pub struct Document {
    pub uri: String,
    pub version: i32,
    pub text: String,
    pub hash: String,
    pub path: String,
    pub language: Language,
}

impl Document {
    pub fn new(uri: &str, version: i32, text: String, language_id: Option<&str>) -> Document {
        let path = path_from_uri(uri);
        let language = Language::of(language_id, &path, &text);
        let hash = content_hash(&text);
        Document {
            uri: uri.to_string(),
            version,
            text,
            hash,
            path,
            language,
        }
    }

    /// Replace the text with a full document synchronisation payload.
    pub fn replace(&mut self, version: i32, text: String) {
        self.version = version;
        self.hash = content_hash(&text);
        self.text = text;
        // Re-resolve only when the client did not tell us: a richer client answer wins.
        if self.language.is_unknown() {
            self.language = Language::of(None, &self.path, &self.text);
        }
    }

    /// Apply incremental changes (UTF-8 byte offsets, matching PROTOCOL.md N1).
    pub fn apply_incremental(&mut self, version: i32, changes: &[(u32, u32, u32, u32, String)]) {
        for (start_line, start_col, end_line, end_col, new_text) in changes {
            let Some(range) = byte_range(&self.text, *start_line, *start_col, *end_line, *end_col)
            else {
                continue;
            };
            if range.0 <= range.1 && range.1 <= self.text.len() {
                self.text.replace_range(range.0..range.1, new_text);
            }
        }
        self.version = version;
        self.hash = content_hash(&self.text);
        if self.language.is_unknown() {
            self.language = Language::of(None, &self.path, &self.text);
        }
    }

    pub fn line_count(&self) -> u32 {
        self.text.split('\n').count() as u32
    }
}

fn line_start(text: &str, line: u32) -> Option<usize> {
    if line == 0 {
        return Some(0);
    }
    let mut seen = 0u32;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            seen += 1;
            if seen == line {
                return Some(i + 1);
            }
        }
    }
    None
}

fn byte_range(
    text: &str,
    start_line: u32,
    start_col: u32,
    end_line: u32,
    end_col: u32,
) -> Option<(usize, usize)> {
    let s = line_start(text, start_line)?;
    let e = line_start(text, end_line)?;
    let start = s + start_col as usize;
    let end = e + end_col as usize;
    if start > text.len() || end > text.len() || start > end {
        return None;
    }
    if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return None;
    }
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_content_sensitive() {
        assert_eq!(content_hash("abc"), content_hash("abc"));
        assert_ne!(content_hash("abc"), content_hash("abd"));
        assert_eq!(content_hash("abc").len(), 64);
    }

    #[test]
    fn uris_become_paths() {
        assert_eq!(path_from_uri("file:///tmp/a.rs"), "/tmp/a.rs");
        assert_eq!(path_from_uri("file:///tmp/a%20b.rs"), "/tmp/a b.rs");
        assert_eq!(path_from_uri("/tmp/a.rs"), "/tmp/a.rs");
    }

    #[test]
    fn language_comes_from_the_client_first() {
        let d = Document::new("file:///tmp/x.py", 1, "print(1)".into(), Some("python"));
        assert_eq!(d.language.name, "python");
        let d2 = Document::new("file:///tmp/x.py", 1, "print(1)".into(), Some(""));
        assert_eq!(d2.language.name, "python", "falls back to the extension");
    }

    #[test]
    fn incremental_changes_apply_on_utf8_offsets() {
        let mut d = Document::new("file:///tmp/a.rs", 1, "let x = 1;\nlet y = 2;\n".into(), Some("rust"));
        d.apply_incremental(2, &[(1, 4, 1, 5, "z".to_string())]);
        assert_eq!(d.text, "let x = 1;\nlet z = 2;\n");
        assert_eq!(d.version, 2);
    }

    #[test]
    fn multi_byte_content_survives_incremental_edits() {
        let mut d = Document::new("file:///tmp/a.rs", 1, "// ünïcode\n".into(), Some("rust"));
        d.apply_incremental(2, &[(0, 0, 0, 2, "//!".to_string())]);
        assert_eq!(d.text, "//! ünïcode\n");
    }

    #[test]
    fn out_of_bounds_changes_are_ignored_not_panicked() {
        let mut d = Document::new("file:///tmp/a.rs", 1, "a\n".into(), Some("rust"));
        d.apply_incremental(2, &[(99, 0, 99, 0, "x".to_string())]);
        assert_eq!(d.text, "a\n");
        d.apply_incremental(3, &[(0, 9, 0, 9, "x".to_string())]);
        assert_eq!(d.text, "a\n", "a column past the line end is refused");
    }

    #[test]
    fn an_unknown_language_is_retried_when_content_changes() {
        let mut d = Document::new("file:///tmp/mystery", 1, "nothing".into(), Some(""));
        assert!(d.language.is_unknown());
        d.replace(2, "#!/usr/bin/env python3\n".to_string());
        assert_eq!(d.language.name, "python");
    }
}
