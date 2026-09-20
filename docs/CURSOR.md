# Cursor

Cursor has no settings-only route to a language server. Its documentation has no page about
language servers — the sitemap at `https://cursor.com/llms.txt` lists plugins, rules, skills,
MCP, hooks, subagents and cloud agents, and nothing else — and the only extension APIs Cursor
adds are `vscode.cursor.mcp.registerServer` and `vscode.cursor.plugins.registerPath`
(`https://cursor.com/docs/extension-api`). Neither starts a language server.

The route is the one VS Code and every fork have: **an extension that starts the server**.
`editors/cursor/` is that extension, in this repository, with no build step. It is the whole
client — `child_process` and JSON-RPC over the same `Content-Length` framing the server already
speaks — and it is what the rest of this document is about.

## 1. The trap, first

**`.cursor/mcp.json` configures MCP servers, not language servers, and it is not a shortcut.**
Cursor's MCP page (`https://cursor.com/docs/mcp`) describes it as a list of `mcpServers` for
*Model Context Protocol* tools — `stdio` commands, SSE endpoints, HTTP endpoints — that the agent
may call. LSP is a different protocol with a different purpose, and a `jev-lsp --stdio` entry in
`mcp.json` produces an agent tool that speaks the wrong wire, or nothing at all. A reader who
tries it will conclude the server is broken. It is not; it was asked for the wrong protocol.

**There is no settings-only route.** "Cursor cannot do this from settings; it needs a small
extension" is the true sentence, and `editors/cursor/` is the extension.

## 2. What you get

| Surface | Where it appears | Rendered by |
|---|---|---|
| A finding | squiggle under the range, message and severity in the hover, a row in Problems | the extension (`DiagnosticCollection`) |
| A gutter mark | a violet dot in the margin on the finding's line | the extension (decoration) |
| The action menu | `⌘.` / the lightbulb on the finding → `Fix: …`, `Fix all findings (2)`, the verb list | the extension (`CodeActionProvider`) |
| The lens line | per declaration: `jev: explain`, or `jev: N finding(s) · fix` | the extension (`CodeLensProvider`) |
| What is already known | hover, from the cache, without a model call | the server (`textDocument/hover`) |
| An explanation, a plan, a review | a Markdown document in the editor group you are already in | the extension (a palette command) |
| Counts and snapshots | one line in a message, with a **Details** action; the body in *Output → Jev* | the extension (`jev.inspect`, `jev.status`, `jev.recompute`, `jev.revert`) |
| The finding count, the queue | `jev: 2 finding(s)` in the status bar while a command runs | the extension (`$/progress`) |

The commands are the server's own, over `workspace/executeCommand` (PROTOCOL §6): `Jev: inspect
this file`, `Jev: review this file`, `Jev: explain the scope at the cursor`, `Jev: plan a goal at
the cursor`, `Jev: ask a question about this file`, `Jev: ask a follow-up about the scope at the
cursor`, `Jev: status`, `Jev: recompute every open file`, `Jev: session log`, `Jev: usage`,
`Jev: revert an applied edit by id`.

![A jev finding in Cursor. The lens line shows `jev: 2 …` on the declaration that has findings
and `jev: explain` on the two that do not; a squiggle sits under `.unwrap()` on line 7; and the
hover carries the rule's title, its prose, the decision's reason and its probability —
`reachable (p=0.90)` — under the `jev` source name. The hover covers the first lenses and the
gutter column, and the image **predates** the `context.only` fix in §6: its View Problem entry
still reads `No quick fixes available`, so the action menu it shows is not what this extension
does now.](assets/jev-cursor.webp)

## 3. Install it

The server binary has a one-command route that needs no clone:

```sh
cargo install --git https://github.com/makefunstuff/jev-lsp --locked jev-lsp jev
```

→ `jev-lsp` and `jev` in `~/.cargo/bin`. Cursor's extension is the part without one: **no `.vsix`
is published anywhere** — no release asset, no marketplace listing — so the extension has to be
packaged from a clone of this repository. `pack.sh` needs only `bash`, `jq`, `zip` and `unzip`; it
does not need npm and it does not touch the network.

```sh
cd /path/to/jev-lsp

