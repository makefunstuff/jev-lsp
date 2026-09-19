-- Evidence for PROTOCOL.md §2, §4 and §8 — the frozen contract exercised as a whole
-- exchange against the real client, using verify/probes/trace/server.py.
--
-- What this proves, and why it matters more than the unit-level probes:
--
--   * the declared capabilities are accepted and every request they advertise is routed;
--   * `codeAction` on the fast path returns an action with NO edit and the client accepts
--     it, keeps `data` for re-sending, and offers it (non-negotiable N2);
--   * `codeAction/resolve` may add `edit`, and the client applies it (N4);
--   * a `WorkspaceEdit` stamped with the version the action was created against is
--     applied when the document has not moved, and DROPPED when it has — the staleness
--     rule proven through the client rather than through a hand-built payload;
--   * findings carrying `data.finding_id` survive a pull-diagnostic round trip;
--   * a server-initiated `workspace/diagnostic/refresh` is acknowledged by the client.
--
-- Exits nonzero on any failure.

local TOKEN_URI = 'file:///tmp/jev-trace-fixture.lua'

local here = debug.getinfo(1, 'S').source:sub(2)
local server = vim.fn.fnamemodify(here, ':h') .. '/trace/server.py'

local failures = {}
local function check(cond, label)
  if cond then
    print('  ok    ' .. label)
  else
    print('  FAIL  ' .. label)
    failures[#failures + 1] = label
  end
end

local buf = vim.api.nvim_create_buf(true, false)
vim.api.nvim_set_current_buf(buf)
vim.api.nvim_buf_set_name(buf, '/tmp/jev-trace-fixture.lua')
vim.api.nvim_buf_set_lines(buf, 0, -1, false, { 'local function parse() end' })
vim.bo[buf].modified = false

--- The document URI, taken from the buffer with the same function the client uses.
---
--- Not a literal `file:///tmp/…`: Neovim resolves the name it is given (`/tmp` is a symlink to
--- `/private/tmp` on macOS), so a hard-coded URI asks the server about one spelling while the
--- client reports the other. The stub then keys its document table under the client's spelling,
--- answers every question about the literal with "version 0", and the staleness assertion below
--- passes for the wrong reason — nothing can be refused against version 0.
local TOKEN_URI = vim.uri_from_bufnr(buf)

local client_id = vim.lsp.start({
  name = 'jev-trace',
  cmd = { 'python3', server },
  root_dir = '/tmp',
}, { bufnr = buf })
assert(client_id, 'vim.lsp.start returned no client id')
local client = assert(vim.lsp.get_client_by_id(client_id))

local ready = vim.wait(10000, function()
  return client.server_capabilities
    and client.server_capabilities.codeActionProvider ~= nil
    and client:supports_method('textDocument/diagnostic')
end, 25)
assert(ready, 'client never became ready')

--- Request helper: Neovim's handler is (err, result, ctx).
local function req(method, params)
  local out = { done = false }
  client:request(method, params, function(err, res)
    out.err, out.res = err, res
    out.done = true
  end, buf)
  assert(vim.wait(10000, function() return out.done end, 25), 'timeout: ' .. method)
  assert(not out.err, method .. ' errored: ' .. vim.inspect(out.err))
  return out.res
end

local function doc_params()
  return { textDocument = { uri = TOKEN_URI } }
end

print('[trace] capabilities')
check(client.server_capabilities.positionEncoding == 'utf-8', 'server chose utf-8 (N1)')
check(client.server_capabilities.codeActionProvider.resolveProvider == true,
  'codeActionProvider.resolveProvider accepted')
check(client.server_capabilities.executeCommandProvider.workDoneProgress == true,
  'executeCommandProvider.workDoneProgress accepted')

-- Phase 1: the fast path returns an action with no edit (N2).
print('[trace] codeAction fast path')
local actions = req('textDocument/codeAction', {
  textDocument = { uri = TOKEN_URI },
  range = { start = { line = 0, character = 0 }, ['end'] = { line = 0, character = 0 } },
  context = { triggerKind = 1, diagnostics = {} },
})
check(type(actions) == 'table' and #actions == 1, 'exactly one action returned')
local action = actions[1]
check(action and action.edit == nil, 'fast-path action carries no edit (N2)')
check(action and type(action.data) == 'table' and action.data.verb == 'harden',
  'action carries structured data for resolve (§4)')
check(action and action.kind == 'refactor.rewrite.jev', 'kind is the declared .jev kind')

-- Phase 2: resolve adds the edit, and it applies while the document has not moved.
print('[trace] codeAction/resolve + apply')
local resolved = req('codeAction/resolve', action)
local edit = resolved and resolved.edit
check(edit ~= nil, 'resolve returned an edit')
check(edit and edit.documentChanges ~= nil, 'edit uses documentChanges, not a bare changes map (N4)')
check(edit and edit.changes == nil, 'edit has no bare `changes` key')
local td_edit = edit and edit.documentChanges[1]
check(td_edit and type(td_edit.textDocument.version) == 'number',
  'textDocument.version is an explicit integer, not absent (N4)')

vim.lsp.util.apply_workspace_edit(edit, 'utf-8')
local applied = vim.api.nvim_buf_get_lines(buf, 0, -1, false)[1]
check(applied == '-- jev: applied', 'edit applied to the buffer: ' .. vim.inspect(applied))

-- Phase 3: staleness, through the client. Fresh action, then move the document, then
-- resolve. The edit is stamped with the version the action was created against, so the
-- client must refuse it.
print('[trace] staleness through the client')
vim.wait(300, function() return false end)   -- let didChange land so versions advance
local fresh = req('textDocument/codeAction', {
  textDocument = { uri = TOKEN_URI },
  range = { start = { line = 0, character = 0 }, ['end'] = { line = 0, character = 0 } },
  context = { triggerKind = 1, diagnostics = {} },
})[1]

vim.api.nvim_buf_set_lines(buf, 0, 0, false, { '-- user typed this' })
vim.wait(300, function() return false end)   -- let didChange notify the server

local stale_resolved = req('codeAction/resolve', fresh)
local before = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
vim.lsp.util.apply_workspace_edit(stale_resolved.edit, 'utf-8')
local after = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
check(vim.deep_equal(before, after),
  'stale edit was refused by the client (buffer unchanged)')

-- Phase 4: pull diagnostics carry finding data.
print('[trace] pull diagnostics')
local diags = req('textDocument/diagnostic', doc_params())
check(diags and diags.kind == 'full' and #diags.items == 1, 'one finding returned')
check(diags and diags.items[1].data and diags.items[1].data.finding_id == 'finding-1',
  'finding carries data.finding_id (§9)')
check(diags and diags.resultId ~= nil, 'resultId present for incremental re-pull')
check(diags and diags.refreshSent >= 1, 'server sent workspace/diagnostic/refresh after didOpen')
check(diags and diags.refreshAcked >= 1, 'client acknowledged the refresh request')

print(('[trace] %d assertion(s) failed'):format(#failures))
client:stop(true)
vim.wait(300, function() return false end)
os.exit(#failures == 0 and 0 or 1)
