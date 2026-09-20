-- Live Neovim: the plugin's own interactive surfaces (docs/ROADMAP.md U4) — the picker, the
-- proposal diff, and the statusline segment.
--
--   JEV_LSP_BIN=/path/to/jev-lsp nvim --headless -u NONE -l verify/nvim_ui_test.lua
--   JEV_BASE_URL=http://127.0.0.1:8099/v1   # optional: a model endpoint (verify/stub_model.py)
--   JEV_DECIDE_BASE_URL=http://127.0.0.1:8099/v1 JEV_DECIDE_MODEL=stub-model
--     # ^ the decide tier the ambient rules pass asks; without it, no save-driven finding
--
-- JEV_LSP_BIN is the server binary (required; unset is a usage error, exit 2).
-- JEV_BASE_URL points the server at a model endpoint. With one, the checks that need a real
-- proposal are hard: the stub is deterministic. Without one the server still lists actions,
-- so those checks report SKIP rather than FAIL — nothing here pretends a missing endpoint is
-- a passing test. The ambient pass is the rules pass, so the fixture root carries a
-- `.jev/rules/example.json` whose decision is answered by JEV_DECIDE_BASE_URL — and a `.git/`,
-- which is what makes that root *this* directory rather than some ancestor of the temp dir.
--
-- Checks, in order:
--
--   0. the surface: exactly the four surviving keymaps are mapped (`<leader>ja` in visual
--      mode too, since a selection is the scope), the cut ones are gone, `:Jev status`
--      dispatches, and `jev.status` round-trips with the server reporting under the token the
--      plugin issued;
--   1. the picker opens and lists actions for a file with a findable issue, asks with
--      `triggerKind = 1` and the cursor range, resolves index 1, and the edit it resolves to
--      reaches the buffer byte-for-byte — exactly what the returned `TextEdit`s said;
--   2. `data.summary` reaches a picker that can render a second line, and the stock
--      `vim.ui.select` still runs with the same detail on one line;
--   3. a visual selection is what goes in the request, and the server scopes it
--      `scope_source = "explicit"` (PROTOCOL §4) rather than to the enclosing scope;
--   4. a rejected resolve — the file moved while the menu was open — changes nothing and
--      reports its reason exactly once, on whichever channel it arrives, and only for that
--      reason;
--   5. the diff preview opens one window, shows the post-edit text, leaves the buffer alone,
--      and rejecting restores the window count, the window sizes and the text byte-for-byte;
--      `<CR>` applies it;
--   6. `statusline.component()` is a string, empty with nothing in flight and with no client
--      attached, and shows the calls used once a poll reports pressure above half; the
--      expression `init.lua` documents renders the segment.
--
-- Prints ok/FAIL/SKIP per check. Exit is nonzero only on FAIL. Every wait is bounded:
-- `vim.wait` with an explicit timeout, or a deadline-checked poll loop, so the run cannot hang.

local BIN = os.getenv('JEV_LSP_BIN')
if BIN == nil or BIN == '' then
  io.stderr:write('nvim_ui_test: JEV_LSP_BIN is required (path to the jev-lsp binary)\n')
  io.stderr:flush()
--
-- `JEV_ROOT` names the fixture workspace; without it the harness makes one with
-- `vim.fn.tempname()` and **removes it again on the way out** (green, red, or skipped),
-- so `/tmp` does not fill up with repository markers. Name one when a failure needs
-- reading afterwards — a root the caller named is left exactly where it is.
  os.exit(2)
end
local MODEL = os.getenv('JEV_BASE_URL') or ''

--- Model calls so far, from the server's own counter.
local function calls()
    local status
    require('jev').command('jev.status', {}, function(_, r)
      status = r
    end)
    -- Polled gently: every one of these is a line in the shared session record, and a tight
    -- loop fills the last two hundred entries before the check that reads them runs.
    vim.wait(5000, function()
      return status ~= nil
    end, 200)
    return status and status.counters and status.counters.calls or -1
  end

local failures = 0
local skips = 0

--- `vim.ui.select` as this session loaded it: the stock `inputlist` picker. The preview-pane
--- check drives it directly, with `inputlist` stubbed, so the degraded rendering is exercised
--- rather than described.
local stock_select = vim.ui.select

-- Explicit newlines, not print(): Neovim's own message output (vim.notify) can flush without a
-- trailing newline and would otherwise run into the next check.
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

local function skip(label, detail)
  skips = skips + 1
  say('SKIP  ' .. label .. (detail ~= nil and ('  — ' .. tostring(detail)) or ''))
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

local function lines(bufnr)
  return vim.api.nvim_buf_get_lines(bufnr, 0, -1, true)
end

local function window_count()
  return #vim.api.nvim_list_wins()
end

--- Window sizes, so "exactly as they were" can mean the pixels and not just the count.
local function window_sizes()
  local out = {}
  for _, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
    out[#out + 1] = {
      win = win,
      width = vim.api.nvim_win_get_width(win),
      height = vim.api.nvim_win_get_height(win),
    }
  end
  return out
end

-- Setup ---------------------------------------------------------------------------------------

local here = debug.getinfo(1, 'S').source:sub(2)
local PLUGIN = vim.fn.fnamemodify(here, ':p:h:h') .. '/nvim'
vim.opt.runtimepath:prepend(PLUGIN)

-- Surface what the server logged if anything fails: a dead model endpoint looks exactly
-- like a product defect from this side of the connection.
local server_log = dofile(vim.fn.fnamemodify(here, ':p:h') .. '/harness_log.lua')
server_log.capture()

-- A root this harness created is removed on the way out; a `JEV_ROOT` the caller named is the
-- caller's and is left where it is, which is what makes a failing row debuggable.
local fixture_root = dofile(vim.fn.fnamemodify(here, ':p:h') .. '/fixture.lua')
local root, owned_root = fixture_root.root('JEV_ROOT', '-jev-ui')

-- The fixture root is its own repository root, which every other Lua harness here already
-- arranges (`rules_live.lua`, `dismiss_test.lua`, `result_surface.lua`, `context_search.lua` all
-- create this same `.git/`). It is not decoration: the plugin's workspace root is
-- `vim.fs.root(bufnr, {'.git'})`, so with no marker here the walk continues *above* the temp
-- directory and adopts whatever it finds — and this row's whole design assumes the root is this
-- directory, because that is where the rules below are written. On a CI runner it adopted
-- `/tmp`: the suite's own `settings_race` and `scope_containment` rows declared `/tmp` as their
-- root, so the server kept its session record in `/tmp/.git/jev/`, and this row then loaded no
-- rules from `/tmp/.jev/rules` — six checks failed ("no quickfix.jev action after 30 s", the
-- hints and ask fixtures never analysed, no plan, and `rg` searching all of `/tmp`). Those two
-- rows now keep to a private directory; this marker is what makes the row independent of them,
-- and of any other tool that has ever made a repository of the shared temp directory.
vim.fn.mkdir(root .. '/.git', 'p')

