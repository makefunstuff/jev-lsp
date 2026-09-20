--- jev-lsp plugin entry: client config, the attach pass, the default keymaps, `:Jev`.
---
--- ```lua
--- require('jev').setup({ cmd = { 'jev-lsp' }, settings = { jev = { /* PROTOCOL §10 */ } } })
--- ```
---
--- `setup` registers the client config (`vim.lsp.config('jev', …)`), enables the built-in
--- attach path (`vim.lsp.enable`), installs the universal pass that covers what the built-in
--- path cannot (`require('jev.attach')`, `docs/LANGUAGE.md` §1), the `docs/UX.md` §2 keymaps,
--- and the `:Jev` command.
---
--- Anything else in `vim.lsp.ClientConfig` — `capabilities`, `on_attach`, `flags`, extra
--- `handlers` — is added with `vim.lsp.config('jev', { … })`, which merges, before or after
--- `setup`: the attach pass starts clients from the resolved config, so both ladders agree.
---
--- @module 'jev'

local attach = require('jev.attach')
local context = require('jev.context')
local picker = require('jev.picker')
local statusline = require('jev.statusline')

local M = {}

M.name = attach.NAME

--- @class jev.Opts : jev.AttachOpts
--- @field keymaps? boolean  `false` leaves every default keymap unset
--- @field prefix? string    Keymap prefix, default `'<leader>j'` (`docs/UX.md` §2)
--- @field surfaces? { layout?: 'current'|'float'|'split' }  Where a generated surface goes,
---   default `'current'`: the report takes the buffer in the window you are in, and no window
---   is created. `'float'` opens a floating window over the code; `'split'` opens the split
---   this plugin used to open. `docs/UX.md` §2.
--- @field settings? table   PROTOCOL §10, under `settings.jev`

--- The plugin's own options, as `setup` last read them — the same register as
--- `require('jev.attach').opts`, and read when a surface *opens* rather than when it is
--- configured, so `surfaces.layout` is a live setting and not only a setup-time one.
--- @type { surfaces: { layout: string } }
M.opts = { surfaces = { layout = 'current' } }

--- Requests this plugin sent, by request id, so `:Jev cancel` can cancel them. Each carries
--- the `workDoneToken` whose progress the server reports under it (PROTOCOL §3.5).
local inflight = {} -- request id -> { client = vim.lsp.Client, token = string }

--- Tokens this plugin issued. Everything else seen on `$/progress` was created by the server
--- with `window/workDoneProgress/create` (PROTOCOL §3.5, path 2).
local issued = {} -- token -> true

--- Server-initiated progress, tracked only so it can be cancelled (PROTOCOL §6). What is *shown*
--- for it is `require('jev.statusline')`, which counts the same events for the statusline
--- segment (`docs/UX.md` §1).
local server_progress = {} -- client id -> { [token] = true }

local token_seq = 0

--- The client to talk to: the one attached to this buffer, else any `jev` client.
--- @return vim.lsp.Client?
local function client()
  local bufnr = vim.api.nvim_get_current_buf()
  return vim.lsp.get_clients({ bufnr = bufnr, name = M.name })[1]
    or vim.lsp.get_clients({ name = M.name })[1]
end

--- Tell the server what the user did with what it offered.
---
--- The server never sees the buffer: the picker applies edits and the dismissal is written to
--- disk here — so without this the only witness to acceptance is the user's memory, and "is
--- this working" has no answer. A command rather than a custom
--- method (N6: the standard surface plus `workspace/executeCommand` is the whole back-channel),
--- fire and forget — the answer is not shown, and a bookkeeping failure must not become a
--- failed action.
--- @param bufnr integer
--- @param params table  `{ kind, id?, line?, verb? }` (`jev.outcome`)
local function report_outcome(bufnr, params)
  if vim.lsp.get_clients({ bufnr = bufnr, name = M.name })[1] == nil then
    return
  end
  M.command('jev.outcome', { params }, function() end)
end

--- Report a command outcome. A Result envelope (PROTOCOL §7) is shown as it is; a failure —
--- including `not_implemented` — is reported, never dressed up as success.
--- @param label string
--- @param err table?
--- @param result any
function M.report(label, err, result)
  if err then
    vim.notify(
      ('jev: %s failed: %s'):format(label, err.message or vim.inspect(err)),
      vim.log.levels.WARN
    )
    return
  end
  if type(result) == 'table' and result.ok == false then
    local e = result.error or {}
    vim.notify(
      ('jev: %s: %s (%s)'):format(label, e.message or 'failed', e.code or 'unknown'),
      vim.log.levels.WARN
    )
    return
  end
  vim.notify(
    ('jev: %s: %s'):format(label, vim.inspect(result, { newline = ' ', indent = '' }))
  )
end

--- Mint a `workDoneToken` this plugin issues.
---
--- The first of the two legal token sources (PROTOCOL §3.5): the token rides in the params of
--- the request that will report under it, so no `window/workDoneProgress/create` round trip is
--- needed, and its lifetime is that request's. Minting it here keeps every plugin-issued token
--- in one registry, which is what tells server-initiated progress apart from ours
--- (`install_progress_tracker`, and `:Jev cancel`).
---
--- @return string token
function M.issue_token()
  token_seq = token_seq + 1
  local token = ('jev:%x'):format(token_seq)
  issued[token] = true
  return token
end

--- Forget a token once its request has answered, so the registry cannot grow without bound.
--- @param token string
function M.release_token(token)
  issued[token] = nil
end

--- The buffer a streamed answer is being written into, by the token carrying it.
local streams = {} -- token -> bufnr

-- Where a generated surface goes ---------------------------------------------------------------
--
-- Asking about a file must not rearrange the windows around it. `:Jev inspect` opened its
-- report with `sbuffer`, which is a *split*: the window the user was reading was halved, and a
-- second window that said nothing when closed stayed on screen for as long as the report did.
-- Every surface that shows a buffer did the same thing through the same call, so the placement
-- is one decision, made here, for all of them (`docs/UX.md` §2, `surfaces.layout`):
--
--   `current` — the default. The report takes the buffer in the window the user is already in.
--     Nothing moves: same windows, same sizes, and the file that was there is still loaded and
--     is the alternate buffer, so `q` and `<C-^>` both come straight back to it. This is the
--     honest shape for a report *about* the file, and for a streamed answer, which is the same
--     surface doing the same thing.
--   `float` — a floating window over the code: nothing moves and the code stays visible. It
--     costs the one thing `current` does not: while it is open there *is* an extra window, so
--     "`#vim.api.nvim_list_wins()` never changed" is a claim about `current` specifically.
--   `split` — what the plugin used to do. Kept as an option because wanting the code and the
--     answer side by side is a legitimate thing to want; not the default, because it is not
--     what asking a question should do to the layout.
--
-- What is *not* decided here is whether a result deserves a buffer at all: `docs/UX.md` §6 says
-- `q` on a generated buffer leaves buffers, windows and files exactly as they were, and a
-- one-line answer is a `vim.notify` (that is what `:Jev status` and `:Jev review` do, and this
-- is why neither of them is in the list above).

--- How each generated buffer was put on screen, so dismissing it can undo exactly that.
--- @type table<integer, { layout: string, win?: integer, prev?: integer }>
local placed = {} -- bufnr -> { layout, win?, prev? }

--- The layout generated buffers go in, as configured.
--- @return string
local function surface_layout()
  return (M.opts.surfaces or {}).layout or 'current'
end

--- Give the window back the way this layout took it.
---
--- The artifact wipes itself on the way out (`bufhidden`), which is what "dismissed" means for
--- a generated buffer: the window is left showing exactly what it was showing before.
--- @param bufnr integer
local function dismiss_surface(bufnr)
  local p = placed[bufnr]
  placed[bufnr] = nil
  if p == nil then
    return
  end
  if p.layout == 'float' then
    if p.win ~= nil and vim.api.nvim_win_is_valid(p.win) then
      vim.api.nvim_win_close(p.win, true)
    end
  elseif p.layout == 'current' then
    -- Guarded: the buffer behind it may be gone (a second report opened over this one), and a
    -- dismiss that cannot find its way back must do nothing rather than raise.
    if p.prev ~= nil and vim.api.nvim_buf_is_valid(p.prev) then
      vim.api.nvim_win_set_buf(0, p.prev)
    end
  else
    vim.cmd('close')
  end
end

--- Put a generated buffer on screen, and give it the one key a generated buffer has.
--- @param bufnr integer
local function place_surface(bufnr)
  if not vim.api.nvim_buf_is_valid(bufnr) then
    return
  end
  local prev = vim.api.nvim_get_current_buf()
  -- `nil` when there is nothing to move: the answer streamed into the buffer it is already in,
  -- so the finished artifact must not be placed a second time. (Spelled as two statements, not
  -- `x and nil or y` — that returns `y` either way.)
  local layout = surface_layout()
  if prev == bufnr then
    layout = nil
  end
  local float = nil
  if layout == 'float' then
    local width = math.max(20, math.min(vim.o.columns - 4, 100))
    local height = math.max(4, math.min(vim.o.lines - 4, 30))
    float = vim.api.nvim_open_win(bufnr, true, {
      relative = 'editor',
      width = width,
      height = height,
      row = math.max(0, math.floor((vim.o.lines - height) / 2) - 1),
      col = math.max(0, math.floor((vim.o.columns - width) / 2)),
      border = 'rounded',
      style = 'minimal',
    })
    placed[bufnr] = { layout = layout, win = float }
  elseif layout == 'split' then
    vim.cmd('sbuffer ' .. bufnr)
    placed[bufnr] = { layout = layout }
  elseif layout == 'current' then
    -- A report opened over another report gives *its* window back first. The one underneath
    -- was an artifact too, and an artifact is not a place: without this the buffer behind the
    -- new report is the dead one, and the alternate buffer with it, so neither `q` nor `<C-^>`
    -- reaches the code. Two swaps in one action, and the user sees only the result.
    local under = placed[prev]
    if under ~= nil and under.layout == 'current' and under.prev ~= nil
      and vim.api.nvim_buf_is_valid(under.prev)
    then
      vim.api.nvim_win_set_buf(0, under.prev)
      placed[prev] = nil
      prev = under.prev
    end
    -- `:hide buffer`, not `nvim_win_set_buf`: with `'hidden'` off and a modified buffer the
    -- swap has to be *asked* for, and `:hide` is Vim's own way of asking — one window, and the
    -- buffer left behind still loaded and now the alternate one. `nvim_win_set_buf` raises
    -- `E37` there instead of showing the report at all (verified on 0.12.1).
    vim.cmd('hide buffer ' .. bufnr)
    placed[bufnr] = { layout = layout, prev = prev }
  end
  local function dismiss()
    dismiss_surface(bufnr)
  end
  vim.keymap.set('n', 'q', dismiss, { buffer = bufnr, desc = 'jev: dismiss and give the layout back' })
  if float ~= nil then
    -- A float is the one surface that hides the code, so it is the one with a second way out.
    vim.keymap.set('n', '<Esc>', dismiss, { buffer = bufnr, desc = 'jev: dismiss and give the layout back' })
  end
