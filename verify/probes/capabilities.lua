-- Evidence for docs/research/nvim-lsp-surface.md §11.
-- Prints the citied client capability subtrees verbatim, so the research document can be
-- checked against a real dump rather than a remembered one.

local caps = vim.lsp.protocol.make_client_capabilities()

local function dump(t, prefix, out)
  for k, v in pairs(t) do
    local key = prefix and (prefix .. '.' .. k) or k
    if type(v) == 'table' then
      if next(v) == nil then
        out[#out + 1] = key .. ' = {}'
      else
        dump(v, key, out)
      end
    else
      out[#out + 1] = key .. ' = ' .. tostring(v)
    end
  end
end

local picks = {
  { 'general.positionEncodings', caps.general.positionEncodings },
  { 'textDocument.codeAction', caps.textDocument.codeAction },
  { 'textDocument.diagnostic', caps.textDocument.diagnostic },
  { 'textDocument.inlineCompletion', caps.textDocument.inlineCompletion },
  { 'textDocument.synchronization', caps.textDocument.synchronization },
  { 'workspace.codeLens', caps.workspace.codeLens },
  { 'workspace.diagnostics', caps.workspace.diagnostics },
  { 'workspace.workspaceEdit', caps.workspace.workspaceEdit },
}

for _, p in ipairs(picks) do
  local out = {}
  dump(p[2], nil, out)
  table.sort(out)
  print('[' .. p[1] .. ']')
  for _, line in ipairs(out) do
    print('  ' .. line)
  end
end
