# Working with meta-lsp

A tutorial you can hand to yourself in three months: install it, press four keys, and know at
each step what you should be looking at.

---

## 0. What this is, and the seam you need to know about

meta-lsp is a language server that runs a model over the buffers Neovim has open. It attaches
to files, it answers about files, and it proposes edits **into files**. Findings arrive as
ordinary diagnostics; everything else is on demand.

**Its unit is a document with text.** That single sentence tells you exactly what it serves and
what it does not:

| Situation | Does it help? |
|---|---|
| You have a codebase and are changing it | **Yes** — that is what it was built for (findings, actions, plans, explain, dismiss, undo) |
| You are starting from nothing and have not written a line | **No.** Nothing is open, so there is no document, and no document means no client, no findings, no anchors to propose an edit against |
| You are starting from nothing **and you wrote 20 thrown-away lines to try an API you do not know** | **Yes** — those 20 lines are a document |

That last row is the greenfield seam, and it is the whole answer: **this tool serves existing
code and greenfield-*with-a-spike*.** It cannot help you think, and it will not read the world
for you. But the moment you have typed something — even something you intend to delete — it can
review it, explain it, answer about it, and rewrite it in place.

### The spike loop

This is the from-scratch workflow. It is not a lesser mode; for a new project it is the one
that works.

1. **Write a spike.** A real file, small, that calls the API or tool you are unsure about. It
   does not have to be good; it has to exist and be in the buffer. `~/scratch/probe.py` is fine.
2. **Save it** (`:w`). The analysis runs on save and a warning sign appears on any line it
   wants to talk about.
3. **Ask the question you actually have**, with the spike in front of you:
   `:Meta ask does this call the pagination API correctly?` — the file is the context, so the
   answer is about your code rather than about APIs in general.
4. **Get it reviewed**: `:Meta review` for findings now; `<leader>ma` on a flagged line to have
   it proposed an edit in place; `:Meta explain` on the function to have it explain what you
   wrote.
5. **Keep it or bin it.** If it answered your question, delete the file; if it is the seed of
   the real thing, keep it and let the rest of the tool take over.

What the spike buys you, concretely: the model sees *your* attempt, so it corrects *your*
mistake instead of describing the API in general; and every answer and edit is anchored to text
that exists, so a suggestion you accept actually lands. What it does not buy you: it will not
discover a library for you, read its docs beyond one page, or tell you what to build.

**Two checks that make the rest of this document work.** With a file open:

```vim
:lua =#vim.lsp.get_clients({name = 'meta'})     " 1 means attached; 0 means nothing below will work
:checkhealth meta                               " version, binary, and "no meta client attached" if it is not
```

A scratch buffer (`:enew`, `buftype=nofile`) is deliberately *not* served: the attach pass only
takes buffers that are real files with names. That is also why `:Meta ask` needs a file open —
see §7, first item.

---

## 1. Install, through to the first finding

Each step has something you should see. If you do not see it, jump to §6.

**1. Build.**

```sh
cd /path/to/meta-lsp
cargo build --release
```

→ `target/release/meta-lsp` (the server) and `target/release/meta` (the CLI). No output beyond
Cargo's own.

**2. Point it at a model.** Either the environment, or settings (§4), or both:

```sh
export META_BASE_URL=http://127.0.0.1:8080/v1
export META_MODEL=your-model-name
export META_REVIEW_MODEL=your-model-name    # optional: a second slot for the review tier
```

→ nothing printed. The environment wins over whatever the client sends, so this is the one
setting you can always rely on. `meta-lsp` holds no key of its own: `api_key_env` names an
environment variable, and nothing else.

**3. Load the plugin.** It lives in the repo's `nvim/` directory; add it to `runtimepath` or
point a plugin manager at it, then:

```lua
require('meta').setup({ cmd = { '/path/to/meta-lsp/target/release/meta-lsp' } })
```

→ `setup()` registers the client config, calls `vim.lsp.enable('meta')`, and starts the attach
pass. Nothing appears on screen yet.