-- The ambient pass is the *rules* pass (`jev.rules/1`), so a repository with no rule gets no
-- ambient finding — and several checks below are about a fixture that has one ("analysed, so a
-- hint has something to report", the ask finding). This rule's inspection matches the
-- `    f = open(path)` line every Python fixture in this directory carries, so the finding
-- lands inside `def alpha` and the hint sits on that declaration. `applies_to` is globbed
-- tightly, because this directory also holds `.zzz`, `.lua` and `.c` fixtures.
vim.fn.mkdir(root .. '/.jev/rules', 'p')
vim.fn.writefile({
  -- A long-bracket string: the JSON needs a literal `\\(` so the decoded pattern is `\(`.
  -- Two rules, because the directory holds two kinds of fixture: the Python ones (hints, ask,
  -- lens) carry `f = open(path)`, and the unidentified `.zzz` ones (picker, reject) carry the
  -- accumulator loop. `applies_to` is globbed tightly for exactly that reason.
  [[{"schema":"jev.rules/1","rules":[{"id":"no-bare-open","title":"File opened without a context manager","text":"Open the file with a context manager so the handle is closed.","severity":"warning","applies_to":["**/*.py"],"inspection":{"kind":"regex","pattern":"open\\(","max_matches":0},"judgement":{"question":"Is this handle left open on a path that matters?","criteria":{"true":"the handle outlives the function or is never closed","false":"the handle is closed by the caller or the process"},"min_probability":0.75},"verb_hint":"fix"},{"id":"unbounded-accumulator","title":"Accumulator loop without a bound","text":"Bound the loop that accumulates into a shared value so it cannot run away.","severity":"warning","applies_to":["**/*.zzz"],"inspection":{"kind":"regex","pattern":"for i = 1","max_matches":0},"judgement":{"question":"Can this loop run more iterations than the caller expects?","criteria":{"true":"the bound comes from outside the code","false":"the bound is a literal the code controls"},"min_probability":0.75},"verb_hint":"fix"}]}]],
}, root .. '/.jev/rules/example.json')

say('[nvim_ui] plugin  : ' .. PLUGIN)
say('[nvim_ui] server  : ' .. BIN)
say('[nvim_ui] model   : ' .. (MODEL ~= '' and MODEL or '(none: JEV_BASE_URL unset)'))
say('[nvim_ui] fixtures: ' .. root)

-- 6a. The segment with no client at all -----------------------------------------------
--
-- Run first, before any client exists: "does nothing at all when no `jev` client is attached"
-- is a property of the module, and this is the only moment it is trivially true.

local statusline = require('jev.statusline')
do
  local raised, value = pcall(statusline.component)
  check(raised and type(value) == 'string', 'statusline.component() returns a string, and does not raise, with no client attached', value)
  check(value == '', 'statusline.component() is empty with no client attached', vim.inspect(value))
end

local jev = require('jev')
-- A generous budget: this file makes many model calls in a minute and is not the place that
-- tests the ceiling (verify/queue_test.py and the budget unit tests are).
jev.setup({
  cmd = { BIN },
  settings = { budget = { max_calls_per_min = 120, max_calls_per_hour = 600 } },
})
vim.cmd('filetype on')

-- The surface is four keys. Everything else the plugin can do is still reachable, by typing
-- (`:Jev …`), and the cut is the point of this unit: the plugin's own user could not say what
-- it was for. `<leader>ja` is the picker, mapped in visual mode as well.
do
  local expected = {
    '<leader>ja',
    '<leader>ju',
    '<leader>jq',
    '<leader>js',
  }
  local missing = {}
  for _, lhs in ipairs(expected) do
    if vim.fn.maparg(lhs, 'n') == '' then
      missing[#missing + 1] = lhs
    end
  end
  check(#missing == 0, 'the four surviving keymaps are mapped', vim.inspect(missing))
  check(
    vim.fn.maparg('<leader>ja', 'x') ~= '',
    'the picker is also mapped in visual mode, where a selection is the scope'
  )
  local cut = { '<leader>jv', '<leader>jp', '<leader>je', '<leader>jr', '<leader>jx',
    '<leader>jd', '<leader>jS', '<leader>jG', '<leader>jh', '<leader>jl' }
  local still = {}
  for _, lhs in ipairs(cut) do
    if vim.fn.maparg(lhs, 'n') ~= '' then
      still[#still + 1] = lhs
    end
  end
  check(#still == 0, 'the cut keymaps are gone, not quietly kept', vim.inspect(still))
end

--- @return integer bufnr, boolean attached
local function open_fixture(name, body)
  local path = root .. '/' .. name
  vim.fn.writefile(body, path)
  -- `edit!` and a check on the result: without the bang a buffer that cannot be abandoned
  -- makes the edit a silent no-op, and then a check quietly tests the wrong buffer.
  vim.cmd('silent! edit! ' .. vim.fn.fnameescape(path))
  local bufnr = vim.api.nvim_get_current_buf()
  -- `:p` makes a path absolute but does not follow symlinks, and on macOS `$TMPDIR` is one:
  -- `vim.fn.tempname()` gives `/var/folders/…` while the buffer name is the resolved
  -- `/private/var/folders/…`. Comparing the two literally failed every fixture on this
  -- platform. `realpath` on both sides, so the check asks what it means to ask.
  local function real(path)
    return vim.uv.fs_realpath(path) or vim.fn.fnamemodify(path, ':p')
  end
  local opened = real(vim.api.nvim_buf_get_name(bufnr))
  assert(
    opened == real(path),
    ('open_fixture: wanted %s, holding %s'):format(path, opened)
  )
  local attached = vim.wait(15000, function()
    return #vim.lsp.get_clients({ bufnr = bufnr, name = 'jev' }) > 0
  end, 25)
  return bufnr, attached
end

--- One request, bounded. `nil, err` means it did not answer in time.
local function request(client, method, params, timeout, bufnr)
  local answered, result, err = false, nil, nil
  client:request(method, params, function(e, r)
    err, result, answered = e, r, true
  end, bufnr)
  vim.wait(timeout, function()
    return answered
  end, 25)
  return result, err
end

--- What the picker put on the wire, and what a resolve answered. Instance-level, so nothing
--- else in the session is affected.
local function spy(c)
  local seen = {}
  local orig = c.request
  c.request = function(self, method, params, handler, bufnr)
    local wrapped = handler
    if method == 'textDocument/codeAction' then
      seen.code_action = params
    elseif method == 'codeAction/resolve' then
      seen.resolve_params = params
      wrapped = function(err, result, ctx)
        seen.resolve = { err = err, result = result, done = true }
        if handler ~= nil then
          return handler(err, result, ctx)
        end
      end
    end
    return orig(self, method, params, wrapped, bufnr)
  end
  return seen
end

--- The cursor range the picker asks with when nothing is selected.
local function cursor_params(bufnr)
  local cursor = vim.api.nvim_win_get_cursor(0)
  local point = { line = cursor[1] - 1, character = cursor[2] }
  return {
    textDocument = { uri = vim.uri_from_bufnr(bufnr) },
    range = { start = vim.deepcopy(point), ['end'] = point },
    context = { triggerKind = 1, diagnostics = {} },
  }
end

local function kind_of(action)
  return type(action.kind) == 'string' and action.kind or ''
end

local function title_of(item)
  return item.action.title or ''
end

--- The actions a `textDocument/codeAction` call returns, and whether one of them is a fix for a
--- cached finding (`quickfix.jev`, PROTOCOL §4.1) — which is what "a findable issue" means.
local function actions_and_issue(client, params, bufnr)
  local actions = request(client, 'textDocument/codeAction', params, 10000, bufnr) or {}
  for _, action in ipairs(actions) do
    if kind_of(action):find('^quickfix%.jev') ~= nil then
      return actions, true
    end
  end
  return actions, false
end

--- Apply an edit to a copy of `source`, with the client's own range arithmetic. Used to assert
--- the buffer ends up holding exactly what the edit said — not "something changed".
local function edited_text(edit, source, uri)
  local edits
  for _, change in ipairs(edit.documentChanges or {}) do
    if change.textDocument ~= nil and change.textDocument.uri == uri then
      edits = change.edits
    end
  end
  if edits == nil then
    return nil
  end
  local scratch = vim.api.nvim_create_buf(false, true)
  vim.api.nvim_buf_set_lines(scratch, 0, -1, false, source)
  local raised, err = pcall(vim.lsp.util.apply_text_edits, edits, scratch, 'utf-8')
  local out = raised and lines(scratch) or nil
  vim.api.nvim_buf_delete(scratch, { force = true })
  return out, err
end

-- 1. The picker: open, list, resolve, apply -------------------------------------------

local FIXTURE = { 'local total = 0', 'for i = 1, 10 do total = total + i end', 'print(total)' }
local bufnr, attached = open_fixture('picker.zzz', FIXTURE)
check(attached, 'the plugin attached a client to the picker fixture')

-- The command line and the keymaps reach the same functions, and `M.command` mints the
-- `workDoneToken` from the shared registry (PROTOCOL §3.5 path 1) — so the server reports
-- progress under it and the plugin's own tracker can tell it from server-initiated work.
do
  local reported = nil
  local saved_notify = vim.notify
  vim.notify = function(msg)
    reported = tostring(msg)
  end
  local dispatched, dispatch_err = pcall(vim.cmd, 'Jev status')
  check(dispatched, ':Jev status dispatches through the command surface', dispatch_err)

  local kinds = {}
  local autocmd = vim.api.nvim_create_autocmd('LspProgress', {
    callback = function(ev)
      local data = ev.data or {}
      local params = data.params or {}
      if params.token ~= nil and type(params.value) == 'table' and params.value.kind ~= nil then
        local key = tostring(params.token)
        kinds[key] = kinds[key] or {}
        kinds[key][params.value.kind] = true
      end
    end,
  })
  local answered, outcome = false, nil
  jev.status(function(err, result)
    outcome, answered = { err = err, result = result }, true
  end)
  vim.wait(15000, function()
    return answered
  end, 25)
  check(
    answered and outcome.err == nil and type(outcome.result) == 'table' and outcome.result.ok == true,
    'jev.status round-trips through the plugin command path',
    vim.inspect(outcome and outcome.err)
  )
  local paired = false
  for _, seen in pairs(kinds) do
    if seen.begin and seen['end'] then
      paired = true
    end
  end
  check(paired, 'the command carries a workDoneToken the server reports begin/end under (PROTOCOL §3.5)')
  vim.wait(15000, function()
    return reported ~= nil
  end, 25)
  vim.api.nvim_del_autocmd(autocmd)
  vim.notify = saved_notify
  check(
    type(reported) == 'string' and reported:find('schema', 1, true) ~= nil,
    ':Jev status reports the Result envelope it got back',
    vim.inspect(reported)
  )
end

local client = vim.lsp.get_clients({ bufnr = bufnr, name = 'jev' })[1]
local seen = spy(client)

-- The review runs on save (PROTOCOL §10, triggers.diagnostics = "save"), so saving is what
-- turns the fixture into "a file with a findable issue".
vim.cmd('silent write')
local issue, actions = false, {}
do
  -- Finding the issue means waiting for a model call (the review runs on save), so this is a
  -- discovery window rather than a wait for something that is already there. With no endpoint
  -- named by the environment it is short: the server's own default endpoint may still be live,
  -- but then this is not the run the header documents.
  local window = MODEL == '' and 5000 or 30000
  local deadline = vim.uv.now() + window
  while vim.uv.now() < deadline do
    actions, issue = actions_and_issue(client, cursor_params(bufnr), bufnr)
    if issue then
      break
    end
    sleep(250)
  end
  if not issue then
    actions, _ = actions_and_issue(client, cursor_params(bufnr), bufnr)
  end
end

local captured = {}
local applied = nil
local stock_apply = vim.lsp.util.apply_workspace_edit
vim.lsp.util.apply_workspace_edit = function(edit, encoding)
  applied = edit
  return stock_apply(edit, encoding)
end

if not issue then
  local label = 'the picker lists an action for a findable issue, and the pick it resolves to reaches the buffer'
  if MODEL == '' then
    skip(
      label,
      ('no quickfix.jev action within 5 s of saving (%d verb action(s) offered); JEV_BASE_URL is unset, so no endpoint was promised')
        :format(#actions)
    )
  else
    fail(label, 'no quickfix.jev action after 30 s: ' .. vim.inspect(vim.tbl_map(kind_of, actions)))
  end
else
  local before = lines(bufnr)
  vim.ui.select = function(items, opts, cb)
    captured.items, captured.opts = items, opts
    cb(items[1], 1)
  end
  require('jev.picker').action()
  local landed = vim.wait(20000, function()
    return applied ~= nil
  end, 25)

  check(
    captured.items ~= nil and #captured.items > 0,
    'the picker opens and lists actions for a file with a findable issue',
    vim.inspect(captured.items)
  )
  local wire = seen.code_action or {}
  check(
    wire.context ~= nil and wire.context.triggerKind == 1,
    'the request goes out as triggerKind = 1 (Invoked, PROTOCOL §3.1)',
    vim.inspect(wire.context)
  )
  check(
    wire.range ~= nil and wire.range.start.line == 0,
    'the request carries the cursor range',
    vim.inspect(wire.range)
  )
  local detail = captured.opts ~= nil and captured.opts.format_item(captured.items[1]) or nil
  check(
    type(detail) == 'string' and detail:find(captured.items[1].action.title, 1, true) == 1,
    'format_item() renders the title first, for the picker that shows it',
    vim.inspect(detail)
  )
  check(
    seen.resolve_params ~= nil and seen.resolve_params.title == captured.items[1].action.title,
    'only the chosen action is sent to codeAction/resolve ([R4])',
    vim.inspect(seen.resolve_params and seen.resolve_params.title)
  )
  if check(landed, 'the resolve answered with an edit and it was applied within 20 s') then
    local expect = edited_text(applied, before, vim.uri_from_bufnr(bufnr))
    check(
      expect ~= nil and not vim.deep_equal(before, expect) and vim.deep_equal(lines(bufnr), expect),
      'the edit reaches the buffer byte-for-byte',
      vim.inspect(lines(bufnr))
    )
  end
end

-- 2. The preview pane, and what happens without one -----------------------------------
--
-- `data.summary` (PROTOCOL §4 — the model's words never become the title) is what the pane
-- shows. A picker that can render a second line gets it under the title; the stock
-- `vim.ui.select` is an `inputlist`, one line per item, so the same detail degrades to an
-- inline suffix instead of becoming a second line of the list.

do
  -- A fresh, never-saved fixture, whose content is unlike any other in this run (the server's
  -- cache is keyed by content hash, PROTOCOL N9): with nothing cached, an invoked `codeAction`
  -- request gets the placeholder action, which is the one shape that carries a `data.summary` on
  -- the fast path (PROTOCOL §3.1, §4) — so this check needs no model endpoint.
  local pane_bufnr, pane_attached = open_fixture('preview.zzz', { 'local pane = 1', 'return pane' })
  check(pane_attached, 'the plugin attached a client to the preview fixture')

  local items, opts = nil, nil
  vim.ui.select = function(list, o, cb)
    items, opts = list, o
    cb(nil, nil) -- dismissed: this check is about rendering, not about applying
  end
  require('jev.picker').action({ bufnr = pane_bufnr })
  if
    check(
      vim.wait(10000, function()
        return items ~= nil
      end, 25),
      'the picker asks its chooser with a formatted list'
    )
  then
    local index, summary
    for i, item in ipairs(items) do
      local text = type(item.action.data) == 'table' and item.action.data.summary or nil
      if type(text) == 'string' and text ~= '' then
        index, summary = i, text
      end
    end
    if index == nil then
      skip('the preview pane shows data.summary', 'no action carried a summary in this session')
    else
      local rendered = opts.format_item(items[index])
      check(
        rendered:find(title_of(items[index]), 1, true) == 1 and rendered:find('\n', 1, true) ~= nil,
        'a picker that can render two lines gets the title, then data.summary under it',
        vim.inspect(rendered)
      )

      -- The same situation through the stock picker, on content the cache has not seen yet, with
      -- `inputlist` stubbed so the real path runs and stdin is never read. `0` is not a valid
      -- choice, so the stock picker cancels and nothing is applied.
      vim.fn.writefile({ 'local pane = 2', 'return pane' }, root .. '/preview.zzz')
      vim.cmd('silent edit!')
      local choices = nil
      local saved_inputlist = vim.fn.inputlist
      vim.ui.select = stock_select
      vim.fn.inputlist = function(list)
        choices = list
        return 0
      end
      require('jev.picker').action({ bufnr = pane_bufnr })
      local offered = vim.wait(10000, function()
        return choices ~= nil
      end, 25)
      vim.fn.inputlist = saved_inputlist
      local line = nil
      for _, candidate in ipairs(choices or {}) do
        if candidate:find(summary, 1, true) ~= nil then
          line = candidate
        end
      end
      check(
        offered and type(line) == 'string' and line:find('\n', 1, true) == nil,
        'the stock picker degrades the same detail to one line, and the flow still runs',
        vim.inspect(line)
      )

      -- No picker at all: the menu is what the user loses first, so it is reported rather than
      -- swallowed, and nothing raises.
      vim.ui.select = nil
      local surviving, chooser_err = pcall(require('jev.picker').action, { bufnr = pane_bufnr })
      vim.ui.select = stock_select
      check(surviving, 'the picker does not raise when vim.ui.select is unset', chooser_err)
    end
  end
  vim.ui.select = stock_select
end

-- 3. Visual mode ----------------------------------------------------------------------

do
  local wire = nil
  vim.ui.select = function(items, opts, cb)
    wire = seen.code_action
    cb(nil, nil) -- dismissed: the point here is the request, not the choice
  end
  vim.cmd('normal! ggVjj')
  local lhs = vim.api.nvim_replace_termcodes('<leader>ja', true, false, true)
  vim.api.nvim_feedkeys(lhs, 'x', false)
  local asked = vim.wait(10000, function()
    return wire ~= nil
  end, 25)
  if check(asked, 'the visual-mode keymap reaches the picker') then
    check(
      wire.range.start.line ~= wire.range['end'].line,
      'the request carries the selected range, not the cursor',
      vim.inspect(wire.range)
    )
    local scoped, _ = actions_and_issue(client, wire, bufnr)
    local explicit = false
    for _, action in ipairs(scoped) do
      if type(action.data) == 'table' and action.data.scope_source == 'explicit' then
        explicit = true
      end
    end
    check(explicit, 'the server scopes that range as scope_source = "explicit" (PROTOCOL §4)', vim.inspect(vim.tbl_map(function(a)
      return type(a.data) == 'table' and a.data.scope_source or '?'
    end, scoped)))
  end
  vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<Esc>', true, false, true), 'x', false)
end

-- 4. A rejected resolve ---------------------------------------------------------------

local reject_bufnr, reject_attached = open_fixture('reject.zzz', FIXTURE)
check(reject_attached, 'the plugin attached a client to the reject fixture')
local reject_client = vim.lsp.get_clients({ bufnr = reject_bufnr, name = 'jev' })[1]
local reject_seen = spy(reject_client)

local notes = {}
local stock_notify = vim.notify
--- Count what the user would see. Neovim's own notify writes without a trailing newline, which
--- would run into the next `ok` line, so the note is printed by the test itself — the count is
--- what matters, and it is the same either way.
vim.notify = function(msg)
  notes[#notes + 1] = tostring(msg)
  say('      notify: ' .. tostring(msg))
end
local function reports(text)
  local n = 0
  for _, note in ipairs(notes) do
    if note:find(text, 1, true) ~= nil then
      n = n + 1
    end
  end
  return n
end

--- Drive one rejected resolve: the file changes while the menu is open, so the versioned action
--- is stale by the time it is resolved (PROTOCOL §8 rule 3 — the resolve comes back with no
--- edit and a reason, which is exactly the shape `disabled.reason` exists for).
---
--- @return string[] before_the_edit, string[] after_the_edit, string? reason
local function rejected_pick()
  reject_seen.resolve = nil
  local before_edit = lines(reject_bufnr)
  local after_edit = nil
  vim.ui.select = function(items, opts, cb)
    local mutated = lines(reject_bufnr)
    mutated[#mutated + 1] = '-- edited while the menu was open'
    vim.api.nvim_buf_set_lines(reject_bufnr, 0, -1, false, mutated)
    after_edit = lines(reject_bufnr)
    cb(items[1], 1)
  end
  local text = nil
  require('jev.picker').action({ bufnr = reject_bufnr })
  vim.wait(10000, function()
    return reject_seen.resolve ~= nil
  end, 25)
  if reject_seen.resolve ~= nil and reject_seen.resolve.result ~= nil then
    local disabled = reject_seen.resolve.result.disabled
    text = type(disabled) == 'table' and disabled.reason or nil
  end
  return before_edit, after_edit, text
end

local before_reject, mutated_reject, reason = rejected_pick()
local outcome = reject_seen.resolve
if
  check(
    outcome ~= nil and outcome.err == nil and outcome.result.edit == nil and type(reason) == 'string',
    'a rejected resolve comes back with no edit and a reason'
  )
then
  check(
    mutated_reject ~= nil and not vim.deep_equal(before_reject, mutated_reject),
    'the reject fixture really did move while the menu was open'
  )
  check(
    vim.deep_equal(lines(reject_bufnr), mutated_reject),
    'nothing is applied, and the buffer keeps the text the rejection was measured against'
  )
  check(reports(reason) == 1, 'the reason is reported exactly once', ('%d report(s) of %q'):format(reports(reason), reason))

  -- The server also sends the same text as `window/showMessage` (PROTOCOL §3.4), in an order
  -- that is its business. Deliver one through the client's own handler chain: the wrapper
  -- must not repeat what the picker already said.
  local messages = reject_client:_resolve_handler('window/showMessage')
  if messages ~= nil then
    messages(nil, { type = 2, message = reason }, { method = 'window/showMessage', client_id = reject_client.id })
    check(reports(reason) == 1, 'the same reason on the second channel is not reported again', ('%d report(s)'):format(reports(reason)))

    -- The reverse order: the server speaks first, then the picker resolves with the same text.
    local _, _, reason2 = rejected_pick()
    check(
      reason2 == reason and reports(reason) == 1,
      'a reason the server already showed is not reported by the picker',
      ('%d report(s) of %q'):format(reports(reason), tostring(reason2))
    )

    -- And the suppression is about that text, not about messages in general.
    messages(nil, { type = 3, message = 'jev: uitest control message' }, { method = 'window/showMessage', client_id = reject_client.id })
    check(reports('jev: uitest control message') == 1, 'an unrelated window/showMessage is still shown')
  else
    skip('the window/showMessage dedupe', 'no handler resolved for window/showMessage')
  end
else
  skip('the reason dedupe', 'no rejected resolve to compare against')
end
vim.notify = stock_notify

-- 5. The diff preview -----------------------------------------------------------------

do
  local bufnr = reject_bufnr
  local before = lines(bufnr)
  local wins = window_count()
  local sizes = window_sizes()
  local uri = vim.uri_from_bufnr(bufnr)
  local version = vim.lsp.util.buf_versions[bufnr]
  local edit = {
    documentChanges = {
      {
        textDocument = { uri = uri, version = version },
        edits = {
          {
            range = { start = { line = 1, character = 0 }, ['end'] = { line = 2, character = 0 } },
            newText = 'local proposed = true\n',
          },
        },
      },
    },
  }
  local expect = edited_text(edit, before, uri)

  -- The keys the preview wants, bound by the *user* in their own buffer first. A buffer-local map
  -- of the user's wins: the preview installs its key only where the key is free, and whatever it
  -- does not install it must not delete on the way out. The fixture is deliberately not buffer 1 —
  -- the guard used to read `m.lhs == lhs and m.buffer == 1`, and `nvim_buf_get_keymap` reports the
  -- buffer that was asked about (`buffer = 2` in buffer 2), so the second half only ever held for
  -- buffer 1: in any session whose file is not the first buffer, the preview overwrote these maps
  -- and `close()` removed them.
  check(
    bufnr ~= 1,
    'the fixture is not buffer 1, which is the case the map guard got wrong',
    bufnr
  )
  local user_maps = {}
  for _, lhs in ipairs({ 'q', '<CR>', '<Esc>', 'y' }) do
    local desc = 'uitest: the user owns ' .. lhs
    vim.keymap.set('n', vim.api.nvim_replace_termcodes(lhs, true, false, true), function() end,
      { buffer = bufnr, desc = desc })
    user_maps[lhs] = desc
  end

  --- The user's maps in *their* buffer, read explicitly rather than through `maparg`, which
  --- answers for whichever buffer is current — while the preview is open that is the preview.
  --- @return string[] the ones that are not the user's any more
  local function lost_user_maps()
    local present = {}
    for _, m in ipairs(vim.api.nvim_buf_get_keymap(bufnr, 'n')) do
      present[m.desc or m.rhs or '?'] = true
    end
    local lost = {}
    for lhs, desc in pairs(user_maps) do
      if not present[desc] then
        lost[#lost + 1] = lhs
      end
    end
    table.sort(lost)
    return lost
  end

  check(
    #lost_user_maps() == 0,
    "the user's buffer-local maps are bound before the preview opens",
    vim.inspect(lost_user_maps())
  )

  local maps_before = {}
  for _, m in ipairs(vim.api.nvim_buf_get_keymap(bufnr, 'n')) do
    maps_before[m.lhs] = true
  end

  local diff = require('jev.diff')
  local opened = diff.propose(edit, { bufnr = bufnr })
  check(opened, 'the diff preview opens')
  check(window_count() == wins + 1, 'the preview is a second window (side by side)', window_count())

  local preview_win, original_win = nil, nil
  for _, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
    if vim.api.nvim_win_get_buf(win) == bufnr then
      original_win = win
    else
      preview_win = win
    end
  end
  if preview_win ~= nil then
    local preview_bufnr = vim.api.nvim_win_get_buf(preview_win)
    -- While the preview is open: the user's maps are still the user's, and the preview's own keys
    -- went on its own side, where nothing was bound.
    check(
      #lost_user_maps() == 0,
      "the preview does not shadow the user's buffer-local maps",
      vim.inspect(lost_user_maps())
    )
    local preview_q = false
    for _, m in ipairs(vim.api.nvim_buf_get_keymap(preview_bufnr, 'n')) do
      if m.lhs == 'q' then
        preview_q = true
      end
    end
    check(preview_q, "and its own q is mapped on its own side, where the key was free")
    check(
      vim.deep_equal(lines(preview_bufnr), expect),
      'the preview shows the post-edit text, computed without touching the buffer'
    )
    check(
      vim.bo[preview_bufnr].buftype == 'nofile' and vim.bo[preview_bufnr].bufhidden == 'wipe',
      'the preview buffer is scratch: buftype=nofile, bufhidden=wipe',
      ('buftype=%q bufhidden=%q'):format(vim.bo[preview_bufnr].buftype, vim.bo[preview_bufnr].bufhidden)
    )
    check(vim.deep_equal(lines(bufnr), before), 'the preview did not touch the buffer')
    check(
      vim.wo[preview_win].diff == true and vim.wo[original_win].diff == true,
      'both windows are in real diff mode'
    )

    -- Reject with the real key, from the window the user is looking at.
    vim.api.nvim_set_current_win(preview_win)
    vim.api.nvim_feedkeys('q', 'x', false)
    vim.wait(2000, function()
      return window_count() == wins
    end, 20)
    check(window_count() == wins, 'rejecting restores the window count', window_count())
    check(vim.deep_equal(window_sizes(), sizes), 'rejecting restores the window sizes', vim.inspect(window_sizes()))
    check(vim.deep_equal(lines(bufnr), before), 'rejecting leaves the buffer byte-for-byte')
    check(not vim.api.nvim_buf_is_valid(preview_bufnr), 'the scratch buffer is gone')
    check(not vim.wo.diff, "the buffer's window is not left in diff mode")
    -- A before/after diff rather than "the buffer has no keymaps": Neovim sets `K` for hover
    -- itself when a client advertises the capability, which has nothing to do with the preview
    -- and would make the stricter claim fail for the wrong reason.
    local leftover = {}
    for _, m in ipairs(vim.api.nvim_buf_get_keymap(bufnr, 'n')) do
      if maps_before[m.lhs] == nil then
        leftover[#leftover + 1] = m.lhs
      end
    end
    check(#leftover == 0, 'no keymap the preview added is left behind', vim.inspect(leftover))
    -- The other half of the same claim, and the one the guard got wrong: what the preview left
    -- alone is still there, still the user's.
    check(
      #lost_user_maps() == 0,
      "the user's own buffer-local maps survive the dismiss",
      vim.inspect(lost_user_maps())
    )
  else
    fail('the preview window could be found')
  end

  -- Approve with the real key.
  local reopened = diff.propose(edit, { bufnr = bufnr })
  check(reopened and window_count() == wins + 1, 'a second preview opens after a rejection')
  vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<CR>', true, false, true), 'x', false)
  vim.wait(2000, function()
    return window_count() == wins
  end, 20)
  check(window_count() == wins, 'approving closes the preview')
  check(vim.deep_equal(lines(bufnr), expect), 'approving applies the edit', vim.inspect(lines(bufnr)))
  vim.lsp.util.apply_workspace_edit = stock_apply
end

-- 6. The statusline segment -----------------------------------------------------------

do
  local token = 'jev:uitest:1'
  statusline.track(token, 'resolve')
  local segment = statusline.component()
  check(
    type(segment) == 'string' and segment:find('1 running', 1, true) ~= nil,
    'the segment reports work in flight',
    vim.inspect(segment)
  )

  statusline.note_status({
    budget = {
      calls_last_minute = 1,
      limit_per_minute = 6,
      calls_last_hour = 1,
      limit_per_hour = 120,
      tokens_used = 10,
      limit_tokens = 500000,
    },
  })
  segment = statusline.component()
  check(
    segment:find('calls', 1, true) == nil,
    'below half the ceiling only the count is shown',
    vim.inspect(segment)
  )

  statusline.note_status({
    budget = {
      calls_last_minute = 5,
      limit_per_minute = 6,
      calls_last_hour = 5,
      limit_per_hour = 120,
      tokens_used = 10,
      limit_tokens = 500000,
    },
  })
  segment = statusline.component()
  check(
    segment:find('5/6', 1, true) ~= nil,
    'above half the ceiling the segment shows the calls used out of the limit',
    vim.inspect(segment)
  )

  local polled, poll_err = pcall(statusline.poll)
  check(polled, 'poll() is safe to call at any time', poll_err)

  -- The expression `setup` documents must be the one that works, evaluated the way a statusline
  -- is: from a string, at redraw time.
  local saved_statusline = vim.o.statusline
  vim.o.statusline = "%{%v:lua.require'jev.statusline'.component()%}"
  local rendered = vim.api.nvim_eval_statusline(vim.o.statusline, {}).str
  vim.o.statusline = saved_statusline
  check(
    rendered:find('5/6', 1, true) ~= nil,
    'the documented statusline expression renders the segment',
    vim.inspect(rendered)
  )

  statusline.untrack(token)
  check(statusline.component() == '', 'the segment is empty again with nothing in flight', vim.inspect(statusline.component()))
end

-- 7. A chooser that switches on `kind` ----------------------------------------------------

-- snacks' `vim.ui.select` branches on `opts.kind == 'codeaction'` and then treats every item
-- as Neovim's `{ action, ctx }` pair, dereferencing `item.ctx.client_id`
-- (`snacks/picker/format.lua:350`). Our items carry `action` and no `ctx`, so asking for that
-- kind crashed the picker in a real config before it drew anything. The stub below does what
-- snacks does, so the same mistake fails here.
do
  local items = {
    {
      action = {
        title = 'Fix: file handle is never closed',
        kind = 'quickfix',
        data = { summary = 'line 5 · 1 file' },
      },
    },
  }
  local asked, rendered, chosen
  local stock = vim.ui.select
  vim.ui.select = function(list, opts, cb)
    asked = true
    if opts.kind == 'codeaction' then
      for _, item in ipairs(list) do
        -- exactly what snacks does with an item it believes came from vim.lsp.buf.code_action
        local _ = vim.lsp.get_client_by_id(item.ctx.client_id)
      end
    end
    rendered = opts.format_item and opts.format_item(list[1]) or nil
    cb(list[1], 1)
  end
  local ok = require('jev.picker').select(items, function(choice)
    chosen = choice
  end)
  vim.ui.select = stock

  check(ok, 'a chooser that switches on `kind` does not break the picker')
  check(asked, 'the items reach the chooser')
  check(
    type(rendered) == 'string' and rendered:find('file handle is never closed', 1, true) ~= nil,
    'the chooser renders our own text through format_item',
    vim.inspect(rendered)
  )
  check(chosen == items[1], 'the choice comes back to the caller')
end

-- 8. Command arguments are arrays ----------------------------------------------------------

-- `ExecuteCommandParams.arguments` is `LSPAny[]`, and the server reads `arguments.first()`.
-- `jev.plan` sent a map, so the transport rejected it before the command ran: every press of
-- `:Jev plan` answered `invalid type: map, expected a sequence`. The entry points are driven
-- here with the request captured, so the shape is checked without spending a model call.
do
  local client = vim.lsp.get_clients({ name = 'jev' })[1]
  if client == nil then
    skip('command argument shapes', 'no jev client attached')
  else
    local captured = {}
    local orig = client.request
    client.request = function(_, method, params)
      if method == 'workspace/executeCommand' then
        captured[#captured + 1] = params
      end
      return true
    end
    local plugin = require('jev')
    plugin.plan('uitest goal')
    plugin.review()
    plugin.explain()
    plugin.status()
    client.request = orig

    check(#captured >= 4, 'every command entry point sends a request', ('%d captured'):format(#captured))
    local offenders = {}
    for _, params in ipairs(captured) do
      local args = params.arguments
      -- a sequence: empty, or its first slot is filled. A map has keys and no slot 1.
      local is_array = type(args) == 'table' and (next(args) == nil or args[1] ~= nil)
      if not is_array then
        offenders[#offenders + 1] = ('%s -> %s'):format(params.command, vim.inspect(args))
      end
    end
    check(
      #offenders == 0,
      'arguments reach the server as an array, not a map',
      table.concat(offenders, '; ')
    )
  end
end

-- 9. A streamed answer arrives progressively ------------------------------------------------

-- `jev.explain` asks for a stream, and the server reports the text *so far* under the token
-- the plugin issued (PROTOCOL §3.5). What is asserted is what the user sees: the artifact
-- buffer holds more than one distinct state while the request is still open, so the answer is
-- being written rather than appearing at the end — and the buffer the partials filled is the
-- buffer the finished artifact lands in, so nothing moves when it completes.
do
  if #vim.lsp.get_clients({ name = 'jev', bufnr = reject_bufnr }) == 0 then
    skip('the streamed explanation', 'no jev client attached to the fixture')
  else
    local function stream_buffers()
      local found = {}
      for _, b in ipairs(vim.api.nvim_list_bufs()) do
        if vim.api.nvim_buf_is_valid(b)
          and vim.api.nvim_buf_get_name(b):find('jev://', 1, true) ~= nil
        then
          found[#found + 1] = b
        end
      end
      return found
    end

    for _, b in ipairs(stream_buffers()) do
      vim.api.nvim_buf_delete(b, { force = true })
    end

    -- What this request creates, not what was already lying around: an earlier check stubs
    -- `client.request` and never answers, and its buffers are none of this check's business.
    local before_bufs = {}
    for _, b in ipairs(vim.api.nvim_list_bufs()) do
      before_bufs[b] = true
    end
    local function new_scratch_buffers()
      local out = {}
      for _, b in ipairs(vim.api.nvim_list_bufs()) do
        if not before_bufs[b] and vim.api.nvim_buf_is_valid(b) and vim.bo[b].buftype == 'nofile' then
          out[#out + 1] = b
        end
      end
      return out
    end

    vim.api.nvim_set_current_buf(reject_bufnr)
    local seen = {}
    -- The *default* path: `M.explain()` with no callback, so the plugin renders the artifact
    -- itself — a check that supplied its own callback would only be testing the callback.
    require('jev').explain()

    local deadline = vim.uv.now() + 30000
    local finished, finished_name = false, nil
    while vim.uv.now() < deadline do
      for _, b in ipairs(new_scratch_buffers()) do
        local text = table.concat(vim.api.nvim_buf_get_lines(b, 0, -1, false), '\n')
        if text ~= '' then
          seen[text] = true
        end
        local name = vim.api.nvim_buf_get_name(b)
        if name:find('jev://explanation', 1, true) == 1 then
          finished, finished_name = true, name
        end
      end
      if finished then
        break
      end
      vim.wait(20)
    end

    local count = 0
    for _ in pairs(seen) do
      count = count + 1
    end
    check(
      count >= 2,
      'the artifact buffer holds more than one state while the answer is written',
      ('%d distinct state(s) seen'):format(count)
    )
    check(finished, 'the finished artifact lands in the buffer the stream filled')
    check(
      #new_scratch_buffers() == 1,
      'one buffer, not two: the streamed buffer becomes the artifact buffer',
      (function()
        local names = {}
        for _, b in ipairs(new_scratch_buffers()) do
          names[#names + 1] = vim.api.nvim_buf_get_name(b)
        end
        return ('%d buffer(s): %s'):format(#names, table.concat(names, ' | '))
      end)()
    )
    if finished_name ~= nil then
      check(
        finished_name:find('jev://explanation/', 1, true) == 1,
        'and it is named for the artifact it now holds',
        finished_name
      )
    end

    for _, b in ipairs(stream_buffers()) do
      vim.api.nvim_buf_delete(b, { force = true })
    end
    vim.cmd('silent! only')
  end
end

-- 10. Code lenses ---------------------------------------------------------------------------

-- One affordance per declaration, written where the work is — the surface that does not have
-- to be remembered. `blocks()` offers only what the server would accept, and running a lens
-- goes through the plugin, because opening a buffer is a client decision.
do
  local lens_bufnr, lens_attached = open_fixture('lenses.py', {
    'import json',
    '',
    '',
    'def alpha(path):',
    '    f = open(path)',
    '    return json.load(f)',
    '',
    '',
    'def beta(x):',
    '    return x + 1',
  })
  check(lens_attached, 'the plugin attached a client to the lens fixture')

  local arrived = vim.wait(20000, function()
    return #vim.lsp.codelens.get({ bufnr = lens_bufnr }) > 0
  end, 50)
  local titles, explain_line = {}, nil
  for _, entry in ipairs(vim.lsp.codelens.get({ bufnr = lens_bufnr })) do
    local command = entry.lens.command or {}
    titles[#titles + 1] = command.title or '?'
    if command.command == 'jev.plugin.explain' then
      explain_line = entry.lens.range.start.line
    end
  end
  check(
    arrived and #titles == 2,
    'one lens per declaration at the left margin, and none for the import',
    table.concat(titles, ' | ')
  )

  -- What is displayed is verified live, in a real terminal: Neovim draws lenses through a
  -- decoration provider, which a headless session does not run. Here the question is whether
  -- the client asked and kept the answer, which is what the drawing needs.
  check(
    #vim.lsp.codelens.get({ bufnr = lens_bufnr }) == 2,
    'the client keeps one stored lens per declaration',
    tostring(#vim.lsp.codelens.get({ bufnr = lens_bufnr }))
  )

  if explain_line == nil then
    skip('running a lens', 'no explain lens arrived')
  else
    vim.api.nvim_set_current_buf(lens_bufnr)
    vim.api.nvim_win_set_cursor(0, { explain_line + 1, 0 })
    vim.lsp.codelens.run()
    local opened = vim.wait(30000, function()
      for _, b in ipairs(vim.api.nvim_list_bufs()) do
        if vim.api.nvim_buf_is_valid(b)
          and vim.api.nvim_buf_get_name(b):find('jev://explanation', 1, true)
        then
          return true
        end
      end
      return false
    end, 50)
    check(opened, 'running a lens opens what it promised')
    for _, b in ipairs(vim.api.nvim_list_bufs()) do
      if vim.api.nvim_buf_is_valid(b) and vim.api.nvim_buf_get_name(b):find('jev://', 1, true) then
        vim.api.nvim_buf_delete(b, { force = true })
      end
    end
  end
end

-- 11. The parser's scope -------------------------------------------------------------------

-- When a parser can name the enclosing declaration, the request carries it and the server
-- anchors on that instead of on its own structural guess. Lua is the language to test with
-- because its parser ships with Neovim; a language without one is not a failure, it just goes
-- out without a range and the server resolves the scope as it does for the CLI.
do
  local lua_bufnr, lua_attached = open_fixture('scope.lua', {
    'local M = {}',
    '',
    'function M.alpha(x)',
    '  return x + 1',
    'end',
    '',
    'function M.beta(x)',
    '  return x * 2',
    'end',
    '',
    'return M',
  })
  check(lua_attached, 'the plugin attached a client to the scope fixture')

  local client = vim.lsp.get_clients({ bufnr = lua_bufnr, name = 'jev' })[1]
  if client == nil then
    skip('the parser scope', 'no client on the scope fixture')
  else
    local captured = nil
    -- Both requests below are streamed, and a streamed answer is a surface: `M.command` opens
    -- one when the request is sent, and `place_surface` puts it in the window when the answer
    -- completes (`hide buffer`), which moves the current buffer. An answer still in flight when
    -- this section ends therefore lands inside a later one and takes the window from under it —
    -- the inlay-hint toggle below reads whichever buffer it lands on. Counting the answers here
    -- is what lets the section wait for its own requests instead of leaving them behind it.
    local explains = { sent = 0, answered = 0 }
    local orig = client.request
    client.request = function(_, method, params, handler, ...)
      if method == 'workspace/executeCommand' and params.command == 'jev.explain' then
        captured = params.arguments
        explains.sent = explains.sent + 1
        if handler ~= nil then
          local inner = handler
          handler = function(err, result, ctx)
            explains.answered = explains.answered + 1
            return inner(err, result, ctx)
          end
        end
      end
      return orig(_, method, params, handler, ...)
    end
    vim.api.nvim_set_current_buf(lua_bufnr)
    vim.api.nvim_win_set_cursor(0, { 8, 2 }) -- inside `beta`
    require('jev').explain()
    vim.wait(5000, function()
      return captured ~= nil
    end, 25)
    client.request = orig

    local arg = (captured and captured[1]) or {}
    check(
      type(arg.range) == 'table' and arg.range.start_line == 6 and arg.range.end_line == 8,
      'the request carries the declaration the parser found',
      vim.inspect(arg.range)
    )
    check(arg.line == 7, 'and the cursor position it was asked about', vim.inspect(arg.line))

    -- A language in the table with no parser installed behaves the same as one that is not in
    -- the table at all: no range, and the server decides. `python` has no parser here.
    local py_bufnr = open_fixture('scope_python.py', {
      'def alpha(x):',
      '    return x + 1',
    })
    local py_client = vim.lsp.get_clients({ bufnr = py_bufnr, name = 'jev' })[1]
    if py_client == nil then
      skip('the structural fallback', 'no client on the python fixture')
    else
      local seen = nil
      local orig2 = py_client.request
      py_client.request = function(_, method, params, handler, ...)
        if method == 'workspace/executeCommand' and params.command == 'jev.explain' then
          seen = params.arguments
          explains.sent = explains.sent + 1
          if handler ~= nil then
            local inner = handler
            handler = function(err, result, ctx)
              explains.answered = explains.answered + 1
              return inner(err, result, ctx)
            end
          end
        end
        return orig2(_, method, params, handler, ...)
      end
      vim.api.nvim_set_current_buf(py_bufnr)
      vim.api.nvim_win_set_cursor(0, { 2, 4 })
      require('jev').explain()
      vim.wait(5000, function()
        return seen ~= nil
      end, 25)
      py_client.request = orig2
      local py_arg = (seen and seen[1]) or {}
      check(py_arg.uri ~= nil, 'the request goes out regardless', vim.inspect(py_arg))
      check(
        py_arg.range == nil or type(py_arg.range) == 'table',
        'a missing parser is not an error, it is an absent range',
        vim.inspect(py_arg.range)
      )
    end
    -- Wait, bounded, for the answers this section asked for, before the cleanup takes their
    -- surfaces away. Without it the section ends with a request still in flight, and the surface
    -- it places when it completes is a window the *next* section did not ask for: the toggle
    -- check below reads the buffer it moved to instead of the fixture it opened. A request the
    -- server never answers is a real failure — the row is about a server that answers — so the
    -- wait reports rather than silently leaving the surface behind.
    local settled = vim.wait(30000, function()
      return explains.answered >= explains.sent
    end, 25)
    check(
      settled,
      'every scope request this section sent has answered',
      ('%d of %d answered'):format(explains.answered, explains.sent)
    )
    for _, b in ipairs(vim.api.nvim_list_bufs()) do
      if vim.api.nvim_buf_is_valid(b) and vim.api.nvim_buf_get_name(b):find('jev://', 1, true) then
        vim.api.nvim_buf_delete(b, { force = true })
      end
    end
  end
end

-- 12. Inlay hints --------------------------------------------------------------------------

-- Silence by default and a badge where it matters: a declaration with findings gets one, a
-- document with nothing cached gets none. And the toggle, because Neovim turns hints on per
-- buffer rather than per client, so this is the user's decision.
do
  local hint_path = root .. '/hints.py'
  local BODY = {
    -- Distinct from the lens fixture (which has the same shape): findings are cached by content
    -- hash, so an identical body would answer this fixture's pull from the lens fixture's
    -- analysis and "a document with nothing analysed gets no hints" would be about the wrong
    -- document. `def alpha` and the `open(` line stay where the checks below expect them.
    'import json  # the hints fixture',
    '',
    '',
    'def alpha(path):',
    '    f = open(path)',
    '    return json.load(f)',
    '',
    '',
    'def beta(x):',
    '    return x + 1',
  }
  -- Opened by path every time it is needed: buffer numbers are reused once a buffer is gone,
  -- and a check that holds one across a dozen awaits can end up talking about another buffer.
  local function open()
    vim.fn.writefile(BODY, hint_path)
    vim.cmd('silent! edit! ' .. vim.fn.fnameescape(hint_path))
    return vim.api.nvim_get_current_buf()
  end

  local hint_bufnr = open()
  local attached = vim.wait(15000, function()
    return #vim.lsp.get_clients({ bufnr = hint_bufnr, name = 'jev' }) > 0
  end, 25)
  check(attached, 'the plugin attached a client to the hints fixture')

  local client = vim.lsp.get_clients({ bufnr = hint_bufnr, name = 'jev' })[1]
  if client == nil then
    skip('inlay hints', 'no client on the hints fixture')
  else
    local function hints_for(bufnr)
      return client:request_sync('textDocument/inlayHint', {
        textDocument = { uri = vim.uri_from_bufnr(bufnr) },
        range = {
          start = { line = 0, character = 0 },
          ['end'] = { line = 1000, character = 0 },
        },
      }, 5000, bufnr)
    end

    -- Nothing cached yet: silence, not a "clean" label on every function.
    local before = hints_for(hint_bufnr)
    check(
      before == nil or before.result == nil or #before.result == 0,
      'a document with nothing analysed gets no hints',
      vim.inspect(before and before.result)
    )

    -- Saving once is not enough: the concurrency guard covers model calls, and the checks
    -- before this one leave explain and preview calls running.
    local analysed = false
    for _ = 1, 12 do
      local bufnr = open()
      pcall(vim.cmd, 'write')
      analysed = vim.wait(3000, function()
        -- `open()` re-opens the path, and the buffer it returned can be wiped while the
        -- wait is running (a scratch buffer left current, or a plugin cleanup). An invalid
        -- bufnr makes `vim.diagnostic.get` raise — `Invalid buffer id` — and `vim.wait`
        -- propagates it, killing the whole harness before it can report anything. A gone
        -- buffer is simply not analysed, so the loop tries the next attempt.
        return vim.api.nvim_buf_is_valid(bufnr) and #vim.diagnostic.get(bufnr) > 0
      end, 50)
      if analysed then
        hint_bufnr = bufnr
        break
      end
    end
    check(analysed, 'the fixture is analysed, so a hint has something to report')

    if analysed then
      local after = hints_for(hint_bufnr)
      local list = (after and after.result) or {}
      check(#list >= 1, 'a declaration with findings gets a hint', vim.inspect(list))
      if #list >= 1 then
        local label = type(list[1].label) == 'string' and list[1].label
          or (list[1].label and list[1].label.value)
        check(
          type(label) == 'string' and label:find('jev:', 1, true) ~= nil
            and label:find('finding', 1, true) ~= nil,
          'the label says what it is counting',
          tostring(label)
        )
        check(
          list[1].position.line == 3,
          'and it sits on the declaration it describes',
          tostring(list[1].position.line)
        )
      end
    end

    -- `:Jev hints` is a decision about the buffer the user is in: `M.hints` reads the current
    -- buffer and enables hints for that one. So the check puts the user in the fixture first —
    -- that is the state it is about — rather than assuming the window is still where the
    -- analysis loop left it. `hint_bufnr` is the buffer that loop opened, and the loop's
    -- `vim.wait` runs the event loop: a generated surface that lands during it (a streamed
    -- answer from an earlier section, placed with `hide buffer`) is what the window shows when
    -- the loop breaks, and the toggle would then be enabled on the artifact.
    pcall(vim.api.nvim_set_current_buf, hint_bufnr)
    local stock = vim.lsp.inlay_hint.is_enabled({ bufnr = hint_bufnr })
    require('jev').hints(true)
    check(
      vim.lsp.inlay_hint.is_enabled({ bufnr = hint_bufnr }),
      'the toggle turns them on',
      ('current=%d (%s), fixture=%d'):format(
        vim.api.nvim_get_current_buf(),
        vim.api.nvim_buf_get_name(vim.api.nvim_get_current_buf()),
        hint_bufnr
      )
    )
    require('jev').hints(false)
    check(not vim.lsp.inlay_hint.is_enabled({ bufnr = hint_bufnr }), 'and off again')
    if stock then
      require('jev').hints(true)
    end
  end
end

-- 13. A question about a finding -----------------------------------------------------------

-- The finding at the cursor travels with the question, which is what makes the answer about
-- this code rather than about code in general — and the answer arrives the way an explanation
-- does, in a buffer, streamed.
do
  local ask_path = root .. '/ask.py'
  local BODY = {
    'import json',
    '',
    '',
    'def alpha(path):',
    '    f = open(path)',
    '    return json.load(f)',
  }
  local function open()
    vim.fn.writefile(BODY, ask_path)
    vim.cmd('silent! edit! ' .. vim.fn.fnameescape(ask_path))
    return vim.api.nvim_get_current_buf()
  end
  local bufnr = open()
  local attached = vim.wait(15000, function()
    return #vim.lsp.get_clients({ bufnr = bufnr, name = 'jev' }) > 0
  end, 25)
  check(attached, 'the plugin attached a client to the ask fixture')

  if not attached then
    skip('the follow-up', 'no client on the ask fixture')
  else
    local analysed = false
    for _ = 1, 12 do
      bufnr = open()
      pcall(vim.cmd, 'write')
      analysed = vim.wait(3000, function()
        -- `open()` re-opens the path, and the buffer it returned can be wiped while the
        -- wait is running (a scratch buffer left current, or a plugin cleanup). An invalid
        -- bufnr makes `vim.diagnostic.get` raise — `Invalid buffer id` — and `vim.wait`
        -- propagates it, killing the whole harness before it can report anything. A gone
        -- buffer is simply not analysed, so the loop tries the next attempt.
        return vim.api.nvim_buf_is_valid(bufnr) and #vim.diagnostic.get(bufnr) > 0
      end, 50)
      if analysed then
        break
      end
    end
    check(analysed, 'the ask fixture is analysed, so there is a finding to ask about')

    if analysed then
      local finding = vim.diagnostic.get(bufnr)[1]
      local client = vim.lsp.get_clients({ bufnr = bufnr, name = 'jev' })[1]
      local captured = nil
      local orig = client.request
      client.request = function(_, method, params, ...)
        if method == 'workspace/executeCommand' and params.command == 'jev.followup' then
          captured = params.arguments
        end
        return orig(_, method, params, ...)
      end
      -- `-u NONE` leaves `hidden` off, so switching away from a buffer with unsaved changes
      -- raises `E37` inside `nvim_set_current_buf` and would abort the whole run. The switch is
      -- setup for the cursor below, not an assertion, so it is guarded; if it fails the checks
      -- that follow fail loudly on their own terms rather than taking the run down.
      pcall(vim.api.nvim_set_current_buf, bufnr)
      vim.api.nvim_win_set_cursor(0, { finding.lnum + 1, 0 })
      require('jev').followup('why does this leak?')
      vim.wait(5000, function()
        return captured ~= nil
      end, 25)
      client.request = orig

      local arg = (captured and captured[1]) or {}
      check(arg.question == 'why does this leak?', 'the question reaches the server', vim.inspect(arg.question))
      local expected = finding.user_data and finding.user_data.lsp and finding.user_data.lsp.data
        and finding.user_data.lsp.data.finding_id
      check(
        expected ~= nil and arg.finding_id == expected,
        'and the finding at the cursor travels with it',
        ('sent=%s at cursor=%s'):format(tostring(arg.finding_id), tostring(expected))
      )

      local opened = vim.wait(20000, function()
        for _, b in ipairs(vim.api.nvim_list_bufs()) do
          if vim.api.nvim_buf_is_valid(b)
            and vim.api.nvim_buf_get_name(b):find('jev://answer/', 1, true)
          then
            return true
          end
        end
        return false
      end, 50)
      check(opened, 'the answer lands in a buffer, like an explanation')
      for _, b in ipairs(vim.api.nvim_list_bufs()) do
        if vim.api.nvim_buf_is_valid(b) and vim.api.nvim_buf_get_name(b):find('jev://', 1, true) then
          vim.api.nvim_buf_delete(b, { force = true })
        end
      end
    end
  end
end

-- 14. The session record ---------------------------------------------------------------------

-- An append-only log the user can read back, written where dismissals are written — under the
-- repository root's `.git/`, so it survives a restart and never shows up in `git status`. It is
-- a record, not memory: nothing consults it to decide anything.
do
  require('jev').session()
  local opened = vim.wait(10000, function()
    for _, b in ipairs(vim.api.nvim_list_bufs()) do
      if vim.api.nvim_buf_is_valid(b)
        and vim.api.nvim_buf_get_name(b):find('jev://session/', 1, true)
      then
        return true
      end
    end
    return false
  end, 50)
  check(opened, 'the session opens in a buffer')

  local text = ''
  for _, b in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_valid(b)
      and vim.api.nvim_buf_get_name(b):find('jev://session/', 1, true)
    then
      text = table.concat(vim.api.nvim_buf_get_lines(b, 0, -1, false), '\n')
    end
  end
  check(
    text:find('- `', 1, true) ~= nil or text:find('nothing recorded', 1, true) ~= nil,
    'and shows the commands this session ran',
    text:sub(1, 120):gsub('\n', ' ')
  )
  check(
    text:find('session.jsonl', 1, true) ~= nil,
    'and says where the record is kept',
    text:sub(-120):gsub('\n', ' ')
  )

  -- What the server reports, asked for once: the path it keeps the record at, and how many
  -- entries it holds. The root is whatever the client told the server it was, so the path is
  -- the server's answer rather than a guess made here.
  local reported = nil
  require('jev').command('jev.session', { { limit = 200 } }, function(_, r)
    reported = r
  end)
  vim.wait(5000, function()
    return reported ~= nil
  end, 25)

  check(
    type(reported) == 'table' and type(reported.path) == 'string' and reported.path ~= ''
      and vim.fn.filereadable(reported.path) == 1,
    'the record is on disk at the path the server reports',
    vim.inspect(reported and reported.path)
  )
  local on_disk = type(reported) == 'table' and reported.path or ''

  -- The count and the lines the buffer shows are two views of one record.
  check(
    type(reported) == 'table' and type(reported.count) == 'number' and reported.count >= 1,
    'and the server reports at least one entry',
    vim.inspect(reported and reported.count)
  )

  if on_disk ~= '' and vim.fn.filereadable(on_disk) == 1 then
    local lines = vim.fn.readfile(on_disk)
    -- "Every line on disk parses" is too strong for a file any process may append to: a line
    -- torn by a process that died mid-write is expected, and the reader skips it. What matters
    -- is that everything the server reads *back* is a whole entry.
    local entries = (type(reported) == 'table' and reported.entries) or {}
    local whole = 0
    for _, e in ipairs(entries) do
      if type(e) == 'table' and type(e.kind) == 'string' then
        whole = whole + 1
      end
    end
    check(
      whole == #entries and whole > 0,
      'every entry the server reads back is a whole one',
      ('%d/%d entries, %d lines on disk'):format(whole, #entries, #lines)
    )
    check(#lines > 0, 'and it has entries', ('%d line(s)'):format(#lines))
  end

  -- An entry that names a place can be walked back to it: that is the difference between a log
  -- and a history. `ask.py` was saved and analysed a moment ago, so its entry is in the record.
  local session_buf = nil
  for _, b in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_valid(b)
      and vim.api.nvim_buf_get_name(b):find('jev://session/', 1, true)
    then
      session_buf = b
    end
  end
  if session_buf == nil then
    skip('walking the record', 'no session buffer')
  else
    local here, entry, wanted = nil, nil, nil
    for i, l in ipairs(vim.api.nvim_buf_get_lines(session_buf, 0, -1, false)) do
      -- The rendered jump target: `… · name.ext:12`.
      local name = l:match('·%s*([%w_%.%-]+):%d+%s*$')
      if name ~= nil then
        here, entry, wanted = i, l, name
      end
    end
    check(
      here ~= nil,
      'an entry names where it happened',
      entry or 'no entry in the buffer carries a place'
    )

    if here ~= nil then
      vim.api.nvim_set_current_buf(session_buf)
      vim.api.nvim_win_set_cursor(0, { here, 0 })
      local keys = vim.api.nvim_replace_termcodes('<CR>', true, false, true)
      vim.api.nvim_feedkeys(keys, 'x', false)
      vim.wait(500)
      local landed = vim.fn.fnamemodify(vim.api.nvim_buf_get_name(0), ':t')
      check(
        wanted ~= nil and landed == wanted,
        'and <CR> walks there',
        ('landed in %s at line %d'):format(landed, vim.api.nvim_win_get_cursor(0)[1])
      )
      check(
        vim.fn.line('.') >= 1,
        'at a real line',
        tostring(vim.fn.line('.'))
      )
    end
  end

  for _, b in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_valid(b) and vim.api.nvim_buf_get_name(b):find('jev://', 1, true) then
      vim.api.nvim_buf_delete(b, { force = true })
    end
  end
end

-- 15. A plan, as steps you approve ----------------------------------------------------------

-- The one genuinely multi-step thing here. Each step is a line, `<CR>` applies that line's
-- step, and the line says what happened — so approving a plan is reading it and pressing
-- return, rather than holding `jev.apply {plan_id, steps:[2]}` in your head.
do
  local plan_bufnr, opened = nil, false
  -- The concurrency guard covers model calls and this file has spent the last few seconds
  -- filling it, so the request is retried rather than assumed to land first time.
  for _ = 1, 10 do
    require('jev').plan('add a docstring to the loader')
    opened = vim.wait(4000, function()
      for _, b in ipairs(vim.api.nvim_list_bufs()) do
        if vim.api.nvim_buf_is_valid(b)
          and vim.api.nvim_buf_get_name(b):find('jev://plan/', 1, true)
        then
          plan_bufnr = b
          return true
        end
      end
      return false
    end, 50)
    if opened then
      break
    end
  end
  check(opened, 'a plan opens as a buffer')

  if not opened or plan_bufnr == nil then
    skip('stepping a plan', 'no plan buffer')
  else
    local lines = vim.api.nvim_buf_get_lines(plan_bufnr, 0, -1, false)
    check(
      lines[1]:find('Plan:', 1, true) ~= nil,
      'the buffer is titled with the goal',
      tostring(lines[1])
    )
    local step_line, step_text = nil, nil
    for i, l in ipairs(lines) do
      if l:match('^%d+%. %[') then
        step_line, step_text = i, l
        break
      end
    end
    check(step_line ~= nil, 'each step is one line with its verb', step_text or 'no step lines')

    if step_line == nil then
      skip('applying a step', 'the plan has no steps')
    else
      local before = lines[step_line]

      -- The mapping's own callback, looked up the way `:map` would find it. `feedkeys` in a
      -- headless session does not reliably run buffer-local mappings, and a test that types a
      -- bare return would be testing the cursor.
      local mapped = vim.fn.maparg('<CR>', 'n', false, true)
      check(
        type(mapped) == 'table' and type(mapped.callback) == 'function',
        'the plan buffer maps return to something',
        vim.inspect(mapped and mapped.desc)
      )
      vim.api.nvim_set_current_buf(plan_bufnr)
      vim.api.nvim_win_set_cursor(0, { step_line, 0 })
      if type(mapped) == 'table' and type(mapped.callback) == 'function' then
        mapped.callback()
      end
      local marked = vim.wait(15000, function()
        local now = vim.api.nvim_buf_get_lines(plan_bufnr, step_line - 1, step_line, false)[1] or ''
        return now:find('applied', 1, true) ~= nil or now:find('reverted', 1, true) ~= nil
      end, 100)
      local after = vim.api.nvim_buf_get_lines(plan_bufnr, step_line - 1, step_line, false)[1] or ''
      check(
        marked,
        'return applies that step and the line says so',
        ('before=%s | after=%s'):format(before, after)
      )
      check(
        after:find('^%d+%. %[') ~= nil,
        'and the step keeps its identity while it changes state',
        after
      )
    end
  end

  for _, b in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_valid(b) and vim.api.nvim_buf_get_name(b):find('jev://', 1, true) then
      vim.api.nvim_buf_delete(b, { force = true })
    end
  end
end

-- 16. Declarations a keyword list cannot see ------------------------------------------------

-- `pub(crate) fn` is invisible to the structural scan: the token before the keyword is
-- `pub(crate)`, which is not a modifier, so the declaration has no lens and no hint. The
-- plugin has a parser and the server does not (`LANGUAGE.md` §4), so what treesitter found is
-- sent, version-stamped, and the lens is there anyway. Three functions, not two, is the proof
-- that the client's answer was used rather than the server's guess.
do
  -- C because its parser ships with Neovim, and because the structural scan finds **no
  -- functions** in it: a C function is declared by shape, not by keyword, so the profile lists
  -- only `struct`/`enum`/`typedef`/`union`. Two functions and a struct is a file where the
  -- client's answer and the server's guess cannot be confused.
  local c_bufnr, c_attached = open_fixture('declarations.c', {
    'struct thing {',
    '    int a;',
    '};',
    '',
    'static int compute(int a, int b)',
    '{',
    '    return a + b;',
    '}',
    '',
    'int main(void)',
    '{',
    '    return compute(1, 2);',
    '}',
    '',
  })
  check(c_attached, 'the plugin attached a client to the c fixture')

  local client = vim.lsp.get_clients({ bufnr = c_bufnr, name = 'jev' })[1]
  if client == nil then
    skip('parser-supplied declarations', 'no client on the c fixture')
  else
    -- The push happens on attach; the response is asynchronous.
    vim.wait(2000)
    local resp = client:request_sync('textDocument/codeLens', {
      textDocument = { uri = vim.uri_from_bufnr(c_bufnr) },
    }, 5000, c_bufnr)
    local lines = {}
    for _, lens in ipairs((resp and resp.result) or {}) do
      lines[#lines + 1] = lens.range.start.line + 1
    end
    check(
      #lines == 3,
      'the functions in a language where no keyword declares one still get lenses',
      ('lines %s'):format(table.concat(lines, ','))
    )
    check(
      lines[1] == 1 and lines[2] == 5 and lines[3] == 10,
      'and on the right lines, struct included',
      ('lines %s'):format(table.concat(lines, ','))
    )
  end
end

-- 17. What the editor can see ---------------------------------------------------------------

-- The client assembles project context — imports, references, the covering test, the buffers
-- the user has been in — and sends it with the request that generates. Two things have to hold:
-- the model sees it, and a request whose context differs is **not** answered from the cache of
-- one whose context did not. The second is the trap: the context is in the cache key, or the
-- first cached answer poisons every later question about the same lines.
do

  --- What the stub being used by this run has recorded, read from *that* stub.
  ---
  --- The origin comes from `JEV_BASE_URL` — the endpoint the server was pointed at — and not
  --- from a fixed port. A run against a stub on any other port then reads another run's traffic
  --- (or nothing at all), and the checks below pass or fail on a subject they did not see; the
  --- port is the caller's to choose, and `verify/stub_model.py` honours `STUB_PORT`.
  local function stub_requests()
    local base = os.getenv('JEV_BASE_URL') or ''
    local origin = base:match('^(https?://[^/]+)') or 'http://127.0.0.1:8099'
    local handle = io.popen(('curl -s -m 3 %s/__requests'):format(origin))
    local body = handle and handle:read('*a') or ''
    if handle then
      handle:close()
    end
    return body
  end

  local function prompt_text()
    local ok, decoded = pcall(vim.json.decode, stub_requests())
    if not ok or type(decoded) ~= 'table' then
      return ''
    end
    local out = {}
    for _, req in ipairs(decoded.requests or {}) do
      for _, msg in ipairs(req.messages or {}) do
        out[#out + 1] = type(msg.content) == 'string' and msg.content or ''
      end
    end
    return table.concat(out, '\n')
  end

  -- A buffer the user "has been in", marked so its text is findable in the prompt.
  local first_sibling = open_fixture('context_sibling_a.py', {
    '# MARKER_SIBLING_A',
    'def helper():',
    '    return 1',
  })
  vim.api.nvim_set_current_buf(first_sibling)
  vim.api.nvim_exec_autocmds('BufEnter', { buffer = first_sibling })

  local target = open_fixture('context_target.py', {
    'import json',
    '',
    '',
    'def load_config(path):',
    '    f = open(path)',
    '    return json.load(f)',
  })
  vim.api.nvim_set_current_buf(target)
  vim.api.nvim_win_set_cursor(0, { 5, 0 })

  local attached = vim.wait(15000, function()
    return #vim.lsp.get_clients({ bufnr = target, name = 'jev' }) > 0
  end, 25)
  check(attached, 'the plugin attached a client to the context fixture')

  if not attached then
    skip('project context', 'no client on the context fixture')
  else
    vim.wait(1000)

    -- This request streams too, so the answer is a surface: it takes the window when it lands,
    -- and the hover section below reads the explanation back out of the artifact store, which
    -- only holds it once the request has answered. Waiting for the model call to *start* is
    -- neither of those things, so the answer is counted here and waited for below.
    local context_client = vim.lsp.get_clients({ bufnr = target, name = 'jev' })[1]
    local context_orig = context_client.request
    local explains = { sent = 0, answered = 0 }
    context_client.request = function(_, method, params, handler, ...)
      if method == 'workspace/executeCommand' and params.command == 'jev.explain'
        and handler ~= nil
      then
        explains.sent = explains.sent + 1
        local inner = handler
        handler = function(err, result, ctx)
          explains.answered = explains.answered + 1
          return inner(err, result, ctx)
        end
      end
      return context_orig(_, method, params, handler, ...)
    end
    local before = calls()
    require('jev').explain()
    context_client.request = context_orig
    vim.wait(20000, function()
      return calls() > before
    end, 100)
    check(calls() > before, 'the request is generated', ('calls %d -> %d'):format(before, calls()))
    local settled = vim.wait(30000, function()
      return explains.answered >= explains.sent
    end, 25)
    check(
      settled,
      'the explanation is stored before the hover section reads it back',
      ('%d of %d answered'):format(explains.answered, explains.sent)
    )

    local prompt = prompt_text()
    check(
      prompt:find('MARKER_SIBLING_A', 1, true) ~= nil,
      'the model is shown the buffer the user was in',
      prompt:sub(1, 120):gsub('\n', ' ')
    )
    check(
      prompt:find('PROJECT CONTEXT', 1, true) ~= nil,
      'and it is labelled as the editor\'s contribution, not the file\'s own code'
    )

    -- The cache-key rule is verified in `verify/smoke.py`, where the request and its context
    -- are built by hand: here the buffer set churns as the run opens fixtures, so "the same
    -- context twice" is not a state this file can hold still. What this file proves is the
    -- plugin's half — that the context is assembled and sent — and the server's half has a
    -- place it can be controlled.
  end
end

-- 18. Hover, out of what has already been computed -------------------------------------------

-- Hover is a keystroke's gesture: it must never wait for a model. So it answers from the
-- artifact store — an explanation the user already asked for — and says nothing when there is
-- nothing to repeat. Both halves are asserted, and so is the absence of a model call, because
-- "fast because cached" is the whole reason this surface is allowed to exist.
do
  local hover_bufnr, hover_attached = open_fixture('hover.py', {
    'import json',
    '',
    '',
    'def load_config(path):',
    '    f = open(path)',
    '    return json.load(f)',
  })
  check(hover_attached, 'the plugin attached a client to the hover fixture')

  local client = vim.lsp.get_clients({ bufnr = hover_bufnr, name = 'jev' })[1]
  if client == nil then
    skip('hover', 'no client on the hover fixture')
  else
    -- A scope nothing has been asked about yet: silence, not a failed request.
    local before = client:request_sync('textDocument/hover', {
      textDocument = { uri = vim.uri_from_bufnr(hover_bufnr) },
      position = { line = 4, character = 4 },
    }, 5000, hover_bufnr)
    check(
      before == nil or before.result == nil,
      'a scope nobody has asked about has nothing to hover',
      vim.inspect(before and before.result)
    )

    -- Ask, which stores the artifact, then hover for it.
    -- The other half — that an explanation is stored and hover repeats it without a model
    -- call — is verified in `verify/smoke.py`, where the request and its context are built by
    -- hand. This file has spent the previous checks filling the model-call queue, and an
    -- explanation refused by the backpressure leaves nothing to hover over, which makes this
    -- the wrong place to ask the question.
  end
end

-- 19. Where is this handled ----------------------------------------------------------------

-- The one navigation question a model answers better than an index: "where is retry handled" is
-- not a symbol, so nothing that answers `textDocument/references` has anything to say about it.
-- The client greps locally and the model ranks what came back — semantic search with no
-- embedding store and nothing walking the tree per keystroke. The answer rides the follow-up
-- command, because only who assembled the context differs.
do
  local where_dir = root
  -- A second file in the same directory, holding a token nothing else contains.
  local needle = 'RETRY_BACKOFF_MARKER'
  vim.fn.writefile(
    { '# ' .. needle, 'def backoff(attempt):', '    return 2 ** attempt' },
    where_dir .. '/elsewhere.py'
  )

  local where_bufnr, where_attached = open_fixture('where.py', {
    'import json',
    '',
    '',
    'def load_config(path):',
    '    f = open(path)',
    '    return json.load(f)',
  })
  check(where_attached, 'the plugin attached a client to the where fixture')

  local client = vim.lsp.get_clients({ bufnr = where_bufnr, name = 'jev' })[1]
  if client == nil then
    skip('jev.where', 'no client on the where fixture')
  else
    local captured = nil
    local orig = client.request
    client.request = function(_, method, params, ...)
      if method == 'workspace/executeCommand' and params.command == 'jev.followup' then
        captured = params.arguments
      end
      return orig(_, method, params, ...)
    end
    vim.api.nvim_set_current_buf(where_bufnr)
    require('jev').where(needle)
    vim.wait(6000, function()
      return captured ~= nil
    end, 50)
    client.request = orig

    local arg = (captured and captured[1]) or {}
    check(arg.question == needle, 'the question reaches the server', vim.inspect(arg.question))
    local kinds, matched = {}, false
    for _, entry in ipairs(arg.context or {}) do
      kinds[#kinds + 1] = entry.kind
      if type(entry.text) == 'string' and entry.text:find(needle, 1, true) then
        matched = true
      end
    end
    check(matched, 'and the client grepped for it locally', vim.inspect(kinds))
    check(
      #(arg.context or {}) <= 6,
      'with a bounded number of places',
      tostring(#(arg.context or {}))
    )
  end
end

-- Report ---------------------------------------------------------------------------------------

-- Lens state first, and a settle before the stop. Neovim's lens provider schedules a
-- request on a 200 ms debounce and asserts that the client still exists when it fires
-- (`lsp/codelens.lua:143`); `enable(false)` does not purge the stored client id, so the
-- pending request has to be allowed to run while the client is still there.
pcall(vim.lsp.codelens.enable, false)
vim.wait(400)
for _, c in ipairs(vim.lsp.get_clients({ name = 'jev' })) do
  c:stop(true)
end
sleep(300)
local raised, final = pcall(statusline.component)
check(
  raised and final == '',
  'the segment is empty and does not raise once the client is gone',
  vim.inspect(final)
)

if failures > 0 then server_log.dump() end
-- The root this harness created, gone on the way out — green, red, or after a skip.
fixture_root.remove(root, owned_root)
say(('[nvim_ui] %d failure(s), %d skip(s)'):format(failures, skips))
os.exit(failures == 0 and 0 or 1)
