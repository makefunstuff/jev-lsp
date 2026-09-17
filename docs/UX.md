# UX

The interface is Neovim's. Nothing is a chat log; everything is a buffer, a sign, an
inline annotation, or a key you already press.

## 1. Surfaces

| Surface | Native mechanism | Used for | Interrupts? |
|---|---|---|---|
| Sign column + virtual text | `publishDiagnostics` / pull diagnostics | findings while you work | never — no notifications |
| Lightbulb / code action menu | `textDocument/codeAction` | all explicit intents | only when invoked |
| Inline annotation | `textDocument/codeLens` | per-symbol affordances: "explain", "test", "+2 findings" | never |
| Ghost text | `textDocument/inlineCompletion` | FIM completion | never |
| Inline hint | `textDocument/inlayHint` | risk markers, off by default | never |
| Plan buffer | plugin + `window/showDocument` | multi-step work, review, approval | once, on completion |
| Statusline segment | `$/progress` via `LspProgress` | what is running, budget remaining | never |
| Pick list | `window/showMessageRequest` | a decision the server must have | once |
| Text prompt | plugin `vim.ui.input` | the goal for `plan` | only when invoked |

Free text appears exactly once, in `:Meta plan`, because the protocol cannot ask for text
and because a goal is the only thing a picker cannot express.

## 2. Keymaps and commands

```lua
-- default; all overridable
<leader>ma   code action (native menu, always available)
<leader>mp   plan for a goal              -- :Meta plan
<leader>me   explain scope                -- :Meta explain
<leader>mr   review this file
<leader>mt   add tests for scope
<leader>md   dismiss finding at cursor
<leader>ms   status: queue, budgets, cache hit rate
<leader>mx   cancel all in-flight work
<leader>mu   undo the last applied edit
```

Commands: `:Meta plan|explain|review|status|stop|start|undo|dismiss|log|recompute`.

`stop` is the kill switch from PROTOCOL §5 and must be reachable in one mapping without
opening anything.

## 3. Scenarios

### 3.1 Ambient finding, fixed in three keystrokes

1. You save. The worker analyses the file against the budget gates, finds an unchecked
   error path, stores it and sends `workspace/diagnostic/refresh`.
2. Neovim re-pulls; a warning sign appears on the line. No popup, no sound, no tab.
3. `<leader>ma` — the menu opens instantly from cache, first entry
   `Handle the error from read_file` marked `isPreferred`.
4. You pick it. `resolve` returns the edit; Neovim applies it; the sign clears.

Elapsed user-visible latency for step 3: under 50 ms. Total keystrokes: 4.

### 3.2 Multi-step work

`:Meta plan` with a goal ("make retry logic cancellable"). The stream arrives in the
statusline, then a plan buffer opens:

```
 meta://plan/7f1c                                          goal: make retry logic cancellable
 ──────────────────────────────────────────────────────────────────────────────────────────
 1  ✗  Extract the backoff loop from `retry` into a cancellable helper
       crates/net/src/retry.rs · function retry · v42
       Adds the token plumbing; no call-site changes yet.
 2  ✓  Thread the token through `retry_with_backoff` and its callers
       crates/net/src/retry.rs · function retry_with_backoff · v42
       Changed 3 call sites.
 3  ·  Add a cancellation test                                     [skipped]
       tests/retry_cancel.rs · new file
 ──────────────────────────────────────────────────────────────────────────────────────────
 usage: reason · 8.1k in / 1.2k out · 12.4 s        budget: 118/120 calls · 412k/500k tokens
 <CR> apply  <C-v> diff  x skip  r recompute  q close  u undo last
```

The buffer is a normal buffer: folds, marks, yank, search work. Step lines are extmarks,
not a rendered TUI grid, so nothing fights your config.

### 3.3 Approval with a diff

`<C-v>` on a step opens the proposal in a real split diff against the current buffer,
computed from the returned `TextEdit` applied to a scratch copy. Nothing touches the
buffer until `<CR>`. Approve or reject per step; `q` leaves everything untouched.

### 3.4 Inline completion

Off by default. When enabled: after 400 ms of idle in insert mode with ≥ 8 non-space
characters before the cursor, a FIM call returns ghost text. `<Tab>` accepts, `<C-e>`
dismisses, moving on clears it. The statusline shows the FIM call count when it exceeds
half the per-minute budget, so the cost is never invisible.

### 3.5 Undo

`:Meta undo` (`<leader>mu`) restores the buffer snapshot taken before the last applied
edit. This does not rely on Neovim's undo-block behaviour, which is unverified for
`WorkspaceEdit` application `[R10]`; the plugin owns the snapshots and they are per-buffer
and in-memory only.

## 4. Noise policy

The failure mode of every ambient agent is crying wolf. Enforced:

- **Nothing notifies during normal editing.** The only `window/showMessage` cases are a
  first budget exhaustion, model unreachability, and post-apply divergence.
- **Display cap.** At most `noise.max_visible_findings` (default 5) findings visible per
  buffer at once. The rest are summarized in the code lens as `+N findings` and in
  `:Meta status`. Severity-ordered, stable across refreshes.
- **Dismissal is permanent and per-repository.** `<leader>md` writes the finding's
  content-addressed key to `.git/meta/dismissed.json` (never into the repo tree).
- **Suppression.** A verb dismissed twice in a session stops being offered until reload.
- **Quiet by default.** `inlay_hints` and `inline_completion` ship disabled; diagnostics
  do not run per keystroke.
- **Cost transparency.** Every model call logs one line (model, tier, tokens, ms, trigger
  reason) at debug level, and the statusline exposes the counters. Nothing hidden.

## 5. Why this beats prompting in a TUI

| | Prompting in a TUI | meta-lsp in Neovim |
|---|---|---|
| Context | You select it, usually incompletely | Scope, neighbours, diagnostics, imports, repo state are gathered by the server |
| Latency to first useful token | Full round trip after you finish typing the prompt | Picker already open; the answer is cached work |
| Result format | Prose | `WorkspaceEdit` applied to the exact bytes, or a diagnostic on the exact line |
| Failure mode | You apply something wrong | Version-stamped refusal, post-apply divergence detection |
| Interruption cost | Context switch to another pane and back | None — the affordance is on the line you are on |
| Repeat cost | Same tokens again | Cache hit |
| Review | Read the whole answer | Per-step diff, approve or reject individually |
| Undo | Manual, error-prone | One snapshot restore |

The single largest difference is not quality of the model output. It is that the context
is already where the code is, and the output lands where the code is.

## 6. Non-negotiable interactions

- No keymap may change meaning based on model state. If nothing is available, the menu
  says so; the key still opens the menu.
- No blocking prompt on a code path the user did not invoke.
- `q` on any generated buffer leaves buffers, windows, and files exactly as they were.
- Every applied edit is reversible by one command.
- The statusline is the only place work is advertised; no spinner text is inserted into a
  buffer the user types in.
