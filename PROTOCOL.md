# PROTOCOL.md — frozen contract

Frozen. Changes require editing this file first, with a dated changelog entry. Every
constraint here is backed by a probe in `docs/research/nvim-lsp-surface.md`; the label
`[R#]` refers to that document's section numbers.

Scope: the LSP surface between `crates/jev-lsp` and any LSP client; the artifact schemas;
the CLI contract. Internal design is not frozen here.

Name: workspace and binary are `jev`, crates `jev-core` / `jev-lsp` / `jev`, plugin
`nvim/`. The repository directory is `jev-lsp`. Reversible by mechanical sweep.

---

## 1. Non-negotiables

| # | Rule | Why |
|---|---|---|
| N1 | Declare `positionEncoding: "utf-8"`. All ranges are byte offsets. | Client honours the server's choice `[R6]`. Byte offsets equal Rust `&str` indices and treesitter byte columns. |
| N2 | The `codeAction` request **never** waits on a model call. It serves cached results or returns no actions. | The client awaits every client before showing the menu; a slow response is a frozen editor. |
| N3 | Expensive work happens in `codeAction/resolve` or in the background worker. | Resolve is invoked only for the picked action `[R4]`. |
| N4 | Every `WorkspaceEdit` uses `documentChanges` with an explicit integer `version` on each `TextDocumentEdit`. | Bare `changes` skips the version check; an absent `version` raises inside the client `[R3]`. |
| N5 | The model never emits LSP ranges. | Ranges are encoding- and version-sensitive; the server derives them from treesitter scope plus byte offsets. |
| N6 | No custom `jev/…` LSP methods. | The standard surface plus the in-process Lua plugin covers every requirement `[R1]`; custom methods buy nothing and cost portability. |
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
      "quickfix", "quickfix.jev",
      "refactor.rewrite", "refactor.rewrite.jev",
      "source", "source.jev", "source.fixAll"
    ]
  },
  "diagnosticProvider": { "identifier": "jev", "interFileDependencies": false, "workspaceDiagnostics": false },
  "hoverProvider": true,
  "codeLensProvider": { "resolveProvider": false },
  "inlayHintProvider": { "resolveProvider": false },
  "executeCommandProvider": { "commands": [ /* §6 */ ], "workDoneProgress": true }
}
```

`hoverProvider` is `true` because `textDocument/hover` is served (§3.2) — it answers from the
artifact store and never from a model. It had been missing from this block while §3.2 listed the
method, which is exactly what the completeness claim below is for: the block was read field by
field against a live `initialize` response on 2026-09-20 and it is eight fields, no more and no
fewer.

`workspaceDiagnostics` is `false` and must stay false until `workspace/diagnostic` is
implemented here. Neovim's `on_refresh` checks that capability *first* and takes the
**workspace** branch when it is set, so advertising it while serving only per-document pull
means every `workspace/diagnostic/refresh` is answered by a method that does not exist: the
server caches findings and nothing ever reaches the sign column. Measured, and fixed — see
`docs/VERIFICATION.md` §8.

`inlineCompletionProvider` is **not** advertised: inline completion was removed from the
server on 2026-09-19 (see the log at the end of this file), so the draft capability, its
custom method and the transport-boundary injection that carried it are gone with it.

**Rule: advertise only what is served.** A provider the server does not implement is a lie
the client will act on — it will send requests that can only fail, or set up UI for progress
that never reports. The block above is therefore the *complete* advertised set, and it grows
one entry at a time as features land. `codeLensProvider` and `inlayHintProvider` are advertised
because both are served (§3.4.1, §3.4.2); only their `resolve` halves are not, which is what the
`resolveProvider: false` in the block above says.

Standard kinds only, with a `.jev` suffix where the origin matters. Kind filtering in the
client is prefix-based on `.` `[R4]`, so `refactor.rewrite` also matches
`refactor.rewrite.jev`, while `only = ["refactor.rewrite.jev"]` selects just ours.

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

Two kinds of row: what the server *serves* today, and what is designed for it. The three
marked † are not implemented and therefore **not advertised** (§2) — a client will never be
asked for them, and the budgets are the design they will be held to.

| Method | Class | Gate | Notes |
|---|---|---|---|
| `textDocument/diagnostic` | p99 < 30 ms | file-size/ignore/binary gates, finding cap, cache | Serves from the conclusion cache; never triggers a model call inline. `resultId` for incremental re-pull. |
| `workspace/diagnostic` † | p99 < 200 ms | same | Whole-repo pull; serves cached findings only. `workspaceDiagnostics: false` until it exists. |
| `textDocument/codeLens` | p99 < 30 ms | per-symbol count cap | Affordances offered per symbol. |
| `codeLens/resolve` † | p99 < 50 ms | — | Fills the `command`. Lenses are returned complete, so `resolveProvider: false`. |
| `textDocument/inlayHint` | p99 < 30 ms | default **off** except risk markers | Cached only. |
| `inlayHint/resolve` † | p99 < 50 ms | — | Hints are returned complete, so `resolveProvider: false`. |
| `textDocument/hover` | p99 < 100 ms | on-demand prefetch | Cached explanation. Miss returns the plain signature immediately and warms the cache. |

### 3.3 Client to server, control

`workspace/executeCommand` (§6) and `workspace/didChangeConfiguration` — how the kill switch
takes effect (PROTOCOL §5). Nothing else: in particular there is no
`workspace/didChangeWatchedFiles` and no dynamic registration, because the repository's changed
set is asked of git when a pass needs it (§5) rather than watched for.

### 3.4 Server to client

Only from the verified set `[R1]`:

The last three are designed and **not sent** by this implementation; they are listed so the
design is visible, not as a claim of coverage.

| Method | Used for | Sent |
|---|---|---|
| `workspace/configuration` | Reading the `jev` section of the client's settings (§10). A request the client answers. | yes |
| `workspace/diagnostic/refresh` | Background analysis produced new findings — client re-pulls. | yes |
| `workspace/codeLens/refresh` | Background work changed available affordances. | yes |
| `$/progress` | Streaming and long-running status, under a token from §3.5. | yes |
| `workspace/applyEdit` | Applying an approved plan step without a pick. | yes |
| `window/showDocument` | Opening a plan or explanation artifact as a buffer. | yes |
| `window/showMessage` | The failures a user must be told about once: the model unreachable, a budget first exhausted, a post-apply divergence (§4, §8). Never on a normal edit. | yes |
| `textDocument/publishDiagnostics` | Only for findings the plugin has explicitly requested as push (edits made *by* the server). | yes |
| `window/workDoneProgress/create` | Progress the server starts with no request to attach to. | no — every token comes from the client (§3.5 path 1), so none is ever created |
| `window/showMessageRequest` | Approval prompt with a pick-list `[R2]`. | no — the decision is the client's own picker (`vim.ui.select`) |
| `client/registerCapability` | `workspace/didChangeWatchedFiles` watchers. | no — no watchers are registered at all, statically or dynamically: the changed set is asked of git when a pass needs it (§5) |

### 3.4.1 `codeLens` — and who runs its command

Lenses are returned fully formed: one per declaration at the left margin, `resolveProvider` is
false, so a document costs one request and no lens costs a request of its own. The title is
deterministic and never model output — `jev: explain` for a clean declaration, `jev: N
finding(s) · fix` for one with cached findings, where "cached" is the same cache the sign
column reads.

The `command.command` of every lens is in the reserved namespace **`jev.plugin.`**, which the
plugin handles in-process and never sends to the server:

| Command | What the client does |
|---|---|
| `jev.plugin.explain` | the plugin's own explain flow, at the cursor the lens was run from |
| `jev.plugin.pick` | the plugin's action picker, same flow as `<leader>ja` |

The reason is §7: **the server never learns a buffer exists.** An explanation has no URI, so
`window/showDocument` cannot open it, and a lens that is rendered but cannot do anything is
worse than no lens. `vim.lsp.codelens.run()` re-requests and then sends the command to the
server, so the plugin wraps `run` and dispatches its own namespace locally; every other
command passes through untouched `[R14]`.

**A client that is not the plugin must bridge these two ids, and what the server answers without
one is stated rather than hidden.** The namespace names who runs the command, and as the contract
stands only the plugin does: a client that is not the plugin runs the lens through its own stock
path, which sends `workspace/executeCommand` with `jev.plugin.pick` (or `jev.plugin.explain`) to
the server, and the server answers `{"ok": false, "error": {"code": "not_implemented", "message":
"jev.plugin.pick is not implemented in this version"}}` — measured over the wire, not inferred. So
the bridge is the client's own: `editors/cursor/extension.js` registers both ids and does the
work, and `docs/CURSOR.md` §6 records the trap. Serving the picker from a command id of the
server's own would remove the bridge, and whether the ids change is open (`STATUS.md`, open
questions); this section states the mechanism as it stands and does not settle it.

### 3.4.3 `jev.document` — what the client knows about a document

The server has no parser, by design (`LANGUAGE.md` §4): its declaration scan is structural, and
in C, C++, Java and C# — languages that declare a function by *shape* rather than by keyword —
it finds no functions at all. The client is the side with parsers, so it sends what treesitter
found:

```jsonc
// plugin -> server, on attach, on change (debounced), on save, and on FileType
{ "command": "jev.document",
  "arguments": [ { "uri": "file:///…/statusline.lua", "version": 7,
                   "definitions": [ { "start_line": 45, "end_line": 51 },
                                    { "start_line": 114, "end_line": 128 } ],
                   "context":     [ { "kind": "imports", "uri": "…", "text": "…" },
                                    { "kind": "sibling", "uri": "…", "text": "…" } ] } ] }
