# Model layer

## 1. Tiers

Three tiers, each an independently configured endpoint. Any tier may be local or remote; routing
does not depend on which.

| Tier | Purpose | Model shape | Latency target | Called from |
|---|---|---|---|---|
| `decide` | The rules pass's questions — one value per candidate | a **decision** model (System One), no prose | background, no user wait; `timeout_ms` 5000 | the rules pass (§2), `jev.inspect` |
| `reason` | Actions, plans, explanations | 7–32B instruct | p50 < 2 s (resolve) | `codeAction/resolve`, `jev.plan` |
| `review` | Findings (the chat review), `jev.review` | 7–32B instruct, different prompt | background, no user wait | worker, `jev.review` |

`reason` and `review` are separate endpoint slots even when they point at the same server: a
finding review and a rewrite must not share a prompt template version, and operators tune them
independently.

**`decide` is not a chat tier.** It does not speak
`chat/completions` and it does not generate prose: the request is a *state* plus a numbered set
of questions, and the response is one value per question with a probability
(`{model, state, questions} → {model, answers, usage}`). That is why the ambient pass can afford
to run on every save: a decision costs a few dozen tokens where a review costs thousands. It is
also why the tier has its own key shape, `{wire, base_url, model, api_key_env, timeout_ms,
max_tokens, temperature, think}` with `timeout_ms` 5000 and `max_tokens` 64 rather than the chat
tiers' 90 000/8192 (PROTOCOL §10). Three routes are documented, all measured: the built-in default
(`https://api.typesafe.ai/v1`, model `jev-latest`, wire `system_one`); a local System One server
(`base_url = "http://127.0.0.1:8009/v1"`, `model = "kev-latest"`); and a hosted gateway carrying the
same model — OpenCode Zen (`https://opencode.ai/zen/v1`, model `jev-1.13`, wire **`system_one`**:
its `/alpha/decisions` path 404s, so the wire is not optional) or OpenRouter
(`https://openrouter.ai/api`, model `typesafe/jev-1.13`, wire `open_router`). §8 compares them.
`wire` names the path appended to `base_url`: `system_one`
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
level sends `reasoning_effort`. Every tier defaults to `think = "off"`: `TierConfig::default()`
for the chat tiers, `DecisionTierConfig::default()` for the decide tier, and nothing overrides
either. A thinking model is therefore opted *into* per tier rather than out.

**Why `off` is the default, measured** (2026-09-19, `deepseek/deepseek-v4-flash` through the
omp auth gateway, and the local `llama.cpp` server):

| Setting | What happened |
|---|---|
| `off` | `chat_template_kwargs: {enable_thinking: false}` → 744 ms, 9 completion tokens, a real answer, **zero** reasoning tokens |
| `low` / `high` | accepted, and the reasoning tokens consume the whole budget: with a 32-token ceiling all 32 went to `reasoning_tokens` and `content` came back `null` — the "empty answer" failure `parse_response` has to name. Over a real ceiling on the local server the same shape spent **47 s producing 16 tokens and no answer** (docs/VERIFICATION.md §7) |
| `medium` | **rejected**: `502 upstream_error — Thinking effort medium is not supported by deepseek/deepseek-v4-flash. Supported efforts: low, high, max`. Every call fails, so an analysis produces nothing at all |

The supported effort set is a property of the endpoint and the model, not of the levels this
config offers, and an unsupported one fails loudly rather than falling back (`:Jev log` shows
`model call failed: POST <url>: <cause>`: the whole `anyhow` chain, so a timeout reads as a
timeout rather than as the URL alone). Two consequences: this server's jobs are narrow and fully
specified (locate an anchor, emit one JSON object), which is where thinking buys least; and a
level spent against a fixed ceiling *removes* the answer rather than improving it. Raise
`max_tokens` and `timeout_ms` with any level.

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
says `no_rules` (PROTOCOL §12). The generative tiers are what only they can do: edits, plans,
explanations. Their findings are labelled `review` where the rules pass's are `rules`
(PROTOCOL §9).

**Language is a second dimension.** The resolved language selects a prompt flavour from its
profile (`jev-core/src/lang.rs`); `languages.overrides` may narrow the verbs offered for a
language, and its `tier` and `prompt` fields are not read (PROTOCOL §10). When the language is
`unknown`, or has no override, the row above still applies with the `generic_text` prompt.
Routing can refine; it can never decline. This is N10 in `PROTOCOL.md` §1: no language,
filetype, or parser may decide *whether* something is served, only *how*.

## 3. Context builder

Deterministic, ordered, and bounded. Built by `context.rs` from the document the client holds and
the documents it sends with the request — nothing is read from disk; the model never chooses what
it sees.

