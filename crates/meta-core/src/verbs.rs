//! Prompts. Kept in one place so the response contract and the wording that demands it
//! cannot drift apart.

use crate::context::{render_block, Context};
use crate::types::{Output, Verb};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptSpec {
    pub system: String,
    pub user: String,
    pub json: bool,
    pub max_tokens: u32,
}

/// A concrete example, not a skeleton. A placeholder-filled schema invites a fast model to
/// echo the placeholders as values — observed against a real model, which returned
/// `"verb_hint"` where an object was expected.
const EDIT_SCHEMA: &str = r#"{
  "summary": "guard the file open",
  "rationale": "The handle is opened without a check and never closed, so a missing file raises and a successful read leaks.",
  "replacements": [
    {
      "anchor": { "kind": "statement", "match": "    f = open(path)" },
      "replacement": "    with open(path, encoding=\"utf-8\") as f:"
    }
  ],
  "new_files": []
}"#;

const FINDINGS_SCHEMA: &str = r#"{
  "findings": [
    {
      "anchor": { "kind": "statement", "match": "    f = open(path)" },
      "severity": "warning",
      "label": "file handle is never closed",
      "detail": "The handle is not managed by a context manager, so the descriptor leaks when the read raises.",
      "verb_hint": "fix"
    }
  ]
}"#;

// NOTE: a raw string ends at `"` followed by N hashes. Markdown headings written right
// after a quote (`"## Summary`) therefore terminate `r#"` and `r##"` early. Keep one `#`
// in this example, or raise the delimiter — this has bitten this file twice.
const ARTIFACT_SCHEMA: &str = r##"{
  "summary": "what this function does",
  "markdown": "# Summary\n\nReads the first item name from a JSON file.\n\n# Watch out\n\nThe open is unchecked."
}"##;

const EDIT_RULES: &str = "\
The examples above show the SHAPE of an answer. They are not content: never copy their \
text, never use a field name as a value, and never emit a placeholder.
Rules that are enforced mechanically, so a violation wastes the whole response:
- `match` MUST be copied verbatim from the CODE block and MUST occur exactly once in the file. \
If a shorter quote would be ambiguous, quote more surrounding text.
- `replacement` replaces the ENTIRE scope of that anchor, from its first line to its last. \
Return the complete new text for the scope, not a fragment and not a diff.
- If your change needs to touch more lines than the anchor names — a one-line `statement` \
whose fix restructures the block around it — do NOT put those extra lines in `replacement`. \
Anchor on the enclosing block instead, and rewrite that whole block. A replacement that \
repeats lines it did not consume is rejected, because applying it would duplicate them.
- Never emit line numbers or character offsets; the server computes them from `match`.
- Do not reformat, reorder, or otherwise touch code outside the scope.
- If no safe change is possible, return empty `replacements` and explain why in `rationale`.";

const FINDINGS_RULES: &str = "\
The example above shows the SHAPE of an answer. It is not content: never copy its text, \
never use a field name as a value, and never emit a placeholder.
Rules:
- Report only defects you can point at with an exact `match` from CODE.
- Prefer few, high-confidence findings over many speculative ones. Zero findings is a valid answer.
- Severity is `warning` for a real defect and `information` for a suggestion. Never `error`.
- `label` states the defect, not the fix.";

const ARTIFACT_RULES: &str = "\
The example above shows the SHAPE of an answer. It is not content: never copy its text, \
never use a field name as a value, and never emit a placeholder.
Rules:
- Write for a reader who has the code in front of them: be concrete and cite what you saw.
- Answer only from the material provided. If something is not visible, say so rather than guess.
- `markdown` is prose. It is never an edit, and nothing in it will be applied to the file.";

fn system(flavour: &str, verb: Verb) -> String {
    let (schema, rules) = match verb.output() {
        Output::Edit => (EDIT_SCHEMA, EDIT_RULES),
        Output::Findings => (FINDINGS_SCHEMA, FINDINGS_RULES),
        Output::Artifact => (ARTIFACT_SCHEMA, ARTIFACT_RULES),
    };
    let persona = if flavour == "generic_text" {
        "You are a careful reviewer working through an editor's language server. \
         The material below is not source in a language you can assume; treat it as text."
            .to_string()
    } else {
        format!(
            "You are a senior {flavour} engineer working through an editor's language server."
        )
    };
    format!(
        "{persona}\nAnswer with a single JSON object and nothing else — no prose, no code fences.\n\
         JSON schema:\n{schema}\n{rules}\n"
    )
}

