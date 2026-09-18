//! `meta`'s command line (PROTOCOL.md §11).
//!
//! Pure by construction: no filesystem, no model, no clock. Every form the contract names —
//! a bare path, a line, a line and a column, a line range, `-` for stdin — is decided here,
//! so the exit-2 surface is testable without a document and without an endpoint.

use meta_core::config::Config;
use meta_core::types::{Output, Verb};

pub const USAGE: &str = "\
meta — a one-shot client for the model behind the editor's language server

USAGE:
    meta explain <path>[:<line>[:<col>]]         explanation artifact (JSON) to stdout
    meta review <path>                           findings (JSON) to stdout
    meta action --verb <verb> <path>[:<range>]   proposed edit (JSON), never applied
    meta plan --goal <text> <path>               plan artifact (JSON) to stdout
    meta status                                  budget, queue and cache (JSON) to stdout

    <path>     the one file to read. `-` reads the document from stdin instead.
    <line>     1-based line number, as a compiler prints it.
    <col>      1-based byte column within <line>.
    <range>    an inclusive 1-based line range, `12-20`. A bare number is the cursor line.

VERBS:
    fix fixAll harden types docs rewrite test generate

OPTIONS:
    --verb <verb>       action: which transformation to propose (required)
    --goal <text>       plan: what the plan is for (required)
    --base-url <url>    override the model endpoint for every tier (also META_BASE_URL)
    --model <name>      override the model name for every tier (also META_MODEL,
                        META_REVIEW_MODEL)
    --max-tokens <n>    ceiling on completion tokens per call; the prompt's own budget
                        still caps it
    -h, --help          print this message
    -V, --version       print the version

OUTPUT:
    stdout carries exactly one JSON line: the artifact or the result, and nothing else.
    stderr carries diagnostics, including one cost line per model call.

EXIT CODES:
    0  success                       1  transport or model failure
    2  usage error or contract violation
    3  budget exhausted              4  stale target (the file changed in flight)
";

/// Where the document comes from. `-` is stdin, as it is everywhere else on a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    File(String),
    Stdin,
}

impl Source {
    /// How the source is named in a diagnostic.
    pub fn describe(&self) -> String {
        match self {
            Source::File(path) => path.clone(),
            Source::Stdin => "stdin".to_string(),
        }
    }
}

/// A document, plus whatever position the command was pointed at.
///
/// Positions are held zero-based and byte-oriented, which is what `meta-core` speaks
/// (PROTOCOL.md N1). The command line spells them one-based, which is what every compiler
/// and every `grep -n` prints, and the conversion happens once, here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub source: Source,
    pub line: Option<u32>,
    /// Byte column within [`Target::line`].
    pub col: Option<u32>,
    /// Inclusive line range, when the spec named one.
    pub range: Option<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Explain(Target),
    Review(Target),
    Action { verb: Verb, target: Target },
    Plan { goal: String, target: Target },
    Status,
}

/// Model-endpoint overrides. Applied after the defaults and after the environment: the flag
/// a user typed is the last word, and the environment is the next-to-last, exactly as it is
/// for the server (PROTOCOL.md §10).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
}

