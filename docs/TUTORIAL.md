# Working with jev-lsp

A tutorial you can hand to yourself in three months: install it, press four keys, and know at
each step what you should be looking at.

---

## 0. What this is, and the seam you need to know about

jev-lsp is a language server that runs a model over the buffers Neovim has open. It attaches
to files, it answers about files, and it proposes edits **into files**. Findings arrive as
ordinary diagnostics; everything else is on demand.

This document is the Neovim path, because that is the one with the plugin, the four keys and
`:Jev`. The server itself assumes nothing about the client — `docs/LANGUAGE.md` §1 — and two
clients that are not Neovim drive the same surfaces (`verify/lsp_client.py`, `verify/omp_lsp.sh`).

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

**The other half of that seam is `clank`** (`~/Work/clank`, on `PATH` as `clank`): a unix filter
that takes a prompt and whatever you pipe into it and returns an answer on stdout. It is for the
questions that have no file behind them yet — *what does this error mean, which flag does this
tool take, how is this API shaped* — and it is where an unfamiliar API stops being unfamiliar.
§3.6 is the loop between the two tools, from an empty directory to code this server can work on.

### The spike loop

This is the from-scratch workflow. It is not a lesser mode; for a new project it is the one
that works.

1. **Write a spike.** A real file, small, that calls the API or tool you are unsure about. It
   does not have to be good; it has to exist and be in the buffer. `~/scratch/probe.py` is fine.
2. **Save it** (`:w`). The analysis runs on save and a warning sign appears on any line it
   wants to talk about.
3. **Ask the question you actually have**, with the spike in front of you:
   `:Jev ask does this call the pagination API correctly?` — the file is the context, so the
   answer is about your code rather than about APIs in general.
4. **Get it reviewed**: `:Jev review` for findings now; `<leader>ja` on a flagged line to have
   it proposed an edit in place; `:Jev explain` on the function to have it explain what you
   wrote.
5. **Keep it or bin it.** If it answered your question, delete the file; if it is the seed of
   the real thing, keep it and let the rest of the tool take over.

What the spike buys you, concretely: the model sees *your* attempt, so it corrects *your*
mistake instead of describing the API in general; and every answer and edit is anchored to text
that exists, so a suggestion you accept actually lands. What it does not buy you: it will not
discover a library for you, read its docs beyond one page, or tell you what to build.

**Two checks that make the rest of this document work.** With a file open:

```vim
:lua =#vim.lsp.get_clients({name = 'jev'})     " 1 means attached; 0 means nothing below will work
:checkhealth jev                               " version, binary, and "no jev client attached" if it is not
```

A scratch buffer (`:enew`, `buftype=nofile`) is deliberately *not* served: the attach pass only
takes buffers that are real files with names. That is also why `:Jev ask` needs a file open —
see §7, first item.

---

## 1. Install, through to the first finding

Each step has something you should see. If you do not see it, jump to §6.

**1. Build.**

```sh
cd /path/to/jev-lsp
cargo build --release
```

→ `target/release/jev-lsp` (the server) and `target/release/jev` (the CLI). No output beyond
Cargo's own.

**2. Point it at a model.** Either the environment, or settings (§4), or both:

```sh
export JEV_BASE_URL=http://127.0.0.1:8080/v1
export JEV_MODEL=your-model-name
export JEV_REVIEW_MODEL=your-model-name    # optional: a second slot for the review tier
export JEV_DECIDE_BASE_URL=http://127.0.0.1:8009/v1   # the rules pass; hosted Jev by default
export JEV_DECIDE_MODEL=kev-latest
```

→ nothing printed. The environment wins over whatever the client sends, so this is the one
setting you can always rely on. `jev-lsp` holds no key of its own: `api_key_env` names an
environment variable, and nothing else. The **decision** tier is a separate endpoint with its
own wire: by default it is remote (`https://api.typesafe.ai/v1`, key from `TYPESAFE_API_KEY`),
and it is what the ambient pass asks — so the two lines above are how you keep the file text on
this machine (`docs/MODEL.md` §8).

**3. Load the plugin.** It lives in the repo's `nvim/` directory; add it to `runtimepath` or
point a plugin manager at it, then:

```lua
require('jev').setup({ cmd = { '/path/to/jev-lsp/target/release/jev-lsp' } })
```

→ `setup()` registers the client config, calls `vim.lsp.enable('jev')`, and starts the attach
pass. Nothing appears on screen yet.

**4. Open a code file and check the client attached.**

```vim
:checkhealth jev
```

→ you want these lines:

