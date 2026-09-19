#!/usr/bin/env bash
# Kiro BYOK Gateway - verified atomic JSON snapshot restore (Spec §7, T07)
# Enforces running-gateway prevention, engine validation in temporary sandbox,
# and complete-generation rollback before modifying live data.
set -euo pipefail

if [[ "$#" -lt 1 ]]; then
  echo "Usage: $0 <backup_file.json|backup_manifest.json> [--force]" >&2
  exit 1
fi

RAW_INPUT="$1"
FORCE=false
if [[ "${2:-}" == "--force" ]] || [[ "${1:-}" == "--force" ]]; then
  FORCE=true
  if [[ "${1:-}" == "--force" ]]; then
    RAW_INPUT="${2:-}"
  fi
fi

if [[ -z "${RAW_INPUT}" ]]; then
  echo "Usage: $0 <backup_file.json|backup_manifest.json> [--force]" >&2
  exit 1
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
DATA_DIR="${DATA_DIR:-${SCRIPT_DIR}/../../data}"
TARGET_FILE="${DATA_FILE:-${DATA_DIR}/billing_state.json}"
TARGET_ANCHOR="${TARGET_FILE}.anchor"
HOST="${HOST:-127.0.0.1}"
PORT="${PORT:-19820}"
HEALTH_URL="${HEALTH_URL:-http://${HOST}:${PORT}/healthz}"

umask 077
GENERATION_FILE=""
GENERATION_SOURCE=""

# Step 1: Prevent running gateway from being overwritten and overwriting back (T07)
# A running gateway holds dirty in-memory state that will overwrite restored disk state on flush
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
  if [[ "${FORCE}" != "true" ]]; then
    echo "Error: Kiro Gateway is actively running at http://${HOST}:${PORT}." >&2
    echo "Refusing to restore over live state because the running gateway's in-memory engine will" >&2
    echo "periodically flush and overwrite the restored state. Stop the gateway service before restore" >&2
    echo "(or pass --force to bypass at your own risk)." >&2
    exit 1
  else
    echo "[!] Warning: Gateway is running. Proceeding with restore due to --force flag."
  fi
fi

# Step 2: Resolve and validate backup artifact bundle
BACKUP_PATH="${RAW_INPUT}"
if [[ "${BACKUP_PATH}" == *.manifest.json ]]; then
  BACKUP_BASE="${BACKUP_PATH%.manifest.json}"
elif [[ "${BACKUP_PATH}" == *.json.anchor ]]; then
  BACKUP_BASE="${BACKUP_PATH%.json.anchor}"
elif [[ "${BACKUP_PATH}" == *.json.sha256 ]]; then
  BACKUP_BASE="${BACKUP_PATH%.json.sha256}"
elif [[ "${BACKUP_PATH}" == *.json ]]; then
  BACKUP_BASE="${BACKUP_PATH%.json}"
else
  BACKUP_BASE="${BACKUP_PATH}"
fi

BACKUP_FILE="${BACKUP_BASE}.json"
ANCHOR_FILE="${BACKUP_BASE}.json.anchor"
CHECKSUM_FILE="${BACKUP_BASE}.json.sha256"
MANIFEST_FILE="${BACKUP_BASE}.manifest.json"

if [[ ! -f "${BACKUP_FILE}" ]]; then
  echo "Error: backup snapshot file '${BACKUP_FILE}' not found" >&2
  exit 1
fi
if [[ ! -s "${BACKUP_FILE}" ]]; then
  echo "Error: backup snapshot file '${BACKUP_FILE}' is empty (0 bytes)" >&2
  exit 1
fi
if [[ ! -f "${ANCHOR_FILE}" ]]; then
  echo "Error: snapshot anchor '${ANCHOR_FILE}' not found" >&2
  exit 1
fi
if [[ ! -f "${CHECKSUM_FILE}" ]]; then
  echo "Error: checksum sidecar '${CHECKSUM_FILE}' not found" >&2
  exit 1
fi
if [[ ! -f "${MANIFEST_FILE}" ]]; then
  echo "Error: backup manifest '${MANIFEST_FILE}' not found (incomplete backup bundle)" >&2
  exit 1
