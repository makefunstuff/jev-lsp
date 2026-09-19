# Model layer

## 1. Tiers

Three tiers, each an independently configured endpoint. Any tier may be local or remote; the
router does not care.

| Tier | Purpose | Model shape | Latency target | Called from |
|---|---|---|---|---|
| `decide` | The rules pass's questions — one value per candidate | a **decision** model (System One), no prose | background, no user wait; `timeout_ms` 5000 | the rules pass (§2), `jev.inspect` |
| `reason` | Actions, plans, explanations | 7–32B instruct | p50 < 2 s (resolve) | `codeAction/resolve`, `jev.plan` |
| `review` | Findings (the chat review), `jev.review` | 7–32B instruct, different prompt | background, no user wait | worker, `jev.review` |

`reason` and `review` are deliberately separate endpoint slots even when they point at the
same server: a finding review and a rewrite must not share a prompt template version, and
operators tune them independently.

**`decide` is not a chat tier, and that is the point.** It does not speak
`chat/completions` and it does not generate prose: the request is a *state* plus a numbered set
of questions, and the response is one value per question with a probability
(`{model, state, questions} → {model, answers, usage}`). That is why the ambient pass can afford
to run on every save — a decision costs a few dozen tokens where a review costs thousands — and
why the tier has its own key shape, `{wire, base_url, model, api_key_env, timeout_ms, max_tokens,
temperature, think}` with `timeout_ms` 5000 and `max_tokens` 64 rather than the chat tiers'
90 000/8192 (PROTOCOL §10). `wire` names the path appended to `base_url`: `system_one`
(`/systemone`) or `open_router` (`/alpha/decisions`). `JEV_DECIDE_BASE_URL`,
`JEV_DECIDE_MODEL`, `JEV_DECIDE_WIRE` and `JEV_DECIDE_TIMEOUT_MS` set those four from the
environment; `JEV_DECIDE_WIRE` accepts `system_one`/`systemone` and `open_router`/`openrouter`
(trimmed, case-insensitive) and **ignores anything else**, keeping the wire in force — a typo must
not silently post to the wrong path, which is indistinguishable from a dead endpoint. An
unparseable or zero `JEV_DECIDE_TIMEOUT_MS` is ignored the same way, keeping the 5000 ms default:
a typo must not turn the ceiling into zero, which would fail every call instantly and look like an
outage. The key's variable *name* is `api_key_env` (default `TYPESAFE_API_KEY`) and is
config-only; only its value comes from the environment. There is deliberately **no
`Tier::Decide`** in the code: `Config::tier()` answers the chat tiers, and a decision is a
different protocol, so `Config::decision()` answers this one.

Config is configuration-layer, not code (PROTOCOL §10 `models`). The chat tiers take
`{base_url, model, api_key_env, timeout_ms, max_tokens, temperature, think}`. `think` mirrors
the CLI's reasoning control — `off` sends `chat_template_kwargs.enable_thinking = false`, a
level sends `reasoning_effort`. Every tier defaults to `think = "off"` — `TierConfig::default()`
for the chat tiers, `DecisionTierConfig::default()` for the decide tier, and nothing overrides
either — so a thinking model is opted *into* per tier rather than out.

**Why `off` is the default, measured** (2026-09-19, `deepseek/deepseek-v4-flash` through the
omp auth gateway, and the local `llama.cpp` server):

| Setting | What happened |
|---|---|
| `off` | `chat_template_kwargs: {enable_thinking: false}` → 744 ms, 9 completion tokens, a real answer, **zero** reasoning tokens |
| `low` / `high` | accepted, and the reasoning tokens consume the whole budget: with a 32-token ceiling all 32 went to `reasoning_tokens` and `content` came back `null` — the "empty answer" failure `parse_response` has to name. Over a real ceiling on the local server the same shape spent **47 s producing 16 tokens and no answer** (docs/VERIFICATION.md §7) |
| `medium` | **rejected**: `502 upstream_error — Thinking effort medium is not supported by deepseek/deepseek-v4-flash. Supported efforts: low, high, max`. Every call fails, so an analysis produces nothing at all |

