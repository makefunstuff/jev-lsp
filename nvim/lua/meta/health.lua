--- `:checkhealth meta`.
---
--- Discovered as `lua/meta/health.lua` on 'runtimepath', which is what this plugin's `nvim/`
--- directory is: `vim.health` finds it with `nvim_get_runtime_file('lua/**/meta/health.lua')`.
---
--- It reports the four things that decide whether the plugin can work at all: a Neovim new
--- enough for the surface it uses, a server binary that can be executed, a client that is
--- actually attached, and the attach pass that the built-in path does not provide.
---
--- @module 'meta.health'

local M = {}

--- The oldest Neovim the ambient surface is verified against: pull diagnostics and
--- arbitrary-token `$/progress` (`docs/research/nvim-lsp-surface.md`).
local MIN_VERSION = { 0, 12 }

function M.check()
  local health = vim.health
  local attach = require('meta.attach')
  health.start('meta')

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
    health.warn('not configured: require("meta").setup({ cmd = { "meta-lsp" } }) was never called')
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
    local enabled = vim.tbl_get(cfg.settings or {}, 'meta', 'enabled')
    if enabled == false then
      health.warn('enabled = false: :Meta stop is in effect, the server issues no model calls')
    else
      health.info('enabled = true')
    end
  end

  local clients = vim.lsp.get_clients({ name = attach.NAME })
  if #clients == 0 then
    health.warn(
      'no meta client attached (open a file; the attach pass covers the buffers the FileType path misses)'
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
