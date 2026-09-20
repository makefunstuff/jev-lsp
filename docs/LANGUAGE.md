# Language

**Support is unconditional. Language is metadata.**

The model needs no grammar, no compiler, and no filetype to read text. So nothing about
language may gate attachment, document sync, the verb set, or whether an action is offered.
A Markdown file, a log, a config, a `Makefile`, an extensionless script, and a file Neovim
cannot identify are all first-class documents.

Language is still resolved, because it improves output: it selects a prompt flavour, a
scope strategy, and sometimes a model. It is a hint, never a filter.

Measured basis: `verify/probes/language.lua` and `verify/probes/README.md`.

---

## 1. The attachment ladder

The requirement is "every file", and the client does not deliver that on its own.

`vim.lsp.enable` attaches **only on the `FileType` autocmd** (`vim/lsp.lua`, the
`nvim.lsp.enable` augroup). `FileType` does not fire for a buffer whose filetype was never
set. So with `filetypes = nil` — documented as "ALL filetypes" (`vim/lsp.lua:174`) — the
server still only receives the buffers Neovim could identify.

Measured, 11 fixtures: **`FileType` fired for 8, and 8 were auto-attached.** `f.zzz`,
`data.log`, and `plain` were never attached. After the plugin's own attach pass: **11 of 11.**

| Step | Owner | Action |
|---|---|---|
| 1 | client | Built-in path. Covers every file Neovim identifies. Free, do nothing. |
| 2 | plugin | On `BufReadPost`, `BufNewFile`, `BufWinEnter` (idempotent): if the buffer is a normal file buffer and no `jev` client is attached, `vim.lsp.start(cfg, { bufnr = bufnr })`. `vim.lsp.start` with an explicit buffer bypasses the `filetypes` filter. |
| 3 | plugin | Never attach twice; never attach to a buffer §6 excludes. |

Step 2 is ~15 lines of Lua and is what makes "universal" true. It is not optional.

**And nothing here is required of a client.** The server's contract is standard LSP: findings
arrive by pull diagnostics plus `workspace/diagnostic/refresh`; actions by `codeAction` and
`codeAction/resolve`; the material a request carries by `workspace/executeCommand` and
`workspace/configuration`; progress by `$/progress` under a token the client itself issued; free
text by the client, because the protocol cannot ask for it (N7). Everything that is *not* a
standard surface — the universal attach pass, the `vim.lsp.codelens.run` interception, the
picker, every scratch buffer — lives in `nvim/` and is **optional convenience, never required**:
any LSP client gets the findings, the edits and the commands without it. What is not optional is
narrower: in Neovim, step 2 is what makes "every file" true, because the built-in path cannot see
a buffer whose filetype was never set.

That claim is checkable because the server is exercised by three clients, two of which share no
code with it: Neovim, through this plugin; `verify/lsp_client.py`,
written from the specification against the standard library alone (`docs/VERIFICATION.md` §1); and
OMP, through its own LSP support (`verify/omp_lsp.sh`, §1.1) — a client nobody here wrote, which
receives a rule's finding over `textDocument/diagnostic` and calls `jev.inspect`. Neovim is the
one it is verified against most deeply, and the only one where the plugin is needed at all.

## 2. Language resolution

Best-effort, pure, and cheap. Precedence, first non-empty wins:

| # | Source | Cost | Wins over |
|---|---|---|---|
| 1 | `languageId` from `didOpen` | free — already on the wire | everything |
| 2 | path extension | table lookup | 3, 4, 5 |
| 3 | filename pattern (`Makefile`, `Dockerfile`, `Cargo.toml`) | table lookup | 4, 5 |
| 4 | shebang line | one regex on line 1 | 5 |
| 5 | content sniff (first 200 lines, anchored patterns) | regex | — |
| 6 | `unknown` | — | never fails |

**The client answers first, and it is allowed to be smarter than `filetype`.** Neovim
sends `vim.bo[bufnr].filetype` as `languageId` by default (`lsp/client.lua:1149` →
`_get_language_id` → `default_get_language_id` at `lsp/client.lua:323`), and
`get_language_id` is a client config hook (`lsp/client.lua:412`). The plugin uses it:

```lua
get_language_id = function(bufnr, ft)
  if ft ~= nil and ft ~= '' then
    return ft                       -- fast path: no work when the client already knows
  end
  return vim.filetype.match({       -- pure: filename+contents, NOT `buf`
    filename = vim.api.nvim_buf_get_name(bufnr),
    contents = vim.api.nvim_buf_get_lines(bufnr, 0, 200, false),
  }) or ''
end
```

Two rules, both enforced by the probe:

- **Pure.** Passing `buf` instead of `filename`+`contents` makes `vim.filetype.match`
  *set* the buffer's filetype — a surprising mutation from a language-id callback that can
  fight the user's own filetype configuration. The pure form does not mutate
  (asserted: `the language hook did not mutate buffer state`).
- **Free in the common case.** When `filetype` is non-empty, no detector call happens at all.

Measured result of the hook: a buffer the built-in path never identified still reports
`python`, recovered from its shebang contents. That is the case the attach pass creates and
this hook closes.

The server implements the full ladder independently anyway — it must serve clients that do
not send a useful `languageId`, and `''` is a normal value, not an error.

## 3. What language actually changes

Two things. If a language is unknown, both fall back and nothing is refused.

| Aspect | Known language | Unknown |
|---|---|---|
| **Scope** (§4) | treesitter node kinds for that language | structural fallback → whole file |
| **Prompt flavour** | per-language template (idioms, stdlib, error conventions) | `generic_text` template |

The model tier is not one of them: it comes from the verb (`PROTOCOL.md` §4.1), the same tier in
every language.

