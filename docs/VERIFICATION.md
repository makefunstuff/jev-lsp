# Verification

Nothing is claimed without a mechanism below. Tests are named for the defect they catch,
not for the code they touch.

**Status of this document.** It specifies the proof each claim requires, and what is built.

| Artefact | State | Last result |
|---|---|---|
| `verify/run-suite.sh` | built | the whole table below in one run — 24 rows `ok`, `quality_eval` reported as `?` (it needs a real model) |
| `verify/probes/` | built | 7 probes, all green (`verify/probes/run.sh`) |
| `verify/lsp_client.py` | built | 44 ok, 0 FAIL, 0 skip, 0 warn against the real binary (step 10 is the three §3.5 failure paths) |
| `verify/smoke.py` | built | 44/44 against the real binary — and three consecutive full-table runs after the two harness defects in §8 were fixed, which is the point |
| `verify/rules_test.py` | built | 45/45 — the rules pass end to end: inspections, gates, cache, skips |
| `verify/lsp_framing_test.py` | built | 9/9 — the test client's own stdio framing; written for a defect the suite found in itself (§8) |
| `verify/omp_lsp.sh` | built | 0 failures, 0 skips — OMP, a third client that shares no code with this repository, receives a rule's finding and calls `jev.inspect` (§1.1) |
| `verify/queue_test.py` | built | 5/5; proven to fail on the pre-fix behaviour |
| `verify/supersede_probe.py` | built | 7/7 — written independently by the verifier agent; control case plus a race case, and it asserts the race was actually set up |
| `verify/plan_test.py` | built | 35/35 — the plan loop, server-side apply, revert, staleness, divergence, multi-file creation |
| `verify/cli_parity.py` | built | 30/30 — the CLI and the LSP produce identical findings and byte-identical edits, `jev.inspect` included; five checks are a nested-file case with a repository-relative `applies_to`, which is the regression test for the CLI resolving `.jev/rules/` at the repository root |
| `verify/scope_containment_test.py` | built | green (exit 0) — a scope the client narrows is the scope the answer stays inside |
| `verify/dismiss_test.lua` | built | 0 failures, 0 skips — a finding is dismissed, recorded per repository, and does not resurface |
| `verify/rules_live.lua` | built | 0 failures, 0 skips on Neovim 0.12.5 **and** 0.12.1 — a rule's finding reaching the sign column, `:Jev inspect` answering with the same finding and its counts |
| `verify/harness_log.lua` | built | shared by the Lua harnesses: a red run prints the server's own log lines |
| `verify/real_model.py` | built | the end-to-end loop against a real endpoint: ambient 0.8 s, 1 finding (`file handle is never closed`), 9 actions offered including `quickfix.jev`, resolve 0.7 s `state=ready`, 1371 tokens. Sets `rules.enabled = false` (it measures the chat review) and exits 1 when no model answered (§7) |
| `verify/soak.py` | built | the whole loop over six languages against a real endpoint: **10/12 runs applied an edit, 2 left the file unparseable, 12/12 billed**; ambient 0.6–60 s, resolve 0.7–2.1 s; exit 1 when a run left a broken file (§7, §11) |
| `verify/stub_model.py` | built | scripted endpoint; no GPU, no network. Answers the decision wire too (`/systemone`) |
| `verify/quality_eval.py` | built | runnable whenever an endpoint and a key are given; the suite reports it as `?` when none is. Last run (`google/gemini-2.5-flash-lite` via OpenRouter): **recall 3/4, precision 3/3, 0 findings on both clean files**, 6 calls / 2950 tokens billed, 4.5 s, exit 0 — `swallowed_error.py` was missed, run-to-run variance on a cheap model (a control with the same model caught 4/4); the older 4/4 was `deepseek/deepseek-v4-flash`, 2026-09-18 |
| `verify/outcome_test.py` | built | 18/18 — `jev.outcome` recorded, `jev.usage` counted, the unknown event kept verbatim. Proven to fail on the pre-change binary |
| `verify/repo_bench.py` | built | real endpoint, a measurement rather than a threshold: 40 files of this repository: 62 findings, **3.21 per 1000 lines** (three runs, 3.21/3.48/3.71; measured 2026-09-18) |
| `verify/nvim_live.lua` | built | 0 failures, 0 skips — real plugin, real server, real buffer |
| `verify/goldens/` | **not built** | planned with U6; the anchor-ambiguity rules are covered by `jev-core` unit tests instead |
| `verify/bench.sh` | **not built** | latency budgets are asserted where they can be (`codeAction` p99 in `lsp_client.py` step 3); a standalone bench waits for U9 |

Rows below that cite an unbuilt harness are the requirement, not a report of coverage.

**The table is one command, and it lives in the repository.**

