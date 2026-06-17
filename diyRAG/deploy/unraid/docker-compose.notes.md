# diyRAG on unraid — Docker Compose terminal notes

> MASTER_BUILD_SPEC.md §16b.3 (unraid via terminal), §17, §16 (GPU),
> §12.8 + §22 #14 (hardening). This is the **primary** unraid deployment path;
> the CA template (`diyrag.xml`) is the convenience tile.

## Prerequisites

- **unraid 6.12+** with the Docker service enabled (Settings → Docker → Enable
  Docker = Yes; unraid auto-starts Docker on array start).
- **Docker Compose Manager** plugin (from Community Applications) **or** the
  bundled `docker compose` CLI in the unraid terminal. Both run the repo's
  compose files unchanged.
- For GPU: the **unraid "Nvidia Driver" plugin** (installs the host driver +
  the NVIDIA Container Toolkit). Reboot after installing it. Verify with
  `nvidia-smi` in the terminal.

## Appdata layout (§16b.3)

Put the repo's compose files + `.env` on the appdata share and point the
named-volume / bind paths at `/mnt/user/appdata/diyrag`:

```
/mnt/user/appdata/diyrag/
├── docker-compose.yml          # copied from the repo root
├── docker-compose.gpu.yml      # GPU overlay
├── docker-compose.dev.yml      # optional, dev only
├── .env                        # copied from repo .env.example, REAL secrets, NEVER committed
├── config/                     # diyragd / app config
├── postgres/                   # Postgres 16 data        (§5.1)
├── qdrant/                     # vector store            (§5.2)
├── minio/                      # blob store              (§5.3)
├── models/                     # ONNX / mistral.rs cache (§16)
└── certs/                      # mTLS CA + service certs (§12.1)
```

> The committed `docker-compose.yml` uses **named volumes** (`pg-data`,
> `qdrant-data`, `minio-data`, `model-cache`, `certs`, …). On unraid you can
> either (a) keep named volumes and let Docker store them under the system
> share, or (b) override them to bind-mount the appdata subdirs above with a
> small `docker-compose.override.yml`, e.g.:
>
> ```yaml
> # /mnt/user/appdata/diyrag/docker-compose.override.yml
> name: diyrag
> volumes:
>   pg-data:     { driver: local, driver_opts: { type: none, o: bind, device: /mnt/user/appdata/diyrag/postgres } }
>   qdrant-data: { driver: local, driver_opts: { type: none, o: bind, device: /mnt/user/appdata/diyrag/qdrant } }
>   minio-data:  { driver: local, driver_opts: { type: none, o: bind, device: /mnt/user/appdata/diyrag/minio } }
>   model-cache: { driver: local, driver_opts: { type: none, o: bind, device: /mnt/user/appdata/diyrag/models } }
>   certs:       { driver: local, driver_opts: { type: none, o: bind, device: /mnt/user/appdata/diyrag/certs } }
> ```
> Compose auto-merges `docker-compose.override.yml` when present.

## Exact terminal commands

Run these from `/mnt/user/appdata/diyrag` (or pass `-f` with absolute paths).

```bash
# 0. One-time: seed config from the template, then edit REAL secrets into .env.
cp /path/to/repo/.env.example /mnt/user/appdata/diyrag/.env
nano /mnt/user/appdata/diyrag/.env            # set passwords/keys; NEVER commit (§12.1)

# 1. Bring the base stack up (CPU profile), detached.
docker compose -f docker-compose.yml --profile cpu up -d

# 2. GPU node: stack the NVIDIA overlay (needs the unraid Nvidia Driver plugin).
docker compose -f docker-compose.yml -f docker-compose.gpu.yml --profile gpu up -d

# 3. Status / health.
docker compose ps

# 4. Follow logs (all, or one service).
docker compose logs -f
docker compose logs -f core-api

# 5. Stop + remove the stack (named volumes are KEPT — data is safe; §6.6).
docker compose down
#    Add -v ONLY to also delete volumes (irreversible data loss):
#    docker compose down -v   # DANGER: wipes postgres/qdrant/minio/models/certs
```

> **Docker Compose Manager plugin equivalent:** add a new stack, set the
> "Compose file" path to `/mnt/user/appdata/diyrag/docker-compose.yml`, set the
> extra compose file to the GPU overlay if needed, then use the plugin's
> Up/Down/Logs buttons — they shell out to the same `docker compose` commands.

## Reboot persistence (the Linux analog of the Windows Service)

Every service in `docker-compose.yml` sets `restart: unless-stopped`. Combined
with unraid auto-starting Docker on array start, the **stack returns after a
reboot** without manual intervention (§16b.3). To also run a command at array
start (e.g. trigger a watched-folder ingest), use the **User Scripts** plugin
with `userscript-start.sh` (this folder).

## GPU notes (§16, §22 #14)

- Only the **Python `gpu-runtime`** (vLLM/Surya OCR) and `parsing-service` use
  the NVIDIA overlay's device reservations; the Rust-native `ort`/`mistral.rs`
  path claims the device in-process and is the default.
- The overlay scopes GPUs via `NVIDIA_VISIBLE_DEVICES` (default `all`; prefer
  specific UUIDs) — it **never** uses `--privileged` and never mounts the whole
  device tree (§22 #14).
- Match CUDA/cuDNN/torch (or ORT-CUDA) versions to the image; pin them (§16).

## Security recap (§12.8 / §22 #14)

- No `--privileged`; scoped GPU devices only.
- Only `caddy` (443/80), `api-gateway` (8443), and `sync-agent` (7443) publish
  host ports; everything else stays on the internal `diyrag-internal` network.
- Secrets live in `.env` (gitignored) or Docker secrets / SOPS — never in the
  compose file or the CA template.
- Containers run as non-root with `no-new-privileges`, `cap_drop: ALL`, and a
  read-only rootfs where possible (already set in `docker-compose.yml`).
