# UX

The interface is the client's, and every row of it is a standard LSP surface: a diagnostic, a
code action, a lens, a hint, a progress report. Nothing is a chat log; nothing needs a custom
method. What this document describes in keystrokes and buffers is the **Neovim plugin's**
rendering of those surfaces — the first-class client, and the only place a keymap exists at all
(`docs/LANGUAGE.md` §1: the plugin is optional convenience, never required).

## 1. Surfaces

| Surface | Native mechanism | Used for | Interrupts? |
|---|---|---|---|
| Sign column + virtual text | `publishDiagnostics` / pull diagnostics | findings while you work | never — no notifications |
| Lightbulb / code action menu | `textDocument/codeAction` | all explicit intents | only when invoked |
| Inline annotation | `textDocument/codeLens` | per-symbol affordances: "explain", "test", "+2 findings" | never |
| Inline hint | `textDocument/inlayHint` | a `jev: N finding(s)` badge on a declaration that has findings, and nothing elsewhere | never |
| Plan buffer | plugin + `window/showDocument` | multi-step work: a line per step, `<CR>` applies one, `a` the rest, `u` takes one back | once, on completion |
| Statusline segment | `$/progress` via `LspProgress` | what is running, budget remaining | never |
| Streamed answer | `$/progress` partial results (§3.5.1) | the answer written into its buffer as it arrives, and `waiting for the model (3s)` before the first token | never |
| Hover | `textDocument/hover` | what has already been explained about this scope, instantly and never from a model | never |
| Text prompt | plugin `vim.ui.input` | the goal for `plan` | only when invoked |

Free text appears exactly once, in `:Jev plan`, because the protocol cannot ask for text
and because a goal is the only thing a picker cannot express.

**Every row above is a standard surface, and the plugin is convenience, never a requirement.**
Findings arrive by pull diagnostics plus `workspace/diagnostic/refresh`; actions by
`codeAction` and `codeAction/resolve`; the material the model is shown by
`workspace/executeCommand` and `workspace/configuration`; progress by `$/progress` under a
token the client itself issued; free text by the client, because the protocol cannot ask for it
(N7). Everything that is *not* a standard surface — the universal attach pass, the
`vim.lsp.codelens.run` interception, the picker, scratch buffers, undo snapshots — lives in
`nvim/` and is optional: a client that speaks LSP gets the findings and the edits without it.
`docs/LANGUAGE.md` §1 says what the plugin exists for; `docs/VERIFICATION.md` §2 says what is
proven with it and what is proven without.

**Context the model is shown is the editor's, not the server's** (PROTOCOL §3.4.4). When a
request generates — an explanation, a question about a finding, a resolved action — the plugin
attaches the imports at the top of the file, what the *other* language servers say refers to
the symbol, the test that covers it, and the other buffers that are open. The server bounds
that (four documents, forty lines each), orders it, and folds it into the cache key, so the
same question about the same state is still answered once.

Every row above is built. The plan buffer came last: `:Jev plan <goal>` renders the plan the
server verified as one line per step, `<CR>` applies the step on the cursor's line, `a` applies
the rest, `u` takes the last one on that line back, and each line says what happened to it.
Nothing is applied until asked (N8), which is why it is a buffer with keystrokes rather than a
progress bar.

The inline hint is built too (`textDocument/inlayHint`, §1 row 4): a `jev: N finding(s)`
badge on a declaration that has findings, and silence everywhere else. It stays **off** by
default — Neovim switches inlay hints on per *buffer*, not per client, so enabling it for this
badge also enables every other server's hints in that buffer. `:Jev hints on|off` toggles it.

The inline annotation is built (`textDocument/codeLens`, §1 row 3): a clean declaration shows
`jev: explain`, one with cached findings shows `jev: N finding(s) · fix`, and running it is
`:lua vim.lsp.codelens.run()` at the cursor or the lens click. The command never reaches the
server — the plugin handles its own `jev.plugin.` namespace, because opening a buffer is the
client's decision (PROTOCOL §3.4.1).

