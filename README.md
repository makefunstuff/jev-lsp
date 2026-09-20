# jev-lsp

> [!WARNING]
> **Heavily work in progress.** The interface, the contract and the
> configuration keys may change without notice; nothing here should be relied on yet.
> this is PoC and heavily under slop-generating phases to proof that lsp approach actually work first

![OMP writing a Rust handler, with jev-lsp's rule finding on the generated line steering the fix](docs/assets/jev-steering.svg)
<sub><b>OMP</b>, a third-party LSP client: the rule's finding on the generated line steers the fix.</sub>

![Neovim: jev's annotations beside the code, next to clangd's own diagnostics](docs/assets/jev-neovim.webp)
<sub><b>Neovim</b>, the primary client: jev's annotations beside the code, next to clangd's own diagnostics.</sub>

An LSP server that runs a model over the files you have open and reports through your editor's
own surfaces: diagnostics, code actions, code lens. The ambient pass runs the rules your
repository states in `.jev/rules/*.json`; each line a rule points at is sent to **Jev**, a
decision model that returns one answer per question with a probability, not prose. Standard LSP
surfaces only. Neovim is the primary client; `nvim/` is the plugin.

## Use it

```sh
cargo build --release   # target/release/jev-lsp (the server) and target/release/jev (a CLI)

# 1. the plugin: the attach pass that covers files Neovim cannot identify, the keymaps and :Jev
ln -s /path/to/jev-lsp/nvim ~/.local/share/nvim/site/pack/jev/start/jev
```

```lua
-- 2. in your config
require('jev').setup({ cmd = { '/path/to/jev-lsp/target/release/jev-lsp' } })
```

A lazy-managed config takes the same plugin as a local `dir` spec instead of the symlink:
`{ dir = '/path/to/jev-lsp/nvim', name = 'jev', lazy = false, config = function() require('jev').setup { cmd = { … } } end }`.

3. Write a rule in `.jev/rules/*.json`, open a file and save. A finding appears on the line when the
rule's inspection and the decision tier both accept it; with no rule files the pass publishes nothing
and reports `no_rules`. `docs/TUTORIAL.md` §3.7 has the rule shape.

### Endpoints

Nothing appears until a rule exists **and** the decision tier answers, so that tier is what a first
run needs. It is hosted Jev by default (`api.typesafe.ai`, key from `TYPESAFE_API_KEY`); this
machine runs it through **OpenCode Zen**, which carries the same model:

```sh
export TYPESAFE_API_KEY=…                      # the variable `api_key_env` names (see below)
export JEV_DECIDE_WIRE=system_one              # required: Zen 404s on the open_router path
export JEV_DECIDE_BASE_URL=https://opencode.ai/zen/v1
export JEV_DECIDE_MODEL=jev-1.13               # NOT jev-1.13-free: it rate-limits (429) in bursts
export JEV_DECIDE_TIMEOUT_MS=15000             # any hosted decide endpoint needs more than 5000 ms

# the OpenRouter route, kept as the alternative (its price is visible from the API):
#   JEV_DECIDE_WIRE=open_router  JEV_DECIDE_BASE_URL=https://openrouter.ai/api
#   JEV_DECIDE_MODEL=typesafe/jev-1.13

# or a local System One server, no key:
export JEV_DECIDE_BASE_URL=http://127.0.0.1:8009/v1
export JEV_DECIDE_MODEL=kev-latest

# the chat tiers answer actions, plans and explanations — needed only for those:
export JEV_BASE_URL=http://127.0.0.1:8080/v1   # chat model, OpenAI-compatible
export JEV_MODEL=your-model-name
export JEV_API_KEY_ENV=OPENROUTER_API_KEY      # a hosted chat tier: the NAME of the key variable
```

`opencode-go` (`…/zen/go/v1`) is the subscription gateway and carries **no Jev** — its decision
route answers `Model is unavailable`. The key's *name* comes from `api_key_env` (default
`TYPESAFE_API_KEY`), and the environment can name another one: `JEV_DECIDE_API_KEY_ENV` for the
decide tier, `JEV_API_KEY_ENV` for the chat tiers. Both take the **name** of the variable holding
the key — never the key itself — and an empty value is ignored, so a shell pointing the decide tier
at another provider names its own key variable instead of borrowing a name that belongs to
somebody else's service. `docs/TUTORIAL.md` §4 lists every setting.

## Use it with another LSP client

`jev-lsp --stdio` is a language server, so any client that can start one can use it. The plugin
is optional: without it you still get findings (pull diagnostics plus
`workspace/diagnostic/refresh`), edits (`codeAction`, `codeAction/resolve`) and every command
(`workspace/executeCommand`, including `jev.inspect`). What the plugin adds is Neovim-specific
only: the attach pass, the keymaps, `:Jev`.

The server reads its endpoints from the environment it is started with, so export
`JEV_BASE_URL`, `JEV_MODEL`, `JEV_DECIDE_BASE_URL`, `JEV_DECIDE_MODEL` and `JEV_DECIDE_WIRE`
before launching the harness, or set them in the client's server block if it has one.

