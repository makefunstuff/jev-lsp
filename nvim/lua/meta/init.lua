--- meta-lsp plugin entry: client config, the attach pass, the default keymaps, `:Meta`.
---
--- ```lua
--- require('meta').setup({ cmd = { 'meta-lsp' }, settings = { meta = { /* PROTOCOL §10 */ } } })
--- ```
---
--- `setup` registers the client config (`vim.lsp.config('meta', …)`), enables the built-in
--- attach path (`vim.lsp.enable`), installs the universal pass that covers what the built-in
--- path cannot (`require('meta.attach')`, `docs/LANGUAGE.md` §1), the `docs/UX.md` §2 keymaps,
--- and the `:Meta` command.
---
--- Anything else in `vim.lsp.ClientConfig` — `capabilities`, `on_attach`, `flags`, extra
--- `handlers` — is added with `vim.lsp.config('meta', { … })`, which merges, before or after
--- `setup`: the attach pass starts clients from the resolved config, so both ladders agree.
---
--- @module 'meta'

local attach = require('meta.attach')
local picker = require('meta.picker')
local statusline = require('meta.statusline')

local M = {}

M.name = attach.NAME

--- @class meta.Opts : meta.AttachOpts
--- @field keymaps? boolean  `false` leaves every default keymap unset
--- @field prefix? string    Keymap prefix, default `'<leader>m'` (`docs/UX.md` §2)
--- @field settings? table   PROTOCOL §10, under `settings.meta`

--- Requests this plugin sent, by request id, so `:Meta cancel` can cancel them. Each carries
--- the `workDoneToken` whose progress the server reports under it (PROTOCOL §3.5).
local inflight = {} -- request id -> { client = vim.lsp.Client, token = string }

--- Tokens this plugin issued. Everything else seen on `$/progress` was created by the server
--- with `window/workDoneProgress/create` (PROTOCOL §3.5, path 2).
local issued = {} -- token -> true

--- Server-initiated progress, tracked only so it can be cancelled (PROTOCOL §6). What is *shown*
--- for it is `require('meta.statusline')`, which counts the same events for the statusline
--- segment (`docs/UX.md` §1).
local server_progress = {} -- client id -> { [token] = true }

local token_seq = 0

--- The client to talk to: the one attached to this buffer, else any `meta` client.
--- @return vim.lsp.Client?
local function client()
  local bufnr = vim.api.nvim_get_current_buf()
  return vim.lsp.get_clients({ bufnr = bufnr, name = M.name })[1]
    or vim.lsp.get_clients({ name = M.name })[1]
end

--- Report a command outcome. A Result envelope (PROTOCOL §7) is shown as it is; a failure —
--- including `not_implemented` — is reported, never dressed up as success.
--- @param label string
--- @param err table?
--- @param result any
function M.report(label, err, result)
  if err then
    vim.notify(
      ('meta: %s failed: %s'):format(label, err.message or vim.inspect(err)),
      vim.log.levels.WARN
    )
    return
  end
  if type(result) == 'table' and result.ok == false then
    local e = result.error or {}
    vim.notify(
      ('meta: %s: %s (%s)'):format(label, e.message or 'failed', e.code or 'unknown'),
      vim.log.levels.WARN
    )
    return
  end
  vim.notify(
    ('meta: %s: %s'):format(label, vim.inspect(result, { newline = ' ', indent = '' }))
  )
end

--- Mint a `workDoneToken` this plugin issues.
---
--- The first of the two legal token sources (PROTOCOL §3.5): the token rides in the params of
--- the request that will report under it, so no `window/workDoneProgress/create` round trip is
--- needed, and its lifetime is that request's. Minting it here keeps every plugin-issued token
--- in one registry, which is what tells server-initiated progress apart from ours
--- (`install_progress_tracker`, and `:Meta cancel`).
---
--- @return string token
function M.issue_token()
  token_seq = token_seq + 1
  local token = ('meta:%x'):format(token_seq)
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
  vim.keymap.set('n', 'q', '<Cmd>close<CR>', { buffer = bufnr, desc = 'meta: close artifact' })
  vim.cmd('sbuffer ' .. bufnr)
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
    vim.notify('meta: no server attached to this buffer', vim.log.levels.WARN)
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
  if success and request_id then
    inflight[request_id] = { client = c, token = token }
  end
  return request_id
end

