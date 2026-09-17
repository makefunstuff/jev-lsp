--- The statusline segment: what is running, and the budget when it is getting close.
---
--- `docs/UX.md` §1 puts every progress signal here and nowhere else, and §6 makes it a rule:
--- "the statusline is the only place work is advertised; no spinner text is inserted into a
--- buffer the user types in". The segment is a plain function, so it fits any statusline:
---
--- ```lua
--- vim.o.statusline = '%{%v:lua.require"meta.statusline".component()%}'
--- ```
---
--- Nothing here blocks. `component()` reads counters and returns a string; the one request it
--- makes — `meta.status` (PROTOCOL §6) — goes out in the background, at most once per
--- `POLL_MS`, and only while work is in flight. With no `meta` client attached it does
--- nothing at all.
---
--- @module 'meta.statusline'

local attach = require('meta.attach')

local M = {}

--- At most one `meta.status` poll per 5 s. `meta.status` is a cache read, not a model call,
--- but a statusline that polls on every redraw is a busy loop with extra steps.
local POLL_MS = 5000

--- Above half of a ceiling the numbers earn the slot (`docs/UX.md` §3.4 shows the FIM count
--- on exactly that rule).
local PRESSURE = 0.5

--- Work in flight, by progress token. `$/progress` `begin` adds and `end` removes (PROTOCOL
--- §3.5 path 2); the picker adds its own `resolve` under a token it minted (path 1).
local inflight = {} -- token -> { label = string?, started = integer }

--- The `meta.status` poll: its token, whether one is outstanding, and when the last one went
--- out. Progress under `poll_token` is not work — counting it would make the poll feed
--- itself and the segment would never settle back to empty.
local poll_token = nil
local polling = false
local polled_at = 0

--- The budget half of the segment, from the last poll: `{ ratio, used, limit, label }`.
local budget = nil

--- Monotonic milliseconds: immune to wall-clock jumps, and what `vim.uv` timers use.
--- @return integer
local function now()
  return vim.uv.now()
end

--- The `meta` client to poll: this buffer's, else any.
--- @return vim.lsp.Client?
local function client()
  local bufnr = vim.api.nvim_get_current_buf()
  return vim.lsp.get_clients({ bufnr = bufnr, name = attach.NAME })[1]
    or vim.lsp.get_clients({ name = attach.NAME })[1]
end

--- The plugin entry. Required lazily: `meta` requires this module, so a load-time require
--- would be a cycle.
--- @return table
local function plugin()
  return require('meta')
end

-- The segment ------------------------------------------------------------------------------

--- The ceilings `docs/UX.md` §4 makes visible, as ratios. The largest is the pressure.
--- @param b table  `result.budget` from `meta.status`
--- @return { ratio: number, used: number, limit: number, label: string }?
local function measure(b)
  local best
  for _, d in ipairs({
    { used = b.calls_last_minute, limit = b.limit_per_minute, label = 'calls' },
    { used = b.calls_last_hour, limit = b.limit_per_hour, label = 'calls/h' },
    { used = b.tokens_used, limit = b.limit_tokens, label = 'tokens' },
  }) do
    if type(d.used) == 'number' and type(d.limit) == 'number' and d.limit > 0 then
      local ratio = d.used / d.limit
      if best == nil or ratio > best.ratio then
        best = { ratio = ratio, used = d.used, limit = d.limit, label = d.label }
      end
    end
  end
  return best
end

--- @return string
local function render()
  if client() == nil then
    return ''
  end
  local count = 0
  for _ in pairs(inflight) do
    count = count + 1
  end
  if count == 0 then
    return ''
  end
  local segment = ('meta: %d running'):format(count)
  if budget ~= nil and budget.ratio > PRESSURE then
    segment = segment
      .. (' · %d/%d %s'):format(budget.used, budget.limit, budget.label)
  end
  return segment
end

