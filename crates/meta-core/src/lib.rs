//! `meta-core` — everything the server knows that is not the language server protocol.
//!
//! Synchronous by construction: no async runtime, no LSP types, no transport. Both the LSP
//! server and the command line front end are thin shells over this crate
//! (docs/ARCHITECTURE.md §1).

pub mod budget;
pub mod cache;
pub mod config;
pub mod context;
pub mod contract;
pub mod document;
pub mod edit;
pub mod fetch;
pub mod findings;
pub mod gates;
pub mod lang;
pub mod model;
pub mod plan;
pub mod scope;
pub mod time;
pub mod types;
pub mod verbs;

/// Crate version, reported by `meta.status` and in trace headers.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use config::Config;
pub use document::{content_hash, Document};
pub use lang::{Profile, Language};
pub use model::{Backend, ChatRequest, ChatResponse, OpenAiCompat};
pub use types::{ActionData, Finding, LineRange, Proposal, ScopeKind, ScopeSource, Verb};