```
ok    Neovim 0.12.5 (>= 0.12)
ok    server binary: /path/to/jev-lsp (…)
info  enabled = true
ok    client 1: positionEncoding = utf-8, root = …
ok    decide tier: jev-latest at https://api.typesafe.ai/v1 (wire system_one)
ok    attach pass installed on BufReadPost, BufNewFile, BufWinEnter
```

The decide-tier line is the one that tells you where the ambient pass's questions go, and
`warn TYPESAFE_API_KEY is not set: the default decide endpoint refuses every call` is what an
unconfigured default looks like.

`warn no jev client attached (open a file; the attach pass covers the buffers the FileType
path misses)` means you are in a buffer that is not a file (a terminal, a scratch buffer, a
help page). Open a real file.

**5. Save the file.** (`:w`)

→ a sign appears in the margin on any line a **rule** flags, plus Neovim's diagnostic virtual
text if your config shows it. How long that takes is the decision model: measured 1.5–7 s
against a cloud endpoint, 5.7–12.4 s against a local 35B on this machine. No popup, no sound.

If nothing appears, that is information, not a failure: the ambient pass is the repository's
rules, and a repository that has written none gets nothing. `:Jev inspect` says which of the two
it was — no rules loaded, nothing claiming this file, or the file git calls unchanged (§3.7).

**6. Get the persistent record of what it is doing.**

```vim
:Jev status
```

→ a JSON blob in the message line. The fields worth knowing: `enabled`, `documents`,
`analysis_in_flight`, `rules_in_flight`, `cache`, `budget.{calls_last_minute,calls_last_hour,tokens_used}`
and their `limit_*`, `models` (the endpoints actually in force, `decide` included),
`rules.{enabled,loaded,hash,last_pass_ms,candidates,calls}`, `triggers.diagnostics` and
`triggers.rules`.

**7. Press the action key on a flagged line.**

```vim
<leader>ja
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
| `<leader>ja` | n, x | — | the action picker; in visual mode the selection is the scope |
| `<leader>ju` | n | `:Jev undo` | the buffer returns to before the last applied edit; `jev: restored 1 buffer(s)` |
| `<leader>jq` | n | `:Jev ask` | a question; the answer opens in a scratch buffer (`q` closes it). A failure or a non-answer is reported in the message line instead |
| `<leader>js` | n | `:Jev status` | the JSON above |

The default prefix is `<leader>j` — chosen because it is free here, where `<leader>ma` is
Telescope marks, `<leader>mb` is make, and most of the other letters are taken. Pass
`prefix = '<leader>X'` to `setup()` if it collides in your config; nothing else changes.

Eighteen subcommands are reachable by typing (`:Jev <Tab>` completes them):

```
ask [--web] <q>   followup <q>    where <q>     explain        review
inspect [--force] plan <goal>     dismiss [id]  undo           hints on|off
usage             session         recompute     status         stop
start             cancel          log
```

`ask --web` is the fetch-enabled form: the answer may ask for **one** https page, which the
server fetches (64 KiB cap, no redirects, https only) and names in the answer as
`_Read: <url>_`. Nothing else in this tool touches the network beyond your model endpoint.

Where the surfaces come from:

| On screen | Produced by | Costs a model call? |
|---|---|---|
| Sign in the margin, diagnostic text | `textDocument/diagnostic`, pulled after the server asks the client to re-pull | no |
| `jev: explain` / `jev: N finding(s) · fix` at a declaration | code lens, one per declaration | no |
| `jev: N finding(s)` badge | inlay hint — **off by default**, `:Jev hints on` | no |
| An explanation in a scratch buffer, streaming | `:Jev explain` | yes, one |
| `jev: explaining — waiting for the model (3s)` in the message line | progress heartbeat while it thinks | — |
| `:Jev status` / `:Jev usage` / `:Jev session` / `:Jev inspect` | the server's own counters, log, and the rules pass | `inspect` asks the decision tier; the rest, no |

---

## 3. The workflows, as sequences

Each step names what you should observe. A step with no observable result means something in
§6 applies.

### 3.1 A finding becomes a fix (and can be taken back)

1. Edit and `:w`. → A sign appears on the flagged line within seconds.
2. Put the cursor there, `<leader>ja`. → Picker opens instantly, first entry `Fix: <finding>`,
   marked preferred.
3. Pick it. → Spinner in the message line; `codeAction/resolve` generates (seconds); the edit
   is applied by Neovim; the sign clears. The edit is a normal buffer change, so `u` works too.
4. Disagree? `<leader>ju`. → `jev: restored 1 buffer(s)`, the buffer is byte-for-byte as it
   was. (`jev: no applied edit to undo` if nothing has been applied, or `jev: the buffer
   changed since that edit; undo refused` if you typed after applying.)
5. The finding is real but not worth fixing now: put the cursor on it and `:Jev dismiss` →
   `jev: dismissed <id> and recorded it in <root>/.git/jev/dismissed.json`. It stops
   appearing in this repository, for this content. Outside a git repository you get a warning
   instead, because there is nowhere to record it.

If the answer cannot be applied you are told why and nothing is written — see the two refused-
edit rows in §6.

### 3.2 Review, explain, hover, lens

- `:Jev review` → re-runs the analysis for this file *now* and prints the findings in the
  message line (`{kind: "review", findings: [...], discarded: N}`). It does **not** repaint the
  sign column — the server asks the client to re-pull on the save path, not on this command —
  so the signs catch up at your next `:w`. Use it to check a file you do not want to write;
  save if you want the margin to show it.
- `:Jev explain` → explains the scope under the cursor (the enclosing declaration) as a
  streamed scratch buffer. `q` closes it; nothing is written to disk.
- `K` (hover) over anything inside that same scope → the explanation again, instantly, from the
  artifact store, as long as the content has not changed. Hover shows nothing before you have
  asked once — it never calls a model.
- `:lua vim.lsp.codelens.run()` on a declaration → runs the lens there: `jev: explain` on a
  clean declaration, `jev: N finding(s) · fix` on one with cached findings, which opens the
  picker at that scope.

### 3.3 Ask

- `<leader>jq`, or `:Jev ask what does this return when the list is empty?` → answered with
  the file you are in as context. With no file open the question stands alone — but see §7.1,
  you need a file open at all for the plugin to have a client to send through.
- `:Jev followup why is this unsafe?` → asks about the finding under the cursor, with the
  finding's own text in the prompt.
- `:Jev where is retry handled?` → answered by grepping locally first, then by the model with a
  bounded number of places.
- `:Jev ask --web which flags does zstd level 3 set?` → the model may request one page; the
  answer is prefixed `_Read: <url>_`. A fetch that fails says so (`fetch_failed`) rather than
  quietly answering from memory.

### 3.4 Plans, and work that spans files

```vim
:Jev plan make the retry loop cancellable
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

