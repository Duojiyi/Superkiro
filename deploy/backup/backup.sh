#!/usr/bin/env bash
# Kiro BYOK Gateway - durable JSON snapshot backup (Spec §7, T07)
# Enforces quiescence check/sync, paired generation/anchor/manifest, and complete-generation pruning.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
BACKUP_DIR="${BACKUP_DIR:-${SCRIPT_DIR}/../../backups}"
DATA_DIR="${DATA_DIR:-${SCRIPT_DIR}/../../data}"
DATA_FILE="${DATA_FILE:-${DATA_DIR}/billing_state.json}"
ANCHOR_FILE="${ANCHOR_FILE:-${DATA_FILE}.anchor}"
RETENTION_DAYS="${RETENTION_DAYS:-7}"
HOST="${HOST:-127.0.0.1}"
PORT="${PORT:-19820}"
HEALTH_URL="${HEALTH_URL:-http://${HOST}:${PORT}/healthz}"
# Set ADMIN_BASE_URL to the reachable HTTPS/Caddy URL in Compose deployments.
ADMIN_BASE_URL="${ADMIN_BASE_URL:-http://${HOST}:${PORT}}"
ADMIN_KEY="${ADMIN_KEY:-}"
FORCE="${FORCE:-false}"

umask 077

# Parse optional command line flags
while [[ $# -gt 0 ]]; do
  case "$1" in
    --force)
      FORCE="true"
      shift
      ;;
    --admin-key)
      ADMIN_KEY="$2"
      shift 2
      ;;
    *)
      echo "Unknown option: $1" >&2
      exit 1
      ;;
  esac
done

# Step 1: Quiescence / Consistency Check (T07)
# Verify whether gateway is actively running
GATEWAY_ONLINE=false
if command -v curl >/dev/null 2>&1; then
  if curl -s -m 2 "${HEALTH_URL}" >/dev/null 2>&1; then
    GATEWAY_ONLINE=true
  fi
fi
# Compose does not publish the gateway port in production. Detect its container
# state too, so an unreachable host port cannot be mistaken for a stopped gateway.
if [[ "${GATEWAY_ONLINE}" != "true" ]] && command -v docker >/dev/null 2>&1     && [[ -f "${SCRIPT_DIR}/../docker-compose.yml" ]]; then
  if docker compose -f "${SCRIPT_DIR}/../docker-compose.yml" ps --status running --services 2>/dev/null       | grep -qx gateway; then
    GATEWAY_ONLINE=true
  fi
fi

if [[ "${GATEWAY_ONLINE}" == "true" ]]; then
  if [[ -n "${ADMIN_KEY}" ]]; then
    echo "[*] Gateway is running at http://${HOST}:${PORT}. Triggering synchronized snapshot flush via Admin API..."
    SESSION_RESP="$(curl -s -f -m 5 -X POST "${ADMIN_BASE_URL}/api/v1/admin/session" \
      -H "x-admin-key: ${ADMIN_KEY}" -H "Content-Type: application/json" || true)"
    ADMIN_TOKEN=""
    if command -v python3 >/dev/null 2>&1; then
      ADMIN_TOKEN="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("accessToken", ""))' <<<"${SESSION_RESP}")"
    fi
    SYNC_RESP="$(curl -s -f -m 5 -X POST "${ADMIN_BASE_URL}/api/v1/admin/snapshot/sync" \
      -H "Authorization: Bearer ${ADMIN_TOKEN}" \
      -H "Content-Type: application/json" || true)"
    if echo "${SYNC_RESP}" | grep -q '"synchronized"'; then
      echo "[√] Application snapshot synchronized successfully."
    else
      echo "Error: Failed to trigger synchronized snapshot via Admin API. Response: ${SYNC_RESP}" >&2
      if [[ "${FORCE}" != "true" ]]; then
        exit 1
      fi
    fi
  else
    if [[ "${FORCE}" != "true" ]]; then
      echo "Error: Gateway is actively running at http://${HOST}:${PORT}." >&2
      echo "To prevent partial reads and race conditions, provide ADMIN_KEY to trigger synchronized export via admin API," >&2
      echo "or stop the gateway service before running backup.sh (or pass --force to bypass at your own risk)." >&2
      exit 1
    else
      echo "[!] Warning: Gateway is running and ADMIN_KEY is not set. Proceeding due to --force flag."
    fi
  fi
fi

# Step 2: Validate live source files
if [[ ! -f "${DATA_FILE}" ]]; then
  echo "Error: billing snapshot '${DATA_FILE}' not found" >&2
  exit 1
fi
if [[ ! -s "${DATA_FILE}" ]]; then
  echo "Error: billing snapshot '${DATA_FILE}' is empty (0 bytes)" >&2
  exit 1
fi
if [[ ! -f "${ANCHOR_FILE}" ]]; then
  echo "Error: billing snapshot anchor '${ANCHOR_FILE}' not found" >&2
  exit 1
fi
if ! command -v sha256sum >/dev/null 2>&1; then
  echo "Error: sha256sum is required" >&2
  exit 1
fi

mkdir -p "${BACKUP_DIR}"

# Step 3: Paired generation, anchor, and manifest creation (T07)
# Use timestamp + random + PID to prevent concurrent backups from colliding
DATE_STR="$(date +%Y%m%d_%H%M%S)"
GEN_ID="${DATE_STR}_${RANDOM}_$$"
TARGET_PREFIX="${BACKUP_DIR}/billing_state_${GEN_ID}"
TARGET_FILE="${TARGET_PREFIX}.json"
TARGET_ANCHOR="${TARGET_FILE}.anchor"
CHECKSUM_FILE="${TARGET_FILE}.sha256"
MANIFEST_FILE="${TARGET_PREFIX}.manifest.json"

