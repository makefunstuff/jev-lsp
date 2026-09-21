# Example authoring fragment (not loaded by the server)

This illustrates the recommended Markdown pack style from
`docs/research/rules-human-readable-format-draft.md`. It is **not** wired up yet.

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
    "false": it is in a test module, quoted in a doc comment, or behind an invariant the surrounding lines state
  min_probability: 0.85
```