**4. Open a code file and check the client attached.**

```vim
:checkhealth meta
```

→ you want these lines:

```
ok    Neovim 0.12.5 (>= 0.12)
ok    server binary: /path/to/meta-lsp (…)
info  enabled = true
ok    attach pass installed on BufReadPost, BufNewFile, BufWinEnter
```

`warn no meta client attached (open a file; the attach pass covers the buffers the FileType
path misses)` means you are in a buffer that is not a file (a terminal, a scratch buffer, a
help page). Open a real file.

**5. Save the file.** (`:w`)

→ a sign appears in the margin on any line the review wants to flag, plus Neovim's diagnostic
virtual text if your config shows it. How long that takes is the model: measured 1.5–7 s
against a cloud endpoint, 5.7–12.4 s against a local 35B on this machine. No popup, no sound.

**6. Get the persistent record of what it is doing.**

```vim
:Meta status
```

→ a JSON blob in the message line. The fields worth knowing: `enabled`, `documents`,
`analysis_in_flight`, `cache`, `budget.{calls_last_minute,calls_last_hour,tokens_used}` and
their `limit_*`, `models` (the endpoints actually in force), `triggers.diagnostics`.

**7. Press the action key on a flagged line.**

```vim
<leader>ma
```

→ the picker opens **instantly** (it reads the cache) and lists actions: one `Fix: <finding>`
per finding in scope, `Fix all findings (N)`, then one per verb — `Harden edge cases`,
`Add type annotations`, `Document`, `Rewrite`, `Add tests`, `Generate`, `Review this`. Picking
one shows a spinner in the message line and eventually applies an edit.

**8. If the menu shows a single disabled entry** saying *"analysing in the background; reopen
the menu in a moment"*, the analysis is still running. Save again or wait for the sign, then
reopen. That string is not an error.

---

## 2. The keys, and the whole command surface

Four keys. Everything else is typed, on purpose: a keymap is an accelerator, not the surface.

| Key | Modes | Same as | Observable result |
|---|---|---|---|
| `<leader>ma` | n, x | — | the action picker; in visual mode the selection is the scope |
| `<leader>mu` | n | `:Meta undo` | the buffer returns to before the last applied edit; `meta: restored 1 buffer(s)` |
| `<leader>mq` | n | `:Meta ask` | a question; the answer opens in a scratch buffer (`q` closes it). A failure or a non-answer is reported in the message line instead |
| `<leader>ms` | n | `:Meta status` | the JSON above |

The default prefix is `<leader>m`; pass `prefix = '<leader>M'` to `setup()` if `<leader>m*` is
taken in your config, as it is in mine.

Seventeen subcommands are reachable by typing (`:Meta <Tab>` completes them):

```
ask [--web] <q>   followup <q>    where <q>     explain        review
plan <goal>       dismiss [id]    undo          hints on|off   usage
session           recompute       status        stop           start
cancel            log
```

`ask --web` is the fetch-enabled form: the answer may ask for **one** https page, which the
server fetches (64 KiB cap, no redirects, https only) and names in the answer as
`_Read: <url>_`. Nothing else in this tool touches the network beyond your model endpoint.

Where the surfaces come from:

| On screen | Produced by | Costs a model call? |
|---|---|---|
| Sign in the margin, diagnostic text | `textDocument/diagnostic`, pulled after the server asks the client to re-pull | no |
| `meta: explain` / `meta: N finding(s) · fix` at a declaration | code lens, one per declaration | no |
| `meta: N finding(s)` badge | inlay hint — **off by default**, `:Meta hints on` | no |
| An explanation in a scratch buffer, streaming | `:Meta explain` | yes, one |
| `meta: explaining — waiting for the model (3s)` in the message line | progress heartbeat while it thinks | — |
| `:Meta status` / `:Meta usage` / `:Meta session` | the server's own counters and log | no |

---

## 3. The five workflows, as sequences

