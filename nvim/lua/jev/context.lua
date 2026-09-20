--- What the editor can see that the server cannot (PROTOCOL §6.1).
---
--- The server has no parser, no other language servers, and no list of what the user has been
--- looking at. This side has all three, so it assembles the project context and sends it with
--- the request that generates. The server bounds it (`MAX_PROVIDED_DOCS`, 40 lines a piece)
--- and hashes it into the cache key, so a request whose context differs is a different
--- request — which is the whole reason this is safe to cache.
---
--- Deterministic on purpose: a fixed kind order, and within a kind, the order the sources
--- produced. The same editing session asks the same question twice and gets a cache hit; a
--- different project state does not.
---
--- @module 'jev.context'

local M = {}

--- How much of any one file travels. The server truncates at 40 too; this keeps the wire
--- honest rather than relying on the other end to do it.
local WINDOW_LINES = 40

--- How many open buffers count as siblings, most recently touched last.
local SIBLINGS = 3

--- Node types that pull something into a file, per parser language. Short by design: a
--- language that is missing sends no imports, which costs context and nothing else.
local TS_IMPORT_NODES = {
  c = { 'preproc_include' },
  cpp = { 'preproc_include' },
  go = { 'import_declaration' },
  javascript = { 'import_statement' },
  lua = { 'variable_declaration' },
  python = { 'import_statement', 'import_from_statement' },
  rust = { 'use_declaration' },
  typescript = { 'import_statement' },
}

--- @param name string
--- @return boolean
local function is_file_buffer(bufnr)
  return vim.api.nvim_buf_is_valid(bufnr)
    and vim.bo[bufnr].buftype == ''
    and vim.api.nvim_buf_get_name(bufnr) ~= ''
end

--- @return string?
local function parser_language(bufnr)
  local ok, lang = pcall(vim.treesitter.language.get_lang, vim.bo[bufnr].filetype)
  if not ok then
    return nil
  end
  return lang
end

--- Lines `start..end` of a buffer, joined. Zero-based, inclusive.
--- @return string
local function lines_of(bufnr, start, finish)
  local last = vim.api.nvim_buf_line_count(bufnr)
  local from = math.max(0, math.min(start, last - 1))
  local to = math.max(from, math.min(finish, last - 1, from + WINDOW_LINES - 1))
  return table.concat(vim.api.nvim_buf_get_lines(bufnr, from, to + 1, false), '\n')
end

--- The imports of a buffer, per the parser.
--- @return table[]  provided entries
local function imports_of(bufnr)
  local lang = parser_language(bufnr)
  local types = lang and TS_IMPORT_NODES[lang]
  if types == nil then
    return {}
  end
  local wanted = {}
  for _, t in ipairs(types) do
    wanted[t] = true
  end
  local ok, found = pcall(function()
    local parser = vim.treesitter.get_parser(bufnr, lang)
    local root = parser:parse()[1]:root()
    local first, last
    for child in root:iter_children() do
      if wanted[child:type()] then
        local start_line, _, end_line = child:range()
        first = first or start_line
        last = end_line
      end
    end
    return first and { first, last } or nil
  end)
  if not ok or type(found) ~= 'table' then
    return {}
  end
  return {
    {
      kind = 'imports',
      uri = vim.uri_from_bufnr(bufnr),
      start_line = found[1],
      end_line = found[2],
      text = lines_of(bufnr, found[1], found[2]),
    },
  }
end

