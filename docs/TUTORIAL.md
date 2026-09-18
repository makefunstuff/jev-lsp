# Working with meta-lsp

A tutorial: what to press, what to expect, what to do when it does nothing.

The short version. meta-lsp is a language server that runs a model over the buffers you have
open. Findings arrive as ordinary diagnostics — a sign in the margin on the line they are
about. Everything else is on demand: a code-action menu for "do something about this line",
`:Meta ask` for a question, `:Meta explain` for a scope, `:Meta plan` for work that spans
files. There is no chat pane to visit and **no ghost text**: inline completion was removed on
2026-09-19 (`STATUS.md`), so if you want generated code you ask for it.

Two properties worth knowing before you press anything:

* **The server proposes; the client applies.** Edits come back as version-stamped
  `WorkspaceEdit`s that Neovim applies through its own machinery, so `u` and your undo blocks
  keep working. `:Meta undo` exists for the case where you want the last server edit gone
  without thinking.
* **It refuses rather than guesses.** An answer that would reach outside the scope you asked
  about, duplicate lines it did not consume, or land on content that moved since is rejected
  and reported — never applied partially.

---

## 1. Install

```sh
cd /path/to/meta-lsp
cargo build --release        # target/release/meta-lsp (the server) and target/release/meta (the CLI)
```

Wire the plugin in however your config loads plugins — it lives in the repo's `nvim/`
directory. The minimum, with lazy.nvim:

```lua
{
  dir = '/path/to/meta-lsp/nvim',
  name = 'meta',
  lazy = false,
  config = function()
    local bin = '/path/to/meta-lsp/target/release/meta-lsp'
    if vim.fn.executable(bin) ~= 1 then
      vim.notify('meta-lsp binary not built: ' .. bin, vim.log.levels.WARN)
      return
    end
    require('meta').setup({
      -- prefix = '<leader>M',   -- default is '<leader>m'; pick one that does not collide
      -- keymaps = false,        -- install no default keys at all
      cmd = { bin },
      settings = {
        -- PROTOCOL §10. Flattened (`{ budget = … }`) or wrapped (`{ meta = { budget = … } }`)
        -- both work; see §5 for the keys that matter.
        models = {
          reason = { base_url = 'http://127.0.0.1:8080/v1', model = 'your-model' },
          review = { base_url = 'http://127.0.0.1:8080/v1', model = 'your-model' },
        },
      },
    })
  end,
}
```

`setup()` registers the client, enables `vim.lsp.enable('meta')`, starts the plugin's own
attach pass (for files Neovim cannot identify), installs the keymaps, the `:Meta` command, the
code lenses, the statusline function and the undo snapshots.

Then:

```vim
:checkhealth meta          " Neovim version, the plugin config, the binary
:LspLog                    " or :Meta log — the server's own log, which is where refusals go
```

Open a file and confirm a client attached: `:lua =vim.lsp.get_clients({name='meta'})`.

**A repository root is required for some things.** Dismissals and the session record live in
`<root>/.git/meta/` (`dismissed.json`, `session.jsonl`). Outside a git repository the server
still analyses and answers; dismissing a finding tells you why it cannot record it.

## 2. What you see, and where it comes from

| On screen | What put it there | Cost |
|---|---|---|
| Sign column on a finding's line | `textDocument/diagnostic`, pulled by Neovim after the server says "re-pull" | cache read |
| `meta: explain` / `meta: N finding(s) · fix` at a declaration | code lens, one per declaration | cache read |
| `meta: N finding(s)` badge | inlay hint — **off by default** (`:Meta hints on`), because Neovim switches hints per buffer, not per client | cache read |
| The scope's explanation in a scratch buffer | `:Meta explain`, streamed as it arrives | one model call |
| `meta: explaining — waiting for the model (3s)` in the message line | `$/progress` heartbeat while the model is thinking | none |
| Anything in `:Meta status` | the server's counters | none |

