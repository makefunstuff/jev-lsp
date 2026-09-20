-- Evidence for docs/LANGUAGE.md — support is unconditional, language is metadata.
--
-- The product requirement this probe locks down: every file buffer is served, including
-- files Neovim cannot identify. The model needs no grammar and no filetype, so nothing
-- about language may gate attachment, sync, or the verb set.
--
-- Measured here, against the real client:
--
--   1. Neovim fires `FileType` only for buffers whose filetype was actually detected, and
--      `vim.lsp.enable` attaches *only* on `FileType`. With `filetypes = nil` an
--      unidentified file is therefore never attached — the gap the plugin must close.
--   2. After the plugin's attach pass, EVERY normal file buffer is attached and synced:
--      unknown extension, no extension, prose, logs, data.
--   3. `get_language_id` supplies Neovim's own richer detection, so a buffer the built-in
--      path never touched still arrives with a correct language.
--   4. The hook is pure: it does not mutate the buffer's filetype.
--
-- Exits nonzero on any failure.

-- Deliberately a fixed path, and the one probe that leaves files behind: the fixtures *are*
-- the evidence. The table printed below says what `languageId` each of these files got, and a
-- reader can open them and check it. Nothing here is a repository marker — no `.git`, so the
-- directory cannot become anybody's workspace root, which is the hazard the harnesses' fixture
-- roots are about (`verify/fixture.lua`) — and the eleven names are fixed, so a second run
-- overwrites rather than accumulates.
local DIR = '/tmp/jev-lang-fixtures'

