# diyrag-mcp-server

Model Context Protocol server — **MASTER_BUILD_SPEC.md §8** (red-team §22 #5).

Exposes the diyRAG platform to LLM clients (Claude Desktop, Cursor, OpenAI
Agents, …) using the **official Rust MCP SDK (`rmcp`)**. It is a **thin protocol
adapter** over `core-api` that enforces the **same tenant scoping + RBAC** as the
REST API — never a privilege bypass.

## Binary

`diyrag-mcp-server` — two transports, chosen by config/flag (`--stdio`):

- **Streamable HTTP (stateless)** for remote clients (horizontal scale), served
  on Axum with CORS for browser MCP clients.
- **stdio** for local clients.

SSE transport is deprecated by the spec and is **not** enabled.

## Modules

| Module | Responsibility |
|---|---|
| `tools.rs` | `rag.search`, `rag.answer`, `documents.list/get`, `documents.add`/`roots.add` (gated `ingest`), `roots.remove` (gated `admin` + confirm), `ingestion.status`. Thin adapters to core-api enforcing the SAME RBAC. |
| `resources.rs` | `document://`, `chunk://`, `collection://` with `ttlMs` + `cacheScope`. |
| `auth.rs` | OAuth 2.1 / mTLS; **tenant scope is SERVER-derived, never client-supplied** (§12.7). |

## Security invariants

- **Tool-poisoning defense (§22 #5):** every tool description is a static,
  reviewed Rust constant (`tools::descriptions`). Descriptions are **never**
  templated from user- or ingested content. A unit test asserts they are
  non-empty static strings.
- **No tenant in arguments:** no tool parameter struct carries a
  tenant/collection field; tenant is derived from the authenticated principal
  (`auth::tenant_of`). A unit test asserts the absence of those keys.
- High-risk tools are scope-gated and `roots.remove` additionally requires an
  explicit `confirm` flag (§12.5).

## DECISION — assumed `rmcp` API surface

Pinned `rmcp = "0.16"` (current crates.io release at scaffold time) with features
`server`, `macros`, `transport-streamable-http-server`, `transport-io`. The
assumed surface (per the official rust-sdk docs):

- `#[rmcp::tool_router]` on an `impl`, `#[rmcp::tool(description = …)]` per method.
- Tool input via `Parameters<T>` where `T: serde::Deserialize + schemars::JsonSchema`;
  output `Result<CallToolResult, rmcp::ErrorData>` (a.k.a. `McpError`).
- `ServerHandler` trait (`get_info`, `list_resources`, `read_resource`).
- stdio via `service.serve(rmcp::transport::io::stdio())`; Streamable HTTP via
  `StreamableHttpService` (stateless mode) mounted on Axum.

The macro `impl` block in `tools.rs` is provided as a commented, ready-to-enable
sketch so the offline scaffold compiles before the `rmcp` dependency is resolved;
the parameter structs, static descriptions, RBAC gates, and resource parsing are
real and unit-tested. Adjust exact paths to the resolved `rmcp` minor.