--- `:Meta status` — queue, budgets, in-flight calls (PROTOCOL §6).
--- @param cb? fun(err: table?, result: any)
function M.status(cb)
  M.command('meta.status', {}, cb)
end

--- `:Meta recompute` — reanalyse from the cache's point of view; cheap and idempotent.
--- @param cb? fun(err: table?, result: any)
function M.recompute(cb)
  M.command('meta.recompute', {}, cb)
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
  c = { 'function_definition' },
  cpp = { 'function_definition', 'class_specifier' },
  go = { 'function_declaration', 'method_declaration' },
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

--- The scope argument `meta.plan`, `meta.explain` and friends take (PROTOCOL §6): where the
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
    if d.source == 'meta' and d.lnum <= line and line <= (d.end_lnum or d.lnum) then
      local data = d.user_data and d.user_data.lsp and d.user_data.lsp.data
      if type(data) == 'table' and type(data.finding_id) == 'string' then
        return data.finding_id
      end
    end
  end
  return nil
end

--- `:Meta explain` — the cursor's file and line.
---
--- Not a code action: Neovim executes a resolved action's `command` by sending it back to the
--- server (`Client:exec_cmd`), so a code action cannot make the client open a buffer. The
--- plugin asks for the artifact and renders it, and the server never learns a buffer exists.
---
--- The one argument is the position itself: `[{ uri, line }]`, line 0-based.
---
--- @param cb? fun(err: table?, result: any)
function M.explain(cb)
  M.command('meta.explain', { cursor_scope() }, cb or function(err, result, ctx)
    render_artifact('meta.explain', err, result, ctx)
  end, { stream = true })
end

--- `:Meta session` — what this server has done here.
---
--- The record is an append-only log under the repository root's `.git/meta/`, beside the
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
  M.command('meta.session', { { limit = 200 } }, function(err, result)
    if err or (type(result) == 'table' and result.ok == false) then
      M.report('meta.session', err, result)
      return
    end
    local entries = (type(result) == 'table' and result.entries) or {}
    local lines = { '# meta session', '' }
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
        lines[#lines + 1] = ('- analysis — %d finding(s)%s'):format(
          type(e.findings) == 'number' and e.findings or 0,
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
      end, { buffer = bufnr, desc = 'meta: open the place this entry names' })
    end
  end)
end

--- `:Meta followup [question]` / `<leader>Mf` — ask about what is under the cursor.
---
--- The question is the second and last place free text enters, after `plan`, and for the same
--- reason: a picker cannot express a question (N7). The finding at the cursor, when there is
--- one, travels with it — that is the difference between "why is this wrong here" and a
--- question about code in general, and it is why this is one keystroke rather than a chat.
---
--- @param question? string
function M.followup(question)
  local bufnr = vim.api.nvim_get_current_buf()
  local line = vim.api.nvim_win_get_cursor(0)[1] - 1
  local function ask(text)
    local arg = { uri = vim.uri_from_bufnr(bufnr), line = line, question = text }
    local range = treesitter_scope(bufnr, line)
    if range ~= nil then
      arg.range = range
    end
    local id = finding_at(bufnr, line)
    if id ~= nil then
      arg.finding_id = id
    end
    M.command('meta.followup', { arg }, function(err, result, ctx)
      render_artifact('meta.followup', err, result, ctx)
    end, { stream = true })
  end
  if question ~= nil and vim.trim(question) ~= '' then
    ask(vim.trim(question))
    return
  end
  vim.ui.input({ prompt = 'meta: ask about this: ' }, function(input)
    if input ~= nil and vim.trim(input) ~= '' then
      ask(vim.trim(input))
    end
  end)
end

--- Render an artifact in a scratch buffer.
---
--- `docs/UX.md` §6: `q` on a generated buffer leaves buffers, windows, and files exactly as
--- they were — so the buffer is `nofile`, unlisted, wiped on close, and nothing is written.
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
    ('meta://%s/%s'):format(artifact.kind or 'artifact', artifact.id or 'scratch'))
  vim.keymap.set('n', 'q', '<Cmd>close<CR>', { buffer = bufnr, desc = 'meta: close artifact' })
  if vim.api.nvim_get_current_buf() ~= bufnr then
    vim.cmd('sbuffer ' .. bufnr)
  end
  return bufnr
end

