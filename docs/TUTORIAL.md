# Working with jev-lsp

The first run, end to end: install it, point it at a decision endpoint, write one rule, save a
file, see the finding, act on it, and watch the second save come back clean. Then the workflow the
rules carry, and where to read more.

`docs/GUIDE.md` is the reference for afterwards — every setting, the rule schema, the CLI,
troubleshooting.

---

## 0. What this is

**Jev is a classifier, not a chat model.** A rule asks one typed question about one line, and Jev
answers with a value and a probability. The rule is your own words plus a deterministic pattern
that names the exact lines, so the pattern is exact and the judgement is a probability that has to
clear the rule's own floor. That is what makes a rule semi-deterministic.

**Its unit is a document with text.** Nothing is open, so there is nothing to analyse:

| Situation | Does it help? |
|---|---|
| You have a codebase and are changing it | **Yes** — findings, actions, plans, explain, dismiss, undo |
| You are starting from nothing and have not written a line | **No.** No document means no client, no findings, no anchors to propose an edit against |
| You are starting from nothing **and you wrote 20 thrown-away lines to try an API you do not know** | **Yes** — those 20 lines are a document |

That last row is the seam: this serves existing code, and greenfield *with a spike*. A rule needs a
file to point at, and a file needs to exist. `docs/GUIDE.md` §8 covers the other half — asking in
the shell with `clank` until there is a file to point at.

Two checks that make the rest of this document work. With a file open:

```vim
:lua =#vim.lsp.get_clients({name = 'jev'})     " 1 means attached; 0 means nothing below will work
:checkhealth jev                               " version, binary, and "no jev client attached" if it is not
```

A scratch buffer (`:enew`, `buftype=nofile`) is not served: the attach pass takes buffers that are
real files with names. `:Jev ask` from one still works while a client is attached to any other
buffer — the plugin falls back to it — but the context is the file the server holds, not the
scratch buffer (`docs/GUIDE.md` §7).

---

## 1. Install, through to the first finding

Each step has something you should see. If you do not see it, `docs/GUIDE.md` §7 has the symptoms.

**1. Get the binaries.** There is no release page yet, so they come from `cargo install`, which
needs a Rust toolchain (1.75 or later):

```sh
cargo install --git https://github.com/makefunstuff/jev-lsp --locked jev-lsp jev
```

→ `jev-lsp` (the server) and `jev` (a CLI) in `~/.cargo/bin`, which is on `PATH` for a normal
toolchain install. `git clone` plus `cargo build --release` produces the same two binaries under
`target/release/`.

**2. Load the plugin.** It owns the attach pass that covers files Neovim cannot identify, the
keymaps and `:Jev`. Point a plugin manager at the repository's `nvim/` directory, or symlink it:

```sh
ln -s /path/to/jev-lsp/nvim ~/.local/share/nvim/site/pack/jev/start/jev
```

```lua
-- in your config; with `jev-lsp` on PATH, the command is the plugin's own default
require('jev').setup({})
```

→ `setup()` registers the client config, calls `vim.lsp.enable('jev')` and starts the attach pass.
Nothing appears on screen yet. Without the plugin, Neovim's own client reaches the same surfaces in
two lines — `vim.lsp.config('jev', { cmd = { 'jev-lsp' }, filetypes = { 'rust' }, root_markers = {
'.git' } })` then `vim.lsp.enable('jev')` — and `filetypes` is then the client's job.

**3. Point the decide tier at an endpoint.** The rules pass asks the decision tier one question per
candidate, and nothing is published until it answers. It is hosted Jev by default
(`https://api.typesafe.ai/v1`, model `jev-latest`), and it needs a key:

```sh
export TYPESAFE_API_KEY=…
```

→ nothing printed. Other routes — OpenCode Zen, OpenRouter, a local System One server — and the
`wire` and `timeout_ms` traps that go with them are in `docs/MODEL.md` §8. The chat tiers (actions,
plans, explanations) are separate and not needed for a finding.

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

`warn TYPESAFE_API_KEY is not set: the default decide endpoint refuses every call` is what an
unconfigured default looks like. `warn no jev client attached (open a file; …)` means you are in a
buffer that is not a file — a terminal, a scratch buffer, a help page.

**5. Write one rule, save, and look.** `.jev/rules/` is read from the repository root, and a rule is
a convention in prose plus the inspection that finds the lines it is about. §3 walks through one;
the shape is:

```jsonc
{ "schema": "jev.rules/1",
  "rules": [
    { "id": "no-unwrap-in-handlers",
      "title": "Unwrap in a request handler",
      "text": "A handler must not unwrap; return the error instead.",
      "severity": "warning",
      "applies_to": ["**/*.rs"],
      "inspection": { "kind": "regex", "pattern": "\\.unwrap\\(\\)" },
      "judgement": { "question": "Is this unwrap reachable from a request handler?",
                     "min_probability": 0.75 } } ] }
```

Now `:w` the file the rule is about.

→ a sign appears in the margin on any line the rule and the decision both accept, plus Neovim's
diagnostic virtual text if your config shows it. How long that takes is the decision model:
measured 1.5–7 s against a cloud endpoint, 5.7–12.4 s against a local 35B on this machine. No
popup, no sound.

If nothing appears, that is information rather than a failure: the ambient pass is the repository's
rules, and a repository that has written none gets nothing. `:Jev inspect` says which of the two it
was — no rules loaded, nothing claiming this file, or the file git calls unchanged.

**6. Act on it.** Put the cursor on the flagged line and press `<leader>ja`.

→ the picker opens from cache and lists actions: one `Fix: <finding>` per finding in scope, `Fix
all findings (N)`, then one per verb — `Harden edge cases`, `Add type annotations`, `Document`,
`Rewrite`, `Add tests`, `Generate`, `Review this`. Picking one shows a spinner in the message line
and applies an edit. Disagree with the edit? `<leader>ju` restores the buffer byte-for-byte.

A single disabled entry saying *"analysing in the background; reopen the menu in a moment"* means
the pass is still running. Wait for the sign and reopen; that string is not an error.

**7. Save again.**

→ the finding is gone from the margin if the fix covered it. A finding you dismissed stays gone: the
dismissal is recorded per repository in `<root>/.git/jev/dismissed.json`, keyed by content, and
filtered out of every pull.

`:Jev status` answers "is it doing anything" with the queue, the budgets and the endpoints in force.
`:Jev usage` counts what was published and what you did with it, over the session record.

---

## 2. The workflow the rules carry

The point of a rule is not that a model read your file. It is that a review you would otherwise do
by hand runs on every save, in the repository, under version control, reviewed like code:

1. **Plan or spec.** You, with coworkers or an LLM, write down what the code has to do.
2. **Rules.** Turn that spec into `.jev/rules/*.json` — one rule per convention or invariant.
3. **Steering.** jev-lsp applies them while you edit by hand, or while a harness generates code, so
   the findings arrive on the line, in the client you already use.
4. **On track.** The codebase keeps matching the requirements, and a harness that reads diagnostics
   picks the steering up.

Three rules, each measured against hosted Jev (`typesafe/jev-1.13`) on the day it was written. The
probabilities are what the model answered; the floor is what turns an answer into a finding.

### 2.1 A team convention: handlers must not unwrap

```jsonc
{ "id": "no-unwrap-in-handlers",
  "title": "Unwrap in a request handler",
  "text": "A handler must not unwrap; return the error instead.",
  "severity": "warning",
  "applies_to": ["**/*.rs"],
  "inspection": { "kind": "regex", "pattern": "\\.unwrap\\(\\)" },
  "judgement": {
    "question": "Is this unwrap reachable from a request handler?",
    "criteria": { "true": "a request can reach it", "false": "test or startup code" },
    "min_probability": 0.75 },
  "verb_hint": "fix" }
```

The pattern finds every `.unwrap()` in the file and cannot tell a test helper from a handler; the
question is what separates them. Measured: a `.unwrap()` on a request-reachable path answered
**0.85–0.91** across runs, and the same call inside `#[cfg(test)]` answered **0.05** on 15 of 15
runs. The finding reads `Unwrap in a request handler — A handler must not unwrap; return the error
instead. — reachable (p=0.90)` in the margin, in the picker, and in a hover in Cursor.

### 2.2 A spec-derived invariant: a public function documents what it can return

Take a line from the spec — *every public function documents the errors and empty values a caller
must handle* — and turn it into an inspection plus one question:

```jsonc
{ "id": "public-functions-document-their-returns",
  "title": "A public function does not document what it can return",
  "text": "The spec says every public function documents the errors and empty values a caller must handle.",
  "severity": "information",
  "applies_to": ["**/*.rs"],
  "inspection": { "kind": "regex", "pattern": "^\\s*pub fn " },
  "judgement": {
    "question": "Does this public function leave a caller unable to know what it can return — an Err it can produce, a None it can produce, or an empty collection it can produce — because its documentation does not say?",
    "criteria": { "true": "the documentation does not say, so a caller cannot know",
                  "false": "the documentation names what it can return" },
    "min_probability": 0.6 },
  "verb_hint": "docs" }
```