impl Overrides {
    /// Point every tier at the override. The CLI serves one request at a time and cannot
    /// know which tier is coming, so a flag that names an endpoint names it for all of them.
    pub fn apply(&self, cfg: &mut Config) {
        for tier in [&mut cfg.models.reason, &mut cfg.models.review] {
            if let Some(url) = &self.base_url {
                tier.base_url = url.clone();
            }
            if let Some(model) = &self.model {
                tier.model = model.clone();
            }
            if let Some(n) = self.max_tokens {
                tier.max_tokens = n;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    Help,
    Version,
    Run(Command, Overrides),
}

/// A bad command line. Nothing is written to stdout for one: there is no artifact and no
/// result, only the exit code and the reason on stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError(pub String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

/// The suffix grammar a command's contract allows (PROTOCOL.md §11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    /// No position at all.
    None,
    /// `[:<line>[:<col>]]`.
    LineCol,
    /// `[:<range>]`, where a range is one line or an inclusive `start-end`.
    Range,
}

pub fn parse(args: &[String]) -> Result<Invocation, UsageError> {
    // A help or version request wins wherever it appears, and asks nothing of the rest.
    if args.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(Invocation::Help);
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        return Ok(Invocation::Version);
    }

    let mut overrides = Overrides::default();
    let mut verb: Option<Verb> = None;
    let mut goal: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        // `--opt=value` and `--opt value` are the same statement.
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value.to_string())),
            _ => (arg, None),
        };
        match name {
            "--verb" => {
                let value = value_of(args, &mut i, inline, "--verb")?;
                verb = Some(parse_verb(&value)?);
            }
            "--goal" => {
                let value = value_of(args, &mut i, inline, "--goal")?;
                if value.trim().is_empty() {
                    return Err(UsageError("--goal needs something to plan for".to_string()));
                }
                goal = Some(value);
            }
            "--base-url" => {
                let value = value_of(args, &mut i, inline, "--base-url")?;
                if value.trim().is_empty() {
                    return Err(UsageError("--base-url needs a URL".to_string()));
                }
                overrides.base_url = Some(value);
            }
            "--model" => {
                let value = value_of(args, &mut i, inline, "--model")?;
                if value.trim().is_empty() {
                    return Err(UsageError("--model needs a name".to_string()));
                }
                overrides.model = Some(value);
            }
            "--max-tokens" => {
                let value = value_of(args, &mut i, inline, "--max-tokens")?;
                match value.parse::<u32>() {
                    Ok(n) if n > 0 => overrides.max_tokens = Some(n),
                    _ => {
                        return Err(UsageError(format!(
                            "--max-tokens needs a positive integer, got `{value}`"
                        )))
                    }
                }
            }
            // A bare `-` is stdin, and `-:3` is stdin at a position, not an option.
            other if other.starts_with('-') && other.len() > 1 && !other.starts_with("-:") => {
                return Err(UsageError(format!("unrecognised option `{other}`")));
            }
            _ => positional.push(arg.to_string()),
        }
        i += 1;
    }

    let mut positional = positional.into_iter();
    let Some(command) = positional.next() else {
        return Err(UsageError("no command given".to_string()));
    };
    let rest: Vec<String> = positional.collect();

    let cmd = match command.as_str() {
        "explain" => {
            no_flag(verb.is_some(), "--verb", "explain")?;
            no_flag(goal.is_some(), "--goal", "explain")?;
            Command::Explain(one_target("explain", &rest, Position::LineCol)?)
        }
        "review" => {
            no_flag(verb.is_some(), "--verb", "review")?;
            no_flag(goal.is_some(), "--goal", "review")?;
            Command::Review(one_target("review", &rest, Position::None)?)
        }
        "action" => {
            no_flag(goal.is_some(), "--goal", "action")?;
            let Some(verb) = verb else {
                return Err(UsageError(
                    "action needs --verb <verb>; see `meta --help` for the verbs".to_string(),
                ));
            };
            Command::Action {
                verb,
                target: one_target("action", &rest, Position::Range)?,
            }
        }
        "plan" => {
            no_flag(verb.is_some(), "--verb", "plan")?;
            let Some(goal) = goal else {
                return Err(UsageError("plan needs --goal <text>".to_string()));
            };
            Command::Plan {
                goal,
                target: one_target("plan", &rest, Position::None)?,
            }
        }
        "status" => {
            no_flag(verb.is_some(), "--verb", "status")?;
            no_flag(goal.is_some(), "--goal", "status")?;
            if !rest.is_empty() {
                return Err(UsageError(format!(
                    "status takes no arguments, got `{}`",
                    rest.join(" ")
                )));
            }
            Command::Status
        }
        other => {
            return Err(UsageError(format!(
                "unknown command `{other}`; try `meta --help`"
            )))
        }
    };
    Ok(Invocation::Run(cmd, overrides))
}

/// The value of `--name`, whether it was written inline or as the next argument.
fn value_of(
    args: &[String],
    i: &mut usize,
    inline: Option<String>,
    name: &str,
) -> Result<String, UsageError> {
    if let Some(value) = inline {
        return Ok(value);
    }
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| UsageError(format!("{name} needs a value")))
}

fn parse_verb(value: &str) -> Result<Verb, UsageError> {
    let verb = Verb::parse(value).ok_or_else(|| {
        UsageError(format!(
            "`{value}` is not a verb; one of {}",
            editable_verbs()
        ))
    })?;
    // `explain` and `review` are verbs in the editor's menu, but they produce an artifact
    // and findings, not an edit. Saying so here keeps `action`'s contract single-valued.
    if verb.output() != Output::Edit {
        return Err(UsageError(format!(
            "`{value}` produces {}, not an edit; use `meta {}`",
            match verb.output() {
                Output::Artifact => "an artifact",
                _ => "findings",
            },
            if verb == Verb::Explain { "explain" } else { "review" }
        )));
    }
    Ok(verb)
}

