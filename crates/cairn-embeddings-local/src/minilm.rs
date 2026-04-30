//! all-MiniLM-L6-v2 embedding impl.
//!
//! No asymmetric prefix: query and document paths are identical.

use std::path::Path;

use cairn_core::config::EmbeddingModelKind;
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use tokenizers::{Tokenizer, TruncationParams};

use crate::EmbeddingError;
use crate::model::{EmbeddingModel, l2_normalize};

const MAX_TOKENS: usize = 512;

/// all-MiniLM-L6-v2 wrapper.
pub struct MiniLm {
    model: BertModel,
    tokenizer: Tokenizer,
}

impl MiniLm {
    /// Load model weights and tokenizer from `model_dir`.
    ///
    /// # Errors
    ///
    /// Returns an error if any model file cannot be read or parsed.
    pub fn load(model_dir: &Path) -> Result<Self, EmbeddingError> {
        let config_path = model_dir.join("config.json");
        let tokenizer_path = model_dir.join("tokenizer.json");
        let weights_path = model_dir.join("model.safetensors");

        let config_str = std::fs::read_to_string(&config_path)?;
        let config: BertConfig = serde_json::from_str(&config_str)
            .map_err(|e| EmbeddingError::Tokenizer(e.to_string()))?;

        let device = Device::Cpu;
        // Use buffered safetensors (safe, no mmap) to stay within
        // the workspace `#![forbid(unsafe_code)]` invariant.
        let weights_bytes = std::fs::read(&weights_path)?;
        let vb = VarBuilder::from_buffered_safetensors(weights_bytes, DType::F32, &device)?;
        let model = BertModel::load(vb, &config)?;

        let mut tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(EmbeddingError::from)?;
        // Enable 512-token truncation; update in-place if already set.
        if let Some(trunc) = tokenizer.get_truncation_mut() {
            trunc.max_length = MAX_TOKENS;
        } else {
            tokenizer
                .with_truncation(Some(TruncationParams {
                    max_length: MAX_TOKENS,
                    ..TruncationParams::default()
                }))
                .map_err(EmbeddingError::from)?;
        }

        Ok(Self { model, tokenizer })
    }

    fn encode(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(EmbeddingError::from)?;

        let ids: Vec<u32> = encoding.get_ids().to_vec();
        let type_ids: Vec<u32> = encoding.get_type_ids().to_vec();
        let mask: Vec<u32> = encoding.get_attention_mask().to_vec();

        let device = Device::Cpu;
        let input_ids = Tensor::new(ids.as_slice(), &device)?.unsqueeze(0)?;
        let token_type_ids = Tensor::new(type_ids.as_slice(), &device)?.unsqueeze(0)?;
        let attention_mask = Tensor::new(mask.as_slice(), &device)?.unsqueeze(0)?;

        // Mean pooling over the sequence dimension.
        let output = self
            .model
            .forward(&input_ids, &token_type_ids, Some(&attention_mask))?;
        let pooled = output.mean(1)?;
        let mut v: Vec<f32> = pooled.squeeze(0)?.to_vec1()?;
        l2_normalize(&mut v);
        Ok(v)
    }
}

impl EmbeddingModel for MiniLm {
    fn kind(&self) -> EmbeddingModelKind {
        EmbeddingModelKind::AllMiniLmL6V2
    }

    fn dim(&self) -> usize {
        384
    }

    fn embed_document(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.encode(text)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        // MiniLM has no asymmetric prefix; query and document paths are identical.
        self.encode(text)
    }
}
