# The prose defaults

Four rules for the documents a repository writes, shipped so that a repository with no rules of
its own gets a pass worth reading on the first save. One JSON file per rule, `jev.rules/1`, the
shape `docs/GUIDE.md` §4 documents.

The bar for this directory is precision, not coverage. A default that fires wrongly is worse than
a default that does not exist, so every rule here is a measurement: the candidates it finds on a
corpus, the answer the hosted decide tier gives each candidate, and a floor set in the gap
between the answers for the violations and the answers for everything else. What follows is that
measurement, the classes that failed it, and the places where a reader should still disagree.

## The rules

| file | catches | `applies_to` | floor | measured band on real documents |
|---|---|---|---|---|
| `promotional-vocabulary.json` | an advertising adjective, or `Simply run …` in front of a step, where a fact belongs | `**/*.md` | `0.87` | violations `0.89–0.92`, everything else `0.13–0.86` |
| `lists-end-in-etc.json` | an enumeration closed with `etc.`, `and so on` or `and more` | `**/*.md` | `0.25` | violations `0.45–0.87`, quotations `0.03–0.04` |
| `appeals-to-the-obvious.json` | `Of course`, `Obviously`, `It is worth noting` in place of the fact | `**/*.md` | `0.75` | violations `0.92`, copied prose `0.56–0.59` |
| `assistant-voice-in-a-document.json` | a chat answer's frame opening a line in a document that is not a chat | `**/*.md` | `0.50` | a pasted answer `0.92–0.93`, sample model output `0.09` |

Each rule is one `regex` candidate finder plus one `noul` question, so one decision call carries
every candidate in a document. A document with no candidate costs nothing, and a rule with no
candidate is not in the state at all.

## The floors, and the runs behind them

**Method.** The corpus is 91 markdown documents under `/tmp/prose-measure` (1.72 MB, 30,697
lines) in three groups: 55 engineer-voice documents from this repository and ten others under
`~/Work`; 20 documents in the same author's product and wiki voice; 16 documents copied in from
elsewhere (a paper, an Evidently blog, tool catalogues, vendored library READMEs). Candidates
were counted through the shipped engine, one rule in `.jev/rules/` at a time:
`JEV_DECIDE_BASE_URL=http://127.0.0.1:8113/v1 target/release/jev inspect --force <file>`, where
port 8113 was a logging proxy forwarding `/v1/systemone` to `https://opencode.ai/zen/v1` with the
key in `~/.omp/agent/opencode.key`. The stub cannot answer this question: a floor is a claim about
the model's answers, and a stub that answers a constant has none. The proxy logged every request
and response, so each candidate's probability was read from the response itself: `answers.<rule
id>#<line>.noul`. Fifteen runs per candidate where the two bands were close, fewer where the gap
was wide. Cost: **$0.0500**, at the rates `docs/MODEL.md` records ($0.10/M in, $0.40/M out); Zen
returns `usage` and no cost field, so this is derived from tokens rather than read from a receipt.

| file | floor | clears the non-violation band by | clears the violation band by | runs |
|---|---|---|---|---|
| `promotional-vocabulary.json` | `0.87` | 0.01 | 0.02 | 60 |
| `lists-end-in-etc.json` | `0.25` | 0.21 | 0.20 | 63 |
| `appeals-to-the-obvious.json` | `0.75` | 0.16 | 0.17 | 12 |
| `assistant-voice-in-a-document.json` | `0.50` | 0.41 | 0.42 | 6 |

Every floor sits inside a measured gap rather than at the median of a straddle. Where the gap was
wide the floor sits below its midpoint, the choice `docs/GUIDE.md` §4 records for
`no-unwrap-outside-tests` (`0.85` in a `0.79–0.97` gap), because a floor that is too high costs a
missed defect.

## What was dropped, and what killed it

Each of these was measured on the same corpus and answered by the same tier, then left out. The
candidate counts are the finders' output over the 91 documents.

| class | candidates | what killed it |
|---|---|---|
| unmeasured performance and status claims | 144 over 43 documents | 4 judged true. `fast path` and `local-fast` are names, `fast end-to-end pre-flight` is a filename, and every speed claim in the corpus already carries its number (`10x`, `12×`, `~100×`, `591 ms`). Two candidates were honesty a rule must not punish: `must not be treated as production-ready` (a safety disclaimer) and `this architecture proved to not scale well` (a post-mortem). |
| contrast constructions | 20, or 1 in 5,756 documents once narrowed to the strawman shapes | Every candidate matching `not a X but Y` states a fact that distinguishes two things (`It's not a single metric but a flexible technique`), the form `docs/STYLE.md` allows. The strawman shapes have one instance on this machine, `returns not just the answer but a reasoning trace`, which is additive. |
| em-dash asides | 121 over 24 documents | It fires on the document that declares the register: `docs/STYLE.md:10` is the register's own illustration of the allowance for a short definition between two dashes. The line cannot separate a definition from an aside, 22 candidates are markdown table cells, and the remaining 99 are a typographic preference a default has no standing to impose. |
| the vocabulary of work not done | 40 over 21 documents | Nearly all accurate: `#### Planned Additions`, `Scaffold ✅` as a completed milestone, `placeholder` naming a real UI state, `MVP` naming a project phase. The class fires on honest scope notes. |
| hedging terms (`various`, `several`, `in order to`) | 56 over 22 documents | `recognize several environment variables`, `an anchor that occurs zero or several times`, `In order to create navigation data from the client's map files` are facts. `in order to` alone had more than 30 candidates, every one ordinary technical prose. |
| rhetorical questions in headings | 15 over 6 documents | The candidates are `### Need help?` and `### Do I need to register in StackOverflow?`: a question the reader is about to get an answer to is the genre's normal form in a support page. |
| non-parallel lists | 26 over 13 documents for the closest expressible shape | A property of a whole list, not of a line, so the classifier cannot decide it from a line and a file head. |

