//! Pure-Rust local embedding runtime for Cairn (brief §3.0).
//!
//! # Quick start
//!
//! ```no_run
//! use std::path::Path;
//! use cairn_embeddings_local::{EmbeddingModelKind, ModelCache};
//!
//! let cache = ModelCache::new(Path::new(".cairn/models"));
//! // After `cairn bootstrap` fetches the model:
//! let model = cache.ensure(EmbeddingModelKind::BgeSmallEnV1_5).unwrap();
//! let vec = model.embed_query("hello world").unwrap();
//! assert_eq!(vec.len(), 384);
//! ```

pub mod cache;
pub mod error;
pub mod model;

mod bge;
mod minilm;

pub use cache::{FetchReport, ModelCache};
pub use cairn_core::config::EmbeddingModelKind;
pub use error::EmbeddingError;
pub use model::{EmbeddingModel, MockEmbedder, l2_normalize, mock_vector};
