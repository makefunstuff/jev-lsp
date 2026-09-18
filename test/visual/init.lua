-- An isolated Neovim config for trying meta-lsp by hand.
--
--   nvim -u /data/jpl/Work/meta-lsp/test/visual/init.lua
--
-- It does not touch your real configuration: no plugin directory, no rtp change outside this
-- process, nothing written to ~/.config/nvim. Quit with :qa! and nothing is left behind
-- except the dismissal file, which lives in the fixture's own .git/.
--
-- Point it at a model with environment variables:
--   META_BASE_URL   default http://127.0.0.1:37313/v1   (the local llama.cpp server)
--   META_MODEL      default qwen3.6-35b-a3b-iq3xxs
--   META_LSP_BIN    default <repo>/target/release/meta-lsp
--
-- The omp auth gateway works too, and needs no key handling:
--   META_BASE_URL=http://127.0.0.1:4000/v1 META_MODEL=deepseek/deepseek-flash nvim -u …

local here = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':p')
local repo = vim.fn.fnamemodify(here, ':h:h:h')

local BIN = os.getenv('META_LSP_BIN') or (repo .. '/target/release/meta-lsp')
local BASE = os.getenv('META_BASE_URL') or 'http://127.0.0.1:37313/v1'
local MODEL = os.getenv('META_MODEL') or 'qwen3.6-35b-a3b-iq3xxs'

if vim.fn.executable(BIN) ~= 1 then
  vim.notify('meta-lsp binary not found or not executable: ' .. BIN ..
    '\n  build it:  cd ' .. repo .. ' && cargo build --release', vim.log.levels.ERROR)
end

-- The fixture lives in its own directory, which becomes the workspace root.
local workdir = vim.fn.stdpath('cache') .. '/meta-visual'
vim.fn.mkdir(workdir, 'p')
vim.fn.mkdir(workdir .. '/.git', 'p') -- a root marker, so dismissals land somewhere sane
local fixture = workdir .. '/review_me.py'

if vim.fn.filereadable(fixture) == 0 then
  vim.fn.writefile({
    'import json',
    '',
    '',
    'def load_config(path):',
    '    f = open(path)',
    '    data = json.load(f)',
    '    return data["services"][0]["port"]',
    '',
    '',
    'def save(path, payload):',
    '    try:',
    '        with open(path, "w") as f:',
    '            json.dump(payload, f)',
    '    except Exception:',
    '        pass',
    '',
  }, fixture)
end

-- A custom `-u` file means none of the usual defaults are on: without this the fixture has
-- no filetype, no highlighting, and the plugin has to fall back to content sniffing.
vim.cmd('syntax on')
vim.opt.runtimepath:prepend(repo .. '/nvim')
vim.opt.number = true
vim.opt.signcolumn = 'yes'   -- findings land here; without it they are easy to miss
vim.opt.updatetime = 300     -- how quickly idle/save-driven work is picked up

require('meta').setup({
  cmd = { BIN },
  settings = {
    inline_completion = { enabled = true },
    models = {
      reason = { base_url = BASE, model = MODEL, timeout_ms = 120000 },
      review = { base_url = BASE, model = MODEL, timeout_ms = 120000 },
      fim = { base_url = BASE, model = MODEL, timeout_ms = 30000 },
    },
  },
})

-- Opened on VimEnter, deferred past startup. Neovim runs VimEnter with autocmd triggering
-- suppressed, so an `:edit` made directly inside it loads the buffer but fires no BufReadPost
-- or BufWinEnter — and the attach pass, which listens for exactly those, never sees the file.
-- That cost a round of "no client attached" while testing. `vim.schedule` runs the edit once
-- startup is over, so this takes the same path a user does when they open a file, rather than
-- papering over it by calling the plugin internals.
vim.api.nvim_create_autocmd('VimEnter', {
  callback = function()
    vim.schedule(function()
      vim.cmd('edit ' .. vim.fn.fnameescape(fixture))
    end)
  end,
})

-- Say where things stand rather than leaving a silent buffer. Poll instead of checking once:
-- starting the server, the initialize handshake and the first configuration round trip take a
-- moment, and a fixed delay reports a failure that has not happened. Never claim to be
-- working when the client is not there.
local waited = 0
local function report()
  local clients = vim.lsp.get_clients({ name = 'meta' })
  if #clients == 0 then
    waited = waited + 250
    if waited < 10000 then
      return vim.defer_fn(report, 250)
    end
    vim.notify('meta: no client attached after 10s — check :LspLog, and that ' .. BIN
      .. ' runs', vim.log.levels.ERROR)
    return
  end
  local inline = clients[1]:supports_method('textDocument/inlineCompletion')
  vim.notify(('meta ready · server attached · inline completion %s\n'
    .. 'model: %s\n'
    .. 'press :Meta status · <leader>ma for actions after saving this file')
    :format(inline and 'advertised' or 'NOT advertised', MODEL), vim.log.levels.INFO)
end
vim.defer_fn(report, 500)
