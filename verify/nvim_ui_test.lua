-- Live Neovim: the plugin's own interactive surfaces (docs/ROADMAP.md U4) — the picker, the
-- proposal diff, and the statusline segment.
--
--   META_LSP_BIN=/path/to/meta-lsp nvim --headless -u NONE -l verify/nvim_ui_test.lua
--   META_BASE_URL=http://127.0.0.1:8099/v1   # optional: a model endpoint (verify/stub_model.py)
--
-- META_LSP_BIN is the server binary (required; unset is a usage error, exit 2).
-- META_BASE_URL points the server at a model endpoint. With one, the checks that need a real
-- proposal are hard: the stub is deterministic. Without one the server still lists actions,
-- so those checks report SKIP rather than FAIL — nothing here pretends a missing endpoint is
-- a passing test.
--
-- Checks, in order:
--
--   0. the surfaces this unit does not own still work: every default keymap is mapped,
--      `<leader>ma`/`<leader>mv` are mapped in visual mode too, `:Meta status` dispatches, and
--      `meta.status` round-trips with the server reporting under the token the plugin issued;
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

local BIN = os.getenv('META_LSP_BIN')
if BIN == nil or BIN == '' then
  io.stderr:write('nvim_ui_test: META_LSP_BIN is required (path to the meta-lsp binary)\n')
  io.stderr:flush()
  os.exit(2)
end
local MODEL = os.getenv('META_BASE_URL') or ''

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

local root = os.getenv('META_ROOT')
if root == nil or root == '' then
  root = vim.fn.tempname() .. '-meta-ui'
end
vim.fn.mkdir(root, 'p')

say('[nvim_ui] plugin  : ' .. PLUGIN)
say('[nvim_ui] server  : ' .. BIN)
say('[nvim_ui] model   : ' .. (MODEL ~= '' and MODEL or '(none: META_BASE_URL unset)'))
say('[nvim_ui] fixtures: ' .. root)

-- 6a. The segment with no client at all -----------------------------------------------
--
-- Run first, before any client exists: "does nothing at all when no `meta` client is attached"
-- is a property of the module, and this is the only moment it is trivially true.

local statusline = require('meta.statusline')
do
  local raised, value = pcall(statusline.component)
  check(raised and type(value) == 'string', 'statusline.component() returns a string, and does not raise, with no client attached', value)
  check(value == '', 'statusline.component() is empty with no client attached', vim.inspect(value))
end

local meta = require('meta')
meta.setup({ cmd = { BIN } })
vim.cmd('filetype on')

