//! The language server surface (PROTOCOL.md §2, §3).

use crate::engine::{Engine, Failure, Generated, Outcome};
use crate::state::{AppState, Claim};
use jev_core::types::{
    ActionData, ActionState, Finding, LineRange, Proposal, ScopeKind, ScopeRef, ScopeSource,
    Severity, Verb, ACTION_DATA_VERSION, ARTIFACT_SCHEMA, RESULT_SCHEMA,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tower_lsp::jsonrpc::Result as RpcResult;
use tower_lsp::lsp_types::notification::Progress;
use tower_lsp::lsp_types::*;
use tower_lsp::{async_trait, Client, LanguageServer};

pub struct JevServer {
    client: Client,
    state: Arc<AppState>,
    engine: Engine,
}

/// Which pass a document is owed. The three differ in what they run and in which slot they hold;
/// how they are supervised is the same and lives in `spawn_pass`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    /// The chat review tier, cached. The ambient pass when rules are disabled.
    Review,
    /// The chat review, forced — the user asked for it, so it never answers from the cache.
    ReviewNow,
    /// The repository's rules.
    Rules,
    /// Whichever pass the settings say: the rules pass when they are enabled, the chat review
    /// when they are not.
    ///
    /// A variant rather than a decision made by the caller, because the caller cannot make it:
    /// a save can land before the client's settings arrive, and answering "which pass" from the
    /// built-in defaults is how a client that turned rules off gets a rules pass anyway.
    Ambient,
}

impl Pass {
    /// How this pass names itself in a log line and in the session record.
    fn source(self) -> &'static str {
        match self {
            Pass::Rules => "rules",
            _ => "review",
        }
    }
}

/// What a completed pass produced, in the one shape the record and the log both read.
struct Passed {
    findings: Vec<Finding>,
    /// Findings produced but dropped because their anchor could not be located.
    discarded: usize,
    from_cache: bool,
    source: &'static str,
    /// What the pass declined to look at: skipped rule files, an unchanged document, a document
    /// beyond the per-pass cap. Reported rather than swallowed.
    skipped: Vec<(String, String)>,
}

impl Passed {
    fn from_review(out: Outcome, pass: Pass) -> Passed {
        Passed {
            findings: out.findings,
            discarded: out.rejected,
            from_cache: out.from_cache,
            source: pass.source(),
            skipped: Vec::new(),
        }
    }

    fn from_rules(out: crate::engine::InspectOutcome) -> Passed {
        Passed {
            findings: out.findings,
            // The rules pass reports anchors it could not locate in `skipped`, with the count,
            // rather than as a bare number.
            discarded: 0,
            from_cache: false,
            source: "rules",
            skipped: out.skipped,
        }
    }

    /// One session-record entry. `kind` stays `analysis` so the surfaces that count published
    /// findings count rules findings too — which is the point of publishing them the same way.
    fn trace_entry(&self, uri: &str) -> Value {
        let mut entry = json!({
            "kind": "analysis",
            "source": self.source,
            "uri": uri,
            "count": self.findings.len(),
            "discarded": self.discarded,
            "from_cache": self.from_cache,
            // What the run actually kept, not only how many. The count alone cannot answer
            // "what is it complaining about on this repository", which is the question the
            // volume measurement exists to answer; `label` is already clipped at MAX_LABEL, so
            // a line stays a line.
            "findings": self
                .findings
                .iter()
                .map(|f| json!({
                    "line": f.line,
                    "severity": severity_str(f.severity),
                    "label": f.label,
                }))
                .collect::<Vec<_>>(),
            // The first finding's line, so the entry can be walked back to the place it is about.
            "line": self.findings.first().map(|f| f.line),
        });
        if !self.skipped.is_empty() {
            entry["skipped"] = json!(
                self.skipped
                    .iter()
                    .map(|(path, reason)| json!({"path": path, "reason": reason}))
                    .collect::<Vec<_>>()
            );
        }
        entry
    }
}

impl JevServer {
    pub fn new(client: Client, state: Arc<AppState>) -> JevServer {
        let engine = Engine::new(state.clone());
        JevServer {
            client,
            state,
            engine,
        }
    }

    // ---- capabilities -------------------------------------------------------

    /// Only what is actually implemented is advertised: a provider we do not serve is a
    /// lie the client will act on (PROTOCOL.md §2).
    fn capabilities() -> ServerCapabilities {
        ServerCapabilities {
            position_encoding: Some(PositionEncodingKind::UTF8),
            text_document_sync: Some(TextDocumentSyncCapability::Options(
                TextDocumentSyncOptions {
                    open_close: Some(true),
                    change: Some(TextDocumentSyncKind::INCREMENTAL),
                    will_save: None,
                    will_save_wait_until: None,
                    save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                        include_text: Some(false),
                    })),
                },
            )),
            code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
                code_action_kinds: Some(vec![
                    CodeActionKind::QUICKFIX,
                    CodeActionKind::new("quickfix.jev"),
                    CodeActionKind::REFACTOR_REWRITE,
                    CodeActionKind::new("refactor.rewrite.jev"),
                    CodeActionKind::SOURCE,
                    CodeActionKind::new("source.jev"),
                    CodeActionKind::SOURCE_FIX_ALL,
                ]),
                work_done_progress_options: WorkDoneProgressOptions::default(),
                resolve_provider: Some(true),
            })),
            diagnostic_provider: Some(DiagnosticServerCapabilities::Options(DiagnosticOptions {
                identifier: Some("jev".to_string()),
                inter_file_dependencies: false,
                // Deliberately false, and it must stay false until `workspace/diagnostic` is
                // implemented. A client that checks `workspaceDiagnostics` first and prefers
                // that branch when it is set will route every `workspace/diagnostic/refresh`
                // to `workspace/diagnostic`, so advertising it while serving only
                // per-document diagnostics means the refresh is answered by a method that
                // does not exist and the client never re-pulls: findings are cached by the
                // server and never reach the sign column.
                workspace_diagnostics: false,
                work_done_progress_options: WorkDoneProgressOptions::default(),
            })),
            // Only when there is something to say: a hint on every declaration that says
            // "clean" is noise on the surface most likely to become noise.
            inlay_hint_provider: Some(OneOf::Right(InlayHintServerCapabilities::Options(
                InlayHintOptions {
                    resolve_provider: Some(false),
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                },
            ))),
            // Fully-formed lenses, no `resolve`: the title carries the finding count and the
            // command carries its own arguments, so a lens costs one request per document and
            // none per lens. `workspace/codeLens/refresh` after an analysis updates the titles.
            // Simple: the contents are complete on arrival, from what has already been
            // computed. A hover is a keystroke's gesture and must never wait for a model.
            hover_provider: Some(HoverProviderCapability::Simple(true)),
            code_lens_provider: Some(CodeLensOptions {
                resolve_provider: Some(false),
            }),
            execute_command_provider: Some(ExecuteCommandOptions {
                commands: COMMANDS.iter().map(|c| c.to_string()).collect(),
                work_done_progress_options: WorkDoneProgressOptions {
                    work_done_progress: Some(true),
                },
            }),
            ..ServerCapabilities::default()
        }
    }

    // ---- analysis -----------------------------------------------------------

    /// Run a pass off the request path, then ask the client to re-pull.
    ///
    /// Guarantees, each of which a test asserts:
    ///  * one run per document at a time; a request during a run is queued, not dropped;
    ///  * the client is always told to re-pull when a run ends, whatever the outcome —
    ///    including when the run is superseded by a newer edit, because a client that pulled
    ///    during the window would otherwise stay empty forever;
    ///  * a superseded result is never reported.
    ///
    /// Every pass shares this machinery: the rules pass differs in what it runs and which slot it
    /// holds, not in how it is supervised.
    /// The ambient pass for a document, whichever pass that is: the rules pass when rules are
    /// enabled, the chat review when they are not. One place decides, so no trigger can
    /// accidentally run the wrong one.
    fn spawn_ambient(&self, uri: String, debounce: Option<std::time::Duration>) {
        self.spawn_pass(uri, debounce, Pass::Ambient);
    }

    /// The chat review, because someone asked for it.
    fn spawn_review(&self, uri: String) {
        self.spawn_pass(uri, None, Pass::ReviewNow);
    }

    /// A rules pass over the open documents that changed, once the typing has stopped.
    ///
    /// Triggered rather than run for one document: an idle pass is the "you stopped, look
    /// around" moment, and how many documents it may cover is bounded by
    /// `rules.max_files_per_pass`. The remainder are named in the log, never dropped silently.
    ///
    /// It holds the rules slot under a key no document can have, so ten changes during the
    /// debounce window coalesce into one pass instead of ten.
    fn spawn_idle_ambient(&self, debounce: Option<std::time::Duration>) {
        if self.state.claim_rules(IDLE_PASS) == Claim::Queued {
            return;
        }
        let state = self.state.clone();
        let client = self.client.clone();
        tokio::spawn(async move {
            state.await_config(CONFIG_GRACE).await;
            let cfg = state.config();
            // Which pass this is, now that the settings are known. The two are exclusive: the
            // rules pass owns the ambient slot while rules are enabled.
            let pass = if cfg.rules.enabled {
                if !cfg.triggers.rules.on_idle {
                    let _ = state.finish_rules(IDLE_PASS);
                    return;
                }
                Pass::Rules
            } else if cfg.triggers.diagnostics == "idle" {
                Pass::Review
            } else {
                let _ = state.finish_rules(IDLE_PASS);
                return;
            };
            let mut wait = debounce;
            loop {
                if let Some(d) = wait.take() {
                    tokio::time::sleep(d).await;
                }
                let cap = state.config().rules.max_files_per_pass;
                let docs = state.all_docs();
                let over_cap: Vec<String> = docs.iter().skip(cap).map(|d| d.path.clone()).collect();
                for doc in docs.into_iter().take(cap) {
                    let generation = state.generation(&doc.uri);
                    let uri = doc.uri.clone();
                    let (state_for_work, target) = (state.clone(), doc.clone());
                    let outcome = tokio::task::spawn_blocking(move || {
                        let engine = Engine::new(state_for_work.clone());
                        match pass {
                            Pass::Rules => engine.inspect(&target, false).map(Passed::from_rules),
                            _ => engine.analyze(&target).map(|o| Passed::from_review(o, pass)),
                        }
                    })
                    .await;
                    if state.generation(&uri) != generation {
                        // Superseded by a newer edit: the content moved, so anything concluded
                        // here describes text nobody holds. Never published.
                        continue;
                    }
                    match outcome {
                        // `inspect` already wrote the cache slots; the record is written here
                        // because this is where the pass completes.
                        Ok(Ok(passed)) => {
                            if let Some(root) = state.root() {
                                let _ = crate::trace::append(&root, &passed.trace_entry(&uri));
                            }
                        }
                        Ok(Err(f)) => report(&client, &uri, Err(f), Pass::Rules).await,
                        Err(_) => continue,
                    }
                    refresh_surfaces(&state, &client).await;
                }
                if !over_cap.is_empty() {
                    client
                        .log_message(
                            MessageType::LOG,
                            format!(
                                "jev: rules pass covered {cap} document(s); {} left for the next pass ({})",
                                over_cap.len(),
                                over_cap.join(", ")
                            ),
                        )
                        .await;
                }
                if let Some(why) = state.take_changed_note() {
                    client
                        .log_message(
                            MessageType::LOG,
                            format!("jev: every open document counts as changed — {why}"),
                        )
                        .await;
                }
                if !state.finish_rules(IDLE_PASS) {
                    break;
                }
            }
        });
    }

    fn spawn_pass(&self, uri: String, debounce: Option<std::time::Duration>, pass: Pass) {
        let state = self.state.clone();
        let client = self.client.clone();
        tokio::spawn(async move {
            // A save that lands during startup must not be analysed against the built-in
            // defaults: wait for the client's settings, then proceed. Nothing is decided before
            // this — not the pass, not its slot, not the trigger that is in force.
            state.await_config(CONFIG_GRACE).await;
            let Some(pass) = resolve_ambient(&state, pass) else {
                return;
            };
            // The rules pass has its own slot: a save that starts a decision must not also queue
            // behind a slow review, and the two are triggered independently.
            let claim = |state: &AppState, uri: &str| match pass {
                Pass::Rules => state.claim_rules(uri),
                _ => state.claim_analysis(uri),
            };
            if claim(&state, &uri) == Claim::Queued {
                return;
            }
            let mut wait = debounce;
            loop {
                if let Some(d) = wait.take() {
                    tokio::time::sleep(d).await;
                }
                let generation = state.generation(&uri);
                let outcome = {
                    let state = state.clone();
                    let uri = uri.clone();
                    tokio::task::spawn_blocking(move || {
                        let doc = state.doc(&uri)?;
                        let engine = Engine::new(state.clone());
                        Some(match pass {
                            Pass::Rules => engine.inspect(&doc, false).map(Passed::from_rules),
                            Pass::ReviewNow => engine.review_now(&doc).map(|o| Passed::from_review(o, pass)),
                            // `Ambient` was resolved before the slot was claimed, so it cannot
                            // reach here; `Review` is what it resolves to when rules are off.
                            Pass::Review | Pass::Ambient => {
                                engine.analyze(&doc).map(|o| Passed::from_review(o, pass))
                            }
                        })
                    })
                    .await
                };

                let superseded = state.generation(&uri) != generation;
                if let Ok(Some(outcome)) = outcome {
                    if !superseded {
                        // `outcome` here is what `report` takes: a Result, because a run can
                        // fail as well as find nothing, and only a completed run is recorded.
                        if let (Ok(passed), Some(root)) = (&outcome, state.root()) {
                            let _ = crate::trace::append(&root, &passed.trace_entry(&uri));
                        }
                        report(&client, &uri, outcome, pass).await;
                    }
                }

                let again = match pass {
                    Pass::Rules => state.finish_rules(&uri),
                    _ => state.finish_analysis(&uri),
                };
                // Always signal, even when superseded or failed: the client re-pulls and
                // sees whatever the cache currently holds for the document.
                if state.doc(&uri).is_some() {
                    refresh_surfaces(&state, &client).await;
                }
                if !again {
                    break;
                }
            }
        });
    }

    /// The declarations to hang a lens or a hint on.
    ///
    /// The client's, when it sent them for this exact version — it has the parser and this side
    /// does not (`LANGUAGE.md` §4). Otherwise this side's structural scan, so a client with no
    /// parser is served exactly as before.
    fn scopes_of(&self, doc: &jev_core::Document, cfg: &jev_core::config::Config) -> Vec<Span> {
        if let Some(defs) = self.state.definitions(&doc.uri, doc.version) {
            return defs
                .into_iter()
                .map(|d| Span {
                    start_line: d.start_line,
                    end_line: d.end_line,
                })
                .collect();
        }
        let profile = jev_core::lang::profile(&doc.language.name);
        jev_core::scope::blocks(&doc.text, &profile, cfg.languages.max_scope_lines)
            .into_iter()
            .map(|b| Span {
                start_line: b.range.start_line,
                end_line: b.range.end_line,
            })
            .collect()
    }

    /// The checks every read-only surface shares: the server is on, and this is a document the
    /// analysis would accept. A surface that offers what the analysis would then refuse is
    /// worse than one that offers nothing.
    fn analysable(&self, doc: &jev_core::Document) -> Option<jev_core::config::Config> {
        let cfg = self.state.config();
        if !cfg.enabled {
            return None;
        }
        if jev_core::gates::evaluate(
            &doc.text,
            &doc.path,
            cfg.languages.max_file_bytes,
            &cfg.languages.ignore,
        )
        .is_some()
        {
            return None;
        }
        Some(cfg)
    }

    /// The findings cached for a document, and which pass produced them.
    ///
    /// Cached only: every read-only surface goes through here, and a surface that waited for a
    /// model would be a surface nobody uses.
    fn findings_for(&self, doc: &jev_core::Document) -> (Vec<Finding>, bool, String) {
        match self.engine.cached(doc) {
            Some(c) => {
                let source = c
                    .source
                    .clone()
                    .unwrap_or_else(|| "review".to_string());
                (c.findings.clone(), true, source)
            }
            None => (Vec::new(), false, "review".to_string()),
        }
    }

    // ---- code actions -------------------------------------------------------

    fn scope_of(&self, doc: &jev_core::Document, params: &CodeActionParams) -> jev_core::scope::Resolved {
        let explicit = if params.range.start.line != params.range.end.line {
            Some(LineRange {
                start_line: params.range.start.line,
                end_line: params.range.end.line,
            })
        } else {
            None
        };
        self.engine.scope_at(doc, params.range.start.line, explicit)
    }

    fn action_data(
        &self,
        doc: &jev_core::Document,
        verb: Verb,
        scope: &jev_core::scope::Resolved,
        state: ActionState,
        finding: Option<&Finding>,
    ) -> ActionData {
        let scope_ref = ScopeRef {
            kind: scope.kind,
            name: scope.name.clone(),
            start_line: scope.range.start_line,
            end_line: scope.range.end_line,
        };
        ActionData {
            v: ACTION_DATA_VERSION,
            id: ActionData::make_id(verb, &doc_ref(doc), &scope_ref, finding.map(|f| f.id.as_str())),
            verb,
            state,
            doc: doc_ref(doc),
            scope: scope_ref,
            scope_source: scope.source,
            language: doc.language.name.clone(),
            finding: finding.map(|f| f.id.clone()),
            summary: None,
        }
    }

    fn make_action(&self, title: String, data: ActionData, kind: &'static str, preferred: bool) -> CodeAction {
        CodeAction {
            title,
            kind: Some(CodeActionKind::new(kind)),
            diagnostics: None,
            edit: None,
            command: None,
            is_preferred: Some(preferred),
            disabled: None,
            data: Some(serde_json::to_value(&data).unwrap_or(Value::Null)),
        }
    }
}