A streamed answer is the one place the interface shows work *while* the model runs: the
scope's explanation appears in its buffer a few words at a time, and the seconds before the
first token are reported rather than left blank. Measured against the local model: 4.6 s to a
complete answer, the first 3 of which are prefill and reasoning.

### 1.1 The rules pass — where an ambient finding comes from

The ambient pass is the repository's *rules* (PROTOCOL §9). A rule is a convention in prose
plus the inspection that names the places it might be about; the decision tier answers one
question about each of those places, and only a `true` above the rule's own floor becomes a
finding:

```jsonc
{ "schema": "jev.rules/1",
  "rules": [
    { "id": "no-unwrap-in-handlers",
      "title": "Unwrap in a request handler",   // the finding's label
      "text": "A handler must not unwrap; return the error instead.",  // its detail
      "severity": "warning",
      "applies_to": ["**/*.rs"],
      "inspection": { "kind": "regex", "pattern": "\\.unwrap\\(\\)" },
      "judgement": { "question": "Is this unwrap reachable from a request handler?",
                     "min_probability": 0.75 },
      "verb_hint": "fix" } ] }
```

The file lives in `.jev/rules/`, and `docs/TUTORIAL.md` §3 walks through writing one. What you
see when a rule fires is an ordinary finding — a sign in the margin, an entry in the menu, a
count in the lens, something to dismiss — whose **label is the rule's title** and whose detail
is the rule's prose followed by the reason the decision gave and the probability it cleared.
Nothing about it is model prose you have to interpret, and the finding says which pass produced
it: `data.source` is `rules` for these and `review` for the chat review's, so a client that
wants to show them differently can (PROTOCOL §9).

**`:Jev inspect` is the answer to "why did — or didn't — this file get a finding".** It runs the
rules pass for the current buffer through the `jev.inspect` command and opens the answer in a
buffer, like any other artifact: the counts (rules considered, candidates found, findings
published), one line per finding with the rule's prose under it, and every skip — `unchanged`
(git reports the file untouched), `no_rules` (nothing in `.jev/rules/` claims this file, or no
rules loaded at all), or a rule file that could not be loaded, with its reason. A `false` answer
leaves no trace here: only a `true` that clears the floor is published, so the negative side of a
question can only be read by posting the request directly or with the floor at `0.0`.
`:Jev inspect --force` re-runs the pass even when git reports the file unchanged. The LSP command
`jev.inspect` is the contract — any client can call it, and the CLI's `jev inspect` is the same
call from a shell — while `:Jev inspect` is this plugin's convenience over it, like every other
row of §1.

**A pull with no pass behind it answers clean.** The pass runs on save and on `jev.inspect`; a
client that never sends `didSave` sees nothing, and that empty answer looks exactly like a clean
file. Measured in one fixture: `diagnostics` alone → `OK`, then `jev.inspect` and `diagnostics`
→ the finding. Save the buffer (or run `:Jev inspect`) before concluding that a rule is broken.

**A rule edit does not repaint what is already on screen.** Diagnose the surprise in the order
it happens: the findings every surface reads are one shared display slot, keyed by content
hash, resolved language and the findings cap, and both the rules pass and the chat review write
it. So after editing a rule, findings already displayed stay exactly as they were until the next
pass for that document — the next save, or the idle trigger. There is no watcher on
`.jev/rules/`, so writing a rule file re-runs nothing by itself. To see the effect *now*:
`:Jev recompute` drops the conclusion cache and re-runs every open document, and
`:Jev inspect --force` re-runs this one, this moment. This is the price of one display slot for
both passes rather than a defect.

**No rules, no ambient findings.** A repository that has written none gets nothing on save —
deliberately, and not silently: `:Jev inspect` says `no_rules`, the pass's log line names the
directory it looked in, and `:Jev status` reports `rules.loaded` (0 means nothing loaded, which
`rules.hash` and `rules.last_pass_ms` distinguish from "no pass has run yet"). The chat review
has not gone away for when you want that instead: `:Jev review`, or the "Review this" action —
and the findings it returns are labelled `review`.