| Slice | Budget | Notes |
|---|---|---|
| Scope text | the whole enclosing symbol, hard-capped at 400 lines | per `docs/LANGUAGE.md` §4: treesitter node, else a structural block, else the whole file. The chosen strategy is reported as `scope_source` |
| Heading | file path, language, symbol signature, and the enclosing type chain | cheapest orientation |
| Neighbours | the two preceding and two following top-level items, signatures only | local conventions |
| Diagnostics | current findings in scope, with severities | stops the model contradicting the linter |
| Imports | the file's import block, verbatim | naming and dependency awareness |
| Client context | the documents the client sends with the request, each labelled with its own `kind` — `imports`, `reference`, `sibling`, `test`, and `match` for `where` — verbatim, capped server-side at 4 documents / 40 lines (`context::MAX_PROVIDED_DOCS`, `MAX_PROVIDED_LINES`) | the client is the side with a parser, the other language servers and the list of buffers the user touched; the server decides how much of it reaches the prompt |

There is **no** repository-rules slice and **no** git slice: nothing reads `AGENTS.md`,
`CLAUDE.md` or a `.jev/context.md`, and no commit subject or diff reaches a prompt. Git is used
for the *changed set* (§5) — which documents the ambient pass looks at — and for nothing else.
The slices above are the whole prompt.

Excluded by default: whole files, other open buffers, the repository tree, chat history.
Each of those costs tokens to add ambiguity. Every slice is included only when the verb
lists it, and `context_hash` — part of the cache key — is the hash of the assembled
context, so a cache hit means the model would have seen byte-identical input.

## 4. Output contracts

The model emits **content and anchors**. It never emits ranges or versions; the server
computes those. This is what makes N5 in PROTOCOL §1 enforceable.

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
model that answers wrongly costs one extra call and then fails visibly rather than looping.

## 6. Determinism

Required for a harness to be learnable:

- `temperature = 0` for the `reason` tier on the fast paths that produce titles and
  findings, and `0.0` for the `decide` tier by default — a decision is a probability, and it
  should not move between two runs over the same state. It still moves with how close the
  *question* sits to the decision boundary: hosted Jev answered 0.96–0.97 across 25 runs on a
  sharply-posed question and 0.82–0.86 across 8 on one nearer the boundary, which is why a
  rule's `min_probability` belongs outside the measured spread (`docs/GUIDE.md` §4).
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

### What a pass costs, measured

Every number here was measured on 2026-09-20 against the hosted decide tier
(`https://opencode.ai/zen/v1`, wire `system_one`, model `jev-1.13`) and a real chat tier
(`google/gemini-2.5-flash-lite` via OpenRouter), with `jev inspect --force` and `jev review`
respectively. Every response carries `usage` (`input_tokens`, `output_tokens`); on the Zen route it
carries no price field, so the per-token rate used here is the **OpenRouter decisions route's own
`usage.cost`** — measured at `$0.000013776` for 349 tokens, i.e. **$0.0395 per million** (an earlier
project measurement, `$0.00002121` for 532 tokens, agrees at $0.0399). The chat tier's own
`cost_details` gave **$0.10 per million in, $0.40 per million out**.

**The rules pass over this repository** — `crates/**/*.rs`, one `jev inspect --force` per file:

| | |
|---|---|
| documents | 30 |
| documents that made a decision call | **20** — the other 10 had no candidate, and a save with no match costs nothing |
| candidates / rules considered / findings | 140 / 362 / 2 |
| tokens | **82,478 in, 3,831 out** |
| wall time | 24.8 s (0.83 s per calling document) |
| cost | **$0.0034** — $0.00011 per document, $0.00017 per document that called |

Per calling document that is 4,124 tokens in and 192 out, and it scales with *candidates*, not with
file size: the smallest calling document (2 candidates) cost 3,151 in / 58 out, the largest
(21 candidates) 6,859 in / 571 out, and a 6-line fixture with one candidate costs **482 in / 29
out** — the shape §7's "~500 tokens in and ~29 out" was written from, now reproduced.

**The same file both ways**, rules pass against chat review of identical content:

| document | rules pass (decide) | chat review | ratio |
|---|---|---|---|
| `crates/jev-lsp/src/server.rs` (2.6k lines) | 5,341 in / 345 out, 13 candidates, **2 findings**, 852 ms → $0.00022 | 30,562 in / 9 out, 0 findings, 3,637 ms → $0.00306 | **5.7× tokens, 13.6× dollars** |
| `crates/jev-core/src/config.rs` (840 lines) | 3,151 in / 58 out, 2 candidates, 0 findings → $0.00013 | 10,617 in / 9 out, 0 findings, 927 ms → $0.00107 | **3.4× tokens, 8.4× dollars** |

The review's prompt grows with the file; the decision's grows with the candidates. That is the
whole cost argument in one line: a review re-reads the file, a decision answers about the lines a
pattern named.

**The instruction document, as arithmetic** (arithmetic, not a measurement): a 2,000-token
instruction file re-sent across 50 turns is ~100,000 instruction tokens in one session — $0.010 at
the measured chat rate above; on a *local* model the cost is context, KV memory
and speed. The same conventions as rules cost one decision per candidate-bearing document and
nothing at all on a save where no pattern matches.

