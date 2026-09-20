# STATUS

**Objective**: a working LSP that turns an editor into an AI-driven harness — the model observing
in the background and proposing work through standard LSP surfaces — rather than a prompting TUI.
Neovim is the first-class client (the plugin, the attach pass, `:Jev`); it is not the only one, and
nothing in the protocol assumes it.

**State**: working and verified end to end. Design frozen in `PROTOCOL.md`; implementation
in `crates/` and `nvim/`; verification in `verify/`.

**Next action**: nothing is open. **The rules have two sources: the repository's own and a set
shipped in the binary** (2026-09-20). `.jev/rules/*.json` still wins — a file there shadows a
shipped rule with the same `id` — so nothing a repository writes is overridden silently; what
changed is that a repository which has written none is inspected with the shipped set and *told
so* (`default_rules`, and `rule_source` on every finding) instead of getting nothing. That was
the measured defect: a repository with no `.jev/rules/` produced no ambient findings at all, so a
fresh install and a broken one looked the same, and this project's own conventions published
nothing on the repository they were written for. `rules.defaults: false` restores the old
behaviour exactly, `jev rules init` writes the shipped set into `.jev/rules/` to read and edit
(idempotent, non-clobbering, exit `2` when it refuses to overwrite a file you edited), and
`:Jev inspect` / `jev inspect` name the source of every finding. Two things a user will otherwise
discover the hard way: **a rule edit does not repaint the findings already on screen** — the
shared display slot is keyed by content hash, resolved language and the findings cap, so the
*next* pass for that document (the next save, or the idle trigger) applies it, while
`:Jev recompute` clears the cache and `jev inspect --force` re-runs it now — and there is still
**no fallback**: with the shipped set off and no rules of your own, a repository gets no ambient
findings and the pass says `no_rules` rather than reporting a clean document. The rule *files*
themselves are being written by two other sessions in `crates/jev-core/default_rules/code/` and
`/prose/`; the mechanism is built and tested against fixtures, and the end-to-end assertion over
the real set is the one to re-run when they land.

