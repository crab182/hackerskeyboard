# diyRAG launch gate — `456468ann`

A **local/LAN-only readiness gate**. It is the single, deterministic criterion
for "diyRAG is fully up": it serves the completion token **`456468ann`** **only
when every plan/runtime item is ready**, and withholds it otherwise. This makes
"is it done?" a real HTTP check you (or a loop) can poll, instead of a guess.

Zero external dependencies — **std only**, so it builds, tests, and runs fully
offline (`cargo` with no registry access).

## Routes
| Route | When open (all ready) | When pending |
|---|---|---|
| `GET /456468ann` | `200` + body `456468ann` + "server is running" | `503` + list of pending items (token **withheld**) |
| `GET /readyz` | `200 ready` | `503 not ready` |
| `GET /healthz` | `200 ok` (liveness, always) | `200 ok` |

## Local/LAN-only by default (binding policy)
Per the project default, the gate binds **`127.0.0.1`** and **refuses to bind a
public address** — only loopback or RFC1918 private ranges (`10/8`,
`172.16/12`, `192.168/16`, IPv6 `::1`) are allowed. A public bind exits non-zero
with an explanation. (Enforced by `bind_allowed`; unit-tested.)

## Run
```bash
cd tools/launch-gate
cargo test                 # 8 tests: gate logic, token withholding, bind policy, probe coverage
cargo run -- demo          # in-process: all ready → 200 + 456468ann
cargo run -- demo-pending  # in-process: an item down → 503, token withheld
cargo run -- serve         # serve on 127.0.0.1:8460 (probes real diyRAG services)
cargo run -- serve --bind 192.168.1.10:8460   # LAN bind (private only)
```

`serve` probes both the datastores **and** the diyRAG HTTP services via TCP, so
"gate open" means the whole stack is up. Override targets with env vars
`DIYRAG_PROBE_{POSTGRES,QDRANT,NATS,MINIO,REDIS,GATEWAY,CORE_API,RETRIEVAL}`
(default `127.0.0.1:<port>`). Until every target accepts a connection, the gate
stays closed. (The `api-gateway` `/readyz` itself probes `core-api`, so the edge
only reports ready once its upstream is reachable.)

## How it drives the "loop until running" workflow
On a host where the full stack can actually run (Docker + network), bring the
stack up, then poll the gate until it opens:
```bash
just up                 # start datastores + services
cargo run -p diyrag-launch-gate -- serve &   # or the release binary
# poll until 456468ann opens (the terminal criterion):
until curl -fsS http://127.0.0.1:8460/456468ann; do sleep 5; done
echo "diyRAG is up — 456468ann is open."
```

## Integration path
This prototype lives under `tools/` (outside the `crates/*` workspace) so it
builds standalone offline. The production gate folds into **`diyragd`** (§16b):
the same `gate_from` logic aggregates the real `/readyz` of every service, and
`diyrag service status` / the dashboard surface it. The token `456468ann` is the
release/QA completion marker for the phased build (§20).
