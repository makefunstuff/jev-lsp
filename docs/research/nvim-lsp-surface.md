# Research: Neovim LSP surface, verified

Every claim in `PROTOCOL.md` that constrains the server traces to a probe here.
Environment: `NVIM v0.12.5`, LuaJIT 2.1.1787165859, runtime at
`/usr/share/nvim/runtime/lua/vim/`. Method: read the runtime source and run headless
probes. Labels: **[V]** observed on this machine (probe output included), **[S]** read
from runtime source at the cited line, **[U]** not established.

---

## 1. Server -> client surface (the complete actuator set)

Reproduce with `verify/probes/surface.lua`. Server→client requests, complete — 13: **[V]**

```
$/progress                        window/workDoneProgress/create
window/showMessageRequest         client/registerCapability
client/unregisterCapability       workspace/applyEdit
workspace/configuration           workspace/workspaceFolders
window/showDocument               workspace/codeLens/refresh
workspace/diagnostic/refresh      workspace/inlayHint/refresh
workspace/semanticTokens/refresh
```

`NSC` — 3 notifications: `textDocument/publishDiagnostics`, `window/logMessage`,
`window/showMessage`. **[V]**

**Consequence.** There is no `window/showInputBox`, no client-command escape hatch, and
no custom-UI request. A server can never ask the user for free text. Any text-entry UI
must come from the Lua plugin, which has the full `vim.api`. Design constraint, not a
preference.

## 2. `window/showMessageRequest` renders a real picker

`lsp/handlers.lua:103` **[S]**: when `params.actions` is non-empty and the call arrives
from a coroutine, the handler calls `vim.ui.select(params.actions, {kind='lsp_message',
format_item=title}, cb)` and returns the chosen `MessageActionItem`. From a non-coroutine
context it degrades to `vim.fn.inputlist`. **[S]**

## 3. Edit application: version enforcement (probe)

Probe `verify/probes/edit-version.lua`, buffer at `buf_versions = 5`, one zero-width edit at 0:0:

| `documentChanges[0].textDocument.version` | Outcome |
|---|---|
| `3` (older than buffer) | Dropped — prints `Buffer file:///tmp/probe_x.lua newer than edits.` |
| `5` (equal) | Applied |
| `null` | Applied, version check skipped |
| **absent** | **`E5113 ... util.lua:541: attempt to compare number with nil`** |
| (bare `changes` map, no `documentChanges`) | Applied, **no version check at all** |

Raw: **[V]**

```
NULL         ok=true  line1="-- NULL"  err=-
ABSENT       ok=false line1="-- NULL"  err=util.lua:541: attempt to compare number with nil
changes-form ok=true
```

Source agrees: `lsp/util.lua:533-545` guards with
`text_document.version ~= vim.NIL and text_document.version > 0 and buf_versions[bufnr] > text_document.version`,
and only for `index == 1` ("do not check the version after the first edit"). **[S]**

**Consequence.** The frozen edit contract: always `documentChanges`, always an explicit
`version`. The bare `changes` form is a silent-clobber hazard; an absent `version` is a
crash in the client's selection callback, which runs `apply_workspace_edit` without
`pcall` (`lsp/buf.lua:1252`). **[S]**

## 4. Code actions: lazy resolve, disabled slots, trigger kind

- `codeAction/resolve` is called **only for the action the user picked**
  (`lsp/buf.lua:1305`), and only when the action lacks both `edit` and `command`. On
  resolve error the client falls back to applying the original action
  (`lsp/buf.lua:1309-1316`). **[S]**
- A `disabled` action is hidden unless `context.triggerKind == Invoked`
  (`lsp/buf.lua:1229`); if the user picks one, its `disabled.reason` is shown as an error
  notification (`lsp/buf.lua:1303`). **[S]** That is a native "nothing to offer / still
  working" slot.
- `vim.lsp.buf.code_action()` defaults `context.triggerKind = Invoked`
  (`lsp/buf.lua:1386`). **[S]**
- With `opts.apply` and exactly one action, the client applies it with no UI
  (`lsp/buf.lua:1322`). **[S]**
- Kind filtering is prefix-based on `.` (`lsp/buf.lua:1219-1226`). **[S]**

Advertised client capability **[V]**: `codeAction.dataSupport = true`,
`disabledSupport = true`, `isPreferredSupport = true`, `honorsChangeAnnotations = true`,
`resolveSupport.properties = [edit, command]`. So `data` round-trips and resolve may add
either field.

## 5. Pull diagnostics wire up automatically

`lsp.lua:876` — on attach, `if client:supports_method('textDocument/diagnostic') then
lsp.diagnostic._enable(bufnr) end`. **[S]** No plugin code needed to receive pull-based
diagnostics; the server only has to declare a `diagnosticProvider`.

