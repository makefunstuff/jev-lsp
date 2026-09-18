# PROTOCOL.md — frozen contract

Frozen. Changes require editing this file first, with a dated changelog entry. Every
constraint here is backed by a probe in `docs/research/nvim-lsp-surface.md`; the label
`[R#]` refers to that document's section numbers.

Scope: the LSP surface between `crates/meta-lsp` and any LSP client; the artifact schemas;
the CLI contract. Internal design is not frozen here.

Name: workspace and binary are `meta`, crates `meta-core` / `meta-lsp` / `meta`, plugin
`nvim/`. The repository directory is `meta-lsp`. Reversible by mechanical sweep.

---

## 1. Non-negotiables

| # | Rule | Why |
|---|---|---|
| N1 | Declare `positionEncoding: "utf-8"`. All ranges are byte offsets. | Client honours the server's choice `[R6]`. Byte offsets equal Rust `&str` indices and treesitter byte columns. |
| N2 | The `codeAction` request **never** waits on a model call. It serves cached results or returns no actions. | The client awaits every client before showing the menu; a slow response is a frozen editor. |
| N3 | Expensive work happens in `codeAction/resolve` or in the background worker. | Resolve is invoked only for the picked action `[R4]`. |
| N4 | Every `WorkspaceEdit` uses `documentChanges` with an explicit integer `version` on each `TextDocumentEdit`. | Bare `changes` skips the version check; an absent `version` raises inside the client `[R3]`. |
| N5 | The model never emits LSP ranges. | Ranges are encoding- and version-sensitive; the server derives them from treesitter scope plus byte offsets. |
| N6 | No custom `meta/…` LSP methods. | The standard surface plus the in-process Lua plugin covers every requirement `[R1]`; custom methods buy nothing and cost portability. |
| N7 | Free-text input comes from the plugin, never from the server. | The protocol cannot ask for text `[R1]`. |
| N8 | Nothing is applied without approval, except verbs explicitly marked `auto` in config. | Silent edits destroy trust faster than wrong edits. |
| N9 | The server holds no cross-session memory. Caches are keyed by content hash and are never a source of truth. | Two editors, two views; a stale cache must be detectably stale, not authoritative. |
| N10 | Support is unconditional. No language, filetype, parser, or resolution result may gate attachment, sync, or the verb set. | The model needs no grammar to read text. `docs/LANGUAGE.md`. |
| N11 | Classification is metadata, carried in `data` and artifacts, and never a filter. `unknown` is a valid language. | A bad result must be attributable to a bad classification, and nothing may be silently excluded. |
| N12 | A partial report carries the answer **so far**, never a delta. | Replacing what is displayed with a cumulative value is idempotent: a lost, duplicated, or delayed report cannot corrupt the text a user is reading `[R13]`. |

---

## 2. Capabilities

Server capabilities returned from `initialize`:

```jsonc
{
  "positionEncoding": "utf-8",
  "textDocumentSync": { "openClose": true, "change": 2, "save": { "includeText": false } },
  "codeActionProvider": {
    "resolveProvider": true,
    "codeActionKinds": [
      "quickfix", "quickfix.meta",
      "refactor.rewrite", "refactor.rewrite.meta",
      "source", "source.meta", "source.fixAll"
    ]
  },
  "diagnosticProvider": { "identifier": "meta", "interFileDependencies": false, "workspaceDiagnostics": false },
  "inlineCompletionProvider": {},
  "executeCommandProvider": { "commands": [ /* §6 */ ], "workDoneProgress": true }
}
```

`workspaceDiagnostics` is `false` and must stay false until `workspace/diagnostic` is
implemented here. Neovim's `on_refresh` checks that capability *first* and takes the
**workspace** branch when it is set, so advertising it while serving only per-document pull
means every `workspace/diagnostic/refresh` is answered by a method that does not exist: the
server caches findings and nothing ever reaches the sign column. Measured, and fixed — see
`docs/VERIFICATION.md` §8.

