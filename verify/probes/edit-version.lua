-- Evidence for docs/research/nvim-lsp-surface.md §3 (and the frozen edit contract,
-- PROTOCOL.md §8).
--
-- Applies one zero-width edit four ways and reports what the client does with each.
-- The case that matters: an absent `version` raises inside the client, and a bare
-- `changes` map skips the version check entirely.
--
-- Reproduction of the probe table. Exits nonzero if any row changes behaviour, because
-- the frozen edit contract is written against these outcomes.

local M = vim.lsp.util

local buf = vim.api.nvim_create_buf(true, false)
vim.api.nvim_set_current_buf(buf)
-- A buffer *name*, not a file: nothing is written to this path, so the probe leaves nothing
-- in `/tmp`. The name exists because the stub needs a `file://` uri, and a fixed one keeps
-- the probe's output reproducible.
vim.api.nvim_buf_set_name(buf, '/tmp/jev-probe-edit-version.lua')
vim.api.nvim_buf_set_lines(buf, 0, -1, false, { 'local x = 1' })
vim.bo[buf].modified = false

local uri = vim.uri_from_bufnr(buf)
local BUFFER_VERSION = 5
M.buf_versions[buf] = BUFFER_VERSION

--- @param version_json string  JSON fragment injected after "uri": <values>
--- @return boolean ok, string first_line, string err
local function apply(version_json)
  local json = string.format(
    '{"documentChanges":[{"textDocument":{"uri":"%s"%s},"edits":'
      .. '[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},'
      .. '"newText":"-- probe\\n"}]}]}',
    uri, version_json)
  local ok, err = pcall(M.apply_workspace_edit, vim.json.decode(json), 'utf-8')
  local line = vim.api.nvim_buf_get_lines(buf, 0, -1, false)[1]
  return ok, line, ok and '' or tostring(err):gsub('.*/lua/vim/lsp/', '')
end

local rows = {
  { 'version older than buffer', ',"version":3', false, 'unchanged' },
  { 'version equal to buffer',   ',"version":5', true,  'edited' },
  { 'version null',              ',"version":null', true, 'edited' },
  -- An absent `version` raises before any text is applied, so the buffer is untouched.
  { 'version absent',            '', false, 'unchanged' },
}

local header = ('\n[edit-version] buffer version = %d, uri = %s'):format(BUFFER_VERSION, uri)
local broken = 0
local report = {}
for _, r in ipairs(rows) do
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, { 'local x = 1' })
  M.buf_versions[buf] = BUFFER_VERSION
  local ok, line, err = apply(r[2])
  local edited = line ~= 'local x = 1'
  local matches_doc = edited == r[3]
  if not matches_doc then broken = broken + 1 end
  report[#report + 1] = ('  %-26s applied=%-5s expect=%-5s %s  %s'):format(
    r[1], tostring(edited), tostring(r[3]), matches_doc and 'ok' or 'CHANGED', err)
end

-- Bare `changes` map: no documentChanges, therefore no version check at all.
vim.api.nvim_buf_set_lines(buf, 0, -1, false, { 'local x = 1' })
M.buf_versions[buf] = BUFFER_VERSION
local bare = vim.json.decode(string.format(
  '{"changes":{"%s":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},'
    .. '"newText":"-- bare\\n"}]}}', uri))
local ok = pcall(M.apply_workspace_edit, bare, 'utf-8')
local bare_applied = vim.api.nvim_buf_get_lines(buf, 0, -1, false)[1] == '-- bare'
report[#report + 1] = ('  %-26s applied=%-5s expect=true  %s'):format(
  'bare changes map', tostring(bare_applied), bare_applied and 'ok' or 'CHANGED')
if not bare_applied then broken = broken + 1 end

-- Emitted as one chunk after every application so the client's own
-- "newer than edits." message (which it prints without a trailing newline)
-- cannot interleave into the table.
report[#report + 1] = ('[edit-version] %d expectation(s) changed'):format(broken)
print(table.concat(vim.list_extend({ header }, report), '\n'))

os.exit(broken == 0 and 0 or 1)
