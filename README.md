# jev-lsp

> [!WARNING]
> **Heavily work in progress.** The interface, the contract and the
> configuration keys may change without notice; nothing here should be relied on yet.
> this is PoC and heavily under slop-generating phases to proof that lsp approach actually work first

An LSP server that turns your editor into an agent harness: the model observes your code in the
background and proposes work through **standard LSP surfaces** — diagnostics, code actions, code
lens, inlay hints — instead of a chat pane you have to talk to.

It assumes nothing about the client. Findings arrive by pull diagnostics plus
`workspace/diagnostic/refresh`, actions by `codeAction` and `codeAction/resolve`, the material a
request carries by `workspace/executeCommand` and `workspace/configuration`, progress by
`$/progress` under a token the client itself issued, and free text by the client, because the
protocol cannot ask for it. Everything that is *not* a standard surface — the universal attach
pass, the picker, the lens keymaps, the scratch buffers — lives in the optional `nvim/` plugin
(`docs/LANGUAGE.md` §1).

**Neovim is the first-class client**, and the one this server is verified against most deeply:
the plugin adds the attach pass that makes "every file" true, `:Jev`, the picker, the statusline
segment and `:checkhealth jev`. It is not the only client the server is exercised by — a
spec-derived stdlib client (`verify/lsp_client.py`) and OMP through its own LSP support
(`verify/omp_lsp.sh`) drive the same surfaces — which is what makes the editor-agnostic claim
checkable rather than aspirational.

The model is not a destination you visit. It is a process attached to the buffer.

**It serves every file.** The model needs no grammar and no compiler to read text, so
support is not gated on language, filetype, or a parser — an unidentified file, a
`Makefile`, a log, and a config are all first-class. See `docs/LANGUAGE.md`.

## Thesis

Prompting in a TUI makes you the transport: you copy context in, read prose out, and
apply it by hand. That is four lossy steps and a context switch. An editor-native agent
removes all four:

| TUI prompt | jev-lsp |
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
| `docs/TUTORIAL.md` | living | **Start here** — install it, the four keys, the workflows (including the `clank` → spike → jev-lsp loop for code that does not exist yet, and writing your first rule), the settings that matter, troubleshooting |
| `docs/MODEL.md` | living | Model tiers, routing, context builder, output contracts, budgets |
| `docs/VERIFICATION.md` | living | How each claim gets proven; independent client, live Nvim, defect injection |
| `docs/ROADMAP.md` | living | Units with acceptance criteria |
| `docs/research/nvim-lsp-surface.md` | **evidence** | Raw probe output and `file:line` citations backing every protocol claim |
| `docs/research/prior-art.md` | **evidence** | The four comparable projects, what converges, what to steal, what to avoid |
| `STATUS.md` | living | Project log; decisions and open questions at the top |

## Layout

```
crates/
  jev-core/    language, scope, changed-set, context, contracts, edits, findings, gates,
                rules + inspections + the decision wire, model client, cache, budget,
                config — no async, no LSP
  jev-lsp/     tower-lsp stdio server: capabilities, sync, code actions, diagnostics,
                commands, and the two analysis passes (rules and review)
  jev/         one-shot CLI, synchronous, stdin -> stdout, exit codes from PROTOCOL §11
nvim/lua/jev/  the plugin: universal attach pass, language hook, keymaps, :Jev, health
verify/         the harness: independent spec-derived client, scripted model, smoke and
                race tests, live Neovim test, the rules harnesses, and the protocol probes
```

All three front ends are thin shells over `jev-core`, which is what `verify/cli_parity.py`
checks: the same request through the CLI and through the editor must produce identical
results, not merely similar ones.

## Enable it

**The plugin** requires Neovim ≥ 0.12 (the ambient surface — pull diagnostics and `$/progress` —
is verified against 0.12.5). The server itself is a plain stdio language server: any client that
speaks LSP can start it without the plugin, and one already does (`verify/omp_lsp.sh`).

```sh
cargo build --release

# try it by hand: see test/visual/README.md for what to press and what to expect
nvim -u test/visual/init.lua

# 1. the plugin: it owns the universal attach pass, the language hook and :Jev
ln -s /path/to/jev-lsp/nvim ~/.local/share/nvim/site/pack/jev/start/jev

# 2. point the server at a chat model (or use settings.jev.models.* instead)
export JEV_BASE_URL=http://127.0.0.1:8080/v1
export JEV_MODEL=your-model-name        # the built-in name is a placeholder, not a suggestion

# 3. the ambient pass asks the *decision* tier, which is a different endpoint speaking a
#    different wire. Hosted Jev by default (api.typesafe.ai, TYPESAFE_API_KEY); a local
#    System One server instead:
export JEV_DECIDE_BASE_URL=http://127.0.0.1:8009/v1
export JEV_DECIDE_MODEL=kev-latest
```

```lua
-- 4. in your config
require('jev').setup({ cmd = { '/path/to/jev-lsp/target/release/jev-lsp' } })
```

