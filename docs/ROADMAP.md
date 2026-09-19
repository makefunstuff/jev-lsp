# Roadmap

Each unit is independently verifiable and ends with its own acceptance check. No unit
starts before the previous one passes. Order is chosen so the two hard problems —
staleness and latency — are exercised by unit 1, not discovered in unit 6.

**Progress**: every unit built, U10 included, with two parts of U9 open. U9's documentation
criterion was met on 2026-09-19: `PROTOCOL.md`, `README.md`, `docs/{UX,MODEL,ARCHITECTURE,LANGUAGE,VERIFICATION}.md`,
`test/visual/README.md` and this file were each read against the implementation, and the
contradictions found were fixed — the §2 capability block was missing two advertised providers,
§3.2 and §3.4 listed designed-but-unserved methods without saying so, §6 was missing two
commands and marked two served ones "no", §10's settings example had keys that do not exist
(`verbs`, `languages.generic`) and the wrong name for one that does (`model` → `tier`), §3's
scheduler description contradicted `state.rs`, and the README called code lens and inlay hints
"not built" while the server serves both. It was met again on 2026-09-19 for U10: the same
documents were swept for the old name and re-read against the new surface — the rules pass, the
decide tier, `jev.inspect`, the keymap prefix, and the cache keys. U0–U8 done, including U5's plugin
half — the plan buffer renders the verified plan as steps, applies one at a time and takes them
back (`nvim/lua/jev/init.lua`, `M.open_plan`). U9 partly: cancellation is cooperative only,
backpressure is the per-document queue, and the daemon is not built. Two scoped gaps remain and are noted in their
units: U2's treesitter scope is still structural-only — grammars are a dependency decision,
not an oversight, and `scope_source` reports the fallback honestly — and its dismissal file
*is* implemented, in the plugin (`nvim/lua/jev/init.lua`: `:Jev dismiss` writes
`<repo>/.git/jev/dismissed.json` and `filter_findings` drops dismissed ids from the pull).
Inline completion (U7) was removed on 2026-09-19, at the user's decision: generated code is
asked for rather than suggested under the cursor.

## U0 — Scaffold ✅

Workspace, three crates, `nvim/` skeleton, `PROTOCOL.md` types in `jev-core` as Rust
structs with serde, plus the schema validator.

**Accept**: `cargo build --release` warning-clean under
`-D warnings`; `jev-core` unit tests green; `nvim/init.lua` starts and stops the server;
`verify/probes/language.lua` green — the attach pass covers the buffers the built-in
`FileType` path misses.

## U1 — The contract and the loop

The minimum honest slice: `initialize` with UTF-8 encoding, document sync, a stub model,
`textDocument/codeAction` served from cache, `codeAction/resolve` returning a versioned
`WorkspaceEdit`, and `workspace/diagnostic/refresh` after a background pass.

**Accept**: `verify/lsp_client.py` steps 1–8 green against the stub; defect-injection rows
for bare `changes`, missing `version`, stale `version`, and model-call-in-`codeAction`
all catch their defect.

## U2 — Language, scope, and real analysis

The language ladder and per-language profiles (`docs/LANGUAGE.md` §2–3), the scope
fallback chain (§4), the practical gates with their reported states (§5), then the context
builder, `review` tier, findings contract, anchor resolution with the ambiguity rule, pull
diagnostics with `resultId`, dismissal file, and the noise cap.

**Accept**: golden intents green; a fixture with **no** treesitter parser still offers the
full verb set and reports `scope_source = structural | whole_file`; an over-size and a
binary fixture are attached and report `over_size` / `binary` rather than going quiet;
live Neovim shows a finding on the right line after save; dismissing it survives a restart
and does not resurface.

## U3 — Action menu

Verbs `fix`, `harden`, `types`, `docs`, `rewrite`, `explain`; deterministic titles; the
`disabled` placeholder path; kind filtering; `isPreferred` on the single best action.

**Accept**: latency bench passes the `codeAction` budget; determinism test green; picking a
placeholder shows the reason in the native menu and returns no edit.

