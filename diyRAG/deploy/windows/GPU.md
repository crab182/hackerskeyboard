# Windows GPU notes

> MASTER_BUILD_SPEC.md §16 ("Windows GPU"), §16b.2 ("GPU under a service"),
> §3.2, §14 (GPU failsafe), §24.1.

## TL;DR

On Windows, **the Rust-native inference backend is the only GPU path.**
vLLM is **not** targeted on Windows. GPU acceleration for embeddings,
reranking, and LLM generation runs in-process inside the `all-in-one`
`diyragd` Windows Service via:

| Workload | Backend (Windows) | GPU via |
|---|---|---|
| BGE-M3 **dense** embeddings | `candle` (XLM-RoBERTa) | **CUDA** (no DirectML — see below) |
| `bge-reranker-v2-m3` cross-encoder | `candle` (XLM-RoBERTa) | **CUDA** (no DirectML — see below) |
| Answer LLM generation | `mistral.rs` / `candle` | **CUDA** |
| OCR (scanned/complex docs) | Rust `ocrs`/Tesseract (CPU) | — (Surya/Marker GPU OCR is a Linux `parsing-service` profile, not Windows) |

> **Dense only in-proc.** candle's XLM-RoBERTa exposes the encoder + rerank
> head but **not** BGE-M3's learned-sparse head, so the Rust-native backend
> emits **dense + rerank** only. The **sparse** signal for sparse/hybrid
> retrieval comes from the Python `gpu-runtime` ([ADR-0009](../../ADR/0009-candle-replaces-ort.md)).

This is what lets the **same release** run as a Windows Service *and* on unraid
(§24.1). On Linux/CUDA throughput nodes you may instead select the Python
`gpu-runtime` (vLLM) profile — that path does not exist on Windows.

---

## Choosing a candle device: CPU vs CUDA (no DirectML)

`candle` (the pure-Rust ML framework) runs the embedding + reranker models on
a **device** chosen by `DIYRAG_CANDLE_DEVICE`. candle supports **CPU, CUDA,
and Metal** — there is **no DirectML**. On Windows that leaves two practical
choices, with an honest gap for non-NVIDIA GPUs ([ADR-0009](../../ADR/0009-candle-replaces-ort.md)).

### CUDA — NVIDIA only, fastest
- Best throughput/latency on NVIDIA GPUs; required if you also want the
  `mistral.rs`/`candle` CUDA LLM path on the same box.
