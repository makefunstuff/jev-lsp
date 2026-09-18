# Trying it by hand

Everything below runs in **your own terminal**, against your own model, with no change to your
Neovim configuration.

## 1. Look at it

```sh
cd /data/jpl/Work/meta-lsp

# the local llama.cpp server (fast, no cost, thinking switch honoured)
nvim -u test/visual/init.lua
```

That loads an isolated config: your `~/.config/nvim` is untouched, nothing is installed, and
quitting leaves nothing behind except a dismissal file inside the fixture's own directory.

To use the cloud model instead — the omp auth gateway needs no key handling:

```sh
META_BASE_URL=http://127.0.0.1:4000/v1 META_MODEL=deepseek/deepseek-flash \
  nvim -u test/visual/init.lua
```

A fixture with two deliberate defects opens, and a message confirms the server attached and
whether inline completion is advertised.

## 2. What should happen

| Do this | Expect |
|---|---|
| wait ~2 s after opening | `meta ready` message; no errors |
| `:write` | a warning sign appears in the sign column within a few seconds |
| put the cursor on that line, `<leader>ma` | a menu whose first entry is `Fix: file handle is never closed` |
| pick it | the edit is applied; the sign clears |
| `<leader>mu` | the buffer is restored byte for byte |
| `<leader>mv` after saving and picking an action | side-by-side diff; `<CR>` applies, `q` leaves everything alone |
| `<leader>me` | an explanation opens in a scratch buffer; `q` closes it |
| `<leader>md` on a finding | it disappears, and stays gone after a re-save |
| `i` then type inside a function | ghost text after ~0.4 s idle; `<Tab>` accepts, `<C-e>` dismisses |
| `:Meta status` | queue, budgets, cache counters, and which endpoints are in force |

`<leader>ms` (status), `<leader>mx` (cancel), `<leader>mS` (stop — the kill switch, no prompt),
`<leader>mG` (start again) are also wired.

## 3. On your own files

```sh
META_BASE_URL=http://127.0.0.1:37313/v1 META_MODEL=qwen3.6-35b-a3b-iq3xxs \
  nvim -u test/visual/init.lua path/to/your/file.py
```

The config opens its fixture on startup, so pass `-c 'edit your/file'` afterwards, or just
open files normally — the plugin attaches to any file buffer, including files Neovim cannot
identify.

## 4. In your real Neovim (optional)

I have not touched your configuration. If you want it there:

```sh
ln -s /data/jpl/Work/meta-lsp/nvim ~/.local/share/nvim/site/pack/meta/start/meta
```

```lua
-- your config
require('meta').setup({
  cmd = { '/data/jpl/Work/meta-lsp/target/release/meta-lsp' },
  settings = {
    -- PROTOCOL.md §10. Either shape works: this one, or the same table under `meta`.
    models = {
      reason = { base_url = 'http://127.0.0.1:37313/v1', model = 'qwen3.6-35b-a3b-iq3xxs' },
      review = { base_url = 'http://127.0.0.1:37313/v1', model = 'qwen3.6-35b-a3b-iq3xxs' },
    },
  },
})
```

`META_BASE_URL` / `META_MODEL` / `META_REVIEW_MODEL` override the settings and are applied
last, which is the shortest path if you just want it running.

## 5. When it does not work

1. `:checkhealth meta` — binary, attachment, capabilities.
2. `:LspLog` — the server logs the endpoint it is actually using:
   `meta: settings applied — reason http://… · review http://… · inline completion on`.
   If that line names an endpoint you did not configure, the settings did not arrive and the
   server is on its built-in defaults.
3. `:Meta status` — what the server believes its configuration is.
4. Nothing after `:write`? The analysis is triggered on save
   (`triggers.diagnostics = "save"`); check `:Meta status` for `calls_last_minute`. Zero after
   a save means the model call was not made or failed, and the reason is in `:LspLog`.

## 6. What is not built

Stated so nothing here promises a surface that does not exist:

- **Plans have no step-through buffer.** `:Meta plan` asks for a goal and the server returns a
  verified plan artifact (targets, per-step verbs, cost), but nothing renders it or applies a
  step: `meta.apply` / `meta.revert` are served and tested, and reachable from the CLI and a
  harness, not from a keymap yet.
- **Code lens and inlay hints** are designed and not advertised.
- **Scope resolution is structural**, not treesitter-backed: brace and indentation blocks with
  a whole-file fallback. It reports which it used.
