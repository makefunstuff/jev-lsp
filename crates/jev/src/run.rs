//! One-shot execution of a parsed command (PROTOCOL.md §11).
//!
//! Synchronous, stateless, and deliberately small: read the one file the user named, run the
//! cost gates, render the prompt `jev-core` owns, make one model call, validate the answer
//! against the contract that verb declares, and print exactly one JSON line. Nothing is
//! written, nothing is applied, and no directory is ever walked.
//!
//! The dependencies are injected rather than constructed, so the whole exit-code contract is
//! exercisable in-process: [`Fs`] is the real filesystem and stdin, and the tests substitute
//! a scripted backend, a memory of documents, and a budget that refuses.

use crate::cli::{self, Command, Invocation, Overrides, Source, Target};
use jev_core::budget::{Budget, Permit};
use jev_core::cache::{self, Cache, Conclusion};
use jev_core::changed;
use jev_core::config::{Config, TierConfig};
use jev_core::context;
use jev_core::contract;
use jev_core::decision::{BadAnswer, DecisionBackend};
use jev_core::document::{content_hash, Document};
use jev_core::edit::{self, BuildOptions};
use jev_core::findings;
use jev_core::gates;
use jev_core::inspections;
use jev_core::lang;
use jev_core::model::{Backend, ChatRequest, ChatResponse};
use jev_core::plan;
use jev_core::rules;
use jev_core::scope::{self, Resolved};
use jev_core::types::{
    ActionData, ActionState, DocRef, Finding, LineRange, Proposal, ScopeKind, ScopeRef,
    ScopeSource, Tier, Usage, Verb, ACTION_DATA_VERSION, ARTIFACT_SCHEMA, PROMPT_VERSION,
    RESULT_SCHEMA,
};
use jev_core::verbs::{self, PromptSpec};
use serde_json::{json, Value};
use std::io::Read;
use std::time::Instant;

pub const EXIT_OK: i32 = 0;
pub const EXIT_TRANSPORT: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_BUDGET: i32 = 3;
pub const EXIT_STALE: i32 = 4;

/// The version stamped on the document this process read.
///
/// There is no editor here to own a version counter, so the number names the read that
/// produced the text. What makes it checkable, as PROTOCOL.md §8 intends, is the content
/// hash: the document is re-read before anything is emitted, and a change is exit 4.
const READ_VERSION: i32 = 1;

/// What the process did: an exit code, at most one JSON line, and diagnostics.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub code: i32,
    /// The single JSON line. `None` when the request never became work: a usage error has no
    /// artifact and no result to report.
    pub stdout: Option<String>,
    pub stderr: Vec<String>,
}

/// Where document text comes from.
pub trait Files {
    fn read(&self, source: &Source) -> std::io::Result<String>;
}

/// The real filesystem, and the real stdin.
pub struct Fs;

impl Files for Fs {
    fn read(&self, source: &Source) -> std::io::Result<String> {
        match source {
            // Only the file the user named, ever.
            Source::File(path) => std::fs::read_to_string(path),
            Source::Stdin => {
                let mut text = String::new();
                std::io::stdin().read_to_string(&mut text)?;
                Ok(text)
            }
        }
    }
}

/// Everything a command needs. Constructed once by `main`, substituted wholesale by tests.
pub struct Deps<'a> {
    pub config: Config,
    pub backend: &'a dyn Backend,
    /// The decision tier, for `jev inspect`. A separate protocol from the chat backend, so a
    /// separate dependency.
    pub decision: &'a dyn DecisionBackend,
    pub budget: &'a Budget,
    pub cache: &'a Cache,
    pub files: &'a dyn Files,
}

/// Parse and run. The only entry point, so what the tests exercise is what the binary does.
pub fn run(args: &[String], deps: &Deps) -> Outcome {
    match cli::parse(args) {
        Err(e) => Outcome {
            code: EXIT_USAGE,
            stdout: None,
            stderr: vec![format!("jev: {e}"), "jev: try `jev --help`".to_string()],
        },
        Ok(Invocation::Help) => Outcome {
            code: EXIT_OK,
            stdout: Some(cli::USAGE.trim_end().to_string()),
            stderr: Vec::new(),
        },
        Ok(Invocation::Version) => Outcome {
            code: EXIT_OK,
            stdout: Some(format!("jev {}", jev_core::VERSION)),
            stderr: Vec::new(),
        },
        Ok(Invocation::Run(command, overrides)) => dispatch(&command, &overrides, deps),
    }
}

fn dispatch(command: &Command, overrides: &Overrides, deps: &Deps) -> Outcome {
    let mut config = deps.config.clone();
    overrides.apply(&mut config);
    match command {
        Command::Status => status(&config, deps),
        Command::Explain(target) => with_log(|log| explain(&config, target, deps, log)),
        Command::Review(target) => with_log(|log| review(&config, target, deps, log)),
        Command::Action { verb, target } => {
            with_log(|log| action(&config, *verb, target, deps, log))
        }
        Command::Plan { goal, target } => {
            with_log(|log| plan_for(&config, goal, target, deps, log))
        }
        Command::Inspect { target, force } => {
            with_log(|log| inspect(&config, target, *force, deps, log))
        }
    }
}

/// Run one command, carrying its diagnostics through to the outcome.
fn with_log(body: impl FnOnce(&mut Vec<String>) -> R<Value>) -> Outcome {
    let mut log = Vec::new();
    settle(body(&mut log), log)
}

fn settle(result: R<Value>, log: Vec<String>) -> Outcome {
    match result {
        Ok(value) => Outcome {
            code: EXIT_OK,
            stdout: Some(one_line(&value)),
            stderr: log,
        },
        Err(failure) => {
            let mut stderr = log;
            stderr.push(format!("jev: {}", failure.message()));
            // The request never became work, so there is nothing to report as a result.
            let stdout = if matches!(failure, Failure::Usage(_)) {
                None
            } else {
                Some(one_line(&result_err(failure.code(), failure.message())))
            };
            Outcome {
                code: failure.exit(),
                stdout,
                stderr,
            }
        }
    }
}

/// Why a command produced nothing showable. The variant decides the exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Failure {
    /// The command line names something that does not exist, or a position the document does
    /// not have.
    Usage(String),
    /// The endpoint could not be reached, or the model answered with an error.
    Transport(String),
    /// The answer does not satisfy the contract it was asked for, or names something that
    /// cannot be located in the document.
    Contract(String),
    /// A gate refused the document: binary, too large, or ignored by configuration.
    Skipped(String),
    /// A cost gate refused the call (PROTOCOL.md §5).
    Budget(String),
    /// The document moved between the read and the answer.
    Stale(String),
}

impl Failure {
    fn exit(&self) -> i32 {
        match self {
            // PROTOCOL.md §11 puts a contract violation with the usage errors, and a gate
            // refusal is the same kind of statement: this request cannot be served.
            Failure::Usage(_) | Failure::Contract(_) | Failure::Skipped(_) => EXIT_USAGE,
            Failure::Transport(_) => EXIT_TRANSPORT,
            Failure::Budget(_) => EXIT_BUDGET,
            Failure::Stale(_) => EXIT_STALE,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Failure::Usage(_) => "usage",
            Failure::Transport(_) => "transport_error",
            Failure::Contract(_) => "contract_error",
            Failure::Skipped(_) => "skipped",
            Failure::Budget(_) => "over_budget",
            Failure::Stale(_) => "stale",
        }
    }

    fn message(&self) -> &str {
        match self {
            Failure::Usage(m)
            | Failure::Transport(m)
            | Failure::Contract(m)
            | Failure::Skipped(m)
            | Failure::Budget(m)
            | Failure::Stale(m) => m,
        }
    }
}

type R<T> = Result<T, Failure>;

/// The document, the scope the command asks about, and where the text came from.
struct Prepared {
    doc: Document,
    scope: Resolved,
    source: Source,
}

// ---------------------------------------------------------------- commands

/// Budget, queue and cache. No document, no endpoint, no model call: this must answer even
/// when nothing else can.
fn status(config: &Config, deps: &Deps) -> Outcome {
    let spent = deps.budget.snapshot();
    let (hits, misses, entries) = deps.cache.stats();
    let (calls, refusals) = deps.budget.counters();
    let (limit_minute, limit_hour, limit_tokens) = Budget::limits(&config.budget);
    let body = json!({
        "schema": RESULT_SCHEMA,
        "ok": true,
        "version": jev_core::VERSION,
        "enabled": config.enabled,
        // One process, one command: there is no daemon holding a queue and no session for
        // counters to accumulate in, so every number here is what *this* invocation spent.
        // The limits and the endpoints are the part a user checks.
        "analysis_in_flight": spent.in_flight > 0,
        "queue": {"depth": 0, "in_flight": spent.in_flight, "pending": 0},
        "cache": {"entries": entries, "hits": hits, "misses": misses},
        "budget": {
            "calls_last_minute": spent.calls_last_minute,
            "decisions_last_minute": spent.decisions_last_minute,
            "calls_last_hour": spent.calls_last_hour,
            "tokens_used": spent.tokens_used,
            "in_flight": spent.in_flight,
            "limit_per_minute": limit_minute,
            "limit_decisions_per_minute": config.budget.max_decisions_per_min,
            "limit_per_hour": limit_hour,
            "limit_tokens": limit_tokens,
        },
        "counters": {"calls": calls, "refusals": refusals},
        "models": {
            "reason": model_json(&config.models.reason),
            "review": model_json(&config.models.review),
        },
        "triggers": {"diagnostics": config.triggers.diagnostics, "idle_ms": config.triggers.idle_ms},
    });
    Outcome {
        code: EXIT_OK,
        stdout: Some(one_line(&body)),
        stderr: Vec::new(),
    }
}

