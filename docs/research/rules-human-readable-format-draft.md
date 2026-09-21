# Draft: human-readable rule authoring (jev.rules/1)

**Status:** draft for review — not implemented  
**Date:** 2026-09-21  
**Repo:** makefunstuff/jev-lsp  
**Ask:** replace the pile of nested JSON rule objects with one (or few) human-readable source document(s), while keeping runtime semantics identical to `jev.rules/1`.

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

### B. Markdown pack (recommended direction) — one document, fenced machine blocks

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

### C. Markdown-only with convention (no YAML fence)

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

---

## 4. Recommendation

**Ship B (Markdown pack → compile to `jev.rules/1`)**, with YAML fences for the mechanical bits only.

| Layer | Format | Role |
|---|---|---|
| Authoring | `.jev/RULES.md` (repo) / packaged MD for builtins | human source of truth |
| Interchange / runtime | `jev.rules/1` JSON (generated or still loadable) | server today, no semantic fork |
| Optional | YAML import | `jev rules import` for people who prefer A |

Phased delivery:

1. **Spec + examples** (this draft) + golden: MD fixture → exact JSON of an existing rule (`no-unwrap-outside-tests`).
2. **`jev rules compile`** (or `build`) reading `.jev/RULES.md` → write `.jev/rules/_generated.json` (or split files).
3. **Loader** keeps reading JSON; later optionally read MD directly if compile-on-load is desired.
4. **Migrate** a subset of `.jev/rules/*.json` into the pack; delete duplicates once compile is in CI.
5. **Docs:** GUIDE §4 points authors at the MD pack; JSON remains documented as the wire schema.

---

## 5. Open questions (for reviewers)

1. **One file vs many MD files?** One `RULES.md` vs `.jev/rules/*.md` compiled together. Lean: one pack file for repo policy; builtins may stay multi-file MD under `default_rules/`.
2. **Where does generated JSON live?** Commit generated output (reproducible CI) vs gitignore + compile in `jev` startup (slower, always fresh).
3. **Bidirectional edit?** JSON → MD is nice for migration once; ongoing edits MD-only avoids drift.
4. **Schema id for authoring?** e.g. `jev.rules.md/1` header so the compiler can evolve without guessing.
5. **Regex quoting rules** in MD/YAML — document a single style (single-quoted YAML strings).
6. **Does Crtique / Research prefer pure YAML (A) for toolability over document feel (B)?**

---

## 6. Acceptance for an implementation PR (later)

- [ ] Golden test: compile example MD → byte-equivalent / semantically equal `jev.rules/1` for at least one real rule.
- [ ] Invalid MD fails with a line-oriented error (not silent skip).
- [ ] Existing `verify/rules_test.py` still green against compiled JSON.
- [ ] GUIDE §4 documents the authoring path; JSON schema still normative for runtime.
- [ ] No slur / policy text changes — format only.

---

## 7. Worked mini-example (target compile output)

Authoring fragment for `no-unwrap-outside-tests` should compile to the same object currently in `.jev/rules/no-unwrap-outside-tests.json` (see that file). Keep `min_probability: 0.85` and the defect-phrased question (GUIDE §4).

---

## 8. Asks

| Role | Ask |
|---|---|
| **Researcher** | Prior art (Semgrep YAML, ESLint MD, dbt YAML, Cue/Starlark policy packs). Risks of MD-as-source. Recommend A vs B with evidence. |
| **Crtique** | Attack this draft: ambiguity, parse hazards, “one document” myths, whether JSON pile is actually fine. |
| **Documentation Manager** | Naming (`RULES.md` vs `POLICY.md`), GUIDE placement, migration wording. |
| **Designer** | Skim readability of the MD sketch vs YAML — anything that reduces cognitive load. |
| **QA** | When an implementation PR lands: golden compile, GUIDE links, no runtime behavior change on fixtures. |
| **Project Manager** | Sequence: draft → decide format → compile tool → migrate builtins. |

---

*End of draft. Implementation is out of scope for this PR; discussion + decision first.*
