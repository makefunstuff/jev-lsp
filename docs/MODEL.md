# Model layer

## 1. Tiers

Three tiers, each an independently configured endpoint. Any tier may be local
(llama.cpp OpenAI-compatible) or remote; the router does not care.

| Tier | Purpose | Model shape | Latency target | Called from |
|---|---|---|---|---|
| `fim` | Fill-in-the-middle ghost text | small, 1–4B, FIM-trained | p50 < 150 ms | `inlineCompletion` only |
| `reason` | Actions, plans, explanations | 7–32B instruct | p50 < 2 s (resolve) | `codeAction/resolve`, `meta.plan` |
| `review` | Findings, post-apply verification | 7–32B instruct, different prompt | background, no user wait | worker, `meta.review` |

`reason` and `review` are deliberately separate endpoint slots even when they point at the
same server: a finding review and a rewrite must not share a prompt template version, and
operators tune them independently.

Config is configuration-layer, not code (PROTOCOL §10 `models`): `{base_url, model,
api_key_env, timeout_ms, max_tokens, temperature, think}`. `think` mirrors the CLI's
reasoning control — `off` sends `chat_template_kwargs.enable_thinking = false`, a level
sends `reasoning_effort`. Tiers default to `think = "off"` except `reason`.

## 2. Routing

```
trigger -> verb -> tier
```

| Trigger | Verb | Tier |
|---|---|---|
| `inlineCompletion` | — | `fim` |
| save / idle analysis | — | `review` |
| `codeAction/resolve` | fix, harden, types, docs, rewrite, test, generate | `reason` |
| `codeAction/resolve` | explain, review | `reason` |
| post-apply verification | — | `review` |

No dynamic routing heuristics. A table, visible in one place, overridable per verb in
config.

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
| Repo | `AGENTS.md` / `CLAUDE.md` / `.meta/context.md` if present, verbatim, capped | durable project rules |
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
{ "schema": "meta.edit/1",
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
{ "schema": "meta.findings/1",
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
{ "schema": "meta.plan/1",
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

Implemented in `Engine::with_repair` (`crates/meta-lsp/src/engine.rs`): one model call, plus
at most `MAX_REPAIR_ATTEMPTS` (= 1) repair calls, only for a contract violation.

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
  findings.
- Title construction is a pure function of `(verb, scope name, finding label)`; the model
  supplies the label, never the whole title.
- Identical `context_hash` + identical prompt template version ⇒ byte-identical output is
  expected, and a cold-cache repeat is part of the golden test suite. Divergence is
  recorded as a finding against us, not tolerated.

## 7. Budgets

Accounted in `budget.rs`, checked before the call, incremented after:

- per-minute and per-hour call caps, per-session token cap (PROTOCOL §5)
- per-buffer inline-completion cap, so one file cannot consume the session
- a cost line per call: `model tier tokens_in tokens_out ms trigger` at debug level, and
  the same numbers surfaced by `:Meta status`

Exhaustion is a state, not an error. The user sees `over_budget` on the action and a
statusline counter; nothing pops up twice.

## 8. Local-first

Default configuration assumes a local OpenAI-compatible server. Remote endpoints are
opt-in per tier, and `api_key_env` names an environment variable — keys are never written
to config, artifacts, or logs. Prompt text and response bodies are never persisted outside
the conclusion cache, and the cache stores parsed conclusions, not raw model traffic.
