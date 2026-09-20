# The code defaults

Two rules for the code a repository writes, shipped so that a repository with no rules of its own
gets a pass worth reading on the first save. One JSON file per rule, `jev.rules/1`, the shape
`docs/GUIDE.md` §4 documents.

The bar for this directory is precision, not coverage. A default that fires wrongly is worse than
a default that does not exist, so every rule here is a measurement: the candidates it finds on a
corpus, the answer the decide tier gives each candidate, and a floor set outside the answers for
everything else. Nineteen further classes were measured the same way and left out; their counts
are below, because the numbers are what stop the next author from re-adding one.

## The rules

| file | catches | `applies_to` | floor | measured, violation band | measured, everything else |
|---|---|---|---|---|---|
| `no-blanket-except.json` | a catch-all handler that discards the failure with nothing beside it saying why the failure cannot matter | `**/*.py` | `0.8` | `0.91–0.96` | `0.05–0.45` |
| `no-suppression-without-a-reason.json` | `# type: ignore` and `@SuppressWarnings` with no reason beside them | `**/*.py`, `**/*.java` | `0.85` | `0.94–0.96` | `0.06–0.11` |

**`no-blanket-except.json`.** `except:` and `except Exception:` take every failure the block can
raise, including the programming mistakes nobody wrote a handler for, and a body that discards the
failure leaves the caller reading an ordinary result. The candidate finder is
``^\s*except\s*(Exception|BaseException)?\s*:\s*$|^\s*except\s*(Exception|BaseException)?\s*:\s*(pass|continue|break|return)\b``
and the question asks about the discard, never about the syntax the regex already matched:

> Does this catch-all discard the failure without saying why the failure cannot matter?

The clause that carries the precision is the one about saying why. Counted handlers that discard
and say nothing measured `0.91–0.96`; the same handler with `except Exception: pass  # Never block
on errors` measured `0.27–0.45`, a handler that logs or re-raises measured `0.39–0.60`, and one
that raises a replacement exception measured `0.05–0.16`. Before the clause was in the question,
the annotated handler measured `0.91–0.97` and published, which is the mistake this text exists to
prevent.

**`no-suppression-without-a-reason.json`.** A suppression silences one finding at one line, and
the line says why the fix is not available there, so the next reader can tell a deliberate
exception from a warning that was made to stop. The candidate finder is
``#\s*type:\s*ignore\b|@SuppressWarnings\b`` and the question is:

> Is this suppression unexplained: does nothing on the line or the line above say why the finding
> cannot be fixed here?

Two forms of the same class were dropped from the finder on measurement, not on taste. `#[allow(`
measured `0.85–0.95` across two sessions for a bare `#[allow(unused_mut)]` against `0.05–0.08` for
one carrying a reason, a band wide enough that it straddles any floor between those numbers, and
its multi-candidate state sat at `0.76–0.83`. `# noqa` measured `0.75–0.88` for a bare
`# noqa: E402`. Both were removed, which is why this rule is Python and Java and not Rust: a floor
that publishes them also publishes the flat band described below, and one that silences the flat
band silences them.

## The floors, and the runs behind them

**The corpus** is thirteen repositories on this machine, walked the way the engine reads them:
`meta-lsp`, `deepseek-harness` (`10,226` git-tracked files), `hardware-lab`, `kb-finetuning`,
`jev-vs-cactus`, `homelab` (`3,660`), `hey-cli`, `firmware-poc`, `vibe-trading`, `quant`,
`llama-cpp-turboquant` (`2,570`), `mvp-headmap-dashboard`, `mqube`. In a repository the scope is
git's own file set, tracked files plus untracked files git does not ignore, and a directory
holding its own `.git` is a repository of its own. That scoping is not a detail: on the first pass
over these corpora roughly nine in ten candidates came out of gitignored build trees
(`firmware/.embuild` toolchains, checked-in `lib/` output), which no pass inspects.

**Candidates** were counted with the stub, one rule in `.jev/rules/` at a time:

```sh
STUB_PORT=8097 python3 verify/stub_model.py &
cp crates/jev-core/default_rules/code/<one rule>.json /tmp/jev-measure/<repo>/.jev/rules/
JEV_DECIDE_BASE_URL=http://127.0.0.1:8097/v1 target/release/jev inspect --force <file>
```