The supported effort set is a property of the endpoint and the model, not of the levels this
config offers, and an unsupported one fails loudly rather than falling back — which is the
right shape (`:Jev log` shows `model call failed: POST <url>: <cause>`: the whole `anyhow`
chain, so a timeout reads as a timeout rather than as the URL alone). Two consequences worth
knowing: this server's jobs are narrow and fully specified (locate an anchor, emit one JSON
object), which is where thinking buys least; and a level spent against a fixed ceiling *removes*
the answer rather than improving it. Raise `max_tokens` and `timeout_ms` with any level.

## 2. Routing

```
trigger -> verb -> tier
```

| Trigger | Verb | Tier |
|---|---|---|
| rules pass — the ambient path (`save`, idle, `jev.inspect`) | — | `decide` |
| save / idle analysis, **when `rules.enabled` is false** | — | `review` |
| `codeAction/resolve` | fix, harden, types, docs, rewrite, test, generate | `reason` |
| `codeAction/resolve` | explain, review | `reason` |
| `jev.review` (the "Review this" action, or `:Jev review`) | — | `review` |

No dynamic routing heuristics. A table, visible in one place, overridable per verb in
config.

**The post-apply check is not a model call.** For an edit the server applied (a plan step), the
server compares the new bytes against its own prediction and publishes an `ERROR` diagnostic on a
mismatch (PROTOCOL §8). A resolved code action is applied by the client, records no prediction, and
is not compared. It does not parse the result, and nothing in `crates/` does
(`docs/VERIFICATION.md` §11).

**The ambient row is the one that changed.** With rules on — the default — the pass that runs on
save asks the `decide` tier one question per candidate and never touches a chat tier; the chat
review runs only when it is asked for (`jev.review`) or when rules are off. There is no
fallback from one to the other: a repository with no rules gets no ambient findings and the pass
says `no_rules` (PROTOCOL §12). The generative tiers are what only they can do — edits, plans,
explanations — and the findings they produce are labelled `review` where the rules pass's are
`rules` (PROTOCOL §9).

**Language is a second dimension, and it has a floor.** The resolved language may select a
prompt flavour and (via `languages.overrides`) a different tier — but when the language is
`unknown`, or has no override, the row above still applies with the `generic_text` prompt.
Routing can refine; it can never decline. This is N10 in `PROTOCOL.md` §1: no language,
filetype, or parser may decide *whether* something is served, only *how*.

## 3. Context builder

Deterministic, ordered, and bounded. Built by `context.rs` from the document store and the
filesystem; the model never chooses what it sees.

| Slice | Budget | Notes |
|---|---|---|
| Scope text | the whole enclosing symbol, hard-capped at 400 lines | per `docs/LANGUAGE.md` §4: treesitter node, else a structural block, else the whole file. The chosen strategy is reported as `scope_source` |
| Heading | file path, language, symbol signature, and the enclosing type chain | cheapest orientation |
| Neighbours | the two preceding and two following top-level items, signatures only | local conventions |
| Diagnostics | current findings in scope, with severities | stops the model contradicting the linter |
| Imports | the file's import block, verbatim | naming and dependency awareness |
| Repo | `AGENTS.md` / `CLAUDE.md` / `.jev/context.md` if present, verbatim, capped | durable project rules |
| Git | last commit subject for the touched file, `git diff --stat` for the working tree | what is in flight |

Excluded by default: whole files, other open buffers, the repository tree, chat history.
Each of those costs tokens to add ambiguity. Every slice is included only when the verb
lists it, and `context_hash` — part of the cache key — is the hash of the assembled
context, so a cache hit means the model would have seen byte-identical input.

## 4. Output contracts

The model emits **content and anchors**. It never emits ranges or versions; the server
computes those. This is what makes N5 in PROTOCOL §1 enforceable rather than aspirational.

### 4.1 Edit contract

```jsonc
{ "schema": "jev.edit/1",
  "summary": "one line, <= 72 chars",     // shown in the picker preview, never as the title
  "rationale": "2-4 sentences",           // shown in the diff view
  "replacements": [
    { "anchor": { "kind": "function", "name": "retry", "match": "pub fn retry(" },
      "replacement": "…entire replacement text for that node…" }
  ],
  "new_files": [
    { "path": "tests/retry_cancel.rs", "content": "…" }
  ]
}
```