fn doc_ref(doc: &jev_core::Document) -> jev_core::types::DocRef {
    jev_core::types::DocRef {
        uri: doc.uri.clone(),
        version: doc.version,
        content_hash: doc.hash.clone(),
    }
}

/// Commands actually served. A command that only answers "not implemented" is not
/// advertised (PROTOCOL.md §2), and a command the plugin offers is one it has to serve —
/// `jev.review` was missing here while `:Jev review` sent it, so that keymap answered
/// "not implemented" every time.
/// The rules slot an *idle* pass holds. No document can have this uri, so the batch pass
/// coalesces with itself and never with a per-document pass.
const IDLE_PASS: &str = "jev:idle-rules";

/// Commands actually served. A command that only answers "not implemented" is not
/// advertised (PROTOCOL.md §2), and a command the plugin offers is one it has to serve —
/// `jev.review` was missing here while `:Jev review` sent it, so that keymap answered
/// "not implemented" every time.
const COMMANDS: &[&str] = &[
    "jev.status",
    "jev.recompute",
    "jev.review",
    "jev.explain",
    "jev.ask",
    "jev.followup",
    "jev.document",
    "jev.session",
    "jev.usage",
    "jev.outcome",
    "jev.plan",
    "jev.apply",
    "jev.revert",
    "jev.cancel",
    "jev.inspect",
];

/// Deterministic title. Never model output (PROTOCOL.md §4).
fn action_title(verb: Verb, scope: &jev_core::scope::Resolved) -> String {
    let subject = scope
        .name
        .clone()
        .unwrap_or_else(|| scope.kind.as_str().to_string());
    format!("{}: {}", verb.label(), subject)
}

fn to_diagnostic(f: &Finding, content_hash: &str, source: &str) -> Diagnostic {
    let end = f.end_col.max(f.start_col + 1);
    Diagnostic {
        range: Range {
            start: Position {
                line: f.line,
                character: f.start_col,
            },
            end: Position {
                line: f.line,
                character: end,
            },
        },
        severity: Some(match f.severity {
            Severity::Warning => DiagnosticSeverity::WARNING,
            Severity::Information => DiagnosticSeverity::INFORMATION,
        }),
        code: Some(NumberOrString::String(f.id.clone())),
        code_description: None,
        // The namespace stays `jev` whatever produced the finding (PROTOCOL §9): one source,
        // one place a client turns the surface off. Which *pass* wrote it is `data.source`.
        source: Some("jev".to_string()),
        message: if f.detail.is_empty() {
            f.label.clone()
        } else {
            format!("{} — {}", f.label, f.detail)
        },
        related_information: None,
        tags: None,
        data: Some(json!({
            "finding_id": f.id,
            "verb": f.verb_hint.as_str(),
            "content_hash": content_hash,
            // Where it came from: a repository convention the decision confirmed, or the chat
            // review tier's opinion. A client that shows the two differently needs to know.
            "source": source,
        })),
    }
}

/// Resolve a relative new-file path against the workspace root, else the document's dir.
fn resolve_new_file_uri(path: &str, root: Option<&str>, doc_path: &str) -> String {
    let base = match root {
        Some(r) => r.trim_end_matches('/').to_string(),
        None => doc_path
            .rsplit_once('/')
            .map(|(dir, _)| dir.to_string())
            .unwrap_or_else(|| ".".to_string()),
    };
    format!("file://{base}/{path}")
}

/// Build the frozen edit shape: `documentChanges`, explicit versions, never a bare
/// `changes` map (PROTOCOL.md N4, §8).
fn build_workspace_edit(
    doc: &jev_core::Document,
    proposal: &Proposal,
    root: Option<&str>,
) -> WorkspaceEdit {
    let mut operations: Vec<DocumentChangeOperation> = Vec::new();

    for nf in &proposal.new_files {
        let uri_str = resolve_new_file_uri(&nf.path, root, &doc.path);
        let Ok(uri) = Url::parse(&uri_str) else {
            continue;
        };
        operations.push(DocumentChangeOperation::Op(ResourceOp::Create(CreateFile {
            uri: uri.clone(),
            options: Some(CreateFileOptions {
                overwrite: Some(false),
                ignore_if_exists: Some(false),
            }),
            annotation_id: None,
        })));
        let mut content = nf.content.clone();
        if !content.ends_with('\n') {
            content.push('\n');
        }
        operations.push(DocumentChangeOperation::Edit(TextDocumentEdit {
            text_document: OptionalVersionedTextDocumentIdentifier { uri, version: None },
            edits: vec![OneOf::Left(TextEdit {
                range: Range {
                    start: Position {
                        line: 0,
                        character: 0,
                    },
                    end: Position {
                        line: 0,
                        character: 0,
                    },
                },
                new_text: content,
            })],
        }));
    }

    if !proposal.ops.is_empty() {
        let edits = proposal
            .ops
            .iter()
            .map(|op| {
                OneOf::Left(TextEdit {
                    range: Range {
                        start: Position {
                            line: op.start_line,
                            character: 0,
                        },
                        end: Position {
                            line: op.end_line,
                            character: op.end_col,
                        },
                    },
                    new_text: op.new_text.clone(),
                })
            })
            .collect();
        operations.push(DocumentChangeOperation::Edit(TextDocumentEdit {
            text_document: OptionalVersionedTextDocumentIdentifier {
                uri: Url::parse(&doc.uri).unwrap_or_else(|_| Url::parse("file:///").unwrap()),
                version: Some(doc.version),
            },
            edits,
        }));
    }

    WorkspaceEdit {
        changes: None,
        document_changes: Some(DocumentChanges::Operations(operations)),
        change_annotations: None,
    }
}

/// A declaration's extent, whichever side found it: the client's parser or this side's scan.
struct Span {
    start_line: u32,
    end_line: u32,
}