Each step names what you should observe. A step with no observable result means something in
§6 applies.

### 3.1 A finding becomes a fix (and can be taken back)

1. Edit and `:w`. → A sign appears on the flagged line within seconds.
2. Put the cursor there, `<leader>ma`. → Picker opens instantly, first entry `Fix: <finding>`,
   marked preferred.
3. Pick it. → Spinner in the message line; `codeAction/resolve` generates (seconds); the edit
   is applied by Neovim; the sign clears. The edit is a normal buffer change, so `u` works too.
4. Disagree? `<leader>mu`. → `meta: restored 1 buffer(s)`, the buffer is byte-for-byte as it
   was. (`meta: no applied edit to undo` if nothing has been applied, or `meta: the buffer
   changed since that edit; undo refused` if you typed after applying.)
5. The finding is real but not worth fixing now: put the cursor on it and `:Meta dismiss` →
   `meta: dismissed <id> and recorded it in <root>/.git/meta/dismissed.json`. It stops
   appearing in this repository, for this content. Outside a git repository you get a warning
   instead, because there is nowhere to record it.

If the answer cannot be applied you are told why and nothing is written — see the two refused-
edit rows in §6.

### 3.2 Review, explain, hover, lens

- `:Meta review` → re-runs the analysis for this file *now* and prints the findings in the
  message line (`{kind: "review", findings: [...], discarded: N}`). It does **not** repaint the
  sign column — the server asks the client to re-pull on the save path, not on this command —
  so the signs catch up at your next `:w`. Use it to check a file you do not want to write;
  save if you want the margin to show it.
- `:Meta explain` → explains the scope under the cursor (the enclosing declaration) as a
  streamed scratch buffer. `q` closes it; nothing is written to disk.
- `K` (hover) over anything inside that same scope → the explanation again, instantly, from the
  artifact store, as long as the content has not changed. Hover shows nothing before you have
  asked once — it never calls a model.
- `:lua vim.lsp.codelens.run()` on a declaration → runs the lens there: `meta: explain` on a
  clean declaration, `meta: N finding(s) · fix` on one with cached findings, which opens the
  picker at that scope.

### 3.3 Ask

- `<leader>mq`, or `:Meta ask what does this return when the list is empty?` → answered with
  the file you are in as context. With no file open the question stands alone — but see §7.1,
  you need a file open at all for the plugin to have a client to send through.
- `:Meta followup why is this unsafe?` → asks about the finding under the cursor, with the
  finding's own text in the prompt.
- `:Meta where is retry handled?` → answered by grepping locally first, then by the model with a
  bounded number of places.
- `:Meta ask --web which flags does zstd level 3 set?` → the model may request one page; the
  answer is prefixed `_Read: <url>_`. A fetch that fails says so (`fetch_failed`) rather than
  quietly answering from memory.

### 3.4 Plans, and work that spans files

```vim
:Meta plan make the retry loop cancellable
```

→ a plan buffer opens: one line per step, each naming its file, its scope, and what it changes.
In that buffer: `<CR>` applies the step under the cursor, `a` applies every step, `u` takes one
back, `q` closes.

1. `<CR>` on a step → the **server** applies it (not the client) against the content it holds
   at that moment, then the buffer reflects it and the step line says what happened.
2. If the target moved since the plan was built → the step is refused as `stale` instead of
   landing somewhere plausible. Nothing is written.
3. A step that creates a file is marked as such; the edit is one `WorkspaceEdit`, so one `u`
   undoes it.

**A greenfield plan will come back empty.** Steps require anchors that exist in an open
document; "scaffold a new project" has none, so there is no step and the command answers *"the
plan contained no step whose target could be located"*. That is a refusal, not a bug — §7.2 is
what it would take to change it, and it is deliberately not built.

### 3.5 Watching it work, and controlling it

- `:Meta status` → the JSON above; the numbers are the answer to "is it doing anything".
- `:Meta usage` → published findings, files analysed, and what you did with them (applied,
  dismissed, undone), over the session log. Opens as a scratch buffer.
