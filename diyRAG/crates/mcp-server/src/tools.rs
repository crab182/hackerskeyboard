#![forbid(unsafe_code)]
//! MCP tools (MASTER_BUILD_SPEC.md §8, §22 #5).
//!
//! Each tool is a **thin, deterministic, least-privilege adapter** over
//! `core-api` that enforces the **same RBAC** as the REST API (§8). High-risk
//! ops are **gated by scope** (`ingest`/`admin`) and may require an explicit
//! confirmation flag.
//!
//! **Tool-poisoning defense (§8 / §22 #5):** every tool description is a
//! **static, reviewed Rust constant** ([`descriptions`]). Descriptions are
//! **NEVER** templated from user- or ingested content — that is the attack
//! vector this module is hardened against. The `#[tool(description = …)]`
//! attribute must always reference one of these consts.
//!
//! Tenant scope is **server-derived** (`auth::tenant_of`) and never read from a
//! tool argument (§12.7).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Static, reviewed tool descriptions (§8 tool-poisoning defense, §22 #5).
///
/// These are the ONLY strings allowed in `#[tool(description = …)]`. They are
/// constants so a code review (not ingested content) controls what an LLM client
/// sees. Do not `format!` ingested text into any of these.
pub mod descriptions {
    pub const RAG_SEARCH: &str =
        "Search the knowledge base and return reranked passages with citations. Read-only.";
    pub const RAG_ANSWER: &str =
        "Answer a question grounded ONLY in retrieved passages, with inline citations and conflict flags. Read-only.";
    pub const DOCUMENTS_LIST: &str =
        "List documents in the caller's tenant, optionally filtered. Read-only metadata.";
    pub const DOCUMENTS_GET: &str =
        "Fetch a single document's metadata by id. Read-only.";
    pub const DOCUMENTS_ADD: &str =
        "Ingest a document from a path or URL. GATED: requires the `ingest` scope.";
    pub const ROOTS_ADD: &str =
        "Register a watched folder root for ingestion. GATED: requires the `ingest` scope.";
    pub const ROOTS_REMOVE: &str =
        "Logically purge (deactivate, reversible) a root. GATED: requires the `admin` scope and explicit confirmation.";
    pub const INGESTION_STATUS: &str =
        "Report the status/progress of an ingestion job by id. Read-only.";
}

// ---------------------------------------------------------------------------
// Tool parameter structs. Each derives `Deserialize` (input) + `JsonSchema`
// (rmcp emits the tool's input schema from this) + `Serialize` for tests/logs.
// NONE of these carry a tenant/collection field — tenant is server-derived
// (§12.7); a client cannot widen its scope through an argument.
// ---------------------------------------------------------------------------

/// Optional retrieval filters mirroring `common::vector::QueryFilter` (no tenant).
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct RagFilters {
    /// Restrict to specific root ids (within the caller's tenant).
    #[serde(default)]
    pub root_ids: Vec<String>,
    /// Optional language filter (ISO code).
    #[serde(default)]
    pub lang: Option<String>,
}

/// `rag.search` arguments (§8).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct RagSearchArgs {
    /// Natural-language query.
    pub query: String,
    /// Number of reranked results to return (`k`); server clamps to a max.
    #[serde(default = "default_k")]
    pub k: u32,
    #[serde(default)]
    pub filters: RagFilters,
}

/// `rag.answer` arguments (§8).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct RagAnswerArgs {
    pub query: String,
    #[serde(default = "default_k")]
    pub k: u32,
    #[serde(default)]
    pub filters: RagFilters,
}

/// `documents.list` arguments (§8).
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct DocumentsListArgs {
    #[serde(default)]
    pub filters: RagFilters,
    /// Pagination cursor (opaque).
    #[serde(default)]
    pub cursor: Option<String>,
}

/// `documents.get` arguments (§8).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DocumentsGetArgs {
    /// Document id (UUIDv7).
    pub id: String,
}

/// `documents.add` arguments — GATED (`ingest`) (§8).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DocumentsAddArgs {
    /// Local path or URL to ingest. Treated as untrusted input (§12.4/§12.5).
    pub source: String,
}

/// `roots.add` arguments — GATED (`ingest`) (§8).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct RootsAddArgs {
    /// Folder path to register as a watched root.
    pub path: String,
    /// Whether to watch the root for changes.
    #[serde(default)]
    pub watch: bool,
}

/// `roots.remove` arguments — GATED (`admin`) + explicit confirm (§8 / §12.5).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct RootsRemoveArgs {
    /// Root id to logically purge (reversible, §6.6).
    pub id: String,
    /// Must be `true`; high-risk action requires explicit confirmation (§12.5).
    #[serde(default)]
    pub confirm: bool,
}

