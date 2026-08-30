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

check_rustsec() {
    require_command cargo-audit "cargo install cargo-audit --version 0.22.2 --locked"
    cargo audit --deny warnings --file Cargo.lock
    cargo audit --no-fetch --deny warnings \
        --file prototypes/ascii-organism/Cargo.lock
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