In the action `data` and in artifacts, `language` is carried as metadata
(`PROTOCOL.md` §4, §7) so a user can see what the model was told, and so a bad result can
be attributed to a bad classification.

### 4.1 Where an extent comes from now

The chain is unchanged on the server: structural, parser-free, always produces something,
reports its provenance as `scope_source` (N12 and `PROTOCOL.md` §6 name the same idea for
partial results). What changed is that a *client* with a parser can answer first. The plugin
sends an explicit `range` for `explain` and `plan` when treesitter can name the enclosing
declaration, and the server anchors on it and reports `scope_source = "explicit"`. Without a
parser, without the language in the plugin's table, or without such a declaration, the range is
absent and the server decides — which is the same path the CLI takes, always.

So a client-resolved scope is visible, not hidden: the answer says which side resolved it. The
listing in `nvim/lua/jev/init.lua` (`TS_SCOPE_NODES`) is deliberately short, and a language
missing from it is not a failure.

The same division now covers *definitions* — the list of declarations a lens or a hint hangs
on. The server's structural scan declares a function by keyword, which works for Python or Lua
and finds **nothing** in C, C++, Java or C#, where a function is declared by shape. The plugin
walks the tree's top level with its parser and sends what it found, version-stamped
(`PROTOCOL.md` §3.4.3); the server uses that set while it describes the version being edited,
and its own scan otherwise. The set replaces the scan rather than merging with it, so the node
table is a superset of what the scan covers — otherwise a lens would disappear instead of
improve.

## 4. Scope resolution — never unavailable

| Strategy | When | Result |
|---|---|---|
| **Tree** | a treesitter parser is installed for the language | enclosing node of an interesting kind (function/method/class/impl/mod/…), per-language node-kind table |
| **Structural** | no parser | blank-line- and indentation-delimited block around the cursor |
| **Whole file** | no parser and no structure, or the file is under ~120 lines | the entire document |
| **Explicit** | the user selected a range, or named a range in a command | that range, verbatim |

The LLM requires no AST, so the absence of a parser changes *quality of scope*, never
availability of the feature. This is the structural difference from a real language server,
and the reason a universal server is coherent.

## 5. Practical gates — the only things that limit support

Support is unconditional in principle; five conditions are about practicality. **Every skip
is stated, never silent** — the lesson taken from Copilot's `Inactive` status
(`docs/research/prior-art.md` §5).

| Gate | Rule | State reported |
|---|---|---|
| `buftype` | only normal file buffers (`buftype == ''`, non-empty name). Terminals, help, quickfix, prompt, and nofile scratch are not files. | not attached |
| size | > `max_file_bytes` (default 1 MiB): not analysed ambiently; explicit requests use a window around the cursor or the selected range | `over_size` |
| binary | a NUL byte in the first 8 KiB: attached, no analysis, no verbs offered | `binary` |
| grammar | no parser for the language | `generic_scope` — a quality flag, not a refusal |
| ignore | matches one of the `languages.ignore` globs (default `**/node_modules/**`, `**/*.min.js`, `**/vendor/**`); the matched pattern is reported | `ignored` |

Only the first is a hard exclusion, and it excludes non-files rather than files. A 4 GiB
log is still *attached and synced*; it is not sent to a model whole. The plugin
surfaces `over_size`, `binary`, `ignored`, and `generic_scope` in `:Jev status` and in the
statusline segment, so a user can always tell why a buffer is quiet.

## 6. Latency

| Step | Budget | Mechanism |
|---|---|---|
| attach decision (plugin) | < 1 ms | one `buftype`/name check |
| `languageId` at `didOpen` (plugin) | < 1 ms | returns `filetype` unless empty |
| server resolution ladder | p99 < 50 µs | table lookups; content sniff reads ≤ 200 lines, once, and never hits disk (the buffer is already loaded) |
| resolution cache | hit = ~0 | keyed by `(uri, content_hash)`; recomputed only when content changes |
| scope resolution | p99 < 1 ms | treesitter query, else the structural scan |

Language resolution is never on a path the user waits on, and it never calls a model. It is
also never on the *startup* path: `initialize` performs no filesystem scan and no
classification.

## 7. Configuration

```jsonc
"languages": {
  "overrides": {                      // per resolved language, not per filetype
    "rust":  { "verbs": ["harden", "types", "test"] },
    "markdown": { "verbs": ["review"] }
  },
  "max_file_bytes": 1048576,          // above this a buffer is skipped, and says so
  "max_scope_lines": 400,             // a declaration longer than this gets no lens
  "ignore": ["**/node_modules/**", "**/*.min.js", "**/vendor/**"]
}
```

There is no per-filetype or "generic" key: an override is keyed by the language the buffer
*resolves* to (`unknown` included). An override may narrow the verb set for a language (Markdown
has no meaningful "add types"); absence of an override means the full set. `tier` and `prompt`
are in the schema and are not read (`PROTOCOL.md` §10 lists them with the other
declared-but-unread keys). The tier follows the verb, and the persona comes from the language's
profile (`jev-core/src/lang.rs`).

Nothing here can *disable* a language — only change how it is served. Disabling is
`enabled = false` at the top level, which stops the whole server.

## 8. Refused

- **A filetype allowlist as the attachment mechanism.** Contradicts the requirement, and
  every prior-art project does it (`docs/research/prior-art.md` §4).
- **Gating support on a treesitter parser.** The model does not need one.
- **Gating support on a resolution result.** `unknown` is a valid language, not a state to
  be excluded from.
- **Silent skips.** Every buffer that is attached but not analysed reports why.
- **Mutating buffer state from the language hook.** Asserted by the probe.
- **Adding non-standard fields to a standard method's params.** N6: staleness is handled
  server-side by content hash and position, so a method never needs a field the spec does not
  define.