- `:Jev status` → the JSON above; the numbers are the answer to "is it doing anything".
- `:Jev usage` → published findings, files analysed, and what you did with them (applied,
  dismissed, undone), over the session log. Opens as a scratch buffer.
- `:Jev session` → the record itself, newest first, in a scratch buffer. An entry naming a
  place (`review_me.py:5`) takes `<CR>` to jump there. The file is
  `<root>/.git/jev/session.jsonl`.
- Statusline segment, if you want it always visible:

  ```lua
  require('lualine').setup({ sections = { lualine_x = { require('jev.statusline').component } } })
  -- or: vim.o.statusline = '%{%v:lua.require"jev.statusline".component()%}'
  ```

  It polls `jev.status` at most once every 5 s, and only while something is in flight.
- `:Jev stop` → the kill switch: the server stops issuing model calls immediately. `:Jev
  start` turns it back on. `:Jev status` shows `enabled: false` in between.
- `:Jev cancel` → cancels this plugin's outstanding requests and any server-initiated work it
  can see; `jev: cancelled N request(s), M server job(s)`.
- `:Jev recompute` → drops the conclusion cache and re-analyses every open document. First
  thing to try after changing model, prompt, or settings.
- `:Jev hints on|off` → inlay hints for this buffer.
- `:Jev log` → opens Neovim's LSP log, which is where refusals and model errors are written.

### 3.6 Starting from nothing: clank, then a spike, then this

The greenfield loop, end to end, for the case §0 describes — a new thing against APIs you do not
know. Two tools, one handoff, and a clear line between them: **`clank` while the answer is a
paragraph, this server once the answer is code.**

`clank` is the same shape as a shell tool: prompt and context in on argv/stdin, data out on
stdout, breadcrumbs on stderr, exit `0` when every prompt was answered and `1` when one was not
(a truncated or empty answer counts as failure — an answer that hit the token ceiling is not an
answer). Its docs are `~/Work/clank/README.md`, `CHEATSHEET.md`, `PROTOCOL.md` and
`docs/use-cases.md`; it defaults to your local server (`CLANK_MODEL`, `CLANK_BASE_URL` override,
as do `--model`/`--base-url`). If the default endpoint is not listening — the router unloads
idle models, so `:40583` is often down — that is a `connection refused`, not a clank bug: point
`CLANK_BASE_URL` at the router or whichever endpoint is up.

**Step 1 — ask, in the shell, with the material piped in.** No file exists yet, so nothing in
the editor can help; the context is whatever you pipe.

