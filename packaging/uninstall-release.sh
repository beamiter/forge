#!/usr/bin/env bash
# Release-bundle entry point. Source and release installs both default to
# ~/.local/bin; make that prefix explicit before forwarding user overrides.

set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
HOME_DIR="${HOME:-}"

if [[ -z "${HOME_DIR}" ]]; then
    printf 'forge release uninstall: HOME is not set\n' >&2
    exit 1
fi

exec "${SCRIPT_DIR}/libexec/uninstall.sh" \
    --prefix "${HOME_DIR}/.local" "$@"