# a .vsix, built with zip and jq alone — no npm, no network
bash editors/cursor/pack.sh /tmp/jev-0.1.0.vsix
/Applications/Cursor.app/Contents/Resources/app/bin/cursor --install-extension /tmp/jev-0.1.0.vsix
/Applications/Cursor.app/Contents/Resources/app/bin/cursor --list-extensions | grep jev
```

Measured on this machine, Cursor 3.21.16:

```
Extension 'jev-0.1.0.vsix' was successfully installed.
makefunstuff.jev
```

`cursor` is usually not on `PATH` on macOS; the CLI lives inside the bundle at
`Contents/Resources/app/bin/cursor`. To work from a clone instead of packaging, symlink it:
`ln -s "$PWD/editors/cursor" ~/.cursor/extensions/makefunstuff.jev-0.1.0`. Reload the window
(`⌘⇧P` → **Developer: Reload Window**) after either.

## 4. Configure the endpoints, and the key

The server reads its endpoints from **its own environment** (PROTOCOL §10), and this is where a
GUI editor bites. A macOS application launched from the Dock does **not** inherit the login
shell's environment, so `export TYPESAFE_API_KEY=…` in `~/.bash_profile` is absent in
Cursor — the decide tier answers `model_error`, the rules pass publishes nothing, and the file
looks clean. Nothing in the editor says why.

Two settings fix both halves. Put them in the project's `.vscode/settings.json` (as the fixture
below does) or in Cursor's user settings:

```jsonc
{
  "jev.server.path": "/Users/you/Work/jev-lsp/target/release/jev-lsp",
  "jev.server.args": ["--stdio"],

  // The decide tier: this is the one the rules pass needs.
  "jev.decide.baseUrl": "https://api.typesafe.ai/v1",
  "jev.decide.model": "jev-latest",
  "jev.decide.wire": "system_one",            // system_one → /systemone, open_router → /alpha/decisions
  "jev.decide.apiKeyEnv": "TYPESAFE_API_KEY", // the NAME of the variable — never the key
  "jev.decide.apiKeyFile": "~/.bash_profile", // the file the key's VALUE is read from
  "jev.decide.timeoutMs": 15000,

  // The chat tiers: needed only by actions, plans and explanations.
  "jev.chat.baseUrl": "http://127.0.0.1:37313/v1",
  "jev.chat.model": "qwen3.6-35b-a3b-iq3xxs",

  // The server's own `jev` section (PROTOCOL §10), passed through verbatim:
  // budget, triggers, rules, noise, languages, and the model tiers themselves.
  "jev.settings": {}
}
```

- **`jev.decide.apiKeyEnv` names the variable; `jev.decide.apiKeyFile` supplies its value.** Given
  a file whose whole trimmed content holds no `=`, that content *is* the key. Given a shell file,
  the key is the value of the last `NAME=value` line for the name in `apiKeyEnv` —
  `export TYPESAFE_API_KEY=…` in `~/.bash_profile` or `~/.zshrc`, read as a shell would read it,
  quotes and all. The value goes into the child process's environment and nowhere else: it is
  never logged, never a setting, and never committed.
- A setting that is empty or whitespace leaves the ambient variable alone, and the server ignores
  empty values itself for the same reason.
- `~` is expanded, because Cursor does not expand it in a string setting.
- Variables §10 lists that are not contributed here — `JEV_REVIEW_MODEL` and the rest — are
  honoured when they are already in the environment the extension inherited.
- Changing `jev.server.*`, `jev.chat.*` or `jev.decide.*` **restarts the server**, because the
  environment it was started with cannot be changed afterwards. Changing `jev.settings` sends
  `workspace/didChangeConfiguration` instead, and the server re-pulls (PROTOCOL §10).

## 5. Look at it working

Any repository with a rule. The fixture used for the captures below is a directory that is a git
repository, has `.git/`, one rule, and one `.rs` file with a `.unwrap()` in it:

```jsonc
// .jev/rules/example.json
{ "schema": "jev.rules/1",
  "rules": [
    { "id": "no-unwrap-in-handlers",
      "title": "Unwrap in a request handler",
      "text": "A handler must not unwrap; return the error instead.",
      "severity": "warning",
      "applies_to": ["**/*.rs"],
      "inspection": { "kind": "regex", "pattern": "\\.unwrap\\(\\)" },
      "judgement": { "question": "Is this unwrap reachable from a request handler?",
                     "criteria": { "true": "a request can reach it", "false": "test code" },
                     "reasons": { "reachable": "a request can reach it" },
                     "min_probability": 0.75 },
      "verb_hint": "fix" } ] }
