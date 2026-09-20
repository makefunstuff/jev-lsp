# jev for Cursor

The whole of the route Cursor requires: an extension that starts `jev-lsp --stdio` and wires the
standard surfaces into the editor. Cursor has no setting for "point me at a stdio language
server" — `docs/CURSOR.md` §1 states that from Cursor's own documentation — so this file exists
rather than a `settings.json` block.

No build step, no `npm install`, no `vscode-languageclient`. One `extension.js` a reader can
inspect, against the VS Code API and `child_process` alone: a language-client library would add
a dependency, a bundler and ~10 MB of `node_modules` to send `Content-Length` frames over a pipe
that the server already speaks (PROTOCOL.md §2), and the harness's own Python client is the
proof that the wire is nothing more than that.

## What it registers

Exactly what the server advertises, and no more (PROTOCOL.md §2):

| VS Code surface | Wire method |
|---|---|
| `DiagnosticCollection("jev")` + a gutter mark | pull `textDocument/diagnostic`, refresh on `workspace/diagnostic/refresh` |
| `registerCodeActionsProvider` (with `resolveCodeAction`) | `textDocument/codeAction`, `codeAction/resolve` |
| `registerCodeLensProvider` | `textDocument/codeLens` |
| `registerHoverProvider` | `textDocument/hover` |
| `registerInlayHintsProvider` (off by default) | `textDocument/inlayHint` |
| 11 palette commands | `workspace/executeCommand` |
| `WorkspaceEdit` applier | server → client `workspace/applyEdit` |
| status bar | `$/progress` |

`inlineCompletionProvider` is not advertised by the server, so nothing here renders one.
Three `jev.plugin.*` command ids are also registered client-side; `docs/CURSOR.md` §5 says why
they cannot be left to the server.

## Install

From a clone, without packaging:

```sh
ln -s "$PWD/editors/cursor" ~/.cursor/extensions/makefunstuff.jev-0.1.0
```

Restart Cursor. (Cursor reads `~/.cursor/extensions/` the way VS Code reads
`~/.vscode/extensions/`.)

As a `.vsix`, with no npm and no network — **verified on this machine**:

```sh
bash editors/cursor/pack.sh /tmp/jev-0.1.0.vsix
/Applications/Cursor.app/Contents/Resources/app/bin/cursor --install-extension /tmp/jev-0.1.0.vsix
/Applications/Cursor.app/Contents/Resources/app/bin/cursor --list-extensions | grep jev
# Extension 'jev-0.1.0.vsix' was successfully installed.
# makefunstuff.jev
```

`cursor` is not usually on `PATH` on macOS; the CLI is inside the app bundle at
`Contents/Resources/app/bin/cursor`. `pack.sh` builds the ZIP a VSIX is —
`[Content_Types].xml`, `extension.vsixmanifest`, the extension — out of `zip`, `jq` and the
`package.json` it is next to.

## Configure

`jev.server.path` should be an absolute path: a macOS GUI application does not inherit the login
shell's `PATH`. The rest become the `JEV_*` variables in the server's environment (PROTOCOL §10);
a setting that is empty leaves the ambient variable alone.

```jsonc
{
  "jev.server.path": "/path/to/jev-lsp/target/release/jev-lsp",
  "jev.server.args": ["--stdio"],

  "jev.decide.baseUrl": "https://api.typesafe.ai/v1",
  "jev.decide.model": "jev-latest",
  "jev.decide.wire": "system_one",            // system_one | open_router
  "jev.decide.apiKeyEnv": "TYPESAFE_API_KEY", // the NAME of the variable, never the key
  "jev.decide.apiKeyFile": "~/.bash_profile", // where to read the key's VALUE from
  "jev.decide.timeoutMs": 15000,

  "jev.chat.baseUrl": "",                     // actions, plans, explanations
  "jev.chat.model": "",

  "jev.settings": {}                          // the server's own `jev` section, verbatim
}
```

`apiKeyFile` accepts two forms: the file *is* the key, or the file is a shell file and the key is
the value of its last `NAME=value` for the name in `apiKeyEnv`. It exists because a GUI app
cannot see `export TYPESAFE_API_KEY=…` in your profile, and because a key that is pasted into a
setting is a key that gets committed. The value is passed to the child process and never logged.

## Where an answer appears

`jev.artifacts.viewColumn` decides, and it defaults to **`active`**: an explanation, a plan or a
review opens in the editor group you are already in, so the layout never changes.

| value | what happens |
|---|---|
| `active` | a document in the current editor group — the default, and no rearrangement |
| `beside` | a new group to the right; with one group open, that is a split |
| `output` | the body goes to *Output → Jev*; no document is opened |

`ViewColumn.Beside` was the first version's default, and with a single group open it splits the
window — an answer to a question rearranging the editor, which is what the default exists to
avoid.

Four commands never open a document whatever the setting says, because their whole answer is a
value rather than prose: `jev.inspect` (counts and skip codes), `jev.status` (a numbers
snapshot), `jev.recompute` and `jev.revert`. They write to *Output → Jev* and put one line in a
message with a **Details** action that reveals the channel. A diagnostic command that replaced
what you were looking at with six lines you then had to close was the original complaint.

## Limits, all of them

- **The protocol layer is verified without a GUI; the rendering is not.** `node --check` passes,
  `package.json` parses, the install is verified, and the bridge is driven end to end against
  the real binary with a stand-in `vscode` (`docs/CURSOR.md` §7). Whether Cursor *paints* a
  squiggle, a lens or a gutter dot is not covered by anything that runs in CI here.
- **`window/showDocument` is answered `{success: false}`.** The server asks for
  `jev://artifact/<id>` and does not send the markdown, so a client with no `jev://` resolver
  cannot render it. Artifacts are reachable whole through `jev.explain`, `jev.ask` and
  `jev.plan`, which this extension opens as a Markdown document.
- **One server process per window, rooted at the first workspace folder.** A multi-root
  workspace gets the first folder's `.jev/rules/` and git root for every folder in it.
- **`jev.document` is never sent.** The extension host has no parser, and PROTOCOL §3.4.3 says
  silence means the server's structural scan answers instead.
- **`jev.apply`, `jev.cancel` and `jev.outcome` are not called.** A plan is shown as a document
  and not as an interactive step buffer; a request is cancelled with `$/cancelRequest` rather
  than `jev.cancel`; and no VS Code API tells an extension that a resolved code action's edit was
  applied, so `jev.usage` under-counts for Cursor rather than guess.
- **Streaming is reported, not rendered.** A `$/progress` message goes to the status bar; the
  partial markdown in `data` is not written into a buffer as it arrives.
- **The gutter mark is one mark.** PROTOCOL §9 permits only `warning` and `information`, so there
  is no severity to distinguish.