local failures = {}
local function check(cond, label)
  if cond then
    print('  ok    ' .. label)
  else
    print('  FAIL  ' .. label)
    failures[#failures + 1] = label
  end
end

vim.cmd('filetype on')
vim.fn.mkdir(DIR, 'p')

-- `expect` is what Neovim's detector says, which is what the client should send.
local fixtures = {
  { 'f.rs',         { 'fn main() {}' },                          'rust' },
  { 'f.lua',        { 'local x = 1' },                           'lua' },
  { 'f.py',         { 'import os' },                             'python' },
  { 'Makefile',     { 'all:', '\techo hi' },                     'make' },
  { 'Cargo.toml',   { '[package]' },                             'toml' },
  { 'data.json',    { '{"a": 1}' },                              'json' },
  { 'notes.md',     { '# Title' },                               'markdown' },
  { 'script-noext', { '#!/usr/bin/env python3', 'import os' },   'python' },
  -- Deliberately unidentifiable: never attached by the built-in path, still supported.
  { 'f.zzz',        { 'mystery' },                               '' },
  { 'data.log',     { '2026-09-18 INFO started' },               '' },
  { 'plain',        { 'nothing detectable here' },               '' },
}
for _, f in ipairs(fixtures) do
  vim.fn.writefile(f[2], DIR .. '/' .. f[1])
end

local ft_events = {}
vim.api.nvim_create_autocmd('FileType', {
  callback = function(ev)
    -- Keyed by buffer, not by name: the name Neovim reports is the *resolved* path, and on
    -- macOS `$TMPDIR` is a symlink (`/var/folders/…` → `/private/var/folders/…`), so a
    -- name-keyed lookup never matched and the check below reported a gap that was not there.
    ft_events[ev.buf] = vim.bo[ev.buf].filetype
  end,
})

local here = debug.getinfo(1, 'S').source:sub(2)
local server = vim.fn.fnamemodify(here, ':h') .. '/language/server.py'

-- The plugin's language hook. Pure (filename+contents, never `buf`), and free in the
-- common case (no detector call when the client already has a filetype).
local cfg = {
  name = 'jev-lang',
  cmd = { 'python3', server },
  root_dir = DIR,
  get_language_id = function(bufnr, ft)
    if ft ~= nil and ft ~= '' then
      return ft
    end
    return vim.filetype.match({
      filename = vim.api.nvim_buf_get_name(bufnr),
      contents = vim.api.nvim_buf_get_lines(bufnr, 0, 200, false),
    }) or ''
  end,
}
vim.lsp.config('jev-lang', cfg)
vim.lsp.enable('jev-lang')   -- no `filetypes` => documented as ALL filetypes

print('[language] built-in auto-attach path (FileType only)')
local bufs = {}
for _, f in ipairs(fixtures) do
  local path = DIR .. '/' .. f[1]
  vim.cmd('edit ' .. vim.fn.fnameescape(path))
  bufs[f[1]] = { path = path, bufnr = vim.fn.bufnr(path), expect = f[3] }
end
vim.wait(500, function() return false end)

local missed = {}
for _, f in ipairs(fixtures) do
  local b = bufs[f[1]]
  b.attached = #vim.lsp.get_clients({ bufnr = b.bufnr, name = 'jev-lang' }) > 0
  if not b.attached then
    missed[#missed + 1] = f[1]
  end
end
print('  FileType fired for : ' .. #vim.tbl_keys(ft_events) .. ' of ' .. #fixtures)
print('  auto-attached      : ' .. (#fixtures - #missed) .. ' of ' .. #fixtures ..
  '   missed: ' .. table.concat(missed, ', '))
check(#missed > 0, 'the built-in path leaves unidentified files unattached (gap is real)')
check(vim.tbl_contains(missed, 'f.zzz') and vim.tbl_contains(missed, 'plain'),
  'the missed set is the unidentifiable set')
-- A shebang file opened normally IS detected (so the built-in path attached it); the
-- rename/attach case below is what exercises the hook.
check(ft_events[bufs['script-noext'].bufnr] ~= nil,
  'a shebang-only file is detected on a normal open, so the built-in path covers it')

-- The plugin's attach pass (BufReadPost/BufNewFile/BufWinEnter in the real plugin).
print('[language] plugin attach pass (universal support)')
for _, f in ipairs(fixtures) do
  local b = bufs[f[1]]
  if not b.attached then
    vim.lsp.start(cfg, { bufnr = b.bufnr })
  end
end

-- The decisive case: a buffer Neovim never identified, attached explicitly, whose language
-- is nonetheless recovered from its contents.
local renamed = vim.api.nvim_create_buf(true, false)
vim.api.nvim_buf_set_lines(renamed, 0, -1, false, { '#!/usr/bin/env python3', 'import os' })
vim.api.nvim_buf_set_name(renamed, DIR .. '/renamed-noext')
local renamed_ft_before = vim.bo[renamed].filetype
vim.lsp.start(cfg, { bufnr = renamed })

local all_attached = vim.wait(5000, function()
  if #vim.lsp.get_clients({ bufnr = renamed, name = 'jev-lang' }) == 0 then
    return false
  end
  for _, f in ipairs(fixtures) do
    if #vim.lsp.get_clients({ bufnr = bufs[f[1]].bufnr, name = 'jev-lang' }) == 0 then
      return false
    end
  end
  return true
end, 25)
check(all_attached, 'every file buffer is attached, including unidentified ones')

vim.wait(800, function() return false end)   -- let the didOpen notifications flush

local client = vim.lsp.get_clients({ name = 'jev-lang' })[1]
assert(client, 'no attached client to query')
local out = { done = false }
client:request('workspace/executeCommand', { command = 'probe.languageIds', arguments = {} },
  function(err, res) out.err, out.res, out.done = err, res, true end)
assert(vim.wait(5000, function() return out.done end, 25), 'no response to probe.languageIds')
assert(not out.err, 'probe.languageIds errored: ' .. vim.inspect(out.err))
local seen = out.res.languageIds or {}

print('[language] languageId per fixture (metadata, never a gate)')
local mismatched = {}
for _, f in ipairs(fixtures) do
  local got = seen[vim.uri_from_bufnr(bufs[f[1]].bufnr)]
  local ok = got == f[3]
  if not ok then mismatched[#mismatched + 1] = f[1] end
  print(('  %-16s languageId=%-10s expected=%-10s %s'):format(
    f[1], string.format('%q', tostring(got)), string.format('%q', f[3]), ok and 'ok' or 'MISMATCH'))
end
check(#mismatched == 0, 'languageId matches Neovim detection for every fixture')

check(seen[vim.uri_from_bufnr(bufs['plain'].bufnr)] ~= nil,
  'a file with no detectable language is still synced')
check(seen[vim.uri_from_bufnr(renamed)] == 'python',
  'the hook recovered `python` for a buffer the built-in path never identified (got ' ..
  string.format('%q', tostring(seen[vim.uri_from_bufnr(renamed)])) .. ')')
check(vim.bo[renamed].filetype == renamed_ft_before,
  'the language hook did not mutate buffer state')

print(('[language] %d assertion(s) failed'):format(#failures))
client:stop(true)
vim.wait(300, function() return false end)
os.exit(#failures == 0 and 0 or 1)
