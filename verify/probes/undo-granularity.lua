-- Evidence for docs/research/nvim-lsp-surface.md §10 — marked **[U]**, deliberately.
--
-- Question: does a multi-edit WorkspaceEdit applied from a real RPC callback form a
-- single undo block, so one `u` reverts the whole agent edit?
--
-- This probe CANNOT answer it. Under `nvim --headless -l script.lua` no undo entries are
-- recorded at all: `undotree().seq_cur` does not advance after buffer writes and one
-- `undo` reverts to the initial empty buffer. Script-mode execution has no main-loop
-- undo sync, so the measurement is meaningless rather than negative.
--
-- It is kept because it documents the failure and prints the shape of the question. The
-- design does not depend on the answer: the plugin snapshots buffer content around every
-- applied edit and provides `:Jev undo` (docs/UX.md §3.4).
--
-- To settle it, run interactively:
--   1. start nvim, open a file, set undolevels to a sane value
--   2. from a real RPC callback (not a --lua/-l script), apply a 3-edit WorkspaceEdit
--   3. press `u` once and compare the buffer to the pre-edit text
-- Exit is always 0; this probe asserts nothing.

local function tick()
  vim.wait(20, function() return false end)
end

local buf = vim.api.nvim_create_buf(true, false)
vim.api.nvim_set_current_buf(buf)
-- A buffer *name*, not a file: nothing is written to this path, so the probe leaves nothing
-- in `/tmp`. The name exists because the stub needs a `file://` uri, and a fixed one keeps
-- the probe's output reproducible.
vim.api.nvim_buf_set_name(buf, '/tmp/jev-probe-undo.lua')
vim.api.nvim_buf_set_lines(buf, 0, -1, false, { 'a', 'b', 'c', 'd' })
vim.bo[buf].modified = false
tick()

local uri = vim.uri_from_bufnr(buf)
local seq_before = vim.fn.undotree().seq_cur

local edits = {}
for i, l in ipairs({ 0, 1, 2 }) do
  edits[#edits + 1] = string.format(
    '{"range":{"start":{"line":%d,"character":0},"end":{"line":%d,"character":0}},"newText":"X%d\\n"}',
    l, l, i)
end

local we = vim.json.decode(string.format(
  '{"documentChanges":[{"textDocument":{"uri":"%s","version":null},"edits":[%s]}]}',
  uri, table.concat(edits, ',')))

vim.lsp.util.apply_workspace_edit(we, 'utf-8')
tick()

print('[undo-granularity] undolevels = ' .. tostring(vim.o.undolevels))
print('  seq before      = ' .. seq_before .. '   (no advance means no undo block was recorded)')
print('  seq after       = ' .. vim.fn.undotree().seq_cur)
print('  applied         = ' .. vim.inspect(vim.api.nvim_buf_get_lines(buf, 0, -1, false)))

vim.cmd('silent undo')
print('  after one undo  = ' .. vim.inspect(vim.api.nvim_buf_get_lines(buf, 0, -1, false)))
print('  verdict         = INCONCLUSIVE under headless script mode; see header for the '
  .. 'interactive probe')
print('  design impact   = none; :Jev undo uses plugin-side snapshots')