Findings are produced by the **review** tier and are capped once, in `findings::build`: at
most `noise.max_visible_findings` (default 5) per file, warnings first. The sign column, the
lens count and the hint all read that same set, so they cannot disagree.

When does an analysis run? `triggers.diagnostics` decides: `"save"` (default) analyses on
`:w`, `"idle"` analyses `triggers.idle_ms` after you stop typing, `"off"` never. A file
skipped by the size, ignore or binary gates is skipped *and says so* in the log rather than
quietly producing nothing.

## 3. The four keys, and the rest by typing

The plugin binds four, and only four: a keymap is an accelerator, and the surface is what
`:Meta` reaches.

| Key (default prefix `<leader>m`) | Command form | What it does |
|---|---|---|
| `<leader>ma` (n, x) | | the action picker — in visual mode the selection *is* the scope |
| `<leader>mu` | `:Meta undo` | restore the buffer as it was before the last applied edit |
| `<leader>mq` | `:Meta ask` | ask a question about the file, or about nothing in particular |
| `<leader>ms` | `:Meta status` | queue, budgets, cache, endpoints — where `:Meta stop` lives |

Everything else is typed. The full subcommand set:

```
:Meta ask [--web] <question>   :Meta followup <question>   :Meta where <question>
:Meta explain                  :Meta review                :Meta plan <goal>
:Meta dismiss [id]             :Meta undo                  :Meta hints on|off
:Meta usage                    :Meta session               :Meta recompute
:Meta status                   :Meta stop                  :Meta start
:Meta cancel                   :Meta log
```

Tab-completion works: `:Meta <Tab>` lists them. `:Meta ask --web <question>` may read **one**
https page to answer, and the artifact names the url it read.

## 4. Workflows

### 4.1 A finding becomes a fix

1. Save the file (`:w`). A warning sign appears on the line — no popup, no sound.
2. Put the cursor on that line and press `<leader>ma`. The picker opens instantly *from
   cache* and lists one action per finding in scope — `Fix: <the finding>` — plus one per verb
   the language allows: `harden`, `types`, `docs`, `rewrite`, `test`, `generate`, `review`
   (`fix` comes from the findings and `explain` is a command, so neither is a menu entry).
   A disabled entry carries its reason (`analyzing…`, `over budget`, `the file changed`).
3. Pick it. `codeAction/resolve` generates the edit (this is the slow part — seconds), the
   summary streams in the message line, and Neovim applies the returned edit. The sign clears.
4. Not what you wanted? `<leader>mu` puts the buffer back.
5. The finding keeps coming back and it is not worth fixing? `:Meta dismiss` records the
   finding's content-addressed id in `<root>/.git/meta/dismissed.json` and it stops appearing
   — in this repository, for this content.

If the answer is refused you will see why, verbatim: *"the proposed change was rejected: the
edit covers lines 1-1, outside the scope (lines 4-9) the user selected"*, or *"the file
changed; reopen the menu to recompute"*, or the repair attempt's reason. Nothing is applied
partially, ever.

### 4.2 Review now, explain a scope, hover

- `:Meta review` re-runs the analysis for this file *now* instead of waiting for the next save.
- `:Meta explain` explains the scope under the cursor — the enclosing declaration — in a
  streamed scratch buffer. `q` closes it; the buffer is `nofile` and nothing is written.
- Hover over anything inside that scope (`K`) shows the explanation again, instantly and
  without a model call, as long as the content has not changed.
- The code lens does the same two things from the left margin:
  `:lua vim.lsp.codelens.run()` on a declaration, or click it.

### 4.3 Ask

- `<leader>mq` / `:Meta ask <question>` — answered from the file you are in, plus what the
  editor pushed for it (imports, the buffers you have been in). With no file open the question
  stands alone.
- `:Meta followup <question>` asks about the finding under the cursor, with the finding's own
  text in the prompt.