`setup()` calls `vim.lsp.config('jev', …)` and `vim.lsp.enable('jev')` for you, and starts
the attach pass. Deliberately **no `filetypes`**: every file buffer is served, including
files Neovim cannot identify (`docs/LANGUAGE.md`). Nothing is gated on language.

This is not aspirational — `verify/nvim_live.lua` bootstraps through exactly these two lines
(`runtimepath` + `require('jev').setup({ cmd = … })`) and is green against the real server.

**The ambient path is your repository's rules.** With `rules.enabled` true — the default — the
pass that runs on save is the *rules* pass, not a chat review: plain-English conventions in
`.jev/rules/*.json` (`"schema": "jev.rules/1"`), each naming candidates with a regex and asking
the decision tier one question about each. `jev.inspect` (the LSP command) and `jev inspect` (the
CLI) run it on demand and report what it considered, what it found and everything it skipped. A
repository with no rules gets no ambient findings, and the pass says `no_rules` rather than
reporting a clean document; `docs/UX.md` §1.1 has the shape of a rule and
`docs/TUTORIAL.md` §3.7 walks through writing one.

```vim
:Jev status|explain|review|inspect|plan|session|usage|stop|start|undo|dismiss|log|recompute
<leader>ja  code actions      <leader>ju  undo the last applied edit
<leader>jq  ask               <leader>js  status (and the kill switch lives there)
```

Two products and four keys. Findings → actions, and ask, are what this server is for;
everything else is reachable by typing. The prefix is `<leader>j` because `<leader>m*` is
crowded on a working config; `:Jev inspect` is the one to reach for when a rule seems not to
fire. There is no ghost text: inline completion was removed on 2026-09-19 (STATUS.md), and
generated code is asked for through an action or `:Jev ask`.

Model endpoints come from config, never from a file of ours: point `settings.jev.models.*`
at a local llama.cpp OpenAI-compatible server, or set `api_key_env` for a remote tier.

## Status

**Working, as far as it goes.** The server builds, runs, and is verified end to end against real
Neovim — and against two clients that share no code with it: the spec-derived
`verify/lsp_client.py` and OMP through its own LSP support. The surface is still moving (see the
note at the top), so read the table as the last measurement rather than a promise.

| | |
|---|---|
| `bash verify/run-suite.sh /tmp/suite.log` | the whole table below, in one run: a supervised stub, then every row, and a verdict per row on stdout (`NVIM_ONLY=1`, `REFUSE_IF_BUSY=1` and `NVIM_BINS` documented in its header) |
| `cargo test` | 274 passing (49 `jev` + 179 `jev-core` + 46 `jev-lsp`), 0 failed, warning-clean |
| `cargo build --release` | no warnings |
| `python3 verify/smoke.py` | 44/44 against the real binary — three consecutive full-table runs since the two harness defects in `docs/VERIFICATION.md` §8 were fixed |
| `python3 verify/rules_test.py` | 45/45 — the rules pass end to end: inspections, the changed set, the cache, the skips |
| `python3 verify/lsp_framing_test.py` | 9/9 — the client's own stdio framing, including the split-header bug the suite found in itself |
| `python3 verify/plan_test.py` | 35/35 — plan, apply, revert, staleness, divergence, multi-file |
| `python3 verify/cli_parity.py` | 25/25 — the CLI and the LSP agree exactly, `jev inspect` included |
| `python3 verify/lsp_client.py --server … --stub-model-url …` | 32 ok, 0 FAIL (independent, spec-derived client) |
| `nvim --headless -l verify/nvim_live.lua` | 0 failures, 0 skips (real plugin, real server) |
| `nvim --headless -l verify/rules_live.lua` | 0 failures, 0 skips on Neovim **0.12.5 and 0.12.1** — a rule's finding on the sign column, `:Jev inspect` with its counts and skips |
| `nvim --headless -l verify/nvim_ui_test.lua` | 0 failures, 0 skips (picker, diff preview, lenses, hints, streaming, plan, session) |
| `bash verify/omp_lsp.sh` | 0 failures, 0 skips — **a third client**, OMP, receives a rule's finding over standard surfaces and calls `jev.inspect`; a no-rules negative control produces nothing |
| `python3 verify/quality_eval.py --base-url … --model …` | recall, precision and noise on a labelled defect set. **Cannot run here** — it needs a real model, and the runner reports the row as `?`; last measured 2026-09-18 against `deepseek/deepseek-v4-flash`: 4/4 recall, 4/4 precision, 0 findings on the two clean files |
| `python3 verify/latency.py` | 7/7 — every editor-driven path under 1 ms against a model made **2 s** slow, which is how the bench tells "fast" from "cached" |
| `python3 verify/queue_test.py` | 5/5 — the mid-flight-edit race, with a stalled model |
| `python3 verify/config_race_test.py` | 3/3 — a save during startup is not analysed against the defaults |
| `python3 verify/supersede_probe.py` | 7 ok, 0 FAIL — supersession, independently probed, with a control |
| `nvim --headless -l verify/dismiss_test.lua` | 0 failures, 0 skips — dismissal is recorded per repository and does not resurface |
| `python3 verify/real_model.py` | 6/6 against **DeepSeek** — 1 finding, an edit, and a file that still parses, every run |
| `python3 verify/soak.py` | **8/9** on DeepSeek, **6/6** on the local `llama.cpp` model; 0 files left unparseable either way |

