--- Universal attach pass and the language hook.
---
--- `vim.lsp.enable` attaches only on the `FileType` autocmd (`vim/lsp.lua`, the
--- `nvim.lsp.enable` augroup), which never fires for a file Neovim cannot identify. With
--- `filetypes = nil` — documented as "ALL filetypes" — 8 of 11 fixtures attach and 3 are
--- never served (`verify/probes/language.lua`). Support is unconditional (PROTOCOL N10), so
--- this pass attaches the rest: steps 2 and 3 of the ladder in `docs/LANGUAGE.md` §1.
---
--- Language never gates any of it (N10/N11). The hook in §2 only decides what the server is
--- told, and the server implements the same ladder for clients that send nothing useful.
---
--- @module 'jev.attach'

local M = {}

--- The client's name, and the group the pass registers under. `:checkhealth jev` reads both.
M.NAME = 'jev'
M.AUGROUP = 'jev.attach'

--- @class jev.AttachOpts : vim.lsp.ClientConfig
--- @field cmd? string[]  Server command, default `{ 'jev-lsp' }`.
--- @field root_dir? string|fun(bufnr: integer, on_dir: fun(dir: string)): nil

--- Last options handed to `setup`, read by `configure`.
--- @type jev.AttachOpts
M.opts = {}

--- The language hook of `docs/LANGUAGE.md` §2, in its pure form.
---
--- Filename and contents only: passing `buf` instead would make `vim.filetype.match` *set*
--- the buffer's filetype — a mutation from a callback that must not have one, and one that
--- can fight the user's own filetype configuration. Free in the common case: when the client
--- already knows the filetype, no detector call happens at all.
---
--- @param bufnr integer
--- @param ft string|nil  `vim.bo[bufnr].filetype`, as the client passes it
--- @return string  a language id; `''` is a normal value, not an error (N11)
function M.get_language_id(bufnr, ft)
  if ft ~= nil and ft ~= '' then
    return ft
  end
  return vim.filetype.match({
    filename = vim.api.nvim_buf_get_name(bufnr),
    contents = vim.api.nvim_buf_get_lines(bufnr, 0, 200, false),
  }) or ''
end

--- Store the plugin options. Everything is `vim.lsp.ClientConfig`; the plugin adds no keys of
--- its own beyond the defaults set here.
--- @param opts? jev.AttachOpts
function M.setup(opts)
  M.opts = opts or {}
end

--- The workspace root for one buffer, always a string.
---
--- A file outside any repository still gets a root, because no resolution result may gate
--- attachment (PROTOCOL N10). `.git` is the marker `README.md` documents.
---
--- @param root_dir? string|fun(bufnr: integer, on_dir: fun(dir: string)): nil
--- @param bufnr integer
--- @return string
function M.root_for(root_dir, bufnr)
  if type(root_dir) == 'string' then
    return root_dir
  end
  if type(root_dir) == 'function' then
    local dir
    root_dir(bufnr, function(d)
      dir = d
    end)
    if dir then
      return dir
    end
  end
  return vim.fs.root(bufnr, { '.git' })
    or vim.fs.dirname(vim.api.nvim_buf_get_name(bufnr))
end

--- `vim.lsp.ClientConfig.root_dir` for the built-in path, which resolves the function form
--- per buffer (`vim/lsp.lua`, the `nvim.lsp.enable` callback).
---
--- A buffer that is not a file gets no root, and so no client: `vim.lsp.enable` replays its
--- `FileType` callback with `:doautoall` when it is enabled, which reaches buffers that have
--- no name yet. Without this, one of those becomes a client of the current directory and the
--- buffer is served twice once it does become a file.
---
--- @param bufnr integer
--- @param on_dir fun(dir: string)
function M.resolve_root(bufnr, on_dir)
  if not M.is_file_buffer(bufnr) then
    return
  end
  on_dir(M.root_for(M.opts.root_dir, bufnr))
end