- `:Meta where <question>` ("where is retry handled?") is answered by grepping locally first
  and then by the model, with a bounded number of places.

### 4.4 Plans, and work that spans files

```vim
:Meta plan make the retry loop cancellable
```

A plan buffer opens: one line per step, each with its file, scope and what it changes.
`<CR>` applies the step under the cursor, `a` applies every step, `u` takes one back, `q`
closes. Applying is done *by the server* against the content it holds at that moment, so a
step whose target moved is refused as `stale` instead of landing somewhere plausible. Steps
that create files are marked as such; the edit arrives as one `WorkspaceEdit`, so one `u`
undoes it.

### 4.5 Watching it work

- `:Meta status` — documents open, analyses in flight, cache hits/misses, the budget counters
  and limits, the endpoints in force, the trigger mode.
- `:Meta usage` — published findings, files analysed, and what you did with them: applied,
  dismissed, undone, over the session log.
- `:Meta session` — the record itself, newest first, opened in a scratch buffer. An entry that
  names a place takes `<CR>` to jump there.
- A statusline segment, if you want it always visible:

  ```lua
  require('lualine').setup({ sections = { lualine_x = { require('meta.statusline').component } } })
  -- or: vim.o.statusline = '%{%v:lua.require"meta.statusline".component()%}'
  ```

  It polls `meta.status` at most every 5 s, and only while something is in flight.

### 4.6 Taking control

- `:Meta stop` — the kill switch: the server stops issuing model calls immediately and drains
  what is in flight. `:Meta start` turns it back on.
- `:Meta cancel` — cancels every request this plugin has outstanding and any server-initiated
  work it can see.
- `:Meta recompute` — drops the conclusion cache and re-analyses every open document. The
  first thing to try when you have changed model or prompt and want new numbers.
- `:Meta hints on|off` — inlay hints, per buffer.

## 5. Settings that matter

Sent as `settings.meta` (PROTOCOL §10). Only the ones a user actually turns are listed; the
rest are in the protocol.

```lua
settings = {
  enabled = true,                     -- false is the same as :Meta stop, from config
  models = {
    reason = { base_url = '…', model = '…', timeout_ms = 90000, max_tokens = 8192,
               temperature = 0.0, think = 'off' },   -- actions, plans, explanations
    review = { base_url = '…', model = '…' },        -- findings, post-apply verification
  },
  budget = { max_calls_per_min = 6, max_calls_per_hour = 120,
             max_tokens_per_session = 500000, timeout_ms = 30000 },
  triggers = { diagnostics = 'save' },   -- 'save' | 'idle' | 'off'; idle_ms = 1500
  ambient  = { code_lens = true, inlay_hints = false, diagnostics = true },
  noise    = { max_visible_findings = 5 },  -- warnings take the budget first
  languages = { overrides = { markdown = { verbs = { 'review' } } }, max_file_bytes = 1048576 },
  log = 'warn',
}
```

Notes that save an afternoon:

- **Two tiers, and they are deliberately separate slots** even when they point at the same
  server: the review prompt and the action prompt must not share a cache, and they are tuned
  independently.
- `think = 'off'` sends `chat_template_kwargs.enable_thinking = false`. It is the default for
  every tier; a reasoning model is opted *into* per tier. Some gateways ignore it — the local
  `llama.cpp` server honours it.
- The environment wins over the client: `META_BASE_URL`, `META_MODEL`, `META_REVIEW_MODEL`.
- `triggers.severity_floor` and `noise.suppress_after_dismissals` are in the schema but **not
  read** by this implementation. Setting them changes nothing; the finding cap is
  `noise.max_visible_findings`.
- Nothing is remembered across sessions (N9). The cache is keyed by content hash and prompt
  revision, and the only durable state is the dismissal file and the session log, both under
  the repository root.

## 6. The CLI: the same core, no editor

