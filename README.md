# jev-lsp

> [!WARNING]
> **Heavily work in progress.** The interface, the contract and the
> configuration keys may change without notice; nothing here should be relied on yet.
> this is PoC and heavily under slop-generating phases to proof that lsp approach actually work first

![OMP writing a Rust handler, with jev-lsp's rule finding on the generated line steering the fix](docs/assets/jev-steering.svg)

An LSP server that runs a model over the files you have open and reports through your editor's
own surfaces: diagnostics, code actions, code lens. The ambient pass runs the rules your
repository states in `.jev/rules/*.json`; each line a rule points at is sent to **Jev**, a
decision model that returns one answer per question with a probability, not prose. Standard LSP
surfaces only. Neovim is the primary client; `nvim/` is the plugin.

## Use it

```sh
cargo build --release   # target/release/jev-lsp (the server) and target/release/jev (a CLI)

# the plugin: the attach pass that covers files Neovim cannot identify, the keymaps and :Jev
ln -s /path/to/jev-lsp/nvim ~/.local/share/nvim/site/pack/jev/start/jev
```

```lua
require('jev').setup({ cmd = { '/path/to/jev-lsp/target/release/jev-lsp' } })
```

Two tiers, two endpoints. The chat tiers answer actions, plans and explanations; the decision
tier answers the rules pass.

```sh
export JEV_BASE_URL=http://127.0.0.1:8080/v1   # chat model, OpenAI-compatible
export JEV_MODEL=your-model-name
export JEV_API_KEY_ENV=OPENROUTER_API_KEY   # hosted: the NAME of the variable holding the key

# the decision tier defaults to hosted Jev (api.typesafe.ai, key from TYPESAFE_API_KEY);
# a local System One server instead:
export JEV_DECIDE_BASE_URL=http://127.0.0.1:8009/v1
export JEV_DECIDE_MODEL=kev-latest
```

A rule is a convention in prose plus the inspection that finds the lines it may apply to. Files
are read from `.jev/rules/` in path order:

```jsonc
{ "schema": "jev.rules/1",
  "rules": [
    { "id": "no-unwrap-in-handlers",
      "title": "Unwrap in a request handler",
      "text": "A handler must not unwrap; return the error instead.",
      "applies_to": ["**/*.rs"],
      "inspection": { "kind": "regex", "pattern": "\\.unwrap\\(\\)" },
      "judgement": { "question": "Is this unwrap reachable from a request handler?",
                     "min_probability": 0.75 } } ] }
```

Open a file and save. A finding appears as a diagnostic on the line when the rule's inspection
and the decision both accept it. With no rule files, the pass publishes nothing and reports
`no_rules` rather than a clean file. `docs/TUTORIAL.md` §3.7 walks through writing one.

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
`~/.omp/agent/lsp.json` to apply it to every project):

```json
{"servers":{"jev-lsp":{"command":"/path/to/jev-lsp/target/release/jev-lsp","args":["--stdio"],"fileTypes":[".rs"],"rootMarkers":[".git"]}}}
```

OpenCode, from its schema (`https://opencode.ai/config.json`, `lsp`), where `command` is an
array and `env` carries the endpoint variables:

```json
{"lsp":{"jev-lsp":{"command":["/path/to/jev-lsp/target/release/jev-lsp","--stdio"],"extensions":[".rs"],
                    "env":{"JEV_DECIDE_BASE_URL":"http://127.0.0.1:8009/v1","JEV_DECIDE_MODEL":"kev-latest"}}}}
```

Other harnesses keep the same three things in their own file: a command, the extensions it
applies to, and a root marker.

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
