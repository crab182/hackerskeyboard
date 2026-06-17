#![forbid(unsafe_code)]
//! `diyrag-sync-agent` — LAN multi-instance sync daemon (MASTER_BUILD_SPEC.md §9).
//!
//! Brings up three cooperating loops, all cancellable for graceful drain (§16b.2):
//!   1. **discovery** — mDNS browse + static peers → [`discovery::PeerTable`]
//!      (peers stay `Pending` until admin cert-pinning, §9).
//!   2. **gRPC server** — answers registry diffs / snapshot manifests / snapshot
//!      + blob fetches over **mutual TLS** with cert pinning, rate-limited (§9).
//!   3. **sync loop** — periodically pulls diffs/snapshots/blobs from approved
//!      peers, resolving registry conflicts with the version-vector CRDT
//!      (`crdt::resolve`; NO wall-clock LWW, §9 / §22 #4).
//!
//! Errors use `anyhow` at the binary boundary (spec §19); library types from
//! `diyrag-common` carry the structured envelope.

mod crdt;
mod discovery;
mod grpc;
mod replication;

use anyhow::Context;
use diyrag_common::config::AppConfig;
use diyrag_common::logging;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Typed config (§0/§19): no hardcoded hosts/ports.
    let config =
        AppConfig::load(Some("config/sync-agent.toml")).context("loading sync-agent configuration")?;

    // 2. Structured JSON logging from diyrag-common (§13.1).
    logging::init(&config.observability).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    info!(service = %config.service_name, "starting sync-agent");

    // 3. Peer table seeded from the static peer list (admin cert-pinned out of
    //    band) + persisted `nodes` rows. DECISION: the static list and this
    //    node's id/endpoint/embed-model live under dedicated config keys; not
    //    yet modeled on AppConfig (owned by another agent), so sourced as TODO
    //    rather than hardcoded (§0).
    let peers = discovery::PeerTable::new(Vec::new());
    // TODO: load static peers + self node id/endpoint + local embed model from
    // config (e.g. AppConfig::sync.{node_id,endpoint,static_peers,embed_model}).
    let self_node_id = config.service_name.clone();
    let self_endpoint = config.http.bind_addr.clone();

    // 4. Cooperative cancellation shared by every loop (graceful drain, §16b.2).
    let cancel = CancellationToken::new();

    // 5. gRPC mTLS server state (cert-pinning verifier + governor rate limit, §9).
    let server_state = grpc::SyncServerState::new(peers.clone());
    let bind_addr = config
        .socket_addr()
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    // 6. Spawn the loops.
    let discovery_handle = {
        let (peers, cancel) = (peers.clone(), cancel.clone());
        tokio::spawn(async move {
            if let Err(e) = discovery::run_discovery(peers, self_node_id, self_endpoint, cancel).await {
                warn!(error = %e, "discovery loop exited with error");
            }
        })
    };

    let server_handle = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            if let Err(e) = grpc::serve(server_state, bind_addr, cancel).await {
                warn!(error = %e, "sync gRPC server exited with error");
            }
        })
    };

    // TODO: spawn the periodic sync loop here — for each `peers.approved()`,
    // connect (cert-pinned), ExchangeRegistryDiff (bounded, diffs-vs-manifest),
    // resolve via crdt, then pull missing snapshots/blobs via `replication`.

    info!(%bind_addr, "sync-agent listening (mTLS gRPC)");

    // 7. Wait for a shutdown signal, then cancel and drain (§16b.2 / §14).
    shutdown_signal().await;
    info!("shutdown signal received; draining sync loops");
    cancel.cancel();

    let _ = tokio::join!(discovery_handle, server_handle);
    info!("sync-agent stopped");
    Ok(())
}

/// Resolve when SIGTERM/Ctrl-C is received, for graceful drain (spec §14/§16b.2).
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