```

1. Open the folder in Cursor. Open the file with the `.unwrap()`.
2. **Save it.** The ambient pass runs on `didSave`, and `workspace/diagnostic/refresh` follows;
   the extension re-pulls and the squiggle, the gutter mark and the Problems row appear.
3. Hover the squiggle: the rule's title, its prose, the reason the decision gave and
   `(p=0.90)` — never model prose you have to interpret (PROTOCOL §9).
4. `⌘.` on the finding: `Fix: Unwrap in a request handler` first and `isPreferred`. Picking it
   resolves to an edit and the editor applies it.
5. The lens line above `read_config` reads `jev: 2 finding(s) · fix`; click it for the picker, or
   **Jev: explain the scope at the cursor** for a Markdown explanation.

**An answer never rearranges the editor.** `jev.artifacts.viewColumn` defaults to `active`, so a
document opens in the group you are already in and the layout does not change. The setting's three
values, and why `active` rather than the first version's `beside`, are in
`editors/cursor/README.md`, *Where an answer appears*.

**Four commands never open a document**, because their whole answer is a value rather than prose:
`jev.inspect` (counts and skip codes — 280 bytes on this fixture), `jev.status` (a numbers
snapshot), `jev.recompute` and `jev.revert`. They write the body to *Output → Jev* and put one
line in a message with a **Details** action that reveals the channel — measured in Cursor:
`Jev: 2 finding(s) · 1 rule(s) considered · 2 candidate(s)`. A command you run *while* looking at
a file must not replace what you are looking at with six lines you then have to close.

**One consequence of opening in place, said plainly.** The answer becomes the active editor, so a
command that reads *this file* — `inspect`, `review`, `explain`, `followup`, `plan`, `ask` — needs
the code tab focused again and otherwise answers `Jev: open a file first.` The commands that read
no file never refuse for that reason, which is what keeps `status` and `session` usable as the way
to check that anything is working at all: with an answer on screen, both still answer.

**Nothing has to be saved first.** A pull runs the rules pass itself when nothing has been
computed for the document, which is what a client whose edits arrive as `didChange` — or one that
writes files itself — depends on: there is no `didSave` coming. If a rule looks broken, run
**Jev: inspect this file** anyway — it forces the pass and says what it found and what it skipped.
There is also no watcher on `.jev/rules/`: after editing
a rule, run **Jev: recompute every open file**.

## 6. Traps, each one measured

**`context.only` must be a sequence, not a string.** tower-lsp 0.20 pins `lsp-types 0.94.1`,
where `CodeActionContext.only` is `Option<Vec<CodeActionKind>>` (`code_action.rs:338`) — a
divergence from LSP 3.17, which describes a single kind. A bare string fails deserialisation of
the *whole* request:

```
-32602 invalid type: string "quickfix", expected a sequence
```

The provider then throws and Cursor reports `No quick fixes available` for a finding that has a
fix. Both shapes were sent to the built binary; the sequence answers with the ten actions, the
string answers with that error.

**The lens commands are in the plugin's namespace.** `code_lens` in
`crates/jev-lsp/src/server.rs` emits exactly two command ids — `jev.plugin.pick` (title
`jev: N finding(s) · fix`) and `jev.plugin.explain` (title `jev: explain`) — and the prefix is
**hardcoded**: it is a string literal in the server, no setting renames it, and `initialize`
carries no option for it (`initialization_options` appears nowhere in `crates/jev-lsp`). Neovim
does not need them registered because `nvim/lua/jev/init.lua` wraps `vim.lsp.codelens.run` and
dispatches the prefix itself — with a comment recording that a stale prefix-length check once
made the same click fail silently. **A generic client has no such hook**, so it hands the id to
its own command registry and nothing happens. `editors/cursor/extension.js` registers both ids
and does the work: `jev.plugin.explain` calls `jev.explain` and opens the artifact;
`jev.plugin.pick` asks the server for its actions at that line
(`textDocument/codeAction`), shows them in a quick pick, resolves the chosen one and applies it.
Neither is a server command, and there is no `jev.actions`/`jev.action` to call — `COMMANDS` in
the server and PROTOCOL §6 both stop at the fifteen listed there.

**A lens's argument is one object, not two.** `code_lens` sends
`arguments: [{uri, line}]`, and VS Code spreads a command's `arguments` into the handler, so the
handler's first parameter is that object. A handler written as `(uri, line)` binds the object to
`uri`, compares it to a string, finds nothing, and returns — another silent click.

**Cursor's `DiagnosticCollection` has no `onDidChange`.** The interface in the app bundle's
`vscode-dts/vscode.d.ts` stops at `get`. An extension that assumes the upstream API does not
merely lose a repaint — it fails to activate at all, which is what the extension host log said:

```
Extension activation failure: makefunstuff.jev
TypeError: client.diagnostics.onDidChange is not a function
```

**`shutdown` and `exit` take no parameters, and `null` is not "no parameters".** Sending
`params: null` is answered `-32602 Unexpected params: null`, so every session ended in a kill
rather than a shutdown.

**`positionEncoding` is `utf-8` and is never negotiated.** The server's `initialize` returns
`"positionEncoding": "utf-8"` unconditionally and never reads the client's list, so every
`character` on the wire is a **byte offset into the line**, while the VS Code API counts UTF-16
code units. A client that does not convert puts every range after the first non-ASCII character
on a line in the wrong place — quietly, because ASCII lines are identical either way.

**`window/showDocument` uses a `jev://` URI.** For an artifact-producing verb the server answers
with no `edit` and no `command` and signals the artifact with `window/showDocument
jev://artifact/<id>`, without sending the markdown. A client with no `jev://` resolver cannot
render it; the extension answers `{success: false}`, says so in the Output channel, and fetches
the same artifact whole through the command that returns it.