fn model_json(tier: &TierConfig) -> Value {
    json!({
        "base_url": tier.base_url,
        "model": tier.model,
        "max_tokens": tier.max_tokens,
        "timeout_ms": tier.timeout_ms,
    })
}

/// An explanation artifact (PROTOCOL.md §7).
fn explain(config: &Config, target: &Target, deps: &Deps, log: &mut Vec<String>) -> R<Value> {
    let prepared = prepare(config, target, deps)?;
    let key = cache::op_key(
        Verb::Explain.as_str(),
        PROMPT_VERSION,
        &config.models.reason.model,
        &prepared.doc.language.name,
        &prepared.doc.hash,
        prepared.scope.range.start_line,
        prepared.scope.range.end_line,
        "",
    );
    let cached = deps.cache.get(&key).and_then(|c| c.artifact.clone());
    let (markdown, usage, from_cache) = match cached {
        Some(markdown) => (markdown, None, true),
        None => {
            let context = context::build(&prepared.doc, &prepared.scope, &[], 0);
            let spec = verbs::render(Verb::Explain, &context);
            let (response, usage) = call(config, Verb::Explain.tier(), &spec, deps, log)?;
            let answer = contract::parse_artifact(&response.text)
                .map_err(|e| Failure::Contract(e.to_string()))?;
            if answer.markdown.trim().is_empty() {
                return Err(Failure::Contract("the artifact carried no text".to_string()));
            }
            deps.cache.put(
                &key,
                Conclusion {
                    artifact: Some(answer.markdown.clone()),
                    ..Default::default()
                },
            );
            (answer.markdown, Some(usage), false)
        }
    };
    stale_check(deps, &prepared)?;

    let artifact = json!({
        "schema": ARTIFACT_SCHEMA,
        "kind": "explanation",
        "id": ActionData::make_id(
            Verb::Explain,
            &document_reference(&prepared.doc),
            &scope_reference(&prepared.scope),
            None,
        ),
        // The same clock the server stamps artifacts with (jev-core::time).
        "created": jev_core::time::now_rfc3339(),
        "language": prepared.doc.language.name,
        "summary": action_title(Verb::Explain, &prepared.scope),
        "markdown": markdown,
        "from_cache": from_cache,
    });
    Ok(with_usage(artifact, usage))
}

/// Findings for the document (PROTOCOL.md §9), as a result envelope.
fn review(config: &Config, target: &Target, deps: &Deps, log: &mut Vec<String>) -> R<Value> {
    let prepared = prepare(config, target, deps)?;
    let key = cache::findings_key(
        &prepared.doc.hash,
        &prepared.doc.language.name,
        config.noise.max_visible_findings,
    );
    let (found, usage, rejected, from_cache) = match deps.cache.get(&key) {
        Some(conclusion) => (conclusion.findings.clone(), None, 0usize, true),
        None => {
            let context = context::build(&prepared.doc, &prepared.scope, &[], 0);
            let spec = verbs::render(Verb::Review, &context);
            let (response, usage) = call(config, Verb::Review.tier(), &spec, deps, log)?;
            let answer = contract::parse_findings(&response.text)
                .map_err(|e| Failure::Contract(e.to_string()))?;
            let built = findings::build(
                &prepared.doc.text,
                &answer,
                &lang::profile(&prepared.doc.language.name),
                config.noise.max_visible_findings,
            );
            deps.cache.put(
                &key,
                Conclusion {
                    findings: built.findings.clone(),
                    ..Default::default()
                },
            );
            (built.findings, Some(usage), built.rejected, false)
        }
    };
    stale_check(deps, &prepared)?;

    let diagnostics: Vec<Value> = found
        .iter()
        .map(|finding| diagnostic(finding, &prepared.doc.hash))
        .collect();
    let result = json!({
        "schema": RESULT_SCHEMA,
        "ok": true,
        "diagnostics": diagnostics,
        // Findings whose anchors could not be located are dropped, never guessed at; saying
        // how many keeps the report honest (PROTOCOL.md §12, "silent skips").
        "rejected": rejected,
        "from_cache": from_cache,
    });
    Ok(with_usage(result, usage))
}

/// A proposed edit, in the frozen `documentChanges` shape (PROTOCOL.md §8). Proposed only.
fn action(
    config: &Config,
    verb: Verb,
    target: &Target,
    deps: &Deps,
    log: &mut Vec<String>,
) -> R<Value> {
    let prepared = prepare(config, target, deps)?;
    let key = cache::op_key(
        verb.as_str(),
        PROMPT_VERSION,
        &config.tier(verb.tier()).model,
        &prepared.doc.language.name,
        &prepared.doc.hash,
        prepared.scope.range.start_line,
        prepared.scope.range.end_line,
        "",
    );
    let (proposal, usage, from_cache) = match deps.cache.get(&key).and_then(|c| c.edit.clone()) {
        Some(proposal) => (proposal, None, true),
        None => {
            let context = context::build(&prepared.doc, &prepared.scope, &[], 0);
            let spec = verbs::render(verb, &context);
            let (response, usage) = call(config, verb.tier(), &spec, deps, log)?;
            let answer = contract::parse_edit(&response.text)
                .map_err(|e| Failure::Contract(e.to_string()))?;
            let options = BuildOptions {
                max_scope_lines: config.languages.max_scope_lines,
                scope_lines: Some((prepared.scope.range.start_line, prepared.scope.range.end_line)),
            };
            let proposal = edit::build_proposal(
                &prepared.doc.text,
                &answer,
                &lang::profile(&prepared.doc.language.name),
                &options,
            )
            .map_err(|e| Failure::Contract(e.to_string()))?;
            deps.cache.put(
                &key,
                Conclusion {
                    edit: Some(proposal.clone()),
                    ..Default::default()
                },
            );
            (proposal, Some(usage), false)
        }
    };
    // Before an edit leaves this process the document is re-read: an edit computed against
    // text that has since changed is not an edit, it is a hazard (PROTOCOL.md §8 rule 3).
    stale_check(deps, &prepared)?;

    let document = document_reference(&prepared.doc);
    let scope = scope_reference(&prepared.scope);
    let id = ActionData::make_id(verb, &document, &scope, None);
    let data = ActionData {
        v: ACTION_DATA_VERSION,
        id: id.clone(),
        verb,
        state: ActionState::Ready,
        doc: document,
        scope,
        scope_source: prepared.scope.source,
        language: prepared.doc.language.name.clone(),
        finding: None,
        summary: Some(proposal.summary.clone()),
    };
    let result = json!({
        "schema": RESULT_SCHEMA,
        "ok": true,
        "action": data,
        "edit": workspace_edit(&prepared.doc, &proposal),
        "edit_ids": [id],
        "from_cache": from_cache,
    });
    Ok(with_usage(result, usage))
}

