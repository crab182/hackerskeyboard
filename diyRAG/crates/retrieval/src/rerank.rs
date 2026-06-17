#![forbid(unsafe_code)]
//! Cross-encoder reranking (spec §7.1).
//!
//! After hybrid search returns the top-`k₀` (≈40) candidates, a
//! `bge-reranker-v2-m3` cross-encoder scores each `(query, chunk)` pair and we
//! keep the top-`k` (8–12). Two interchangeable backends behind one interface
//! (spec §16): an **in-process candle** model (pure-Rust, CPU by default;
//! CUDA/Metal via opt-in cargo features on GPU nodes), or the **gpu-runtime
//! HTTP** `/rerank` endpoint. The default is selected by config.
//!
//! `bge-reranker-v2-m3` is an XLM-RoBERTa sequence-classifier producing a single
//! relevance logit per `(query, passage)` pair; we load it with candle's
//! [`XLMRobertaForSequenceClassification`] from local safetensors (no Hub fetch —
//! offline / LAN-only default; ADR-0009).

use std::path::Path;

use async_trait::async_trait;
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::xlm_roberta::{Config, XLMRobertaForSequenceClassification};
use diyrag_common::config::AppConfig;
use diyrag_common::errors::AppError;
use tokenizers::Tokenizer;

use crate::SearchHit;

/// Map a candle inference error to the structured app error envelope (§11.3).
fn candle_err(e: candle_core::Error) -> AppError {
    AppError::Internal {
        message: format!("candle reranker inference: {e}"),
    }
}

