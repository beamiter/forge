#!/usr/bin/env bash
# Summarize forge legacy session state without printing its contents.

set -Eeuo pipefail

readonly STATE_FILE="${HOME}/.config/forge/tabs.state"

show_usage() {
    printf '%s\n' "Usage: $0 [--raw]"
    printf '%s\n' '  default  Show metadata only.'
    printf '%s\n' '  --raw    Print raw contents only with FORGE_DEBUG_ALLOW_STATE_CONTENT=1.'
}

print_metadata() {
    printf '%s\n' 'forge legacy session state summary'
    printf '%s\n' '=================================='
    printf 'Path: %s\n' "${STATE_FILE}"
    printf 'Size: %s bytes\n' "$(wc -c < "${STATE_FILE}")"
    printf 'Lines: %s\n' "$(wc -l < "${STATE_FILE}")"
    stat -c 'Mode: %a  Owner: %U:%G  Modified: %y' "${STATE_FILE}"
    printf 'Current page entries: %s\n' "$(grep -c '^current_page=' "${STATE_FILE}")"
    printf 'Tab entries: %s\n' "$(grep -c '^tab=' "${STATE_FILE}")"
}

print_raw() {
    if [[ "${FORGE_DEBUG_ALLOW_STATE_CONTENT:-0}" != "1" ]]; then
        printf '%s\n' \
            'Refusing to print raw session contents.' \
            'Set FORGE_DEBUG_ALLOW_STATE_CONTENT=1 and rerun only if you fully trust the destination.' >&2
        exit 2
    fi
    printf '%s\n' 'Warning: raw session state may expose commands, directories, and layout details.' >&2
    cat "${STATE_FILE}"
}

case "${1:-}" in
    -h|--help)
        show_usage
        exit 0
        ;;
esac

if [[ ! -f "${STATE_FILE}" ]]; then
    printf 'No legacy state file found at %s\n' "${STATE_FILE}" >&2
    exit 1
fi

case "${1:-}" in
    "")
        print_metadata
        ;;
    --raw)
        print_raw
        ;;
    *)
        show_usage >&2
        exit 1
        ;;
esac