-- The surface this unit touched: `<leader>ma` changes implementation and `<leader>mv` is new.
-- Everything else in `docs/UX.md` §2 has to be exactly where it was, and `:Meta` has to reach
-- the same functions it always did.
do
  local expected = {
    '<leader>ma',
    '<leader>mv',
    '<leader>mp',
    '<leader>me',
    '<leader>mr',
    '<leader>mt',
    '<leader>md',
    '<leader>ms',
    '<leader>mx',
    '<leader>mu',
    '<leader>mS',
    '<leader>mG',
  }
  local missing = {}
  for _, lhs in ipairs(expected) do
    if vim.fn.maparg(lhs, 'n') == '' then
      missing[#missing + 1] = lhs
    end
  end
  check(#missing == 0, 'every default keymap is still mapped, and <leader>mv is new in this unit', vim.inspect(missing))
  check(
    vim.fn.maparg('<leader>ma', 'x') ~= '' and vim.fn.maparg('<leader>mv', 'x') ~= '',
    'the picker and its diff variant are mapped in visual mode'
  )
end

--- @return integer bufnr, boolean attached
local function open_fixture(name, body)
  local path = root .. '/' .. name
  vim.fn.writefile(body, path)
  vim.cmd('silent edit ' .. vim.fn.fnameescape(path))
  local bufnr = vim.api.nvim_get_current_buf()
  local attached = vim.wait(15000, function()
    return #vim.lsp.get_clients({ bufnr = bufnr, name = 'meta' }) > 0
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
--- cached finding (`quickfix.meta`, PROTOCOL §4.1) — which is what "a findable issue" means.
local function actions_and_issue(client, params, bufnr)
  local actions = request(client, 'textDocument/codeAction', params, 10000, bufnr) or {}
  for _, action in ipairs(actions) do
    if kind_of(action):find('^quickfix%.meta') ~= nil then
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
  local dispatched, dispatch_err = pcall(vim.cmd, 'Meta status')
  check(dispatched, ':Meta status dispatches through the command surface', dispatch_err)

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
  meta.status(function(err, result)
    outcome, answered = { err = err, result = result }, true
  end)
  vim.wait(15000, function()
    return answered
  end, 25)
  check(
    answered and outcome.err == nil and type(outcome.result) == 'table' and outcome.result.ok == true,
    'meta.status round-trips through the plugin command path',
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
    ':Meta status reports the Result envelope it got back',
    vim.inspect(reported)
  )
end

local client = vim.lsp.get_clients({ bufnr = bufnr, name = 'meta' })[1]
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
      ('no quickfix.meta action within 5 s of saving (%d verb action(s) offered); META_BASE_URL is unset, so no endpoint was promised')
        :format(#actions)
    )
  else
    fail(label, 'no quickfix.meta action after 30 s: ' .. vim.inspect(vim.tbl_map(kind_of, actions)))
  end
else
  local before = lines(bufnr)
  vim.ui.select = function(items, opts, cb)
    captured.items, captured.opts = items, opts
    cb(items[1], 1)
  end
  require('meta.picker').action()
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
  require('meta.picker').action({ bufnr = pane_bufnr })
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
      require('meta.picker').action({ bufnr = pane_bufnr })
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
      local surviving, chooser_err = pcall(require('meta.picker').action, { bufnr = pane_bufnr })
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
  local lhs = vim.api.nvim_replace_termcodes('<leader>ma', true, false, true)
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
local reject_client = vim.lsp.get_clients({ bufnr = reject_bufnr, name = 'meta' })[1]
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
  require('meta.picker').action({ bufnr = reject_bufnr })
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
    messages(nil, { type = 3, message = 'meta: uitest control message' }, { method = 'window/showMessage', client_id = reject_client.id })
    check(reports('meta: uitest control message') == 1, 'an unrelated window/showMessage is still shown')
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

  local diff = require('meta.diff')
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
    local leftover = {}
    for _, m in ipairs(vim.api.nvim_buf_get_keymap(bufnr, 'n')) do
      leftover[#leftover + 1] = m.lhs
    end
    check(#leftover == 0, 'no keymap the preview added is left behind', vim.inspect(leftover))
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
  local token = 'meta:uitest:1'
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
  vim.o.statusline = "%{%v:lua.require'meta.statusline'.component()%}"
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
  local ok = require('meta.picker').select(items, function(choice)
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
-- `meta.plan` sent a map, so the transport rejected it before the command ran: every press of
-- `:Meta plan` answered `invalid type: map, expected a sequence`. The entry points are driven
-- here with the request captured, so the shape is checked without spending a model call.
do
  local client = vim.lsp.get_clients({ name = 'meta' })[1]
  if client == nil then
    skip('command argument shapes', 'no meta client attached')
  else
    local captured = {}
    local orig = client.request
    client.request = function(_, method, params)
      if method == 'workspace/executeCommand' then
        captured[#captured + 1] = params
      end
      return true
    end
    local plugin = require('meta')
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

-- `meta.explain` asks for a stream, and the server reports the text *so far* under the token
-- the plugin issued (PROTOCOL §3.5). What is asserted is what the user sees: the artifact
-- buffer holds more than one distinct state while the request is still open, so the answer is
-- being written rather than appearing at the end — and the buffer the partials filled is the
-- buffer the finished artifact lands in, so nothing moves when it completes.
do
  if #vim.lsp.get_clients({ name = 'meta', bufnr = reject_bufnr }) == 0 then
    skip('the streamed explanation', 'no meta client attached to the fixture')
  else
    local function stream_buffers()
      local found = {}
      for _, b in ipairs(vim.api.nvim_list_bufs()) do
        if vim.api.nvim_buf_is_valid(b)
          and vim.api.nvim_buf_get_name(b):find('meta://', 1, true) ~= nil
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
    require('meta').explain()

    local deadline = vim.uv.now() + 30000
    local finished, finished_name = false, nil
    while vim.uv.now() < deadline do
      for _, b in ipairs(new_scratch_buffers()) do
        local text = table.concat(vim.api.nvim_buf_get_lines(b, 0, -1, false), '\n')
        if text ~= '' then
          seen[text] = true
        end
        local name = vim.api.nvim_buf_get_name(b)
        if name:find('meta://explanation', 1, true) == 1 then
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
        finished_name:find('meta://explanation/', 1, true) == 1,
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

-- Report ---------------------------------------------------------------------------------------

for _, c in ipairs(vim.lsp.get_clients({ name = 'meta' })) do
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
say(('[nvim_ui] %d failure(s), %d skip(s)'):format(failures, skips))
os.exit(failures == 0 and 0 or 1)
