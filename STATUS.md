# STATUS

**Objective**: a working LSP that makes Neovim an AI-driven harness — the model observing in
the background and proposing work through native surfaces — rather than a prompting TUI.

**State**: working and verified end to end. Design frozen in `PROTOCOL.md`; implementation
in `crates/` and `nvim/`; verification in `verify/`.

**Next action**: everything in the roadmap is built. The next useful step is a longer
real-model soak — the six runs so far are one language on one model, and the last three
defects were all real-model-only — and an interactive check of inline-completion ghost text,
which a headless harness cannot drive.

---

## Open questions

1. **Which model, and at what cost.** Everything automated runs against `verify/stub_model.py`.
   A real run needs an endpoint and a model name; the local servers serve
   `qwen3.6-35b-a3b-iq3xxs` (port 37313) and `qwen3.8-27b-gsq-rco-iq3xxs` (port 40583). No
   automated test has touched either, because waking a sleeping model consumes VRAM and GPU
   arbitration here is manual. Awaiting the user's call.
2. **Whether `triggers.diagnostics` should stay `save`.** `save` is predictable and cheap;
   `idle` is more ambient but fires while typing. Default is `save`; measured cost on a real
   model will decide it.

## Decisions taken (reversible, recorded so they are not relitigated)

- Repository directory is `meta-lsp`; workspace, binary, and crates are `meta*`.
- UTF-8 position encoding, frozen (PROTOCOL N1).
- No custom `meta/…` LSP methods (N6). `workspace/executeCommand` plus `$/progress` is the
  whole back-channel.
- **Advertise only what is served** (PROTOCOL §2). `codeLensProvider`, `inlayHintProvider`,
  `inlineCompletionProvider` are designed but not advertised; `meta.plan`/`apply`/`revert`
  are specified but not advertised.
- **`explain` is a command, not a code action**, because a resolved action's `command` is
  executed by the client by sending it back to the server, so it cannot open a buffer.
- The plugin, not the server, owns UI that LSP cannot express: text input, scratch buffers,
  undo snapshots.
- No treesitter dependency yet. Scope is structural with a whole-file fallback, and
  `scope_source` reports which was used.

## Verification backing the implementation

| Check | Result |
|---|---|
| `cargo test` | 200 passing, no warnings |
| `cargo build --release` | 4.9 MB, no warnings |
| `verify/probes/run.sh` | 7 probes green |
| `python3 verify/smoke.py` | 32/32 against the real binary |
| `python3 verify/lsp_client.py --server …` | 27 ok, 0 FAIL, 0 skip (independent client) |
| `nvim --headless -l verify/nvim_live.lua` | 0 failures, 0 skips |
| `python3 verify/plan_test.py` | 35/35 |
| `python3 verify/cli_parity.py` | 12/12 |
| `nvim --headless -l verify/nvim_ui_test.lua` | 0 failures, 0 skips, 48 ok |

The independent client is written from the specification and shares no code with the server;
it caught two things the unit tests could not, both now resolved and one of them documented
as a deliberate boundary in `docs/VERIFICATION.md` §10.

## Log

