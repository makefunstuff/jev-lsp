-- Live Neovim: the repository's rules, end to end, through the plugin and the server.
--
--   JEV_LSP_BIN=/path/to/jev-lsp \
--   JEV_DECIDE_BASE_URL=http://127.0.0.1:8099/v1 JEV_DECIDE_MODEL=stub-model \
--   JEV_BASE_URL=http://127.0.0.1:8099/v1 JEV_MODEL=stub-model \
--     nvim --headless -u NONE -l verify/rules_live.lua
--
-- The ambient pass is *rules*: plain-English conventions in `<root>/.jev/rules/*.json`, each
-- with an `inspection` (a regex over the file) that names candidates and a `judgement` (one
-- question for the decision model, with a `min_probability` gate). A candidate the decision
-- answers `true` above the floor becomes an ordinary finding — the same `findings::build`
-- path as a review finding — so it must reach the sign column like any other diagnostic.
--
-- The decision model is Jev (`POST {base_url}/systemone`), not a chat model. `JEV_DECIDE_*`
-- points that tier at the stub; the defaults (`https://api.typesafe.ai/v1`, TYPESAFE_API_KEY)
-- are never required by this harness. `JEV_BASE_URL`/`JEV_MODEL` are exported too so the chat
-- tiers cannot reach the network either.
--
-- Asserts, in order:
--
--   1. the fixture repository exists: `.git/`, a Rust file with a `.unwrap()` outside tests,
--      and a `.jev/rules/example.json` that decodes to the schema under test;
--   2. on save the confirmed candidate reaches the sign column as a `source == "jev"`
--      diagnostic on the `.unwrap()` line, its message carrying the rule title and the
--      judgement, and its data carrying `finding_id`/`verb`/`content_hash`/`source = "rules"`;
--   3. `workspace/executeCommand` `jev.inspect {path, force=true}` — driven through a real
--      client, not a keymap — answers `ok`, the same finding, and numeric counts;
--   4. `:Jev dismiss` removes it and it stays gone on a fresh pull;
--   5. `:Jev usage` counts it, and the record it counts from attributes the analysis to
--      `source = "rules"`;
--   6. `:checkhealth jev` names the decide tier's endpoint.
--
-- A missing or unreachable decision endpoint is a SKIP with the reason for checks 2-5, never a
-- FAIL and never an ok: at this end of the connection a dead endpoint and a rules pass that
-- found nothing look identical. Every wait is bounded, so the run cannot hang.
--
-- Prints ok/FAIL/SKIP per check. Exit is nonzero only on FAIL; exit 2 when JEV_LSP_BIN is unset.

local BIN = os.getenv('JEV_LSP_BIN')
if BIN == nil or BIN == '' then
  io.stderr:write(
    'rules_live: JEV_LSP_BIN is not set and is required (path to the jev-lsp server).\n'
      .. '  usage: JEV_LSP_BIN=/path/to/jev-lsp JEV_DECIDE_BASE_URL=http://127.0.0.1:8099/v1 '
      .. 'nvim --headless -u NONE -l verify/rules_live.lua\n'
  )
  io.stderr:flush()
  os.exit(2)
end

local DECIDE_URL = os.getenv('JEV_DECIDE_BASE_URL') or ''

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

-- The rule file, exactly as a repository would write it (PROTOCOL §9 / `jev.rules/1`).
local RULE = [[
{
  "schema": "jev.rules/1",
  "rules": [
    {
      "id": "no-unwrap-in-handlers",
      "title": "Unwrap in a request handler",
      "text": "A handler must not unwrap; return the error instead.",
      "severity": "warning",
      "applies_to": ["**/*.rs"],
      "inspection": {"kind": "regex", "pattern": "\\.unwrap\\(\\)", "max_matches": 0},
      "judgement": {
        "question": "Is this unwrap reachable from a request handler?",
        "criteria": {
          "true": "the call sits on a path a request can reach",
          "false": "the call is in a test, a startup path, or behind an invariant"
        },
        "min_probability": 0.75
      },
      "verb_hint": "fix"
    }
  ]
}
]]

-- One `.unwrap()` in a non-test position. The line is asserted against, so it is named here.
local RUST = {
  'use std::fs;',
  '',
  'pub fn handle(path: &str) -> String {',
  '    let body = fs::read_to_string(path).unwrap();',
  '    body',
  '}',
}
local UNWRAP_LINE = 3 -- 0-based: `    let body = fs::read_to_string(path).unwrap();`
local RULE_TITLE = 'Unwrap in a request handler'
local RULE_TEXT = 'A handler must not unwrap; return the error instead.'