/// What the client sent about the project, from an argument or from a code action's `data`.
///
/// `data` is round-tripped by the client, so the context for a *resolve* rides there: the
/// request that generates is the slow one by design (N3), and the fast paths stay fast because
/// nothing else carries context.
fn provided_context(source: Option<&Value>) -> Vec<jev_core::context::Provided> {
    source
        .and_then(|v| v.get("context"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// A value from the first argument, or from `scope` inside it.
///
/// Commands put the anchor at the top level (`jev.explain`) or under `scope` (`jev.plan`),
/// and the record wants it either way without teaching every caller about both shapes.
fn anchored(arguments: &[Value], field: &str) -> Option<Value> {
    let arg = arguments.first()?;
    arg.get(field)
        .or_else(|| arg.get("scope").and_then(|s| s.get(field)))
        .filter(|v| !v.is_null())
        .cloned()
}

/// An explicit scope carried in a command's argument, when the client resolved it itself.
///
/// `ExecuteCommandParams` has no range of its own, so a client that knows better than the
/// structural resolver — one with a parser, which is where nesting, strings and comments are
/// actually understood — says so. The result is an *explicit* scope, and the server reports it
/// as such (`scope_source`), so where an extent came from is visible rather than hidden.
fn explicit_scope(arg: &Value) -> Option<jev_core::types::LineRange> {
    let range = arg.get("range")?;
    let start_line = range.get("start_line").and_then(|v| v.as_u64())? as u32;
    let end_line = range.get("end_line").and_then(|v| v.as_u64())? as u32;
    if end_line < start_line {
        return None;
    }
    Some(jev_core::types::LineRange {
        start_line,
        end_line,
    })
}

/// The wire name of a severity. One mapping, so a diagnostic and a log entry can never
/// describe the same finding differently.
fn severity_str(s: Severity) -> &'static str {
    match s {
        Severity::Warning => "warning",
        Severity::Information => "information",
    }
}

/// One finding as a client receives it in a Result. The same fields the diagnostic carries,
/// so the two surfaces cannot describe the same finding differently.
fn finding_json(f: &Finding) -> Value {
    json!({
        "id": f.id,
        "line": f.line,
        "start_col": f.start_col,
        "end_col": f.end_col,
        "severity": severity_str(f.severity),
        "label": f.label,
        "detail": f.detail,
        "verb": f.verb_hint.as_str(),
    })
}

fn result_ok(payload: Value) -> Value {
    let mut base = json!({"schema": RESULT_SCHEMA, "ok": true});
    if let (Some(b), Value::Object(extra)) = (base.as_object_mut(), payload) {
        for (k, v) in extra {
            b.insert(k, v);
        }
    }
    base
}

fn result_err(code: &str, message: &str) -> Value {
    json!({
        "schema": RESULT_SCHEMA,
        "ok": false,
        "error": {"code": code, "message": message},
    })
}

/// Which pass an ambient trigger runs, once the settings are known.
///
/// `None` means "not this trigger": the rules pass is the ambient one while rules are enabled,
/// and the chat review covers the ambient slot when they are not — never both, and never the
/// chat review stepping in for a repository that simply has no rules.
fn resolve_ambient(state: &AppState, pass: Pass) -> Option<Pass> {
    let cfg = state.config();
    match pass {
        Pass::Ambient => {
            let (wanted, resolved) = if cfg.rules.enabled {
                (cfg.triggers.rules.on_save, Pass::Rules)
            } else {
                (cfg.triggers.diagnostics == "save", Pass::Review)
            };
            wanted.then_some(resolved)
        }
        concrete => Some(concrete),
    }
}

/// Ask the client to re-pull everything that carries a finding count.
///
/// The lenses and the hints both carry a count, so a new conclusion changes them — but only
/// what someone is displaying is refreshed. The client broadcasts this to every attached server,
/// and a hint answer carries no version, so refreshing hints nobody asked for is how an unrelated
/// server ends up returning positions that no longer exist.
async fn refresh_surfaces(state: &AppState, client: &Client) {
    if state.hints_are_wanted() && client.inlay_hint_refresh().await.is_err() {
        client
            .log_message(
                MessageType::LOG,
                "jev: client does not serve workspace/inlayHint/refresh",
            )
            .await;
    }
    if client.code_lens_refresh().await.is_err() {
        client
            .log_message(
                MessageType::LOG,
                "jev: client does not serve workspace/codeLens/refresh",
            )
            .await;
    }
    if client.workspace_diagnostic_refresh().await.is_err() {
        client
            .log_message(
                MessageType::LOG,
                "jev: client does not serve workspace/diagnostic/refresh",
            )
            .await;
    }
}

/// Log what a pass did. Never user-facing: findings reach the user through diagnostics, and a
/// skip or a failure is a log line rather than a popup, because the user did not ask for this
/// work (docs/UX.md §4).
async fn report(client: &Client, uri: &str, outcome: Result<Passed, Failure>, pass: Pass) {
    let what = match pass {
        Pass::Rules => "rules pass",
        _ => "analysis",
    };
    match outcome {
        Ok(passed) => {
            client
                .log_message(
                    MessageType::LOG,
                    format!(
                        "jev: {what} cached {} finding(s) for {}{}",
                        passed.findings.len(),
                        uri,
                        if passed.from_cache { " (cache hit)" } else { "" }
                    ),
                )
                .await;
            if passed.discarded > 0 {
                client
                    .log_message(
                        MessageType::LOG,
                        format!(
                            "jev: {} finding(s) discarded because their anchors could not be located",
                            passed.discarded
                        ),
                    )
                    .await;
            }
            // Every rule file that could not be read, and every document the pass declined to
            // look at. Silence here would make "no findings" and "nothing was inspected" the
            // same observation — which, in a repository that has written no rules, is the
            // difference between "nothing to do" and "a bug".
            for (code, detail) in &passed.skipped {
                let line = if code == "no_rules" {
                    // The sentence is already the whole message; a code in front of it would
                    // only be noise.
                    format!("jev: {detail}")
                } else {
                    format!("jev: skipped {code} — {detail}")
                };
                client.log_message(MessageType::LOG, line).await;
            }
        }
        Err(Failure::Skipped(reason)) => {
            client
                .log_message(MessageType::LOG, format!("jev: skipping {what} — {reason}"))
                .await;
        }
        Err(f) => {
            client
                .log_message(MessageType::LOG, format!("jev: {what} failed — {}", f.message()))
                .await;
        }
    }
}

/// `$/progress` with a value we shape ourselves.
///
/// `ProgressParamsValue` in the pinned `lsp-types` has exactly one variant, and
/// `WorkDoneProgressReport` has no room for the partial text that has to travel beside
/// `message` (§3.5). Same method, same token, same rules — the value is ours.
enum RawProgress {}

impl tower_lsp::lsp_types::notification::Notification for RawProgress {
    type Params = Value;
    const METHOD: &'static str = "$/progress";
}

/// A partial result, under a token the client issued.
///
/// `message` stays short and human, because a client's own progress UI renders it; the text so
/// far travels in `data`, which only the plugin reads. The artifact in the response remains
/// the authoritative one — this is a preview, and a repair attempt means it may be a preview
/// of an answer that was rejected.
async fn partial_artifact(client: &Client, token: &ProgressToken, markdown: &str) {
    let value = json!({
        "kind": "report",
        "message": format!("jev: explaining — {} bytes", markdown.len()),
        "data": {
            "schema": ARTIFACT_SCHEMA,
            "kind": "explanation",
            "partial": true,
            "markdown": markdown,
        },
    });
    client
        .send_notification::<RawProgress>(json!({"token": token, "value": value}))
        .await;
}

/// How often a streamed answer is forwarded. One notification per token would be hundreds of
/// messages for one explanation; this is fast enough to look live and slow enough not to
/// matter.
/// How long model work waits for the client's settings before proceeding on the defaults.
///
/// The round trip is local and takes milliseconds; the limit exists for a client that never
/// answers, which must not stall the server forever.
const CONFIG_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

const STREAM_FLUSH: std::time::Duration = std::time::Duration::from_millis(120);

/// How often to say something while the model is still thinking: slow enough not to be noise,
/// fast enough that a four-second prefill does not look like a hang.
const STREAM_HEARTBEAT: std::time::Duration = std::time::Duration::from_millis(1000);

/// Nothing to show yet, but the work is real and it is running.
async fn nothing_yet(client: &Client, token: &ProgressToken, waited: std::time::Duration) {
    let value = json!({
        "kind": "report",
        "message": format!(
            "jev: explaining — waiting for the model ({:.0}s)",
            waited.as_secs_f64()
        ),
        "data": { "partial": true, "markdown": "", "waiting": true },
    });
    client
        .send_notification::<RawProgress>(json!({"token": token, "value": value}))
        .await;
}

/// Sends `end` for a progress token unless it was already sent.
///
/// PROTOCOL.md §3.5 requires *exactly one* `end` on every path, "including model error, budget
/// refusal, cancellation, and panic". The first three are code and are written out where they
/// happen; a panic is not code — it is unwinding — so the clause had nothing behind it once the
/// `bridge.rs` drop guard it was written for was deleted, and a panic between `begin` and `end`
/// would have left the token open. This is that guard, in the one place that needs it.
///
/// A destructor cannot await, and this may run while a panic unwinds, so the send is handed to
/// the runtime instead of performed here.
///
/// Two paths reach it: a panic unwinding through the body, and a `$/cancelRequest` that aborts
/// the task the body runs in — which is why the task is spawned through `AbortOnDrop` and not
/// merely `tokio::spawn`ed.
struct EndOnDrop<F: FnOnce(ProgressToken)> {
    token: Option<ProgressToken>,
    send: Option<F>,
}

impl<F: FnOnce(ProgressToken)> EndOnDrop<F> {
    fn new(token: ProgressToken, send: F) -> EndOnDrop<F> {
        EndOnDrop {
            token: Some(token),
            send: Some(send),
        }
    }

    /// The explicit `end` has been sent; the guard has nothing left to do.
    fn done(&mut self) {
        self.token = None;
        self.send = None;
    }
}

impl<F: FnOnce(ProgressToken)> Drop for EndOnDrop<F> {
    fn drop(&mut self) {
        if let (Some(token), Some(send)) = (self.token.take(), self.send.take()) {
            send(token);
        }
    }
}

/// Aborts a spawned command when the request it belongs to goes away.
///
/// `$/cancelRequest` never reaches this crate: tower-lsp 0.20 aborts the task that is running
/// the handler (`Pending::cancel` → `JoinHandle::abort`). Aborting *that* future would only drop
/// the `JoinHandle` a plain `tokio::spawn` returned, and dropping a `JoinHandle` detaches — so
/// the command would run on after the client cancelled it, keep streaming `report`s under a
/// token the response has already closed, keep spending budget, and close the token only when
/// work nobody is waiting for finally finished. Measured before this guard existed: `begin`,
/// `-32800` at 53 ms, eight `report`s, and `end` 4.7 s later.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn progress(client: &Client, token: &ProgressToken, value: WorkDoneProgress) {
    client
        .send_notification::<Progress>(ProgressParams {
            token: token.clone(),
            value: ProgressParamsValue::WorkDone(value),
        })
        .await;
}

#[async_trait]
impl LanguageServer for JevServer {
    async fn initialize(&self, params: InitializeParams) -> RpcResult<InitializeResult> {
        let root = params
            .workspace_folders
            .as_ref()
            .and_then(|f| f.first())
            .map(|f| f.uri.to_string())
            .or_else(|| params.root_uri.as_ref().map(|u| u.to_string()));
        if let Some(r) = root {
            if let Ok(url) = Url::parse(&r) {
                self.state.set_root(Some(url.path().trim_end_matches('/').to_string()));
            }
        }
        Ok(InitializeResult {
            capabilities: Self::capabilities(),
            server_info: Some(ServerInfo {
                name: "jev-lsp".to_string(),
                version: Some(jev_core::VERSION.to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.pull_configuration().await;
    }

    /// The client says its settings changed: ask for them again (PROTOCOL.md §10).
    ///
    /// Without this the server keeps whatever it read at startup, so the plugin's kill switch
    /// (`:Jev stop`, which flips `enabled` and notifies) would have no effect at all.
    async fn did_change_configuration(&self, _: DidChangeConfigurationParams) {
        self.pull_configuration().await;
    }

    async fn shutdown(&self) -> RpcResult<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let d = params.text_document;
        let doc = jev_core::Document::new(
            d.uri.as_str(),
            d.version,
            d.text,
            Some(d.language_id.as_str()),
        );
        self.state.put_doc(doc);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        let version = params.text_document.version;
        self.state.bump_generation(&uri);
        // The document is about to differ from what git last saw; a cached answer would say
        // otherwise.
        self.state.forget_changed();

        let changes: Vec<(u32, u32, u32, u32, String)> = params
            .content_changes
            .into_iter()
            .map(|c| match c.range {
                Some(r) => (
                    r.start.line,
                    r.start.character,
                    r.end.line,
                    r.end.character,
                    c.text,
                ),
                None => (0, 0, u32::MAX, 0, c.text),
            })
            .collect();

        let cfg = self.state.config();
        let mode = cfg.triggers.diagnostics.clone();
        let idle = cfg.triggers.idle_ms;
        let rules = cfg.rules.enabled;
        let rules_idle = cfg.triggers.rules.clone();

        let mut doc = match self.state.doc(&uri) {
            Some(d) => d,
            None => return,
        };
        // A change with no range replaces the whole document.
        if changes.len() == 1 && changes[0].2 == u32::MAX {
            doc.replace(version, changes.into_iter().next().map(|c| c.4).unwrap_or_default());
        } else {
            doc.apply_incremental(version, &changes);
        }
        self.state.put_doc(doc);
        self.verify_prediction(&uri).await;

        // The ambient pass is the rules pass whenever rules are enabled; the chat review runs
        // when they are off, or when someone asks for it explicitly. That demotion is what makes
        // the ambient path cheap enough to run on every save.
        //
        // The test below is deliberately loose — either trigger may want this change — because
        // which one is in force is a setting, and a setting may not have arrived yet. The pass
        // resolves that once it has them.
        let _ = (rules, rules_idle.clone());
        if rules_idle.on_idle || mode == "idle" {
            let debounce = std::time::Duration::from_millis(rules_idle.idle_ms.min(idle.max(1)));
            self.spawn_idle_ambient(Some(debounce));
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        self.state.bump_generation(&uri);
        // This save is what the pass is for: ask git again rather than reuse an answer from a
        // moment ago, when the file may still have been clean.
        self.state.forget_changed();
        self.verify_prediction(&uri).await;
        // Which pass, and whether it is wanted at all, is decided inside: both answers come
        // from settings that may not have arrived yet.
        self.spawn_ambient(uri, None);
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.state.remove_doc(uri.as_str());
        // Clear what we published, so a reopened file does not show stale signs.
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn code_action(&self, params: CodeActionParams) -> RpcResult<Option<CodeActionResponse>> {
        let uri = params.text_document.uri.to_string();
        let Some(doc) = self.state.doc(&uri) else {
            return Ok(None);
        };
        let cfg = self.state.config();
        if !cfg.enabled {
            return Ok(None);
        }

        let scope = self.scope_of(&doc, &params);
        let (findings, cached, _) = self.findings_for(&doc);
        let cursor_line = params.range.start.line;
        let invoked = params.context.trigger_kind != Some(CodeActionTriggerKind::AUTOMATIC);
        let mut actions: Vec<CodeActionOrCommand> = Vec::new();

        // One action per finding, so the user fixes what they highlighted. The count is already
        // capped where the finding set is built, so every surface shows the same ones.
        let mut relevant: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.line >= scope.range.start_line && f.line <= scope.range.end_line)
            .collect();
        if relevant.is_empty() {
            relevant = findings
                .iter()
                .filter(|f| f.line == cursor_line)
                .collect();
        }

        for f in &relevant {
            let fscope = self.engine.scope_at(&doc, f.line, None);
            let data = self.action_data(&doc, Verb::Fix, &fscope, ActionState::Ready, Some(f));
            actions.push(CodeActionOrCommand::CodeAction(self.make_action(
                format!("Fix: {}", f.label),
                data,
                Verb::Fix.kind(),
                f.line == cursor_line,
            )));
        }

        if !findings.is_empty() {
            let last = doc.line_count().saturating_sub(1);
            let file_scope = jev_core::scope::Resolved {
                range: LineRange {
                    start_line: 0,
                    end_line: last,
                },
                kind: ScopeKind::File,
                source: ScopeSource::WholeFile,
                name: None,
                truncated: false,
            };
            let data = self.action_data(&doc, Verb::FixAll, &file_scope, ActionState::Ready, None);
            actions.push(CodeActionOrCommand::CodeAction(self.make_action(
                format!("{} ({})", Verb::FixAll.label(), findings.len()),
                data,
                Verb::FixAll.kind(),
                false,
            )));
        }

        // The always-available verb menu. `fix` and `fixAll` are finding-driven, and
        // `explain` is a command rather than an action, so all three are excluded here.
        for verb in cfg.verbs_for(&doc.language.name) {
            if matches!(verb, Verb::Fix | Verb::FixAll | Verb::Explain) {
                continue;
            }
            let data = self.action_data(&doc, verb, &scope, ActionState::Ready, None);
            actions.push(CodeActionOrCommand::CodeAction(self.make_action(
                action_title(verb, &scope),
                data,
                verb.kind(),
                false,
            )));
        }

        // Cold cache: offer the reason in the menu and warm it in the background.
        if !cached && invoked {
            let mut data = self.action_data(&doc, Verb::Review, &scope, ActionState::Pending, None);
            data.summary = Some(format!("jev {} is available", jev_core::VERSION));
            let mut action = self.make_action(
                "jev: no findings cached for this file yet".to_string(),
                data,
                Verb::Review.kind(),
                false,
            );
            action.disabled = Some(CodeActionDisabled {
                reason: "analysing in the background; reopen the menu in a moment".to_string(),
            });
            actions.push(CodeActionOrCommand::CodeAction(action));
            self.spawn_ambient(uri, None);
        }

        Ok(Some(actions))
    }

    async fn code_action_resolve(&self, action: CodeAction) -> RpcResult<CodeAction> {
        let Some(raw) = action.data.clone() else {
            return Ok(action);
        };
        // Read before the raw value is consumed: the client round-trips `data`, so the
        // context it attached for this resolve is in there.
        let provided = provided_context(Some(&raw));
        let Ok(mut data) = serde_json::from_value::<ActionData>(raw) else {
            return Ok(action);
        };
        let mut action = action;

        let Some(doc) = self.state.doc(&data.doc.uri) else {
            return Ok(action);
        };

        // Staleness first: never return an edit computed against other content (§8 rule 3).
        if doc.hash != data.doc.content_hash {
            data.state = ActionState::Stale;
            action.data = Some(serde_json::to_value(&data).unwrap_or(Value::Null));
            action.disabled = Some(CodeActionDisabled {
                reason: "the file changed; reopen the menu to recompute".to_string(),
            });
            return Ok(action);
        }

        let scope = jev_core::scope::Resolved {
            range: LineRange {
                start_line: data.scope.start_line,
                end_line: data.scope.end_line,
            },
            kind: data.scope.kind,
            source: data.scope_source,
            name: data.scope.name.clone(),
            truncated: false,
        };
        let (findings, _, _) = self.findings_for(&doc);

        // `review` asks the chat review tier for its opinion now. It is the one place the
        // ambient rules pass is *not* what a user gets: the action names the review verb, so it
        // has to be the review.
        if data.verb == Verb::Review {
            let uri = data.doc.uri.clone();
            self.state.bump_generation(&uri);
            self.spawn_review(uri);
            return Ok(action);
        }

        let in_scope: Vec<Finding> = match &data.finding {
            Some(id) => findings.iter().filter(|f| &f.id == id).cloned().collect(),
            None => findings
                .iter()
                .filter(|f| f.line >= scope.range.start_line && f.line <= scope.range.end_line)
                .cloned()
                .collect(),
        };

        let verb = data.verb;
        // The document is still needed afterwards, to stamp the edit.
        let target = doc.clone();
        let outcome = self
            .blocking(move |engine| {
                engine.generate_with_context(&target, verb, &scope, &in_scope, &provided, None)
            })
            .await;
        match outcome {
            Ok(Generated::Edit(proposal)) => {
                let root = self.state.root();
                action.edit = Some(build_workspace_edit(&doc, &proposal, root.as_deref()));
                data.state = ActionState::Ready;
                data.summary = Some(proposal.summary.clone());
                action.data = Some(serde_json::to_value(&data).unwrap_or(Value::Null));
                Ok(action)
            }
            Ok(Generated::Artifact(markdown)) => {
                data.state = ActionState::Ready;
                action.data = Some(serde_json::to_value(&data).unwrap_or(Value::Null));
                self.client
                    .show_document(ShowDocumentParams {
                        uri: Url::parse(&format!("jev://artifact/{}", data.id))
                            .unwrap_or_else(|_| Url::parse("jev://artifact").unwrap()),
                        external: Some(false),
                        take_focus: Some(false),
                        selection: None,
                    })
                    .await
                    .ok();
                let _ = markdown;
                Ok(action)
            }
            Err(failure) => {
                data.state = match failure {
                    Failure::Refused(_) => ActionState::OverBudget,
                    _ => ActionState::Failed,
                };
                action.data = Some(serde_json::to_value(&data).unwrap_or(Value::Null));
                action.disabled = Some(CodeActionDisabled {
                    reason: failure.message(),
                });
                let level = match failure {
                    Failure::Refused(_) | Failure::Skipped(_) => MessageType::INFO,
                    _ => MessageType::WARNING,
                };
                self.client.show_message(level, failure.message()).await;
                Ok(action)
            }
        }
    }

    async fn diagnostic(
        &self,
        params: DocumentDiagnosticParams,
    ) -> RpcResult<DocumentDiagnosticReportResult> {
        let uri = params.text_document.uri.to_string();
        let empty = DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(
            RelatedFullDocumentDiagnosticReport {
                related_documents: None,
                full_document_diagnostic_report: FullDocumentDiagnosticReport {
                    result_id: None,
                    items: Vec::new(),
                },
            },
        ));
        let Some(doc) = self.state.doc(&uri) else {
            return Ok(empty);
        };
        let (findings, _, source) = self.findings_for(&doc);
        let items = findings
            .iter()
            .map(|f| to_diagnostic(f, &doc.hash, &source))
            .collect();
        Ok(DocumentDiagnosticReportResult::Report(
            DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
                related_documents: None,
                full_document_diagnostic_report: FullDocumentDiagnosticReport {
                    result_id: Some(format!("jev-{}", doc.hash)),
                    items,
                },
            }),
        ))
    }

    /// A badge at the head of a declaration that has findings, and nothing anywhere else.
    ///
    /// Inlay hints sit inside the text, so this is the quietest of the surfaces and the one
    /// that has to earn its place: only a declaration with cached findings gets a hint, and
    /// the label is the count. Silence is the default, not a state to be reported.
    async fn inlay_hint(&self, params: InlayHintParams) -> RpcResult<Option<Vec<InlayHint>>> {
        self.state.mark_hints_asked();
        let uri = params.text_document.uri.to_string();
        let Some(doc) = self.state.doc(&uri) else {
            return Ok(None);
        };
        let Some(cfg) = self.analysable(&doc) else {
            return Ok(None);
        };
        let (findings, _, _) = self.findings_for(&doc);
        if findings.is_empty() {
            return Ok(None);
        }

        let first = params.range.start.line;
        let last = params.range.end.line;
        let lines: Vec<&str> = doc.text.lines().collect();
        let hints = self
            .scopes_of(&doc, &cfg)
            .into_iter()
            .filter(|block| block.start_line >= first && block.start_line <= last)
            .filter_map(|block| {
                let here: Vec<&Finding> = findings
                    .iter()
                    .filter(|f| f.line >= block.start_line && f.line <= block.end_line)
                    .collect();
                if here.is_empty() {
                    return None;
                }
                // Byte offset, because this server speaks utf-8 (N1).
                let head = lines.get(block.start_line as usize)?;
                Some(InlayHint {
                    position: Position {
                        line: block.start_line,
                        character: head.len() as u32,
                    },
                    label: InlayHintLabel::String(format!(
                        "jev: {} finding{}",
                        here.len(),
                        if here.len() == 1 { "" } else { "s" }
                    )),
                    kind: None,
                    text_edits: None,
                    tooltip: Some(InlayHintTooltip::String(
                        here.iter().map(|f| f.label.clone()).collect::<Vec<_>>().join("; "),
                    )),
                    padding_left: Some(true),
                    padding_right: None,
                    data: None,
                })
            })
            .collect::<Vec<_>>();
        Ok(if hints.is_empty() { None } else { Some(hints) })
    }

    /// What is already known about the scope under the cursor.
    ///
    /// Reads the artifact store — explanations and answers the user has already asked for —
    /// and never calls a model. Empty when there is nothing to repeat, which the client
    /// renders as no hover rather than as a failed one.
    async fn hover(&self, params: HoverParams) -> RpcResult<Option<Hover>> {
        let pos = params.text_document_position_params;
        let uri = pos.text_document.uri.to_string();
        let line = pos.position.line;
        let Some(doc) = self.state.doc(&uri) else {
            return Ok(None);
        };
        if self.analysable(&doc).is_none() {
            return Ok(None);
        }
        // Any artifact that *covers* this line, not only one whose extent matches exactly: the
        // explanation was about the scope the user asked from, and hovering anywhere inside it
        // is the same question. The content hash still has to match — a stale explanation shown
        // against lines it was not written about is worse than none.
        let Some((start, end, markdown)) = self.state.artifact_covering(&uri, line, &doc.hash)
        else {
            return Ok(None);
        };
        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: markdown,
            }),
            range: Some(Range {
                start: Position { line: start, character: 0 },
                end: Position { line: end, character: 0 },
            }),
        }))
    }

    /// One lens per declaration at the left margin (docs/UX.md §1, "inline annotation").
    ///
    /// The lens is the affordance that does not have to be remembered: the work available on a
    /// declaration is visible on it. Nothing here may be slow — the client asks for every
    /// visible document — so it reads the cache and never calls a model. A scope with findings
    /// offers the fix; a clean one offers the explanation.
    async fn code_lens(&self, params: CodeLensParams) -> RpcResult<Option<Vec<CodeLens>>> {
        let uri = params.text_document.uri.to_string();
        let Some(doc) = self.state.doc(&uri) else {
            return Ok(None);
        };
        let Some(cfg) = self.analysable(&doc) else {
            return Ok(None);
        };

        let (findings, _, _) = self.findings_for(&doc);
        let lenses = self
            .scopes_of(&doc, &cfg)
            .into_iter()
            .map(|block| {
                let line = block.start_line;
                let in_scope = findings
                    .iter()
                    .filter(|f| f.line >= block.start_line && f.line <= block.end_line)
                    .count();
                let (title, command) = if in_scope > 0 {
                    (
                        format!(
                            "jev: {} finding{} · fix",
                            in_scope,
                            if in_scope == 1 { "" } else { "s" }
                        ),
                        "jev.plugin.pick",
                    )
                } else {
                    ("jev: explain".to_string(), "jev.plugin.explain")
                };
                CodeLens {
                    range: Range {
                        start: Position { line, character: 0 },
                        end: Position { line, character: 0 },
                    },
                    command: Some(Command {
                        title,
                        // The plugin owns these, because opening a buffer is a client
                        // decision (PROTOCOL §6, §7): `window/showDocument` is for artifacts
                        // the server has a URI for, and an explanation has none.
                        command: command.to_string(),
                        arguments: Some(vec![json!({ "uri": uri, "line": line })]),
                    }),
                    data: None,
                }
            })
            .collect::<Vec<_>>();
        Ok(Some(lenses))
    }

    /// Run a command in its own task, so a panic in it is a `JoinError` here rather than an
    /// unwind through the transport loop.
    ///
    /// Measured, not assumed: tower-lsp 0.20.0 contains no `catch_unwind` anywhere and awaits
    /// the service future in the transport task (`src/service.rs:333`) rather than spawning one
    /// per request, so a panic in a handler propagates out of `serve` and takes the process with
    /// it — leaving the client with no response *and* no `end`, and no destructor able to send
    /// one because the runtime dies with it. Containing the body is therefore what makes
    /// PROTOCOL.md §3.5's "exactly one `end` … including panic" true: the token is closed by the
    /// guard inside the body, the request is answered, and the server keeps serving.
    ///
    /// The task is held through `AbortOnDrop`, so the other way this future can end — the client
    /// cancelling the request, which drops it — stops the command instead of detaching it.
    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> RpcResult<Option<serde_json::Value>> {
        let body = JevServer::new(self.client.clone(), self.state.clone());
        let mut task = AbortOnDrop(tokio::spawn(async move { body.command_body(params).await }));
        match (&mut task.0).await {
            Ok(value) => Ok(Some(value)),
            // A panic: the guard inside the body has already closed the token, so this only has
            // to say what happened and keep the server alive.
            Err(join) if join.is_panic() => {
                let why = join
                    .try_into_panic()
                    .ok()
                    .and_then(|p| p.downcast_ref::<&str>().map(|s| s.to_string())
                        .or_else(|| p.downcast_ref::<String>().cloned()))
                    .unwrap_or_else(|| "a command panicked".to_string());
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!("jev: a command panicked and was contained: {why}"),
                    )
                    .await;
                Ok(Some(result_err(
                    "panic",
                    &format!("the command panicked and was contained: {why}"),
                )))
            }
            // Not a panic, so the task was aborted before it could answer: there is no work left
            // to report, and nothing is waiting for it. A live cancellation does not arrive here
            // — it drops this future, and `AbortOnDrop` aborts the task from there.
            Err(_) => Ok(Some(result_err("cancelled", "the command was cancelled"))),
        }
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::Arc;

    /// A sink standing in for the client, so the guard can be exercised without a socket.
    fn sink() -> (Arc<Mutex<Vec<String>>>, Box<dyn FnOnce(ProgressToken) + Send>) {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let written = seen.clone();
        (
            seen,
            Box::new(move |token: ProgressToken| {
                written.lock().push(format!("{token:?}"));
            }),
        )
    }

    fn token() -> ProgressToken {
        ProgressToken::String("jev:test".to_string())
    }

    #[test]
    fn a_token_closed_normally_is_not_closed_twice() {
        // The ordinary path: `begin` … `end` … `done()`. One end, from the code, not the guard.
        let (seen, send) = sink();
        let mut guard = EndOnDrop::new(token(), send);
        guard.done();
        drop(guard);
        assert!(seen.lock().is_empty(), "the explicit end is the only one");
    }

    #[test]
    fn a_panic_between_begin_and_end_still_sends_its_end() {
        // The clause names panic explicitly, so the guard is exercised through a real unwind
        // rather than only by dropping it on a path that cannot panic.
        let (seen, send) = sink();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = EndOnDrop::new(token(), send);
            panic!("as a command handler might, between begin and end");
        }));
        assert!(result.is_err(), "the probe panicked, which is the path under test");
        assert_eq!(
            seen.lock().len(),
            1,
            "exactly one end arrives for a token left open by unwinding"
        );
    }

    #[tokio::test]
    async fn a_dropped_request_aborts_the_command_and_closes_its_token() {
        // Cancellation, as the transport delivers it: `$/cancelRequest` drops this request's
        // future, and the task running the command goes with it. Without `AbortOnDrop` the task
        // would keep going, the guard would not run until the work nobody wants finished, and
        // the token would stay open for as long as that took.
        let (seen, send) = sink();
        let task = AbortOnDrop(tokio::spawn(async move {
            let _guard = EndOnDrop::new(token(), send);
            std::future::pending::<()>().await;
        }));
        // Let the command start, so the guard exists before the request goes away — a cancel
        // arrives while work is in flight, never before it begins.
        tokio::task::yield_now().await;
        drop(task);
        for _ in 0..100 {
            if !seen.lock().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            seen.lock().len(),
            1,
            "the aborted command's token was closed exactly once"
        );
    }
}

