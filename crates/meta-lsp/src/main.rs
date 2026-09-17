//! `meta-lsp` — a language server that runs a model over the buffers an editor has open.
//!
//! Transport only: capabilities, document sync, and the four handlers that carry the
//! product (`codeAction`, `codeAction/resolve`, `textDocument/diagnostic`,
//! `workspace/executeCommand`). All the thinking lives in `meta-core`.

mod advertised;
mod engine;
mod inline;
mod server;
mod state;

use meta_core::config::Config;
use meta_core::model::OpenAiCompat;
use std::sync::Arc;
use tower_lsp::{LspService, Server};

const USAGE: &str = "\
meta-lsp — an LLM-backed language server

USAGE:
    meta-lsp [--stdio]

OPTIONS:
    --stdio      communicate over stdin/stdout (the default, and the only transport)
    -V, --version print the version
    -h, --help   print this message

CONFIGURATION:
    Model endpoints are taken from the client's `workspace/configuration` under the
    `meta` section (PROTOCOL.md §10). For shells that already know where the local model
    lives, three environment variables override the defaults:

    META_BASE_URL        OpenAI-compatible base URL for every tier
    META_MODEL           model name for the reason and fim tiers
    META_REVIEW_MODEL    model name for the review tier
";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return;
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("meta-lsp {}", meta_core::VERSION);
        return;
    }
    // `--stdio` is the only transport; accept it so editor configs do not have to differ.
    let unknown: Vec<&String> = args.iter().filter(|a| *a != "--stdio").collect();
    if !unknown.is_empty() {
        eprintln!(
            "meta-lsp: unrecognised argument(s): {}. Try --help.",
            unknown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        );
        std::process::exit(2);
    }

    let config = Config::default().with_env_overrides();
    let state = state::AppState::new(Arc::new(OpenAiCompat::new()), config);

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    // `build` rather than `new`: inline completion is a 3.18-draft method the pinned
    // `lsp-types` has no handler for, so it is registered as a custom method.
    let (service, socket) = LspService::build(|client| server::MetaServer::new(client, state))
        .custom_method(
            "textDocument/inlineCompletion",
            server::MetaServer::inline_completion,
        )
        .finish();
    // `advertised` adds the one draft capability the pinned `lsp-types` cannot express.
    Server::new(stdin, stdout, socket)
        .serve(advertised::Advertised::new(service))
        .await;
}
