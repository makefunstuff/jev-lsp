# Draft: human-readable rule authoring (jev.rules/1)

**Status:** **LOCKED — A (YAML)**, and implemented 2026-09-21  
**Date:** 2026-09-21  
**Repo:** makefunstuff/jev-lsp  
**Ask:** replace the pile of nested JSON rule objects with one (or few) human-readable source document(s), while keeping runtime semantics identical to `jev.rules/1`.

---

## 0. Revision note (this supersedes the first draft)

The first version of this draft recommended **B (Markdown pack)** in §4. That was corrected, in the
same session, to **A (YAML)**, and the correction is recorded here rather than folded in silently:
the whiplash was real, the earlier recommendation was wrong, and *why* it was wrong is the useful
part. B looked attractive because a rule pack "reads as a policy document", and the cost only
shows up at implementation: an MD source needs a compiler, a heading/`id` convention that has to
be enforced, prose that has to be split from fences, and a second grammar between the author and
the schema — every bit of it to produce the JSON the loader already reads.

A (YAML) needs **no new component at all**. The loader reads `.yaml` beside `.json`, parses by
extension into the same `RuleFile`, and everything downstream — the merge, the hash, the lints,
the findings — is unchanged. So:

- **A is locked and shipped.** `.jev/rules/*.yaml` and `*.yml` load next to `*.json`; the runtime
  is still the `jev.rules/1` loader path, unchanged. `jev rules compile` emits the JSON.
- **B and C are rejected: no Markdown source.** No MD-as-source, no table convention, no compiler
  for a pack document. The reasons are in §3 and §4; this draft keeps the sketches as the record
  of what was weighed, not as a plan.
- **Crtique's review aligned with A** (§8). The MD direction had no advocate left once the
  compiler cost was priced.

---

## 1. Problem

Today each rule is a JSON object in `.jev/rules/*.json` (or the shipped `default_rules/**/*.json`) shaped like:

```json
{
  "schema": "jev.rules/1",
  "rules": [
    {
      "id": "no-unwrap-outside-tests",
      "title": "Unwrap outside tests",
      "text": "…long prose…",
      "severity": "warning",
      "applies_to": ["**/crates/**/*.rs"],
      "inspection": { "kind": "regex", "pattern": "\\.unwrap\\(\\)" },
      "judgement": {
        "question": "…",
        "criteria": { "true": "…", "false": "…" },
        "min_probability": 0.85
      },
      "verb_hint": "fix"
    }
  ]
}
```

What hurts when authoring:

1. **Noise** — braces, commas, Unicode escapes (`\u2014`), escaping every backslash in regexes twice.
2. **Fragmentation** — many small files; hard to skim the *set* of rules as one policy document.
3. **Prose buried in strings** — `text`, `question`, and criteria are the parts humans actually edit; JSON is the worst host for multi-paragraph prose.
4. **Same schema, two audiences** — machines want stable fields; authors want readable narratives.

Runtime (`jev.rules/1`) is fine. The pain is the **authoring surface**.

---

## 2. Goals / non-goals

### Goals

- One primary **human-readable** document (or a small, intentional set) that describes the whole rule pack.
- Round-trip (or one-way compile) to **`jev.rules/1` JSON** so the server does not change semantics.
- Keep field meaning from GUIDE §4: `applies_to`, `inspection`, `judgement.*`, `title`/`text`, `verb_hint`, floors.
- Diff-friendly in git; easy to review in a PR.
- `jev rules` tooling path: validate → emit JSON (or load authoring format directly later).

### Non-goals (this draft)

- Changing decide wire / criteria shapes.
- A full programming language for rules.
- Auto-generating good questions from titles alone.
- Dropping JSON forever (JSON remains the interchange / ship format unless we prove otherwise).

---

## 3. Options

### A. YAML (`jev.rules.yaml`) — same tree, less punctuation

**Chosen and shipped (2026-09-21).** The loader reads `.yaml`/`.yml` beside `.json`; there is no
compile step in the path, and `jev rules compile` exists only for interchange.

```yaml
schema: jev.rules/1
rules:
  - id: no-unwrap-outside-tests
    title: Unwrap outside tests
    severity: warning
    applies_to: ["**/crates/**/*.rs"]
    verb_hint: fix
    text: |
      A `.unwrap()` outside test code is a defect: the value was not checked,
      and a failed check takes the process down instead of the request —
      handle the error, or say in a comment why it cannot fail.
    inspection:
      kind: regex
      pattern: '\.unwrap\(\)'
    judgement:
      question: >
        Is this unwrap on a path that ships and can reach a value it did not
        check, rather than inside a test module, a doc comment, or behind an
        invariant the surrounding code states?
      criteria:
        "true": code that ships can reach it and the value is not guaranteed
        "false": it is in a test module, quoted in a doc comment, or behind an invariant the surrounding lines state
      min_probability: 0.85
```

**Pros:** maps 1:1 to schema; trivial to implement; regexes need less escaping than JSON.  
**Cons:** still a “pile of objects”; YAML footguns (`yes`/`no`, indentation); not a *document*.

