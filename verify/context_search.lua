-- The client's own local search: both engines, and what it says when it cannot search at all.
--
--   nvim --headless -u NONE -l verify/context_search.lua
--
-- `JEV_ROOT` names the fixture workspace; without it the harness makes one with
-- `vim.fn.tempname()` and **removes it again on the way out** (green, red, or skipped),
-- so `/tmp` does not fill up with repository markers. Name one when a failure needs
-- reading afterwards — a root the caller named is left exactly where it is.
--
-- No server, no model, no network: `context.matches_for` is the client grepping the project for
-- the words of a `:Jev where` question, and the two things this pins are both silent failures.
--
--   * The fallback for a machine without `ripgrep` answered a *different question*. The pattern
--     is lowercased out of the question, so `rg --smart-case` is case-insensitive — and plain
--     `grep -rnE` is not. On a fixture whose match is `Retry` for the word `retry`, the fallback
--     returned nothing, and `grep` driven by hand on the same words looked fine, which is why it
--     took a PATH with `rg` hidden to see it.
--   * An empty result and a search that never ran were the same empty table. No engine installed,
--     an engine that refused the pattern: the question went to the model with no matches and
--     nothing said, so the model answered about a file it could not see.
--
-- Asserted, in order:
--
--   1. the fixture: a match with a capital (`Retry`) and a match with none (`retry`), in a
--      repository root, so both the case difference and the ordinary case are in play;
--   2. the `rg` path returns both (SKIP when `rg` is not installed — CI installs it);
--   3. the same question with `rg` hidden from `PATH` returns the *same* matches, file for file
--      and line for line — the equivalence the fallback has to have;
--   4. with neither engine on `PATH`: no matches **and** the reason, and `:Jev where` says the
--      reason out loud rather than passing an empty context off as an answer.
--
-- Prints ok/FAIL/SKIP per check. Exit is nonzero only on FAIL.

local failures, skips = 0, 0

local function say(line)
  io.stdout:write(line .. '\n')
  io.stdout:flush()
end

local function check(cond, label, detail)
  if cond then
    say('ok    ' .. label)
  else
    failures = failures + 1
    say('FAIL  ' .. label .. (detail ~= nil and ('  — ' .. tostring(detail)) or ''))
  end
  return cond
end

local function skip(label, detail)
  skips = skips + 1
  say('SKIP  ' .. label .. (detail ~= nil and ('  — ' .. tostring(detail)) or ''))
end

-- Setup ---------------------------------------------------------------------------------------

local here = debug.getinfo(1, 'S').source:sub(2)
vim.opt.runtimepath:prepend(vim.fn.fnamemodify(here, ':p:h:h') .. '/nvim')
local context = require('jev.context')

-- A repository root for the search to walk, and — like every harness here — one this harness
-- removes again: `/tmp` accumulating repository markers is what made `/tmp/.git` somebody's
-- workspace. A `JEV_ROOT` the caller named is left where it is.
local fixture_root = dofile(vim.fn.fnamemodify(here, ':p:h') .. '/fixture.lua')
local root, owned_root = fixture_root.root('JEV_ROOT', '-jev-search')
vim.fn.mkdir(root .. '/.git', 'p')
vim.fn.writefile({ 'def Retry(fn):', '    return backoff(fn)' }, root .. '/typed.py')
vim.fn.writefile({ 'def retry(fn):', '    return fn()' }, root .. '/lower.py')

local bufnr = vim.fn.bufadd(root .. '/lower.py')
vim.fn.bufload(bufnr)

say('[search] fixture : ' .. root)
say('[search] rg       : ' .. (vim.fn.executable('rg') == 1 and vim.fn.exepath('rg') or '(not installed)'))
say('[search] grep     : ' .. (vim.fn.executable('grep') == 1 and vim.fn.exepath('grep') or '(not installed)'))

-- The two other directories this harness makes — the `PATH` a fallback runs under, and an empty
-- one — are named here so the same cleanup takes them: every path created with `tempname()` is
-- this harness's to remove.
local shim, empty = nil, nil

--- The matches as `basename:line`, sorted, so two runs can be compared without caring about the
--- order either engine happened to produce them in. The basename, because macOS's `tempname()`
--- hands out `/var/…` while a resolved path reads `/private/var/…`, and which *file* matched is
--- what this is about.
--- @param matches table[]
--- @return string[]
local function cited(matches)
  local out = {}
  for _, m in ipairs(matches) do
    out[#out + 1] = ('%s:%d'):format(
      vim.fn.fnamemodify(vim.uri_to_fname(m.uri), ':t'), m.start_line + 1)
  end
  table.sort(out)
  return out
