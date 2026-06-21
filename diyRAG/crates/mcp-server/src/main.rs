#![forbid(unsafe_code)]
//! `diyrag-mcp-server` — Model Context Protocol server (MASTER_BUILD_SPEC.md §8).
//!
//! Exposes the RAG platform to LLM clients via the official Rust MCP SDK
//! (`rmcp`) over two transports:
//!   * **Streamable HTTP (stateless)** for remote clients (horizontal scale, §8),
//!     served on an Axum listener with CORS for browser MCP clients,
//!   * **stdio** for local clients (Claude Desktop, Cursor, §8).
//!
//! It is a **thin adapter** over `core-api` enforcing the **same RBAC + tenant
//! isolation** (§8 / §12). SSE transport is deprecated and intentionally absent.
//!
//! Transport selection is driven by config/flags (no hardcoded choice): an
//! `--stdio` flag (or `DIYRAG_MCP__TRANSPORT=stdio`) runs the stdio server;
//! otherwise the Streamable HTTP server binds the configured address.

mod auth;
mod resources;
mod tools;

use anyhow::Context;
use diyrag_common::config::AppConfig;
use diyrag_common::logging;
use tracing::info;

use crate::tools::RagTools;

/// Which transport to serve (§8). Resolved from config/flags, never hardcoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    /// Streamable HTTP, stateless (remote clients).
    StreamableHttp,
    /// stdio (local clients).
    Stdio,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Container HEALTHCHECK form (`mcp-server healthcheck`): liveness only — the
    // Streamable HTTP server (and its /healthz) is still scaffold, so the probe
    // can't be HTTP yet. Upgrade to `http_healthcheck` once /healthz is served.
    if diyrag_common::health::is_healthcheck_invocation() {
        std::process::exit(diyrag_common::health::liveness_ok());
    }

    // 1. Typed config (§0/§19).
    let config = AppConfig::load(Some("config/mcp-server.toml"))
        .context("loading mcp-server configuration")?;

    // 2. Logging. DECISION: under the stdio transport, logs MUST NOT pollute
    //    stdout (it is the MCP byte stream). The common logger writes to the
    //    tracing subscriber; for stdio we rely on it targeting stderr/files
    //    (configured via observability), never stdout. TODO: assert stderr sink
    //    when transport == Stdio.
    logging::init(&config.observability).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    info!(service = %config.service_name, "starting mcp-server");

    // 3. Resolve transport from config/flags (§8). Placeholder default keeps the
    //    scaffold honest; real value comes from a config key + a `--stdio` flag.
    let transport = resolve_transport();

    // 4. Build the tool surface adapter (thin client of core-api, §8).
    //    DECISION: core-api base URL comes from a config key (no hardcoded host,
    //    §0); placeholder until AppConfig grows an `upstreams` section.
    let tools = RagTools::new(
        "https://core-api:8081".to_owned(), // TODO: config.upstreams.core_api
        reqwest::Client::builder()
            .build()
            .context("building core-api adapter client")?,
    );

    match transport {
        Transport::Stdio => serve_stdio(tools).await,
        Transport::StreamableHttp => {
            let addr = config
                .socket_addr()
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            serve_streamable_http(tools, addr).await
        }
    }
}

/// Resolve the transport from flags/env (§8). No hardcoded transport.
fn resolve_transport() -> Transport {
    // TODO: read a `--stdio` clap flag or DIYRAG_MCP__TRANSPORT; default to
    // Streamable HTTP for the scalable remote path (§8).
    if std::env::args().any(|a| a == "--stdio") {
        Transport::Stdio
    } else {
        Transport::StreamableHttp
    }
}

/// Serve the MCP server over stdio for local clients (§8).
async fn serve_stdio(tools: RagTools) -> anyhow::Result<()> {
    let _ = tools;
    // TODO (rmcp 0.16): build the ServerHandler from `tools` (its #[tool_router])
    // plus `resources` handlers, then `service.serve(rmcp::transport::io::stdio())`
    // and `.waiting().await`. Auth for stdio is the local mTLS/identity path
    // (§8 / auth::AuthMethod::Mtls).
    info!("mcp-server stdio transport ready");
    // Block until EOF / shutdown; placeholder keeps the binary well-formed.
    shutdown_signal().await;
    Ok(())
}

/// Serve the MCP server over Streamable HTTP (stateless) for remote clients (§8).
async fn serve_streamable_http(tools: RagTools, addr: std::net::SocketAddr) -> anyhow::Result<()> {
    let _ = tools;
    // TODO (rmcp 0.16, feature `transport-streamable-http-server`): build a
    // `StreamableHttpService` in STATELESS mode wrapping the ServerHandler, mount
    // it on an Axum router, add a CORS layer for browser MCP clients (§8), wrap
    // with OAuth 2.1 auth middleware (auth::authenticate → server-derived tenant,
    // §12.7), expose /healthz + /readyz, and `axum::serve(...).with_graceful_shutdown(...)`.
    info!(%addr, "mcp-server Streamable HTTP transport ready (stateless)");
    shutdown_signal().await;
    Ok(())
}

/// Resolve when SIGTERM/Ctrl-C is received, for graceful drain (spec §14/§16b.2).
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    info!("shutdown signal received; draining mcp-server");
}
