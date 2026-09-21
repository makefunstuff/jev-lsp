# Re-testing the ambient push against omp

Notes for whoever re-runs the one-shot-client path after the 2026-09-21 ambient fix. The fix is
server-side (`crates/jev-lsp/src/server.rs`); nothing here needs `--opencode`, a bridge, or a
severity remap.

## What changed

Before: a background pass ended by asking the client to re-pull
(`workspace/diagnostic/refresh`). A client that one-shots `textDocument/diagnostic` once, early
— omp at ~500 ms — read the empty answer as "clean" and was never brought back, so a late
finding was invisible forever, and a slow decision was a silent empty gutter meanwhile.

After: the finished pass **pushes** what it concluded with `textDocument/publishDiagnostics`,
and still sends the refresh. If the pass is not sub-second it first pushes a document-level
information diagnostic, `jev: checking…` (code `jev.checking`, `data.pending = true`), which the
findings replace — or which is cleared on a no-hit or at the budget.

Clock, both config keys under `ambient` (`PROTOCOL.md` §10):

| Key | Default | Meaning |
|---|---|---|
| `ambient.pending_ms` | 500 | Show the `jev.checking…` cue if the pass is still running. |
| `ambient.budget_ms` | 8000 | Clear the cue and stop awaiting the pass. Above `models.decide.timeout_ms` (5000), so a healthy decide always lands. |

## Where to look

- `crates/jev-lsp/src/server.rs` — `supervise_ambient` (the cue/budget loop), `ClientSurfaces`
  and `AmbientSurfaces` (the push surface), `ambient_items` (findings → diagnostics),
  `spawn_pass` and `spawn_idle_ambient` (the two callers).
- `crates/jev-core/src/config.rs` — `Ambient.pending_ms` / `budget_ms`.
- `PROTOCOL.md` §3.4 (the method table), §9 (diagnostics), §10 (the keys), and the changelog.

## How to verify

Set the decide tier slow so the cue is visible, then drive the two paths:

1. **Late publish, no pull.** Point `JEV_DECIDE_BASE_URL` at `verify/stub_model.py` with
   `STUB_DELAY_MS=3000` (or any slow endpoint), open a document with a rule hit, and `didSave`.
   Do **not** send `textDocument/diagnostic`. Expect, in order:
   - a `textDocument/publishDiagnostics` with one `jev.checking` diagnostic after ~500 ms;
   - another `textDocument/publishDiagnostics` with the findings (source `jev`, `data.source =
     "rules"`) when the decide returns.
2. **One-shot pull that arrives early.** Send one `textDocument/diagnostic` immediately after the
   save, read the (empty) `items`, then keep reading notifications: the findings still arrive by
   push. This is the omp shape.
3. **No-hit clears the cue.** Repeat with a document no rule claims: the cue is replaced by an
   empty `textDocument/publishDiagnostics` (the sign column goes clean, not stuck on "checking…").
4. **Budget.** Point the decide tier at a stalled endpoint (`verify/supersede_probe.py`'s
   `_start_stalled_stub`, or `STUB_DELAY_MS` above `ambient.budget_ms`): the cue is cleared at the
   budget and `window/logMessage` carries `exceeded its 8000 ms budget`.

Automated coverage, on the real binary and its stub:

- `verify/rules_test.py` step 9 — "the ambient pass pushes its findings with publishDiagnostics,
  no pull needed".
- `crates/jev-lsp/src/server.rs` `ambient_tests` — sub-second (no cue), slow (cue then result),
  budget (cue then clear, logged).
