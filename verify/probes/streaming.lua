-- Evidence for PROTOCOL.md §3.5 — the streaming channel, exercised against the real client.
--
-- Proves three things that the design depends on:
--   1. a client-supplied `workDoneToken` reaches the server inside the request params
--      (ExecuteCommandParams extends WorkDoneProgressParams), so no
--      window/workDoneProgress/create is needed for the normal path;
--   2. `$/progress` under that token surfaces in the client's `LspProgress` autocmd;
--   3. the begin/report/end sequence arrives in order and terminates.
--
-- Runs the stub server in verify/probes/streaming/server.py over real stdio framing.
-- Exits nonzero if any assertion fails.

local TOKEN = 'jev:probe-8f3c1d'

local here = debug.getinfo(1, 'S').source:sub(2)
local dir = vim.fn.fnamemodify(here, ':h')
local server = dir .. '/streaming/server.py'

local buf = vim.api.nvim_create_buf(true, false)
vim.api.nvim_set_current_buf(buf)
vim.api.nvim_buf_set_name(buf, '/tmp/jev-probe-streaming.lua')
vim.api.nvim_buf_set_lines(buf, 0, -1, false, { '-- probe' })

local seen = {}
vim.api.nvim_create_autocmd('LspProgress', {
  callback = function(ev)
    local params = ev.data and ev.data.params
    if not params or params.token ~= TOKEN then
      return
    end
    seen[#seen + 1] = params.value and params.value.kind or '?'
  end,
})

local client_id = vim.lsp.start({
  name = 'jev-streaming-probe',
  cmd = { 'python3', server },
  root_dir = '/tmp',
}, { bufnr = buf })

assert(client_id, 'vim.lsp.start returned no client id')
local client = assert(vim.lsp.get_client_by_id(client_id), 'client not created')

local attached = vim.wait(10000, function()
  return vim.lsp.get_client_by_id(client_id) ~= nil
    and client:supports_method('workspace/executeCommand')
end, 50)
assert(attached, 'client never became ready')
assert(client.server_capabilities.executeCommandProvider.workDoneProgress == true,
  'stub did not declare workDoneProgress')

local result, req_err = nil, nil
local got_response = false
-- Neovim's request handler is (err, result, ctx, config) — error first.
client:request('workspace/executeCommand', {
  command = 'probe.emit',
  arguments = {},
  workDoneToken = TOKEN,
}, function(err, res)
  req_err, result = err, res
  got_response = true
end, buf)

assert(vim.wait(10000, function() return got_response end, 50), 'no response to executeCommand')
-- The server writes progress before the response, so these have all arrived; wait anyway
-- rather than depend on autocmd dispatch timing.
vim.wait(1000, function() return #seen >= 3 end, 50)
assert(not req_err, 'server returned an error: ' .. vim.inspect(req_err))

local failures = {}
local function check(cond, label) if not cond then failures[#failures + 1] = label end end

check(result and result.sawWorkDoneToken == true,
  'server did not receive workDoneToken in the request params')
check(table.concat(seen, ',') == 'begin,report,end',
  'progress sequence was {' .. table.concat(seen, ',') .. '}, expected begin,report,end')

print('[streaming] token in request params : ' .. tostring(result and result.sawWorkDoneToken))
print('[streaming] progress kinds observed : {' .. table.concat(seen, ',') .. '}')
print('[streaming] ' .. #failures .. ' assertion(s) failed')
for _, f in ipairs(failures) do
  print('  FAIL ' .. f)
end

client:stop(true)
vim.wait(500, function() return false end)
os.exit(#failures == 0 and 0 or 1)