-- Setup ---------------------------------------------------------------------------------------

local here = debug.getinfo(1, 'S').source:sub(2)
local PLUGIN = vim.fn.fnamemodify(here, ':p:h:h') .. '/nvim'
vim.opt.runtimepath:prepend(PLUGIN)

-- Surface what the server logged if anything fails: a dead endpoint in the decide tier looks
-- exactly like a rules pass that found nothing from this side of the connection.
local server_log = dofile(vim.fn.fnamemodify(here, ':p:h') .. '/harness_log.lua')

-- A repository root: the plugin keys the session record and dismissals on `.git`. A root this
-- harness created is removed on the way out; a `JEV_ROOT` the caller named is left where it is.
local fixture_root = dofile(vim.fn.fnamemodify(here, ':p:h') .. '/fixture.lua')
local root, owned_root = fixture_root.root('JEV_ROOT', '-jev-rules')
vim.fn.mkdir(root .. '/.git', 'p')
vim.fn.mkdir(root .. '/.jev/rules', 'p')
local src = root .. '/handler.rs'
vim.fn.writefile(RUST, src)
local rule_path = root .. '/.jev/rules/example.json'
vim.fn.writefile(vim.split(RULE, '\n', { plain = true }), rule_path)

say('[rules] plugin : ' .. PLUGIN)
say('[rules] server : ' .. BIN)
say('[rules] decide : ' .. (DECIDE_URL == '' and '(unset)' or DECIDE_URL))
say('[rules] fixture: ' .. root)

-- 1. The fixture repository ------------------------------------------------------------------

check(vim.fn.isdirectory(root .. '/.git') == 1, '.git/ is present, so the repository root is resolvable')
check(
  vim.fn.filereadable(rule_path) == 1,
  'the rules file is readable at .jev/rules/example.json'
)
do
  local raw = table.concat(vim.fn.readfile(rule_path), '\n')
  local decoded = vim.json.decode(raw)
  local rule = type(decoded) == 'table' and decoded.rules and decoded.rules[1] or nil
  check(
    type(decoded) == 'table' and decoded.schema == 'jev.rules/1' and rule ~= nil,
    'the rules file decodes and carries schema jev.rules/1',
    raw
  )
  check(
    rule ~= nil and rule.id == 'no-unwrap-in-handlers' and rule.applies_to[1] == '**/*.rs',
    'the rule applies to **/*.rs and carries the id the finding is keyed on'
  )
end
local unwrap_text = RUST[UNWRAP_LINE + 1]
check(
  unwrap_text:find('.unwrap()', 1, true) ~= nil,
  "the Rust fixture holds the .unwrap() the rule's inspection names",
  unwrap_text
)

-- The decision endpoint's liveness decides SKIP vs FAIL for checks 2-5. `curl` is bounded by
-- `--max-time`, so a black-holed endpoint costs two seconds and not a hang.
local endpoint_ok, endpoint_detail = false, nil
do
  local origin = DECIDE_URL:match('^(https?://[^/]+)')
  if DECIDE_URL == '' then
    endpoint_detail = 'JEV_DECIDE_BASE_URL is unset, so no decision endpoint was promised'
  elseif origin == nil then
    endpoint_detail = ('JEV_DECIDE_BASE_URL is not an http(s) URL: %s'):format(DECIDE_URL)
  elseif vim.fn.executable('curl') ~= 1 then
    endpoint_detail = 'curl is not installed, so the decision endpoint cannot be probed'
  else
    local probe = vim
      .system({ 'curl', '-fsS', '-o', '/dev/null', '--max-time', '2', origin .. '/health' }, { text = true })
      :wait()
    if probe.code == 0 then
      endpoint_ok = true
    else
      endpoint_detail =
        ('%s/health did not answer (curl exit %s): %s'):format(origin, tostring(probe.code), vim.trim(probe.stderr or ''))
    end
  end
end

-- 2. On save, the finding reaches the sign column --------------------------------------------

require('jev').setup({ cmd = { BIN }, keymaps = false })
vim.cmd('edit ' .. vim.fn.fnameescape(src))
local buf = vim.api.nvim_get_current_buf()

local attached = vim.wait(10000, function()
  return #vim.lsp.get_clients({ bufnr = buf, name = 'jev' }) > 0
end, 25)
check(attached, 'the plugin attached a client to the rules fixture')