Advertised **[V]**: `textDocument.diagnostic.dynamicRegistration = true`,
`dataSupport = true`, `relatedDocumentSupport = true`; `workspace.diagnostics.refreshSupport = true`.

## 6. Position encoding: the server chooses

`lsp/client.lua:596` — `self.offset_encoding = self.server_capabilities.positionEncoding`. **[S]**

Advertised **[V]**: `general.positionEncodings = [utf-8, utf-16, utf-32]`.

**Consequence.** The server declares `positionEncoding: "utf-8"` and every range is a
byte offset — identical to Rust `&str` indices and treesitter byte columns. No UTF-16
code-unit arithmetic anywhere in the codebase.

## 7. Inline completion is advertised and self-driving

- Advertised in the default client capabilities: `textDocument.inlineCompletion =
  { dynamicRegistration = false }` (`lsp/protocol.lua:522`). **[S]** Static, so no
  dynamic registration is needed.
- `vim.lsp.inline_completion` autocmds on `InsertEnter`, `CursorMovedI`, `TextChangedP`
  and calls `automatic_request()`, which does `vim.defer_fn(request, 200)` —
  **200 ms debounce**, `triggerKind = Automatic` (`lsp/inline_completion.lua:326`). **[S]**
- Manual invocation uses `triggerKind = Invoked` and attaches `selectedCompletionInfo`
  when a range is selected (`lsp/inline_completion.lua:289-300`). **[S]**
- Rendering is an extmark with `virt_text_pos = 'inline'` or `'overlay'`
  (`lsp/inline_completion.lua:262-270`). **[S]**

**Consequence.** This is a firehose: a request every 200 ms of insert-mode activity. All
cost control must live server-side (content-hash dedupe, prefix floor, per-buffer and
per-session budgets, kill switch).

## 8. Ambient renderers are opt-in on the client

`vim.lsp.codelens.enable/refresh/on_refresh` (`lsp/codelens.lua:329,483,552`) and
`vim.lsp.inlay_hint` (`lsp/inlay_hint.lua:113`) exist, with `workspace/*/refresh`
handlers wired to re-pull. **[S]** Both must be enabled by user config — they are not on
by default.

## 9. `$/progress` tolerates arbitrary tokens

`lsp/handlers.lua:60-90` **[S]**: the handler validates nothing, pushes `params` onto
`client.progress`, tracks `pending[token]` titles across begin/report/end, and fires

```lua
api.nvim_exec_autocmds('LspProgress', { pattern = kind, data = { client_id = ..., params = ... } })
```

**Consequence.** The plugin can receive a stream of `begin`/`report`/`end` updates through
the `LspProgress` autocmd without any custom LSP method. This is the streaming channel.

**Conformance caveat — this is tolerance, not permission.** The specification does not
allow a server to send `$/progress` for a token it neither received as a `workDoneToken`
nor created via `window/workDoneProgress/create`; see §12. Neovim's handler validates
nothing, so a bare token smuggled through `arguments` happens to work *in Neovim only*. The
design therefore uses the legal token sources (§12), and this probe records the gap between
what this client tolerates and what the protocol permits.

## 10. Undo granularity of an applied agent edit — **[U]**

Three edits in one `documentChanges` entry applied via `apply_workspace_edit`: one
`:undo` reverted to `{""}`, and `seq_cur` did not advance. Headless `-l` execution cannot
distinguish "all three edits form one undo block" from "no undo entries are recorded at
all in script mode". **Not established.** Probe to settle it: interactive Neovim, apply a
multi-edit `WorkspaceEdit` from a real RPC callback, then count `u` presses needed to
return to the pre-edit text.

Design response: do not depend on it. The plugin snapshots buffer content before and
after every applied edit and provides `:Jev undo`, which is correct regardless of what
undo blocks do.

## 11. Client capability dump (the ones the design depends on)

`vim.lsp.protocol.make_client_capabilities()`, dumped and filtered. All **[V]**:

```jsonc
"general": { "positionEncodings": ["utf-8", "utf-16", "utf-32"] },
"textDocument": {
  "codeAction": {
    "dataSupport": true,              // `data` round-trips through resolve
    "disabledSupport": true,          // a `disabled` action + reason is legal
    "isPreferredSupport": true,       // best action sorts first
    "honorsChangeAnnotations": true,
    "resolveSupport": { "properties": ["edit", "command"] },  // resolve may add either
    "dynamicRegistration": true
  },
  "diagnostic": { "dynamicRegistration": true, "dataSupport": true,
                  "relatedDocumentSupport": true },
  "inlineCompletion": { "dynamicRegistration": false },        // static: no registration needed
  "hover": { "dynamicRegistration": true, "contentFormat": ["markdown", "plaintext"] },
  "inlayHint": { "dynamicRegistration": true, "resolveSupport": { "properties": [...] } },
  "synchronization": { "didSave": true, "willSave": true, "willSaveWaitUntil": true }
},
"workspace": {
  "applyEdit": true,
  "configuration": true,
  "workspaceFolders": true,
  "didChangeWatchedFiles": { "relativePatternSupport": true },
  "codeLens": { "refreshSupport": true },
  "diagnostics": { "refreshSupport": true },
  "inlayHint": { "refreshSupport": true },
  "semanticTokens": { "refreshSupport": true },
  "workspaceEdit": { "resourceOperations": ["rename", "create", "delete"],
                     "changeAnnotationSupport": { "groupsOnLabel": true },
                     "normalizesLineEndings": true }
}
```

`textDocument.inlineCompletion` does **not** appear as a flat `true` in a naive leaf walk
because its value is a table (`{dynamicRegistration = false}`) — it is nevertheless
advertised, at `lsp/protocol.lua:522` **[S]**. A capability-diff tool that only looks for
`true` would wrongly conclude inline completion is unsupported.

## 12. Progress token rules — the protocol, not the client

Client behaviour is recorded in §9; this section is what the **specification** requires.
Primary source, read directly (not a search snippet):

- `_specifications/lsp/3.17/types/workDoneProgress.md`
- `_specifications/lsp/3.17/window/workDoneProgressCreate.md`
- `_specifications/lsp/3.17/window/workDoneProgressCancel.md`
- `_specifications/lsp/3.17/workspace/executeCommand.md`

Verbatim, **[P]**:

> Work Done progress can be initiated in two different ways: 1. by the sender of a request
> (mostly clients) using the predefined `workDoneToken` property in the requests parameter
> literal. […] 1. by a server using the request `window/workDoneProgress/create`.

> The token received via the `workDoneToken` property in a request's param literal is only
> valid as long as the request has not send a response back.

> To keep the protocol backwards compatible servers are only allowed to use
> `window/workDoneProgress/create` request if the client signals corresponding support
> using the client capability `window.workDoneProgress`.

> In case an error occurs a server must not send any progress notification using the token
> provided in the `WorkDoneProgressCreateParams`.

> To avoid that clients set up a progress monitor user interface before sending a request
> but the server doesn't actually report any progress a server needs to signal general work
> done progress reporting support in the corresponding server capability.

And the type inheritance that makes the client-initiated path usable for our back-channel,
**[P]**:

```typescript
export interface ExecuteCommandParams extends WorkDoneProgressParams { command: string; arguments?: LSPAny[]; }
export interface WorkDoneProgressParams { workDoneToken?: ProgressToken; }
export interface ExecuteCommandOptions extends WorkDoneProgressOptions { commands: string[]; }
export interface WorkDoneProgressOptions { workDoneProgress?: boolean; }
```

`window/workDoneProgress/cancel` is client→server, cancels server-initiated progress, and
explicitly does not require the progress to have been marked `cancellable` **[P]**.

**Consequence for the design.** Three things, all now enforced in `PROTOCOL.md`:

1. Progress rides the `workDoneToken` of the `workspace/executeCommand` params. A bare
   token smuggled through `arguments` is non-conformant even though Neovim accepts it (§9).
2. `executeCommandProvider` declares `workDoneProgress: true`; nothing else does, because
   nothing else reports progress.
3. `window/workDoneProgress/create` is used only for work with no request to attach to, and
   only after the client advertises `window.workDoneProgress` — true for Neovim `[R11]`.

**Verified end to end `[V]`.** `verify/probes/streaming.lua` drives a stub stdio server
(`streaming/server.py`) from the real client and asserts the whole path:

```
[streaming] token in request params : true
[streaming] progress kinds observed : {begin,report,end}
[streaming] 0 assertion(s) failed
```

So the normal path needs no `window/workDoneProgress/create`, and the token supplied by the
client arrives at the server *inside the request params* — which is what makes the
streaming channel legal, not merely tolerated (§9). Written probe-first: the first run
failed twice (a path bug, then a reversed `(err, result)` callback signature), which is
exactly the kind of mistake this section exists to prevent in the server.

## 13. Not used

- `textDocument/willSaveWaitUntil` — advertised as `true` **[V]**, but blocking a write on
  a model call is a hazard. Off by default, behind an explicit opt-in.
- `textDocument/formatting`, `rangeFormatting` — belong to real formatters.
- `textDocument/semanticTokens` — belongs to real parsers; the model has nothing to add.
- Custom `jev/…` LSP methods — unnecessary. The plugin is in-process with Neovim and can
  call Lua directly; the standard `workspace/executeCommand` + `$/progress` pair covers
  the back-channel.