- `:Meta session` → the record itself, newest first, in a scratch buffer. An entry naming a
  place (`review_me.py:5`) takes `<CR>` to jump there. The file is
  `<root>/.git/meta/session.jsonl`.
- Statusline segment, if you want it always visible:

  ```lua
  require('lualine').setup({ sections = { lualine_x = { require('meta.statusline').component } } })
  -- or: vim.o.statusline = '%{%v:lua.require"meta.statusline".component()%}'
  ```

  It polls `meta.status` at most once every 5 s, and only while something is in flight.
- `:Meta stop` → the kill switch: the server stops issuing model calls immediately. `:Meta
  start` turns it back on. `:Meta status` shows `enabled: false` in between.
- `:Meta cancel` → cancels this plugin's outstanding requests and any server-initiated work it
  can see; `meta: cancelled N request(s), M server job(s)`.
- `:Meta recompute` → drops the conclusion cache and re-analyses every open document. First
  thing to try after changing model, prompt, or settings.
- `:Meta hints on|off` → inlay hints for this buffer.
- `:Meta log` → opens Neovim's LSP log, which is where refusals and model errors are written.

---

## 4. Settings that matter

`setup({ settings = { … } })`, under `meta` (either wrapped as `{ meta = { … } }` or flat —
both work). Only the keys a user actually turns:

```lua
settings = {
  enabled = true,                   -- same as :Meta stop, but from config
  models = {
    reason = { base_url = '…', model = '…', timeout_ms = 90000, max_tokens = 8192,
               temperature = 0.0, think = 'off' },  -- actions, plans, explanations
    review = { base_url = '…', model = '…' },       -- findings, post-apply verification
  },
  budget = { max_calls_per_min = 6, max_calls_per_hour = 120,
             max_tokens_per_session = 500000 },
  triggers = { diagnostics = 'save' },   -- 'save' | 'idle' | 'off'  (idle_ms = 1500)
  ambient  = { code_lens = true, inlay_hints = false, diagnostics = true },
  noise    = { max_visible_findings = 5 },
  languages = { overrides = { markdown = { verbs = { 'review' } } }, max_file_bytes = 1048576 },
  log = 'warn',
}
```

- **Two tiers, two endpoints.** `reason` serves actions/plans/explanations; `review` serves
  findings and post-apply verification. They are separate slots even when they point at the
  same server, and they are tuned independently.
- `think = 'off'` sends `chat_template_kwargs: {enable_thinking: false}` and is the default for
  every tier. Measured: with it, 744 ms and a direct answer; with a level, the reasoning tokens
  eat the answer's budget, and one level the endpoint may not even support (`docs/MODEL.md` §1).
  If you set a level, raise `max_tokens` and `timeout_ms` with it.
- **Findings are capped once**, in the server, at `noise.max_visible_findings` (default 5),
  warnings before information. The sign column, the lens count and the hint all read that one
  set, so they cannot disagree.
- Files are skipped, and *say* they were skipped, when: they look binary; they exceed
  `languages.max_file_bytes` (1 MiB) — *"N bytes exceeds the M byte analysis limit"*; or their
  path matches `languages.ignore`. `triggers.diagnostics = 'save'` means nothing is analysed
  until you save.
- `triggers.severity_floor` and `noise.suppress_after_dismissals` exist in the schema and are
  **not read** by this implementation (PROTOCOL §10). Setting them changes nothing.
- No state survives a restart: the cache is content-keyed and evictable, and the only durable
  things are the dismissal file and the session log, both under the repository root.

---

## 5. The CLI: the same core without an editor

```sh
meta explain src/lib.rs:40              # explanation artifact, JSON
meta review src/lib.rs                  # findings, JSON
meta action --verb harden src/lib.rs:40-80
meta plan --goal "make retry cancellable" src/lib.rs
meta status                             # budget, queue and cache
```

