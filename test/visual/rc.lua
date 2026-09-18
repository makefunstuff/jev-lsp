-- meta-lsp, set up and nothing else: no fixture, no autocommands, no options touched.
--
-- For trying it against your own configuration and your own files. It is the same setup the
-- isolated config uses, so what you see here is what you see there.
--
--   nvim -c 'luafile <repo>/test/visual/rc.lua' your/file.py
--
-- The plugin directory is put on the runtimepath here rather than with `--cmd`: a config
-- manager runs after `--cmd` and can rebuild the runtimepath, which is exactly what happened
-- the first time this was tried against a real config. One flag, no ordering assumption.
--
-- Paste the `require` call into your config for a permanent install, after putting `nvim/`
-- on the runtimepath (or after symlinking it into `site/pack/*/start/`).
--
-- Environment variables:
--   META_LSP_BIN    default <repo>/target/release/meta-lsp
--   META_BASE_URL   default http://127.0.0.1:37313/v1   (the local llama.cpp server)
--   META_MODEL      default qwen3.6-35b-a3b-iq3xxs
--   META_REVIEW_MODEL  defaults to META_MODEL

local here = vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':p')
local repo = vim.fn.fnamemodify(here, ':h:h:h')

vim.opt.runtimepath:prepend(repo .. '/nvim')

local BIN = os.getenv('META_LSP_BIN') or (repo .. '/target/release/meta-lsp')
local BASE = os.getenv('META_BASE_URL') or 'http://127.0.0.1:37313/v1'
local MODEL = os.getenv('META_MODEL') or 'qwen3.6-35b-a3b-iq3xxs'
local REVIEW = os.getenv('META_REVIEW_MODEL') or MODEL

if vim.fn.executable(BIN) ~= 1 then
  vim.notify('meta-lsp binary not found or not executable: ' .. BIN
    .. '\n  build it:  cd ' .. repo .. ' && cargo build --release', vim.log.levels.ERROR)
  return
end

require('meta').setup({
  cmd = { BIN },
  settings = {
    models = {
      reason = { base_url = BASE, model = MODEL, timeout_ms = 120000 },
      review = { base_url = BASE, model = REVIEW, timeout_ms = 120000 },
    },
  },
})

-- Nothing is opened here: the file you passed on the command line is already open, and a file
-- opened the usual way goes through the plugin's attach pass. Only if you started with an
-- empty buffer does it need the server on the next file you open, which the same pass handles.