**Outcome:** shipped, with the cons answered rather than waved away. "Still a pile of objects" is
true and is equally true of the JSON it replaces — A is a *readability* change, one level, not a
restructuring; the restructuring is the part that would have needed a compiler. The footguns are
handled where they bite: `criteria` keys are quoted, because an unquoted `true:` is a YAML
boolean where the wire wants the string `"true"`, and `docs/GUIDE.md` §4 states the two
conventions a YAML author has to know. The "not a document" objection is the B argument, and §4
is where it fails.

### B. Markdown pack — one document, fenced machine blocks

**Rejected (MD-as-source).** Kept below as the record of what was weighed.

Author `RULES.md` (or `.jev/RULES.md`) as prose + structured sections. Each rule is a heading; machine fields live in a small YAML fence (or definition list). Long `text` / criteria stay as Markdown paragraphs.

Sketch:

```markdown
# Repository rules

Schema: `jev.rules/1`  
Source: compiled to `.jev/rules/*.json` (or a single `rules.generated.json`).

## no-unwrap-outside-tests

**Title:** Unwrap outside tests  
**Severity:** warning · **Verb:** fix · **Applies:** `**/crates/**/*.rs`

A `.unwrap()` outside test code is a defect: the value was not checked, and a
failed check takes the process down instead of the request — handle the error,
or say in a comment why it cannot fail.

```yaml
inspection:
  kind: regex
  pattern: '\.unwrap\(\)'
judgement:
  question: >
    Is this unwrap on a path that ships and can reach a value it did not check,
    rather than inside a test module, a doc comment, or behind an invariant the
    surrounding code states?
  criteria:
    "true": code that ships can reach it and the value is not guaranteed
    "false": it is in a test module, quoted in a doc comment, or behind an invariant
  min_probability: 0.85
```
```

**Pros:** the pack reads as a policy doc; PR review matches how people think; prose is first-class; still compiles to exact JSON.  
**Cons:** needs a small compiler + golden tests; heading/`id` conventions must be strict.

**Rejected.** The cons are not incidental, they *are* the design: a compiler, a heading/`id`
convention to enforce, a fence grammar to parse, and a second place for the schema to drift — all
of it to emit a document the loader already reads. The pro ("reads as a policy doc") is bought
back for free by A: `.yaml` prose is not escaped, and the rule still sits beside the code it is
about.

### C. Markdown-only with convention (no YAML fence)

**Rejected.** Fragile parsing was the con; the reason it is fatal is in the `## 3.C` sketch below —
a table is the wrong host for multi-line criteria, and a regex with a `|` or a backtick in it
breaks the cell it sits in. Nothing here is worth a parser.

Machine fields as a tight definition list under each `## id`:

```markdown
## no-unwrap-outside-tests

| field | value |
|---|---|
| title | Unwrap outside tests |
| severity | warning |
| applies_to | `**/crates/**/*.rs` |
| inspection | regex: `\.unwrap\(\)` |
| min_probability | 0.85 |
| verb_hint | fix |

**Question.** Is this unwrap on a path that ships…?

**When true.** code that ships can reach it…  
**When false.** it is in a test module…
```

**Pros:** zero nested structure in the file.  
**Cons:** fragile parsing; tables are awkward for multi-line criteria; worse than B for regex-heavy rules.

### D. Keep many files, switch extension only (`.yaml` per rule)

Smallest change; does not solve “pile” or “single document.” Reject as the primary answer; maybe allow as an *also*.

**Outcome:** this *is* what shipped, and the "primary answer" it was rejected as was B — which is
now rejected too. What A gets over the first draft's reading of D is that it is not an *also*: it
is the authoring surface, JSON is demoted to interchange, and the loader accepts both so no
migration or compile step stands between the two.

---

## 4. Decision (locked)

**A. The loader reads `.yaml` and `.yml` in `.jev/rules/` beside `.json`, and the runtime stays
exactly the `jev.rules/1` loader path.** No compile step in the path, no second format at run
time, no markdown.

| Layer | Format | Role |
|---|---|---|
| Authoring | `.jev/rules/*.yaml`, `*.yml` | what a person writes and reviews |
| Runtime | the same loader, the same `RuleFile` | nothing about the pass changes |
| Also read | `.jev/rules/*.json` | unchanged, still loads, still first-class |
| Interchange | `jev rules compile <file> [-o <file>]` | one file as the JSON document |

Why this and not B, in one line each:

- **A needs no new component.** B needs a compiler, a fence grammar, a heading/`id` convention,
  and golden tests for the compiler itself. A needs `serde_yaml` and an extension check.
- **The JSON is not going anywhere.** It is the shipped set's embedded format and the thing
  `rules::hash_of` is taken over; B's whole value was producing JSON, so B pays a compiler to
  reach a format the repository already has.
- **Two spellings, one rule.** Because both parse into the same struct, there is nothing to keep
  in sync: `cargo test -p jev-core yaml_and_json` is a golden over a real rule, not a compile
  gate. A repository that prefers JSON keeps it, file by file.