```sh
# what is this thing telling me?
cat build-error.log | clank --thinking off -m "what is the cause, and what is the smallest fix?"

# how is this API shaped? (nothing to pipe, nothing to anchor)
clank --thinking off -m "show the minimal Python call that paginates the foo API, one snippet"

# let it look at the project itself instead of pasting files: four read-only tools
clank --tools -m "where is the transcript cap defined? cite file:line"

# when the answer should be data rather than prose
clank --json-schema @schema.json -m "extract the required config keys" | jq -er .
```

(`--json-schema` needs an endpoint that honours the field the way llama.cpp does. Against an
OpenAI-style gateway that ignores it, the model answers prose and clank exits `1` with
*"final output is not valid JSON"* instead of passing it off — verified on both kinds of
endpoint.)

Observable: exactly one answer on stdout, nothing else; with `--tools` you also get `> tool …` /
`< tool ok (N B)` lines on **stderr**, and `--jsonl` gives you one event per step with a `run`
line first (model, endpoint, argv, thinking) so a trace says what produced it months later.

**Pass `--thinking off` explicitly here.** The two tools differ on the default: this server
sends `enable_thinking: false` unless told otherwise, while clank sends nothing at all unless
told — so a local template that thinks by default will think, and bill for it, without saying
so. It is the same switch in both (`chat_template_kwargs` for off, `reasoning_effort` for a
level), it is portable on the local servers and advisory on a gateway, and the measured reason
to prefer off is in `docs/MODEL.md` §1: with a level, the reasoning tokens eat the answer.

**Step 2 — turn the answer into a file.** Ten to thirty lines that call the API the way you
understood it. It does not need to work; it needs to exist:

```sh
mkdir -p ~/scratch/foo && cd ~/scratch/foo && git init
vim probe.py     # the snippet, adapted to your guess
```

(`git init` is not decoration: dismissals and the session log live in `<root>/.git/jev/`, so a
spike directory without a repository root gets no `:Jev dismiss` and no `:Jev session`.)

**Step 3 — cross the handoff.** `:w` in that file. From here `clank` has nothing to add,
because the question is no longer about the world — it is about *your* text:

```vim
:w                          " the analysis runs; a sign appears on any line worth talking about
:Jev ask is this the right way to paginate, and what happens on a 429?
<leader>ja                  " have it propose the edit in place, or:
:Jev explain               " have it explain what you wrote, in a streamed buffer
:Jev review                " findings for the file now, without saving
```

The difference from step 1 is not the model: it is that the file *is* the context, automatically,
and the answer comes back as an edit that lands on the exact bytes or a diagnostic on the exact
line. Note what the editor hands it for free — imports, the buffers you have been in, the scope
under the cursor — and that `:Jev ask` needs no paste.

**Step 4 — iterate.** Each new unknown goes back to the shell, each new file comes back here:

```sh
clank -c /tmp/foo.jsonl --thinking off -m "now add retry with backoff to that snippet"   # continue a chain
clank --jsonl --thinking off -m "explain the 429 branch" > /tmp/foo.jsonl                # keep it for later turns
```

Then, as the spike becomes the project: `:Jev plan` over files that exist (steps need anchors —
a from-scratch plan is refused, §7.2), findings on every save, `:Jev dismiss` for the ones you
will not fix, `:Jev usage` to see whether any of it is paying off.

| The question | Reach for | Because |
|---|---|---|
| What does this error mean, what does this tool take, how is this API shaped | `clank` | there is no document yet; the context is a pipe, and the answer is prose |
| Where is X defined, in a project I have open | `clank --tools` **or** `:Jev where` | `:Jev where` greps locally first and asks with a bounded number of places, inside the editor — no shell hop |
| Is this call right, what does this function do | `:Jev ask` / `:Jev explain` | the open file is the context, and the answer comes back anchored to it |
| Change this, add a test, harden this | `<leader>ja` | needs a document and a unique anchor; that is the whole point of the trade |
| Do this across three files | `:Jev plan` | one step per file, applied by the server with staleness refusal |

Both keep stdout for data and stderr for diagnostics, both take the same thinking switch, and
both refuse rather than guess: clank exits `1` on a truncated or empty answer rather than
returning half of one, and this server refuses an edit it cannot anchor. That is why they compose
instead of overlapping — the seam between them is *whether a file exists yet*, not quality.

### 3.7 Write a rule: teach it your conventions

The ambient pass runs the repository's rules, so the first rule you write is what turns this from
a general reviewer into *yours*. Rules are data, in `.jev/rules/`, read in path order:

```sh
mkdir -p .jev/rules
cat > .jev/rules/handlers.json <<'JSON'
{ "schema": "jev.rules/1",
  "rules": [
    { "id": "no-unwrap-in-handlers",
      "title": "Unwrap in a request handler",
      "text": "A handler must not unwrap: a bad request would take the worker down. Return the error instead.",
      "severity": "warning",
      "applies_to": ["src/**/*.rs"],
      "inspection": { "kind": "regex", "pattern": "\\.unwrap\\(\\)" },
      "judgement": {
        "question": "Is this unwrap reachable from a request handler, rather than from test or startup code?",
        "criteria": { "true": "a request can reach it", "false": "test or startup code" },
        "min_probability": 0.75 },
      "verb_hint": "fix" } ] }
JSON
```

Then, with a file it claims in a buffer:

```vim
:Jev inspect     " what the pass did: rules considered, candidates found, findings, and every skip
:w               " and on the next save the sign appears by itself
```

What each field decides, in the order you will care about them:

| Field | What it decides |
|---|---|
| `applies_to` | globs over the document path; a rule for `**/*.rs` never sees a Python file |
| `inspection.kind` | `regex` (every matching line) or `absent` (the file is expected to contain the pattern and does not — a licence header, a module declaration) |
| `inspection.pattern` | the candidate finder. It **decides nothing**: it names lines, nothing more |
| `judgement.question` | what the decision tier is asked about each candidate. Ask about the *decision* ("is this reachable from a handler"), never the syntax the regex already matched |
| `judgement.criteria`, `reasons` | passed through to the decision wire unchanged; this question kind wants `{"true": …, "false": …}` |
| `judgement.min_probability` | the floor a `true` must clear. Default 0.5 — a coin flip is not a finding |
| `title`, `text` | the finding's label (clipped at 60 characters) and its detail |
| `verb_hint` | the verb the menu offers first for it: `fix`, `harden`, `types`, `docs`, `rewrite`, `test`, `generate` |

Three things that will otherwise cost you an hour:

- **Editing a rule does not repaint the findings already on screen.** The next pass for that
  document applies it; `:Jev inspect --force` or `:Jev recompute` apply it now
  (`docs/UX.md` §1.1). There is no watcher on `.jev/rules/`.
- **A pass that found nothing says so.** `:Jev inspect` lists `unchanged` (git reports the file
  untouched), `no_rules` (nothing in `.jev/rules/` claims this file, or none loaded), and any
  rule file that failed to load with its reason — so "no finding" never looks like "nothing ran".
- **A broken rule file does not take the pass down.** A file that cannot be read or parsed, or
  that carries another `schema`, is skipped with a reason and the rest still load — a typo shows
  up in the skips rather than as silence.

The semantics are covered by `verify/rules_test.py` (a below-floor answer publishes nothing, N
candidates cost exactly one decision call, an unchanged document costs none), and
`verify/rules_live.lua` proves a rule's finding reaches the sign column on save through the real
plugin.

---

## 4. Settings that matter

`setup({ settings = { … } })`, under `jev` (either wrapped as `{ jev = { … } }` or flat —
both work). Only the keys a user actually turns:

```lua
settings = {
  enabled = true,                   -- same as :Jev stop, but from config
  models = {
    decide = { base_url = 'https://api.typesafe.ai/v1', model = 'jev-latest',
               api_key_env = 'TYPESAFE_API_KEY', wire = 'system_one',
               timeout_ms = 5000, max_tokens = 64 },  -- the rules pass's questions
    reason = { base_url = '…', model = '…', timeout_ms = 90000, max_tokens = 8192,
               temperature = 0.0, think = 'off' },  -- actions, plans, explanations
    review = { base_url = '…', model = '…' },       -- the chat review's findings
  },
  budget = { max_calls_per_min = 6, max_calls_per_hour = 120,
             max_tokens_per_session = 500000 },
  triggers = { diagnostics = 'save',     -- 'save' | 'idle' | 'off'  (idle_ms = 1500)
               rules = { on_save = true, on_idle = true, idle_ms = 1500 } },
  ambient  = { code_lens = true, inlay_hints = false, diagnostics = true },
  rules    = { enabled = true, max_candidates_per_rule = 8, max_files_per_pass = 8 },
  noise    = { max_visible_findings = 5 },
  languages = { overrides = { markdown = { verbs = { 'review' } } }, max_file_bytes = 1048576 },
  log = 'warn',
}
```

- **Three endpoints, and one of them is the ambient path.** `reason` serves
  actions/plans/explanations; `review` answers the chat review's findings (`:Jev review`);
  `decide` answers the rules pass's questions. The three are separate slots even when they point
  at the same server, and they are tuned independently — the decide tier's ceilings are
  deliberately tiny (64 tokens, 5 s) because a decision is one value per question, not prose. The
  check that runs after an edit is not a model call: the server compares the applied bytes
  against its own prediction (`docs/VERIFICATION.md` §11, and nothing parses the result).