`inlineCompletionProvider` is 3.18 draft and is **not expressible** by the `lsp-types` 0.94
that tower-lsp 0.20 pins, so it is injected into the `initialize` response at the transport
boundary (`crates/meta-lsp/src/advertised.rs`). Neovim only attaches its completor for a
client that advertises it, so without the injection the handler would be unreachable.

**Rule: advertise only what is served.** A provider the server does not implement is a lie
the client will act on — it will send requests that can only fail, or set up UI for progress
that never reports. The block above is therefore the *complete* advertised set, and it grows
one entry at a time as features land. `codeLensProvider` and `inlayHintProvider` are part of the design
(`docs/UX.md`, `docs/ROADMAP.md`) and are deliberately **not** advertised yet: they are not
implemented.

Standard kinds only, with a `.meta` suffix where the origin matters. Kind filtering in the
client is prefix-based on `.` `[R4]`, so `refactor.rewrite` also matches
`refactor.rewrite.meta`, while `only = ["refactor.rewrite.meta"]` selects just ours.

`workDoneProgress: true` is declared on `executeCommandProvider` because that is the only
request we report progress on. Per the specification this flag exists so a client does not
set up a progress UI for a request the server will never report on; declaring it where it
is not used is as wrong as omitting it where it is `[R12]`.

---

## 3. Method surface

`class` is the latency budget the client experiences. `gate` is the cost control that must
be enforced before any model call.

### 3.1 Client to server, code actions

| Method | Class | Gate | Notes |
|---|---|---|---|
| `textDocument/codeAction` | p99 < 50 ms, no model call | none needed | Reads the conclusion cache for `(uri, content_hash)`. Cold cache with `triggerKind = Invoked` returns one `disabled` action carrying `data` and `disabled.reason = "analyzing…"` `[R4]`. With `triggerKind = Automatic`, returns only already-computed actions, never a placeholder. |
| `codeAction/resolve` | p50 < 2 s, hard timeout 90 s | per-session call budget | Where generation happens. Returns the action with `edit` and/or `command` filled. On timeout, returns the action unchanged so the client falls back rather than erroring `[R4]`. |

### 3.2 Client to server, observation

| Method | Class | Gate | Notes |
|---|---|---|---|
| `textDocument/diagnostic` | p99 < 30 ms | severity floor, debounce | Serves from the conclusion cache; never triggers a model call inline. `resultId` for incremental re-pull. |
| `workspace/diagnostic` | p99 < 200 ms | same | Whole-repo pull; serves cached findings only. |
| `textDocument/codeLens` | p99 < 30 ms | per-symbol count cap | Affordances offered per symbol. |
| `codeLens/resolve` | p99 < 50 ms | — | Fills the `command`. |
| `textDocument/inlayHint` | p99 < 30 ms | default **off** except risk markers | Cached only. |
| `inlayHint/resolve` | p99 < 50 ms | — | |
| `textDocument/inlineCompletion` | p50 < 150 ms, p99 < 500 ms | see §5 — the strictest gate in the system | FIM tier only. |
| `textDocument/hover` | p99 < 100 ms | on-demand prefetch | Cached explanation. Miss returns the plain signature immediately and warms the cache. |

### 3.3 Client to server, control

`workspace/executeCommand` (§6) and `workspace/didChangeWatchedFiles` (repo-level
observation, dynamically registered).

### 3.4 Server to client

Only from the verified set `[R1]`:

| Method | Used for |
|---|---|
| `workspace/diagnostic/refresh` | Background analysis produced new findings — client re-pulls. |
| `workspace/codeLens/refresh` | Background work changed available affordances. |
| `$/progress` | Streaming and long-running status, under a token from §3.5. |
| `window/workDoneProgress/create` | Only for progress the server starts with no request to attach to. |
| `window/showMessageRequest` | Approval prompt with a pick-list `[R2]`. |
| `workspace/applyEdit` | Applying an approved plan step without a pick. |
| `window/showDocument` | Opening a plan or explanation artifact as a buffer. |
| `client/registerCapability` | `workspace/didChangeWatchedFiles` watchers. |
| `textDocument/publishDiagnostics` | Only for findings the plugin has explicitly requested as push (edits made *by* the server). |

