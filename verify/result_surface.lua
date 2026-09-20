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
--   4. a generated buffer is not a document: with the report in the window the user is in,
--      `:Jev inspect [--force]`, `:Jev explain`, `:Jev review`, `:Jev plan`, `:Jev followup` and
--      `:Jev where` refuse by name and send *nothing* to the server — while `:Jev ask` still
--      asks, because a question about nothing is a question it answers, and carries no document
--      rather than the report's name as one;
--   5. `:Jev explain` — a streamed answer opens its surface in the same window *before* the
--      answer exists, and the finished artifact lands in that same buffer;
--   6. `surfaces.layout = 'float'` and `'split'` — the two opt-in layouts: the float keeps the
--      code visible behind it and costs a window while it is open, the split is the old shape;
--   7. the same as 1 with `'hidden'` off and the buffer modified: the reason the placement asks
--      with `:hide buffer` instead of `nvim_win_set_buf` — and the unsaved buffer survives it;
--   8. a command that cannot go out: a send the client refuses says why, and takes back the
--      surface it had already opened for the answer; a server that is gone is answered the same
--      way. Never a silent no-op leaving an empty window behind.
--
-- Prints ok/FAIL/SKIP per check. Exit is nonzero only on FAIL; exit 2 when JEV_LSP_BIN is
-- unset. Every wait is bounded, so a run cannot hang. Check 5 needs a chat model: with no
-- reachable endpoint it is a SKIP with the reason, never a FAIL and never an ok.

local BIN = os.getenv('JEV_LSP_BIN')
if BIN == nil or BIN == '' then
  io.stderr:write(
    'result_surface: JEV_LSP_BIN is not set and is required (path to the jev-lsp server).\n'
      .. '  usage: JEV_LSP_BIN=/path/to/jev-lsp nvim --headless -u NONE -l verify/result_surface.lua\n'
  )
  io.stderr:flush()
--
-- `JEV_ROOT` names the fixture workspace; without it the harness makes one with
-- `vim.fn.tempname()` and **removes it again on the way out** (green, red, or skipped),
-- so `/tmp` does not fill up with repository markers. Name one when a failure needs
-- reading afterwards — a root the caller named is left exactly where it is.
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
-- this harness is about where the report goes, not about what is in it. A root this harness
-- created is removed on the way out; a `JEV_ROOT` the caller named is left where it is.
local fixture_root = dofile(vim.fn.fnamemodify(here, ':p:h') .. '/fixture.lua')
local root, owned_root = fixture_root.root('JEV_ROOT', '-jev-surface')
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

-- 4. A generated buffer is not a document ------------------------------------------------------