## U4 — Plugin picker and approval

`nvim/picker.lua` with a preview pane (rationale from `data.summary`), the streaming
consumer over `LspProgress`, and a statusline segment.

**Accept**: live Neovim test drives the picker end to end; the statusline shows the running
call within 100 ms of the trigger.

## U5 — Plan

`jev.plan`, the plan contract, the plan buffer with per-step state, diff view, apply,
skip, recompute, `:Jev undo`.

**Accept**: live Neovim applies a 3-step plan one step at a time, skips one, reverts one,
and `:Jev undo` restores the buffer byte-for-byte; a step whose target moved is marked
stale and is not applied.

## U6 — Test generation and multi-file edits

`test` verb with `resourceOperations: create`, file creation, a post-apply divergence check
(the applied bytes against the server's own prediction — local, no model call), divergence
diagnostics.

**Accept**: golden test creates a test file and edits the source in one `WorkspaceEdit`;
injected post-apply divergence produces an `ERROR` diagnostic naming the mismatch.

## U7 — Inline completion ✗ withdrawn

Built, then removed on 2026-09-19 at the user's decision: the ghost-text path (`fim` tier,
`inlineCompletion` handler, gate stack, `<Tab>` acceptance) is gone from the server, the
plugin, the contract and the harnesses. Generated code is asked for — an action, `:Jev ask` —
rather than offered under the cursor. `STATUS.md` records the decision.

## U8 — CLI parity

`jev explain|review|action|plan|status` with the exit-code contract.

**Accept**: CLI artifacts are byte-identical to the LSP server's results for the same
fixtures; all four exit codes exercised; `cargo build --release` still warning-clean.

## U9 — Hardening

`$/cancelRequest` end-to-end, backpressure caps, daemon (optional), `:checkhealth`,
`docs/` reconciled with the code.

**Accept**: cancel test green; the design documents contain no claim the code contradicts —
checked by reading each document against the implementation, not by a test.

## U10 — `jev`, and the ambient pass is rules ✅

The rename, the demotion, and the pass that replaces the chat review on the ambient path. The
old name is swept out of every crate, binary, plugin path, command, schema string, environment
variable and `.git/` path with **no alias and no migration shim** (the session and dismissal files
were not migrated: a dismissal recorded under the old name is lost once). The repository's
conventions became data — `.jev/rules/*.json`, `"schema": "jev.rules/1"`, an `inspection` that
names candidates and a `judgement` gated by `min_probability` — answered by a new `decide` tier
that speaks the decision wire rather than a chat, and served on demand by `jev.inspect` (LSP) and
`jev inspect` (CLI) through the same code the save path runs. `data.source` says `rules` or
`review`; there is no fallback from one to the other.

**Accept**: the whole table in one command — `bash verify/run-suite.sh <out-file>` — with
`cargo test` and `cargo build --release` warning-free; `verify/rules_test.py`,
`verify/cli_parity.py` and `verify/lsp_client.py` green; `verify/nvim_live.lua`,
`verify/dismiss_test.lua`, `verify/nvim_ui_test.lua` and `verify/rules_live.lua` green on **both**
Neovim 0.12.5 and 0.12.1; `verify/omp_lsp.sh` green — a third client, which shares no code with
this repository, receives a rule's finding over standard surfaces; `verify/lsp_framing_test.py`
green, so an intermittent red run cannot be the harness's own framing; the live rule-on-save
behaviour (a rule's finding reaches the sign column in a real editor, and `:Jev inspect` returns
the same finding plus its counts); and a sweep of every document for the old name returning only
the dated history rows in `PROTOCOL.md` and `STATUS.md`.

## Deliberately deferred

- Session/time-travel UI beyond `:Jev undo`
- Multi-file refactors spanning more than one symbol
- Any second editor target (the protocol allows it; nothing else is tested)
- Model fine-tuning for the `review` tier — evaluate only once U2's findings have a
  measured false-positive rate