TMP_FILE="${TARGET_FILE}.tmp.$$"
TMP_ANCHOR="${TARGET_ANCHOR}.tmp.$$"
TMP_CHECKSUM="${CHECKSUM_FILE}.tmp.$$"
TMP_MANIFEST="${MANIFEST_FILE}.tmp.$$"
GENERATION_FILE=""
GENERATION_TMP=""

cleanup() {
  rm -f -- "${TMP_FILE}" "${TMP_ANCHOR}" "${TMP_CHECKSUM}" "${TMP_MANIFEST}" ${GENERATION_TMP:+"${GENERATION_TMP}"}
}
trap cleanup EXIT

# Copy to temporary staging files
cp -- "${DATA_FILE}" "${TMP_FILE}"
cp -- "${ANCHOR_FILE}" "${TMP_ANCHOR}"
# A committed anchor may point at a generation file when the convenience
# mirror is stale. Carry that generation into the bundle as well.
GENERATION_FILE=""
if command -v python3 >/dev/null 2>&1; then
  GENERATION_FILE="$(python3 - "${TMP_ANCHOR}" <<'PY'
import json, sys
print(json.load(open(sys.argv[1], encoding="utf-8")).get("generation_file") or "")
PY
)"
fi
if [[ -n "${GENERATION_FILE}" ]]; then
  if [[ "${GENERATION_FILE}" == */* || "${GENERATION_FILE}" == *\\* || "${GENERATION_FILE}" == .* ]]; then
    echo "Error: invalid generation filename in anchor" >&2; exit 1
  fi
  if [[ ! -f "${DATA_DIR}/${GENERATION_FILE}" ]]; then
    echo "Error: authoritative generation '${DATA_DIR}/${GENERATION_FILE}' not found" >&2; exit 1
  fi
  cp -- "${DATA_DIR}/${GENERATION_FILE}" "${BACKUP_DIR}/.${GENERATION_FILE}.tmp.$$"
  GENERATION_TMP="${BACKUP_DIR}/.${GENERATION_FILE}.tmp.$$"
else
  GENERATION_TMP=""
fi
chmod 600 "${TMP_FILE}" "${TMP_ANCHOR}" ${GENERATION_TMP:+"${GENERATION_TMP}"}

# Compute cryptographic digests
SNAPSHOT_HASH="$(sha256sum "${TMP_FILE}" | awk '{print $1}')"
ANCHOR_HASH="$(sha256sum "${TMP_ANCHOR}" | awk '{print $1}')"

# Verify anchor matches snapshot before publishing
if command -v python3 >/dev/null 2>&1; then
  python3 - "${TMP_ANCHOR}" "${SNAPSHOT_HASH}" <<'PY'
import json, sys
anchor_path, actual_hash = sys.argv[1], sys.argv[2]
with open(anchor_path, "r", encoding="utf-8") as f:
    data = json.load(f)
expected_hash = data.get("checksum")
if expected_hash != actual_hash:
    sys.exit(f"Anchor checksum mismatch: expected {expected_hash}, got {actual_hash}")
PY
fi

printf '%s  %s\n' "${SNAPSHOT_HASH}" "$(basename "${TARGET_FILE}")" > "${TMP_CHECKSUM}"
chmod 600 "${TMP_CHECKSUM}"

# Build Manifest file
cat > "${TMP_MANIFEST}" <<JSON
{
  "version": 1,
  "generation_id": "${GEN_ID}",
  "created_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "snapshot_file": "$(basename "${TARGET_FILE}")",
  "snapshot_sha256": "${SNAPSHOT_HASH}",
  "anchor_file": "$(basename "${TARGET_ANCHOR}")",
  "anchor_sha256": "${ANCHOR_HASH}",
  "status": "completed"
}
JSON
chmod 600 "${TMP_MANIFEST}"

# Step 4: Atomic publication sequence
# Manifest is published LAST as the atomic commit point of the backup bundle
mv -f -- "${TMP_FILE}" "${TARGET_FILE}"
mv -f -- "${TMP_ANCHOR}" "${TARGET_ANCHOR}"
if [[ -n "${GENERATION_TMP}" ]]; then
  mv -f -- "${GENERATION_TMP}" "${BACKUP_DIR}/${GENERATION_FILE}"
fi
mv -f -- "${TMP_CHECKSUM}" "${CHECKSUM_FILE}"
mv -f -- "${TMP_MANIFEST}" "${MANIFEST_FILE}"

BACKUP_SIZE="$(du -h -- "${TARGET_FILE}" | cut -f1)"
echo "[$(date -Iseconds)] Backup completed: ${TARGET_FILE} (${BACKUP_SIZE})"
echo "[$(date -Iseconds)] SHA-256: ${CHECKSUM_FILE}"
echo "[$(date -Iseconds)] Manifest: ${MANIFEST_FILE}"

# Step 5: Complete-Generation Retention Pruning (T07)
# Prunes entire backup sets together based on manifests; never deletes loose individual files
if command -v find >/dev/null 2>&1; then
  while IFS= read -r manifest; do
    if [[ -n "${manifest}" ]]; then
      BASE="${manifest%.manifest.json}"
      echo "[*] Pruning expired backup generation: ${BASE}"
      rm -f -- "${manifest}" "${BASE}.json" "${BASE}.json.anchor" "${BASE}.json.sha256"
    fi
  done < <(find "${BACKUP_DIR}" -type f -name 'billing_state_*.manifest.json' -mtime +"${RETENTION_DAYS}" 2>/dev/null || true)
fi

echo "[$(date -Iseconds)] Backup retention pruning completed."