/// The repository's rules, run over one file (PROTOCOL.md §6, `jev.inspect`).
///
/// The same pass the language server's ambient path runs, over the same rules, through the same
/// code: `inspections::select` finds the candidates, one decision call answers them, and
/// `inspections::resolve` builds the findings. That is deliberate — a CLI that disagreed with the
/// server about what the rules say would be worse than no CLI.
fn inspect(
    config: &Config,
    target: &Target,
    force: bool,
    deps: &Deps,
    log: &mut Vec<String>,
) -> R<Value> {
    let prepared = prepare(config, target, deps)?;
    let doc = &prepared.doc;
    let root = rules_root(&doc.path).unwrap_or_else(|| ".".to_string());
    let set = rules::load(std::path::Path::new(&root));
    // `applies_to` is written the way a repository names its own files, so it is matched
    // against the path relative to the root — the same rule the server follows.
    let match_path = gates::relative_to(&doc.path, &root);
    let (considered, asked) = inspections::select(
        &set.rules,
        match_path,
        &doc.text,
        config.rules.max_candidates_per_rule,
    );
    let candidates = asked.len();
    let mut skipped = set.skipped.clone();
    // The same skip the language server reports for the same tree, from the same function: a
    // repository with no rules that claim this file has nothing to run, and says so.
    if let Some(skip) = inspections::nothing_to_run(&set, considered, &doc.path, &root) {
        skipped.push(skip);
    }

    // One process, one command: the cache is always cold here, so a hit can only come from
    // something this invocation already did. Kept anyway, because the key is what a *rule edit*
    // invalidates and having the shape right is what stops the CLI and the server drifting.
    let key = cache::rules_key(&doc.hash, &set.hash, &doc.path);
    let built = match deps.cache.get(&key) {
        Some(hit) => findings::FindingBuild {
            findings: hit.findings.clone(),
            rejected: 0,
        },
        None => {
            if !force {
                match changed::changed_paths(&root) {
                    Ok(changed) if !changed.contains(&doc.path) => {
                        let mut body = envelope();
                        merge(
                            &mut body,
                            jev_core::findings::inspect_fields(
                                &[],
                                0,
                                0,
                                &[(doc.path.clone(), "unchanged".to_string())],
                            ),
                        );
                        return Ok(body);
                    }
                    // Changed, or unanswerable. Either way the safe answer is to inspect.
                    _ => {}
                }
            }
            if asked.is_empty() {
                findings::FindingBuild {
                    findings: Vec::new(),
                    rejected: 0,
                }
            } else {
                // Before the call, never after (PROTOCOL.md §5).
                let _permit = match deps.budget.try_acquire_decision(&config.budget) {
                    Permit::Granted => Held(deps.budget),
                    Permit::Refused(refusal) => {
                        return Err(Failure::Budget(refusal.reason().to_string()))
                    }
                };
                let request = inspections::request(&doc.path, &doc.text, &asked, &config.rules);
                let started = Instant::now();
                let response = deps
                    .decision
                    .decide(config.decision(), &request)
                    .map_err(|e| match e.downcast_ref::<BadAnswer>() {
                        // An answer that arrived and cannot be read is a contract violation;
                        // an endpoint that did not answer is a transport failure. The exit codes
                        // differ, so the distinction has to survive this far.
                        Some(bad) => Failure::Contract(bad.to_string()),
                        None => Failure::Transport(format!("decision call failed: {e:#}")),
                    })?;
                deps.budget
                    .record_tokens(response.input_tokens + response.output_tokens);
                log.push(format!(
                    "jev: model={} tier=decide tokens_in={} tokens_out={} ms={} trigger=cli",
                    config.decision().model,
                    response.input_tokens,
                    response.output_tokens,
                    started.elapsed().as_millis() as u64
                ));
                let built = inspections::resolve(
                    &doc.text,
                    &asked,
                    &response,
                    &lang::profile(&doc.language.name),
                    config.noise.max_visible_findings,
                );
                deps.cache.put(
                    &key,
                    Conclusion {
                        findings: built.findings.clone(),
                        source: Some("rules".to_string()),
                        ..Default::default()
                    },
                );
                built
            }
        }
    };
    if built.rejected > 0 {
        skipped.push((
            "unlocatable_anchor".to_string(),
            format!(
                "{} finding(s) could not be anchored on a unique line",
                built.rejected
            ),
        ));
    }
    // The file was read twice; the second read is compared with the first, exactly as it is for
    // every other command that produces a conclusion about text.
    stale_check(deps, &prepared)?;

    let mut body = envelope();
    merge(
        &mut body,
        jev_core::findings::inspect_fields(&built.findings, considered, candidates, &skipped),
    );
    Ok(body)
}

/// The result envelope a command builds before adding its own fields.
///
/// The CLI prints what it returns, so it carries the schema and `ok` itself rather than relying on
/// a wrapper: a Result envelope with a missing `schema` is a Result envelope a client cannot
/// identify.
fn envelope() -> Value {
    json!({"schema": RESULT_SCHEMA, "ok": true})
}

/// The root a rules pass reads from: the repository the file belongs to.
///
/// `.jev/rules/` lives at the repository root, and the language server reads it from its
/// workspace root — so the CLI must resolve the same root, or the two front ends disagree about
/// which rules exist. It used to return the file's own directory, so `jev inspect
/// crates/jev-core/src/rules.rs` looked for `crates/jev-core/src/.jev/rules`, found nothing, and
/// answered `no_rules` while the server found the rules: a parity failure that a flat fixture
/// could not show.
///
/// The nearest ancestor holding `.git` is that root, which is what `git rev-parse --show-toplevel`
/// would report; when no ancestor has one there is no repository to report either, so the file's
/// own directory is the honest answer.
fn rules_root(path: &str) -> Option<String> {
    let dir = path.rsplit_once('/').map(|(dir, _)| {
        if dir.is_empty() {
            "/".to_string()
        } else {
            dir.to_string()
        }
    })?;
    let mut here = std::path::PathBuf::from(&dir);
    loop {
        if here.join(".git").exists() {
            return Some(here.to_string_lossy().into_owned());
        }
        match here.parent() {
            Some(parent) if parent != here => here = parent.to_path_buf(),
            _ => return Some(dir),
        }
    }
}

/// A plan artifact (PROTOCOL.md §7). The plan names work; it applies nothing.
fn plan_for(
    config: &Config,
    goal: &str,
    target: &Target,
    deps: &Deps,
    log: &mut Vec<String>,
) -> R<Value> {
    let prepared = prepare(config, target, deps)?;
    let key = cache::op_key(
        "plan",
        PROMPT_VERSION,
        &config.models.reason.model,
        &prepared.doc.language.name,
        &prepared.doc.hash,
        prepared.scope.range.start_line,
        prepared.scope.range.end_line,
        "",
    );
    // The cached thing is the artifact's body: id, goal, language, steps, usage, and the
    // count of steps that named nothing locatable.
    let cached = deps
        .cache
        .get(&key)
        .and_then(|c| c.artifact.clone())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let (body, from_cache) = match cached {
        Some(body) => (body, true),
        None => {
            let context = context::build(&prepared.doc, &prepared.scope, &[], 0);
            let spec = verbs::render_plan(&context, goal);
            let (response, usage) = call(config, Tier::Reason, &spec, deps, log)?;
            let answer = contract::parse_plan(&response.text)
                .map_err(|e| Failure::Contract(e.to_string()))?;
            if answer.steps.is_empty() {
                return Err(Failure::Contract(
                    "the plan named no steps; a plan with nothing in it is not a plan"
                        .to_string(),
                ));
            }
            let built = plan::build(
                &prepared.doc.uri,
                prepared.doc.version,
                goal,
                &prepared.doc.language.name,
                &prepared.doc.text,
                &answer,
                usage,
            );
            if built.plan.steps.is_empty() {
                // Every step quoted text that is not in the document, and a step that cannot
                // be located is not a step (jev-core::plan). Say so, rather than print an
                // empty plan that reads like agreement.
                return Err(Failure::Contract(format!(
                    "no step named a location that occurs in the document ({} rejected)",
                    built.rejected
                )));
            }
            let body = plan_body(&built);
            if let Ok(text) = serde_json::to_string(&body) {
                deps.cache.put(
                    &key,
                    Conclusion {
                        artifact: Some(text),
                        ..Default::default()
                    },
                );
            }
            (body, false)
        }
    };
    stale_check(deps, &prepared)?;

    let mut artifact = json!({
        "schema": ARTIFACT_SCHEMA,
        "kind": "plan",
        // The same clock the server stamps artifacts with (jev-core::time).
        "created": jev_core::time::now_rfc3339(),
    });
    merge(&mut artifact, body);
    artifact["from_cache"] = json!(from_cache);
    Ok(artifact)
}

// ---------------------------------------------------------------- the pipeline

/// Read the one file the user named, gate it, and resolve the scope the command asks about.
fn prepare(config: &Config, target: &Target, deps: &Deps) -> R<Prepared> {
    let text = deps
        .files
        .read(&target.source)
        .map_err(|e| Failure::Usage(format!("cannot read {}: {e}", target.source.describe())))?;
    let uri = match &target.source {
        Source::File(path) => file_uri(&absolute(path)),
        // A document from stdin has no path. `jev-core` reads the language out of the
        // content instead, and `unknown` is a valid answer (PROTOCOL.md N11).
        Source::Stdin => "file://-".to_string(),
    };
    let doc = Document::new(&uri, READ_VERSION, text, None);
    check_position(target, &doc)?;

    if let Some(skip) = gates::evaluate(
        &doc.text,
        &doc.path,
        config.languages.max_file_bytes,
        &config.languages.ignore,
    ) {
        return Err(Failure::Skipped(format!(
            "{} is not analysed: {} ({})",
            doc.path,
            skip.reason(),
            skip.code()
        )));
    }

    let profile = lang::profile(&doc.language.name);
    let max_lines = config.languages.max_scope_lines;
    let scope = match (target.range, target.line) {
        (Some((start, end)), _) => scope::resolve(
            &doc.text,
            start,
            &profile,
            Some(LineRange {
                start_line: start,
                end_line: end,
            }),
            max_lines,
        ),
        (None, Some(line)) => scope::resolve(&doc.text, line, &profile, None, max_lines),
        // No position at all means the whole document: `explain` and `review` describe the
        // file, and `action` without a range proposes a change to all of it.
        (None, None) => whole_file(&doc),
    };
    Ok(Prepared {
        doc,
        scope,
        source: target.source.clone(),
    })
}

