-- Live Neovim: the real plugin against the real server (docs/VERIFICATION.md §2).
--
--   META_LSP_BIN=/path/to/meta-lsp nvim --headless -u NONE -l verify/nvim_live.lua
--
-- META_LSP_BIN is the server binary (required). META_ROOT is the fixture workspace
-- (optional; a fresh temporary directory otherwise).
--
-- Asserts, in order:
--
--   1. the plugin attaches to a file Neovim cannot identify — an unknown extension is never
--      reached by the built-in `FileType` path, which is the whole claim of "every file"
--      (docs/LANGUAGE.md §1);
--   2. the pass never attaches twice, and never to a non-file buffer (§5);
--   3. the language hook recovers a language from contents and does not mutate buffer state
--      (§2);
--   4. `:Meta status` round-trips (PROTOCOL §6);
--   5. a code action, if the server offers one, resolves to a `WorkspaceEdit` that applies
--      and that `:Meta undo` restores byte-for-byte. No action is a SKIP, not a failure: the
--      server is allowed to have nothing to say about this fixture.
--
-- Prints ok/FAIL/SKIP per assertion. Exit is nonzero only on FAIL.
--
-- Every wait is bounded: `vim.wait` with an explicit timeout, so the test cannot hang.

local BIN = os.getenv('META_LSP_BIN')
if BIN == nil or BIN == '' then
  io.stderr:write(
    'nvim_live: META_LSP_BIN is not set and is required (path to the meta-lsp server).\n'
      .. '  usage: META_LSP_BIN=/path/to/meta-lsp nvim --headless -u NONE -l verify/nvim_live.lua\n'
  )
  os.exit(2)
end

local failures = 0
local skips = 0

-- Explicit newlines, not print(): Neovim's own message output (vim.notify, :Meta status)
-- can flush without a trailing newline and would otherwise run into the next check.
local function say(line)
  io.stdout:write(line .. '\n')
  io.stdout:flush()
end

local function ok(label)
  say('ok    ' .. label)
end

local function fail(label, detail)
  failures = failures + 1
  say('FAIL  ' .. label .. (detail ~= nil and ('  — ' .. tostring(detail)) or ''))
end

local function skip(label)
  skips = skips + 1
  say('SKIP  ' .. label)
end

local function check(cond, label, detail)
  if cond then
    ok(label)
  else
    fail(label, detail)
  end
  return cond
end

local function sleep(ms)
  vim.wait(ms, function()
    return false
  end, 10)
end

--- One request, bounded. `nil, err` means it did not answer in time.
local function request(client, method, params, timeout, bufnr)
  local answered, result, err = false, nil, nil
  client:request(method, params, function(e, r)
    err, result, answered = e, r, true
  end, bufnr)
  if not vim.wait(timeout or 15000, function()
    return answered
  end, 25) then
    return nil, { message = ('no response to %s within %d ms'):format(method, timeout or 15000) }
  end
  return result, err
end

-- Setup ---------------------------------------------------------------------------------------

local here = debug.getinfo(1, 'S').source:sub(2)
local PLUGIN = vim.fn.fnamemodify(here, ':p:h:h') .. '/nvim'
vim.opt.runtimepath:prepend(PLUGIN)

-- Surface what the server logged if anything fails: a dead model endpoint looks
-- exactly like a product defect from this side of the connection.
local server_log = dofile(vim.fn.fnamemodify(here, ':p:h') .. '/harness_log.lua')
server_log.capture()

local root = os.getenv('META_ROOT')
if root == nil or root == '' then
  root = vim.fn.tempname() .. '-meta-live'
end
vim.fn.mkdir(root, 'p')

local fixture = root .. '/fixture.zzz'
vim.fn.writefile(
  { 'local total = 0', 'for i = 1, 10 do total = total + i end', 'print(total)' },
  fixture
)

local meta = require('meta')
meta.setup({ cmd = { BIN } })

vim.cmd('filetype on') -- as a real session: FileType fires for whatever Neovim can identify
vim.cmd('edit ' .. vim.fn.fnameescape(fixture))
local bufnr = vim.api.nvim_get_current_buf()

say('[nvim_live] plugin  : ' .. PLUGIN)
say('[nvim_live] server  : ' .. BIN)
say('[nvim_live] fixture : ' .. fixture)

-- 1. Universality -----------------------------------------------------------------------------

check(
  vim.bo[bufnr].filetype == '',
  'fixture.zzz has no filetype, so the built-in FileType path cannot serve it'
)
local attached = vim.wait(10000, function()
  return #vim.lsp.get_clients({ bufnr = bufnr, name = 'meta' }) > 0
end, 25)
check(attached, 'the attach pass attached a client to an unidentified file (docs/LANGUAGE.md §1)')

-- 2. The guards -------------------------------------------------------------------------------

local before = #vim.lsp.get_clients({ bufnr = bufnr, name = 'meta' })
vim.api.nvim_exec_autocmds('BufWinEnter', { buffer = bufnr })
vim.api.nvim_exec_autocmds('BufReadPost', { buffer = bufnr })
sleep(200)
check(
  #vim.lsp.get_clients({ bufnr = bufnr, name = 'meta' }) == before,
  'the attach pass never attaches a second client to the same buffer'
)

