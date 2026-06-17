# `diyrag` — diyRAG control CLI

`diyrag` is the **terminal control plane** for a diyRAG node
(MASTER_BUILD_SPEC.md §16b.1). One CLI, identical on **Windows**, **unraid**, and
**generic Linux** (acceptance #9). It does two jobs:

1. **Manage the OS service** that runs `diyragd` — install it so it
   **auto-starts on boot**, then start/stop/restart/status it.
2. **Drive the running stack headlessly** over the REST API — ingest, batch,
   query, and node/sync ops — ideal for unraid where there is no desktop GUI.

Built with `#![forbid(unsafe_code)]`, `anyhow`, and `tracing` (§19). **No
secrets in code or flags:** the API key is read from `DIYRAG_API_KEY` in the
environment (§12.2).

## Command tree (spec §16b.1)

```
diyrag service install|uninstall|start|stop|restart|status
diyrag node    status|peers|snapshot|restore <snapshot>
diyrag ingest  <path|root> [--watch]
diyrag batch   submit <archive>
diyrag query   "<q>" [--answer] [--k N]
diyrag config  show|set <key> <value>
```

## Service management — the cross-platform abstraction (§16b.4)

`service install|…` dispatch to a `ServiceManager` trait with three impls,
selected by `cfg!(windows)` (default) with a `--manager {scm|systemd|docker}`
override:

| Backend | Platform | Wraps |
|---|---|---|
| `WindowsScm` (`#[cfg(windows)]`) | Windows | SCM via the `windows-service` crate |
| `Systemd` (`#[cfg(unix)]`) | generic Linux | `systemctl enable --now` / `stop` / `status` / `disable` |
| `DockerCompose` | unraid / any Docker host | `docker compose up -d` / `down` / `ps` |

### Windows (service autostart)

```powershell
# install with boot auto-start under a low-privilege account, then start
diyrag service install --mode all-in-one --auto-start --account "NT SERVICE\diyRAG"
diyrag service start
diyrag service status
```

`install` creates the service with `StartType::AutoStart` +
`ServiceType::OWN_PROCESS`, sets a description, and configures **recovery
actions** (restart on the 1st/2nd/3rd failure with 5s/10s/30s backoff,
counter reset after 24h) — satisfying §14 service-level recovery. The SCM then
launches `diyragd.exe service --mode all-in-one` on **every device restart**.

`sc.exe` equivalents (reference / fallback):

```powershell
sc.exe create diyRAG binPath= "C:\Program Files\diyRAG\diyragd.exe service --mode all-in-one" start= auto type= own obj= "NT SERVICE\diyRAG"
sc.exe failure diyRAG reset= 86400 actions= restart/5000/restart/10000/restart/30000
sc.exe start diyRAG
```

### unraid (CLI over API / docker compose)

unraid's first-class app model is Docker, so use the `docker` backend:

```bash
# bring the stack up (restart: unless-stopped gives boot-autostart on array start)
diyrag service install --manager docker --compose-file /mnt/user/appdata/diyrag/docker-compose.yml
diyrag service status  --manager docker --compose-file /mnt/user/appdata/diyrag/docker-compose.yml

# then drive everything headlessly over the API (no GUI needed)
export DIYRAG_API_URL=https://127.0.0.1:8443
export DIYRAG_API_KEY=…            # from your admin key; never commit this
diyrag ingest /mnt/user/Documents --watch
diyrag query "What changed in the Q2 report?" --answer
diyrag node snapshot
```

Equivalent raw commands:

```bash
docker compose -f /mnt/user/appdata/diyrag/docker-compose.yml up -d
docker compose -f /mnt/user/appdata/diyrag/docker-compose.yml ps
docker compose -f /mnt/user/appdata/diyrag/docker-compose.yml down
```

### Generic Linux (systemd)

```bash
diyrag service install   # wraps: systemctl enable --now diyragd
diyrag service status     # wraps: systemctl status diyragd
diyrag service stop       # wraps: systemctl stop diyragd
```

## Headless API control

`node` / `ingest` / `batch` / `query` call the `api-gateway` REST surface (§11)
via `reqwest` (rustls, JSON). Connection + auth come from the environment:

| Env var | Purpose | Default |
|---|---|---|
| `DIYRAG_API_URL` | api-gateway base URL | `https://127.0.0.1:8443` |
| `DIYRAG_API_KEY` | bearer API key (marked sensitive, never logged) | — |

```bash
diyrag query "vector search internals" --k 12          # reranked chunks
diyrag query "summarize the design doc" --answer        # grounded answer + citations
diyrag batch submit ./corpus.zip                        # returns a job id
diyrag node peers                                       # LAN peers + sync lag
```

## Status

The command tree, the cross-platform `ServiceManager` (with the real
`windows-service` 0.7 create/failure-action calls), and the REST client wiring
are complete and unit-tested. The REST request bodies and config persistence are
`// TODO:` stubs that log their intent; they fill in as the `api-gateway`
endpoints (§11) and the shared `diyrag-common` config land.
