# native/ — diyRAG Windows desktop client (Tauri 2)

A **Tauri 2** (Rust shell, system WebView2) application that wraps the `../web` build into a small, sandboxed Windows-native desktop client. Aligns with the Rust-first architecture (ADR-0001); small binary, low RAM, strong security sandbox.

## Responsibilities
- Render the same React app as the browser GUI (single source of truth in `../web`).
- **Detect and manage the local Windows Service** (`diyragd`): show service status, and offer start/stop/restart (elevating via UAC) so a non-terminal user manages everything from the GUI. The service keeps running and ingesting even when the GUI is closed and across reboots (spec §16b, ADR-0003).
- Talk to the local API (`https://127.0.0.1:8443` by default) for all RAG operations.

## Relationship to the service
The desktop client is a **thin GUI**, not the engine. The engine is the `diyragd` Windows Service installed via `deploy/windows/install.ps1` (or `diyrag service install`). Closing the window does not stop ingestion or query serving.

## Build (Windows)
```bash
# from repo root: build the web bundle first
pnpm --dir web build
# then the Tauri app
cargo tauri build        # produces an MSI/EXE in target/release/bundle
```
Requirements: Rust toolchain, WebView2 runtime, the Tauri prerequisites. Binaries are Authenticode-signed for release.

## Capabilities / security
- Minimal Tauri allowlist; only the commands needed to query service status and invoke the elevated installer helper.
- No secrets bundled; the API key is read from the local config/Windows credential store.

> Scaffolding placeholder — implemented in phase **M5**, with service integration finalized in **M9** (spec §20).
