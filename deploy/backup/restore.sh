#!/usr/bin/env bash
# Kiro BYOK Gateway - verified atomic JSON snapshot restore (Spec §7, T07)
# Enforces running-gateway prevention, engine validation in temporary sandbox,
# and complete-generation rollback before modifying live data.
set -euo pipefail

if [[ "$#" -ne 1 || "$1" == --* ]]; then
  echo "Usage: $0 <backup_file.json|backup_manifest.json> (gateway must be stopped)" >&2
  exit 1
fi
RAW_INPUT="$1"
command -v python3 >/dev/null 2>&1 || { echo "Error: python3 is required" >&2; exit 1; }

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
if command -v curl >/dev/null 2>&1 && curl -s -m 2 "${HEALTH_URL}" >/dev/null 2>&1; then
  echo "Error: stop gateway before restore; live-state overwrite is forbidden." >&2
  exit 1
fi
# Both shipped Compose configurations name the container kiro-gateway.
# Inspect its runtime identity directly, avoiding wrong-project empty lists and
# Compose env-file availability. Missing/uninspectable containers fail closed.
# Keep the gateway stopped for the entire restore; do not race deployment/startup.
GATEWAY_CONTAINER="${GATEWAY_CONTAINER:-kiro-gateway}"
command -v docker >/dev/null 2>&1 || { echo "Error: docker is required to confirm gateway is stopped" >&2; exit 1; }
if ! state="$(docker inspect --type container --format '{{.State.Status}}' -- "${GATEWAY_CONTAINER}")"; then
  echo "Error: cannot determine gateway container state; restore refused" >&2
  exit 1
fi
case "${state}" in
  exited|created) ;;
  *) echo "Error: gateway must be stopped (container state: ${state})" >&2; exit 1 ;;
esac

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
# Matches the non-root UID/GID in deploy/Dockerfile. Override for custom deployments.
GATEWAY_UID="${GATEWAY_UID:-1000}"
GATEWAY_GID="${GATEWAY_GID:-1000}"
[[ "${GATEWAY_UID}" =~ ^[0-9]+$ && "${GATEWAY_GID}" =~ ^[0-9]+$ ]] || { echo "Invalid gateway UID/GID" >&2; exit 1; }
if [[ ! -d "${TARGET_DIR}" ]]; then
  mkdir -m 700 -p "${TARGET_DIR}"
  chown "${GATEWAY_UID}:${GATEWAY_GID}" "${TARGET_DIR}"
fi
if [[ "$(id -u)" == 0 ]]; then
  command -v setpriv >/dev/null 2>&1 || { echo "setpriv is required to verify gateway file access" >&2; exit 1; }
  setpriv --reuid="${GATEWAY_UID}" --regid="${GATEWAY_GID}" --clear-groups test -w "${TARGET_DIR}"
else
  [[ "$(id -u)" == "${GATEWAY_UID}" && "$(id -g)" == "${GATEWAY_GID}" ]] || { echo "Run restore as root or the configured gateway identity" >&2; exit 1; }
fi
DATE_STR="$(date +%Y%m%d_%H%M%S)"
ROLLBACK_DIR="${TARGET_DIR}/.rollback_${DATE_STR}_$$"

TARGET_BASENAME="$(basename -- "${TARGET_FILE}")"
ANCHOR_BASENAME="$(basename -- "${TARGET_ANCHOR}")"
OLD_GENERATION=""
if [[ -f "${TARGET_ANCHOR}" ]]; then
  OLD_GENERATION="$(python3 - "${TARGET_ANCHOR}" <<'PY'
import json, re, sys
name = json.load(open(sys.argv[1], encoding="utf-8")).get("generation_file") or ""
if not isinstance(name, str) or (name and (not re.fullmatch(r"[A-Za-z0-9._-]+", name) or name.startswith("."))):
    sys.exit("Invalid live generation filename")
print(name)
PY
)"
fi
# Preserve both the live authority and any existing incoming generation. The
# latter may share a sequence with a different history after a previous restore.
ROLLBACK_NAMES=()
ROLLBACK_PRESENT=()
for name in "${OLD_GENERATION}" "${GENERATION_FILE}" "${TARGET_BASENAME}" "${ANCHOR_BASENAME}"; do
  [[ -n "${name}" ]] || continue
  duplicate=false
  for saved in "${ROLLBACK_NAMES[@]}"; do
    [[ "${saved}" != "${name}" ]] || duplicate=true
  done
  [[ "${duplicate}" == false ]] || continue
  ROLLBACK_NAMES+=("${name}")
done
mkdir -m 700 -- "${ROLLBACK_DIR}" "${ROLLBACK_DIR}/files"
for name in "${ROLLBACK_NAMES[@]}"; do
  source="${TARGET_DIR}/${name}"
  if [[ -L "${source}" || ( -e "${source}" && ! -f "${source}" ) ]]; then
    echo "Error: rollback source must be a regular file: ${source}" >&2; exit 1
  fi
  if [[ -f "${source}" ]]; then
    cp -p -- "${source}" "${ROLLBACK_DIR}/files/${name}"
    ROLLBACK_PRESENT+=(true)
  else
    if [[ "${name}" == "${OLD_GENERATION}" ]]; then
      echo "Error: live authoritative generation is missing: ${source}" >&2; exit 1
    fi
    ROLLBACK_PRESENT+=(false)
  fi