```

Two payloads, one lifecycle: both describe the document at one version, and one guard covers the
pair. `definitions` answer the surfaces that enumerate (§3.4.1, §3.4.2). `context` is the
standing set: what the editor can see for this document, pushed once per change rather than
assembled per request. Its one consumer was the completion — the only path that could not
afford a round trip to another language server — and that was removed on 2026-09-19, so the
server stores this set and reads it for nothing. It stays in the contract because the client
pushes it and a stale client must not be rejected; the next consumer of it is a change to this
document, not a silent one. Context assembled *per request* (§6.1) goes on the generating
commands, which is why references appear there and not here.

Rules:

- **Version-stamped.** The server uses the set only while it describes the document version the
  client is editing. A set from an older version is ignored, and the structural scan answers
  instead — so a missed push costs accuracy and never correctness. A lens pointing at whatever
  now occupies those lines would be worse than no lens.
- **It replaces the scan, it does not merge with it.** The client's set has to be a superset of
  what the scan would have found, which is why the plugin's node table lists type declarations
  beside functions. Two sources merged per request would be two sources of truth.
- **Silence means the scan.** A client without a parser for the language sends nothing, and
  gets exactly the behaviour described in §3.4.1 and §3.4.2.
- **Not in the session record.** This one is the client telling the server what it already
  knows, sent on every change; recording it would bury the work the record exists to show.

Measured: on a 249-line Lua file the scan found 12 of 12 after a keyword fix and the parser
found the same 12; on a C file the scan finds the struct and **no functions**, and the parser's
set gives all three declarations.

### 3.4.2 `inlayHint` — the badge, and why it is off

One hint per declaration that has **cached findings**, at the end of the declaration's head
line: `jev: N finding(s)`, with the labels in the tooltip. Nothing anywhere else. Silence is
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
server's hints in that buffer too. `:Jev hints on|off` toggles it for the buffer.

### 3.4.4 Project context — what the editor sends with a request

The server has no parser, no other language servers, and no idea what the user has been
reading. The client has all three, so it may attach what it found to any request that
generates:

```jsonc
{ "command": "jev.explain",
  "arguments": [ { "uri": "…", "line": 40,
    "context": [ { "kind": "imports",   "uri": "…", "start_line": 0, "end_line": 12, "text": "…" },
                 { "kind": "reference", "uri": "…", "start_line": 88, "end_line": 92, "text": "…" },
                 { "kind": "test",      "uri": "…", "text": "…" },
                 { "kind": "sibling",   "uri": "…", "text": "…" } ] } ] }
