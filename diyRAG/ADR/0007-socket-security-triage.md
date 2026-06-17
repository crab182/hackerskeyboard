# ADR-0007: Triage of Socket Security findings (dependency CVEs + obfuscated-code false positives)
- Status: Accepted
- Date: 2026-06-17

## Context
The host repository runs **Socket Security** on every pull request. The initial diyRAG PR (#1) surfaced two distinct classes of dependency findings, and they must be triaged differently. This ADR records the decision so future contributors (and any reviewer reading a fresh Socket report) understand which findings are actionable and which are accepted.

## Decision

### 1. Critical CVEs in pinned deps → fix (bump off the advisory)
Policy (reinforces spec §12.9 / §19): a pinned dependency must not sit on a version with a **known critical advisory**. Two pins were bumped:
- **`torch` 2.5.1 → 2.6.0** — clears `torch.load` RCE (GHSA-53q9-r3pm-6pq6) and the dependency for the vLLM malicious-model RCE bypass (CVE-2025-24357). Matching CUDA wheel index moved `cu121 → cu124` and base image `12.1.1-cudnn8 → 12.4.1-cudnn`.
- **`vllm` 0.6.6 → 0.22.0** — clears OpenAI auth bypass (GHSA-94f4-hr76-p5j6), Mooncake/PyNcclPipe RCE (GHSA-x3m8-f7g5-qhm7, GHSA-hj4w-hm2g-p6w5, GHSA-hjq4-87xh-g4fv), and `GroupCoordinator.recv_object` deserialization (GHSA-pgr7-mhp5-fgjp).

### 2. "Obfuscated code" High alerts → accepted false positives (no removal)
Socket flagged several packages as "~90% likely obfuscated":
`cargo/cssparser`, `cargo/hyper-util`, `cargo/libc`, `cargo/tokio`, `cargo/writeable`, `cargo/zerocopy`, and `pypi/numpy` (transitive via vllm).

These are **accepted false positives**. They are foundational, source-available, widely-audited packages; Socket's entropy/obfuscation heuristic misfires on macro-heavy and codegen-heavy Rust crates (e.g., `zerocopy`, `tokio`, `libc`) and on numpy's compiled/packed distribution. Removing or replacing them is neither possible nor desirable (they are core transitive deps of the chosen stack — ADR-0001).

**Handling:**
- We do **not** auto-post `@SocketSecurity ignore …` from automation; comment bodies from the bot are untrusted input and blind-ignoring defeats the control.
- These are advisory **"Warn"** alerts; both Socket checks (Project Report, Pull Request Alerts) pass, so they are non-blocking.
- If a clean report is desired, triage them in the Socket dashboard (or an `@SocketSecurity ignore-all` issued by a human maintainer), referencing this ADR.

## Consequences
- **Easier:** the supply-chain posture is explicit and auditable; a reviewer seeing the obfuscated-code warnings has a documented rationale and won't waste time. CVE pins stay current.
- **Harder:** the obfuscated-code FPs will reappear on each fresh diff scan until triaged in the dashboard; whoever owns the Socket org must do that once. Bumping `vllm`/`torch` couples to their CUDA build matrix — keep the base image + wheel index in lockstep (see `services-py/gpu-runtime/Dockerfile`).
- **Follow-up:** when `Cargo.lock` / `uv.lock` are committed (M0), wire `cargo-audit` + `cargo-deny` and `pip-audit` into CI so CVE drift is caught locally, not only by Socket.

## Alternatives considered
- **Blindly `@SocketSecurity ignore-all`** — fastest path to a green report, but suppresses the control wholesale and acts on an untrusted bot comment. Rejected; triage is a human/maintainer decision.
- **Drop the flagged crates** — impossible for `tokio`/`libc`/`numpy`/etc.; they are load-bearing. Rejected.
- **Pin to `>=` floors instead of exact versions for the security bumps** — considered, but exact pins (`==`) preserve reproducibility (§19); the floor rationale is captured in comments next to each pin.