server_log.capture()

--- Every diagnostic this plugin produced for the fixture, from the client's point of view.
local function jev_diagnostics()
  local out = {}
  for _, d in ipairs(vim.diagnostic.get(buf)) do
    if d.source == 'jev' then
      out[#out + 1] = d
    end
  end
  return out
end

--- The finding on the `.unwrap()` line, or nil.
local function unwrap_finding()
  for _, d in ipairs(jev_diagnostics()) do
    if d.lnum == UNWRAP_LINE then
      return d
    end
  end
end

local function diagnostics_dump()
  local out = {}
  for _, d in ipairs(vim.diagnostic.get(buf)) do
    out[#out + 1] = { line = d.lnum, source = d.source, code = d.code, message = d.message }
  end
  return vim.inspect(out)
end

local function data_of(diagnostic)
  return ((diagnostic or {}).user_data or {}).lsp and diagnostic.user_data.lsp.data or {}
end

--- The rendered artifact buffer for a kind, e.g. `jev://usage/usage`.
local function artifact_text(kind)
  for _, b in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_valid(b) then
      local name = vim.api.nvim_buf_get_name(b)
      if name:find('jev://' .. kind, 1, true) ~= nil then
        return table.concat(vim.api.nvim_buf_get_lines(b, 0, -1, false), '\n')
      end
    end
  end
  return nil
end

--- Drop the artifact buffers of a kind, so the next dispatch's buffer is unambiguous.
local function close_artifacts(kind)
  for _, b in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_valid(b) and vim.api.nvim_buf_get_name(b):find('jev://' .. kind, 1, true) then
      vim.api.nvim_buf_delete(b, { force = true })
    end
  end
end

--- Dispatch `:Jev inspect [args]` and return the artifact text it opens (nil if none).
---
--- `:Jev inspect` reads the *current* buffer's path, and it shows the report in that same
--- window (`docs/UX.md` §2, `surfaces.layout`) — so the fixture has to be current before, and
--- made current again after, or the next check acts on the report
--- (`require('jev').dismiss()` was the one that noticed). Switching back also wipes the report
--- (`bufhidden = 'wipe'`), which is what `close_artifacts` is for when nothing switched.
local function inspect_command(args)
  close_artifacts('inspect')
  pcall(vim.api.nvim_set_current_buf, buf)
  local dispatched, dispatch_err = pcall(vim.cmd, 'Jev inspect' .. (args or ''))
  local text = nil
  vim.wait(15000, function()
    text = artifact_text('inspect')
    return text ~= nil
  end, 50)
  pcall(vim.api.nvim_set_current_buf, buf)
  return dispatched, dispatch_err, text
end

local finding, finding_data = nil, nil
if not endpoint_ok then
  skip('the rule finding reaches the sign column on save', endpoint_detail)
else
  vim.cmd('write')
  local arrived = vim.wait(30000, function()
    return unwrap_finding() ~= nil
  end, 50)
  finding = unwrap_finding()
  if not check(
    arrived and finding ~= nil,
    'a jev diagnostic arrives on the .unwrap() line within 30 s of saving',
    diagnostics_dump()
  ) then
    finding = nil
  end
end

if finding ~= nil then
  finding_data = data_of(finding)
  check(finding.source == 'jev', 'the diagnostic reports source = "jev"', tostring(finding.source))
  check(
    type(finding.code) == 'string' and finding.code == finding_data.finding_id,
    'the diagnostic is coded with the finding id its data carries',
    ('code=%s finding_id=%s'):format(tostring(finding.code), tostring(finding_data.finding_id))
  )
  local message = tostring(finding.message)
  check(
    message:find(RULE_TITLE, 1, true) ~= nil,
    'the message carries the rule title',
    message
  )
  check(
    message:find(RULE_TEXT, 1, true) ~= nil,
    "the message carries the rule's prose",
    message
  )
  check(
    #message > #(RULE_TITLE .. ' — ' .. RULE_TEXT),
    "the message carries the judgement that followed the rule's prose",
    message
  )
  check(
    type(finding_data.finding_id) == 'string' and finding_data.finding_id ~= '',
    'the finding carries a stable finding_id',
    vim.inspect(finding_data)
  )
  check(
    type(finding_data.verb) == 'string' and finding_data.verb ~= '',
    'the finding carries the verb the picker will offer',
    vim.inspect(finding_data)
  )
  check(
    type(finding_data.content_hash) == 'string' and finding_data.content_hash ~= '',
    'the finding carries the content hash it was keyed on',
    vim.inspect(finding_data)
  )
  check(
    finding_data.source == 'rules',
    'the finding is attributed to the rules pass through data.source',
    vim.inspect(finding_data.source)
  )
end

-- 3. `jev.inspect` through a real client -----------------------------------------------------

--- One request, bounded. `nil, err` means it did not answer in time.
local function request(client, method, params, timeout, bufnr)
  local answered, result, err = false, nil, nil
  client:request(method, params, function(e, r)
    err, result, answered = e, r, true
  end, bufnr)
  if not vim.wait(timeout or 15000, function()
    return answered
  end, 25) then
    return nil, { message = ('no response to %s within %d ms'):format(method, timeout or 15000) }
  end
  return result, err
end

local client = vim.lsp.get_clients({ bufnr = buf, name = 'jev' })[1]

if not endpoint_ok then
  skip('jev.inspect answers through the client with the counts', endpoint_detail)
elseif client == nil then
  skip('jev.inspect answers through the client with the counts', 'no client attached')
else
  local result, err = request(client, 'workspace/executeCommand', {
    command = 'jev.inspect',
    arguments = { { path = src, force = true } },
  }, 30000, buf)

  if err ~= nil then
    fail('jev.inspect answers through the client without a transport error', vim.inspect(err))
  elseif type(result) ~= 'table' or result.ok ~= true then
    fail(
      'jev.inspect answers ok through the client',
      vim.inspect(type(result) == 'table' and result.error or result)
    )
  else
    check(true, 'jev.inspect answers ok through the client')
    check(
      type(result.considered) == 'number' and type(result.candidates) == 'number',
      'jev.inspect returns the counts it considered and found, as numbers',
      ('considered=%s candidates=%s'):format(tostring(result.considered), tostring(result.candidates))
    )
    check(
      type(result.candidates) == 'number' and result.candidates >= 1,
      'the inspection named at least the one candidate in the fixture',
      tostring(result.candidates)
    )
    local listed = type(result.findings) == 'table' and next(result.findings) ~= nil
    check(listed, 'jev.inspect lists the finding it published', vim.inspect(result.findings))
    if listed then
      local same, labels = nil, {}
      for _, f in ipairs(result.findings) do
        labels[#labels + 1] = f.label
        if f.label == RULE_TITLE and f.line == UNWRAP_LINE then
          same = f
        end
      end
      check(
        same ~= nil,
        'the finding jev.inspect returns is the one the diagnostic showed',
        vim.inspect(labels)
      )
      if same ~= nil then
        check(
          finding_data ~= nil and same.id == finding_data.finding_id,
          'its id is the finding_id the diagnostic carries',
          ('inspect=%s diagnostic=%s'):format(tostring(same.id), tostring(finding_data and finding_data.finding_id))
        )
      end
    end
  end
end

-- 3b. The same command through the plugin's own surface (`:Jev inspect`) ----------------------
--
-- The LSP command above is the contract; this is the convenience over it. Both are asserted,
-- because a subcommand that renders the wrong thing would leave the protocol correct and the
-- user blind.

if not endpoint_ok then
  skip(':Jev inspect dispatches through the command surface', endpoint_detail)
  skip(':Jev inspect reports the same finding the diagnostic showed', endpoint_detail)
  skip(':Jev inspect reports the counts it considered and found', endpoint_detail)
else
  local dispatched, dispatch_err, text = inspect_command()
  check(dispatched, ':Jev inspect dispatches through the command surface', dispatch_err)
  check(
    type(text) == 'string' and text:find(RULE_TITLE, 1, true) ~= nil,
    ':Jev inspect reports the same finding the diagnostic showed',
    vim.inspect(text)
  )
  check(
    type(text) == 'string'
      and text:find('line ' .. (UNWRAP_LINE + 1), 1, true) ~= nil
      and text:find(RULE_TEXT, 1, true) ~= nil,
    ':Jev inspect reports the finding on the line the diagnostic was on, with its reason',
    vim.inspect(text)
  )
  check(
    type(text) == 'string'
      and text:find('considered', 1, true) ~= nil
      and text:find('candidate', 1, true) ~= nil,
    ':Jev inspect reports the counts it considered and found',
    vim.inspect(text)
  )
  check(
    type(text) == 'string' and text:find('skipped', 1, true) ~= nil,
    ':Jev inspect shows a skip section, so "nothing inspected" is never silent',
    vim.inspect(text)
  )
end

-- 4. Dismissal: `:Jev dismiss`, the record, and a fresh pull ----------------------------------

if not endpoint_ok or finding == nil then
  skip('the finding is dismissible and stays gone on a fresh pull', endpoint_detail or 'no finding arrived')
else
  vim.api.nvim_win_set_cursor(0, { UNWRAP_LINE + 1, 0 })
  require('jev').dismiss()
  vim.wait(500, function()
    return false
  end)
  check(#jev_diagnostics() == 0, 'the finding disappears once dismissed', diagnostics_dump())

  local dismissals = root .. '/.git/jev/dismissed.json'
  check(
    vim.fn.filereadable(dismissals) == 1,
    'the dismissal is recorded in the repository, not in memory: ' .. dismissals
  )
  if vim.fn.filereadable(dismissals) == 1 then
    local raw = table.concat(vim.fn.readfile(dismissals), '')
    local decoded_ok, doc = pcall(vim.json.decode, raw)
    check(decoded_ok and type(doc) == 'table', 'and it is valid JSON')
    if decoded_ok and type(doc) == 'table' and type(doc.dismissed) == 'table' and finding_data ~= nil then
      check(
        doc.dismissed[finding_data.finding_id] ~= nil,
        ('and it records this finding id: %s'):format(tostring(finding_data.finding_id))
      )
    end
  end

  -- The point of the record: a fresh pull must not bring it back.
  if client ~= nil then
    local pulled = request(client, 'textDocument/diagnostic', {
      textDocument = { uri = vim.uri_from_bufnr(buf) },
    }, 5000, buf)
    check(pulled ~= nil, 'the diagnostic request went out')
  end
  vim.wait(1500, function()
    return false
  end)
  check(
    #jev_diagnostics() == 0,
    'and the dismissed finding does not resurface on a fresh pull',
    diagnostics_dump()
  )
end

-- 5. `:Jev usage` counts it, attributed to the rules pass -------------------------------------

--- The analysis entries the session record holds, decoded.
local function session_analyses()
  local path = root .. '/.git/jev/session.jsonl'
  if vim.fn.filereadable(path) ~= 1 then
    return {}
  end
  local out = {}
  for _, line in ipairs(vim.fn.readfile(path)) do
    local ok, entry = pcall(vim.json.decode, line)
    if ok and type(entry) == 'table' and entry.kind == 'analysis' then
      out[#out + 1] = entry
    end
  end
  return out
end

if not endpoint_ok then
  skip(':Jev usage counts the rules finding', endpoint_detail)
elseif client == nil then
  skip(':Jev usage counts the rules finding', 'no client attached')
else
  local dispatched, dispatch_err = pcall(vim.cmd, 'Jev usage')
  check(dispatched, ':Jev usage dispatches through the command surface', dispatch_err)

  local result, err = request(client, 'workspace/executeCommand', { command = 'jev.usage', arguments = {} }, 15000, buf)
  if err ~= nil or type(result) ~= 'table' or result.ok ~= true then
    fail(
      ':Jev usage answers ok through the command surface',
      vim.inspect(err or (type(result) == 'table' and result.error or result))
    )
  else
    check(true, ':Jev usage answers ok through the command surface')
    check(
      type(result.published) == 'number' and result.published >= 1,
      'it counts the published finding',
      vim.inspect(result.published)
    )
    local analyses = session_analyses()
    local rules_entry = nil
    for _, entry in ipairs(analyses) do
      if entry.source == 'rules' then
        rules_entry = rules_entry or entry
      end
    end
    check(
      rules_entry ~= nil,
      'the record it counts from attributes the analysis to source = "rules"',
      vim.inspect(vim.tbl_map(function(e) return e.source end, analyses))
    )
    if rules_entry ~= nil then
      local labelled = false
      for _, f in ipairs(rules_entry.findings or {}) do
        if f.label == RULE_TITLE then
          labelled = true
        end
      end
      check(labelled, 'and that entry names the rule finding it counted', vim.inspect(rules_entry))
    end
  end

  -- The dispatch above is asynchronous: the buffer is opened by the request's own callback, not
  -- by `vim.cmd`. Wait for the buffer this check is about, rather than reading the moment the
  -- second request happens to return.
  local rendered = nil
  vim.wait(10000, function()
    rendered = artifact_text('usage')
    return rendered ~= nil
  end, 50)
  check(
    type(rendered) == 'string' and rendered:find('published', 1, true) ~= nil,
    ':Jev usage opens the artifact buffer with the counts in it',
    vim.inspect(rendered)
  )
end

-- 6. `:Jev inspect --force` on a document git calls unchanged ---------------------------------
--
-- "I edited a rule and nothing happened": a rule edit does not re-run a pass, and a document
-- git reports as unchanged is not inspected. `--force` is the escape hatch. Setting it up needs
-- a clean repository, so the fixture is committed here — cheap, and the only place the harness
-- depends on git.

if not endpoint_ok then
  skip(':Jev inspect --force re-runs a document git calls unchanged', endpoint_detail)
elseif vim.fn.executable('git') ~= 1 then
  skip(
    ':Jev inspect --force re-runs a document git calls unchanged',
    'git is not installed, so an "unchanged" document cannot be set up'
  )
else
  local function git(args)
    local cmd = { 'git', '-C', root }
    for _, a in ipairs(args) do
      cmd[#cmd + 1] = a
    end
    return vim.fn.system(cmd)
  end
  -- The previous check left its artifact buffer current; nothing below should act on it.
  pcall(vim.api.nvim_set_current_buf, buf)
  git({ 'init', '-q' })
  git({ 'config', 'user.email', 'jev@example.invalid' })
  git({ 'config', 'user.name', 'jev harness' })
  git({ 'add', '-A' })
  git({ '-c', 'commit.gpgsign=false', 'commit', '-q', '-m', 'fixture' })
  local dirty = vim.trim(git({ 'status', '--porcelain' }))

  if dirty ~= '' then
    skip(
      ':Jev inspect --force re-runs a document git calls unchanged',
      'the fixture repository is not clean: ' .. vim.inspect(dirty)
    )
  else
    -- Edit a rule, never the document. That changes the rules hash and so the cache key, which
    -- is the one way a *cache miss* and an *unchanged document* are both true — the state the
    -- escape hatch exists for ("I edited a rule and nothing happened"). The document is
    -- committed and untouched, so git reports only the rule file as changed.
    vim.fn.writefile(
      vim.split((RULE:gsub('A handler must not unwrap', 'A handler should not unwrap')), '\n', { plain = true }),
      rule_path
    )
    -- Outlive the server's 2 s changed-set cache (`state::CHANGED_TTL`), so the question is
    -- asked of the repository as it now is rather than of the answer it cached before the
    -- commit. No timeout is being relaxed; this is a cache TTL.
    vim.wait(2600, function()
      return false
    end)

    local _, _, plain = inspect_command()
    check(
      type(plain) == 'string' and plain:find('unchanged', 1, true) ~= nil,
      ':Jev inspect reports `unchanged` for a document git calls unchanged',
      vim.inspect(plain)
    )
    local _, _, forced = inspect_command(' --force')
    check(
      type(forced) == 'string' and forced:find(RULE_TITLE, 1, true) ~= nil,
      ':Jev inspect --force re-runs the pass on that unchanged document',
      vim.inspect(forced)
    )
  end
end

-- 7. `:checkhealth jev` names the decide tier -------------------------------------------------

if DECIDE_URL == '' then
  skip(':checkhealth jev names the decide endpoint', 'JEV_DECIDE_BASE_URL is unset, so there is no endpoint to name')
else
  pcall(vim.cmd, 'checkhealth jev')
  local report = nil
  vim.wait(2000, function()
    for _, b in ipairs(vim.api.nvim_list_bufs()) do
      if vim.api.nvim_buf_is_valid(b) and vim.api.nvim_buf_get_name(b):find('health', 1, true) then
        report = table.concat(vim.api.nvim_buf_get_lines(b, 0, -1, false), '\n')
      end
    end
    return report ~= nil and report:find('jev', 1, true) ~= nil
  end, 50)
  local origin = DECIDE_URL:match('^(https?://[^/]+)') or DECIDE_URL
  check(
    type(report) == 'string' and report:find(origin, 1, true) ~= nil,
    ':checkhealth jev names the decide endpoint it is configured against',
    vim.inspect(report)
  )
end

-- Report --------------------------------------------------------------------------------------

if failures > 0 then
  server_log.dump()
end
-- The root this harness created, gone on the way out — green or red.
fixture_root.remove(root, owned_root)
say(('[rules] %d failure(s), %d skip(s)'):format(failures, skips))
os.exit(failures == 0 and 0 or 1)
