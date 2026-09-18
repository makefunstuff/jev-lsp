# Verification

Nothing is claimed without a mechanism below. Tests are named for the defect they catch,
not for the code they touch.

**Status of this document.** It specifies the proof each claim requires, and what is built.

| Artefact | State | Last result |
|---|---|---|
| `verify/probes/` | built | 7 probes, all green (`verify/probes/run.sh`) |
| `verify/lsp_client.py` | built | 27 ok, 0 FAIL, 0 skip, 0 warn against the real binary |
| `verify/smoke.py` | built | 32/32 against the real binary |
| `verify/queue_test.py` | built | 5/5; proven to fail on the pre-fix behaviour |
| `verify/supersede_probe.py` | built | 7/7 — written independently by the verifier agent; control case plus a race case, and it asserts the race was actually set up |
| `verify/plan_test.py` | built | 35/35 — the plan loop, server-side apply, revert, staleness, divergence, multi-file creation |
| `verify/cli_parity.py` | built | 12/12 — the CLI and the LSP produce identical findings and byte-identical edits |
| `verify/dismiss_test.lua` | built | 9/9 — a finding is dismissed, recorded per repository, and does not resurface |
| `verify/harness_log.lua` | built | shared by the Lua harnesses: a red run prints the server's own log lines |
| `verify/real_model.py` | built | real endpoint, reports rather than asserts; run against DeepSeek through the omp auth gateway |
| `verify/soak.py` | built | several languages through the whole loop against a real endpoint; last result 8/9 applied, 0 unparseable |
| `verify/stub_model.py` | built | scripted endpoint; no GPU, no network |
| `verify/quality_eval.py` | built | real endpoint: 4/4 planted defects caught, 4/4 precision, **0 findings across 2 clean files**, 0 discarded |
| `verify/outcome_test.py` | built | 18/18 — `meta.outcome` recorded, `meta.usage` counted, the unknown event kept verbatim. Proven to fail on the pre-change binary |
| `verify/repo_bench.py` | built | real endpoint, a measurement rather than a threshold: 40 files of this repository: 62 findings, **3.21 per 1000 lines** (three runs, 3.21/3.48/3.71) |
| `verify/nvim_live.lua` | built | 0 failures, 0 skips — real plugin, real server, real buffer |
| `verify/goldens/` | **not built** | planned with U6; the anchor-ambiguity rules are covered by `meta-core` unit tests instead |
| `verify/bench.sh` | **not built** | latency budgets are asserted where they can be (`codeAction` p99 in `lsp_client.py` step 3); a standalone bench waits for U9 |

Rows below that cite an unbuilt harness are the requirement, not a report of coverage.

**A red run says why.** Every harness prints the server's own `window/logMessage` lines when
it fails (`verify/harness_log.lua` for the Lua ones; the Python ones carry the same in their
FAIL detail). This matters more than it sounds: at the other end of an LSP connection a dead
model endpoint is indistinguishable from a product defect — the client simply gets no
findings and no edit — and a stale stub process left bound to the port has twice been
mistaken for a regression.

## 1. Independent LSP client

`verify/lsp_client.py` — a stdio LSP client written against the specification, **sharing
no code with the server**, depending only on the Python standard library.

It performs, in order, and asserts at each step:

1. `initialize` → `initialized`; assert `server_capabilities.positionEncoding == "utf-8"`,
   `codeActionProvider.resolveProvider == true`, `diagnosticProvider.identifier == "meta"`.
2. `textDocument/didOpen` with a fixture file.
3. `textDocument/codeAction` → assert p99 latency budget, assert **no action contains an
   `edit`** (N2 — the fast path must not carry edits).
4. `textDocument/codeAction` with `triggerKind: Automatic` → assert only `ready` actions,
   no `pending` placeholder.
5. `codeAction/resolve` on the first action → assert `documentChanges` is present,
   every `TextDocumentEdit` has an **integer** `version`, and no bare `changes` key.
6. `workspace/applyEdit` → assert `applied == true`.
7. Mutate the document, resolve the *same* action again → assert **no `edit` is returned**
   (staleness), and that the response is not an error.
8. `textDocument/diagnostic` → assert findings carry `data.finding_id` and `data.verb`.
9. `workspace/executeCommand` `meta.cancel` mid-flight → assert a `$/progress` `end` was
   received for the token and no `edit` followed.

Because it is written from the spec, a disagreement between it and the server is a real
protocol defect, not a test artifact.

## 2. Live Neovim

`verify/nvim_live.lua` — real Neovim, real plugin, real server, run under
`nvim --headless -u verify/minimal_init.lua`:

- start the server through `vim.lsp.start` with the plugin's config
- open a fixture, save it, wait for the sign column to gain a diagnostic; assert the
  diagnostic text and line
- call `vim.lsp.buf.code_action()`, drive `vim.ui.select` with a stubbed chooser, assert
  the buffer changed exactly as the returned edit specified
- assert one `:Meta undo` restores the buffer byte-for-byte
- assert `:Meta stop` results in zero further model calls within 2 s (counted by a stub
  endpoint)

This is the only test that proves the product claim — the rest prove the protocol.

## 3. Golden intents

Fixture repository plus a **stub model** that returns canned responses keyed by
`context_hash`. Property-based, not exact-diff:

- for every verb × fixture scope, the pipeline produces an edit that (a) parses, (b) lands
  inside the requested scope, (c) is idempotent — re-applying the same response to the
  result produces a no-op edit
- ambiguous anchors (`match` occurring 0 or 2 times) produce **no edit** and one repair
  attempt, verified by call count against the stub
- malformed JSON, over-long output, and truncated output each produce no edit and no
  panic

Fixtures are regenerable and byte-identical across runs; `verify/goldens/` is committed
with a header recording the stub version and model tier.

## 4. Defect injection

Each row is a test that must fail when the defect is injected and pass otherwise. This
suite is the actual regression net; it is run in CI *and* as part of the design review.

| Injected defect | Test that must catch it |
|---|---|
| `documentChanges` replaced by bare `changes` | `test_edit_contract.py` — validator self-test |
| `version` omitted from a `TextDocumentEdit` | validator self-test, reproducing `[R3]`'s `util.lua:541` crash as a *rejection* |
| `version` set to a stale value | `verify/lsp_client.py` step 7 |
| Model call placed inside the `codeAction` handler | latency bench, `codeAction` p99 budget |
| Cache keyed by `(uri, version)` instead of content hash | revert-then-resolve test: same content, different version, must hit |
| `title` derived from model output | determinism test: two cold runs must produce identical titles |
| Budget check after the call instead of before | budget test asserting the stub sees exactly `max_calls_per_min` calls |
| `$/progress` `end` omitted on the error path | `verify/lsp_client.py` step 9 |
| Progress sent for a token the client never supplied or created | `verify/lsp_client.py` conformance check; `verify/probes/streaming.lua` for the client side |
| Token smuggled through `arguments` instead of `workDoneToken` | same — the server would still "work" against Neovim, which is why the check lives in the independent client |
| `workDoneProgress: true` declared on a provider that never reports | capability audit in the independent client's `initialize` assertions |
| Support gated on a filetype allowlist | `verify/probes/language.lua` — with `filetypes = nil`, all 11 fixtures attach after the plugin pass |
| An edit arriving while an analysis is in flight is dropped rather than queued | `verify/queue_test.py` — proven to fail with the old behaviour injected (0 refreshes, 0 findings) and pass when queued |
| A superseded analysis emits no refresh | same test, first assertion |
| A capability is advertised but not served (`workspaceDiagnostics`) | `verify/smoke.py` asserts the sub-capability, not only the top-level providers |
| A client answering `workspace/configuration` with `{}` resets unrelated settings | `meta-core` `config::tests::an_empty_payload_changes_nothing` and `the_environment_wins_over_the_client_payload` |
| A reasoning model exhausting its token budget returns nothing | `meta-core` `model::tests::a_reasoning_model_that_ran_out_of_budget_says_so` — the error must name `finish_reason=length` and the fix |
| The model echoes the schema instead of filling it | `meta-core` `verbs::tests::the_schema_is_an_example_not_a_template_to_echo`; the schema is a concrete example plus an explicit "never use a field name as a value" rule |
| A slow-but-valid answer turned into a transport error by a timeout below the token ceiling | `verify/soak.py` — the run that measured 66 s against a 30 s cap |
| An answer that cannot be applied is never re-prompted | `meta-lsp` `engine::tests::an_answer_that_does_not_apply_is_repaired_with_the_reason` |
| An answer that re-emits the lines it did not consume duplicates them | `meta-core` `edit::tests::an_answer_that_reshapes_a_block_absorbs_the_re_emitted_lines` and `an_insertion_that_would_duplicate_a_line_that_stays_is_refused` |
| A file with no detectable language not synced | same probe: `plain`, `data.log`, `f.zzz` must arrive as documents |
| Gating the verb set on a treesitter parser | golden test with the parser absent: the same verb set is offered and scope falls back to `structural`/`whole_file` |
| Language hook mutating buffer state | `verify/probes/language.lua`: `the language hook did not mutate buffer state` |
| A skipped buffer reported silently | `:Meta status` test asserting `over_size`, `binary`, `ignored`, `generic_scope` are surfaced |
| Model output applied without anchor resolution | golden test: ambiguous anchor must yield no edit |
| `ERROR` severity emitted from the findings contract | schema rejection test |
| The finding cap applied at some surfaces and not others | `meta-core` `findings::tests::the_cap_keeps_warnings_over_information_and_truncates`; every surface reads the one finalised set, so `verify/quality_eval.py` keeps its zero on the clean files |
| What the client did with an offer never reaching the server | `verify/outcome_test.py` — proven to fail on the binary without `meta.outcome` (17 checks) |
| A record that can be read as a schema instead of a log | same test: an unknown `kind` is recorded verbatim and counted as nothing |

