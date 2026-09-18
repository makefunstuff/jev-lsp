# Roadmap

Each unit is independently verifiable and ends with its own acceptance check. No unit
starts before the previous one passes. Order is chosen so the two hard problems —
staleness and latency — are exercised by unit 1, not discovered in unit 6.

**Progress**: every unit built. U0–U8 done; U9 partly (cancellation, budgets and
`:checkhealth` exist, the daemon does not). Two scoped gaps remain and are noted in their
units: U2's treesitter scope is still structural-only — grammars are a dependency decision,
not an oversight, and `scope_source` reports the fallback honestly — and its dismissal file
*is* implemented, in the plugin (`nvim/lua/meta/init.lua`: `:Meta dismiss` writes
`<repo>/.git/meta/dismissed.json` and `filter_findings` drops dismissed ids from the pull).
Inline completion's ghost text is unverified in a headless harness
(`docs/VERIFICATION.md` §9).

## U0 — Scaffold ✅

Workspace, three crates, `nvim/` skeleton, `PROTOCOL.md` types in `meta-core` as Rust
structs with serde, plus the schema validator.

**Accept**: `cargo build --release` warning-clean under
`-D warnings`; `meta-core` unit tests green; `nvim/init.lua` starts and stops the server;
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

`meta.plan`, the plan contract, the plan buffer with per-step state, diff view, apply,
skip, recompute, `:Meta undo`.

**Accept**: live Neovim applies a 3-step plan one step at a time, skips one, reverts one,
and `:Meta undo` restores the buffer byte-for-byte; a step whose target moved is marked
stale and is not applied.

## U6 — Test generation and multi-file edits

`test` verb with `resourceOperations: create`, file creation, post-apply verification pass
via the `review` tier, divergence diagnostics.

**Accept**: golden test creates a test file and edits the source in one `WorkspaceEdit`;
injected post-apply divergence produces an `ERROR` diagnostic naming the mismatch.

## U7 — Inline completion

`inlineCompletion` handler, FIM tier, the server-side gate stack (idle floor, prefix
floor, dedupe, per-buffer cap), ghost-text acceptance.

**Accept**: p50 within budget against the stub; the defect-injection "unbounded FIM calls"
test passes; with `enabled = false` the server issues zero FIM calls.

## U8 — CLI parity

`meta explain|review|action|plan|status` with the exit-code contract.

**Accept**: CLI artifacts are byte-identical to the LSP server's results for the same
fixtures; all four exit codes exercised; `cargo build --release` still warning-clean.

## U9 — Hardening

`$/cancelRequest` end-to-end, backpressure caps, daemon (optional), `:checkhealth`,
`docs/` reconciled with the code.

**Accept**: cancel test green; the design documents contain no claim the code contradicts —
checked by reading each document against the implementation, not by a test.

## Deliberately deferred

- Session/time-travel UI beyond `:Meta undo`
- Multi-file refactors spanning more than one symbol
- Any second editor target (the protocol allows it; nothing else is tested)
- Model fine-tuning for the `review` tier — evaluate only once U2's findings have a
  measured false-positive rate
