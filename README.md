# jev-lsp

> [!WARNING]
> **Heavily work in progress.** The interface, the contract and the
> configuration keys may change without notice; nothing here should be relied on yet.
> this is PoC and heavily under slop-generating phases to proof that lsp approach actually work first

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