fn task(verb: Verb) -> &'static str {
    match verb {
        Verb::Fix => "Fix the problem reported under KNOWN FINDINGS. Make the smallest change that removes it.",
        Verb::FixAll => "Fix every problem reported under KNOWN FINDINGS in this scope.",
        Verb::Harden => "Harden this scope against unhandled error paths, edge cases, and adversarial input. \
                         Behaviour on ordinary valid input must not change.",
        Verb::Types => "Add precise type annotations to this scope. Change no behaviour.",
        Verb::Docs => "Add or improve documentation for this scope. Change no executable code.",
        Verb::Rewrite => "Rewrite this scope for clarity and maintainability, preserving behaviour exactly.",
        Verb::Test => "Write tests for this scope. Return them as a new file whose path fits the \
                       project's existing test layout, and cover the interesting boundary cases.",
        Verb::Generate => "Implement what this scope's name and signature imply. It is currently a stub.",
        Verb::Explain => "Explain what this scope does, why it is shaped this way, and what a reader \
                          should watch out for. Be concrete and cite the code.",
        Verb::Review => "Review this scope for real defects. Report what a careful reviewer would block a \
                         change on.",
    }
}

const PLAN_SCHEMA: &str = r#"{
  "goal": "make the file load path fail loudly instead of silently",
  "steps": [
    {
      "title": "Fail when the file cannot be opened",
      "rationale": "The handle is opened with no check, so a missing file raises deep inside the caller.",
      "verb": "harden",
      "anchors": [ { "kind": "function", "match": "def load_first_name(path):" } ]
    },
    {
      "title": "Cover the missing-file case with a test",
      "rationale": "Nothing currently asserts the failure path.",
      "verb": "test",
      "anchors": [ { "kind": "function", "match": "def load_first_name(path):" } ]
    }
  ]
}"#;

const PLAN_RULES: &str = "\
The example above shows the SHAPE of an answer. It is not content: never copy its text, \
never use a field name as a value, and never emit a placeholder.
Rules:
- Produce the smallest sequence of steps that achieves the goal. Three focused steps beat eight vague ones.
- Each step names ONE verb from: fix, harden, types, docs, rewrite, test, generate.
- Each `match` MUST be copied verbatim from the CODE block and MUST occur exactly once in the file.
- A step is applied later, against the file as it is then, so do not describe line numbers or ordering
  dependencies that a small unrelated edit would break.
- Do not put edits in a plan. Titles and rationale only.";

/// Render the planning prompt. Not tied to a [`Verb`]: planning is a command, and its
/// output is an artifact, not an edit (PROTOCOL.md §4.1, §6).
pub fn render_plan(ctx: &Context, goal: &str) -> PromptSpec {
    let persona = if ctx.flavour == "generic_text" {
        "You are a careful engineer working through an editor's language server.".to_string()
    } else {
        format!("You are a senior {} engineer working through an editor's language server.", ctx.flavour)
    };
    let system = format!(
        "{persona}\nAnswer with a single JSON object and nothing else — no prose, no code fences.\n\
         JSON schema:\n{PLAN_SCHEMA}\n{PLAN_RULES}\n"
    );
    let user = format!("GOAL: {goal}\n\n{}\n", render_block(ctx));
    PromptSpec {
        system,
        user,
        json: true,
        max_tokens: 2048,
    }
}

const COMPLETION_RULES: &str = "\
Rules:
- Return only the text that belongs at the cursor. No prose, no code fences, no repetition
  of what is already there.
- If nothing sensible belongs there, return an empty string.";

/// Render a fill-in-the-middle completion prompt (docs/MODEL.md, tier `fim`).
///
/// The chat form rather than raw FIM tokens: a chat endpoint cannot take a raw continuation
/// prompt, and the instruction form works with any model. `fim_tokens`, when configured, are
/// still honoured for a server that expects them.
pub fn render_completion(
    flavour: &str,
    path: &str,
    prefix: &str,
    suffix: &str,
    fim_tokens: Option<&crate::config::FimTokens>,
) -> PromptSpec {
    let persona = if flavour == "generic_text" {
        "You are a code completion engine.".to_string()
    } else {
        format!("You are a {flavour} code completion engine.")
    };
    let user = match fim_tokens {
        Some(t) => format!(
            "{}{prefix}{}{suffix}{}",
            t.prefix, t.suffix, t.middle
        ),
        None => format!(
            "FILE: {path}\nLANGUAGE: {flavour}\n\nTEXT BEFORE THE CURSOR:\n{prefix}\n\n\
             TEXT AFTER THE CURSOR (do not repeat it):\n{suffix}\n\n\
             Write exactly what belongs at the cursor."
        ),
    };
    PromptSpec {
        system: format!("{persona}\n{COMPLETION_RULES}\n"),
        user,
        json: false,
        // Completing a line or two, not authoring a file.
        max_tokens: 128,
    }
}