-- The report is in the window the user is sitting in, so running the same command again is one
-- keystroke away — and the buffer's name is `jev://inspect/inspect`, not a path. Every command
-- that sends a document has to refuse it by name, before it prompts for free text, and without
-- reaching the server with a URI it would answer `bad_arguments` about.
--
-- `:Jev ask` is the documented exception — a question about nothing is a question it answers
-- ("with none the question stands alone") — so what is asserted there is the difference: the
-- request still goes out, and it carries no document rather than the report's name as one.
do
  local wins = window_count()
  local reported = nil
  dispatch_watching_windows(function()
    pcall(vim.cmd, 'Jev inspect')
  end, function()
    reported = artifact('inspect')
    return reported ~= nil
  end, 15000)

  if reported == nil then
    skip('a generated buffer is not a document', 'no report was open to run the commands from')
  else
    local text = artifact_text(reported)
    local client = vim.lsp.get_clients({ name = 'jev' })[1]

    -- Every request the commands below initiate, observed at the client boundary rather than
    -- assumed from the absence of a failure. `nvim_ui_test.lua` stubs the same method. The
    -- `ask` request is recorded and not forwarded: this section is about what goes out, and a
    -- model answer would open a buffer the next section would have to tidy up.
    local sent = {}
    local asked = nil
    local real_request = client.request
    client.request = function(self, method, params, ...)
      sent[#sent + 1] = { method = method, params = params }
      if type(params) == 'table' and params.command == 'jev.ask' then
        asked = params
        return 1
      end
      return real_request(self, method, params, ...)
    end

    --- Run one command with the report on screen and say what it did.
    local function refusal(label, dispatch)
      local messages, levels = {}, {}
      local real_notify = vim.notify
      vim.notify = function(msg, level, ...)
        messages[#messages + 1] = tostring(msg)
        levels[#levels + 1] = level
        return real_notify(msg, level, ...)
      end
      local prompted = false
      local real_input = vim.ui.input
      vim.ui.input = function(_, cb)
        -- If a command reaches its prompt, the guard is after it: answer, so the defect shows
        -- itself as a request that goes out rather than as a harness that hangs.
        prompted = true
        if cb then
          cb('harness')
        end
      end
      local before = #sent
      local dispatched, err = pcall(dispatch)
      vim.wait(300, function()
        return false
      end, 50)
      vim.ui.input = real_input
      vim.notify = real_notify

      local said = table.concat(messages, ' ')
      check(dispatched, label .. ' dispatches without raising', err)
      check(
        said:find('needs a file buffer', 1, true) ~= nil
          and said:find('generated buffer (jev://inspect/inspect)', 1, true) ~= nil
          and levels[1] == vim.log.levels.WARN,
        label .. ' refuses by name, at warn level',
        said == '' and 'said nothing' or said
      )
      check(
        #sent == before,
        label .. ' sends nothing to the server',
        ('%d request(s): %s'):format(#sent - before, vim.inspect(sent[#sent] and sent[#sent].method))
      )
      check(not prompted, label .. ' does not prompt for free text it cannot use')
      check(window_count() == wins, label .. ' leaves the window count alone', window_count())
      check(
        artifact('inspect') == reported and artifact_text(reported) == text,
        label .. ' leaves the report as it was'
      )
    end

    refusal(':Jev inspect', function()
      vim.cmd('Jev inspect')
    end)
    refusal(':Jev inspect --force', function()
      vim.cmd('Jev inspect --force')
    end)
    refusal(':Jev explain', function()
      vim.cmd('Jev explain')
    end)
    refusal(':Jev review', function()
      vim.cmd('Jev review')
    end)
    refusal(':Jev plan', function()
      vim.cmd('Jev plan')
    end)
    refusal(':Jev followup', function()
      vim.cmd('Jev followup')
    end)
    refusal(':Jev where', function()
      vim.cmd('Jev where')
    end)

    local before = #sent
    pcall(vim.cmd, 'Jev ask what does the loader do')
    local left = vim.wait(2000, function()
      return asked ~= nil
    end, 25)
    check(
      left and #sent > before and asked ~= nil,
      ':Jev ask still asks: a question about nothing is a question it answers',
      vim.inspect(sent[#sent] and sent[#sent].method)
    )
    check(
      asked ~= nil and type(asked.arguments) == 'table' and asked.arguments[1].uri == nil,
      'and it carries no document, rather than the report name as one',
      asked == nil and 'nothing was sent' or vim.inspect(asked.arguments)
    )
    check(
      artifact('inspect') == reported,
      'and asking leaves the report where it was'
    )

    client.request = real_request
    dismiss('a generated buffer is not a document', wins)
  end
end

-- 5. A streamed answer ------------------------------------------------------------------------

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

-- 6. The opt-in layouts -----------------------------------------------------------------------

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

-- 7. An unsaved buffer ------------------------------------------------------------------------

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

-- 8. A command that cannot go out --------------------------------------------------------------

-- `Client:request` returns false when the send itself fails — the server stopped between
-- `client()` and the request — and `M.command` used to answer that by doing nothing at all: no
-- message, and with `stream = true` the surface it had just opened for the answer stayed empty and
-- open. Two ways in, one observable outcome: the command says something, and nothing empty is left
-- behind.
do
  local wins = window_count()
  local bufs = #vim.api.nvim_list_bufs()

  vim.api.nvim_set_current_buf(code_bufnr)

  -- `a` — the send itself fails, provoked at the boundary `Client:request` documents by stubbing
  -- it, the way `nvim_ui_test.lua` stubs a request that never answers. A real kill cannot stand in
  -- for this: in the tick after one, the client has either already left `get_clients` (so the
  -- command never gets as far as opening a surface) or still accepts the write, and the branch
  -- under test is the client that is still there and refuses.
  local client = vim.lsp.get_clients({ bufnr = code_bufnr, name = 'jev' })[1]
  local real_request = client.request
  client.request = function()
    return false, 'the server is not taking requests'
  end
  local messages, levels = {}, {}
  local real_notify = vim.notify
  vim.notify = function(msg, level, ...)
    messages[#messages + 1] = tostring(msg)
    levels[#levels + 1] = level
    return real_notify(msg, level, ...)
  end
  local answered = false
  local request_id = require('jev').command('jev.status', {}, function()
    answered = true
  end, { stream = true })
  client.request = real_request
  vim.notify = real_notify

  local said = table.concat(messages, ' ')
  check(
    said:find('was not sent', 1, true) ~= nil
      and said:find('the server is not taking requests', 1, true) ~= nil
      and levels[1] == vim.log.levels.WARN,
    'a send that fails says so, with the reason it failed',
    said == '' and 'said nothing' or said
  )
  check(request_id == nil, 'and asks for no request id it does not have', vim.inspect(request_id))
  check(not answered, 'and calls back for nothing, because no answer is coming')
  check(
    vim.api.nvim_get_current_buf() == code_bufnr,
    'the surface opened for the answer is taken back',
    vim.api.nvim_buf_get_name(vim.api.nvim_get_current_buf())
  )
  check(window_count() == wins, 'with the window count as it was', window_count())
  check(
    #vim.api.nvim_list_bufs() == bufs,
    'and no empty buffer left holding it',
    ('%d buffer(s) -> %d'):format(bufs, #vim.api.nvim_list_bufs())
  )

  -- `b` — the server is gone for real. The client this plugin talks to is stopped, so the command
  -- cannot open a surface for an answer nobody will fill.
  for _, c in ipairs(vim.lsp.get_clients({ name = 'jev' })) do
    c:stop(true)
  end
  vim.wait(5000, function()
    return #vim.lsp.get_clients({ name = 'jev' }) == 0
  end, 25)

  vim.api.nvim_set_current_buf(code_bufnr)
  local stopped_said = {}
  local real_notify_stopped = vim.notify
  vim.notify = function(msg, level, ...)
    stopped_said[#stopped_said + 1] = tostring(msg)
    return real_notify_stopped(msg, level, ...)
  end
  require('jev').command('jev.explain', { {} }, function() end, { stream = true })
  vim.notify = real_notify_stopped

  check(
    #stopped_said > 0,
    'with the server stopped, a streaming command says something instead of nothing',
    vim.inspect(stopped_said)
  )
  check(
    vim.api.nvim_get_current_buf() == code_bufnr and window_count() == wins,
    'and leaves no surface open for an answer that cannot come',
    ('current=%s wins=%d'):format(vim.api.nvim_buf_get_name(vim.api.nvim_get_current_buf()), window_count())
  )
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
-- The root this harness created, gone on the way out — green, red, or after a skip.
fixture_root.remove(root, owned_root)
say(('[surface] %d failure(s), %d skip(s)'):format(failures, skips))
os.exit(failures == 0 and 0 or 1)