Four words left the vocabulary in `promotional-vocabulary.json` for cause, each with a measured
candidate set. `robust`: 31 candidates over 16 documents, of which 21 name a method (`Robust
z-score`, `robust baselines`) or compare against a named alternative (`More robust than relying on
PM2's internal variables`), 7 are another project's text, and 3 are the author's own support for a
claim with nothing behind it. `unlock`: 26, of which 24 are a token-vesting event and 2 a
password-manager session. `leverage`: 14, 12 of them `no leverage, no margin` in trading documents
or another project's guide. The bare word `simply`: 31, of which the ones read by hand do factual
work (`simply the minimum of`, `simply prefixed at the start`). The `simply <verb>` branch stays,
because its two measured candidates are both violations.

## Soft spots

1. **The ownership clause in `promotional-vocabulary.json`.** Its `criteria.false` excludes
   another project's description cited here, and 34 of the rule's 39 candidates ride on that
   clause. Measured, it is honoured where the citation is visible in the line (a catalogue entry:
   `0.13–0.21`) and not honoured where the whole file is another project's (a copied README's own
   marketing: `0.82–0.86`). A README at a repository path is indistinguishable from the subject's
   own README, so the clause cannot be sharpened from the line and the file head. The gap this
   leaves is 0.03 wide, which is narrower than the 0.04 spread of the copied README that bounds
   it: a reader who vendors third-party READMEs into `docs/` should raise the floor to `0.95`,
   and a reader who edits this rule should re-measure rather than move the number.
2. **The volume of `lists-end-in-etc.json`.** 49 candidates over 21 of the 91 documents, up to 7
   in one, against a visible-noise cap of 5 (`settings.noise.max_visible_findings`). Every false
   in the set was a citation of another project's text, and its question had no floor that
   separated the two: with the ownership clause in force the bands overlap (`0.45–0.57` and
   `0.76–0.85` for violations against `0.83–0.87` for a copied README), so the clause was removed
   and the rule made a statement about the line. The price is scope: a copied file in the tree is
   read the same way as the repository's own, which is why the rule's own text says so.
3. **The true band of `assistant-voice-in-a-document.json` is constructed.** The class has no
   unquoted instance in 5,756 markdown documents on this machine: every occurrence of the
   phrasing is a quotation (a style guide forbidding it, an antipattern wiki, a chat-template
   README showing sample output). The `0.92–0.93` band comes from a nine-line probe written for
   the measurement, `probe/assistant-answer.md`, and the `0.09` from a real sample model output.
   The question separates the two, but a reader who wants one rule removed should remove this one:
   it costs one candidate in 91 documents and its positive side is unproven in the field.
4. **The floor of `0.25` is below the schema's default.** For a rule whose pattern was judged
   correct on all 49 candidates by hand, the floor's only job is to drop a line the model reads as
   a path, a code span or a quotation, and those answered `0.03–0.04`. The number is low on
   purpose, and it is the number to change first if this rule turns out noisy.

## Re-running the measurement

```sh
# the corpus, in three groups, under one git root so `applies_to` resolves
ls /tmp/prose-measure                       # 91 documents, 16 of them copied from elsewhere

# the hosted decide tier, through the logging proxy, with one rule loaded
python3 /tmp/prose-measure/logdecide.py &   # PROXY_PORT=8113, UPSTREAM=https://opencode.ai/zen
cp crates/jev-core/default_rules/prose/<one rule>.json /tmp/prose-measure/.jev/rules/
JEV_DECIDE_BASE_URL=http://127.0.0.1:8113/v1 target/release/jev inspect --force \
  /tmp/prose-measure/<document>.md
# every request and response is in /tmp/prose-measure/decisions.jsonl

# the candidate counts alone need no endpoint at all
python3 - <<'PY'
import json, re, pathlib
pat = json.load(open('crates/jev-core/default_rules/prose/<one rule>.json'))['rules'][0]['inspection']['pattern']
for p in pathlib.Path('/tmp/prose-measure').rglob('*.md'):
    n = sum(1 for line in p.read_text().split('\n') if re.search(pat, line))
    if n: print(n, p)
PY
```

A rule added here without a band behind it is a rule nobody measured, and the corpus is the
cheapest place to find that out: `lists-end-in-etc.json` looks obvious and had no floor until the
answers were read, and the em-dash rule looks like the house register and fires on the line that
states it.
