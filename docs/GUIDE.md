# Usage guide

The reference for a returning reader. `docs/TUTORIAL.md` is the first run, end to end; this file is
what you open afterwards: the surfaces and the keys, where a report goes, the settings and
environment variables, the rule schema, budgets, the CLI, troubleshooting, and what the server
writes to disk.

## 1. Surfaces and keys

Four keys, and every surface they reach. Everything else is typed, because a keymap is an
accelerator and not the surface.

| Key | Modes | Same as | Observable result |
|---|---|---|---|
| `<leader>ja` | n, x | — | the action picker; in visual mode the selection is the scope |
| `<leader>ju` | n | `:Jev undo` | the buffer returns to before the last applied edit; `jev: restored 1 buffer(s)` |
| `<leader>jq` | n | `:Jev ask` | a question; the answer opens in a scratch buffer (`q` closes it). A failure or a non-answer is reported in the message line instead |
| `<leader>js` | n | `:Jev status` | the status JSON in the message line |

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


## 2. Where a report goes

Asking a question must not rearrange the windows around it, so the placement of every generated
buffer is one setting, read when a surface opens rather than at `setup`:

```lua
require('jev').setup({ surfaces = { layout = 'current' } })   -- the default
```

| value | what it does |
|---|---|
| `current` | **the default.** The report takes the buffer in the window you are already in: the window count, the sizes and every buffer are untouched, and the file you left stays loaded as the alternate buffer, so `q` and `<C-^>` both come back to it. This is what `:Jev inspect`, `:Jev explain`, `:Jev ask`, `:Jev followup`, `:Jev usage`, `:Jev plan` and `:Jev session` do |
| `float` | a rounded floating window over the code; the code stays visible. The one layout that adds a window while it is open (dismissed with `q` or `<Esc>`) |
| `split` | the old `sbuffer` behaviour, for code and report side by side |

A value that is not one of the three notifies at ERROR and keeps the previous layout —
`jev: surfaces.layout = "window" is not a layout (current|float|split); keeping current` — rather
than falling back silently, so nobody believes they asked for a split and got something else.

`status`, `review`, `recompute`, `dismiss`, `undo`, `hints`, `cancel`, `start` and `stop` are
messages rather than buffers: they open nothing. `:Jev log` is `hide edit`, the same window as
before. The picker's diff preview is a real split, because a side-by-side diff is what was asked
for.

In Cursor the same decision is `jev.artifacts.viewColumn`, default `active`: `active` opens the
answer as a document in the current editor group, `beside` adds a group to the right, and `output`
writes the body to *Output → Jev* and opens no document. Four commands report to that channel
whatever the setting says, because their whole answer is a value rather than prose: `jev.inspect`,
`jev.status`, `jev.recompute` and `jev.revert`.

## 3. Settings and environment

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
  ambient  = { diagnostics = true },
  rules    = { enabled = true, defaults = true, max_candidates_per_rule = 8, max_files_per_pass = 8 },
  noise    = { max_visible_findings = 5 },
  languages = { overrides = { markdown = { verbs = { 'review' } } }, max_file_bytes = 1048576 },
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
  goes to `api.typesafe.ai` on every rules pass. This machine runs the same model through OpenCode
  Zen instead (`wire = 'system_one'`, `base_url = 'https://opencode.ai/zen/v1'`,
  `model = 'jev-1.13'`, `timeout_ms = 15000`); OpenRouter (`wire = 'open_router'`,
  `base_url = 'https://openrouter.ai/api'`, `model = 'typesafe/jev-1.13'`) is the alternative, and
  its price is visible from its API. To keep it local, point `models.decide` at a System One server
  (`base_url = 'http://127.0.0.1:8009/v1'`, `model = 'kev-latest'`) or set `rules.enabled = false`
  to end the ambient pass entirely (`docs/MODEL.md` §8). `JEV_BASE_URL` does **not** move this
  tier: it names an OpenAI-compatible chat server, and a decision is not a chat.
  `JEV_DECIDE_BASE_URL` / `JEV_DECIDE_MODEL` do. `opencode-go` (`…/zen/go/v1`) carries no Jev.
