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
  "codeLensProvider": { "resolveProvider": false },
  "inlayHintProvider": { "resolveProvider": false },
  "executeCommandProvider": { "commands": [ /* §6 */ ], "workDoneProgress": true }
}
```

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
one entry at a time as features land. `codeLensProvider` and `inlayHintProvider` are part of the design
(`docs/UX.md`, `docs/ROADMAP.md`) and are deliberately **not** advertised yet: they are not
implemented.

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
   re-analysis), the server sends `window/workDoneProgress/create` and **waits for the
   response** before any `$/progress` for that token. Permitted only when the client
   advertises `window.workDoneProgress` (Neovim does `[R11]`). If the create request fails
   or errors, the server MUST NOT send progress with that token `[R12]`.

Rules that follow, and are enforced by the streaming module:

- Each token is used once: one `begin`, zero or more `report`, one `end`.
- Exactly one `end` is sent on every path the server survives. Verified over the wire for
  **model error** (the failure is a `Result` envelope, not an early return, so the send on the way
  out still runs), **budget refusal** (the same), and **cancellation** (the command's task is
  aborted and its token guard closes the token; the client sees `-32800 Canceled`). All three are
  asserted by `verify/lsp_client.py` step 10, including that the `end` arrives **before** the
  response — the token is valid only until then. A token left open is a defect, not a leak to
  tolerate.
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

- `considered` — how many loaded rules claim this file (`applies_to`, §9);
- `candidates` — how many places their inspections named;
- `findings` — the same shape `jev.review` prints: `{id, line, start_col, end_col, severity,
  label, detail, verb}`, with `label` the rule's title and `detail` its prose plus the reason
  the decision gave and the probability it cleared;
- `skipped` — a list of `{code, detail}`: a rule file that could not be read (the code is its
  path), `("unchanged", <path>)` for a document the changed set does not name,
  `("unlocatable_anchor", <n> finding(s) …)` for an answer whose anchor occurs zero or several
  times in the text — the same rule the review pathway applies (§4) — and `("no_rules",
  <sentence>)` when the pass had nothing to run.

**There is no fallback.** A repository with no rules gets no ambient findings, and the pass
*says so* (`no_rules`) rather than reporting a clean document — "nothing was inspected" and
"nothing was wrong" must never look the same (§12). The failure codes are the ordinary ones:
`over_budget` for a refused permit, `model_error` for a decision call that did not answer,
`contract_error` for an answer that arrived and could not be read, `skipped` for a gate.

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

**Where ambient findings come from: the repository, not the model's taste.** The ambient pass
is the *rules* pass (`docs/MODEL.md` §2, `docs/UX.md` §1.1), and what it runs is data the
repository owns — `.jev/rules/*.json`, read in path order, relative to the workspace root (or,
with no root, the document's own directory):

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

A rule file that cannot be read, cannot be parsed, or does not carry
`"schema": "jev.rules/1"` is skipped **with a stated reason** and the rest still load; so is a
candidate whose anchor is not uniquely locatable. A repository that has written no rules gets
no ambient findings (§6, `jev.inspect`), and that is the whole of the fallback story — see §12.

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
  "rules": { "enabled": true, "max_candidates_per_rule": 8, "max_state_lines": 200,
             "max_state_bytes": 16000, "max_files_per_pass": 8 },
  "noise": { "max_visible_findings": 5, "suppress_after_dismissals": 2 },
  "log": "warn" }
```

Defaults are conservative: `auto_apply` off, `inlay_hints` off.

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
key's **variable name** comes from `api_key_env` (default `TYPESAFE_API_KEY`) and has no
environment override — only its value is read from the environment, so a hosted provider means
exporting the key under the configured name or changing `api_key_env`.

**`rules` is the ambient pass.** With `rules.enabled` true — the default — the ambient pass is
the rules pass, and the chat review runs only when it is asked for explicitly (`jev.review`,
the "Review this" action) or when rules are off. The keys bound the work: `max_candidates_per_rule`
is the most questions one regex may put to the decision, `max_state_lines` and
`max_state_bytes` the most of the file it is shown, and `max_files_per_pass` how many documents
one idle pass covers. What it declines to look at is reported, never dropped quietly (§9).

**Two keys are declared and not read by this implementation**: `triggers.severity_floor` and
`noise.suppress_after_dismissals`. They are in the schema because a client that sends them must
not be rejected, and they are named here so a reader does not configure a silence that never
happens — the finding cap is `noise.max_visible_findings`, which *is* read, and a dismissed
finding stays dismissed per repository (`.git/jev/dismissed.json`).

---

## 11. CLI contract

`jev` is a thin sync client of `jev-core`, no daemon required, no state.

```
jev explain <path>[:<line>[:<col>]]        # artifact to stdout
jev review <path>                          # findings, JSON
jev action --verb <verb> <path>[:<range>]  # proposed edit, JSON (never applied)
jev plan --goal <text> <path>              # plan artifact
jev inspect <path> [--force]               # the repository's rules, run over <path>
jev status                                 # budget and queue
```

Flags: `--verb <verb>` (action, required), `--goal <text>` (plan, required), `--force`
(inspect: run the rules even for a document git reports as unchanged), `--base-url <url>`
and `--model <name>` (override every tier, the decision tier included; `JEV_BASE_URL`,
`JEV_MODEL` and `JEV_REVIEW_MODEL` do the same for the chat tiers), `--max-tokens <n>`;
`-h`/`--help`, `-V`/`--version`. Flags may be written `--k v` or `--k=v`.

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
| 2 | Usage error or contract violation (bad verb, unparsable range) |
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
  never ran. A repository with no rules gets no ambient findings and `jev.inspect` says
  `no_rules`; if a user wants the review tier's opinion they ask for it, and the finding says
  which pass it came from (`data.source`, §9).
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
| 2026-09-20 | **§3.5's "exactly one `end` … including panic" is measured, and its boundary is named.** The clause had nothing behind the panic case once the `bridge.rs` drop guard it was written for was deleted with inline completion: tower-lsp 0.20 contains no `catch_unwind` and awaits handlers in the transport task (`src/service.rs`), so an unwind in a handler takes the process with it and no destructor can close the token. The command body now runs in its own task, with the token guard armed *before* `begin`, so a panic in it arrives as a `JoinError`: the token is closed, the request is answered `{"code": "panic", …}`, a `window/logMessage` says so, and the server keeps serving. Measured over the wire: a command that panicked after `begin` gave `begin, end`, the `panic` result, no transport close, and a later request answered normally; the same probe gave `begin, end` for a model error (`model_error`), a budget refusal (`over_budget`) and a cancelled request (`-32800 Canceled`). The clause now states the guarantee for the paths the server survives and says what a client sees when a panic lands outside the command body — the transport closing, with no response and no `end`. Pinned now by `verify/lsp_client.py` **step 10**, which asserts over the wire — for model error, budget refusal and cancellation — that exactly one `begin` and one `end` arrive in that order under the supplied token, that the answer is the `Result` envelope (`model_error` / `over_budget` / `-32800 Canceled`), that the `end` arrives **before** the response, and that the server still answers a following command; the cancellation case also asserts promptness — an `end` within 2 s of the cancel while the model stalls for 4 s — which is the assertion that goes red on the pre-fix behaviour. **Panic stays unpinned by a harness**, because triggering it needs a command the product must not ship: it is pinned by the committed Rust unit tests (`progress_tests::{a_panic_between_begin_and_end_still_sends_its_end, a_token_closed_normally_is_not_closed_twice, a_dropped_request_aborts_the_command_and_closes_its_token}`), and its wire behaviour is probe-only. Measuring the cancellation path found two real defects in the containment as it first stood: `$/cancelRequest` is handled by tower-lsp aborting the *handler* task, and the body was spawned with a bare `tokio::spawn` whose `JoinHandle` was merely dropped — a dropped handle detaches, so the cancel did not cancel (measured: `begin` at 4595 ms, `-32800 Canceled` at 4648 ms, then **eight more `report`s and an `end` at 9372 ms**, 4.7 s after the response that had already invalidated the token); and the progress forwarder was guarded only after the blocking model call, leaving the guard unarmed for exactly the window that matters. Both are fixed by holding the command task and the forwarder in one `AbortOnDrop` armed at the spawn: after the fix the response and the `end` arrive at 4612 ms, 52 ms after the cancel, with nothing after it. Three pre-existing observations the same measurement recorded. `cancellable: Some(false)` in the `begin` is demonstrably wrong as a hint — cancellation *is* honoured, and the value is being corrected. A cancelled command's model call keeps running to the tier timeout and holds its budget permit until then, because it is a `spawn_blocking` synchronous client in `jev-core` (the harness's post-cancel `jev.status` shows `in_flight: 1`): "cancelled" means the command stops, not that the call is interrupted, and §3.5 says so now. And a command that panics writes **no** session-record line, because the record is written after `run_command` returns — §3.6 now names that boundary. |
| 2026-09-20 | **§8's post-apply verification is scoped to the edits the server applies.** The clause read as though every applied edit were checked; `remember_prediction` and `record_applied` are called from one place — the `jev.apply` step path (`crates/jev-lsp/src/server.rs:2581, 2592`) — so a resolved code action, which the client applies, records no prediction and `verify_prediction` returns early: the divergence diagnostic and `:Jev revert` do not cover it. Both paths still get the structural validation (`edit::build_proposal`: every anchor locates once, nothing re-emits lines it did not consume, scope and version stamps), and nothing parses the result on either path — `docs/VERIFICATION.md` §11 weighs the alternatives and names the client-diagnostics correlation as the upgrade, since `CodeActionContext.diagnostics` already arrives and is dropped. Found by a design study of the parsing question. |
