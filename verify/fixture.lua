-- Shared by the Lua harnesses: a private fixture root, and giving it back.
--
--   local fixture = dofile(vim.fn.getcwd() .. '/verify/fixture.lua')
--   local root, owned = fixture.root('JEV_ROOT', '-jev-ui')
--   … the harness runs …
--   fixture.remove(root, owned)
--
-- Every harness owns a private fixture root and marks it as its own (stated in
-- `docs/VERIFICATION.md`'s preamble). That half is about *where a root looks*: the plugin resolves
-- a buffer's workspace with `vim.fs.root(bufnr, {'.git'})` and the server keys its session record
-- on `<root>/.git/jev/`, so a fixture that is not marked can make somebody else's `.git` the
-- workspace — `/tmp/.git`, created by two Python rows that declared `/tmp` their workspace, did
-- exactly that to `nvim_ui_test.lua`, which then read no rules from `/tmp/.jev/rules`, cached no
-- findings, and failed six checks that need an ambient finding, none of them naming the cause.
--
-- This is the other half, pointed at `/tmp` itself: a root created here is *this harness's* to
-- remove, and a root the caller named is the caller's to keep — which is the whole reason a
-- failing row is debuggable, so naming one is not an oversight to be tidied away. Nothing here
-- deletes a path it did not create, and nothing deletes a parent of one.

local M = {}

--- A fixture root: the caller's when the environment names one, else a fresh temporary directory.
--- @param env string  the variable a caller sets, e.g. `'JEV_ROOT'`
--- @param suffix string  what a fresh one is called, e.g. `'-jev-ui'` (the temp dir is shared, so
---   the suffix is what tells one harness's root from another's in `/tmp`)
--- @return string root
--- @return boolean owned  true when this harness created it, and must therefore remove it
function M.root(env, suffix)
  local named = os.getenv(env)
  if named ~= nil and named ~= '' then
    vim.fn.mkdir(named, 'p')
    return named, false
  end
  local root = vim.fn.tempname() .. (suffix or '')
  vim.fn.mkdir(root, 'p')
  return root, true
end

--- Remove a root this harness created, on every exit path — green, red, or skipped.
---
--- The exact directory `M.root` returned, recursively, and nothing else: no parent, no pattern, no
--- second guess about what is inside it. A caller's root is left where it is, and that is the
--- point of passing `owned` around rather than re-deriving it here.
--- @param root string
--- @param owned boolean
function M.remove(root, owned)
  if not owned or type(root) ~= 'string' or root == '' then
    return
  end
  vim.fn.delete(root, 'rf')
end

return M
