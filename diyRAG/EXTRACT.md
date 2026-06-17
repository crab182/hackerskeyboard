# Extracting diyRAG into its own private repository

diyRAG currently lives under `diyRAG/` inside `crab182/hackerskeyboard` (that was
the only repo this build session could push to). It was authored as a **self-contained
repo root** — its own `Cargo.toml` workspace, `.gitignore`, `LICENSE`, `README.md`,
`ARCHITECTURE.md`, `ADR/`, and `.github/workflows/ci.yml`. This guide lifts it into a
standalone **private** `diyRAG` repo. The CI workflow (`.github/workflows/ci.yml`)
only runs once diyRAG is a repo root, so it activates automatically after extraction.

Pick **one** method.

---

## Method A — `git subtree split` (preserves only diyRAG's history; simple)

```bash
# from a clone of crab182/hackerskeyboard, on the branch holding diyRAG/
git switch claude/exciting-franklin-99vuda

# 1) create a branch whose root IS the diyRAG/ subtree
git subtree split --prefix=diyRAG -b diyrag-only

# 2) make the new repo on GitHub (private). With the gh CLI:
gh repo create diyRAG --private --disable-wiki

# 3) push the split branch as the new repo's main
git push git@github.com:<you>/diyRAG.git diyrag-only:main

# 4) (optional) work in a fresh clone
git clone git@github.com:<you>/diyRAG.git
cd diyRAG && git switch main
```

The paths in the new repo are already correct: `crates/`, `services-py/`,
`deploy/`, etc. sit at the root — no rewriting needed.

---

## Method B — `git filter-repo` (cleanest history rewrite; recommended for a true fresh start)

```bash
pip install git-filter-repo   # if not present

# work on a SCRATCH clone — filter-repo rewrites history irreversibly
git clone --no-local crab182/hackerskeyboard diyrag-extract
cd diyrag-extract
git switch claude/exciting-franklin-99vuda

# keep only diyRAG/ and hoist it to the repo root
git filter-repo --path diyRAG/ --path-rename diyRAG/:

# point at the new private repo and push
gh repo create diyRAG --private --disable-wiki
git remote add origin git@github.com:<you>/diyRAG.git
git push -u origin HEAD:main
```

`--path-rename diyRAG/:` strips the `diyRAG/` prefix so `crates/…` lands at the root.

---

## Method C — no history (snapshot a clean repo)

```bash
# from a checkout that contains diyRAG/
cp -r diyRAG /tmp/diyRAG && cd /tmp/diyRAG
git init -b main
git add .
git commit -m "diyRAG: initial import (Rust-first self-hosted RAG platform)"
gh repo create diyRAG --private --source=. --remote=origin --push
```

---

## After extraction — verify

```bash
cd diyRAG                      # the new repo root
ls Cargo.toml crates/ services-py/ .github/workflows/ci.yml
cargo fmt --all -- --check     # CI gate 1
cargo clippy --workspace --all-targets -- -D warnings   # gate 2 (expect TODO-driven warnings until M0–M2 land)
cargo metadata --no-deps >/dev/null   # workspace resolves
```

CI (`.github/workflows/ci.yml`) now triggers on push/PR in the new repo: Rust
fmt/clippy/test, `cargo-deny` + `cargo-audit`, Python ruff/pytest for the two
sidecars, and a Trivy scan. See `MASTER_BUILD_SPEC.md` §20 for the phased build
order to turn the scaffold into a running system.

> Keep the repo **private** (the spec assumes a self-hosted/SMB posture). No
> secrets are committed; `.gitignore` already excludes `.env`, certs, and model
> caches.