## 5. Latency bench

`verify/bench.sh` drives the Python client against a fixture workspace with a stub model
whose latency is configurable, and reports p50/p99 per method against the table in
`docs/ARCHITECTURE.md` §4. A budget miss is a failure, not a warning. Real-model latency
is measured separately and reported as `[U]` context, never as a pass condition.

## 6. What counts as proof

| Claim | Proof required |
|---|---|
| "The protocol conforms" | `verify/lsp_client.py` green |
| "It works in Neovim" | `verify/nvim_live.lua` green |
| "It does not burn tokens" | stub-endpoint call counters under the defect-injection suite |
| "The edit contract is safe" | validator self-test + the `[R3]` probe table reproduced |
| "It is fast enough" | `verify/bench.sh` against the §4 budgets |
| "Streaming status reaches the editor" | `verify/probes/streaming.lua` green — client-supplied token in the request params, `begin,report,end` observed |
| "The frozen contract is implemented by a real client" | `verify/probes/trace.lua` green — a reference server's payloads for §2/§4/§8 are accepted, applied, and refused exactly as specified |
| "Every file is supported, not just known filetypes" | `verify/probes/language.lua` green — 8/11 attached by the built-in path, 11/11 after the plugin pass, including files with no language at all |
| "The model output is usable" | golden intents |
| "The findings are quiet enough to live with" | `verify/quality_eval.py` — 4/4 planted defects caught and zero findings on the two clean files |
| "What the user does with an offer is known" | `verify/outcome_test.py` — `meta.usage` counts it and the line is in `<root>/.git/meta/session.jsonl` |

Anything not covered above is reported as unverified, with the exact probe that would
settle it.

## 7. Real model

`verify/real_model.py` runs the live server against a real endpoint and reports what it
observes rather than asserting stub-shaped expectations. Run through the omp auth gateway,
which resolves the provider credential server-side, so no key is handled here:

```sh
python3 verify/real_model.py --base-url http://127.0.0.1:4000/v1 --model deepseek/deepseek-flash
```

Against `deepseek/deepseek-flash`, six consecutive runs, after the three defects below were
fixed:

| | measured |
|---|---|
| ambient review | 2.2–3.1 s, 1 finding each |
| `codeAction/resolve` | 1.1–5.1 s, edit returned every time |
| tokens per session | ~1.4–2.2 k |
| resulting file parses | 6/6 |

This is what the fast path buys: the menu itself is still a cache read, and the seconds are
spent only after a pick. It also closed the last untested claim — every earlier result came
from the scripted endpoint.

### Local model

The same soak against the local `llama.cpp` server (`qwen3.6-35b-a3b-iq3xxs`,
`127.0.0.1:37313`), two rounds over Python, Rust and TypeScript:

| | cloud (`deepseek-flash`) | local (`qwen3.6-35b-a3b-iq3xxs`) |
|---|---|---|
| applied edits | 8/9 | **6/6** |
| files left unparseable | 0 | 0 |
| ambient pass | 1.5–7 s | 5.7–12.4 s |
| `codeAction/resolve` | 1.5–31 s | 7.3–15.5 s |

The local model was *more reliable* on this task and *slower*, and it produced none of the
failures the cloud model did — no re-emission rejections, no exhausted ceilings. Its latency
spread is also much tighter, which matters more than its median: a resolve that always takes
twelve seconds is easier to design around than one that takes two or thirty.

**`think: off` works here and does not on the gateway.** The tier default sends
`chat_template_kwargs: { enable_thinking: false }`, which llama.cpp honours and the omp auth
gateway ignores. A bare probe against the local server — same prompt, no template kwargs — took
**47 s for 16 tokens and returned nothing but reasoning**; with the server's own request shape
the same call answered in 0.9 s, then 0.3 s warm. That is the difference between a usable tier
and an unusable one, and it is why the client-side control is not optional.

## 8. The defect that passed every test