end

--- Open the buffer an answer is written into *while* it is being written.
---
--- It is the artifact buffer from the start, not a placeholder that is replaced: the finished
--- artifact lands in the same buffer, so the answer never moves when it completes.
local function stream_open()
  local bufnr = vim.api.nvim_create_buf(false, true)
  vim.bo[bufnr].filetype = 'markdown'
  vim.bo[bufnr].bufhidden = 'wipe'
  -- Deliberately unnamed. Renaming an already-named buffer leaves a stub buffer holding the
  -- old name — verified: create a buffer, name it, rename it, and a second empty buffer
  -- appears under the first name. This one is named once, when it becomes the artifact.
  place_surface(bufnr)
  return bufnr
end

--- Write the text so far.
---
--- The server sends the whole answer so far, never deltas, so this replaces the buffer's
--- contents and no bookkeeping is needed: a lost or repeated report is harmless.
local function stream_write(token, markdown)
  local bufnr = streams[token]
  if not bufnr or not vim.api.nvim_buf_is_valid(bufnr) then
    return
  end
  local lines = vim.split(markdown, '\n', { plain = true })
  vim.bo[bufnr].modifiable = true
  vim.api.nvim_buf_set_lines(bufnr, 0, -1, false, lines)
  vim.bo[bufnr].modifiable = false
  -- Follow the text as it grows, the way a terminal would.
  for _, win in ipairs(vim.fn.win_findbuf(bufnr)) do
    pcall(vim.api.nvim_win_set_cursor, win, { math.max(1, #lines), 0 })
  end
end

--- `workspace/executeCommand` (PROTOCOL §6).
---
--- The request carries its own `workDoneToken`: that is the first of the two legal token
--- sources (§3.5), so the server reports progress under it with no
--- `window/workDoneProgress/create` round trip, and the token's lifetime is the request's.
---
--- @param command string
--- @param arguments? table
--- `opts.stream` opens a buffer that the answer is written into as it arrives. Only prose
--- streams: an edit is validated before anyone is allowed to see it, because half a JSON
--- object is not a preview and a streamed edit is one nobody can stop.
---
--- @param cb? fun(err: table?, result: any, ctx: table?)  default: report the outcome
--- @param opts? { stream?: boolean }
--- @return integer? request_id
function M.command(command, arguments, cb, opts)
  local c = client()
  if not c then
    vim.notify('jev: no server attached to this buffer', vim.log.levels.WARN)
    return
  end
  local bufnr = vim.api.nvim_get_current_buf()
  local token = M.issue_token()
  if opts and opts.stream then
    streams[token] = stream_open()
  end
  local success, request_id = c:request('workspace/executeCommand', {
    command = command,
    arguments = arguments or {},
    workDoneToken = token,
  }, function(err, result, ctx)
    if ctx and ctx.request_id then
      inflight[ctx.request_id] = nil
    end
    -- The token's life is the request's (§3.5): a report that arrives after this is for a
    -- token we no longer own, and `stream_write` finds nothing to write.
    local streamed = streams[token]
    streams[token] = nil
    if streamed ~= nil and (err ~= nil or (type(result) == 'table' and result.ok == false)) then
      -- Nothing is coming that could fill it: an error is not a partial answer. Without this
      -- a refused or cancelled request leaves an empty buffer behind, and the user is left
      -- holding a window that says nothing.
      if vim.api.nvim_buf_is_valid(streamed) then
        pcall(vim.api.nvim_buf_delete, streamed, { force = true })
      end
      streamed = nil
    end
    M.release_token(token)
    if cb then
      ctx = ctx or {}
      ctx.stream_bufnr = streamed
      cb(err, result, ctx)
    else
      M.report(command, err, result)
    end
  end, bufnr)
  if not success then
    -- The send itself failed. `Client:request` returns false when its own `pcall` raised — the
    -- server stopped between `client()` and here, which is the one state this plugin's documents
    -- say it never has to answer for, and it is still reachable. Nothing is coming, so: take back
    -- the surface that was opened two lines up for an answer (`dismiss_surface`, the same door
    -- `q` uses, so the window goes back the way the layout took it), release the token whose
    -- report will never arrive, and say the reason. Without this the command was a total silent
    -- no-op: no message, and an empty window left open holding it.
    local streamed = streams[token]
    streams[token] = nil
    M.release_token(token)
    if streamed ~= nil then
      dismiss_surface(streamed)
    end
    vim.notify(
      ('jev: %s was not sent: %s'):format(command, tostring(request_id)),
      vim.log.levels.WARN
    )
    return
  end
  if request_id then
    inflight[request_id] = { client = c, token = token }
  end
  return request_id
end

--- `:Jev status` — queue, budgets, in-flight calls (PROTOCOL §6).
--- @param cb? fun(err: table?, result: any)
function M.status(cb)
  M.command('jev.status', {}, cb)
end

--- `:Jev recompute` — reanalyse from the cache's point of view; cheap and idempotent.
--- @param cb? fun(err: table?, result: any)
function M.recompute(cb)
  M.command('jev.recompute', {}, cb)
end

--- Node types that are a scope, per parser language.
---
--- Deliberately short. A language that is not listed is not a failure: the request goes out
--- without a range and the server resolves the scope with the same structural rules the CLI
--- uses, and the answer says which one produced the extent (`scope_source`). A parser is
--- simply a better answer to the same question when one is available, because it knows about
--- nesting, strings and comments and the structural resolver has to guess.
local TS_SCOPE_NODES = {
  bash = { 'function_definition' },
  -- C and its relatives declare functions by *shape* — a type, a name, parentheses — which a
  -- keyword list cannot express, so the server's structural scan finds no functions in them at
  -- all. That is the gap this fills, and the set has to be a superset of what the scan found or
  -- a lens would disappear rather than improve.
  c = { 'function_definition', 'struct_specifier', 'enum_specifier', 'union_specifier', 'type_definition' },
  cpp = {
    'function_definition',
    'class_specifier',
    'struct_specifier',
    'namespace_definition',
    'template_declaration',
  },
  csharp = { 'method_declaration', 'class_declaration', 'interface_declaration', 'struct_declaration' },
  go = { 'function_declaration', 'method_declaration', 'type_declaration' },
  java = { 'method_declaration', 'class_declaration', 'interface_declaration', 'enum_declaration' },
  javascript = { 'function_declaration', 'method_definition', 'class_declaration' },
  lua = { 'function_declaration', 'function_definition' },
  python = { 'function_definition', 'class_definition', 'decorated_definition' },
  rust = { 'function_item', 'impl_item', 'struct_item', 'enum_item', 'trait_item', 'mod_item' },
  typescript = { 'function_declaration', 'method_definition', 'class_declaration' },
}

--- The enclosing declaration according to the parser, when there is a parser.
---
--- `nil` is a normal answer — no parser installed, no language in the table, no such
--- declaration — and the server then decides, exactly as it does for the CLI.
--- @return { start_line: integer, end_line: integer }?
local function treesitter_scope(bufnr, line)
  local ok, range = pcall(function()
    local lang = vim.treesitter.language.get_lang(vim.bo[bufnr].filetype)
    local types = lang and TS_SCOPE_NODES[lang]
    if not types then
      return nil
    end
    local wanted = {}
    for _, t in ipairs(types) do
      wanted[t] = true
    end
    -- The parser is obtained explicitly rather than via `get_node`, which answers nil unless a
    -- parser is already attached — and an error here is the answer "no parser installed",
    -- which the caller turns into "let the server decide".
    local parser = vim.treesitter.get_parser(bufnr, lang)
    local root = parser:parse()[1]:root()
    local node = root:named_descendant_for_range(line, 0, line, 0)
    local found = nil
    while node do
      if wanted[node:type()] then
        found = node
      end
      node = node:parent()
    end
    if not found then
      return nil
    end
    local start_line, _, end_line = found:range()
    if start_line > line or end_line < line then
      return nil
    end
    return { start_line = start_line, end_line = end_line }
  end)
  if not ok or type(range) ~= 'table' then
    return nil
  end
  return range
end

--- The declarations at the left margin, per the parser.
---
--- The converse of `treesitter_scope`: that finds the ancestor of a cursor, this walks the top
--- level. Both exist because the server has no parser by design (`LANGUAGE.md` §4) and this is
--- the side that has one — so what the parser found is sent, version-stamped, and the server
--- stops guessing. Only the root's own children count: a declaration nested inside a top-level
--- block belongs to that block, and a lens per nested function is noise.
--- @return table?  `{ { start_line, end_line }, … }`, or nil when it cannot answer
local function treesitter_definitions(bufnr)
  local ok, defs = pcall(function()
    local lang = vim.treesitter.language.get_lang(vim.bo[bufnr].filetype)
    local types = lang and TS_SCOPE_NODES[lang]
    if not types then
      return nil
    end
    local wanted = {}
    for _, t in ipairs(types) do
      wanted[t] = true
    end
    local parser = vim.treesitter.get_parser(bufnr, lang)
    local root = parser:parse()[1]:root()
    local out = {}
    local function walk(node, top)
      for child in node:iter_children() do
        if top and wanted[child:type()] then
          local start_line, _, end_line = child:range()
          out[#out + 1] = { start_line = start_line, end_line = end_line }
        end
        walk(child, false)
      end
    end
    walk(root, true)
    return out
  end)
  if not ok or type(defs) ~= 'table' or #defs == 0 then
    return nil
  end
  return defs
end

--- Tell the server what this buffer's declarations and standing context are, for the version
--- it is seeing (`PROTOCOL.md` §3.4.3, §6.1).
---
--- Nothing is sent when the parser cannot answer: `nil` means the server keeps its own
--- structural scan, which is what a client without a parser gets. A definition set the server
--- cannot match to the current version is ignored there, so a missed push costs accuracy and
--- never correctness.
local function push_definitions(bufnr)
  local client = client()
  if client == nil or not vim.api.nvim_buf_is_valid(bufnr) then
    return
  end
  local defs = treesitter_definitions(bufnr)
  if defs == nil then
    return
  end
  local version = vim.lsp.util.buf_versions[bufnr]
  if type(version) ~= 'number' then
    return
  end
  -- Cheap by construction: imports come from a parser that has already parsed, and siblings
  -- are buffer text. References are deliberately absent — they cost a round trip to another
  -- language server, which is worth paying when the user asks for something and not worth
  -- paying on a keystroke.
  local ok, standing = pcall(function()
    return require('jev.context').standing(bufnr)
  end)
  client:request('workspace/executeCommand', {
    command = 'jev.document',
    arguments = {
      {
        uri = vim.uri_from_bufnr(bufnr),
        version = version,
        definitions = defs,
        context = ok and standing or {},
      },
    },
  }, function() end, bufnr)
end

--- Keep the server's view of a buffer's declarations current, without a request per keystroke.
---
--- The parse is incremental and only the top level is read, so a change is cheap to answer;
--- what a change is *not* is worth a round trip each time. A short debounce keeps the lenses
--- where the declarations are while typing, and the version stamp makes a missed one harmless.
local push_timers = {} -- bufnr -> uv_timer_t

local function schedule_push(bufnr)
  if push_timers[bufnr] ~= nil then
    push_timers[bufnr]:stop()
  else
    push_timers[bufnr] = vim.uv.new_timer()
  end
  local timer = push_timers[bufnr]
  timer:start(300, 0, vim.schedule_wrap(function()
    push_definitions(bufnr)
  end))
end

--- Install the declaration push: on attach, on a change, and on a save.
function M.install_definitions()
  vim.api.nvim_create_autocmd('LspAttach', {
    callback = function(ev)
      local c = vim.lsp.get_client_by_id(ev.data.client_id)
      if c ~= nil and c.name == M.name then
        -- Deferred, not immediate: `LspAttach` fires on `BufReadPost`, before `FileType`, so at
        -- this moment the language is not known yet and a parser cannot be chosen. That cost an
        -- afternoon of wondering why the definitions never arrived.
        schedule_push(ev.buf)
      end
    end,
  })
  vim.api.nvim_create_autocmd({ 'TextChanged', 'InsertLeave', 'BufWritePost', 'FileType' }, {
    group = vim.api.nvim_create_augroup('jev.definitions', { clear = true }),
    callback = function(ev)
      if #vim.lsp.get_clients({ bufnr = ev.buf, name = M.name }) > 0 then
        schedule_push(ev.buf)
      end
    end,
  })
end

--- Why this buffer cannot be the document a command is about, or `nil` when it can.
---
--- A generated buffer has a name, and its name is not a path: `vim.uri_from_bufnr` turns
--- `jev://inspect/inspect` into a URI the server has never heard of, and the answer comes back
--- as `bad_arguments` about a file the user never opened. That used to take a deliberate `q` to
--- reach; with the report in the window they were reading, running the same command again is
--- one keystroke away — so every command that sends a document starts here. One predicate,
--- applied where each command resolves its document, because the resolution is not in one
--- place: `cursor_scope` for three of them, an inline `uri` for two, and the path alone for
--- `inspect`.
---
--- `docs/LANGUAGE.md` §5 is the same rule from the other side: a non-file buffer is never
--- attached, so `vim.lsp.get_clients({ bufnr })` already answers "is this a document" for the
--- picker and the hints badge (`M.action`, `M.hints`), which refuse by name of their own.
--- @param bufnr integer
--- @return string?  a phrase naming what this buffer is, for the refusal
local function not_a_document(bufnr)
  local name = vim.api.nvim_buf_get_name(bufnr)
  if vim.bo[bufnr].buftype ~= '' then
    return name ~= '' and ('a generated buffer (%s)'):format(name) or 'a scratch buffer'
  end
  if name == '' then
    return 'an unnamed buffer'
  end
  return nil
end

--- The file buffer a command is about, or `nil` after saying which buffer it refused.
---
--- A message rather than a request: the user asked about a file, and the honest answer is which
--- buffer is in the way, not a protocol error about a URI they never typed.
--- @param what string  the command, for the message
--- @return integer? bufnr
local function document_buffer(what)
  local bufnr = vim.api.nvim_get_current_buf()
  local why = not_a_document(bufnr)
  if why ~= nil then
    vim.notify(
      ('jev: %s needs a file buffer, and this one is %s'):format(what, why),
      vim.log.levels.WARN
    )
    return nil
  end
  return bufnr
end

--- The scope argument `jev.plan`, `jev.explain` and friends take (PROTOCOL §6): where the
--- request is anchored.
---
--- The range is the one thing this adds beyond file and line: when a parser can name the
--- enclosing declaration, the request carries it, and the server anchors on that instead of
--- on its own structural guess. Without one the server resolves the scope itself, which is
--- the same path the CLI takes (PROTOCOL N5, `LANGUAGE.md` §4).
--- @return { uri: string, line: integer, range: table? }
local function cursor_scope()
  local bufnr = vim.api.nvim_get_current_buf()
  local line = vim.api.nvim_win_get_cursor(0)[1] - 1
  local scope = { uri = vim.uri_from_bufnr(bufnr), line = line }
  local range = treesitter_scope(bufnr, line)
  if range ~= nil then
    scope.range = range
  end
  return scope
end


--- Render whatever came back as an artifact.
---
--- The shared end of every artifact command, so a second one cannot grow its own idea of what
--- to do with an answer. `ctx.stream_bufnr` is the buffer a stream was already filling, so the
--- finished text lands where the partial text was instead of in a second window.
--- @param command string  the command being reported, if it fails
local function render_artifact(command, err, result, ctx)
  if err or (type(result) == 'table' and result.ok == false) then
    M.report(command, err, result)
    return
  end
  if type(result) ~= 'table' or type(result.markdown) ~= 'string' then
    M.report(command, nil, result)
    return
  end
  M.open_artifact(result, ctx and ctx.stream_bufnr)
end

--- The finding on a line, if one is there.
---
--- The id is what makes a question about "this" about this: the server looks the finding up and
--- puts it in the prompt, so the answer is grounded in the code rather than in a description of
--- it. Neovim keeps the LSP diagnostic under `user_data.lsp`, which is where the data we attach
--- to it can be read back.
local function finding_at(bufnr, line)
  for _, d in ipairs(vim.diagnostic.get(bufnr)) do
    if d.source == 'jev' and d.lnum <= line and line <= (d.end_lnum or d.lnum) then
      local data = d.user_data and d.user_data.lsp and d.user_data.lsp.data
      if type(data) == 'table' and type(data.finding_id) == 'string' then
        return data.finding_id
      end
    end
  end
  return nil
end

--- `:Jev explain` — the cursor's file and line.
---
--- Not a code action: Neovim executes a resolved action's `command` by sending it back to the
--- server (`Client:exec_cmd`), so a code action cannot make the client open a buffer. The
--- plugin asks for the artifact and renders it, and the server never learns a buffer exists.
---
--- The one argument is the position itself: `[{ uri, line }]`, line 0-based.
---
--- @param cb? fun(err: table?, result: any)
--- Ask a question: about this file, or about nothing in particular.
---
--- Not a verb and not a scope: the question decides. With a file open the server is handed the
--- enclosing scope, because that is what makes "this API" or "this method" concrete; with none
--- the question stands alone. With `opts.web` the answer may ask for a page to be fetched —
--- https only, one page, size- and time-capped — and the artifact says which url was read.
--- @param question string? asked interactively when absent
--- @param opts? { web?: boolean }
function M.ask(question, opts)
  opts = opts or {}
  if question == nil or question == '' then
    vim.ui.input({ prompt = opts.web and 'jev: ask (web) › ' or 'jev: ask › ' }, function(text)
      if text ~= nil and text ~= '' then
        M.ask(text, opts)
      end
    end)
    return
  end
  local args = { question = question, web = opts.web == true }
  local bufnr = vim.api.nvim_get_current_buf()
  -- Silent when there is no document, and deliberately so: a question about nothing is a
  -- question this command is documented to answer ("with none the question stands alone"), so
  -- a generated buffer is not a refusal here — the scope is simply not there to send.
  if not_a_document(bufnr) == nil then
    local scope = cursor_scope()
    args.uri = scope.uri
    args.line = scope.line
    local provided = context.for_position(bufnr, scope.line, vim.api.nvim_win_get_cursor(0)[2])
    if #provided > 0 then
      args.context = provided
    end
  end
  M.command('jev.ask', { args }, function(err, result, ctx)
    render_artifact('jev.ask', err, result, ctx)
  end)
end

function M.explain(cb)
  local bufnr = document_buffer('explain')
  if bufnr == nil then
    return
  end
  local scope = cursor_scope()
  local provided = context.for_position(
    bufnr,
    scope.line,
    vim.api.nvim_win_get_cursor(0)[2]
  )
  if #provided > 0 then
    scope.context = provided
  end
  M.command('jev.explain', { scope }, cb or function(err, result, ctx)
    render_artifact('jev.explain', err, result, ctx)
  end, { stream = true })
end

--- A plan, as steps you approve.
---
--- The server has produced and applied plans since U5; what was missing was a surface. A plan is
--- the one thing here that is genuinely multi-step, and a multi-step thing rendered as a wall of
--- Markdown asks the user to hold `jev.apply {plan_id, steps:[2]}` in their head. So each step
--- is a line, the line says where it will work, `<CR>` applies that one step, `a` applies the
--- rest, and `u` takes back the last one applied on that line. Nothing is applied until asked —
--- N8 — which is why this is a buffer with keystrokes rather than a progress bar.
local plans = {} -- bufnr -> { id, lines, applied }

local function plan_text(plan)
  local lines = {
    ('# Plan: %s'):format(type(plan.goal) == 'string' and plan.goal or ''),
    '',
    ('_%d step(s) · %s_'):format(#(plan.steps or {}), type(plan.language) == 'string' and plan.language or ''),
    '',
  }
  local at = {}
  for _, step in ipairs(plan.steps or {}) do
    local target = step.targets and step.targets[1]
    local where = ''
    if type(target) == 'table' and type(target.uri) == 'string' then
      local line = target.range and target.range.start and target.range.start.line or 0
      where = (' · %s:%d'):format(
        vim.fn.fnamemodify(vim.uri_to_fname(target.uri), ':t'), line + 1
      )
    end
    lines[#lines + 1] = ('%d. [%s] %s%s'):format(
      step.n, step.verb or '?', step.title or '', where
    )
    -- Keyed to the step's own line, before the rationale adds another: the rationale is part
    -- of the step, not the place the step starts.
    at[#lines] = step.n
    if type(step.rationale) == 'string' and step.rationale ~= '' then
      lines[#lines + 1] = ('      %s'):format(step.rationale)
    end
  end
  return lines, at
end

--- Write what happened to a step onto its own line.
local function plan_mark(bufnr, n, what)
  local plan = plans[bufnr]
  if plan == nil or not vim.api.nvim_buf_is_valid(bufnr) then
    return
  end
  for line, step_n in pairs(plan.at) do
    if step_n == n then
      local current = vim.api.nvim_buf_get_lines(bufnr, line - 1, line, false)[1] or ''
      current = current:gsub('%s+·%s+(applied|reverted)$', '')
      vim.bo[bufnr].modifiable = true
      vim.api.nvim_buf_set_lines(bufnr, line - 1, line, false, { current .. ' · ' .. what })
      vim.bo[bufnr].modifiable = false
    end
  end
end

local function plan_apply(bufnr, steps)
  local plan = plans[bufnr]
  if plan == nil or #steps == 0 then
    return
  end
  M.command('jev.apply', { { plan_id = plan.id, steps = steps } }, function(err, result)
    if err or (type(result) == 'table' and result.ok == false) then
      M.report('jev.apply', err, result)
      return
    end
    local applied = (type(result) == 'table' and result.applied) or {}
    for _, a in ipairs(applied) do
      plan.applied[a.n] = a.edit_id
      plan_mark(bufnr, a.n, 'applied')
    end
    local summary = applied[1] and applied[1].summary
    vim.notify(
      ('jev: %d step(s) applied%s'):format(#applied,
        type(summary) == 'string' and (' — ' .. summary) or ''),
      vim.log.levels.INFO
    )
  end)
end

--- `:Jev plan [goal]` — the goal is the only free text this interface asks for.
function M.open_plan(plan)
  if type(plan) ~= 'table' or type(plan.steps) ~= 'table' or #plan.steps == 0 then
    M.report('jev.plan', nil, plan)
    return
  end
  local lines, at = plan_text(plan)
  local bufnr = M.open_artifact({
    kind = 'plan',
    id = type(plan.id) == 'string' and plan.id or 'plan',
    markdown = table.concat(lines, '\n'),
  })
  if bufnr == nil then
    return
  end
  plans[bufnr] = { id = plan.id, at = at, applied = {} }

  --- The step on the cursor's line, if there is one.
  local function step_here()
    local plan_for_buffer = plans[bufnr]
    return plan_for_buffer and plan_for_buffer.at[vim.api.nvim_win_get_cursor(0)[1]] or nil
  end

  vim.keymap.set('n', '<CR>', function()
    local n = step_here()
    if n ~= nil then
      plan_apply(bufnr, { n })
    end
  end, { buffer = bufnr, desc = 'jev: apply this step' })

  vim.keymap.set('n', 'a', function()
    local remaining = {}
    for _, step in ipairs(plan.steps) do
      if plans[bufnr].applied[step.n] == nil then
        remaining[#remaining + 1] = step.n
      end
    end
    plan_apply(bufnr, remaining)
  end, { buffer = bufnr, desc = 'jev: apply every step' })

  vim.keymap.set('n', 'u', function()
    local n = step_here()
    local edit_id = n ~= nil and plans[bufnr].applied[n] or nil
    if edit_id == nil then
      return
    end
    M.command('jev.revert', { { edit_id = edit_id } }, function(err, result)
      if err or (type(result) == 'table' and result.ok == false) then
        M.report('jev.revert', err, result)
        return
      end
      plans[bufnr].applied[n] = nil
      plan_mark(bufnr, n, 'reverted')
    end)
  end, { buffer = bufnr, desc = 'jev: take back this step' })
  -- `q` is not set here: the dismiss key belongs to the placement (`place_surface`), and it
  -- means something different in each layout — closing a window here would close the only one.
end

--- `:Jev where <question>` — where is this handled?
---
--- The one navigation question a model answers better than an index: *"where is retry handled"*
--- is not a symbol, so nothing that answers `textDocument/references` has anything to say about
--- it. The client greps — locally, offline, in milliseconds — and the model ranks what came
--- back. Semantic search with no embedding store and nothing walking the tree per keystroke.
---
--- The answer rides `jev.followup`: same context, same contract, same buffer. Only who
--- assembled the context differs — a grep here, the language servers there.
--- @param question? string
function M.where(question)
  local bufnr = document_buffer('where')
  if bufnr == nil then
    return
  end
  local function ask(text)
    local line = vim.api.nvim_win_get_cursor(0)[1] - 1
    local arg = { uri = vim.uri_from_bufnr(bufnr), line = line, question = text }
    local range = treesitter_scope(bufnr, line)
    if range ~= nil then
      arg.range = range
    end
    -- The local search, and what it *could not* do. An empty context and a search that never ran
    -- are different answers, and only the first one is about the code: a machine without `rg` and
    -- without `grep`, or an engine that refused the pattern, used to send the question on with no
    -- matches and no word said about it.
    local ok, matches, why = pcall(context.matches_for, text, bufnr)
    if not ok then
      matches, why = {}, tostring(matches)
    end
    if why ~= nil then
      vim.notify(
        ('jev: where could not search for that: %s'):format(why),
        vim.log.levels.WARN
      )
    end
    local provided = matches or {}
    if #provided > 0 then
      arg.context = provided
    end
    M.command('jev.followup', { arg }, function(err, result, ctx)
      render_artifact('jev.followup', err, result, ctx)
    end, { stream = true })
  end
  if question ~= nil and vim.trim(question) ~= '' then
    ask(vim.trim(question))
    return
  end
  vim.ui.input({ prompt = 'jev: where is …: ' }, function(input)
    if input ~= nil and vim.trim(input) ~= '' then
      ask(vim.trim(input))
    end
  end)
end

--- `:Jev session` — what this server has done here.
---
--- The record is an append-only log under the repository root's `.git/jev/`, beside the
--- dismissals, so it survives a restart and never appears in `git status`. It is something to
--- read, not state the plugin acts on: nothing in this file consults it.
--- Where each line of a session buffer wants to take you, by buffer.
local session_targets = {} -- bufnr -> { [line] = { uri, line } }

--- Open the file and line an entry names.
---
--- The record is only useful if a line in it can be walked back: an entry that names a place
--- is a place you can return to, which is the difference between a log and a history.
local function jump_to(target)
  if type(target) ~= 'table' or type(target.uri) ~= 'string' then
    return
  end
  local path = vim.uri_to_fname(target.uri)
  vim.cmd('edit ' .. vim.fn.fnameescape(path))
  if type(target.line) == 'number' then
    pcall(vim.api.nvim_win_set_cursor, 0, { target.line + 1, 0 })
  end
end

function M.session()
  M.command('jev.session', { { limit = 200 } }, function(err, result)
    if err or (type(result) == 'table' and result.ok == false) then
      M.report('jev.session', err, result)
      return
    end
    local entries = (type(result) == 'table' and result.entries) or {}
    local lines = { '# jev session', '' }
    local targets = {}
    for i = #entries, 1, -1 do
      local e = entries[i]
      -- Entry order is newest first, and the target is keyed by the buffer line it lands on.
      local target = type(e.uri) == 'string'
          and { uri = e.uri, line = type(e.line) == 'number' and e.line or nil }
        or nil
      local where = ''
      if target ~= nil then
        where = vim.fn.fnamemodify(vim.uri_to_fname(e.uri), ':t')
        if target.line ~= nil then
          where = ('%s:%d'):format(where, target.line + 1)
        end
      end
      -- A field the server sent as null arrives as `vim.NIL`, which is userdata: it has to be
      -- checked rather than used, or a renderer turns a missing value into an error.
      local at = (#lines + 1)
      if e.kind == 'command' then
        local verdict = 'ok'
        if e.ok ~= true then
          verdict = type(e.error) == 'string' and ('failed · ' .. e.error) or 'failed'
        end
        lines[#lines + 1] = ('- `%s` — %s%s'):format(
          type(e.command) == 'string' and e.command or '?',
          verdict,
          type(e.ms) == 'number' and (' · ' .. e.ms .. ' ms') or ''
        )
      elseif e.kind == 'analysis' then
        -- `findings` is the list the server kept; older entries carry only the count.
        local kept = type(e.findings) == 'table' and #e.findings
          or (type(e.findings) == 'number' and e.findings)
          or (type(e.count) == 'number' and e.count)
          or 0
        lines[#lines + 1] = ('- analysis — %d finding(s)%s'):format(
          kept,
          e.from_cache == true and ' · cache' or ''
        )
      else
        lines[#lines + 1] = '- ' .. vim.inspect(e):gsub('%s+', ' ')
      end
      if target ~= nil and where ~= '' then
        lines[#lines] = lines[#lines] .. '  · ' .. where
      end
      if target ~= nil then
        targets[at] = target
      end
    end
    if #entries == 0 then
      lines[#lines + 1] = '_nothing recorded for this root yet_'
    end
    if type(result.path) == 'string' then
      lines[#lines + 1] = ''
      lines[#lines + 1] = ('_record: %s_'):format(result.path)
    end
    local bufnr = M.open_artifact({
      kind = 'session',
      id = 'session',
      markdown = table.concat(lines, '\n'),
    })
    if bufnr ~= nil and next(targets) ~= nil then
      session_targets[bufnr] = targets
      vim.keymap.set('n', '<CR>', function()
        local here = vim.api.nvim_win_get_cursor(0)[1]
        jump_to(session_targets[bufnr] and session_targets[bufnr][here])
      end, { buffer = bufnr, desc = 'jev: open the place this entry names' })
    end
  end)
end

--- `:Jev followup [question]` — ask about what is under the cursor.
---
--- The question is the second and last place free text enters, after `plan`, and for the same
--- reason: a picker cannot express a question (N7). The finding at the cursor, when there is
--- one, travels with it — that is the difference between "why is this wrong here" and a
--- question about code in general, and it is why this is one keystroke rather than a chat.
---
--- @param question? string
function M.followup(question)
  local bufnr = document_buffer('followup')
  if bufnr == nil then
    return
  end
  local line = vim.api.nvim_win_get_cursor(0)[1] - 1
  local function ask(text)
    local arg = { uri = vim.uri_from_bufnr(bufnr), line = line, question = text }
    local range = treesitter_scope(bufnr, line)
    if range ~= nil then
      arg.range = range
    end
    -- What the editor can see and the server cannot: imports, what refers to this, the test
    -- that covers it, and the buffers the user has been in. Sent with the question, because a
    -- question about code is worth more when the project is in front of the model.
    local provided = context.for_position(bufnr, line, vim.api.nvim_win_get_cursor(0)[2])
    if #provided > 0 then
      arg.context = provided
    end
    local id = finding_at(bufnr, line)
    if id ~= nil then
      arg.finding_id = id
    end
    M.command('jev.followup', { arg }, function(err, result, ctx)
      render_artifact('jev.followup', err, result, ctx)
    end, { stream = true })
  end
  if question ~= nil and vim.trim(question) ~= '' then
    ask(vim.trim(question))
    return
  end
  vim.ui.input({ prompt = 'jev: ask about this: ' }, function(input)
    if input ~= nil and vim.trim(input) ~= '' then
      ask(vim.trim(input))
    end
  end)
end

--- Render an artifact in a scratch buffer.
---
--- `docs/UX.md` §6: `q` on a generated buffer leaves buffers, windows, and files exactly as
--- they were — so the buffer is `nofile`, unlisted, wiped on close, and nothing is written.
--- Where it is *shown* is `place_surface` (`surfaces.layout`), which is also what makes the `q`
--- in that sentence true in every layout.
--- @param artifact table  `{ schema, kind, summary, markdown, … }` (PROTOCOL §7)
--- @param bufnr? integer  an existing buffer to finish in (the one a stream filled)
function M.open_artifact(artifact, bufnr)
  if bufnr == nil or not vim.api.nvim_buf_is_valid(bufnr) then
    bufnr = vim.api.nvim_create_buf(false, true)
  end
  vim.bo[bufnr].modifiable = true
  vim.api.nvim_buf_set_lines(bufnr, 0, -1, false,
    vim.split(artifact.markdown, '\n', { plain = true }))
  vim.bo[bufnr].filetype = 'markdown'
  vim.bo[bufnr].modifiable = false
  vim.bo[bufnr].bufhidden = 'wipe'
  pcall(vim.api.nvim_buf_set_name, bufnr,
    ('jev://%s/%s'):format(artifact.kind or 'artifact', artifact.id or 'scratch'))
  place_surface(bufnr)
  return bufnr
end

--- `:Jev plan [goal]` — `docs/UX.md` §1: free text enters here and nowhere else, because the
--- protocol cannot ask for it ([R1]). With no goal argument the plugin prompts.
---
--- The server returns a plan artifact (targets, per-step verbs, cost). There is no
--- step-through plan buffer yet, so it arrives as a reported Result.
--- @param goal? string
function M.plan(goal)
  -- Before the prompt, not after: a goal is free text the interface asks for once
  -- (`docs/UX.md` §1), and asking for it when the command cannot run is a prompt that ends in
  -- a refusal.
  if document_buffer('plan') == nil then
    return
  end
  local function ask(text)
    -- `arguments` is an LSP array, not a map: the server reads `arguments.first()`, and a map
    -- is rejected in transport before the command runs.
    M.command('jev.plan', { { goal = text, scope = cursor_scope() } }, function(err, result)
      if err or (type(result) == 'table' and result.ok == false) then
        M.report('jev.plan', err, result)
        return
      end
      M.open_plan(result)
    end)
  end
  if goal ~= nil and vim.trim(goal) ~= '' then
    ask(vim.trim(goal))
    return
  end
  vim.ui.input({ prompt = 'jev goal: ' }, function(input)
    if input ~= nil and vim.trim(input) ~= '' then
      ask(vim.trim(input))
    end
  end)
end

--- `:Jev review` — the cursor's file and line, the same argument shape as `jev.explain`.
--- Findings come back in the Result and reach the buffer through pull diagnostics.
function M.review()
  if document_buffer('review') == nil then
    return
  end
  M.command('jev.review', { cursor_scope() })
end

--- The artifact `:Jev inspect` opens: the findings, the counts, and every skip.
---
--- The skips are the point of the surface. "No finding" and "nothing was inspected" look
--- identical on a sign column, and only one of them is a bug — so `unchanged`, `no_rules`, and
--- a rule file that failed to load each get a line of their own rather than being counted away.
--- @param path string  the document this is about, for the title
--- @param result table  the `jev.inspect` Result
--- @return table artifact  `{ kind, id, summary, markdown }` (PROTOCOL §7)
local function inspect_artifact(path, result)
  local findings = type(result.findings) == 'table' and result.findings or {}
  local skipped = type(result.skipped) == 'table' and result.skipped or {}
  local considered = tonumber(result.considered) or 0
  local candidates = tonumber(result.candidates) or 0
  local name = vim.fn.fnamemodify(path, ':t')
  local lines = {
    ('# inspect: %s'):format(name),
    '',
    ('%d rule(s) considered · %d candidate(s) found · %d finding(s)')
      :format(considered, candidates, #findings),
    '',
    '## findings',
    '',
  }
  if #findings == 0 then
    lines[#lines + 1] = 'No finding was published for this document.'
  else
    for _, f in ipairs(findings) do
      -- 1-based, the way the line is numbered in the editor.
      lines[#lines + 1] = ('- line %d: %s'):format((tonumber(f.line) or 0) + 1, tostring(f.label))
      if type(f.detail) == 'string' and f.detail ~= '' then
        lines[#lines + 1] = ('  %s'):format(f.detail)
      end
    end
  end
  lines[#lines + 1] = ''
  lines[#lines + 1] = ('## skipped (%d)'):format(#skipped)
  lines[#lines + 1] = ''
  if #skipped == 0 then
    lines[#lines + 1] = 'Nothing was skipped.'
  else
    for _, s in ipairs(skipped) do
      lines[#lines + 1] = ('- [%s] %s'):format(tostring(s.code), tostring(s.detail))
    end
  end
  return {
    kind = 'inspect',
    id = 'inspect',
    summary = ('%d finding(s) in %s'):format(#findings, name),
    markdown = table.concat(lines, '\n'),
  }
end

--- `:Jev inspect [--force]` — the repository's rules over this buffer, with the counts.
---
--- The same `jev.inspect` the ambient pass runs, asked on demand: what it found, how many rules
--- it considered, how many candidate lines their inspections named, and every skip, so "no
--- finding" can be told apart from "nothing was inspected". `--force` re-runs the pass for a
--- document git calls unchanged — the escape hatch after editing a rule, which on its own does
--- not make a pass run.
--- @param force? boolean
function M.inspect(force)
  local bufnr = document_buffer('inspect')
  if bufnr == nil then
    return
  end
  local path = vim.api.nvim_buf_get_name(bufnr)
  M.command('jev.inspect', { { path = path, force = force == true } }, function(err, result)
    if err ~= nil then
      vim.notify(
        ('jev: inspect failed: %s'):format(err.message or vim.inspect(err)),
        vim.log.levels.WARN
      )
      return
    end
    if type(result) ~= 'table' then
      vim.notify('jev: inspect returned nothing', vim.log.levels.WARN)
      return
    end
    if result.ok == false then
      -- The code, not a bare "failed": the codes are contract (§6.1), and the message says
      -- which one and why.
      local e = type(result.error) == 'table' and result.error or {}
      vim.notify(
        ('jev: inspect failed: %s: %s'):format(tostring(e.code), tostring(e.message)),
        vim.log.levels.WARN
      )
      return
    end
    M.open_artifact(inspect_artifact(path, result))
  end)
end

--- `:Jev cancel` — cancel all in-flight work (`docs/UX.md` §2).
---
--- Our own requests are cancelled with `$/cancelRequest` against the id `Client:request`
--- returned. Server-initiated work — a background re-analysis with no request of ours to
--- attach to — is cancelled with `window/workDoneProgress/cancel` (PROTOCOL §6); those tokens
--- are only known because the plugin observes `$/progress` under them (§3.5, path 2).
function M.cancel()
  local requests, jobs = 0, 0
  for id, entry in pairs(inflight) do
    entry.client:cancel_request(id)
    inflight[id] = nil
    requests = requests + 1
  end
  for client_id, set in pairs(server_progress) do
    local c = vim.lsp.get_client_by_id(client_id)
    if c then
      for token in pairs(set) do
        c:notify('window/workDoneProgress/cancel', { token = token })
        jobs = jobs + 1
      end
    end
  end
  server_progress = {}
  vim.notify(('jev: cancelled %d request(s), %d server job(s)'):format(requests, jobs))
end

--- The kill switch, one command with no prompt (PROTOCOL §5, `docs/UX.md` §2).
---
--- `workspace/configuration` is answered from `client.settings` (`lsp/handlers.lua`), so the
--- switch is flipped there *and* in the registered config — which is what a client started
--- later will read — and then the server is told to re-read it. That is the documented
--- mechanism: the server stops issuing model calls immediately and drains what is in flight.
--- @param enabled boolean
function M.set_enabled(enabled)
  vim.lsp.config(M.name, { settings = { jev = { enabled = enabled } } })
  local clients = vim.lsp.get_clients({ name = M.name })
  for _, c in ipairs(clients) do
    c.settings.jev = c.settings.jev or {}
    c.settings.jev.enabled = enabled
    c:notify('workspace/didChangeConfiguration', { settings = c.settings })
  end
  vim.notify(
    ('jev: enabled = %s (%d client(s) notified)'):format(tostring(enabled), #clients)
  )
end

function M.stop()
  M.set_enabled(false)
end

--- `:Jev start` — the inverse of the kill switch, and the pass again for the buffers that
--- were opened while it was off.
function M.start()
  M.set_enabled(true)
  attach.start()
end

--- `:Jev log` — the client log, where the server's stderr and the LSP traffic land.
---
--- A real file, so it is `:edit` — the layout is not a generated surface's to move — but
--- `:hide edit`, because the log is worth reading *while* the buffer you are in is unsaved and
--- plain `:edit` refuses that swap with `E37`.
function M.log()
  vim.cmd('hide edit ' .. vim.fn.fnameescape(vim.lsp.log.get_filename()))
end

-- Dismissals ---------------------------------------------------------------------------------
--
-- PROTOCOL §9: findings must be dismissible, and a dismissal is recorded in a per-repository
-- file so it does not resurface. `docs/UX.md` §4 fixes the location: `<root>/.git/jev/`,
-- never in the repo tree. The file is the durable record; the set is also read back once per
-- root so a dismissed finding is filtered out of the next pull in the same session.

--- Keys dismissed in this session, across roots.
local dismissed = {} -- finding id -> true
local loaded_roots = {} -- root -> true

--- @param root string
--- @return string
local function dismissals_path(root)
  return root .. '/.git/jev/dismissed.json'
end

--- @param path string
--- @return table?  `{ version, dismissed = { [id] = { verb, content_hash, at } } }`
local function read_dismissals(path)
  local ok, lines = pcall(vim.fn.readfile, path)
  if not ok then
    return nil
  end
  local decoded_ok, doc = pcall(vim.json.decode, table.concat(lines, '\n'))
  if not decoded_ok or type(doc) ~= 'table' then
    return nil
  end
  return doc
end

--- @param root string|nil
local function load_dismissals(root)
  if not root or loaded_roots[root] then
    return
  end
  loaded_roots[root] = true
  local doc = read_dismissals(dismissals_path(root))
  for id in pairs((doc and doc.dismissed) or {}) do
    dismissed[id] = true
  end
end

--- Filter pulled findings by the dismissal set, before the client renders them.
---
--- This is the client's own handler table (`vim.lsp.ClientConfig.handlers`), so it applies to
--- this server only; the server's findings keep arriving and the dismissal stays a rendering
--- decision, which is what `PROTOCOL N11` means by metadata never being a filter.
local function filter_findings(err, result, ctx)
  if not err and type(result) == 'table' and type(result.items) == 'table' then
    local root = ctx.bufnr and vim.fs.root(ctx.bufnr, { '.git' })
    load_dismissals(root)
    local kept = {}
    for _, finding in ipairs(result.items) do
      local id = finding.data and finding.data.finding_id
      if not (id and dismissed[id]) then
        kept[#kept + 1] = finding
      end
    end
    result.items = kept
  end
  return vim.lsp.diagnostic.on_diagnostic(err, result, ctx)
end

--- The finding metadata the server attaches to a diagnostic (PROTOCOL §9:
--- `data = { finding_id, verb, content_hash }`).
---
--- A rendered diagnostic is not the wire object: the client stores the original LSP
--- diagnostic under `user_data.lsp` (`lsp/diagnostic.lua`, `diagnostic_lsp_to_vim`), which is
--- the same shape `vim.lsp.diagnostic.from()` reads back.
--- @param d vim.Diagnostic
--- @return table?
local function finding_data(d)
  local lsp = d.user_data and d.user_data.lsp
  return lsp and lsp.data
end

--- `:Jev dismiss [finding_id]` — PROTOCOL §9, `docs/UX.md` §4.
---
--- With no argument the finding on the cursor line is dismissed, which is what the keymap
--- means. The id is the content-addressed key the server put in the finding's `data`
--- (`data.finding_id`), so the record survives content moving around the file.
--- @param id? string
function M.dismiss(id)
  local bufnr = vim.api.nvim_get_current_buf()
  local line = vim.api.nvim_win_get_cursor(0)[1] - 1
  local verb, content_hash
  if id == nil then
    for _, d in ipairs(vim.diagnostic.get(bufnr)) do
      local data = finding_data(d)
      if d.source == 'jev' and d.lnum == line and data and data.finding_id then
        id, verb, content_hash = data.finding_id, data.verb, data.content_hash
        break
      end
    end
  end
  if id == nil then
    vim.notify('jev: no finding at the cursor to dismiss', vim.log.levels.WARN)
    return
  end
  local root = vim.fs.root(bufnr, { '.git' })
  if not root then
    vim.notify(
      ('jev: %s has no repository root, so there is nowhere to record a dismissal (PROTOCOL §9)')
        :format(vim.api.nvim_buf_get_name(bufnr)),
      vim.log.levels.WARN
    )
    return
  end
  local path = dismissals_path(root)
  local doc = read_dismissals(path)
  if not doc or type(doc.dismissed) ~= 'table' then
    doc = { version = 1, dismissed = {} }
  end
  if doc.dismissed[id] == nil then
    doc.dismissed[id] = {
      verb = verb,
      content_hash = content_hash,
      at = os.date('!%Y-%m-%dT%H:%M:%SZ'),
    }
    vim.fn.mkdir(vim.fs.dirname(path), 'p')
    vim.fn.writefile({ vim.json.encode(doc) }, path)
    vim.notify(('jev: dismissed %s and recorded it in %s'):format(id, path))
  end
  dismissed[id] = true
  loaded_roots[root] = true
  M.hide_dismissed(bufnr)
  report_outcome(bufnr, { kind = 'finding-dismissed', id = id, line = line })
end

--- The namespace this server's pulled findings were rendered into, or nil if there are none.
---
--- Read off the diagnostics rather than asked of `vim.lsp.diagnostic.get_namespace`, whose
--- signature moved under us: 0.12.5 takes `(client_id, is_pull, pull_id)`, 0.12.1 takes
--- `(client_id, pull_id)`, and Lua *silently ignores* the extra argument. On 0.12.1 the old
--- three-argument call answered the namespace of the deprecated boolean form — a different
--- namespace, into which nothing is ever written — so `hide_dismissed` filtered an empty set
--- and every dismissal looked like a no-op until the next pull. The namespace is already on
--- every rendered diagnostic (`vim.Diagnostic.namespace`), so there is nothing to guess.
--- @param bufnr integer
--- @return integer?
local function findings_namespace(bufnr)
  for _, d in ipairs(vim.diagnostic.get(bufnr)) do
    if d.source == M.name then
      return d.namespace
    end
  end
  return nil
end

--- Drop dismissed findings from what is on screen now, so the sign disappears with the
--- command instead of at the next refresh.
--- @param bufnr integer
function M.hide_dismissed(bufnr)
  local ns = findings_namespace(bufnr)
  if not ns then
    return
  end
  local kept = {}
  for _, d in ipairs(vim.diagnostic.get(bufnr, { namespace = ns })) do
    local data = finding_data(d)
    local id = data and data.finding_id
    if not (id and dismissed[id]) then
      kept[#kept + 1] = d
    end
  end
  vim.diagnostic.set(ns, bufnr, kept)
end

-- Undo --------------------------------------------------------------------------------------
--
-- `docs/UX.md` §3.4: `:Jev undo` restores the snapshot taken before the last applied edit.
-- Neovim's undo-block granularity for an applied `WorkspaceEdit` is unverified ([R10],
-- `docs/research/nvim-lsp-surface.md` §10), so the plugin does not rely on it.
--
-- Every client-applied edit funnels through `vim.lsp.util.apply_workspace_edit`, whether it
-- came from the native menu (`lsp/buf.lua:1260`) or from the server's `workspace/applyEdit`
-- (`lsp/handlers.lua:201,335`), so wrapping that function is the one place that sees them all.

--- Snapshots in application order: `{ { { bufnr, before, after }, … }, … }`.
local snapshots = {}
local snapshots_hooked = false

--- Buffers an edit touches that `jev` is attached to, deduplicated.
---
--- Resource operations (`create`, `rename`, `delete`) carry a `uri` and no text document;
--- snapshots are per-buffer text, so they are not covered here.
--- @param edit table  `lsp.WorkspaceEdit`
--- @return integer[]
local function touched_buffers(edit)
  local bufnrs, seen = {}, {}
  local function add(uri)
    if type(uri) ~= 'string' then
      return
    end
    local bufnr = vim.uri_to_bufnr(uri)
    if seen[bufnr] or not vim.api.nvim_buf_is_loaded(bufnr) then
      return
    end
    if #vim.lsp.get_clients({ bufnr = bufnr, name = M.name }) == 0 then
      return
    end
    seen[bufnr] = true
    bufnrs[#bufnrs + 1] = bufnr
  end
  for _, change in ipairs(edit.documentChanges or {}) do
    if change.textDocument then
      add(change.textDocument.uri)
    end
  end
  for uri in pairs(edit.changes or {}) do
    add(uri)
  end
  return bufnrs
end

--- Take the snapshot hook. Idempotent.
function M.install_snapshot_hook()
  if snapshots_hooked then
    return
  end
  snapshots_hooked = true
  local apply = vim.lsp.util.apply_workspace_edit
  vim.lsp.util.apply_workspace_edit = function(edit, position_encoding)
    local bufnrs = touched_buffers(edit)
    local before = {}
    for _, bufnr in ipairs(bufnrs) do
      before[bufnr] = vim.api.nvim_buf_get_lines(bufnr, 0, -1, true)
    end
    local ok, err = pcall(apply, edit, position_encoding)
    if not ok then
      error(err, 0) -- the client's own error path, unchanged: nothing was applied
    end
    if #bufnrs > 0 then
      local entries = {}
      for _, bufnr in ipairs(bufnrs) do
        entries[#entries + 1] = {
          bufnr = bufnr,
          before = before[bufnr],
          after = vim.api.nvim_buf_get_lines(bufnr, 0, -1, true),
        }
      end
      snapshots[#snapshots + 1] = entries
    end
  end
end

--- `:Jev undo` / `<leader>ju` — restore the text from before the last applied edit.
---
--- Refused when a buffer has moved since: the restore would throw away typing the snapshot
--- never saw. The refusal is a message, not an edit.
function M.undo()
  local entries = snapshots[#snapshots]
  if not entries then
    vim.notify('jev: no applied edit to undo')
    return
  end
  for _, e in ipairs(entries) do
    if
      not vim.api.nvim_buf_is_valid(e.bufnr)
      or not vim.deep_equal(vim.api.nvim_buf_get_lines(e.bufnr, 0, -1, true), e.after)
    then
      vim.notify('jev: the buffer changed since that edit; undo refused')
      return
    end
  end
  table.remove(snapshots)
  for _, e in ipairs(entries) do
    vim.api.nvim_buf_set_lines(e.bufnr, 0, -1, false, e.before)
  end
  -- The one outcome that says the edit was wrong, which is the other half of "was this worth
  -- applying" (`jev.outcome`).
  report_outcome(entries[1].bufnr, { kind = 'edit-undone' })
  vim.notify(('jev: restored %d buffer(s)'):format(#entries))
end

-- Surface -----------------------------------------------------------------------------------

--- Track server-initiated progress so `:Jev cancel` can cancel it (PROTOCOL §3.5, §6).
function M.install_progress_tracker()
  local group = vim.api.nvim_create_augroup('jev', { clear = true })
  vim.api.nvim_create_autocmd('LspProgress', {
    group = group,
    pattern = '*',
    desc = 'jev: track server-initiated progress so :Jev cancel can cancel it',
    callback = function(ev)
      local data = ev.data or {}
      local params = data.params or {}
      local token = params.token
      local value = type(params.value) == 'table' and params.value or nil
      local kind = value and value.kind
      -- A streamed answer under a token we issued. It is not server-initiated progress, so it
      -- is handled before the guard that filters those out.
      if token and kind == 'report' and type(value.data) == 'table'
        and value.data.partial and type(value.data.markdown) == 'string'
      then
        stream_write(token, value.data.markdown)
        return
      end
      local c = data.client_id and vim.lsp.get_client_by_id(data.client_id)
      if not token or not c or c.name ~= M.name or issued[token] then
        return
      end
      server_progress[c.id] = server_progress[c.id] or {}
      if kind == 'begin' then
        server_progress[c.id][token] = true
      elseif kind == 'end' then
        server_progress[c.id][token] = nil
      end
    end,
  })
end

--- Inlay hints: a badge on a declaration that has findings, and nothing anywhere else.
---
--- Off unless asked for, and the reason is concrete rather than taste: Neovim enables inlay
--- hints **per buffer, not per client** (`lsp/inlay_hint.lua`), so switching them on for this
--- badge also switches on every other server's hints in that buffer — rust-analyzer's type
--- hints, clangd's parameter hints. That is a decision for the user, not a side effect of
--- installing this. `:Jev hints on|off` toggles it for the buffer.
function M.hints(on)
  local bufnr = vim.api.nvim_get_current_buf()
  local enable = on
  if enable == nil then
    enable = not vim.lsp.inlay_hint.is_enabled({ bufnr = bufnr })
  end
  -- Scoped to this server's client. Without `client_id`, Neovim enables hints for *every*
  -- client attached to the buffer, so pressing this key turned on clangd's hints as well — and
  -- a hint request carries no document version, so a reply computed before the edit is applied
  -- to the edited buffer and `nvim_buf_set_extmark` rejects the column. That is a decoration
  -- provider error on every redraw, from a key that is supposed to be about this server.
  local client = vim.lsp.get_clients({ bufnr = bufnr, name = M.name })[1]
  if client == nil then
    vim.notify('jev: no server attached to this buffer', vim.log.levels.WARN)
    return enable
  end
  local ok, err = pcall(vim.lsp.inlay_hint.enable, enable, { bufnr = bufnr, client_id = client.id })
  if not ok then
    vim.notify('jev: inlay hints are not available here: ' .. tostring(err), vim.log.levels.WARN)
    return enable
  end
  vim.notify(('jev: inlay hints %s'):format(enable and 'on' or 'off'), vim.log.levels.INFO)
  return enable
end

--- Code lenses: one affordance per declaration, written on the declaration.
---
--- This is the surface that does not have to be remembered. A clean declaration offers
--- `jev: explain`; one with findings offers `jev: N finding(s) · fix`. The commands are the
--- plugin's own namespace, because opening a buffer is a client decision — the server cannot
--- open one, and `window/showDocument` needs a URI an explanation does not have.
---
--- `vim.lsp.codelens.run()` re-requests the lenses and then sends the command to the
--- *server*, so it is wrapped: ours are handled here, everything else passes through
--- untouched.
local lenses_wrapped = false

--- Drop one client's lens state for one buffer, and the provider with it when it empties.
---
--- Neovim's own teardown does not run for the form `install_lenses` enables with:
--- `vim.lsp._capability.is_enabled` requires the client marker *and* the buffer marker, while
--- `enable(true, { client_id })` can only set the client half (0.12.1 asserts that `bufnr` and
--- `client_id` are mutually exclusive), so its answer is `false` for the pair. `Client:_on_detach`
--- skips the capability on that same check, and `vim.lsp.codelens.enable(false, { client_id })`
--- skips it a second time — the `enable ~= is_enabled` guard in `_capability.enable` already
--- reads equal. `Provider.active` then keeps the buffer, and the next
--- `workspace/codeLens/refresh` walks it into `util.make_text_document_params` →
--- `uri_from_bufnr` → `SERVER_REQUEST_HANDLER_ERROR: … Invalid buffer id`, for a buffer the
--- user has already closed. `vim.lsp._capability` is private and version-dependent; the
--- alternative is a crash inside the editor.
---
--- @param bufnr integer
--- @param client_id integer
local function drop_lens_state(bufnr, client_id)
  local classes = vim.lsp._capability and vim.lsp._capability.all
  local provider = classes and classes.codelens and classes.codelens.active[bufnr]
  if not provider or provider.client_state[client_id] == nil then
    return
  end
  -- Clears this client's lens extmarks — the buffer is still there when a client detaches — and
  -- throws when it is not, which is why the state is dropped either way.
  pcall(provider.on_detach, provider, client_id)
  provider.client_state[client_id] = nil
  if next(provider.client_state) == nil then
    pcall(function()
      provider:destroy()
    end)
  end
end

function M.install_lenses()
  -- `client_id` alone, never `bufnr` and `client_id` together: 0.12.5 accepts the pair, but
  -- 0.12.1 asserts that they are mutually exclusive (`lsp/_capability.lua`, `enable`), and an
  -- error inside `LspAttach` leaves lenses enabled for nobody. One key is enough for the
  -- intent — this server's lenses, on the buffers it is attached to.
  vim.api.nvim_create_autocmd('LspAttach', {
    callback = function(ev)
      local client = vim.lsp.get_client_by_id(ev.data.client_id)
      if client and client.name == M.name and client:supports_method('textDocument/codeLens') then
        vim.lsp.codelens.enable(true, { client_id = ev.data.client_id })
      end
    end,
  })
  -- Stop asking for lenses for a client that has gone. Neovim's lens provider keeps the client
  -- id per buffer and asserts that it still exists when a debounced request fires
  -- (`runtime/lua/vim/lsp/codelens.lua:143`), and `enable(false, { client_id })` cannot purge
  -- that state (`drop_lens_state` says why) — so a stop within the 200 ms debounce window used
  -- to leave a request scheduled for a client that was no longer there.
  vim.api.nvim_create_autocmd('LspDetach', {
    callback = function(ev)
      local client = vim.lsp.get_client_by_id(ev.data.client_id)
      if client and client.name == M.name then
        drop_lens_state(ev.buf, ev.data.client_id)
      end
    end,
  })
  -- And the same drop for the buffer itself going away: the provider Neovim leaves behind still
  -- holds a buffer id that no longer exists, which is what the next
  -- `workspace/codeLens/refresh` trips over.
  local lenses_group = vim.api.nvim_create_augroup('jev.lenses', { clear = true })
  vim.api.nvim_create_autocmd({ 'BufDelete', 'BufWipeout' }, {
    group = lenses_group,
    callback = function(ev)
      local classes = vim.lsp._capability and vim.lsp._capability.all
      local provider = classes and classes.codelens and classes.codelens.active[ev.buf]
      if not provider then
        return
      end
      -- Snapshotted: dropping mutates the table being walked.
      for _, id in ipairs(vim.tbl_keys(provider.client_state)) do
        local client = vim.lsp.get_client_by_id(id)
        -- A client that is gone can no longer be asked for lenses either, so its state is as
        -- dead as the buffer's.
        if client == nil or client.name == M.name then
          drop_lens_state(ev.buf, id)
        end
      end
    end,
  })
  if lenses_wrapped then
    return
  end
  lenses_wrapped = true
  -- The plugin's own command namespace, and the length taken from the string itself: a
  -- `sub(1, 12)` was correct only while the server was called `meta` (twelve characters), and
  -- the rename to `jev` (eleven) made the check silently false — the lens was no longer
  -- intercepted, so the command went to the server, which does not serve it.
  local prefix = 'jev.plugin.'
  local stock = vim.lsp.codelens.run
  vim.lsp.codelens.run = function(opts)
    local bufnr = vim.api.nvim_get_current_buf()
    local row = vim.api.nvim_win_get_cursor(0)[1]
    for _, entry in ipairs(vim.lsp.codelens.get({ bufnr = bufnr })) do
      local lens = entry.lens
      local command = (lens.command and lens.command.command) or ''
      if command:sub(1, #prefix) == prefix and lens.range.start.line + 1 == row then
        if command == prefix .. 'pick' then
          picker.action()
        else
          M.explain()
        end
        return
      end
    end
    return stock(opts)
  end
end

--- The default keymaps, `docs/UX.md` §2: four keys, and the reason there are only four is that
--- eighteen made the plugin unreadable to its own user. `a` is the picker and is also defined
--- in visual mode: it reads the selection and puts it in the request, so the server scopes the
--- action to it (`scope_source = "explicit"`) instead of to the enclosing function.
--- `u` and `q` are the other two products (taking an edit back, asking); `s` is status, which
--- is also where the kill switch lives. Everything else — explain, review, plan, followup,
--- where, hints, cancel, dismiss, stop, start — is reachable by typing (`:Jev …`), and
--- `:Jev usage` is the answer to whether any of it is working.
--- @param prefix? string  default `'<leader>j'`
function M.keymaps(prefix)
  prefix = prefix or '<leader>j'
  -- Four, and no more. Six products behind eighteen keymaps made the plugin's own user unable
  -- to say what it is for: two products survive — findings → actions (`a`) with `u` to take it
  -- back, and ask (`q`) — plus one non-product, `s`, the window onto whether any of it is
  -- working. Everything else still works by typing
  -- (`:Jev explain|review|plan|session|usage|inspect|stop|start`, `docs/UX.md` §2); it just does not
  -- claim a key.
  local maps = {
    { 'a', { 'n', 'x' }, 'code action (picker, summary in the preview pane)', function()
      picker.action()
    end },
    { 'u', { 'n' }, 'undo the last applied edit', function()
      M.undo()
    end },
    { 'q', { 'n' }, 'ask a question', function()
      M.ask()
    end },
    { 's', { 'n' }, 'status: queue, budgets, cache hit rate', function()
      M.status()
    end },
  }
  for _, m in ipairs(maps) do
    vim.keymap.set(m[2], prefix .. m[1], m[4], { desc = 'jev: ' .. m[3] })
  end
end

--- `:Jev …` — `docs/UX.md` §2. Every subcommand is a function above, so the keymaps and the
--- command line reach the same things; no keymap's meaning depends on model state.
--- @type table<string, fun(arg: string|nil)>
M.subcommands = {
  ask = function(arg)
    -- `--web` is the fetch-enabled form: the answer may ask for one https page to be read.
    -- It used to be a keymap of its own (<leader>jW) and lost it when the surface was cut to
    -- four keys, which left the capability unreachable from the editor — a keymap is an
    -- accelerator, and nothing may exist only as one.
    local web = false
    if arg:sub(1, 6) == '--web ' then
      web, arg = true, arg:sub(7)
    end
    M.ask(arg ~= '' and arg or nil, web and { web = true } or nil)
  end,
  cancel = function()
    M.cancel()
  end,
  dismiss = function(arg)
    M.dismiss(arg ~= '' and arg or nil)
  end,
  explain = function()
    M.explain()
  end,
  followup = function(arg)
    M.followup(arg ~= '' and arg or nil)
  end,
  log = function()
    M.log()
  end,
  plan = function(arg)
    M.plan(arg ~= '' and arg or nil)
  end,
  recompute = function()
    M.recompute()
  end,
  where = function(arg)
    M.where(arg ~= '' and arg or nil)
  end,
  hints = function(arg)
    M.hints(arg == 'on' and true or (arg == 'off' and false or nil))
  end,
  inspect = function(arg)
    M.inspect(vim.trim(arg) == '--force')
  end,
  review = function()
    M.review()
  end,
  session = function()
    M.session()
  end,
  start = function()
    M.start()
  end,
  status = function()
    M.status()
  end,
  usage = function()
    M.command('jev.usage', {}, function(err, result, ctx)
      render_artifact('jev.usage', err, result, ctx)
    end)
  end,
  stop = function()
    M.stop()
  end,
  undo = function()
    M.undo()
  end,
}

local SUBCOMMAND_NAMES = vim.tbl_keys(M.subcommands)
table.sort(SUBCOMMAND_NAMES)

function M.create_command()
  vim.api.nvim_create_user_command('Jev', function(cmd)
    local parts = vim.split(cmd.args, '%s+', { trimempty = true })
    local sub = parts[1] or ''
    local fn = M.subcommands[sub]
    if not fn then
      vim.notify(
        ('jev: unknown subcommand %q (try :Jev %s)'):format(sub, table.concat(SUBCOMMAND_NAMES, '|')),
        vim.log.levels.ERROR
      )
      return
    end
    fn(vim.trim(cmd.args:sub(#sub + 1)))
  end, {
    nargs = '*',
    desc = 'jev-lsp: ' .. table.concat(SUBCOMMAND_NAMES, '|'),
    complete = function(lead)
      return vim.tbl_filter(function(name)
        return name:sub(1, #lead) == lead
      end, SUBCOMMAND_NAMES)
    end,
  })
end

--- The layouts a generated surface may be put in (`place_surface`).
local SURFACE_LAYOUTS = { current = true, float = true, split = true }

--- Register the client, enable both attachment ladders, install the surface.
--- @param opts? jev.Opts
function M.setup(opts)
  local install = vim.tbl_extend('force', {}, opts or {})
  local prefix = install.prefix or '<leader>j'
  local keymaps = install.keymaps
  local surfaces = install.surfaces
  install.prefix, install.keymaps, install.surfaces = nil, nil, nil
  -- A layout name that is not a layout would otherwise be silently the default, which is the
  -- one failure mode a "where does the report go" setting cannot have: the user would believe
  -- they had asked for a split. Named and refused instead.
  if type(surfaces) == 'table' and surfaces.layout ~= nil and not SURFACE_LAYOUTS[surfaces.layout] then
    vim.notify(
      ('jev: surfaces.layout = %s is not a layout (current|float|split); keeping %s')
        :format(vim.inspect(surfaces.layout), surface_layout()),
      vim.log.levels.ERROR
    )
    surfaces = nil
  end
  if type(surfaces) == 'table' then
    M.opts.surfaces = vim.tbl_extend('force', M.opts.surfaces, surfaces)
  end
  install.handlers = vim.tbl_deep_extend(
    'force',
    { ['textDocument/diagnostic'] = filter_findings },
    install.handlers or {}
  )

  attach.setup(install)
  vim.lsp.config(M.name, attach.configure())
  vim.lsp.enable(M.name)
  attach.start()

  M.install_lenses()
  M.install_definitions()
  M.install_progress_tracker()
  M.install_snapshot_hook()
  -- The picker's own surfaces: the attempt at the `window/showMessage` dedupe (PROTOCOL §4 —
  -- the same reason also comes back as `disabled.reason`) and the `$/progress` reader that
  -- feeds the spinner while a resolve is outstanding.
  picker.install()
  -- The statusline segment (`docs/UX.md` §1: the statusline is the only place work is
  -- advertised). It is a plain function, so it goes anywhere a statusline expression goes,
  -- and it is empty when no `jev` client is attached or nothing is in flight:
  --
  --   vim.o.statusline = '%{%v:lua.require"jev.statusline".component()%}'
  --   require('lualine').setup({ sections = { lualine_x = { require('jev.statusline').component } } })
  --
  -- It polls `jev.status` (PROTOCOL §6) at most once per 5 s, only while something is in
  -- flight, and never on the main path of a redraw.
  statusline.install()
  M.create_command()
  if keymaps ~= false then
    M.keymaps(prefix)
  end
  return M
end

return M