- **`wire` picks the path, and the key's name comes from `api_key_env`.** `wire` (or
  `JEV_DECIDE_WIRE`) is `system_one` → `{base_url}/systemone` or `open_router` →
  `{base_url}/alpha/decisions`; anything else is ignored and the wire in force is kept, so a typo
  never silently posts to the wrong path (`:Jev status` shows `models.decide.wire`). Zen needs
  `system_one` — its `open_router` path is a 404. The API key is read from the variable **named
  by** `api_key_env` — `TYPESAFE_API_KEY` by default — and `JEV_DECIDE_API_KEY_ENV` names a
  different one from the environment (`JEV_API_KEY_ENV` does the same for the chat tiers; both
  take a name, never a key, and ignore an empty value). `JEV_DECIDE_TIMEOUT_MS` raises the 5000 ms
  ceiling, and a hosted route needs it: at 5000 a client-attached call failed with
  `timeout: global` while the CLI on the same route succeeded, and 15000 answered `ok`. A value
  that does not parse, or parses to zero, is ignored. `docs/MODEL.md` §8 has the worked recipe.
- `rules.enabled = false` also returns the ambient path to the `review` tier, which is what this
  server did before rules existed — and which is *also* remote by default.
- `think = 'off'` sends `chat_template_kwargs: {enable_thinking: false}` and is the default for
  every tier. Measured: with it, 744 ms and a direct answer; with a level, the reasoning tokens
  eat the answer's budget, and one level the endpoint may not even support (`docs/MODEL.md` §1).
  If you set a level, raise `max_tokens` and `timeout_ms` with it.
- **Findings are capped once**, in the server, at `noise.max_visible_findings` (default 5),
  warnings before information. The sign column, the lens count and the hint all read that one
  set, so they cannot disagree — and it is also why a rule edit waits for the next pass
  (§4).
- Files are skipped, and *say* they were skipped, when: they look binary; they exceed
  `languages.max_file_bytes` (1 MiB) — *"N bytes exceeds the M byte analysis limit"*; or their
  path matches `languages.ignore`. `triggers.diagnostics = 'save'` governs the *review* pass;
  `triggers.rules.on_save` (default true) governs the rules pass, and `rules.max_files_per_pass`
  bounds how many documents one idle pass covers.
- `rules.defaults` (default true) runs the set shipped in the binary alongside `.jev/rules/`
  (PROTOCOL §9): a finding says which set it came from, a file of yours **shadows** the shipped
  rule with the same `id`, and `false` runs the repository's rules alone. `jev rules init` writes
  the shipped files out to read and edit (§4, §6).
- Ten keys are in the schema and are **not read** by this implementation (PROTOCOL §10):
  `ambient.code_lens`, `ambient.inlay_hints`, `auto_apply.fix`, `auto_apply.fixAll`,
  `budget.timeout_ms`, `log`, `triggers.severity_floor`, `noise.suppress_after_dismissals`, and
  `languages.overrides.<lang>.tier` / `.prompt`. Setting one changes nothing. They are absent
  from the example above for that reason.
- No state survives a restart: the cache is content-keyed and evictable, and the only durable
  things are the dismissal file and the session log, both under the repository root.

---

### Environment variables

The server reads its endpoints from the environment it is started with. A client can send the same
values through `workspace/configuration` under the `jev` section; the environment wins over the
client's payload, which is what makes a stub or a local server work without editing a client config.

| variable | what it sets |
|---|---|
| `JEV_BASE_URL` | the chat tiers' base URL (`reason` and `review`) |
| `JEV_MODEL` | the `reason` tier's model |
| `JEV_REVIEW_MODEL` | the `review` tier's model |
| `JEV_API_KEY_ENV` | the *name* of the variable holding the chat tiers' key |
| `JEV_DECIDE_BASE_URL` | the decide tier's base URL |
| `JEV_DECIDE_MODEL` | the decide tier's model |
| `JEV_DECIDE_WIRE` | `system_one` (`/systemone`) or `open_router` (`/alpha/decisions`) |
| `JEV_DECIDE_TIMEOUT_MS` | the decide tier's ceiling in ms (default 5000; a hosted route needs more) |
| `JEV_DECIDE_API_KEY_ENV` | the *name* of the variable holding the decide tier's key |

An empty or whitespace value is ignored everywhere; an unparseable `JEV_DECIDE_TIMEOUT_MS`, or one
that parses to zero, keeps the value in force rather than lowering the ceiling to nothing.

## 4. Rules: the `jev.rules/1` document