OMP, the entry `verify/omp_lsp.sh` exercises, written to `<project>/.omp/lsp.json` (or
`~/.omp/agent/lsp.json` to apply it to every project). OMP answers the server's
`workspace/configuration` pull with `settings.jev`, so the entry can carry the endpoints:

```json
{"servers":{"jev-lsp":{"command":"/path/to/jev-lsp/target/release/jev-lsp","args":["--stdio"],
  "fileTypes":[".rs",".py",".md",".toml",".json",".lua",".sh"],"rootMarkers":[".git"],
  "settings":{"jev":{"models":{"decide":{"wire":"system_one","base_url":"https://opencode.ai/zen/v1",
                                          "model":"jev-1.13","api_key_env":"TYPESAFE_API_KEY",
                                          "timeout_ms":15000}}}}}}}
```

`settings` can name the key's **variable** (`api_key_env`) but cannot carry the key itself, so
export it before the harness starts (`export TYPESAFE_API_KEY=…`); without it a hosted decision
call fails with `model_error`. Environment variables win over the `settings` block, which is what
lets the stub and local-server recipes work without editing the client config.

OpenCode, from its schema (`https://opencode.ai/config.json`, `lsp`), where `command` is an
array and `env` carries the endpoint variables:

```json
{"lsp":{"jev-lsp":{"command":["/path/to/jev-lsp/target/release/jev-lsp","--stdio"],"extensions":[".rs"],
                    "env":{"JEV_DECIDE_BASE_URL":"http://127.0.0.1:8009/v1","JEV_DECIDE_MODEL":"kev-latest"}}}}
```

Cursor has no settings-only route for a language server — `.cursor/mcp.json` configures MCP tools,
a different protocol — so it needs the extension in `editors/cursor/`:

```sh
ln -s "$PWD/editors/cursor" ~/.cursor/extensions/makefunstuff.jev-0.1.0   # from a clone; reload the window
bash editors/cursor/pack.sh /tmp/jev-0.1.0.vsix                           # or as a .vsix (verified here)
/Applications/Cursor.app/Contents/Resources/app/bin/cursor --install-extension /tmp/jev-0.1.0.vsix
```

The minimum for a finding is the decide endpoint and the key — the key's *value* comes from a file,
never a setting — and the whole recipe, including what Cursor's API does not do, is `docs/CURSOR.md`:

```jsonc
{
  "jev.server.path": "/path/to/jev-lsp/target/release/jev-lsp",
  "jev.decide.baseUrl": "https://api.typesafe.ai/v1",
  "jev.decide.model": "jev-latest",
  "jev.decide.wire": "system_one",            // system_one | open_router
  "jev.decide.apiKeyEnv": "TYPESAFE_API_KEY", // the NAME of the variable, never the key
  "jev.decide.apiKeyFile": "~/.bash_profile"  // where the key's VALUE is read from
}
```

![Cursor: jev's finding in the hover over the squiggled line, with jev: explain as an inlay hint](docs/assets/jev-cursor.webp)
<sub><b>Cursor / VS Code</b>, a client that is neither Neovim nor the harness: jev's finding in the hover, the squiggle on the line, and <code>jev: explain</code> as an inlay hint.</sub>

Other harnesses keep the same three things in their own file: a command, the extensions it
applies to, and a root marker.

## This repository uses it

The project checks itself with jev. `~/.omp/agent/lsp.json` registers
`target/release/jev-lsp` for OMP on this machine (absolute path, `--stdio`, `fileTypes`,
`rootMarkers`, hosted Jev in a `settings` block), so a rule finding reaches OMP's own `lsp` tool
with no project config, and the conventions this code is checked against live in
`.jev/rules/*.json` — saving a file runs the pass over it. With hosted Jev the key has to be
exported as `TYPESAFE_API_KEY`; measured over this repository's own code the rules published
**zero** findings, and 12 of its 30 Rust files had no candidate.

## What you get

| key | |
|---|---|
| `<leader>ja` | the code-action picker; in visual mode the selection is the scope |
| `<leader>ju` | undo the last applied edit |
| `<leader>jq` | ask a question about the file |
| `<leader>js` | status: queue, budgets, endpoints in force |

`:Jev inspect` reports what the rules pass did for the current file: the counts, the findings,
and every skip, so "no finding" can be told apart from "nothing ran". `:Jev inspect --force`
re-runs it for a file git reports as unchanged. Other subcommands are reachable by typing;
`:Jev <Tab>` completes them. The plugin requires Neovim ≥ 0.12.

## Read more

- `PROTOCOL.md` — the frozen contract: methods, commands, schemas, CLI, exit codes
- `docs/TUTORIAL.md` — install, the keys, workflows, writing a rule
- `docs/UX.md` — the surfaces and the keymaps
- `docs/MODEL.md` — tiers, routing, the decision wire, and what leaves your machine
- `docs/VERIFICATION.md` — how each claim is proven, and what is unverified
- `STATUS.md` — project state and the verification table

The verification table is one command: `bash verify/run-suite.sh /tmp/suite.log`.