--- The segment. `''` when no `meta` client is attached, nothing is in flight, or the numbers
--- are not there yet.
---
--- A statusline expression runs on every redraw, in the middle of someone else's error
--- handling, so it is total by construction and still guarded: a raise here would be printed
--- on every redraw.
---
--- @return string
function M.component()
  local ok, segment = pcall(render)
  if not ok then
    return ''
  end
  return segment
end

-- Feeding it ---------------------------------------------------------------------------------

--- Note an item of work starting, for work this module's caller owns rather than the server
--- (the picker's `codeAction/resolve`, PROTOCOL §3.1).
--- @param token string
--- @param label? string
function M.track(token, label)
  inflight[token] = { label = label, started = now() }
end

--- Note an item of work finishing. Unknown tokens are ignored.
--- @param token string
function M.untrack(token)
  inflight[token] = nil
end

--- Fold one `$/progress` notification in (PROTOCOL §3.5). The token is not attributed to any
--- request — that is the tracker in `meta` — only counted, so the segment answers "what is
--- running".
--- @param params table  `ProgressParams`
function M.note(params)
  if type(params) ~= 'table' then
    return
  end
  local token, value = params.token, params.value
  if token == nil or type(value) ~= 'table' then
    return
  end
  if poll_token ~= nil and token == poll_token then
    return
  end
  if value.kind == 'begin' then
    inflight[token] = { label = value.title or value.message, started = now() }
  elseif value.kind == 'report' then
    local item = inflight[token]
    if item ~= nil then
      item.message = value.message or item.message
    end
  elseif value.kind == 'end' then
    inflight[token] = nil
  end
end

--- Fold a `meta.status` Result in (PROTOCOL §6/§7). Public because anything that already has
--- the numbers should not ask for them again.
--- @param result table?  the `Result` envelope
--- @return boolean  whether a budget reading was taken
function M.note_status(result)
  local b = type(result) == 'table' and result.budget or nil
  if type(b) ~= 'table' then
    return false
  end
  local measured = measure(b)
  if measured == nil then
    return false
  end
  measured.at = now()
  budget = measured
  return true
end

--- Ask for the numbers, if it is worth asking and legal to ask (PROTOCOL §3.5, path 1: the
--- token rides in the request, so no `window/workDoneProgress/create` round trip).
---
--- Rate-limited, skipped with nothing in flight, and never awaited.
--- @return boolean  whether a request went out
function M.poll()
  if polling or next(inflight) == nil then
    return false
  end
  local c = client()
  if c == nil then
    return false
  end
  local t = now()
  if t - polled_at < POLL_MS then
    return false
  end
  local token = plugin().issue_token()
  poll_token, polling, polled_at = token, true, t
  local ok = c:request('workspace/executeCommand', {
    command = 'meta.status',
    arguments = {},
    workDoneToken = token,
  }, function(err, result)
    polling = false
    poll_token = nil
    plugin().release_token(token)
    if err == nil then
      M.note_status(result)
    end
  end)
  if not ok then
    polling = false
    poll_token = nil
    plugin().release_token(token)
  end
  return ok
end

--- Consume `$/progress` (counters) and, while anything is running, the one poll that keeps
--- the budget half fresh.
---
--- Idempotent: one augroup, cleared and recreated.
function M.install()
  local group = vim.api.nvim_create_augroup('meta.statusline', { clear = true })
  vim.api.nvim_create_autocmd('LspProgress', {
    group = group,
    pattern = '*',
    desc = 'meta: in-flight count and budget pressure for the statusline segment',
    callback = function(ev)
      local data = ev.data or {}
      local c = data.client_id and vim.lsp.get_client_by_id(data.client_id)
      if c == nil or c.name ~= attach.NAME then
        return
      end
      M.note(data.params)
      -- Never from inside the notification: the request goes out on the main loop, once the
      -- handler that triggered it has returned.
      vim.schedule(function()
        M.poll()
      end)
    end,
  })
end

return M