**Bands** were measured against the live tier, `wire` `system_one`, model `jev-1.13`, through a
logging proxy that forwards `/v1/systemone` to `https://opencode.ai/zen/v1` with the key in
`~/.omp/agent/opencode.key` and writes every request and response to
`/tmp/jev-measure/decisions.jsonl`. Each probability below is read from the response itself,
`answers.<rule id>#<line>.noul`. The stub cannot answer these questions: a floor is a claim about
the model's answers, and a constant has none. Ten runs per case for the shipped wording, fifteen
for the bands that decide a floor.

| file | floor | clears the non-violation band by | clears the violation band by | runs |
|---|---|---|---|---|
| `no-blanket-except.json` | `0.8` | `0.35` | `0.11` | `100` |
| `no-suppression-without-a-reason.json` | `0.85` | `0.73` | `0.09` | `80` |

Both floor margins are computed against the shipped wording's bands, and both floors sit inside a
measured gap rather than at a median. Where a gap is wide the floor sits below its midpoint, the
choice `docs/GUIDE.md` §4 records for `no-unwrap-outside-tests` (`0.85` in a `0.79–0.97` gap),
because a floor that is too high costs a missed defect.

## The flat band, and why the floors sit above it

One decision call carries every candidate of a document, and on this tier a call carrying two or
more candidates of one rule is answered at one shared probability whatever the code says:

| state | candidates | 15 runs each |
|---|---|---|
| a handler that discards, and a handler that logs and re-raises, in one document | `2` | `0.70` for the discard, `0.69` for the re-raise |
| a bare suppression and a reasoned one in one document | `2` | `0.71` and `0.70` |
| a bare `#[allow]`, a reasoned one, a bare one | `3` | `0.80`, `0.80`, `0.80` |
| a bare `# type: ignore` alone | `1` | `0.94–0.96` |
| a reasoned `# type: ignore` alone | `1` | `0.06–0.11` |
| a handler that discards alone | `1` | `0.92–0.97` |
| a handler that raises a replacement alone | `1` | `0.05–0.06` |

Two control questions on the same wire separate what the answers above do not: `Is line 2 a
comment?` answered `0.99`, `Does line 3 open a socket?` answered `0.02`. The tier reads lines; it
stops reading them per candidate once one call carries several. That is a property of the tier
this machine wires *and* of this engine's batching, which asks one document's candidates in one
call, so any rule that fires more than once in a document meets it.

The consequence is the reason both floors sit where they do. On this batching the flat band for
these rules reaches `0.83`, so a floor under it publishes the band, and the band contains the
non-violations as well as the violations. A floor above it silences every multi-candidate document
on this tier but publishes the single-candidate ones the tier answers. The shipped floors take the
second option, and the price is named rather than hidden: on these corpora a document whose
candidates the tier pulls together publishes nothing.

**If the pass ever asks one candidate per call, or the tier separates candidates inside one call**
(the hosted tier does, per `docs/GUIDE.md` §4: false band `0.75–0.79`, true band `0.97–0.98`),
the gaps are `0.45–0.91` for `no-blanket-except.json` and `0.11–0.94` for
`no-suppression-without-a-reason.json`. A floor of `0.45` or `0.5` then publishes the whole
measured violation band and none of the rest. That is the sentence to read before lowering either
number: the floors here are set for the batching as it is now, and this paragraph is the
measurement that says so.

## What was dropped, and what killed it

Every class below was measured on the same corpus and answered by the same tier, then left out.
The counts are the finders' output over the corpora; the judgements are hand reads of the rows.