- Compiled into a **GPU build of the image** (candle's `cuda` cargo feature);
  needs the CUDA + cuDNN runtime versions that match that build (pin them —
  §16, §12.9). Mismatched cuDNN is the #1 silent-fail.
- Set:
  ```
  DIYRAG_CANDLE_DEVICE=cuda
  ```
  (Mirrors the default written by `install.ps1` into
  `%ProgramData%\diyRAG\config\diyrag.env`.)

### No DirectML — what AMD / Intel GPUs do instead
- candle has **no DirectML / DX12 path**, so AMD and Intel Arc/iGPU Windows
  GPUs no longer get an in-proc Rust acceleration path for embed/rerank. This
  is a real regression vs the old `ort` DirectML execution provider.
- On those boxes the in-proc backend **falls back to CPU**. For GPU throughput
  on non-NVIDIA hardware, point embed/rerank at the Python `gpu-runtime`
  profile instead (selected by config, no code change — ADR-0004).
- The `mistral.rs`/`candle` LLM path was already **CUDA-only**; on non-NVIDIA
  Windows the LLM likewise falls back to CPU (or use `llama.cpp` with a
  Vulkan build if you need GPU LLM there — documented alternative, §3.2).

### CPU fallback (default)
- `DIYRAG_CANDLE_DEVICE=cpu` is the **default** and always works; correctness
  over speed. Every node builds and boots on it without a GPU image.
- The platform also **auto-downgrades** to CPU on CUDA OOM/thermal events and
  logs the §14 hardware codes — see below.

---

## Driver & runtime requirements

Install these **machine-wide** (not per-user) so the session-0 service can use
them:

1. **GPU driver**
   - NVIDIA: a current Game Ready / Studio / datacenter driver. For CUDA +
     `mistral.rs`, the driver's bundled CUDA version must be ≥ the toolkit the
     GPU build ships against.
   - AMD / Intel: a current WDDM 2.x+ driver. Note that candle has **no
     DirectML path** — embed/rerank fall back to CPU on these GPUs (use the
     Python `gpu-runtime` for GPU throughput there).
2. **CUDA only:** matching **CUDA runtime + cuDNN** DLLs reachable on the
   service's `PATH` (or shipped beside `diyragd.exe`). Pin exact versions and
   record the model + runtime hashes (§12.9).
3. **Windows 10 21H2+/Windows 11** recommended for current driver feature
   levels.

Verify the GPU is visible to the host:

```powershell
nvidia-smi                      # NVIDIA: driver + CUDA version + VRAM
dxdiag /t dxdiag.txt            # any vendor: confirm DX12 + WDDM driver
```

---

## GPU under a session-0 Windows Service (the important gotcha)

Windows Services run in **session 0** with **no interactive desktop**. Key
consequences (§16b.2):

- **Headless GPU works.** `candle` (CUDA) and `mistral.rs`/`candle` do
  **not** need a desktop, an interactive login, or a logged-in user. The
  service can use the GPU after a clean boot before anyone signs in — exactly
  what the boot-autostart requirement needs.
- **Drivers must be installed for the whole machine**, because the service
  account is not the install user. A driver installed "for current user" is
  invisible to the service.
- **Account choice (§12.8, §22 #13):** prefer the dedicated low-privilege
  account `NT SERVICE\diyRAG`. If a specific driver/runtime requires elevated
  device access that the virtual account cannot get, the documented trade-off
  is to run under a more privileged account — record it as a `DECISION:` and
  keep the install dir + `%ProgramData%\diyRAG` ACL-locked regardless. Avoid
  LocalSystem unless GPU access genuinely forces it.
- **No `nvidia-smi` GUI tools in session 0**, but the CLI `nvidia-smi` and the
  service's own telemetry still report VRAM/temperature for the §13.3 metrics.
- **Model cache:** the service reads models from
  `%ProgramData%\diyRAG\models` (ACL-locked, machine-wide), not a user profile
  path — so the cache survives across users and reboots.

---

## VRAM governance & failsafe (§14, §16)

- The in-process backend owns the device(s), sizes embedding batches up to the
  VRAM limit (target batch ≥ 32), and queues requests so a runaway job cannot
  starve interactive queries (§15).
- On **CUDA OOM** or **thermal throttle**, the backend gracefully downgrades:
  `candle` CUDA → CPU, and `mistral.rs` → CPU — emitting the §14 hardware
  codes (`HW-OOM`, `HW-THERMAL-LIMIT`) against the affected job and alerting.
- Multi-GPU: schedule by device (process/sharding) and document the policy
  (§16). On a single-GPU homelab box this is a no-op.

---

## Quick checklist

- [ ] Machine-wide GPU driver installed (NVIDIA Studio/Game Ready; AMD/Intel
      WDDM drivers work but get no in-proc acceleration — CPU or `gpu-runtime`).
- [ ] If CUDA: GPU image build + matching CUDA runtime + cuDNN on `PATH`;
      `nvidia-smi` works.
- [ ] `DIYRAG_CANDLE_DEVICE` set in
      `%ProgramData%\diyRAG\config\diyrag.env` (`cuda` | `cpu`; `metal` is
      macOS-only). `DIYRAG_EMBED_MODEL_DIR` / `DIYRAG_RERANK_MODEL_DIR` point
      at the local model dirs.
- [ ] Service account can read `%ProgramData%\diyRAG\models`.
- [ ] After install, confirm the service started clean post-reboot and the
      Event Log shows the device it selected (not a silent CPU fallback).
