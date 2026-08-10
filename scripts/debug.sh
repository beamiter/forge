#!/usr/bin/env bash
# Debug helper script for forge

set -Eeuo pipefail

readonly CONFIG_DIR="${HOME}/.config/forge"
readonly CONFIG_FILE="${CONFIG_DIR}/config.toml"
readonly STATE_FILE="${CONFIG_DIR}/tabs.state"

state_exists() {
    [[ -f "${STATE_FILE}" ]]
}

show_file_metadata() {
    local path="$1"
    if [[ -f "${path}" ]]; then
        printf '   Path: %s\n' "${path}"
        printf '   Size: %s bytes\n' "$(wc -c < "${path}")"
        printf '   Lines: %s\n' "$(wc -l < "${path}")"
        stat -c '   Mode: %a  Owner: %U:%G  Modified: %y' "${path}"
    else
        printf '   (missing)\n'
    fi
}

show_state_summary() {
    if ! state_exists; then
        printf '   (No legacy tabs.state file)\n'
        return
    fi

    show_file_metadata "${STATE_FILE}"
    printf '   Current page entries: %s\n' "$(grep -c '^current_page=' "${STATE_FILE}")"
    printf '   Tab entries: %s\n' "$(grep -c '^tab=' "${STATE_FILE}")"
}

show_config_summary() {
    if [[ -f "${CONFIG_FILE}" ]]; then
        show_file_metadata "${CONFIG_FILE}"
    else
        printf '   (No config file - using defaults)\n'
    fi
}

show_process_summary() {
    local pids
    pids="$(pgrep -x forge || true)"
    if [[ -z "${pids}" ]]; then
        printf '   (No forge processes)\n'
        return
    fi
    printf '   Running instances: %s\n' "$(wc -l <<< "${pids}")"
}

print_raw_state() {
    if [[ "${FORGE_DEBUG_ALLOW_STATE_CONTENT:-0}" != "1" ]]; then
        printf '%s\n' \
            'Refusing to print session contents.' \
            'Set FORGE_DEBUG_ALLOW_STATE_CONTENT=1 and rerun this command only if you fully trust the destination.' >&2
        exit 2
    fi
    if ! state_exists; then
        printf '(No legacy tabs.state file)\n'
        return
    fi
    printf '%s\n' 'Warning: printing raw session state may expose commands, directories, and layout details.' >&2
    cat "${STATE_FILE}"
}

CMD="${1:-info}"

case "${CMD}" in
    info)
        printf '%s\n\n' 'forge debug information'
        printf '%s\n' 'Paths:'
        printf '   Config dir: %s\n' "${CONFIG_DIR}"
        printf '   Config: %s\n' "${CONFIG_FILE}"
        printf '   Legacy state: %s\n' "${STATE_FILE}"
        printf '   Binary: %s\n\n' "$(command -v forge 2>/dev/null || echo 'Not in PATH')"

        printf '%s\n' 'Legacy state summary:'
        show_state_summary
        printf '\n%s\n' 'Config summary:'
        show_config_summary
        printf '\n%s\n' 'Running forge processes:'
        show_process_summary
        ;;

    logs)
        printf '%s\n' 'Running forge with debug logs...'
        FORGE_LOG=debug target/release/forge
        ;;

    trace)
        printf '%s\n' 'Running forge with trace logs...'
        FORGE_LOG=trace target/release/forge
        ;;

    state)
        printf '%s\n' 'Legacy state summary:'
        show_state_summary
        ;;

    state-raw)
        print_raw_state
        ;;

    clean-state)
        printf '%s\n' 'Cleaning legacy tabs.state file...'
        if state_exists; then
            rm "${STATE_FILE}"
            printf '%s\n' 'Legacy state file removed.'
        else
            printf '%s\n' 'No legacy tabs.state file to remove.'
        fi
        ;;

    reset-config)
        printf '%s\n' 'Resetting config to defaults...'
        if [[ -f config.toml.example ]]; then
            mkdir -p "${CONFIG_DIR}"
            cp config.toml.example "${CONFIG_FILE}"
            printf '%s\n' 'Config reset to defaults.'
        else
            printf '%s\n' 'config.toml.example not found.' >&2
            exit 1
        fi
        ;;

    valgrind)
        printf '%s\n' 'Running with valgrind...'
        valgrind --leak-check=full --show-leak-kinds=all target/release/forge
        ;;

    strace)
        printf '%s\n' 'Running with strace...'
        strace -o /tmp/forge-strace.log target/release/forge
        printf '%s\n' 'Trace saved to /tmp/forge-strace.log'
        ;;

    *)
        printf '%s\n\n' "Usage: $0 {info|logs|trace|state|state-raw|clean-state|reset-config|valgrind|strace}"
        printf '%s\n' 'Commands:'
        printf '%s\n' '  info         - Show privacy-preserving debug information'
        printf '%s\n' '  logs         - Run with debug logs'
        printf '%s\n' '  trace        - Run with trace logs'
        printf '%s\n' '  state        - Show legacy state metadata only'
        printf '%s\n' '  state-raw    - Print raw legacy state only with FORGE_DEBUG_ALLOW_STATE_CONTENT=1'
        printf '%s\n' '  clean-state  - Remove the legacy tabs.state file'
        printf '%s\n' '  reset-config - Reset config to defaults'
        printf '%s\n' '  valgrind     - Run with valgrind'
        printf '%s\n' '  strace       - Run with strace'
        exit 1
        ;;
esac
