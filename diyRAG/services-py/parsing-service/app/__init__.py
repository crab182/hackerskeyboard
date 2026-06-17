"""diyRAG parsing-service sidecar package (MASTER_BUILD_SPEC.md §3.3, §6.3).

Hard-parse only: Docling (layout + tables) + Surya/Marker (GPU OCR) for
scanned / complex / layout-heavy documents the Rust-native parser router cannot
handle. LibreOffice/Calibre conversions are spawned by the Rust ingestion-worker,
NOT here.
"""