/// The first line where two texts differ, with both versions of it.
fn first_difference(expected: &str, actual: &str) -> (u32, String, String) {
    let a: Vec<&str> = expected.split('\n').collect();
    let b: Vec<&str> = actual.split('\n').collect();
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or("<end of file>");
        let y = b.get(i).copied().unwrap_or("<end of file>");
        if x != y {
            return (i as u32, x.to_string(), y.to_string());
        }
    }
    (0, String::new(), String::new())
}

impl JevServer {
    /// Check a server-applied edit against its prediction (PROTOCOL.md §8).
    ///
    /// This is the one legitimate use of `publishDiagnostics`: the document changed *because
    /// the server changed it*, so pushing the outcome is not an unsolicited interruption.
    /// A divergence means the client did something other than what it acknowledged, and
    /// silently ignoring that would leave the user with code nobody planned.
    async fn verify_prediction(&self, uri: &str) {
        let Some(predicted) = self.state.take_prediction(uri) else {
            return;
        };
        let Some(doc) = self.state.doc(uri) else {
            return;
        };
        if doc.text == predicted {
            return;
        }
        let (line, expected, actual) = first_difference(&predicted, &doc.text);
        self.client
            .log_message(
                MessageType::ERROR,
                format!("jev: divergence after an applied edit in {uri} at line {}", line + 1),
            )
            .await;
        let Ok(url) = Url::parse(uri) else {
            return;
        };
        self.client
            .publish_diagnostics(
                url,
                vec![Diagnostic {
                    range: Range {
                        start: Position {
                            line,
                            character: 0,
                        },
                        end: Position {
                            line,
                            character: 1,
                        },
                    },
                    severity: Some(DiagnosticSeverity::ERROR),
                    code: Some(NumberOrString::String("jev.divergence".to_string())),
                    code_description: None,
                    source: Some("jev".to_string()),
                    message: format!(
                        "the applied edit did not produce what was expected: line {} was \
                         predicted to be {expected:?} but is {actual:?}",
                        line + 1
                    ),
                    related_information: None,
                    tags: None,
                    data: Some(json!({"divergence": true})),
                }],
                None,
            )
            .await;
    }

