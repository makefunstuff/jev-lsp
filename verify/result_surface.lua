-- Live Neovim: showing a result must not move the user's windows.
--
--   JEV_LSP_BIN=/path/to/jev-lsp nvim --headless -u NONE -l verify/result_surface.lua
--
-- `:Jev inspect` used to open its report with `sbuffer`, which is a *split*: asking about a
-- file halved the window the user was reading. Every other command that shows a buffer did the
-- same thing through the same call (`explain`, `ask`, `followup`, `where`, `usage`, `session`,
-- `plan`), and the fix is one placement for all of them (`nvim/lua/jev/init.lua`,
-- `place_surface`). This is the harness for that claim, so it asserts what the user feels:
--
--   1. `:Jev inspect` — the window count does not change *across the command*, not merely
--      before and after it (a split that closes itself would pass a before/after check); the
--      report is the surface the user is in; and the code buffer is still loaded, still
--      byte-for-byte, still unmodified, and still the alternate buffer;
--   2. a second report opened over the first gives the *code* buffer back, not the dead one
--      underneath it, and `q` wipes the artifact it dismisses;
--   3. `:Jev status` — a one-line answer is a message, and opens no buffer and no window;
--   4. `:Jev explain` — a streamed answer opens its surface in the same window *before* the
--      answer exists, and the finished artifact lands in that same buffer;
--   5. `surfaces.layout = 'float'` — an opt-in floating window: the code stays visible behind
--      it, and dismissing closes it;
--   6. `surfaces.layout = 'split'` — the old shape, still available on request;
--   7. the same as 1 with `'hidden'` off and the buffer modified: the reason the placement asks
--      with `:hide buffer` instead of `nvim_win_set_buf` — and the unsaved buffer survives it.
--
-- Prints ok/FAIL/SKIP per check. Exit is nonzero only on FAIL; exit 2 when JEV_LSP_BIN is
-- unset. Every wait is bounded, so a run cannot hang. Checks 4 needs a chat model: with no
-- reachable endpoint it is a SKIP with the reason, never a FAIL and never an ok.

local BIN = os.getenv('JEV_LSP_BIN')
if BIN == nil or BIN == '' then
  io.stderr:write(
    'result_surface: JEV_LSP_BIN is not set and is required (path to the jev-lsp server).\n'
      .. '  usage: JEV_LSP_BIN=/path/to/jev-lsp nvim --headless -u NONE -l verify/result_surface.lua\n'
  )
  io.stderr:flush()
  os.exit(2)
end

local failures, skips = 0, 0

-- Explicit newlines, not print(): Neovim's own message output (vim.notify, the plugin's
-- failures) can flush without a trailing newline and would run into the next check.
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

local function skip(label, detail)
  skips = skips + 1
  say('SKIP  ' .. label .. (detail ~= nil and ('  — ' .. tostring(detail)) or ''))
end

-- Setup ---------------------------------------------------------------------------------------

local here = debug.getinfo(1, 'S').source:sub(2)
local PLUGIN = vim.fn.fnamemodify(here, ':p:h:h') .. '/nvim'
vim.opt.runtimepath:prepend(PLUGIN)

-- Surface what the server logged if anything fails: a dead endpoint looks exactly like a
-- product defect from this side of the connection.
local server_log = dofile(vim.fn.fnamemodify(here, ':p:h') .. '/harness_log.lua')

-- A repository root: the plugin keys the session record and dismissals on `.git`. No
-- `.jev/rules/`, so `jev.inspect` needs no decision tier and answers the same way every run —
-- this harness is about where the report goes, not about what is in it.
local root = vim.fn.tempname() .. '-jev-surface'
vim.fn.mkdir(root .. '/.git', 'p')
local src = root .. '/surface.py'
vim.fn.writefile({
  'def load(path):',
  '    handle = open(path)',
  '    return handle.read()',
}, src)

require('jev').setup({ cmd = { BIN } })

say('[surface] plugin  : ' .. PLUGIN)
say('[surface] server  : ' .. BIN)
say('[surface] fixture : ' .. src)

vim.cmd('edit ' .. vim.fn.fnameescape(src))
local code_bufnr = vim.api.nvim_get_current_buf()
local code_lines = vim.api.nvim_buf_get_lines(code_bufnr, 0, -1, true)

local attached = vim.wait(10000, function()
  return #vim.lsp.get_clients({ bufnr = code_bufnr, name = 'jev' }) > 0
end, 25)
check(attached, 'the plugin attached a client to the fixture')

server_log.capture()

local function window_count()
  return #vim.api.nvim_list_wins()
end

