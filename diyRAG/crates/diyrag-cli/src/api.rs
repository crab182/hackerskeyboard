//! REST API client used by the `node`/`ingest`/`batch`/`query` subcommands
//! (MASTER_BUILD_SPEC.md §16b.1).
//!
//! These commands drive the running stack headlessly over the `api-gateway`
//! REST surface (§11), which is ideal for unraid where there is no desktop GUI.
//! Auth is an **API key** sourced from the environment or config — **never** a
//! committed secret (§0 / §12.2). The key is sent as a bearer token and is
//! never logged.

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};

/// Environment variables the CLI reads for connection + auth (12-factor, §16b.1).
const ENV_BASE_URL: &str = "DIYRAG_API_URL";
const ENV_API_KEY: &str = "DIYRAG_API_KEY";

/// Default API base when `DIYRAG_API_URL` is unset (local node over TLS, §12.1).
const DEFAULT_BASE_URL: &str = "https://127.0.0.1:8443";

/// A thin REST client bound to one node's `api-gateway`.
pub struct ApiClient {
    http: reqwest::Client,
    base_url: String,
}

impl ApiClient {
    /// Build a client from the environment (`DIYRAG_API_URL`, `DIYRAG_API_KEY`).
    ///
    /// DECISION: the API key is read from env/config only — there is no `--key`
    /// flag, to keep secrets out of shell history and process listings (§12.2).
    pub fn from_env() -> Result<Self> {
        let base_url =
            std::env::var(ENV_BASE_URL).unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());

        let mut headers = HeaderMap::new();
        if let Ok(key) = std::env::var(ENV_API_KEY) {
            // `Bearer <key>`; mark sensitive so reqwest/tracing won't echo it.
            let mut value = HeaderValue::from_str(&format!("Bearer {key}"))
                .context("building Authorization header from DIYRAG_API_KEY")?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        } else {
            tracing::warn!(
                "no {ENV_API_KEY} set; requests will be unauthenticated and likely rejected"
            );
        }

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .context("building reqwest client")?;

        Ok(Self { http, base_url })
    }

    /// Join the base URL with a `/api/v1`-relative path.
    fn url(&self, path: &str) -> String {
        format!("{}/api/v1/{}", self.base_url.trim_end_matches('/'), path.trim_start_matches('/'))
    }

    // --- node (§9 / §11.2) ------------------------------------------------

    /// `GET /admin/runtime` — this node's runtime/health status (§11.2).
    pub async fn node_status(&self) -> Result<()> {
        // TODO: GET self.url("admin/runtime"); deserialize into a typed struct
        // (service uptime, restart count, container health) and render.
        let _ = self.http.get(self.url("admin/runtime"));
        tracing::info!("TODO: GET /api/v1/admin/runtime and render node status");
        Ok(())
    }

    /// List known LAN peers + sync state (§9).
    pub async fn node_peers(&self) -> Result<()> {
        // TODO: GET self.url("admin/nodes"); render peers + version-vector lag.
        tracing::info!("TODO: GET /api/v1/admin/nodes and render peers");
        Ok(())
    }

    /// Trigger a Qdrant snapshot (the unit of vector replication, §9).
    pub async fn node_snapshot(&self) -> Result<()> {
        // TODO: POST a snapshot request; print the resulting snapshot id/path.
        tracing::info!("TODO: POST snapshot request");
        Ok(())
    }

    /// Restore from a snapshot (§9).
    pub async fn node_restore(&self, snapshot: &str) -> Result<()> {
        // TODO: POST restore { snapshot } and poll until applied.
        tracing::info!(%snapshot, "TODO: POST snapshot restore");
        Ok(())
    }

    // --- ingest (§6) ------------------------------------------------------

    /// Trigger ingest of a path; optionally register it as a watched root (§6.1).
    pub async fn ingest(&self, path: &str, watch: bool) -> Result<()> {
        // TODO:
        // * if `watch`: POST /api/v1/files/roots { path, watch: true, globs }.
        // * else: POST /api/v1/ingestion/trigger { path }.
        // Then print the returned job/document id.
        tracing::info!(%path, watch, "TODO: POST ingest/roots request");
        Ok(())
    }

    // --- batch (§6.7) -----------------------------------------------------

    /// Submit an archive for batch ingestion; returns immediately with a job id.
    pub async fn batch_submit(&self, archive: &str) -> Result<()> {
        // TODO: multipart POST /api/v1/batch/submit with the archive stream
        // (reqwest "stream" feature); print job_id + the status URL (§6.7).
        tracing::info!(%archive, "TODO: POST /api/v1/batch/submit (multipart)");
        Ok(())
    }

    // --- query (§7) -------------------------------------------------------

    /// Run a search (`answer=false`) or grounded answer (`answer=true`) query.
    pub async fn query(&self, query: &str, k: usize, answer: bool) -> Result<()> {
        // TODO:
        // * answer: POST /api/v1/query/answer { query, k } → cited answer (§7.2).
        // * else:   POST /api/v1/query/search { query, k } → reranked chunks.
        // Render the structured envelope, surfacing any reference_code (§11.3).
        let endpoint = if answer { "query/answer" } else { "query/search" };
        tracing::info!(%query, k, answer, "TODO: POST /api/v1/{}", endpoint);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_join_is_clean() {
        let client = ApiClient {
            http: reqwest::Client::new(),
            base_url: "https://host:8443/".to_string(),
        };
        assert_eq!(
            client.url("/query/search"),
            "https://host:8443/api/v1/query/search"
        );
        assert_eq!(
            client.url("admin/runtime"),
            "https://host:8443/api/v1/admin/runtime"
        );
    }
}
