//! Build script for the sync-agent.
//!
//! Compiles `proto/sync.proto` into Rust **server + client** stubs (via
//! `tonic-build`) for the bidirectional LAN sync RPC (MASTER_BUILD_SPEC.md §9).
//! Each peer is both a server (answers diffs/snapshots/blobs) and a client
//! (pulls from peers), so unlike the ingestion-worker (client-only) we build
//! both. The generated code is emitted to `OUT_DIR` and `include!`d in `grpc.rs`.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // TODO: enable once proto/sync.proto is finalized with the peer-node owners
    // and `protoc` is available in CI/build images. Kept guarded so the skeleton
    // compiles without protoc installed (matches the ingestion-worker pattern).
    //
    // tonic_build::configure()
    //     .build_server(true)
    //     .build_client(true)
    //     .compile_protos(&["proto/sync.proto"], &["proto"])?;
    println!("cargo:rerun-if-changed=proto/sync.proto");
    Ok(())
}