local scratch = vim.api.nvim_create_buf(false, true)
vim.api.nvim_buf_set_name(scratch, root .. '/scratch-nofile')
require('meta.attach').attach(scratch)
sleep(200)
check(
  #vim.lsp.get_clients({ bufnr = scratch, name = 'meta' }) == 0,
  'a non-file buffer (buftype=nofile) is never attached (docs/LANGUAGE.md §5)'
)
vim.api.nvim_buf_delete(scratch, { force = true })

-- 3. The language hook ------------------------------------------------------------------------

local hook_buf = vim.api.nvim_create_buf(false, false)
vim.api.nvim_buf_set_lines(hook_buf, 0, -1, false, { '#!/usr/bin/env python3', 'import os' })
vim.api.nvim_buf_set_name(hook_buf, root .. '/hook.zzz')
local recovered = require('meta.attach').get_language_id(hook_buf, vim.bo[hook_buf].filetype)
check(recovered == 'python', 'the language hook recovers `python` from contents', vim.inspect(recovered))
check(vim.bo[hook_buf].filetype == '', 'the language hook did not mutate buffer state')
vim.api.nvim_buf_delete(hook_buf, { force = true })

-- 4. `:Meta status` ---------------------------------------------------------------------------

local dispatched, dispatch_err = pcall(vim.cmd, 'Meta status')
check(dispatched, ':Meta status dispatches without raising', dispatch_err)

local status_done, status_result, status_err = false, nil, nil
meta.status(function(err, result)
  status_err, status_result, status_done = err, result, true
end)
if check(vim.wait(15000, function()
  return status_done
end, 25), 'meta.status answered within 15 s') then
  check(status_err == nil, 'meta.status round-tripped without error', vim.inspect(status_err))
  check(type(status_result) == 'table', 'meta.status returned a result', vim.inspect(status_result))
end

-- 5. Code action: resolve, apply, undo ---------------------------------------------------------

local client = vim.lsp.get_clients({ bufnr = bufnr, name = 'meta' })[1]
if client == nil then
  skip('no client attached, so code actions cannot be exercised')
else
  local actions, actions_err = request(client, 'textDocument/codeAction', {
    textDocument = { uri = vim.uri_from_bufnr(bufnr) },
    range = { start = { line = 0, character = 0 }, ['end'] = { line = 0, character = 0 } },
    context = { triggerKind = 1, diagnostics = {} }, -- Invoked: cached, never a model call (N2)
  }, 15000, bufnr)

  if actions_err ~= nil then
    fail('textDocument/codeAction answered without error', vim.inspect(actions_err))
  elseif type(actions) ~= 'table' or #actions == 0 then
    skip('no code action offered for this fixture; resolve and apply are not exercised')
  else
    ok(('%d code action(s) offered'):format(#actions))
    -- A cold cache answers with a `disabled` placeholder carrying the reason (PROTOCOL §3.1);
    -- resolve the first action a user could actually pick.
    local candidate, placeholder
    for _, action in ipairs(actions) do
      if action.disabled ~= nil then
        placeholder = action
      elseif candidate == nil then
        candidate = action
      end
    end
    if candidate == nil then
      skip('every offered action is a disabled placeholder: ' .. vim.inspect(placeholder.disabled))
    else
      local resolved, resolve_err = request(client, 'codeAction/resolve', candidate, 40000, bufnr)
      if resolve_err ~= nil then
        fail('codeAction/resolve answered without error', vim.inspect(resolve_err))
      elseif type(resolved) == 'table' and type(resolved.edit) == 'table' then
        local text_before = vim.api.nvim_buf_get_lines(bufnr, 0, -1, true)
        vim.lsp.util.apply_workspace_edit(resolved.edit, client.offset_encoding)
        local text_after = vim.api.nvim_buf_get_lines(bufnr, 0, -1, true)
        check(
          not vim.deep_equal(text_before, text_after),
          'the resolved WorkspaceEdit changed the buffer'
        )
        vim.cmd('Meta undo')
        check(
          vim.deep_equal(vim.api.nvim_buf_get_lines(bufnr, 0, -1, true), text_before),
          ':Meta undo restored the buffer byte-for-byte (docs/UX.md §3.5)'
        )
      else
        skip('resolve returned no edit; the server reports why through window/showMessage')
      end
    end
  end
end

-- Report --------------------------------------------------------------------------------------

-- Lens state first, and a settle before the stop. Neovim's lens provider schedules a
-- request on a 200 ms debounce and asserts that the client still exists when it fires
-- (`lsp/codelens.lua:143`); `enable(false)` does not purge the stored client id, so the
-- pending request has to be allowed to run while the client is still there.
pcall(vim.lsp.codelens.enable, false)
vim.wait(400)
for _, c in ipairs(vim.lsp.get_clients({ name = 'meta' })) do
  c:stop(true)
end
sleep(300)

if failures > 0 then server_log.dump() end
say(('[nvim_live] %d failure(s), %d skip(s)'):format(failures, skips))
os.exit(failures == 0 and 0 or 1)