```sh
bash verify/run-suite.sh /tmp/suite.log            # the table above, in order
NVIM_ONLY=1 bash verify/run-suite.sh /tmp/nvim.log # only the stub lifecycle and the Lua rows
```

`verify/run-suite.sh` runs every row, captures each row's output and exit code into `<out-file>`,
and prints a summary block to its **own stdout** when it finishes — `ok` / `FAIL` / `?` per row,
where `?` means the row did not run, which is how `quality_eval` reports itself when no real
endpoint is reachable (`bash verify/run-suite.sh /tmp/suite.log > /tmp/suite-summary.log` gets
both). It never uses `set -e`: a red row has to be *visible*, not fatal to the run. Three knobs,
all in its header: `NVIM_ONLY=1` skips cargo, the probes and every Python/OMP row (use it while
the Rust tree is being edited concurrently — a sibling's half-finished edit is a phantom failure
here); `REFUSE_IF_BUSY=1` refuses rather than killing when something already answers on the stub's
port; `NVIM_BINS` names the Neovim binaries for the Lua rows (default: the 0.12.5 build plus the
installed `nvim`).

**A red run says why.** Every harness prints the server's own `window/logMessage` lines when
it fails (`verify/harness_log.lua` for the Lua ones; the Python ones carry the same in their
FAIL detail). This matters more than it sounds: at the other end of an LSP connection a dead
model endpoint is indistinguishable from a product defect — the client simply gets no
findings and no edit — and a stale stub process left bound to the port has twice been
mistaken for a regression. That is why the runner owns the stub's whole lifecycle: it kills any
leftover, waits until the port is *actually* free, starts exactly one stub for the run, and
checks the **pid** as well as `/health` — a process that lost the race for the port exits while
the port keeps answering, and then the harnesses would be talking to someone else's stub.

## 1. Independent LSP client

`verify/lsp_client.py` — a stdio LSP client written against the specification, **sharing
no code with the server**, depending only on the Python standard library.

It performs, in order, and asserts at each step:

1. `initialize` → `initialized`; assert `server_capabilities.positionEncoding == "utf-8"`,
   `codeActionProvider.resolveProvider == true`, `diagnosticProvider.identifier == "jev"`.
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
9. `workspace/executeCommand` `jev.cancel` mid-flight → assert a `$/progress` `end` was
   received for the token and no `edit` followed.
10. The three reachable §3.5 failure paths — **model error**, **budget refusal**, **cancellation** —
   each assert exactly one `begin` and one `end` in that order under the supplied `workDoneToken`,
   the `Result` envelope's code (`model_error` / `over_budget` / `-32800 Canceled`), that the `end`
   arrives **before** the response, and that the server answers a following command. The
   cancellation case also asserts promptness: the `end` within 2 s of the cancel while the model
   call stalls for 4 s, which is the assertion that goes red on the pre-fix behaviour.

Because it is written from the spec, a disagreement between it and the server is a real
protocol defect, not a test artifact.

### 1.1 A third client: OMP

`verify/omp_lsp.sh` drives `jev-lsp` from **OMP**, which is a client nobody here wrote and which
shares no code with this repository. That is the point: the independent client above is
spec-derived, but it is still *ours*, and Neovim is the client the plugin was built for. A
finding that reaches OMP's own `lsp` tool is evidence the standard surfaces are enough on their
own (`docs/LANGUAGE.md` §1).

The fixture registers `target/release/jev-lsp` in its own `<fixture>/.omp/lsp.json` — the
repository's `.omp/` and `~/.omp` are untouched — and the agent is asked to call the `lsp` tool
twice, in order: a `workspace/executeCommand` request for `jev.inspect`, then `diagnostics` for
the file. It asserts:

- the rule's finding reaches OMP's diagnostics with the rule's **title**, the judgement that
  followed its prose, the `.unwrap()` line, the finding id and the rule's severity:
  `4:5 [warning] [jev] Unwrap in a request handler — A handler must not unwrap; return the error
  instead. — reachable (p=0.90) (83f83e989c25)`;
- `jev.inspect` is reachable through OMP's tool surface and answers with the same finding, its
  counts (`considered`/`candidates`) and its skips;
- a **negative control** — the same fixture with no `.jev/rules/` — produces no jev diagnostic and
  an `inspect` that says `no_rules`, so a green run cannot be the harness finding something else.

`omp` unavailable, no model, or an agent that never drives the tool is a **skip with the reason**,
never a false `ok`.

**One observation about the client, recorded so it is not read as a defect here.** OMP's symbol
paths fan out to every non-custom server without checking the negotiated symbol capabilities, so
`workspace/symbol` and `textDocument/documentSymbol` reach `jev-lsp` and are answered `-32601`
because it never advertised them — which is PROTOCOL §2 working as intended. From the probe:

```
Workspace symbol search failed: all language servers failed
Server failures:
  jev-lsp: LSP error -32601: Method not found
```

```
{"action":"symbols","file":"handler.rs"} → LSP error: LSP error -32601: Method not found
```

The server is not missing a feature here; a client is asking for one it was not told existed.

## 2. Live Neovim

`verify/nvim_live.lua` — real Neovim, real plugin, real server, run under
`nvim --headless -u verify/minimal_init.lua`:

- start the server through `vim.lsp.start` with the plugin's config
- open a fixture, save it, wait for the sign column to gain a diagnostic; assert the
  diagnostic text and line
- call `vim.lsp.buf.code_action()`, drive `vim.ui.select` with a stubbed chooser, assert
  the buffer changed exactly as the returned edit specified
- assert one `:Jev undo` restores the buffer byte-for-byte
- assert `:Jev stop` results in zero further model calls within 2 s (counted by a stub
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
| `$/progress` `end` omitted on the error path | `verify/lsp_client.py` step 9 (the normal path) and step 10 (model error, budget refusal, cancellation) |
| Progress sent for a token the client never supplied or created | `verify/lsp_client.py` conformance check; `verify/probes/streaming.lua` for the client side |
| Token smuggled through `arguments` instead of `workDoneToken` | same — the server would still "work" against Neovim, which is why the check lives in the independent client |
| `workDoneProgress: true` declared on a provider that never reports | capability audit in the independent client's `initialize` assertions |
| Support gated on a filetype allowlist | `verify/probes/language.lua` — with `filetypes = nil`, all 11 fixtures attach after the plugin pass |
| An edit arriving while an analysis is in flight is dropped rather than queued | `verify/queue_test.py` — proven to fail with the old behaviour injected (0 refreshes, 0 findings) and pass when queued |
| A superseded analysis emits no refresh | same test, first assertion |
| A capability is advertised but not served (`workspaceDiagnostics`) | `verify/smoke.py` asserts the sub-capability, not only the top-level providers |
| A client answering `workspace/configuration` with `{}` resets unrelated settings | `jev-core` `config::tests::an_empty_payload_changes_nothing` and `the_environment_wins_over_the_client_payload` |
| A reasoning model exhausting its token budget returns nothing | `jev-core` `model::tests::a_reasoning_model_that_ran_out_of_budget_says_so` — the error must name `finish_reason=length` and the fix |
| The model echoes the schema instead of filling it | `jev-core` `verbs::tests::the_schema_is_an_example_not_a_template_to_echo`; the schema is a concrete example plus an explicit "never use a field name as a value" rule |
| A slow-but-valid answer turned into a transport error by a timeout below the token ceiling | `verify/soak.py` — the run that measured 66 s against a 30 s cap |
| An answer that cannot be applied is never re-prompted | `jev-lsp` `engine::tests::an_answer_that_does_not_apply_is_repaired_with_the_reason` |
| An answer that re-emits the lines it did not consume duplicates them | `jev-core` `edit::tests::an_answer_that_reshapes_a_block_absorbs_the_re_emitted_lines` and `an_insertion_that_would_duplicate_a_line_that_stays_is_refused` |
| A file with no detectable language not synced | same probe: `plain`, `data.log`, `f.zzz` must arrive as documents |
| Gating the verb set on a treesitter parser | golden test with the parser absent: the same verb set is offered and scope falls back to `structural`/`whole_file` |
| Language hook mutating buffer state | `verify/probes/language.lua`: `the language hook did not mutate buffer state` |
| A skipped buffer reported silently | `:Jev status` test asserting `over_size`, `binary`, `ignored`, `generic_scope` are surfaced |
| Model output applied without anchor resolution | golden test: ambiguous anchor must yield no edit |
| `ERROR` severity emitted from the findings contract | schema rejection test |
| The finding cap applied at some surfaces and not others | `jev-core` `findings::tests::the_cap_keeps_warnings_over_information_and_truncates`; every surface reads the one finalised set, so `verify/quality_eval.py` keeps its zero on the clean files |
| What the client did with an offer never reaching the server | `verify/outcome_test.py` — proven to fail on the binary without `jev.outcome` (17 checks) |
| A record that can be read as a schema instead of a log | same test: an unknown `kind` is recorded verbatim and counted as nothing |
| A malformed rule file taking the whole pass down | `verify/rules_test.py` — the file is skipped with a stated reason and the rest still load |
| One decision call per candidate instead of one per document | `verify/rules_test.py` — N candidates, exactly one call, counted by the stub |
| An ambient pass with no rules reporting silence as "clean" | `verify/rules_test.py` and `jev-lsp` `engine::tests::a_pass_with_no_rules_says_so_instead_of_finding_nothing` — `("no_rules", …)` |
| A rule edit served a conclusion taken under the old text | `jev-core` `cache::tests::the_rules_key_separates_content_rules_and_path`; `verify/rules_test.py` revises the rules for every check, so a stale hit fails it |
| A finding that does not say which pass produced it | `verify/rules_test.py` asserts `data.source == "rules"` on the pull; `verify/rules_live.lua` asserts it on the diagnostic |
| An unchanged document re-inspected on every save | `verify/rules_test.py` (no call for an unchanged file) and `jev-lsp` `engine::tests::a_document_the_changed_set_does_not_name_is_skipped_without_a_call` |
| An answer below the rule's floor published anyway | `verify/rules_test.py` — a below-floor answer publishes nothing, and the counts still say it was looked at |
| The test client's framing desynchronising on a header split across reads | `verify/lsp_framing_test.py` — the frame is completed on the next read, and a bad frame is reported and skipped by length instead of killing the reader |
| A client the server was not written against cannot get a finding | `verify/omp_lsp.sh` — OMP receives the rule's finding through its own `lsp` tool and reaches `jev.inspect`; the no-rules control receives nothing |
| A rule that matches nothing because the pattern is root-relative | `verify/cli_parity.py`'s nested-file case (a repository-relative `applies_to`, below the root) and `jev-core` `gates` tests — the path is reduced to the workspace root before matching |
| The CLI finding no rules where the server finds them | `verify/cli_parity.py` — a nested file (`crates/.../rules.rs`) with the rules at the repository root: both front ends report the same findings. The CLI used to look for `.jev/rules/` beside the file |
| A workspace sweep dying on the chat tiers' call cap | `verify/rules_test.py` and the budget tests — a rules pass takes a permit from `budget.max_decisions_per_min` (its own window), not `max_calls_per_min` |
| A transport failure reported as a bare URL | `verify/soak.py` / `verify/real_model.py` against an unreachable endpoint — the row carries the whole `anyhow` chain (`POST <url>: <cause>`) |

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
| "The findings are quiet enough to live with" | `verify/quality_eval.py` against a real endpoint — `google/gemini-2.5-flash-lite` via OpenRouter: 3/4 defective files caught, 3/3 findings at a planted defect, **0 findings across the two clean files**, 0 discarded (one miss, run-to-run variance on a cheap model). The suite reports the row as `?` when no endpoint is configured; §7 has the invocation and the older `deepseek/deepseek-v4-flash` figures |
| "What the user does with an offer is known" | `verify/outcome_test.py` — `jev.usage` counts it and the line is in `<root>/.git/jev/session.jsonl` |
| "The repository's own rules are what runs ambiently" | `verify/rules_test.py` green, and `verify/rules_live.lua` green on both Neovim versions — the rule's finding reaches the sign column carrying the rule's title, the judgement's reason and `data.source = "rules"` |
| "Both front ends answer a rule the same way" | `verify/cli_parity.py` — `jev inspect` and the LSP path produce identical findings for the same rules and text |
| "A pass that had nothing to run says so" | `verify/rules_test.py` — `("no_rules", …)` in the result, and `:Jev inspect` renders the skip section |
| "It is an LSP server, not a Neovim feature" | `verify/omp_lsp.sh` green — OMP, through its own LSP support and with no code from this repository, receives a rule's finding over `textDocument/diagnostic` and calls `workspace/executeCommand jev.inspect` |
| "The harness itself is not the source of an intermittent red run" | `verify/lsp_framing_test.py` green — the split-header, bad-body and length-less-header cases, all deterministic and server-free |

Anything not covered above is reported as unverified, with the exact probe that would
settle it.

## 7. Real endpoints

The three harnesses below run against a real endpoint on request. They are runnable whenever one
is reachable — the suite's `?` means "no endpoint configured", not "cannot run". All three set
`"rules": {"enabled": false}` in their `workspace/configuration` payload, because they measure the
**chat review**, which is no longer the ambient pass (the rules pass is).

```sh
export OPENROUTER_API_KEY="$(cat ~/.omp/agent/openrouter.key)"   # the value, never printed
export JEV_API_KEY_ENV=OPENROUTER_API_KEY                        # the NAME, for the chat tiers

python3 verify/quality_eval.py --base-url https://openrouter.ai/api/v1 --model google/gemini-2.5-flash-lite
python3 verify/real_model.py   --base-url https://openrouter.ai/api/v1 --model google/gemini-2.5-flash-lite
python3 verify/soak.py         --base-url https://openrouter.ai/api/v1 --model google/gemini-2.5-flash-lite --rounds 2
```

`JEV_API_KEY_ENV` names the variable holding the chat tiers' key (`models.reason` and
`models.review`); the decide tier keeps its own name (`api_key_env`, default `TYPESAFE_API_KEY`).
Each harness exits non-zero when no model was reached, so `0 findings, 0 edits` cannot be mistaken
for a clean run.

