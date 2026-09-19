//! Configuration, merged from defaults, `workspace/configuration`, and model-endpoint
//! environment overrides (PROTOCOL.md §10).

use crate::decision::Wire;
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
        }
    }
}

/// The decision tier. Not a chat tier: it speaks the decision wire (`crate::decision`), takes
/// one *decision* per call and generates no prose at all.
///
/// The ceilings are small on purpose. A decision produces one value per question, so sixty-four
/// tokens is generous and five seconds is a long time for it; the reason tier's numbers are
/// sized for a rewrite and would make every ambient pass wait for a budget nobody spends.
/// Pointing it at a local System One server is one config change away:
/// `base_url = "http://127.0.0.1:8009/v1"`, `model = "kev-latest"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DecisionTierConfig {
    pub wire: Wire,
    pub base_url: String,
    pub model: String,
    /// Name of the environment variable holding the key. Never the key itself.
    pub api_key_env: Option<String>,
    pub timeout_ms: u64,
    pub max_tokens: u32,
    pub temperature: f32,
    pub think: Think,
}

impl Default for DecisionTierConfig {
    fn default() -> Self {
        DecisionTierConfig {
            wire: Wire::SystemOne,
            base_url: "https://api.typesafe.ai/v1".to_string(),
            model: "jev-latest".to_string(),
            api_key_env: Some("TYPESAFE_API_KEY".to_string()),
            timeout_ms: 5000,
            max_tokens: 64,
            temperature: 0.0,
            think: Think::Off,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Models {
    pub reason: TierConfig,
    pub review: TierConfig,
    pub decide: DecisionTierConfig,
}

/// How the ambient pass behaves. The pass is a *rules* pass: inspections are milliseconds of
/// local work and the one decision call generates no tokens, which is what makes it viable on
/// every save.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RulesConfig {
    pub enabled: bool,
    /// Most candidates one rule may contribute to a pass. Bounds the questions a single regex
    /// can put to the model, and with them the size of the request.
    pub max_candidates_per_rule: usize,
    /// Lines of the file head shown to the decision. The decision reads a numbered excerpt,
    /// not the whole document: a decision that costs a megabyte of state is not cheap.
    pub max_state_lines: usize,
    pub max_state_bytes: usize,
    /// Documents one idle pass may cover. The remainder are reported, never dropped quietly.
    pub max_files_per_pass: usize,
}

impl Default for RulesConfig {
    fn default() -> Self {
        RulesConfig {
            enabled: true,
            max_candidates_per_rule: 8,
            max_state_lines: 200,
            max_state_bytes: 16_000,
            max_files_per_pass: 8,
        }
    }
}

/// When the rules pass runs. Separate from `triggers.diagnostics`, which governs the chat
/// review: the two passes cost nothing alike and a user may want one without the other.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RulesTrigger {
    pub on_save: bool,
    pub on_idle: bool,
    pub idle_ms: u64,
}

impl Default for RulesTrigger {
    fn default() -> Self {
        RulesTrigger {
            on_save: true,
            on_idle: true,
            idle_ms: 1500,
        }
    }
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
    pub rules: RulesTrigger,
}

impl Default for Triggers {
    fn default() -> Self {
        Triggers {
            diagnostics: "save".to_string(),
            idle_ms: 1500,
            severity_floor: "information".to_string(),
            rules: RulesTrigger::default(),
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
    pub ambient: Ambient,
    pub auto_apply: AutoApply,
    pub languages: Languages,
    pub noise: Noise,
    pub rules: RulesConfig,
    pub log: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            models: Models::default(),
            budget: BudgetConfig::default(),
            triggers: Triggers::default(),
            ambient: Ambient::default(),
            auto_apply: AutoApply::default(),
            languages: Languages::default(),
            noise: Noise::default(),
            rules: RulesConfig::default(),
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
        self.try_merged_with(value).unwrap_or_else(|_| self.clone())
    }

    /// [`merged_with`](Self::merged_with), but saying why it gave up.
    ///
    /// The fallback keeps the previous settings, which is the safe thing to do and also a
    /// silent one: a payload that fails to parse leaves the server on its defaults while the
    /// client believes it configured something. Callers that can report the error should use
    /// this and log it.
    pub fn try_merged_with(&self, value: Option<&serde_json::Value>) -> Result<Config, String> {
        let Some(patch) = value else {
            return Ok(self.clone());
        };
        if !patch.is_object() {
            return Ok(self.clone());
        }
        let mut base = serde_json::to_value(self).map_err(|e| e.to_string())?;
        deep_merge(&mut base, patch);
        serde_json::from_value(base).map_err(|e| e.to_string())
    }

    pub fn tier(&self, tier: crate::types::Tier) -> &TierConfig {
        match tier {
            crate::types::Tier::Reason => &self.models.reason,
            crate::types::Tier::Review => &self.models.review,
        }
    }

    /// The decision tier.
    ///
    /// Deliberately not a `Tier` variant: `tier()` answers a *chat* endpoint, and a decision is
    /// a different protocol entirely (System One, one value per question, no messages). A
    /// variant this accessor could not answer honestly would be a landmine for every caller
    /// that matches on the enum.
    pub fn decision(&self) -> &DecisionTierConfig {
        &self.models.decide
    }

    /// Model endpoints may be set by environment, because that is how a shell already
    /// knows where the local model lives (PROTOCOL.md §10 permits exactly this).
    ///
    /// Applied last, on every merge: an operator's environment is an explicit override and
    /// must not be undone by a client that has no opinion about model endpoints.
    ///
    /// `JEV_BASE_URL` deliberately does **not** touch the decision tier. It names an
    /// OpenAI-compatible chat server, and the decision tier speaks System One — pointing one at
    /// the other answers every decision with an HTTP error. The decision tier has its own pair
    /// of variables.
    pub fn apply_env_overrides(&mut self) {
        if let Ok(url) = std::env::var("JEV_BASE_URL") {
            if !url.trim().is_empty() {
                self.models.reason.base_url = url.clone();
                self.models.review.base_url = url;
            }
        }
        if let Ok(model) = std::env::var("JEV_MODEL") {
            if !model.trim().is_empty() {
                self.models.reason.model = model;
            }
        }
        if let Ok(model) = std::env::var("JEV_REVIEW_MODEL") {
            if !model.trim().is_empty() {
                self.models.review.model = model;
            }
        }
        if let Ok(url) = std::env::var("JEV_DECIDE_BASE_URL") {
            if !url.trim().is_empty() {
                self.models.decide.base_url = url;
            }
        }
        if let Ok(model) = std::env::var("JEV_DECIDE_MODEL") {
            if !model.trim().is_empty() {
                self.models.decide.model = model;
            }
        }
        if let Ok(name) = std::env::var("JEV_API_KEY_ENV") {
            let name = name.trim();
            if !name.is_empty() {
                // The value is the *name* of another variable, never a key — the convention
                // `api_key_env` already states ("never the key itself"): a key exported into the
                // environment is a key in every process listing.
                //
                // The chat tiers only. The decision tier keeps its own `api_key_env`: it speaks
                // a different wire and can sit behind a different provider, so one shell
                // variable must not silently repoint it at the wrong credential.
                self.models.reason.api_key_env = Some(name.to_string());
                self.models.review.api_key_env = Some(name.to_string());
            }
        }
        if let Ok(wire) = std::env::var("JEV_DECIDE_WIRE") {
            let value = wire.trim();
            if !value.is_empty() {
                // An unrecognised value is *ignored*, keeping whatever was in force, never
                // coerced to the default: a typo that quietly sent every decision to the wrong
                // path would be indistinguishable from the endpoint being down, and it would
                // be the operator's own spelling that was wrong. The value actually in force is
                // visible in `jev.status` (`models.decide.wire`) and in the server's
                // "settings applied" line, so a mistyped override can be seen there.
                if let Some(parsed) = Wire::parse(value) {
                    self.models.decide.wire = parsed;
                }
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
    fn the_payload_the_plugin_actually_sends_applies() {
        // Lifted verbatim from a debug trace of a client answering
        // `workspace/configuration`.
        // `inline_completion` and `models.fim` are still in it because that is what the client
        // sent, and a config section the server no longer has must be ignored rather than
        // rejected — an older client with a stale settings table still has to work.
        let payload = serde_json::json!({
            "enabled": true,
            "inline_completion": { "enabled": true },
            "models": {
                "fim": { "base_url": "http://127.0.0.1:37313/v1", "model": "qwen3.6-35b-a3b-iq3xxs", "timeout_ms": 30000 },
                "reason": { "base_url": "http://127.0.0.1:37313/v1", "model": "qwen3.6-35b-a3b-iq3xxs", "timeout_ms": 120000 },
                "review": { "base_url": "http://127.0.0.1:37313/v1", "model": "qwen3.6-35b-a3b-iq3xxs", "timeout_ms": 120000 }
            }
        });
        let merged = Config::default().merged_with(Some(&payload));
        assert_eq!(merged.models.reason.base_url, "http://127.0.0.1:37313/v1");
        assert_eq!(merged.models.reason.model, "qwen3.6-35b-a3b-iq3xxs");
        assert_eq!(merged.models.reason.timeout_ms, 120_000);
        assert_eq!(merged.models.review.base_url, "http://127.0.0.1:37313/v1");
    }

    #[test]
    fn the_environment_wins_over_the_client_payload() {
        // Guards the bug this test was written for: a client answering with {} used to
        // reset the endpoint the shell had provided.
        std::env::set_var("JEV_BASE_URL", "http://from-env:1234/v1");
        let merged = Config::default()
            .merged_with(Some(&serde_json::json!({})))
            .with_env_overrides();
        std::env::remove_var("JEV_BASE_URL");
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

    #[test]
    fn the_decision_tier_merges_without_disturbing_the_chat_tiers() {
        let mut c = Config::default();
        c.models.reason.base_url = "http://chat:1/v1".into();
        c.models.review.base_url = "http://chat:1/v1".into();
        let merged = c.merged_with(Some(&serde_json::json!({
            "models": {"decide": {"base_url": "http://decide:2/v1", "model": "kev-latest"}}
        })));
        assert_eq!(merged.decision().base_url, "http://decide:2/v1");
        assert_eq!(merged.decision().model, "kev-latest");
        assert_eq!(
            merged.models.reason.base_url, "http://chat:1/v1",
            "a decision endpoint is not a chat endpoint"
        );
        assert_eq!(merged.models.review.base_url, "http://chat:1/v1");
        // The rest of the decision tier survives a patch that mentions only one key.
        assert_eq!(merged.decision().timeout_ms, c.decision().timeout_ms);
        assert_eq!(merged.decision().wire, c.decision().wire);
    }

    #[test]
    fn the_chat_tiers_key_variable_is_nameable_from_the_environment() {
        // Without this a shell can point the chat tiers at a hosted endpoint and still have no
        // way to give them a credential: `api_key_env` is a *name*, and nothing else in the
        // environment could supply one.
        std::env::set_var("JEV_API_KEY_ENV", "OPENROUTER_API_KEY");
        let mut c = Config::default();
        c.apply_env_overrides();
        std::env::remove_var("JEV_API_KEY_ENV");
        assert_eq!(c.models.reason.api_key_env.as_deref(), Some("OPENROUTER_API_KEY"));
        assert_eq!(c.models.review.api_key_env.as_deref(), Some("OPENROUTER_API_KEY"));
        assert_eq!(
            c.decision().api_key_env.as_deref(),
            Some("TYPESAFE_API_KEY"),
            "the decision tier names its own variable and is not repointed by this one"
        );

        // Empty is not an opinion, exactly as for the neighbouring variables.
        std::env::set_var("JEV_API_KEY_ENV", "   ");
        let mut held = Config::default();
        held.models.reason.api_key_env = Some("KEPT".to_string());
        held.apply_env_overrides();
        std::env::remove_var("JEV_API_KEY_ENV");
        assert_eq!(held.models.reason.api_key_env.as_deref(), Some("KEPT"));

        // And a decide-tier override in the same pass leaves it alone.
        std::env::set_var("JEV_API_KEY_ENV", "OPENROUTER_API_KEY");
        std::env::set_var("JEV_DECIDE_BASE_URL", "https://openrouter.ai/api");
        let mut both = Config::default();
        both.apply_env_overrides();
        std::env::remove_var("JEV_DECIDE_BASE_URL");
        std::env::remove_var("JEV_API_KEY_ENV");
        assert_eq!(both.models.review.api_key_env.as_deref(), Some("OPENROUTER_API_KEY"));
        assert_eq!(both.decision().api_key_env.as_deref(), Some("TYPESAFE_API_KEY"));
        assert_eq!(both.decision().base_url, "https://openrouter.ai/api");
    }

    #[test]
    fn the_decision_wire_is_overridable_by_environment_in_both_spellings() {
        // The wire decides the path, so reaching a provider's own endpoint depends on this
        // being settable from the shell — `JEV_DECIDE_BASE_URL=https://openrouter.ai/api` with
        // the default wire would POST `/api/systemone`, which is not the endpoint.
        for (value, want) in [
            ("system_one", Wire::SystemOne),
            ("systemone", Wire::SystemOne),
            ("open_router", Wire::OpenRouter),
            ("openrouter", Wire::OpenRouter),
            ("  OpenRouter  ", Wire::OpenRouter),
        ] {
            std::env::set_var("JEV_DECIDE_WIRE", value);
            let mut c = Config::default();
            c.apply_env_overrides();
            std::env::remove_var("JEV_DECIDE_WIRE");
            assert_eq!(c.decision().wire, want, "{value:?}");
        }

        // A typo keeps the previous value: silently coercing it to the default would send
        // requests somewhere the operator never asked for.
        let mut held = Config::default();
        held.models.decide.wire = Wire::OpenRouter;
        std::env::set_var("JEV_DECIDE_WIRE", "openroute");
        held.apply_env_overrides();
        std::env::remove_var("JEV_DECIDE_WIRE");
        assert_eq!(held.decision().wire, Wire::OpenRouter, "an unrecognised value is ignored");

        // And whitespace is not an opinion, exactly as for the neighbouring variables.
        std::env::set_var("JEV_DECIDE_WIRE", "   ");
        let mut blank = Config::default();
        blank.apply_env_overrides();
        std::env::remove_var("JEV_DECIDE_WIRE");
        assert_eq!(blank.decision().wire, Wire::SystemOne);
    }

    #[test]
    fn a_payload_that_disables_rules_is_honoured() {
        // The ambient pass is the rules pass by default, so this key is the one that turns the
        // whole ambient path off. If it did not merge, `rules.enabled = false` would be a
        // setting a client sends and the server ignores.
        let c = Config::default().merged_with(Some(&serde_json::json!({
            "rules": {"enabled": false}
        })));
        assert!(!c.rules.enabled);
        assert_eq!(c.rules.max_candidates_per_rule, 8, "its siblings survived");
        assert!(c.triggers.rules.on_save, "and a different subtree was untouched");
    }

    #[test]
    fn the_decision_and_rules_defaults_are_what_this_code_says() {
        let c = Config::default();
        assert_eq!(c.decision().wire, crate::decision::Wire::SystemOne);
        assert_eq!(c.decision().base_url, "https://api.typesafe.ai/v1");
        assert_eq!(c.decision().model, "jev-latest");
        assert_eq!(c.decision().api_key_env.as_deref(), Some("TYPESAFE_API_KEY"));
        assert_eq!(c.decision().timeout_ms, 5000);
        assert_eq!(c.decision().max_tokens, 64);
        assert_eq!(c.decision().temperature, 0.0);
        assert_eq!(c.decision().think, Think::Off);
        assert!(c.rules.enabled, "the ambient pass is the rules pass by default");
        assert_eq!(c.rules.max_candidates_per_rule, 8);
        assert_eq!(c.rules.max_state_lines, 200);
        assert_eq!(c.rules.max_state_bytes, 16_000);
        assert_eq!(c.rules.max_files_per_pass, 8);
        assert!(c.triggers.rules.on_save);
        assert!(c.triggers.rules.on_idle);
        assert_eq!(c.triggers.rules.idle_ms, 1500);
    }

    #[test]
    fn the_environment_overrides_the_decision_tier_alone() {
        // `JEV_BASE_URL` names an OpenAI-compatible chat server; pointing the decision tier at
        // it would break every decision. Only the decide-specific pair moves it.
        //
        // `JEV_BASE_URL` is deliberately not set here: another test in this module owns it, and
        // tests run on threads that share one environment.
        std::env::set_var("JEV_DECIDE_BASE_URL", "http://decision:2/v1");
        std::env::set_var("JEV_DECIDE_MODEL", "kev-latest");
        let mut c = Config::default();
        c.apply_env_overrides();
        std::env::remove_var("JEV_DECIDE_MODEL");
        std::env::remove_var("JEV_DECIDE_BASE_URL");
        assert_eq!(c.decision().base_url, "http://decision:2/v1");
        assert_eq!(c.decision().model, "kev-latest");

        // Empty is not an opinion, exactly as for the existing variables.
        let mut blank = Config::default();
        std::env::set_var("JEV_DECIDE_BASE_URL", "   ");
        blank.apply_env_overrides();
        std::env::remove_var("JEV_DECIDE_BASE_URL");
        assert_eq!(blank.decision().base_url, "https://api.typesafe.ai/v1");
    }
}