| class | candidates | what killed it |
|---|---|---|
| debug prints on shipping paths (`print(`, `console.log(`) | `275`, and `491` in the unbiased cost sample, `5.2` per document | `14` judged by hand, `0` true. Every one is the program's own output: a harness printing its results, a report writer, a probe. The candidate volume is also the largest here, and it would crowd the question budget of any document that prints. |
| commented-out code | `91` | `2` look like statements nothing runs. The rest are prose containing `=`, doc comments, JavaScript private fields (`#settled = false`, `#delegated = Symbol(...)`) and usage blocks. Rust's `regex` has no lookahead, so the private-field and prose shapes cannot be excluded from the finder. |
| empty handlers (`except E: pass`, `catch {}`) | `19` | `0` true. `catch {}` is JavaScript's idiom for ignoring a failure (`25` instances in one repository's client code), the Python single-line form is a filter idiom, and the remainder were agent-produced eval artifacts. |
| `TODO`/`FIXME`/`XXX`/`HACK` with no owner | `49` | `15` true. Whether a marker is tracked is a fact about the repository's convention (`TODO(e2b-pgid-identity)`, `TODO(L6)`, `TODO(v1)`) and it is not on the line. Firing on a repository whose markers are its tracking gives noise and nothing else. |
| temporary vocabulary (`temporary`, `for now`, `placeholder`, `do not remove`) | `43` | `0` of `30` true. In libstdc++ `temporary` names an object, in a terminal emulator `placeholder` names a Kitty protocol character, and `#[ignore = "…"]` explains a decision. |
| `time.sleep` in tests | `67` | `7` true. Poll intervals, deliberate fake delays, coalescing windows and timeout guards account for the rest. |
| an identifier built from a clock reading | `28` | `3` true, all in test scratch paths. |
| float equality | `7` | `0` true. `== 0.0` is deliberate exactness at a lossless-JSON boundary, and one candidate was inside a string literal. |
| `SELECT *` | `5` | `0` true. All five are a query builder's common table expressions. |
| a committed credential (`sk-`, `ghp_`, `AKIA`, PEM blocks) | `4` | `0` true. All four are fixtures (`sk-live-DO-NOT-LOG-abcdef123456`, `sk-e2efixture1234567890` twice). A committed private key block, the one shape nobody writes by accident, appears nowhere in the corpora, so the class has no measured positive here. |
| `raise NotImplementedError` on a shipping path | `2` | `0` true. Both are fixtures whose whole purpose is to be patched. |
| a mutable default argument (`def f(x=[])`) | `5` | `0` true. The three real ones rebind a copy before writing (`request_kwargs = {**request_kwargs}`) and answer false; the other two are the definition quoted inside this repository's own rule fixture. Narrowed twice and still all-false, so dropped rather than floored. |
| a parameterised query built from an f-string | `1` | true, and one candidate does not measure a rule. |
| SQL assembled by interpolation | `1` | `0` true: `"delete: %s does not exist" % url` is a status string, because SQL keywords are ordinary English words. |
| debug stops (`breakpoint()`, `pdb.set_trace()`, `dbg!(`) | `0` | Nothing to measure once the scope is git's file set. The only hits in a session were a TypeScript class field named `debugger` in an untracked build tree. |
| `eval`/`exec` of a string built at the line | `0` | Nothing in the tracked corpora. |
| `== None` and `== True` | `0` | Nothing in the tracked corpora. |
| `*args`/`**kwargs` pass-through | `0` | Nothing in the tracked corpora. |
| `os.system` and `shell=True` | `0` | Nothing in the tracked corpora; the whole workspace holds `15` and `9`, all in untracked trees. |

## Two classes that cannot be rules here

**Overengineering.** A single-use abstraction, a class whose body is one delegation, a
`Manager`/`Factory`/`Strategy`/`Wrapper`/`Helper`/`Util`/`Service` with no second implementation, a
config knob nobody reads. Every formulation needs a fact that is not on the line and is not in the
file head: how many call sites exist, how many implementations exist, whether anything reads the
setting. The classifier sees the file head and the candidate needles, so a rule of this shape is
answered wrongly by construction, and a wrongly answered rule costs a decision call and the
reader's trust.

**Reinventing the wheel.** A hand-rolled JSON parser, base64, an LRU cache, a UUID, a version
comparator. The fact that makes one of these a defect is that the standard library or a declared
dependency already ships it, which is a fact about the environment and not about the line. The
name-based finders (`*args` forwarding, an `os.system` call, a `shell=True` argument) either fire
on deliberate implementations or find nothing, and both outcomes are in the table above.

Both are findings about the product rather than gaps in this directory: a rule that counts
implementations needs a different state than a file head, and the place to record that is here.

## Soft spots