| Date | Event |
|---|---|
| 2026-09-18 | Probe pass over the Neovim 0.12.5 LSP runtime; two surprises recorded (absent-version crash, arbitrary-token progress) |
| 2026-09-18 | Design frozen: `PROTOCOL.md` plus `docs/{ARCHITECTURE,UX,MODEL,VERIFICATION,ROADMAP}.md` |
| 2026-09-18 | Audit found the streaming design smuggled a bare progress token; fixed and verified by `verify/probes/streaming.lua` |
| 2026-09-18 | The frozen contract exercised as a whole exchange by `verify/probes/trace.lua`; `source.meta` was missing from the declared kinds |
| 2026-09-18 | Prior-art sweep (`docs/research/prior-art.md`): none of the four comparable projects does ambient findings, splits latency, or supports unidentified files |
| 2026-09-18 | **Support is unconditional** (N10/N11): measured that `filetypes = nil` still misses unidentified files, specified the plugin attach pass, proved 11/11 in `verify/probes/language.lua` |
| 2026-09-18 | **Implementation.** `meta-core` (13 modules) + `meta-lsp` (server, engine, state) + `nvim/lua/meta`. 106 unit tests. |
| 2026-09-18 | **Real bug found by end-to-end run**: a client answering `workspace/configuration` with `{}` reset every setting via `#[serde(default)]`, discarding the model endpoint the environment supplied. Fixed with a deep merge plus explicit environment precedence, with a regression test. |
| 2026-09-18 | First green end-to-end run: smoke 32/32, independent client 23 ok / 0 FAIL, live Neovim 0 failures, and a resolved edit landing in a real buffer with the model call carrying the right language flavour |
| 2026-09-18 | **Two scheduler defects found by the independent client and fixed**: an analysis request arriving while one was in flight was silently dropped, and a superseded run emitted no `workspace/diagnostic/refresh`, leaving a client that had pulled during the window empty forever. Replaced the global in-flight flag with a per-document queue that always signals. Independent client now 27 ok / 0 FAIL / 0 skip. |
| 2026-09-18 | **First runs against a real model**, via the omp auth gateway (`127.0.0.1:4000`), which resolves the provider credential server-side so no key is handled locally. Model `deepseek/deepseek-flash`. |
| 2026-09-18 | **Three real-model defects found and fixed**: (1) a reasoning model exhausts a 2048-token ceiling and returns empty content — the error now names `finish_reason=length`, and ceilings are 4096/2048; (2) a placeholder-shaped schema was echoed back with field names as values — the schema is now a concrete example plus an explicit anti-echo rule; (3) an answer covering more lines than its anchor duplicated code on apply — the replaced range now absorbs the lines the answer re-emits. The bounded repair loop specified in docs/MODEL.md §5, previously unimplemented, was added and is separately budgeted. |
| 2026-09-18 | **Green against a real model**: 6/6 runs produced a finding, an edit, and a file that still parses. Ambient 2.2–3.1 s, resolve 1.1–5.1 s, ~1.4–2.2k tokens per session. |
| 2026-09-18 | **U4, U5, U6, U8 built.** Plans apply through the server with per-step staleness refusal and revert; multi-file edits create files; post-apply verification publishes a divergence diagnostic when reality does not match the prediction. The CLI implements all five frozen exit codes, and parity with the LSP path is proven field-by-field (`verify/cli_parity.py`, 12/12). |
| 2026-09-18 | **U7 recorded as blocked, not skipped.** Neovim attaches its inline-completion handler only for a client advertising `inlineCompletionProvider`; `lsp-types` 0.94 (pinned by tower-lsp 0.20) cannot express that field — it first appears in 0.95.0. Writing the handler now would be dead code no client could reach, so it waits for the dependency move. |
| 2026-09-18 | Re-verified every harness after integration: 187 unit tests, 7 probes, smoke 33/33, queue 5/5, supersede 7/7, plan 35/35, parity 12/12, live Neovim 0/0, plugin UI 0/0. |
| 2026-09-18 | **Found the defect that every test missed.** `diagnosticProvider.workspaceDiagnostics` had been advertised since the first capability block while `workspace/diagnostic` was never implemented; Neovim prefers the workspace branch on refresh when that flag is set, so the client stopped pulling per-document diagnostics altogether and findings never reached the sign column. Every harness passed because the Python ones pull by hand and the Neovim ones assert a *code action* resolves, which reads the server's cache rather than the buffer. Found by writing a dismissal test, which needs a displayed diagnostic to dismiss. Fixed by not advertising what is not served; `verify/smoke.py` now asserts the sub-capability too, and `verify/dismiss_test.lua` (9/9) covers dismissal end to end. |
| 2026-09-18 | **Local-model soak, on the user's go-ahead.** `qwen3.6-35b-a3b-iq3xxs` on the local llama.cpp server: **6/6 applied, 0 unparseable** across Python, Rust and TypeScript — more reliable than the cloud model on the same fixtures, and slower (ambient 5.7–12.4 s, resolve 7.3–15.5 s). Measured while running: `chat_template_kwargs.enable_thinking=false` *is* honoured locally and is *ignored* by the auth gateway, so a bare request to the local server spent 47 s producing 16 tokens of reasoning and no answer, while the server's own request answered in 0.9 s. |
| 2026-09-18 | **Soak across languages, and three fixes it forced.** `verify/soak.py` drives Python, Rust, TypeScript, Go, Markdown and an unidentifiable config file through the whole loop against a real model, asking the one question no unit test can: does the file still parse. It found that an answer the server cannot apply was never re-prompted (docs/MODEL.md §5 had specified it), that a 30 s resolve timeout turned a 66 s reasoning answer into a transport error, and that one repair attempt is not enough for a model that repeats itself. After widening repair to two attempts, covering applicability failures, and raising the ceiling and timeout together: **8/9 applied, 0 unparseable**, against 2/6 and 8/12 on the same fixtures. The one remaining failure is the re-emission guard refusing rather than corrupting. |
| 2026-09-18 | **Interactive verification of inline completion.** In a real PTY, Neovim fires the insert-mode events headless withholds, asks at the cursor, and renders the ghost text — while a headless run fires zero of those events. Driving the real client also exposed a defect the headless tests had missed: `require('meta').setup({settings = {inline_completion = …}})` placed the section at the top level while the server reads it by the name `meta`, so the setting was silently dropped and every completion was refused with "inline completion is off". `configure()` now accepts either shape. |
| 2026-09-18 | **U7 built after all.** The blocker was `lsp-types` 0.94 having no `inlineCompletionProvider` field, not the feature being hard: the field is now injected into the `initialize` response at the transport boundary, `textDocument/inlineCompletion` is served as a custom method, and the whole gate stack (off by default, prefix floor for timed requests only, its own per-minute window, a position-keyed answer cache) is verified 14/14. Found and fixed a second real defect on the way: `workspace/didChangeConfiguration` was unimplemented, so the plugin's kill switch could never have taken effect. |