### 3.4.1 `codeLens` — and who runs its command

Lenses are returned fully formed: one per declaration at the left margin, `resolveProvider` is
false, so a document costs one request and no lens costs a request of its own. The title is
deterministic and never model output — `meta: explain` for a clean declaration, `meta: N
finding(s) · fix` for one with cached findings, where "cached" is the same cache the sign
column reads.

The `command.command` of every lens is in the reserved namespace **`meta.plugin.`**, which the
plugin handles in-process and never sends to the server:

| Command | What the client does |
|---|---|
| `meta.plugin.explain` | the plugin's own explain flow, at the cursor the lens was run from |
| `meta.plugin.pick` | the plugin's action picker, same flow as `<leader>ma` |

The reason is §7: **the server never learns a buffer exists.** An explanation has no URI, so
`window/showDocument` cannot open it, and a lens that is rendered but cannot do anything is
worse than no lens. `vim.lsp.codelens.run()` re-requests and then sends the command to the
server, so the plugin wraps `run` and dispatches its own namespace locally; every other
command passes through untouched `[R14]`.

### 3.4.2 `inlayHint` — the badge, and why it is off

One hint per declaration that has **cached findings**, at the end of the declaration's head
line: `meta: N finding(s)`, with the labels in the tooltip. Nothing anywhere else. Silence is
the default rather than a state to be reported — a hint reading "clean" on every function in a
file would be the most intrusive surface in the editor, and hints sit inside the text where
they cannot be skimmed past.

- `resolveProvider: false`; the label and the tooltip are complete on arrival.
- The character is a **byte offset** into that line, because this server speaks utf-8 (N1).
- Gated exactly as the analysis is: `enabled`, and the same binary/size/ignore refusals. An
  affordance for work the server would refuse is worse than none.
- `workspace/inlayHint/refresh` after an analysis, so a badge appears and disappears with the
  findings it counts.

The client decides whether to draw any of this, and the plugin keeps it **off** by default for
a reason of Neovim's rather than a matter of taste: `vim.lsp.inlay_hint.enable` switches hints
on **per buffer, not per client**, so turning it on for this badge turns on every other
server's hints in that buffer too. `<leader>Mh` toggles it for the buffer, and
`:Meta hints on|off` does the same.

### 3.5 Progress tokens — the two legal sources

The server MUST NOT send `$/progress` for a token it did not receive or create `[R12]`.
A token is obtained in exactly one of two ways:

1. **Client-initiated (the normal path).** The plugin puts `workDoneToken` in the
   `workspace/executeCommand` params — `ExecuteCommandParams extends WorkDoneProgressParams`,
   so the field is part of the request, not a smuggled argument. No create request is made
   for that token. It is valid only until the response to that request is sent `[R12]`.

   ```jsonc
   // plugin -> server
   { "command": "meta.plan",
     "arguments": [ { "goal": "make retry cancellable" } ],
     "workDoneToken": "meta:8f3c1d" }
   ```

2. **Server-initiated.** For work with no client request to attach to (a background
   re-analysis), the server sends `window/workDoneProgress/create` and **waits for the
   response** before any `$/progress` for that token. Permitted only when the client
   advertises `window.workDoneProgress` (Neovim does `[R11]`). If the create request fails
   or errors, the server MUST NOT send progress with that token `[R12]`.

Rules that follow, and are enforced by the streaming module:

- Each token is used once: one `begin`, zero or more `report`, one `end`.
- Exactly one `end` is sent on every path, including model error, budget refusal,
  cancellation, and panic. A token left open is a defect, not a leak to tolerate.
