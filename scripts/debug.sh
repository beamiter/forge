#!/usr/bin/env bash
# Debug helper script for forge

set -Eeuo pipefail

if [[ "${XDG_CONFIG_HOME:-}" == /* ]]; then
    forge_config_home="${XDG_CONFIG_HOME}"
else
    forge_config_home="${HOME}/.config"
fi
readonly FORGE_CONFIG_HOME="${forge_config_home}"
unset forge_config_home

if [[ -n "${FORGE_CONFIG:-}" ]]; then
    config_file="${FORGE_CONFIG}"
    config_source='custom FORGE_CONFIG override'
else
    config_file="${FORGE_CONFIG_HOME}/forge/config.toml"
    config_source='default XDG config location'
fi
readonly CONFIG_FILE="${config_file}"
config_dir="$(dirname -- "${CONFIG_FILE}")"
readonly CONFIG_DIR="${config_dir}"
readonly CONFIG_SOURCE="${config_source}"
readonly STATE_FILE="${FORGE_CONFIG_HOME}/forge/tabs.state"
unset config_dir config_file config_source

state_exists() {
    [[ -f "${STATE_FILE}" ]]
}

show_file_metadata() {
    local path="$1"
    if [[ -f "${path}" ]]; then
        printf '   Size: %s bytes\n' "$(wc -c < "${path}")"
        printf '   Lines: %s\n' "$(wc -l < "${path}")"
        stat -c '   Mode: %a  Modified: %y' "${path}"
    else
        printf '   (missing)\n'
    fi
}

run_strace() {
    if [[ "${FORGE_DEBUG_ALLOW_SENSITIVE_TRACE:-0}" != "1" ]]; then
        printf '%s\n' \
            'Refusing to capture a system-call trace by default.' \
            'A trace can contain commands, paths, environment data, and file contents.' \
            'Set FORGE_DEBUG_ALLOW_SENSITIVE_TRACE=1 only when you trust the trace destination.' >&2
        exit 2
    fi

    local trace_file status
    umask 077
    trace_file="$(mktemp "${TMPDIR:-/tmp}/forge-strace.XXXXXX.log")"
    printf '%s\n' 'Running forge with strace; the output may contain sensitive data.' >&2
    printf 'Trace destination (owner-only): %s\n' "${trace_file}" >&2
    if strace -o "${trace_file}" target/release/forge; then
        printf 'Trace saved with owner-only permissions: %s\n' "${trace_file}"
    else
        status=$?
        rm -f -- "${trace_file}"
        printf '%s\n' 'Incomplete trace removed.' >&2
        return "${status}"
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
        printf '%s\n' 'Locations (absolute paths withheld):'
        if [[ -f "${CONFIG_FILE}" ]]; then
            printf '   Config: %s (exists)\n' "${CONFIG_SOURCE}"
        else
            printf '   Config: %s (missing)\n' "${CONFIG_SOURCE}"
        fi
        if state_exists; then
            printf '%s\n' '   Legacy state: XDG config location (exists)'
        else
            printf '%s\n' '   Legacy state: XDG config location (missing)'
        fi
        if command -v forge >/dev/null 2>&1; then
            printf '%s\n\n' '   Binary in PATH: yes'
        else
            printf '%s\n\n' '   Binary in PATH: no'
        fi

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
        run_strace
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
        printf '%s\n' '  strace       - Capture an owner-only trace with explicit sensitive-data opt-in'
        exit 1
        ;;
esac