**A rule set ships in the binary, and it runs where the repository's own have nothing to say.**
With `rules.defaults` on (the default) a pass runs both sources together, and which source a
finding came from is on the finding: `rule_source` in the result, and the ` [builtin]` /
` [repository]` marker `:Jev inspect` prints after each label. The shipped rules are ordinary files
in the same format, and `jev rules init` writes them into `.jev/rules/` under `prose-…json` /
`code-…json` so you can read and edit them; a file of yours **shadows** the shipped rule with the
same `id` rather than running beside it, and `rules.defaults: false` runs the repository's rules
alone. When the shipped set is what carried a pass, `:Jev inspect` says so in a skip line of its
own (`default_rules`).

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
| `applies_to` | globs over the document's path **relative to the workspace root** (the absolute path when there is no root), so `src/**/*.rs` matches a file at the root's `src/`, and a leading `**/` is optional; a rule for `**/*.rs` never sees a Python file |
| `inspection.kind` | `regex` (every matching line) or `absent` (the file is expected to contain the pattern and does not — a licence header, a module declaration) |
| `inspection.pattern` | the candidate finder. It **decides nothing**: it names lines, nothing more |
| `judgement.question` | what the decision tier is asked about each candidate. Ask about the *decision* ("is this reachable from a handler"), never the syntax the regex already matched |
| `judgement.criteria`, `reasons` | passed through to the decision wire unchanged; this question kind wants `{"true": …, "false": …}` |
| `judgement.min_probability` | the floor a `true` must clear. Default 0.5; put it outside the answer's measured spread, not on it (below) |
| `title`, `text` | the finding's label (clipped at 60 characters) and its detail |
| `verb_hint` | the verb the menu offers first for it: `fix`, `harden`, `types`, `docs`, `rewrite`, `test`, `generate` |

**The question has to state the violation.** A `true` answer is what publishes, so `true` must
mean *the code is wrong* — ask "is this unwrap reachable from a handler?", not "is this unwrap
confined to tests?". Measured on the spec-derived rule in `docs/TUTORIAL.md` §2.2: phrased as the
property you want ("does this function document what it can return?"), it answered `false` for all
three candidates at every floor, `0.6` and `0.0` alike, and published nothing; rephrased as the
defect ("does this function leave a caller unable to know what it can return?") it answered `0.80`,
`0.78`, `0.78` and published all three. A `false` answer is invisible through `:Jev inspect`, so a
backwards question looks exactly like a rule that never fires.