fi
if ! command -v sha256sum >/dev/null 2>&1; then
  echo "Error: sha256sum is required" >&2
  exit 1
fi

# Check manifest status
if command -v python3 >/dev/null 2>&1; then
  python3 - "${MANIFEST_FILE}" <<'PY'
import json, sys
with open(sys.argv[1], "r", encoding="utf-8") as f:
    manifest = json.load(f)
if manifest.get("status") != "completed":
    sys.exit(f"Backup manifest status is not 'completed': {manifest.get('status')}")
PY
fi

# Verify sha256 sidecar
BACKUP_DIRNAME="$(dirname -- "${BACKUP_FILE}")"
CHECKSUM_BASENAME="$(basename -- "${CHECKSUM_FILE}")"
(
  cd -- "${BACKUP_DIRNAME}"
  sha256sum -c -- "${CHECKSUM_BASENAME}"
)

# Step 3: Temporary sandbox pre-verification with real engine (T07)
# Verifies decryption, checksum, anchor matching, and ledger invariants before touching live data
STAGE_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t kiro_stage)"
cleanup_stage() {
  rm -rf -- "${STAGE_DIR}"
}
trap cleanup_stage EXIT

cp -- "${BACKUP_FILE}" "${STAGE_DIR}/billing_state.json"
cp -- "${ANCHOR_FILE}" "${STAGE_DIR}/billing_state.json.anchor"
if command -v python3 >/dev/null 2>&1; then
  GENERATION_FILE="$(python3 - "${ANCHOR_FILE}" <<'PY'
import json, sys
print(json.load(open(sys.argv[1], encoding="utf-8")).get("generation_file") or "")
PY
)"
  if [[ -n "${GENERATION_FILE}" ]]; then
    GENERATION_SOURCE="$(dirname -- "${BACKUP_FILE}")/${GENERATION_FILE}"
    if [[ ! "${GENERATION_FILE}" =~ ^[A-Za-z0-9._-]+$ || "${GENERATION_FILE}" == .* || ! -f "$(dirname -- "${BACKUP_FILE}")/${GENERATION_FILE}" ]]; then
      echo "Error: authoritative generation referenced by anchor is missing or invalid" >&2; exit 1
    fi
    cp -- "$(dirname -- "${BACKUP_FILE}")/${GENERATION_FILE}" "${STAGE_DIR}/${GENERATION_FILE}"
  fi