--- The file that looks like this one's test, if one is next to it.
---
--- A convention, not a search: the point is to give the model the expectations that already
--- exist, and a convention that misses costs context while a glob that walks the tree costs
--- milliseconds on every request.
--- @param path string
--- @return string[]  candidate paths
local function test_candidates(path)
  local dir = vim.fn.fnamemodify(path, ':h')
  local base = vim.fn.fnamemodify(path, ':t')
  local stem, ext = base:match('^(.*)%.([^.]+)$')
  if stem == nil then
    return {}
  end
  local names = {
    ('%s_test.%s'):format(stem, ext),
    ('test_%s.%s'):format(stem, ext),
    ('%s.test.%s'):format(stem, ext),
    ('%s.spec.%s'):format(stem, ext),
    ('%s_test.go'):format(stem),
  }
  local out = {}
  for _, name in ipairs(names) do
    out[#out + 1] = dir .. '/' .. name
  end
  return out
end

--- @return table[]
local function test_of(bufnr)
  local path = vim.api.nvim_buf_get_name(bufnr)
  for _, candidate in ipairs(test_candidates(path)) do
    if vim.fn.filereadable(candidate) == 1 and candidate ~= path then
      local lines = vim.fn.readfile(candidate, '', WINDOW_LINES)
      return {
        {
          kind = 'test',
          uri = 'file://' .. candidate,
          start_line = 0,
          end_line = math.max(0, #lines - 1),
          text = table.concat(lines, '\n'),
        },
      }
    end
  end
  return {}
end

--- What the *other* language servers say refers to the symbol under the cursor.
---
--- Asked of them rather than guessed: `textDocument/references` is what an index is for, and
--- this editor is already running the ones that have one. A short timeout, because a request
--- that generates is the slow path by design and this must not be what makes it slow.
--- @return table[]
local function references_of(bufnr, line, character)
  local servers = {}
  for _, c in ipairs(vim.lsp.get_clients({ bufnr = bufnr })) do
    if c.name ~= 'jev' and c:supports_method('textDocument/references') then
      servers[#servers + 1] = c
    end
  end
  if #servers == 0 then
    return {}
  end
  local out = {}
  for _, c in ipairs(servers) do
    local ok, responses = pcall(vim.lsp.buf_request_sync, bufnr, 'textDocument/references', {
      textDocument = { uri = vim.uri_from_bufnr(bufnr) },
      position = { line = line, character = character or 0 },
      context = { includeDeclaration = false },
    }, 300)
    if ok and type(responses) == 'table' then
      for client_id, response in pairs(responses) do
        if client_id ~= c.id or true then
          for _, loc in ipairs((response or {}).result or {}) do
            if #out >= 2 then
              return out
            end
            local uri = type(loc.uri) == 'string' and loc.uri or nil
            local start_line = loc.range and loc.range.start and loc.range.start.line or 0
            if uri ~= nil and uri ~= vim.uri_from_bufnr(bufnr) then
              local buf = vim.uri_to_bufnr(uri)
              local text = nil
              if vim.api.nvim_buf_is_loaded(buf) then
                text = lines_of(buf, start_line, start_line + 6)
              elseif vim.fn.filereadable(vim.uri_to_fname(uri)) == 1 then
                text = table.concat(vim.fn.readfile(vim.uri_to_fname(uri), '', start_line + 7), '\n')
              end
              if text ~= nil and text ~= '' then
                out[#out + 1] = {
                  kind = 'reference',
                  uri = uri,
                  start_line = start_line,
                  end_line = start_line + 6,
                  text = text,
                }
              end
            end
          end
        end
      end
    end
  end
  return out
end

--- The loaded file buffers, taken in a fixed order.
---
--- Sorted by uri rather than by how recently they were visited, which is a decision about the
--- cache rather than about context: recency changes every time the user switches buffers, so a
--- context that included it would differ for questions whose *content* did not — and a cache
--- that never hits is a model call every time. Which buffers are loaded is stable while the
--- user works; the order they were visited in is not.
--- @return table[]
local function siblings_of(bufnr)
  local candidates = {}
  for _, b in ipairs(vim.api.nvim_list_bufs()) do
    if b ~= bufnr and is_file_buffer(b) and vim.api.nvim_buf_is_loaded(b) then
      candidates[#candidates + 1] = b
    end
  end
  table.sort(candidates, function(a, b)
    return vim.api.nvim_buf_get_name(a) < vim.api.nvim_buf_get_name(b)
  end)
  local out = {}
  for _, b in ipairs(candidates) do
    out[#out + 1] = {
      kind = 'sibling',
      uri = vim.uri_from_bufnr(b),
      start_line = 0,
      end_line = WINDOW_LINES - 1,
      text = lines_of(b, 0, WINDOW_LINES - 1),
    }
    if #out >= SIBLINGS then
      break
    end
  end
  return out
end

--- Where a question's words appear in the project, found locally.
---
--- The one navigation question a model answers better than an index: "where is retry handled"
--- is not a symbol, so `textDocument/references` has nothing to say about it. The client greps
--- — offline, in milliseconds — and the model ranks what came back. Semantic search without an
--- embedding store and without a daemon walking the tree.
---
--- The words are the question's own, minus the ones that appear in every question. A grep that
--- matched "where" would return the whole repository, which is worse than nothing.
---
--- The answer says *why* it is empty, because two different states look the same from here: a
--- project that does not mention the word, and a search that never ran (no engine installed, a
--- pattern the engine refused). The first is an answer, the second is a defect the user has to
--- hear about — the model would otherwise be asked about a file it cannot see, and nothing would
--- say so.
--- @param question string
--- @param bufnr integer
--- @return table[] matches
--- @return string? why  nil when the search ran (empty means "no match"); else why it did not
function M.matches_for(question, bufnr)
  -- Bracketed where a word is also a Lua keyword (`and`, `for`, `do`, `in`).
  local stopwords = {
    where = true, what = true, which = true, when = true, how = true, why = true,
    is = true, are = true, the = true, this = true, that = true, ['and'] = true,
    ['for'] = true, with = true, does = true, ['do'] = true, ['in'] = true, on = true,
    to = true, of = true, a = true, an = true, it = true, be = true, handle = true,
    handles = true, handled = true, code = true, file = true, files = true,
    ['function'] = true, ['functions'] = true, called = true, calls = true, used = true,
  }
  local words = {}
  for word in question:lower():gmatch('[%w_]+') do
    if #word > 2 and not stopwords[word] then
      words[#words + 1] = word
    end
  end
  if #words == 0 then
    return {}
  end

  -- The project root, or the file's own directory when nothing marks one: grepping the current
  -- directory would search wherever Neovim happened to be started, which is not where the file
  -- the user is looking at lives.
  local root = vim.fs.root(bufnr, { '.git' })
    or vim.fn.fnamemodify(vim.api.nvim_buf_get_name(bufnr), ':h')
    or vim.fn.getcwd()
  local pattern = table.concat(vim.tbl_map(vim.pesc, words), '|')

  -- Two engines, and the fallback has to return the same *kind* of answer as the first:
  --
  --   `-i` — every word was lowercased above, so the pattern never has an uppercase letter and
  --     `rg --smart-case` is therefore case-insensitive. Case-sensitive `grep` answered a
  --     different question (it missed `Retry` for the word `retry`), which on a small project
  --     means the fallback returns nothing at all.
  --   `-I` and `--exclude-dir=.git` — `grep -r` has no ignore rules: without these it reports
  --     hits inside the object store and lines from files it calls binary, which do not parse
  --     into `path:line:` and quietly count against the six kept below.
  --
  -- What the fallback cannot be is *equal*: `rg` reads `.gitignore` and skips what it names, and
  -- reproducing that here would be a gitignore implementation. The difference is therefore in the
  -- direction of more matches, from trees the user has chosen to ignore — named here rather than
  -- papered over.
  local engine = nil
  if vim.fn.executable('rg') == 1 then
    engine = {
      'rg', '--line-number', '--no-heading', '--smart-case', '--max-count', '2',
      '--max-filesize', '1M', '--', pattern, root,
    }
  elseif vim.fn.executable('grep') == 1 then
    engine = { 'grep', '-rnE', '-i', '-I', '--exclude-dir=.git', '-m', '2', pattern, root }
  end
  if engine == nil then
    return {}, 'neither `rg` nor `grep` is installed'
  end

  local found = vim.fn.systemlist(engine)
  -- Both engines: 0 has matches, 1 is "no matches", and above that is the search itself failing
  -- (an unreadable directory, a pattern it refused). Only the last one is not an answer.
  if vim.v.shell_error > 1 then
    return {}, ('`%s` exited %d'):format(engine[1], vim.v.shell_error)
  end

  local out, seen = {}, {}
  for _, line in ipairs(found) do
    local path, number = line:match('^([^:]+):(%d+):')
    if path ~= nil and number ~= nil and #out < 6 then
      local key = path .. ':' .. number
      if not seen[key] then
        seen[key] = true
        local text = vim.fn.readfile(path, '', tonumber(number) + 2)
        out[#out + 1] = {
          kind = 'match',
          uri = 'file://' .. path,
          start_line = tonumber(number) - 1,
          end_line = math.max(0, #text - 1),
          text = table.concat(text, '\n'),
        }
      end
    end
  end
  return out
end

--- What is cheap enough to keep for every document, all the time.
---
--- Imports come from a parser that has already parsed, and siblings are buffer text — neither
--- costs a round trip. References are *not* here on purpose: they are a request to another
--- language server, which is worth paying when the user asks for something and not worth
--- paying on a keystroke.
--- @param bufnr integer
--- @return table[]
function M.standing(bufnr)
  if not is_file_buffer(bufnr) then
    return {}
  end
  local out = {}
  for _, provider in ipairs({ imports_of, siblings_of }) do
    local ok, part = pcall(provider, bufnr)
    if ok then
      for _, entry in ipairs(part) do
        out[#out + 1] = entry
      end
    end
  end
  return out
end

--- Everything this editor can contribute about one position.
---
--- Order is fixed — imports, references, test, siblings — and the server re-sorts by kind
--- anyway, so the same state produces byte-identical context and therefore a cache hit.
--- @param bufnr integer
--- @param line integer  zero-based
--- @return table[]  provided entries, possibly empty
function M.for_position(bufnr, line, character)
  if not is_file_buffer(bufnr) then
    return {}
  end
  local out = {}
  for _, provider in ipairs({ imports_of, test_of }) do
    local ok, part = pcall(provider, bufnr)
    if ok then
      for _, entry in ipairs(part) do
        out[#out + 1] = entry
      end
    end
  end
  local ok, refs = pcall(references_of, bufnr, line, character)
  if ok then
    for _, entry in ipairs(refs) do
      out[#out + 1] = entry
    end
  end
  local ok2, sibs = pcall(siblings_of, bufnr)
  if ok2 then
    for _, entry in ipairs(sibs) do
      out[#out + 1] = entry
    end
  end
  return out
end

return M