```

Rules, each of which exists because of a specific way this could go wrong:

- **Bounded here, not trusted.** At most four documents, forty lines each, enforced by the
  server: an over-eager client cannot flood the prompt.
- **Ordered by kind, then by uri.** The client's order within a kind is kept; across kinds it
  is not. The same project state must produce the same prompt, or the cache is a coin toss.
- **In the cache key.** What the client sent is hashed into it. Two requests differing only in
  their context are two different questions, and answering the second from the first's cache
  entry is the failure mode that would make everything here a lie.
- **Text travels in the request.** Nothing is read from disk by the server: no index, no
  watcher, and the server hashes exactly what it was given.
- **Not on the fast paths.** Context goes with the request that *generates* — the explain, the
  follow-up, the `codeAction/resolve` — never with `textDocument/codeAction`, which answers
  from cache and must stay in single-digit milliseconds (N2, N3). A resolve may take 300 ms to
  ask another language server for references; a menu must not.

Kinds are the client's words; the server renders them and orders them. `imports`, `reference`,
`test` and `sibling` are what the plugin sends today, and an unknown kind is rendered as
`context` rather than refused.

### 3.4.5 `hover` — what has already been said about a scope

Hover shows the answer to a question the user has already asked. `jev.explain` and
`jev.followup` store what they produced, keyed by document and scope, and hover returns it:

- **No model call, ever.** A hover is a keystroke's gesture; one that waits ten seconds is one
  nobody uses. `resolveProvider` is therefore not advertised — the contents are complete.
- **Covering, not equal.** Any stored artifact whose extent contains the hovered line answers
  it. Requiring the two sides to agree on an exact extent would make hover work only when a
  parser and a scan resolve a declaration the same way, which is not the same question.
- **The content hash still has to match.** A stale explanation shown against lines it was not
  written about is worse than none.
- **Silence when there is nothing.** An empty result, not an error: the client renders no hover
  rather than a failed one.

The store holds sixty-four artifacts, oldest out.

### 3.5 Progress tokens — the two legal sources

The server MUST NOT send `$/progress` for a token it did not receive or create `[R12]`.
A token is obtained in exactly one of two ways:

1. **Client-initiated (the normal path).** The plugin puts `workDoneToken` in the
   `workspace/executeCommand` params — `ExecuteCommandParams extends WorkDoneProgressParams`,
   so the field is part of the request, not a smuggled argument. No create request is made
   for that token. It is valid only until the response to that request is sent `[R12]`.

   ```jsonc
   // plugin -> server
   { "command": "jev.plan",
     "arguments": [ { "goal": "make retry cancellable" } ],
     "workDoneToken": "jev:8f3c1d" }
   ```

2. **Server-initiated.** For work with no client request to attach to (a background
   re-analysis), the design is that the server sends `window/workDoneProgress/create` and
   **waits for the response** before any `$/progress` for that token, permitted only when the
   client advertises `window.workDoneProgress` (Neovim does `[R11]`), and that a create request
   which fails or errors means the server MUST NOT send progress with that token `[R12]`.
   **No code path does this today**: nothing in `crates/` calls
   `window/workDoneProgress/create`, so every token in this contract is the client's own
   (§3.4's table says the same). The rules below hold for a token of either origin, which is why
   they are written now and not when the second origin exists.

Rules that follow, and are enforced by the streaming module:

- Each token is used once: one `begin`, zero or more `report`, one `end`.
- Exactly one `end` is sent on every path the server survives. Verified over the wire for
  **model error** (the failure is a `Result` envelope, not an early return, so the send on the way
  out still runs), **budget refusal** (the same), and **cancellation** (the command's task is
  aborted and its token guard closes the token; the client sees `-32800 Canceled`). All three are
  asserted by `verify/lsp_client.py` step 10 — one `begin`, one `end`, in that order, and nothing
  under the token after the `end`, read after a settle window. **The order of the `end` and the
  response is not asserted, and is not the command's to fix.** The server keeps its side of the
  rule in its own order — the `end` is handed to the transport before the command body returns,
  so before the response exists — but the two are written to stdout through two arms of one
  `futures::stream::select` in `tower-lsp` (`transport.rs`), which polls them round-robin, so when
  both are ready in the same poll the winner is the scheduler's. Measured on one unchanged
  binary, ±100 µs either way: green at `end 207.431844719, response 207.431873793`, red at
  `end 157.797434027, response 157.797412763`. A token left open is a defect, not a leak to
  tolerate; a token closed a few microseconds after the answer is the transport.
- **Cancellation is honoured, and the `begin` says so.** Every command's `begin` carries
  `cancellable: true` (`crates/jev-lsp/src/server.rs`): the command body runs in its own task, so
  `$/cancelRequest` aborts it — the request answers `-32800 Canceled`, the token is closed exactly
  once, and the answer is discarded. Unconditional, because `begin` is only ever sent from inside
  the spawned body: by the time a client can see the flag and act on it, there is a task to abort.
- **Cancelled means the command stops, not that the call is interrupted.** The model client in
  `jev-core` is synchronous and runs in `spawn_blocking`, so a cancelled request's HTTP call keeps
  running to the tier's `timeout_ms` and holds its budget permit until it returns (`jev.status`
  shows `in_flight: 1` in that window). What the cancel guarantees is the token's `end`, the
  response, and that nothing further is published under that token.
- **A panic inside a command is contained, and that is what makes this clause true there.** The
  command body runs in its own task (`crates/jev-lsp/src/server.rs`, `execute_command`), so an
  unwind is a `JoinError` rather than an unwind through the transport loop, and the token guard is
  armed *before* `begin` so the unwind closes the token. The request is answered
  `{"code": "panic", "message": "the command panicked and was contained: …"}`, a
  `window/logMessage` says the same, and the server keeps serving. Measured: a command that
  panicked after `begin` produced `begin, end`, the `panic` result, no transport close, and a
  later request answered normally.
- **A panic outside the command body is not covered.** No other handler is spawned, and
  tower-lsp 0.20 contains no `catch_unwind` and awaits handlers in the transport task
  (`src/service.rs`), so such a panic ends the process: the client observes the transport closing
  with no response and no `end`, and no destructor can send one, because the runtime dies with it.
  The guard declines to send when no runtime is current (`Handle::try_current`), which is the same
  situation seen from inside.
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
{ "token": "jev:8f3c1d",
  "value": { "kind": "report",
             "message": "jev: explaining — 412 bytes",
             "data": { "schema": "jev.artifact/1",
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

### 3.6 The session record, and why it is not memory

Every command and every analysis appends one line to `<root>/.git/jev/session.jsonl`, and
`jev.session` reads the tail back. An entry carries the place the request was anchored on
(`uri`, and `line` when the request or the findings give one), which is what lets a client put
a jump target on the line rather than a bare description of it. It sits where dismissals sit, so it survives a restart,
survives a buffer being closed, and never appears in `git status`.

**N9 still holds, and this is the reason it can.** The record is written and never read to
decide anything: no request consults it, the cache is still keyed by content hash, and the same
question about the same document still gets the same answer with or without it. What it gives
the user is the thing an editor usually loses — a record of what happened, in order, that can
be read after the fact. A log that fed back into behaviour would be memory, and memory here
would make the cache a lie.

A line that cannot be parsed is skipped when read, and a root that cannot be written to is not
an error: a read-only checkout should not fail a request over a convenience. **One command
writes no line**: a command that panics, because the record is written after the command's body
returns and the panic path answers with a `Result` envelope instead (§3.5). "Every command"
above means every command that returns.

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
| `fix` | `quickfix.jev` | Edit | yes, when config allows and a finding id is present |
| `harden` | `refactor.rewrite.jev` | Edit | no |
| `types` | `refactor.rewrite.jev` | Edit | no |
| `docs` | `refactor.rewrite.jev` | Edit | no |
| `rewrite` | `refactor.rewrite.jev` | Edit | no |
| `test` | `refactor.rewrite.jev` | Edit incl. `create` resource operation | no |
| `explain` | — | Artifact, via the `jev.explain` command (§6) | n/a |
| `review` | `source.jev` | Findings + refresh | n/a |
| `generate` | `refactor.rewrite.jev` | Edit at cursor | no |
| `fixAll` | `source.fixAll` | Edits for every `ready` finding in scope | yes, when config allows |

`explain` and `review` still arrive through the code action menu; they resolve to a
`command` that opens a buffer. One entry point for every intent.

`jev.followup` is the one command that is not an action. It carries a question the *plugin*
asked the user for (N7 — free text enters at the client, never at the server) and, when the
cursor is on a finding, that finding's id, which the server looks up and puts in the prompt.
That is the whole difference from `explain`: same context, same artifact contract, same
streaming, and a question instead of a task. It is not a chat — there is no conversation held
anywhere, and the same question about the same unchanged code is answered from the cache.

---

## 5. Cost gates

Ordered, all mandatory, all evaluated before any model call:

1. **Dedupe** — `sha256(model_tier, prompt_template_version, context_hash, verb)`; hit
   returns the cached conclusion.
2. **Debounce** — diagnostics: `on_save` or `on_idle_ms` (default 1500), never per
   keystroke.
3. **Budgets** — `max_calls_per_min`, `max_calls_per_hour`, `max_tokens_per_session`.
   Exhaustion is not an error: the server returns `state = "over_budget"` actions and a
   single `window/showMessage` on first exhaustion, then stays silent.
5. **Kill switch** — `:Jev stop` (plugin) sets `enabled = false` through
   `workspace/configuration` re-read; the server stops issuing model calls immediately and
   drains in-flight work.
6. **Cancellation** — `$/cancelRequest` aborts the model call via an `AbortSignal`-style
   token threaded through `jev-core`.

**The rules pass takes the same gates in its own order**, because two of them have to come
before the call to mean anything: the file gates; the rules and their `applies_to`; the
inspections, which are milliseconds of local work and decide nothing; the cache, keyed by
content hash *and* the rules' hash, so a rule edit invalidates every conclusion taken under the
old text; the changed-set test (§6, `force` by exception), which is what keeps a save from
re-asking about files nobody touched; then one budget permit taken *before* one decision call
for the whole document. No answer is invented: a question the response does not mention
publishes nothing.

**The rules' hash is taken over the merged set** — the repository's rules and the shipped ones
(§9) — so the shipped set enters the cache key through it, and there is no separate version
string to keep in step. Editing a shipped rule, adding one, or turning the whole set off with
`rules.defaults` changes the hash and no conclusion taken under the previous set is served. A
shipped rule the repository shadows is not in the hash, because it is not in the set that ran.
The key also carries `noise.max_visible_findings`: the stored findings are the *capped* set
(`findings::build`), so widening the cap is a different question and was, until 2026-09-20, a
cache hit that answered it with the old cap's findings. **And it carries a digest of the
declarations the client sent** (§9): the state is a window around each candidate and the window
is the enclosing declaration when the client sent one, so two sessions over identical bytes can
be asking different questions — the same shape of defect as the cap, added with the window rule
and named here so the next input is checked against this list rather than remembered.

---

## 6. Commands

| Command | Arguments | Returns | Served |
|---|---|---|---|
| `jev.status` | `{}` | `Result` with queue, budgets, cache counters, in-flight calls | yes |
| `jev.recompute` | `{}` | `Result` | yes |
| `jev.explain` | `{uri, line, range?}` | `Artifact` (§7, `kind: "explanation"`) | yes |
| `jev.ask` | `{question, uri?, context?, web?}` | `Artifact` (§7, `kind: "answer"`) — or a one-line `FETCH` request | yes |
| `jev.review` | `{uri}` | `Result` with the findings for the file as it is now | yes |
| `jev.followup` | `{uri, line, question, finding_id?, range?}` | `Artifact` (§7, `kind: "answer"`) | yes |
| `jev.document` | `{uri, version, definitions?, context?}` | `Result` with `{stored}` — the client's own parser answering §3.4.3 | yes |
| `jev.session` | `{limit?}` | `Result` with `{entries, count, path}` | yes |
| `jev.usage` | `{}` | `Result` counting what was published and what was done with it, over the session log | yes |
| `jev.outcome` | `{kind, id?, line?, verb?}` | `Result` with `{recorded}` — the client reporting what the user did | yes |
| `jev.cancel` | `{progress_token}` | `Result` | yes |
| `jev.plan` | `{goal, scope}` | `Artifact` | yes |
| `jev.apply` | `{plan_id, steps: [n]}` | `Result` | yes |
| `jev.revert` | `{edit_id}` | `Result` | yes |
| `jev.inspect` | `{path?, force?}` | `Result` with `findings`, `considered`, `candidates`, `skipped` | yes |

Sent as `workspace/executeCommand`. Only the served commands are advertised in
`executeCommandProvider.commands` (§2): a client should not be told about a command that can
only answer "not implemented". Any command that is not served — an unknown name, or one a
future version adds before it is implemented — still answers with a structured
`{ok: false, error: {code: "not_implemented"}}` rather than failing silently.

**What the model may read off the machine.** Only `jev.ask` with `web: true`, and only this:
the answer may be exactly one line, `FETCH <https url>`, which the server fetches once
(https only, 64 KiB, no redirect following), shows the model the text under a heading naming
the url, and records in the artifact as `_Read: <url>_`. A fetch that fails answers
`fetch_failed` rather than dropping the page silently. Nothing else in this contract gives
model-authored text a route off the machine: **no document is read from disk** — every byte of
one arrives over the protocol from the client — and the only things the server reads on its own
are the repository's rule files (§9) and git's answer about which files changed (§5). What it
writes is its own record (§3.6), which is a log and never an input (N9). This is the boundary
that keeps a model from pulling arbitrary bytes into its own prompt without the user being able
to see which page it read.

**A plan step is applied by the server, not the client.** `jev.apply` resolves the step
against the content the server currently holds, builds the edit, and sends
`workspace/applyEdit` back to the client — so a step that has become stale is refused before
anything is written (it answers `{"code": "stale"}` in the `failed` list). `jev.revert`
restores the bytes recorded before the edit and then forgets the id, so a second revert is
`unknown_edit` rather than a silent no-op.

No command takes a token argument: progress is reported under the `workDoneToken` of the
enclosing request (§3.5), and cancelling a request is `$/cancelRequest` against the id the
plugin's send call returned. `jev.cancel` exists only for work the plugin did not issue.

**`jev.inspect` is the ambient pass, on demand.** It runs the same call the save path runs,
over the same rules, through the same code (`inspections::select` → one decision call →
`inspections::resolve`), because two front ends that disagreed about what a rule says would be
worse than one that never ran. `path` names an open document — a filesystem path, a suffix of
one, or its uri — and omitted it means *the one open document*, which is `bad_arguments` when
more than one is open. `force` skips the changed-set check: **without it a document git does
not report as changed is not inspected at all**, which is the whole reason the flag exists. The
result carries what the pass did, not only what it found:

- `considered` — how many loaded rules claim this file (`applies_to`, §9) — the repository's and
  the shipped ones together;
- `candidates` — how many places their inspections named;
- `findings` — the same shape `jev.review` prints: `{id, line, start_col, end_col, severity,
  label, detail, verb, rule_source}`, with `label` the rule's title and `detail` its prose plus
  the reason the decision gave and the probability it cleared. `rule_source` is `"repository"`
  for a rule in `.jev/rules/`, `"builtin"` for one shipped in the binary, and `null` on a
  finding no rule stands behind (the review's, §9). It is not `data.source`: that names the
  *pass*, this names the *rule set*, and a reader needs both — one says what ran, the other says
  where to go to change it;
- `skipped` — a list of `{code, detail}`: a rule file that could not be read (the code is its
  path, and a shipped one is named `default_rules/<group>/<file>.json`), `("lint", <message>)`
  for a rule that cannot work, `("unchanged", <path>)` for a document the changed set does not
  name, `("unlocatable_anchor", <n> finding(s) …)` for an answer whose anchor occurs zero or
  several times in the text — the same rule the review pathway applies (§4) —
  `("default_rules", <sentence>)` when the pass is running on the shipped set because this
  repository has written no rules of its own, and `("no_rules", <sentence>)` when the pass had
  nothing to run.

**There is no fallback.** The rules are data, and always were; what changed is that they now
have a *shipped* source as well as the repository's own (§9). Nothing generative steps in: a
repository that has switched the shipped set off and written no rules of its own gets no ambient
findings, and the pass *says so* (`no_rules`) rather than reporting a clean document — "nothing
was inspected" and "nothing was wrong" must never look the same (§12). The failure codes are the
ordinary ones: `over_budget` for a refused permit, `model_error` for a decision call that did not
answer, `contract_error` for an answer that arrived and could not be read, `skipped` for a gate.

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

**`window/workDoneProgress/cancel` is not dispatched by the pinned tower-lsp, so the server
cannot honour it, and this document does not ask it to.** The specification cancels
server-initiated progress with that client→server notification
(`WorkDoneProgressCancelParams {token}`), and the plugin does send it
(`nvim/lua/jev/init.lua:1232`) — but tower-lsp 0.20.0, pinned here by `Cargo.lock`, has no
dispatch arm for it: its own source carries `TODO: Add `work_done_progress_cancel()` here (since
3.15.0) when supported by `tower-lsp`.` (`tower-lsp-0.20.0/src/lib.rs:1329`), and
`impl LanguageServer for JevServer` has no handler either. Measured: the notification produces no
reply, no log and no effect. Nothing is broken by that, and the reason is structural rather than
lucky — the server never creates a progress token (§3.5 path 2, §3.4), so there is no
server-initiated progress for a client to cancel. **Cancelling work is the other path, and it is
the one this contract relies on**: `$/cancelRequest` against the request id, which aborts a
running command — the body runs in its own task, the request answers `-32800 Canceled`, the token
is closed exactly once and the answer is discarded. Step 10 of `verify/lsp_client.py` pins that
path. A tower-lsp that dispatches the notification would make the first path available; until
then, a client that sends it is sending a notification into a library that has not implemented
it, and no behaviour depends on it.

**Why `explain` is a command and not a code action.** A resolved code action's `command`
field is executed by the client by sending it *back to the server* as
`workspace/executeCommand` (`vim/lsp/buf.lua:1252-1258`, verified). A server therefore
cannot use that field to make the client open a buffer, and an artifact has nowhere else to
go. So the artifact is *pulled* by the plugin instead: `:Jev explain` calls
`jev.explain` and renders the returned Markdown itself.

---

## 7. Artifacts

Stable, versioned, self-describing, plain JSON on one line per record where streamed.

```jsonc
// Artifact
{ "schema": "jev.artifact/1", "kind": "plan" | "explanation" | "review",
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
{ "schema": "jev.result/1", "ok": true,
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

Verification after apply is not optional **for an edit the server applies** (a `jev.apply` plan
step): the server re-hashes each touched document and, when the hash does not match its
prediction, publishes a diagnostic naming the divergence. A resolved code action is applied by
the client and carries no prediction, so nothing is compared — the gap is named in
`docs/VERIFICATION.md` §11.

This section is verified as a whole exchange against the real client by
`verify/probes/trace.lua`: an edit stamped with the version the action was created against
is applied when the document has not moved, and is refused by the client when it has.

---

## 9. Diagnostics

- Source name: `jev`. One namespace, own `resultId` per document. The **name** stays `jev`
  whatever produced a finding — one source, one place a client turns the surface off; which
  *pass* wrote it travels in `data`.
- Two severities are permitted: `INFORMATION` for observations, `WARNING` for findings.
  `ERROR` is reserved for a divergence the server can prove §8 violated.
- Every finding carries `data = { finding_id, verb, content_hash, source }` so `quickfix.jev`
  actions can be keyed to it `[R4]`. `source` is `"rules"` (a repository convention the
  decision tier confirmed) or `"review"` (the chat review tier's opinion) — a client that shows
  the two differently needs to know, and nothing else about the finding differs.
- Findings must be dismissible (`:Jev dismiss <finding_id>`), and a dismissal is recorded
  in a per-repository file so it does not resurface.
- Publish is reserved for changes the server made; otherwise findings are served by pull,
  refreshed by `workspace/diagnostic/refresh` `[R5]`.

**Two sources, and the repository's own wins.** A rule comes from one of exactly two places,
and every finding says which (`rule_source`, §6):

- **the repository's** — `.jev/rules/<id>.json`, read in path order, relative to the workspace
  root (or, with no root, the document's own directory);
- **the shipped set** — the defaults embedded in the binary from `default_rules/<group>/*.json`,
  applied when `rules.defaults` is true (the default, §10). They are ordinary rule files in the
  same format, and `jev rules init` writes them out so a user can read and edit them (§11).

**Precedence: the repository's file shadows the shipped rule with the same `id`.** A shipped
rule is a default, not an override: where the repository has written a file for that `id`, that
file is the one that runs, and the shipped rule is dropped rather than run beside it — two rules
with one id would report the same line twice under one label, and the reader could not tell
which of them they had calibrated. That is a rule about *sources only*: **within one source**
duplicate ids are all kept and all run, and `rules::lint` reports "duplicate rule id" for them,
which is the loader's behaviour unchanged. Nothing about a shadowed shipped rule is reported as a
skip, because nothing was skipped. The scope of this section is the *rules*; there is still no
generative fallback (§12).

**The shipped set is an input to every rules conclusion.** The rules' hash is taken over the
merged set, and the cache key carries it (§5), so a conclusion taken under one shipped set is
never served under another — and a repository that shadows a shipped rule is unaffected by that
rule changing. `rules.defaults: false` is the setting to run the repository's rules alone.

**Where ambient findings come from: the repository, not the model's taste.** The ambient pass
is the *rules* pass (`docs/MODEL.md` §2, `docs/UX.md` §1.1), and what it runs is data the
repository owns:

```jsonc
{ "schema": "jev.rules/1",
  "rules": [
    { "id": "no-unwrap-in-handlers",
      "title": "Unwrap in a request handler",  // becomes the finding's label (≤ 60 chars)
      "text": "A handler must not unwrap; return the error instead.",  // its detail
      "severity": "warning",                   // information | warning; error is reserved, and a
                                               // rule that asks for it gets warning
      "applies_to": ["**/*.rs"],               // globs over the document's path, relative to
                                               // the workspace root
      "inspection": { "kind": "regex", "pattern": "\\.unwrap\\(\\)", "max_matches": 0 },
      "judgement": { "question": "Is this unwrap reachable from a request handler?",
                     "criteria": { "true": "a request can reach it", "false": "test code" },
                     "reasons": { "reachable": "a request can reach it" },
                     "min_probability": 0.75 },
      "verb_hint": "fix" } ] }
```

An `inspection` is tagged by `kind`: `regex` (every matching line; `max_matches` means
"report only when the file holds *more* than this many", so any match at all is
`max_matches: 0`) or `absent` (the file is expected to contain the pattern and does not — one
candidate at the head of the file). It is deliberately dumb and local: it names candidates and
**decides nothing**. A `judgement` is the one question the decide tier (§10,
`docs/MODEL.md` §1) is asked about them, with `criteria` and `reasons` passed through to the
wire unchanged; a `true` that clears `min_probability` (default 0.5 — a coin flip is not a
finding) becomes a finding through `findings::build`, the same function the review's findings
pass through, so ids, ordering, dismissal and the noise cap behave identically everywhere.

**The state is a window around each candidate, not the file head.** What the decision is shown
for one document is three parts — the lines around each candidate, the candidate list, and the
rules' own prose and criteria — and the first of those was, until 2026-09-20, the **first
`max_state_lines` lines of the document**. Candidates are found anywhere in a file, so the two
were unrelated: on this repository's own `crates/jev-lsp/src/server.rs` (2,805 lines, thirteen
candidates from line 243 to line 2518) not one of the thirteen was inside the 200-line head it
was asked about, and every floor measured against that state was measured with the line
invisible. The rule now, in order of preference:

- **the enclosing declaration, when the client sent one.** `jev.document` (§3.4.3) already
  carries the client's parser's ranges, version-stamped; the smallest declaration containing the
  candidate's line is the window. It is the *smallest*, so a candidate inside a function inside
  a module is shown the function.
- **otherwise a bounded neighbourhood** — a few tens of lines either side, snapped out to the
  enclosing blank-line-separated block when that block is no bigger than the window itself.
- **merged** where they overlap or touch, so a region with several candidates is sent once
  rather than once per candidate.
- **numbered absolutely**, from zero, exactly as the candidate ids are: the numbers in the state
  and the numbers in the candidate list are the same numbers, or the answer cannot be mapped
  back to a line. A gap between two windows is marked and the numbering continues across it.

**The budgets bound the windows.** `max_state_lines` is the most *code* the state may hold and
`max_state_bytes` the most bytes, and both are spent by trimming each window around its
candidates — never by cutting the state at a byte offset, which would drop the rules a judgement
is read against, and never by dropping a candidate: a window whose candidates cannot be held is
split between them, and a candidate's own line is the floor of every operation. `truncate_state`
survives as the bound for the one case that cannot be met this way — a budget smaller than the
candidate lines themselves.

**The definitions are an input to the cache key, and were added with this.** Two sessions over
the same bytes, the same rules and the same path ask different questions when one sent
declarations and the other did not, so `cache::rules_key` carries a digest of them
(`cache::definitions_digest`); `jev inspect` has no client and passes the digest of the empty
set, and a client with no parser for the language sends nothing and lands under the same key.
This is the same class of defect §5 records twice — a key that names one input too few — and it
was not optional: without it the second session's conclusion would answer the first's question.

A rule file that cannot be read, cannot be parsed, or does not carry `"schema": "jev.rules/1"`
is skipped **with a stated reason** and the rest still load; so is a candidate whose anchor is
not uniquely locatable. A repository that has written no rules of its
own is inspected with the shipped set and is *told so* (`default_rules`, §6, `jev.inspect`); with
the shipped set switched off as well it gets no ambient findings and hears `no_rules`. Neither
of those is silence, and neither is a generative fallback — see §12.

---

## 10. Configuration

Read through `workspace/configuration` under one section, `jev`. The server asks; the
plugin supplies from `vim.lsp.config('jev')`. No configuration file of our own beyond the
repository's rules document (§9), and no environment variables beyond the model endpoints.

```jsonc
{ "enabled": true,
  "models": { "reason": {…}, "review": {…}, "decide": {…} },  // see docs/MODEL.md
  "budget": { "max_calls_per_min": 6, "max_calls_per_hour": 120,
              "max_decisions_per_min": 60,
              "max_tokens_per_session": 500000, "timeout_ms": 30000 },
  "triggers": { "diagnostics": "save", "idle_ms": 1500, "severity_floor": "information",
                "rules": { "on_save": true, "on_idle": true, "idle_ms": 1500 } },
  "ambient": { "code_lens": true, "inlay_hints": false, "diagnostics": true },
  "auto_apply": { "fix": false, "fixAll": false },
  "languages": {                    // docs/LANGUAGE.md §7 — may narrow, never disable
    "overrides": { "rust": { "tier": "reason", "prompt": "rust", "verbs": ["fix", "review"] } },
    "max_file_bytes": 1048576,
    "max_scope_lines": 400,
    "ignore": ["**/node_modules/**", "**/*.min.js"]
  },
  "rules": { "enabled": true, "defaults": true, "max_candidates_per_rule": 8,
             "max_state_lines": 200, "max_state_bytes": 16000, "max_files_per_pass": 8 },
  "noise": { "max_visible_findings": 5, "suppress_after_dismissals": 2 },
  "log": "warn" }
```

**`models.decide` is not a chat tier.** It is the endpoint that answers the rules pass's
questions, and a decision is a different protocol from a chat: the model is handed a state and
a numbered set of questions and returns one value per question with a probability, no prose and
no messages (`docs/MODEL.md` §1). Keys `{wire, base_url, model, api_key_env, timeout_ms,
max_tokens, temperature, think}`; defaults wire `system_one`, `https://api.typesafe.ai/v1`,
model `jev-latest`, `api_key_env = TYPESAFE_API_KEY`, `timeout_ms` 5000, `max_tokens` 64,
`temperature` 0.0, `think` `off`. `wire` names the path appended to `base_url`: `system_one`
posts to `/systemone`, `open_router` to `/alpha/decisions`. The ceilings are small on purpose —
a decision generates one value per question, so 64 tokens is generous and five seconds a long
time for it, where the reason tier's numbers are sized for a rewrite. Pointing it at a local
System One server is one config change away: `base_url = "http://127.0.0.1:8009/v1"`,
`model = "kev-latest"`. `JEV_BASE_URL` — which names an OpenAI-compatible chat server —
deliberately does **not** touch this tier; it has its own variables, `JEV_DECIDE_BASE_URL`,
`JEV_DECIDE_MODEL`, `JEV_DECIDE_WIRE` and `JEV_DECIDE_TIMEOUT_MS` (an empty or whitespace value is
ignored).
`JEV_DECIDE_WIRE` takes `system_one`/`systemone` or `open_router`/`openrouter`, trimmed and
case-insensitive, and selects the path appended to `base_url`; a value that is neither is
**ignored with the wire already in force kept**, never coerced to the default, because a typo
that quietly posted every decision to the wrong path would look exactly like the endpoint being
down (`jev.status` reports `models.decide.wire`, so a mistyped override is visible). The API
key's **variable name** comes from `api_key_env` (default `TYPESAFE_API_KEY`) and is
config-only; only its value comes from the environment. The *name* is nameable from the
environment too: `JEV_DECIDE_API_KEY_ENV` assigns it to `models.decide.api_key_env` — a name,
never a key, trimmed, and an empty or whitespace-only value keeps the name in force rather than
clearing it — while `JEV_API_KEY_ENV` names the chat tiers' and never repoints this one. The two
variables name different tiers' keys, because the tiers can sit behind different providers.

**`rules` is the ambient pass.** With `rules.enabled` true — the default — the ambient pass is
the rules pass, and the chat review runs only when it is asked for explicitly (`jev.review`,
the "Review this" action) or when rules are off. `rules.defaults` (default `true`) adds the set
shipped inside the binary to the repository's own, with `.jev/rules/<id>.json` shadowing a
shipped rule of the same id (§9); `false` is the behaviour that predates the shipped set
exactly. The keys bound the work: `max_candidates_per_rule`
is the most questions one regex may put to the decision, `max_state_lines` and
`max_state_bytes` the most of the file it is shown — the most *code* it holds, spent on the
windows around the candidates and no longer on the head (§9) — and `max_files_per_pass` how many
documents one idle pass covers. What it declines to look at is reported, never dropped quietly (§9), and
so is the fact that the shipped set is carrying a pass (`default_rules`, §6).

**Ten declared settings are not read by this implementation**, and each is named here so a
reader does not configure an effect that never happens. They are in the schema because a client
that sends them must not be rejected; setting one changes nothing:

- `ambient.code_lens` (default `true`) and `ambient.inlay_hints` (default `false`): neither
  surface is gated by configuration — `crates/jev-lsp/src/server.rs` advertises
  `codeLensProvider` and `inlayHintProvider` unconditionally, and whether hints are drawn is the
  client's call (`state.rs::hints_are_wanted`, set by `:Jev hints on`).
- `auto_apply.fix` and `auto_apply.fixAll` (default `false`): an edit is applied because the
  user picked it; no setting applies one.
- `budget.timeout_ms` (default `30000`): the enforced ceilings are `max_calls_per_min`,
  `max_calls_per_hour`, `max_decisions_per_min` and `max_tokens_per_session`; a call's own
  timeout is the tier's `models.<tier>.timeout_ms`.
- `log` (default `"warn"`): the level and destination are not read; the record kept on disk is
  the session log, `.git/jev/session.jsonl` (§3.6).
- `triggers.severity_floor` (default `"information"`) and `noise.suppress_after_dismissals`
  (default `2`): the finding cap is `noise.max_visible_findings`, which *is* read, and a
  dismissed finding stays dismissed per repository (`.git/jev/dismissed.json`); a verb is never
  suppressed.
- `languages.overrides.<lang>.tier` and `.prompt`: only `.verbs` is read
  (`config.rs::verbs_for`). The tier follows the verb and the prompt flavour follows the
  language profile (`docs/LANGUAGE.md` §7, `jev-core/src/lang.rs`).

---

## 11. CLI contract

`jev` is a thin sync client of `jev-core`, no daemon required, no state.

```
jev explain <path>[:<line>[:<col>]]        # artifact to stdout
jev review <path>                          # findings, JSON
jev action --verb <verb> <path>[:<range>]  # proposed edit, JSON (never applied)
jev plan --goal <text> <path>              # plan artifact
jev inspect <path> [--force]               # the repository's rules, run over <path>
jev rules init [--dir <dir>] [--force]     # write the shipped rule set out to read and edit
jev status                                 # budget and queue
```

Flags: `--verb <verb>` (action, required), `--goal <text>` (plan, required), `--force`
(inspect: run the rules even for a document git reports as unchanged; rules init: overwrite rule
files that are already there), `--dir <dir>` (rules init: where to write; default
`<root>/.jev/rules`, where `<root>` is the nearest ancestor of the working directory holding
`.git`), `--base-url <url>`
and `--model <name>` (override every tier, the decision tier included; `JEV_BASE_URL`,
`JEV_MODEL` and `JEV_REVIEW_MODEL` do the same for the chat tiers), `--max-tokens <n>`;
`-h`/`--help`, `-V`/`--version`. Flags may be written `--k v` or `--k=v`.

**`jev rules init` materialises the shipped set** (§9): one file per shipped file, in the format
`.jev/rules/*.json` already uses, so that what a repository is inspected with is a file it can
open. Each is written under **its group and its name** — `default_rules/prose/lists-end-in-etc.json`
becomes `.jev/rules/prose-lists-end-in-etc.json` — because the loader reads one flat directory
and the groups are authored in parallel by people who cannot see each other's file names; a name
carrying its group is the thing that makes that coordination unnecessary, and it puts the
provenance on disk that `rule_source` reports about a finding. Two files *within* one group with
the same basename are still refused (one would be written over the other) and nothing is written
at all when that happens. It is idempotent and non-clobbering — a file that is already there is
never replaced without `--force`, and the result names every file it left alone, split into
`unchanged` (already byte-identical to the shipped rule) and `refused` (different: the user's
bytes won). It writes nothing outside the target directory, and it calls no model. A refusal
exits `2` with the refused names in the result and on stderr; a run that writes nothing because
everything already matched exits `0`. `rules` takes no other subcommand.

- stdin: additional context (diff, buffer text) when the path is `-`.
- stdout: exactly one artifact or result, JSON, one line, no decoration.
- stderr: diagnostics, including the per-request cost line.
- No interactive mode, no prompts, no colour, no markdown. Exit codes are the only
  channel besides stdout.

`jev inspect` prints exactly the body §6's `jev.inspect` returns — the counts, the findings and
everything the pass skipped — from the same code, and writes nothing to the file.

| Exit | Meaning |
|---|---|
| 0 | Success, artifact on stdout |
| 1 | Transport or model failure |
| 2 | Usage error or contract violation (bad verb, unparsable range; a `rules init` that would have overwritten a file the user edited) |
| 3 | Budget exhausted |
| 4 | Stale target — the document changed since the request was built |

The LSP server is not required for the CLI, and the CLI is not required for the LSP
server. Both call `jev-core`.

---

## 12. Explicitly refused

Recorded so the refusals are not relitigated:

- **A chat buffer as the primary interface.** Free text has no place to go; the menu, the
  lens, and the finding are the interface. A prompt box exists only as the input to
  `jev.plan`.
- **Model-generated action titles on the fast path.** Non-deterministic labels destroy
  muscle memory.
- **Server-side session or conversation memory.** The plan artifact is the continuation.
- **`willSaveWaitUntil` by default.** Blocking writes on a model call. With N8 in force it
  could only ever return edits nobody approved, so the honest version of this hook is one that
  returns nothing — which is a no-op with a 1 s tax on every save.
- **`relatedDocuments` on a finding.** The diagnostic provider advertises
  `interFileDependencies: false`: nothing here is computed across files, so the field would
  carry an empty map on every finding. It becomes meaningful the day cross-file analysis does,
  and not before.
- **Auto-apply beyond `fixAll`/`fix` with explicit opt-in.**
- **A filetype or language allowlist as the attachment mechanism.** Every comparable
  project gates support this way (`docs/research/prior-art.md` §4); it contradicts N10.
- **Gating the verb set on a treesitter parser or on a resolution result.** The model needs
  neither; absence of a parser changes scope quality, not availability.
- **Silent skips.** A buffer that is attached but not analysed must report why
  (`over_size`, `binary`, `ignored`, `generic_scope`).
- **Telemetry.**
- **A fallback from the rules pass to the chat review.** The ambient pass is the rules pass or
  nothing: a generative review on every save costs thousands of tokens where a decision costs
  dozens, and when it found nothing there would be no way to tell a clean file from a pass that
  never ran. **This still holds now that the rules have a shipped source** (2026-09-20, §9): the
  defaults are data, written by hand, in the same format the repository's own rules use — no
  model writes a rule, nothing is generated at run time, and a repository can read them
  (`jev rules init`), shadow them by id, or switch them off (`rules.defaults = false`), after
  which a repository with no rules gets no ambient findings and `jev.inspect` says `no_rules`.
  What the shipped source changes is only that "no rule files of your own" no longer means
  "nothing to inspect with": the pass names its source (`default_rules` / `rule_source`) so that
  a fresh install and a broken one are different answers. A user who wants the review tier's
  opinion asks for it, and the finding says which pass it came from (`data.source`, §9).
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
| 2026-09-19 | **The model's reach is written down.** §6 gains the `meta.ask --web` policy it had been relying on the implementation to keep: one `FETCH <https url>` line, one page, https only, 64 KiB, no redirects, the url named in the artifact, and `fetch_failed` when it does not arrive — plus the statement that nothing else in this contract touches the network or the filesystem (the server reads no file; everything else arrives over the protocol). Found by reading the tutorial against the code: the caps existed and were tested, but the contract did not say them. |
| 2026-09-19 | **Inline completion withdrawn.** The feature was removed at the user's decision — generated code is asked for, not suggested under the cursor — so this contract no longer carries it: §2's capability block and the paragraph about injecting `inlineCompletionProvider`, §3.2's latency row for `textDocument/inlineCompletion`, §5's 200 ms debounce and 8-character prefix floor, §10's `models.fim` and `inline_completion` section, and the method itself. `crates/meta-lsp/src/{inline,advertised}.rs` are deleted with it. A client whose settings still mention `fim` or `inline_completion` is unaffected: unknown sections are ignored. |
| 2026-09-19 | **The workspace is `jev`, and the ambient pass is the repository's own rules.** The old name is gone from every crate, binary, plugin path, command name, schema string, environment variable and `.git/` path — a clean cutover with no alias and no migration shim, so a session log or a dismissal recorded under the old name is **not** carried over: that record is lost once, by decision rather than oversight. The ambient pass is now the *rules* pass. A repository states its conventions as data — `.jev/rules/*.json`, `"schema": "jev.rules/1"` (§9) — where each rule pairs an `inspection` (a regex, or the absence of one) that names candidates and decides nothing with a `judgement`: one question, gated by `min_probability` (default 0.5), that a new **decision tier** answers over the wire §10 names (`system_one` or `open_router`, hosted by default, local System One one config change away). `jev.inspect` (§6) and `jev inspect` (§11) run that pass on demand through the same code the save path runs, and report what it considered, what it found, and everything it skipped; findings carry `data.source` (`rules` or `review`) beside the `jev` namespace (§9). The generative tiers are demoted to what only they can do — edits, plans, explanations. There is no fallback: a repository with no rules gets no ambient findings, and the pass says `no_rules` rather than reporting a clean document (§12). Verified: `cargo test` 274 passing (49 `jev` + 179 `jev-core` + 46 `jev-lsp`), 0 failed, with `cargo build --release` and `cargo test --no-run` warning-free; `verify/rules_test.py` 45/45; `verify/rules_live.lua` 0 failures, 0 skips on Neovim 0.12.5 **and** 0.12.1; `verify/cli_parity.py` 25/25; and the rest of the table green — one command now, `bash verify/run-suite.sh <out-file>` (`STATUS.md`, `docs/VERIFICATION.md` §8). |
| 2026-09-20 | **§3.5's "exactly one `end` … including panic" is measured, and its boundary is named.** The clause had nothing behind the panic case once the `bridge.rs` drop guard it was written for was deleted with inline completion: tower-lsp 0.20 contains no `catch_unwind` and awaits handlers in the transport task (`src/service.rs`), so an unwind in a handler takes the process with it and no destructor can close the token. The command body now runs in its own task, with the token guard armed *before* `begin`, so a panic in it arrives as a `JoinError`: the token is closed, the request is answered `{"code": "panic", …}`, a `window/logMessage` says so, and the server keeps serving. Measured over the wire: a command that panicked after `begin` gave `begin, end`, the `panic` result, no transport close, and a later request answered normally; the same probe gave `begin, end` for a model error (`model_error`), a budget refusal (`over_budget`) and a cancelled request (`-32800 Canceled`). The clause now states the guarantee for the paths the server survives and says what a client sees when a panic lands outside the command body — the transport closing, with no response and no `end`. Pinned now by `verify/lsp_client.py` **step 10**, which asserts over the wire — for model error, budget refusal and cancellation — that exactly one `begin` and one `end` arrive in that order under the supplied token, that the answer is the `Result` envelope (`model_error` / `over_budget` / `-32800 Canceled`), that the `end` arrives **before** the response, and that the server still answers a following command; the cancellation case also asserts promptness — an `end` within 2 s of the cancel while the model stalls for 4 s — which is the assertion that goes red on the pre-fix behaviour. **Panic stays unpinned by a harness**, because triggering it needs a command the product must not ship: it is pinned by the committed Rust unit tests (`progress_tests::{a_panic_between_begin_and_end_still_sends_its_end, a_token_closed_normally_is_not_closed_twice, a_dropped_request_aborts_the_command_and_closes_its_token}`), and its wire behaviour is probe-only. Measuring the cancellation path found two real defects in the containment as it first stood: `$/cancelRequest` is handled by tower-lsp aborting the *handler* task, and the body was spawned with a bare `tokio::spawn` whose `JoinHandle` was merely dropped — a dropped handle detaches, so the cancel did not cancel (measured: `begin` at 4595 ms, `-32800 Canceled` at 4648 ms, then **eight more `report`s and an `end` at 9372 ms**, 4.7 s after the response that had already invalidated the token); and the progress forwarder was guarded only after the blocking model call, leaving the guard unarmed for exactly the window that matters. Both are fixed by holding the command task and the forwarder in one `AbortOnDrop` armed at the spawn: after the fix the response and the `end` arrive at 4612 ms, 52 ms after the cancel, with nothing after it. Three pre-existing observations the same measurement recorded. `cancellable: Some(false)` in the `begin` was demonstrably wrong as a hint — cancellation *is* honoured, and the value was corrected below. A cancelled command's model call keeps running to the tier timeout and holds its budget permit until then, because it is a `spawn_blocking` synchronous client in `jev-core` (the harness's post-cancel `jev.status` shows `in_flight: 1`): "cancelled" means the command stops, not that the call is interrupted, and §3.5 says so now. And a command that panics writes **no** session-record line, because the record is written after `run_command` returns — §3.6 now names that boundary. |
| 2026-09-20 | **§8's post-apply verification is scoped to the edits the server applies.** The clause read as though every applied edit were checked; `remember_prediction` and `record_applied` are called from one place — the `jev.apply` step path (`crates/jev-lsp/src/server.rs:2581, 2592`) — so a resolved code action, which the client applies, records no prediction and `verify_prediction` returns early: the divergence diagnostic and `:Jev revert` do not cover it. Both paths still get the structural validation (`edit::build_proposal`: every anchor locates once, nothing re-emits lines it did not consume, scope and version stamps), and nothing parses the result on either path — `docs/VERIFICATION.md` §11 weighs the alternatives and names the client-diagnostics correlation as the upgrade, since `CodeActionContext.diagnostics` already arrives and is dropped. Found by a design study of the parsing question. |
| 2026-09-20 | **A command's `begin` says it can be cancelled, because it can; and the decide tier's key variable is nameable from the shell.** §3.5's `begin` carried `cancellable: Some(false)`, which was wrong as a hint: cancellation is honoured. It is now `true` unconditionally (`7d31a80`, `crates/jev-lsp/src/server.rs`), and it can be unconditional because `begin` is only ever sent from inside the spawned command body — by the time a client can see the flag and act on it, there is a task to abort. The boundary that stays true is unchanged and now written down: a cancel stops the *command* (the request answers `-32800 Canceled`, the token is closed exactly once, the answer is discarded), not the model call, which is a synchronous client on a blocking worker that runs on to the tier timeout and holds its budget permit until it returns (`jev.status` shows `in_flight: 1`). §10 gains `JEV_DECIDE_API_KEY_ENV` (`4566305`): its value is the *name* of the variable holding the decide tier's key — never the key — assigned to `models.decide.api_key_env`; trimmed, and an empty or whitespace-only value keeps the name in force rather than clearing it (with nothing in force the `TYPESAFE_API_KEY` default survives). The chat tiers are untouched, and `JEV_API_KEY_ENV` still does not repoint the decide tier: the two variables name different tiers' keys, because the tiers can sit behind different providers. This removes the documented workaround of exporting a key as `TYPESAFE_API_KEY` because the name could not be changed. Verified: `cargo test` 289 (49 `jev` + 191 `jev-core` + 49 `jev-lsp`), 0 failed, no warnings; `verify/lsp_client.py` 44 ok / 0 FAIL / 0 skip, with step 10's twelve assertions over the three reachable §3.5 paths. Step 10 does **not** assert `cancellable` — the contract did not state the value until this row, which is why the value is a document change rather than a harness change. |
| 2026-09-20 | **§2's advertised set was one field short, and §3.5 promised a cancellation path the pinned library cannot deliver.** A live `initialize` answers `"hoverProvider": true` while §2's block — the one that calls itself "the *complete* advertised set" — did not list it, and `hoverProvider` appeared nowhere in this file, `README.md`, `STATUS.md` or `docs/`, even though §3.2 lists `textDocument/hover` as served. The block was then read field by field against a live `initialize` response rather than against §3, and it is eight fields: `positionEncoding`, `textDocumentSync`, `codeActionProvider`, `diagnosticProvider`, `hoverProvider`, `codeLensProvider`, `inlayHintProvider`, `executeCommandProvider`. Second, §3.5 required the server to handle `window/workDoneProgress/cancel` and abort the corresponding job; no handler exists in `impl LanguageServer for JevServer`, and tower-lsp 0.20.0 does not dispatch the method at all — its own source carries `TODO: Add `work_done_progress_cancel()` here (since 3.15.0) when supported by `tower-lsp`.` (`tower-lsp-0.20.0/src/lib.rs:1329`) — so the notification produces no reply, no log and no effect. The clause now says what is true: the plugin does send it (`nvim/lua/jev/init.lua:1232`), the pinned library drops it, cancellation of a *running command* is the `$/cancelRequest` path (§3.5 path 1, pinned by step 10 of `verify/lsp_client.py`), and a tower-lsp that dispatches the notification would make the other path available. §3.5 path 2's server-initiated progress is marked specified-but-unimplemented in the same pass — nothing in `crates/` calls `window/workDoneProgress/create`, which is also why no token exists for a client to cancel. |
| 2026-09-20 | **§3.4.1 now states what a client without the plugin gets from a lens, and §2's completeness claim is checked against a live `initialize`.** The `jev.plugin.` namespace in a lens command is deliberate (the plugin dispatches it in-process; §7 leaves the server no way to open the buffer an explanation goes in), but the consequence for a client that is not the plugin was left implicit. It is now written down and measured: such a client runs the lens through its stock path, the command reaches the server as `workspace/executeCommand`, and the answer is `{"ok": false, "error": {"code": "not_implemented", "message": "jev.plugin.pick is not implemented in this version"}}` — an inert lens with a structured refusal, not silence. Recorded because a reader who finds `jev.plugin.*` in the server's source deserves the reason and the cost. A rule in `.jev/rules/` (`no-client-namespace-in-a-server-id`) fires on this line; whether the ids change or the rule is narrowed is an open item (`STATUS.md`, open questions), and this row records the fact rather than the choice. |
| 2026-09-20 | **§10 overstated what it implements: ten declared settings have no reader.** §10 named two unread keys (`triggers.severity_floor`, `noise.suppress_after_dismissals`); a sweep of `crates/` finds eight settings and two override fields that no code reads — `ambient.code_lens`, `ambient.inlay_hints`, `auto_apply.fix`, `auto_apply.fixAll`, `budget.timeout_ms`, `log`, `triggers.severity_floor`, `noise.suppress_after_dismissals`, and `languages.overrides.<lang>.tier` / `.prompt` (`config.rs::verbs_for` reads only `.verbs`, `crates/jev-core/src/config.rs:477-483`). Each is in the schema only so a payload naming it deserialises. §10 now lists all ten with their defaults and what each appears to promise; `docs/LANGUAGE.md` §7 stops showing `tier` and `prompt` as working settings, `docs/GUIDE.md` §3 stops presenting the unread keys as settings a user turns, and `docs/MODEL.md` §2 and `docs/UX.md` §4 stop claiming they act. Whether to implement each setting or delete the key with its paragraph is an open item (`STATUS.md`, open questions). |
| 2026-09-20 | **§3.5's "the `end` arrives before the response" was the transport's ordering, and the row that asserted it was a coin flip.** That parenthetical was `verify/lsp_client.py`'s own reading of "valid only until the response to that request is sent" (`[R12]`): the command keeps the lifetime in the order it controls — the `end` is awaited into the transport before the command body returns — but `tower-lsp` writes the two through two arms of one `futures::stream::select` (`transport.rs`), polled round-robin, so which of the server's own two messages is written first is the scheduler's. Measured on one unchanged binary: `jev.status` inverted in 4/50 runs (−36.1 µs … +29.9 µs), with the CI red at −21 µs and a green run the same day at +29 µs, and step 10's paths sat 5 µs from flipping. §3.5 now states what step 9 and step 10 assert — one `begin`, one `end`, and nothing under the token after it, read after a settle window — and that the order of those two is not the command's to fix; `--selftest` gained the `progress_after_end` defect so the replacement can fail. |
| 2026-09-20 | **The rules have a shipped source, and missing rule files are no longer silence.** A repository with no `.jev/rules/` produced no ambient findings at all — `no_rules` was the whole answer — so a fresh install and a broken one were indistinguishable, and this project's own conventions published nothing on the repository they were written for. §9 now names **two sources**: the repository's files and a set embedded in the binary from `crates/jev-core/default_rules/<group>/*.json` (globbed by `build.rs`; an empty tree builds and behaves exactly as before). The repository's file **shadows** the shipped rule with the same `id`, and within one source duplicate ids are still kept and still linted. `rules.defaults` (default `true`, §10) turns the shipped set off. `jev rules init [--dir <dir>] [--force]` (§11) writes it into `.jev/rules/` so a rule can be read before it is believed — each file named for its group (`prose-lists-end-in-etc.json`), so two groups authored in parallel cannot collide, and idempotent, non-clobbering, exit `2` when it refuses to overwrite a file the user edited. Every finding now carries `rule_source` (`repository` | `builtin` | `null`, §6), because a finding you cannot trace to a file you can open is one you cannot turn off; `:Jev inspect` and `jev inspect` print it, and a pass carried by the shipped set says so (`default_rules`). §5's key is taken over the merged set, so the shipped rules are an input to every rules conclusion — and `noise.max_visible_findings`, which was missing from it, is in it now. §12 keeps its refusal: no generative fallback, and the defaults are hand-written data an editor can open. |
| 2026-09-20 | **The state is a window around each candidate, not the file head — and the client's declarations are a new input to the cache key.** §9 said the decision is handed a state; what the state *was* is the first `max_state_lines` lines, and candidates live anywhere. On this repository's own `crates/jev-lsp/src/server.rs` (2,805 lines, thirteen candidates from line 243 to line 2518) none of the thirteen was inside the 200-line head, so every floor measured on it was measured with the line invisible, and a class of rules — how many call sites, how many implementations, whether a dependency ships it — could not be authored at all. §9 now states the rule: the smallest enclosing declaration the client sent (`jev.document`, §3.4.3), else a bounded neighbourhood around the candidate, merged where windows overlap, numbered absolutely so the state's numbers and the candidate ids stay the same numbers; the two budgets are spent trimming windows around their candidates and never drop one, with `truncate_state` the bound of last resort. Because the window now reads what the client sent, `cache::rules_key` carries a digest of it (`cache::definitions_digest`) — the same defect class §5 records for `noise.max_visible_findings` — and §5 says so. Measured on `server.rs`: 13/13 candidates in the state against 0/13, 15,389 bytes against 14,017, both inside the unchanged 16,000-byte and 200-line budgets; on a 126-line file with six candidates, 7,355 → 3,615 bytes. Tests in `crates/jev-core/src/inspections.rs` (including one over that real document) and `crates/jev-core/src/cache.rs`. |