--- The generated buffer of a kind, if it is on screen — the name is the artifact's
--- (`jev://<kind>/<id>`), which is the same shape `rules_live.lua` reads.
local function artifact(kind)
  for _, b in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_valid(b)
      and vim.api.nvim_buf_get_name(b):find('jev://' .. kind, 1, true) == 1
    then
      return b
    end
  end
  return nil
end

local function artifact_text(bufnr)
  return table.concat(vim.api.nvim_buf_get_lines(bufnr, 0, -1, false), '\n')
end

--- The window count *across* a command, not at its end.
---
--- This is the whole claim: a layout that split and then put itself back would pass a
--- before/after comparison and still be the thing the user complained about. So the count is
--- sampled on every poll for the duration, and every sample has to be the count it started at.
--- @return integer[] the distinct counts seen, ascending
local function dispatch_watching_windows(dispatch, done, timeout)
  local seen = {}
  local function sample()
    local n = window_count()
    seen[n] = (seen[n] or 0) + 1
  end
  sample()
  dispatch()
  vim.wait(timeout, function()
    sample()
    return done()
  end, 20)
  sample()
  local counts = {}
  for n in pairs(seen) do
    counts[#counts + 1] = n
  end
  table.sort(counts)
  return counts
end

--- Dismiss the surface the user is on, the way the user does.
---
--- Through the mapping's own callback, looked up the way `:map` would find it: `feedkeys` in a
--- headless session does not reliably run buffer-local mappings (`nvim_ui_test.lua` §15).
local function dismiss(label, wins)
  local mapped = vim.fn.maparg('q', 'n', false, true)
  check(
    type(mapped) == 'table' and type(mapped.callback) == 'function',
    label .. ': the surface maps q to something',
    vim.inspect(mapped and (mapped.desc or mapped.rhs))
  )
  if type(mapped) == 'table' and type(mapped.callback) == 'function' then
    mapped.callback()
  end
  vim.wait(2000, function()
    return vim.api.nvim_get_current_buf() == code_bufnr
  end, 20)
  check(
    vim.api.nvim_win_get_buf(0) == code_bufnr,
    label .. ': q puts the buffer the user was in back on screen'
  )
  check(
    window_count() == wins,
    label .. ': and leaves the window count where it found it',
    window_count()
  )
end

--- Everything about the code buffer that must survive a report about it.
local function code_survived(label)
  check(vim.api.nvim_buf_is_valid(code_bufnr), label .. ': the code buffer is still loaded')
  check(
    vim.deep_equal(vim.api.nvim_buf_get_lines(code_bufnr, 0, -1, true), code_lines),
    label .. ': and byte-for-byte what it was'
  )
  check(not vim.bo[code_bufnr].modified, label .. ': and not marked modified')
  check(
    vim.fn.bufnr('#') == code_bufnr,
    label .. ': and is the alternate buffer, so <C-^> returns to it',
    vim.fn.bufnr('#')
  )
end

-- 1. `:Jev inspect` — the complaint -----------------------------------------------------------

check(window_count() == 1, 'the fixture is open in one window', window_count())

do
  local wins = window_count()
  local reported = nil
  local counts = dispatch_watching_windows(function()
    local dispatched, err = pcall(vim.cmd, 'Jev inspect')
    check(dispatched, ':Jev inspect dispatches without raising', err)
  end, function()
    reported = artifact('inspect')
    return reported ~= nil
  end, 15000)

  check(
    #counts == 1 and counts[1] == wins,
    ':Jev inspect never changes the window count across the command',
    ('counts seen: %s (started at %d)'):format(vim.inspect(counts), wins)
  )
  if check(reported ~= nil, ':Jev inspect opens its report', 'no jev://inspect buffer') then
    check(
      vim.api.nvim_get_current_buf() == reported and vim.api.nvim_win_get_buf(0) == reported,
      'and the report is the surface the user is in, in the window they were in'
    )
    check(
      vim.bo[reported].buftype == 'nofile'
        and vim.bo[reported].bufhidden == 'wipe'
        and vim.bo[reported].buflisted == false
        and vim.bo[reported].modifiable == false,
      'the report is scratch: nofile, unlisted, unmodifiable, wiped on dismissal',
      ('buftype=%q bufhidden=%q buflisted=%s modifiable=%s'):format(
        vim.bo[reported].buftype, vim.bo[reported].bufhidden,
        tostring(vim.bo[reported].buflisted), tostring(vim.bo[reported].modifiable))
    )
    local text = artifact_text(reported)
    check(
      text:find('inspect: surface.py', 1, true) ~= nil
        and text:find('skipped', 1, true) ~= nil,
      'and it is the report itself, with the counts and every skip in it',
      text:sub(1, 120)
    )
    code_survived(':Jev inspect')
  end
  dismiss(':Jev inspect', wins)
end

-- 2. A second report over the first -----------------------------------------------------------

-- `:Jev usage` does not read the current buffer, so it is the honest way to open a report
-- while a report is on screen. The one underneath is wiped as it leaves (`bufhidden`), and the
-- dismiss of the one on top must give back the *code*, not that dead buffer.
do
  local wins = window_count()
  local counts = dispatch_watching_windows(function()
    pcall(vim.cmd, 'Jev inspect')
  end, function()
    return artifact('inspect') ~= nil
  end, 15000)
  check(
    #counts == 1 and counts[1] == wins,
    'a report opens without moving the window count',
    vim.inspect(counts)
  )
  local first = artifact('inspect')
  if first == nil then
    skip('a second report over the first', 'the first report never opened')
  else
    local over = dispatch_watching_windows(function()
      pcall(vim.cmd, 'Jev usage')
    end, function()
      return artifact('usage') ~= nil
    end, 15000)
    check(
      #over == 1 and over[1] == wins,
      'and so does a second one over it',
      vim.inspect(over)
    )
    check(
      vim.api.nvim_get_current_buf() == artifact('usage'),
      'the second report is the surface the user is in'
    )
    check(not vim.api.nvim_buf_is_valid(first), 'the first is wiped as it is left behind')
    code_survived('the second report')
    dismiss('the second report', wins)
  end
end

-- 3. A one-line answer is a message, not a buffer ----------------------------------------------

do
  local wins = window_count()
  local messages = {}
  local real_notify = vim.notify
  vim.notify = function(msg, ...)
    messages[#messages + 1] = tostring(msg)
    return real_notify(msg, ...)
  end
  local counts = dispatch_watching_windows(function()
    pcall(vim.cmd, 'Jev status')
  end, function()
    return #messages > 0
  end, 15000)
  vim.notify = real_notify
  check(#counts == 1 and counts[1] == wins, ':Jev status opens no window', vim.inspect(counts))
  check(
    #messages > 0,
    'and says what it has to say as a message',
    'nothing was notified'
  )
  check(
    artifact('status') == nil,
    'and does not spend a buffer on one line',
    vim.inspect(messages[1])
  )
end

-- 4. A streamed answer ------------------------------------------------------------------------

-- `jev.explain` asks for a stream: the surface exists before the answer does
-- (`stream_open`), which is the other placement path. The finished artifact has to land in
-- that same buffer, so nothing moves twice.
do
  local base = os.getenv('JEV_BASE_URL') or ''
  local origin = base:match('^(https?://[^/]+)')
  local live, reason = false, nil
  if origin == nil then
    reason = 'JEV_BASE_URL is unset or is not an http(s) URL, so no model endpoint was promised'
  elseif vim.fn.executable('curl') ~= 1 then
    reason = 'curl is not installed, so the model endpoint cannot be probed'
  else
    local probe = vim
      .system({ 'curl', '-fsS', '-o', '/dev/null', '--max-time', '2', origin .. '/health' }, { text = true })
      :wait()
    if probe.code == 0 then
      live = true
    else
      reason = ('%s/health did not answer (curl exit %s)'):format(origin, tostring(probe.code))
    end
  end

  if not live then
    skip('a streamed answer in the same window', reason)
    skip('the finished answer landing in the surface that was already open', reason)
  else
    local wins = window_count()
    local streamed = nil
    local counts = dispatch_watching_windows(function()
      vim.api.nvim_set_current_buf(code_bufnr)
      require('jev').explain()
      -- `explain` opens the surface synchronously, before the model has been asked anything:
      -- that is the moment the layout must already be right.
      streamed = vim.api.nvim_get_current_buf()
    end, function()
      return artifact('explanation') ~= nil
    end, 30000)

    check(
      streamed ~= nil and streamed ~= code_bufnr and vim.api.nvim_buf_is_valid(streamed),
      'the streamed answer opens its surface as soon as the request goes out',
      'the code buffer was still current'
    )
    check(
      #counts == 1 and counts[1] == wins,
      ':Jev explain never changes the window count across the command',
      ('counts seen: %s (started at %d)'):format(vim.inspect(counts), wins)
    )
    local finished = artifact('explanation')
    check(
      finished ~= nil and finished == streamed,
      'the finished answer lands in the buffer that was already open',
      ('streamed=%s finished=%s'):format(tostring(streamed), tostring(finished))
    )
    if finished ~= nil then
      check(
        artifact_text(finished):find('%S') ~= nil,
        'and the answer is in it',
        artifact_text(finished):sub(1, 80)
      )
    end
    code_survived(':Jev explain')
    dismiss(':Jev explain', wins)
  end
end

-- 5. The opt-in layouts -----------------------------------------------------------------------

-- `surfaces.layout` is read when a surface opens, so these flip it directly rather than
-- calling `setup` again: a second `setup` would install a second `LspAttach` for the lenses and
-- the declaration push, which is a different thing from what is under test.
do
  local wins = window_count()

  require('jev').opts.surfaces.layout = 'float'
  local floated = nil
  dispatch_watching_windows(function()
    pcall(vim.cmd, 'Jev inspect')
  end, function()
    floated = artifact('inspect')
    return floated ~= nil
  end, 15000)
  if check(floated ~= nil, 'surfaces.layout = "float": the report still opens') then
    local win = vim.api.nvim_get_current_win()
    check(
      vim.api.nvim_win_get_config(win).relative ~= '',
      'and it is a floating window'
    )
    check(
      vim.api.nvim_win_get_buf(win) == floated,
      'and it is the surface the user is in'
    )
    check(
      window_count() == wins + 1,
      'a float is a window while it is open, which is what it costs',
      window_count()
    )
    check(
      #vim.fn.win_findbuf(code_bufnr) == 1,
      'and the code stays on screen behind it'
    )
    dismiss('surfaces.layout = "float"', wins)
    check(not vim.api.nvim_buf_is_valid(floated), 'the float took its buffer with it')
  end

  require('jev').opts.surfaces.layout = 'split'
  local split_buf = nil
  dispatch_watching_windows(function()
    pcall(vim.cmd, 'Jev inspect')
  end, function()
    split_buf = artifact('inspect')
    return split_buf ~= nil
  end, 15000)
  if check(split_buf ~= nil, 'surfaces.layout = "split": the report still opens') then
    check(
      vim.api.nvim_win_get_config(0).relative == '',
      'as a real split, for anyone who wants the code and the report side by side'
    )
    check(
      window_count() == wins + 1,
      'which is two windows while it is open',
      window_count()
    )
    check(
      #vim.fn.win_findbuf(code_bufnr) == 1,
      'with the code buffer still in its own window'
    )
    dismiss('surfaces.layout = "split"', wins)
  end

  require('jev').opts.surfaces.layout = 'current'
end

-- 6. An unsaved buffer ------------------------------------------------------------------------

-- `'hidden'` off and a modified buffer is the one case where the swap has to be asked for:
-- `nvim_win_set_buf` raises `E37` there and no report would appear at all, which is why the
-- placement uses `:hide buffer` (verified: 0.12.1 raises, `:hide` does not). Nothing of the
-- user's is lost either way — the buffer is hidden, not unloaded, and untouched.
do
  vim.o.hidden = false
  vim.api.nvim_buf_set_lines(code_bufnr, -1, -1, false, { '    # edited and not saved' })
  check(vim.bo[code_bufnr].modified, 'the fixture is modified with hidden off, for this check')

  local wins = window_count()
  local reported = nil
  local counts = dispatch_watching_windows(function()
    pcall(vim.cmd, 'Jev inspect')
  end, function()
    reported = artifact('inspect')
    return reported ~= nil
  end, 15000)
  check(
    reported ~= nil,
    'a report about a buffer with unsaved changes still opens'
  )
  check(
    #counts == 1 and counts[1] == wins,
    'without moving the window count',
    vim.inspect(counts)
  )
  check(
    vim.api.nvim_buf_is_valid(code_bufnr) and vim.bo[code_bufnr].modified,
    'and the unsaved buffer is still there, still modified'
  )
  dismiss('an unsaved buffer', wins)
  local kept = vim.api.nvim_buf_get_lines(code_bufnr, 0, -1, true)
  check(
    kept[#kept] == '    # edited and not saved',
    'and still holds the edit when the report is dismissed',
    tostring(kept[#kept])
  )
  vim.cmd('silent! undo')
  vim.bo[code_bufnr].modified = false
  vim.o.hidden = true
end

-- Report --------------------------------------------------------------------------------------

-- Lens state first, and a settle before the stop: Neovim's lens provider schedules a request
-- on a 200 ms debounce and asserts that the client still exists when it fires
-- (`lsp/codelens.lua:143`), so `enable(false)` does not let the client go the same tick.
pcall(vim.lsp.codelens.enable, false)
vim.wait(400, function()
  return false
end)
for _, c in ipairs(vim.lsp.get_clients({ name = 'jev' })) do
  c:stop(true)
end
vim.wait(300, function()
  return false
end)

if failures > 0 then
  server_log.dump()
end
say(('[surface] %d failure(s), %d skip(s)'):format(failures, skips))
os.exit(failures == 0 and 0 or 1)