Delivery (all landed 2026-09-21):

1. **Loader**: `rules::load_dir` reads `json|yaml|yml` by extension; `Format` picks the parser;
   everything downstream is untouched. Unit tests cover the YAML twin of the JSON fixture, a
   broken YAML file skipped with its line, a wrong schema, a mixed directory, a multi-rule YAML
   file, and a non-rule file left alone.
2. **Golden**: `docs/research/examples/no-unwrap-outside-tests.yaml` is `.jev/rules/no-unwrap-outside-tests.json`
   in the other spelling, asserted field by field (`yaml_and_json_are_the_same_rule`).
3. **`jev rules compile`**: one file in, the `jev.rules/1` document out; validates with the
   loader's own parse and writes nothing on a refusal.
4. **Docs**: GUIDE §4 leads with YAML and keeps the field table normative for both spellings;
   PROTOCOL §9 names both extensions and §11 documents `compile`.

---

## 5. Open questions (answered)

1. **One file vs many?** Many, as before: one file holds one or many rules, and `.jev/rules/` is
   a flat directory read in path order. A pack document is what B wanted; it is not needed.
2. **Where does generated JSON live?** Nowhere by default. Nothing generates; `jev rules compile`
   writes only where a user names `-o`, and a repository that prefers JSON simply writes JSON.
3. **Bidirectional edit?** Moot. Both spellings are inputs, so there is no canonical side to
   drift from — the loader reads whichever file is there.
4. **A schema id for the authoring format?** No second id. The file carries `schema: jev.rules/1`
   whichever spelling it uses, because it *is* a `jev.rules/1` document.
5. **Regex quoting.** Settled and documented: single-quoted YAML strings (`'\\.unwrap\\(\\)'`),
   where a backslash is a backslash. JSON keeps its own double escaping.
6. **Does Crtique / Research prefer A over B?** Yes — A (§0, §8).

---

## 6. Acceptance (met)

- [x] Golden: the example YAML equals the real `.jev/rules/no-unwrap-outside-tests.json` rule,
      field by field (`cargo test -p jev-core yaml_and_json`).
- [x] An invalid YAML file is skipped with a line-oriented reason, and the rest still load.
- [x] `cargo test -p jev-core`, `cargo test -p jev`, `cargo build -p jev-core -p jev` green.
- [x] GUIDE §4 documents the authoring path; the field table is normative for both spellings and
      the runtime is unchanged.
- [x] No rule text, floor or criteria changed — format only.

---

## 7. Worked example (shipped)

`docs/research/examples/no-unwrap-outside-tests.yaml` is the authoring fragment, and it is the
same rule as `.jev/rules/no-unwrap-outside-tests.json` — same `id`, `title`, `applies_to`,
pattern, question, criteria, `min_probability: 0.85` and `verb_hint`. Nothing was reworded to make
the spellings match: the YAML uses a folded scalar for the prose and a single-quoted scalar for
the regex, and the parse is byte-identical where the fields are strings.

---

## 8. Asks

| Role | Ask | Outcome |
|---|---|---|
| **Researcher** | Prior art (Semgrep YAML, ESLint MD, dbt YAML, Cue/Starlark policy packs). Risks of MD-as-source. Recommend A vs B with evidence. | **A.** The comparable tools that put rules in front of people who write them (Semgrep, dbt, ESLint's flat config) use YAML or data, not a prose document with embedded fences; MD-as-source is what lint *output* and rule *documentation* look like, not rule *input*. |
| **Crtique** | Attack this draft: ambiguity, parse hazards, “one document” myths, whether JSON pile is actually fine. | **Aligned with A.** The draft's own first recommendation (B) did not survive the attack: "one document" buys a compiler and a second grammar, and the JSON-pile complaint is answered by punctuation rather than by structure. |
| **Documentation Manager** | Naming (`RULES.md` vs `POLICY.md`), GUIDE placement, migration wording. | Moot: there is no pack document to name. GUIDE §4 keeps the field table and leads with the YAML example; PROTOCOL §9 names both extensions. |
| **Designer** | Skim readability of the MD sketch vs YAML — anything that reduces cognitive load. | The YAML sketch won on the thing that matters: one file, one rule, no fence to open before the machine fields start. |
| **QA** | When an implementation PR lands: golden compile, GUIDE links, no runtime behavior change on fixtures. | Landed as a **golden load** rather than a golden compile: the example YAML and the real JSON rule are asserted equal field by field, and the loader tests cover mixed, broken and multi-rule directories. |
| **Project Manager** | Sequence: draft → decide format → compile tool → migrate builtins. | Sequence shortened: draft → decide (A) → loader + golden → `compile` as interchange → docs. **No migration**: the builtins stay JSON and the JSON files stay where they are. |

---

*End of draft. Implementation landed 2026-09-21: `crates/jev-core/src/rules.rs` (`Format`,
`load_dir`, `compile`), `docs/research/examples/no-unwrap-outside-tests.yaml`, GUIDE §4 and
PROTOCOL §9/§11. Nothing about the runtime's semantics moved.*