/// Build a `(1, seq_len)` `u32` tensor from a token-id / mask / type-id slice.
fn row_u32(values: &[u32], device: &Device) -> Result<Tensor, AppError> {
    Tensor::new(values, device)
        .and_then(|t| t.unsqueeze(0))
        .map_err(candle_err)
}

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
    /// DECISION: defaults to the in-proc candle backend so the common path is
    /// Python-free (spec §21); a config flag selects the gpu-runtime HTTP backend
    /// on throughput nodes. Both implement [`RerankBackend`].
    ///
    /// The model directory is read from `DIYRAG_RERANK_MODEL_DIR` as a stopgap
    /// until the key is promoted into [`AppConfig`]. When it is unset the service
    /// still boots with an *unloaded* candle backend that returns a clear error if
    /// reranking is attempted — keeping nodes bootable without a model present
    /// (offline / LAN-only default; spec §16/§21).
    pub async fn init(_config: &AppConfig) -> anyhow::Result<Self> {
        let backend: Box<dyn RerankBackend> = match std::env::var("DIYRAG_RERANK_MODEL_DIR") {
            Ok(dir) if !dir.trim().is_empty() => Box::new(CandleReranker::load(Path::new(&dir))?),
            _ => Box::new(CandleReranker::unloaded()),
        };
        Ok(Self { backend })
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

/// In-process candle cross-encoder reranker (spec §7.1 default, §16).
///
/// Loads `bge-reranker-v2-m3` (an XLM-RoBERTa sequence-classifier) from a local
/// model directory containing `config.json`, `tokenizer.json`, and
/// `model.safetensors`. The forward pass produces one relevance logit per
/// `(query, passage)` pair. CPU is the default device; CUDA/Metal are enabled via
/// candle cargo features on GPU nodes (ADR-0009).
pub struct CandleReranker {
    device: Device,
    /// `None` until a model directory is configured (see [`Reranker::init`]).
    model: Option<XLMRobertaForSequenceClassification>,
    tokenizer: Option<Tokenizer>,
}

impl CandleReranker {
    /// An unloaded backend: the service boots, but scoring returns a clear error
    /// until `DIYRAG_RERANK_MODEL_DIR` is set (offline / LAN-only default).
    #[must_use]
    fn unloaded() -> Self {
        Self {
            device: Device::Cpu,
            model: None,
            tokenizer: None,
        }
    }

    /// Load the bge-reranker model + tokenizer from a local directory.
    ///
    /// Uses the **safe** (non-mmap) safetensors loader so the crate keeps
    /// `#![forbid(unsafe_code)]`. No network access — the directory is expected to
    /// be vendored into the image / model-cache (spec §16, ADR-0009).
    pub fn load(model_dir: &Path) -> Result<Self, AppError> {
        // CPU by default; CUDA/Metal are opt-in candle cargo features on GPU nodes
        // (with a CPU fallback as the §14 GPU failsafe). Kept CPU here so every
        // node — including the Windows service and unraid — builds and boots.
        let device = Device::Cpu;

        let cfg_bytes =
            std::fs::read(model_dir.join("config.json")).map_err(|e| AppError::Internal {
                message: format!("read reranker config.json: {e}"),
            })?;
        let cfg: Config = serde_json::from_slice(&cfg_bytes).map_err(|e| AppError::Internal {
            message: format!("parse reranker config.json: {e}"),
        })?;

        let tokenizer = Tokenizer::from_file(model_dir.join("tokenizer.json")).map_err(|e| {
            AppError::Internal {
                message: format!("load reranker tokenizer.json: {e}"),
            }
        })?;

        let tensors = candle_core::safetensors::load(model_dir.join("model.safetensors"), &device)
            .map_err(candle_err)?;
        let vb = VarBuilder::from_tensors(tensors, DType::F32, &device);
        // bge-reranker-v2-m3 emits a single relevance logit per pair (num_labels=1).
        let model = XLMRobertaForSequenceClassification::new(1, &cfg, vb).map_err(candle_err)?;

        Ok(Self {
            device,
            model: Some(model),
            tokenizer: Some(tokenizer),
        })
    }
}

#[async_trait]
impl RerankBackend for CandleReranker {
    async fn score(&self, query: &str, candidates: &[SearchHit]) -> Result<Vec<f32>, AppError> {
        let (model, tokenizer) = match (&self.model, &self.tokenizer) {
            (Some(m), Some(t)) => (m, t),
            _ => {
                return Err(AppError::Internal {
                    message: "reranker model not loaded; set DIYRAG_RERANK_MODEL_DIR to a \
                              local bge-reranker-v2-m3 directory"
                        .to_owned(),
                })
            }
        };

        // TODO(perf, §6.5): batch pairs up to the VRAM limit instead of one-by-one.
        // On CUDA OOM/thermal, fall back to CPU and emit HW-OOM/HW-THERMAL (spec §14).
        let mut scores = Vec::with_capacity(candidates.len());
        for hit in candidates {
            let enc = tokenizer
                .encode((query, hit.text.as_str()), true)
                .map_err(|e| AppError::Internal {
                    message: format!("reranker tokenize: {e}"),
                })?;
            let input_ids = row_u32(enc.get_ids(), &self.device)?;
            let attention_mask = row_u32(enc.get_attention_mask(), &self.device)?;
            let token_type_ids = row_u32(enc.get_type_ids(), &self.device)?;

            let logits = model
                .forward(&input_ids, &attention_mask, &token_type_ids)
                .map_err(candle_err)?;
            let values = logits
                .flatten_all()
                .and_then(|t| t.to_vec1::<f32>())
                .map_err(candle_err)?;
            scores.push(values.first().copied().unwrap_or(f32::MIN));
        }
        Ok(scores)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(text: &str, score: f32) -> SearchHit {
        SearchHit {
            chunk_id: uuid::Uuid::now_v7(),
            document_id: uuid::Uuid::now_v7(),
            score,
            page_number: None,
            text: text.to_owned(),
        }
    }

    /// An unloaded candle reranker boots but fails scoring with a clear error
    /// (offline / LAN-only default; spec §16/§21).
    #[tokio::test]
    async fn unloaded_reranker_errors_clearly() {
        let backend = CandleReranker::unloaded();
        let err = backend.score("q", &[hit("a", 0.0)]).await.unwrap_err();
        assert!(matches!(err, AppError::Internal { .. }), "got {err:?}");
    }

    /// Reranking an empty candidate set is a no-op (no backend call, no panic).
    #[tokio::test]
    async fn rerank_empty_is_noop() {
        let reranker = Reranker {
            backend: Box::new(CandleReranker::unloaded()),
        };
        let out = reranker.rerank("q", Vec::new(), 5).await.unwrap();
        assert!(out.is_empty());
    }
}