**`quality_eval.py`** — is the review right, on six labelled fixtures:

| | |
|---|---|
| `google/gemini-2.5-flash-lite` (OpenRouter) | recall **3/4**, precision **3/3**, **0 findings on both clean files**, 0 discarded; 6 calls / 2950 tokens billed, 4.5 s, exit 0 |
| the miss | `swallowed_error.py` drew no finding (`discarded=0`, so nothing was dropped for an unlocatable anchor). A control run with the same model caught 4/4 — run-to-run variance on a cheap model |
| `deepseek/deepseek-v4-flash`, 2026-09-18 | 4/4 recall, 4/4 precision, 0 findings on the clean files |

**`real_model.py`** — the end-to-end loop, one run: the ambient review pass 0.8 s and 1 finding
(`file handle is never closed`, line 5), 9 actions offered including `quickfix.jev`, resolve 0.7 s
with `state=ready`, 1371 tokens across 2 calls, exit 0.

**`soak.py`** — two rounds over Python, Rust, Go, TypeScript, Markdown and an unknown file:
**10/12 runs applied an edit, 2 left the file unparseable, 12/12 runs billed**; ambient
0.6–60 s (the 60 s rows are runs where the review found nothing — the loop waits for a finding
rather than for the pass to finish), resolve 0.7–2.1 s. It exits 1 because of the two broken
files. Both markdown runs were model answers with no replacements, rejected by the server with
the reason kept in the row.

