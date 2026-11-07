#!/usr/bin/env bash
# Reproducible dependency and shell-script security checks.

set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
cd "${PROJECT_ROOT}"

usage() {
    cat <<'EOF'
Usage: scripts/security-check.sh [--all | --policy | --audit | --shell]

  --all     Run every check (default).
  --policy  Enforce dependency sources, licenses, bans, and lock consistency.
  --audit   Audit both committed lockfiles with RustSec.
  --shell   Parse and lint every shell script in scripts/ and packaging/.
EOF
}

if (($# > 1)); then
    printf 'Error: expected at most one mode.\n' >&2
    usage >&2
    exit 2
fi

mode="${1:---all}"
case "${mode}" in
    --all | --policy | --audit | --shell) ;;
    -h | --help)
        usage
        exit 0
        ;;
    *)
        printf 'Error: unknown mode: %s\n' "${mode}" >&2
        usage >&2
        exit 2
        ;;
esac

require_command() {
    local command_name="$1"
    local install_hint="$2"

    if ! command -v "${command_name}" >/dev/null 2>&1; then
        printf 'Error: %s is required (%s).\n' \
            "${command_name}" "${install_hint}" >&2
        exit 1
    fi
}

check_locked_graphs() {
    require_command cargo 'install Rust with rustup'
    cargo metadata --locked --format-version 1 --no-deps >/dev/null
    cargo metadata --locked --manifest-path prototypes/ascii-organism/Cargo.toml \
        --format-version 1 --no-deps >/dev/null
}

check_dependency_policy() {
    require_command cargo-deny "cargo install cargo-deny --version 0.20.2 --locked"
    cargo deny --locked check
    cargo tree --locked --duplicates
    cargo tree --locked --manifest-path prototypes/ascii-organism/Cargo.toml \
        --duplicates
}

audit_lockfile() {
    local lockfile="$1"
    local audit_output
    local audit_status=0

    # cargo-audit 0.22.2 reports registry lookup failures without failing its
    # exit status or JSON report. JSON also hides index update/open warnings,
    # so keep terminal diagnostics visible and uncolored for this guard.
    # .cargo/audit.toml must retain quiet = false; the regression checks it.
    audit_output="$(cargo audit --format terminal --color never \
        --deny warnings --file "${lockfile}" 2>&1)" || audit_status=$?
    printf '%s\n' "${audit_output}"

    # Preserve all upstream failures, including vulnerabilities and denied
    # informational/yanked warnings, before checking its fail-open cases.
    if ((audit_status != 0)); then
        return "${audit_status}"
    fi

    local diagnostic_status=0
    grep -Eq "^[[:space:]]*(error: couldn't check if the package is yanked:|warning: couldn't (update|open) crates[.]io index:)" \
        <<<"${audit_output}" || diagnostic_status=$?
    case "${diagnostic_status}" in
        0)
            printf 'Error: incomplete yanked-crate check for %s.\n' "${lockfile}" >&2
            return 1
            ;;
        1) return 0 ;;
        *)
            printf 'Error: could not validate audit diagnostics for %s (grep exited %s).\n' \
                "${lockfile}" "${diagnostic_status}" >&2
            return "${diagnostic_status}"
            ;;
    esac
}

check_rustsec() {
    require_command cargo-audit "cargo install cargo-audit --version 0.22.2 --locked"
    audit_lockfile Cargo.lock
    # --no-fetch also disables sparse-index downloads. The prototype has
    # dependencies absent from the root graph, so it needs its own fetch.
    audit_lockfile prototypes/ascii-organism/Cargo.lock
}

check_shell_scripts() {
    require_command shellcheck 'install the distro shellcheck package'

    local -a shell_files
    mapfile -t shell_files < <(find scripts packaging -type f -name '*.sh' -print | sort)
    if ((${#shell_files[@]} == 0)); then
        printf 'Error: no shell scripts found below scripts/ or packaging/.\n' >&2
        exit 1
    fi

    bash -n "${shell_files[@]}"
    shellcheck "${shell_files[@]}"
}

case "${mode}" in
    --all)
        check_locked_graphs
        check_dependency_policy
        check_rustsec
        check_shell_scripts
        ;;
    --policy)
        check_locked_graphs
        check_dependency_policy
        ;;
    --audit)
        check_locked_graphs
        check_rustsec
        ;;
    --shell)
        check_shell_scripts
        ;;
esac
