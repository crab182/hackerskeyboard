#!/usr/bin/env bash
# diyRAG — issue a short-lived (<=90 day) service cert from the internal CA
# (MASTER_BUILD_SPEC.md §12.1).
#
# DEV / BOOTSTRAP ONLY. Production issues + auto-rotates these with `rcgen` from
# `diyragd` (rotate >= 7 days before expiry; failed rotation = high-sev alert +
# identity quarantine — §12.1). This script mirrors that policy for local mTLS.
#
# The cert carries both serverAuth and clientAuth EKUs so a single identity can
# act as both ends of an mTLS connection (east-west, sync, inference paths).
#
# Output goes to infra/ca/ (GITIGNORED for *.crt/*.key/*.pem). Keys never commit.
#
# Usage:
#   ./gen-cert.sh api-gateway diyrag.local api-gateway 127.0.0.1
#   ./gen-cert.sh sync-agent node-a.lan
#   CERT_DAYS=30 ./gen-cert.sh gpu-runtime gpu-runtime
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <service-name> [SAN-dns-or-ip ...]" >&2
  exit 2
fi

SERVICE="$1"; shift
SANS=("$@")

CA_DIR="${CA_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
CA_KEY="${CA_DIR}/ca.key"
CA_CRT="${CA_DIR}/ca.crt"
# §12.1: leaf cert lifespan <= 90 days. Clamp the requested value.
CERT_DAYS="${CERT_DAYS:-90}"
if (( CERT_DAYS > 90 )); then
  echo "error: CERT_DAYS=${CERT_DAYS} exceeds the 90-day policy ceiling (§12.1)." >&2
  exit 1
fi

if ! command -v openssl >/dev/null 2>&1; then
  echo "error: openssl not found." >&2
  exit 1
fi
if [[ ! -f "${CA_KEY}" || ! -f "${CA_CRT}" ]]; then
  echo "error: CA not found in ${CA_DIR}. Run ./gen-ca.sh first." >&2
  exit 1
fi

KEY="${CA_DIR}/${SERVICE}.key"
CSR="${CA_DIR}/${SERVICE}.csr"
CRT="${CA_DIR}/${SERVICE}.crt"

umask 077

# Build the SAN list. Always include the service name as a DNS SAN.
SAN_ENTRIES=("DNS:${SERVICE}")
for s in "${SANS[@]}"; do
  if [[ "${s}" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    SAN_ENTRIES+=("IP:${s}")
  else
    SAN_ENTRIES+=("DNS:${s}")
  fi
done
SAN_CSV=$(IFS=, ; echo "${SAN_ENTRIES[*]}")

echo ">> issuing cert for '${SERVICE}' (${CERT_DAYS}d) SANs=[${SAN_CSV}]"

# Leaf key: ECDSA P-256.
openssl ecparam -name prime256v1 -genkey -noout -out "${KEY}"
chmod 600 "${KEY}"

openssl req -new -key "${KEY}" -subj "/CN=${SERVICE}/O=diyRAG" -out "${CSR}"

# Sign with serverAuth + clientAuth EKUs and the SANs.
openssl x509 -req \
  -in "${CSR}" \
  -CA "${CA_CRT}" -CAkey "${CA_KEY}" -CAcreateserial \
  -sha256 \
  -days "${CERT_DAYS}" \
  -extfile <(printf 'subjectAltName=%s\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth,clientAuth\nbasicConstraints=critical,CA:FALSE\n' "${SAN_CSV}") \
  -out "${CRT}"

rm -f "${CSR}"

echo ">> done:"
echo "     cert: ${CRT}"
echo "     key : ${KEY}  (SECRET — gitignored; mount read-only; rotate before expiry)"
echo
echo "Verify:  openssl verify -CAfile ${CA_CRT} ${CRT}"
echo "Fingerprint (pin in 'nodes' for sync peers — §9):"
openssl x509 -in "${CRT}" -noout -fingerprint -sha256