end

local QUESTION = 'where is the retry handled'

--- `matches_for` under a `PATH` of our choosing, with the real one put back afterwards.
--- @param path string|nil  nil keeps the current PATH
local function search_with_path(path, fn)
  local saved = vim.env.PATH
  if path ~= nil then
    vim.env.PATH = path
  end
  local ok, matches, why = pcall(fn)
  vim.env.PATH = saved
  if not ok then
    return {}, 'raised: ' .. tostring(matches)
  end
  return matches, why
end

-- 1. The engines the run has ---------------------------------------------------------------

if vim.fn.executable('rg') ~= 1 then
  skip('the rg path returns both matches', 'rg is not installed on this machine')
else
  local matches, why = search_with_path(nil, function()
    return context.matches_for(QUESTION, bufnr)
  end)
  local got = cited(matches)
  check(why == nil, 'a search that ran reports no reason', tostring(why))
  check(
    vim.deep_equal(got, { 'lower.py:1', 'typed.py:1' }),
    'the rg path finds both the capitalised and the lowercase match',
    vim.inspect(got)
  )
end

-- 2. The same question with rg hidden --------------------------------------------------------

if vim.fn.executable('grep') ~= 1 then
  skip('the fallback finds what rg found', 'grep is not installed on this machine')
else
  -- A shim directory holding what the search needs and *not* rg. `grep` is symlinked rather than
  -- copied: the point is this machine's grep with the other engine absent.
  shim = vim.fn.tempname() .. '-only-grep'
  vim.fn.mkdir(shim, 'p')
  vim.uv.fs_symlink(vim.fn.exepath('grep'), shim .. '/grep')
  local rg_visible_through_the_shim = nil
  local matches, why = search_with_path(shim, function()
    rg_visible_through_the_shim = vim.fn.executable('rg')
    return context.matches_for(QUESTION, bufnr)
  end)
  local got = cited(matches)

  check(
    rg_visible_through_the_shim == 0,
    'rg is not on the PATH the fallback ran with',
    tostring(rg_visible_through_the_shim)
  )
  check(why == nil, 'the fallback reports no reason: it searched', tostring(why))
  check(
    vim.deep_equal(got, { 'lower.py:1', 'typed.py:1' }),
    'the fallback cites the same files and lines the rg path does, capitalised match included',
    vim.inspect(got)
  )
end

-- 3. No engine at all ------------------------------------------------------------------------

do
  empty = vim.fn.tempname() .. '-no-engine'
  vim.fn.mkdir(empty, 'p')
  local matches, why = search_with_path(empty, function()
    return context.matches_for(QUESTION, bufnr)
  end)
  check(#matches == 0, 'with no engine the result is empty', vim.inspect(cited(matches)))
  check(
    type(why) == 'string' and why:find('rg', 1, true) ~= nil and why:find('grep', 1, true) ~= nil,
    'and the reason names both engines it looked for',
    tostring(why)
  )

  -- And the user hears it: `:Jev where` says why instead of sending the question with no context
  -- and letting the answer read as "the project does not mention it".
  vim.api.nvim_set_current_buf(bufnr)
  local said = {}
  local real_notify = vim.notify
  vim.notify = function(msg, ...)
    said[#said + 1] = tostring(msg)
    return real_notify(msg, ...)
  end
  search_with_path(empty, function()
    pcall(function()
      require('jev').where(QUESTION)
    end)
    return {}
  end)
  vim.notify = real_notify
  local text = table.concat(said, ' ')
  check(
    text:find('where could not search', 1, true) ~= nil and text:find('rg', 1, true) ~= nil,
    ':Jev where says why the search did not run',
    text == '' and 'said nothing' or text
  )
end

-- Report --------------------------------------------------------------------------------------

-- Everything this harness created, gone on the way out — green, red, or after a skip. The root,
-- the `PATH` shim and the empty directory: `/tmp` keeps no marker, and a caller who named a
-- `JEV_ROOT` still has it.
fixture_root.remove(shim, shim ~= nil)
fixture_root.remove(empty, empty ~= nil)
fixture_root.remove(root, owned_root)
say(('[search] %d failure(s), %d skip(s)'):format(failures, skips))
os.exit(failures == 0 and 0 or 1)
