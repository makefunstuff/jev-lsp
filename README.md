# meta-lsp

An LSP server that makes Neovim an agent harness: the model observes your code in the
background and proposes work through Neovim's **native** surfaces — diagnostics, code
actions, code lens, inlay hints, ghost text — instead of a chat pane you have to talk to.

The model is not a destination you visit. It is a process attached to the buffer.

**It serves every file.** The model needs no grammar and no compiler to read text, so
support is not gated on language, filetype, or a parser — an unidentified file, a
`Makefile`, a log, and a config are all first-class. See `docs/LANGUAGE.md`.

## Thesis

Prompting in a TUI makes you the transport: you copy context in, read prose out, and
apply it by hand. That is four lossy steps and a context switch. An editor-native agent
removes all four:

| TUI prompt | meta-lsp |
|---|---|
| You select and paste context | Server observes documents, versions, and the repo — context is built from what is already open |
| Output is prose you read | Output is a `WorkspaceEdit` the client validates and applies, or a diagnostic on the exact line |
| You decide when to ask | Findings arrive as sign-column diagnostics while you work; the menu is ready when you open it |
| You apply and hope | Edits are version-stamped; the client refuses stale ones; verification runs after apply |
| Nothing is running between prompts | A background worker keeps conclusions warm so the menu is instant |

## Documents

| Path | Lifecycle | Contents |
|---|---|---|
| `PROTOCOL.md` | **frozen** | LSP method surface, edit contract, code action taxonomy, CLI contract, exit codes |
| `docs/ARCHITECTURE.md` | living | Components, process topology, document store, scheduler |
| `docs/LANGUAGE.md` | living | Unconditional support, attachment ladder, language resolution, scope strategies |
| `docs/UX.md` | living | The experience: scenarios, keymaps, plan buffer, approval, noise policy |
| `docs/MODEL.md` | living | Model tiers, routing, context builder, output contracts, budgets |
| `docs/VERIFICATION.md` | living | How each claim gets proven; independent client, live Nvim, defect injection |
| `docs/ROADMAP.md` | living | Units with acceptance criteria |
| `docs/research/nvim-lsp-surface.md` | **evidence** | Raw probe output and `file:line` citations backing every protocol claim |
| `docs/research/prior-art.md` | **evidence** | The four comparable projects, what converges, what to steal, what to avoid |
| `STATUS.md` | living | Project log; decisions and open questions at the top |

## Layout

```
crates/
  meta-core/    language, scope, context, contracts, edits, findings, gates, model
                client, cache, budget, config — no async, no LSP
  meta-lsp/     tower-lsp stdio server: capabilities, sync, code actions, diagnostics,
                commands, and the analysis scheduler
  meta/         one-shot CLI, synchronous, stdin -> stdout, exit codes from PROTOCOL §11
nvim/lua/meta/  the plugin: universal attach pass, language hook, keymaps, :Meta, health
verify/         the harness: independent spec-derived client, scripted model, smoke and
                race tests, live Neovim test, and the protocol probes
```

All three front ends are thin shells over `meta-core`, which is what `verify/cli_parity.py`
checks: the same request through the CLI and through the editor must produce identical
results, not merely similar ones.

## Enable it

Requires Neovim ≥ 0.12 (the ambient surface — pull diagnostics and `$/progress` — is
verified against 0.12.5).

```sh
cargo build --release

# try it by hand: see test/visual/README.md for what to press and what to expect
nvim -u test/visual/init.lua

# 1. the plugin: it owns the universal attach pass, the language hook and :Meta
ln -s /path/to/meta-lsp/nvim ~/.local/share/nvim/site/pack/meta/start/meta

# 2. point the server at a model (or use settings.meta.models.* instead)
export META_BASE_URL=http://127.0.0.1:8080/v1
export META_MODEL=qwen2.5-coder-7b-instruct
```

```lua
-- 3. in your config
require('meta').setup({ cmd = { '/path/to/meta-lsp/target/release/meta-lsp' } })
```

`setup()` calls `vim.lsp.config('meta', …)` and `vim.lsp.enable('meta')` for you, and starts
the attach pass. Deliberately **no `filetypes`**: every file buffer is served, including
files Neovim cannot identify (`docs/LANGUAGE.md`). Nothing is gated on language.

This is not aspirational — `verify/nvim_live.lua` bootstraps through exactly these two lines
(`runtimepath` + `require('meta').setup({ cmd = … })`) and is green against the real server.

```vim
:Meta status|explain|recompute|cancel|stop|start|undo|dismiss|log
<leader>ma  code actions      <leader>me  explain      <leader>ms  status
<leader>mt  add tests         <leader>mS  stop (kill switch, no prompt)
```

Served today: `status`, `explain`, `recompute`, `cancel`, plus the plugin-local `undo`,
`dismiss`, `stop`, `start`, `log`. `:Meta plan` and `:Meta review` answer with a structured
`not_implemented` and say so — they are designed (`docs/ROADMAP.md` U5) but not built, and
neither is advertised in the server's capabilities.

Model endpoints come from config, never from a file of ours: point `settings.meta.models.*`
at a local llama.cpp OpenAI-compatible server, or set `api_key_env` for a remote tier.

## Status

**Working.** The server builds, runs, and is verified end to end against real Neovim.