`diagnosticProvider.workspaceDiagnostics` was advertised as `true` from the first capability
block while `workspace/diagnostic` was never implemented. Neovim's `on_refresh` checks that
capability **first** (`lsp/diagnostic.lua`) and takes the workspace branch when it is set, so
every `workspace/diagnostic/refresh` was answered by a method that does not exist and the
client never re-pulled per-document diagnostics. The server analysed, cached, and refreshed —
and **nothing ever reached the sign column.**

Every existing harness passed. The Python ones pull `textDocument/diagnostic` explicitly
rather than relying on the refresh round trip, and the Neovim ones assert that a code action
resolves, which reads the server's cache directly. The bug was found by writing a test for
something else: dismissal needs a *displayed* diagnostic to dismiss.

Two lessons, both now enforced:

* **"Advertise only what is served" applies to sub-capabilities.** `verify/smoke.py` checks
  the providers *and* `diagnosticProvider.workspaceDiagnostics`.
* **A pull-based design has to be tested through the client's pull path.** Pulling by hand
  from a harness proves the server answers; it does not prove the client ever asks.

## 9. What the real model found that the stub could not

Recorded because each was invisible to a scripted model, and each is now pinned by a test:

1. **Reasoning models exhaust a tight ceiling.** At 2048 tokens `deepseek-flash` returned
   `finish_reason=length` with empty content, because reasoning consumed the whole budget.
   The error now names that cause and the fix, and verb ceilings are 4096/2048.
2. **A placeholder schema gets echoed.** The model returned the field name `verb_hint` where
   an object was expected — the schema had been written as a fillable template. It is now a
   concrete example plus an explicit "never use a field name as a value" rule.
3. **An answer can cover more than its anchor.** Anchored on a one-line `statement`, the
   model answered with a block opener plus the body re-indented under it. Applied literally
   that duplicated the body; truncated, it left an empty block. The replaced range now
   *absorbs* the lines the answer re-emits, bounded by the document.

## 10. Known unverified

- **Undo granularity** of a client-applied `WorkspaceEdit` `[R10]`. Headless script
  execution cannot record undo blocks, so the probe was inconclusive. What *is* verified is
  the plugin's own path: `verify/nvim_live.lua` asserts `:Meta undo` restores the buffer
  byte-for-byte, which is the behaviour the product depends on. Plain `u` remains unmeasured.
- **Model quality** on any verb. The suite proves the pipeline, never the usefulness of a
  particular model's output; that is measured by the user, in the editor, and recorded
  separately.
- **A real model.** Every automated run uses `verify/stub_model.py`. The HTTP client, the
  tier config, the `think` control, and the response parser are unit-tested against canned
  payloads, and the wiring is exercised end to end — but no automated test has talked to a
  live `llama.cpp` server, because that costs a GPU. Run it deliberately:
  `META_BASE_URL=http://127.0.0.1:<port>/v1 META_MODEL=<model> python3 verify/smoke.py`.
- **Real-model latency.** The `codeAction` budget is asserted against the stub. What a
  7B–35B model costs on this machine in `codeAction/resolve` is unmeasured.

## 11. Known limitations

Recorded because they are deliberate boundaries, not oversights:

- **Inline completion is gone, and with it the two boundaries it used to carry.** Ghost text
  was verified interactively (a headless Neovim fires none of `InsertEnter` / `CursorMovedI` /
  `TextChangedP`) and `inlineCompletionProvider` had to be injected into the `initialize`
  response because `lsp-types` 0.94 cannot express it. Both are moot since 2026-09-19: the
  method, the capability, the `fim` tier and the `<Tab>` acceptance were removed, along with
  `crates/meta-lsp/src/advertised.rs` and the transport-boundary wrapper that existed only to
  carry that one field.
- **`shutdown` with an explicit `params` member is rejected** with `-32602 Unexpected
  params`. tower-lsp only accepts `()` for a no-params method, and JSON-RPC 2.0 permits
  omitting `params` but does not define a null one, so the rejection is spec-correct.
  Measured: params absent → `{"result": null}`; `null`, `[]`, `{}` → `-32602`. **Neovim is
  unaffected** — `lsp/client.lua:911` calls `rpc.request('shutdown', nil, …)` and Lua drops
  a nil table value, so the member never reaches the wire. A client that sends null will see
  a failed shutdown and may force-stop the server.
- **No treesitter scope.** Scope resolution is structural (brace or indentation) with a
  whole-file fallback. `scope_source` reports which was used, so the quality is never
  claimed to be higher than it is. Adding grammars is additive and changes no protocol.
- **`languages.overrides[].verbs` narrows the menu but nothing enforces the complement** —
  a client can invoke any verb through a hand-built action. The server validates the result,
  not the request (the version stamp and anchor rules are what protect the buffer).
