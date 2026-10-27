#!/usr/bin/env bash
# Diagnostic-only regression checks; never inspect the caller's session state.
set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
WORK_DIR="$(mktemp -d)"
trap 'rm -rf -- "${WORK_DIR}"' EXIT

env -u HOME bash "${SCRIPT_DIR}/show-state.sh" --help >"${WORK_DIR}/help"
grep -q '^Usage:' "${WORK_DIR}/help"

check_invalid_home() {
    local status=0
    "$@" bash "${SCRIPT_DIR}/show-state.sh" >"${WORK_DIR}/out" 2>"${WORK_DIR}/err" || status=$?
    [[ "${status}" == 1 ]]
    [[ ! -s "${WORK_DIR}/out" ]]
    grep -q 'an absolute HOME is required' "${WORK_DIR}/err"
}

check_invalid_home env -u HOME
check_invalid_home env HOME=
check_invalid_home env HOME=.
check_invalid_home env HOME=relative-home

mkdir -p -- "${WORK_DIR}/home/.config/forge"
printf 'current_page=0\ntab=private-test-command\n' >"${WORK_DIR}/home/.config/forge/tabs.state"
HOME="${WORK_DIR}/home" bash "${SCRIPT_DIR}/show-state.sh" >"${WORK_DIR}/metadata"
grep -q '^Current page entries: 1$' "${WORK_DIR}/metadata"
grep -q '^Tab entries: 1$' "${WORK_DIR}/metadata"
if grep -q 'private-test-command' "${WORK_DIR}/metadata"; then
    printf 'metadata unexpectedly contains private command content\n' >&2
    exit 1
fi

status=0
HOME="${WORK_DIR}/home" FORGE_DEBUG_ALLOW_STATE_CONTENT=0 \
    bash "${SCRIPT_DIR}/show-state.sh" --raw >"${WORK_DIR}/raw" 2>"${WORK_DIR}/err" || status=$?
[[ "${status}" == 2 ]]
[[ ! -s "${WORK_DIR}/raw" ]]
grep -q 'Refusing to print raw session contents' "${WORK_DIR}/err"

printf '%s\n' 'show-state diagnostics regressions passed'