- `report` carries `message` and, only when genuinely monotonic, `percentage`.
- The plugin ignores `$/progress` for tokens it did not issue. Neovim's own handler
  tolerates unknown tokens and routes them all to `LspProgress` `[R9]` — that tolerance is
  not permission, and the design does not rely on it.

Path 1 is verified end to end against NVIM v0.12.5: `verify/probes/streaming.lua` drives a
stub stdio server and asserts that the client-supplied token arrives in the request params
and that `begin,report,end` arrives under it `[R12]`.

#### 3.5.1 Partial results — the answer as it is written

A prose answer is reported while it is being generated, under the same token, as `report`
values that carry the text so far in `data`:

```jsonc
// server -> client
{ "token": "meta:8f3c1d",
  "value": { "kind": "report",
             "message": "meta: explaining — 412 bytes",
             "data": { "schema": "meta.artifact/1",
                       "kind": "explanation",
                       "partial": true,
                       "markdown": "# Summary\n\n…" } } }
```

Rules:

- **Cumulative, never deltas.** `data.markdown` is the whole answer so far. A lost, repeated,
  or out-of-order report therefore cannot corrupt what the user is reading, and the client
  writes by replacing, with no bookkeeping `[R13]`.
- **`message` stays short and human.** Neovim's own progress UI renders it; the text travels
  in `data`, which only the plugin reads.
- **The response is authoritative.** The artifact in the response is the answer; a report is a
  preview of it. A repair attempt means the preview may have shown a rejected first attempt —
  which is why an *edit* is never streamed: half a JSON object is not a preview, and a
  streamed edit is one nobody can stop.
- **Silence is a defect.** Before the first token the server reports `waiting` on a slow
  cadence, because the prefill and the reasoning phase are seconds long and an empty window
  is indistinguishable from a hang. Measured: 4.6 s to a complete answer, of which the first
  3 were reported as waiting.
- **Only for a token the client issued** — §3.5 path 1, unchanged. Nothing is created.

The client must tolerate `data` being absent: a report without it is ordinary progress and is
rendered as a message.

---

## 4. Code action taxonomy

Every action carries `data` (round-tripped, `dataSupport = true` `[R4]`):

```jsonc
{
  "v": 1,
  "id": "9f2c…",                 // stable: hash(verb, uri, range, content_hash)
  "verb": "harden",              // §4.1
  "state": "ready",              // ready | pending | stale | over_budget | failed
  "doc": { "uri": "file:///…", "version": 42, "content_hash": "sha256:…" },
  "scope": { "kind": "function", "name": "parse", "range": {...} },
  "language": "rust",            // metadata, never a filter (N10/N11); "unknown" is valid
  "scope_source": "tree",        // tree | structural | whole_file | explicit
  "finding": "…"                 // optional: id of the diagnostic this fixes
}
```

`titles are deterministic`: `title = f(verb, scope.name, finding.label)`. Two invocations
on unchanged content must produce byte-identical titles. No model-generated titles on the
fast path; a model-supplied summary is attached as a `data.summary` for the picker's
preview pane, never as the label.

### 4.1 Verbs

| Verb | Kind | Produces | Auto-apply? |
|---|---|---|---|
| `fix` | `quickfix.meta` | Edit | yes, when config allows and a finding id is present |
| `harden` | `refactor.rewrite.meta` | Edit | no |
| `types` | `refactor.rewrite.meta` | Edit | no |
| `docs` | `refactor.rewrite.meta` | Edit | no |
| `rewrite` | `refactor.rewrite.meta` | Edit | no |
| `test` | `refactor.rewrite.meta` | Edit incl. `create` resource operation | no |
| `explain` | — | Artifact, via the `meta.explain` command (§6) | n/a |
| `review` | `source.meta` | Findings + refresh | n/a |
| `generate` | `refactor.rewrite.meta` | Edit at cursor | no |
| `fixAll` | `source.fixAll` | Edits for every `ready` finding in scope | yes, when config allows |

