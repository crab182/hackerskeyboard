//! Build script for the ingestion-worker.
//!
//! Compiles `proto/parsing.proto` into Rust client stubs (via `tonic-build`) for
//! delegating hard/scanned documents to the Python `parsing-service` over gRPC
//! (MASTER_BUILD_SPEC.md §3.3, §6.3). The generated code is emitted to `OUT_DIR`
//! and `include!`d at the call site (see `parser::pdf`).

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // NOTE: build client stubs only — this crate is a gRPC *client* of the
    // parsing-service, not a server. mTLS is configured at channel-build time in
    // the parser, not here (§12.1).
    //
    // TODO: enable once proto/parsing.proto is finalized with the parsing-service
    // owner. Kept guarded so the skeleton builds without protoc installed.
    //
    // tonic_build::configure()
    //     .build_server(false)
    //     .build_client(true)
    //     .compile_protos(&["proto/parsing.proto"], &["proto"])?;
    println!("cargo:rerun-if-changed=proto/parsing.proto");
    Ok(())
}