--- The client config, for `vim.lsp.config('jev', …)`. Fresh on every call; mutation of the
--- result is therefore safe.
--- @return vim.lsp.Config
--- The `jev` section from whatever shape the caller passed.
---
--- `{ ambient = { code_lens = false } }` and `{ jev = { ambient = … } }` both mean the same
--- thing here; anything else would be a silent no-op.
local function section(settings)
  if type(settings) ~= 'table' then
    return {}
  end
  return settings.jev or settings
end

function M.configure()
  local opts = M.opts
  return {
    name = M.NAME,
    cmd = opts.cmd or { 'jev-lsp' },
    root_dir = M.resolve_root,
    get_language_id = M.get_language_id,
    -- Only the kill switch lives here: PROTOCOL §10 supplies the rest, and the server holds
    -- its own defaults, so there is exactly one place that owns each default.
    --
    -- The section the server reads is `jev`, and Neovim looks that up by name, so what the
    -- caller passes has to end up under it. `setup({ settings = { budget = … } })` is the
    -- shape the config schema (PROTOCOL §10) invites, so it is accepted directly rather than
    -- silently dropped because the `jev` wrapper was missing.
    settings = { jev = vim.tbl_deep_extend('force', { enabled = true }, section(opts.settings)) },
    handlers = opts.handlers,
  }
end

--- A normal file buffer, the only kind that is attached (`docs/LANGUAGE.md` §5): terminals,
--- help, quickfix, prompt, and nofile scratch are not files, and analysis of them is
--- meaningless.
--- @param bufnr integer
--- @return boolean
function M.is_file_buffer(bufnr)
  return vim.api.nvim_buf_is_valid(bufnr)
    and vim.bo[bufnr].buftype == ''
    and vim.api.nvim_buf_get_name(bufnr) ~= ''
end

--- The kill switch (PROTOCOL §5, `:Jev stop`). `enabled = false` stops the whole server
--- (`docs/LANGUAGE.md` §7), so nothing is attached while it is off; `:Jev start` re-runs the
--- pass over the buffers that arrived while it was off.
--- @return boolean
function M.is_enabled()
  local cfg = vim.lsp.config[M.NAME]
  local settings = (cfg and cfg.settings) or M.opts.settings
  return vim.tbl_get(settings or {}, 'jev', 'enabled') ~= false
end

--- The config both ladders start clients from, so the built-in `FileType` path and this pass
--- share one client per `(name, root)`. `M.configure()` is the not-yet-registered case.
--- @return vim.lsp.Config
local function config_for_start()
  return vim.lsp.config[M.NAME] or M.configure()
end

--- Attach a client to one buffer, at most once.
--- @param bufnr? integer  default: the current buffer
--- @return integer? client_id  set only when this call started the client
function M.attach(bufnr)
  bufnr = bufnr or vim.api.nvim_get_current_buf()
  if not M.is_file_buffer(bufnr) then
    return
  end
  if not M.is_enabled() then
    return
  end
  if #vim.lsp.get_clients({ bufnr = bufnr, name = M.NAME }) > 0 then
    return
  end
  -- `vim.lsp.start` compares `root_dir` to reuse a client, so the function form the built-in
  -- path accepts is resolved to a string here. Shallow copy: the registered config is cached
  -- and shared.
  local cfg = vim.tbl_extend('force', {}, config_for_start())
  cfg.root_dir = M.root_for(cfg.root_dir, bufnr)
  return vim.lsp.start(cfg, { bufnr = bufnr })
end

--- Install the pass. `BufReadPost`, `BufNewFile`, and `BufWinEnter` are the events the ladder
--- names; a buffer that is already loaded when the plugin is set up is swept too.
---
--- Idempotent: one augroup, cleared and recreated, and `M.attach` refuses a buffer that
--- already has a client.
function M.start()
  vim.api.nvim_create_augroup(M.AUGROUP, { clear = true })
  vim.api.nvim_create_autocmd({ 'BufReadPost', 'BufNewFile', 'BufWinEnter' }, {
    group = M.AUGROUP,
    desc = 'jev: attach to a file buffer the FileType path missed (docs/LANGUAGE.md §1)',
    callback = function(ev)
      M.attach(ev.buf)
    end,
  })
  for _, bufnr in ipairs(vim.api.nvim_list_bufs()) do
    M.attach(bufnr)
  end
end

return M