`explain` and `review` still arrive through the code action menu; they resolve to a
`command` that opens a buffer. One entry point for every intent.

---

## 5. Cost gates

Ordered, all mandatory, all evaluated before any model call:

1. **Dedupe** — `sha256(model_tier, prompt_template_version, context_hash, verb)`; hit
   returns the cached conclusion.
2. **Debounce** — inline completion: 200 ms client-side `[R7]` plus a server-side idle
   floor (`inline_completion.idle_ms`, default 400 ms). Diagnostics: `on_save` or
   `on_idle_ms` (default 1500), never per keystroke.
3. **Prefix floor** — inline completion requires ≥ 8 non-whitespace characters before the
   cursor.
4. **Budgets** — `max_calls_per_min`, `max_calls_per_hour`, `max_tokens_per_session`.
   Exhaustion is not an error: the server returns `state = "over_budget"` actions and a
   single `window/showMessage` on first exhaustion, then stays silent.
5. **Kill switch** — `:Meta stop` (plugin) sets `enabled = false` through
   `workspace/configuration` re-read; the server stops issuing model calls immediately and
   drains in-flight work.
6. **Cancellation** — `$/cancelRequest` aborts the model call via an `AbortSignal`-style
   token threaded through `meta-core`.

---

## 6. Commands

| Command | Arguments | Returns | Served |
|---|---|---|---|
| `meta.status` | `{}` | `Result` with queue, budgets, cache counters, in-flight calls | yes |
| `meta.recompute` | `{}` | `Result` | yes |
| `meta.explain` | `{uri, line}` | `Artifact` (§7, `kind: "explanation"`) | yes |
| `meta.cancel` | `{progress_token}` | `Result` | yes |
| `meta.plan` | `{goal, scope}` | `Artifact` | yes |
| `meta.apply` | `{plan_id, steps: [n]}` | `Result` | yes |
| `meta.revert` | `{edit_id}` | `Result` | yes |

Sent as `workspace/executeCommand`. Only the served commands are advertised in
`executeCommandProvider.commands` (§2): a client should not be told about a command that can
only answer "not implemented". Any command that is not served — an unknown name, or one a
future version adds before it is implemented — still answers with a structured
`{ok: false, error: {code: "not_implemented"}}` rather than failing silently.

**A plan step is applied by the server, not the client.** `meta.apply` resolves the step
against the content the server currently holds, builds the edit, and sends
`workspace/applyEdit` back to the client — so a step that has become stale is refused before
anything is written (it answers `{"code": "stale"}` in the `failed` list). `meta.revert`
restores the bytes recorded before the edit and then forgets the id, so a second revert is
`unknown_edit` rather than a silent no-op.

No command takes a token argument: progress is reported under the `workDoneToken` of the
enclosing request (§3.5), and cancelling a request is `$/cancelRequest` against the id the
plugin's send call returned. `meta.cancel` exists only for work the plugin did not issue.

### 6.1 Error codes

`error.code` is contract, not prose: the CLI maps it to an exit code and the plugin shows it
verbatim, so it is named here and asserted by the harness. Every command answers with a
`Result` envelope (§7) whether it succeeded or not.

| Code | Meaning |
|---|---|
| `bad_arguments` | The command's arguments are missing or malformed; the message names the shape it needs |
| `unknown_document` | No open document matches the `uri` given |
| `unknown_plan` | The plan id is not one this session holds (never persisted — N9) |
| `unknown_edit` | Nothing to revert for that `edit_id`; a second revert is this, not a no-op |
| `bad_uri` | The document's uri cannot be parsed |
| `rejected_by_client` | `workspace/applyEdit` was refused by the client; its reason is passed through |
| `apply_failed` | The apply request itself failed |
| `not_implemented` | The command is not served at all — asserted against a name no version serves |
| `skipped` | A gate or a disabled feature refused the work; the message names which |
| `over_budget` | A call or token budget is exhausted (the CLI maps this to exit 3) |
| `stale` | The target moved or changed since the work was prepared (exit 4) |
| `model_error` | The model call failed, including a timeout or an exhausted answer |
| `contract_error` | The model's answer did not satisfy its contract after repair (`docs/MODEL.md` §5) |
| `rejected_edit` | The answer could not be applied to this document — an anchor that does not locate, or a replacement that repeats lines it did not consume |