- Paths are `path[:line[:col]]` (1-based, as a compiler prints) or `12-20` for a range; `-`
  reads the document from stdin instead of a file.
- `--verb` takes one of `fix fixAll harden types docs rewrite test generate`. `--goal` is
  required for `plan`. `--base-url`, `--model` and `--max-tokens` override per run.
- stdout is exactly one JSON value; stderr carries diagnostics including one cost line per
  model call.
- Exit codes are the contract: `0` success, `1` transport or model failure, `2` usage or
  contract violation, `3` budget exhausted, `4` stale target.

It shares `meta-core` with the server and no state with it. Useful for scripts, and for telling
"the model is bad at this" apart from "the plugin is misbehaving".

---

## 6. Troubleshooting, by symptom

| What you see | What it means | What to do |
|---|---|---|
| `meta: no server attached to this buffer` | No client on this buffer: it is not a file (scratch, terminal, help), or `setup()` never ran | `:checkhealth meta`. Open a real file. Wrapping a scratch buffer's question is §7.1 |
| `warn no meta client attached (open a file; …)` in `:checkhealth meta` | Same thing, from the health check | Open a file; the attach pass covers `BufReadPost`, `BufNewFile`, `BufWinEnter` |
| No sign ever appears | Nothing was analysed | Did you `:w`? (`triggers.diagnostics = 'save'`.) Then `:Meta status` for `enabled` and `documents`, then `:Meta log` for the analysis line and the endpoint in force |
| `:Meta review` returns `findings: []`, no sign | Either the file is genuinely clean, or it was skipped | `:Meta log`: a skip says *"the file looks binary"*, *"N bytes exceeds the M byte analysis limit"*, or *"path matches the ignore pattern …"* |
| `not_implemented` for a command | The server does not serve that name | `:Meta` completion lists the 17 subcommands; the server advertises its 14 commands in the `initialize` result (PROTOCOL §6) |
| `meta: settings applied — reason … · review …` in the log shows an endpoint you did not configure | The environment is overriding your client settings | Unset `META_BASE_URL` / `META_MODEL` / `META_REVIEW_MODEL`, or set them to what you want |
| The model call fails and the answer is empty | The endpoint refused or timed out | `:Meta log`. A reasoning model that spent its whole budget now says so explicitly (`finish_reason=length`) — raise `max_tokens`, or set `think = 'off'` |
| Menu entry disabled: *"analysing in the background; reopen the menu in a moment"* | The cache is cold; the analysis is in flight | Wait for the sign and reopen the menu. `:Meta review` also warms the cache the menu reads, but it does not repaint the signs |
| Menu entry disabled: *"the file changed; reopen the menu to recompute"* | You edited after the analysis | Reopen the menu; it recomputes |
| `over_budget` | A budget ceiling was hit | `:Meta status` prints the counters and limits; raise `budget.*` or wait a minute |
| An edit is refused: *"the proposed change was rejected: the edit covers lines X-Y, outside the scope (lines A-B) the user selected"* | The model answered outside the scope you asked about | Nothing was applied. Narrow or widen the selection and try again |
| An edit is refused with a duplication or anchor reason | The answer named a target that does not occur exactly once, or re-emitted lines it did not consume | Nothing was applied. Retry — the refusal is fed back to the model as a repair attempt first |
| `meta: no finding at the cursor to dismiss` | No meta diagnostic on this line | Put the cursor inside the flagged range, or pass the id: `:Meta dismiss <id>` (it is in the diagnostic's `data.finding_id`) |
| `meta: no applied edit to undo` | Nothing has been applied yet in this session | — |
| `meta: the buffer changed since that edit; undo refused` | You typed after the edit, so restoring would discard it | Undo with `u` instead, or accept the risk deliberately |
| A dismissed finding stays gone after you fixed it | Dismissal is permanent and per repository | Delete the id from `<root>/.git/meta/dismissed.json` |
| Ghost text never appears | By design — inline completion was removed | Use `<leader>ma` or `:Meta ask` |
| Everything stops working at once | The kill switch, or a dead client | `:Meta status` for `enabled`; `:Meta start`; `:checkhealth vim.lsp` |

---

## 7. Known limitations, with the fix I would make

Recorded so they are not rediscovered as bugs. None of these is built.

1. **`:Meta ask` is unreachable with no file open.** The attach pass only takes real, named
   file buffers, so an empty Neovim (or a session with only a scratch buffer) has no `meta`
   client at all, and `:Meta ask` answers `meta: no server attached to this buffer`. Verified:
   with only an unnamed buffer open, `#vim.lsp.get_clients({name='meta'}) == 0`. The server side
   is fine — `meta.ask` explicitly supports a question with no document — it is the transport
   that is missing. *Fix I would make:* when the command has no client, create a stable scratch
   file under `vim.fn.stdpath('run')`, start the client against it behind the scenes, and send
   the question there; it is a real named file, so it passes the attach rule unchanged. Cost, to
   be honest about it: that buffer lives outside any repository, so the question writes no
   session record and no dismissal bookkeeping. Cheap alternative if that matters: document
   "keep one file open" — which is what this section is.
2. **No greenfield plan.** `plan` steps require anchors that resolve in an open document, and a
   step with no usable anchor is dropped, so a from-scratch plan comes back empty with *"the
   plan contained no step whose target could be located"*. *Fix I would make:* keep
   unresolvable steps as `unlocatable` items in the artifact, so the plan buffer becomes a
   checklist you tick yourself, with the apply path unchanged (a step still needs a resolved
   target to be applied). Contract change to PROTOCOL §7, not a small one.
3. **`ask` can read one page, chosen by the model.** 64 KiB, https only, no redirects, requested
   as a bare `FETCH <url>` line. For an unfamiliar API that is often not the page you need, and
   there is no search, no `--help`, no man page, no registry. *Fix I would make:* nothing here —
   a read-only command runner belongs in a shell tool, not in a language server. That is what
   `clank` is for; this tool should keep proposing edits, not executing things.
4. **No repo index, no session memory, no chat pane.** Deliberate, and in PROTOCOL §12: the
   client owns document state, the cache is content-keyed and evictable, and the plan artifact
   is the continuation. Anything else would be a second source of truth.
5. **No ghost text.** Inline completion was removed on 2026-09-19 (`STATUS.md`): generated code
   is asked for rather than offered under the cursor.

---

## 8. Verifying it yourself, and what the numbers are

Everything above is covered by scripts that need no editor, and most need no model:

```sh
python3 verify/stub_model.py &                    # a scripted endpoint on 127.0.0.1:8099
python3 verify/smoke.py                           # 44 end-to-end checks, self-hosting its stub
python3 verify/lsp_client.py --server "$PWD/target/release/meta-lsp" \
    --stub-model-url http://127.0.0.1:8099/v1     # the independent, spec-derived client
python3 verify/queue_test.py                      # a save arriving mid-analysis
python3 verify/plan_test.py                       # plan -> apply -> revert, staleness included
python3 verify/latency.py                         # the editor-driven paths against a model made 2 s slow
nvim --headless -u NONE -l verify/nvim_ui_test.lua
```

With a real model: `verify/quality_eval.py` (is the review *right*; `--think off|low|medium|high`
runs the same fixtures with a reasoning level), `verify/repo_bench.py` (findings per 1000 lines
over a repository), `verify/soak.py` (does the file still parse).

Current state, measured on the checkout this document ships with: `cargo test` **219 passing**,
warning-free; smoke **44/44**; the independent client **28 ok, 0 FAIL**; the plugin's own UI
test **0 failures, 0 skips**; `quality_eval` **4/4** planted defects with **0 findings** on the
two clean files in five of six runs on this endpoint — one run caught 3/4, and that variance
is the honest reading of a six-file fixture set against a model that is not deterministic.
`STATUS.md` carries the fuller table and `docs/VERIFICATION.md` says what is deliberately
unverified.
