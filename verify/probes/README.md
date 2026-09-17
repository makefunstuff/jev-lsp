# Probes

Reproduces every number cited in `docs/research/nvim-lsp-surface.md`.

```sh
verify/probes/run.sh          # all four, exit code = number of failures
nvim --headless -u NONE -l verify/probes/surface.lua
```

| Probe | Evidence for | Asserts |
|---|---|---|
| `surface.lua` | §1, §11 | The server→client surface this client implements, and the capability values the design depends on. Nonzero on mismatch. |
| `capabilities.lua` | §11 | Verbatim dump of the cited capability subtrees. Data, not assertions. |
| `edit-version.lua` | §3 | The four `WorkspaceEdit` version behaviours the frozen edit contract is written against. Nonzero if any row changes. |
| `streaming.lua` | §12 | The client-initiated progress path against a real stdio server: a client-supplied `workDoneToken` reaches the server, and `$/progress` under it arrives as `begin,report,end`. Nonzero on mismatch. |
| `trace.lua` | §2, §4, §8 | The frozen contract as a **whole exchange** against the real client: capabilities accepted, fast-path action carries no edit, resolve adds a versioned `documentChanges` edit that applies, a stale-stamped edit is refused, findings round-trip with `data.finding_id`, and `workspace/diagnostic/refresh` is acknowledged. Nonzero on mismatch. |
| `language.lua` | `docs/LANGUAGE.md` §1, §2 | Universal support: the built-in path misses unidentified files, the plugin attach pass covers them, `get_language_id` recovers language for a buffer Neovim never identified, and the hook does not mutate buffer state. Nonzero on mismatch. |
| `undo-granularity.lua` | §10 | Nothing. Documents an inconclusive probe; see its header for the interactive version. |

`language.lua`, `streaming.lua` and `trace.lua` need `python3` on `PATH` for their stub
servers (`language/server.py`, `streaming/server.py`, `trace/server.py` — standard library
only, no LSP in the product's dependency set).

`trace.lua` is the closest thing to an acceptance test for unit U1 in
`docs/ROADMAP.md`: it fails if any message shape in the frozen contract is rejected or
ignored by the client, which is exactly the class of defect that the design's own progress
token bug belonged to.

Requirements: a `nvim` on `PATH`. No plugins, no config — every probe runs under
`-u NONE`.

These run at the start of any session that touches the protocol, and their output is
appended to the research document when it changes. A probe changing behaviour is a
protocol event, not a test failure to work around.
