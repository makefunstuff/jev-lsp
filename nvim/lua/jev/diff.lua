--- The proposal diff: the buffer on the left, the post-edit text on the right, `<CR>`/`y`
--- approves, `q`/`<Esc>` rejects and leaves everything exactly as it was.
---
--- `docs/UX.md` §3.3 and §6. Two rules shape the whole module:
---
--- 1. **Nothing touches the buffer until `<CR>`.** The post-edit text is computed by applying
---    the edit's `TextEdit`s to a scratch copy, with the client's own range arithmetic
---    (`vim.lsp.util.apply_text_edits`; PROTOCOL N1 — byte offsets, `utf-8`), so the preview
---    cannot disagree with what approval will do.
--- 2. **Rejection is a no-op.** One split is opened and closed, the options `:diffthis` sets
---    are snapshotted and restored, window sizes are put back, the maps this module added are
---    removed, the scratch buffer is wiped, and no file is written.
---
--- The diff itself is Vim's, not a rendering of `vim.diff`'s output: both windows get
--- `:diffthis`, so the user's own `'diffopt'` (filler, algorithm, context, whitespace rules)
--- decides what is shown. `vim.diff` is used only to find the first changed line, so a
--- proposal deep inside a long file opens on the change rather than at line 1. No global
--- option is written — `'diff'` and its neighbours are window-local.
---
--- @module 'jev.diff'

local M = {}

--- Window-local options `:diffthis` changes and `:diffoff!` does not put back exactly.
--- Snapshotting these is what makes "`q` leaves windows exactly as they were" true rather
--- than approximately true.
local WINDOW_OPTIONS = {
  'diff',
  'scrollbind',
  'cursorbind',
  'foldmethod',
  'foldenable',
  'foldcolumn',
  'wrap',
}

--- The preview that is open, if any: opening a second one rejects the first rather than
--- leaving two sets of conflicting maps behind.
local current = nil

--- @class jev.DiffOpts
--- @field bufnr? integer     Buffer to diff against; default: the edit's first open target
--- @field encoding? string   Position encoding, default `'utf-8'` (PROTOCOL N1)
--- @field on_approve? fun(edit: table)  Called after the edit has been applied
--- @field on_reject? fun()   Called after the diff is closed with nothing applied

--- @param win integer
--- @return table<string, any>
local function snapshot(win)
  local saved = {}
  for _, name in ipairs(WINDOW_OPTIONS) do
    saved[name] = vim.wo[win][name]
  end
  return saved
end

--- @param win integer
--- @param saved table<string, any>
local function restore(win, saved)
  for name, value in pairs(saved) do
    pcall(function()
      vim.wo[win][name] = value
    end)
  end
end

--- Every ordinary window of this tabpage with its size, so the split this module opens can be
--- undone to the pixel and not merely to the same window count.
--- @return { win: integer, width: integer, height: integer }[]
local function sizes()
  local out = {}
  for _, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
    if vim.api.nvim_win_get_config(win).relative == '' then
      out[#out + 1] = {
        win = win,
        width = vim.api.nvim_win_get_width(win),
        height = vim.api.nvim_win_get_height(win),
      }
    end
  end
  return out
end

--- Put the sizes back. Sizes in a tabpage sum to a fixed total, so pinning every surviving
--- window pins the layout.
--- @param saved { win: integer, width: integer, height: integer }[]
local function restore_sizes(saved)
  for _, s in ipairs(saved) do
    if vim.api.nvim_win_is_valid(s.win) then
      pcall(vim.api.nvim_win_set_height, s.win, s.height)
      pcall(vim.api.nvim_win_set_width, s.win, s.width)
    end
  end
end

