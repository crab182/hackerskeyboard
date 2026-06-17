//! Embedding backend (MASTER_BUILD_SPEC.md §6.5 / §16).
//!
//! Two interchangeable backends behind one trait:
//!   * in-process ONNX (`ort` / fastembed) — the cross-platform default, and
//!   * the Python `gpu-runtime` over HTTP (`/embed`) for the CUDA throughput profile.
//!
//! Both produce **dense + sparse** BGE-M3 vectors and use **dynamic batch sizing**
//! (grow toward the VRAM limit; target batch ≥ 32) (§6.5).

use async_trait::async_trait;

use crate::chunker::Chunk;

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

/// In-process ONNX backend (`ort`) — default on Windows / CPU / low-VRAM (§16).
pub struct OrtEmbeddingBackend {
    model_id: String,
    // TODO: session: ort::session::Session, loaded from a pinned BGE-M3 ONNX export.
}

impl OrtEmbeddingBackend {
    pub fn new(model_id: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
        }
    }
}

#[async_trait]
impl EmbeddingBackend for OrtEmbeddingBackend {
    async fn embed_batch(&self, _chunks: &[Chunk]) -> Result<Vec<Embedding>, EmbedError> {
        // TODO: tokenize, run the ORT session (CUDA/DirectML/CPU EP per profile),
        //       pool dense + extract sparse logits, map to Embedding per chunk.
        Err(EmbedError::Permanent("ort embedding not yet implemented".into()))
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
        Err(EmbedError::Transient("gpu-runtime /embed not yet implemented".into()))
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }
}

/// Placeholder backend so the worker skeleton wires up before models exist.
pub struct NoopEmbeddingBackend;

#[async_trait]
impl EmbeddingBackend for NoopEmbeddingBackend {
    async fn embed_batch(&self, chunks: &[Chunk]) -> Result<Vec<Embedding>, EmbedError> {
        Ok(chunks
            .iter()
            .map(|c| Embedding {
                vector_id: c.chunk_id,
                dense: Vec::new(),
                sparse: Vec::new(),
                embed_model: self.model_id().to_string(),
            })
            .collect())
    }

    fn model_id(&self) -> &str {
        "noop"
    }
}
