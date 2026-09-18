//! Orchestration: gates, budget, cache, prompt, model call, validation.
//!
//! This is the whole product pipeline, minus the transport. It is deliberately free of LSP
//! types so it can be exercised in-process.

use crate::state::AppState;
use meta_core::budget::{Budget, Refusal};
use meta_core::cache::{self, Conclusion};
use meta_core::config::Config;
use meta_core::context;
use meta_core::contract;
use meta_core::document::Document;
use meta_core::edit::{self, BuildOptions};
use meta_core::findings;
use meta_core::gates;
use meta_core::model::{ChatRequest, ChatResponse};
use meta_core::scope::{self, Resolved};
use meta_core::types::{Finding, Proposal, Tier, Usage, Verb, PROMPT_VERSION};
use meta_core::verbs;

/// Where generated text goes while it is being generated.
///
/// `None` means nothing until the answer is complete, which is what an *edit* needs: half a
/// JSON object is not a preview, and an edit that is streamed is an edit that cannot be
/// validated before someone sees it. Prose is the case that wants streaming.
type Delta<'a> = Option<&'a mut dyn FnMut(&str)>;

/// Reborrow, so a caller can hand the same callback to more than one call.
fn reborrow<'a, 'b>(delta: &'a mut Delta<'b>) -> Delta<'a>
where
    'b: 'a,
{
    delta.as_mut().map(|f| &mut **f as &mut dyn FnMut(&str))
}
use std::sync::Arc;

/// A successful generation.
#[derive(Debug, Clone)]
pub enum Generated {
    Edit(Proposal),
    Artifact(String),
}

/// Why an operation produced nothing. Every variant carries something showable.
///
/// Staleness is deliberately *not* here: it is an action state, not a failure
/// (PROTOCOL.md §8 rule 3), and the caller decides it before calling in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// A gate or a disabled feature. Not an error; the reason is still shown.
    Skipped(String),
    Refused(Refusal),
    /// The target moved or changed since the work was requested.
    Stale,
    Model(String),
    Contract(String),
    Edit(String),
}

impl Failure {
    pub fn message(&self) -> String {
        match self {
            Failure::Skipped(r) => r.clone(),
            Failure::Refused(r) => r.reason().to_string(),
            Failure::Stale => "the target changed since this was prepared".to_string(),
            Failure::Model(m) => format!("model call failed: {m}"),
            Failure::Contract(m) => format!("the model did not honour the response contract: {m}"),
            Failure::Edit(m) => format!("the proposed change was rejected: {m}"),
        }
    }

    /// Short code carried in the action data for programmatic use.
    pub fn code(&self) -> &'static str {
        match self {
            Failure::Skipped(_) => "skipped",
            Failure::Refused(_) => "over_budget",
            Failure::Stale => "stale",
            Failure::Model(_) => "model_error",
            Failure::Contract(_) => "contract_error",
            Failure::Edit(_) => "rejected_edit",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub findings: Vec<Finding>,
    pub from_cache: bool,
    /// Findings the review tier produced that could not be located in the document.
    pub rejected: usize,
}

/// Releases the budget permit however the call ends, including on `?` early return.
struct Permit<'a>(&'a Budget);

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// How many times a rejected answer is retried before giving up (docs/MODEL.md §5).
///
/// Measured against a real model: one attempt is not enough. A model that answers with the
/// wrong shape tends to answer with it again, and a second, differently-worded complaint
/// converts a meaningful share of those. The cost is bounded either way — two extra calls at
/// most, each taking its own budget permit.
const MAX_REPAIR_ATTEMPTS: usize = 2;

/// Build the follow-up request for a rejected answer.
///
/// The original context is re-sent unchanged so the model is not asked to work from a
/// summary, and the previous answer is quoted back with the parser's own complaint — which
/// is far more actionable than "your JSON was wrong".
fn repair_context(original: &context::Context, error: &str, previous: &str) -> context::Context {
    let quoted: String = previous.chars().take(1200).collect();
    let mut repaired = original.clone();
    repaired.code = original.code.clone();
    repaired.findings = original.findings.clone();
    repaired.around = original.around.clone();
    // Carry the retry instruction in the field the prompt renders last, so the rules and
    // the code are still visible above it.
    repaired.scope_name = original.scope_name.clone();
    let mut block = String::new();
    block.push_str("YOUR PREVIOUS ANSWER WAS REJECTED.\n");
    block.push_str(&format!("Reason: {error}\n"));
    block.push_str("Return the same JSON shape again, and change nothing else.\n");
    block.push_str("Previous answer, quoted:\n");
    block.push_str(&quoted);
    repaired.around = Some(match original.around.clone() {
        Some(a) => format!("{a}\n{block}"),
        None => block,
    });
    repaired
}