### The decide tier against a real Jev endpoint

**The route in force now: OpenCode Zen** (`https://opencode.ai/zen/v1`), wire `system_one`
(`POST {base}/systemone`), model `jev-1.13`, key from the harness's own credential store:

```sh
TYPESAFE_API_KEY="$(cat ~/.omp/agent/opencode.key)" JEV_DECIDE_TIMEOUT_MS=15000 \
JEV_DECIDE_WIRE=system_one JEV_DECIDE_BASE_URL=https://opencode.ai/zen/v1 \
JEV_DECIDE_MODEL=jev-1.13 target/release/jev inspect --force handler.rs
```

- through the shipped CLI: `rc=0`, **514 in / 29 out tokens, 732 ms**, the finding published at
  **p = 0.91**; through the plugin, after a real save: `source=jev` on line 4 at **p = 0.87**;
  through OMP, with no project `.omp/` and no `JEV_DECIDE_*` in the environment:
  `4:5 [warning] [jev] Unwrap in a request handler … (p=0.90)`;
- eight observations on this route ranged **p = 0.84–0.91** (OpenRouter, same model: 0.86–0.88) —
  same judgement, ±0.04 of endpoint noise;
- **the wire is not optional here**: `open_router` against Zen is `HTTP 404`, so the request must
  be `system_one`. `opencode-go` (`…/zen/go/v1`) carries **no Jev** — its decision route answers
  `400 Model is unavailable` — and `jev-1.13-free` answered **429 `FreeUsageLimitError`** three
  calls into a burst, so it is not a default;
- **the ceiling is a measurement**: at the shipped `timeout_ms = 5000` a client-attached call
  failed with `decision call failed: POST …/systemone: timeout: global` while the CLI on the same
  route succeeded (732–929 ms typical, 4.98 s once on the free tier, plus the client's first-call
  config grace), and the same run answered `ok` at `15000`. Zen's price is **not** visible: the
  response carries `usage` and no cost field.

**The OpenRouter route**, kept as the documented alternative because its price is visible and it
has no rate-limit surprise — wire `open_router` (`POST {base}/alpha/decisions`), model
`typesafe/jev-1.13`:

```sh
export TYPESAFE_API_KEY="$(cat ~/.omp/agent/openrouter.key)"   # the name api_key_env holds
JEV_DECIDE_WIRE=open_router JEV_DECIDE_BASE_URL=https://openrouter.ai/api \
JEV_DECIDE_MODEL=typesafe/jev-1.13 target/release/jev inspect --force handler.rs
```

- one `.unwrap()` in a handler-shaped Rust function answered `noul = true` at **p = 0.84**, above
  the rule's 0.75 floor, and the finding reached stdout with `exit=0` (504 in / 29 out tokens,
  0.33 s);
- eight observations of the same question on the same fixture ranged **p = 0.82–0.86** — the
  endpoint does not reproduce a fixed value even at `temperature: 0.0`, so a rule floor close to
  the answer will flip between runs (measured, not inferred);
