//! Embedding backend (MASTER_BUILD_SPEC.md §6.5 / §16).
//!
//! Two interchangeable backends behind one trait:
//!   * in-process candle (pure-Rust; CPU default, CUDA/Metal opt-in) — the
//!     cross-platform default, and
//!   * the Python `gpu-runtime` over HTTP (`/embed`) for the CUDA throughput profile.
//!
//! BGE-M3 produces **dense + sparse** vectors. candle's XLM-RoBERTa encoder gives
//! us the **dense** signal (CLS pooling + L2 normalize) in pure Rust; the
//! **learned-sparse** signal has no candle head and stays behind the Python
//! `gpu-runtime` (tracked gap, ADR-0009). Dynamic batch sizing (grow toward the
//! VRAM limit; target batch ≥ 32) is a §6.5 perf TODO.

use std::path::Path;

use async_trait::async_trait;
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::xlm_roberta::{Config, XLMRobertaModel};
use tokenizers::Tokenizer;

use crate::chunker::Chunk;

/// Map a candle error to a transient embedding failure (GPU OOM is retryable; the
/// caller backs off and may downgrade to CPU — §14 GPU failsafe).
fn candle_transient(e: candle_core::Error) -> EmbedError {
    EmbedError::Transient(format!("candle embedding inference: {e}"))
}

/// Build a `(1, seq_len)` `u32` tensor from a token-id / mask / type-id slice.
fn row_u32(values: &[u32], device: &Device) -> Result<Tensor, EmbedError> {
    Tensor::new(values, device)
        .and_then(|t| t.unsqueeze(0))
        .map_err(candle_transient)
}

/// A single chunk's embedding: dense vector + learned-sparse terms (§5.2).
#[derive(Debug, Clone)]
pub struct Embedding {
    pub vector_id: uuid::Uuid,
    pub dense: Vec<f32>,
    /// Sparse vector as (term-index, weight) pairs (BGE-M3 learned sparse).
    pub sparse: Vec<(u32, f32)>,
    pub embed_model: String,
}

/// Embedding failures. `Transient` (GPU-OOM, network) is retryable; `Permanent`
/// is not (§14).
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("transient embedding failure: {0}")]
    Transient(String),
    #[error("permanent embedding failure: {0}")]
    Permanent(String),
}

/// Default embedding model identifier, recorded per chunk so mixed-model corpora
/// are detectable across LAN peers (§7.3 / §9). DECISION: a stopgap default until
/// the value is sourced from config.
pub const DEFAULT_EMBED_MODEL_ID: &str = "bge-m3";

/// The swappable embedding contract (§19 — "modules behind traits").
#[async_trait]
pub trait EmbeddingBackend: Send + Sync {
    /// Embed a batch of chunks, returning one [`Embedding`] per input in order.
    async fn embed_batch(&self, chunks: &[Chunk]) -> Result<Vec<Embedding>, EmbedError>;

    /// Model identifier recorded per chunk so mixed-model corpora are detectable
    /// across LAN peers (§7.3 / §9 / §22 row 11).
    fn model_id(&self) -> &str;

    /// Suggested starting batch size; the caller grows toward the VRAM limit (§6.5).
    fn target_batch_size(&self) -> usize {
        32
    }
}

/// In-process candle backend — default on Windows / CPU / low-VRAM (§16).
///
/// Loads BGE-M3 (an XLM-RoBERTa encoder) from a local model directory
/// (`config.json`, `tokenizer.json`, `model.safetensors`) using the **safe**
/// (non-mmap) safetensors loader so the crate keeps `#![forbid(unsafe_code)]`.
/// Produces the **dense** vector via CLS pooling + L2 normalize. The
/// learned-sparse vector is left empty here (no candle head) and is supplied by
/// the `gpu-runtime` backend when sparse retrieval is enabled (ADR-0009).
pub struct CandleEmbeddingBackend {
    model_id: String,
    device: Device,
    /// `None` until a model directory is loaded via [`CandleEmbeddingBackend::load`].
    model: Option<XLMRobertaModel>,
    tokenizer: Option<Tokenizer>,
}

