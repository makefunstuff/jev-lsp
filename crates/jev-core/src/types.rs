//! Shared vocabulary for the whole server. No LSP, no HTTP, no async.

use serde::{Deserialize, Serialize};
use sha2::Digest;

/// Version of the action `data` payload (PROTOCOL.md §4).
pub const ACTION_DATA_VERSION: u32 = 1;
/// Schema version stamped on every artifact (PROTOCOL.md §7).
pub const ARTIFACT_SCHEMA: &str = "jev.artifact/1";
/// Schema version stamped on every command result (PROTOCOL.md §7).
pub const RESULT_SCHEMA: &str = "jev.result/1";
/// Prompt template revision. Part of every cache key and of determinism checks.
///
/// Bumped whenever the wording or the schema changes, so a conclusion produced by an older
/// prompt can never be served as if it came from the current one.
pub const PROMPT_VERSION: &str = "4";

/// What a verb does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verb {
    Fix,
    #[serde(rename = "fixAll")]
    FixAll,
    Harden,
    Types,
    Docs,
    Rewrite,
    Test,
    Generate,
    Explain,
    Review,
}

impl Verb {
    pub const ALL: [Verb; 10] = [
        Verb::Fix,
        Verb::FixAll,
        Verb::Harden,
        Verb::Types,
        Verb::Docs,
        Verb::Rewrite,
        Verb::Test,
        Verb::Generate,
        Verb::Explain,
        Verb::Review,
    ];

    /// Stable identifier, used in `data.verb`, cache keys, and config.
    pub fn as_str(self) -> &'static str {
        match self {
            Verb::Fix => "fix",
            Verb::FixAll => "fixAll",
            Verb::Harden => "harden",
            Verb::Types => "types",
            Verb::Docs => "docs",
            Verb::Rewrite => "rewrite",
            Verb::Test => "test",
            Verb::Generate => "generate",
            Verb::Explain => "explain",
            Verb::Review => "review",
        }
    }

    pub fn parse(s: &str) -> Option<Verb> {
        Verb::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// The code action kind advertised (PROTOCOL.md §2, §4.1).
    pub fn kind(self) -> &'static str {
        match self {
            Verb::Fix => "quickfix.jev",
            Verb::FixAll => "source.fixAll",
            Verb::Explain | Verb::Review => "source.jev",
            _ => "refactor.rewrite.jev",
        }
    }

    /// Which model tier serves this verb (docs/MODEL.md §2).
    pub fn tier(self) -> Tier {
        match self {
            Verb::Review => Tier::Review,
            _ => Tier::Reason,
        }
    }

    /// What the model is asked to produce.
    pub fn output(self) -> Output {
        match self {
            Verb::Review => Output::Findings,
            Verb::Explain => Output::Artifact,
            _ => Output::Edit,
        }
    }

    /// Deterministic human label, paired with the scope name for the action title
    /// (PROTOCOL.md §4 — titles are never model output).
    pub fn label(self) -> &'static str {
        match self {
            Verb::Fix => "Fix",
            Verb::FixAll => "Fix all findings",
            Verb::Harden => "Harden edge cases",
            Verb::Types => "Add type annotations",
            Verb::Docs => "Document",
            Verb::Rewrite => "Rewrite",
            Verb::Test => "Add tests",
            Verb::Generate => "Generate",
            Verb::Explain => "Explain",
            Verb::Review => "Review this",
        }
    }

    /// May be applied without a separate approval (PROTOCOL.md §4.1).
    pub fn auto_applicable(self) -> bool {
        matches!(self, Verb::Fix | Verb::FixAll)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Reason,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    Edit,
    Findings,
    Artifact,
}

/// Lifecycle of an action, surfaced to the user (PROTOCOL.md §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionState {
    Ready,
    Pending,
    Stale,
    OverBudget,
    Failed,
}

impl ActionState {
    pub fn is_ready(self) -> bool {
        matches!(self, ActionState::Ready)
    }
}

/// The document a piece of work was computed against. Stamped on every edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocRef {
    pub uri: String,
    pub version: i32,
    pub content_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Function,
    Method,
    Class,
    Impl,
    Module,
    Block,
    Statement,
    File,
    Selection,
}

impl ScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ScopeKind::Function => "function",
            ScopeKind::Method => "method",
            ScopeKind::Class => "class",
            ScopeKind::Impl => "impl",
            ScopeKind::Module => "module",
            ScopeKind::Block => "block",
            ScopeKind::Statement => "statement",
            ScopeKind::File => "file",
            ScopeKind::Selection => "selection",
        }
    }

    pub fn parse(s: &str) -> Option<ScopeKind> {
        Some(match s {
            "function" => ScopeKind::Function,
            "method" => ScopeKind::Method,
            "class" => ScopeKind::Class,
            "impl" => ScopeKind::Impl,
            "module" => ScopeKind::Module,
            "block" => ScopeKind::Block,
            "statement" => ScopeKind::Statement,
            "file" => ScopeKind::File,
            "selection" => ScopeKind::Selection,
            _ => return None,
        })
    }
}

/// How the scope was obtained. A quality flag, never a refusal (docs/LANGUAGE.md §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeSource {
    Tree,
    Structural,
    WholeFile,
    Explicit,
}

