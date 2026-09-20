//! Orchestration: gates, budget, cache, prompt, model call, validation.
//!
//! This is the whole product pipeline, minus the transport. It is deliberately free of LSP
//! types so it can be exercised in-process.

use crate::state::AppState;
use jev_core::budget::{Budget, Refusal};
use jev_core::cache::{self, Conclusion};
use jev_core::config::Config;
use jev_core::context::{self, Provided};
use jev_core::contract;
use jev_core::document::Document;
use jev_core::edit::{self, BuildOptions};
use jev_core::findings;
use jev_core::gates;
use jev_core::inspections;
use jev_core::model::{ChatRequest, ChatResponse};
use jev_core::scope::{self, Resolved};
use jev_core::types::{Finding, Proposal, Tier, Usage, Verb, PROMPT_VERSION};
use jev_core::verbs;

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

/// What one rules pass found, and what it cost to find it.
///
/// The counts are part of the answer, not diagnostics: `candidates` says how many places the
/// inspections named, `considered` how many rules survived `applies_to`, and `skipped` names
/// every rule file that could not be read. A pass that inspected nothing and a pass that
/// inspected a hundred lines and agreed with all of them are different results, and a caller
/// that only sees `findings` cannot tell them apart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InspectOutcome {
    pub findings: Vec<Finding>,
    pub considered: usize,
    pub candidates: usize,
    /// `(path, reason)` for rule files that were skipped, and `("unchanged", path)` when the
    /// document was not in the changed set.
    pub skipped: Vec<(String, String)>,
}

/// The source label the rules pass stamps on its conclusions, and the chat review its own.
const SOURCE_RULES: &str = "rules";
const SOURCE_REVIEW: &str = "review";

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

pub struct Engine {
    pub state: Arc<AppState>,
}

