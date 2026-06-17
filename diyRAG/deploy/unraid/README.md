# diyRAG on unraid — quickstart

> MASTER_BUILD_SPEC.md §16b.3 (unraid deployment), §17, §16 (GPU),
> §9 (LAN sync), §12.8 + §22 #14 (hardening), acceptance #9.

unraid is a Slackware-based NAS OS whose first-class app model is **Docker**.
diyRAG runs there as a Docker stack you control **entirely from the terminal**
via the `diyrag` CLI — no desktop GUI required. The stack **returns after a
reboot** automatically (the Linux analog of the Windows Service autostart).

Files in this folder:

| File | Purpose |
|---|---|
| `diyrag.xml` | unraid Community Applications (CA) container template — the convenience "one tile" (`diyrag-stack`, `diyragd --mode agent`). |
| `docker-compose.notes.md` | Exact terminal commands (Compose Manager plugin or CLI), appdata layout, GPU + security. |
| `userscript-start.sh` | User Scripts "At Startup of Array" script — explicit, logged bring-up. |

---

## Two ways to deploy

### A. CA template (convenience tile)

Add `diyrag.xml` as a private CA template (or via the **Docker → Add Container
→ Template** UI). It deploys one `diyrag-stack` container running
`diyragd --mode agent`, which orchestrates the rest of the stack (§16b.1).
Set the appdata paths and the `.env` file path in the UI, then Apply. The
**WebUI** button opens `https://<server>:8443/` (the api-gateway).

DECISION: the CA template is a single orchestrator container for simplicity.
For per-service containers and fine-grained scaling, use compose (option B) —
that is the spec's primary path.

### B. Docker Compose (primary, recommended)

Run the repo's `docker-compose.yml` from `/mnt/user/appdata/diyrag` with the
**Docker Compose Manager** plugin or the terminal. Full commands, the appdata
layout, and a bind-mount override are in
[`docker-compose.notes.md`](./docker-compose.notes.md). The short version:

```bash
cp /path/to/repo/.env.example /mnt/user/appdata/diyrag/.env   # then set real secrets
docker compose -f /mnt/user/appdata/diyrag/docker-compose.yml --profile cpu up -d
docker compose ps
```

---

## Headless control via the `diyrag` CLI (§16b.3)

Everything is driven from the unraid terminal — no GUI needed. Auth is via API
key / OAuth (see `.env.example`). If the CLI is not on the host PATH, run it
inside the stack container (`docker exec <container> diyrag ...`).

```bash
# Ingest a share and keep watching it for new/changed files (§6.1):
diyrag ingest /mnt/user/Documents --watch

# Ask a grounded question with citations (§7.2):
diyrag query "Summarize the latest incident reports" --answer

# Submit an archive as a batch job (§6.7):
diyrag batch submit /mnt/user/Downloads/corpus.zip

# Node + LAN-sync operations (§9):
diyrag node status
diyrag node peers
diyrag node snapshot          # Qdrant snapshot — the unit of vector replication
diyrag node restore <snap>

# Service control (compose-backed on unraid; §16b.4):
diyrag service status
diyrag service start
diyrag service stop
```

The CLI surface is **identical** to Windows and generic Linux (§16b.4) — only
the underlying `ServiceManager` impl differs (DockerCompose here, WindowsScm on
Windows, Systemd on bare Linux).

---

## Reboot persistence (acceptance #9)

Two layers make the stack return after an unraid reboot / array start:

1. **`restart: unless-stopped`** on every service in `docker-compose.yml`
   (and `--restart=unless-stopped` in the CA template's Extra Parameters).
2. **unraid auto-starts the Docker service** on array start.

That combination is the Linux analog of the Windows Service `StartType=Auto`.
For an explicit, logged bring-up (and optional boot-time ingest), add
[`userscript-start.sh`](./userscript-start.sh) to the **User Scripts** plugin on
the "At Startup of Array" schedule.

**Verify after a reboot:**

```bash
docker compose -f /mnt/user/appdata/diyrag/docker-compose.yml ps   # all Up/healthy
curl -k https://localhost:8443/healthz                             # gateway liveness
curl -k https://localhost:8443/readyz                              # readiness
diyrag node status                                                 # node healthy
diyrag query "smoke test" --answer                                 # cited answer
```

---

## GPU passthrough (§16, §22 #14)

- Install the unraid **"Nvidia Driver" plugin** (driver + NVIDIA Container
  Toolkit); reboot; confirm with `nvidia-smi`.
- Compose: stack the overlay —
  `docker compose -f docker-compose.yml -f docker-compose.gpu.yml --profile gpu up -d`.
- CA template: uncomment `--runtime=nvidia` in Extra Parameters and set
  `NVIDIA_VISIBLE_DEVICES`.
- The overlay scopes devices via `NVIDIA_VISIBLE_DEVICES` (prefer specific GPU
  UUIDs over `all`) and **never** uses `--privileged` (§22 #14). The Rust-native
  `candle`/`mistral.rs` backend is the default and claims the device in-process;
  the Python `gpu-runtime` (vLLM) is the optional Linux/CUDA throughput profile.

---

## Security (§12.8 / §22 #14)

- **No `--privileged`.** Scoped GPU devices only; no unbounded device mounts.
- **Only gateway and sync ports are exposed:** `8443` (api-gateway WebUI/REST/WS)
  and `7443` (sync-agent mTLS gRPC). Everything else stays on the internal
  `diyrag-internal` network. Caddy adds `443/80` if you front the gateway.
- **No secrets in the template or compose file** — they reference `.env`
  (gitignored) or Docker secrets / SOPS (§12.1).
- Containers run **non-root** with `no-new-privileges`, `cap_drop: ALL`, and a
  read-only rootfs where possible (already configured in `docker-compose.yml`).
- LAN sync to peers is mTLS with cert-pinned, admin-approved nodes only (§9);
  expose `7443` only if you actually run multiple cooperating instances.
