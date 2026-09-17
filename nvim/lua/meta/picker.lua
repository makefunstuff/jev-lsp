--- The action picker: `<leader>ma`'s flow, and `<leader>mv`'s diff preview.
---
--- This is the plugin's own menu rather than `vim.lsp.buf.code_action`, for three things the
--- native client cannot do (`docs/ROADMAP.md` U4):
---
--- * the preview pane — `data.summary` under each title, since PROTOCOL §4 forbids a
---   model-written title and the picker is where the model's words belong;
--- * the streaming consumer — `codeAction/resolve` can take seconds and the client never says
---   so (`docs/UX.md` §1), so the spinner is fed from `$/progress` through `LspProgress`;
--- * the layout — a `disabled` action's reason is reported once, not once per channel.
---
--- The flow is the protocol's, in order:
---
--- 1. `textDocument/codeAction` with `triggerKind = 1` (Invoked; `[R4]`) and the cursor or
---    selection range. The server answers from cache (N2) — no model call — and a cold cache
---    answers with one `disabled` action carrying its reason.
--- 2. `vim.ui.select` over the actions.
--- 3. `codeAction/resolve` for the picked action only (`[R4]`), with no `workDoneToken` to
---    smoke it out: the spinner therefore takes the message from whatever the server reports
---    while the resolve is outstanding, and falls back to its own text after 250 ms.
--- 4. `vim.lsp.util.apply_workspace_edit`, or `require('meta.diff')` when the caller asked for
---    a preview.
---
--- @module 'meta.picker'

local attach = require('meta.attach')
local diff = require('meta.diff')
local statusline = require('meta.statusline')

local M = {}

--- A reason heard within this window is not repeated (PROTOCOL §4: the same text arrives as
--- `disabled.reason` *and* as `window/showMessage`; `docs/UX.md` §4: nothing notifies twice).
local WINDOW = 1000

--- Show something even when the server reports no progress at all.
local SPINNER_DELAY = 250
local SPINNER_TICK = 120
local SPINNER_MAX = 30000

local FRAMES = { '|', '/', '-', '\\' }