**Every round trip is logged, so the next person can see rather than guess.** *Output → Jev* shows
the command line the server was started with, whether the key was read and from which file (never
its value), what each click did, and every error the server reported. Cursor also persists that
channel to disk, which is what makes a headless check possible:
`~/Library/Application Support/Cursor/logs/<session>/window*/exthost/output_logging_*/3-Jev.log`.

## 7. What is verified, and what is not

Verified on this machine, Cursor 3.21.16, `target/release/jev-lsp`:

- **The extension installs and activates.** `cursor --install-extension` reports success,
  `--list-extensions` names `makefunstuff.jev`, and the extension host log records
  `Extension activated success` with a `jev-lsp --stdio` child process underneath it.
- **An answer and a summary go where they should.** In the real window, `explain` left exactly
  one editor group at the same geometry with the artifact as a tab — no split — and `inspect`
  opened no document, showed one line and put the body in the channel.
- **Diagnostics reach the editor.** Driving the real window — `⌘S` on the fixture — leaves an
  `analysis` entry in the server's own record (`<root>/.git/jev/session.jsonl`) with
  `count: 2`, `findings: [4, 6]`; the capture shows the squiggle and the hover that follow, and
  the lens line above the declarations.
- **The key path works with no secret anywhere but the environment.** The decide endpoint was
  driven end to end through a gate that refuses any call whose `Authorization` is not the key
  read out of `~/.bash_profile` by the extension; without the key it answers `401` and the pass
  produces nothing.
- **The lens bridge works.** `jev.explain` at a finding's line returns `ok: true` in the server's
  record when the command is run from Cursor's own palette, and the artifact opens.
- **The protocol layer, without a GUI.** `extension.js` is loaded against a stand-in `vscode`
  and the real binary: the pull returns two findings, the quick-fix path returns the actions with
  `context.only` set, the gutter is painted, the exact lens `arguments` are fed to
  `jev.plugin.explain` and `jev.plugin.pick` the way VS Code spreads them, and both produce the
  artifact and the edit.

Not verified, and why:

- **That a human click renders and applies.** Nothing scriptable can click Cursor's lens; the
  bridge is proven up to the boundary the editor owns. This is the gap the capture is for.
- **`rule_source` on an inspect finding.** The extension renders `jev.inspect` through its
  summary path: one line in *Output → Jev*, carrying the counts and the number of findings. That
  line does not print which rule set a finding came from, so the ` [builtin]` / ` [repository]`
  marker the Neovim surface shows after each label has no equivalent here, and a reader cannot
  tell a shipped rule's finding from one this repository wrote. A known gap rather than a design
  choice; the code change is routed separately.
- **Multi-root workspaces, remote/SSH and dev containers.** One server process per window, rooted
  at the first workspace folder.
- **Streaming into a buffer.** `$/progress` messages reach the status bar; the partial markdown
  in `data` is not written into a document as it arrives.
- **Windows and Linux.** Only macOS was exercised.