**What these numbers do not include.** The generation itself — the code your harness writes — is
untouched; what changes is what steering and review cost. The chat tiers (actions, plans,
explanations) stay the expensive path and are on-demand. And the local routes below are
cost characteristics, not verified configurations: no local decide tier has been run end to end by
this project, and no local harness generation run has been measured here.

## 8. Local-first

Default configuration assumes a local OpenAI-compatible server **for the chat tiers**. Remote
endpoints are opt-in per tier, and `api_key_env` names an environment variable — keys are never
written to config, artifacts, or logs. Prompt text and response bodies are never persisted
outside the conclusion cache, and the cache stores parsed conclusions, not raw model traffic.

**The decide tier is the exception.** Its default is *remote*:
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
hosted cold start: the endpoint's own p50 is ~0.3 s and 25 measured calls never came near the
5000 ms default. A value that does not parse, or parses to zero, is ignored rather than lowering
the ceiling to nothing. Turning rules off returns the ambient path to the `review` tier
(§2) — which is also remote by default, so a reader who wants *nothing* leaving the machine should
point `models.reason` and `models.review` at a local endpoint too.

**A hosted provider other than the default needs the wire, the ceiling, and the key's name.**

```sh
# the path is selected by the wire, not guessed from the host: OpenCode Zen answers on
# /systemone and 404s on the open_router path
export JEV_DECIDE_WIRE=system_one
export JEV_DECIDE_BASE_URL=https://opencode.ai/zen/v1
export JEV_DECIDE_MODEL=jev-1.13            # not jev-1.13-free: 429 FreeUsageLimitError in bursts
export JEV_DECIDE_TIMEOUT_MS=15000          # the 5000 ms default is too close to its tail

# the key is read from the variable *named by* `api_key_env` (default TYPESAFE_API_KEY).
# JEV_DECIDE_API_KEY_ENV names a different one from the environment — a name, never a key,
# and an empty value is ignored; JEV_API_KEY_ENV does the same for the chat tiers.
export JEV_DECIDE_API_KEY_ENV=OPENCODE_API_KEY
export OPENCODE_API_KEY=<key>
```

The OpenRouter route is the alternative, and its price is visible from its API:

```sh
export JEV_DECIDE_WIRE=open_router          # -> {base}/alpha/decisions
export JEV_DECIDE_BASE_URL=https://openrouter.ai/api
export JEV_DECIDE_MODEL=typesafe/jev-1.13
```

Measured on the same fixture and rule: both routes answer the same judgement (Zen 0.85–0.86,
OpenRouter 0.86, and today's spread on Zen 0.84–0.91), Zen is ~2× slower (0.93 s against 0.42 s),
Zen's response carries `usage` but no cost field, and `opencode-go` — the subscription gateway,
`…/zen/go/v1` — carries no Jev at all (`Model is unavailable`). The raised `timeout_ms` is a
measurement, not a loosened check: at the shipped 5000 ms a client-attached call failed with
`decision call failed: POST …/systemone: timeout: global` while the CLI on the same route
succeeded, and the same run answered `ok` at 15000.

An unrecognised `JEV_DECIDE_WIRE` is ignored rather than coerced, so a typo leaves the previous
wire in force; `jev.status` (`models.decide.wire`) and the server's `settings applied` log line
are where you see which one is actually in force.

### The decide tier on your own machine

A System One server on `127.0.0.1:8009` is one config change (the recipe above), and a decision
then costs **nothing per call** — measured on this box over 18 decisions: **p50 583 ms,
$0.000000 per decision**, against **$0.000015** per decision for `typesafe/jev-1.13` through
OpenRouter at p50 591 ms. The latency is comparable; the price is not.

The accuracy is not comparable, and that is the trade: the same 18 decisions answered **12/18 =
67%** on the local arm against **17/18 = 94%** for `typesafe/jev-1.13`. The box's own classifier
research, on a smaller model, reports ~83 ms and 89%. So a local decide tier is not a smaller
version of the hosted one — it has less margin, which is exactly why the two things this document
keeps repeating matter more locally, not less: put `min_probability` outside the answer's measured
spread, and phrase the question as the violation (`docs/GUIDE.md` §4). A local tier also widens the
spread, so measure it before choosing the floor.

**Local is cheap per token, not fast.** Measured here: `llama.cpp` with a 4B-active MoE
(`gemma-4-E4B-it-Q4_K_M`) generates **33 tok/s** on this M1 Pro (96 tokens in 2.9 s). The serving
logs on this machine for the larger code models sit at **~1.5–2 tok/s per request**, and this
project's own local soak measured a 35B at **5.7–12.4 s** per ambient review and **7.3–15.5 s** per
resolve (`docs/VERIFICATION.md` §7). A model that slow is only viable when the tokens it must
produce drop — which is what a rule set does: the conventions are enforced by the rule, on the line,
so the generation side no longer has to hold them in context or re-read the file to check them.

**Not verified here.** No local decide tier has been run end to end by this project — the recipe,
the price and the latency are measured, the wiring is not — and no local harness generation run has
been measured on this machine. These are the intended deployments with stated cost
characteristics, not configurations this repository has proven.
