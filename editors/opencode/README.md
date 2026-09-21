# jev for OpenCode

**Known distortion:** by default the bridge remaps rule findings Warning→Error and prefixes
the message `[jev warning]`, because OpenCode 1.18’s agent transcript drops Warning. That is a
display lie to bypass a client filter — not the rule’s real severity. Prefer keeping Warning
as Warning when OpenCode will show it; until then set `JEV_OPENCODE_REMAP=0` to opt out of the
remap (findings may then be invisible to the agent).

OpenCode 1.18 shows jev findings through a bridge: a small stdio proxy that starts
`jev-lsp --stdio` and translates the server's pull-based finding path into the
`textDocument/publishDiagnostics` push OpenCode listens for. This is the **supported
workaround**, not a fix of OpenCode’s native path (issue #21 stays open).

```
OpenCode  ──stdio──▶  jev-lsp-opencode-bridge.py  ──stdio──▶  jev-lsp --stdio
```

Everything else — rules, inspections, judgement, code actions, lenses, commands — is the
server in `PROTOCOL.md`, unmodified.

## Why the server is not configured directly

Measured against OpenCode 1.18 (issue #21), the native path loses findings for three reasons:

- OpenCode advertises `workspace.diagnostics.refreshSupport: false` and answers the
  `workspace/diagnostic/refresh` request the server sends anyway with an empty OK. The re-pull
  the server asks for never happens.
- It pulls `textDocument/diagnostic` once when a document opens, and keeps that answer for the
  session. At that moment the ambient pass has not finished, so the report is empty and stays
  empty in OpenCode's view.
- `Diagnostic.report()`, which feeds the agent after a write, keeps `severity === 1` (Error)
  only. Every rule finding is a Warning, so even a re-pulled document reports nothing to the
  model.

`jev-lsp` serves findings by pull: ambient analysis finishes, the server sends
`workspace/diagnostic/refresh`, the client re-pulls (`PROTOCOL.md` §3.4, §9). VS Code
(`editors/cursor/`) and the Neovim plugin implement that; OpenCode does not finish it. Pushing
ambient findings from the server would break §9, so the translation belongs on the client side,
which is what this bridge is.

The symptom is an empty `opencode debug lsp diagnostics` for a rule that Neovim and VS Code
surface on the same tree. It is not a rules-authoring or YAML-loading problem.

## Install

`python3` (3.8 or newer, standard library only) and a `jev-lsp` binary. The binary is found in
this order: `JEV_LSP_BIN`, then `jev-lsp` on `PATH`, then this clone's
`target/release/jev-lsp`.

`opencode.json` (or `opencode.jsonc`) in the project root, or in `~/.config/opencode/`:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "lsp": {
    "jev": {
      "command": ["python3", "/path/to/jev-lsp/editors/opencode/jev-lsp-opencode-bridge.py"],
      "extensions": [".rs", ".py", ".go", ".md", ".toml"],
      "env": {
        "JEV_LSP_BIN": "/path/to/jev-lsp/target/release/jev-lsp",
        "JEV_DECIDE_BASE_URL": "https://api.typesafe.ai/v1",
        "JEV_DECIDE_MODEL": "jev-latest",
        "JEV_DECIDE_WIRE": "system_one",
        "JEV_DECIDE_API_KEY_ENV": "TYPESAFE_API_KEY"
      }
    }
  }
}
```

- `extensions` is what OpenCode opens this server for; a language server it already has for the
  same extension keeps running alongside.
- `env` is how the endpoint reaches the child (PROTOCOL §10). `JEV_DECIDE_API_KEY_ENV` names the
  variable holding the key; export the value yourself. `JEV_DECIDE_*` wins over a `jev` section
  sent through the client's settings.
- Settings also travel through the client: `"initialization": {"jev": {…}}` on this entry, or
  `workspace/configuration`, whichever OpenCode 1.18 asks for. The `jev` object is the same one
  `docs/GUIDE.md` §4 documents.

Restart OpenCode after editing the file. `JEV_DECIDE_BASE_URL` unset with no key exported means
every rule judgement is skipped, and the pass reports that rather than a finding.

## What the bridge does

1. Proxies stdio JSON-RPC between OpenCode and `jev-lsp --stdio`, one child process per
   OpenCode session. Requests, notifications, server-initiated requests and errors pass
   through unchanged apart from the four points below.
2. Answers OpenCode's `initialize` with `diagnosticProvider` removed from the capability block,
   which puts OpenCode on its push path instead of the pull path it does not finish.
3. Tells `jev-lsp` `refreshSupport: true`, and on every `workspace/diagnostic/refresh` answers
   the server, pulls `textDocument/diagnostic` for each open document, and pushes the report to
   OpenCode as `textDocument/publishDiagnostics`.
4. Remaps severity Warning → Error and prefixes the message `[jev warning]`, because Error is
   the only severity OpenCode's write transcript shows the agent. The prefix keeps the message
   honest about what the rule decided. `JEV_OPENCODE_REMAP=0` leaves severities alone.

OpenCode never sends `textDocument/didSave`, and the rules pass runs on save, so the bridge
synthesizes a save for a document it opens or sees change. Nothing else is invented.

`jev-lsp` publishes on its own only for edits the server applied (PROTOCOL §9); those publishes
are forwarded with the same severity remap.

## Limits

- **A finding appears after the pass, not on keystroke.** Ambient analysis runs on the
  synthesized save and calls the decide tier, so the diagnostic lands a few seconds after
  OpenCode opens or changes the file. The agent transcript shows it on the write that follows.
- **Only documents OpenCode has opened are pushed.** A refresh for a file no client opened has
  nothing to push, which is also true of the pull path.
- **Severity is a display concession.** A rule finding is a Warning in `jev-lsp`; OpenCode sees
  it as an Error whose message begins `[jev warning]`. Anything reading diagnostics
  programmatically should read that prefix, or set `JEV_OPENCODE_REMAP=0`.
- **This is an OpenCode client, not a general LSP shim.** VS Code and Neovim take the pull path
  directly; nothing else should be pointed at this file.

## Verify

```sh
bash editors/opencode/verify-bridge.sh            # add a server path as an argument to override
```

Two stages, on one fixture (a `.rs` file with an `.unwrap()` and a rule that flags it) with the
shipped rule set off, and no key or network involved: the decide tier is `verify/stub_model.py`,
and its decision call is stalled (`BRIDGE_STUB_DELAY_MS`, default 1200 ms) so the native client's
single pull lands before the finding exists rather than winning a race.

- **OpenCode itself**, when `opencode` is on `PATH`: `opencode debug lsp diagnostics handler.rs`,
  run twice with only `lsp.jev.command` changed. With the server configured directly it prints
  `[]` for a file the rule flags; behind the bridge it prints the finding at severity `1` with
  `[jev warning]` in the message. OpenCode absent is a SKIP with the reason.
- **A client written to OpenCode's behaviour** (`editors/opencode/opencode_probe.py`:
  `refreshSupport: false`, one pull on open, no `didSave`), run both ways. The native run has to
  record the empty pull, the acknowledged refresh and zero pushes, which is issue #21 stated as
  fields; the bridge run has to receive the finding at severity `1`.

Both directions are asserted in both stages, so a green run cannot come from the fixture having
produced no finding. The harness fails where it should: with `editors/opencode/` replaced by a
plain `exec` of the server, the bridge stage goes red and the native reproduction stays green.

Knobs: `BRIDGE_STUB_PORT` (default 8098), `BRIDGE_STUB_DELAY_MS`, `BRIDGE_NATIVE_TIMEOUT`,
`BRIDGE_TIMEOUT`, `OPENCODE_BRIDGE_OUT` (log path, default `/tmp/jev-opencode-bridge.log`). A
missing binary or a port already in use is a SKIP with the reason, never a pass.

## Upstream

Issue [#21](https://github.com/makefunstuff/jev-lsp/issues/21) is the OpenCode client path. Two
of the bridge's four jobs exist only because of OpenCode 1.18 behaviour: the stripped
`diagnosticProvider` and the severity remap. When OpenCode implements
`workspace/diagnostic/refresh` and shows warnings to its agent, `lsp.jev.command` can point at
`jev-lsp --stdio` with `["--stdio"]` as the argument, and this bridge can be deleted.

Until then, `jev-lsp --stdio` configured directly in OpenCode is unsupported: it produces empty
diagnostics, and no server-side change can be made without breaking `PROTOCOL.md` §9.