- the endpoint billed **532 tokens ≈ $0.000021** for one decision (its own `usage.cost`, replayed
  raw and matched against the server's `budget.tokens_used`), and the finding's shape was
  identical to the stub's — same `label`, `line`, `start_line`/`end_line`, `severity`, `verb`,
  counts and exit code — with a different `id`, because the id hashes the detail and the two
  details differ;
- the key had to be exported under the name `api_key_env` holds: with the wire corrected but the
  key as `OPENROUTER_API_KEY`, the call failed loudly — `decision call failed: POST
  https://openrouter.ai/api/alpha/decisions: <cause>`, exit 1 — rather than silently; the
  settings channel can name another variable (`models.decide.api_key_env`), the environment
  cannot. The cause is printed because the failure formats the whole `anyhow` chain (`{e:#}`).

`JEV_DECIDE_WIRE` spellings were checked end to end: `openrouter`, `OPEN_ROUTER` and
`" OpenRouter "` all selected `/alpha/decisions`; `openroute` (a typo) left the wire unchanged and
posted to `/systemone`, which is the documented behaviour, not a fallback.

**The older gateway run**, kept for comparison: `deepseek/deepseek-flash` through the omp auth
gateway (which resolves the credential server-side), six consecutive runs after the three defects
below were fixed:

| | measured |
|---|---|
| ambient review | 2.2–3.1 s, 1 finding each |
| `codeAction/resolve` | 1.1–5.1 s, edit returned every time |
| tokens per session | ~1.4–2.2 k |
| resulting file parses | 6/6 |

This is what the fast path buys: the menu itself is still a cache read, and the seconds are
spent only after a pick.

### Local model (2026-09-18)

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

### The rules pass, and the two harnesses that pin it

The ambient pass is the rules pass, and it is pinned from two directions — the protocol and the
editor:

**What the shipped floors do on real code.** This repository's ten-rule set, over **30
`crates/**/*.rs` documents**: **99 candidates and 2 findings** — both from
`no-unwrap-outside-tests`, on one line of `crates/jev-lsp/src/server.rs` — with 7 rules considered
per document and every other rule publishing nothing. Per-rule candidate counts summed to the
server's own per-document total, so the attribution is the server's, not a harness's. That sweep
ran at the floors then in force (0.75 for that rule); the rule ships **0.85** now, and measured
against it: 15 runs of that document published **nothing** (at 0.75, 4 of 15 runs published one of
its two lines and not the other), while the fixture that must fire answered **0.97–0.98** and
published 15 of 15 at both floors. The false band's measured top is 0.79 and the true sample's
bottom is 0.97; 0.85 sits in that gap (`docs/TUTORIAL.md` §3.7 has the guidance this is the worked
instance of).

- **`verify/rules_test.py`** (45 checks) drives it end to end against the real binary and the
  scripted endpoint: a malformed rule file skipped with a reason while the rest still load,
  `regex` and `absent` semantics, `max_matches` meaning "only when the file holds more than
  this many", a below-floor answer publishing nothing, N candidates costing **exactly one**
  decision call, an unchanged document skipped without a call, a cache hit making no call,
  `data.source == "rules"` on the pull, and `("no_rules", …)` when nothing claims the file.
  Each check writes its own revision of the rules, because the rules' hash is part of the cache
  key — a revision is a different question, which is what makes "exactly one call" an assertion
  about *this* check rather than about whatever ran before it.
- **`verify/rules_live.lua`** (0 failures, 0 skips on Neovim 0.12.5 and 0.12.1) drives the same
  claims through a real editor: a rule's finding arrives on the `.unwrap()` line within 30 s of
  the save, coded with the `finding_id` its `data` carries, its message carrying the rule's
  title, the rule's prose and the reason the judgement gave, and `data.source = "rules"`;
  `:Jev inspect` dispatches through the command surface and reports the same finding on the
  same line with its reason, the counts, and a skip section; `unchanged` appears for a document
  git calls unchanged and `--force` re-runs it; the finding is still dismissible, still stays
  gone on a fresh pull, and `:Jev usage` still counts it.

**The suite supervises its stub, and that is not housekeeping.** Twice in this project a red
harness turned out to be a **stale stub** bound to the port: the process answered `/health`, so
every pre-flight passed, and then served whatever state it had been left in — which, from the
client's end, is indistinguishable from a product defect (no findings, no edit). So the runner
used for the table in `STATUS.md` kills any leftover stub, waits until the port is actually
free, starts exactly one stub for the whole run, and only then lets a harness near it; it also
refuses to adopt a stub that answers `/health` while another suite may be mid-run
(`REFUSE_IF_BUSY=1`), because nothing can tell a sibling's live stub from a stale one. It checks
the **pid** as well as `/health`, since a process that lost the race for the port exits while
the port keeps answering.

Two negative controls are recorded for the rules harness, and both are meant to be red in a
particular way:

| Control | What it looks like |
|---|---|
| the decide endpoint is **dead** (`JEV_DECIDE_BASE_URL` at a port nobody listens on) | 0 failures, **4 skips**, each naming why (`…/health did not answer (curl exit 7)`). "No endpoint" must not read as "no product" |
| the stub is **alive but answers nothing usable** | **4 failures, 1 skip** — the pass records "cached 0 finding(s)" and no diagnostic ever arrives. Nothing in those failures says *stub*, which is exactly why the runner checks the pid and why this control is kept |

The second control is the one worth keeping: it reproduces, on purpose, the shape of a
regression report, and the only way to tell the two apart is the stub's own liveness and
identity.

### Two defects the suite found in itself

Both of these were red harness rows that had nothing to do with the product, and they are
different failures with different fixes. They are recorded together because the lesson is the
same one: "the harness is red" is not the same claim as "the product is broken", and a suite that
cannot tell the two apart will send you hunting in the wrong repository.

#### 1. The test client's framing — a dead reader thread

`verify/lsp_framing_test.py` (9 checks) exists because of a bug in **our own test client**, and it
is worth reading before blaming the server for an intermittent timeout.

`verify/smoke.py`'s `Lsp._read_message` read the message header **one byte at a time** into a
local buffer with a one-second deadline, and **discarded what it had read** when that deadline
expired. Under load the deadline could expire mid-header; the next call then began in the middle
of a message, mis-framed everything after it, and — through the reader thread's blanket
`except Exception: break` — killed the reader for the rest of the session. Every later request
timed out at 30 s, which is exactly how it presented: `verify/latency.py` red **two runs in
three**, on a server that was innocent. It pre-dated this refactor and the rules pass.

Three fixes, each with a check:

* framing state lives in a **per-instance buffer**, so a deadline that expires mid-frame *keeps*
  the bytes and the next read completes the frame;
* an unreadable frame is **reported** (`FramingError`, recorded in `framing_errors`) and skipped by
  its stated length, so the next good frame on the same stream is still delivered — rather than
  the reader dying quietly;
* a header with no usable length **raises** instead of guessing where the following frame begins.

The regression test drives the reader over a real pipe, with no server and no product code: a
header split across two reads with a sleep past the deadline; a bad body followed by a good frame;
a length-less header; two frames arriving in one read; and the error contract (`FramingError` is a
distinct, reportable error rather than a timeout). Deterministic, fast, and it needs neither a
model nor a server.

#### 2. An assertion with no wait — a race reported as a defect

The second one is subtler, and it is why `verify/smoke.py` was intermittently red on a healthy
server. Its refresh assertion checked `server.saw_request("workspace/diagnostic/refresh")`
**immediately** after the model call appeared, but the server sends that refresh *after* the pass
finishes — so under load the assertion ran first, found nothing, and reported a race as a defect.

The fix gives the assertion its own wait: up to 20 s after the model call is seen, polling for the
refresh, and when it still does not arrive the failure names **what it saw and how long it
waited** (`seen 0 after 20.0s; server log: …`) rather than leaving a bare mismatch to interpret.
That is the same discipline as the framing fix: a harness may be impatient, but it may not be
impatient *and* silent about it.

Together the two are what make the table trustworthy rather than merely green: **three consecutive
full-table runs** with `FAIL`/`SKIP` counts of zero and identical summary blocks, on a suite that
used to be red two times in three.

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
  the plugin's own path: `verify/nvim_live.lua` asserts `:Jev undo` restores the buffer
  byte-for-byte, which is the behaviour the product depends on. Plain `u` remains unmeasured.
- **Model quality** on any verb. The suite proves the pipeline, never the usefulness of a
  particular model's output; that is measured by the user, in the editor, and recorded
  separately.
- **A local model, since the rules pass became the ambient path.** The runs in §7 are hosted
  (OpenRouter). The local `llama.cpp` soak in §7 is from 2026-09-18, before the ambient pass
  changed, and no local server was started for the current numbers. The chat harnesses are run
  the same way against one:
  `JEV_BASE_URL=http://127.0.0.1:<port>/v1 JEV_MODEL=<model> python3 verify/real_model.py`.
- **The hosted default decide endpoint.** `api.typesafe.ai` has not been called from here. What
  *was* called is the same model through a different route — OpenRouter's
  `/alpha/decisions`, `typesafe/jev-1.13`, eight observations at p = 0.82–0.86 (§7) — so the
  wire, the body, the parsing and the key name are verified against a real decision endpoint;
  what remains untested is the vendor's own base URL and its credential. A local System One
  server (`http://127.0.0.1:8009/v1`, model `kev-latest`) is likewise unexercised. A hosted cold
  start was seen to exceed the tier's 5000 ms default once (7.15 s, reported as a transport
  error); `JEV_DECIDE_TIMEOUT_MS` is the knob for that, and no measured call has needed it since.
