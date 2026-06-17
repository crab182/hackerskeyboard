#!/usr/bin/env bash
# diyRAG — generate a local .env with strong secrets from .env.example.
#
# Idempotent guard: refuses to overwrite an existing .env unless --force is
# passed (so you don't clobber live secrets by accident). The resulting .env is
# gitignored and MUST NOT be committed — production secrets belong in Docker
# secrets / SOPS / Vault (MASTER_BUILD_SPEC.md §12.1, §12.8).
#
# Usage:
#   scripts/gen-env.sh            # create .env (fails if one exists)
#   scripts/gen-env.sh --force    # regenerate, overwriting the existing .env
set -euo pipefail

cd "$(dirname "$0")/.."

force=""
[[ "${1:-}" == "--force" ]] && force="1"

if [[ -f .env && -z "$force" ]]; then
  echo "error: .env already exists. Re-run with --force to regenerate (this overwrites existing secrets)." >&2
  exit 1
fi
[[ -f .env.example ]] || { echo "error: .env.example not found (run from the repo root or via the Makefile/justfile)." >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo "error: python3 is required to generate secrets." >&2; exit 1; }

python3 - <<'PY'
import secrets, re, pathlib

pg    = secrets.token_urlsafe(18)
muser = "diyrag_" + secrets.token_hex(4)
mpw   = secrets.token_urlsafe(24)

vals = {
    "POSTGRES_PASSWORD":       pg,
    "DATABASE_URL":            f"postgres://diyrag:{pg}@postgres:5432/diyrag",
    "QDRANT_API_KEY":          secrets.token_urlsafe(24),
    "OBJECT_STORE_ACCESS_KEY": muser,
    "OBJECT_STORE_SECRET_KEY": mpw,
    "MINIO_ROOT_USER":         muser,
    "MINIO_ROOT_PASSWORD":     mpw,
    "REDIS_PASSWORD":          secrets.token_urlsafe(18),
    "JWT_SECRET":              secrets.token_urlsafe(48),
    "API_KEY_ARGON2_PEPPER":   secrets.token_hex(32),
}

out = []
for line in pathlib.Path(".env.example").read_text().splitlines():
    m = re.match(r"^([A-Z0-9_]+)=", line)
    out.append(f"{m.group(1)}={vals[m.group(1)]}" if (m and m.group(1) in vals) else line)
text = "\n".join(out) + "\n"
text = text.replace(
    "# 12-factor config (MASTER_BUILD_SPEC.md §19). Every value here is a SAFE\n"
    "# PLACEHOLDER. NO REAL SECRETS GO IN THIS FILE OR IN `.env` COMMITTED TO GIT.",
    "# 12-factor config (MASTER_BUILD_SPEC.md §19). THIS IS A LIVE .env WITH\n"
    "# GENERATED SECRETS. It is gitignored and MUST NOT be committed. For prod,\n"
    "# deliver these via Docker secrets / SOPS / Vault instead (§12.1, §12.8).")
pathlib.Path(".env").write_text(text)

print("Wrote .env with generated secrets (masked):")
for k, v in vals.items():
    if k == "DATABASE_URL":
        print("  DATABASE_URL             = postgres://diyrag:****@postgres:5432/diyrag")
    else:
        print(f"  {k:24} = {v[:2]}…{v[-2:]}  (len {len(v)})")
PY

echo
echo ".env is gitignored — do NOT commit it. Next: 'just up' (or 'make up') to start the datastores."
