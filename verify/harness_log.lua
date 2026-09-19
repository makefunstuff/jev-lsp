-- Shared by the Lua harnesses: surface what the server said when a run goes red.
--
-- A dead model endpoint is indistinguishable from a product defect at the other end of the
-- LSP connection: the client just gets no findings and no edit. Twice today a stale stub
-- made `nvim_ui_test.lua` report "no quickfix.jev action after 30 s", which reads as a
-- server bug and is not one. The server already says what happened, through
-- `window/logMessage`; this captures it and prints it on failure.
--
--   local log = dofile(root .. '/verify/harness_log.lua')
--   log.capture()
--   ...
--   if failures > 0 then log.dump() end

local M = {}

local captured = {}

--- Wrap the client's `window/logMessage` handler so the messages are kept as well as logged.
--- The stock handler still runs, so `:LspLog` is unaffected.
function M.capture()
  local previous = vim.lsp.handlers['window/logMessage']
  vim.lsp.handlers['window/logMessage'] = function(err, result, ctx)
    captured[#captured + 1] = tostring(result and result.message or result)
    if previous then
      return previous(err, result, ctx)
    end
  end
end

--- Print everything the server logged. Called only when something failed, so a green run
--- stays one line per check.
function M.dump(limit)
  limit = limit or 12
  if #captured == 0 then
    io.stdout:write('      (the server logged nothing)\n')
    io.stdout:flush()
    return
  end
  local from = math.max(1, #captured - limit + 1)
  for i = from, #captured do
    io.stdout:write('      server: ' .. captured[i] .. '\n')
  end
  io.stdout:flush()
end

function M.count()
  return #captured
end

return M
