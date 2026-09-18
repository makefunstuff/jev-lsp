# STATUS

**Objective**: a working LSP that makes Neovim an AI-driven harness — the model observing in
the background and proposing work through native surfaces — rather than a prompting TUI.

**State**: working and verified end to end. Design frozen in `PROTOCOL.md`; implementation
in `crates/` and `nvim/`; verification in `verify/`.

**Next action**: the review's recall is now measured rather than asserted — 4/4 planted defects
caught, 4/5 precision, zero discards, with the local model — and the one open question about it is
whether the defect classes added to the prompt this session are *why*, which needs an A/B against
the prompt as it was. Otherwise: an interactive check of inline-completion ghost text, which
headless Neovim cannot fire the insert-mode events for, and which is the only surface never driven
live.

**Previously**: everything in the roadmap is built. The next useful step is a longer
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
| 2026-09-18 | **Hover, project context, `meta.where`, and a quality metric — with two retractions of my own claims.** `textDocument/hover` shows what has already been explained about the scope under the cursor, read from an artifact store and never from a model (verified: silence in 1 ms for an unexplained scope, the markdown for an explained one, the call counter unmoved). Requests that *generate* now carry what the editor can see — imports from the parser, references from the other language servers, the covering test, the open buffers — bounded server-side, ordered, and hashed into the cache key, which is what keeps a cached answer from being served for a question asked in a different project state. The completion cannot assemble that per keystroke, so it gets the cheap half pushed per document version (`meta.document` replaces `meta.definitions`, one guard for both payloads). `:Meta where` greps locally with the question's own words and lets the model rank — the navigation question no index answers, riding the follow-up channel and needing no new server surface. **The retractions:** `verify/quality_eval.py` (recall, precision and noise on planted defects) first reported 1/4 caught identically across a local model, a frontier model, and a prompt naming every defect class, and I said the misses were 'in the pipeline'. Both readings were harness artifacts: the wait keyed on *any* `workspace/diagnostic/refresh`, and `saw_request` accumulates, so after the first file it stopped waiting and called every later file a miss — the server's record showed one analysis in the whole run; and with that fixed the wait is still wrong. **The 25% recall and 'the pipeline is at fault' are retracted.** Three of the harness's own bugs followed: a wait that keyed on an accumulating notification, a wait that then keyed on the wrong file, and two fixture labels whose line numbers were simply wrong. With all three fixed and every row backed by the server's own record, the numbers are: **recall 4/4 (100%), precision 4/5 (80%), zero findings on the clean writer** — the fifth finding is on the file labelled clean and says the code accesses `'port'` without checking the key exists, which is a defensible judgement about a file, not noise. Each file produces exactly one finding, none is discarded, and the phrasings for the two defects the earlier runs 'missed' — `mutable default argument`, `index used without checking the collection is non-empty` — match the wording added to the prompt this session, which is suggestive and **not** proven: the runs that would have been the control were the broken ones. |
| 2026-09-18 | **Scope: the parser answers first where there is one.** `explain` and `plan` now carry an explicit `range` when treesitter can name the enclosing declaration, and the server anchors on it (`scope_source = explicit`); without a parser, the language not listed, or no such declaration, the range is absent and the server resolves structurally exactly as the CLI does. Found while wiring it: `cursor_scope` was defined **twice** in the plugin, and everything before the second definition closed over the first — so `explain` used the unpatched one and `plan` the patched one. Merged to a single definition. |
| 2026-09-18 | **Code lenses: the affordance you do not have to remember.** One lens per declaration at the left margin, above the declaration it belongs to: `meta: explain` on a clean one, `meta: N finding(s) · fix` on one with cached findings. Returned fully formed with `resolveProvider: false`, so a document costs one request and no lens costs a request — and `workspace/codeLens/refresh` after an analysis updates the titles. `meta-core::scope::blocks` is the converse of `resolve`: it enumerates what `resolve` can find, reusing the same structural rules, and skips a declaration longer than the cap rather than offering an affordance the server would refuse. The commands are `meta.plugin.`-namespaced and handled in the plugin, because opening a buffer is a client decision (`window/showDocument` needs a URI an explanation does not have); `vim.lsp.codelens.run` is wrapped to dispatch them, everything else passes through. Verified: lenses arrive and are stored for a fixture with two declarations, running one opens the artifact it promised. Found on the way: Neovim's lens provider asserts on a client that has gone (`lsp/codelens.lua:143`) when a stop lands inside its 200 ms debounce, and `enable(false)` does not purge the stored id — unreachable from a plugin, so the harness lets the debounce settle before stopping, and it is recorded as Neovim's. |
| 2026-09-18 | **Streaming: the answer is written as it arrives.** `meta.explain` now streams. The model client speaks SSE (`chat_stream`, with the whole-answer path as the default for a backend that cannot stream, and a fallback when a server ignores `stream: true` rather than reporting an empty answer); the engine threads a delta callback; the server reports the text *so far* as `$/progress` partial results under the token the client already issued (§3.5.1, N12) and reports `waiting for the model (Ns)` during the prefill, because staying silent is indistinguishable from hanging. Measured against the local model: 49 progressive states from 1 B to 1487 B, done in 10.7 s, with the first 4.1 s covered by heartbeats. No custom methods — N6 holds. Two bugs the tests caught: a stream buffer leaked when the request failed, and naming a buffer then renaming it leaves a stub buffer behind (Neovim behaviour, reproduced in isolation), so the stream buffer is now named once, at the end. |
| 2026-09-18 | **Two defects that only a real config could show.** Pressing the action key in a config with snacks.nvim: `meta: the picker failed: snacks/picker/format.lua:350: attempt to index local 'ctx' (a nil value)`. The picker passed `kind = 'codeaction'` to `vim.ui.select`; snacks branches on that and expects Neovim's `{action, ctx}` pair, while our items carry `{action}` — so the picker died before it drew. Removed: `format_item` is the documented contract and is enough. Then `meta.plan` passed its arguments as a map where `ExecuteCommandParams.arguments` is an array, so the transport rejected it before the command ran (`invalid type: map, expected a sequence`) and every `:Meta plan` failed. Both pinned: one check drives a chooser that branches on `kind` the way snacks does, another asserts every command entry point sends an array. Both proven to fail with the bug reintroduced. |
| 2026-09-18 | **Installed into a real config and verified there.** The answer to "it doesn't work as is" was that it was not installed at all: that session had zero LSP clients and no config reference to meta, and it had been running five days. Loaded live it worked, and after one spec block in `~/.config/nvim/lua/plugins/init.lua` a fresh Neovim with that config reports `clients: meta,pylsp` and lands the finding. Two integration decisions the surrounding config forced: the keymap prefix moved to `<leader>M` (`<leader>ma` is Telescope marks, `<leader>mb` is make) and inline completion is off (llama.vim owns ghost text). Also verified: the server exits when its client is killed, so a dead Neovim leaves no processes behind. |
| 2026-09-18 | **Testing against a real configuration, as asked, found three more defects.** (1) The visual config's own fixture was opened from `VimEnter`, where Neovim suppresses autocmd triggering — the file loaded and the attach pass never saw it, so the config that exists for trying the thing reported "no client attached". Deferred with `vim.schedule`; verified interactively. (2) `meta.review` was not in the server's command list, so `:Meta review` and `<leader>mr` answered "not implemented" on every press. Wired to the same engine call the save path makes, returning findings in the Result. (3) The visual config's banner checked for the client once at 1.5 s and reported failure for a client that had simply not finished starting. It polls now. The `/tmp`-rooted workspace was also ruled out as a cause by direct test (1 finding, 5 s). |
| 2026-09-18 | **Visual-testing the real thing found a startup race.** A buffer saved before the client's first `workspace/configuration` reply was analysed against the built-in endpoint, so a client that configured one watched the server call somewhere else and get nothing back — the first thing a user would have hit, and invisible to every harness because they all sleep before acting. Model work now waits for the settings (`verify/config_race_test.py`, proven to fail with the gate removed). Also fixed: **every model call blocked a tokio worker thread** — the synchronous HTTP client called directly from `async fn`s, including inline completion at a 200 ms cadence. And `:LspLog` now states which endpoints are in force. |
| 2026-09-18 | **Found the defect that every test missed.** `diagnosticProvider.workspaceDiagnostics` had been advertised since the first capability block while `workspace/diagnostic` was never implemented; Neovim prefers the workspace branch on refresh when that flag is set, so the client stopped pulling per-document diagnostics altogether and findings never reached the sign column. Every harness passed because the Python ones pull by hand and the Neovim ones assert a *code action* resolves, which reads the server's cache rather than the buffer. Found by writing a dismissal test, which needs a displayed diagnostic to dismiss. Fixed by not advertising what is not served; `verify/smoke.py` now asserts the sub-capability too, and `verify/dismiss_test.lua` (9/9) covers dismissal end to end. |
| 2026-09-18 | **Local-model soak, on the user's go-ahead.** `qwen3.6-35b-a3b-iq3xxs` on the local llama.cpp server: **6/6 applied, 0 unparseable** across Python, Rust and TypeScript — more reliable than the cloud model on the same fixtures, and slower (ambient 5.7–12.4 s, resolve 7.3–15.5 s). Measured while running: `chat_template_kwargs.enable_thinking=false` *is* honoured locally and is *ignored* by the auth gateway, so a bare request to the local server spent 47 s producing 16 tokens of reasoning and no answer, while the server's own request answered in 0.9 s. |
| 2026-09-18 | **Soak across languages, and three fixes it forced.** `verify/soak.py` drives Python, Rust, TypeScript, Go, Markdown and an unidentifiable config file through the whole loop against a real model, asking the one question no unit test can: does the file still parse. It found that an answer the server cannot apply was never re-prompted (docs/MODEL.md §5 had specified it), that a 30 s resolve timeout turned a 66 s reasoning answer into a transport error, and that one repair attempt is not enough for a model that repeats itself. After widening repair to two attempts, covering applicability failures, and raising the ceiling and timeout together: **8/9 applied, 0 unparseable**, against 2/6 and 8/12 on the same fixtures. The one remaining failure is the re-emission guard refusing rather than corrupting. |
| 2026-09-18 | **Interactive verification of inline completion.** In a real PTY, Neovim fires the insert-mode events headless withholds, asks at the cursor, and renders the ghost text — while a headless run fires zero of those events. Driving the real client also exposed a defect the headless tests had missed: `require('meta').setup({settings = {inline_completion = …}})` placed the section at the top level while the server reads it by the name `meta`, so the setting was silently dropped and every completion was refused with "inline completion is off". `configure()` now accepts either shape. |
| 2026-09-18 | **U7 built after all.** The blocker was `lsp-types` 0.94 having no `inlineCompletionProvider` field, not the feature being hard: the field is now injected into the `initialize` response at the transport boundary, `textDocument/inlineCompletion` is served as a custom method, and the whole gate stack (off by default, prefix floor for timed requests only, its own per-minute window, a position-keyed answer cache) is verified 14/14. Found and fixed a second real defect on the way: `workspace/didChangeConfiguration` was unimplemented, so the plugin's kill switch could never have taken effect. |