fn editable_verbs() -> String {
    Verb::ALL
        .iter()
        .filter(|v| v.output() == Output::Edit)
        .map(|v| v.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn no_flag(given: bool, flag: &str, command: &str) -> Result<(), UsageError> {
    if given {
        return Err(UsageError(format!(
            "{flag} is only meaningful for another command, not for `meta {command}`"
        )));
    }
    Ok(())
}

fn one_target(command: &str, rest: &[String], position: Position) -> Result<Target, UsageError> {
    match rest.len() {
        1 => parse_target(command, &rest[0], position),
        0 => Err(UsageError(format!("{command} needs a path (or `-`)"))),
        _ => Err(UsageError(format!(
            "{command} takes exactly one path, got {} arguments",
            rest.len()
        ))),
    }
}

/// Parse `path[:<line>[:<col>]]` or `path[:<range>]`.
///
/// The split runs from the right and only accepts a trailing group that is a number or a
/// `start-end` pair, so a path that legitimately contains a colon (`notes:1.txt`) survives
/// untouched.
fn parse_target(command: &str, spec: &str, position: Position) -> Result<Target, UsageError> {
    let (path, parts) = split_suffix(spec);
    if path.is_empty() {
        return Err(UsageError(format!("`{spec}` names no file")));
    }
    let source = if path == "-" {
        Source::Stdin
    } else {
        Source::File(path.to_string())
    };
    let mut target = Target {
        source,
        line: None,
        col: None,
        range: None,
    };

    match position {
        Position::None => {
            if !parts.is_empty() {
                return Err(UsageError(format!(
                    "`meta {command}` takes a bare path; `{spec}` carries the position `{}`. \
                     Use `meta explain` or `meta action` for a position.",
                    parts.join(":")
                )));
            }
        }
        Position::LineCol | Position::Range => {
            if parts.len() > 2 {
                return Err(UsageError(format!(
                    "`{spec}` has too many positions; expected `path[:<line>[:<col>]]`"
                )));
            }
            let head = parts.first().copied();
            let tail = parts.get(1).copied();
            if let Some(tail) = tail {
                if tail.contains('-') {
                    return Err(UsageError(format!(
                        "`{tail}` is a range where a column belongs; a range cannot also name a \
                         column"
                    )));
                }
                if head.is_some_and(|h| h.contains('-')) {
                    return Err(UsageError(format!(
                        "`{spec}` names a column on a line range; a range cannot carry one"
                    )));
                }
            }
            if let Some(head) = head {
                match head.split_once('-') {
                    Some((start, end)) => {
                        if position == Position::LineCol {
                            return Err(UsageError(format!(
                                "`{head}` is a line range, and `meta {command}` takes a single \
                                 line; use `meta action` for a range"
                            )));
                        }
                        let start = number(start, "line")?;
                        let end = number(end, "line")?;
                        if start > end {
                            return Err(UsageError(format!(
                                "the range `{head}` ends before it starts"
                            )));
                        }
                        target.range = Some((start, end));
                    }
                    None => {
                        target.line = Some(number(head, "line")?);
                    }
                }
            }
            if let Some(tail) = tail {
                let col = number(tail, "column")?;
                if target.line.is_none() {
                    return Err(UsageError(format!(
                        "`{spec}` names the column `{tail}` without a line"
                    )));
                }
                target.col = Some(col);
            }
        }
    }
    Ok(target)
}

/// A 1-based number becomes a 0-based offset. Zero is not a position.
fn number(text: &str, what: &str) -> Result<u32, UsageError> {
    match text.parse::<u32>() {
        Ok(n) if n > 0 => Ok(n - 1),
        _ => Err(UsageError(format!(
            "`{text}` is not a {what}; positions are 1-based"
        ))),
    }
}

fn split_suffix(spec: &str) -> (&str, Vec<&str>) {
    let mut parts = Vec::new();
    let mut head = spec;
    while let Some((rest, tail)) = head.rsplit_once(':') {
        if !is_position(tail) {
            break;
        }
        parts.push(tail);
        head = rest;
    }
    parts.reverse();
    (head, parts)
}

/// A trailing `:` group is a position only when it is a number or a `start-end` pair.
fn is_position(text: &str) -> bool {
    match text.split_once('-') {
        Some((start, end)) => is_number(start) && is_number(end),
        None => is_number(text),
    }
}

fn is_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Invocation, UsageError> {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse(&owned)
    }

    fn run(args: &[&str]) -> (Command, Overrides) {
        match parse_args(args).unwrap() {
            Invocation::Run(cmd, overrides) => (cmd, overrides),
            other => panic!("expected a command, got {other:?}"),
        }
    }

    fn file(path: &str, line: Option<u32>, col: Option<u32>, range: Option<(u32, u32)>) -> Target {
        Target {
            source: Source::File(path.to_string()),
            line,
            col,
            range,
        }
    }

    #[test]
    fn every_command_parses() {
        assert_eq!(run(&["status"]).0, Command::Status);
        assert_eq!(
            run(&["explain", "a.py"]).0,
            Command::Explain(file("a.py", None, None, None))
        );
        assert_eq!(
            run(&["review", "a.py"]).0,
            Command::Review(file("a.py", None, None, None))
        );
        assert_eq!(
            run(&["action", "--verb", "harden", "a.py"]).0,
            Command::Action {
                verb: Verb::Harden,
                target: file("a.py", None, None, None),
            }
        );
        assert_eq!(
            run(&["plan", "--goal", "make it cancellable", "a.py"]).0,
            Command::Plan {
                goal: "make it cancellable".to_string(),
                target: file("a.py", None, None, None),
            }
        );
    }

    #[test]
    fn a_line_and_a_column_are_one_based_on_the_command_line_and_zero_based_in_the_target() {
        assert_eq!(
            run(&["explain", "a.py:12"]).0,
            Command::Explain(file("a.py", Some(11), None, None))
        );
        assert_eq!(
            run(&["explain", "a.py:12:4"]).0,
            Command::Explain(file("a.py", Some(11), Some(3), None))
        );
    }

    #[test]
    fn a_range_is_an_inclusive_one_based_line_span() {
        assert_eq!(
            run(&["action", "--verb", "docs", "a.py:10-20"]).0,
            Command::Action {
                verb: Verb::Docs,
                target: file("a.py", None, None, Some((9, 19))),
            }
        );
        assert_eq!(
            run(&["action", "--verb", "docs", "a.py:7"]).0,
            Command::Action {
                verb: Verb::Docs,
                target: file("a.py", Some(6), None, None),
            }
        );
    }

    #[test]
    fn stdin_is_spelled_the_same_way_it_is_spelled_in_a_shell() {
        assert_eq!(
            run(&["explain", "-"]).0,
            Command::Explain(Target {
                source: Source::Stdin,
                line: None,
                col: None,
                range: None,
            })
        );
        assert_eq!(
            run(&["explain", "-:3"]).0,
            Command::Explain(Target {
                source: Source::Stdin,
                line: Some(2),
                col: None,
                range: None,
            })
        );
    }

    #[test]
    fn a_path_that_contains_a_colon_is_still_a_path() {
        assert_eq!(
            run(&["review", "notes:1.txt"]).0,
            Command::Review(file("notes:1.txt", None, None, None))
        );
        assert_eq!(
            run(&["explain", "notes:1.txt:4"]).0,
            Command::Explain(file("notes:1.txt", Some(3), None, None))
        );
    }

    #[test]
    fn flags_may_be_written_inline_or_with_a_space() {
        let (_, overrides) = run(&[
            "review",
            "--base-url=http://127.0.0.1:9/v1",
            "--model",
            "m",
            "--max-tokens=64",
            "a.py",
        ]);
        assert_eq!(
            overrides,
            Overrides {
                base_url: Some("http://127.0.0.1:9/v1".to_string()),
                model: Some("m".to_string()),
                max_tokens: Some(64),
            }
        );
    }

    #[test]
    fn overrides_move_every_tier() {
        let mut cfg = Config::default();
        Overrides {
            base_url: Some("http://elsewhere/v1".to_string()),
            model: Some("other".to_string()),
            max_tokens: Some(32),
        }
        .apply(&mut cfg);
        for tier in [&cfg.models.reason, &cfg.models.review] {
            assert_eq!(tier.base_url, "http://elsewhere/v1");
            assert_eq!(tier.model, "other");
            assert_eq!(tier.max_tokens, 32);
        }
    }

    #[test]
    fn an_unknown_command_is_a_usage_error() {
        let e = parse_args(&["explainn", "a.py"]).unwrap_err();
        assert!(e.0.contains("unknown command"), "{e}");
    }

    #[test]
    fn an_unknown_verb_is_a_usage_error_that_names_the_real_ones() {
        let e = parse_args(&["action", "--verb", "polish", "a.py"]).unwrap_err();
        assert!(e.0.contains("not a verb"), "{e}");
        assert!(e.0.contains("harden"), "{e}");
    }

    #[test]
    fn the_verbs_that_produce_no_edit_are_refused_with_a_pointer_to_the_right_command() {
        let e = parse_args(&["action", "--verb", "explain", "a.py"]).unwrap_err();
        assert!(e.0.contains("meta explain"), "{e}");
        let e = parse_args(&["action", "--verb", "review", "a.py"]).unwrap_err();
        assert!(e.0.contains("findings"), "{e}");
    }

    #[test]
    fn position_forms_are_enforced_per_command() {
        // `review` has no position in its contract.
        let e = parse_args(&["review", "a.py:12"]).unwrap_err();
        assert!(e.0.contains("bare path"), "{e}");
        // `explain` has a line, not a range.
        let e = parse_args(&["explain", "a.py:10-20"]).unwrap_err();
        assert!(e.0.contains("line range"), "{e}");
        // A range is a line span, never a line and a column.
        let e = parse_args(&["action", "--verb", "docs", "a.py:10-20:3"]).unwrap_err();
        assert!(e.0.contains("cannot carry one"), "{e}");
    }

    #[test]
    fn missing_required_flags_are_usage_errors() {
        assert!(parse_args(&["action", "a.py"]).unwrap_err().0.contains("--verb"));
        assert!(parse_args(&["plan", "a.py"]).unwrap_err().0.contains("--goal"));
        assert!(parse_args(&["plan", "--goal", "  ", "a.py"]).is_err());
        assert!(parse_args(&["action", "--verb"]).is_err(), "a flag needs a value");
    }

    #[test]
    fn flags_belonging_to_another_command_are_refused() {
        assert!(parse_args(&["review", "--verb", "docs", "a.py"]).is_err());
        assert!(parse_args(&["status", "--goal", "x"]).is_err());
        assert!(parse_args(&["explain", "--goal", "x", "a.py"]).is_err());
    }

    #[test]
    fn malformed_numbers_ranges_and_options_are_usage_errors() {
        assert!(parse_args(&["explain", "a.py:0"]).is_err(), "zero is not a line");
        assert!(parse_args(&["explain", "a.py:2:0"]).is_err(), "zero is not a column");
        assert!(parse_args(&["action", "--verb", "docs", "a.py:20-10"]).is_err());
        assert!(
            parse_args(&["action", "--verb", "docs", "a.py:1-2-3"]).is_ok(),
            "a trailing group that is not a number or a range stays part of the path"
        );
        assert!(parse_args(&["action", "--verb", "docs", "a.py:4:2"]).is_ok(), "a line and a column are legal for action");
        assert!(parse_args(&["explain", "--colour", "a.py"]).is_err());
        assert!(parse_args(&["explain", "--max-tokens", "0", "a.py"]).is_err());
        assert!(parse_args(&["explain", "--max-tokens", "many", "a.py"]).is_err());
        assert!(parse_args(&["explain", "-"]).is_ok());
        assert!(parse_args(&["explain", "-x"]).is_err());
    }

    #[test]
    fn arity_is_enforced() {
        assert!(parse_args(&[]).unwrap_err().0.contains("no command"));
        assert!(parse_args(&["explain"]).is_err());
        assert!(parse_args(&["explain", "a.py", "b.py"]).is_err());
        assert!(parse_args(&["status", "a.py"]).is_err());
        assert!(parse_args(&["status"]).is_ok());
    }

    #[test]
    fn help_and_version_win_over_everything_else() {
        assert_eq!(parse_args(&["--help"]).unwrap(), Invocation::Help);
        assert_eq!(parse_args(&["explain", "-h", "a.py"]).unwrap(), Invocation::Help);
        assert_eq!(parse_args(&["-V"]).unwrap(), Invocation::Version);
        assert_eq!(parse_args(&["review", "--version"]).unwrap(), Invocation::Version);
    }

    #[test]
    fn the_usage_text_names_every_command_and_every_flag() {
        for command in ["explain", "review", "action", "plan", "status"] {
            assert!(USAGE.contains(&format!("meta {command} ")), "{command}");
        }
        for flag in [
            "--verb",
            "--goal",
            "--base-url",
            "--model",
            "--max-tokens",
            "--help",
            "--version",
        ] {
            assert!(USAGE.contains(flag), "{flag}");
        }
        for code in ["0  success", "1  transport", "2  usage", "3  budget", "4  stale"] {
            assert!(USAGE.contains(code), "{code}");
        }
    }
}
