//! The language server surface (PROTOCOL.md §2, §3).

use crate::engine::{Engine, Failure, Generated, Outcome};
use crate::state::{AppState, Claim};
use meta_core::types::{
    ActionData, ActionState, Finding, LineRange, Proposal, ScopeKind, ScopeRef, ScopeSource,
    Severity, Verb, ACTION_DATA_VERSION, ARTIFACT_SCHEMA, RESULT_SCHEMA,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tower_lsp::jsonrpc::Result as RpcResult;
use tower_lsp::lsp_types::notification::Progress;
use tower_lsp::lsp_types::*;
use tower_lsp::{async_trait, Client, LanguageServer};

pub struct MetaServer {
    client: Client,
    state: Arc<AppState>,
    engine: Engine,
}

impl MetaServer {
    pub fn new(client: Client, state: Arc<AppState>) -> MetaServer {
        let engine = Engine::new(state.clone());
        MetaServer {
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
                    CodeActionKind::new("quickfix.meta"),
                    CodeActionKind::REFACTOR_REWRITE,
                    CodeActionKind::new("refactor.rewrite.meta"),
                    CodeActionKind::SOURCE,
                    CodeActionKind::new("source.meta"),
                    CodeActionKind::SOURCE_FIX_ALL,
                ]),
                work_done_progress_options: WorkDoneProgressOptions::default(),
                resolve_provider: Some(true),
            })),
            diagnostic_provider: Some(DiagnosticServerCapabilities::Options(DiagnosticOptions {
                identifier: Some("meta".to_string()),
                inter_file_dependencies: false,
                // Deliberately false, and it must stay false until `workspace/diagnostic` is
                // implemented. Neovim's `on_refresh` checks this capability first and takes
                // the *workspace* branch when it is set, so advertising it while serving only
                // per-document diagnostics means every `workspace/diagnostic/refresh` is
                // answered by a method that does not exist and the client never re-pulls:
                // findings are cached by the server and never reach the sign column.
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

    /// Run the ambient analysis off the request path, then ask the client to re-pull.
    ///
    /// Guarantees, each of which a test asserts:
    ///  * one run per document at a time; a request during a run is queued, not dropped;
    ///  * the client is always told to re-pull when a run ends, whatever the outcome —
    ///    including when the run is superseded by a newer edit, because a client that pulled
    ///    during the window would otherwise stay empty forever;
    ///  * a superseded result is never reported.
    fn spawn_analysis(&self, uri: String, debounce: Option<std::time::Duration>) {
        if self.state.claim_analysis(&uri) == Claim::Queued {
            return;
        }
        let state = self.state.clone();
        let client = self.client.clone();
        tokio::spawn(async move {
            // A save that lands during startup must not be analysed against the built-in
            // defaults: wait for the client's settings, then proceed.
            state.await_config(std::time::Duration::from_secs(5)).await;
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
                        Some(engine.analyze(&doc))
                    })
                    .await
                };

                let superseded = state.generation(&uri) != generation;
                if let Ok(Some(outcome)) = outcome {
                    if !superseded {
                        // `outcome` here is what `report` takes: a Result, because a run can
                        // fail as well as find nothing, and only a completed run is recorded.
                        if let (Ok(out), Some(root)) = (&outcome, state.root()) {
                            let _ = crate::trace::append(
                                &root,
                                &json!({
                                    "kind": "analysis",
                                    "uri": uri,
                                    "findings": out.findings.len(),
                                    "discarded": out.rejected,
                                    "from_cache": out.from_cache,
                                    // The first finding's line, so the entry can be walked
                                    // back to the place it is about.
                                    "line": out.findings.first().map(|f| f.line),
                                }),
                            );
                        }
                        report(&client, &uri, outcome).await;
                    }
                }

                let again = state.finish_analysis(&uri);
                // Always signal, even when superseded or failed: the client re-pulls and
                // sees whatever the cache currently holds for the document.
                if state.doc(&uri).is_some() {
                    // The lenses and the hints both carry a finding count, so a new analysis
                    // changes them.
                    if client.inlay_hint_refresh().await.is_err() {
                        client
                            .log_message(
                                MessageType::LOG,
                                "meta: client does not serve workspace/inlayHint/refresh",
                            )
                            .await;
                    }
                    if client.code_lens_refresh().await.is_err() {
                        client
                            .log_message(
                                MessageType::LOG,
                                "meta: client does not serve workspace/codeLens/refresh",
                            )
                            .await;
                    }
                    if client.workspace_diagnostic_refresh().await.is_err() {
                        client
                            .log_message(
                                MessageType::LOG,
                                "meta: client does not serve workspace/diagnostic/refresh",
                            )
                            .await;
                    }
                }
                if !again {
                    break;
                }
            }
        });
    }

    /// The checks every read-only surface shares: the server is on, and this is a document the
    /// analysis would accept. A surface that offers what the analysis would then refuse is
    /// worse than one that offers nothing.
    fn analysable(&self, doc: &meta_core::Document) -> Option<meta_core::config::Config> {
        let cfg = self.state.config();
        if !cfg.enabled {
            return None;
        }
        if meta_core::gates::evaluate(
            &doc.text,
            &doc.path,
            cfg.languages.max_file_bytes,
            cfg.languages.max_scope_lines,
            &cfg.languages.ignore,
        )
        .is_some()
        {
            return None;
        }
        Some(cfg)
    }

    fn findings_for(&self, doc: &meta_core::Document) -> (Vec<Finding>, bool) {
        match self.engine.cached(doc) {
            Some(c) => (c.findings.clone(), true),
            None => (Vec::new(), false),
        }
    }

    // ---- code actions -------------------------------------------------------

    fn scope_of(&self, doc: &meta_core::Document, params: &CodeActionParams) -> meta_core::scope::Resolved {
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
        doc: &meta_core::Document,
        verb: Verb,
        scope: &meta_core::scope::Resolved,
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

fn doc_ref(doc: &meta_core::Document) -> meta_core::types::DocRef {
    meta_core::types::DocRef {
        uri: doc.uri.clone(),
        version: doc.version,
        content_hash: doc.hash.clone(),
    }
}

/// Commands actually served. A command that only answers "not implemented" is not
/// advertised (PROTOCOL.md §2), and a command the plugin offers is one it has to serve —
/// `meta.review` was missing here while `:Meta review` sent it, so that keymap answered
/// "not implemented" every time.
const COMMANDS: &[&str] = &[
    "meta.status",
    "meta.recompute",
    "meta.review",
    "meta.explain",
    "meta.followup",
    "meta.session",
    "meta.plan",
    "meta.apply",
    "meta.revert",
    "meta.cancel",
];

/// Deterministic title. Never model output (PROTOCOL.md §4).
fn action_title(verb: Verb, scope: &meta_core::scope::Resolved) -> String {
    let subject = scope
        .name
        .clone()
        .unwrap_or_else(|| scope.kind.as_str().to_string());
    format!("{}: {}", verb.label(), subject)
}

fn to_diagnostic(f: &Finding, content_hash: &str) -> Diagnostic {
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
        source: Some("meta".to_string()),
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
    doc: &meta_core::Document,
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

/// A value from the first argument, or from `scope` inside it.
///
/// Commands put the anchor at the top level (`meta.explain`) or under `scope` (`meta.plan`),
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
fn explicit_scope(arg: &Value) -> Option<meta_core::types::LineRange> {
    let range = arg.get("range")?;
    let start_line = range.get("start_line").and_then(|v| v.as_u64())? as u32;
    let end_line = range.get("end_line").and_then(|v| v.as_u64())? as u32;
    if end_line < start_line {
        return None;
    }
    Some(meta_core::types::LineRange {
        start_line,
        end_line,
    })
}

/// One finding as a client receives it in a Result. The same fields the diagnostic carries,
/// so the two surfaces cannot describe the same finding differently.
fn finding_json(f: &Finding) -> Value {
    json!({
        "id": f.id,
        "line": f.line,
        "start_col": f.start_col,
        "end_col": f.end_col,
        "severity": match f.severity {
            Severity::Warning => "warning",
            Severity::Information => "information",
        },
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

/// Log what an analysis did. Never user-facing: findings reach the user through
/// diagnostics, and a skip or a failure is a log line rather than a popup, because the
/// user did not ask for this work (docs/UX.md §4).
async fn report(client: &Client, uri: &str, outcome: Result<Outcome, Failure>) {
    match outcome {
        Ok(out) => {
            client
                .log_message(
                    MessageType::LOG,
                    format!(
                        "meta: cached {} finding(s) for {}{}",
                        out.findings.len(),
                        uri,
                        if out.from_cache { " (cache hit)" } else { "" }
                    ),
                )
                .await;
            if out.rejected > 0 {
                client
                    .log_message(
                        MessageType::LOG,
                        format!(
                            "meta: {} finding(s) discarded because their anchors could not be located",
                            out.rejected
                        ),
                    )
                    .await;
            }
        }
        Err(Failure::Skipped(reason)) => {
            client
                .log_message(MessageType::LOG, format!("meta: skipping analysis — {reason}"))
                .await;
        }
        Err(f) => {
            client
                .log_message(MessageType::LOG, format!("meta: analysis failed — {}", f.message()))
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
/// `message` stays short and human, because Neovim's own progress UI renders it; the text so
/// far travels in `data`, which only the plugin reads. The artifact in the response remains
/// the authoritative one — this is a preview, and a repair attempt means it may be a preview
/// of an answer that was rejected.
async fn partial_artifact(client: &Client, token: &ProgressToken, markdown: &str) {
    let value = json!({
        "kind": "report",
        "message": format!("meta: explaining — {} bytes", markdown.len()),
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
const STREAM_FLUSH: std::time::Duration = std::time::Duration::from_millis(120);

/// How often to say something while the model is still thinking: slow enough not to be noise,
/// fast enough that a four-second prefill does not look like a hang.
const STREAM_HEARTBEAT: std::time::Duration = std::time::Duration::from_millis(1000);

/// Nothing to show yet, but the work is real and it is running.
async fn nothing_yet(client: &Client, token: &ProgressToken, waited: std::time::Duration) {
    let value = json!({
        "kind": "report",
        "message": format!(
            "meta: explaining — waiting for the model ({:.0}s)",
            waited.as_secs_f64()
        ),
        "data": { "partial": true, "markdown": "", "waiting": true },
    });
    client
        .send_notification::<RawProgress>(json!({"token": token, "value": value}))
        .await;
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
impl LanguageServer for MetaServer {
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
                name: "meta-lsp".to_string(),
                version: Some(meta_core::VERSION.to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.pull_configuration().await;
    }

    /// The client says its settings changed: ask for them again (PROTOCOL.md §10).
    ///
    /// Without this the server keeps whatever it read at startup, so the plugin's kill switch
    /// (`:Meta stop`, which flips `enabled` and notifies) would have no effect at all.
    async fn did_change_configuration(&self, _: DidChangeConfigurationParams) {
        self.pull_configuration().await;
    }

    async fn shutdown(&self) -> RpcResult<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let d = params.text_document;
        let doc = meta_core::Document::new(
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

        let mode = self.state.config().triggers.diagnostics.clone();
        let idle = self.state.config().triggers.idle_ms;

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

        if mode == "idle" {
            self.spawn_analysis(uri, Some(std::time::Duration::from_millis(idle)));
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        self.state.bump_generation(&uri);
        self.verify_prediction(&uri).await;
        if self.state.config().triggers.diagnostics == "save" {
            self.spawn_analysis(uri, None);
        }
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
        let (findings, cached) = self.findings_for(&doc);
        let cursor_line = params.range.start.line;
        let invoked = params.context.trigger_kind != Some(CodeActionTriggerKind::AUTOMATIC);
        let mut actions: Vec<CodeActionOrCommand> = Vec::new();

        // One action per finding, so the user fixes what they highlighted.
        let mut relevant: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.line >= scope.range.start_line && f.line <= scope.range.end_line)
            .take(cfg.noise.max_visible_findings)
            .collect();
        if relevant.is_empty() {
            relevant = findings
                .iter()
                .filter(|f| f.line == cursor_line)
                .take(cfg.noise.max_visible_findings)
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
            let file_scope = meta_core::scope::Resolved {
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
            data.summary = Some(format!("meta {} is available", meta_core::VERSION));
            let mut action = self.make_action(
                "meta: no findings cached for this file yet".to_string(),
                data,
                Verb::Review.kind(),
                false,
            );
            action.disabled = Some(CodeActionDisabled {
                reason: "analysing in the background; reopen the menu in a moment".to_string(),
            });
            actions.push(CodeActionOrCommand::CodeAction(action));
            self.spawn_analysis(uri, None);
        }

        Ok(Some(actions))
    }

    async fn code_action_resolve(&self, action: CodeAction) -> RpcResult<CodeAction> {
        let Some(raw) = action.data.clone() else {
            return Ok(action);
        };
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

        let scope = meta_core::scope::Resolved {
            range: LineRange {
                start_line: data.scope.start_line,
                end_line: data.scope.end_line,
            },
            kind: data.scope.kind,
            source: data.scope_source,
            name: data.scope.name.clone(),
            truncated: false,
        };
        let (findings, _) = self.findings_for(&doc);

        // `review` re-runs the analysis rather than editing anything.
        if data.verb == Verb::Review {
            let uri = data.doc.uri.clone();
            self.state.bump_generation(&uri);
            self.spawn_analysis(uri, None);
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
            .blocking(move |engine| engine.generate(&target, verb, &scope, &in_scope))
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
                        uri: Url::parse(&format!("meta://artifact/{}", data.id))
                            .unwrap_or_else(|_| Url::parse("meta://artifact").unwrap()),
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
        let cfg = self.state.config();
        let (findings, _) = self.findings_for(&doc);
        let items = findings
            .iter()
            .take(cfg.noise.max_visible_findings)
            .map(|f| to_diagnostic(f, &doc.hash))
            .collect();
        Ok(DocumentDiagnosticReportResult::Report(
            DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
                related_documents: None,
                full_document_diagnostic_report: FullDocumentDiagnosticReport {
                    result_id: Some(format!("meta-{}", doc.hash)),
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
        let uri = params.text_document.uri.to_string();
        let Some(doc) = self.state.doc(&uri) else {
            return Ok(None);
        };
        let Some(cfg) = self.analysable(&doc) else {
            return Ok(None);
        };
        let (findings, _) = self.findings_for(&doc);
        if findings.is_empty() {
            return Ok(None);
        }

        let profile = meta_core::lang::profile(&doc.language.name);
        let first = params.range.start.line;
        let last = params.range.end.line;
        let lines: Vec<&str> = doc.text.lines().collect();
        let hints = meta_core::scope::blocks(&doc.text, &profile, cfg.languages.max_scope_lines)
            .into_iter()
            .filter(|block| block.range.start_line >= first && block.range.start_line <= last)
            .filter_map(|block| {
                let here: Vec<&Finding> = findings
                    .iter()
                    .filter(|f| f.line >= block.range.start_line && f.line <= block.range.end_line)
                    .collect();
                if here.is_empty() {
                    return None;
                }
                // Byte offset, because this server speaks utf-8 (N1).
                let head = lines.get(block.range.start_line as usize)?;
                Some(InlayHint {
                    position: Position {
                        line: block.range.start_line,
                        character: head.len() as u32,
                    },
                    label: InlayHintLabel::String(format!(
                        "meta: {} finding{}",
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

        let profile = meta_core::lang::profile(&doc.language.name);
        let (findings, _) = self.findings_for(&doc);
        let lenses = meta_core::scope::blocks(&doc.text, &profile, cfg.languages.max_scope_lines)
            .into_iter()
            .map(|block| {
                let line = block.range.start_line;
                let in_scope = findings
                    .iter()
                    .filter(|f| f.line >= block.range.start_line && f.line <= block.range.end_line)
                    .count();
                let (title, command) = if in_scope > 0 {
                    (
                        format!(
                            "meta: {} finding{} · fix",
                            in_scope,
                            if in_scope == 1 { "" } else { "s" }
                        ),
                        "meta.plugin.pick",
                    )
                } else {
                    ("meta: explain".to_string(), "meta.plugin.explain")
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

    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> RpcResult<Option<serde_json::Value>> {
        let token = params.work_done_progress_params.work_done_token.clone();
        let command = params.command.clone();
        let started = std::time::Instant::now();
        if let Some(t) = &token {
            progress(
                &self.client,
                t,
                WorkDoneProgress::Begin(WorkDoneProgressBegin {
                    title: format!("meta {command}"),
                    cancellable: Some(false),
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
        Ok(Some(value))
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

impl MetaServer {
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
                format!("meta: divergence after an applied edit in {uri} at line {}", line + 1),
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
                    code: Some(NumberOrString::String("meta.divergence".to_string())),
                    code_description: None,
                    source: Some("meta".to_string()),
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

    /// `textDocument/inlineCompletion` (3.18 draft), registered as a custom method because
    /// the pinned `lsp-types` has no handler for it.
    ///
    /// Everything here answers with an empty list rather than an error: a completion is
    /// offered while the user is typing, so a refusal must be silent — no popup, no
    /// diagnostic, just no ghost text (`docs/UX.md` §6). The reason goes to the log.
    pub async fn inline_completion(
        &self,
        params: crate::inline::InlineParams,
    ) -> tower_lsp::jsonrpc::Result<crate::inline::InlineList> {
        let uri = params.text_document.uri.to_string();
        let empty = crate::inline::InlineList::default();
        let Some(doc) = self.state.doc(&uri) else {
            return Ok(empty);
        };
        // Neovim sends triggerKind 1 for a manual request and 2 when it fired on its own
        // timer (`lsp/inline_completion.lua`); only the timed path is rate-limited by context.
        let invoked = params
            .context
            .as_ref()
            .and_then(|c| c.trigger_kind)
            .is_some_and(|k| k == 1);
        let (line, character) = (params.position.line, params.position.character);
        let outcome = self
            .blocking(move |engine| engine.complete(&doc, line, character, invoked))
            .await;
        match outcome {
            Ok(text) if !text.is_empty() => Ok(crate::inline::InlineList {
                items: vec![crate::inline::InlineItem {
                    insert_text: text,
                    // No range: the text belongs exactly at the cursor, and proposing a
                    // range invites the client to replace more than the user selected.
                    range: None,
                }],
            }),
            Ok(_) => Ok(empty),
            Err(f) => {
                self.client
                    .log_message(
                        MessageType::LOG,
                        format!("meta: no completion — {}", f.message()),
                    )
                    .await;
                Ok(empty)
            }
        }
    }

    /// Run an engine call off the async worker.
    ///
    /// The model client is synchronous (one blocking `ureq` implementation, no async in
    /// `meta-core`), so calling it from an `async fn` blocks a runtime thread for as long as
    /// the request takes — up to the tier timeout. Inline completion fires on a 200 ms timer
    /// while the user types, so this is not theoretical: it is how a language server stalls
    /// every other handler behind one model call.
    ///
    /// Every model-calling handler goes through here.
    async fn blocking<T, F>(&self, work: F) -> Result<T, Failure>
    where
        F: FnOnce(Engine) -> Result<T, Failure> + Send + 'static,
        T: Send + 'static,
    {
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
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let forwarder = token.map(|t| {
            let client = self.client.clone();
            tokio::spawn(async move {
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
            })
        });
        let outcome = self
            .blocking(move |engine| {
                let mut on_delta = move |delta: &str| {
                    let _ = tx.send(delta.to_string());
                };
                work(engine, &mut on_delta)
            })
            .await;
        if let Some(handle) = forwarder {
            let _ = handle.await;
        }
        outcome
    }

    /// Read the `meta` section from the client and merge it over the current settings.
    async fn pull_configuration(&self) {
        let items = vec![ConfigurationItem {
            scope_uri: None,
            section: Some("meta".to_string()),
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
                                        "meta: the `meta` settings could not be applied ({why}); running on the built-in defaults"
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
                                        "meta: settings applied — reason {} · review {} · inline completion {}",
                                        cfg.models.reason.base_url,
                                        cfg.models.review.base_url,
                                        if cfg.inline_completion.enabled { "on" } else { "off" }
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
                        "meta: workspace/configuration unavailable; using defaults",
                    )
                    .await;
            }
        }
    }

    async fn run_command(&self, params: &ExecuteCommandParams) -> Value {
        match params.command.as_str() {
            "meta.status" => {
                let cfg = self.state.config();
                let (hits, misses, entries) = self.state.cache.stats();
                let snap = self.state.budget.snapshot();
                let (calls, refusals) = self.state.budget.counters();
                result_ok(json!({
                    "version": meta_core::VERSION,
                    "enabled": cfg.enabled,
                    "documents": self.state.doc_count(),
                    "analysis_in_flight": self.state.is_analyzing(),
                    "plans": self.state.plan_count(),
                    "fim_calls_last_minute": self.state.fim.calls_last_minute(),
                    "cache": {"entries": entries, "hits": hits, "misses": misses},
                    "budget": {
                        "calls_last_minute": snap.calls_last_minute,
                        "calls_last_hour": snap.calls_last_hour,
                        "tokens_used": snap.tokens_used,
                        "in_flight": snap.in_flight,
                        "limit_per_minute": cfg.budget.max_calls_per_min,
                        "limit_per_hour": cfg.budget.max_calls_per_hour,
                        "limit_tokens": cfg.budget.max_tokens_per_session,
                    },
                    "counters": {"calls": calls, "refusals": refusals},
                    "models": {
                        "reason": {"base_url": cfg.models.reason.base_url, "model": cfg.models.reason.model},
                        "review": {"base_url": cfg.models.review.base_url, "model": cfg.models.review.model},
                    },
                    "triggers": {"diagnostics": cfg.triggers.diagnostics, "idle_ms": cfg.triggers.idle_ms},
                }))
            }
            "meta.recompute" => {
                self.state.cache.clear();
                let uris: Vec<String> = self.state.all_docs().iter().map(|d| d.uri.clone()).collect();
                for uri in uris {
                    self.state.bump_generation(&uri);
                    self.spawn_analysis(uri, None);
                }
                let _ = self.client.workspace_diagnostic_refresh().await;
                result_ok(json!({"recomputed": true}))
            }
            "meta.cancel" => result_ok(json!({"cancelled": true})),
            "meta.explain" => {
                let Some(arg) = params.arguments.first() else {
                    return result_err("bad_arguments", "meta.explain needs {uri, line}");
                };
                let explicit = explicit_scope(arg);
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
                            engine.generate_streaming(
                                &target,
                                Verb::Explain,
                                &scope_for_call,
                                &[],
                                Some(delta),
                            )
                        },
                    )
                    .await;
                match outcome {
                    Ok(Generated::Artifact(markdown)) => json!({
                        "schema": ARTIFACT_SCHEMA,
                        "kind": "explanation",
                        "id": ActionData::make_id(Verb::Explain, &doc_ref(&doc), &data_scope(&scope), None),
                        "language": doc.language.name,
                        "summary": action_title(Verb::Explain, &scope),
                        "markdown": markdown,
                    }),
                    Ok(Generated::Edit(_)) => result_err("unexpected", "explain produced an edit"),
                    Err(f) => result_err(f.code(), &f.message()),
                }
            }
            "meta.review" => {
                let Some(arg) = params.arguments.first() else {
                    return result_err("bad_arguments", "meta.review needs {uri, line}");
                };
                let uri = arg.get("uri").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let Some(doc) = self.state.doc(&uri) else {
                    return result_err("unknown_document", "no open document for that uri");
                };
                let target = doc.clone();
                // The same analysis a save runs, but the caller waits for it instead of the
                // findings arriving later as diagnostics. `:Meta review` is the user asking
                // now, so the answer belongs in the Result.
                let outcome = self
                    .blocking(move |engine| engine.analyze(&target))
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
            "meta.session" => {
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
            "meta.followup" => {
                let Some(arg) = params.arguments.first() else {
                    return result_err("bad_arguments", "meta.followup needs {uri, line, question}");
                };
                let question = arg
                    .get("question")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if question.is_empty() {
                    return result_err("bad_arguments", "meta.followup needs a question");
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
                    meta_core::cache::op_key(
                        "follow-up",
                        meta_core::types::PROMPT_VERSION,
                        &doc.hash,
                        scope.range.start_line,
                        scope.range.end_line,
                    ),
                    question
                );
                let asked = question.clone();
                let outcome = self
                    .streaming(
                        params.work_done_progress_params.work_done_token.clone(),
                        move |engine, delta| {
                            engine.artifact_for(
                                &target,
                                &scope_for_call,
                                &about,
                                &key,
                                |ctx| meta_core::verbs::follow_up(ctx, &asked),
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
            "meta.plan" => {
                let Some(arg) = params.arguments.first() else {
                    return result_err("bad_arguments", "meta.plan needs {goal, scope:{uri, line}}");
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
            "meta.apply" => {
                let Some(arg) = params.arguments.first() else {
                    return result_err("bad_arguments", "meta.apply needs {plan_id, steps:[n]}");
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
                    return result_err("bad_arguments", "meta.apply needs at least one step number");
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
                    let (findings, _) = self.findings_for(&doc);
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
                                        meta_core::edit::predict_after(&before, &proposal);
                                    self.state.remember_prediction(&doc.uri, prediction);
                                    self.state.mark_step(&plan_id, n, meta_core::types::StepStatus::Applied);
                                    applied.push(json!({"n": n, "edit_id": edit_id, "uri": doc.uri, "summary": proposal.summary}));
                                }
                                Ok(r) => {
                                    self.state.mark_step(&plan_id, n, meta_core::types::StepStatus::Failed);
                                    failed.push(json!({"n": n, "code": "rejected_by_client",
                                        "message": r.failure_reason.unwrap_or_default()}));
                                }
                                Err(e) => {
                                    failed.push(json!({"n": n, "code": "apply_failed", "message": e.to_string()}));
                                }
                            }
                        }
                        Err(f) => {
                            self.state.mark_step(&plan_id, n, meta_core::types::StepStatus::Failed);
                            failed.push(json!({"n": n, "code": f.code(), "message": f.message()}));
                        }
                    }
                }
                let mut out = result_ok(json!({"plan_id": plan_id, "applied": applied, "failed": failed}));
                out["ok"] = json!(failed.is_empty());
                out
            }
            "meta.revert" => {
                let Some(arg) = params.arguments.first() else {
                    return result_err("bad_arguments", "meta.revert needs {edit_id}");
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
                    (last_content, meta_core::scope::line_len(text, last_content))
                } else {
                    let last = doc.line_count().saturating_sub(1);
                    (last, meta_core::scope::line_len(text, last))
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

fn data_scope(scope: &meta_core::scope::Resolved) -> ScopeRef {
    ScopeRef {
        kind: scope.kind,
        name: scope.name.clone(),
        start_line: scope.range.start_line,
        end_line: scope.range.end_line,
    }
}

impl MetaServer {
    /// The plan artifact of PROTOCOL.md §7: targets as `{uri, version, range}`, steps in
    /// order, and the cost of producing it.
    fn plan_artifact(&self, plan: &meta_core::types::Plan, doc: &meta_core::Document) -> Value {
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
                                        "character": meta_core::scope::line_len(&doc.text, t.line)},
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
            "created": meta_core::time::now_rfc3339(),
            "goal": plan.goal,
            "language": plan.language,
            "steps": steps,
            "usage": plan.usage,
        })
    }
}
