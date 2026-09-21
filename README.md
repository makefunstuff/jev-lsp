# jev-lsp

> [!WARNING]
> **Proof of concept / WIP.** Interface, contract, and config keys may change. Nothing here should be relied on yet.

[Landing](https://makefunstuff.github.io/jev-lsp/) · [Tutorial](docs/TUTORIAL.md) · [Guide](docs/GUIDE.md)

**Jev is a classifier, not a chat model.** A rule asks one typed question about one line; the answer is a value and a probability — never prose. **jev-lsp** runs `.jev/rules/*.json` as ordinary LSP diagnostics, code actions, and code lens while you edit or a harness writes.

## Demo

Silent action loop across OpenCode, Neovim, and VS Code: paste an AI-slop comment, get a finding, apply the fix.

[![Demo: OpenCode → Neovim → VS Code](https://makefunstuff.github.io/jev-lsp/assets/demo-slop-triptych-poster.jpg)](https://makefunstuff.github.io/jev-lsp/assets/demo-slop-triptych.mp4)

[Watch (~32s)](https://makefunstuff.github.io/jev-lsp/assets/demo-slop-triptych.mp4) · [landing page](https://makefunstuff.github.io/jev-lsp/#demo)

## Loop

1. **Spec** — what the code must do.
2. **Rules** — `.jev/rules/*.json` (words + pattern for candidate lines), versioned like code.
3. **Steer** — jev-lsp on save (human or harness).
4. **Stay on track** — findings are ordinary diagnostics.

Semi-deterministic: pattern is exact; judgement must clear the rule’s floor before publish. Not a correctness oracle. A repository with no `.jev/rules/` of its own is inspected by the rule set shipped in the binary.

## Cost

Well-defined rules let cheap / local models work: conventions live in the rule, the model answers about one line. **No matching pattern → no model call.**

Measured 2026-09-20 on this repo (`jev inspect --force`, decide `jev-1.13` vs `jev review` / `gemini-2.5-flash-lite`):

| | rules pass | chat review (same file) |
|---|---|---|
| `server.rs` ~2.6k lines | 5,341 in / 345 out · 2 findings · **$0.00022** | 30,562 in / 9 out · 0 findings · **$0.00306** |
| 30 `crates/**/*.rs` | 82,478 in / 3,831 out · 20 calls · **$0.0034** | — |

10 of 30 docs had no candidate (cost nothing). Prices from the decide route’s `usage.cost` ($0.0395/MTok) and chat `cost_details` ($0.10 in / $0.40 out per MTok). Does not include generation cost; chat tiers stay on-demand. **No local decide tier run end-to-end here** — method in [`docs/MODEL.md`](docs/MODEL.md).

## Install

Requires Rust ≥ 1.75. Puts `jev-lsp` and `jev` on `PATH` (`~/.cargo/bin`):

```sh
cargo install --git https://github.com/makefunstuff/jev-lsp --locked jev-lsp jev
```

### Neovim (primary)

```sh
ln -s /path/to/jev-lsp/nvim ~/.local/share/nvim/site/pack/jev/start/jev
```

```lua
require('jev').setup({})
```

Then: export a decide key, open a file, save.

```sh
export TYPESAFE_API_KEY=…   # hosted Jev default (api.typesafe.ai)
```

A repository with no `.jev/rules/` is inspected all the same: the rule set the binary ships runs on it, those findings carry `rule_source: "builtin"`, and `:Jev inspect` prints ` [builtin]` or ` [repository]` after each label. `jev rules init` writes the shipped set into `.jev/rules/` as `prose-…json` / `code-…json` to read and edit; a file you edit **shadows** the shipped rule of the same `id`, and `rules.defaults: false` turns the shipped set off.

- First rule of your own: [`docs/TUTORIAL.md`](docs/TUTORIAL.md) §3 · schema: [`docs/GUIDE.md`](docs/GUIDE.md) §4
- Without the plugin (Neovim ≥ 0.12):

```lua
vim.lsp.config('jev', { cmd = { 'jev-lsp' }, filetypes = { 'rust' }, root_markers = { '.git' } })
vim.lsp.enable('jev')
```

Lazy.nvim: `{ dir = '/path/to/jev-lsp/nvim', name = 'jev', lazy = false, config = function() require('jev').setup {} end }`.

### Keymaps (plugin)

| Key | Action |
|---|---|
| `<leader>ja` | code-action picker (visual = selection scope) |
| `<leader>ju` | undo last applied edit |
| `<leader>jq` | ask about the file |
| `<leader>js` | status: queue, budgets, endpoints |

`:Jev inspect` — what the rules pass did (counts, findings, skips). `:Jev inspect --force` re-runs unchanged files. Tab-completes. Plugin needs Neovim ≥ 0.12.

## Other clients

`jev-lsp --stdio` is a standard language server — any client that can start one works, provided it re-pulls diagnostics on `workspace/diagnostic/refresh` (PROTOCOL §3.4/§9). Plugin is Neovim-only extras (attach, keymaps, `:Jev`).

Export endpoints before launch (`JEV_DECIDE_*`, `JEV_BASE_URL`, …) or set them in the client config. Other routes / local decide: [`docs/MODEL.md`](docs/MODEL.md) §8.

Chat tiers (actions / plans / explanations) — only when needed:

```sh
export JEV_BASE_URL=http://127.0.0.1:8080/v1
export JEV_MODEL=your-model-name
export JEV_API_KEY_ENV=OPENROUTER_API_KEY   # name of the env var, not the key
```

### OMP

```json
{"servers":{"jev-lsp":{"command":"/path/to/jev-lsp/target/release/jev-lsp","args":["--stdio"],
  "fileTypes":[".rs",".py",".md",".toml",".json",".lua",".sh"],"rootMarkers":[".git"],
  "settings":{"jev":{"models":{"decide":{"wire":"system_one","base_url":"https://opencode.ai/zen/v1",
    "model":"jev-1.13","api_key_env":"TYPESAFE_API_KEY","timeout_ms":15000}}}}}}}
```

`api_key_env` names the variable — export the value yourself. Env wins over `settings`. Helper: `verify/omp_lsp.sh`.

### OpenCode

OpenCode 1.18 does not finish the pull path: it answers `workspace/diagnostic/refresh` with an
empty OK, pulls `textDocument/diagnostic` once per open (before the pass has run), and shows its
agent only `severity: 1`. Configure the bridge in [`editors/opencode/`](editors/opencode/README.md)
rather than `jev-lsp --stdio` directly:

```json
{"lsp":{"jev":{"command":["python3","/path/to/jev-lsp/editors/opencode/jev-lsp-opencode-bridge.py"],
  "extensions":[".rs"],"env":{"JEV_LSP_BIN":"/path/to/jev-lsp/target/release/jev-lsp",
  "JEV_DECIDE_BASE_URL":"http://127.0.0.1:8009/v1","JEV_DECIDE_MODEL":"kev-latest"}}}}
```

[`editors/opencode/README.md`](editors/opencode/README.md) documents the four things the bridge
translates, the limits, and `bash editors/opencode/verify-bridge.sh`, which reproduces the empty
native path and proves the bridge surfaces a finding to a client shaped like OpenCode 1.18.

### Cursor / VS Code

Needs the extension in `editors/cursor/` (no settings-only LSP route). See [`docs/CURSOR.md`](docs/CURSOR.md).

```sh
ln -s "$PWD/editors/cursor" ~/.cursor/extensions/makefunstuff.jev-0.1.0   # reload window
# or: bash editors/cursor/pack.sh /tmp/jev-0.1.0.vsix && cursor --install-extension /tmp/jev-0.1.0.vsix
```

```jsonc
{
  "jev.server.path": "/path/to/jev-lsp/target/release/jev-lsp",
  "jev.decide.baseUrl": "https://api.typesafe.ai/v1",
  "jev.decide.model": "jev-latest",
  "jev.decide.wire": "system_one",
  "jev.decide.apiKeyEnv": "TYPESAFE_API_KEY",
  "jev.decide.apiKeyFile": "~/.bash_profile"
}
```

## This repo

Conventions live in `.jev/rules/*.json`. Saving a file runs the pass. With hosted Jev, export `TYPESAFE_API_KEY`. At shipped floors this repo publishes **zero** findings (at the old 0.75 unwrap floor: two lines in one file); 12 of 30 Rust files had no candidate.

## Docs

| Doc | What |
|---|---|
| [`docs/TUTORIAL.md`](docs/TUTORIAL.md) | First run end-to-end — **start here** |
| [`docs/GUIDE.md`](docs/GUIDE.md) | Surfaces, settings, schema, CLI, troubleshooting |
| [`docs/CURSOR.md`](docs/CURSOR.md) | Cursor extension + limits |
| [`docs/MODEL.md`](docs/MODEL.md) | Tiers, routing, cost method, local routes |
| [`docs/UX.md`](docs/UX.md) | Surfaces & product decisions |
| [`docs/LANGUAGE.md`](docs/LANGUAGE.md) | Attachment, language resolution, gates |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | Components, store, scheduler |
| [`PROTOCOL.md`](PROTOCOL.md) | **Frozen** LSP surface, commands, schemas, exit codes |
| [`docs/VERIFICATION.md`](docs/VERIFICATION.md) | How claims are proven |
| [`STATUS.md`](STATUS.md) | State & dated log |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | Work units |
| [`docs/STYLE.md`](docs/STYLE.md) | Writing register |

Verify: `bash verify/run-suite.sh /tmp/suite.log`

## Licence

MIT — see [`LICENSE`](LICENSE).
