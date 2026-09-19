--- `:checkhealth jev`.
---
--- Discovered as `lua/jev/health.lua` on 'runtimepath', which is what this plugin's `nvim/`
--- directory is: `vim.health` finds it with `nvim_get_runtime_file('lua/**/jev/health.lua')`.
---
--- It reports the four things that decide whether the plugin can work at all: a Neovim new
--- enough for the surface it uses, a server binary that can be executed, a client that is
--- actually attached, and the attach pass that the built-in path does not provide.
---
--- @module 'jev.health'

local M = {}

--- The oldest Neovim the ambient surface is verified against: pull diagnostics and
--- arbitrary-token `$/progress` (`docs/research/nvim-lsp-surface.md`).
local MIN_VERSION = { 0, 12 }

function M.check()
  local health = vim.health
  local attach = require('jev.attach')
  health.start('jev')

  local v = vim.version()
  local version = ('%d.%d.%d'):format(v.major, v.minor, v.patch)
  if v.major > MIN_VERSION[1] or (v.major == MIN_VERSION[1] and v.minor >= MIN_VERSION[2]) then
    health.ok(('Neovim %s (>= %d.%d)'):format(version, MIN_VERSION[1], MIN_VERSION[2]))
  else
    health.error(
      ('Neovim %s is older than %d.%d; the LSP surface this plugin uses is not verified there')
        :format(version, MIN_VERSION[1], MIN_VERSION[2])
    )
  end

  local cfg = vim.lsp.config[attach.NAME]
  if not cfg then
    health.warn('not configured: require("jev").setup({ cmd = { "jev-lsp" } }) was never called')
  else
    local cmd = cfg.cmd
    if type(cmd) == 'table' and cmd[1] then
      if vim.fn.executable(cmd[1]) == 1 then
        health.ok(('server binary: %s (%s)'):format(cmd[1], vim.fn.exepath(cmd[1])))
      else
        health.error(('server binary is not executable: %s'):format(cmd[1]))
      end
    elseif type(cmd) == 'function' then
      health.info('server command is a function; the executable check does not apply')
    else
      health.error('no server command configured')
    end
    local enabled = vim.tbl_get(cfg.settings or {}, 'jev', 'enabled')
    if enabled == false then
      health.warn('enabled = false: :Jev stop is in effect, the server issues no model calls')
    else
      health.info('enabled = true')
    end
  end

  local clients = vim.lsp.get_clients({ name = attach.NAME })
  if #clients == 0 then
    health.warn(
      'no jev client attached (open a file; the attach pass covers the buffers the FileType path misses)'
    )
  end
  for _, c in ipairs(clients) do
    local encoding = c.server_capabilities and c.server_capabilities.positionEncoding
    if encoding == 'utf-8' then
      health.ok(('client %d: positionEncoding = utf-8, root = %s'):format(c.id, tostring(c.root_dir)))
    else
      health.error(
        ('client %d: positionEncoding = %s, expected utf-8 (PROTOCOL N1 — every range is a byte offset)')
          :format(c.id, tostring(encoding))
      )
    end
  end

  -- The decide tier: which endpoint answers the rules pass's questions, and whether the
  -- credential the default one needs is present. Asked of the server, because it is the only
  -- end that saw the environment it runs in — the client never sees `JEV_DECIDE_*`.
  do
    local status = nil
    for _, c in ipairs(clients) do
      local answered = false
      c:request('workspace/executeCommand', { command = 'jev.status', arguments = {} }, function(_, result)
        status, answered = result, true
      end, 0)
      vim.wait(5000, function()
        return answered
      end, 25)
      if status ~= nil then
        break
      end
    end
    local decide = vim.tbl_get(status or {}, 'models', 'decide')
    if decide == nil then
      health.warn('decide tier unknown: no attached jev client answered jev.status')
    else
      health.ok(
        ('decide tier: %s at %s (wire %s)'):format(
          tostring(decide.model),
          tostring(decide.base_url),
          tostring(decide.wire)
        )
      )
      local key = vim.env.TYPESAFE_API_KEY
      local has_key = key ~= nil and key ~= ''
      -- `TYPESAFE_API_KEY` is the variable the decide tier reads by default (`config.rs`); a
      -- configured `api_key_env` would name another, and only the endpoint it belongs to knows.
      if type(decide.base_url) == 'string' and decide.base_url:find('api.typesafe.ai', 1, true) then
        if has_key then
          health.ok('TYPESAFE_API_KEY is set')
        else
          health.warn('TYPESAFE_API_KEY is not set: the default decide endpoint refuses every call')
        end
      else
        health.info(
          ('TYPESAFE_API_KEY is %s; the configured decide endpoint does not require it')
            :format(has_key and 'set' or 'not set')
        )
      end
    end
  end

  -- docs/LANGUAGE.md §1: `vim.lsp.enable` attaches only on FileType, which does not fire for
  -- a file Neovim cannot identify, so this pass is what makes "every file" true.
  local events = { 'BufReadPost', 'BufNewFile', 'BufWinEnter' }
  local missing = {}
  for _, event in ipairs(events) do
    local found = false
    for _, ac in ipairs(vim.api.nvim_get_autocmds({ event = event })) do
      if ac.group_name == attach.AUGROUP then
        found = true
        break
      end
    end
    if not found then
      missing[#missing + 1] = event
    end
  end
  if #missing == 0 then
    health.ok(('attach pass installed on %s'):format(table.concat(events, ', ')))
  else
    health.error(
      ('attach pass incomplete: %s not covered, so unidentified files are never served')
        :format(table.concat(missing, ', '))
    )
  end
end

return M
