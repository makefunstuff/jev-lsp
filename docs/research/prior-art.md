# Prior art

Searched 2026-09-18. What exists, what it does, and what that means for this design.

**Evidence labels.** **[P]** primary source read directly (README/wiki/release notes).
**[S]** search-engine synthesis only — treated as a lead, never as verification. No claim
below about another project's behaviour is made from a snippet alone.

## 1. The four projects that matter

### LSP-AI — `SilasMarvin/lsp-ai` **[P]** ([repo](https://github.com/SilasMarvin/lsp-ai), [wiki](https://github.com/SilasMarvin/lsp-ai/wiki/Server-Capabilities-and-Functions))

The closest prior art, and the same core thesis: *"a language server that serves as a
backend for AI-powered functionality in your favorite code editors… because it is a
language server, it works with any editor that has LSP support."*

- **Documented LSP surface:** `textDocument/completion`, `textDocument/didOpen`,
  `textDocument/didChange`, `textDocument/rename`, plus a **custom**
  `textDocument/generation` request. Read from the wiki page verbatim.
- Configured through `initializationOptions`: a `models` registry plus `completion`,
  `actions`, and `chat` blocks; prompts are message templates with `{SELECTED_TEXT}`,
  `{CODE}`, `{CONTEXT}`; actions **replace the selection**.
- Backends: llama.cpp, Ollama, OpenAI/Anthropic/Gemini/Mistral-compatible.
- Status: *"has reached a stage where it has all the features I want for it… no new
  features are currently being developed."*
- Roadmap still lists *"semantic search-powered context building… planning to use
  Tree-sitter to chunk code correctly"* — i.e. context building is unbuilt.

### `huggingface/llm-ls` **[P]** ([repo](https://github.com/huggingface/llm-ls))

A Rust LSP server for completion, used as the backend for `llm.nvim`, `llm-vscode`,
`llm-intellij`.

- Scoped to completion; decides single-line / multi-line / empty by parsing the AST.
- Tokenizes the prompt to stay inside the model's context window.
- **Gathers telemetry "that can enable retraining"**, logged to `~/.cache/llm_ls/`.
- Self-described: *"This is currently a work in progress, expect things to be broken!"*
  Last release 0.5.3, May 2024 **[S]**.

### `mattn/llm-lsp` **[P]** ([repo](https://github.com/mattn/llm-lsp))

Smallest of the four: Go, single binary, OpenAI-compatible endpoint.

- `textDocument/completion`, `textDocument/hover` (explain the line under the cursor), and
  `textDocument/definition` — the latter **runs a tool-calling agent loop**
  (grep / read_file / list_files) with a **default timeout of 180 seconds**.
- Configures the model through env vars or `initializationOptions`.
- Its own Neovim example uses an explicit `filetypes` allowlist.

### `github/copilot-language-server` **[P]** ([repo](https://github.com/github/copilot-language-server-release))

Commercially-deployed proof that the LSP-as-AI-backend architecture scales, and the most
instructive on where the standard protocol runs out.

- Uses **`textDocument/inlineCompletion`** from the draft 3.18 spec for ghost text — with
  **non-standard additions** `textDocument.version` and `formattingOptions` in the params.
- Employs many custom messages its README admits are outside the spec: `textDocument/didFocus`,
  `didChangeStatus`, `textDocument/didShowCompletion`,
  `github.copilot.didAcceptCompletionItem`, `signIn`/`signOut`.
- `didChangeStatus` carries busy/message/kind/command, and one kind is directly relevant
  here: **`Inactive` — "when the current file is ignored due to file size or content
  exclusions."**
- Uses `window/showDocument` to open auth URLs, and `workspace/executeCommand` **after**
  a completion is accepted, purely for acceptance telemetry.
- Also supports the Agent Client Protocol (ACP) for Zed/JetBrains — a second protocol
  alongside LSP.

## 2. Where this design converges with prior art

Independent agreement is weak evidence, but it is evidence. These are decisions this
design reached from the protocol and from probing Neovim, which others reached separately:

| Decision | Corroboration |
|---|---|
| LSP as the editor-agnostic AI backend | LSP-AI's entire thesis **[P]**; Copilot ships it **[P]** |
| Local-first backends (llama.cpp / Ollama / OpenAI-compatible) | all four **[P]** |
| Small model for completion, larger for actions | LSP-AI's getting-started advice **[P]** |
| `textDocument/inlineCompletion` as the ghost-text method | Copilot, in production **[P]** |
| Completion decisions need an AST, not just text | `llm-ls` parses the AST **[P]** |
| Structured output, validated before use | Copilot's `items[].insertText` shape **[P]** |
| Status must be surfaced to the user out-of-band | Copilot's `didChangeStatus` **[P]** |

**[S]** A search synthesis also independently described: debounced background review,
snapshot validation on `(version, contentHash)`, priority lanes, one job per file,
cache keyed by `(contentHash, model, promptVersion, configurationHash)`, a separate
diagnostic namespace, dismissible findings with persisted fingerprints, and confidence
thresholds. That is the shape of §3–§5 of `PROTOCOL.md`. It is a lead, not verification —
the primary sources it cites were not all read here.

## 3. Where this design diverges

Stated as claims about the sample examined, not about the world.

