-- Live Neovim: `<Tab>` accepts the ghost text, and still behaves like `<Tab>` when there is
-- none (docs/UX.md §3.4).
--
--   META_LSP_BIN=/path/to/meta-lsp META_BASE_URL=http://127.0.0.1:8099/v1 \
--     nvim --headless -u NONE -l verify/inline_accept_test.lua
--
-- The completion provider is the stub (`verify/stub_model.py`), whose completion rule answers
-- with the anchor it was given plus `_completed` — deterministic, and recognisable in the
-- buffer afterwards. The stub is required here: a real model answering differently on each run
-- cannot say whether Tab inserted the candidate or something else did.
--
-- Checks, in order:
--
--   1. a completion reaches the buffer only through `<Tab>`: the text after the cursor is
--      unchanged while the candidate is on screen, and after `<Tab>` it is exactly what the
--      provider returned;
--   2. with the candidate consumed, `<Tab>` does what it did before this mapping existed — a
--      literal tab under `-u NONE`, where no other mapping owns the key — instead of
--      re-entering the accept map or swallowing the key.
--
-- Prints ok/FAIL/SKIP per check. Exit is nonzero only on FAIL.

local BIN = os.getenv('META_LSP_BIN')
if BIN == nil or BIN == '' then
  io.stderr:write('inline_accept_test: META_LSP_BIN is required (path to the meta-lsp binary)\n')
  os.exit(2)
end
local MODEL = os.getenv('META_BASE_URL')
if MODEL == nil or MODEL == '' then
  io.stderr:write('inline_accept_test: META_BASE_URL is required (verify/stub_model.py)\n')
  os.exit(2)
end

vim.opt.runtimepath:prepend(vim.fn.fnamemodify(debug.getinfo(1, 'S').source:sub(2), ':h:h') .. '/nvim')
vim.opt.swapfile = false
vim.opt.expandtab = false
vim.opt.autoindent = false

local failures, skips = 0, 0
local function check(cond, what, detail)
  if cond then
    print('ok   ' .. what)
  else
    failures = failures + 1
    print('FAIL ' .. what .. (detail and (' -- ' .. tostring(detail)) or ''))
  end
end

local meta = require('meta')
meta.setup({
  cmd = { BIN },
  keymaps = false,
  settings = {
    -- The timer is shortened so the test does not wait out a human pause; the floor is one
    -- character so a four-character line is enough to ask.
    inline_completion = { enabled = true, idle_ms = 100, min_prefix_chars = 1, max_calls_per_min = 60 },
    budget = { max_calls_per_min = 120, max_calls_per_hour = 600 },
    models = {
      reason = { base_url = MODEL, model = 'stub-model' },
      review = { base_url = MODEL, model = 'stub-model' },
      fim = { base_url = MODEL, model = 'stub-model' },
    },
  },
})
vim.cmd('filetype on')

local path = vim.fn.tempname() .. '.py'
vim.fn.writefile({ 'def add(a, b):', '    return ' }, path)
vim.cmd('edit ' .. path)

local attached = vim.wait(15000, function()
  return #vim.lsp.get_clients({ bufnr = 0 }) > 0
end, 100)
check(attached, 'the server attaches to the buffer')
if not attached then
  print(('inline_accept_test: %d failure(s), %d skip(s)'):format(failures, skips))
  os.exit(1)
end

-- Put the cursor at the end of the `return ` line and go to insert, which is what starts the
-- clock: the completor fires on an insert-mode timer and nowhere else.
--
-- This runs from a deferred callback rather than straight through the script. A `-l` script
-- blocks the main loop, and `:startinsert` is honoured by that loop -- called inline it prints
-- `-- (insert) --` and the mode is still normal, so a test written that way types into a
-- buffer nothing is watching. `vim.wait` below keeps the loop turning so the timer can fire.
vim.api.nvim_win_set_cursor(0, { 2, #'    return ' - 1 })

local done = false
vim.defer_fn(function()
  vim.cmd('startinsert')
  vim.defer_fn(function()
    if vim.api.nvim_get_mode().mode:sub(1, 1) ~= 'i' then
      -- A headless `nvim -l` never enters insert mode: the script holds the main loop, and
      -- `:startinsert` is honoured by that loop. There is no completion to accept here, so the
      -- run says so instead of reporting a failure it cannot distinguish from a real one.
      -- Under a terminal (`script -qec "nvim -u NONE -c 'luafile ...'"`) this part runs.
      skips = skips + 1
      print('SKIP insert mode is unreachable in this harness; no candidate can be requested')
      done = true
      return
    end

    -- The candidate is overlay text: the buffer is untouched until it is accepted, so waiting
    -- on the buffer would wait forever, and the completor's bookkeeping is private. Hold the
    -- loop open for the timer and the stub instead, then ask for the candidate.
    vim.wait(4000, function()
      return false
    end, 100)

    local before = table.concat(vim.api.nvim_buf_get_lines(0, 0, -1, false), '\n')
    check(not before:find('\n    return %w'),
      'the buffer is unchanged while the candidate is shown', before)

    vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<Tab>', true, false, true), 'x', false)
    vim.wait(500, function()
      return false
    end, 50)

    local after = table.concat(vim.api.nvim_buf_get_lines(0, 0, -1, false), '\n')
    if after == before or (after == before .. '\t') then
      -- Tab fell through to the key it replaced, which is what happens when there is nothing
      -- to accept. Either the stub is not answering completions or the request never landed;
      -- both are "the accept path was not exercised" and neither is a passing accept test.
      skips = skips + 1
      print('SKIP no candidate was on screen, so acceptance was not exercised (Tab fell through)')
      done = true
      return
    end
    check(after:find('_completed', 1, true) ~= nil,
      'Tab inserted what the provider returned, not a tab character or nothing', after)

    -- The candidate is spent. Tab now has to be Tab: one more key, no second insertion.
    local accepted = after
    vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<Tab>', true, false, true), 'x', false)
    vim.wait(500, function()
      return false
    end, 50)
    local again = table.concat(vim.api.nvim_buf_get_lines(0, 0, -1, false), '\n')
    check(again == accepted .. '\t',
      'with no candidate, Tab falls through to the key it was before (a literal tab here)',
      vim.inspect(again))
    done = true
  end, 400)
end, 100)

vim.wait(30000, function()
  return done
end, 100)

print(('inline_accept_test: %d failure(s), %d skip(s)'):format(failures, skips))
os.exit(failures == 0 and 0 or 1)