The real-endpoint rows are the ones that matter — every other row uses a scripted endpoint, and
`quality_eval` is the only one the suite cannot even start without a live model. Real-model
latency measured 2.2–3.1 s for the ambient pass and 1.1–5.1 s for `codeAction/resolve`, with
the menu itself still an instant cache read.

Running against a real model found three defects the scripted endpoint could not —
reasoning-budget exhaustion, a placeholder schema being echoed back, and an answer that
covered more lines than its anchor (which duplicated code on apply). All three are fixed and
pinned by tests; `docs/VERIFICATION.md` §8 records them.

Served today, to any LSP client: document sync, ambient findings **from the repository's own
rules** (`.jev/rules/*.json`) via pull diagnostics with `workspace/diagnostic/refresh`, a
code-action menu with lazy `codeAction/resolve`, version-stamped `WorkspaceEdit`s, plans with
per-step approval and revert, multi-file edits, code lens, inlay hints, hover, `$/progress`
streaming, and fifteen `workspace/executeCommand` commands — `jev.status`, `recompute`,
`review`, `explain`, `ask`, `followup`, `document`, `session`, `usage`, `outcome`, `plan`,
`apply`, `revert`, `cancel`, `inspect` (PROTOCOL §6). Capabilities and commands not implemented
are **not advertised** (PROTOCOL §2), and the independent client asserts both directions.
Universal attachment is the one thing that is the *plugin's*: it is what makes "every file" true
in Neovim (`docs/LANGUAGE.md` §1).

Measured on a real model: ambient 1.5–7 s, resolve 1.5–31 s, and the menu itself instant.
One recurring failure remains and is by design: when the model answers with a replacement that
spans more than the anchor it named, the edit is *refused* rather than applied, because
applying it would duplicate the lines it did not consume.

Built and advertised: code lens (per-declaration affordances that refresh after an analysis)
and inlay hints (a finding-count badge, off by default because Neovim switches hints per
buffer). Not built: treesitter-backed scope — resolution is structural with a whole-file
fallback, and `scope_source` says which was used. `docs/ROADMAP.md` tracks the rest, and
`docs/VERIFICATION.md` §10 lists what is deliberately unverified.

```sh
cargo build --release

# everything at once: one supervised stub, then every row of the table above
bash verify/run-suite.sh /tmp/suite.log       # NVIM_ONLY=1 / REFUSE_IF_BUSY=1 / NVIM_BINS

# no GPU, no network: a scripted endpoint stands in for the model
python3 verify/smoke.py                       # 44 end-to-end checks, self-hosting its stub
python3 verify/queue_test.py                  # the mid-flight-edit race, stalled stub
python3 verify/rules_test.py                  # the rules pass, end to end
python3 verify/lsp_framing_test.py            # the client's own stdio framing
bash verify/omp_lsp.sh                        # a third client: OMP over its own LSP support
# real model (via the omp auth gateway, which resolves the credential server-side)
python3 verify/real_model.py --base-url http://127.0.0.1:4000/v1 --model deepseek/deepseek-flash
python3 verify/soak.py --base-url http://127.0.0.1:4000/v1 --model deepseek/deepseek-flash --rounds 3
python3 verify/supersede_probe.py             # same contract, independent probe + control
python3 verify/plan_test.py                   # plan -> server-side apply -> revert
python3 verify/cli_parity.py                  # the CLI and the LSP must agree exactly

# the independent, spec-derived client (needs the stub to be listenable)
python3 verify/stub_model.py &                # binds 127.0.0.1:8099
python3 verify/lsp_client.py --server "$PWD/target/release/jev-lsp" \
    --workspace /tmp/jev-ws --stub-model-url http://127.0.0.1:8099/v1

# the one-shot CLI (PROTOCOL §11): stdout is one JSON line, the exit code is the verdict
./target/release/jev status | jq .
./target/release/jev review path/to/file.py
./target/release/jev action --verb harden path/to/file.py:12 | jq .edit

# live Neovim: the real plugin, the real server, a real buffer
JEV_LSP_BIN="$PWD/target/release/jev-lsp" \
JEV_BASE_URL=http://127.0.0.1:8099/v1 JEV_MODEL=stub-model \
  nvim --headless -u NONE -l verify/nvim_live.lua

# against a real model instead of the stub
JEV_BASE_URL=http://127.0.0.1:8080/v1 JEV_MODEL=<model> python3 verify/smoke.py
```

Every protocol constraint is probe-backed by `docs/research/nvim-lsp-surface.md`, and the
probes are re-runnable: `verify/probes/run.sh`.

Known limitations are recorded rather than hidden: see `docs/VERIFICATION.md` §10, and the real-model results in §7.
