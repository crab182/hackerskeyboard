# syntax=docker/dockerfile:1
# ============================================================================
# diyRAG — shared multi-stage build for every Rust service (§3.1, §12.8).
# ============================================================================
# One Dockerfile builds any workspace binary, selected by the BIN build-arg
# (the cargo artifact, e.g. `diyrag-api-gateway`). The binary is installed at a
# fixed path AND under its service name (OUT) so a single ENTRYPOINT and the
# per-service container HEALTHCHECK both resolve:
#   * ENTRYPOINT boots the service (fixed path; exec-form can't expand args).
#   * HEALTHCHECK re-invokes the same binary as `<service> healthcheck` (§16b) —
#     an HTTP /healthz loopback probe for HTTP services, liveness for workers.
#
# Runtime is distroless/cc (glibc + libgcc only). Our binaries are pure-Rust +
# rustls — no OpenSSL, no other native libs (candle is CPU/pure-Rust here) — so
# no extra shared objects are required. Non-root, no shell, no package manager.
#
# Build from the repo root (BuildKit):
#   docker build -f infra/rust-service.Dockerfile \
#     --build-arg BIN=diyrag-api-gateway --build-arg OUT=api-gateway \
#     -t diyrag/api-gateway .
# docker-compose.yml passes BIN/OUT per service.
# ============================================================================

ARG RUST_VERSION=1.94

FROM rust:${RUST_VERSION}-bookworm AS builder
ARG BIN
WORKDIR /build
# The whole workspace is required (path deps); .dockerignore keeps target/, .git,
# and .env out of the build context.
COPY . .
# Build only the requested binary: release for size/speed, --locked for a
# reproducible build pinned to the committed Cargo.lock.
RUN cargo build --release --locked --bin "${BIN}" \
 && install -D "target/release/${BIN}" /out/diyrag-service

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
ARG OUT
# Fixed boot path (ENTRYPOINT) ...
COPY --from=builder /out/diyrag-service /usr/local/bin/diyrag-service
# ... and the service-named path the compose HEALTHCHECK invokes.
COPY --from=builder /out/diyrag-service /usr/local/bin/${OUT}
# distroless :nonroot = uid 65532; compose may further pin `user:`.
USER nonroot
ENTRYPOINT ["/usr/local/bin/diyrag-service"]