--- `:Meta plan [goal]` — `docs/UX.md` §1: free text enters here and nowhere else, because the
--- protocol cannot ask for it ([R1]). With no goal argument the plugin prompts.
---
--- The server returns a plan artifact (targets, per-step verbs, cost). There is no
--- step-through plan buffer yet, so it arrives as a reported Result.
--- @param goal? string
function M.plan(goal)
  if goal ~= nil and vim.trim(goal) ~= '' then
    -- `arguments` is an LSP array, not a map: the server reads `arguments.first()`, and a
    -- map is rejected in transport before the command runs.
    M.command('meta.plan', { { goal = vim.trim(goal), scope = cursor_scope() } })
    return
  end
  vim.ui.input({ prompt = 'meta goal: ' }, function(input)
    if input ~= nil and vim.trim(input) ~= '' then
      M.command('meta.plan', { { goal = vim.trim(input), scope = cursor_scope() } })
    end
  end)
end

--- `:Meta review` — the cursor's file and line, the same argument shape as `meta.explain`.
--- Findings come back in the Result and reach the buffer through pull diagnostics.
function M.review()
  M.command('meta.review', { cursor_scope() })
end

--- `:Meta cancel` / `<leader>mx` — cancel all in-flight work (`docs/UX.md` §2).
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
  vim.notify(('meta: cancelled %d request(s), %d server job(s)'):format(requests, jobs))
end

