#![forbid(unsafe_code)]
//! LAN peer discovery + cert pinning (MASTER_BUILD_SPEC.md §9, §22 #4).
//!
//! Peers are discovered two ways:
//!   1. **mDNS** (`mdns-sd`) over the LAN, advertising/browsing the diyRAG sync
//!      service type, and
//!   2. a **static peer list** from config (fallback for segmented LANs).
//!
//! Discovery alone grants **no trust**: a newly-seen peer is `Pending` until an
//! **admin pins its cert fingerprint** (matching a `nodes` row, spec §5.1). Only
//! `Approved` peers may sync; the rustls `ServerCertVerifier`/`ClientCertVerifier`
//! in `grpc.rs` enforces the pin on every connection (no auto-trust, §9).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use diyrag_common::errors::AppError;
use tokio_util::sync::CancellationToken;

use crate::crdt::NodePriority;

/// The mDNS service type diyRAG sync agents advertise/browse on the LAN.
/// DECISION: a dedicated `_diyrag-sync._tcp` type avoids colliding with other
/// services; the instance name carries the node id. (Reversible via config.)
pub const MDNS_SERVICE_TYPE: &str = "_diyrag-sync._tcp.local.";

/// Trust state of a discovered peer. Discovery never auto-trusts (§9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustState {
    /// Seen on the LAN or in config but not yet admin-approved — cannot sync.
    Pending,
    /// Admin-pinned cert fingerprint present in `nodes`; eligible to sync.
    Approved,
    /// Explicitly rejected/revoked; connections refused.
    Rejected,
}

/// A known LAN peer (mirrors the `nodes` row, spec §5.1).
#[derive(Debug, Clone)]
pub struct Peer {
    /// Stable node id (also the mDNS instance name and tiebreak key, §9).
    pub node_id: String,
    /// `host:port` gRPC endpoint.
    pub endpoint: String,
    /// Sync priority; higher wins the CRDT tiebreak (§9 / `crdt.rs`).
    pub priority: i32,
    /// SHA-256 fingerprint of the peer's pinned X.509 cert (hex). Empty until
    /// an admin pins it.
    pub cert_fingerprint: String,
    /// Whether this peer may participate in sync.
    pub trust: TrustState,
}

/// Thread-safe table of known peers, shared by the discovery loop, the gRPC
/// server (to authorize inbound connections) and the CRDT resolver (priorities).
#[derive(Clone, Default)]
pub struct PeerTable {
    inner: Arc<RwLock<HashMap<String, Peer>>>,
}

impl PeerTable {
    /// Seed the table from the configured static peer list (already cert-pinned
    /// by an admin out of band) plus the persisted `nodes` rows.
    #[must_use]
    pub fn new(static_peers: Vec<Peer>) -> Self {
        let map = static_peers.into_iter().map(|p| (p.node_id.clone(), p)).collect();
        Self {
            inner: Arc::new(RwLock::new(map)),
        }
    }

    /// Record (or update) a peer seen via mDNS. New peers land as [`TrustState::Pending`]
    /// and MUST be admin-approved before they can sync (§9). An existing peer's
    /// trust/fingerprint is never silently upgraded here.
    pub fn observe(&self, node_id: &str, endpoint: &str) {
        // TODO: handle a poisoned lock without unwrap on the runtime path (§19):
        // log + skip the update rather than panic.
        if let Ok(mut map) = self.inner.write() {
            map.entry(node_id.to_owned())
                .and_modify(|p| p.endpoint = endpoint.to_owned())
                .or_insert_with(|| Peer {
                    node_id: node_id.to_owned(),
                    endpoint: endpoint.to_owned(),
                    priority: 0,
                    cert_fingerprint: String::new(),
                    trust: TrustState::Pending,
                });
        }
    }

    /// Admin action: pin a peer's cert fingerprint and approve it for sync
    /// (writes the corresponding `nodes` row elsewhere). Privileged + audited at
    /// the API layer (§12.6 / §12.9).
    pub fn approve(&self, node_id: &str, cert_fingerprint: &str, priority: i32) -> Result<(), AppError> {
        let mut map = self
            .inner
            .write()
            .map_err(|_| AppError::Internal { message: "peer table lock poisoned".to_owned() })?;
        let peer = map.get_mut(node_id).ok_or_else(|| AppError::NotFound {
            resource: format!("peer {node_id}"),
        })?;
        peer.cert_fingerprint = cert_fingerprint.to_owned();
        peer.priority = priority;
        peer.trust = TrustState::Approved;
        Ok(())
    }

    /// Is the given cert fingerprint pinned to an `Approved` peer? The rustls
    /// verifier in `grpc.rs` calls this on every TLS handshake (§9 / §12.1).
    #[must_use]
    pub fn is_pinned(&self, cert_fingerprint: &str) -> bool {
        self.inner.read().is_ok_and(|map| {
            map.values()
                .any(|p| p.trust == TrustState::Approved && p.cert_fingerprint == cert_fingerprint)
        })
    }

    /// Snapshot of all peers eligible to sync (for the diff/snapshot loops).
    #[must_use]
    pub fn approved(&self) -> Vec<Peer> {
        self.inner
            .read()
            .map(|map| map.values().filter(|p| p.trust == TrustState::Approved).cloned().collect())
            .unwrap_or_default()
    }
}

/// The CRDT resolver reads node priorities straight from the peer table (§9).
impl NodePriority for PeerTable {
    fn priority_of(&self, node_id: &str) -> i32 {
        self.inner
            .read()
            .ok()
            .and_then(|map| map.get(node_id).map(|p| p.priority))
            // Unknown/unenrolled nodes get the lowest priority so they can never
            // win a tiebreak (defense-in-depth with cert pinning).
            .unwrap_or(i32::MIN)
    }
}

/// Run the mDNS browse loop until cancelled, feeding discovered peers into the
/// shared [`PeerTable`] as `Pending` (§9). Advertises this node too.
pub async fn run_discovery(
    table: PeerTable,
    self_node_id: String,
    self_endpoint: String,
    cancel: CancellationToken,
) -> Result<(), AppError> {
    let _ = (&table, &self_node_id, &self_endpoint);
    // TODO: build an `mdns_sd::ServiceDaemon`, register this node as a
    // `ServiceInfo` under MDNS_SERVICE_TYPE (instance = self_node_id, the gRPC
    // port from self_endpoint), then `browse(MDNS_SERVICE_TYPE)` and, on each
    // `ServiceEvent::ServiceResolved`, call `table.observe(node_id, endpoint)`.
    // Select on `cancel.cancelled()` for graceful shutdown (§16b.2). Map daemon
    // errors to AppError::Dependency { dependency: "mdns", .. }.
    cancel.cancelled().await;
    Ok(())
}
