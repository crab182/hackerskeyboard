#!/bin/bash
# ============================================================================
# diyRAG — unraid "User Scripts" plugin script: bring the stack up at array start
# ============================================================================
# MASTER_BUILD_SPEC.md §16b.3: "For non-Docker control, a User Scripts plugin
# script ... running 'At Startup of Array' can invoke `diyrag service start` /
# `docker compose up -d`." This is the Linux analog of the Windows Service
# boot-autostart (§16b.2).
#
# INSTALL (unraid):
#   1. Install the "User Scripts" plugin from Community Applications.
#   2. Settings -> User Scripts -> Add New Script -> name it "diyrag-start".
#   3. Paste this file's contents (or point it at this file).
#   4. Set its schedule to: "At Startup of Array".
#
# NOTE: with `restart: unless-stopped` in docker-compose.yml AND unraid
# auto-starting Docker on array start, the stack already returns after a reboot.
# This script is a belt-and-suspenders trigger for environments that disabled
# autostart on the containers, or that want an explicit, logged bring-up plus a
# post-start watched-folder ingest. It is idempotent: `up -d` is a no-op if the
# stack is already running.
# ============================================================================

set -euo pipefail

# --- Config (edit to taste) --------------------------------------------------
APPDATA_DIR="/mnt/user/appdata/diyrag"
COMPOSE_FILE="${APPDATA_DIR}/docker-compose.yml"
GPU_OVERLAY="${APPDATA_DIR}/docker-compose.gpu.yml"
ENV_FILE="${APPDATA_DIR}/.env"
LOG_FILE="${APPDATA_DIR}/logs/userscript-start.log"

# Set to "yes" to stack the NVIDIA GPU overlay (needs the unraid Nvidia plugin).
USE_GPU="no"
# Compose profile: cpu | gpu (gpu also requires USE_GPU=yes for the overlay).
COMPOSE_PROFILE="cpu"

# If the diyrag CLI is installed on the host (rare on unraid), prefer it.
# Otherwise we use plain `docker compose`. Leave empty to force compose.
DIYRAG_CLI="$(command -v diyrag || true)"

# --- Logging helper ----------------------------------------------------------
mkdir -p "$(dirname "$LOG_FILE")"
log() { echo "[$(date -u +'%Y-%m-%dT%H:%M:%SZ')] $*" | tee -a "$LOG_FILE"; }

log "diyRAG array-start hook beginning."

# --- Preconditions -----------------------------------------------------------
if ! command -v docker >/dev/null 2>&1; then
  log "ERROR: docker not found; is the Docker service enabled? Aborting."
  exit 1
fi

# `docker compose` (v2) vs legacy `docker-compose`.
if docker compose version >/dev/null 2>&1; then
  COMPOSE=(docker compose)
elif command -v docker-compose >/dev/null 2>&1; then
  COMPOSE=(docker-compose)
else
  log "ERROR: neither 'docker compose' nor 'docker-compose' is available. Aborting."
  exit 1
fi

if [[ ! -f "$COMPOSE_FILE" ]]; then
  log "ERROR: compose file not found at ${COMPOSE_FILE}. Aborting."
  exit 1
fi
if [[ ! -f "$ENV_FILE" ]]; then
  log "WARNING: ${ENV_FILE} not found. Copy repo .env.example there and set secrets (§12.1)."
fi

# --- Bring the stack up ------------------------------------------------------
# Path 1 (preferred where available): the diyrag CLI wraps compose (§16b.4).
if [[ -n "$DIYRAG_CLI" ]]; then
  log "Using diyrag CLI: ${DIYRAG_CLI} service start"
  if "$DIYRAG_CLI" service start >>"$LOG_FILE" 2>&1; then
    log "diyrag service start succeeded."
  else
    log "diyrag service start failed; falling back to docker compose."
    DIYRAG_CLI=""
  fi
fi

# Path 2: plain docker compose up -d (idempotent).
if [[ -z "$DIYRAG_CLI" ]]; then
  args=(-f "$COMPOSE_FILE")
  if [[ "$USE_GPU" == "yes" && -f "$GPU_OVERLAY" ]]; then
    args+=(-f "$GPU_OVERLAY")
    log "GPU overlay enabled: ${GPU_OVERLAY}"
  fi
  log "Running: ${COMPOSE[*]} ${args[*]} --profile ${COMPOSE_PROFILE} up -d"
  "${COMPOSE[@]}" "${args[@]}" --profile "$COMPOSE_PROFILE" up -d >>"$LOG_FILE" 2>&1
  log "docker compose up -d completed."
  "${COMPOSE[@]}" "${args[@]}" ps >>"$LOG_FILE" 2>&1 || true
fi

# --- Optional: kick off a headless watched-folder ingest (§16b.3) -----------
# Uncomment to start watching a documents share on boot. Needs the CLI either on
# the host or invoked inside the stack container, with valid API auth.
#
# if [[ -n "$DIYRAG_CLI" ]]; then
#   "$DIYRAG_CLI" ingest /mnt/user/Documents --watch >>"$LOG_FILE" 2>&1 || \
#     log "WARNING: ingest --watch failed (auth/config?)."
# else
#   docker exec diyrag-diyrag-stack-1 diyrag ingest /data/documents --watch \
#     >>"$LOG_FILE" 2>&1 || log "WARNING: in-container ingest --watch failed."
# fi

log "diyRAG array-start hook done."