Measured on three public functions: **0.80, 0.78, 0.78**, all published.

**The question has to state the violation.** A `true` answer is what publishes, so a question phrased
as the property you *want* — "does this function document its returns?" — publishes nothing: asked
that way, this rule answered `false` for all three functions at every floor, including `0.0`. Write
the question so that `true` means the code is wrong.

### 2.3 Machine-written slop: a comment must explain why, not narrate the next line

This is the rule that fires on generated code, and the argument for the workflow:

```jsonc
{ "id": "comments-explain-why",
  "title": "A comment narrates the code instead of explaining it",
  "text": "A comment states why the code is the way it is; the next line already says what it does.",
  "severity": "warning",
  "applies_to": ["**/*.py"],
  "inspection": { "kind": "regex", "pattern": "^\\s*#\\s*[A-Za-z]" },
  "judgement": {
    "question": "Does this comment restate what the next line does instead of explaining why the code is the way it is?",
    "criteria": { "true": "it restates the operation the next line performs",
                  "false": "it explains a reason, a constraint, or a consequence" },
    "min_probability": 0.6 },
  "verb_hint": "docs" }
```

Measured on three comments: **0.65** (`# Increment the counter for each frame.`, a narration),
**0.67** (`# The upstream API returns 429 with no Retry-After, so back off by hand.`, which explains
*why* — a false positive at this floor), and **0.64** (`# Return the parsed body.`, a narration).
All three answers sit inside a 0.03 band, so no floor separates the narration from the explanation.
That is the honest reading of a rule like this: the pattern is right and the question is too coarse,
which is a rule to sharpen rather than a floor to move (`docs/GUIDE.md` §4 has the measurement
behind that).

### 2.4 Choosing the floor, and what no rules means

Measure the answer's spread before you choose `min_probability`, and put the floor outside it.
Measured against hosted Jev at `temperature: 0.0`: a sharply-posed question answered **0.96–0.97
across 25 runs** (sd 0.0048), while the same endpoint on a question nearer the decision boundary
varied **0.82–0.86 across 8 runs**. A floor at the answer's median turns the endpoint's own noise
into a coin flip: identical input, half the runs publishing. This repository's own rule for unwraps
moved from 0.75 to 0.85 for exactly that reason — at 0.75, 4 of 15 runs published one of its two
lines and not the other.

With no rule files there are no ambient findings. The pass reports `no_rules` rather than a clean
file, and the chat review does not step in: that is a decision, not a gap (`PROTOCOL.md` §12).

---

## 3. Write your first rule

`.jev/rules/` is read from the repository root, in path order, and every file carries the schema:

```sh
mkdir -p .jev/rules
cat > .jev/rules/handlers.json <<'JSON'
{ "schema": "jev.rules/1",
  "rules": [
    { "id": "no-unwrap-in-handlers",
      "title": "Unwrap in a request handler",
      "text": "A handler must not unwrap: a bad request would take the worker down. Return the error instead.",
      "severity": "warning",
      "applies_to": ["**/src/**/*.rs"],
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

`applies_to` is matched against the document's path relative to the repository root, so
`src/**/*.rs` matches a file at the root's `src/`, and a leading `**/` is optional. The full field
table, the two inspection kinds, the lint rules and the measured guidance for `min_probability` are
in `docs/GUIDE.md` §4.

Three things that will otherwise cost you an hour:

- **Editing a rule does not repaint the findings already on screen.** The next pass for that
  document applies it; `:Jev inspect --force` or `:Jev recompute` apply it now. Nothing watches
  `.jev/rules/`.
- **A pass that found nothing says so.** `:Jev inspect` lists `unchanged` (git reports the file
  untouched), `no_rules` (nothing claims this file, or none loaded), and any rule file that failed
  to load with its reason.
- **A broken rule file does not take the pass down.** A file that cannot be read or parsed, or that
  carries another `schema`, is skipped with a reason and the rest still load.

---

## 4. Where to go next

- **`docs/GUIDE.md`** — the reference: surfaces and keys, where a report goes, every setting and
  environment variable, the rule schema, budgets, the CLI, troubleshooting, what lives on disk.
- **`docs/CURSOR.md`** — the same server in Cursor, which needs an extension rather than a setting.
- **`docs/UX.md`** — the surfaces and the decisions behind them: noise policy, approval, the plan
  buffer, why asking a question must not rearrange your windows.
- **`docs/MODEL.md`** — the tiers, the routing, the decision wire, the provider routes, and what
  leaves your machine.
- **`docs/VERIFICATION.md`** — how each claim in these documents is proven, and what is unverified.
- **`README.md`** — the front page, with the install routes for every client.