1. **`no-blanket-except.json` on a document the tier pulls together.** One file in this
   repository (`verify/quality_eval.py`, three candidates, one of them a handler quoted inside a
   triple-quoted test fixture) answered `0.79–0.87` for both violations and non-violations under
   the wording before the annotation clause, and `0.80–0.87` with it. At `0.8` the shipped wording
   can publish the quoted-fixture false positive there. The wordings that separated it lowered
   every other case into the same band (`0.73–0.95` against `0.16–0.86`), so the wording that
   separates was kept and this document is named instead.
2. **`no-suppression-without-a-reason.json` answers the class twice over.** `#[allow(...)]` and
   `# noqa` are out on the measured bands above, so the rule is Python and Java while the class it
   names is in every language with a linter. On a tier that separates candidates inside one call,
   the Rust band (`0.85–0.95` for a bare `#[allow(unused_mut)]`) sits under a `0.9` floor and
   those two forms could come back with a re-measurement.
3. **A lower floor is the alternative, and it is measured.** With one candidate per call the gaps
   are `0.45–0.91` and `0.11–0.94`, and floors of `0.45–0.5` publish the whole violation band.
   Anyone who lowers either shipped number should read the flat-band section first, because on
   today's batching a lower number publishes the pulled band, and the pulled band contains the
   non-violations.
4. **Every band here is session-dated, and the margins are 0.04 to 0.74 wide.** A bare
   `#[allow(unused_mut)]` measured `0.85–0.92` in one session and `0.93–0.95` in another; the
   annotated-discard handler measured `0.88–0.92` before the annotation clause and `0.74–0.86`
   with it. The narrow margins are the two that decide the floors.
5. **Two labels are my reading, not the rule's.** An annotated discard
   (`except Exception: pass  # Never block on errors`) counts as a non-violation because the
   annotation is what the question asks about, and a discard in a poll loop that returns a default
   counts as a violation. Change either reading and the floor moves with it: the first is worth
   `0.27–0.45` against `0.91–0.96`, the second sits at the top of the violation band.

## What the session spent, and what it did not re-measure

`896` calls reached the tier through the logging proxy, `893` were answered, `3` failed with
`502`, and the token total is `1,241,627`. At the rate `docs/MODEL.md` measures from the
decisions route's own `usage.cost` (`$0.0395` per million), that is **`$0.04904`**, derived from
tokens because Zen returns `usage` and no cost field.

Re-measured under the shipped wording: the six single-candidate `no-blanket-except` fixtures and
two real documents for the annotated shape (`vibe-trading/agent/src/agent/progress.py`,
`mvp-headmap-dashboard/.claude/hooks/remind_review.py`, both `0.27–0.45`) and one for the
unannotated shape (`vibe-trading/agent/src/agent/skills.py`, `0.91–0.93`); the four
`no-suppression-without-a-reason` fixtures at `10` runs each; and the candidate counts for both
shipped patterns over all thirteen corpora.

Measured under the wording before the annotation clause, and not re-measured after it:
`verify/outcome_test.py`, `verify/repo_bench.py`, `verify/quality_eval.py`, `verify/smoke.py`,
`one/converts.py`, `one/records.py`, `one/cleans_up.py`. Their numbers are quoted above with that
wording's name against them. The upstream then began answering **`402 Payment Required`**, which
is the reason the list of re-measured documents stops where it does, and why the floor for
`no-blanket-except.json` is the less measured of the two.

## Re-running the measurement

```sh
# the corpus: thirteen repositories, scoped to git's own file set per repository
# candidates, with no endpoint involved
STUB_PORT=8097 python3 verify/stub_model.py &
RULES_DIR=crates/jev-core/default_rules/code python3 /tmp/jev-measure/measure.py

# bands, against the live tier, through the logging proxy
PROXY_PORT=8099 python3 /tmp/jev-measure/proxy.py &
python3 /tmp/jev-measure/bands.py 15        # single-candidate fixtures, both rules
python3 /tmp/jev-measure/regimeb.py 15      # one document's candidates in one call
python3 /tmp/jev-measure/floors_single.py 5 # the shipped pass, one candidate per document
# every request and response lands in /tmp/jev-measure/decisions.jsonl
```

A rule added here without a band behind it is a rule nobody measured. The corpus says so quickly:
`no-untracked-todo` reads as the safest rule in the list and its candidates are one repository's
tracking convention, and `no-debug-print-left-in` reads as the plainest and every candidate it
found was the program's own output.