## 2. Keymaps and commands

Two products, four keys. Findings → actions, and ask, are what this server is for;
everything else is reachable by typing. Generated code is asked for, not suggested under the
cursor: inline completion was removed on 2026-09-19 (STATUS.md).

```lua
-- default; all overridable
<leader>ja   code action (picker, summary in the preview pane)
<leader>ju   undo the last applied edit
<leader>jq   ask a question
<leader>js   status: queue, budgets, cache hit rate
```

Reachable by typing — the full subcommand set, since a keymap is an accelerator and not the
surface: `ask|followup|where|explain|review|inspect|plan|dismiss|undo|hints|usage|session|`
`recompute|status|stop|start|cancel|log`. `:Jev ask --web <question>` is the
fetch-enabled form of ask: one https page may be read to answer, and the artifact names it.
`:Jev inspect [--force]` runs the rules pass for this buffer and shows what it found and what it
skipped — §1.1 is the paragraph worth reading before you conclude a rule is broken.

`:Jev usage` is the answer to "is this working": published findings, files analysed, and
counts of what was done with them — applied, dismissed, accepted, undone — over what the
session log still holds. `:Jev session` opens the log itself at this root (`<root>/.git/jev/session.jsonl`),
newest first, with a pointer to the file. An entry that names a place says so
(`review_me.py:5`), and `<CR>` on it opens that file at that line: the record is a history you
can walk, not only read.

`stop` is the kill switch from PROTOCOL §5 and must be reachable without opening anything —
it is one `:Jev stop` away, and the status line it silences says so.

### 2.1 Where a report goes — `surfaces.layout`

Asking a question must not rearrange the windows around it, so the placement of every generated
buffer is one setting, a `setup` option like the prefix:

```lua
require('jev').setup({ surfaces = { layout = 'current' } })   -- the default
```

The full settings reference — every key, its default, and the environment variable that
overrides it — is `docs/GUIDE.md` §3.

| value | what it does |
|---|---|
| `current` | **the default.** The report takes the buffer in the window you are already in: the window count, the sizes and every buffer are untouched, and the file you left stays loaded as the alternate buffer, so `q` and `<C-^>` both come straight back to it. This is what `:Jev inspect`, `:Jev explain`, `:Jev ask`, `:Jev followup`, `:Jev usage`, `:Jev plan` and `:Jev session` do |
| `float` | a rounded floating window over the code; the code stays visible. The one layout that *does* add a window while it is open (dismissed with `q` or `<Esc>`) |
| `split` | the old `sbuffer` behaviour, for code and report side by side |

It is read when a surface opens, not at `setup`, so it is live. A value that is not one of the
three notifies at ERROR and keeps the previous layout —
`jev: surfaces.layout = "window" is not a layout (current|float|split); keeping current` — rather
than falling back silently, so nobody believes they asked for a split and got something else.

What did **not** change: `status`, `review`, `recompute`, `dismiss`, `undo`, `hints`, `cancel`,
`start` and `stop` were already messages rather than buffers. `:Jev log` is `hide edit` now (the
same window as before, and it no longer raises `E37` on an unsaved buffer). The `<C-v>` diff
preview is still a real split, deliberately: a side-by-side diff is something the user asked for.

## 3. Scenarios

### 3.1 Ambient finding, fixed in three keystrokes

1. You save. The rules pass runs against the budget gates: a rule's inspection names a line, the
   decision tier confirms it, the finding is stored and `workspace/diagnostic/refresh` goes out.
2. Neovim re-pulls; a warning sign appears on the line. No popup, no sound, no tab.
3. `<leader>ja` — the menu opens instantly from cache, first entry
   `Handle the error from read_file` marked `isPreferred`.
4. You pick it. `resolve` returns the edit; Neovim applies it; the sign clears.