Server-initiated progress is cancelled by the client with `window/workDoneProgress/cancel`
(client→server notification, `WorkDoneProgressCancelParams {token}`), which the server must
handle by aborting the corresponding job — a job that keeps running after its progress is
cancelled is a defect. The progress need not have been marked `cancellable` `[R12]`.

**Why `explain` is a command and not a code action.** A resolved code action's `command`
field is executed by the client by sending it *back to the server* as
`workspace/executeCommand` (`vim/lsp/buf.lua:1252-1258`, verified). A server therefore
cannot use that field to make the client open a buffer, and an artifact has nowhere else to
go. So the artifact is *pulled* by the plugin instead: `:Meta explain` calls
`meta.explain` and renders the returned Markdown itself.

---

## 7. Artifacts

Stable, versioned, self-describing, plain JSON on one line per record where streamed.

```jsonc
// Artifact
{ "schema": "meta.artifact/1", "kind": "plan" | "explanation" | "review",
  "id": "…", "created": "2026-09-18T12:00:00Z",
  "goal": "…",
  "language": "rust",            // resolved language for the primary target (N11)
  "steps": [
    { "n": 1, "title": "…", "rationale": "…", "verb": "harden",
      "targets": [{ "uri": "…", "version": 42, "range": {…} }],
      "status": "proposed" | "applied" | "rejected" | "failed",
      "edit_id": "…" }            // present once applied
  ],
  "usage": { "model": "…", "tier": "reason", "tokens_in": 0, "tokens_out": 0, "ms": 0 }
}

// Result — the envelope for every command
{ "schema": "meta.result/1", "ok": true,
  "artifacts": ["…"], "diagnostics": [ … ], "edit_ids": ["…"],
  "error": { "code": "…", "message": "…" },   // present iff ok == false
  "usage": { … } }
```

`steps[].targets[].version` is the version the plan was computed against. The plugin
compares it to the live buffer before applying; a mismatch marks the step `stale` and
offers recomputation instead of applying.

---

## 8. Edit contract

Frozen. All four rules are enforced by a validator with a self-test that reproduces the
`[R3]` probe table.

1. `documentChanges` form only. The `changes` map is rejected.
2. `textDocument.version` is a required integer on every `TextDocumentEdit`.
   `null` is permitted **only** for buffers the server itself authored (plan/explanation
   scratch buffers), where staleness is benign.
3. Every edit is generated against a recorded `(uri, version, content_hash)`. Before
   returning, the server re-reads the document version; if it moved, the response carries
   no `edit` and the action is re-marked `stale`.
4. Edits are expressed as whole-file or whole-symbol replacement text. The server computes
   ranges. Overlapping edits within one `TextDocumentEdit` are forbidden.

Multi-file edits (for example `test`, which creates a file plus edits the source) use a
single `WorkspaceEdit` with `resourceOperations: ["create", "rename", "delete"]`, all three
advertised by the client `[R11]`.

Verification after apply is not optional: the server re-hashes each touched document and,
when the hash does not match its prediction, publishes a diagnostic naming the divergence.

This section is verified as a whole exchange against the real client by
`verify/probes/trace.lua`: an edit stamped with the version the action was created against
is applied when the document has not moved, and is refused by the client when it has.

---

## 9. Diagnostics

- Source name: `meta`. One namespace, own `resultId` per document.
- Two severities are permitted: `INFORMATION` for observations, `WARNING` for findings.
  `ERROR` is reserved for a divergence the server can prove §8 violated.