**Previously**: **inline completion is gone** (2026-09-19, at the user's decision): the `fim`
tier, the `textDocument/inlineCompletion` method, the `inlineCompletionProvider` injection, the
gate stack, the `<Tab>` acceptance and their harnesses are removed from the server, the plugin,
the contract and the docs. Generated code is asked for — an action, `:Jev ask` — rather than
offered under the cursor; the staged `qwen2.5-coder-7b` preset was reverted with it.

**Retracted (2026-09-19).** The two `verify/nvim_ui_test.lua` failures recorded here on
2026-09-18 (the sibling buffer's `MARKER_SIBLING_A` never reaching the prompt) do not
reproduce: three consecutive runs are **0 failures, 0 skips**. The root cause given for them —
`push_definitions` returning early because a stock Neovim 0.12 ships no Python parser, which is
still true (`vim.treesitter.language.add('python')` answers `No parser for language "python"`) —
was never established as the cause of those failures, and the checks that failed do not read the
pushed context at all. The likeliest explanation is the one `docs/VERIFICATION.md` warns about:
a stale stub process bound to the port, since a fresh stub on a free port makes every one of
those checks pass. What is verified now: 44 ok / 0 FAIL on the independent client (including the
assertion that no draft capability is advertised), smoke 44/44, plan 35/35, parity 30/30,
queue 5/5, config race 3/3, supersede 7/7, dismiss 0 failures, rules 45/45 and rules_live 0
failures on both Neovim versions, nvim_live and nvim_ui_test 0/0, latency 7/7 paths. The
real-endpoint harnesses were re-run through OpenRouter (`docs/VERIFICATION.md` §7):
`quality_eval` 3/4 recall and 3/3 precision with 0 findings on both clean files,
`real_model` one finding in 0.8 s, `soak` 10/12 applied with 2 unparseable.

**Previously**: everything in the roadmap is built. The next useful step is a longer
real-model soak — the six runs so far are one language on one model, and the last three
defects were all real-model-only — and an interactive check of inline-completion ghost text,
which a headless harness cannot drive.

---

## Open questions

1. **Which model, and at what cost.** The suite runs against `verify/stub_model.py` by default;
   the real-endpoint harnesses now run through OpenRouter (`google/gemini-2.5-flash-lite`, key
   from `~/.omp/agent/openrouter.key`), and the decide tier was exercised against the hosted
   `typesafe/jev-1.13` — see `docs/VERIFICATION.md` §7 for the numbers, including 3/4 recall on
   that cheap model against 4/4 for `deepseek/deepseek-v4-flash`. The local servers serve
   `qwen3.6-35b-a3b-iq3xxs` (port 37313) and `qwen3.8-27b-gsq-rco-iq3xxs` (port 40583); neither
   has been woken for this work, because that consumes VRAM and GPU arbitration here is manual.
   Awaiting the user's call.
2. **Whether `triggers.diagnostics` should stay `save`.** `save` is predictable and cheap;
   `idle` is more ambient but fires while typing. Default is `save`; measured cost on a real
   model will decide it.
3. **`no-client-namespace-in-a-server-id` fires on `crates/jev-lsp/src/server.rs:1523`
   (`jev.plugin.pick`) and `:1526` (`jev.plugin.explain`), the two lens command ids the server
   emits into the plugin's namespace, and it is open rather than fixed** (2026-09-20). The
   gate's decide tier put it at **0.71–0.76 across four runs**, against the rule's floor of
   0.55, so the verdict moves between runs; that spread is the argument for **narrowing the
   rule** rather than raising the floor. What it costs today: a client that is not the plugin
   must bridge both ids itself (`editors/cursor/extension.js` registers them, and
   `docs/CURSOR.md` §6 records the trap), and without that bridge the click does nothing, which
   is the whole of the reported symptom. Two honest exits, one line each: **change the ids** so
   the server emits its own served command names, or **narrow the rule** to what it means, a
   server id naming a client that is not the one attached. Raising the floor is not an exit.
   The finding is a decision for the user, not a defect in the rule: the pattern matches the two
   ids the server emits.
   A gate run reports only the paths it was given — by default the files that differ from
   `origin/main`. A green therefore means those paths were scanned and clean; it says nothing
   about a file that was not in the set. The counts line it prints names how many files were
   scanned (`rules-gate: N file(s), …`), and a run that prints no counts line and exits `2` checked
   nothing and is not a pass. That distinction is why two reports disagreed about these two lines:
   the run that read as quiet exited `2` in a checkout with no decide key, no built binary and no
   base to diff against, so `server.rs` was never scanned.
   Awaiting the user's call.
4. **Ten declared settings are in the schema and read by nothing; whether to implement each or
   delete it is open** (2026-09-20). Eight settings and two override fields: `ambient.code_lens`
   (default `true`), `ambient.inlay_hints` (`false`), `auto_apply.fix` (`false`),
   `auto_apply.fixAll` (`false`), `budget.timeout_ms` (`30000`), `log` (`"warn"`),
   `triggers.severity_floor` (`"information"`), `noise.suppress_after_dismissals` (`2`), and
   `languages.overrides.<lang>.tier` / `.prompt` (`config.rs::verbs_for` reads only `.verbs`,
   `crates/jev-core/src/config.rs:477-483`). Each appears to promise something: that the code
   lens or the hints can be switched off, that a fix can apply itself, that a budget has a
   per-call ceiling, that the log has a level, that a severity floor silences low findings, that
   a dismissal count suppresses a repeat, and that a language's tier and prompt flavour can be
   overridden per language. Setting any of them changes nothing today, which is the "advertise
   only what is served" rule applied to configuration. `PROTOCOL.md` §10 now lists all ten;
   `docs/LANGUAGE.md` §3 and §7, `docs/GUIDE.md` §3, `docs/MODEL.md` §2 and `docs/UX.md` §4 no
   longer present them as working. Two exits, one line each: **implement the setting**, or
   **delete the key with the paragraph documenting it**. Awaiting the user's call.

## Decisions taken (reversible, recorded so they are not relitigated)

- Repository directory is `jev-lsp`; workspace, binary, and crates are `jev*`.
- UTF-8 position encoding, frozen (PROTOCOL N1).
- No custom `jev/…` LSP methods (N6). `workspace/executeCommand` plus `$/progress` is the
  whole back-channel.
- **Advertise only what is served** (PROTOCOL §2). A capability nothing answers for is a lie
  the client acts on, and `verify/lsp_client.py` asserts the absence as well as the presence.
- **The ambient pass is the rules pass, and there is no fallback** (PROTOCOL §12). A
  repository's conventions are data (`.jev/rules/*.json`, `jev.rules/1`), a decision is a
  different protocol from a chat, and the review tier does not step in when no rule claims a
  file: with the shipped set off and no rules of your own, `jev.inspect` says `no_rules` instead
  of reporting a clean document. A generative review on every save would cost thousands of tokens
  and its silence would be unattributable.
- **The rules have a shipped source as well as the repository's** (2026-09-20). The same refusal
  above, applied to the observed defect it left: `.jev/rules/` was the only source, so a
  repository with none got nothing and a fresh install could not be told from a broken one.
  `crates/jev-core/default_rules/<group>/*.json` is now embedded by `build.rs` (an empty tree
  builds, and behaves exactly as before), the repository's file shadows a shipped rule with the
  same `id`, `rules.defaults` turns the shipped set off, and `jev rules init` writes it out so a
  rule can be read before it is believed. The defaults are hand-written data — §12's refusal of a
  *generative* fallback is untouched — and every finding says which source it came from
  (`rule_source`), because a finding you cannot trace to a file you can open is one you cannot
  calibrate or turn off.
- **One name, swept clean.** No alias and no migration shim anywhere — an old command name
  answers `not_implemented`, settings are read under the section `jev`, and the harness tables
  and documents were renamed with the code. Only the dated history rows keep the old name.
- **No ghost text.** Inline completion was removed on 2026-09-19 (see the log). The cursor is
  not a place this server writes to; generated code arrives as an action or an answer.
- **`explain` is a command, not a code action**, because a resolved action's `command` is
  executed by the client by sending it back to the server, so it cannot open a buffer.
- The plugin, not the server, owns UI that LSP cannot express: text input, scratch buffers,
  undo snapshots.
- No treesitter dependency yet. Scope is structural with a whole-file fallback, and
  `scope_source` reports which was used.
- **The rules gate is a per-change instrument, not a suite row** (2026-09-20). It runs at commit
  time and in CI over the changed files (`verify/rules-gate.sh`, the same pass the editor runs);
  the suite has no whole-tree gate row, because a row permanently red on `main` for a known
  finding that is decided and waiting is how red stops meaning anything, and a row that exits 0
  while printing the same findings cannot be told from passing, which is what
  `no-success-without-a-measurement` exists to prevent. A finding it raises goes to the open
  questions above until it is decided.
- **MIT** (2026-09-20). The user's choice of licence; the text is in `LICENSE` at the repository
  root.

## Verification backing the implementation

| Check | Result |
|---|---|
| `bash verify/run-suite.sh <out-file>` | the whole table, one run: one supervised stub for the run, every row's output and `EXIT=` captured into the file, a verdict per row to stdout, and the exit code aggregated from those `EXIT=` values rather than from a text grep. Its first row is the runner's own `stub health` (`/health` answered and the pid alive), and the six Lua rows below run once per binary in `NVIM_BINS`. Knobs in its header: `NVIM_ONLY=1`, `TAKE_OVER=1` (`REFUSE_IF_BUSY=1` is the older spelling of the same opt-in), `STUB_PORT`, `NVIM_BINS`, `JEV_WS`, `KEEP_WORKSPACE=1` |
| `cargo test` | 306 passing (52 `jev` + 202 `jev-core` + 52 `jev-lsp`), 0 failed, no warnings |
| `cargo build --release` | no warnings, no errors |
| `verify/probes/run.sh` | 7 probes, `0 probe(s) failed` |
| `python3 verify/latency.py` | `[latency] 7 path(s) within budget`, against a model made 2 s slow |
| `python3 verify/queue_test.py` | `[queue] 5/5 checks passed` |
| `python3 verify/config_race_test.py` | `[config-race] 3/3 checks passed` |
| `python3 verify/settings_race_test.py --bin …` | exit 0 — the first model call of a session used the endpoint the client configured, not the built-in default (`SETTINGS_RACE_OK`) |
| `python3 verify/scope_containment_test.py --bin …` | exit 0 — an answer may not reach outside the scope the client named, and the refusal names the scope |
| `python3 verify/supersede_probe.py` | `7 ok, 0 FAIL, 0 skip, 0 warn` |
| `python3 verify/smoke.py` | `[smoke] 44/44 checks passed` against the real binary, three consecutive full-table runs since the two harness defects in `docs/VERIFICATION.md` §8 were fixed |
| `python3 verify/outcome_test.py` | 18 checks, `every check passed` — the `jev.outcome` record and the `jev.usage` counts |
| `python3 verify/plan_test.py` | `[plan] 35/35 checks passed` |
| `python3 verify/cli_parity.py` | `[parity] 34/34 checks passed` — the CLI and the LSP agree exactly, `jev inspect` included; five of the checks are the nested-file case that pins the CLI's rules root |
| `python3 verify/lsp_client.py --server … --workspace … --stub-model-url …` | `44 ok, 0 FAIL, 0 skip, 0 warn` — the independent, spec-derived client. Step 10 covers one `begin`, one `end` and a live server on the model-error, budget-refusal and cancellation paths; step 9 records the `end`/response interleaving instead of asserting it and asserts what the command controls, nothing under the token after its `end`, read after a settle window; the row also asserts that no draft capability is advertised |
| `python3 verify/lsp_client.py --selftest` | `32 ok, 0 FAIL, 0 skip, 0 warn` — the client's own defect-injection net: no server, no stub, no network, each injected defect required to turn its step red |
| `python3 verify/lsp_framing_test.py` | `[framing] 9/9 checks passed` — the client's own stdio framing; the bug it was written for is in `docs/VERIFICATION.md` §8 |
| `python3 verify/rules_test.py --bin …` | `[rules] 55/55 checks passed` — the rules pass: inspections, `applies_to`, the changed set, the cache, the skips |
| `python3 verify/rules_gate_test.py` (the runner hands it the stub) | `[rules_gate] 0 failure(s), 0 skip(s)` — the gate's own three codes: a seeded violation exits 1, a file no rule claims 0, a gate that cannot run 2 |
| `nvim --headless -l verify/nvim_live.lua` | 11 ok, 0 failures, 0 skips with a stub endpoint (1 skip without one: the resolve step has no model) |
| `nvim --headless -l verify/dismiss_test.lua` | 7 ok, 0 failures, 0 skips — a finding is dismissed, recorded per repository, and does not resurface |
| `nvim --headless -l verify/nvim_ui_test.lua` | 116 ok, 0 failures, 0 skips — the plugin's own surfaces with a fresh stub; it reads the stub's control plane from `JEV_BASE_URL`, and it gives its fixture root back unless the caller names one |
| `nvim --headless -l verify/rules_live.lua` | 39 ok, 0 failures, 0 skips on Neovim **0.12.5 and 0.12.1** — a rule's finding on the sign column after a save, `:Jev inspect` answering with the same finding, its counts and its skips, `--force` re-running an unchanged document |
| `nvim --headless -l verify/result_surface.lua` | 110 ok, 0 failures, 0 skips on **both** Neovim versions — a report never changes the window count across the command, `q` puts the buffer the user was in back on screen, an unsaved buffer is still modified, the diff preview leaves the user's buffer-local maps alone, and a send that fails says so and takes its surface back |
| `nvim --headless -l verify/context_search.lua` | 7 ok, 0 failures, 0 skips — the plugin's own local search: `rg` and the `grep` fallback cite the same files and lines, and with neither engine installed the refusal names both |
| `bash verify/omp_lsp.sh` | `[omp] 0 failure(s), 1 skip(s)` in CI — OMP, a client that shares no code with this repository, receives a rule's finding over `textDocument/diagnostic` and reaches `workspace/executeCommand jev.inspect`, a no-rules control finding nothing. The skip reads `omp is not on PATH, so a non-Neovim client cannot be driven`; on a machine with `omp`, the row reports 0 skips |
| `python3 verify/quality_eval.py --base-url https://openrouter.ai/api/v1 --model google/gemini-2.5-flash-lite` | **recall 3/4, precision 3/3, 0 findings on both clean files**, 6 calls / 2950 tokens billed, 4.5 s, exit 0 — the miss (`swallowed_error.py`) is run-to-run variance on a cheap model (a control run with the same model caught 4/4). Runnable whenever an endpoint and a key are given (`JEV_API_KEY_ENV` names the chat tiers' key variable); the suite reports the row as `?` with the reason when none is, which is what it does in CI (`JEV_API_KEY_ENV names OPENROUTER_API_KEY, which is unset or empty`) |
| `python3 verify/repo_bench.py --repo . --limit 40` | **not a suite row**: nothing in `verify/run-suite.sh` invokes it. Run by hand: 40 files, 33 analysed, 62 findings, **3.21 per 1000 lines** (three runs: 3.21 / 3.48 / 3.71; 7 files per run outran the 60 s per-file bound and are reported as such). Measured 2026-09-18, before the inline-completion removal, which touches no findings path |

The table mirrors `verify/run-suite.sh`'s row list in the runner's order. The numbers are the
suite's verdict at `f9a3290` (run `35510412935`, `verification (full-table)`): 33 rows, every one
`ok` except `quality_eval` (`?`, no key) and `omp_lsp` (1 SKIP, `omp` not on the runner's `PATH`).
Every count and summary line is identical to the previous green run, `bce3d8e` at `35509988121`.
`stub health` is the runner's own first row rather than a harness of its own, and `repo_bench` is
the one row in this table that no runner invokes: it is a measurement taken by hand.

**What the table still cannot say** (2026-09-20). Three limiters, recorded rather than left
implied:

- **Step 2's race is bounded, not proven absent.** The wait is new; the fix's own measurement puts
  the residual at ≲2–3% at 95% over 100–150 clean runs, and the instance that produced it was 1
  red run in 100 under load (`verify/lsp_client.py`'s step 2 note). The assertion itself is
  deterministic, since a stub that asks 0.4 s late fails without the wait and passes with it, and
  `--selftest`'s `no_configuration` still fails. What is unmeasured is the flake's absence, not the
  check.
- **The Lua rows and `verify/probes/*.lua` have never been screened for that class**, an assertion
  reading a server-initiated message with no bounded wait. `verify/probes/streaming.lua`'s single
  wait looks bounded, and nobody has claimed the rest are clean. The screen that found the Python
  instances is a hand-listed vocabulary, and it reported nothing at all for a file that reads a
  differently-named accessor, so for the Lua rows the honest instrument is CI observation rather
  than a screen.
- **The `tower-lsp` ordering gap is unobservable by design.** A token's `end` and the response that
  closes it leave through two arms of one `futures::stream::select`, and the round-robin decides
  which is written first, so `verify/lsp_client.py` records the interleaving instead of asserting
  it. Closing it for real means one ordered outbound queue in the transport, which is upstream.
  `PROTOCOL.md` §3.5, `docs/VERIFICATION.md` §1 and the step 9 note carry that.

The independent client is written from the specification and shares no code with the server;
it caught two things the unit tests could not, both now resolved and one of them documented
as a deliberate boundary in `docs/VERIFICATION.md` §10.

## Log

| Date | Event |
|---|---|
| 2026-09-20 | **The rules gained a shipped source, because "no rules" was indistinguishable from "broken".** Measured: a repository with no `.jev/rules/` produced no ambient findings at all, and this project's own set published almost nothing on the repository it was built for. `crates/jev-core/default_rules/<group>/*.json` is embedded by a `build.rs` that globs it (`std::fs`, no new dependency; an empty tree compiles and behaves exactly as before, which is what lets two other sessions write the rule files in parallel), and `rules::load(root, rules.defaults, builtin)` merges it under the repository's own: a `.jev/rules/<id>.json` **shadows** the shipped rule with the same id, duplicate ids *within* a source are still kept and still linted, and `rules.defaults: false` is the old behaviour exactly. `jev rules init [--dir <dir>] [--force]` materialises the set to read and edit — idempotent, non-clobbering, exit `2` with the refused names when it will not overwrite a file the user edited, and it writes nothing outside the target directory. Every finding carries `rule_source` (`repository` \| `builtin` \| `null` on a review finding), printed by `jev inspect` and by `:Jev inspect`; a pass carried by the shipped set says `default_rules` beside `no_rules`. The cache key is taken over the merged set, so the shipped rules are an input to every rules conclusion, and `noise.max_visible_findings` — which `findings_key` carried and `rules_key` forgot, so widening the cap served the old cap's findings — is in it now. The plugin has no `jobstart`/`vim.system` anywhere and does not know where the CLI binary is, so `rules init` is CLI-only: the plugin's every subcommand is either a server command or an action over data it already holds, and materialising the set from the client would be a second implementation of the writer. `verify/rules_test.py`'s fixtures now set `rules.defaults: false` (their counts are about the rules *they* write, and a shipped `.rs` rule would make every one of them wrong without anything being broken); the shipped path itself is asserted in the crate tests, where the shipped input is a fixture of the test's own. |
| 2026-09-20 | **The verification table's own defects are closed, and CI's rows now mean what they claim.** `verification (full-table)` was red because `verify/settings_race_test.py` and `verify/scope_containment_test.py` declared `file:///tmp` as their workspace: the server writes its session record to `<root>/.git/jev/session.jsonl`, so those two rows created `/tmp/.git`, which then became the workspace of `verify/nvim_ui_test.lua`, the one Lua row with no `.git` of its own, so it read no rules from `/tmp/.jev/rules`, cached 0 findings and lost six checks. `verification (nvim-only)` was green because `NVIM_ONLY=1` skips every Python row, and so ran neither row that creates the marker. Both ends are fixed (`730bd51`), and the rule with its second half, a harness gives back the root it made and leaves a named one where it is, is in `docs/VERIFICATION.md`'s preamble. Four runs since are green: `35505060119`, `35505703708`, `35505942972`, `35506102991`. The narrow row no longer validates a stale binary: it builds `cargo build --release --locked`, guarded to that row (`8092d9e`), proven by the step's own log (`Compiling jev-core` / `jev-lsp` / `jev` above ``Finished `release` profile [optimized] target(s) in 46.42s``), by a run with the binary moved aside, and by a deliberate syntax error (`STEP EXIT=101`). Python **3.10** is the floor, decided by `pathlib.Path.write_text(newline=…)` (`verify/lsp_client.py:1225`, `verify/supersede_probe.py:103`) with no 3.11 stdlib in any row, and pinned in `.github/workflows/ci.yml` at `python-version: "3.10"` (`a0c94d9`), so CI tests the floor rather than inheriting the runner image's. The `lsp_client` row's two flake classes are fixed, each proved two-sided: step 9 asserted the wire order of a token's `end` against its response, which `tower-lsp`'s round-robin `futures::stream::select` decides (4/50 inversions quiet and 1/50 loaded, on one unchanged binary; step 10's paths within 5 µs of flipping), so the row now records the interleaving and asserts the stronger property, nothing under the token after its `end` read after a settle window (`2b45e06`, `--selftest` gains `progress_after_end`); step 2 read `workspace/configuration` with no wait (1 red run in 100 under load) and now waits, bounded, with `no_configuration` still failing (`c0c15c2`). |
| 2026-09-19 | **The workspace is `jev` and the ambient pass is the repository's own rules.** Two changes landed together. First the name: crates, binaries, the plugin path, every command, every schema string, every environment variable, the diagnostic `source` and the `.git/` state directory are `jev*`, swept in one pass with **no alias and no migration shim** — and the session log and dismissal file were deliberately *not* migrated, so a dismissal recorded under the old name is lost once (said here rather than papered over). Second, the demotion: the ambient pass is a *rules* pass. `.jev/rules/*.json` — `"schema": "jev.rules/1"`, each rule pairing an `inspection` (a regex, or the absence of one, matched against the path's `applies_to`) that names candidates and decides nothing with a `judgement` (one question, gated by `min_probability`, default 0.5) — runs over the documents git reports as changed, and asks the new `decide` tier **one call per document**, which answers over the decision wire (`system_one` at `api.typesafe.ai` by default, `open_router` as the alternative, a local System One server one setting away) rather than `chat/completions`. Only a `true` above the rule's floor becomes a finding, and it goes through `findings::build` like every review finding, so ids, ordering, dismissal and the noise cap behave identically; `data.source` says `rules` or `review` while the diagnostic's own `source` stays `jev`. `jev.inspect` (LSP) and `jev inspect` (CLI) run the same code on demand and report `considered`/`candidates`/`skipped` — `unchanged`, `no_rules`, `unlocatable_anchor`, a rule file that failed to load — so "no finding" can never be confused with "nothing was inspected". There is **no fallback**: a repository with no rules gets no ambient findings. Two consequences recorded because they are the things a reader would otherwise discover the hard way: a rule edit is hashed into the rules cache key but the *display* slot is keyed by content, language and the findings cap, so findings already on screen stay until the next pass for that document (`:Jev recompute` or `jev inspect --force` apply it now, and nothing watches `.jev/rules/`); and the decide tier is remote by default, so changed-file text leaves the machine on every rules pass unless `rules.enabled = false` or `base_url` points at a local System One server. Verified: `cargo test` 274 passing (49 + 179 + 46), 0 failed, `cargo build --release` and `cargo test --no-run` warning-free; `verify/rules_test.py` 45/45; `verify/rules_live.lua` 0 failures, 0 skips on Neovim 0.12.5 and 0.12.1 (including `:Jev inspect` returning the same finding and its counts, and `--force` re-running an unchanged document); `verify/cli_parity.py` 25/25; `verify/lsp_client.py` 32 ok / 0 FAIL / 0 skip; `verify/lsp_framing_test.py` 9/9 (two defects the suite found in *itself*, both now pinned: the client's reader discarded a half-read header, desynchronised the stream and died silently — which presented as `latency.py` red two runs in three — and a refresh assertion had no wait of its own, so a race read as a defect; `docs/VERIFICATION.md` §8); `verify/omp_lsp.sh` 0 failures — OMP, a client that shares no code with this repository, received the rule's finding through `textDocument/diagnostic` and reached `workspace/executeCommand jev.inspect`, with a no-rules control producing nothing; smoke, plan, queue, latency, config-race, supersede, outcome, dismiss, nvim_live and nvim_ui_test all green; and a sweep of every document finds the old name only in the dated rows that record it. The whole table is now one command in the repository — `bash verify/run-suite.sh <out-file>` — with one supervised stub and `NVIM_ONLY=1` / `REFUSE_IF_BUSY=1` / `NVIM_BINS` documented in its header; `quality_eval` is the one row it reports as `?` rather than running, because it needs a real model. |
| 2026-09-19 | **Inline completion removed, at the user's decision.** Not just the local coder model: `fim` at all — the tier, `textDocument/inlineCompletion`, the `inlineCompletionProvider` injection, the `<Tab>` acceptance, the prompt and schema, the per-minute window and the prefix floor, and the three harnesses that covered them. `crates/meta-lsp/src/inline.rs` and `advertised.rs` are deleted, so the transport boundary is untouched again and `Server::new(...).serve(service)` is a plain tower-lsp service; the `meta.status` line no longer reports a FIM window and `meta.usage` no longer counts accepted completions (the `meta.outcome` event is still recorded if a stale client sends one). The staged `qwen2.5-coder-7b` preset was reverted from the homelab IaC source, and the live `models.ini` was never touched. Reason given: generated code is asked for, not suggested under the cursor. 219 unit tests, warning-free build; smoke, lsp_client, plan, parity, dismiss, outcome and repo_bench all green. |
| 2026-09-18 | **Three surfaces, quiet, measured — and an outcome the server finally hears.** The plugin's own user could not say what it was for, so six products behind eighteen keymaps are cut to four (`a` act, `u` take it back, `q` ask, `s` status); everything else still works through `:Meta …`. Findings are quieted in exactly one place — `findings::build`, the set every surface reads — and the prompt now asks for less (`PROMPT_VERSION` 3→4): `verify/quality_eval.py` holds 4/4 planted defects with **0 findings on the two clean files**. Every outcome the user makes is recorded (`meta.outcome` from the picker, `:Meta dismiss`, undo, and `<Tab>` accepting a completion; accepts only, since a dismissed overlay is indistinguishable from a cursor move) and counted by `:Meta usage` — the client was the only witness and never said anything, which is why "is this working" had no answer; `verify/outcome_test.py` proves the counting and fails on the binary without it. Completions get one shape: an answer that continues a line which already has code is the rest of *that* line. And the volume is measured on real code at last — `verify/repo_bench.py`, 40 files of this repository, 34 analysed, **62 findings, 3.21 per 1000 lines**. The `qwen2.5-coder-7b` preset for the `fim` tier is staged in the homelab IaC source but deliberately not deployed: CUDA1 has 1.7 GiB free against its 7.5 GiB of weights, and the router reads presets only at startup. |
| 2026-09-18 | Probe pass over the Neovim 0.12.5 LSP runtime; two surprises recorded (absent-version crash, arbitrary-token progress) |
| 2026-09-18 | Design frozen: `PROTOCOL.md` plus `docs/{ARCHITECTURE,UX,MODEL,VERIFICATION,ROADMAP}.md` |
| 2026-09-18 | Audit found the streaming design smuggled a bare progress token; fixed and verified by `verify/probes/streaming.lua` |
| 2026-09-18 | The frozen contract exercised as a whole exchange by `verify/probes/trace.lua`; `source.meta` was missing from the declared kinds |
| 2026-09-18 | Prior-art sweep (`docs/research/prior-art.md`): none of the four comparable projects does ambient findings, splits latency, or supports unidentified files |
| 2026-09-18 | **Support is unconditional** (N10/N11): measured that `filetypes = nil` still misses unidentified files, specified the plugin attach pass, proved 11/11 in `verify/probes/language.lua` |
| 2026-09-18 | **Implementation.** `meta-core` (13 modules) + `meta-lsp` (server, engine, state) + `nvim/lua/meta`. 106 unit tests. |
| 2026-09-18 | **Real bug found by end-to-end run**: a client answering `workspace/configuration` with `{}` reset every setting via `#[serde(default)]`, discarding the model endpoint the environment supplied. Fixed with a deep merge plus explicit environment precedence, with a regression test. |
| 2026-09-18 | First green end-to-end run: smoke 32/32, independent client 23 ok / 0 FAIL, live Neovim 0 failures, and a resolved edit landing in a real buffer with the model call carrying the right language flavour |
| 2026-09-18 | **Two scheduler defects found by the independent client and fixed**: an analysis request arriving while one was in flight was silently dropped, and a superseded run emitted no `workspace/diagnostic/refresh`, leaving a client that had pulled during the window empty forever. Replaced the global in-flight flag with a per-document queue that always signals. Independent client now 27 ok / 0 FAIL / 0 skip. |
| 2026-09-18 | **First runs against a real model**, via the omp auth gateway (`127.0.0.1:4000`), which resolves the provider credential server-side so no key is handled locally. Model `deepseek/deepseek-flash`. |
| 2026-09-18 | **Three real-model defects found and fixed**: (1) a reasoning model exhausts a 2048-token ceiling and returns empty content — the error now names `finish_reason=length`, and ceilings are 4096/2048; (2) a placeholder-shaped schema was echoed back with field names as values — the schema is now a concrete example plus an explicit anti-echo rule; (3) an answer covering more lines than its anchor duplicated code on apply — the replaced range now absorbs the lines the answer re-emits. The bounded repair loop specified in docs/MODEL.md §5, previously unimplemented, was added and is separately budgeted. |
| 2026-09-18 | **Green against a real model**: 6/6 runs produced a finding, an edit, and a file that still parses. Ambient 2.2–3.1 s, resolve 1.1–5.1 s, ~1.4–2.2k tokens per session. |
| 2026-09-18 | **U4, U5, U6, U8 built.** Plans apply through the server with per-step staleness refusal and revert; multi-file edits create files; post-apply verification publishes a divergence diagnostic when reality does not match the prediction. The CLI implements all five frozen exit codes, and parity with the LSP path is proven field-by-field (`verify/cli_parity.py`, 12/12). |
| 2026-09-18 | **U7 recorded as blocked, not skipped.** Neovim attaches its inline-completion handler only for a client advertising `inlineCompletionProvider`; `lsp-types` 0.94 (pinned by tower-lsp 0.20) cannot express that field — it first appears in 0.95.0. Writing the handler now would be dead code no client could reach, so it waits for the dependency move. |
| 2026-09-18 | Re-verified every harness after integration: 187 unit tests, 7 probes, smoke 33/33, queue 5/5, supersede 7/7, plan 35/35, parity 12/12, live Neovim 0/0, plugin UI 0/0. |
| 2026-09-18 | **Hover, project context, `meta.where`, and a quality metric — with two retractions of my own claims.** `textDocument/hover` shows what has already been explained about the scope under the cursor, read from an artifact store and never from a model (verified: silence in 1 ms for an unexplained scope, the markdown for an explained one, the call counter unmoved). Requests that *generate* now carry what the editor can see — imports from the parser, references from the other language servers, the covering test, the open buffers — bounded server-side, ordered, and hashed into the cache key, which is what keeps a cached answer from being served for a question asked in a different project state. The completion cannot assemble that per keystroke, so it gets the cheap half pushed per document version (`meta.document` replaces `meta.definitions`, one guard for both payloads). `:Meta where` greps locally with the question's own words and lets the model rank — the navigation question no index answers, riding the follow-up channel and needing no new server surface. **The retractions:** `verify/quality_eval.py` (recall, precision and noise on planted defects) first reported 1/4 caught identically across a local model, a frontier model, and a prompt naming every defect class, and I said the misses were 'in the pipeline'. Both readings were harness artifacts: the wait keyed on *any* `workspace/diagnostic/refresh`, and `saw_request` accumulates, so after the first file it stopped waiting and called every later file a miss — the server's record showed one analysis in the whole run; and with that fixed the wait is still wrong. **The 25% recall and 'the pipeline is at fault' are retracted.** Three of the harness's own bugs followed: a wait that keyed on an accumulating notification, a wait that then keyed on the wrong file, and two fixture labels whose line numbers were simply wrong. With all three fixed and every row backed by the server's own record, the numbers are: **recall 4/4 (100%), precision 4/5 (80%), zero findings on the clean writer** — the fifth finding is on the file labelled clean and says the code accesses `'port'` without checking the key exists, which is a defensible judgement about a file, not noise. Each file produces exactly one finding, none is discarded, and the phrasings for the two defects the earlier runs 'missed' — `mutable default argument`, `index used without checking the collection is non-empty` — matched the wording added to the prompt this session, which looked like cause and was not: an A/B against the prompt as it was catches 4/4 as well, with one fewer finding on the clean file, so the classes were removed as prompt noise that moves no measured number. |
| 2026-09-18 | **Scope: the parser answers first where there is one.** `explain` and `plan` now carry an explicit `range` when treesitter can name the enclosing declaration, and the server anchors on it (`scope_source = explicit`); without a parser, the language not listed, or no such declaration, the range is absent and the server resolves structurally exactly as the CLI does. Found while wiring it: `cursor_scope` was defined **twice** in the plugin, and everything before the second definition closed over the first — so `explain` used the unpatched one and `plan` the patched one. Merged to a single definition. |
| 2026-09-18 | **Code lenses: the affordance you do not have to remember.** One lens per declaration at the left margin, above the declaration it belongs to: `meta: explain` on a clean one, `meta: N finding(s) · fix` on one with cached findings. Returned fully formed with `resolveProvider: false`, so a document costs one request and no lens costs a request — and `workspace/codeLens/refresh` after an analysis updates the titles. `meta-core::scope::blocks` is the converse of `resolve`: it enumerates what `resolve` can find, reusing the same structural rules, and skips a declaration longer than the cap rather than offering an affordance the server would refuse. The commands are `meta.plugin.`-namespaced and handled in the plugin, because opening a buffer is a client decision (`window/showDocument` needs a URI an explanation does not have); `vim.lsp.codelens.run` is wrapped to dispatch them, everything else passes through. Verified: lenses arrive and are stored for a fixture with two declarations, running one opens the artifact it promised. Found on the way: Neovim's lens provider asserts on a client that has gone (`lsp/codelens.lua:143`) when a stop lands inside its 200 ms debounce, and `enable(false)` does not purge the stored id — unreachable from a plugin, so the harness lets the debounce settle before stopping, and it is recorded as Neovim's. |
| 2026-09-18 | **Streaming: the answer is written as it arrives.** `meta.explain` now streams. The model client speaks SSE (`chat_stream`, with the whole-answer path as the default for a backend that cannot stream, and a fallback when a server ignores `stream: true` rather than reporting an empty answer); the engine threads a delta callback; the server reports the text *so far* as `$/progress` partial results under the token the client already issued (§3.5.1, N12) and reports `waiting for the model (Ns)` during the prefill, because staying silent is indistinguishable from hanging. Measured against the local model: 49 progressive states from 1 B to 1487 B, done in 10.7 s, with the first 4.1 s covered by heartbeats. No custom methods — N6 holds. Two bugs the tests caught: a stream buffer leaked when the request failed, and naming a buffer then renaming it leaves a stub buffer behind (Neovim behaviour, reproduced in isolation), so the stream buffer is now named once, at the end. |
| 2026-09-18 | **Two defects that only a real config could show.** Pressing the action key in a config with snacks.nvim: `meta: the picker failed: snacks/picker/format.lua:350: attempt to index local 'ctx' (a nil value)`. The picker passed `kind = 'codeaction'` to `vim.ui.select`; snacks branches on that and expects Neovim's `{action, ctx}` pair, while our items carry `{action}` — so the picker died before it drew. Removed: `format_item` is the documented contract and is enough. Then `meta.plan` passed its arguments as a map where `ExecuteCommandParams.arguments` is an array, so the transport rejected it before the command ran (`invalid type: map, expected a sequence`) and every `:Meta plan` failed. Both pinned: one check drives a chooser that branches on `kind` the way snacks does, another asserts every command entry point sends an array. Both proven to fail with the bug reintroduced. |
| 2026-09-18 | **Installed into a real config and verified there.** The answer to "it doesn't work as is" was that it was not installed at all: that session had zero LSP clients and no config reference to meta, and it had been running five days. Loaded live it worked, and after one spec block in `~/.config/nvim/lua/plugins/init.lua` a fresh Neovim with that config reports `clients: meta,pylsp` and lands the finding. Two integration decisions the surrounding config forced: the keymap prefix moved to `<leader>M` (`<leader>ma` is Telescope marks, `<leader>mb` is make) and inline completion is off (llama.vim owns ghost text). Also verified: the server exits when its client is killed, so a dead Neovim leaves no processes behind. |
| 2026-09-18 | **Testing against a real configuration, as asked, found three more defects.** (1) The visual config's own fixture was opened from `VimEnter`, where Neovim suppresses autocmd triggering — the file loaded and the attach pass never saw it, so the config that exists for trying the thing reported "no client attached". Deferred with `vim.schedule`; verified interactively. (2) `meta.review` was not in the server's command list, so `:Meta review` and `<leader>mr` answered "not implemented" on every press. Wired to the same engine call the save path makes, returning findings in the Result. (3) The visual config's banner checked for the client once at 1.5 s and reported failure for a client that had simply not finished starting. It polls now. The `/tmp`-rooted workspace was also ruled out as a cause by direct test (1 finding, 5 s). |
| 2026-09-18 | **Visual-testing the real thing found a startup race.** A buffer saved before the client's first `workspace/configuration` reply was analysed against the built-in endpoint, so a client that configured one watched the server call somewhere else and get nothing back — the first thing a user would have hit, and invisible to every harness because they all sleep before acting. Model work now waits for the settings (`verify/config_race_test.py`, proven to fail with the gate removed). Also fixed: **every model call blocked a tokio worker thread** — the synchronous HTTP client called directly from `async fn`s, including inline completion at a 200 ms cadence. And `:LspLog` now states which endpoints are in force. |
| 2026-09-18 | **Found the defect that every test missed.** `diagnosticProvider.workspaceDiagnostics` had been advertised since the first capability block while `workspace/diagnostic` was never implemented; Neovim prefers the workspace branch on refresh when that flag is set, so the client stopped pulling per-document diagnostics altogether and findings never reached the sign column. Every harness passed because the Python ones pull by hand and the Neovim ones assert a *code action* resolves, which reads the server's cache rather than the buffer. Found by writing a dismissal test, which needs a displayed diagnostic to dismiss. Fixed by not advertising what is not served; `verify/smoke.py` now asserts the sub-capability too, and `verify/dismiss_test.lua` (9/9) covers dismissal end to end. |
| 2026-09-18 | **Local-model soak, on the user's go-ahead.** `qwen3.6-35b-a3b-iq3xxs` on the local llama.cpp server: **6/6 applied, 0 unparseable** across Python, Rust and TypeScript — more reliable than the cloud model on the same fixtures, and slower (ambient 5.7–12.4 s, resolve 7.3–15.5 s). Measured while running: `chat_template_kwargs.enable_thinking=false` *is* honoured locally and is *ignored* by the auth gateway, so a bare request to the local server spent 47 s producing 16 tokens of reasoning and no answer, while the server's own request answered in 0.9 s. |
| 2026-09-18 | **Soak across languages, and three fixes it forced.** `verify/soak.py` drives Python, Rust, TypeScript, Go, Markdown and an unidentifiable config file through the whole loop against a real model, asking the one question no unit test can: does the file still parse. It found that an answer the server cannot apply was never re-prompted (docs/MODEL.md §5 had specified it), that a 30 s resolve timeout turned a 66 s reasoning answer into a transport error, and that one repair attempt is not enough for a model that repeats itself. After widening repair to two attempts, covering applicability failures, and raising the ceiling and timeout together: **8/9 applied, 0 unparseable**, against 2/6 and 8/12 on the same fixtures. The one remaining failure is the re-emission guard refusing rather than corrupting. |
| 2026-09-18 | **Interactive verification of inline completion.** In a real PTY, Neovim fires the insert-mode events headless withholds, asks at the cursor, and renders the ghost text — while a headless run fires zero of those events. Driving the real client also exposed a defect the headless tests had missed: `require('meta').setup({settings = {inline_completion = …}})` placed the section at the top level while the server reads it by the name `meta`, so the setting was silently dropped and every completion was refused with "inline completion is off". `configure()` now accepts either shape. |
| 2026-09-18 | **U7 built after all.** The blocker was `lsp-types` 0.94 having no `inlineCompletionProvider` field, not the feature being hard: the field is now injected into the `initialize` response at the transport boundary, `textDocument/inlineCompletion` is served as a custom method, and the whole gate stack (off by default, prefix floor for timed requests only, its own per-minute window, a position-keyed answer cache) is verified 14/14. Found and fixed a second real defect on the way: `workspace/didChangeConfiguration` was unimplemented, so the plugin's kill switch could never have taken effect. |