Elapsed user-visible latency for step 3: under 50 ms. Total keystrokes: 4.

### 3.2 Multi-step work

`:Jev plan` with a goal ("make retry logic cancellable"). The stream arrives in the
statusline, then a plan buffer opens:

```
 jev://plan/7f1c                                          goal: make retry logic cancellable
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
 <CR> apply this step   a apply every step   u take this one back   q close
```

The buffer is a normal buffer: folds, marks, yank, search work. Step lines are extmarks,
not a rendered TUI grid, so nothing fights your config. `<C-v>` (diff a step before applying
it) is the picker's preview, not a plan-buffer key.

### 3.3 Approval with a diff

`<C-v>` on a step opens the proposal in a real split diff against the current buffer,
computed from the returned `TextEdit` applied to a scratch copy. Nothing touches the
buffer until `<CR>`. Approve or reject per step; `q` leaves everything untouched.

### 3.4 Undo

`:Jev undo` (`<leader>ju`) restores the buffer snapshot taken before the last applied
edit. This does not rely on Neovim's undo-block behaviour, which is unverified for
`WorkspaceEdit` application `[R10]`; the plugin owns the snapshots and they are per-buffer
and in-memory only.

## 4. Noise policy

An ambient agent fails by being ignored, so the policy is written down and enforced:

- **Nothing notifies during normal editing.** The only `window/showMessage` cases are a
  first budget exhaustion, model unreachability, and post-apply divergence.
- **Display cap.** At most `noise.max_visible_findings` (default 5) findings per buffer,
  applied once where the finding set is finalised (`findings::build`) so the sign column, the
  lens and the hint always describe the same set. Warnings take the budget before information,
  and the order within a severity does not move between refreshes. Nothing is hidden behind a
  summary line: the cap decides what exists.
- **Dismissal is permanent and per-repository.** `:Jev dismiss` writes the finding's
  content-addressed key to `.git/jev/dismissed.json` (never into the repo tree).
- **Suppression.** A *finding* stays dismissed per repository — `.git/jev/dismissed.json`,
  filtered from every pull (`filter_findings`). A verb is never suppressed: `noise.suppress_after_dismissals`
  is in the settings schema and is not read (PROTOCOL §10).
- **Quiet by default.** `inlay_hints` ships disabled; diagnostics do not run per
  keystroke.
- **Cost transparency.** Every model call logs one line (model, tier, tokens, ms, trigger
  reason) at debug level, and the statusline exposes the counters. Nothing hidden.

## 5. Why it lives in the editor

| | Asking in a chat window | jev-lsp in Neovim |
|---|---|---|
| Context | You select it, usually incompletely | Scope, neighbours, diagnostics, imports, repo state are gathered by the server |
| Latency to first useful token | Full round trip after you finish typing the prompt | Picker already open; the answer is cached work |
| Result format | Prose | `WorkspaceEdit` applied to the exact bytes, or a diagnostic on the exact line |
| Failure mode | You apply something wrong | Version-stamped refusal, post-apply divergence detection |
| Interruption cost | Context switch to another pane and back | None — the affordance is on the line you are on |
| Repeat cost | Same tokens again | Cache hit |
| Review | Read the whole answer | Per-step diff, approve or reject individually |
| Undo | Manual, error-prone | One snapshot restore |

The largest difference is not the quality of the model output. It is that the context
is already where the code is, and the output lands where the code is.

## 6. Non-negotiable interactions

- No keymap may change meaning based on model state. If nothing is available, the menu
  says so; the key still opens the menu.
- No blocking prompt on a code path the user did not invoke.
- `q` on any generated buffer leaves buffers, windows, and files exactly as they were. That is
  literal under the default `surfaces.layout = 'current'` (§2.1), which never adds a window; the
  `float` layout adds one for as long as the report is open, and the `<C-v>` diff preview is a
  split because the user asked for one.
- Every applied edit is reversible by one command.
- The statusline is the only place work is advertised; no spinner text is inserted into a
  buffer the user types in.