/// One model call: the permits, the request, the elapsed time, and the cost line.
///
/// Every cost gate is checked *before* the call and never after (PROTOCOL.md §5).
fn call(
    config: &Config,
    tier_kind: Tier,
    spec: &PromptSpec,
    deps: &Deps,
    log: &mut Vec<String>,
) -> R<(ChatResponse, Usage)> {
    let _permit = match deps.budget.try_acquire(&config.budget) {
        Permit::Granted => Held(deps.budget),
        Permit::Refused(refusal) => return Err(Failure::Budget(refusal.reason().to_string())),
    };
    let tier = config.tier(tier_kind);
    let request = ChatRequest {
        system: spec.system.clone(),
        user: spec.user.clone(),
        temperature: tier.temperature,
        max_tokens: ceiling(spec.max_tokens, tier),
        json: spec.json,
        think: tier.think,
    };
    let started = Instant::now();
    let response = deps
        .backend
        .chat(tier, &request)
        // `{e:#}` prints the whole `anyhow` chain: a transport failure without its cause reads
        // as "the endpoint is down" when it was a timeout at 5 s.
        .map_err(|e| Failure::Transport(format!("{e:#}")))?;
    let ms = started.elapsed().as_millis() as u64;
    deps.budget.record_tokens(response.total_tokens());
    log.push(format!(
        "jev: model={} tier={} tokens_in={} tokens_out={} ms={ms} trigger=cli",
        tier.model,
        tier_name(tier_kind),
        response.prompt_tokens,
        response.completion_tokens
    ));
    let usage = Usage {
        model: tier.model.clone(),
        tier: tier_name(tier_kind).to_string(),
        tokens_in: response.prompt_tokens,
        tokens_out: response.completion_tokens,
        ms,
        changes: None,
        files: None,
    };
    Ok((response, usage))
}

/// Releases the permit however the call ends, including on an early return.
struct Held<'a>(&'a Budget);

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// The prompt's own budget is a ceiling and so is the tier's: the lower one wins.
fn ceiling(spec_max: u32, tier: &TierConfig) -> u32 {
    if tier.max_tokens == 0 {
        spec_max
    } else {
        spec_max.min(tier.max_tokens)
    }
}

fn tier_name(tier: Tier) -> &'static str {
    match tier {
        Tier::Reason => "reason",
        Tier::Review => "review",
    }
}

/// Re-read the document and compare content hashes (exit 4).
///
/// A document that arrived on stdin cannot move under us; one on disk can, and the answer
/// printed to stdout would describe text that is no longer there.
fn stale_check(deps: &Deps, prepared: &Prepared) -> R<()> {
    if matches!(prepared.source, Source::Stdin) {
        return Ok(());
    }
    match deps.files.read(&prepared.source) {
        Ok(again) if content_hash(&again) == prepared.doc.hash => Ok(()),
        Ok(_) => Err(Failure::Stale(format!(
            "{} changed while the request was in flight; the answer describes the previous \
             content",
            prepared.doc.path
        ))),
        Err(e) => Err(Failure::Stale(format!(
            "{} could not be re-read after the request was built: {e}",
            prepared.doc.path
        ))),
    }
}

// ---------------------------------------------------------------- shapes

/// The frozen edit shape: `documentChanges`, an explicit version on every edit, never a bare
/// `changes` map (PROTOCOL.md N4, §8).
///
/// A created file carries `version: null`, which §8 permits only for a document the server
/// itself authors — which is exactly what a file created by this edit is.
fn workspace_edit(doc: &Document, proposal: &Proposal) -> Value {
    let mut operations = Vec::new();
    for file in &proposal.new_files {
        let uri = file_uri(&format!("{}/{}", parent_dir(&doc.path), file.path));
        operations.push(json!({
            "kind": "create",
            "uri": uri,
            "options": {"overwrite": false, "ignoreIfExists": false},
        }));
        let mut content = file.content.clone();
        if !content.ends_with('\n') {
            content.push('\n');
        }
        operations.push(json!({
            "textDocument": {"uri": uri, "version": Value::Null},
            "edits": [{
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}},
                "newText": content,
            }],
        }));
    }
    if !proposal.ops.is_empty() {
        let edits: Vec<Value> = proposal
            .ops
            .iter()
            .map(|op| {
                json!({
                    "range": {
                        "start": {"line": op.start_line, "character": 0},
                        "end": {"line": op.end_line, "character": op.end_col},
                    },
                    "newText": op.new_text,
                })
            })
            .collect();
        operations.push(json!({
            "textDocument": {"uri": doc.uri, "version": doc.version},
            "edits": edits,
        }));
    }
    json!({"documentChanges": operations})
}

fn plan_body(built: &plan::PlanBuild) -> Value {
    json!({
        "id": built.plan.id,
        "goal": built.plan.goal,
        "language": built.plan.language,
        "steps": built.plan.steps,
        "usage": built.plan.usage,
        "rejected": built.rejected,
    })
}

/// Fold an artifact's body into its envelope. A body that is not an object contributes
/// nothing rather than a bogus field.
fn merge(envelope: &mut Value, body: Value) {
    if let (Value::Object(into), Value::Object(from)) = (envelope, body) {
        for (key, value) in from {
            into.insert(key, value);
        }
    }
}

/// Add the cost of the call that produced this result. A conclusion served from the dedupe
/// cache has no call behind it, and therefore no cost to report.
fn with_usage(result: Value, usage: Option<Usage>) -> Value {
    let Some(usage) = usage else {
        return result;
    };
    let mut result = result;
    if let Value::Object(map) = &mut result {
        map.insert("usage".to_string(), json!(usage));
    }
    result
}

fn result_err(code: &str, message: &str) -> Value {
    json!({
        "schema": RESULT_SCHEMA,
        "ok": false,
        "error": {"code": code, "message": message},
    })
}

/// A `Finding`, plus the keys PROTOCOL.md §9 requires every finding to carry.
fn diagnostic(finding: &Finding, content_hash: &str) -> Value {
    json!({
        "id": finding.id,
        "line": finding.line,
        "start_col": finding.start_col,
        "end_col": finding.end_col,
        "severity": finding.severity,
        "label": finding.label,
        "detail": finding.detail,
        "verb": finding.verb_hint,
        "data": {
            "finding_id": finding.id,
            "verb": finding.verb_hint,
            "content_hash": content_hash,
        },
    })
}

fn whole_file(doc: &Document) -> Resolved {
    Resolved {
        range: LineRange {
            start_line: 0,
            end_line: doc.line_count().saturating_sub(1),
        },
        kind: ScopeKind::File,
        source: ScopeSource::WholeFile,
        name: None,
        truncated: false,
    }
}

fn scope_reference(scope: &Resolved) -> ScopeRef {
    ScopeRef {
        kind: scope.kind,
        name: scope.name.clone(),
        start_line: scope.range.start_line,
        end_line: scope.range.end_line,
    }
}

fn document_reference(doc: &Document) -> DocRef {
    DocRef {
        uri: doc.uri.clone(),
        version: doc.version,
        content_hash: doc.hash.clone(),
    }
}

/// The editor's action title: deterministic, never model output (PROTOCOL.md §4).
fn action_title(verb: Verb, scope: &Resolved) -> String {
    let subject = scope
        .name
        .clone()
        .unwrap_or_else(|| scope.kind.as_str().to_string());
    format!("{}: {}", verb.label(), subject)
}

// ---------------------------------------------------------------- paths and time

/// The position the command named must exist in the document: a line past the end is a usage
/// error, not an empty answer.
fn check_position(target: &Target, doc: &Document) -> R<()> {
    let last = doc.line_count().saturating_sub(1);
    let check = |line: u32| -> R<()> {
        if line > last {
            return Err(Failure::Usage(format!(
                "line {} is past the end of {} ({} lines)",
                line + 1,
                doc.path,
                doc.line_count()
            )));
        }
        Ok(())
    };
    if let Some((start, end)) = target.range {
        check(start)?;
        check(end)?;
    }
    if let Some(line) = target.line {
        check(line)?;
    }
    if let Some(col) = target.col {
        let line = target.line.unwrap_or(0);
        let len = scope::line_len(&doc.text, line);
        if col > len {
            return Err(Failure::Usage(format!(
                "column {} is past the end of line {} of {} ({} bytes)",
                col + 1,
                line + 1,
                doc.path,
                len
            )));
        }
    }
    Ok(())
}

