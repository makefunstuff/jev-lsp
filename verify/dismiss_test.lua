-- Dismissal (PROTOCOL §9): a dismissed finding does not come back.
--
-- The claim in the frozen contract is specific — findings are dismissible, the dismissal is
-- recorded *per repository*, and it does not resurface — and nothing tested it. It is a
-- client-side behaviour (the plugin filters what it displays), which is why it lives here
-- rather than in a Rust unit test.
--
--   JEV_LSP_BIN=/path/to/jev-lsp JEV_BASE_URL=http://127.0.0.1:8099/v1 \
--   JEV_DECIDE_BASE_URL=http://127.0.0.1:8099/v1 JEV_DECIDE_MODEL=stub-model \
--     nvim --headless -u NONE -l verify/dismiss_test.lua
--
-- The ambient pass is the rules pass, so the decide tier has to be reachable too: without a
-- decision endpoint the rule below produces no finding and the dismissal has nothing to act on.

local BIN = os.getenv('JEV_LSP_BIN')
if BIN == nil or BIN == '' then
  io.stderr:write(
    'dismiss_test: JEV_LSP_BIN is not set and is required (path to the jev-lsp server).\n'
      .. '  usage: JEV_LSP_BIN=/path/to/jev-lsp JEV_BASE_URL=http://127.0.0.1:8099/v1 '
      .. 'JEV_DECIDE_BASE_URL=http://127.0.0.1:8099/v1 '
      .. 'nvim --headless -u NONE -l verify/dismiss_test.lua\n'
  )
--
-- `JEV_ROOT` names the fixture workspace; without it the harness makes one with
-- `vim.fn.tempname()` and **removes it again on the way out** (green, red, or skipped),
-- so `/tmp` does not fill up with repository markers. Name one when a failure needs
-- reading afterwards — a root the caller named is left exactly where it is.
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

local server_log = dofile(vim.fn.getcwd() .. '/verify/harness_log.lua')
server_log.capture()

-- A repository root: `vim.fs.root(…, {'.git'})` is what the plugin keys dismissals on. A root this
-- harness created is removed on the way out; a `JEV_ROOT` the caller named is left where it is.
local fixture_root = dofile(vim.fn.getcwd() .. '/verify/fixture.lua')
local root, owned_root = fixture_root.root('JEV_ROOT', '-jev-dismiss')
vim.fn.mkdir(root .. '/.git', 'p')
-- The ambient pass is the *rules* pass (PROTOCOL §9 / `jev.rules/1`), so a repository with no
-- rule has no ambient finding to dismiss. This rule's inspection matches the `open(` the
-- fixture below actually contains; the decision tier the stub answers clears its floor.
vim.fn.mkdir(root .. '/.jev/rules', 'p')
vim.fn.writefile({
  -- A long-bracket string: the JSON needs a literal `\\(` so the decoded pattern is `\(`.
  [[{"schema":"jev.rules/1","rules":[{"id":"no-bare-open","title":"File opened without a context manager","text":"Open the file with a context manager so the handle is closed.","severity":"warning","applies_to":["**/*.py"],"inspection":{"kind":"regex","pattern":"open\\(","max_matches":0},"judgement":{"question":"Is this handle left open on a path that matters?","criteria":{"true":"the handle outlives the function or is never closed","false":"the handle is closed by the caller or the process"},"min_probability":0.75},"verb_hint":"fix"}]}]],
}, root .. '/.jev/rules/example.json')
local path = root .. '/loader.py'
vim.fn.writefile({ 'def load(path):', '    f = open(path)', '    return f' }, path)

require('jev').setup({ cmd = { BIN }, keymaps = false })
vim.cmd('edit ' .. vim.fn.fnameescape(path))
local buf = vim.api.nvim_get_current_buf()

vim.wait(3000, function()
  return #vim.lsp.get_clients({ bufnr = buf, name = 'jev' }) > 0
end, 25)
check(#vim.lsp.get_clients({ bufnr = buf, name = 'jev' }) > 0, 'the plugin attached a client')

server_log.capture()

--- Diagnostics this plugin produced, from the server's point of view (pre-filter).
local function jev_diagnostics()
  local out = {}
  for _, d in ipairs(vim.diagnostic.get(buf)) do
    if d.source == 'jev' then
      out[#out + 1] = d
    end
  end
  return out
end

server_log.capture()
vim.cmd('write')

local found = vim.wait(30000, function()
  return #jev_diagnostics() > 0
end, 50)

if not found then
  skip('no finding arrived, so nothing can be dismissed (is JEV_BASE_URL set and the '
    .. 'endpoint reachable?)')
  server_log.dump()
  -- Every exit path, including this one: a skip is not a reason to leave a repository marker in
  -- `/tmp` for the next run to trip over.
  fixture_root.remove(root, owned_root)
  say(('[dismiss] %d failure(s), %d skip(s)'):format(failures, skips))
  os.exit(failures == 0 and 0 or 1)
end

local first = jev_diagnostics()[1]
-- `user_data.lsp` is the original LSP diagnostic; its `data` is the payload the server put
-- there (PROTOCOL §9), which is what dismissal is keyed on.
local lsp_payload = ((first.user_data or {}).lsp or {}).data or {}
local finding_id = lsp_payload.finding_id
check(type(finding_id) == 'string' and finding_id ~= '',
  ('the finding carries a stable id to dismiss: %s'):format(tostring(finding_id)))

print('[dismiss] ' .. tostring(#jev_diagnostics()) .. ' finding(s) before')
vim.api.nvim_win_set_cursor(0, { first.lnum + 1, first.col })
require('jev').dismiss()

vim.wait(500, function()
  return false
end)
check(#jev_diagnostics() == 0, 'it disappears once dismissed',
  ('still %d'):format(#jev_diagnostics()))

local dismissals = root .. '/.git/jev/dismissed.json'
check(vim.fn.filereadable(dismissals) == 1,
  'the dismissal is recorded in the repository, not in memory: ' .. dismissals)
if vim.fn.filereadable(dismissals) == 1 then
  local raw = table.concat(vim.fn.readfile(dismissals), '')
  local ok, doc = pcall(vim.json.decode, raw)
  check(ok and type(doc) == 'table', 'and it is valid JSON')
  if ok and doc and doc.dismissed then
    local entry = finding_id and doc.dismissed[finding_id] or nil
    check(entry ~= nil, ('and it records this finding id: %s'):format(tostring(finding_id)))
    check(entry ~= nil and entry.content_hash ~= nil,
      'along with the content hash it was dismissed against')
  end
end

-- The point of the record: a fresh pull must not bring it back.
local refresh = vim.lsp.get_clients({ bufnr = buf, name = 'jev' })[1]:request(
  'textDocument/diagnostic',
  { textDocument = { uri = vim.uri_from_bufnr(buf) } },
  function() end,
  buf
)
vim.wait(1500, function()
  return false
end)
check(refresh ~= nil, 'the diagnostic request went out')
check(#jev_diagnostics() == 0,
  'and the dismissed finding does not resurface on a fresh pull',
  ('%d came back'):format(#jev_diagnostics()))

if failures > 0 then
  server_log.dump()
end
fixture_root.remove(root, owned_root)
say(('[dismiss] %d failure(s), %d skip(s)'):format(failures, skips))
os.exit(failures == 0 and 0 or 1)
