#!/usr/bin/env bash
# Remove forge while preserving user configuration and state by default.

set -Eeuo pipefail

APP_ID="io.github.beamiter.forge"
HOME_DIR="${HOME:-}"
DESTDIR="${DESTDIR:-}"
DESTDIR_ACTIVE=0
if [[ -n "${DESTDIR}" ]]; then
    DESTDIR_ACTIVE=1
fi
PREFIX="${HOME_DIR}/.local"
BIN_DIR=""
PREFIX_EXPLICIT=0
PURGE_CONFIG=0
DRY_RUN=0

usage() {
    cat <<'USAGE'
Usage: uninstall.sh [options]

Options:
  --prefix PATH          Runtime prefix (default: ~/.local)
  --bin-dir PATH         Runtime binary directory (default: ~/.cargo/bin;
                         with --prefix, defaults to PREFIX/bin)
  --purge-config         Also remove forge config and default XDG state
  --dry-run              Print commands without changing files
  -h, --help             Show this help

Environment:
  DESTDIR                Optional staging root for packaging
  XDG_CONFIG_HOME        Config base (default: ~/.config)
  XDG_STATE_HOME         State base (default: ~/.local/state)
USAGE
}

die() {
    printf 'forge uninstall: %s\n' "$*" >&2
    exit 1
}

print_command() {
    printf '  '
    printf '%q ' "$@"
    printf '\n'
}

run() {
    print_command "$@"
    if ((DRY_RUN == 0)); then
        "$@"
    fi
}

remove_file() {
    local path="$1"
    validate_staging_removal_target "${path}"
    if [[ -e "${path}" || -L "${path}" ]]; then
        run rm -f -- "${path}"
    fi
}

remove_dir_if_empty() {
    local path="$1"
    validate_staging_removal_target "${path}"
    if [[ -d "${path}" ]]; then
        run rmdir --ignore-fail-on-non-empty -- "${path}"
    fi
}