fi
chmod 600 "${STAGE_DIR}"/*

# The Rust gateway verifier is mandatory. Structural JSON parsing is not a
# substitute for AEAD, generation and ledger invariant checks.
VERIFY_SUCCESS=false
if command -v kiro-gateway >/dev/null 2>&1; then
  kiro-gateway verify-snapshot "${STAGE_DIR}/billing_state.json" && VERIFY_SUCCESS=true
elif [[ -x "${SCRIPT_DIR}/../../target/release/gateway" ]]; then
  "${SCRIPT_DIR}/../../target/release/gateway" verify-snapshot "${STAGE_DIR}/billing_state.json" && VERIFY_SUCCESS=true
elif [[ -x "${SCRIPT_DIR}/../../target/debug/gateway" ]]; then
  "${SCRIPT_DIR}/../../target/debug/gateway" verify-snapshot "${STAGE_DIR}/billing_state.json" && VERIFY_SUCCESS=true
elif command -v cargo >/dev/null 2>&1; then
  ( cd -- "${SCRIPT_DIR}/../.." && cargo run -q -p gateway --bin gateway -- verify-snapshot "${STAGE_DIR}/billing_state.json" ) && VERIFY_SUCCESS=true
fi

if [[ "${VERIFY_SUCCESS}" != "true" ]]; then
  echo "Error: Snapshot engine verification failed! Aborting restore BEFORE touching live state." >&2
  exit 1
fi
echo "[√] Sandbox verification passed cleanly."

# Step 4: Complete Rollback Generation Preparation (T07)
TARGET_DIR="$(dirname -- "${TARGET_FILE}")"
mkdir -p "${TARGET_DIR}"
DATE_STR="$(date +%Y%m%d_%H%M%S)"
ROLLBACK_DIR="${TARGET_DIR}/.rollback_${DATE_STR}_$$"

if [[ -f "${TARGET_FILE}" ]] || [[ -f "${TARGET_ANCHOR}" ]]; then
  echo "[*] Preserving complete live state into rollback generation: ${ROLLBACK_DIR}"
  mkdir -m 700 -p "${ROLLBACK_DIR}"
  if [[ -f "${TARGET_FILE}" ]]; then
    cp -p -- "${TARGET_FILE}" "${ROLLBACK_DIR}/"
  fi
  if [[ -f "${TARGET_ANCHOR}" ]]; then
    cp -p -- "${TARGET_ANCHOR}" "${ROLLBACK_DIR}/"
  fi
  # Preserve any live generation files
  find "${TARGET_DIR}" -maxdepth 1 -name "billing_state.json.gen_*" -exec cp -p {} "${ROLLBACK_DIR}/" \; 2>/dev/null || true

  # Record rollback manifest
  cat > "${ROLLBACK_DIR}/rollback.manifest.json" <<JSON
{
  "rollback_created_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "target_file": "${TARGET_FILE}",
  "reason": "pre-restore backup before restoring ${BACKUP_FILE}"
}
JSON
fi

# Step 5: Atomic replacement with safe rollback on failure
TEMP_FILE="${TARGET_FILE}.restore.tmp.$$"
TEMP_ANCHOR="${TARGET_ANCHOR}.restore.tmp.$$"
TEMP_GENERATION="${TARGET_DIR}/.${GENERATION_FILE}.restore.tmp.$$"

emergency_rollback() {
  echo "[!] Error during atomic replacement! Initiating emergency rollback from ${ROLLBACK_DIR}..." >&2
  if [[ -d "${ROLLBACK_DIR}" ]]; then
    if [[ -f "${ROLLBACK_DIR}/billing_state.json" ]]; then
      cp -p -- "${ROLLBACK_DIR}/billing_state.json" "${TARGET_FILE}"
    fi
    if [[ -f "${ROLLBACK_DIR}/billing_state.json.anchor" ]]; then
      cp -p -- "${ROLLBACK_DIR}/billing_state.json.anchor" "${TARGET_ANCHOR}"
    fi
    find "${ROLLBACK_DIR}" -maxdepth 1 -name "billing_state.json.gen_*" -exec cp -p {} "${TARGET_DIR}/" \; 2>/dev/null || true
    echo "[√] Live state successfully restored from rollback generation." >&2
  fi
  rm -f -- "${TEMP_FILE}" "${TEMP_ANCHOR}" "${TEMP_GENERATION}"
}
trap emergency_rollback ERR

cp -- "${BACKUP_FILE}" "${TEMP_FILE}"
cp -- "${ANCHOR_FILE}" "${TEMP_ANCHOR}"
if [[ -n "${GENERATION_SOURCE}" ]]; then
  cp -- "${GENERATION_SOURCE}" "${TEMP_GENERATION}"
fi
chmod 600 "${TEMP_FILE}" "${TEMP_ANCHOR}" ${GENERATION_SOURCE:+"${TEMP_GENERATION}"}

mv -f -- "${TEMP_FILE}" "${TARGET_FILE}"
mv -f -- "${TEMP_ANCHOR}" "${TARGET_ANCHOR}"
if [[ -n "${GENERATION_SOURCE}" ]]; then
  mv -f -- "${TEMP_GENERATION}" "${TARGET_DIR}/${GENERATION_FILE}"
fi

# Directory sync if supported
if command -v sync >/dev/null 2>&1; then
  sync "${TARGET_DIR}" 2>/dev/null || sync 2>/dev/null || true
fi

# Clear error trap now that commit succeeded
trap - ERR
rm -f -- "${TEMP_FILE}" "${TEMP_ANCHOR}"

echo "[$(date -Iseconds)] Restore completed successfully: ${TARGET_FILE}"
if [[ -d "${ROLLBACK_DIR}" ]]; then
  echo "[$(date -Iseconds)] Complete rollback generation preserved at: ${ROLLBACK_DIR}"
fi
