# Writing

The register for every document in this repository, stated so a reviewer can check it rather than
argue about taste.

- **A word earns its place by carrying a fact.** Banned outright: `powerful`, `simple`, `simply`,
  `seamless`, `robust`, `delightful`, `easy`, `effortless`, `just`, `mostly`, `quite`, `very`.
  `just` and `simply` are usually a hedge in front of a step that is neither.
- **No "not X but Y"**, and no sentence that argues with a hypothetical reader. A warning is a
  fact — "`.cursor/mcp.json` configures MCP tools, not language servers" — a case is not.
- **No em-dash aside inside a sentence.** An em dash introducing a short definition is fine; a
  clause between two dashes becomes two sentences.
- **No rhetorical questions, no sales framing, no metaphors** (`front door`, `the whole point`,
  `worth its keep`, `crying wolf`), no throat-clearing (`It is worth noting`, `Of course`).
- **`by design` and `deliberately` only where they carry a decision the reader has to know**
  ("there is no fallback, deliberately"), never as decoration.
- **A sentence whose subject is the document is deleted.** "This section explains the rules"
  becomes the rules.
- **Never trade a fact for a shorter sentence.** Measurements, error strings, paths, schema keys,
  command lines and dated decisions stay verbatim, however long they run.
- **Every number says where it came from** — a log, a run, a commit — and every command, flag,
  setting, environment variable, path and schema key is read against the code before it is written.

The audit, re-runnable:

```sh
grep -rniE '\b(powerful|simple|simply|seamless|robust|delightful|easy|easily|effortless|just|mostly|quite|very)\b|not (a|an|the)? ?[a-z ]+ but |worth noting|of course|the whole point|front door|by design|deliberately' --include='*.md' .
```

Hits are reviewed, not zeroed: `deliberately` and `by design` survive where they name a decision
(a missing fallback, a silent default), and `just` survives where it means *only* ("just the
cursor's line"). Everything else is rewritten.
