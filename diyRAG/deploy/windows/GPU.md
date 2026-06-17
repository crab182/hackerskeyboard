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
| BGE-M3 dense + sparse embeddings | `ort` (ONNX Runtime) | **CUDA** or **DirectML** execution provider |
| `bge-reranker-v2-m3` cross-encoder | `ort` (ONNX Runtime) | **CUDA** or **DirectML** execution provider |
| Answer LLM generation | `mistral.rs` / `candle` | **CUDA** |
| OCR (scanned/complex docs) | Rust `ocrs`/Tesseract (CPU) | — (Surya/Marker GPU OCR is a Linux `parsing-service` profile, not Windows) |

This is what lets the **same release** run as a Windows Service *and* on unraid
(§24.1). On Linux/CUDA throughput nodes you may instead select the Python
`gpu-runtime` (vLLM) profile — that path does not exist on Windows.

---

## Choosing an execution provider: CUDA vs DirectML

`ort` (the Rust ONNX Runtime binding) selects an **execution provider (EP)**
for the embedding + reranker models. On Windows you have two GPU EPs:

### CUDA EP — NVIDIA only, fastest
- Best throughput/latency on NVIDIA GPUs; required if you also want the
  `mistral.rs`/`candle` CUDA LLM path on the same box.
- Needs the CUDA + cuDNN runtime versions that match the ONNX Runtime build
  you ship (pin them — §16, §12.9). Mismatched cuDNN is the #1 silent-fail.
- Set:
  ```
  ORT_EXECUTION_PROVIDER=cuda
  ```
  (Mirrors the default written by `install.ps1` into
  `%ProgramData%\diyRAG\config\diyrag.env`.)

### DirectML EP — vendor-neutral fallback
- Runs on **any DX12 GPU** (NVIDIA, AMD, Intel Arc/iGPU). No CUDA/cuDNN install.
- Lower peak throughput than CUDA but a great fit for mixed/AMD/Intel hardware
  or when you cannot install the CUDA toolkit.
- Set:
  ```
  ORT_EXECUTION_PROVIDER=directml
  ```
- DirectML accelerates the **ONNX** models (`ort`: embeddings + reranker).
  The `mistral.rs`/`candle` LLM path is **CUDA-only**; on a DirectML box the
  LLM falls back to CPU (or use `llama.cpp` with a DirectML/Vulkan build if you
  need GPU LLM on non-NVIDIA Windows — documented alternative, §3.2).

### CPU fallback
- `ORT_EXECUTION_PROVIDER=cpu` always works; correctness over speed.
- The platform also **auto-downgrades** to CPU on CUDA OOM/thermal events and
  logs the §14 hardware codes — see below.

---

## Driver & runtime requirements

Install these **machine-wide** (not per-user) so the session-0 service can use
them:

1. **GPU driver**
   - NVIDIA: a current Game Ready / Studio / datacenter driver. For CUDA EP +
     `mistral.rs`, the driver's bundled CUDA version must be ≥ the toolkit you
     ship against.
   - AMD / Intel: a current WDDM 2.x+ driver (DirectML rides on the OS DX12
     stack; no extra SDK needed at runtime).
2. **CUDA EP only:** matching **CUDA runtime + cuDNN** DLLs reachable on the
   service's `PATH` (or shipped beside `diyragd.exe`). Pin exact versions and
   record the model + runtime hashes (§12.9).
3. **DirectML EP:** `DirectML.dll` ships in modern Windows 10/11; ensure the OS
   is current. No SDK install.
4. **Windows 10 21H2+/Windows 11** recommended for current DX12 + driver
   feature levels.

Verify the GPU is visible to the host:

```powershell
nvidia-smi                      # NVIDIA: driver + CUDA version + VRAM
dxdiag /t dxdiag.txt            # any vendor: confirm DX12 + WDDM driver
```

---

## GPU under a session-0 Windows Service (the important gotcha)

Windows Services run in **session 0** with **no interactive desktop**. Key
consequences (§16b.2):

- **Headless GPU works.** `ort` (CUDA/DirectML) and `mistral.rs`/`candle` do
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
  `ort` CUDA EP → CPU EP, and `mistral.rs` → CPU — emitting the §14 hardware
  codes (`HW-OOM`, `HW-THERMAL-LIMIT`) against the affected job and alerting.
- Multi-GPU: schedule by device (process/sharding) and document the policy
  (§16). On a single-GPU homelab box this is a no-op.

---

## Quick checklist

- [ ] Machine-wide GPU driver installed (NVIDIA Studio/Game Ready, or current
      AMD/Intel WDDM driver).
- [ ] If CUDA EP: matching CUDA runtime + cuDNN on `PATH`; `nvidia-smi` works.
- [ ] `ORT_EXECUTION_PROVIDER` set in
      `%ProgramData%\diyRAG\config\diyrag.env` (`cuda` | `directml` | `cpu`).
- [ ] Service account can read `%ProgramData%\diyRAG\models`.
- [ ] After install, confirm the service started clean post-reboot and the
      Event Log shows the EP it selected (not a silent CPU fallback).