/// The directory a path sits in, when it names one.
fn parent_dir(path: &str) -> Option<String> {
    path.rsplit_once('/').map(|(dir, _)| {
        if dir.is_empty() {
            "/".to_string()
        } else {
            dir.to_string()
        }
    })
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
            range: jev_core::types::LineRange {
                start_line: 0,
                end_line: last,
            },
            kind: jev_core::types::ScopeKind::File,
            source: jev_core::types::ScopeSource::WholeFile,
            name: None,
            truncated: false,
        }
    }

    /// Cached findings for the document's *current* content, if any.
    pub fn cached(&self, doc: &Document) -> Option<Arc<Conclusion>> {
        self.state.cache.get(&cache::findings_key(
            &doc.hash,
            &doc.language.name,
            self.config().noise.max_visible_findings,
        ))
    }

    /// Document-level findings from the chat review tier, from cache or freshly produced.
    ///
    /// This is the ambient path *when rules are disabled*: it never runs inline with a user
    /// gesture, and a failure here is reported as a skip rather than surfaced as an error. When
    /// rules are enabled the ambient pass is [`Engine::inspect`] instead.
    pub fn analyze(&self, doc: &Document) -> Result<Outcome, Failure> {
        let cfg = self.config();
        if !cfg.enabled {
            return Err(Failure::Skipped("jev is stopped".to_string()));
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
        self.chat_review(doc, &cfg)
    }

    /// The chat review, asked for explicitly.
    ///
    /// Unlike [`Engine::analyze`] this never answers from the findings cache. The cache holds the
    /// *ambient* conclusion, which under the default settings was produced by the rules pass —
    /// and answering a request for the model's opinion with something the model never said is not
    /// an answer to it.
    pub fn review_now(&self, doc: &Document) -> Result<Outcome, Failure> {
        let cfg = self.config();
        if !cfg.enabled {
            return Err(Failure::Skipped("jev is stopped".to_string()));
        }
        if let Some(skip) = gates::evaluate(
            &doc.text,
            &doc.path,
            cfg.languages.max_file_bytes,
            &cfg.languages.ignore,
        ) {
            return Err(Failure::Skipped(skip.reason()));
        }
        self.chat_review(doc, &cfg)
    }

    /// One chat review call, cached in the slot every read-only surface reads.
    fn chat_review(&self, doc: &Document, cfg: &Config) -> Result<Outcome, Failure> {
        let scope = Self::review_scope(doc);
        let ctx = context::build(doc, &scope, &[], 0);
        let (raw, _usage) = self.with_repair(
            cfg,
            Verb::Review.tier(),
            &ctx,
            |c| verbs::render(Verb::Review, c),
            contract::parse_findings,
            None,
        )?;
        let built = findings::build(
            &doc.text,
            &raw,
            &jev_core::lang::profile(&doc.language.name),
            cfg.noise.max_visible_findings,
        );

        self.state.cache.put(
            &cache::findings_key(
                &doc.hash,
                &doc.language.name,
                cfg.noise.max_visible_findings,
            ),
            Conclusion {
                findings: built.findings.clone(),
                source: Some(SOURCE_REVIEW.to_string()),
                ..Default::default()
            },
        );

        Ok(Outcome {
            findings: built.findings,
            from_cache: false,
            rejected: built.rejected,
        })
    }

    /// Run the repository's rules over one document.
    ///
    /// The order is the contract, and two of the steps are deliberately not in the numbered
    /// order they are described in: the changed-set test and the budget permit both come *before*
    /// the decision call, because a check that runs after the call is not a check.
    ///
    /// 1. the gates, exactly as the chat path runs them;
    /// 2. the rules for this root, cached by their hash;
    /// 3. `applies_to`, so a rule for `**/*.rs` never sees a Python file;
    /// 4. the inspections, which are milliseconds of local work and decide nothing;
    /// 5. the cache, keyed by content *and* rules, so a second pass over unchanged text and
    ///    unchanged rules costs nothing;
    /// 6. the changed set, unless the caller insists;
    /// 7. one budget permit, taken before the call and never after;
    /// 8. **one** decision call for the whole document;
    /// 9. a finding only where a `true` cleared the rule's own probability floor;
    /// 10. `findings::build`, unchanged, so every surface treats these like any other finding.
    pub fn inspect(&self, doc: &Document, force: bool) -> Result<InspectOutcome, Failure> {
        let cfg = self.config();
        if let Some(skip) = gates::evaluate(
            &doc.text,
            &doc.path,
            cfg.languages.max_file_bytes,
            &cfg.languages.ignore,
        ) {
            return Err(Failure::Skipped(skip.reason()));
        }

        let root = self
            .state
            .root()
            .or_else(|| parent_dir(&doc.path))
            .unwrap_or_else(|| ".".to_string());
        let rule_set = self.state.rule_set(std::path::Path::new(&root), cfg.rules.defaults);
        let mut skipped = rule_set.skipped.clone();

        // Steps 2-4: the rules that claim this path, and the candidates their inspections found.
        // All of it is local work that decides nothing.
        // `applies_to` is written the way a repository names its own files, so it is matched
        // against the path relative to the root, never the absolute one.
        let match_path = gates::relative_to(&doc.path, &root);
        let (considered, asked) = inspections::select(
            &rule_set.rules,
            match_path,
            &doc.text,
            cfg.rules.max_candidates_per_rule,
        );
        let candidates = asked.len();

        // Everything wrong with the rules *document* and everything a reader needs to know about
        // where they came from, reported beside everything wrong with the pass, in the one list a
        // caller reads (`skipped`, PROTOCOL §6), from the one function both front ends call. A
        // rule whose pattern does not compile finds no candidates and would otherwise be inert in
        // silence — the same answer a repository gets from a convention it keeps perfectly —
        // which is the failure mode `rules::lint` exists for; and `("default_rules", …)` is what
        // stops a pass running on the shipped set from looking like a pass running on files the
        // reader cannot find, and `no_rules` from reading as "you have rules" when none applies.
        //
        // All of it is a fact about the *rule set*, not about this document, which is why the
        // unchanged shortcut below reports it too: a user who has just edited a rule, saved, and
        // watched an untouched file change nothing is exactly who needs to be told why.
        let notes = inspections::pass_notes(&rule_set, considered, &doc.path, &root);
        skipped.extend(notes.iter().cloned());

        // Step 5: the cache is consulted once the candidate count is known, so a hit can still
        // answer with the numbers this pass would have reported.
        let key = cache::rules_key(&doc.hash, &rule_set.hash, &doc.path, &cfg);
        if let Some(hit) = self.state.cache.get(&key) {
            return Ok(InspectOutcome {
                findings: hit.findings.clone(),
                considered,
                candidates,
                skipped,
            });
        }

        // Step 6: a document nobody touched is not inspected. The question is asked once per
        // pass and cached briefly — never once per document.
        if !force {
            match self.state.changed_paths(Some(&root)) {
                Ok(changed) if !changed.contains(&doc.path) => {
                    // `("unchanged", path)`: a pass-level skip names its reason and the path it is
                    // about, exactly as a file-level skip does. The rules' own problems come
                    // first, because they are true whatever this document is.
                    let mut listed = notes;
                    listed.push(("unchanged".to_string(), doc.path.clone()));
                    return Ok(InspectOutcome {
                        skipped: listed,
                        ..Default::default()
                    });
                }
                // Changed, or the question could not be answered. Either way the safe answer is
                // to inspect: "I could not tell" must not read as "nothing changed".
                _ => {}
            }
        }

        let started = std::time::Instant::now();
        // The lint messages, for `jev.status`'s count. Counted off the notes rather than by
        // linting a second time: `lint` compiles each rule's pattern, and a pass runs on every
        // save.
        let lint_count = notes.iter().filter(|(code, _)| code == "lint").count();

        // Nothing to ask: no rules matched, or none of them found anything. A decision call with
        // zero questions would spend a permit to be told nothing. The pass is still recorded, so
        // `jev.status` can say that it ran and what it loaded.
        if asked.is_empty() {
            self.state.note_rules_pass(
                rule_set.rules.len(),
                &rule_set.hash,
                started.elapsed().as_millis() as u64,
                candidates,
                0,
                lint_count,
            );
            self.state.cache.put(
                &key,
                Conclusion {
                    findings: Vec::new(),
                    source: Some(SOURCE_RULES.to_string()),
                    ..Default::default()
                },
            );
            return Ok(InspectOutcome {
                considered,
                candidates,
                skipped,
                ..Default::default()
            });
        }

        // The permit is taken *before* the call. A check that runs after the call is not a
        // check.
        let _permit = match self.state.budget.try_acquire_decision(&cfg.budget) {
            jev_core::budget::Permit::Granted => Permit(&self.state.budget),
            jev_core::budget::Permit::Refused(r) => return Err(Failure::Refused(r)),
        };

        // Step 8: one call for the whole document.
        let request = inspections::request(&doc.path, &doc.text, &asked, &cfg.rules);
        let response = match self.state.decision.decide(cfg.decision(), &request) {
            Ok(response) => response,
            Err(e) => {
                // An answer that arrived and cannot be read is a contract violation, not an
                // unreachable endpoint, and the difference is what the user is told.
                if let Some(bad) = e.downcast_ref::<jev_core::decision::BadAnswer>() {
                    return Err(Failure::Contract(bad.to_string()));
                }
                return Err(Failure::Model(format!("decision call failed: {e:#}")));
            }
        };
        self.state.budget.record_tokens(response.input_tokens + response.output_tokens);

        // Steps 9-10: a `true` above the rule's own floor becomes a finding, and the whole set
        // goes through `findings::build` — the same function the chat review's findings pass
        // through, so ids, dismissal, ordering and the noise cap behave identically everywhere.
        let built = inspections::resolve(
            &doc.text,
            &asked,
            &response,
            &jev_core::lang::profile(&doc.language.name),
            cfg.noise.max_visible_findings,
        );
        if built.rejected > 0 {
            skipped.push((
                "unlocatable_anchor".to_string(),
                format!(
                    "{} finding(s) could not be anchored on a unique line",
                    built.rejected
                ),
            ));
        }

        let conclusion = Conclusion {
            findings: built.findings.clone(),
            source: Some(SOURCE_RULES.to_string()),
            ..Default::default()
        };
        // Two entries on purpose: the rules key is this pass's own dedupe (a rule edit must
        // invalidate it), and the findings key is the slot every read-only surface reads.
        self.state.cache.put(&key, conclusion.clone());
        self.state
            .cache
            .put(
                &cache::findings_key(&doc.hash, &doc.language.name, cfg.noise.max_visible_findings),
                conclusion,
            );
        self.state.note_rules_pass(
            rule_set.rules.len(),
            &rule_set.hash,
            started.elapsed().as_millis() as u64,
            candidates,
            1,
            lint_count,
        );

        Ok(InspectOutcome {
            findings: built.findings,
            considered,
            candidates,
            skipped,
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
        self.generate_with_context(doc, verb, scope, findings_in_scope, &[], None)
    }

    /// The same generation, with the answer reported as it arrives.
    ///
    /// The stream is a *preview*: what the caller finally receives is the artifact this
    /// returns, and a repair attempt — which only ever happens for artifacts — means the
    /// preview was the first, rejected attempt. The Result is authoritative; the stream is
    /// what makes the wait visible.
    /// The same, with whatever the editor could see that this side cannot (PROTOCOL §6.1).
    ///
    /// The context is hashed into the cache key: two requests that differ only in what the
    /// client sent are two different questions.
    pub fn generate_with_context(
        &self,
        doc: &Document,
        verb: Verb,
        scope: &Resolved,
        findings_in_scope: &[Finding],
        provided: &[Provided],
        mut delta: Delta<'_>,
    ) -> Result<Generated, Failure> {
        let cfg = self.config();
        if !cfg.enabled {
            return Err(Failure::Skipped("jev is stopped".to_string()));
        }
        if let Some(skip) = gates::evaluate(
            &doc.text,
            &doc.path,
            cfg.languages.max_file_bytes,
            &cfg.languages.ignore,
        ) {
            return Err(Failure::Skipped(skip.reason()));
        }

        let key = cache::op_key(
            verb.as_str(),
            PROMPT_VERSION,
            &cfg.tier(verb.tier()).model,
            &doc.language.name,
            &doc.hash,
            scope.range.start_line,
            scope.range.end_line,
            &context::provided_digest(provided),
        );
        if let Some(hit) = self.state.cache.get(&key) {
            if let Some(p) = &hit.edit {
                return Ok(Generated::Edit(p.clone()));
            }
            if let Some(a) = &hit.artifact {
                return Ok(Generated::Artifact(a.clone()));
            }
        }

        let ctx = context::build_with(doc, scope, findings_in_scope, 12, provided);

        let generated = match verb.output() {
            jev_core::types::Output::Artifact => {
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
                let profile = jev_core::lang::profile(&doc.language.name);
                let opts = BuildOptions {
                    max_scope_lines: cfg.languages.max_scope_lines,
                    // The user selected a scope; the answer stays inside it or it is repaired.
                    scope_lines: Some((scope.range.start_line, scope.range.end_line)),
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
                    attempt_ctx = verbs::repair_context(&ctx, &refusal, &attempt.text);
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

    /// An artifact for an arbitrary prompt, through the same gates, budget, repair and cache
    /// path a verb's artifact takes.
    ///
    /// The follow-up is a question rather than an action, so it has no verb — and this is what
    /// keeps it from being a second implementation of everything an artifact needs.
    /// One model call for a question, returning the text as it came.
    ///
    /// Not an artifact: a question may be answered by asking for a page to be fetched, and that
    /// request is a single line rather than a document. The caller decides what the answer means.
    pub fn ask_round(&self, spec: verbs::PromptSpec) -> Result<String, Failure> {
        let cfg = self.config();
        if !cfg.enabled {
            return Err(Failure::Skipped("jev is stopped".to_string()));
        }
        let response = self.chat_with(&cfg, Tier::Reason, spec, None)?;
        Ok(response.text)
    }

    pub fn artifact_for(
        &self,
        doc: &Document,
        scope: &Resolved,
        findings_in_scope: &[Finding],
        provided: &[Provided],
        cache_key: &str,
        render_prompt: impl Fn(&context::Context) -> verbs::PromptSpec,
        mut delta: Delta<'_>,
    ) -> Result<Generated, Failure> {
        let cfg = self.config();
        if !cfg.enabled {
            return Err(Failure::Skipped("jev is stopped".to_string()));
        }
        if let Some(skip) = gates::evaluate(
            &doc.text,
            &doc.path,
            cfg.languages.max_file_bytes,
            &cfg.languages.ignore,
        ) {
            return Err(Failure::Skipped(skip.reason()));
        }
        if let Some(hit) = self.state.cache.get(cache_key) {
            if let Some(artifact) = &hit.artifact {
                return Ok(Generated::Artifact(artifact.clone()));
            }
        }

        let ctx = context::build_with(doc, scope, findings_in_scope, 12, provided);
        let (raw, _usage) = self.with_repair(
            &cfg,
            Tier::Reason,
            &ctx,
            render_prompt,
            contract::parse_artifact,
            reborrow(&mut delta),
        )?;
        if raw.markdown.trim().is_empty() {
            return Err(Failure::Contract("artifact carried no text".to_string()));
        }
        self.state.cache.put(
            cache_key,
            Conclusion {
                artifact: Some(raw.markdown.clone()),
                ..Default::default()
            },
        );
        Ok(Generated::Artifact(raw.markdown))
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
        // The permit is taken *before* the call. A check that runs after the call is not a
        // check.
        let _permit = match self.state.budget.try_acquire(&cfg.budget) {
            jev_core::budget::Permit::Granted => Permit(&self.state.budget),
            jev_core::budget::Permit::Refused(r) => return Err(Failure::Refused(r)),
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
        // `{e:#}` prints the whole `anyhow` chain, so a failed call names its cause (a timeout,
        // a TLS failure, a reset) instead of only the URL it was aimed at.
        .map_err(|e| Failure::Model(format!("{e:#}")))?;
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
            let repair = verbs::repair_context(ctx, &last.to_string(), &response.text);
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
    pub fn scope_at(&self, doc: &Document, line: u32, explicit: Option<jev_core::types::LineRange>) -> Resolved {
        let cfg = self.config();
        let profile = jev_core::lang::profile(&doc.language.name);
        scope::resolve(
            &doc.text,
            line,
            &profile,
            explicit,
            cfg.languages.max_scope_lines,
        )
    }

    /// Turn a goal into a plan (PROTOCOL.md §6, §7). A plan holds no edits: each step is
    /// applied later, against the content live at that moment.
    pub fn plan(&self, doc: &Document, scope: &Resolved, goal: &str) -> Result<(jev_core::types::Plan, usize), Failure> {
        let cfg = self.config();
        if !cfg.enabled {
            return Err(Failure::Skipped("jev is stopped".to_string()));
        }
        if goal.trim().is_empty() {
            return Err(Failure::Skipped(
                "a plan needs a goal; `:Jev plan` asks for one".to_string(),
            ));
        }
        if let Some(skip) = gates::evaluate(
            &doc.text,
            &doc.path,
            cfg.languages.max_file_bytes,
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
        let built = jev_core::plan::build(
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
        plan: &jev_core::types::Plan,
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
    use jev_core::config::TierConfig;
    use jev_core::decision::{DecisionRequest, DecisionValue};
    use jev_core::model::{Backend, ChatResponse};
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
        engine_with(responses, Arc::new(ScriptedDecision::default()))
    }

    /// The same, with a decision backend the test can script.
    fn engine_with(
        responses: &[&str],
        decision: Arc<ScriptedDecision>,
    ) -> (Engine, Arc<Scripted>) {
        engine_full(responses, decision, &[])
    }

    /// The same, with the shipped rule set substituted: what `main` passes the embedded set for,
    /// so the pass under test does not depend on whether `default_rules/` has been filled in yet.
    fn engine_full(
        responses: &[&str],
        decision: Arc<ScriptedDecision>,
        builtin: &'static [(&'static str, &'static str)],
    ) -> (Engine, Arc<Scripted>) {
        let scripted = Scripted::new(responses);
        let state = AppState::new(scripted.clone(), decision, Config::default(), builtin);
        (Engine::new(state), scripted)
    }

    /// A decision backend that answers from a table of `(question id, probability)`, records
    /// what it was asked, and — for an id it does not know — answers nothing at all.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Manner {
        /// Answer from the table.
        Answer,
        /// The call itself fails, as an unreachable endpoint would.
        Fail,
        /// The call returns something that cannot be read.
        Malformed,
    }

    struct ScriptedDecision {
        table: Mutex<Vec<(String, f64)>>,
        seen: Mutex<Vec<DecisionRequest>>,
        manner: Manner,
    }

    impl ScriptedDecision {
        fn with(table: &[(&str, f64)]) -> Arc<ScriptedDecision> {
            Arc::new(ScriptedDecision {
                table: Mutex::new(
                    table
                        .iter()
                        .map(|(id, p)| (id.to_string(), *p))
                        .collect(),
                ),
                seen: Mutex::new(Vec::new()),
                manner: Manner::Answer,
            })
        }

        fn failing() -> Arc<ScriptedDecision> {
            Arc::new(ScriptedDecision {
                table: Mutex::new(Vec::new()),
                seen: Mutex::new(Vec::new()),
                manner: Manner::Fail,
            })
        }

        fn malformed() -> Arc<ScriptedDecision> {
            Arc::new(ScriptedDecision {
                table: Mutex::new(Vec::new()),
                seen: Mutex::new(Vec::new()),
                manner: Manner::Malformed,
            })
        }

        fn calls(&self) -> usize {
            self.seen.lock().len()
        }
    }

    impl Default for ScriptedDecision {
        fn default() -> Self {
            ScriptedDecision {
                table: Mutex::new(Vec::new()),
                seen: Mutex::new(Vec::new()),
                manner: Manner::Answer,
            }
        }
    }

    impl jev_core::decision::DecisionBackend for ScriptedDecision {
        fn decide(
            &self,
            _cfg: &jev_core::config::DecisionTierConfig,
            req: &DecisionRequest,
        ) -> anyhow::Result<jev_core::decision::DecisionResponse> {
            self.seen.lock().push(req.clone());
            match self.manner {
                Manner::Fail => return Err(anyhow::anyhow!("the endpoint is not there")),
                Manner::Malformed => {
                    return Err(anyhow::Error::new(jev_core::decision::BadAnswer {
                        id: Some("no-unwrap#1".to_string()),
                        why: "a noul answer needs a numeric `noul`".to_string(),
                    }))
                }
                Manner::Answer => {}
            }
            let table = self.table.lock().clone();
            let answers = req
                .questions
                .iter()
                .filter_map(|q| {
                    let (_, probability) = table.iter().find(|(id, _)| *id == q.id)?;
                    Some(jev_core::decision::DecisionAnswer {
                        id: q.id.clone(),
                        value: DecisionValue::Bool(*probability > 0.5),
                        probability: Some(*probability),
                        reason: Some("reachable".to_string()),
                    })
                })
                .collect();
            Ok(jev_core::decision::DecisionResponse {
                answers,
                input_tokens: 10,
                output_tokens: 2,
            })
        }
    }

    /// Commit everything under `root`, so the changed-set check has a clean repository to answer
    /// from. False when git cannot be run at all, in which case a test gives up rather than
    /// asserting against a machine that has no git.
    fn git_commit(root: &std::path::Path) -> bool {
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        run(&["init", "-q"])
            && run(&["config", "user.email", "test@example.invalid"])
            && run(&["config", "user.name", "test"])
            && run(&["add", "-A"])
            && run(&["-c", "commit.gpgsign=false", "commit", "-q", "-m", "fixture"])
    }

    /// A rules directory under a temp root, plus the root itself.
    struct Rules {
        root: std::path::PathBuf,
    }

    impl Rules {
        fn new(tag: &str) -> Rules {
            let root = std::env::temp_dir().join(format!("jev-inspect-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join(".jev/rules")).unwrap();
            Rules { root }
        }

        fn file(&self, name: &str, body: &str) -> &Rules {
            std::fs::write(self.root.join(".jev/rules").join(name), body).unwrap();
            self
        }

        fn rule(&self, id: &str, pattern: &str, applies_to: &str, min: f64) -> String {
            serde_json::json!({
                "schema": "jev.rules/1",
                "rules": [{
                    "id": id,
                    "title": "Unwrap in a handler",
                    "text": "A handler must not unwrap.",
                    "severity": "warning",
                    "applies_to": [applies_to],
                    "inspection": {"kind": "regex", "pattern": pattern},
                    "judgement": {
                        "question": "Is this unwrap reachable from a request handler?",
                        "criteria": {"true": "reachable", "false": "test code"},
                        "reasons": {"reachable": "a request can reach it"},
                        "min_probability": min
                    },
                    "verb_hint": "fix"
                }]
            })
            .to_string()
        }
    }

    impl Drop for Rules {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    const UNWRAP: &str = "fn h() {\n    a.unwrap();\n    b.unwrap();\n}\n";

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
            Failure::Skipped("jev is stopped".to_string())
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

    // ---- the rules pass -----------------------------------------------------

    /// An engine whose rules live in `rules`, with the root pointed at them.
    fn inspector(rules: &Rules, decision: Arc<ScriptedDecision>) -> Engine {
        let (e, _) = engine_with(&[], decision);
        e.state.set_root(Some(rules.root.display().to_string()));
        e
    }

    #[test]
    fn a_confirmed_candidate_becomes_a_finding_and_a_denied_one_does_not() {
        let rules = Rules::new("confirm");
        rules.file("a.json", &rules.rule("no-unwrap", r"\.unwrap\(\)", "**/*.rs", 0.75));
        let d = doc(UNWRAP);

        for (probability, want) in [(0.9f64, 2usize), (0.7, 0), (0.2, 0)] {
            let decision = ScriptedDecision::with(&[
                ("no-unwrap#1", probability),
                ("no-unwrap#2", probability),
            ]);
            let e = inspector(&rules, decision.clone());
            let out = e.inspect(&d, true).unwrap();
            assert_eq!(out.findings.len(), want, "p={probability}: {out:?}");
            assert_eq!(decision.calls(), 1, "one decision call for the whole document");
            if want == 0 {
                continue;
            }
            let f = &out.findings[0];
            assert_eq!(f.label, "Unwrap in a handler", "the rule's title is the label");
            assert_eq!(f.line, 1, "the candidate's line");
            assert_eq!(
                f.severity,
                jev_core::types::Severity::Warning,
                "the rule asked for a warning"
            );
            assert_eq!(f.verb_hint, Verb::Fix);
            assert!(f.detail.contains("A handler must not unwrap."), "{:?}", f.detail);
            assert!(f.detail.contains("reachable"), "the reason the decision gave: {:?}", f.detail);
            assert!(f.detail.contains("p=0.90"), "and the probability: {:?}", f.detail);
            // The same slot every read-only surface reads, tagged with its source.
            let cached = e.cached(&d).expect("a conclusion for this content");
            assert_eq!(cached.source.as_deref(), Some("rules"));
            assert_eq!(cached.findings.len(), want);
        }
    }

    #[test]
    fn a_missing_answer_publishes_nothing_and_the_counts_say_so() {
        let rules = Rules::new("missing");
        rules.file("a.json", &rules.rule("no-unwrap", r"\.unwrap\(\)", "**/*.rs", 0.75));
        let decision = ScriptedDecision::with(&[]);
        let e = inspector(&rules, decision.clone());
        let out = e.inspect(&doc(UNWRAP), true).unwrap();
        assert!(out.findings.is_empty(), "an answer nobody gave is not a finding");
        assert_eq!(out.considered, 1);
        assert_eq!(out.candidates, 2, "two lines matched, and that is the report");
        assert_eq!(decision.calls(), 1);
        assert_eq!(decision.seen.lock()[0].questions.len(), 2, "both were asked");
    }

    #[test]
    fn considered_candidates_and_skipped_are_the_numbers_they_claim() {
        let rules = Rules::new("counts");
        let body = serde_json::json!({
            "schema": "jev.rules/1",
            "rules": [
                serde_json::from_str::<serde_json::Value>(
                    &rules.rule("rs-rule", r"\.unwrap\(\)", "**/*.rs", 0.75)
                ).unwrap()["rules"][0].clone(),
                serde_json::from_str::<serde_json::Value>(
                    &rules.rule("py-rule", r"open\(", "**/*.py", 0.75)
                ).unwrap()["rules"][0].clone(),
            ]
        })
        .to_string();
        rules.file("a.json", &body);
        rules.file("broken.json", "{not json");

        let decision = ScriptedDecision::with(&[("rs-rule#1", 0.9), ("rs-rule#2", 0.9)]);
        let e = inspector(&rules, decision);
        let out = e.inspect(&doc(UNWRAP), true).unwrap();
        assert_eq!(out.considered, 1, "the python rule does not apply to a .rs document");
        assert_eq!(out.candidates, 2);
        assert_eq!(out.findings.len(), 2);
        assert_eq!(out.skipped.len(), 1, "{:?}", out.skipped);
        assert!(out.skipped[0].0.ends_with("broken.json"));
        assert!(out.skipped[0].1.contains("not a rules document"), "{:?}", out.skipped);
    }

    #[test]
    fn a_document_the_changed_set_does_not_name_is_skipped_without_a_call() {
        let rules = Rules::new("unchanged");
        rules.file("a.json", &rules.rule("no-unwrap", r"\.unwrap\(\)", "**/*.rs", 0.75));
        std::fs::write(rules.root.join("a.rs"), UNWRAP).unwrap();
        if !git_commit(&rules.root) {
            return; // no git on this machine; the harness covers the same claim
        }
        let d = Document::new(
            &format!("file://{}/a.rs", rules.root.display()),
            1,
            UNWRAP.to_string(),
            Some("rust"),
        );
        let decision =
            ScriptedDecision::with(&[("no-unwrap#1", 0.9), ("no-unwrap#2", 0.9)]);
        let e = inspector(&rules, decision.clone());

        let skipped = e.inspect(&d, false).unwrap();
        assert!(skipped.findings.is_empty());
        assert_eq!(skipped.skipped, vec![("unchanged".to_string(), d.path.clone())]);
        assert_eq!(decision.calls(), 0, "an untouched document costs no decision");

        // The same document, insisted on, is inspected.
        let forced = e.inspect(&d, true).unwrap();
        assert_eq!(forced.findings.len(), 2);
        assert_eq!(decision.calls(), 1);
    }

    #[test]
    fn a_second_pass_over_identical_content_hits_the_cache_and_issues_no_call() {
        let rules = Rules::new("cache");
        rules.file("a.json", &rules.rule("no-unwrap", r"\.unwrap\(\)", "**/*.rs", 0.75));
        let decision =
            ScriptedDecision::with(&[("no-unwrap#1", 0.9), ("no-unwrap#2", 0.9)]);
        let e = inspector(&rules, decision.clone());
        let d = doc(UNWRAP);

        let first = e.inspect(&d, true).unwrap();
        let second = e.inspect(&d, true).unwrap();
        assert_eq!(decision.calls(), 1, "identical content and rules: no second call");
        assert_eq!(first.findings, second.findings);
        assert_eq!(second.candidates, first.candidates, "the counts survive a hit too");

        // Editing a rule is a different question, and is asked again.
        rules.file("a.json", &rules.rule("no-unwrap", r"unwrap", "**/*.rs", 0.75));
        let third = e.inspect(&d, true).unwrap();
        assert_eq!(decision.calls(), 2, "a rule edit must invalidate the conclusion");
        assert_eq!(third.candidates, 2);
    }

    #[test]
    fn a_failed_decision_call_is_a_model_failure_and_a_bad_answer_is_a_contract_one() {
        let rules = Rules::new("failures");
        rules.file("a.json", &rules.rule("no-unwrap", r"\.unwrap\(\)", "**/*.rs", 0.75));
        let d = doc(UNWRAP);

        let broken = inspector(&rules, ScriptedDecision::failing());
        match broken.inspect(&d, true).unwrap_err() {
            Failure::Model(m) => assert!(m.contains("decision call failed"), "{m}"),
            other => panic!("expected a model failure, got {other:?}"),
        }

        let garbled = inspector(&rules, ScriptedDecision::malformed());
        match garbled.inspect(&d, true).unwrap_err() {
            Failure::Contract(m) => assert!(m.contains("no-unwrap#1"), "names the answer: {m}"),
            other => panic!("expected a contract failure, got {other:?}"),
        }
    }

    #[test]
    fn a_decision_budget_refusal_is_refused_not_called() {
        let rules = Rules::new("budget");
        rules.file("a.json", &rules.rule("no-unwrap", r"\.unwrap\(\)", "**/*.rs", 0.75));
        let decision = ScriptedDecision::with(&[("no-unwrap#1", 0.9)]);
        let e = inspector(&rules, decision.clone());
        // The decision tier's own cap, not the chat tiers': a decision is not a chat call and
        // no longer spends their minute window.
        e.state.merge_config(Some(&serde_json::json!({
            "budget": {"max_decisions_per_min": 0}
        })));
        let f = e.inspect(&doc(UNWRAP), true).unwrap_err();
        assert!(matches!(f, Failure::Refused(_)), "{f:?}");
        assert_eq!(decision.calls(), 0, "the permit is taken before the call, never after");
    }

    /// A shipped rule set a test can read: the same document shape `default_rules/*.json`
    /// holds, so nothing here depends on the authoring sessions having landed their files.
    const SHIPPED: &[(&str, &str)] = &[(
        "code/shipped-unwrap.json",
        r#"{"schema": "jev.rules/1", "rules": [{
            "id": "shipped-unwrap",
            "title": "Shipped: unwrap in a handler",
            "text": "A handler must not unwrap.",
            "severity": "warning",
            "applies_to": ["**/*.rs"],
            "inspection": {"kind": "regex", "pattern": "\\.unwrap\\(\\)"},
            "judgement": {"question": "Is this unwrap reachable?", "min_probability": 0.75}
        }]}"#,
    )];

    /// An engine over `rules` with the shipped set in play, the way a fresh checkout runs.
    fn shipped_inspector(rules: &Rules, decision: Arc<ScriptedDecision>) -> Engine {
        let (e, _) = engine_full(&[], decision, SHIPPED);
        e.state.set_root(Some(rules.root.display().to_string()));
        e
    }

    #[test]
    fn a_repository_with_no_rules_of_its_own_is_inspected_by_the_shipped_set() {
        // The whole point of shipping defaults: a repository that has written no rules still
        // gets findings, through the pass the server runs, with the source of each finding said
        // out loud. Before this, the answer here was `no_rules` and nothing else — which is what
        // made a fresh install and a broken one indistinguishable.
        let rules = Rules::new("shipped-only");
        let decision = ScriptedDecision::with(&[("shipped-unwrap#1", 0.9), ("shipped-unwrap#2", 0.9)]);
        let e = shipped_inspector(&rules, decision.clone());

        let out = e.inspect(&doc(UNWRAP), true).unwrap();
        assert_eq!(out.considered, 1, "the shipped rule claimed the file");
        assert_eq!(out.findings.len(), 2, "and its candidates became findings");
        assert_eq!(decision.calls(), 1, "one decision call, as any rules pass makes");
        assert_eq!(
            out.findings[0].rule_source,
            Some(jev_core::types::RuleSource::Builtin),
            "and a reader can tell it came from the shipped set"
        );
        assert_eq!(out.findings[0].label, "Shipped: unwrap in a handler");

        // `no_rules` is not the answer any more, and the pass says what it ran on instead:
        // the reader is told where the rules came from and how to get them on disk.
        let codes: Vec<&str> = out.skipped.iter().map(|(c, _)| c.as_str()).collect();
        assert!(!codes.contains(&"no_rules"), "{:?}", out.skipped);
        assert!(codes.contains(&"default_rules"), "{:?}", out.skipped);
        assert!(
            out.skipped
                .iter()
                .any(|(_, d)| d.contains("jev rules init")),
            "and how to see them: {:?}",
            out.skipped
        );
    }

    #[test]
    fn the_repositorys_own_rule_shadows_the_shipped_one_and_is_not_reported_twice() {
        let rules = Rules::new("shadow");
        // The same id, the repository's own text, and a pattern that matches one line of the
        // two the shipped rule matches — so a pass that ran both would publish three findings
        // rather than two, under a label the reader could not attribute.
        rules.file(
            "mine.json",
            &rules
                .rule("shipped-unwrap", r"a\.unwrap\(\)", "**/*.rs", 0.75)
                .replace("Unwrap in a handler", "Ours: unwrap in a handler"),
        );
        let decision = ScriptedDecision::with(&[("shipped-unwrap#1", 0.9), ("shipped-unwrap#2", 0.9)]);
        let e = shipped_inspector(&rules, decision.clone());

        let out = e.inspect(&doc(UNWRAP), true).unwrap();
        assert_eq!(out.considered, 1, "one rule claims the file, not two");
        assert_eq!(out.candidates, 1, "the repository's pattern is the one that ran");
        assert_eq!(out.findings.len(), 1);
        assert_eq!(out.findings[0].label, "Ours: unwrap in a handler");
        assert_eq!(
            out.findings[0].rule_source,
            Some(jev_core::types::RuleSource::Repository)
        );
        assert!(
            !out.skipped.iter().any(|(c, _)| c == "default_rules"),
            "the repository has rules of its own: {:?}",
            out.skipped
        );
    }

    #[test]
    fn a_repository_with_no_rules_and_defaults_off_says_the_shipped_set_is_off() {
        // The other half of the setting: through the config path the server uses
        // (`workspace/configuration` → `merge_config`), with nothing else changed. The pass must
        // not report "none shipped in this build" while the build ships them — that sentence
        // would send the reader looking for a missing file that is right there.
        let rules = Rules::new("defaults-off");
        let decision = ScriptedDecision::with(&[("shipped-unwrap#1", 0.9)]);
        let e = shipped_inspector(&rules, decision.clone());
        e.state.merge_config(Some(&serde_json::json!({
            "rules": {"defaults": false}
        })));

        let out = e.inspect(&doc(UNWRAP), true).unwrap();
        assert_eq!(out.considered, 0);
        assert!(out.findings.is_empty());
        assert_eq!(decision.calls(), 0, "nothing to ask costs nothing");
        let no_rules = out
            .skipped
            .iter()
            .find(|(c, _)| c == "no_rules")
            .expect("the pass says why it did nothing");
        assert!(
            no_rules.1.contains("rules.defaults = false"),
            "and names the switch: {}",
            no_rules.1
        );
        assert!(!out.skipped.iter().any(|(c, _)| c == "default_rules"));
    }

    #[test]
    fn a_pass_with_no_rules_says_so_instead_of_finding_nothing() {
        // The demotion's edge: rules are the ambient path, and a repository that has written
        // none gets no ambient findings. The chat review does not step in — which makes saying
        // "there was nothing to run" the only thing standing between a user and silence.
        let rules = Rules::new("norules");
        let decision = ScriptedDecision::with(&[("anything", 0.9)]);
        let e = inspector(&rules, decision.clone());
        let d = doc(UNWRAP);

        let out = e.inspect(&d, true).unwrap();
        assert!(out.findings.is_empty());
        assert_eq!(out.considered, 0);
        assert_eq!(out.candidates, 0);
        assert_eq!(out.skipped.len(), 1, "{:?}", out.skipped);
        assert_eq!(out.skipped[0].0, "no_rules");
        assert!(
            out.skipped[0].1.contains("no rules loaded from"),
            "the reason names what was looked at: {:?}",
            out.skipped[0].1
        );
        assert!(
            out.skipped[0].1.contains(".jev/rules"),
            "and where it looked: {:?}",
            out.skipped[0].1
        );
        assert!(
            out.skipped[0].1.contains("nothing to run"),
            "and that nothing ran: {:?}",
            out.skipped[0].1
        );
        assert_eq!(decision.calls(), 0, "no rules means no question to ask");
        let stats = e.state.rules_stats();
        assert_eq!((stats.loaded, stats.calls), (0, 0), "and status can say so");
    }

    #[test]
    fn rules_that_do_not_claim_this_path_are_reported_too() {
        // The other half of the same silence: rules exist, and none of them is about this file.
        let rules = Rules::new("notmine");
        rules.file("a.json", &rules.rule("py-only", r"open\(", "**/*.py", 0.75));
        let decision = ScriptedDecision::with(&[("py-only#1", 0.9)]);
        let e = inspector(&rules, decision.clone());

        let out = e.inspect(&doc(UNWRAP), true).unwrap();
        assert!(out.findings.is_empty());
        assert_eq!(out.considered, 0);
        assert_eq!(out.skipped[0].0, "no_rules");
        assert!(
            out.skipped[0].1.contains("no rule applies to"),
            "the reason names the path that nothing claimed: {:?}",
            out.skipped[0].1
        );
        assert_eq!(decision.calls(), 0);
    }

    #[test]
    fn a_rules_pass_leaves_the_file_untouched_and_a_later_review_still_asks_the_model() {
        // The rules pass writes conclusions, never the document; and an explicit review must
        // not answer from the ambient cache a rules pass filled.
        let rules = Rules::new("sources");
        rules.file("a.json", &rules.rule("no-unwrap", r"\.unwrap\(\)", "**/*.rs", 0.75));
        let decision =
            ScriptedDecision::with(&[("no-unwrap#1", 0.9), ("no-unwrap#2", 0.9)]);
        let (e, chat) = engine_with(
            &[r#"{"findings":[{"anchor":{"match":"a.unwrap()"},"label":"from the model"}]}"#],
            decision,
        );
        e.state.set_root(Some(rules.root.display().to_string()));
        let d = doc(UNWRAP);

        let out = e.inspect(&d, true).unwrap();
        assert_eq!(out.findings.len(), 2);
        assert_eq!(d.text, UNWRAP, "the pass writes conclusions, not code");

        let reviewed = e.review_now(&d).unwrap();
        assert_eq!(reviewed.findings.len(), 1, "the chat tier was asked, not the cache");
        assert_eq!(reviewed.findings[0].label, "from the model");
        assert_eq!(chat.seen.lock().len(), 1);
        assert_eq!(
            e.cached(&d).unwrap().source.as_deref(),
            Some("review"),
            "and its conclusion replaced the ambient one for this content"
        );
        // `analyze` still answers from the cache: it is the ambient path, not a request.
        assert!(e.analyze(&d).unwrap().from_cache);
    }
}