- Every finding carries `data = { finding_id, verb, content_hash }` so `quickfix.meta`
  actions can be keyed to it `[R4]`.
- Findings must be dismissible (`:Meta dismiss <finding_id>`), and a dismissal is recorded
  in a per-repository file so it does not resurface.
- Publish is reserved for changes the server made; otherwise findings are served by pull,
  refreshed by `workspace/diagnostic/refresh` `[R5]`.

---

## 10. Configuration

Read through `workspace/configuration` under one section, `meta`. The server asks; the
plugin supplies from `vim.lsp.config('meta')`. No configuration file of our own, no
environment variables beyond the model endpoints.

```jsonc
{ "enabled": true,
  "models": { "fim": {…}, "reason": {…}, "review": {…} },   // see docs/MODEL.md
  "budget": { "max_calls_per_min": 6, "max_calls_per_hour": 120,
              "max_tokens_per_session": 500000, "timeout_ms": 30000 },
  "triggers": { "diagnostics": "save", "idle_ms": 1500, "severity_floor": "information" },
  "inline_completion": { "enabled": false, "idle_ms": 400, "min_prefix_chars": 8,
                         "max_calls_per_min": 4 },
  "ambient": { "code_lens": true, "inlay_hints": false, "diagnostics": true },
  "auto_apply": { "fix": false, "fixAll": false },
  "verbs": { "explain": true, "review": true, "test": true, "generate": true },
  "languages": {                    // docs/LANGUAGE.md §7 — may narrow, never disable
    "overrides": { "rust": { "model": "reason", "prompt": "rust" } },
    "generic": { "prompt": "generic_text", "model": "reason" },
    "max_file_bytes": 1048576,
    "ignore": ["**/node_modules/**", "**/*.min.js"]
  },
  "noise": { "max_visible_findings": 5, "suppress_after_dismissals": 2 },
  "log": "warn" }
```

Defaults are conservative: `inline_completion.enabled = false`, `auto_apply` off,
`inlay_hints` off.

---

## 11. CLI contract

`meta` is a thin sync client of `meta-core`, no daemon required, no state.

```
meta explain <path>[:<line>[:<col>]]        # artifact to stdout
meta review <path>                          # findings, JSON
meta action --verb <verb> <path>[:<range>]  # proposed edit, JSON (never applied)
meta plan --goal <text> <path>              # plan artifact
meta status                                 # budget and queue
```

- stdin: additional context (diff, buffer text) when the path is `-`.
- stdout: exactly one artifact or result, JSON, one line, no decoration.
- stderr: diagnostics, including the per-request cost line.
- No interactive mode, no prompts, no colour, no markdown. Exit codes are the only
  channel besides stdout.

| Exit | Meaning |
|---|---|
| 0 | Success, artifact on stdout |
| 1 | Transport or model failure |
| 2 | Usage error or contract violation (bad verb, unparsable range) |
| 3 | Budget exhausted |
| 4 | Stale target — the document changed since the request was built |

The LSP server is not required for the CLI, and the CLI is not required for the LSP
server. Both call `meta-core`.

---

## 12. Explicitly refused

Recorded so the refusals are not relitigated:

- **A chat buffer as the primary interface.** Free text has no place to go; the menu, the
  lens, and the finding are the interface. A prompt box exists only as the input to
  `meta.plan`.
- **Model-generated action titles on the fast path.** Non-deterministic labels destroy
  muscle memory.
- **Server-side session or conversation memory.** The plan artifact is the continuation.
- **`willSaveWaitUntil` by default.** Blocking writes on a model call.
- **Auto-apply beyond `fixAll`/`fix` with explicit opt-in.**
- **A filetype or language allowlist as the attachment mechanism.** Every comparable
  project gates support this way (`docs/research/prior-art.md` §4); it contradicts N10.