```sh
meta explain src/lib.rs:40          # an explanation, JSON, on stdout
meta review src/lib.rs              # findings, JSON
meta action --verb harden src/lib.rs:40-80
meta plan --goal "make retry cancellable" src/lib.rs
meta status                         # budget and queue
```

Flags: `--verb`, `--goal` (required for their commands), `--base-url`, `--model`,
`--max-tokens`, `-h/--help`, `-V/--version`. `--k v` and `--k=v` both parse. Exit codes are
the contract: `0` success, `1` transport or model failure, `2` usage or contract violation,
`3` budget exhausted, `4` stale target. stdout is exactly one JSON value per run; diagnostics
go to stderr, including the per-call cost line.

It shares `meta-core` with the server and shares no state with it — useful for scripts, hooks,
and for telling "the model is bad at this" apart from "the plugin is misbehaving".

## 7. Troubleshooting

| Symptom | First thing to check |
|---|---|
| No sign ever appears | Did you `:w`? `triggers.diagnostics` defaults to `save`. Then `:Meta status` (`enabled`), then `:Meta log` for the analysis line |
| `no client attached to this buffer` | `:checkhealth meta` — Neovim version, plugin config, binary path |
| Findings stopped after an edit | Dismissed findings stay dismissed: `:Meta dismiss` writes them to `.git/meta/dismissed.json` (delete the id, or the file) |
| Every action is disabled with `analyzing…` | The analysis is in flight or the cache is cold — wait for the sign, or `:Meta review` to wait on it |
| `over_budget` | `:Meta status`: the per-minute/hour counters and their limits are printed there. Raise `budget.*` or wait |
| The model is called but answers with the wrong thing | `:Meta log` prints `settings applied — reason … · review …`, which is the endpoint actually in force |
| A change to model or prompt changed nothing | The answer cache is content-and-prompt keyed: `:Meta recompute` |
| An edit landed that you want gone | `<leader>mu` (`:Meta undo`) — the plugin snapshots the buffers an edit touches |
| The scope is wrong | `scope_source` says which resolver won: `explicit` (the client's treesitter range), `structural`, or `whole_file`. Without a parser for the language, resolution is structural |
| Ghost text never appears | By design: inline completion was removed. Use `<leader>ma` or `:Meta ask` |

`docs/VERIFICATION.md` §10 lists what is deliberately *unverified* — read it before trusting a
number that looks better than the method behind it.

## 8. Watching the harnesses instead of the editor

Everything above is covered by scripts that need no editor, and most need no model:

```sh
python3 verify/stub_model.py &                     # a scripted endpoint on 127.0.0.1:8099
python3 verify/smoke.py                            # 44 end-to-end checks, self-hosting its stub
python3 verify/lsp_client.py --server "$PWD/target/release/meta-lsp" \
    --stub-model-url http://127.0.0.1:8099/v1      # the independent, spec-derived client
python3 verify/queue_test.py                       # a save arriving mid-analysis
python3 verify/plan_test.py                        # plan → apply → revert, staleness included
nvim --headless -u NONE -l verify/nvim_ui_test.lua # the plugin's own surfaces
```

And with a real model: `verify/quality_eval.py` (is the review *right*), `verify/repo_bench.py`
(how much does it produce per 1000 lines), `verify/soak.py` (does the file still parse).
`quality_eval.py` takes `--think off|low|medium|high` to run the same fixtures with a thinking
level, which is how the default was checked rather than argued. `STATUS.md` carries the latest
numbers.

## 9. What it will not do

- **No ghost text.** Removed 2026-09-19, deliberately.
- **No repo-wide autonomy.** One action is one scope. Work across files is a plan you approve
  step by step.
- **No guessing.** Ambiguous anchors, out-of-scope answers and edits that would duplicate
  lines are refused with a reason, at the cost of occasionally doing nothing.
- **No second channel.** stdout is data, stderr is diagnostics, exit codes are the contract;
  in the editor everything is a buffer, a sign, or a message — there is no progress UI to
  babysit and no daemon to run.