/// Make a path absolute without resolving symlinks: the document is the one the user named,
/// and `docs/LANGUAGE.md` resolves its language from exactly that spelling.
fn absolute(path: &str) -> String {
    if path.starts_with('/') {
        return path.to_string();
    }
    match std::env::current_dir() {
        Ok(cwd) => format!("{}/{}", cwd.display(), path),
        Err(_) => path.to_string(),
    }
}

fn parent_dir(path: &str) -> String {
    match path.rsplit_once('/') {
        Some(("", _)) => "/".to_string(),
        Some((dir, _)) => dir.to_string(),
        None => ".".to_string(),
    }
}

/// A `file://` URI, percent-encoded the way the LSP spells one. `jev-core`'s `path_from_uri`
/// decodes exactly this shape, which is what the round-trip test pins.
fn file_uri(path: &str) -> String {
    let mut uri = String::from("file://");
    for byte in path.bytes() {
        match byte {
            b'/' | b'-' | b'_' | b'.' | b'~' | b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' => {
                uri.push(byte as char)
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

/// The single line stdout carries. A `Value` cannot fail to serialize; the expect is here so
/// a future payload that cannot be serialized fails loudly instead of printing nothing.
fn one_line(value: &Value) -> String {
    serde_json::to_string(value).expect("a serde_json::Value always serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use jev_core::decision::{DecisionAnswer, DecisionRequest, DecisionResponse, DecisionValue};
    use jev_core::document::path_from_uri;
    use jev_core::model::OpenAiCompat;
    use jev_core::types::Severity;
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const RUST: &str = "fn retry() {\n    let f = File::open(p)?;\n}\n";
    const REVIEW_JSON: &str = r#"{"findings":[{"anchor":{"match":"File::open(p)"},"label":"unchecked error path","detail":"the failure is discarded","severity":"warning","verb_hint":"fix"}]}"#;
    const EDIT_JSON: &str = r#"{"summary":"harden the retry","rationale":"the open is unchecked","replacements":[{"anchor":{"kind":"function","match":"fn retry()"},"replacement":"fn retry() {\n    let f = File::open(p)?;\n    ok();\n}"}]}"#;
    const ARTIFACT_JSON: &str =
        r###"{"summary":"s","markdown":"# What it does\n\nReads a file."}"###;
    const PLAN_JSON: &str = r#"{"goal":"make it cancellable","steps":[{"title":"thread a token","rationale":"r","verb":"harden","anchors":[{"match":"File::open(p)"}]}]}"#;
    const TEST_JSON: &str = r##"{"summary":"add tests","rationale":"r","replacements":[],"new_files":[{"path":"tests/retry.rs","content":"#[test]\nfn t() {}\n"}]}"##;

    /// A backend that answers from a script and remembers what it was asked.
    #[derive(Clone)]
    struct Scripted {
        inner: Arc<ScriptedInner>,
    }

    struct ScriptedInner {
        responses: Mutex<Vec<String>>,
        seen: Mutex<Vec<ChatRequest>>,
        calls: AtomicUsize,
    }

    impl Scripted {
        fn new(responses: &[&str]) -> Scripted {
            Scripted {
                inner: Arc::new(ScriptedInner {
                    responses: Mutex::new(responses.iter().rev().map(|s| s.to_string()).collect()),
                    seen: Mutex::new(Vec::new()),
                    calls: AtomicUsize::new(0),
                }),
            }
        }

        fn calls(&self) -> usize {
            self.inner.calls.load(Ordering::SeqCst)
        }

        fn seen(&self) -> Vec<ChatRequest> {
            self.inner.seen.lock().clone()
        }
    }

    impl Backend for Scripted {
        fn chat(&self, _tier: &TierConfig, request: &ChatRequest) -> anyhow::Result<ChatResponse> {
            self.inner.calls.fetch_add(1, Ordering::SeqCst);
            self.inner.seen.lock().push(request.clone());
            let text = self
                .inner
                .responses
                .lock()
                .pop()
                .unwrap_or_else(|| "the script has nothing left to say".to_string());
            Ok(ChatResponse {
                text,
                prompt_tokens: 11,
                completion_tokens: 7,
                finish_reason: Some("stop".to_string()),
                had_reasoning: false,
            })
        }
    }

    /// Documents held in memory: scripted files (which can change between reads), scripted
    /// stdin, and a record of every read, so a test can prove nothing else was touched.
    #[derive(Clone)]
    struct Memory {
        inner: Arc<MemoryInner>,
    }

    struct MemoryInner {
        files: Mutex<HashMap<String, Vec<String>>>,
        stdin: String,
        reads: Mutex<Vec<String>>,
    }

    impl Memory {
        fn new(stdin: &str) -> Memory {
            Memory {
                inner: Arc::new(MemoryInner {
                    files: Mutex::new(HashMap::new()),
                    stdin: stdin.to_string(),
                    reads: Mutex::new(Vec::new()),
                }),
            }
        }

        fn with(self, path: &str, text: &str) -> Memory {
            self.inner
                .files
                .lock()
                .insert(path.to_string(), vec![text.to_string()]);
            self
        }

        /// A later read of the same path returns different content.
        fn moved(self, path: &str, text: &str) -> Memory {
            self.inner
                .files
                .lock()
                .entry(path.to_string())
                .or_default()
                .push(text.to_string());
            self
        }

        fn reads(&self) -> Vec<String> {
            self.inner.reads.lock().clone()
        }
    }

    impl Files for Memory {
        fn read(&self, source: &Source) -> std::io::Result<String> {
            self.inner.reads.lock().push(source.describe());
            match source {
                Source::Stdin => Ok(self.inner.stdin.clone()),
                Source::File(path) => {
                    let mut files = self.inner.files.lock();
                    let missing =
                        || std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
                    let queue = files.get_mut(path).ok_or_else(missing)?;
                    if queue.len() > 1 {
                        Ok(queue.remove(0))
                    } else {
                        queue.first().cloned().ok_or_else(missing)
                    }
                }
            }
        }
    }

    struct Harness {
        backend: Box<dyn Backend>,
        decision: Arc<dyn DecisionBackend>,
        budget: Budget,
        cache: Cache,
        config: Config,
        files: Box<dyn Files>,
    }

    /// A decision backend that answers every question with a fixed probability, and remembers
    /// what it was asked.
    struct ScriptedDecision {
        probability: f64,
        seen: Mutex<Vec<DecisionRequest>>,
    }

    impl ScriptedDecision {
        fn new(probability: f64) -> Arc<ScriptedDecision> {
            Arc::new(ScriptedDecision {
                probability,
                seen: Mutex::new(Vec::new()),
            })
        }
    }

    impl DecisionBackend for ScriptedDecision {
        fn decide(
            &self,
            _cfg: &jev_core::config::DecisionTierConfig,
            req: &DecisionRequest,
        ) -> anyhow::Result<DecisionResponse> {
            self.seen.lock().push(req.clone());
            Ok(DecisionResponse {
                answers: req
                    .questions
                    .iter()
                    .map(|q| DecisionAnswer {
                        id: q.id.clone(),
                        value: DecisionValue::Bool(self.probability > 0.5),
                        probability: Some(self.probability),
                        reason: Some("reachable".to_string()),
                    })
                    .collect(),
                input_tokens: 12,
                output_tokens: 3,
            })
        }
    }

    impl Harness {
        fn new(backend: Box<dyn Backend>, files: Box<dyn Files>) -> Harness {
            Harness {
                backend,
                decision: ScriptedDecision::new(0.0),
                budget: Budget::new(4),
                cache: Cache::new(8),
                config: Config::default(),
                files,
            }
        }

        fn decision(mut self, decision: Arc<ScriptedDecision>) -> Harness {
            self.decision = decision;
            self
        }

        fn config(mut self, config: Config) -> Harness {
            self.config = config;
            self
        }

        fn cache(mut self, cache: Cache) -> Harness {
            self.cache = cache;
            self
        }

        fn budget(mut self, budget: Budget) -> Harness {
            self.budget = budget;
            self
        }

        fn deps(&self) -> Deps<'_> {
            Deps {
                config: self.config.clone(),
                backend: &*self.backend,
                decision: &*self.decision,
                budget: &self.budget,
                cache: &self.cache,
                files: &*self.files,
            }
        }

        fn run(&self, args: &[&str]) -> Outcome {
            let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            run(&owned, &self.deps())
        }
    }

    /// stdout must be exactly one line of JSON.
    fn line(out: &Outcome) -> Value {
        let text = out
            .stdout
            .as_ref()
            .unwrap_or_else(|| panic!("no stdout; stderr: {:?}", out.stderr));
        assert!(!text.contains('\n'), "stdout is one line: {text}");
        serde_json::from_str(text).unwrap_or_else(|e| panic!("stdout is JSON ({e}): {text}"))
    }

    fn rust_files() -> Memory {
        Memory::new("").with("a.rs", RUST)
    }

    #[test]
    fn review_prints_one_line_of_findings() {
        let backend = Scripted::new(&[REVIEW_JSON]);
        let harness = Harness::new(Box::new(backend.clone()), Box::new(rust_files()));
        let out = harness.run(&["review", "a.rs"]);

        assert_eq!(out.code, EXIT_OK, "{:?}", out.stderr);
        let value = line(&out);
        assert_eq!(value["schema"], RESULT_SCHEMA);
        assert_eq!(value["ok"], true);
        let diagnostics = value["diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0]["label"], "unchecked error path");
        assert_eq!(diagnostics[0]["severity"], "warning");
        assert_eq!(diagnostics[0]["line"], 1);
        assert_eq!(diagnostics[0]["data"]["verb"], "fix");
        assert_eq!(
            diagnostics[0]["data"]["content_hash"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        assert_eq!(value["usage"]["tier"], "review");
        assert_eq!(value["usage"]["tokens_in"], 11);
        assert_eq!(value["usage"]["tokens_out"], 7);
        assert_eq!(value["from_cache"], false);
        assert!(
            out.stderr.iter().any(|l| l.contains("tokens_in=11")),
            "one cost line per call: {:?}",
            out.stderr
        );
        assert_eq!(backend.calls(), 1);
    }

    #[test]
    fn only_the_named_file_is_ever_read() {
        let files = rust_files();
        let harness = Harness::new(
            Box::new(Scripted::new(&[REVIEW_JSON])),
            Box::new(files.clone()),
        );
        let out = harness.run(&["review", "a.rs"]);
        assert_eq!(out.code, EXIT_OK, "{:?}", out.stderr);
        assert_eq!(
            files.reads(),
            vec!["a.rs".to_string(), "a.rs".to_string()],
            "the document, then the staleness re-read, and nothing else"
        );
    }

    #[test]
    fn a_dash_reads_the_document_from_stdin() {
        let files = Memory::new(RUST);
        let harness = Harness::new(
            Box::new(Scripted::new(&[REVIEW_JSON])),
            Box::new(files.clone()),
        );
        let out = harness.run(&["review", "-"]);
        assert_eq!(out.code, EXIT_OK, "{:?}", out.stderr);
        assert_eq!(line(&out)["diagnostics"].as_array().unwrap().len(), 1);
        assert_eq!(files.reads(), vec!["stdin".to_string()]);
    }

    #[test]
    fn explain_prints_an_explanation_artifact_for_the_cursor_scope() {
        let backend = Scripted::new(&[ARTIFACT_JSON]);
        let harness = Harness::new(Box::new(backend.clone()), Box::new(rust_files()));
        let out = harness.run(&["explain", "a.rs:2"]);

        assert_eq!(out.code, EXIT_OK, "{:?}", out.stderr);
        let value = line(&out);
        assert_eq!(value["schema"], ARTIFACT_SCHEMA);
        assert_eq!(value["kind"], "explanation");
        assert_eq!(value["language"], "rust");
        assert_eq!(value["summary"], "Explain: retry");
        assert!(value["markdown"].as_str().unwrap().contains("What it does"));
        assert_eq!(value["id"].as_str().unwrap().len(), 16);
        assert_eq!(value["usage"]["model"], "qwen2.5-coder-7b-instruct");
        let created = value["created"].as_str().unwrap();
        assert_eq!(created.len(), 20, "{created} is RFC 3339 to the second");
        assert!(created.ends_with('Z'));
        assert_eq!(created.as_bytes()[10], b'T', "{created}");

        let seen = backend.seen();
        assert!(
            seen[0].user.contains("SCOPE: function retry"),
            "the cursor line picked the function: {}",
            seen[0].user
        );
    }

    #[test]
    fn plan_prints_a_plan_artifact_whose_steps_are_located_in_the_document() {
        let backend = Scripted::new(&[PLAN_JSON]);
        let harness = Harness::new(Box::new(backend.clone()), Box::new(rust_files()));
        let out = harness.run(&["plan", "--goal", "make it cancellable", "a.rs"]);

        assert_eq!(out.code, EXIT_OK, "{:?}", out.stderr);
        let value = line(&out);
        assert_eq!(value["schema"], ARTIFACT_SCHEMA);
        assert_eq!(value["kind"], "plan");
        assert_eq!(value["goal"], "make it cancellable");
        assert_eq!(value["language"], "rust");
        let steps = value["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0]["title"], "thread a token");
        assert_eq!(steps[0]["verb"], "harden");
        assert_eq!(steps[0]["status"], "proposed");
        assert_eq!(steps[0]["targets"][0]["line"], 1);
        assert_eq!(steps[0]["targets"][0]["version"], READ_VERSION);
        assert_eq!(value["usage"]["tier"], "reason");
        assert!(
            backend.seen()[0].user.contains("GOAL: make it cancellable"),
            "the goal reaches the model"
        );
    }

    #[test]
    fn a_plan_whose_steps_name_nothing_locatable_is_a_contract_violation() {
        let answer = r#"{"goal":"g","steps":[{"title":"t","verb":"harden","anchors":[{"match":"not in the file"}]}]}"#;
        let harness = Harness::new(Box::new(Scripted::new(&[answer])), Box::new(rust_files()));
        let out = harness.run(&["plan", "--goal", "g", "a.rs"]);
        assert_eq!(out.code, EXIT_USAGE, "{:?}", out.stderr);
        assert_eq!(line(&out)["error"]["code"], "contract_error");
    }

    #[test]
    fn a_range_suffix_scopes_the_action_to_those_lines() {
        let answer = r#"{"summary":"note it","rationale":"r","replacements":[{"anchor":{"kind":"statement","match":"three"},"replacement":"three  # documented"}]}"#;
        let files = Memory::new("").with("a.rs", "one\ntwo\nthree\nfour\n");
        let harness = Harness::new(Box::new(Scripted::new(&[answer])), Box::new(files.clone()));
        let out = harness.run(&["action", "--verb", "docs", "a.rs:2-3"]);

        assert_eq!(out.code, EXIT_OK, "{:?}", out.stderr);
        let value = line(&out);
        assert_eq!(value["action"]["verb"], "docs");
        assert_eq!(value["action"]["scope"]["kind"], "selection");
        assert_eq!(value["action"]["scope"]["start_line"], 1);
        assert_eq!(value["action"]["scope"]["end_line"], 2);
        assert_eq!(value["action"]["scope_source"], "explicit");
        assert_eq!(value["action"]["state"], "ready");
        assert_eq!(value["edit"]["documentChanges"][0]["edits"][0]["range"]["start"]["line"], 2);
        assert_eq!(files.reads().len(), 2);
    }

    #[test]
    fn action_prints_a_proposed_edit_and_leaves_the_file_on_disk_byte_identical() {
        let path = temp_path("action");
        std::fs::write(&path, RUST).unwrap();
        let before = std::fs::read(&path).unwrap();

        let backend = Scripted::new(&[EDIT_JSON]);
        let harness = Harness::new(Box::new(backend.clone()), Box::new(Fs));
        let out = harness.run(&["action", "--verb", "harden", &format!("{path}:1-3")]);

        assert_eq!(out.code, EXIT_OK, "{:?}", out.stderr);
        let value = line(&out);
        assert_eq!(value["ok"], true);
        let edit = value
            .get("edit")
            .unwrap_or_else(|| panic!("the result carries the proposed edit: {value}"));
        let changes = edit["documentChanges"].as_array().unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0]["textDocument"]["uri"], file_uri(&path));
        assert_eq!(changes[0]["textDocument"]["version"], READ_VERSION);
        assert_eq!(changes[0]["edits"][0]["range"]["start"]["line"], 0);
        assert_eq!(changes[0]["edits"][0]["range"]["end"]["line"], 2);
        assert!(changes[0]["edits"][0]["newText"]
            .as_str()
            .unwrap()
            .contains("ok();"));
        assert_eq!(value["edit_ids"].as_array().unwrap().len(), 1);
        assert_eq!(
            value["edit_ids"][0].as_str().unwrap(),
            value["action"]["id"].as_str().unwrap()
        );
        assert_eq!(value["action"]["doc"]["version"], READ_VERSION);
        assert_eq!(value["action"]["summary"], "harden the retry");

        // The whole point: proposing an edit writes nothing.
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "action must never touch the file"
        );
        clean_up(&path);
    }

    #[test]
    fn a_created_file_is_a_create_operation_with_a_null_version() {
        let harness = Harness::new(Box::new(Scripted::new(&[TEST_JSON])), Box::new(rust_files()));
        let out = harness.run(&["action", "--verb", "test", "a.rs"]);
        assert_eq!(out.code, EXIT_OK, "{:?}", out.stderr);
        let value = line(&out);
        let changes = value["edit"]["documentChanges"].as_array().unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0]["kind"], "create");
        assert!(changes[0]["uri"]
            .as_str()
            .unwrap()
            .ends_with("/tests/retry.rs"));
        assert_eq!(changes[1]["textDocument"]["version"], Value::Null);
        assert!(changes[1]["edits"][0]["newText"]
            .as_str()
            .unwrap()
            .ends_with('\n'));
    }

    #[test]
    fn a_model_endpoint_that_cannot_be_reached_exits_1_with_one_json_line() {
        let harness = Harness::new(Box::new(OpenAiCompat::new()), Box::new(rust_files()));
        let out = harness.run(&["review", "--base-url", "http://127.0.0.1:1/v1", "a.rs"]);

        assert_eq!(out.code, EXIT_TRANSPORT, "{:?}", out.stderr);
        let value = line(&out);
        assert_eq!(value["schema"], RESULT_SCHEMA);
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "transport_error");
        assert!(
            out.stderr.iter().any(|l| l.contains("POST")),
            "the reason is on stderr: {:?}",
            out.stderr
        );
    }

    #[test]
    fn status_needs_no_endpoint_and_still_succeeds() {
        let harness = Harness::new(Box::new(OpenAiCompat::new()), Box::new(Memory::new("")));
        let out = harness.run(&[
            "status",
            "--base-url",
            "http://127.0.0.1:1/v1",
            "--model",
            "m",
            "--max-tokens",
            "77",
        ]);

        assert_eq!(out.code, EXIT_OK, "{:?}", out.stderr);
        let value = line(&out);
        assert_eq!(value["ok"], true);
        assert_eq!(value["version"], jev_core::VERSION);
        assert_eq!(value["budget"]["limit_per_minute"], 6);
        assert_eq!(value["budget"]["limit_per_hour"], 120);
        assert_eq!(value["budget"]["limit_tokens"], 500_000);
        assert_eq!(value["queue"]["depth"], 0);
        assert_eq!(value["queue"]["in_flight"], 0);
        assert_eq!(value["cache"]["entries"], 0);
        assert_eq!(value["models"]["reason"]["base_url"], "http://127.0.0.1:1/v1");
        assert_eq!(value["models"]["review"]["model"], "m");
        assert_eq!(value["models"]["reason"]["max_tokens"], 77);
        assert!(out.stderr.is_empty(), "{:?}", out.stderr);
    }

    #[test]
    fn the_environment_is_the_last_word_before_an_explicit_flag() {
        let mut config = Config::default();
        config.models.reason.base_url = "http://from-env/v1".to_string();
        let harness = Harness::new(Box::new(Scripted::new(&[])), Box::new(Memory::new("")))
            .config(config);

        assert_eq!(
            line(&harness.run(&["status"]))["models"]["reason"]["base_url"],
            "http://from-env/v1"
        );
        assert_eq!(
            line(&harness.run(&["status", "--base-url", "http://from-flag/v1"]))["models"]
                ["reason"]["base_url"],
            "http://from-flag/v1"
        );
    }

    #[test]
    fn an_unknown_verb_exits_2_and_writes_nothing_to_stdout() {
        let harness = Harness::new(Box::new(Scripted::new(&[])), Box::new(rust_files()));
        let out = harness.run(&["action", "--verb", "polish", "a.rs"]);
        assert_eq!(out.code, EXIT_USAGE);
        assert!(out.stdout.is_none());
        assert!(
            out.stderr.iter().any(|l| l.contains("not a verb")),
            "{:?}",
            out.stderr
        );
    }

    #[test]
    fn a_missing_file_exits_2_and_writes_nothing_to_stdout() {
        let harness = Harness::new(Box::new(Scripted::new(&[])), Box::new(rust_files()));
        let out = harness.run(&["review", "nope.py"]);
        assert_eq!(out.code, EXIT_USAGE);
        assert!(out.stdout.is_none());
        assert!(
            out.stderr.iter().any(|l| l.contains("cannot read nope.py")),
            "{:?}",
            out.stderr
        );
    }

    #[test]
    fn a_line_past_the_end_of_the_document_exits_2() {
        let harness = Harness::new(Box::new(Scripted::new(&[])), Box::new(rust_files()));
        let out = harness.run(&["explain", "a.rs:99"]);
        assert_eq!(out.code, EXIT_USAGE);
        assert!(out.stdout.is_none());
        assert!(
            out.stderr.iter().any(|l| l.contains("past the end")),
            "{:?}",
            out.stderr
        );
    }

    #[test]
    fn a_column_past_the_end_of_the_line_exits_2() {
        let harness = Harness::new(Box::new(Scripted::new(&[])), Box::new(rust_files()));
        let out = harness.run(&["explain", "a.rs:1:99"]);
        assert_eq!(out.code, EXIT_USAGE);
        assert!(out.stdout.is_none());
        assert!(
            out.stderr
                .iter()
                .any(|l| l.contains("past the end of line 1")),
            "{:?}",
            out.stderr
        );
    }

    #[test]
    fn a_document_a_gate_refuses_exits_2_and_names_the_gate() {
        let files = Memory::new("").with("a.rs", "a\0b");
        let harness = Harness::new(Box::new(Scripted::new(&[])), Box::new(files));
        let out = harness.run(&["review", "a.rs"]);
        assert_eq!(out.code, EXIT_USAGE);
        let value = line(&out);
        assert_eq!(value["error"]["code"], "skipped");
        assert!(value["error"]["message"].as_str().unwrap().contains("binary"));
    }

    #[test]
    fn an_exhausted_call_budget_exits_3_without_calling_the_model() {
        let backend = Scripted::new(&[REVIEW_JSON]);
        let harness = Harness::new(Box::new(backend.clone()), Box::new(rust_files()));
        let mut config = Config::default();
        config.budget.max_calls_per_min = 0;
        let out = harness.config(config).run(&["review", "a.rs"]);

        assert_eq!(out.code, EXIT_BUDGET, "{:?}", out.stderr);
        let value = line(&out);
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "over_budget");
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("per-minute"));
        assert_eq!(backend.calls(), 0, "a refusal must not become a call");
    }

    #[test]
    fn a_token_budget_that_is_already_spent_exits_3_before_the_call() {
        // A one-shot process starts with nothing spent, so the session token cap cannot
        // refuse its first call; a budget that *has* been spent must refuse, which is the
        // gate this pins. Both refusals reach the same exit code.
        let backend = Scripted::new(&[REVIEW_JSON]);
        let budget = Budget::new(4);
        budget.record_tokens(200);
        let harness =
            Harness::new(Box::new(backend.clone()), Box::new(rust_files())).budget(budget);
        let mut config = Config::default();
        config.budget.max_tokens_per_session = 100;
        let out = harness.config(config).run(&["review", "a.rs"]);

        assert_eq!(out.code, EXIT_BUDGET, "{:?}", out.stderr);
        let value = line(&out);
        assert_eq!(value["error"]["code"], "over_budget");
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("token budget"));
        assert_eq!(backend.calls(), 0);
    }

    #[test]
    fn a_model_answer_that_breaks_its_contract_exits_2() {
        let harness = Harness::new(
            Box::new(Scripted::new(&["I read the file and it looks fine to me."])),
            Box::new(rust_files()),
        );
        let out = harness.run(&["review", "a.rs"]);
        assert_eq!(out.code, EXIT_USAGE, "{:?}", out.stderr);
        let value = line(&out);
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "contract_error");
    }

    #[test]
    fn an_edit_whose_anchor_is_not_in_the_document_exits_2_without_an_edit() {
        let answer =
            r#"{"summary":"s","replacements":[{"anchor":{"match":"not here"},"replacement":"x"}]}"#;
        let harness = Harness::new(Box::new(Scripted::new(&[answer])), Box::new(rust_files()));
        let out = harness.run(&["action", "--verb", "harden", "a.rs"]);
        assert_eq!(out.code, EXIT_USAGE, "{:?}", out.stderr);
        let value = line(&out);
        assert_eq!(value["error"]["code"], "contract_error");
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("does not occur"),
            "{value}"
        );
        assert!(value.get("edit").is_none(), "no edit is handed out");
    }

    #[test]
    fn a_target_that_moves_while_the_request_is_in_flight_exits_4_without_an_edit() {
        let files = Memory::new("")
            .with("a.rs", RUST)
            .moved("a.rs", &format!("{RUST}// moved while you were thinking\n"));
        let harness = Harness::new(Box::new(Scripted::new(&[EDIT_JSON])), Box::new(files));
        let out = harness.run(&["action", "--verb", "harden", "a.rs:1"]);

        assert_eq!(out.code, EXIT_STALE, "{:?}", out.stderr);
        let value = line(&out);
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "stale");
        assert!(
            value.get("edit").is_none(),
            "an edit computed against old text is not handed out: {value}"
        );
    }

    #[test]
    fn a_document_that_moves_before_the_findings_are_printed_exits_4() {
        let files = Memory::new("")
            .with("a.rs", RUST)
            .moved("a.rs", "fn other() {}\n");
        let harness = Harness::new(Box::new(Scripted::new(&[REVIEW_JSON])), Box::new(files));
        let out = harness.run(&["review", "a.rs"]);
        assert_eq!(out.code, EXIT_STALE, "{:?}", out.stderr);
        assert_eq!(line(&out)["error"]["code"], "stale");
    }

    #[test]
    fn the_dedupe_gate_serves_a_cached_conclusion_without_calling_the_model() {
        let backend = Scripted::new(&[]);
        let cache = Cache::new(4);
        cache.put(
            &cache::findings_key(&content_hash(RUST), "rust", 5),
            Conclusion {
                findings: vec![Finding {
                    id: "abc123".to_string(),
                    line: 1,
                    start_col: 12,
                    end_col: 24,
                    severity: Severity::Warning,
                    label: "cached finding".to_string(),
                    detail: "detail".to_string(),
                    verb_hint: Verb::Fix,
                }],
                ..Default::default()
            },
        );
        let harness = Harness::new(Box::new(backend.clone()), Box::new(rust_files())).cache(cache);
        let out = harness.run(&["review", "a.rs"]);

        assert_eq!(out.code, EXIT_OK, "{:?}", out.stderr);
        let value = line(&out);
        assert_eq!(value["diagnostics"][0]["label"], "cached finding");
        assert_eq!(value["from_cache"], true);
        assert!(value.get("usage").is_none(), "no call, no cost to report");
        assert_eq!(backend.calls(), 0, "the cache served it (PROTOCOL.md §5 gate 1)");
        assert!(out.stderr.is_empty(), "{:?}", out.stderr);
    }

    #[test]
    fn help_lists_every_command_and_flag() {
        let harness = Harness::new(Box::new(Scripted::new(&[])), Box::new(Memory::new("")));
        let out = harness.run(&["--help"]);
        assert_eq!(out.code, EXIT_OK);
        let text = out.stdout.clone().unwrap();
        for expected in [
            "jev explain",
            "jev review",
            "jev action",
            "jev plan",
            "jev status",
            "--verb",
            "--goal",
            "--base-url",
            "--model",
            "--max-tokens",
        ] {
            assert!(text.contains(expected), "{expected} missing from --help");
        }
        assert!(out.stderr.is_empty());
    }

    #[test]
    fn version_is_one_line() {
        let harness = Harness::new(Box::new(Scripted::new(&[])), Box::new(Memory::new("")));
        let out = harness.run(&["--version"]);
        assert_eq!(out.code, EXIT_OK);
        assert_eq!(
            out.stdout.as_deref(),
            Some(format!("jev {}", jev_core::VERSION).as_str())
        );
    }

    #[test]
    fn the_file_uri_round_trips_through_the_parser_jev_core_uses() {
        let uri = file_uri("/tmp/a b/ünïcode.rs");
        assert_eq!(uri, "file:///tmp/a%20b/%C3%BCn%C3%AFcode.rs");
        assert_eq!(path_from_uri(&uri), "/tmp/a b/ünïcode.rs");
    }

    #[test]
    fn the_lower_of_the_prompt_and_the_tier_is_what_is_requested() {
        let backend = Scripted::new(&[ARTIFACT_JSON]);
        let harness = Harness::new(Box::new(backend.clone()), Box::new(rust_files()));
        let out = harness.run(&["explain", "--max-tokens", "100", "a.rs"]);
        assert_eq!(out.code, EXIT_OK, "{:?}", out.stderr);
        assert_eq!(backend.seen()[0].max_tokens, 100);

        let backend = Scripted::new(&[ARTIFACT_JSON]);
        let harness = Harness::new(Box::new(backend.clone()), Box::new(rust_files()));
        let out = harness.run(&["explain", "--max-tokens", "100000", "a.rs"]);
        assert_eq!(out.code, EXIT_OK, "{:?}", out.stderr);
        assert_eq!(
            backend.seen()[0].max_tokens,
            2048,
            "the prompt's own budget caps it"
        );
    }

    #[test]
    fn a_whole_file_command_shows_the_model_the_whole_file() {
        let backend = Scripted::new(&[REVIEW_JSON]);
        let harness = Harness::new(Box::new(backend.clone()), Box::new(rust_files()));
        let out = harness.run(&["review", "a.rs"]);
        assert_eq!(out.code, EXIT_OK, "{:?}", out.stderr);
        let seen = backend.seen();
        assert!(seen[0].user.contains("SCOPE: file"), "{}", seen[0].user);
        assert!(seen[0].user.contains("let f = File::open(p)?;"));
        assert!(seen[0].system.contains("single JSON object"));
    }

    /// A rules directory beside a file on disk, and a harness pointed at it.
    fn rules_dir(tag: &str, rule: &str) -> String {
        let dir = std::env::temp_dir().join(format!("jev-cli-rules-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".jev/rules")).unwrap();
        std::fs::write(dir.join(".jev/rules/a.json"), rule).unwrap();
        dir.to_string_lossy().into_owned()
    }

    const UNWRAP_RS: &str = "fn h() {\n    a.unwrap();\n}\n";

    const RULE: &str = r#"{"schema":"jev.rules/1","rules":[{
        "id":"no-unwrap","title":"Unwrap in a handler","text":"A handler must not unwrap.",
        "severity":"warning","applies_to":["**/*.rs"],
        "inspection":{"kind":"regex","pattern":"\\.unwrap\\(\\)"},
        "judgement":{"question":"Is this unwrap reachable?",
                     "criteria":{"true":"reachable","false":"test code"},
                     "min_probability":0.75},
        "verb_hint":"fix"}]}"#;

    #[test]
    fn inspect_prints_one_json_line_of_rule_findings_and_leaves_the_file_alone() {
        let dir = rules_dir("ok", RULE);
        let path = format!("{dir}/a.rs");
        std::fs::write(&path, UNWRAP_RS).unwrap();
        let before = std::fs::read(&path).unwrap();
        let decision = ScriptedDecision::new(0.9);
        // The real filesystem: the point of this check is that the file on disk is read and left
        // alone, which a memory store cannot demonstrate.
        let harness = Harness::new(Box::new(Scripted::new(&[])), Box::new(Fs))
            .decision(decision.clone());
        let out = harness.run(&["inspect", &path]);
        assert_eq!(out.code, EXIT_OK, "{:?}", out.stderr);
        let body = line(&out);
        assert_eq!(body["schema"], RESULT_SCHEMA);
        assert_eq!(body["ok"], true);
        assert_eq!(body["considered"], 1);
        assert_eq!(body["candidates"], 1);
        let findings = body["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1, "{body}");
        assert_eq!(findings[0]["label"], "Unwrap in a handler");
        assert_eq!(findings[0]["line"], 1);
        assert_eq!(findings[0]["verb"], "fix");
        assert!(findings[0]["detail"].as_str().unwrap().contains("p=0.90"));
        assert_eq!(decision.seen.lock().len(), 1, "one decision call for the file");
        assert_eq!(decision.seen.lock()[0].questions.len(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), before, "the file is untouched");
        assert!(
            out.stderr.iter().any(|l| l.contains("tier=decide")),
            "{:?}",
            out.stderr
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inspect_below_the_rules_floor_publishes_nothing_and_force_skips_the_changed_check() {
        let dir = rules_dir("quiet", RULE);
        let path = format!("{dir}/a.rs");
        std::fs::write(&path, UNWRAP_RS).unwrap();

        // A probability under the rule's own floor: the candidate is not a finding.
        let low = ScriptedDecision::new(0.4);
        let harness = Harness::new(Box::new(Scripted::new(&[])), Box::new(Fs))
            .decision(low.clone());
        let out = harness.run(&["inspect", &path]);
        assert_eq!(out.code, EXIT_OK, "{:?}", out.stderr);
        let body = line(&out);
        assert_eq!(body["candidates"], 1, "it was looked at");
        assert!(body["findings"].as_array().unwrap().is_empty(), "{body}");
        assert_eq!(low.seen.lock().len(), 1, "the question was still asked");

        // `--force` skips the changed-set check, so a tree git cannot describe is inspected
        // anyway. That is the difference the flag exists for.
        let forced = Harness::new(Box::new(Scripted::new(&[])), Box::new(Fs))
            .decision(ScriptedDecision::new(0.9));
        let out = forced.run(&["inspect", "--force", &path]);
        assert_eq!(out.code, EXIT_OK, "{:?}", out.stderr);
        assert_eq!(line(&out)["findings"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn temp_path(tag: &str) -> String {
        let dir = std::env::temp_dir().join(format!("jev-cli-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("f.rs").to_string_lossy().into_owned()
    }

    fn clean_up(path: &str) {
        if let Some(parent) = std::path::Path::new(path).parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}