--- `vim.ui.select` as it ships. The stock implementation is an `inputlist` — one line per
--- item — so the preview pane degrades to an inline suffix rather than a second line.
--- Compared by identity at call time, so a picker plugin loaded later (telescope, fzf-lua,
--- snacks, or a test's stub) counts as overridden.
local stock_select = vim.ui.select

--- Text reported or received in the last second, whichever channel it came on.
local shown = {} -- text -> monotonic ms

--- The `window/showMessage` wrappers this module installed, as a set, so a handler table
--- shared between clients (or inherited from the registered config) is never wrapped twice.
--- A function cannot carry a field of its own, so identity is the marker.
local wrappers = {} -- function -> true

--- The newest `$/progress` text from a `meta` client, and when it arrived.
local progress_message, progress_at = nil, 0

--- The plugin entry. Required lazily: `meta` requires this module, so a load-time require
--- would be a cycle.
--- @return table
local function plugin()
  return require('meta')
end

--- @return integer
local function now()
  return vim.uv.now()
end

--- The `meta` client for a buffer, if there is one.
--- @param bufnr integer
--- @return vim.lsp.Client?
local function pick_client(bufnr)
  local here = vim.lsp.get_clients({ bufnr = bufnr, name = attach.NAME, method = 'textDocument/codeAction' })[1]
  return here or vim.lsp.get_clients({ bufnr = bufnr, name = attach.NAME })[1]
end

-- Reporting ------------------------------------------------------------------------------------

--- Report a reason once.
---
--- This is the only path that speaks for the picker about a `disabled` action or a resolve
--- that came back without an edit, and it is deliberately shared with the `window/showMessage`
--- wrapper below: the server sends both, in an order that is its business, and the user sees
--- one message either way.
---
--- @param text string
--- @param level? integer
--- @return boolean  whether it was reported (false: already said within `WINDOW`)
function M.reason(text, level)
  local at = shown[text]
  shown[text] = now()
  if at ~= nil and now() - at <= WINDOW then
    return false
  end
  vim.notify(text, level or vim.log.levels.WARN)
  return true
end

--- Take the dedupe hook on a client's `window/showMessage` (PROTOCOL §3.4).
---
--- The handler chain is the client's own (`Client:_resolve_handler`), so this neither touches
--- the global `vim.lsp.handlers` table nor changes what any other server does. The wrapper
--- forwards everything it does not recognise, including a message whose duplicate arrives
--- *after* the picker has already reported the same text.
---
--- @param c vim.lsp.Client
function M.watch_messages(c)
  if type(c.handlers) ~= 'table' then
    c.handlers = {}
  end
  local current = c.handlers['window/showMessage']
  if current ~= nil and wrappers[current] then
    return
  end
  local forward = current or vim.lsp.handlers['window/showMessage']
  local function wrapper(err, params, ctx)
    local text = type(params) == 'table' and params.message or nil
    if type(text) == 'string' then
      local at = shown[text]
      shown[text] = now()
      if at ~= nil and now() - at <= WINDOW then
        return
      end
    end
    if forward == nil then
      return
    end
    return forward(err, params, ctx)
  end
  wrappers[wrapper] = true
  c.handlers['window/showMessage'] = wrapper
end

-- The request ----------------------------------------------------------------------------------

--- The range the request is anchored on: the active selection in visual mode, the cursor
--- otherwise.
---
--- A selection is what `docs/UX.md` §2's `<leader>ma` means in visual mode, and the server
--- turns a multi-line range into `scope_source = "explicit"` (`PROTOCOL §4`, `scope_of`) — the
--- only way to say "this block, not the enclosing function".
---
--- The active selection is read from `getpos('v')`/`getpos('.')`, the same pair the native
--- client uses (`lsp/buf.lua`'s `range_from_selection`); the `'<`/`'>` marks are the fallback
--- for a mapping that left visual mode first. Visual mode cannot be detected, only asked for:
--- every path back to the cursor keeps the key working (docs/UX.md §6 — no keymap's meaning
--- depends on model state).
---
--- @param bufnr integer
--- @return table  `lsp.Range`
local function request_range(bufnr)
  local cursor = vim.api.nvim_win_get_cursor(0)
  local mode = vim.api.nvim_get_mode().mode
  local visual = bufnr == vim.api.nvim_get_current_buf()
    and (mode == 'v' or mode == 'V' or mode == '\22')

  local start_row, start_col, end_row, end_col
  if visual then
    local anchor = vim.fn.getpos('v')
    start_row, start_col = anchor[2], anchor[3]
    end_row, end_col = cursor[1], cursor[2] + 1
  else
    local from, to = vim.fn.getpos("'<"), vim.fn.getpos("'>")
    if from[2] == 0 or to[2] == 0 then
      local line, col = cursor[1] - 1, cursor[2]
      local point = { line = line, character = col }
      return { start = vim.deepcopy(point), ['end'] = point }
    end
    start_row, start_col = from[2], from[3]
    end_row, end_col = to[2], to[3]
  end

  -- A selection can be made backwards; normalise it, as the runtime does.
  if start_row == end_row and end_col < start_col then
    start_col, end_col = end_col, start_col
  elseif end_row < start_row then
    start_row, end_row = end_row, start_row
    start_col, end_col = end_col, start_col
  end
  start_row = math.max(1, start_row)
  end_row = math.min(math.max(1, end_row), vim.api.nvim_buf_line_count(bufnr))
  if visual and mode == 'V' then
    -- Linewise: the whole of both lines, so the range is never "one character of the last
    -- one" (`'>`'s column is a sentinel in this mode).
    start_col = 1
    end_col = #(vim.api.nvim_buf_get_lines(bufnr, end_row - 1, end_row, false)[1] or '') + 1
  end
  local clamp = function(row, col)
    local line = vim.api.nvim_buf_get_lines(bufnr, row - 1, row, false)[1] or ''
    return { line = row - 1, character = math.max(0, math.min(col - 1, #line)) }
  end
  return { start = clamp(start_row, start_col), ['end'] = clamp(end_row, end_col) }
end

--- The diagnostics the action is for. `CodeActionContext.diagnostics` is not optional in the
--- protocol, and the native path fills it from the diagnostics under the cursor; the server
--- reads findings from its own cache either way (`PROTOCOL §3.1`).
--- @param bufnr integer
--- @param range table
--- @return table[]
local function diagnostics_in(bufnr, range)
  local out = {}
  for _, d in ipairs(vim.diagnostic.get(bufnr)) do
    local wire = d.user_data and d.user_data.lsp
    if wire ~= nil and d.lnum >= range.start.line and d.lnum <= range['end'].line then
      out[#out + 1] = wire
    end
  end
  return out
end

--- Ask for the actions. One request, no model call on the server's side (N2).
--- @param c vim.lsp.Client
--- @param bufnr integer
--- @param range table
--- @param only string[]?
--- @param cb fun(actions: table[])
local function request_actions(c, bufnr, range, only, cb)
  local context = {
    -- 1 = Invoked (`vim.lsp.protocol.CodeActionTriggerKind`). Automatic hides `disabled`
    -- placeholders, and a placeholder is how the server says "still analysing" ([R4]).
    triggerKind = vim.lsp.protocol.CodeActionTriggerKind.Invoked,
    diagnostics = diagnostics_in(bufnr, range),
  }
  if only ~= nil then
    context.only = only
  end
  local sent = c:request('textDocument/codeAction', {
    textDocument = { uri = vim.uri_from_bufnr(bufnr) },
    range = range,
    context = context,
  }, function(err, result)
    if err ~= nil then
      vim.notify(
        ('meta: code actions failed: %s'):format(err.message or vim.inspect(err)),
        vim.log.levels.WARN
      )
      cb({})
      return
    end
    cb(type(result) == 'table' and result or {})
  end, bufnr)
  if not sent then
    vim.notify('meta: no server attached to this buffer', vim.log.levels.WARN)
    cb({})
  end
end

-- The list --------------------------------------------------------------------------------------

--- What the preview pane has to say about an action: `data.summary` — the model's own words,
--- attached to `data` and never to the title (PROTOCOL §4) — else the `disabled` reason, which
--- is the only thing a placeholder has to show.
--- @param action table
--- @return string?
local function detail_of(action)
  local data = action.data
  local summary = type(data) == 'table' and data.summary or nil
  if type(summary) == 'string' and vim.trim(summary) ~= '' then
    return vim.trim(summary)
  end
  local reason = action.disabled and action.disabled.reason or nil
  if type(reason) == 'string' and vim.trim(reason) ~= '' then
    return vim.trim(reason)
  end
  return nil
end

--- The list entry: the deterministic title (PROTOCOL §4), and the detail under it when the
--- picker can render a second line.
--- @param item meta.PickerItem
--- @param preview boolean  whether `vim.ui.select` has been overridden
--- @return string
local function format_item(item, preview)
  local action = item.action
  local title = action.title or '(untitled)'
  if action.disabled ~= nil then
    title = title .. ' (disabled)'
  end
  local detail = detail_of(action)
  if detail == nil then
    return title
  end
  if preview then
    return title .. '\n    ' .. detail
  end
  return title .. '  — ' .. detail
end

--- Put the actions in front of the user and hand the choice back.
---
--- `vim.ui.select` is the extension point (`[R4]`: "the main extension point is
--- `vim.ui.select`"), so the menu works with any picker and with none. With the stock
--- `vim.ui.select` the detail is an inline suffix; with a picker plugin it is a second line
--- under the title. When `vim.ui.select` is missing entirely the menu is reported instead of
--- swallowed — the key keeps doing something and says why (`docs/UX.md` §6).
---
--- @param items meta.PickerItem[]
--- @param on_choice fun(item: meta.PickerItem?)
--- @return boolean  whether a chooser was asked
function M.select(items, on_choice)
  local chooser = vim.ui.select
  if type(chooser) ~= 'function' then
    local titles = {}
    for _, item in ipairs(items) do
      titles[#titles + 1] = format_item(item, false)
    end
    vim.notify(
      ('meta: no picker (vim.ui.select is unset); actions: %s'):format(table.concat(titles, ' | ')),
      vim.log.levels.INFO
    )
    return false
  end
  local preview = chooser ~= stock_select
  local ok, err = pcall(chooser, items, {
    prompt = 'Code actions:',
    kind = 'codeaction',
    format_item = function(item)
      return format_item(item, preview)
    end,
  }, function(choice)
    on_choice(choice)
  end)
  if not ok then
    vim.notify(('meta: the picker failed: %s'):format(err), vim.log.levels.ERROR)
    return false
  end
  return true
end

-- Resolve and apply -----------------------------------------------------------------------------

--- The newest progress text, when it arrived at or after `since`. A `codeAction/resolve` is
--- not a `WorkDoneProgressParams`, so the server has no token of ours to report under: the
--- spinner echoes whatever the same client said while the resolve was outstanding, and
--- attributes none of it to a request (PROTOCOL §3.5).
--- @param since integer
--- @return string?
local function progress_since(since)
  if progress_message ~= nil and progress_at >= since then
    return progress_message
  end
  return nil
end

--- The message line exists only when something is attached to render it. Without a UI — a
--- headless run, a script — echoing would add escape sequences to stdout and show nobody
--- anything, so the spinner quietly does nothing but keep the in-flight count.
--- @return boolean
local function can_echo()
  return #vim.api.nvim_list_uis() > 0
end

--- Show that a resolve is outstanding, and stop when it is done.
---
--- Two surfaces, both `docs/UX.md`: the in-flight item is what `statusline.component()`
--- reports (§1, §6 — "the statusline is the only place work is advertised"), and the message
--- line carries *what* is being generated, which a statusline segment cannot. Nothing is ever
--- written into a buffer the user types in (§6).
---
--- @param token string
--- @param label string
--- @return fun()  stop
local function spin(token, label)
  statusline.track(token, label)
  local started = now()
  local frame, stopped = 0, false
  local timer = vim.uv.new_timer()
  local function stop()
    if stopped then
      return
    end
    stopped = true
    if timer ~= nil then
      timer:stop()
      pcall(function()
        timer:close()
      end)
    end
    statusline.untrack(token)
    if can_echo() then
      vim.api.nvim_echo({ { '' } }, false, {}) -- clear the message line
    end
  end
  if timer == nil then
    return stop
  end
  timer:start(SPINNER_DELAY, SPINNER_TICK, vim.schedule_wrap(function()
    if now() - started > SPINNER_MAX then
      stop()
      return
    end
    frame = frame + 1
    if not can_echo() then
      return
    end
    local text = progress_since(started) or ('meta: generating %s'):format(label)
    vim.api.nvim_echo({ { ('%s %s'):format(text, FRAMES[frame % #FRAMES + 1]) } }, false, {})
  end))
  return stop
end

--- Apply an edit, and say so when it does not land.
---
--- A refusal — a `TextDocumentEdit` whose version moved under it — comes back as `false` and is
--- already reported by the client (`vim/lsp/util.lua`), so nothing is added here.
--- @param c vim.lsp.Client
--- @param edit table
--- @return boolean
local function apply(c, edit)
  local ok, result = pcall(vim.lsp.util.apply_workspace_edit, edit, c.offset_encoding)
  if not ok then
    vim.notify(('meta: the edit was not applied: %s'):format(result), vim.log.levels.ERROR)
    return false
  end
  return result ~= false
end

--- Do what the action says — with whatever resolve filled in.
--- @param c vim.lsp.Client
--- @param bufnr integer
--- @param action table
--- @param opts meta.PickerOpts
local function finish(c, bufnr, action, opts)
  if action.disabled ~= nil then
    M.reason(action.disabled.reason or ('meta: %s is unavailable'):format(action.title or 'the action'))
    return
  end
  if action.edit ~= nil then
    if opts.preview then
      diff.propose(action.edit, { bufnr = bufnr, encoding = c.offset_encoding })
    else
      apply(c, action.edit)
    end
    return
  end
  if action.command ~= nil then
    local command = type(action.command) == 'table' and action.command or action
    c:exec_cmd(command, { bufnr = bufnr, client_id = c.id })
    return
  end
  vim.notify(
    ('meta: %s produced no change'):format(action.title or 'the action'),
    vim.log.levels.INFO
  )
end

--- Run one action: resolve it when that is needed, then apply what came back.
--- @param c vim.lsp.Client
--- @param bufnr integer
--- @param action table
--- @param opts meta.PickerOpts
local function run(c, bufnr, action, opts)
  if action.disabled ~= nil then
    M.reason(action.disabled.reason or ('meta: %s is unavailable'):format(action.title or 'the action'))
    return
  end
  if action.edit ~= nil or action.command ~= nil then
    return finish(c, bufnr, action, opts)
  end
  if not c:supports_method('codeAction/resolve') then
    vim.notify(
      ('meta: %s resolved to nothing and the server cannot resolve it'):format(action.title or 'the action'),
      vim.log.levels.INFO
    )
    return
  end
  local token = plugin().issue_token()
  local stop = spin(token, action.title or 'the action')
  local sent = c:request('codeAction/resolve', action, function(err, resolved)
    stop()
    plugin().release_token(token)
    if err ~= nil then
      -- The native menu's behaviour on a resolve error ([R4]) and PROTOCOL §3.1's rule that a
      -- timeout returns the action unchanged: report, apply nothing.
      vim.notify(('%s: %s'):format(err.code or 'error', err.message or ''), vim.log.levels.ERROR)
      return
    end
    finish(c, bufnr, resolved or action, opts)
  end, bufnr)
  if not sent then
    stop()
    plugin().release_token(token)
    vim.notify('meta: no server attached to this buffer', vim.log.levels.WARN)
  end
end

--- Choose from `actions` and run the pick.
--- @param c vim.lsp.Client
--- @param bufnr integer
--- @param actions table[]
--- @param opts meta.PickerOpts
function M.choose(c, bufnr, actions, opts)
  if #actions == 0 then
    vim.notify('meta: no code actions available', vim.log.levels.INFO)
    return
  end
  local items = {}
  for i, action in ipairs(actions) do
    items[i] = { action = action, index = i }
  end
  M.select(items, function(choice)
    if choice == nil then
      return -- dismissed: nothing is applied and nothing is said
    end
    run(c, bufnr, choice.action, opts)
  end)
end

-- Entry point -----------------------------------------------------------------------------------

--- @class meta.PickerItem
--- @field action table   `lsp.CodeAction` as it came off the wire, `data` included
--- @field index integer

--- @class meta.PickerOpts
--- @field bufnr? integer    Buffer to act on; default: the current one
--- @field preview? boolean  Open the resolved edit as a diff instead of applying it
--- @field only? string[]    `CodeActionKind`s to offer (PROTOCOL §2: prefix-matched)

--- The `<leader>ma` flow. `<leader>mv` is the same flow with `preview = true`.
--- @param opts? meta.PickerOpts
function M.action(opts)
  opts = opts or {}
  local bufnr = opts.bufnr or vim.api.nvim_get_current_buf()
  local c = pick_client(bufnr)
  if c == nil then
    vim.notify('meta: no server attached to this buffer', vim.log.levels.WARN)
    return
  end
  local range = request_range(bufnr)
  request_actions(c, bufnr, range, opts.only, function(actions)
    M.choose(c, bufnr, actions, opts)
  end)
end

--- Consume `$/progress` for the spinner, and keep the dedupe hook on every `meta` client.
---
--- Idempotent: one augroup, cleared and recreated; `watch_messages` refuses to wrap twice.
function M.install()
  local group = vim.api.nvim_create_augroup('meta.picker', { clear = true })
  vim.api.nvim_create_autocmd('LspProgress', {
    group = group,
    pattern = '*',
    desc = 'meta: what a resolve is doing, when the server says so',
    callback = function(ev)
      local data = ev.data or {}
      local c = data.client_id and vim.lsp.get_client_by_id(data.client_id)
      if c == nil or c.name ~= attach.NAME then
        return
      end
      local value = type(data.params) == 'table' and data.params.value or nil
      if type(value) ~= 'table' then
        return
      end
      local text
      if value.kind == 'report' then
        text = value.message
      elseif value.kind == 'begin' then
        text = value.title or value.message
      end
      if type(text) == 'string' and text ~= '' then
        progress_message, progress_at = text, now()
      end
    end,
  })
  vim.api.nvim_create_autocmd('LspAttach', {
    group = group,
    desc = 'meta: report a resolve reason once, not once per channel',
    callback = function(ev)
      local id = ev.data and ev.data.client_id
      local c = id and vim.lsp.get_client_by_id(id)
      if c ~= nil and c.name == attach.NAME then
        M.watch_messages(c)
      end
    end,
  })
  for _, c in ipairs(vim.lsp.get_clients({ name = attach.NAME })) do
    M.watch_messages(c)
  end
end

return M