    /// Run an engine call off the async worker.
    ///
    /// The model client is synchronous (one blocking `ureq` implementation, no async in
    /// `jev-core`), so calling it from an `async fn` blocks a runtime thread for as long as
    /// the request takes — up to the tier timeout, which is how a language server ends up
    /// stalling every other handler behind one model call.
    ///
    /// Every model-calling handler goes through here.
    async fn blocking<T, F>(&self, work: F) -> Result<T, Failure>
    where
        F: FnOnce(Engine) -> Result<T, Failure> + Send + 'static,
        T: Send + 'static,
    {
        // Model work started before the client's settings arrive would run against the
        // built-in defaults: the wrong endpoint, the wrong limits, the wrong model. A request
        // that lands during startup waits here instead of answering from a configuration the
        // user never chose.
        self.state.await_config(CONFIG_GRACE).await;
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || work(Engine::new(state)))
            .await
            .map_err(|e| Failure::Model(format!("the worker task failed: {e}")))?
    }

    /// Run artifact work, reporting the answer as it arrives when the client gave us a token
    /// to report under (§3.5 path 1).
    ///
    /// The sender lives inside the work closure, so the channel closes when the work ends and
    /// the forwarder drains and returns — which is why this awaits it rather than detaching.
    async fn streaming<T, F>(&self, token: Option<ProgressToken>, work: F) -> Result<T, Failure>
    where
        F: FnOnce(Engine, &mut dyn FnMut(&str)) -> Result<T, Failure> + Send + 'static,
        T: Send + 'static,
    {
        // Same gate as `blocking`: a plan or a review started during startup must not be
        // produced by a configuration the client never sent.
        self.state.await_config(CONFIG_GRACE).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        // Armed at the spawn, not after the work: the request can go away while the model call
        // is still in flight, and a guard that is wrapped onto the handle only once the call has
        // returned has nothing to abort — that is how a cancelled request kept sending `report`s
        // under a token its own `end` had already closed.
        let mut forwarder = token.map(|t| {
            let client = self.client.clone();
            AbortOnDrop(tokio::spawn(async move {
                let mut so_far = String::new();
                let mut flushed = std::time::Instant::now();
                let started = std::time::Instant::now();
                let mut beat = std::time::Instant::now();
                // The model spends seconds before the first token — prefill, and reasoning that
                // never becomes part of the answer. Staying silent until content arrives leaves
                // that window empty, exactly where a user is deciding whether this is working.
                loop {
                    match tokio::time::timeout(STREAM_FLUSH, rx.recv()).await {
                        Ok(Some(chunk)) => {
                            so_far.push_str(&chunk);
                            if flushed.elapsed() < STREAM_FLUSH {
                                continue;
                            }
                            flushed = std::time::Instant::now();
                            partial_artifact(&client, &t, &so_far).await;
                            beat = std::time::Instant::now();
                        }
                        Ok(None) => break, // work done, and the sender is gone
                        Err(_) => {
                            if beat.elapsed() >= STREAM_HEARTBEAT {
                                beat = std::time::Instant::now();
                                if so_far.is_empty() {
                                    nothing_yet(&client, &t, started.elapsed()).await;
                                } else {
                                    partial_artifact(&client, &t, &so_far).await;
                                }
                            }
                        }
                    }
                }
            }))
        });
        let outcome = self
            .blocking(move |engine| {
                let mut on_delta = move |delta: &str| {
                    let _ = tx.send(delta.to_string());
                };
                work(engine, &mut on_delta)
            })
            .await;
        // The forwarder must not outlive this future: a cancelled request closes the token
        // through the command's guard, and a forwarder that survived that would go on sending
        // `report`s under a token that is already closed — measured before this guard: `end`
        // 46 ms after the cancel, then eight more `report`s over the next 4.5 s. The blocking
        // model call itself cannot be interrupted (`spawn_blocking`, and `jev-core`'s client is
        // synchronous), so it runs to the end of the tier timeout; its answer is discarded, and
        // it has nothing left to report to.
        if let Some(handle) = forwarder.as_mut() {
            let _ = (&mut handle.0).await;
        }
        outcome
    }

    /// Read the `jev` section from the client and merge it over the current settings.
    async fn pull_configuration(&self) {
        let items = vec![ConfigurationItem {
            scope_uri: None,
            section: Some("jev".to_string()),
        }];
        match self.client.configuration(items).await {
            Ok(values) => {
                if let Some(first) = values.into_iter().next() {
                    if !first.is_null() {
                        if let Some(why) = self.state.merge_config(Some(&first)) {
                            // Silence here means the client believes it configured the server
                            // and the server is on its defaults — a whole class of confusing
                            // behaviour, so it is reported rather than swallowed.
                            self.client
                                .log_message(
                                    MessageType::ERROR,
                                    format!(
                                        "jev: the `jev` settings could not be applied ({why}); running on the built-in defaults"
                                    ),
                                )
                                .await;
                        } else {
                            // Which endpoints are actually in force. A client that believes it
                            // configured something while the server runs on its defaults is
                            // otherwise invisible, and proving that took an afternoon.
                            let cfg = self.state.config();
                            self.client
                                .log_message(
                                    MessageType::LOG,
                                    format!(
                                        "jev: settings applied — reason {} · review {}",
                                        cfg.models.reason.base_url,
                                        cfg.models.review.base_url
                                    ),
                                )
                                .await;
                        }
                    }
                }
            }
            Err(_) => {
                self.client
                    .log_message(
                        MessageType::LOG,
                        "jev: workspace/configuration unavailable; using defaults",
                    )
                    .await;
            }
        }
        // Applied, refused, null, or impossible: the answer has arrived, and every model path
        // that waits on this gate may proceed instead of waiting out CONFIG_GRACE.
        self.state.mark_config_ready();
    }

    /// The command itself: `begin`, the work, `end`, and the record.
    async fn command_body(&self, params: ExecuteCommandParams) -> serde_json::Value {
        let token = params.work_done_progress_params.work_done_token.clone();
        let command = params.command.clone();
        let started = std::time::Instant::now();
        // Armed *before* `begin`, because everything after it can unwind and the contract counts
        // that path: a panic between the two explicit sends drops this and the token is closed
        // on the way out.
        let mut guard = token.as_ref().map(|t| {
            let client = self.client.clone();
            EndOnDrop::new(t.clone(), move |token: ProgressToken| {
                // No runtime means there is no client left to notify; the alternative is
                // panicking inside a destructor, which during an unwind aborts the process.
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        progress(
                            &client,
                            &token,
                            WorkDoneProgress::End(WorkDoneProgressEnd {
                                message: Some("done".to_string()),
                            }),
                        )
                        .await;
                    });
                }
            })
        });
        if let Some(t) = &token {
            progress(
                &self.client,
                t,
                WorkDoneProgress::Begin(WorkDoneProgressBegin {
                    title: format!("jev {command}"),
                    // `true`, unconditionally: every command body runs in a task the transport
                    // can abort — `begin` is only ever sent from inside it — so a client that
                    // offers a cancel affordance is not being lied to. `$/cancelRequest` is
                    // honoured: the request answers `-32800 Canceled`, the guard closes the
                    // token, and the answer is discarded.
                    //
                    // What a cancel does *not* interrupt is the model call itself: `jev-core`'s
                    // client is synchronous and runs in a blocking worker that cannot be
                    // cancelled, so that call finishes and its answer is dropped. That is a cost
                    // — a permit held until the tier timeout — not a caveat about what the client
                    // may ask for, which is what this flag states.
                    cancellable: Some(true),
                    message: None,
                    percentage: None,
                }),
            )
            .await;
        }

        let value = self.run_command(&params).await;

        // One line per command, written where every command passes so none can be missed. The
        // record is never read back to decide anything (N9); a read-only checkout that cannot
        // be written to is not a reason to fail a request over a convenience.
        //
        // Five commands are left out on purpose, for the same reason: they are not work.
        // `jev.document` is the client telling the server what it already knows, sent on every
        // change; `jev.status` and `jev.session` are polls — the statusline asks for
        // the first every few seconds and the session buffer for the second. Recording polls
        // fills the record with the act of reading it and buries the work it exists to show,
        // which is exactly what happened: a day of testing left ninety-five megabytes of
        // `jev.status` behind and nothing else in the last two hundred entries. `jev.usage`
        // reads the same record, so it is a poll by construction, and `jev.outcome` is not
        // work either — it *is* the entry it would otherwise duplicate.
        let recorded = !matches!(
            command.as_str(),
            "jev.document" | "jev.status" | "jev.session" | "jev.usage" | "jev.outcome"
        );
        if recorded {
        if let Some(root) = self.state.root() {
            let _ = crate::trace::append(
                &root,
                &json!({
                    "kind": "command",
                    "command": command,
                    // An artifact result carries no `ok` at all — it is the artifact — so
                    // "not an error" is the honest reading, and only a Result envelope's own
                    // `ok` overrides it.
                    "ok": value
                        .get("ok")
                        .and_then(|v| v.as_bool())
                        .unwrap_or_else(|| value.get("error").is_none()),
                    "error": value
                        .get("error")
                        .and_then(|e| e.get("code"))
                        .and_then(|v| v.as_str()),
                    "ms": started.elapsed().as_millis() as u64,
                    // Where the request was anchored, when it says. This is what makes the
                    // record something you can walk back through rather than only read: an
                    // entry that knows its file and line can be opened from the session
                    // buffer. `plan` nests its anchor under `scope`, so both are checked.
                    "uri": anchored(&params.arguments, "uri"),
                    "line": anchored(&params.arguments, "line"),
                }),
            );
        }
        }

        if let Some(t) = &token {
            progress(
                &self.client,
                t,
                WorkDoneProgress::End(WorkDoneProgressEnd {
                    message: Some("done".to_string()),
                }),
            )
            .await;
        }
        // The end has been sent by the code above; the guard must not send a second one.
        if let Some(guard) = guard.as_mut() {
            guard.done();
        }
        value
    }

    async fn run_command(&self, params: &ExecuteCommandParams) -> Value {
        match params.command.as_str() {
            "jev.status" => {
                let cfg = self.state.config();
                let (hits, misses, entries) = self.state.cache.stats();
                let snap = self.state.budget.snapshot();
                let (calls, refusals) = self.state.budget.counters();
                let rules = self.state.rules_stats();
                result_ok(json!({
                    "version": jev_core::VERSION,
                    "enabled": cfg.enabled,
                    "documents": self.state.doc_count(),
                    "analysis_in_flight": self.state.is_analyzing(),
                    "rules_in_flight": self.state.rules_running(),
                    "plans": self.state.plan_count(),
                    "cache": {"entries": entries, "hits": hits, "misses": misses},
                    "budget": {
                        "calls_last_minute": snap.calls_last_minute,
                        "calls_last_hour": snap.calls_last_hour,
                        "tokens_used": snap.tokens_used,
                        "in_flight": snap.in_flight,
                        "limit_per_minute": cfg.budget.max_calls_per_min,
                        "decisions_last_minute": snap.decisions_last_minute,
                        "limit_decisions_per_minute": cfg.budget.max_decisions_per_min,
                        "limit_per_hour": cfg.budget.max_calls_per_hour,
                        "limit_tokens": cfg.budget.max_tokens_per_session,
                    },
                    "counters": {"calls": calls, "refusals": refusals},
                    // What the last rules pass did. Zeros before the first one, which is
                    // information: a repository whose rules never run should be able to say so.
                    "rules": {
                        "enabled": cfg.rules.enabled,
                        "loaded": rules.loaded,
                        "hash": rules.hash,
                        "last_pass_ms": rules.last_pass_ms,
                        "candidates": rules.candidates,
                        "calls": rules.calls,
                        // Problems with the rules *document* itself, as of that pass, and the
                        // count of the `skipped` entries a pass reports for them. A repository
                        // whose rules do not compile sees a number here instead of a rule that
                        // quietly never fires.
                        "lint": rules.lint,
                    },
                    // Whether the client's settings have arrived yet. Until they have, the
                    // endpoints below are the built-in defaults and not what the user
                    // configured — one status call in six reported the default decide endpoint
                    // for exactly that reason. Status is a read, so it does not *wait* on the
                    // configuration gate the way model work does; it says which of the two
                    // answers it is giving.
                    "config_ready": self.state.config_is_ready(),
                    "models": {
                        "reason": {"base_url": cfg.models.reason.base_url, "model": cfg.models.reason.model},
                        "review": {"base_url": cfg.models.review.base_url, "model": cfg.models.review.model},
                        "decide": {"wire": cfg.models.decide.wire, "base_url": cfg.models.decide.base_url, "model": cfg.models.decide.model, "timeout_ms": cfg.models.decide.timeout_ms},
                    },
                    "triggers": {
                        "diagnostics": cfg.triggers.diagnostics,
                        "idle_ms": cfg.triggers.idle_ms,
                        "rules": {
                            "on_save": cfg.triggers.rules.on_save,
                            "on_idle": cfg.triggers.rules.on_idle,
                            "idle_ms": cfg.triggers.rules.idle_ms,
                        },
                    },
                }))
            }
            "jev.recompute" => {
                self.state.cache.clear();
                let uris: Vec<String> = self.state.all_docs().iter().map(|d| d.uri.clone()).collect();
                for uri in uris {
                    self.state.bump_generation(&uri);
                    self.spawn_ambient(uri, None);
                }
                let _ = self.client.workspace_diagnostic_refresh().await;
                result_ok(json!({"recomputed": true}))
            }
            "jev.cancel" => result_ok(json!({"cancelled": true})),
            "jev.explain" => {
                let Some(arg) = params.arguments.first() else {
                    return result_err("bad_arguments", "jev.explain needs {uri, line}");
                };
                let explicit = explicit_scope(arg);
                let provided = provided_context(Some(arg));
                let uri = arg.get("uri").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let line = arg.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let Some(doc) = self.state.doc(&uri) else {
                    return result_err("unknown_document", "no open document for that uri");
                };
                let scope = self.engine.scope_at(&doc, line, explicit);
                let (target, scope_for_call) = (doc.clone(), scope.clone());
                // The answer as it arrives, for the client that asked to see it. The token is
                // the one the client put in the request (§3.5 path 1), so no create request is
                // made and nothing is sent for a token we were not given.
                let outcome = self
                    .streaming(
                        params.work_done_progress_params.work_done_token.clone(),
                        move |engine, delta| {
                            engine.generate_with_context(
                                &target,
                                Verb::Explain,
                                &scope_for_call,
                                &[],
                                &provided,
                                Some(delta),
                            )
                        },
                    )
                    .await;
                match outcome {
                    Ok(Generated::Artifact(markdown)) => {
                        self.state.note_artifact(
                            &uri,
                            scope.range.start_line,
                            scope.range.end_line,
                            &doc.hash,
                            &markdown,
                        );
                        json!({
                        "schema": ARTIFACT_SCHEMA,
                        "kind": "explanation",
                        "id": ActionData::make_id(Verb::Explain, &doc_ref(&doc), &data_scope(&scope), None),
                        "language": doc.language.name,
                        "summary": action_title(Verb::Explain, &scope),
                        "markdown": markdown,
                        })
                    }
                    Ok(Generated::Edit(_)) => result_err("unexpected", "explain produced an edit"),
                    Err(f) => result_err(f.code(), &f.message()),
                }
            }
            "jev.review" => {
                let Some(arg) = params.arguments.first() else {
                    return result_err("bad_arguments", "jev.review needs {uri, line}");
                };
                let uri = arg.get("uri").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let Some(doc) = self.state.doc(&uri) else {
                    return result_err("unknown_document", "no open document for that uri");
                };
                let target = doc.clone();
                // The chat review, because the user asked for it: `:Jev review` is an explicit
                // request for the review tier's opinion, so it does not answer from the ambient
                // cache, which under the default settings holds the rules pass's conclusions.
                let outcome = self
                    .blocking(move |engine| engine.review_now(&target))
                    .await;
                match outcome {
                    Ok(out) => result_ok(json!({
                        "kind": "review",
                        "uri": uri,
                        "findings": out.findings.iter().map(finding_json).collect::<Vec<_>>(),
                        "from_cache": out.from_cache,
                        "discarded": out.rejected,
                    })),
                    Err(f) => result_err(f.code(), &f.message()),
                }
            }
            "jev.inspect" => {
                // The rules pass, on demand, with its counts. The same call the ambient path
                // makes, so what a user sees here is what a save would have produced.
                let arg = params.arguments.first().cloned().unwrap_or_else(|| json!({}));
                let force = arg.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
                let wanted = arg.get("path").and_then(|v| v.as_str());
                let doc = match wanted {
                    Some(path) => self
                        .state
                        .all_docs()
                        .into_iter()
                        .find(|d| d.path == path || d.uri == path || d.path.ends_with(path)),
                    None => {
                        let mut open = self.state.all_docs();
                        if open.len() == 1 {
                            open.pop()
                        } else {
                            None
                        }
                    }
                };
                let Some(doc) = doc else {
                    return result_err(
                        "bad_arguments",
                        "jev.inspect needs {path} naming an open document (or exactly one open)",
                    );
                };
                let target = doc.clone();
                match self
                    .blocking(move |engine| engine.inspect(&target, force))
                    .await
                {
                    Ok(out) => result_ok(jev_core::findings::inspect_fields(
                        &out.findings,
                        out.considered,
                        out.candidates,
                        &out.skipped,
                    )),
                    Err(f) => result_err(f.code(), &f.message()),
                }
            }
            "jev.document" => {
                let Some(arg) = params.arguments.first() else {
                    return result_err(
                        "bad_arguments",
                        "jev.document needs {uri, version, definitions?, context?}",
                    );
                };
                let uri = arg.get("uri").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let version = arg.get("version").and_then(|v| v.as_i64()).unwrap_or(-1) as i32;
                let known = crate::state::KnownDocument {
                    version,
                    definitions: arg
                        .get("definitions")
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_default(),
                    context: arg
                        .get("context")
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_default(),
                };
                let stored = known.definitions.len();
                let contexts = known.context.len();
                self.state.put_known(&uri, known);
                result_ok(json!({
                    "stored": stored,
                    "context": contexts,
                    "uri": uri,
                    "version": version,
                }))
            }
            "jev.session" => {
                let limit = params
                    .arguments
                    .first()
                    .and_then(|a| a.get("limit"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50) as usize;
                let root = self.state.root();
                let entries = root
                    .as_deref()
                    .map(|r| crate::trace::tail(r, limit))
                    .unwrap_or_default();
                result_ok(json!({
                    "entries": entries,
                    "count": entries.len(),
                    // Where the record is, so a user can read it directly and the plugin can
                    // tell "nothing happened yet" from "everything happened elsewhere".
                    "path": root
                        .as_deref()
                        .map(|r| crate::trace::path_for(r).to_string_lossy().to_string()),
                }))
            }
            "jev.usage" => {
                // Counts over what the record still holds, not since the beginning of time:
                // the log trims itself (`trace::MAX_BYTES`), so a number here is a number for
                // the window that is still readable, and saying so is part of the answer.
                let entries = self
                    .state
                    .root()
                    .map(|r| crate::trace::tail(&r, 2000))
                    .unwrap_or_default();
                let mut published = 0usize;
                let mut files: Vec<String> = Vec::new();
                let mut applied = 0usize;
                let mut dismissed = 0usize;
                let mut undone = 0usize;
                for e in &entries {
                    match e.get("kind").and_then(|v| v.as_str()) {
                        Some("analysis") => {
                            // The array is authoritative when present; the older count field
                            // still speaks for entries written before it existed.
                            published += match e.get("findings") {
                                Some(Value::Array(items)) => items.len(),
                                _ => e.get("count").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                            };
                            if let Some(uri) = e.get("uri").and_then(|v| v.as_str()) {
                                if !files.iter().any(|f| f.as_str() == uri) {
                                    files.push(uri.to_string());
                                }
                            }
                        }
                        Some("outcome") => match e.get("event").and_then(|v| v.as_str()) {
                            Some("action-applied") => applied += 1,
                            Some("action-dismissed") | Some("finding-dismissed") => dismissed += 1,
                            Some("edit-undone") => undone += 1,
                            _ => {}
                        },
                        _ => {}
                    }
                }
                result_ok(json!({
                    "window": "session log",
                    "published": published,
                    "files": files.len(),
                    "applied": applied,
                    "dismissed": dismissed,
                    "undone": undone,
                    // The client renders this with `M.open_artifact`, so the answer has to be
                    // an artifact (`PROTOCOL.md` §7): the counts above are for anything that
                    // wants to read them, this is for the person who asked.
                    "kind": "usage",
                    "id": "usage",
                    "summary": "what the server offered and what was done with it",
                    "markdown": format!(
                        "published {published} finding(s) across {} file(s)\n\n\
                         - applied    {applied}\n\
                         - dismissed  {dismissed}\n\
                         - undone     {undone}\n\n\
                         Over the session log: the record trims itself, so these count what is \
                         still readable, not everything since the server started.\n",
                        files.len()
                    ),
                }))
            }
            "jev.outcome" => {
                // What the user did with what was offered. The client is the only witness — it
                // applies the edit, dismisses the finding, accepts the completion — and none of
                // that used to come back, which is why "is this working" had no answer.
                //
                // A command rather than a custom method: N6 freezes the surface at the standard
                // methods plus this one back-channel, and a `jev/…` method would be exactly
                // the invented method the rule forbids. It is a record, not a schema — `event`
                // is written exactly as it arrives, so a client that starts reporting something
                // new is recorded rather than rejected — and it never fails: the user asked for
                // an edit, not for bookkeeping, and a metric must not become a failed action.
                let given = params.arguments.first().cloned().unwrap_or_else(|| json!({}));
                if let Some(root) = self.state.root() {
                    let mut entry = json!({"kind": "outcome"});
                    if let (Some(e), Value::Object(payload)) = (entry.as_object_mut(), given) {
                        for (k, v) in payload {
                            // `kind` is how the log tells one entry type from another, so the
                            // payload's own event name is recorded under `event` instead.
                            if k == "kind" {
                                e.insert("event".to_string(), v);
                            } else {
                                e.insert(k, v);
                            }
                        }
                    }
                    let _ = crate::trace::append(&root, &entry);
                }
                result_ok(json!({"recorded": true}))
            }
            "jev.ask" => {
                let arg = params.arguments.first().cloned().unwrap_or_else(|| json!({}));
                let question = arg
                    .get("question")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if question.is_empty() {
                    return result_err("bad_arguments", "jev.ask needs a question");
                }
                // Reaching the network happens only when the caller allows it, and only to a
                // url the answer names in full — a model that could pull arbitrary bytes into
                // its own prompt is a prompt-injection path, and a user who cannot see which
                // page was read cannot judge the answer.
                let web = arg.get("web").and_then(|v| v.as_bool()).unwrap_or(false);

                // Context is optional and never wider than what the client named: a question
                // about an API has no file at all, and a question about a function is not handed
                // the whole project because the cursor happened to be somewhere.
                let uri = arg.get("uri").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let held = arg
                    .get("line")
                    .and_then(|v| v.as_u64())
                    .and_then(|line| {
                        self.state.doc(&uri).map(|doc| {
                            let scope = self.engine.scope_at(&doc, line as u32, None);
                            (doc, scope)
                        })
                    });
                let ctx = held.as_ref().map(|(doc, scope)| {
                    jev_core::context::build_with(
                        doc,
                        scope,
                        &[],
                        12,
                        &provided_context(Some(&arg)),
                    )
                });

                let cfg = self.state.config();
                let key = jev_core::cache::op_key(
                    "ask",
                    jev_core::types::PROMPT_VERSION,
                    &cfg.models.reason.model,
                    // A question with no file has no language; one with a file has the
                    // language of the prompt it was asked in.
                    ctx.as_ref().map(|c| c.language.as_str()).unwrap_or(""),
                    &format!("{question}|{web}"),
                    0,
                    0,
                    "",
                );
                if let Some(hit) = self.state.cache.get(&key) {
                    if let Some(markdown) = &hit.artifact {
                        return result_ok(json!({"markdown": markdown, "cached": true}));
                    }
                }

                let spec = jev_core::verbs::ask(ctx.as_ref(), &question, web);
                let first = match self.blocking(move |engine| engine.ask_round(spec)).await {
                    Ok(text) => text,
                    Err(f) => return result_err("failed", &f.message()),
                };

                let url = if web {
                    jev_core::verbs::fetch_request(&first).map(|u| u.to_string())
                } else {
                    None
                };
                let (answer, fetched) = match url {
                    None => (first, None),
                    Some(url) => {
                        let page = tokio::task::spawn_blocking({
                            let url = url.clone();
                            move || jev_core::fetch::page(&url)
                        })
                        .await
                        .unwrap_or_else(|e| Err(format!("the fetch task failed: {e}")));
                        match page {
                            Ok(text) => {
                                let grounded = format!(
                                    "{question}\n\nDOCUMENTATION FETCHED FROM {url}:\n\n{text}"
                                );
                                let spec = jev_core::verbs::ask(ctx.as_ref(), &grounded, false);
                                let grounded_answer =
                                    match self.blocking(move |engine| engine.ask_round(spec)).await {
                                        Ok(text) => text,
                                        Err(f) => return result_err("failed", &f.message()),
                                    };
                                (grounded_answer, Some(url))
                            }
                            Err(why) => {
                                return result_err(
                                    "fetch_failed",
                                    &format!("{url} could not be read: {why}"),
                                );
                            }
                        }
                    }
                };

                // The model is shown an artifact schema either way, so an answer that is JSON
                // is one — parse it whatever round it came from. Measured: the first round,
                // asked not to be JSON, answered with an artifact anyway, and the raw object was
                // handed to the user as the answer.
                let markdown = match jev_core::contract::parse_artifact(&answer) {
                    Ok(raw) => raw.markdown,
                    Err(_) if fetched.is_some() => {
                        return result_err(
                            "bad_answer",
                            "the answer after reading a page was not usable",
                        )
                    }
                    Err(_) => answer,
                };
                let markdown = match &fetched {
                    Some(url) => format!("_Read: <{url}>_\n\n{markdown}"),
                    None => markdown,
                };
                self.state.cache.put(
                    &key,
                    jev_core::cache::Conclusion {
                        artifact: Some(markdown.clone()),
                        ..Default::default()
                    },
                );
                result_ok(json!({"markdown": markdown, "fetched": fetched}))
            }
            "jev.followup" => {
                let Some(arg) = params.arguments.first() else {
                    return result_err("bad_arguments", "jev.followup needs {uri, line, question}");
                };
                let question = arg
                    .get("question")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if question.is_empty() {
                    return result_err("bad_arguments", "jev.followup needs a question");
                }
                let uri = arg.get("uri").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let line = arg.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let explicit = explicit_scope(arg);
                let Some(doc) = self.state.doc(&uri) else {
                    return result_err("unknown_document", "no open document for that uri");
                };
                let scope = self.engine.scope_at(&doc, line, explicit);
                // The finding the question is about, when the client says which one: it is what
                // makes the question grounded in the code rather than general.
                let about: Vec<Finding> = arg
                    .get("finding_id")
                    .and_then(|v| v.as_str())
                    .and_then(|id| {
                        self.findings_for(&doc)
                            .0
                            .into_iter()
                            .find(|f| f.id == id)
                    })
                    .into_iter()
                    .collect();
                let (target, scope_for_call) = (doc.clone(), scope.clone());
                // The question is part of the key: two questions about the same range are two
                // different answers, and the cache is content-addressed or it is wrong.
                let key = format!(
                    "{}:{}",
                    jev_core::cache::op_key(
                        "follow-up",
                        jev_core::types::PROMPT_VERSION,
                        &self.state.config().models.reason.model,
                        &doc.language.name,
                        &doc.hash,
                        scope.range.start_line,
                        scope.range.end_line,
                        "",
                    ),
                    question
                );
                let provided = provided_context(Some(arg));
                let asked = question.clone();
                let outcome = self
                    .streaming(
                        params.work_done_progress_params.work_done_token.clone(),
                        move |engine, delta| {
                            engine.artifact_for(
                                &target,
                                &scope_for_call,
                                &about,
                                &provided,
                                &key,
                                |ctx| jev_core::verbs::follow_up(ctx, &asked),
                                Some(delta),
                            )
                        },
                    )
                    .await;
                match outcome {
                    Ok(Generated::Artifact(markdown)) => json!({
                        "schema": ARTIFACT_SCHEMA,
                        "kind": "answer",
                        "id": ActionData::make_id(Verb::Explain, &doc_ref(&doc), &data_scope(&scope), Some(&question)),
                        "language": doc.language.name,
                        "summary": question,
                        "markdown": markdown,
                    }),
                    Ok(Generated::Edit(_)) => {
                        result_err("unexpected", "a follow-up cannot produce an edit")
                    }
                    Err(f) => result_err(f.code(), &f.message()),
                }
            }
            "jev.plan" => {
                let Some(arg) = params.arguments.first() else {
                    return result_err("bad_arguments", "jev.plan needs {goal, scope:{uri, line}}");
                };
                let goal = arg.get("goal").and_then(|v| v.as_str()).unwrap_or_default().trim().to_string();
                let explicit = arg.get("scope").and_then(explicit_scope);
                let uri = arg
                    .get("scope")
                    .and_then(|s| s.get("uri"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let line = arg
                    .get("scope")
                    .and_then(|s| s.get("line"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let Some(doc) = self.state.doc(&uri) else {
                    return result_err("unknown_document", "no open document for that uri");
                };
                let scope = self.engine.scope_at(&doc, line, explicit);
                let (target, scope_for_call) = (doc.clone(), scope.clone());
                let outcome = self
                    .blocking(move |engine| engine.plan(&target, &scope_for_call, &goal))
                    .await;
                match outcome {
                    Ok((plan, rejected)) => {
                        let artifact = self.plan_artifact(&plan, &doc);
                        self.state.put_plan(plan);
                        let mut out = artifact;
                        if rejected > 0 {
                            out["rejected_steps"] = json!(rejected);
                        }
                        out
                    }
                    Err(f) => result_err(f.code(), &f.message()),
                }
            }
            "jev.apply" => {
                let Some(arg) = params.arguments.first() else {
                    return result_err("bad_arguments", "jev.apply needs {plan_id, steps:[n]}");
                };
                let plan_id = arg.get("plan_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let steps: Vec<u32> = arg
                    .get("steps")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_u64()).map(|n| n as u32).collect())
                    .unwrap_or_default();
                let Some(plan) = self.state.plan(&plan_id) else {
                    return result_err("unknown_plan", "that plan is no longer available");
                };
                if steps.is_empty() {
                    return result_err("bad_arguments", "jev.apply needs at least one step number");
                }

                let mut applied = Vec::new();
                let mut failed = Vec::new();
                for n in steps {
                    // Each step is re-anchored against the content the server currently
                    // holds. A step whose target moved is refused as stale rather than
                    // applied to whatever is at that line now (PROTOCOL.md §7).
                    let Some(target_uri) = plan
                        .steps
                        .iter()
                        .find(|s| s.n == n)
                        .and_then(|s| s.targets.first())
                        .map(|t| t.uri.clone())
                    else {
                        failed.push(json!({"n": n, "code": "unknown_step"}));
                        continue;
                    };
                    let Some(doc) = self.state.doc(&target_uri) else {
                        failed.push(json!({"n": n, "code": "unknown_document"}));
                        continue;
                    };
                    let (findings, _, _) = self.findings_for(&doc);
                    let (step_plan, target) = (plan.clone(), doc.clone());
                    let outcome = self
                        .blocking(move |engine| engine.apply_step(&step_plan, n, &target, &findings))
                        .await;
                    match outcome {
                        Ok(proposal) => {
                            let root = self.state.root();
                            let edit = build_workspace_edit(&doc, &proposal, root.as_deref());
                            let before = doc.text.clone();
                            let outcome = self.client.apply_edit(edit).await;
                            match outcome {
                                Ok(r) if r.applied => {
                                    let edit_id = format!("{plan_id}:{n}");
                                    self.state.record_applied(
                                        edit_id.clone(),
                                        crate::state::AppliedEdit {
                                            uri: doc.uri.clone(),
                                            before: before.clone(),
                                        },
                                    );
                                    // Predict what this edit should produce, so the next
                                    // sync can be checked against it (PROTOCOL.md §8).
                                    let prediction =
                                        jev_core::edit::predict_after(&before, &proposal);
                                    self.state.remember_prediction(&doc.uri, prediction);
                                    self.state.mark_step(&plan_id, n, jev_core::types::StepStatus::Applied);
                                    applied.push(json!({"n": n, "edit_id": edit_id, "uri": doc.uri, "summary": proposal.summary}));
                                }
                                Ok(r) => {
                                    self.state.mark_step(&plan_id, n, jev_core::types::StepStatus::Failed);
                                    failed.push(json!({"n": n, "code": "rejected_by_client",
                                        "message": r.failure_reason.unwrap_or_default()}));
                                }
                                Err(e) => {
                                    failed.push(json!({"n": n, "code": "apply_failed", "message": e.to_string()}));
                                }
                            }
                        }
                        Err(f) => {
                            self.state.mark_step(&plan_id, n, jev_core::types::StepStatus::Failed);
                            failed.push(json!({"n": n, "code": f.code(), "message": f.message()}));
                        }
                    }
                }
                let mut out = result_ok(json!({"plan_id": plan_id, "applied": applied, "failed": failed}));
                out["ok"] = json!(failed.is_empty());
                out
            }
            "jev.revert" => {
                let Some(arg) = params.arguments.first() else {
                    return result_err("bad_arguments", "jev.revert needs {edit_id}");
                };
                let edit_id = arg.get("edit_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let Some(record) = self.state.take_applied(&edit_id) else {
                    return result_err("unknown_edit", "nothing to revert for that id");
                };
                let Some(doc) = self.state.doc(&record.uri) else {
                    return result_err("unknown_document", "the document is no longer open");
                };
                let Ok(uri) = Url::parse(&doc.uri) else {
                    return result_err("bad_uri", "the document uri cannot be parsed");
                };
                // The replacement range stops before the file's trailing newline, so that
                // newline is preserved rather than doubled — and the restored text must
                // therefore have its own trailing newline removed.
                let text = &doc.text;
                let (end_line, end_col) = if text.ends_with('\n') {
                    let last_content = doc.line_count().saturating_sub(2);
                    (last_content, jev_core::scope::line_len(text, last_content))
                } else {
                    let last = doc.line_count().saturating_sub(1);
                    (last, jev_core::scope::line_len(text, last))
                };
                let mut restore = record.before.clone();
                if restore.ends_with('\n') {
                    restore.pop();
                }
                let edit = WorkspaceEdit {
                    changes: None,
                    document_changes: Some(DocumentChanges::Edits(vec![TextDocumentEdit {
                        text_document: OptionalVersionedTextDocumentIdentifier {
                            uri,
                            version: Some(doc.version),
                        },
                        edits: vec![OneOf::Left(TextEdit {
                            range: Range {
                                start: Position { line: 0, character: 0 },
                                end: Position {
                                    line: end_line,
                                    character: end_col,
                                },
                            },
                            new_text: restore,
                        })],
                    }])),
                    change_annotations: None,
                };
                match self.client.apply_edit(edit).await {
                    Ok(r) if r.applied => result_ok(json!({"reverted": record.uri})),
                    Ok(r) => result_err("rejected_by_client", r.failure_reason.as_deref().unwrap_or("")),
                    Err(e) => result_err("apply_failed", &e.to_string()),
                }
            }
            other => result_err(
                "not_implemented",
                &format!("{other} is not implemented in this version"),
            ),
        }
    }
}

fn data_scope(scope: &jev_core::scope::Resolved) -> ScopeRef {
    ScopeRef {
        kind: scope.kind,
        name: scope.name.clone(),
        start_line: scope.range.start_line,
        end_line: scope.range.end_line,
    }
}

impl JevServer {
    /// The plan artifact of PROTOCOL.md §7: targets as `{uri, version, range}`, steps in
    /// order, and the cost of producing it.
    fn plan_artifact(&self, plan: &jev_core::types::Plan, doc: &jev_core::Document) -> Value {
        let steps: Vec<Value> = plan
            .steps
            .iter()
            .map(|s| {
                let targets: Vec<Value> = s
                    .targets
                    .iter()
                    .map(|t| {
                        json!({
                            "uri": t.uri,
                            "version": t.version,
                            "range": {
                                "start": {"line": t.line, "character": 0},
                                "end": {"line": t.line,
                                        "character": jev_core::scope::line_len(&doc.text, t.line)},
                            },
                        })
                    })
                    .collect();
                json!({
                    "n": s.n,
                    "title": s.title,
                    "rationale": s.rationale,
                    "verb": s.verb.as_str(),
                    "status": s.status,
                    "targets": targets,
                })
            })
            .collect();

        json!({
            "schema": ARTIFACT_SCHEMA,
            "kind": "plan",
            "id": plan.id,
            "created": jev_core::time::now_rfc3339(),
            "goal": plan.goal,
            "language": plan.language,
            "steps": steps,
            "usage": plan.usage,
        })
    }
}
