#![forbid(unsafe_code)]
//! Cross-encoder reranking (spec §7.1).
//!
//! After hybrid search returns the top-`k₀` (≈40) candidates, a
//! `bge-reranker-v2-m3` cross-encoder scores each `(query, chunk)` pair and we
//! keep the top-`k` (8–12). Two interchangeable backends behind one interface
//! (spec §16): an **in-process ONNX** model via `ort`, or the **gpu-runtime
//! HTTP** `/rerank` endpoint. The default is selected by config.

use async_trait::async_trait;
use diyrag_common::config::AppConfig;
use diyrag_common::errors::AppError;

use crate::SearchHit;

/// Swappable reranker backend (spec §16: `EmbeddingBackend`-style trait).
#[async_trait]
pub trait RerankBackend: Send + Sync {
    /// Score `(query, candidate.text)` pairs and return relevance scores in the
    /// same order as the input candidates.
    async fn score(&self, query: &str, candidates: &[SearchHit]) -> Result<Vec<f32>, AppError>;
}

/// The reranker: holds the selected backend and the final-`k` policy.
pub struct Reranker {
    backend: Box<dyn RerankBackend>,
}

impl Reranker {
    /// Initialize the reranker from config (spec §16 backend selection).
    ///
    /// DECISION: defaults to the in-proc ONNX backend so the common path is
    /// Python-free (spec §21); a config flag selects the gpu-runtime HTTP backend
    /// on throughput nodes. Both implement [`RerankBackend`].
    pub async fn init(_config: &AppConfig) -> anyhow::Result<Self> {
        // TODO: read a config key to choose OnnxReranker (load the ONNX model via
        //       ort + the HF tokenizer) vs HttpReranker (gpu-runtime base URL).
        Ok(Self {
            backend: Box::new(OnnxReranker::placeholder()),
        })
    }

    /// Rerank `candidates` and return the top-`k` by descending score (spec §7.1).
    ///
    /// `k` is clamped into the spec's 8–12 envelope is the *typical* range but we
    /// honor the caller's requested `k` (validated 1..=100 upstream).
    pub async fn rerank(
        &self,
        query: &str,
        mut candidates: Vec<SearchHit>,
        k: usize,
    ) -> Result<Vec<SearchHit>, AppError> {
        if candidates.is_empty() {
            return Ok(candidates);
        }
        let scores = self.backend.score(query, &candidates).await?;
        if scores.len() != candidates.len() {
            return Err(AppError::Internal {
                message: "reranker returned a score count != candidate count".to_owned(),
            });
        }
        // Overwrite the fusion score with the cross-encoder score, then sort.
        for (hit, score) in candidates.iter_mut().zip(scores) {
            hit.score = score;
        }
        candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        candidates.truncate(k);
        Ok(candidates)
    }
}

/// In-process ONNX cross-encoder reranker via `ort` (spec §7.1 default, §16).
pub struct OnnxReranker {
    // TODO: hold the `ort::session::Session` and a `tokenizers::Tokenizer`.
}

impl OnnxReranker {
    /// Placeholder constructor used by the scaffold before the model is wired.
    #[must_use]
    fn placeholder() -> Self {
        Self {}
    }

    /// Load the bge-reranker ONNX model + tokenizer from disk (paths from config).
    pub fn load(_model_path: &str, _tokenizer_path: &str) -> Result<Self, AppError> {
        // TODO: ort::session::Session::builder()?.commit_from_file(model_path)?;
        //       Tokenizer::from_file(tokenizer_path)?. Choose the CUDA/DirectML EP
        //       with a CPU fallback (spec §16 / §14 GPU failsafe).
        Err(AppError::Internal {
            message: "OnnxReranker::load not yet implemented".to_owned(),
        })
    }
}

#[async_trait]
impl RerankBackend for OnnxReranker {
    async fn score(&self, _query: &str, candidates: &[SearchHit]) -> Result<Vec<f32>, AppError> {
        // TODO: for each candidate, tokenize the (query, text) pair, run the
        //       cross-encoder session, and read the relevance logit. Batch to the
        //       VRAM limit (spec §6.5/§16). On CUDA OOM/thermal, fall back to CPU
        //       and emit HW-OOM/HW-THERMAL-LIMIT (spec §14).
        let _ = candidates;
        Err(AppError::Internal {
            message: "OnnxReranker::score not yet implemented".to_owned(),
        })
    }
}

/// HTTP reranker delegating to the `gpu-runtime` `/rerank` endpoint (spec §16).
pub struct HttpReranker {
    http: reqwest::Client,
    base_url: String,
}

impl HttpReranker {
    /// Construct an HTTP reranker pointed at the gpu-runtime base URL.
    #[must_use]
    pub fn new(http: reqwest::Client, base_url: String) -> Self {
        Self { http, base_url }
    }
}

#[async_trait]
impl RerankBackend for HttpReranker {
    async fn score(&self, _query: &str, candidates: &[SearchHit]) -> Result<Vec<f32>, AppError> {
        // TODO: POST {base_url}/rerank { query, passages: [...] } and parse the
        //       per-passage scores; map transport failures to AppError::Dependency
        //       { dependency: "gpu-runtime", .. } (spec §14).
        let _ = (&self.http, &self.base_url, candidates);
        Err(AppError::Dependency {
            dependency: "gpu-runtime".to_owned(),
            message: "HttpReranker::score not yet implemented".to_owned(),
        })
    }
}
