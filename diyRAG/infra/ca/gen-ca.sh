#!/usr/bin/env bash
# diyRAG — generate an internal CA (MASTER_BUILD_SPEC.md §12.1).
#
# DEV / BOOTSTRAP ONLY. In production the Rust supervisor (`diyragd`) issues the
# CA and service certs programmatically with `rcgen` (§12.1, §17 first-run
# bootstrap). This OpenSSL script is a convenience for local development and CI
# so engineers do not need the Rust toolchain to spin up an mTLS mesh.
#
# Output goes to this directory (infra/ca/), which is GITIGNORED for *.crt/*.key/
# *.pem (see repo .gitignore §12.1) — private keys NEVER enter version control.
#
# Usage:
#   ./gen-ca.sh                 # creates ca.key + ca.crt (10y dev CA)
#   CA_DIR=/secure/path ./gen-ca.sh
set -euo pipefail

CA_DIR="${CA_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
CA_KEY="${CA_DIR}/ca.key"
CA_CRT="${CA_DIR}/ca.crt"
CA_CN="${CA_CN:-diyRAG Internal CA}"
# CA may be long-lived; LEAF certs are <= 90 days (see gen-cert.sh, §12.1).
CA_DAYS="${CA_DAYS:-3650}"

if ! command -v openssl >/dev/null 2>&1; then
  echo "error: openssl not found. Install it, or use 'diyragd' (rcgen) in prod." >&2
  exit 1
fi

mkdir -p "${CA_DIR}"
umask 077  # private key is created 0600

if [[ -f "${CA_KEY}" || -f "${CA_CRT}" ]]; then
  echo "refusing to overwrite existing CA at ${CA_DIR} (delete manually to rotate)." >&2
  exit 1
fi

echo ">> generating internal CA in ${CA_DIR}"

# Modern key: ECDSA P-256 (smaller, fast, supported by rustls).
openssl ecparam -name prime256v1 -genkey -noout -out "${CA_KEY}"
chmod 600 "${CA_KEY}"

openssl req -x509 -new -nodes \
  -key "${CA_KEY}" \
  -sha256 \
  -days "${CA_DAYS}" \
  -subj "/CN=${CA_CN}/O=diyRAG" \
  -addext "basicConstraints=critical,CA:TRUE,pathlen:1" \
  -addext "keyUsage=critical,keyCertSign,cRLSign" \
  -out "${CA_CRT}"

echo ">> done:"
echo "     CA cert: ${CA_CRT}  (distribute as the trust anchor; safe to share)"
echo "     CA key : ${CA_KEY}  (SECRET — gitignored; restrict ACLs; never in an image)"
echo
echo "Next: issue per-service certs with ./gen-cert.sh <service-name> [SAN ...]"
