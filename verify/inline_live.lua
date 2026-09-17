-- Live Neovim proof for inline completion (U7).
--
-- The server side — the capability, the handler, every gate — is covered by
-- `verify/inline_test.py`. What only a real client can show is that Neovim accepts the
-- advertised capability and attaches its completor.
--
--   META_LSP_BIN=/path/to/meta-lsp META_BASE_URL=http://127.0.0.1:8099/v1 \
--     nvim --headless -u NONE -l verify/inline_live.lua

local BIN = os.getenv('META_LSP_BIN')
if BIN == nil or BIN == '' then
  io.stderr:write(
    'inline_live: META_LSP_BIN is not set and is required (path to the meta-lsp server).\n'
      .. '  usage: META_LSP_BIN=/path/to/meta-lsp META_BASE_URL=http://127.0.0.1:8099/v1 '
      .. 'nvim --headless -u NONE -l verify/inline_live.lua\n'
  )
  os.exit(2)
end

local failures, skips = 0, 0
local function say(line)
  io.stdout:write(line .. '\n')
  io.stdout:flush()
end
local function check(cond, label, detail)
  if cond then
    say('ok    ' .. label)
  else
    failures = failures + 1
    say('FAIL  ' .. label .. (detail ~= nil and ('  — ' .. tostring(detail)) or ''))
  end
  return cond
end
local function skip(label)
  skips = skips + 1
  say('SKIP  ' .. label)
end

vim.opt.runtimepath:prepend(vim.fn.getcwd() .. '/nvim')
require('meta').setup({
  cmd = { BIN },
  keymaps = false,
  settings = { inline_completion = { enabled = true } },
})

local dir = vim.fn.tempname()
vim.fn.mkdir(dir, 'p')
local path = dir .. '/loader.py'
vim.fn.writefile({ 'def load(path):', '    total = 0' }, path)
vim.cmd('edit ' .. vim.fn.fnameescape(path))
local buf = vim.api.nvim_get_current_buf()

vim.wait(3000, function()
  return #vim.lsp.get_clients({ bufnr = buf, name = 'meta' }) > 0
end, 25)

local client = vim.lsp.get_clients({ bufnr = buf, name = 'meta' })[1]
if not check(client ~= nil, 'the plugin attached a client') then
  say(('[inline_live] %d failure(s), %d skip(s)'):format(failures, skips))
  os.exit(1)
end

-- The whole point of injecting the capability: without it Neovim never attaches the
-- completor, and the server's handler is unreachable.
check(
  client:supports_method('textDocument/inlineCompletion'),
  'the client sees the advertised inlineCompletionProvider'
)

vim.lsp.inline_completion.enable(true)
local cap = vim.lsp._capability.all.inline_completion
local attached = vim.wait(2000, function()
  return cap ~= nil and cap.active[buf] ~= nil
end, 25)
check(
  attached,
  'enabling it attaches the completor Neovim drives on a 200 ms insert-mode timer',
  cap and 'the capability never became active' or nil
)

-- Ghost text cannot be observed here. Measured: headless fires none of the events the
-- completor listens for — `InsertEnter`, `CursorMovedI` and `TextChangedP` all fire zero
-- times after `startinsert` plus a text change — and neither the automatic path nor
-- `inline_completion.get()` issues a request. A harness limitation, reported as a skip
-- rather than dressed up as a pass; docs/VERIFICATION.md §9 carries the interactive probe.
local ns = vim.api.nvim_create_namespace('nvim.lsp.inline_completion')
vim.api.nvim_win_set_cursor(0, { 2, 13 })
vim.cmd('startinsert')
vim.wait(2000, function()
  return false
end)
local marks = vim.api.nvim_buf_get_extmarks(buf, ns, 0, -1, {})
if #marks > 0 then
  local virt = marks[1][4] and marks[1][4].virt_text
  check(virt ~= nil and virt[1][1] ~= '', ('ghost text is the model text: %q'):format(virt and virt[1][1] or ''))
else
  skip('ghost text needs an interactive session: headless fires no insert-mode events')
end

check(
  vim.api.nvim_buf_get_lines(buf, 0, -1, false)[2] == '    total = 0',
  'and nothing was inserted into the buffer unasked'
)

say(('[inline_live] %d failure(s), %d skip(s)'):format(failures, skips))
os.exit(failures == 0 and 0 or 1)
