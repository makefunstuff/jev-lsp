//! `meta` — the one-shot client (PROTOCOL.md §11).
//!
//! Transport only: the command line lives in `cli`, the pipeline in `run`. Both are shells
//! over `meta-core`, exactly as the language server is. No daemon, no state, one command per
//! process, and the exit code is the only channel besides stdout.

mod cli;
mod run;

use meta_core::budget::Budget;
use meta_core::cache::Cache;
use meta_core::config::Config;
use meta_core::model::OpenAiCompat;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let backend = OpenAiCompat::new();
    let budget = Budget::new(1);
    let cache = Cache::new(64);
    let files = run::Fs;
    let deps = run::Deps {
        // The same environment the server reads, plus the flags `run` applies on top.
        config: Config::default().with_env_overrides(),
        backend: &backend,
        budget: &budget,
        cache: &cache,
        files: &files,
    };

    let outcome = run::run(&args, &deps);
    for diagnostic in &outcome.stderr {
        eprintln!("{diagnostic}");
    }
    if let Some(line) = &outcome.stdout {
        println!("{line}");
    }
    std::process::exit(outcome.code);
}
