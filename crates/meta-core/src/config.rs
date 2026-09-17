//! Configuration, merged from defaults, `workspace/configuration`, and model-endpoint
//! environment overrides (PROTOCOL.md §10).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Think {
    Off,
    Low,
    Medium,
    High,
}

impl Think {
    pub fn as_effort(self) -> Option<&'static str> {
        match self {
            Think::Off => None,
            Think::Low => Some("low"),
            Think::Medium => Some("medium"),
            Think::High => Some("high"),
        }
    }
}

/// Fill-in-the-middle markers, for a server that expects a raw continuation prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FimTokens {
    pub prefix: String,
    pub suffix: String,
    pub middle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TierConfig {
    pub base_url: String,
    pub model: String,
    /// Name of the environment variable holding the key. Never the key itself.
    pub api_key_env: Option<String>,
    pub timeout_ms: u64,
    pub max_tokens: u32,
    pub temperature: f32,
    pub think: Think,
    /// Only meaningful for the `fim` tier; absent means the instruction form is used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fim_tokens: Option<FimTokens>,
}

impl Default for TierConfig {
    fn default() -> Self {
        TierConfig {
            base_url: "http://127.0.0.1:8080/v1".to_string(),
            model: "qwen2.5-coder-7b-instruct".to_string(),
            api_key_env: None,
            // Measured: a reasoning model answering an 8192-token ceiling took over 60 s on a
            // Rust rewrite, so a 30 s cap turned a slow-but-valid answer into a transport
            // error. This is a ceiling too — the call returns as soon as the model stops.
            timeout_ms: 90_000,
            max_tokens: 8192,
            temperature: 0.0,
            think: Think::Off,
            fim_tokens: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Models {
    pub fim: TierConfig,
    pub reason: TierConfig,
    pub review: TierConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetConfig {
    pub max_calls_per_min: u32,
    pub max_calls_per_hour: u32,
    pub max_tokens_per_session: u64,
    pub timeout_ms: u64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        BudgetConfig {
            max_calls_per_min: 6,
            max_calls_per_hour: 120,
            max_tokens_per_session: 500_000,
            timeout_ms: 30_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Triggers {
    /// `save`, `idle`, or `off`.
    pub diagnostics: String,
    pub idle_ms: u64,
    pub severity_floor: String,
}

impl Default for Triggers {
    fn default() -> Self {
        Triggers {
            diagnostics: "save".to_string(),
            idle_ms: 1500,
            severity_floor: "information".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InlineConfig {
    pub enabled: bool,
    pub idle_ms: u64,
    pub min_prefix_chars: u32,
    pub max_calls_per_min: u32,
}

impl Default for InlineConfig {
    fn default() -> Self {
        InlineConfig {
            enabled: false,
            idle_ms: 400,
            min_prefix_chars: 8,
            max_calls_per_min: 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Ambient {
    pub code_lens: bool,
    pub inlay_hints: bool,
    pub diagnostics: bool,
}

impl Default for Ambient {
    fn default() -> Self {
        Ambient {
            code_lens: true,
            inlay_hints: false,
            diagnostics: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoApply {
    pub fix: bool,
    #[serde(rename = "fixAll")]
    pub fix_all: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Languages {
    pub overrides: std::collections::BTreeMap<String, LanguageOverride>,
    pub max_file_bytes: u64,
    pub max_scope_lines: u32,
    pub ignore: Vec<String>,
}

impl Default for Languages {
    fn default() -> Self {
        Languages {
            overrides: std::collections::BTreeMap::new(),
            max_file_bytes: 1024 * 1024,
            max_scope_lines: 400,
            ignore: vec![
                "**/node_modules/**".to_string(),
                "**/*.min.js".to_string(),
                "**/vendor/**".to_string(),
            ],
        }
    }
}

/// May narrow the verb set and pick a tier. It can never disable a language
/// (docs/LANGUAGE.md §7).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LanguageOverride {
    pub prompt: Option<String>,
    pub tier: Option<crate::types::Tier>,
    pub verbs: Option<Vec<crate::types::Verb>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Noise {
    pub max_visible_findings: usize,
    pub suppress_after_dismissals: u32,
}

impl Default for Noise {
    fn default() -> Self {
        Noise {
            max_visible_findings: 5,
            suppress_after_dismissals: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub models: Models,
    pub budget: BudgetConfig,
    pub triggers: Triggers,
    pub inline_completion: InlineConfig,
    pub ambient: Ambient,
    pub auto_apply: AutoApply,
    pub languages: Languages,
    pub noise: Noise,
    pub log: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            models: Models::default(),
            budget: BudgetConfig::default(),
            triggers: Triggers::default(),
            inline_completion: InlineConfig::default(),
            ambient: Ambient::default(),
            auto_apply: AutoApply::default(),
            languages: Languages::default(),
            noise: Noise::default(),
            log: "warn".to_string(),
        }
    }
}

/// Recursively overlay `patch` onto `base`, so a client that mentions one setting does not
/// silently reset the others to their defaults.
fn deep_merge(base: &mut serde_json::Value, patch: &serde_json::Value) {
    match (base, patch) {
        (serde_json::Value::Object(b), serde_json::Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(slot) => deep_merge(slot, v),
                    None => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, p) => *b = p.clone(),
    }
}

impl Config {
    /// Overlay a `workspace/configuration` payload onto the current settings.
    ///
    /// The payload is merged, never substituted: a client that sets one key keeps every
    /// other setting — including the model endpoints — exactly as it was. A malformed
    /// payload is ignored rather than fatal, because a bad settings table must not take
    /// the editor's language server down.
    pub fn merged_with(&self, value: Option<&serde_json::Value>) -> Config {
        let Some(patch) = value else {
            return self.clone();
        };
        if !patch.is_object() {
            return self.clone();
        }
        let mut base = match serde_json::to_value(self) {
            Ok(v) => v,
            Err(_) => return self.clone(),
        };
        deep_merge(&mut base, patch);
        serde_json::from_value(base).unwrap_or_else(|_| self.clone())
    }

    pub fn tier(&self, tier: crate::types::Tier) -> &TierConfig {
        match tier {
            crate::types::Tier::Fim => &self.models.fim,
            crate::types::Tier::Reason => &self.models.reason,
            crate::types::Tier::Review => &self.models.review,
        }
    }

    /// Model endpoints may be set by environment, because that is how a shell already
    /// knows where the local model lives (PROTOCOL.md §10 permits exactly this).
    ///
    /// Applied last, on every merge: an operator's environment is an explicit override and
    /// must not be undone by a client that has no opinion about model endpoints.
    pub fn apply_env_overrides(&mut self) {
        if let Ok(url) = std::env::var("META_BASE_URL") {
            if !url.trim().is_empty() {
                self.models.reason.base_url = url.clone();
                self.models.review.base_url = url.clone();
                self.models.fim.base_url = url;
            }
        }
        if let Ok(model) = std::env::var("META_MODEL") {
            if !model.trim().is_empty() {
                self.models.reason.model = model.clone();
                self.models.fim.model = model;
            }
        }
        if let Ok(model) = std::env::var("META_REVIEW_MODEL") {
            if !model.trim().is_empty() {
                self.models.review.model = model;
            }
        }
    }

    pub fn with_env_overrides(mut self) -> Config {
        self.apply_env_overrides();
        self
    }

    /// Verbs offered for a language. Config may narrow; defaults are the full set.
    pub fn verbs_for(&self, language: &str) -> Vec<crate::types::Verb> {
        self.languages
            .overrides
            .get(language)
            .and_then(|o| o.verbs.clone())
            .unwrap_or_else(|| crate::types::Verb::ALL.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Verb;

    #[test]
    fn defaults_are_conservative() {
        let c = Config::default();
        assert!(c.enabled);
        assert!(!c.inline_completion.enabled, "inline completion ships off");
        assert!(!c.auto_apply.fix, "auto-apply ships off");
        assert!(!c.ambient.inlay_hints);
        assert_eq!(c.triggers.diagnostics, "save");
    }

    #[test]
    fn partial_settings_keep_the_other_defaults() {
        let c = Config::default();
        let merged = c.merged_with(Some(&serde_json::json!({"enabled": false, "log": "debug"})));
        assert!(!merged.enabled);
        assert_eq!(merged.log, "debug");
        assert_eq!(merged.budget.max_calls_per_min, c.budget.max_calls_per_min);
    }

    #[test]
    fn a_nested_patch_leaves_its_siblings_alone() {
        let mut c = Config::default();
        c.models.reason.base_url = "http://saved:9999/v1".into();
        c.models.review.base_url = "http://saved:9999/v1".into();
        c.budget.max_calls_per_hour = 42;
        let merged = c.merged_with(Some(&serde_json::json!({
            "budget": {"max_calls_per_min": 1}
        })));
        assert_eq!(merged.budget.max_calls_per_min, 1, "the patch applied");
        assert_eq!(merged.budget.max_calls_per_hour, 42, "its sibling survived");
        assert_eq!(
            merged.models.reason.base_url, "http://saved:9999/v1",
            "an unrelated subtree survived"
        );
    }

    #[test]
    fn an_empty_payload_changes_nothing() {
        let mut c = Config::default();
        c.models.reason.base_url = "http://kept:1/v1".into();
        assert_eq!(c.merged_with(Some(&serde_json::json!({}))).models.reason.base_url, "http://kept:1/v1");
        assert_eq!(c.merged_with(None).models.reason.base_url, "http://kept:1/v1");
    }

    #[test]
    fn the_environment_wins_over_the_client_payload() {
        // Guards the bug this test was written for: a client answering with {} used to
        // reset the endpoint the shell had provided.
        std::env::set_var("META_BASE_URL", "http://from-env:1234/v1");
        let merged = Config::default()
            .merged_with(Some(&serde_json::json!({})))
            .with_env_overrides();
        std::env::remove_var("META_BASE_URL");
        assert_eq!(merged.models.reason.base_url, "http://from-env:1234/v1");
        assert_eq!(merged.models.review.base_url, "http://from-env:1234/v1");
    }

    #[test]
    fn a_malformed_payload_is_ignored_not_fatal() {
        let c = Config::default();
        let merged = c.merged_with(Some(&serde_json::json!({"enabled": "yes please"})));
        assert!(merged.enabled, "fell back to defaults");
        assert!(c.merged_with(None).enabled);
    }

    #[test]
    fn full_verb_set_is_the_default_for_every_language() {
        let c = Config::default();
        for lang in ["rust", "python", "markdown", "unknown"] {
            assert_eq!(c.verbs_for(lang).len(), Verb::ALL.len(), "{lang}");
        }
    }

    #[test]
    fn config_may_narrow_a_language_but_never_disable_it() {
        let c = Config::default().merged_with(Some(&serde_json::json!({
            "languages": { "overrides": { "markdown": { "verbs": ["review"] } } }
        })));
        assert_eq!(c.verbs_for("markdown"), vec![Verb::Review]);
        assert_eq!(c.verbs_for("rust").len(), Verb::ALL.len());
    }

    #[test]
    fn tier_configs_round_trip_through_json() {
        let c = Config::default();
        let v = serde_json::to_value(&c).unwrap();
        let back: Config = serde_json::from_value(v).unwrap();
        assert_eq!(back.models.reason.model, c.models.reason.model);
        assert_eq!(back.budget.max_calls_per_hour, c.budget.max_calls_per_hour);
    }
}