--- The kill switch, one command with no prompt (PROTOCOL §5, `docs/UX.md` §2).
---
--- `workspace/configuration` is answered from `client.settings` (`lsp/handlers.lua`), so the
--- switch is flipped there *and* in the registered config — which is what a client started
--- later will read — and then the server is told to re-read it. That is the documented
--- mechanism: the server stops issuing model calls immediately and drains what is in flight.
--- @param enabled boolean
function M.set_enabled(enabled)
  vim.lsp.config(M.name, { settings = { meta = { enabled = enabled } } })
  local clients = vim.lsp.get_clients({ name = M.name })
  for _, c in ipairs(clients) do
    c.settings.meta = c.settings.meta or {}
    c.settings.meta.enabled = enabled
    c:notify('workspace/didChangeConfiguration', { settings = c.settings })
  end
  vim.notify(
    ('meta: enabled = %s (%d client(s) notified)'):format(tostring(enabled), #clients)
  )
end

function M.stop()
  M.set_enabled(false)
end

--- `:Meta start` — the inverse of the kill switch, and the pass again for the buffers that
--- were opened while it was off.
function M.start()
  M.set_enabled(true)
  attach.start()
end

--- `:Meta log` — the client log, where the server's stderr and the LSP traffic land.
function M.log()
  vim.cmd('edit ' .. vim.fn.fnameescape(vim.lsp.log.get_filename()))
end

-- Dismissals ---------------------------------------------------------------------------------
--
-- PROTOCOL §9: findings must be dismissible, and a dismissal is recorded in a per-repository
-- file so it does not resurface. `docs/UX.md` §4 fixes the location: `<root>/.git/meta/`,
-- never in the repo tree. The file is the durable record; the set is also read back once per
-- root so a dismissed finding is filtered out of the next pull in the same session.

--- Keys dismissed in this session, across roots.
local dismissed = {} -- finding id -> true
local loaded_roots = {} -- root -> true

--- @param root string
--- @return string
local function dismissals_path(root)
  return root .. '/.git/meta/dismissed.json'
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

--- `:Meta dismiss [finding_id]` / `<leader>md` — PROTOCOL §9, `docs/UX.md` §4.
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
      if d.source == 'meta' and d.lnum == line and data and data.finding_id then
        id, verb, content_hash = data.finding_id, data.verb, data.content_hash
        break
      end
    end
  end
  if id == nil then
    vim.notify('meta: no finding at the cursor to dismiss', vim.log.levels.WARN)
    return
  end
  local root = vim.fs.root(bufnr, { '.git' })
  if not root then
    vim.notify(
      ('meta: %s has no repository root, so there is nowhere to record a dismissal (PROTOCOL §9)')
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
    vim.notify(('meta: dismissed %s and recorded it in %s'):format(id, path))
  end
  dismissed[id] = true
  loaded_roots[root] = true
  M.hide_dismissed(bufnr)
end

--- Drop dismissed findings from what is on screen now, so the sign disappears with the
--- command instead of at the next refresh.
--- @param bufnr integer
function M.hide_dismissed(bufnr)
  for _, c in ipairs(vim.lsp.get_clients({ bufnr = bufnr, name = M.name })) do
    -- `meta` is this server's `diagnosticProvider.identifier` (PROTOCOL §2), which is the
    -- pull id the client keys that namespace by.
    local ns = vim.lsp.diagnostic.get_namespace(c.id, true, 'meta')
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
end

-- Undo --------------------------------------------------------------------------------------
--
-- `docs/UX.md` §3.5: `:Meta undo` restores the snapshot taken before the last applied edit.
-- Neovim's undo-block granularity for an applied `WorkspaceEdit` is unverified ([R10],
-- `docs/research/nvim-lsp-surface.md` §10), so the plugin does not rely on it.
--
-- Every client-applied edit funnels through `vim.lsp.util.apply_workspace_edit`, whether it
-- came from the native menu (`lsp/buf.lua:1260`) or from the server's `workspace/applyEdit`
-- (`lsp/handlers.lua:201,335`), so wrapping that function is the one place that sees them all.

--- Snapshots in application order: `{ { { bufnr, before, after }, … }, … }`.
local snapshots = {}
local snapshots_hooked = false

--- Buffers an edit touches that `meta` is attached to, deduplicated.
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

--- `:Meta undo` / `<leader>mu` — restore the text from before the last applied edit.
---
--- Refused when a buffer has moved since: the restore would throw away typing the snapshot
--- never saw. The refusal is a message, not an edit.
function M.undo()
  local entries = snapshots[#snapshots]
  if not entries then
    vim.notify('meta: no applied edit to undo')
    return
  end
  for _, e in ipairs(entries) do
    if
      not vim.api.nvim_buf_is_valid(e.bufnr)
      or not vim.deep_equal(vim.api.nvim_buf_get_lines(e.bufnr, 0, -1, true), e.after)
    then
      vim.notify('meta: the buffer changed since that edit; undo refused')
      return
    end
  end
  table.remove(snapshots)
  for _, e in ipairs(entries) do
    vim.api.nvim_buf_set_lines(e.bufnr, 0, -1, false, e.before)
  end
  vim.notify(('meta: restored %d buffer(s)'):format(#entries))
end

-- Surface -----------------------------------------------------------------------------------

--- Track server-initiated progress so `:Meta cancel` can cancel it (PROTOCOL §3.5, §6).
function M.install_progress_tracker()
  local group = vim.api.nvim_create_augroup('meta', { clear = true })
  vim.api.nvim_create_autocmd('LspProgress', {
    group = group,
    pattern = '*',
    desc = 'meta: track server-initiated progress so :Meta cancel can cancel it',
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
--- installing this. `<leader>Mh` toggles it for the buffer so it can be tried in one keystroke.
function M.hints(on)
  local bufnr = vim.api.nvim_get_current_buf()
  local enable = on
  if enable == nil then
    enable = not vim.lsp.inlay_hint.is_enabled({ bufnr = bufnr })
  end
  local ok, err = pcall(vim.lsp.inlay_hint.enable, enable, { bufnr = bufnr })
  if not ok then
    vim.notify('meta: inlay hints are not available here: ' .. tostring(err), vim.log.levels.WARN)
    return enable
  end
  vim.notify(('meta: inlay hints %s'):format(enable and 'on' or 'off'), vim.log.levels.INFO)
  return enable
end

--- Code lenses: one affordance per declaration, written on the declaration.
---
--- This is the surface that does not have to be remembered. A clean declaration offers
--- `meta: explain`; one with findings offers `meta: N finding(s) · fix`. The commands are the
--- plugin's own namespace, because opening a buffer is a client decision — the server cannot
--- open one, and `window/showDocument` needs a URI an explanation does not have.
---
--- `vim.lsp.codelens.run()` re-requests the lenses and then sends the command to the
--- *server*, so it is wrapped: ours are handled here, everything else passes through
--- untouched.
local lenses_wrapped = false
local lens_teardown_installed = false

function M.install_lenses()
  vim.api.nvim_create_autocmd('LspAttach', {
    callback = function(ev)
      local client = vim.lsp.get_client_by_id(ev.data.client_id)
      if client and client.name == M.name and client:supports_method('textDocument/codeLens') then
        vim.lsp.codelens.enable(true, { bufnr = ev.buf, client_id = ev.data.client_id })
      end
    end,
  })
  vim.api.nvim_create_autocmd('LspDetach', {
    callback = function(ev)
      local client = vim.lsp.get_client_by_id(ev.data.client_id)
      if client and client.name == M.name then
        pcall(vim.lsp.codelens.enable, false, { bufnr = ev.buf, client_id = ev.data.client_id })
      end
    end,
  })
  -- Stop asking for lenses for a client that has gone. Neovim's lens provider keeps the
  -- client id per buffer and asserts that it still exists when a debounced request fires
  -- (`runtime/lua/vim/lsp/codelens.lua:143`); `enable(false)` does not purge that state, so a
  -- stop within the 200 ms debounce window can still trip the assertion inside the editor.
  -- That is Neovim's, and out of reach here — what this can do is make sure no *new* request
  -- is scheduled for a client that is no longer there.
  vim.api.nvim_create_autocmd('LspDetach', {
    callback = function(ev)
      local client = vim.lsp.get_client_by_id(ev.data.client_id)
      if client and client.name == M.name then
        pcall(vim.lsp.codelens.enable, false, { bufnr = ev.buf, client_id = ev.data.client_id })
      end
    end,
  })
  if lenses_wrapped then
    return
  end
  lenses_wrapped = true
  local stock = vim.lsp.codelens.run
  vim.lsp.codelens.run = function(opts)
    local bufnr = vim.api.nvim_get_current_buf()
    local row = vim.api.nvim_win_get_cursor(0)[1]
    for _, entry in ipairs(vim.lsp.codelens.get({ bufnr = bufnr })) do
      local lens = entry.lens
      local command = (lens.command and lens.command.command) or ''
      if command:sub(1, 12) == 'meta.plugin.' and lens.range.start.line + 1 == row then
        if command == 'meta.plugin.pick' then
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

--- The default keymaps, `docs/UX.md` §2. `<leader>ms` and `<leader>mS` are different keys:
--- `s` is status, `S` is the kill switch, which must be reachable in one mapping without
--- opening anything. `<leader>mG` starts again.
---
--- `<leader>ma` is the plugin's own picker (`require('meta.picker')`, `docs/ROADMAP.md` U4) and
--- is also defined in visual mode: the picker reads the selection and puts it in the request,
--- so the server scopes the action to it (`scope_source = "explicit"`) instead of to the
--- enclosing function. `<leader>mv` is the same flow with the resolved edit opened as a
--- side-by-side diff, applied only on `<CR>` (`docs/UX.md` §3.3).
---
--- The verb keymaps are not selection-aware: the commands they send carry one position
--- (`cursor_scope`), and a keymap that silently dropped a selection would be worse than one
--- that does not exist — `<leader>mt` still filters the *native* menu to the `test` verb's
--- family, which is prefix-matched by the client (`[R4]`).
--- @param prefix? string  default `'<leader>m'`
function M.keymaps(prefix)
  prefix = prefix or '<leader>m'
  local maps = {
    { 'a', { 'n', 'x' }, 'code action (picker, summary in the preview pane)', function()
      picker.action()
    end },
    { 'v', { 'n', 'x' }, 'code action with a diff preview', function()
      picker.action({ preview = true })
    end },
    { 'p', { 'n' }, 'plan for a goal', function()
      M.plan()
    end },
    { 'e', { 'n' }, 'explain scope', function()
      M.explain()
    end },
    { 'f', { 'n' }, 'ask about what is under the cursor', function()
      M.followup()
    end },
    { 'r', { 'n' }, 'review this file', function()
      M.review()
    end },
    { 't', { 'n' }, 'add tests for scope', function()
      -- `test` is a `refactor.rewrite.meta` action (PROTOCOL §4.1). Until the plugin picker
      -- (docs/ROADMAP.md U4) selects a verb, the native menu is filtered to that family;
      -- kind filtering is prefix-based on `.`, so nothing else matches.
      vim.lsp.buf.code_action({ context = { only = { 'refactor.rewrite.meta' } } })
    end },
    { 'd', { 'n' }, 'dismiss finding at cursor', function()
      M.dismiss()
    end },
    { 'l', { 'n' }, 'run the lens on this line', function()
      vim.lsp.codelens.run()
    end },
    { 'h', { 'n' }, 'toggle inlay hints (off by default)', function()
      M.hints()
    end },
    { 's', { 'n' }, 'status: queue, budgets, cache hit rate', function()
      M.status()
    end },
    { 'x', { 'n' }, 'cancel all in-flight work', function()
      M.cancel()
    end },
    { 'u', { 'n' }, 'undo the last applied edit', function()
      M.undo()
    end },
    { 'S', { 'n' }, 'stop: kill switch (PROTOCOL §5)', function()
      M.stop()
    end },
    { 'G', { 'n' }, 'start: re-enable after :Meta stop', function()
      M.start()
    end },
  }
  for _, m in ipairs(maps) do
    vim.keymap.set(m[2], prefix .. m[1], m[4], { desc = 'meta: ' .. m[3] })
  end
end

--- `:Meta …` — `docs/UX.md` §2. Every subcommand is a function above, so the keymaps and the
--- command line reach the same things; no keymap's meaning depends on model state.
--- @type table<string, fun(arg: string|nil)>
M.subcommands = {
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
  hints = function(arg)
    M.hints(arg == 'on' and true or (arg == 'off' and false or nil))
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
  vim.api.nvim_create_user_command('Meta', function(cmd)
    local parts = vim.split(cmd.args, '%s+', { trimempty = true })
    local sub = parts[1] or ''
    local fn = M.subcommands[sub]
    if not fn then
      vim.notify(
        ('meta: unknown subcommand %q (try :Meta %s)'):format(sub, table.concat(SUBCOMMAND_NAMES, '|')),
        vim.log.levels.ERROR
      )
      return
    end
    fn(vim.trim(cmd.args:sub(#sub + 1)))
  end, {
    nargs = '*',
    desc = 'meta-lsp: ' .. table.concat(SUBCOMMAND_NAMES, '|'),
    complete = function(lead)
      return vim.tbl_filter(function(name)
        return name:sub(1, #lead) == lead
      end, SUBCOMMAND_NAMES)
    end,
  })
end

--- Register the client, enable both attachment ladders, install the surface.
--- @param opts? meta.Opts
function M.setup(opts)
  local install = vim.tbl_extend('force', {}, opts or {})
  local prefix = install.prefix or '<leader>m'
  local keymaps = install.keymaps
  install.prefix, install.keymaps = nil, nil
  install.handlers = vim.tbl_deep_extend(
    'force',
    { ['textDocument/diagnostic'] = filter_findings },
    install.handlers or {}
  )

  -- Inline completion is off by default (PROTOCOL §10, docs/UX.md §3.4). When the settings
  -- turn it on, enable Neovim's own machinery: the server advertises
  -- `inlineCompletionProvider`, but the client only attaches its completor once enabled, and
  -- it fires on a 200 ms timer in insert mode (`lsp/inline_completion.lua`).
  local inline = install.settings
    and install.settings.inline_completion
    and install.settings.inline_completion.enabled
  if inline then
    vim.api.nvim_create_autocmd('LspAttach', {
      callback = function(ev)
        local client = vim.lsp.get_client_by_id(ev.data.client_id)
        if client and client.name == M.name
          and client:supports_method('textDocument/inlineCompletion')
        then
          vim.lsp.inline_completion.enable(true, { bufnr = ev.buf, client_id = ev.data.client_id })
        end
      end,
    })
  end

  attach.setup(install)
  vim.lsp.config(M.name, attach.configure())
  vim.lsp.enable(M.name)
  attach.start()

  M.install_lenses()
  M.install_progress_tracker()
  M.install_snapshot_hook()
  -- The picker's own surfaces: the attempt at the `window/showMessage` dedupe (PROTOCOL §4 —
  -- the same reason also comes back as `disabled.reason`) and the `$/progress` reader that
  -- feeds the spinner while a resolve is outstanding.
  picker.install()
  -- The statusline segment (`docs/UX.md` §1: the statusline is the only place work is
  -- advertised). It is a plain function, so it goes anywhere a statusline expression goes,
  -- and it is empty when no `meta` client is attached or nothing is in flight:
  --
  --   vim.o.statusline = '%{%v:lua.require"meta.statusline".component()%}'
  --   require('lualine').setup({ sections = { lualine_x = { require('meta.statusline').component } } })
  --
  -- It polls `meta.status` (PROTOCOL §6) at most once per 5 s, only while something is in
  -- flight, and never on the main path of a redraw.
  statusline.install()
  M.create_command()
  if keymaps ~= false then
    M.keymaps(prefix)
  end
  return M
end

return M
