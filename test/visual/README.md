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
which endpoints are in force.

## 2. What should happen

| Do this | Expect |
|---|---|
| wait ~2 s after opening | `meta ready` message; no errors |
| `:write` | a warning sign appears in the sign column within a few seconds |
| put the cursor on that line, `<leader>ma` | a menu whose first entry is `Fix: file handle is never closed` |
| pick it | the edit is applied; the sign clears |
| `<leader>mu` | the buffer is restored byte for byte |
| `:Meta explain` | an explanation opens in a scratch buffer; `q` closes it |
| `:Meta dismiss` on a finding | it disappears, and stays gone after a re-save |
| `:Meta status` | queue, budgets, cache counters, and which endpoints are in force |
| `:Meta followup` with the cursor on a finding | it asks you a question, then the answer arrives in a buffer a few words at a time |
| `:Meta plan`, type a goal | a plan opens as one line per step; `<CR>` applies that step, `a` the rest, `u` takes one back |
| `:lua vim.lsp.codelens.run()` on a function | runs the code lens there — `meta: explain`, or `meta: N finding(s) · fix` |
| `:Meta usage` | published findings, files analysed, and what was applied, dismissed, accepted, undone |
| `:Meta session` | what this server has done here; `<CR>` on an entry opens the file and line it names |

Four keys are bound: those above, plus `<leader>mq` (ask). The rest of the surface is typed —
`:Meta review` (review the file now; the findings come back in the Result rather than waiting
for the next save), `:Meta hints on|off` (inlay hints, off by default: Neovim switches hints on
per *buffer*, not per client, so turning them on for meta's badge would turn on every other
server's hints in that buffer too), `:Meta cancel`, `:Meta stop` (the kill switch, no prompt),
`:Meta start`, `:Meta log`, `:Meta recompute`, `:Meta where`.

## 3. On your own files

```sh
META_BASE_URL=http://127.0.0.1:37313/v1 META_MODEL=qwen3.6-35b-a3b-iq3xxs \
  nvim -u test/visual/init.lua path/to/your/file.py
```

The config opens its fixture on startup, so pass `-c 'edit your/file'` afterwards, or just
open files normally — the plugin attaches to any file buffer, including files Neovim cannot
identify.

## 4. In your own configuration

Verified against a real config (lazy.nvim, pylsp, blink.cmp): the server attaches, the save
triggers an analysis, and the finding lands. No change to your configuration:

```sh
cd /data/jpl/Work/meta-lsp
nvim -c 'luafile test/visual/rc.lua' path/to/your/file.py
```

`test/visual/rc.lua` puts the plugin on the runtimepath itself and calls `require('meta').setup`.
It does not use `--cmd 'set rtp^=…'`: a config manager runs after `--cmd` and rebuilds the
runtimepath, so that form fails with `module 'meta' not found` — the first thing tried here.

For a permanent install, put `nvim/` on the runtimepath and paste the `require('meta').setup`
call from `rc.lua` into your config:

```sh
ln -s /data/jpl/Work/meta-lsp/nvim ~/.local/share/nvim/site/pack/meta/start/meta
```

With lazy.nvim, a local directory spec does the same and is what was used against a real
config (`~/.config/nvim/lua/plugins/init.lua`, which requires each spec file explicitly, so a
new file alone would have been inert):

```lua
{
  dir = '/data/jpl/Work/meta-lsp/nvim',
  name = 'meta',
  lazy = false,          -- must be attached before the first buffer, not on an event
  config = function()
    local bin = '/data/jpl/Work/meta-lsp/target/release/meta-lsp'
    if vim.fn.executable(bin) ~= 1 then
      vim.notify('meta-lsp binary not built: ' .. bin, vim.log.levels.WARN)
      return               -- a missing binary must not break startup
    end
    require('meta').setup({
      prefix = '<leader>M',
      cmd = { bin },
      settings = {
        models = {
          reason = { base_url = 'http://127.0.0.1:37313/v1', model = 'qwen3.6-35b-a3b-iq3xxs' },
          review = { base_url = 'http://127.0.0.1:37313/v1', model = 'qwen3.6-35b-a3b-iq3xxs' },
        },
      },
    })
  end,
}
```

Two deviations from the defaults there, both because of what the surrounding config already
does, and both worth checking for in any config:

- **`prefix`.** The plugin's default is `<leader>m`. A config that already maps `<leader>ma`
  and `<leader>mb` — Telescope marks and `make` in the sample config — will have one of the
  two silently win. `<leader>M` keeps every meta key in one namespace and collides with
  nothing. Check `<leader>m*` before installing.
- **No ghost text.** Inline completion was removed from the server on 2026-09-19; generated
  code is asked for with `<leader>Ma` or `:Meta ask`. Two
  providers driving the same surface is worse than one.

`META_BASE_URL` / `META_MODEL` / `META_REVIEW_MODEL` are applied last and override the settings.

### Give it time

An analysis against the local model takes **10–40 s**: the prompt is the scope plus context,
and generation is in the tens of tokens per second. Looking for a sign column 20 s after a
save and concluding it is broken is a mistake this document has already caused once. `:Meta
status` shows `cache.misses` and `cache.entries` — watch those rather than the clock.

### One thing in the sample config is not ours

`client.supports_method(...)` called with a dot is deprecated in Neovim 0.12 and prints a
warning (`client.supports_method is deprecated. Run ':checkhealth vim.deprecated'`); the
replacement is the colon form, `client:supports_method(...)`. The plugin uses the colon form.

## 5. When it does not work

1. `:checkhealth meta` — binary, attachment, capabilities.
2. `:LspLog` — the server logs the endpoint it is actually using:
   `meta: settings applied — reason http://… · review http://…`.
   If that line names an endpoint you did not configure, the settings did not arrive and the
   server is on its built-in defaults.
3. `:Meta status` — what the server believes its configuration is.
4. Nothing after `:write`? The analysis is triggered on save
   (`triggers.diagnostics = "save"`); check `:Meta status` for `calls_last_minute`. Zero after
   a save means the model call was not made or failed, and the reason is in `:LspLog`.

## 6. Driving a Neovim that is already open

To try this in a long-running Neovim without restarting it, load the setup into it:

```
:luafile /data/jpl/Work/meta-lsp/test/visual/rc.lua
```

The attach pass sweeps the open buffers, so a client appears for each distinct workspace root
among them — six buffers in six projects is six servers, which is ordinary LSP behaviour and
worth knowing before it surprises you.

Over Herdr, drive it with `pane send-text` and then `pane send-keys <pane> enter`. **Do not use
`pane run`**: it wraps the text in a bracketed paste, and Neovim inserts pasted text into the
buffer whatever mode it is in — two accidental edits to a real file before that was clear. Send
`esc` first in any case.

## 7. What is not built

Stated so nothing here promises a surface that does not exist:

- **Nothing else from `docs/UX.md` §1 is missing.** Plans have their step-through buffer
  (`:Meta plan <goal>`): one line per step, `<CR>` applies that step, `a` the
  rest, `u` takes one back, and the line says what happened to it.
- **Code lens and inlay hints** are designed and not advertised.
- **Scope resolution is structural**, not treesitter-backed: brace and indentation blocks with
  a whole-file fallback. It reports which it used.
