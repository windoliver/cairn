//! `OpenAI` embedding adapter for Cairn.
//!
//! Opt-in: this crate is only compiled when `cairn-cli` is built with
//! `--features openai`. Reads `OPENAI_API_KEY` from env or config; talks to
//! `https://api.openai.com/v1/embeddings` via `reqwest` + `rustls`.
//!
//! See `docs/superpowers/plans/2026-04-30-hybrid-rerank-brainbench.md`
//! (Task 9).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod client;
mod error;
mod types;

pub use client::OpenAiEmbedder;
pub use error::OpenAiEmbeddingError;