impl CandleEmbeddingBackend {
    /// An unloaded backend: constructed by the worker skeleton before a model is
    /// present. `embed_batch` returns a clear error until [`load`](Self::load) is
    /// used (keeps the worker bootable offline; spec §16/§21).
    pub fn new(model_id: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            device: Device::Cpu,
            model: None,
            tokenizer: None,
        }
    }

    /// Load BGE-M3 + tokenizer from a local directory (no Hub fetch; ADR-0009).
    pub fn load(model_dir: &Path, model_id: impl Into<String>) -> Result<Self, EmbedError> {
        // CPU by default so every node builds/boots; CUDA/Metal are opt-in candle
        // cargo features on GPU nodes (with CPU fallback as the §14 failsafe).
        let device = Device::Cpu;

        let cfg_bytes = std::fs::read(model_dir.join("config.json"))
            .map_err(|e| EmbedError::Permanent(format!("read embed config.json: {e}")))?;
        let cfg: Config = serde_json::from_slice(&cfg_bytes)
            .map_err(|e| EmbedError::Permanent(format!("parse embed config.json: {e}")))?;
        let tokenizer = Tokenizer::from_file(model_dir.join("tokenizer.json"))
            .map_err(|e| EmbedError::Permanent(format!("load embed tokenizer.json: {e}")))?;

        let tensors = candle_core::safetensors::load(model_dir.join("model.safetensors"), &device)
            .map_err(|e| EmbedError::Permanent(format!("load embed safetensors: {e}")))?;
        let vb = VarBuilder::from_tensors(tensors, DType::F32, &device);
        let model = XLMRobertaModel::new(&cfg, vb)
            .map_err(|e| EmbedError::Permanent(format!("build embed model: {e}")))?;

        Ok(Self {
            model_id: model_id.into(),
            device,
            model: Some(model),
            tokenizer: Some(tokenizer),
        })
    }

    /// Embed one chunk's text into a dense, L2-normalized vector (CLS pooling).
    fn embed_dense(
        &self,
        model: &XLMRobertaModel,
        tokenizer: &Tokenizer,
        text: &str,
    ) -> Result<Vec<f32>, EmbedError> {
        let enc = tokenizer
            .encode(text, true)
            .map_err(|e| EmbedError::Permanent(format!("embed tokenize: {e}")))?;
        let input_ids = row_u32(enc.get_ids(), &self.device)?;
        let attention_mask = row_u32(enc.get_attention_mask(), &self.device)?;
        let token_type_ids = row_u32(enc.get_type_ids(), &self.device)?;

        // (1, seq, hidden) last-hidden-state.
        let hidden = model
            .forward(
                &input_ids,
                &attention_mask,
                &token_type_ids,
                None,
                None,
                None,
            )
            .map_err(candle_transient)?;
        // CLS pooling: take token 0 → (1, hidden), then L2-normalize.
        let cls = hidden.get_on_dim(1, 0).map_err(candle_transient)?;
        let norm = cls
            .sqr()
            .and_then(|t| t.sum_keepdim(1))
            .and_then(|t| t.sqrt())
            .map_err(candle_transient)?;
        let normed = cls.broadcast_div(&norm).map_err(candle_transient)?;
        normed
            .flatten_all()
            .and_then(|t| t.to_vec1::<f32>())
            .map_err(candle_transient)
    }
}

#[async_trait]
impl EmbeddingBackend for CandleEmbeddingBackend {
    async fn embed_batch(&self, chunks: &[Chunk]) -> Result<Vec<Embedding>, EmbedError> {
        let (model, tokenizer) = match (&self.model, &self.tokenizer) {
            (Some(m), Some(t)) => (m, t),
            _ => {
                return Err(EmbedError::Permanent(
                    "embedding model not loaded; load a local BGE-M3 directory first".to_owned(),
                ))
            }
        };

        // TODO(perf, §6.5): batch chunks up to the VRAM limit instead of one-by-one.
        let mut out = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            let dense = self.embed_dense(model, tokenizer, &chunk.text)?;
            out.push(Embedding {
                vector_id: chunk.chunk_id,
                dense,
                // Learned-sparse has no candle head — supplied by gpu-runtime when
                // sparse retrieval is enabled (ADR-0009 tracked gap).
                sparse: Vec::new(),
                embed_model: self.model_id.clone(),
            });
        }
        Ok(out)
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }
}

/// HTTP backend calling the Python `gpu-runtime` `/embed` endpoint (§16).
pub struct GpuRuntimeEmbeddingBackend {
    endpoint: String,
    model_id: String,
    // TODO: client: reqwest::Client (rustls, mTLS for the inference path, §12.1).
}

impl GpuRuntimeEmbeddingBackend {
    pub fn new(endpoint: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            model_id: model_id.into(),
        }
    }
}

#[async_trait]
impl EmbeddingBackend for GpuRuntimeEmbeddingBackend {
    async fn embed_batch(&self, _chunks: &[Chunk]) -> Result<Vec<Embedding>, EmbedError> {
        // TODO: POST batched texts to `{endpoint}/embed` over mTLS; on CUDA OOM
        //       the runtime downgrades and returns a hardware code → map to
        //       EmbedError::Transient for backoff (§14 GPU failsafe).
        let _ = &self.endpoint;
        Err(EmbedError::Transient(
            "gpu-runtime /embed not yet implemented".into(),
        ))
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(text: &str) -> Chunk {
        Chunk {
            chunk_id: uuid::Uuid::now_v7(),
            tenant_id: uuid::Uuid::now_v7(),
            document_id: uuid::Uuid::now_v7(),
            ordinal: 0,
            text: text.to_owned(),
            token_count: 1,
            section_heading: None,
            page_number: None,
            structure_type: crate::parser::StructureType::Prose,
        }
    }

    /// An unloaded candle backend boots but fails embedding with a clear,
    /// non-retryable error (offline / LAN-only default; spec §16/§21).
    #[tokio::test]
    async fn unloaded_candle_backend_errors_clearly() {
        let backend = CandleEmbeddingBackend::new(DEFAULT_EMBED_MODEL_ID);
        assert_eq!(backend.model_id(), DEFAULT_EMBED_MODEL_ID);
        let err = backend.embed_batch(&[chunk("hello")]).await.unwrap_err();
        assert!(matches!(err, EmbedError::Permanent(_)), "got {err:?}");
    }

    /// A missing model directory is a permanent load error, never a panic.
    #[test]
    fn loading_missing_dir_is_permanent_error() {
        // `unwrap_err` would require the Ok type (the model) to be `Debug`, which
        // candle models are not — assert on the variant directly instead.
        let result = CandleEmbeddingBackend::load(
            std::path::Path::new("/nonexistent/diyrag-model"),
            DEFAULT_EMBED_MODEL_ID,
        );
        assert!(
            matches!(result, Err(EmbedError::Permanent(_))),
            "expected a permanent load error for a missing model dir"
        );
    }
}
