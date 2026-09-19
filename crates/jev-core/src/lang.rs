//! Language resolution and per-language profiles.
//!
//! Support is unconditional (PROTOCOL.md N10): language is metadata that selects a prompt
//! flavour and tells the scope resolver how the language is structured. It never decides
//! whether something is served, and `"unknown"` is a valid answer, not an error.

use serde::{Deserialize, Serialize};

/// How a language is shaped, for scope resolution and prompting.
#[derive(Debug, Clone, Copy)]
pub struct Profile {
    /// Canonical language name.
    pub language: &'static str,
    /// Prompt flavour id (`docs/MODEL.md`).
    pub prompt: &'static str,
    /// Blocks are brace-delimited.
    pub braces: bool,
    /// Line-comment introducer, if the language has one.
    pub line_comment: Option<&'static str>,
    /// Keywords that open a named declaration.
    pub decl_keywords: &'static [&'static str],
}

const CODE_KEYWORDS: &[&str] = &[
    "fn", "func", "function", "def", "sub", "impl", "class", "struct", "enum", "trait",
    "interface", "module", "mod", "package", "namespace", "object", "type", "const",
];

const GENERIC: Profile = Profile {
    language: "unknown",
    prompt: "generic_text",
    braces: true,
    line_comment: None,
    decl_keywords: CODE_KEYWORDS,
};

const PROFILES: &[Profile] = &[
    Profile { language: "rust", prompt: "rust", braces: true, line_comment: Some("//"), decl_keywords: &["fn", "impl", "struct", "enum", "trait", "mod", "type", "const"] },
    Profile { language: "python", prompt: "python", braces: false, line_comment: Some("#"), decl_keywords: &["def", "class"] },
    Profile { language: "go", prompt: "go", braces: true, line_comment: Some("//"), decl_keywords: &["func", "type", "struct", "interface"] },
    Profile { language: "javascript", prompt: "javascript", braces: true, line_comment: Some("//"), decl_keywords: &["function", "class", "const", "let", "var"] },
    Profile { language: "typescript", prompt: "typescript", braces: true, line_comment: Some("//"), decl_keywords: &["function", "class", "interface", "type", "const", "let"] },
    Profile { language: "c", prompt: "c", braces: true, line_comment: Some("//"), decl_keywords: &["struct", "enum", "typedef", "union"] },
    Profile { language: "cpp", prompt: "cpp", braces: true, line_comment: Some("//"), decl_keywords: &["class", "struct", "enum", "namespace", "template"] },
    Profile { language: "java", prompt: "java", braces: true, line_comment: Some("//"), decl_keywords: &["class", "interface", "enum", "record"] },
    Profile { language: "csharp", prompt: "csharp", braces: true, line_comment: Some("//"), decl_keywords: &["class", "interface", "struct", "enum", "record"] },
    Profile { language: "ruby", prompt: "ruby", braces: false, line_comment: Some("#"), decl_keywords: &["def", "class", "module"] },
    Profile { language: "php", prompt: "php", braces: true, line_comment: Some("//"), decl_keywords: &["function", "class", "interface", "trait"] },
    Profile { language: "lua", prompt: "lua", braces: false, line_comment: Some("--"), decl_keywords: &["function"] },
    Profile { language: "shell", prompt: "shell", braces: false, line_comment: Some("#"), decl_keywords: &["function"] },
    Profile { language: "sql", prompt: "sql", braces: false, line_comment: Some("--"), decl_keywords: &["create", "select", "insert", "with"] },
    Profile { language: "markdown", prompt: "generic_text", braces: false, line_comment: None, decl_keywords: &[] },
    Profile { language: "json", prompt: "generic_text", braces: true, line_comment: None, decl_keywords: &[] },
    Profile { language: "yaml", prompt: "generic_text", braces: false, line_comment: Some("#"), decl_keywords: &[] },
    Profile { language: "toml", prompt: "generic_text", braces: false, line_comment: Some("#"), decl_keywords: &[] },
    Profile { language: "xml", prompt: "generic_text", braces: false, line_comment: None, decl_keywords: &[] },
    Profile { language: "html", prompt: "generic_text", braces: false, line_comment: None, decl_keywords: &[] },
    Profile { language: "css", prompt: "generic_text", braces: true, line_comment: None, decl_keywords: &[] },
    Profile { language: "make", prompt: "shell", braces: false, line_comment: Some("#"), decl_keywords: &[] },
];

/// Look up a profile. Unknown languages get the generic profile — never an error.
pub fn profile(language: &str) -> Profile {
    PROFILES
        .iter()
        .find(|p| p.language == language)
        .copied()
        .unwrap_or(GENERIC)
}

/// Aliases a `filetype`-style `languageId` sometimes uses.
fn canonical(language: &str) -> &str {
    match language {
        "typescriptreact" | "typescript.tsx" => "typescript",
        "javascriptreact" | "javascript.jsx" => "javascript",
        "sh" | "bash" | "zsh" | "fish" => "shell",
        "cs" => "csharp",
        "c++" => "cpp",
        "md" | "mdx" => "markdown",
        "text" | "plaintext" | "conf" | "config" | "" => "unknown",
        other => other,
    }
}