| | LSP-AI | llm-ls | llm-lsp | Copilot LS | this design |
|---|---|---|---|---|---|
| Code actions | ✗ (not in the documented surface) | ✗ | ✗ | partial | **core surface** |
| Ambient findings without a prompt | ✗ | ✗ | ✗ | ✗ | **pull diagnostics + refresh** |
| Runs the model inside a user-blocking request | yes (`actions`, `generation`) | yes | yes (up to 180 s on `definition`) | yes | **never** (N2/N3) |
| Version-stamped edits with staleness refusal | ✗ | ✗ | ✗ | ✗ | **N4** |
| Multi-step plan with approval | ✗ | ✗ | ✗ | ✗ | **plan artifact + buffer** |
| Scope derived from the syntax tree | ✗ (manual `{SELECTED_TEXT}`) | completion only | ✗ | ✗ | **treesitter scope** |
| Universal support, no filetype allowlist | client-side only | client-side globs | allowlist in its own example | n/a | **plugin attach pass (§4)** |
| Cost gates and budgets | token caps only | token budgeting | token caps | quotas | **six ordered gates** |
| Telemetry | — | **retraining telemetry** | — | acceptance telemetry | **refused (§12)** |

Two of these deserve emphasis:

1. **No prior art does ambient findings.** Every project is request-driven: the user asks,
   the model answers. The distinguishing thesis here — the model works while you work, and
   the editor's sign column is the notification — is unoccupied ground.
2. **No prior art splits latency.** LSP-AI generates inside the action request; llm-lsp
   will hold `textDocument/definition` for three minutes. That is the single most
   consequential difference, and the reason for N2/N3 and the `codeAction/resolve` split.

## 4. Universal support: nobody has solved it

The project-specific evidence is unambiguous: `llm-lsp` ships an explicit `filetypes`
allowlist in its own Neovim example **[P]**; `llm.nvim` gates suggestions with
`enable_suggestions_on_files` globs **[S]**; and **[S]** summarizes the situation as
*"universal 'any file' support is therefore editor-specific, not a portable LSP-server
setting"* — with Helix's catch-all `file-types = [{ glob = "*" }]` reported to clobber
built-in language configuration.

That matches what was measured on this machine in `verify/probes/language.lua`: a server
attached with `filetypes = nil` still only receives the buffers where Neovim fired
`FileType`, and **3 of 11 fixtures were never attached** — the unidentified ones. The gap
is in the client, and closing it requires a plugin-side attach pass. See `docs/LANGUAGE.md`.

## 5. Stealable

| Idea | Source | How it lands here |
|---|---|---|
| A user-visible `Inactive` status for files skipped by policy | Copilot **[P]** | The `over_size` / `ignored` state in `docs/LANGUAGE.md` §6; support is universal, so skips must be *stated*, never silent |
| Separating "completion model" from "action model" as first-class config | LSP-AI **[P]** | Already the `fim` / `reason` / `review` tiers in `docs/MODEL.md` |
| FIM token configuration for non-Mistral models | LSP-AI **[P]** | `models.fim.fim_tokens` in the config schema |
| AST-informed completion shape (single-line vs multi-line vs none) | llm-ls **[P]** | The FIM gate stack in `PROTOCOL.md` §5 |
| Both configuration channels (push `didChangeConfiguration` and pull `workspace/configuration`) | Copilot **[P]** | Pull only, for now — Neovim uses `vim.lsp.config`; noted as an interoperability gap for other clients |

## 6. Do not copy

| Practice | Source | Why not |
|---|---|---|
| Running the model inside a user-blocking request | LSP-AI, llm-lsp **[P]** | A frozen editor is the failure mode N2 exists to prevent |
| Hijacking a semantic method (`definition`) for a slow agent loop | llm-lsp **[P]** | Breaks the meaning of `gd`; a 180 s timeout on a navigation key is a defect, not a feature |
| Telemetry intended for retraining | llm-ls **[P]** | Refused in `PROTOCOL.md` §12 |
| Custom methods for core features (`textDocument/generation`) | LSP-AI **[P]** | N6. Costs portability for nothing we need |
| Non-standard additions to standard params (e.g. `version` inside `inlineCompletion`) | Copilot **[P]** | Same reason; staleness is handled by content hash on our side |
| A per-filetype allowlist as the attachment mechanism | llm-lsp, llm.nvim **[P]** | Directly contradicts the requirement: support is unconditional |

**One caveat on Copilot's custom methods.** They pay for them because they must: auth,
fleet telemetry, and an accept/decline signal have no standard representation. This design
has none of those needs, so N6 holds. `didFocus` is the one idea worth reconsidering later —
knowing which buffer has the user's attention is genuinely useful for prioritising ambient
work, and the standard-only substitute (recency from `didChange`, plus the buffer that made
an explicit request) is what the scheduler lanes already encode.

## 7. Risk to record

Three of the four projects are dormant or self-described as unstable: LSP-AI is
feature-frozen by its author, `llm-ls` says "expect things to be broken" and last shipped
in May 2024 **[S]**, and `llm-lsp` is a personal tool. The AI-LSP niche has a maintenance
cliff. This design therefore depends on **none of them**: no shared libraries, no config
format compatibility, no forked code. Borrowed ideas are recorded here; borrowed code is
not.

The same cliff is a warning about scope. Every one of these projects stopped when it had
"all the features I wanted". `docs/ROADMAP.md` is deliberately ordered so that units U0–U2
alone are a complete, useful product — ambient findings on save — and everything after is
additive.