--- The document of `edit` to preview, with the edits that touch it.
---
--- A `WorkspaceEdit` may touch several documents (PROTOCOL §8: `test` creates a file *and*
--- edits the source). The preview shows one side-by-side pair, so it takes the first target
--- that is open and loaded, `opts.bufnr` first when it is one of them. Resource operations
--- carry a `uri` and no text document, and `vim.uri_to_bufnr` conjures an empty buffer for a
--- file that does not exist yet — which is why the loaded check is not optional.
---
--- @param edit table  `lsp.WorkspaceEdit`
--- @param bufnr integer?
--- @return { bufnr: integer, uri: string, edits: table[] }?
local function target_of(edit, bufnr)
  local chosen
  for _, change in ipairs(edit.documentChanges or {}) do
    local doc = change.textDocument
    if doc ~= nil and type(doc.uri) == 'string' then
      local candidate = vim.uri_to_bufnr(doc.uri)
      if vim.api.nvim_buf_is_loaded(candidate) and vim.api.nvim_buf_is_valid(candidate) then
        local found = { bufnr = candidate, uri = doc.uri, edits = change.edits or {} }
        if bufnr ~= nil and candidate == bufnr then
          return found
        end
        chosen = chosen or found
      end
    end
  end
  return chosen
end

--- A window showing `bufnr`, preferring the current one.
--- @param bufnr integer
--- @return integer?
local function window_showing(bufnr)
  local here = vim.api.nvim_get_current_win()
  if vim.api.nvim_win_get_buf(here) == bufnr then
    return here
  end
  for _, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
    if vim.api.nvim_win_get_buf(win) == bufnr then
      return win
    end
  end
  return nil
end

--- The first hunk of the two texts, as `{ line_a, line_b }` (1-based), or nil when there is
--- nothing to show. Only used to aim the view.
--- @param before string[]
--- @param after string[]
--- @return integer[]?
local function first_hunk(before, after)
  local ok, hunks = pcall(
    vim.diff,
    table.concat(before, '\n') .. '\n',
    table.concat(after, '\n') .. '\n',
    { result_type = 'indices' }
  )
  local first = ok and type(hunks) == 'table' and hunks[1] or nil
  if first == nil then
    return nil
  end
  return { first[1], first[3] }
end

--- @param win integer
--- @param line integer
local function aim(win, line)
  if not vim.api.nvim_win_is_valid(win) then
    return
  end
  local last = vim.api.nvim_buf_line_count(vim.api.nvim_win_get_buf(win))
  pcall(vim.api.nvim_win_set_cursor, win, { math.max(1, math.min(line, last)), 0 })
  pcall(vim.api.nvim_win_call, win, function()
    vim.cmd('normal! zz')
  end)
end

-- The preview ---------------------------------------------------------------------------------

