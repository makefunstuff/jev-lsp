-- Evidence for docs/research/nvim-lsp-surface.md §1 and §12.
-- Dumps the server->client surface this client actually implements, then asserts the
-- client capability values the design depends on. Exits nonzero on any mismatch.

local runtime = vim.env.VIMRUNTIME
assert(runtime and runtime ~= '', 'VIMRUNTIME is unset; run under a real nvim')

local handlers = runtime .. '/lua/vim/lsp/handlers.lua'
local fh = assert(io.open(handlers, 'r'), 'cannot read ' .. handlers)
local src = fh:read('a')
fh:close()

local function keys(prefix)
  local out = {}
  for k in src:gmatch(prefix .. "%['([^']+)'%]") do
    out[#out + 1] = k
  end
  table.sort(out)
  return out
end

local rsc = keys('RSC')
local nsc = keys('NSC')
print(('[surface] server->client requests: %d'):format(#rsc))
print('  ' .. table.concat(rsc, '\n  '))
print(('[surface] server->client notifications: %d'):format(#nsc))
print('  ' .. table.concat(nsc, '\n  '))

-- Capability assertions: the design's non-negotiables depend on these exact values.
local caps = vim.lsp.protocol.make_client_capabilities()
local td, ws = caps.textDocument, caps.workspace

local checks = {
  { 'positionEncodings includes utf-8', vim.tbl_contains(caps.general.positionEncodings, 'utf-8') },
  { 'codeAction.dataSupport', td.codeAction.dataSupport == true },
  { 'codeAction.disabledSupport', td.codeAction.disabledSupport == true },
  { 'codeAction.isPreferredSupport', td.codeAction.isPreferredSupport == true },
  { 'codeAction.resolveSupport.properties has edit',
    vim.tbl_contains(td.codeAction.resolveSupport.properties, 'edit') },
  { 'inlineCompletion advertised (table, not true)',
    type(td.inlineCompletion) == 'table' and td.inlineCompletion.dynamicRegistration == false },
  { 'diagnostic.dynamicRegistration', td.diagnostic.dynamicRegistration == true },
  { 'diagnostic.dataSupport', td.diagnostic.dataSupport == true },
  { 'workspace.diagnostics.refreshSupport', ws.diagnostics.refreshSupport == true },
  { 'workspace.codeLens.refreshSupport', ws.codeLens.refreshSupport == true },
  { 'workspace.applyEdit', ws.applyEdit == true },
  { 'workspaceEdit resourceOperations covers create+delete',
    vim.tbl_contains(ws.workspaceEdit.resourceOperations, 'create')
      and vim.tbl_contains(ws.workspaceEdit.resourceOperations, 'delete') },
}

local bad = 0
print('[capabilities]')
for _, c in ipairs(checks) do
  print(('  %-52s %s'):format(c[1], c[2] and 'ok' or 'MISMATCH'))
  if not c[2] then bad = bad + 1 end
end

print(('[capabilities] %d assertion(s) failed'):format(bad))
os.exit(bad == 0 and 0 or 1)