- **The decide tier is remote by default**, so on a default install the text of a changed file
  goes to `api.typesafe.ai` on every rules pass. To keep it local, point `models.decide` at a
  System One server (`base_url = 'http://127.0.0.1:8009/v1'`, `model = 'kev-latest'`) or set
  `rules.enabled = false` to end the ambient pass entirely (`docs/MODEL.md` §8). `JEV_BASE_URL`
  does **not** move this tier: it names an OpenAI-compatible chat server, and a decision is not a
  chat. `JEV_DECIDE_BASE_URL` / `JEV_DECIDE_MODEL` do.
- **`wire` picks the path, and the key's name comes from `api_key_env`.** `wire` (or
  `JEV_DECIDE_WIRE`) is `system_one` → `{base_url}/systemone` or `open_router` →
  `{base_url}/alpha/decisions`; anything else is ignored and the wire in force is kept, so a typo
  never silently posts to the wrong path (`:Jev status` shows `models.decide.wire`). The API key
  is read from the variable **named by** `api_key_env` — `TYPESAFE_API_KEY` by default, with no
  environment override for the name — so a hosted provider needs either that variable exported or
  `api_key_env` changed in settings. `docs/MODEL.md` §8 has the worked recipe.
- `rules.enabled = false` also returns the ambient path to the `review` tier, which is what this
  server did before rules existed — and which is *also* remote by default.
- `think = 'off'` sends `chat_template_kwargs: {enable_thinking: false}` and is the default for
  every tier. Measured: with it, 744 ms and a direct answer; with a level, the reasoning tokens
  eat the answer's budget, and one level the endpoint may not even support (`docs/MODEL.md` §1).
  If you set a level, raise `max_tokens` and `timeout_ms` with it.
- **Findings are capped once**, in the server, at `noise.max_visible_findings` (default 5),
  warnings before information. The sign column, the lens count and the hint all read that one
  set, so they cannot disagree — and it is also why a rule edit waits for the next pass
  (§3.7).
- Files are skipped, and *say* they were skipped, when: they look binary; they exceed
  `languages.max_file_bytes` (1 MiB) — *"N bytes exceeds the M byte analysis limit"*; or their
  path matches `languages.ignore`. `triggers.diagnostics = 'save'` governs the *review* pass;
  `triggers.rules.on_save` (default true) governs the rules pass, and `rules.max_files_per_pass`
  bounds how many documents one idle pass covers.
- `triggers.severity_floor` and `noise.suppress_after_dismissals` exist in the schema and are
  **not read** by this implementation (PROTOCOL §10). Setting them changes nothing.
- No state survives a restart: the cache is content-keyed and evictable, and the only durable
  things are the dismissal file and the session log, both under the repository root.

---

## 5. The CLI: the same core without an editor

```sh
jev explain src/lib.rs:40              # explanation artifact, JSON
jev review src/lib.rs                  # findings, JSON
jev action --verb harden src/lib.rs:40-80
jev plan --goal "make retry cancellable" src/lib.rs
jev inspect src/lib.rs [--force]       # the repository's rules, with the counts and skips
jev status                             # budget, queue and cache
```

- Paths are `path[:line[:col]]` (1-based, as a compiler prints) or `12-20` for a range; `-`
  reads the document from stdin instead of a file.
- `--verb` takes one of `fix fixAll harden types docs rewrite test generate`. `--goal` is
  required for `plan`; `--force` is only for `inspect` and skips the git-changed-set check.
  `--base-url` and `--model` override per run and move **every** tier, the decision tier
  included — so `--base-url` at a stub is how `jev inspect` is exercised without a cloud
  endpoint — while `--max-tokens` applies to the chat tiers.
- `jev inspect` prints the same body the LSP command `jev.inspect` returns: `findings`,
  `considered`, `candidates` and `skipped`. It is the same code the ambient pass runs, which is
  the whole point — a CLI that disagreed with the server about a rule would be worse than none.
- stdout is exactly one JSON value; stderr carries diagnostics including one cost line per
  model call.
- Exit codes are the contract: `0` success, `1` transport or model failure, `2` usage or
  contract violation, `3` budget exhausted, `4` stale target.

It shares `jev-core` with the server and no state with it. Useful for scripts, and for telling
"the model is bad at this" apart from "the plugin is misbehaving".

---

## 6. Troubleshooting, by symptom

