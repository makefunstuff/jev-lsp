# First run

Install jev-lsp, add one rule, save a file, see a finding, act on it. After that, use [`docs/GUIDE.md`](GUIDE.md) as the reference.

**Jev is a classifier.** A rule asks one typed question about one line; the answer is a value and a probability. The ambient pass publishes a finding only when that probability clears the rule's floor.

---

## 1. Install

Rust ≥ 1.75. Puts `jev-lsp` and `jev` on `PATH` (`~/.cargo/bin`):

```sh
cargo install --git https://github.com/makefunstuff/jev-lsp --locked jev-lsp jev
```

→ both binaries resolve from a new shell (`which jev-lsp`).

Load the Neovim plugin (owns attach, keymaps, `:Jev`). Symlink the repo's `nvim/` directory, or point your plugin manager at it:

```sh
ln -s /path/to/jev-lsp/nvim ~/.local/share/nvim/site/pack/jev/start/jev
```

```lua
require('jev').setup({})
```

→ nothing on screen yet. Plugin needs Neovim ≥ 0.12.

Without the plugin:

```lua
vim.lsp.config('jev', {
  cmd = { 'jev-lsp' },
  filetypes = { 'rust' },
  root_markers = { '.git' },
})
vim.lsp.enable('jev')
```

---

## 2. Decide endpoint

The rules pass needs a decide tier. Hosted Jev is the default (`https://api.typesafe.ai/v1`, model `jev-latest`):

```sh
export TYPESAFE_API_KEY=…
```

→ nothing printed. Other routes: [`docs/MODEL.md`](MODEL.md) §8. Chat tiers (actions, plans, explanations) are separate and not required for a finding.

---

## 3. Attach

Open a real file (not a scratch buffer) and check:

```vim
:checkhealth jev
```

→ look for:

```
ok    Neovim 0.12.x (>= 0.12)
ok    server binary: …/jev-lsp
ok    client attached
ok    decide tier: jev-latest at https://api.typesafe.ai/v1
```

`warn TYPESAFE_API_KEY is not set` means the default endpoint will refuse calls. `warn no jev client attached` means the current buffer is not a named file.

Confirm from Lua:

```vim
:lua =#vim.lsp.get_clients({ name = 'jev' })
```

→ `1` (or more) when attached; `0` means nothing below will work.

---

## 4. One rule

Rules live under `.jev/rules/` at the repository root:

```sh
mkdir -p .jev/rules
cat > .jev/rules/handlers.json <<'JSON'
{
  "schema": "jev.rules/1",
  "rules": [
    {
      "id": "no-unwrap-in-handlers",
      "title": "Unwrap in a request handler",
      "text": "A handler must not unwrap: a bad request would take the worker down. Return the error instead.",
      "severity": "warning",
      "applies_to": ["**/src/**/*.rs"],
      "inspection": { "kind": "regex", "pattern": "\\.unwrap\\(\\)" },
      "judgement": {
        "question": "Is this unwrap reachable from a request handler, rather than from test or startup code?",
        "criteria": { "true": "a request can reach it", "false": "test or startup code" },
        "min_probability": 0.75
      },
      "verb_hint": "fix"
    }
  ]
}
JSON
```

Open a Rust file the glob claims, then:

```vim
:Jev inspect
:w
```

→ `:Jev inspect` prints what the pass did (rules considered, candidates, findings, skips). After `:w`, a sign appears on any line both the pattern and the decide answer accept. Latency is the decide model (about 1.5–7 s on a cloud endpoint).

If nothing appears, that is data: no rules claiming this file, git reports the file unchanged, or the answer stayed under the floor. `:Jev inspect` says which. A repository with no rule files gets `no_rules` and no ambient findings.

Three traps:

- Editing a rule does not repaint findings already on screen. The next pass for that document applies it; `:Jev inspect --force` applies it now. Nothing watches `.jev/rules/`.
- A broken rule file is skipped with a reason; the rest still load.
- Write the judgement question so `true` means the code is wrong. A question phrased as the property you want publishes nothing.

Full field table and floor guidance: [`docs/GUIDE.md`](GUIDE.md) §4.

---

## 5. Act, then save again

Cursor on the flagged line, `<leader>ja` → action picker (cached). Pick a fix; `<leader>ju` restores the buffer if you disagree.

A single disabled entry *"analysing in the background; reopen the menu in a moment"* means the pass is still running — wait for the sign and reopen.

`:w` again → the finding leaves the margin if the fix covered it. Dismissals persist under `<root>/.git/jev/dismissed.json` and stay filtered out.

`:Jev status` — queue, budgets, endpoints. `:Jev usage` — what was published this session.

Plugin keymaps:

| Key | Action |
|---|---|
| `<leader>ja` | code-action picker (visual = selection scope) |
| `<leader>ju` | undo last applied edit |
| `<leader>jq` | ask about the file |
| `<leader>js` | status |

---

## 6. Next

| Doc | What |
|---|---|
| [`docs/GUIDE.md`](GUIDE.md) | Surfaces, settings, schema, CLI, troubleshooting |
| [`docs/CURSOR.md`](CURSOR.md) | Cursor extension |
| [`docs/MODEL.md`](MODEL.md) | Tiers, routing, local routes |
| [`docs/UX.md`](UX.md) | Surface decisions |
| [`PROTOCOL.md`](../PROTOCOL.md) | Frozen LSP surface |
| [`README.md`](../README.md) | Install routes for every client |