Rules the server enforces after parsing:

- Each `anchor.match` must appear **exactly once** in the current document text. Zero
  matches or two matches → the replacement is rejected; the server retries once with the
  ambiguity reported back, then gives up. It never guesses.
- `kind` is one of `function`, `method`, `struct`, `class`, `impl`, `mod`, `file`,
  `statement`. `file` means whole-file replacement, permitted only under
  `max_lines_whole_file` (default 200).
- Replacements must not overlap after resolution.
- The resolved `WorkspaceEdit` is stamped with the document version recorded at request
  time, then re-checked (PROTOCOL §8).

### 4.2 Findings contract

```jsonc
{ "schema": "jev.findings/1",
  "findings": [
    { "anchor": { "kind": "statement", "match": "let f = File::open(p)?;" },
      "severity": "warning",                         // information | warning
      "label": "unchecked error path",                // <= 60 chars, deterministic input to the title
      "detail": "…",
      "verb_hint": "fix" }
  ]
}
```

Severity `error` is not accepted from this contract — it is reserved for server-detected
divergence (PROTOCOL §9).

### 4.3 Plan contract

```jsonc
{ "schema": "jev.plan/1",
  "goal": "…",
  "steps": [
    { "title": "…", "rationale": "…", "verb": "harden",
      "anchors": [ { "kind": "function", "name": "retry", "match": "pub fn retry(" } ] }
  ]
}
```

A plan step contains no edit. Edits are produced when the step is applied, against the
version live at that moment — which is why a plan is still valid after you edit around it.

## 5. Repair

Implemented in `crates/jev-lsp/src/engine.rs`: one model call, plus at most
`MAX_REPAIR_ATTEMPTS` (= 2) repair calls, for either kind of rejected answer.

Two failures are repaired, sharing one budget:

* the response does not satisfy its contract (not JSON, or missing required fields);
* the response cannot be applied to this document — an anchor that occurs zero or several
  times, or a replacement that repeats lines it did not consume, which would duplicate them.

The second kind was specified here and missing from the implementation for most of this
work. Measured: a real model produced it in a substantial share of soak runs, and a second,
differently-worded complaint converts a meaningful part of them. Two attempts rather than one
because a model that answers with the wrong shape tends to answer with it again.

1. Parse the response against the verb's contract.
2. On failure, re-prompt with the **parser's own complaint** and the rejected answer quoted
   back, verbatim.
3. The original material is re-sent unchanged, so the model is not asked to work from a
   summary of what it was already given.
4. Still failing → `Failure::Contract`, which resolves the action with no edit and one
   `window/showMessage` naming the reason.

Never repaired silently, never retried for a timeout, never retried for a transport error.
**Every repair attempt is a separate budgeted call** — it takes its own permit, so a repair
cannot slip past the per-minute cap (`a_repair_call_is_budgeted_like_any_other_call`).

Why it exists, from a real model: against DeepSeek, roughly one run in six violated the
contract — a prose answer with no JSON at all, or the schema echoed back with its field
names as values. The complaint-based retry fixes both shapes, and the loop is bounded so a
model that is simply wrong costs one extra call and then fails visibly rather than looping.

## 6. Determinism

Required for a harness to be learnable:

- `temperature = 0` for the `reason` tier on the fast paths that produce titles and
  findings, and `0.0` for the `decide` tier by default — a decision is a probability, and it
  should not move between two runs over the same state. It still moves with how close the
  *question* sits to the decision boundary: hosted Jev answered 0.96–0.97 across 25 runs on a
  sharply-posed question and 0.82–0.86 across 8 on one nearer the boundary, which is why a
  rule's `min_probability` belongs outside the measured spread (`docs/TUTORIAL.md` §3.7).
- Title construction is a pure function of `(verb, scope name, finding label)`; the model
  supplies the label, never the whole title.
- Identical `context_hash` + identical prompt template version ⇒ byte-identical output is
  expected, and a cold-cache repeat is part of the golden test suite. Divergence is
  recorded as a finding against us, not tolerated.