/// A zero-based, half-open line range. Columns are byte offsets within the line,
/// because the server selects UTF-8 encoding (PROTOCOL.md N1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRange {
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeRef {
    pub kind: ScopeKind,
    pub name: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Information,
    Warning,
}

/// A finding produced by the review tier (PROTOCOL.md §9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub line: u32,
    pub start_col: u32,
    pub end_col: u32,
    pub severity: Severity,
    pub label: String,
    pub detail: String,
    pub verb_hint: Verb,
}

/// A single replacement, in whole-line terms. `end_col` excludes the trailing newline, so
/// the newline itself is preserved and `new_text` never needs one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextOp {
    pub start_line: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFile {
    pub path: String,
    pub content: String,
}

/// A validated proposal from the model, ready to become a `WorkspaceEdit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub summary: String,
    pub rationale: String,
    pub ops: Vec<TextOp>,
    pub new_files: Vec<NewFile>,
}

impl Proposal {
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty() && self.new_files.is_empty()
    }
}

/// A step of a plan (PROTOCOL.md §7). A step carries no edit: it is applied later against
/// the content that is live *then*, which is why a plan survives unrelated editing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub n: u32,
    pub title: String,
    pub rationale: String,
    pub verb: Verb,
    /// Where the step intends to work, as text rather than offsets.
    pub targets: Vec<PlanTarget>,
    pub status: StepStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanTarget {
    pub uri: String,
    /// The document version the plan was computed against (PROTOCOL.md §7).
    pub version: i32,
    /// Resolved when the plan was built; the step is re-anchored when it is applied.
    pub line: u32,
    pub match_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Proposed,
    Applied,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub model: String,
    pub tier: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub goal: String,
    pub language: String,
    pub steps: Vec<PlanStep>,
    pub usage: Usage,
}

/// The round-tripped payload carried by every code action (PROTOCOL.md §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionData {
    pub v: u32,
    pub id: String,
    pub verb: Verb,
    pub state: ActionState,
    pub doc: DocRef,
    pub scope: ScopeRef,
    pub scope_source: ScopeSource,
    pub language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finding: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl ActionData {
    /// Stable identity: identical inputs must produce an identical id (PROTOCOL.md §4).
    pub fn make_id(verb: Verb, doc: &DocRef, scope: &ScopeRef, finding: Option<&str>) -> String {
        let mut h = sha2::Sha256::new();
        h.update(verb.as_str().as_bytes());
        h.update(b"\x00");
        h.update(doc.content_hash.as_bytes());
        h.update(b"\x00");
        h.update(scope.kind.as_str().as_bytes());
        h.update(b"\x00");
        h.update(scope.name.as_deref().unwrap_or("").as_bytes());
        h.update(scope.start_line.to_le_bytes());
        h.update(b"\x00");
        h.update(finding.unwrap_or("").as_bytes());
        let digest = h.finalize();
        digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verb_strings_round_trip() {
        for v in Verb::ALL {
            assert_eq!(Verb::parse(v.as_str()), Some(v));
            serde_json::from_str::<Verb>(&format!("\"{}\"", v.as_str())).unwrap();
        }
    }

    #[test]
    fn action_ids_are_stable_and_discriminating() {
        let doc = DocRef {
            uri: "file:///a.rs".into(),
            version: 1,
            content_hash: "abc".into(),
        };
        let scope = ScopeRef {
            kind: ScopeKind::Function,
            name: Some("parse".into()),
            start_line: 3,
            end_line: 9,
        };
        let a = ActionData::make_id(Verb::Harden, &doc, &scope, None);
        let b = ActionData::make_id(Verb::Harden, &doc, &scope, None);
        assert_eq!(a, b, "identical inputs must produce identical ids");

        let c = ActionData::make_id(Verb::Fix, &doc, &scope, None);
        assert_ne!(a, c);
        let d = ActionData::make_id(Verb::Harden, &doc, &scope, Some("f1"));
        assert_ne!(a, d);
    }

    #[test]
    fn action_data_serializes_with_frozen_field_names() {
        let data = ActionData {
            v: ACTION_DATA_VERSION,
            id: "deadbeef".into(),
            verb: Verb::Harden,
            state: ActionState::Ready,
            doc: DocRef {
                uri: "file:///a.rs".into(),
                version: 7,
                content_hash: "hash".into(),
            },
            scope: ScopeRef {
                kind: ScopeKind::Function,
                name: Some("retry".into()),
                start_line: 1,
                end_line: 4,
            },
            scope_source: ScopeSource::Structural,
            language: "rust".into(),
            finding: None,
            summary: None,
        };
        let json = serde_json::to_value(&data).unwrap();
        assert_eq!(json["verb"], "harden");
        assert_eq!(json["state"], "ready");
        assert_eq!(json["scope_source"], "structural");
        assert_eq!(json["doc"]["version"], 7);
        assert!(json.get("finding").is_none(), "absent optionals are omitted");
        // Round-trips through the wire form the client sends back to resolve.
        let back: ActionData = serde_json::from_value(json).unwrap();
        assert_eq!(back, data);
    }
}