validate_absolute_path() {
    local label="$1" path="$2"
    [[ -n "${path}" ]] || die "${label} must not be empty"
    [[ "${path}" == /* ]] || die "${label} must be an absolute path"
    if [[ "${path}" =~ [[:cntrl:]] ]]; then
        die "${label} must not contain control characters"
    fi
    case "/${path#/}/" in
        */../*) die "${label} must not contain '..' path components" ;;
    esac
}

normalize_absolute_path() {
    local path="$1" normalized="" component
    local -a components=()
    IFS='/' read -r -a components <<<"${path}"
    for component in "${components[@]}"; do
        [[ -n "${component}" && "${component}" != . ]] || continue
        normalized="${normalized}/${component}"
    done
    printf '%s' "${normalized:-/}"
}

# Full-chain, point-in-time validation for the caller-owned staging root.
validate_destdir_root() {
    local suffix current="" component
    local -a components=()
    ((DESTDIR_ACTIVE == 1)) || return 0
    [[ -n "${DESTDIR}" && "${DESTDIR}" != / ]] || return 0
    suffix="${DESTDIR#/}"
    IFS='/' read -r -a components <<<"${suffix}"
    for component in "${components[@]}"; do
        [[ -n "${component}" ]] || continue
        current="${current}/${component}"
        [[ ! -L "${current}" ]] \
            || die "DESTDIR path contains a symbolic-link component: ${current}"
        [[ -e "${current}" ]] || break
    done
}

# A staged uninstall must not follow a directory symlink out of a caller-owned
# non-root DESTDIR.  The final component is deliberately excluded: removing a
# destination symlink itself is safe, while any symlink in its parent chain
# would redirect the removal outside the package tree.
validate_staging_removal_target() {
    local target="$1" parent suffix current component
    local -a components=()
    ((DESTDIR_ACTIVE == 1)) || return 0
    [[ -n "${DESTDIR}" ]] || return 0
    validate_destdir_root
    case "${target}" in
        "${DESTDIR}"/*) ;;
        *) die "staged uninstall target is outside DESTDIR: ${target}" ;;
    esac
    parent="${target%/*}"
    suffix="${parent#"${DESTDIR}"}"
    suffix="${suffix#/}"
    current="${DESTDIR}"
    IFS='/' read -r -a components <<<"${suffix}"
    for component in "${components[@]}"; do
        [[ -n "${component}" && "${component}" != . ]] || continue
        current="${current}/${component}"
        [[ ! -L "${current}" ]] \
            || die "staged uninstall path contains a symbolic-link ancestor: ${current}"
        [[ -e "${current}" ]] || break
    done
}

while (($# > 0)); do
    case "$1" in
        --prefix)
            (($# >= 2)) || die "--prefix requires a path"
            PREFIX="$2"
            [[ -n "${PREFIX}" ]] || die "--prefix must not be empty"
            PREFIX_EXPLICIT=1
            shift 2
            ;;
        --prefix=*)
            PREFIX="${1#*=}"
            [[ -n "${PREFIX}" ]] || die "--prefix must not be empty"
            PREFIX_EXPLICIT=1
            shift
            ;;
        --bin-dir)
            (($# >= 2)) || die "--bin-dir requires a path"
            BIN_DIR="$2"
            [[ -n "${BIN_DIR}" ]] || die "--bin-dir must not be empty"
            shift 2
            ;;
        --bin-dir=*)
            BIN_DIR="${1#*=}"
            [[ -n "${BIN_DIR}" ]] || die "--bin-dir must not be empty"
            shift
            ;;
        --purge-config)
            PURGE_CONFIG=1
            shift
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        --)
            shift
            (($# == 0)) || die "unexpected positional arguments: $*"
            ;;
        *)
            die "unknown option: $1"
            ;;
    esac
done

[[ -n "${HOME_DIR}" ]] || die "HOME is not set"
validate_absolute_path "--prefix" "${PREFIX}"
if [[ -z "${BIN_DIR}" ]]; then
    if ((PREFIX_EXPLICIT == 1)); then
        BIN_DIR="${PREFIX}/bin"
    else
        BIN_DIR="${HOME_DIR}/.cargo/bin"
    fi
fi
validate_absolute_path "--bin-dir" "${BIN_DIR}"
if ((DESTDIR_ACTIVE == 1)); then
    validate_absolute_path "DESTDIR" "${DESTDIR}"
    DESTDIR="$(normalize_absolute_path "${DESTDIR}")"
    validate_destdir_root
    if [[ "${DESTDIR}" == / ]]; then
        DESTDIR=""
    fi
fi

if ((PURGE_CONFIG == 1)); then
    CONFIG_HOME="${XDG_CONFIG_HOME:-${HOME_DIR}/.config}"
    STATE_HOME="${XDG_STATE_HOME:-${HOME_DIR}/.local/state}"
    validate_absolute_path "XDG_CONFIG_HOME" "${CONFIG_HOME}"
    validate_absolute_path "XDG_STATE_HOME" "${STATE_HOME}"
    # Check both recursive deletion roots before removing ordinary installed
    # files, preserving the existing all-or-nothing purge preflight contract.
    validate_staging_removal_target "${DESTDIR}${CONFIG_HOME}/forge"
    validate_staging_removal_target "${DESTDIR}${STATE_HOME}/forge"
fi

remove_file "${DESTDIR}${BIN_DIR}/forge"
remove_file "${DESTDIR}${BIN_DIR}/forge-support-bundle"
SHARE_DIR="${DESTDIR}${PREFIX}/share"
remove_file "${SHARE_DIR}/applications/${APP_ID}.desktop"
remove_file "${SHARE_DIR}/metainfo/${APP_ID}.metainfo.xml"
remove_file "${SHARE_DIR}/icons/hicolor/scalable/apps/${APP_ID}.svg"
remove_file "${SHARE_DIR}/icons/hicolor/128x128/apps/${APP_ID}.png"
remove_file "${SHARE_DIR}/icons/hicolor/256x256/apps/${APP_ID}.png"
# Desktop integration from before the jterm4 -> forge rename.
remove_file "${SHARE_DIR}/applications/io.github.beamiter.jterm4.desktop"
remove_file "${SHARE_DIR}/metainfo/io.github.beamiter.jterm4.metainfo.xml"
remove_file "${SHARE_DIR}/icons/hicolor/scalable/apps/io.github.beamiter.jterm4.svg"
remove_file "${SHARE_DIR}/icons/hicolor/128x128/apps/io.github.beamiter.jterm4.png"
remove_file "${SHARE_DIR}/icons/hicolor/256x256/apps/io.github.beamiter.jterm4.png"
remove_file "${SHARE_DIR}/forge/shell-integration/README.md"
remove_file "${SHARE_DIR}/forge/shell-integration/forge.bash"
remove_file "${SHARE_DIR}/forge/shell-integration/forge.zsh"
remove_file "${SHARE_DIR}/forge/shell-integration/forge.fish"
remove_file "${SHARE_DIR}/forge/shell-integration/forge.ps1"
remove_file "${SHARE_DIR}/forge/workflows/git-feature.yaml"
remove_file "${SHARE_DIR}/forge/workflows/find-large-files.yaml"
remove_file "${SHARE_DIR}/forge/workflows/git-rebase-interactive.yaml"
remove_file "${SHARE_DIR}/forge/workflows/ssh-tunnel.yaml"
remove_file "${SHARE_DIR}/forge/workflows/docker-tail-logs.yaml"
remove_file "${SHARE_DIR}/forge/workflows/kill-port.yaml"
remove_file "${SHARE_DIR}/forge/notebooks/welcome.jtnb.md"
remove_file "${SHARE_DIR}/doc/forge/README.md"
remove_file "${SHARE_DIR}/doc/forge/config.toml.example"
remove_file "${SHARE_DIR}/doc/forge/Cargo.lock"
remove_file "${SHARE_DIR}/doc/forge/BUILDINFO"
remove_dir_if_empty "${SHARE_DIR}/forge/shell-integration"
remove_dir_if_empty "${SHARE_DIR}/forge/workflows"
remove_dir_if_empty "${SHARE_DIR}/forge/notebooks"
remove_dir_if_empty "${SHARE_DIR}/forge"
remove_dir_if_empty "${SHARE_DIR}/doc/forge"

# Without this the launcher keeps offering a dead entry and a cached icon.
if ((DESTDIR_ACTIVE == 0 && DRY_RUN == 0)); then
    if command -v update-desktop-database >/dev/null 2>&1 \
        && [[ -d "${SHARE_DIR}/applications" ]]; then
        (umask 022 && update-desktop-database "${SHARE_DIR}/applications") \
            >/dev/null 2>&1 || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1 \
        && [[ -d "${SHARE_DIR}/icons/hicolor" ]]; then
        (umask 022 && gtk-update-icon-cache --force --ignore-theme-index --quiet \
            "${SHARE_DIR}/icons/hicolor") >/dev/null 2>&1 || true
    fi
fi

if ((PURGE_CONFIG == 1)); then
    CONFIG_DIR="${DESTDIR}${CONFIG_HOME}/forge"
    validate_staging_removal_target "${CONFIG_DIR}"
    if [[ -e "${CONFIG_DIR}" || -L "${CONFIG_DIR}" ]]; then
        run rm -rf -- "${CONFIG_DIR}"
    else
        printf 'Config/state directory not present: %s\n' "${CONFIG_HOME}/forge"
    fi
    STATE_DIR="${DESTDIR}${STATE_HOME}/forge"
    validate_staging_removal_target "${STATE_DIR}"
    if [[ -e "${STATE_DIR}" || -L "${STATE_DIR}" ]]; then
        run rm -rf -- "${STATE_DIR}"
    else
        printf 'Default state directory not present: %s\n' "${STATE_HOME}/forge"
    fi
else
    printf 'Preserved config and state. Use --purge-config to remove them.\n'
fi