done
# Record absence too: a failed first restore must not leave a partial new state.
python3 - "${ROLLBACK_DIR}/rollback.manifest.json" "${TARGET_FILE}" "${ROLLBACK_NAMES[@]}" <<'PY'
import json, pathlib, sys
manifest = pathlib.Path(sys.argv[1])
manifest.write_text(json.dumps({"target_file": sys.argv[2], "files": {
    name: (manifest.parent / "files" / name).is_file() for name in sys.argv[3:]
}}, indent=2), encoding="utf-8")
PY

# Step 5: Atomic replacement with safe rollback on failure
TEMP_FILE="${TARGET_FILE}.restore.tmp.$$"
TEMP_ANCHOR="${TARGET_ANCHOR}.restore.tmp.$$"
TEMP_GENERATION="${TARGET_DIR}/.${GENERATION_FILE}.restore.tmp.$$"

publication_started=false
emergency_rollback() {
  trap - ERR
  local failed=false authority_ready=true name index rollback_temp
  echo "[!] Restore failed; rollback record: ${ROLLBACK_DIR}" >&2
  if [[ "${publication_started}" == true ]]; then
    # Dependencies first, mirror next, anchor last. Check every operation and
    # continue after failures so one bad file does not suppress other recovery.
    for index in "${!ROLLBACK_NAMES[@]}"; do
      name="${ROLLBACK_NAMES[index]}"
      if [[ "${name}" == "${ANCHOR_BASENAME}" && "${authority_ready}" == false ]]; then
        echo "Error: cannot republish old anchor after authoritative generation rollback failure" >&2
        continue
      fi
      if [[ "${ROLLBACK_PRESENT[index]}" == true ]]; then
        rollback_temp="${TARGET_DIR}/.${name}.rollback.tmp.$$"
        if cp -p -- "${ROLLBACK_DIR}/files/${name}" "${rollback_temp}" &&
            mv -f -- "${rollback_temp}" "${TARGET_DIR}/${name}"; then
          :
        else
          failed=true
          if [[ "${name}" == "${OLD_GENERATION}" || ( -z "${OLD_GENERATION}" && "${name}" == "${TARGET_BASENAME}" ) ]]; then
            authority_ready=false
          fi
          echo "Error: rollback failed for ${name}" >&2
        fi
        rm -f -- "${rollback_temp}" || failed=true
      elif [[ "${name}" == "${TARGET_BASENAME}" || "${name}" == "${ANCHOR_BASENAME}" ]]; then
        rm -f -- "${TARGET_DIR}/${name}" || failed=true
      fi
    done
    # Remove a newly introduced generation only after the incoming anchor is
    # removed/reverted. If rollback failed, preserve it for manual recovery.
    if [[ "${failed}" == false && -n "${GENERATION_FILE}" ]]; then
      for index in "${!ROLLBACK_NAMES[@]}"; do
        if [[ "${ROLLBACK_NAMES[index]}" == "${GENERATION_FILE}" && "${ROLLBACK_PRESENT[index]}" == false ]]; then
          rm -f -- "${TARGET_DIR}/${GENERATION_FILE}" || failed=true
        fi
      done
    fi
  fi
  rm -f -- "${TEMP_FILE}" "${TEMP_ANCHOR}" "${TEMP_GENERATION}" || failed=true
  if [[ "${failed}" == true ]]; then
    echo "Error: rollback incomplete; preserve data and rollback directory for manual recovery." >&2
  else
    echo "Live state successfully restored to its pre-restore state." >&2
  fi
  exit 1
}
trap emergency_rollback ERR

cp -- "${BACKUP_FILE}" "${TEMP_FILE}"
cp -- "${ANCHOR_FILE}" "${TEMP_ANCHOR}"
if [[ -n "${GENERATION_SOURCE}" ]]; then
  cp -- "${GENERATION_SOURCE}" "${TEMP_GENERATION}"
fi
chmod 600 "${TEMP_FILE}" "${TEMP_ANCHOR}" ${GENERATION_SOURCE:+"${TEMP_GENERATION}"}
chown "${GATEWAY_UID}:${GATEWAY_GID}" "${TEMP_FILE}" "${TEMP_ANCHOR}" ${GENERATION_SOURCE:+"${TEMP_GENERATION}"}
if [[ "$(id -u)" == 0 ]]; then
  for restored in "${TEMP_FILE}" "${TEMP_ANCHOR}" ${GENERATION_SOURCE:+"${TEMP_GENERATION}"}; do
    setpriv --reuid="${GATEWAY_UID}" --regid="${GATEWAY_GID}" --clear-groups test -r "${restored}"
  done
fi

# Publish the authoritative generation before its anchor, the commit point.
publication_started=true
if [[ -n "${GENERATION_SOURCE}" ]]; then
  mv -f -- "${TEMP_GENERATION}" "${TARGET_DIR}/${GENERATION_FILE}"
fi
mv -f -- "${TEMP_FILE}" "${TARGET_FILE}"
mv -f -- "${TEMP_ANCHOR}" "${TARGET_ANCHOR}"

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
