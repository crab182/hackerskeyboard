#![forbid(unsafe_code)]
//! Content-addressed blob store (spec §5.3).
//!
//! A thin wrapper over the `object_store` crate (S3/MinIO/Azure/GCS/local FS,
//! chosen by config). Original bytes are stored content-addressed so removing a
//! source or unmounting a root never loses content; re-embed/repair is always
//! possible (spec §5.3, §6.6). GC only happens on explicit, audited hard-delete.
//!
//! Content-addressed key layout (spec §5.3): `sha256/{first2}/{sha256}`.

use std::sync::Arc;

use bytes::Bytes;
use object_store::ObjectStore;

use crate::config::BlobConfig;
use crate::errors::AppError;

/// Compute the content-addressed object key for the given bytes (spec §5.3).
///
/// Returns `sha256/{first2}/{sha256_hex}` where `first2` is the first two hex
/// characters of the digest (a cheap fan-out prefix).
#[must_use]
pub fn content_key(bytes: &[u8]) -> String {
    let digest = crate::ids::sha256_hex(bytes);
    content_key_from_hex(&digest)
}

/// Build the content-addressed key from an already-computed sha256 hex digest.
#[must_use]
pub fn content_key_from_hex(sha256_hex: &str) -> String {
    let prefix = &sha256_hex[..sha256_hex.len().min(2)];
    format!("sha256/{prefix}/{sha256_hex}")
}

/// A reference to a stored blob (its content-addressed key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRef(pub String);

/// Wrapper holding a configured [`ObjectStore`] backend.
#[derive(Clone)]
pub struct BlobStore {
    inner: Arc<dyn ObjectStore>,
}

impl BlobStore {
    /// Build a [`BlobStore`] from [`BlobConfig`], selecting the backend by
    /// `cfg.backend` (`s3`, `local`, `memory`).
    pub fn connect(cfg: &BlobConfig) -> Result<Self, AppError> {
        // TODO: match cfg.backend:
        //   "s3"     -> object_store::aws::AmazonS3Builder (bucket/endpoint/region)
        //   "local"  -> object_store::local::LocalFileSystem::new_with_prefix(bucket)
        //   "memory" -> object_store::memory::InMemory::new()
        // Map builder errors to AppError::Config / AppError::Dependency.
        let _ = cfg;
        Err(AppError::Internal {
            message: "BlobStore::connect not yet implemented".to_owned(),
        })
    }

    /// Construct directly from a prebuilt object store (useful for tests).
    #[must_use]
    pub fn from_store(inner: Arc<dyn ObjectStore>) -> Self {
        Self { inner }
    }

    /// Store bytes content-addressed; returns the [`BlobRef`]. Idempotent: the
    /// same bytes always map to the same key (spec §5.3).
    pub async fn put_content_addressed(&self, bytes: Bytes) -> Result<BlobRef, AppError> {
        let key = content_key(&bytes);
        let path = object_store::path::Path::from(key.clone());
        self.inner
            .put(&path, bytes.into())
            .await
            .map_err(|e| AppError::Dependency {
                dependency: "blob".to_owned(),
                message: e.to_string(),
            })?;
        Ok(BlobRef(key))
    }

    /// Fetch the bytes for a [`BlobRef`].
    pub async fn get(&self, blob: &BlobRef) -> Result<Bytes, AppError> {
        let path = object_store::path::Path::from(blob.0.clone());
        let res = self
            .inner
            .get(&path)
            .await
            .map_err(|e| AppError::Dependency {
                dependency: "blob".to_owned(),
                message: e.to_string(),
            })?;
        res.bytes().await.map_err(|e| AppError::Dependency {
            dependency: "blob".to_owned(),
            message: e.to_string(),
        })
    }

    /// Whether a blob exists (used for sync "fetch missing by hash", spec §9).
    pub async fn exists(&self, blob: &BlobRef) -> Result<bool, AppError> {
        let path = object_store::path::Path::from(blob.0.clone());
        match self.inner.head(&path).await {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(e) => Err(AppError::Dependency {
                dependency: "blob".to_owned(),
                message: e.to_string(),
            }),
        }
    }
}