| | |
|---|---|
| `cargo test` | 200 passing, warning-clean |
| `cargo build --release` | no warnings |
| `python3 verify/smoke.py` | 35/35 against the real binary |
| `python3 verify/plan_test.py` | 35/35 — plan, apply, revert, staleness, divergence, multi-file |
| `python3 verify/cli_parity.py` | 12/12 — the CLI and the LSP agree exactly |
| `python3 verify/lsp_client.py --server …` | 28 ok, 0 FAIL (independent, spec-derived client) |
| `nvim --headless -l verify/nvim_live.lua` | 0 failures, 0 skips (real plugin, real server) |
| `nvim --headless -l verify/nvim_ui_test.lua` | 0 failures, 0 skips (picker, diff preview, lenses, hints, streaming, plan, session) |
| `python3 verify/inline_test.py` | 14/14 — the capability, the handler, and every gate |
| `python3 verify/quality_eval.py --base-url … --model …` | recall, precision and noise on a labelled defect set — the only harness that answers "is the review right", and it needs a real model |
| `python3 verify/latency.py` | 8/8 — every editor-driven path under 1 ms against a model made **2 s** slow, which is how the bench tells "fast" from "cached" |
| `python3 verify/queue_test.py` | 5/5 — the mid-flight-edit race, with a stalled model |
| `python3 verify/config_race_test.py` | 3/3 — a save during startup is not analysed against the defaults |
| `python3 verify/supersede_probe.py` | 7 ok, 0 FAIL — supersession, independently probed, with a control |
| `nvim --headless -l verify/inline_live.lua` | 0 failures (ghost text itself needs an interactive session) |
| `nvim --headless -l verify/dismiss_test.lua` | 9/9 — dismissal is recorded per repository and does not resurface |
| `python3 verify/real_model.py` | 6/6 against **DeepSeek** — 1 finding, an edit, and a file that still parses, every run |
| `python3 verify/soak.py` | **8/9** on DeepSeek, **6/6** on the local `llama.cpp` model; 0 files left unparseable either way |

The last row is the one that matters: everything else uses a scripted endpoint. Real-model
latency measured 2.2–3.1 s for the ambient pass and 1.1–5.1 s for `codeAction/resolve`, with
the menu itself still an instant cache read.

Running against a real model found three defects the scripted endpoint could not —
reasoning-budget exhaustion, a placeholder schema being echoed back, and an answer that
covered more lines than its anchor (which duplicated code on apply). All three are fixed and
pinned by tests; `docs/VERIFICATION.md` §8 records them.

Served today: universal attachment, document sync, ambient findings via pull diagnostics
with `workspace/diagnostic/refresh`, a code-action menu with lazy `codeAction/resolve`,
version-stamped `WorkspaceEdit`s, plans with per-step approval and revert, multi-file edits,
inline completion (off by default), `$/progress` streaming, and the `meta.status`,
`meta.recompute`, `meta.explain`, `meta.plan`, `meta.apply`, `meta.revert`, `meta.cancel`
commands. Capabilities and commands not yet
implemented are **not advertised** (PROTOCOL §2).

Measured on a real model: ambient 1.5–7 s, resolve 1.5–31 s, and the menu itself instant.
One recurring failure remains and is by design: when the model answers with a replacement that
spans more than the anchor it named, the edit is *refused* rather than applied, because
applying it would duplicate the lines it did not consume.

Not built: code lens, inlay hints, and treesitter-backed scope. `docs/ROADMAP.md` tracks
each, and `docs/VERIFICATION.md` §9 records what is served but not yet proven here.

```sh
cargo build --release

# try it by hand: see test/visual/README.md for what to press and what to expect
nvim -u test/visual/init.lua

# no GPU, no network: a scripted endpoint stands in for the model
python3 verify/smoke.py                       # 32 end-to-end checks, self-hosting its stub
python3 verify/queue_test.py                  # the mid-flight-edit race, stalled stub
# real model (via the omp auth gateway, which resolves the credential server-side)
python3 verify/real_model.py --base-url http://127.0.0.1:4000/v1 --model deepseek/deepseek-flash
python3 verify/soak.py --base-url http://127.0.0.1:4000/v1 --model deepseek/deepseek-flash --rounds 3
python3 verify/supersede_probe.py             # same contract, independent probe + control
python3 verify/plan_test.py                   # plan -> server-side apply -> revert
python3 verify/inline_test.py                 # inline completion and every gate before it
python3 verify/cli_parity.py                  # the CLI and the LSP must agree exactly

# the independent, spec-derived client (needs the stub to be listenable)
python3 verify/stub_model.py &                # binds 127.0.0.1:8099
python3 verify/lsp_client.py --server "$PWD/target/release/meta-lsp" \
    --workspace /tmp/meta-ws --stub-model-url http://127.0.0.1:8099/v1

# the one-shot CLI (PROTOCOL §11): stdout is one JSON line, the exit code is the verdict
./target/release/meta status | jq .
./target/release/meta review path/to/file.py
./target/release/meta action --verb harden path/to/file.py:12 | jq .edit

# live Neovim: the real plugin, the real server, a real buffer
META_LSP_BIN="$PWD/target/release/meta-lsp" \
META_BASE_URL=http://127.0.0.1:8099/v1 META_MODEL=stub-model \
  nvim --headless -u NONE -l verify/nvim_live.lua

# against a real model instead of the stub
META_BASE_URL=http://127.0.0.1:8080/v1 META_MODEL=<model> python3 verify/smoke.py
```

Every protocol constraint is probe-backed by `docs/research/nvim-lsp-surface.md`, and the
probes are re-runnable: `verify/probes/run.sh`.

Known limitations are recorded rather than hidden: see `docs/VERIFICATION.md` §10, and the real-model results in §7.