**Choosing `min_probability`.** Measure the answer's spread before you choose the floor, and put
the floor outside it with room to spare — not on it. Measured against hosted Jev
(`typesafe/jev-1.13`, the tier's `temperature: 0.0`): a sharply-posed question answered **0.96–0.97
across 25 real runs** (median 0.97, sd 0.0048); the same endpoint on a question nearer the
decision boundary varied **0.82–0.86 across 8 runs**. A floor at the answer's median turns that
noise into a coin flip: identical input publishes on half the runs and not the other half, with
nothing in the report naming the odd run. If the spread straddles the floor you want, sharpen the
question (name the deciding property, add the separating criterion, or split it) and measure
again — no value of `min_probability` stabilises a boundary-straddling question. A `false` answer
is invisible through `:Jev inspect`, which publishes only what clears the floor; read the negative
side by posting the request directly, or with the floor at `0.0` (which still hides a `false`).

**A floor separates only what the tier separates.** One decision call carries every candidate of a
document, and on the tier this machine wires a call carrying two or more candidates of one rule is
answered at one shared probability whatever the code says: the tier reads the lines once, and a
flat answer cannot tell a violation from its neighbour. So the floor has to sit **above** that flat
band, and a floor under it publishes the band rather than the class. Two consequences for a rule
author: the rule publishes from documents whose candidates the tier separates and stays silent on
documents it pulls together, and one call per document, the property the cost case rests on
(`README.md`), is what buys those document-level verdicts at all. The bands and the runs behind
each shipped floor are in that rule's own directory
(`crates/jev-core/default_rules/prose/README.md`, `crates/jev-core/default_rules/code/README.md`,
which also states what a lower floor would publish if the pass ever asked one candidate per call);
`STATUS.md`'s open questions carry that alternative.

On this repository: `no-unwrap-outside-tests` over `crates/jev-lsp/src/server.rs` (two `.unwrap()`
calls on literal URLs — an invariant, not a defect) answered in a **0.75–0.79** band; at the 0.75
floor it shipped with, **4 of 15 runs published one line and not the other**. It ships **0.85**
now, where none of 15 runs published either line, while the fixture that must fire answered
**0.97–0.98** and published on 15 of 15 at both floors. The false band's top is 0.79 and the true
sample's bottom is 0.97; 0.85 sits in that gap, deliberately below its midpoint (0.88), because a
floor that is too high costs a missed defect.

Three things that will otherwise cost you an hour:

- **Editing a rule does not repaint the findings already on screen.** The next pass for that
  document applies it; `:Jev inspect --force` or `:Jev recompute` apply it now
  (`docs/UX.md` §1.1). There is no watcher on `.jev/rules/`.
- **A pass that found nothing says so.** `:Jev inspect` lists `unchanged` (git reports the file
  untouched), `default_rules` (the shipped set is carrying this pass because `.jev/rules/` holds
  nothing of this repository's own), `no_rules` (no rule from either source claims this file, or
  the pass has nothing to run at all), and any rule file that failed to load with its reason — so
  "no finding" never looks like "nothing ran".
- **A broken rule file does not take the pass down.** A file that cannot be read or parsed, or
  that carries another `schema`, is skipped with a reason and the rest still load — a typo shows
  up in the skips rather than as silence.

The semantics are covered by `verify/rules_test.py` (a below-floor answer publishes nothing, N
candidates cost exactly one decision call, an unchanged document costs none), and
`verify/rules_live.lua` proves a rule's finding reaches the sign column on save through the real
plugin.

---

## 5. Budgets

Accounted in `budget.rs`, checked before the call and incremented after. Exhaustion is a state, not
an error: the action reports `over_budget` and the statusline shows the counters.

| key | default | what it bounds |
|---|---|---|
| `budget.max_calls_per_min` | 6 | chat calls per minute |
| `budget.max_calls_per_hour` | 120 | chat calls per hour |
| `budget.max_decisions_per_min` | 60 | decision calls per minute, in their own window |
| `budget.max_tokens_per_session` | 500000 | tokens per session, across both |

A rules pass takes one permit per document, so `max_decisions_per_min` is a cap on documents per
minute, not on candidates: one decision is ~500 tokens in and ~29 out, about $0.00002 and 0.3–0.6 s
on a hosted tier. `0` means "no calls" for both caps; the session token cap applies to both tiers.

What a pass actually costs over a repository — the sweep, the same file both ways, the measured
prices, and the local routes — is `docs/MODEL.md` §7 (measured) and §8 (local), with the short
version on the front page of `README.md`.

## 6. The CLI

```sh
jev explain src/lib.rs:40              # explanation artifact, JSON
jev review src/lib.rs                  # findings, JSON
jev action --verb harden src/lib.rs:40-80
jev plan --goal "make retry cancellable" src/lib.rs
jev inspect src/lib.rs [--force]       # the repository's rules, with the counts and skips
jev rules init [--dir <dir>] [--force] # write the shipped rule set out to read and edit
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
  the CLI and the server share the rules code, so they cannot disagree about a rule.
- `jev rules init` materialises the shipped set (PROTOCOL §11) into `.jev/rules/`: one file per
  shipped file, named by its group (`prose-lists-end-in-etc.json`), idempotent and
  non-clobbering. A file already there that differs is **refused by name** rather than
  overwritten; `--force` replaces it, and `--dir` writes somewhere else.
- **The decide tier's key variable is nameable from the shell**: `api_key_env` defaults to
  `TYPESAFE_API_KEY`, and `JEV_DECIDE_API_KEY_ENV` points it at another variable
  (`JEV_DECIDE_API_KEY_ENV=OPENCODE_API_KEY OPENCODE_API_KEY=… jev inspect …`). It takes a *name*,
  never a key, and an empty value keeps the name in force. `JEV_DECIDE_BASE_URL`,
  `JEV_DECIDE_MODEL`, `JEV_DECIDE_WIRE` and `JEV_DECIDE_TIMEOUT_MS` do the rest of the pointing; a
  hosted route needs the raised ceiling.
- stdout is exactly one JSON value; stderr carries diagnostics including one cost line per
  model call.
- Exit codes are the contract: `0` success, `1` transport or model failure, `2` usage or
  contract violation, `3` budget exhausted, `4` stale target.

It shares `jev-core` with the server and no state with it. Useful for scripts, and for telling
"the model is bad at this" apart from "the plugin is misbehaving".

---

## 7. Troubleshooting

| What you see | What it means | What to do |
|---|---|---|
| `jev: no server attached to this buffer` | No client on this buffer: it is not a file (scratch, terminal, help), or `setup()` never ran | `:checkhealth jev`. Open a real file. `:Jev ask` from a scratch buffer still works while a client is attached to any other buffer — the plugin falls back to it — but the context is the file the server holds, never the scratch buffer |
| `warn no jev client attached (open a file; …)` in `:checkhealth jev` | Same thing, from the health check | Open a file; the attach pass covers `BufReadPost`, `BufNewFile`, `BufWinEnter` |
| No sign ever appears | Nothing was analysed | Did you `:w`? (`triggers.diagnostics = 'save'`.) Then `:Jev status` for `enabled` and `documents`, then `:Jev log` for the analysis line and the endpoint in force |
| No sign ever appears, and `:Jev log` says the pass had nothing to run | No rule from either source claims this file, or the shipped set is off (`rules.defaults = false`) | `:Jev inspect` lists the skips (`default_rules`, `no_rules`, `unchanged`, a rule file that failed to load); write one as in §4, or set `rules.enabled = false` and use `:Jev review` |
| You edited a rule and the findings on screen did not change | The display slot is keyed by content, language and the cap, not by rules, and nothing watches `.jev/rules/` | `:Jev inspect --force` for this buffer, or `:Jev recompute` for every open document — or save (§4, `docs/UX.md` §1.1) |
| `:Jev inspect` reports `unchanged` and no findings | git reports the file untouched since HEAD, so the pass skipped it | That is the point of the check; `--force` inspects it anyway |
| The ambient pass fails with `model_error` / `contract_error` | The *decision* tier did not answer, or answered something unreadable | `:Jev log`, then check `models.decide` and `TYPESAFE_API_KEY` (`JEV_DECIDE_BASE_URL` does not come from `JEV_BASE_URL`) |
| `:Jev review` returns `findings: []`, no sign | Either the file is genuinely clean, or it was skipped | `:Jev log`: a skip says *"the file looks binary"*, *"N bytes exceeds the M byte analysis limit"*, or *"path matches the ignore pattern …"* |
| `:Jev inspect` finds it, but the sign column never shows it | The pass runs on save and on `jev.inspect`; a client that never sends `didSave` never runs one, and an empty diagnostics pull looks exactly like a clean file | Save the buffer (or send `didSave`); `docs/UX.md` §1.1 |
| `not_implemented` for a command | The server does not serve that name | `:Jev` completion lists the 18 subcommands; the server advertises its 15 commands in the `initialize` result (PROTOCOL §6) |
| `:Jev status` shows an endpoint you did not configure, then the right one moments later | `config_ready` is false: the client's configuration pull has not been merged yet, and status reports the built-in defaults rather than waiting for it | Nothing to do; re-run `:Jev status` (model work does wait for the pull, so it never runs against the defaults) |
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

## 8. Starting from nothing

When no file exists yet, the context is a pipe and the answer is prose — that is outside this
server. Use a shell ask-tool for that step (for example `clank`: prompt on argv/stdin, data
on stdout, exit `1` on a truncated or empty answer). Once a file exists, hand off here:

```sh
mkdir -p ~/scratch/foo && cd ~/scratch/foo && git init
# write a short probe file, then:
```

```vim
:w                          " save runs the rules pass
:Jev ask is this the right way to paginate, and what happens on a 429?
<leader>ja                  " propose an edit in place
:Jev explain               " explain what you wrote, streamed
:Jev review                " findings for the file now, without saving
```

`git init` matters: dismissals and the session log live under `<root>/.git/jev/`, so a spike
directory without a repository root gets no `:Jev dismiss` and no `:Jev session`.

| The question | Reach for | Because |
|---|---|---|
| What does this error mean, what does this tool take, how is this API shaped | a shell ask-tool | no document yet; context is a pipe; answer is prose |
| Where is X defined, in a project I have open | `:Jev where` | greps locally first, asks with a bounded set of places, inside the editor |
| Is this call right, what does this function do | `:Jev ask` / `:Jev explain` | the open file is the context; the answer is anchored to it |
| Change this, add a test, harden this | `<leader>ja` | needs a document and a unique anchor |
| Do this across three files | `:Jev plan` | one step per file, applied by the server with staleness refusal |

First-run walkthrough: `docs/TUTORIAL.md`. Surfaces and schema detail: §1–§4 above.


## 9. What lives on disk

| path | what it is |
|---|---|
| `.jev/rules/*.json` | the repository's rules, read in path order |
| `<root>/.git/jev/session.jsonl` | the session record: one line per command and per analysis |
| `<root>/.git/jev/dismissed.json` | dismissed findings, keyed by content |
| `~/.cache/jev/<workspace-id>/` | the daemon's cache, when the daemon exists (phases 1–2 keep it in memory) |

The record and the dismissal file sit under `.git/`, so they survive a restart, survive a buffer
being closed, and never appear in `git status`. The conclusion cache does not survive: it is keyed
by content hash and is evictable at any time.