--- Open `edit` as a side-by-side diff and wait for a decision.
---
--- @param edit table  `lsp.WorkspaceEdit` (PROTOCOL §8: `documentChanges`, explicit versions)
--- @param opts? jev.DiffOpts
--- @return boolean  whether a preview was opened
function M.propose(edit, opts)
  opts = opts or {}
  if type(edit) ~= 'table' then
    return false
  end
  local encoding = opts.encoding or 'utf-8'
  local target = target_of(edit, opts.bufnr)
  if target == nil then
    vim.notify('jev: nothing to preview — the edit touches no open buffer', vim.log.levels.WARN)
    return false
  end
  local bufnr = target.bufnr
  local orig_win = window_showing(bufnr)
  if orig_win == nil then
    vim.notify(
      ('jev: %s is in no window; open it to review the proposal')
        :format(vim.api.nvim_buf_get_name(bufnr)),
      vim.log.levels.WARN
    )
    return false
  end

  -- The post-edit text, on a copy. The buffer is read here and never written.
  local before = vim.api.nvim_buf_get_lines(bufnr, 0, -1, true)
  local scratch = vim.api.nvim_create_buf(false, true)
  vim.api.nvim_buf_set_lines(scratch, 0, -1, false, before)
  local applied, err = pcall(vim.lsp.util.apply_text_edits, target.edits, scratch, encoding)
  if not applied then
    vim.api.nvim_buf_delete(scratch, { force = true })
    vim.notify(('jev: the proposal does not apply cleanly: %s'):format(err), vim.log.levels.ERROR)
    return false
  end
  local after = vim.api.nvim_buf_get_lines(scratch, 0, -1, true)
  if vim.deep_equal(before, after) then
    vim.api.nvim_buf_delete(scratch, { force = true })
    vim.notify('jev: the proposal does not change this buffer', vim.log.levels.INFO)
    return false
  end

  vim.bo[scratch].buftype = 'nofile'
  vim.bo[scratch].bufhidden = 'wipe'
  vim.bo[scratch].swapfile = false
  vim.bo[scratch].filetype = vim.bo[bufnr].filetype
  vim.bo[scratch].modifiable = false
  pcall(vim.api.nvim_buf_set_name, scratch, ('jev://proposal/%s'):format(target.uri))

  if current ~= nil then
    current.reject() -- one preview at a time: the previous one's maps die with it
  end

  local saved = snapshot(orig_win)
  local layout = sizes()

  -- `:rightbelow vsplit` puts the proposal on the right whatever `'splitright'` says, so the
  -- pair always reads left to right: the buffer, then the proposal.
  vim.api.nvim_set_current_win(orig_win)
  vim.cmd('rightbelow vsplit')
  local split_win = vim.api.nvim_get_current_win()
  vim.api.nvim_win_set_buf(split_win, scratch)
  vim.cmd('diffthis')
  vim.api.nvim_set_current_win(orig_win)
  vim.cmd('diffthis')

  local hunk = first_hunk(before, after)
  if hunk ~= nil then
    aim(orig_win, hunk[1])
    aim(split_win, hunk[2])
  end

  -- Map only where the key is free: a buffer-local map of the user's wins outright, a global
  -- one is shadowed for the duration and visible again once this closes. The map is looked up
  -- in the buffer it is about to be set in, not in whichever buffer happens to be current.
  local ours = {}
  local function map(win, mode, lhs, fn, desc)
    local buffer = vim.api.nvim_win_get_buf(win)
    for _, m in ipairs(vim.api.nvim_buf_get_keymap(buffer, mode)) do
      if m.lhs == lhs and m.buffer == 1 then
        return
      end
    end
    vim.keymap.set(mode, lhs, fn, { buffer = buffer, desc = desc })
    ours[#ours + 1] = { bufnr = buffer, mode = mode, lhs = lhs }
  end
  local function both(mode, lhs, fn, desc)
    map(orig_win, mode, lhs, fn, desc)
    map(split_win, mode, lhs, fn, desc)
  end

  local function close()
    for _, m in ipairs(ours) do
      pcall(vim.api.nvim_buf_del_keymap, m.bufnr, m.mode, m.lhs)
    end
    for _, win in ipairs({ orig_win, split_win }) do
      if vim.api.nvim_win_is_valid(win) then
        pcall(vim.api.nvim_win_call, win, function()
          pcall(vim.cmd, 'diffoff!')
        end)
      end
    end
    if vim.api.nvim_win_is_valid(split_win) then
      pcall(vim.api.nvim_win_close, split_win, true)
    end
    if vim.api.nvim_buf_is_valid(scratch) then
      pcall(vim.api.nvim_buf_delete, scratch, { force = true })
    end
    if vim.api.nvim_win_is_valid(orig_win) then
      pcall(vim.api.nvim_set_current_win, orig_win)
      restore(orig_win, saved)
    end
    restore_sizes(layout)
    current = nil
  end

  local function approve()
    close()
    local ok, result = pcall(vim.lsp.util.apply_workspace_edit, edit, encoding)
    if not ok then
      vim.notify(('jev: the edit was not applied: %s'):format(result), vim.log.levels.ERROR)
      return
    end
    if result == false then
      -- The client refused it — a version that moved under the proposal — and said so itself
      -- (`vim/lsp/util.lua`), so this adds no message of its own.
      return
    end
    if opts.on_approve ~= nil then
      opts.on_approve(edit)
    end
  end

  local function reject()
    close()
    if opts.on_reject ~= nil then
      opts.on_reject()
    end
  end

  both('n', '<CR>', approve, 'jev: apply this proposal')
  both('n', 'y', approve, 'jev: apply this proposal')
  both('n', 'q', reject, 'jev: reject this proposal')
  -- `<Esc>` on the proposal side only: mapping it in the buffer the user is editing would turn
  -- their reflex into a rejected proposal.
  map(split_win, 'n', '<Esc>', reject, 'jev: reject this proposal')

  current = { reject = reject }
  vim.api.nvim_set_current_win(split_win)
  return true
end

return M