pub fn render(verb: Verb, ctx: &Context) -> PromptSpec {
    let user = format!(
        "TASK: {}\n\n{}\n",
        task(verb),
        render_block(ctx)
    );
    let max_tokens = match verb.output() {
        // Reasoned answers need room for the reasoning as well as the answer: a thinking
        // model that exhausts this returns `finish_reason=length` with empty content.
        // Measured against a real reasoning model: 2048 was not enough, and 4096 was still
        // not enough for a Rust rewrite, so the ceiling is now generous. These are ceilings,
        // not targets — a model that finishes early costs nothing extra — and the session
        // token budget is what actually bounds the spend.
        Output::Edit => 8192,
        _ => 2048,
    };
    PromptSpec {
        system: system(&ctx.flavour, verb),
        user,
        json: true,
        max_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::scope;
    use crate::types::Output;

    fn ctx_of(text: &str, lang: &str) -> Context {
        let d = Document::new("file:///tmp/a", 1, text.to_string(), Some(lang));
        let s = scope::resolve(text, 0, &crate::lang::profile(lang), None, 400);
        crate::context::build(&d, &s, &[], 5)
    }

    #[test]
    fn every_verb_produces_a_prompt_demanding_json() {
        let ctx = ctx_of("fn a() {}\n", "rust");
        for v in Verb::ALL {
            let p = render(v, &ctx);
            assert!(p.json, "{v:?} must request JSON");
            assert!(p.system.contains("single JSON object"), "{v:?}");
            assert!(p.user.contains("TASK:"), "{v:?}");
            assert!(p.user.contains("FILE: /tmp/a"), "{v:?}");
            assert!(p.max_tokens > 0);
        }
    }

    #[test]
    fn output_class_selects_the_schema() {
        let ctx = ctx_of("fn a() {}\n", "rust");
        assert!(render(Verb::Harden, &ctx).system.contains("new_files"));
        assert!(render(Verb::Review, &ctx).system.contains("\"findings\""));
        assert!(render(Verb::Explain, &ctx).system.contains("\"markdown\""));
        assert_eq!(Verb::Review.output(), Output::Findings);
        assert_eq!(Verb::Explain.output(), Output::Artifact);
    }

    #[test]
    fn the_model_is_told_never_to_invent_positions() {
        let ctx = ctx_of("fn a() {}\n", "rust");
        let s = render(Verb::Harden, &ctx).system;
        assert!(s.contains("Never emit line numbers"));
        assert!(s.contains("occur exactly once"));
    }

    #[test]
    fn the_schema_is_an_example_not_a_template_to_echo() {
        // A placeholder skeleton invites a fast model to return the placeholders as values;
        // observed against a real model, which answered `"verb_hint"` where an object was
        // expected. The schema is therefore a concrete example plus an explicit rule.
        let ctx = ctx_of("f = open(path)\n", "python");
        for verb in [Verb::Harden, Verb::Review, Verb::Explain] {
            let system = render(verb, &ctx).system;
            assert!(
                system.contains("never use a field name as a value"),
                "{verb:?} must warn against echoing the schema"
            );
            assert!(
                !system.contains("function|method|class"),
                "{verb:?} must not show a placeholder alternation"
            );
            // The example is concrete JSON that actually parses.
            let start = system.find('{').unwrap();
            let end = system.rfind('}').unwrap();
            serde_json::from_str::<serde_json::Value>(&system[start..=end])
                .unwrap_or_else(|e| panic!("{verb:?} example is not valid JSON: {e}"));
        }
    }

    #[test]
    fn reasoning_models_get_room_for_reasoning_plus_an_answer() {
        // Measured: a thinking model at a 2048 ceiling returned finish_reason=length with
        // empty content. Ceilings are not targets, so being generous here is nearly free.
        let ctx = ctx_of("f = open(path)\n", "python");
        assert!(render(Verb::Harden, &ctx).max_tokens >= 4096);
        assert!(render(Verb::Review, &ctx).max_tokens >= 2048);
        assert!(render(Verb::Explain, &ctx).max_tokens >= 2048);
    }

    #[test]
    fn an_unknown_language_gets_the_generic_persona_not_a_refusal() {
        let ctx = ctx_of("mystery content\n", "unknown");
        let s = render(Verb::Review, &ctx).system;
        assert!(s.contains("treat it as text"));
        assert!(!s.contains("senior unknown engineer"));
    }

    #[test]
    fn a_completion_prompt_carries_both_sides_and_no_json() {
        let p = render_completion("rust", "/a/b.rs", "let x = ", ";\n", None);
        assert!(!p.json, "a completion is text, not JSON");
        assert!(p.user.contains("let x = "));
        assert!(p.user.contains(";"));
        assert!(p.user.contains("do not repeat"));
        assert!(p.max_tokens <= 256, "completions are cheap or they are useless");
        assert!(p.system.contains("only the text that belongs"));
    }

    #[test]
    fn configured_fim_tokens_replace_the_instruction_form() {
        let tokens = crate::config::FimTokens {
            prefix: "<P>".into(),
            suffix: "<S>".into(),
            middle: "<M>".into(),
        };
        let p = render_completion("rust", "/a.rs", "abc", "def", Some(&tokens));
        assert_eq!(p.user, "<P>abc<S>def<M>");
    }

    #[test]
    fn prompts_are_deterministic() {
        let ctx = ctx_of("fn a() {}\n", "rust");
        assert_eq!(render(Verb::Docs, &ctx), render(Verb::Docs, &ctx));
    }
}