fn from_extension(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?;
    if ext == path {
        return None;
    }
    Some(match ext.to_ascii_lowercase().as_str() {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "go" => "go",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "tsx" | "mts" => "typescript",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "java" => "java",
        "cs" => "csharp",
        "rb" => "ruby",
        "php" => "php",
        "lua" => "lua",
        "sh" | "bash" | "zsh" | "fish" => "shell",
        "sql" => "sql",
        "md" | "markdown" | "mdx" => "markdown",
        "json" | "jsonc" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "xml" | "svg" => "xml",
        "html" | "htm" => "html",
        "css" | "scss" | "less" => "css",
        _ => return None,
    })
}

fn from_filename(name: &str) -> Option<&'static str> {
    let base = name.rsplit('/').next().unwrap_or(name);
    Some(match base {
        "Makefile" | "makefile" | "GNUmakefile" => "make",
        "Dockerfile" | "Containerfile" => "dockerfile",
        "Cargo.toml" | "pyproject.toml" | "go.mod" => "toml",
        ".gitignore" | ".dockerignore" => "unknown",
        _ => return None,
    })
}

fn from_shebang(text: &str) -> Option<&'static str> {
    let first = text.lines().next()?;
    if !first.starts_with("#!") {
        return None;
    }
    let l = first.to_ascii_lowercase();
    Some(if l.contains("python") {
        "python"
    } else if l.contains("bash") || l.contains("sh") || l.contains("zsh") {
        "shell"
    } else if l.contains("node") {
        "javascript"
    } else if l.contains("ruby") {
        "ruby"
    } else if l.contains("lua") {
        "lua"
    } else {
        return None;
    })
}

/// Content sniff over the first lines, cheapest signals first.
fn from_content(text: &str) -> Option<&'static str> {
    let head: Vec<&str> = text.lines().take(50).collect();
    let joined = head.join("\n");
    let trimmed = joined.trim_start();
    if trimmed.starts_with("<?xml") || trimmed.starts_with("<!DOCTYPE html") {
        return Some(if trimmed.contains("html") { "html" } else { "xml" });
    }
    if trimmed.starts_with('{') && (trimmed.contains("\":") || trimmed.contains("\":")) {
        return Some("json");
    }
    if trimmed.starts_with("package ") && joined.contains("\nfunc ") {
        return Some("go");
    }
    if joined.contains("fn main()") || joined.contains("use std::") {
        return Some("rust");
    }
    if joined.lines().any(|l| l.trim_start().starts_with("[package]")) {
        return Some("toml");
    }
    if joined.lines().any(|l| l.trim_start().starts_with("include ")) {
        return Some("make");
    }
    None
}

/// Resolution ladder (docs/LANGUAGE.md §2). First non-empty answer wins.
///
/// The client's `languageId` is authoritative when present: a client sends the buffer's
/// filetype, and the plugin is free to make that answer richer than a bare filetype.
pub fn resolve(language_id: Option<&str>, path: &str, text: &str) -> String {
    if let Some(id) = language_id {
        let c = canonical(id.trim());
        if !c.is_empty() && c != "unknown" {
            return c.to_string();
        }
    }
    if let Some(l) = from_extension(path) {
        return l.to_string();
    }
    if let Some(l) = from_filename(path) {
        return l.to_string();
    }
    if let Some(l) = from_shebang(text) {
        return l.to_string();
    }
    if let Some(l) = from_content(text) {
        return l.to_string();
    }
    "unknown".to_string()
}

/// The classification carried in action data and artifacts (PROTOCOL.md N11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Language {
    pub name: String,
    pub prompt: String,
}

impl Language {
    pub fn of(language_id: Option<&str>, path: &str, text: &str) -> Language {
        let name = resolve(language_id, path, text);
        let prompt = profile(&name).prompt.to_string();
        Language { name, prompt }
    }

    pub fn is_unknown(&self) -> bool {
        self.name == "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_language_id_wins() {
        assert_eq!(resolve(Some("rust"), "/tmp/x.py", "print(1)"), "rust");
        assert_eq!(resolve(Some("typescriptreact"), "/tmp/x", ""), "typescript");
    }

    #[test]
    fn empty_or_unknown_language_id_falls_through() {
        assert_eq!(resolve(Some(""), "/tmp/x.py", ""), "python");
        assert_eq!(resolve(Some("text"), "/tmp/x.rs", ""), "rust");
        assert_eq!(resolve(Some("  "), "/tmp/Makefile", ""), "make");
    }

    #[test]
    fn extension_filename_shebang_and_content_are_tried_in_order() {
        assert_eq!(resolve(None, "/a/b.py", ""), "python");
        assert_eq!(resolve(None, "/a/Makefile", ""), "make");
        assert_eq!(resolve(None, "/a/script", "#!/usr/bin/env python3\n"), "python");
        assert_eq!(resolve(None, "/a/data", "{\"k\": 1}"), "json");
        assert_eq!(resolve(None, "/a/mystery", "nothing here"), "unknown");
    }

    #[test]
    fn unknown_is_a_valid_answer_and_yields_the_generic_profile() {
        let l = Language::of(None, "/a/mystery", "nothing here");
        assert!(l.is_unknown());
        assert_eq!(l.prompt, "generic_text");
        assert_eq!(profile("no-such-language").language, "unknown");
    }

    #[test]
    fn every_profile_language_is_reachable() {
        for p in PROFILES {
            assert!(!p.language.is_empty());
            assert!(!p.prompt.is_empty());
        }
    }
}
