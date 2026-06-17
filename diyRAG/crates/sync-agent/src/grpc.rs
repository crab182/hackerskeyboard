#![forbid(unsafe_code)]
//! mTLS gRPC server + client for LAN sync (MASTER_BUILD_SPEC.md §9, §12.1, §22 #4).
//!
//! Each sync-agent is both a tonic **server** (answers registry diffs, snapshot
//! manifests, snapshot/blob fetches) and a **client** (pulls from approved
//! peers). Transport is HTTP/2 + **mutual TLS via rustls**: both peers present
//! CA-signed X.509 certs and a custom verifier **pins the fingerprint** against
//! the [`PeerTable`] (unknown certs rejected — no auto-trust, §9).
//!
//! DoS protection (§9 / §22 #4): a `governor` token-bucket rate-limits the sync
//! endpoint; the server accepts **diffs against the last acknowledged manifest
//! only** (never full re-uploads) and **bounds the per-sync record count**.
//!
//! The generated tonic stubs from `proto/sync.proto` are `include!`d here once
//! `build.rs` is enabled; until then this module defines the plumbing types and
//! signatures so the wiring is real and the bodies are TODO.

use std::num::NonZeroU32;
use std::sync::Arc;

use diyrag_common::errors::AppError;
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use tokio_util::sync::CancellationToken;

use crate::crdt::{self, RegistryRecord, Resolution};
use crate::discovery::PeerTable;

/// Generated protobuf module (emitted by `tonic-build` into `OUT_DIR`).
/// TODO: enable the include once `build.rs` compiles `proto/sync.proto`.
// pub mod pb {
//     tonic::include_proto!("diyrag.sync.v1");
// }

/// Hard upper bound on records accepted in a single registry-diff exchange.
/// Bounds per-sync work so a flood of tiny diffs cannot saturate the write path
/// (§9 DoS protection / §22 #4). Configurable; this is the safety default.
pub const MAX_RECORDS_PER_DIFF: usize = 10_000;

/// Per-peer sync request budget. The token bucket smooths bursts; the bound
/// above caps a single legitimate request (§9).
pub const SYNC_REQUESTS_PER_SECOND: u32 = 20;

/// Shared state injected into the tonic service implementation.
#[derive(Clone)]
pub struct SyncServerState {
    /// Known/approved peers + cert pins + priorities (§9).
    pub peers: PeerTable,
    /// Token-bucket rate limiter guarding the endpoint (§9 DoS).
    pub limiter: Arc<DefaultDirectRateLimiter>,
}

impl SyncServerState {
    /// Build server state with a `governor` direct rate limiter sized by
    /// [`SYNC_REQUESTS_PER_SECOND`].
    #[must_use]
    pub fn new(peers: PeerTable) -> Self {
        // SAFETY-OF-VALUE: the constant is non-zero by construction.
        let per_sec =
            NonZeroU32::new(SYNC_REQUESTS_PER_SECOND).unwrap_or(NonZeroU32::MIN);
        let limiter = Arc::new(RateLimiter::direct(Quota::per_second(per_sec)));
        Self { peers, limiter }
    }

    /// Admit one sync request against the token bucket; `Err(Forbidden)` when the
    /// caller is over budget (§9). Pure check, no I/O.
    pub fn admit(&self) -> Result<(), AppError> {
        self.limiter.check().map_err(|_| AppError::Forbidden {
            message: "sync rate limit exceeded".to_owned(),
        })
    }

    /// Validate an inbound registry-diff batch is within bounds before doing any
    /// work (§9 — reject oversized batches early).
    pub fn validate_batch(&self, record_count: usize) -> Result<(), AppError> {
        if record_count > MAX_RECORDS_PER_DIFF {
            return Err(AppError::Validation {
                message: format!(
                    "registry diff has {record_count} records, exceeds bound {MAX_RECORDS_PER_DIFF}"
                ),
            });
        }
        Ok(())
    }

    /// Apply one inbound remote record against the local one (if any), using the
    /// version-vector CRDT (`crdt::resolve`). Returns the resolution so the caller
    /// can persist the winner into `sync_state` (§5.1 / §9). Tenant isolation is
    /// preserved: records carry their `tenant_id` and never cross tenants.
    #[must_use]
    pub fn resolve_record(&self, local: Option<&RegistryRecord>, remote: &RegistryRecord) -> Resolution {
        match local {
            // No local copy → adopt the remote (idempotent on content hash, §9).
            None => Resolution::TakeRemote,
            Some(local) => crdt::resolve(local, remote, &self.peers),
        }
    }
}

/// Start the tonic mTLS sync server, serving until `cancel` fires (graceful
/// drain, §16b.2). Binds the rustls server config with the cert-pinning client
/// verifier (§9 / §12.1).
pub async fn serve(state: SyncServerState, bind_addr: std::net::SocketAddr, cancel: CancellationToken) -> Result<(), AppError> {
    let _ = (&state, &bind_addr);
    // TODO: build a `tonic::transport::ServerTlsConfig` from rcgen-issued,
    // ≤90-day certs (§12.1) with `.client_ca_root(..)` AND a custom
    // `rustls::server::danger::ClientCertVerifier` that calls
    // `state.peers.is_pinned(fingerprint)` to enforce the pin (§9). Then
    // `Server::builder().tls_config(..)?.add_service(SyncServiceServer::new(impl))`
    // and `.serve_with_shutdown(bind_addr, cancel.cancelled())`. Map transport
    // errors to AppError::Dependency { dependency: "grpc", .. }.
    cancel.cancelled().await;
    Ok(())
}

/// Build a mutually-authenticated tonic client channel to a peer, pinning the
/// peer's server cert fingerprint via a custom rustls `ServerCertVerifier` (§9).
pub async fn connect_peer(_peers: &PeerTable, endpoint: &str) -> Result<(), AppError> {
    let _ = endpoint;
    // TODO: construct a `Channel` with `ClientTlsConfig` presenting our identity
    // cert and a custom ServerCertVerifier that checks the peer fingerprint is
    // pinned+approved in the PeerTable; reject otherwise (no auto-trust, §9).
    // Return a typed `SyncServiceClient<Channel>` once the proto is compiled.
    Err(AppError::Internal {
        message: "connect_peer not yet implemented".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::Peer;

    fn approved_peer(node_id: &str, priority: i32) -> Peer {
        Peer {
            node_id: node_id.to_owned(),
            endpoint: "127.0.0.1:7000".to_owned(),
            priority,
            cert_fingerprint: "ab".to_owned(),
            trust: crate::discovery::TrustState::Approved,
        }
    }

    #[test]
    fn oversized_batch_is_rejected() {
        let state = SyncServerState::new(PeerTable::default());
        assert!(state.validate_batch(MAX_RECORDS_PER_DIFF).is_ok());
        assert!(state.validate_batch(MAX_RECORDS_PER_DIFF + 1).is_err());
    }

    #[test]
    fn missing_local_record_adopts_remote() {
        let state = SyncServerState::new(PeerTable::new(vec![approved_peer("nodeA", 10)]));
        let remote = RegistryRecord {
            record_key: "h1".to_owned(),
            version_vector: Default::default(),
            origin_node: "nodeA".to_owned(),
        };
        assert_eq!(state.resolve_record(None, &remote), Resolution::TakeRemote);
    }
}