/// How much of the document either side of the cursor is shown to the model.
const PREFIX_WINDOW: usize = 2000;
const SUFFIX_WINDOW: usize = 500;
/// A completion is a line or two. More than this is the model writing the file.
const MAX_COMPLETION_LINES: usize = 4;

/// Byte offset of a position, or `None` if it is out of range or not a boundary.
fn byte_offset(text: &str, line: u32, character: u32) -> Option<usize> {
    let mut start = 0usize;
    let mut seen = 0u32;
    if line > 0 {
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                seen += 1;
                if seen == line {
                    start = i + 1;
                    break;
                }
            }
        }
        if seen != line {
            return None;
        }
    }
    let offset = start + character as usize;
    if offset > text.len() || !text.is_char_boundary(offset) {
        return None;
    }
    Some(offset)
}

/// The last `n` characters.
fn tail(text: &str, n: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(n)).collect()
}

/// The first `n` characters.
fn head(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// Strip what a model wraps completions in, and stop it writing the rest of the file.
fn clean_completion(raw: &str) -> String {
    let mut text = raw.trim_start_matches(['\n', '\r']).to_string();
    if text.starts_with("```") {
        // Drop the opening fence and its language tag, then the closing fence.
        if let Some(nl) = text.find('\n') {
            text = text[nl + 1..].to_string();
        }
        if let Some(end) = text.rfind("```") {
            text.truncate(end);
        }
    }
    let mut lines: Vec<&str> = text.lines().collect();
    lines.truncate(MAX_COMPLETION_LINES);
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n").trim_end().to_string()
}

pub struct Engine {
    pub state: Arc<AppState>,
}

impl Engine {
    pub fn new(state: Arc<AppState>) -> Engine {
        Engine { state }
    }

    fn config(&self) -> Config {
        self.state.config()
    }

    /// The scope used for whole-document review.
    fn review_scope(doc: &Document) -> Resolved {
        let last = doc.line_count().saturating_sub(1);
        Resolved {
            range: meta_core::types::LineRange {
                start_line: 0,
                end_line: last,
            },
            kind: meta_core::types::ScopeKind::File,
            source: meta_core::types::ScopeSource::WholeFile,
            name: None,
            truncated: false,
        }
    }

    /// Cached findings for the document's *current* content, if any.
    pub fn cached(&self, doc: &Document) -> Option<Arc<Conclusion>> {
        self.state.cache.get(&cache::findings_key(&doc.hash))
    }

    /// Document-level findings, from cache or freshly produced.
    ///
    /// This is the ambient path: it never runs inline with a user gesture, and a failure
    /// here is reported as a skip rather than surfaced as an error.
    pub fn analyze(&self, doc: &Document) -> Result<Outcome, Failure> {
        let cfg = self.config();
        if !cfg.enabled {
            return Err(Failure::Skipped("meta is stopped".to_string()));
        }
        if !cfg.ambient.diagnostics {
            return Err(Failure::Skipped(
                "ambient diagnostics are turned off in settings".to_string(),
            ));
        }
        if let Some(skip) = gates::evaluate(
            &doc.text,
            &doc.path,
            cfg.languages.max_file_bytes,
            cfg.languages.max_scope_lines,
            &cfg.languages.ignore,
        ) {
            return Err(Failure::Skipped(skip.reason()));
        }

        if let Some(hit) = self.cached(doc) {
            return Ok(Outcome {
                findings: hit.findings.clone(),
                from_cache: true,
                rejected: 0,
            });
        }

        let scope = Self::review_scope(doc);
        let ctx = context::build(doc, &scope, &[], 0);
        let (raw, _usage) = self.with_repair(
            &cfg,
            Verb::Review.tier(),
            &ctx,
            |c| verbs::render(Verb::Review, c),
            contract::parse_findings,
            None,
        )?;
        let built = findings::build(&doc.text, &raw, &meta_core::lang::profile(&doc.language.name));

        self.state.cache.put(
            &cache::findings_key(&doc.hash),
            Conclusion {
                findings: built.findings.clone(),
                ..Default::default()
            },
        );

        Ok(Outcome {
            findings: built.findings,
            from_cache: false,
            rejected: built.rejected,
        })
    }

    /// Produce an edit (or an artifact) for one verb and scope.
    pub fn generate(
        &self,
        doc: &Document,
        verb: Verb,
        scope: &Resolved,
        findings_in_scope: &[Finding],
    ) -> Result<Generated, Failure> {
        self.generate_streaming(doc, verb, scope, findings_in_scope, None)
    }

    /// The same generation, with the answer reported as it arrives.
    ///
    /// The stream is a *preview*: what the caller finally receives is the artifact this
    /// returns, and a repair attempt — which only ever happens for artifacts — means the
    /// preview was the first, rejected attempt. The Result is authoritative; the stream is
    /// what makes the wait visible.
    pub fn generate_streaming(
        &self,
        doc: &Document,
        verb: Verb,
        scope: &Resolved,
        findings_in_scope: &[Finding],
        mut delta: Delta<'_>,
    ) -> Result<Generated, Failure> {
        let cfg = self.config();
        if !cfg.enabled {
            return Err(Failure::Skipped("meta is stopped".to_string()));
        }
        if let Some(skip) = gates::evaluate(
            &doc.text,
            &doc.path,
            cfg.languages.max_file_bytes,
            cfg.languages.max_scope_lines,
            &cfg.languages.ignore,
        ) {
            return Err(Failure::Skipped(skip.reason()));
        }

        let key = cache::op_key(
            verb.as_str(),
            PROMPT_VERSION,
            &doc.hash,
            scope.range.start_line,
            scope.range.end_line,
        );
        if let Some(hit) = self.state.cache.get(&key) {
            if let Some(p) = &hit.edit {
                return Ok(Generated::Edit(p.clone()));
            }
            if let Some(a) = &hit.artifact {
                return Ok(Generated::Artifact(a.clone()));
            }
        }

        let ctx = context::build(doc, scope, findings_in_scope, 12);

        let generated = match verb.output() {
            meta_core::types::Output::Artifact => {
                let (raw, _usage) = self.with_repair(
                    &cfg,
                    verb.tier(),
                    &ctx,
                    |c| verbs::render(verb, c),
                    contract::parse_artifact,
                    reborrow(&mut delta),
                )?;
                if raw.markdown.trim().is_empty() {
                    return Err(Failure::Contract("artifact carried no text".to_string()));
                }
                Generated::Artifact(raw.markdown)
            }
            _ => {
                let profile = meta_core::lang::profile(&doc.language.name);
                let opts = BuildOptions {
                    max_scope_lines: cfg.languages.max_scope_lines,
                };
                // Both kinds of failure are repaired, within one shared budget: the response
                // not being valid JSON, and the answer not being applicable to this document
                // (an anchor that cannot be located, or one whose replacement re-emits the
                // lines that follow it). docs/MODEL.md §5 specifies the second kind, and a
                // real model produced it in 2 of 12 soak runs — the model is never otherwise
                // told that its answer was unusable, and re-prompting with the reason fixes it.
                let proposal;
                let mut attempt_ctx = ctx.clone();
                let mut repairs = 0usize;
                loop {
                    let attempt = self.chat_with(&cfg, verb.tier(), verbs::render(verb, &attempt_ctx), None)?;
                    let refusal = match contract::parse_edit(&attempt.text) {
                        Err(e) => e.to_string(),
                        Ok(raw) => match edit::build_proposal(&doc.text, &raw, &profile, &opts) {
                            Ok(built) => {
                                proposal = built;
                                break;
                            }
                            Err(e) => e.to_string(),
                        },
                    };
                    if repairs >= MAX_REPAIR_ATTEMPTS {
                        return Err(Failure::Edit(format!(
                            "{refusal} (after {MAX_REPAIR_ATTEMPTS} repair attempt(s))"
                        )));
                    }
                    repairs += 1;
                    attempt_ctx = repair_context(&ctx, &refusal, &attempt.text);
                }
                Generated::Edit(proposal)
            }
        };

        let conclusion = match &generated {
            Generated::Edit(p) => Conclusion {
                edit: Some(p.clone()),
                ..Default::default()
            },
            Generated::Artifact(a) => Conclusion {
                artifact: Some(a.clone()),
                ..Default::default()
            },
        };
        self.state.cache.put(&key, conclusion);

        Ok(generated)
    }

    /// The lowest-level model call. Every call takes a permit, including a repair attempt:
    /// two calls are two calls (PROTOCOL.md §5).
    fn chat_with(
        &self,
        cfg: &Config,
        tier_kind: Tier,
        spec: verbs::PromptSpec,
        delta: Delta<'_>,
    ) -> Result<ChatResponse, Failure> {
        let _permit = match self.state.budget.try_acquire(&cfg.budget) {
            meta_core::budget::Permit::Granted => Permit(&self.state.budget),
            meta_core::budget::Permit::Refused(r) => return Err(Failure::Refused(r)),
        };
        let tier = cfg.tier(tier_kind);
        let max_tokens = match tier.max_tokens {
            0 => spec.max_tokens,
            ceiling => spec.max_tokens.min(ceiling).max(64),
        };
        let request = ChatRequest {
            system: spec.system,
            user: spec.user,
            temperature: tier.temperature,
            max_tokens,
            json: spec.json,
            think: tier.think,
        };
        let response = match delta {
            Some(cb) => self.state.backend.chat_stream(tier, &request, cb),
            None => self.state.backend.chat(tier, &request),
        }
        .map_err(|e| Failure::Model(e.to_string()))?;
        self.state.budget.record_tokens(response.total_tokens());
        Ok(response)
    }

    /// One model call, plus at most [`MAX_REPAIR_ATTEMPTS`] repair calls if the answer does
    /// not satisfy its contract (docs/MODEL.md §5), with what the attempt cost.
    ///
    /// A real model produced a contract violation in roughly one run in six — a prose
    /// answer with no JSON at all, or a schema echoed verbatim. Re-prompting with the
    /// parser's own complaint fixes most of them for the price of one more call, and the
    /// attempt is budgeted like any other.
    fn with_repair<T>(
        &self,
        cfg: &Config,
        tier_kind: Tier,
        ctx: &context::Context,
        render: impl Fn(&context::Context) -> verbs::PromptSpec,
        parse: impl Fn(&str) -> Result<T, contract::ContractError>,
        mut delta: Delta<'_>,
    ) -> Result<(T, Usage), Failure> {
        let started = std::time::Instant::now();
        let response = self.chat_with(cfg, tier_kind, render(ctx), reborrow(&mut delta))?;
        let mut tokens_in = response.prompt_tokens;
        let mut tokens_out = response.completion_tokens;

        let first_error = match parse(&response.text) {
            Ok(value) => {
                return Ok((
                    value,
                    Usage {
                        model: cfg.tier(tier_kind).model.clone(),
                        tier: "reason".to_string(),
                        tokens_in,
                        tokens_out,
                        ms: started.elapsed().as_millis() as u64,
                        changes: None,
                        files: None,
                    },
                ))
            }
            Err(e) => e,
        };

        let mut last = first_error;
        for _ in 0..MAX_REPAIR_ATTEMPTS {
            let repair = repair_context(ctx, &last.to_string(), &response.text);
            let retry = self.chat_with(cfg, tier_kind, render(&repair), reborrow(&mut delta))?;
            tokens_in += retry.prompt_tokens;
            tokens_out += retry.completion_tokens;
            match parse(&retry.text) {
                Ok(value) => {
                    return Ok((
                        value,
                        Usage {
                            model: cfg.tier(tier_kind).model.clone(),
                            tier: "reason".to_string(),
                            tokens_in,
                            tokens_out,
                            ms: started.elapsed().as_millis() as u64,
                            changes: None,
                            files: None,
                        },
                    ))
                }
                Err(e) => last = e,
            }
        }
        Err(Failure::Contract(format!(
            "{last} (after {MAX_REPAIR_ATTEMPTS} repair attempt(s))"
        )))
    }

    /// Scope for a cursor position in a document.
    pub fn scope_at(&self, doc: &Document, line: u32, explicit: Option<meta_core::types::LineRange>) -> Resolved {
        let cfg = self.config();
        let profile = meta_core::lang::profile(&doc.language.name);
        scope::resolve(
            &doc.text,
            line,
            &profile,
            explicit,
            cfg.languages.max_scope_lines,
        )
    }

    /// A fill-in-the-middle completion at a cursor position (PROTOCOL.md §3.2, §5).
    ///
    /// The gate order matters and is asserted by tests: cheap refusals come first, the
    /// expensive one last. Nothing here is allowed to be slow — the client fires this on a
    /// 200 ms timer while the user types.
    ///
    /// `invoked` means the user pressed a key rather than the client firing on a timer:
    /// an explicit request is not second-guessed by the prefix floor, because they asked.
    pub fn complete(
        &self,
        doc: &Document,
        line: u32,
        character: u32,
        invoked: bool,
    ) -> Result<String, Failure> {
        let cfg = self.config();
        if !cfg.enabled {
            return Err(Failure::Skipped("meta is stopped".to_string()));
        }
        if !cfg.inline_completion.enabled {
            return Err(Failure::Skipped("inline completion is off".to_string()));
        }
        if let Some(skip) = gates::evaluate(
            &doc.text,
            &doc.path,
            cfg.languages.max_file_bytes,
            cfg.languages.max_scope_lines,
            &cfg.languages.ignore,
        ) {
            return Err(Failure::Skipped(skip.reason()));
        }

        let offset = byte_offset(&doc.text, line, character)
            .ok_or_else(|| Failure::Skipped("the cursor is not in the document".to_string()))?;
        let prefix = &doc.text[..offset];
        let suffix = &doc.text[offset..];

        // Do not fire on trivial context: a completion for every keystroke is noise.
        let non_whitespace = prefix
            .chars()
            .rev()
            .take(PREFIX_WINDOW)
            .filter(|c| !c.is_whitespace())
            .count();
        if !invoked && (non_whitespace as u32) < cfg.inline_completion.min_prefix_chars {
            return Err(Failure::Skipped(
                "not enough context before the cursor".to_string(),
            ));
        }

        // The same cursor in the same content is the same question.
        let key = cache::op_key("completion", PROMPT_VERSION, &doc.hash, line, character);
        if let Some(hit) = self.state.cache.get(&key) {
            if let Some(text) = &hit.artifact {
                return Ok(text.clone());
            }
        }

        if !self
            .state
            .fim
            .try_acquire(cfg.inline_completion.max_calls_per_min)
        {
            return Err(Failure::Refused(meta_core::budget::Refusal::PerMinute));
        }

        let spec = verbs::render_completion(
            &doc.language.prompt,
            &doc.path,
            &tail(prefix, PREFIX_WINDOW),
            &head(suffix, SUFFIX_WINDOW),
            cfg.models.fim.fim_tokens.as_ref(),
        );
        let response = self.chat_with(&cfg, Tier::Fim, spec, None)?;
        let text = clean_completion(&response.text);
        self.state.cache.put(
            &key,
            Conclusion {
                artifact: Some(text.clone()),
                ..Default::default()
            },
        );
        Ok(text)
    }

    /// Turn a goal into a plan (PROTOCOL.md §6, §7). A plan holds no edits: each step is
    /// applied later, against the content live at that moment.
    pub fn plan(&self, doc: &Document, scope: &Resolved, goal: &str) -> Result<(meta_core::types::Plan, usize), Failure> {
        let cfg = self.config();
        if !cfg.enabled {
            return Err(Failure::Skipped("meta is stopped".to_string()));
        }
        if goal.trim().is_empty() {
            return Err(Failure::Skipped(
                "a plan needs a goal; `:Meta plan` asks for one".to_string(),
            ));
        }
        if let Some(skip) = gates::evaluate(
            &doc.text,
            &doc.path,
            cfg.languages.max_file_bytes,
            cfg.languages.max_scope_lines,
            &cfg.languages.ignore,
        ) {
            return Err(Failure::Skipped(skip.reason()));
        }

        let ctx = context::build(doc, scope, &[], 0);
        let (raw, usage) = self.with_repair(
            &cfg,
            Tier::Reason,
            &ctx,
            |c| verbs::render_plan(c, goal),
            contract::parse_plan,
            None,
        )?;
        let built = meta_core::plan::build(
            &doc.uri,
            doc.version,
            goal,
            &doc.language.name,
            &doc.text,
            &raw,
            usage,
        );
        if built.plan.steps.is_empty() {
            return Err(Failure::Contract(
                "the plan contained no step whose target could be located".to_string(),
            ));
        }
        Ok((built.plan, built.rejected))
    }

    /// Apply one step of a plan to the *current* content.
    ///
    /// The plan recorded the text each step aimed at; if that text is no longer present, or
    /// is no longer unique, the step is `Stale` rather than being applied to whatever
    /// happens to be at that line now.
    pub fn apply_step(
        &self,
        plan: &meta_core::types::Plan,
        n: u32,
        doc: &Document,
        findings: &[Finding],
    ) -> Result<Proposal, Failure> {
        let step = plan
            .steps
            .iter()
            .find(|s| s.n == n)
            .ok_or_else(|| Failure::Skipped(format!("the plan has no step {n}")))?;
        let target = step
            .targets
            .iter()
            .find(|t| t.uri == doc.uri)
            .ok_or_else(|| Failure::Stale)?;

        let hits = doc.text.match_indices(target.match_text.as_str()).count();
        if hits != 1 {
            return Err(Failure::Stale);
        }
        let line = doc
            .text
            .find(target.match_text.as_str())
            .map(|offset| doc.text[..offset].bytes().filter(|b| *b == b'\n').count() as u32)
            .ok_or(Failure::Stale)?;

        let scope = self.scope_at(doc, line, None);
        match self.generate(doc, step.verb, &scope, findings)? {
            Generated::Edit(proposal) => Ok(proposal),
            Generated::Artifact(_) => Err(Failure::Skipped(
                "that step produces an artifact, not an edit".to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meta_core::config::TierConfig;
    use meta_core::model::{Backend, ChatResponse};
    use parking_lot::Mutex;

    /// A backend that returns queued responses and records the requests it saw.
    struct Scripted {
        responses: Mutex<Vec<String>>,
        seen: Mutex<Vec<ChatRequest>>,
    }

    impl Scripted {
        fn new(responses: &[&str]) -> Arc<Scripted> {
            Arc::new(Scripted {
                responses: Mutex::new(responses.iter().rev().map(|s| s.to_string()).collect()),
                seen: Mutex::new(Vec::new()),
            })
        }
    }

    impl Backend for Scripted {
        fn chat(&self, _c: &TierConfig, r: &ChatRequest) -> anyhow::Result<ChatResponse> {
            self.seen.lock().push(r.clone());
            let text = self
                .responses
                .lock()
                .pop()
                .unwrap_or_else(|| "{}".to_string());
            Ok(ChatResponse {
                text,
                prompt_tokens: 10,
                completion_tokens: 5,
                finish_reason: Some("stop".to_string()),
                had_reasoning: false,
            })
        }
    }

    fn engine(responses: &[&str]) -> (Engine, Arc<Scripted>) {
        let scripted = Scripted::new(responses);
        let state = AppState::new(scripted.clone(), Config::default());
        (Engine::new(state), scripted)
    }

    fn doc(text: &str) -> Document {
        Document::new("file:///tmp/a.rs", 1, text.to_string(), Some("rust"))
    }

    const CODE: &str = "fn a() {\n    let f = File::open(p)?;\n}\n";

    #[test]
    fn analysis_stores_findings_and_a_second_pass_hits_the_cache() {
        let (e, scripted) = engine(&[
            r#"{"findings":[{"anchor":{"match":"File::open(p)"},"label":"unchecked","detail":"d"}]}"#,
        ]);
        let d = doc(CODE);
        let first = e.analyze(&d).unwrap();
        assert_eq!(first.findings.len(), 1);
        assert!(!first.from_cache);

        let second = e.analyze(&d).unwrap();
        assert!(second.from_cache, "identical content must not call the model twice");
        assert_eq!(scripted.seen.lock().len(), 1);
    }

    #[test]
    fn a_disabled_server_skips_without_calling_the_model() {
        let (e, scripted) = engine(&[]);
        e.state.merge_config(Some(&serde_json::json!({"enabled": false})));
        assert_eq!(
            e.analyze(&doc(CODE)).unwrap_err(),
            Failure::Skipped("meta is stopped".to_string())
        );
        assert!(scripted.seen.lock().is_empty());
    }

    #[test]
    fn an_ignored_path_and_a_binary_file_skip_by_name() {
        let (e, _) = engine(&[]);
        let ignored = Document::new("file:///w/node_modules/x.js", 1, "code".into(), Some("javascript"));
        assert!(matches!(
            e.analyze(&ignored).unwrap_err(),
            Failure::Skipped(_)
        ));
        let binary = Document::new("file:///t/x.bin", 1, "a\0b".into(), Some(""));
        assert!(matches!(e.analyze(&binary).unwrap_err(), Failure::Skipped(_)));
    }

    #[test]
    fn repair_is_bounded_never_a_loop() {
        // A persistently malformed model is still a Contract failure, not an infinite
        // retry: this subsumes the older "malformed review is a contract failure" case,
        // which stopped being true the moment one repair attempt was added.
        let (e, scripted) = engine(&["nope", "still not JSON", "and again", "one more"]);
        let f = e.analyze(&doc(CODE)).unwrap_err();
        assert!(matches!(f, Failure::Contract(_)), "{f:?}");
        assert!(f.message().contains("repair attempt"), "{}", f.message());
        assert_eq!(
            scripted.seen.lock().len(),
            1 + MAX_REPAIR_ATTEMPTS,
            "the repair budget is spent, and no further call is made"
        );
    }

    #[test]
    fn a_rejected_answer_is_retried_once_with_the_parser_complaint() {
        let (e, scripted) = engine(&[
            "I looked at the file and it seems fine. No JSON for you.",
            r#"{"findings":[{"anchor":{"match":"File::open(p)"},"label":"unchecked","detail":"d"}]}"#,
        ]);
        let out = e.analyze(&doc(CODE)).unwrap();
        assert_eq!(out.findings.len(), 1, "the repaired answer was used");

        let seen = scripted.seen.lock();
        assert_eq!(seen.len(), 2, "one attempt plus one repair");
        assert!(
            seen[1].user.contains("YOUR PREVIOUS ANSWER WAS REJECTED"),
            "the retry must say what happened"
        );
        assert!(
            seen[1].user.contains("no JSON object found"),
            "and quote the parser's own complaint"
        );
        assert!(
            seen[1].user.contains("CODE:"),
            "the original material is re-sent, not summarised"
        );
    }

    #[test]
    fn a_repair_call_is_budgeted_like_any_other_call() {
        let (e, scripted) = engine(&["no JSON here", r#"{"findings":[]}"#]);
        e.state.merge_config(Some(&serde_json::json!({
            "budget": {"max_calls_per_min": 1, "max_calls_per_hour": 10}
        })));
        let f = e.analyze(&doc(CODE)).unwrap_err();
        assert!(matches!(f, Failure::Refused(_)), "{f:?}");
        assert_eq!(scripted.seen.lock().len(), 1, "the repair was never issued");
    }

    #[test]
    fn generate_produces_a_validated_edit_and_caches_it() {
        let (e, scripted) = engine(&[
            r#"{"summary":"s","rationale":"r","replacements":[{"anchor":{"kind":"function","match":"fn a()"},"replacement":"fn a() {\n    ok();\n}"}]}"#,
        ]);
        let d = doc(CODE);
        let scope = e.scope_at(&d, 1, None);
        match e.generate(&d, Verb::Harden, &scope, &[]).unwrap() {
            Generated::Edit(p) => {
                assert_eq!(p.ops.len(), 1);
                assert_eq!(p.ops[0].start_line, 0);
                assert_eq!(p.ops[0].end_line, 2);
            }
            other => panic!("expected an edit, got {other:?}"),
        }
        let again = e.generate(&d, Verb::Harden, &scope, &[]).unwrap();
        assert!(matches!(again, Generated::Edit(_)));
        assert_eq!(scripted.seen.lock().len(), 1, "second call served from cache");
    }

    #[test]
    fn an_unlocatable_anchor_surfaces_as_a_rejected_edit() {
        // Both attempts answer the same unusable thing, so the surfaced message is the last
        // complaint and the repair budget is visibly spent.
        let bad = r#"{"replacements":[{"anchor":{"match":"not in the file"},"replacement":"x"}]}"#;
        // One attempt plus the full repair budget, all answering the same unusable thing.
        let (e, scripted) = engine(&[bad, bad, bad]);
        let d = doc(CODE);
        let scope = e.scope_at(&d, 1, None);
        let f = e.generate(&d, Verb::Rewrite, &scope, &[]).unwrap_err();
        assert!(matches!(f, Failure::Edit(_)), "{f:?}");
        assert!(f.message().contains("does not occur"), "{}", f.message());
        assert!(f.message().contains("repair attempt"), "{}", f.message());
        assert_eq!(
            scripted.seen.lock().len(),
            1 + MAX_REPAIR_ATTEMPTS,
            "one attempt plus the full repair budget"
        );
    }

    #[test]
    fn an_answer_that_does_not_apply_is_repaired_with_the_reason() {
        // The shape a real model produced in a soak run: anchored on one statement, it
        // answered with text that repeats a line further down, which would duplicate it.
        // Re-prompting with that complaint turns a dead end into a usable edit.
        let (e, scripted) = engine(&[
            r#"{"summary":"s","replacements":[{"anchor":{"kind":"statement","match":"two"},"replacement":"TWO\nx\nfour"}]}"#,
            r#"{"summary":"s","replacements":[{"anchor":{"kind":"statement","match":"two"},"replacement":"TWO"}]}"#,
        ]);
        let d = Document::new(
            "file:///tmp/notes.txt",
            1,
            "one\ntwo\nthree\nfour\n".to_string(),
            Some(""),
        );
        let scope = e.scope_at(&d, 1, None);
        match e.generate(&d, Verb::Rewrite, &scope, &[]).unwrap() {
            Generated::Edit(p) => assert_eq!(p.ops[0].new_text, "TWO"),
            other => panic!("expected an edit, got {other:?}"),
        }
        let seen = scripted.seen.lock();
        assert_eq!(seen.len(), 2, "the rejection was retried once");
        assert!(seen[1].user.contains("YOUR PREVIOUS ANSWER WAS REJECTED"));
        assert!(
            seen[1].user.contains("already follow the anchor"),
            "the model is told what was wrong with its answer: {}",
            &seen[1].user[seen[1].user.len().saturating_sub(300)..]
        );
    }

    #[test]
    fn explain_returns_an_artifact() {
        let (e, _) = engine(&[r###"{"summary":"s","markdown":"## What it does\nstuff"}"###]);
        let d = doc(CODE);
        let scope = e.scope_at(&d, 1, None);
        match e.generate(&d, Verb::Explain, &scope, &[]).unwrap() {
            Generated::Artifact(md) => assert!(md.contains("What it does")),
            other => panic!("expected an artifact, got {other:?}"),
        }
    }

    #[test]
    fn the_budget_refuses_rather_than_queueing_forever() {
        let (e, _) = engine(&[
            r#"{"findings":[]}"#,
            r#"{"findings":[]}"#,
            r#"{"findings":[]}"#,
        ]);
        e.state.merge_config(Some(&serde_json::json!({
            "budget": {"max_calls_per_min": 1, "max_calls_per_hour": 100}
        })));
        assert!(e.analyze(&doc(CODE)).is_ok());
        let second = e.analyze(&doc("fn b() {}\n")).unwrap_err();
        assert!(matches!(second, Failure::Refused(_)), "{second:?}");
        assert_eq!(e.state.budget.snapshot().in_flight, 0, "permit released");
    }

    #[test]
    fn failures_are_always_showable() {
        for f in [
            Failure::Skipped("x".into()),
            Failure::Refused(Refusal::PerMinute),
            Failure::Model("timeout".into()),
            Failure::Contract("bad json".into()),
            Failure::Edit("ambiguous".into()),
        ] {
            assert!(!f.message().is_empty());
            assert!(!f.code().is_empty());
        }
    }

    #[test]
    fn a_plan_is_built_from_a_goal_and_its_targets_resolved() {
        let (e, _) = engine(&[
            r#"{"goal":"fail loudly","steps":[
                {"title":"guard the open","rationale":"r","verb":"harden","anchors":[{"match":"File::open(p)"}]},
                {"title":"add a test","verb":"test","anchors":[{"match":"fn a()"}]}]}"#,
        ]);
        let d = doc(CODE);
        let scope = e.scope_at(&d, 1, None);
        let (plan, rejected) = e.plan(&d, &scope, "fail loudly").unwrap();
        assert_eq!(rejected, 0);
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.goal, "fail loudly");
        assert_eq!(plan.steps[0].targets[0].line, 1);
        assert_eq!(plan.steps[0].targets[0].uri, "file:///tmp/a.rs");
        assert!(!plan.usage.model.is_empty(), "the artifact records which model ran");
        assert!(plan.usage.ms < 60_000);
        assert!(plan.usage.tokens_in > 0, "and what it cost");
    }

    #[test]
    fn a_plan_with_no_locatable_step_is_a_contract_failure() {
        let (e, _) = engine(&[r#"{"steps":[{"title":"vague","verb":"harden","anchors":[{"match":"nowhere"}]}]}"#]);
        let d = doc(CODE);
        let scope = e.scope_at(&d, 1, None);
        let f = e.plan(&d, &scope, "do something").unwrap_err();
        assert!(matches!(f, Failure::Contract(_)), "{f:?}");
    }

    #[test]
    fn a_plan_without_a_goal_is_refused_before_any_call() {
        let (e, scripted) = engine(&[]);
        let d = doc(CODE);
        let scope = e.scope_at(&d, 1, None);
        assert!(matches!(
            e.plan(&d, &scope, "   ").unwrap_err(),
            Failure::Skipped(_)
        ));
        assert!(scripted.seen.lock().is_empty(), "no call for an empty goal");
    }

    #[test]
    fn applying_a_step_re_anchors_on_the_current_content() {
        let (e, _) = engine(&[
            r#"{"steps":[{"title":"guard","verb":"harden","anchors":[{"match":"File::open(p)"}]}]}"#,
            r#"{"summary":"s","replacements":[{"anchor":{"kind":"statement","match":"File::open(p)"},"replacement":"let f = checked_open(p)?;"}]}"#,
        ]);
        let d = doc(CODE);
        let scope = e.scope_at(&d, 1, None);
        let (plan, _) = e.plan(&d, &scope, "guard the open").unwrap();

        // The file gained a line above; the step must still find its target by text.
        let moved = "// a new header\n".to_string() + CODE;
        let d2 = doc(&moved);
        let proposal = e.apply_step(&plan, 1, &d2, &[]).unwrap();
        assert_eq!(proposal.ops[0].start_line, 2, "anchored by text, not by line");
    }

    #[test]
    fn a_step_whose_target_is_gone_is_stale_not_applied_blindly() {
        let (e, _) = engine(&[
            r#"{"steps":[{"title":"guard","verb":"harden","anchors":[{"match":"File::open(p)"}]}]}"#,
        ]);
        let d = doc(CODE);
        let scope = e.scope_at(&d, 1, None);
        let (plan, _) = e.plan(&d, &scope, "guard").unwrap();
        let rewritten = Document::new("file:///tmp/a.rs", 1, "fn a() {\n    totally_different();\n}\n".into(), Some("rust"));
        assert_eq!(e.apply_step(&plan, 1, &rewritten, &[]).unwrap_err(), Failure::Stale);
    }

    #[test]
    fn applying_a_step_that_does_not_exist_says_so() {
        let (e, _) = engine(&[
            r#"{"steps":[{"title":"guard","verb":"harden","anchors":[{"match":"File::open(p)"}]}]}"#,
        ]);
        let d = doc(CODE);
        let scope = e.scope_at(&d, 1, None);
        let (plan, _) = e.plan(&d, &scope, "guard").unwrap();
        assert!(matches!(
            e.apply_step(&plan, 99, &d, &[]).unwrap_err(),
            Failure::Skipped(_)
        ));
    }

    #[test]
    fn the_tier_ceiling_bounds_max_tokens() {
        let (e, scripted) = engine(&[r#"{"summary":"s","markdown":"m"}"#]);
        e.state.merge_config(Some(&serde_json::json!({
            "models": {"reason": {"max_tokens": 100}}
        })));
        let d = doc(CODE);
        let scope = e.scope_at(&d, 1, None);
        e.generate(&d, Verb::Explain, &scope, &[]).unwrap();
        let seen = scripted.seen.lock();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].max_tokens, 100, "the configured ceiling wins");
    }
}