- **Gating the verb set on a treesitter parser or on a resolution result.** The model needs
  neither; absence of a parser changes scope quality, not availability.
- **Silent skips.** A buffer that is attached but not analysed must report why
  (`over_size`, `binary`, `ignored`, `generic_scope`).
- **Telemetry.**
- **A second source of truth for document state.** The client owns text; the server's
  cache is keyed by content hash and evictable at any time.

---

## Changelog

| Date | Change |
|---|---|
| 2026-09-18 | Initial freeze. Probe-verified against NVIM v0.12.5. |
| 2026-09-18 | **N10/N11 added**: support is unconditional and language is metadata. `language` and `scope_source` added to action `data` and to plan artifacts; `languages` config section added; filetype allowlists and grammar-gated verbs explicitly refused. Verified by `verify/probes/language.lua` — with `filetypes = nil` the client attaches 8 of 11 fixtures, leaving the unidentifiable ones to the plugin's attach pass. |
| 2026-09-18 | **§2 rewritten around "advertise only what is served"**, with the advertised set reduced to the implemented providers. `explain` moved out of the code-action menu to the `meta.explain` command, because a resolved action's `command` is executed by the client by sending it back to the server, so it cannot open a buffer. `meta.recompute` added; `meta.plan`/`apply`/`revert` marked specified-but-unserved and unadvertised. Implementation: `crates/meta-lsp`. |
| 2026-09-18 | **U5, U6 and U8 built**: `meta.plan`/`meta.apply`/`meta.revert` are served and advertised; a plan step is applied *by the server* through `workspace/applyEdit`, re-anchored against live content and refused as `stale` when the target moved. Multi-file edits (`resourceOperations: create`) verified end to end. Post-apply verification implemented: the server predicts what an applied edit will produce and publishes an `ERROR` diagnostic naming the divergence — the one legitimate use of `publishDiagnostics` (§9). The one-shot CLI (`crates/meta`) implements §11 with all five exit codes, and `verify/cli_parity.py` proves it produces identical findings and byte-identical edits to the LSP path. |
| 2026-09-18 | **`workspaceDiagnostics` corrected from `true` to `false`.** It had been advertised since the first capability block while `workspace/diagnostic` was never implemented, and Neovim prefers the workspace branch on refresh when the flag is set — so the client stopped pulling per-document diagnostics entirely and findings never appeared. Fixed by not advertising what is not served, which is this document's own §2 rule. |
| 2026-09-18 | **§6.1 added: the command error codes are named.** They were already contract in practice — the CLI maps them to exit codes and the plugin surfaces them verbatim — but only `not_implemented` and `unknown_edit` were written down, so a harness asserting the real codes could be broken by a rename the document never mentioned. |
| 2026-09-18 | **Resolve timeout raised 30 s → 90 s, ceilings raised, repair widened.** Measured against a real reasoning model: an 8192-token answer took over 60 s on a Rust rewrite, so the old 30 s cap converted valid-but-slow answers into transport errors; and a rejected answer is often answered the same way again, so the repair budget went from one attempt to two. `MAX_REPAIR_ATTEMPTS` now covers *applicability* failures as well as JSON ones — an anchor that cannot be located, or a replacement that repeats lines it did not consume — which docs/MODEL.md §5 specified and the implementation had not. Over a nine-run soak on three languages: 8 applied, 0 left a file unparseable, against 2/6 and 8/12 before. |
| 2026-09-18 | **U7 built.** Inline completion is served and advertised: `textDocument/inlineCompletion` is registered as a custom method (the pinned `lsp-types` has no handler for a 3.18-draft method) and `inlineCompletionProvider` is injected into the `initialize` response, because Neovim attaches its completor only for a client that advertises it. Gates: off by default, binary/size/ignore, a content-hash answer cache, a prefix floor that applies only to timed requests, and its own per-minute window so completions cannot starve explicit work. Also implemented `workspace/didChangeConfiguration`, without which the plugin's kill switch could never take effect. |