## 7. Budgets

Accounted in `budget.rs`, checked before the call, incremented after:

- per-minute and per-hour call caps, per-session token cap (PROTOCOL §5)
- a cost line per call: `model tier tokens_in tokens_out ms trigger` at debug level, and
  the same numbers surfaced by `:Jev status`

Exhaustion is a state, not an error. The user sees `over_budget` on the action and a
statusline counter; nothing pops up twice.

**Decision calls have their own per-minute cap.** A rules pass takes one permit per *document*,
and it comes from `budget.max_decisions_per_min` (default 60) — a separate window from the chat
tiers' `budget.max_calls_per_min` (6), so a decision no longer spends the chat minute and a sweep
of a workspace is bounded by 60 files a minute instead of dying at six. The two caps exist for
different reasons: one chat call can be a rewrite costing thousands of tokens, while a decision is
~500 tokens in and ~29 out, ≈$0.00002 and 0.3–0.6 s. `0` means "no decision calls", exactly as it
does for `max_calls_per_min`; the session token cap applies to decisions too; and the chat windows
are untouched. `jev.status.budget` reports `decisions_last_minute` with
`limit_decisions_per_minute` beside the chat counters, so the number a reader sees is the one that
applies.

## 8. Local-first

Default configuration assumes a local OpenAI-compatible server **for the chat tiers**. Remote
endpoints are opt-in per tier, and `api_key_env` names an environment variable — keys are never
written to config, artifacts, or logs. Prompt text and response bodies are never persisted
outside the conclusion cache, and the cache stores parsed conclusions, not raw model traffic.

**The decide tier is the exception, and it is worth being blunt about.** Its default is *remote*:
`wire = "system_one"`, `base_url = "https://api.typesafe.ai/v1"`, model `jev-latest`, key from
`TYPESAFE_API_KEY`. So on a default install, **the text of a changed file leaves the machine on
every rules pass** — that is what a decision is made of (PROTOCOL §9: the file head, the
candidates and the rules are the state), and it happens on save without anything being asked
for. Two ways to keep it local, both one setting:

```jsonc
// a local System One server — same wire, your machine
{ "models": { "decide": { "base_url": "http://127.0.0.1:8009/v1", "model": "kev-latest" } } }

// or no ambient pass at all: the rules pass is the only ambient path, so this ends it
{ "rules": { "enabled": false } }
```

`JEV_DECIDE_BASE_URL` / `JEV_DECIDE_MODEL` / `JEV_DECIDE_TIMEOUT_MS` do the same from the
environment, and `JEV_BASE_URL` deliberately does **not** move this tier: it names an
OpenAI-compatible chat server, and a decision is not a chat. `JEV_DECIDE_TIMEOUT_MS` exists for a
hosted cold start — the endpoint's own p50 is ~0.3 s and 25 measured calls never came near the
5000 ms default — and a value that does not parse, or parses to zero, is ignored rather than
lowering the ceiling to nothing. Turning rules off returns the ambient path to the `review` tier
(§2) — which is also remote by default, so a reader who wants *nothing* leaving the machine should
point `models.reason` and `models.review` at a local endpoint too.

**A hosted provider other than the default needs two things, and one of them is easy to miss.**

```sh
# the path is selected by the wire, not guessed from the host: without this the request would
# POST {base}/systemone at OpenRouter and miss
export JEV_DECIDE_BASE_URL=https://openrouter.ai/api
export JEV_DECIDE_WIRE=open_router          # -> https://openrouter.ai/api/alpha/decisions
export JEV_DECIDE_MODEL=<model>

# the key is read from the variable *named by* `api_key_env`, which defaults to
# TYPESAFE_API_KEY and has no environment override of its own — so export it under that name,
# or set models.decide.api_key_env in config. Any other variable name is simply not read.
export TYPESAFE_API_KEY=<key>
```

An unrecognised `JEV_DECIDE_WIRE` is ignored rather than coerced, so a typo leaves the previous
wire in force; `jev.status` (`models.decide.wire`) and the server's `settings applied` log line
are where you see which one is actually in force.
