# diyRAG on generic Linux — systemd quickstart

> MASTER_BUILD_SPEC.md §16b.3 (generic Linux), §16b.4 (cross-platform
> `ServiceManager`), §14 (recovery), §12.8 (hardening), acceptance #9.

For bare-metal / VM Linux that is **not** unraid (where Docker Compose is the
path — see `deploy/unraid/`), diyRAG runs as a native **systemd service** that
**starts on every boot** and is controlled with the same `diyrag` CLI used on
Windows and unraid.

Files in this folder:

| File | Purpose |
|---|---|
| `diyragd.service` | systemd unit running `diyragd --mode all-in-one`, `Restart=always`, hardened, `WantedBy=multi-user.target`. |

---

## Install

### Preferred — via the CLI

`diyrag service install` wraps `systemctl enable --now diyragd` (the `Systemd`
impl of the cross-platform `ServiceManager` trait, §16b.4). It copies the unit,
creates the `diyrag` system user and state dirs, reloads systemd, then enables +
starts the service:

```bash
sudo diyrag service install --mode all-in-one
```

### Manual

```bash
# 1. Binary on PATH.
sudo install -m 0755 target/release/diyragd /usr/local/bin/diyragd
sudo install -m 0755 target/release/diyrag  /usr/local/bin/diyrag

# 2. Dedicated low-privilege account + dirs (§12.8).
sudo useradd --system --no-create-home --shell /usr/sbin/nologin diyrag
sudo install -d -o diyrag -g diyrag -m 0750 /var/lib/diyrag
sudo install -d -o diyrag -g diyrag -m 0750 /etc/diyrag

# 3. Config + secrets (placeholders in the repo; real values here, NOT committed).
sudo cp /path/to/repo/.env.example /etc/diyrag/diyrag.env
sudo chown diyrag:diyrag /etc/diyrag/diyrag.env
sudo chmod 0640 /etc/diyrag/diyrag.env
sudo nano /etc/diyrag/diyrag.env            # set real secrets (§12.1)

# 4. Install + enable the unit (enable = auto-start on every boot).
sudo cp deploy/systemd/diyragd.service /etc/systemd/system/diyragd.service
sudo systemctl daemon-reload
sudo systemctl enable --now diyragd
```

---

## Control

```bash
# Via the CLI (identical surface on Windows / unraid / Linux, §16b.4):
diyrag service status
diyrag service start
diyrag service stop          # graceful: SIGTERM drains in-flight work (§16b.2)
diyrag service restart

# Native equivalents:
systemctl status diyragd
sudo systemctl start diyragd
sudo systemctl stop diyragd
journalctl -u diyragd -f      # follow logs (JSON; §13.1)
```

---

## Verify-after-reboot checklist (acceptance #9)

- [ ] `sudo systemctl reboot` (or simulate with `disable`/`enable`).
- [ ] **Enabled for boot:** `systemctl is-enabled diyragd` → `enabled`.
- [ ] **Running after boot:** `systemctl is-active diyragd` → `active`;
      `diyrag service status` agrees with a fresh uptime.
- [ ] **Health green:** `curl -k https://localhost:8443/healthz` and `/readyz`
      (8443 = api-gateway, matching the Docker stack).
- [ ] **Recovery works:** `sudo systemctl kill -s SIGKILL diyragd` — systemd
      restarts it within `RestartSec` (5s); the crash-loop limit
      (`StartLimitBurst`) bounds repeated failures (§14).
- [ ] **Graceful stop drains:** `diyrag service stop` returns after in-flight
      work units drain; restart shows no duplicate chunks (idempotency, §6.2).
- [ ] **Sandbox intact:** `systemd-analyze security diyragd` shows the hardening
      directives (`NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome`,
      `ReadWritePaths=/var/lib/diyrag`) in effect.

---

## Notes

- **Writable paths:** the unit confines writes to `/var/lib/diyrag` via
  `ReadWritePaths` + `StateDirectory`; `/etc/diyrag` stays read-only at runtime.
  If you add new state locations, extend `ReadWritePaths` accordingly.
- **GPU:** for the Rust-native `candle`/`mistral.rs` path, ensure the `diyrag`
  account can reach `/dev/nvidia*` (e.g. add it to the `video`/`render` group)
  and that drivers are installed machine-wide. Pin CUDA/cuDNN versions (§16).
- **Uninstall:** `diyrag service uninstall` (or `sudo systemctl disable --now
  diyragd && sudo rm /etc/systemd/system/diyragd.service && sudo systemctl
  daemon-reload`). State under `/var/lib/diyrag` is kept unless you remove it
  explicitly (mirrors the retention guarantee, §1 / §6.6).