- **Real-endpoint latency** beyond the samples in §7. The `codeAction` budget is asserted
  against the stub; the hosted chat numbers are ambient 0.6–60 s and resolve 0.7–2.1 s on a
  cheap model, which is a measurement rather than a budget.

## 11. Known limitations

Recorded because they are deliberate boundaries, not oversights:

- **Inline completion is gone, and with it the two boundaries it used to carry.** Ghost text
  was verified interactively (a headless Neovim fires none of `InsertEnter` / `CursorMovedI` /
  `TextChangedP`) and `inlineCompletionProvider` had to be injected into the `initialize`
  response because `lsp-types` 0.94 cannot express it. Both are moot since 2026-09-19: the
  method, the capability, the `fim` tier and the `<Tab>` acceptance were removed, along with
  `crates/jev-lsp/src/advertised.rs` and the transport-boundary wrapper that existed only to
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
- **Nothing in `crates/` parses the result of an applied edit, and that is a decision rather than
  an omission.** Every alternative was weighed against the one failure this project has actually
  measured. A *structural probe* is cheap enough (µs, in `edit.rs`, no dependency) and would have
  to run on `predict_after`, before the client applies anything — but its balance form is blind to
  the failure that prompted it (the replacement's delimiters were balanced), and its indentation
  form (an opener at the end of the range with the following line not indented deeper) flags that
  failure only when gated on `Profile.braces == false`; ungated, it flags legal code, and a probe
  that refuses a valid edit is worse than one that misses an invalid one. A *parser behind a
  feature or a subprocess* would be the largest dependency this repository carries (tree-sitter
  grammars), puts language knowledge in the server where N10 keeps it out, and `rustc
  --emit=metadata` needs a crate context a single buffer does not have — while the parser that
  already holds the file is the user's own language server, whose verdict reaches the client and
  not us. *Correlating the client's own diagnostics* is the cheapest true signal there is —
  `CodeActionContext.diagnostics` is already populated on every code-action request
  (`nvim/lua/jev/picker.lua:204-226`) and the plugin already pushes a versioned
  `{uri, version, definitions, context}` document per change (300 ms debounce,
  `nvim/lua/jev/init.lua:354-423`, guard-tested server-side) — and it is the upgrade to make the
  day a user complains: as `INFORMATION` attributed to the client, never as `ERROR` (a parser that
  has not run yet, or another server's stale diagnostic, cannot be read as proof), with jev's own
  findings excluded from the correlation. Until then `state = "ready"` means *the contract
  passed*, never *the file still compiles*, and the party that knows is the client that parses.

  What the server does validate on both paths: `edit::build_proposal` checks that every anchor
  locates exactly once, that a replacement does not duplicate lines it did not consume, and the
  scope and version stamps. It never parses, and no language-level check exists anywhere.

  The shape this misses, measured three times on `google/gemini-2.5-flash-lite` (§7): the answer
  replaced one statement with a block header and did not re-indent the body —

  ```diff
  -    f = open(path)
  +    with open(path, encoding="utf-8") as f:
         return json.load(f)["port"]
  ```

  — `state = "ready"`, the anchor resolved where the server said it did, and the file no longer
  parses. That is the model choosing the wrong anchor granularity, and it is why `soak.py` and
  `real_model.py` call `ast.parse` on the result and report `BROKEN`/`unparseable`: the harness
  is the only place that question is asked. `docs/MODEL.md` §5's repair loop answers a different
  question (did the answer satisfy its contract, and can it be anchored).
- **The post-apply prediction check covers the plan path only.** `edit::predict_after` materialises
  the exact post-edit text in µs, and `verify_prediction` compares the next synced text to it and
  publishes the `ERROR` divergence diagnostic (PROTOCOL §8) — but `remember_prediction` and
  `record_applied` are called from one place, the `jev.apply` step path
  (`crates/jev-lsp/src/server.rs:2581, 2592`). On a **resolved code action** — the most common way
  an edit reaches a buffer — there is no prediction recorded, so the comparison returns early and
  nothing is checked. The server validates anchors, duplicates, scope and version stamps on both
  paths, and compares the client's applied bytes against its own prediction on the plan path only:
  on a resolved code action it can be wrong and silent, and `:Jev revert` does not know about it
  either. The fix is the same correlation as above, and it is not built.
- **The client's diagnostics already reach the server and are dropped.** The plugin fills
  `CodeActionContext.diagnostics` from `vim.diagnostic.get(bufnr)` on every code-action request
  (`nvim/lua/jev/picker.lua:204-226`), and pushes a versioned `{uri, version, definitions,
  context}` document on every change with a 300 ms debounce
  (`nvim/lua/jev/init.lua:354-423`, guard-tested server-side as `KnownDocument`); the server reads
  `params.context.trigger_kind` and nothing else of the context. That is why the correlation above
  is ~30–40 lines rather than a new subsystem — the wire already carries the signal.
