//! Parsing and validation of model output (docs/MODEL.md §4).
//!
//! The model emits content and anchors. It never emits ranges or document versions — those
//! are computed here and stamped by the LSP layer, which is what makes PROTOCOL.md N5
//! enforceable rather than aspirational.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct RawAnchor {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// The text the anchor points at. Must occur exactly once in the document.
    #[serde(rename = "match")]
    pub needle: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawReplacement {
    pub anchor: RawAnchor,
    pub replacement: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawNewFile {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawEdit {
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub replacements: Vec<RawReplacement>,
    #[serde(default)]
    pub new_files: Vec<RawNewFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawFinding {
    pub anchor: RawAnchor,
    #[serde(default)]
    pub severity: Option<String>,
    pub label: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub verb_hint: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawFindings {
    #[serde(default)]
    pub findings: Vec<RawFinding>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawArtifact {
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub markdown: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawPlanStep {
    pub title: String,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub verb: Option<String>,
    #[serde(default)]
    pub anchors: Vec<RawAnchor>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawPlan {
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub steps: Vec<RawPlanStep>,
}

/// Why a contract failed. Fed back to the model verbatim on a repair attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    NoJson,
    NotJson(String),
}

impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContractError::NoJson => write!(f, "no JSON object found in the response"),
            ContractError::NotJson(e) => write!(f, "response is not valid JSON: {e}"),
        }
    }
}

impl std::error::Error for ContractError {}

/// Extract the outermost JSON object from a response that may carry prose or a code fence.
///
/// Models wrap output in ```json fences or add a sentence before it; both are recoverable
/// without a retry, and a retry costs a budgeted call.
pub fn extract_json(response: &str) -> Result<&str, ContractError> {
    let start = response.find('{').ok_or(ContractError::NoJson)?;
    let end = response.rfind('}').ok_or(ContractError::NoJson)?;
    if end <= start {
        return Err(ContractError::NoJson);
    }
    Ok(&response[start..=end])
}

fn parse<T: for<'de> Deserialize<'de>>(response: &str) -> Result<T, ContractError> {
    let json = extract_json(response)?;
    serde_json::from_str(json).map_err(|e| ContractError::NotJson(e.to_string()))
}

pub fn parse_edit(response: &str) -> Result<RawEdit, ContractError> {
    parse(response)
}

pub fn parse_findings(response: &str) -> Result<RawFindings, ContractError> {
    parse(response)
}

pub fn parse_artifact(response: &str) -> Result<RawArtifact, ContractError> {
    parse(response)
}

pub fn parse_plan(response: &str) -> Result<RawPlan, ContractError> {
    parse(response)
}

/// Severity names the findings contract accepts. `error` is deliberately absent: it is
/// reserved for divergence the server can prove (PROTOCOL.md §9).
pub fn parse_severity(s: Option<&str>) -> crate::types::Severity {
    match s.unwrap_or("warning").trim().to_ascii_lowercase().as_str() {
        "information" | "info" | "hint" | "note" => crate::types::Severity::Information,
        _ => crate::types::Severity::Warning,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_json_parses() {
        let e = parse_edit(r#"{"summary":"s","replacements":[]}"#).unwrap();
        assert_eq!(e.summary.as_deref(), Some("s"));
    }

    #[test]
    fn fenced_and_prose_wrapped_json_parses() {
        let fenced = "Here you go:\n```json\n{\"summary\":\"s\"}\n```\nHope that helps.";
        assert_eq!(parse_edit(fenced).unwrap().summary.as_deref(), Some("s"));
    }

    #[test]
    fn missing_json_is_a_contract_error_not_a_panic() {
        assert_eq!(extract_json("no braces here"), Err(ContractError::NoJson));
        assert_eq!(extract_json("}{"), Err(ContractError::NoJson));
    }

    #[test]
    fn malformed_json_is_a_contract_error_carrying_the_parser_message() {
        match parse_edit("{oops}") {
            Err(ContractError::NotJson(m)) => assert!(!m.is_empty()),
            other => panic!("expected NotJson, got {other:?}"),
        }
    }

    #[test]
    fn missing_optional_fields_default_instead_of_failing() {
        let f = parse_findings(r#"{"findings":[{"anchor":{"match":"x"},"label":"l"}]}"#).unwrap();
        assert_eq!(f.findings.len(), 1);
        assert!(f.findings[0].severity.is_none());
        let e = parse_edit("{}").unwrap();
        assert!(e.replacements.is_empty());
        assert!(e.new_files.is_empty());
    }

    #[test]
    fn severity_is_conservative_and_never_error() {
        use crate::types::Severity;
        assert_eq!(parse_severity(None), Severity::Warning);
        assert_eq!(parse_severity(Some("information")), Severity::Information);
        assert_eq!(parse_severity(Some("INFO")), Severity::Information);
        assert_eq!(parse_severity(Some("error")), Severity::Warning);
        assert_eq!(parse_severity(Some("nonsense")), Severity::Warning);
    }
}