| What you see | What it means | What to do |
|---|---|---|
| `jev: no server attached to this buffer` | No client on this buffer: it is not a file (scratch, terminal, help), or `setup()` never ran | `:checkhealth jev`. Open a real file. Wrapping a scratch buffer's question is §7.1 |
| `warn no jev client attached (open a file; …)` in `:checkhealth jev` | Same thing, from the health check | Open a file; the attach pass covers `BufReadPost`, `BufNewFile`, `BufWinEnter` |
| No sign ever appears | Nothing was analysed | Did you `:w`? (`triggers.diagnostics = 'save'`.) Then `:Jev status` for `enabled` and `documents`, then `:Jev log` for the analysis line and the endpoint in force |
| No sign ever appears, and `:Jev log` says the pass had nothing to run | The ambient pass is the repository's rules, and this repository has none that claim this file | `:Jev inspect` lists the skips (`no_rules`, `unchanged`, a rule file that failed to load); write one as in §3.7, or set `rules.enabled = false` and use `:Jev review` |
| You edited a rule and the findings on screen did not change | The display slot is keyed by content, language and the cap, not by rules, and nothing watches `.jev/rules/` | `:Jev inspect --force` for this buffer, or `:Jev recompute` for every open document — or just save (§3.7, `docs/UX.md` §1.1) |
| `:Jev inspect` reports `unchanged` and no findings | git reports the file untouched since HEAD, so the pass skipped it | That is the point of the check; `--force` inspects it anyway |
| The ambient pass fails with `model_error` / `contract_error` | The *decision* tier did not answer, or answered something unreadable | `:Jev log`, then check `models.decide` and `TYPESAFE_API_KEY` (`JEV_DECIDE_BASE_URL` does not come from `JEV_BASE_URL`) |
| `:Jev review` returns `findings: []`, no sign | Either the file is genuinely clean, or it was skipped | `:Jev log`: a skip says *"the file looks binary"*, *"N bytes exceeds the M byte analysis limit"*, or *"path matches the ignore pattern …"* |
| `not_implemented` for a command | The server does not serve that name | `:Jev` completion lists the 18 subcommands; the server advertises its 15 commands in the `initialize` result (PROTOCOL §6) |
| `jev: settings applied — reason … · review …` in the log shows an endpoint you did not configure | The environment is overriding your client settings | Unset `JEV_BASE_URL` / `JEV_MODEL` / `JEV_REVIEW_MODEL`, or set them to what you want |
| The model call fails and the answer is empty | The endpoint refused or timed out | `:Jev log`. A reasoning model that spent its whole budget now says so explicitly (`finish_reason=length`) — raise `max_tokens`, or set `think = 'off'` |
| Menu entry disabled: *"analysing in the background; reopen the menu in a moment"* | The cache is cold; the analysis is in flight | Wait for the sign and reopen the menu. `:Jev review` also warms the cache the menu reads, but it does not repaint the signs |
| Menu entry disabled: *"the file changed; reopen the menu to recompute"* | You edited after the analysis | Reopen the menu; it recomputes |
| `over_budget` | A budget ceiling was hit | `:Jev status` prints the counters and limits; raise `budget.*` or wait a minute |
| An edit is refused: *"the proposed change was rejected: the edit covers lines X-Y, outside the scope (lines A-B) the user selected"* | The model answered outside the scope you asked about | Nothing was applied. Narrow or widen the selection and try again |
| An edit is refused with a duplication or anchor reason | The answer named a target that does not occur exactly once, or re-emitted lines it did not consume | Nothing was applied. Retry — the refusal is fed back to the model as a repair attempt first |
| `jev: no finding at the cursor to dismiss` | No jev diagnostic on this line | Put the cursor inside the flagged range, or pass the id: `:Jev dismiss <id>` (it is in the diagnostic's `data.finding_id`) |
| `jev: no applied edit to undo` | Nothing has been applied yet in this session | — |
| `jev: the buffer changed since that edit; undo refused` | You typed after the edit, so restoring would discard it | Undo with `u` instead, or accept the risk deliberately |
| A dismissed finding stays gone after you fixed it | Dismissal is permanent and per repository | Delete the id from `<root>/.git/jev/dismissed.json` |
| Ghost text never appears | By design — inline completion was removed | Use `<leader>ja` or `:Jev ask` |
| Everything stops working at once | The kill switch, or a dead client | `:Jev status` for `enabled`; `:Jev start`; `:checkhealth vim.lsp` |

---

## 7. Known limitations, with the fix I would make

Recorded so they are not rediscovered as bugs. None of these is built.

1. **`:Jev ask` is unreachable with no file open.** The attach pass only takes real, named
   file buffers, so an empty Neovim (or a session with only a scratch buffer) has no `jev`
   client at all, and `:Jev ask` answers `jev: no server attached to this buffer`. Verified:
   with only an unnamed buffer open, `#vim.lsp.get_clients({name='jev'}) == 0`. The server side
   is fine — `jev.ask` explicitly supports a question with no document — it is the transport
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
   a read-only command runner belongs in a shell tool, not in a language server. That is exactly
   what `clank` is for (§3.6, step 1); this tool should keep proposing edits, not running things.
4. **No repo index, no session memory, no chat pane.** Deliberate, and in PROTOCOL §12: the
   client owns document state, the cache is content-keyed and evictable, and the plan artifact
   is the continuation. Anything else would be a second source of truth.
5. **No ghost text.** Inline completion was removed on 2026-09-19 (`STATUS.md`): generated code
   is asked for rather than offered under the cursor.

---

## 8. Verifying it yourself, and what the numbers are

Everything above is covered by scripts that need no editor, and most need no model:

```sh
# everything at once — one supervised stub, then every row, with a summary block
bash verify/run-suite.sh /tmp/suite.log            # NVIM_ONLY=1 / REFUSE_IF_BUSY=1 / NVIM_BINS

python3 verify/stub_model.py &                    # or one harness at a time: a scripted
                                                  # endpoint on 127.0.0.1:8099
python3 verify/smoke.py                           # 44 end-to-end checks, self-hosting its stub
python3 verify/rules_test.py                      # the rules pass: inspections, gates, skips
python3 verify/lsp_framing_test.py                # the test client's own stdio framing
python3 verify/lsp_client.py --server "$PWD/target/release/jev-lsp" \
    --stub-model-url http://127.0.0.1:8099/v1     # the independent, spec-derived client
python3 verify/queue_test.py                      # a save arriving mid-analysis
python3 verify/plan_test.py                       # plan -> apply -> revert, staleness included
python3 verify/latency.py                         # the editor-driven paths against a model made 2 s slow
nvim --headless -u NONE -l verify/nvim_ui_test.lua
nvim --headless -u NONE -l verify/rules_live.lua  # a rule's finding reaching the sign column
bash verify/omp_lsp.sh                            # OMP, a client that is not Neovim and not ours
```

`verify/rules_live.lua` and the other Lua harnesses need the decision tier pointed at the stub
too (`JEV_DECIDE_BASE_URL=http://127.0.0.1:8099/v1`, `JEV_DECIDE_MODEL=stub-model`) — the ambient
pass is the rules pass, so a harness that only sets `JEV_BASE_URL` watches every save reach for
the default cloud endpoint. `verify/run-suite.sh` does all of that for you, and supervises the
stub while it does.

With a real endpoint — all three are runnable whenever one is reachable, and the suite reports
them as `?` when none is configured:

```sh
export OPENROUTER_API_KEY="$(cat ~/.omp/agent/openrouter.key)"   # the value
export JEV_API_KEY_ENV=OPENROUTER_API_KEY                        # the NAME, for the chat tiers
python3 verify/quality_eval.py --base-url https://openrouter.ai/api/v1 --model google/gemini-2.5-flash-lite
python3 verify/real_model.py   --base-url https://openrouter.ai/api/v1 --model google/gemini-2.5-flash-lite
python3 verify/soak.py         --base-url https://openrouter.ai/api/v1 --model google/gemini-2.5-flash-lite --rounds 2
```

`quality_eval.py` answers "is the review right" (`--think off|low|medium|high` runs the same
fixtures with a reasoning level); `real_model.py` the end-to-end loop; `soak.py` "does the file
still parse". Each exits non-zero when no model was reached. Latest runs on
`google/gemini-2.5-flash-lite`: **quality_eval 3/4 recall, 3/3 precision, 0 findings on both clean
files** (a control run caught 4/4 — the miss is run-to-run variance); **real_model** one finding in
0.8 s and a `state=ready` resolve in 0.7 s; **soak 10/12 applied, 2 left the file unparseable**,
which is the model's anchor granularity and not a server check — nothing in `crates/` parses the
result (`docs/VERIFICATION.md` §7, §11).

Current state, measured on the checkout this document ships with: `cargo test` **274 passing**
(49 `jev` + 179 `jev-core` + 46 `jev-lsp`), warning-free; `verify/rules_test.py` **45/45**;
`verify/rules_live.lua` **0 failures, 0 skips** on Neovim 0.12.5 and 0.12.1; smoke **44/44**
(three consecutive full-table runs); the independent client **32 ok, 0 FAIL**;
`verify/omp_lsp.sh` **0 failures**; `verify/lsp_framing_test.py` **9/9**; the plugin's own UI test
**0 failures, 0 skips**. `STATUS.md` carries the fuller table and `docs/VERIFICATION.md` says what
is deliberately unverified.