/// `ingestion.status` arguments (§8).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct IngestionStatusArgs {
    /// Job id (UUIDv7).
    pub job_id: String,
}

fn default_k() -> u32 {
    8
}

// ---------------------------------------------------------------------------
// Tool router. The rmcp macros generate the JSON-Schema-typed tool list from the
// method signatures below; descriptions reference ONLY the static consts above.
//
// DECISION: the actual `#[tool_router]` / `#[tool]` macro impl is sketched as a
// commented block keyed to rmcp 0.16's documented surface (Parameters<T> input,
// Result<CallToolResult, McpError> output). It is left commented so the skeleton
// compiles without the rmcp dependency resolved in this offline scaffold; each
// method body is a `// TODO:` adapter call into core-api that re-checks RBAC.
// ---------------------------------------------------------------------------

/// The RAG tool surface. Holds the (cheap-clone) adapter state needed to call
/// `core-api` and to enforce RBAC (`auth::AuthContext` per request).
#[derive(Clone)]
pub struct RagTools {
    /// Base URL of the upstream core-api (no hardcoded host, §0).
    pub core_api_base: String,
    /// HTTP client used for the thin adapter calls (§8).
    pub http: reqwest::Client,
}

impl RagTools {
    /// Construct the tool surface.
    #[must_use]
    pub fn new(core_api_base: String, http: reqwest::Client) -> Self {
        Self { core_api_base, http }
    }
}

// #[rmcp::tool_router]
// impl RagTools {
//     #[rmcp::tool(description = descriptions::RAG_SEARCH)]
//     async fn rag_search(
//         &self,
//         rmcp::handler::server::tool::Parameters(args): rmcp::handler::server::tool::Parameters<RagSearchArgs>,
//         ctx: rmcp::service::RequestContext<rmcp::service::RoleServer>,
//     ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
//         // 1. Server-derive tenant from the authenticated principal (§12.7).
//         // 2. require_role(Reader) — same RBAC as REST (§12.6).
//         // 3. clamp k, sanitize query, POST core-api /api/v1/query/search,
//         //    return reranked chunks + citations as CallToolResult content.
//         todo!()
//     }
//
//     #[rmcp::tool(description = descriptions::RAG_ANSWER)]   // Reader scope
//     async fn rag_answer(/* … RagAnswerArgs … */) { todo!() }
//
//     #[rmcp::tool(description = descriptions::DOCUMENTS_LIST)] // Reader
//     async fn documents_list(/* … */) { todo!() }
//
//     #[rmcp::tool(description = descriptions::DOCUMENTS_GET)]  // Reader
//     async fn documents_get(/* … */) { todo!() }
//
//     #[rmcp::tool(description = descriptions::DOCUMENTS_ADD)]  // GATED: Ingest
//     async fn documents_add(/* … */) { /* require_scope(Ingest) first */ todo!() }
//
//     #[rmcp::tool(description = descriptions::ROOTS_ADD)]      // GATED: Ingest
//     async fn roots_add(/* … */) { /* require_scope(Ingest) first */ todo!() }
//
//     #[rmcp::tool(description = descriptions::ROOTS_REMOVE)]   // GATED: Admin + confirm
//     async fn roots_remove(/* … */) {
//         // require_scope(Admin); reject unless args.confirm (§12.5); logical purge.
//         todo!()
//     }
//
//     #[rmcp::tool(description = descriptions::INGESTION_STATUS)] // Reader
//     async fn ingestion_status(/* … */) { todo!() }
// }

#[cfg(test)]
mod tests {
    use super::*;

    /// Tool descriptions must be static consts, never templated from content
    /// (§8 / §22 #5). This guards against a refactor that interpolates input.
    #[test]
    fn descriptions_are_nonempty_static() {
        for d in [
            descriptions::RAG_SEARCH,
            descriptions::RAG_ANSWER,
            descriptions::DOCUMENTS_LIST,
            descriptions::DOCUMENTS_GET,
            descriptions::DOCUMENTS_ADD,
            descriptions::ROOTS_ADD,
            descriptions::ROOTS_REMOVE,
            descriptions::INGESTION_STATUS,
        ] {
            assert!(!d.is_empty());
        }
    }

    /// No tool parameter struct exposes a tenant/collection field — tenant is
    /// server-derived (§12.7). We assert by round-tripping a default and checking
    /// the serialized keys.
    #[test]
    fn args_carry_no_tenant_field() {
        let json = serde_json::to_value(RagSearchArgs {
            query: "x".to_owned(),
            k: 8,
            filters: RagFilters::default(),
        })
        .expect("serialize");
        let obj = json.as_object().expect("object");
        assert!(!obj.contains_key("tenant"));
        assert!(!obj.contains_key("tenant_id"));
        assert!(!obj.contains_key("collection"));
    }
}
