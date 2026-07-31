#!/usr/bin/env bash
# Install jterm4 and its Linux desktop integration from a source checkout.

set -Eeuo pipefail
umask 077

APP_ID="io.github.beamiter.jterm4"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
HOME_DIR="${HOME:-}"
DESTDIR="${DESTDIR:-}"
PREFIX="${HOME_DIR}/.local"
BIN_DIR=""
PREFIX_EXPLICIT=0
BACKEND="auto"
INSTALL_CONFIG=1
INSTALL_DESKTOP=1
DRY_RUN=0

usage() {
    cat <<'USAGE'
Usage: ./scripts/install.sh [options]

Options:
  --prefix PATH          Runtime prefix (default: ~/.local)
  --bin-dir PATH         Runtime binary directory (default: ~/.cargo/bin;
                         with --prefix, defaults to PREFIX/bin)
  --backend auto|nix|cargo
                         Build backend (default: auto; prefers Nix)
  --no-config            Do not install config.toml.example
  --no-desktop           Do not install desktop, AppStream, or icon files
  --dry-run              Print commands without changing files
  -h, --help             Show this help

Environment:
  DESTDIR                Optional staging root for packaging
  XDG_CONFIG_HOME        Config base (default: ~/.config)
  CARGO_TARGET_DIR       Cargo target directory (default: <repo>/target)
USAGE
}

die() {
    printf 'jterm4 install: %s\n' "$*" >&2
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

run_optional() {
    print_command "$@"
    if ((DRY_RUN == 0)); then
        "$@" || printf 'jterm4 install: warning: %s failed (non-fatal)\n' "$1" >&2
    fi
}

# Like run_optional, but relaxes this script's restrictive umask: the desktop
# and icon caches are generated files that every user of a shared prefix has to
# be able to read, unlike the config we deliberately keep owner-only.
run_optional_public() {
    print_command "$@"
    if ((DRY_RUN == 0)); then
        (umask 022 && "$@") \
            || printf 'jterm4 install: warning: %s failed (non-fatal)\n' "$1" >&2
    fi
}

run_in_repo() {
    printf '  (cd %q && ' "${REPO_ROOT}"
    printf '%q ' "$@"
    printf ')\n'
    if ((DRY_RUN == 0)); then
        (cd -- "${REPO_ROOT}" && "$@")
    fi
}

bin_dir_on_path() {
    case ":${PATH}:" in
        *":${BIN_DIR}:"*) return 0 ;;
        *) return 1 ;;
    esac
}

# A desktop session fixes its PATH at login, so an entry that only says
# `Exec=jterm4` fails TryExec and is hidden from the launcher whenever the
# binary lives in a per-user bin dir that PATH does not list. Point the entry at
# the real path unless the target is a system bin dir that is always on PATH.
desktop_exec_path() {
    case "${BIN_DIR}" in
        /usr/bin | /usr/local/bin | /bin) printf 'jterm4' ;;
        *) printf '%s/jterm4' "${BIN_DIR}" ;;
    esac
}

install_desktop_entry() {
    local source="$1" dest="$2" exec_path
    exec_path="$(desktop_exec_path)"
    printf '  install -Dm0644 (Exec=%s) %q %q\n' "${exec_path}" "${source}" "${dest}"
    ((DRY_RUN == 0)) || return 0
    install -d -m 0755 "$(dirname -- "${dest}")"
    awk -v exec_path="${exec_path}" '
        /^Exec=jterm4([[:space:]]|$)/ || /^TryExec=jterm4([[:space:]]|$)/ {
            eq = index($0, "=")
            print substr($0, 1, eq) exec_path substr($0, eq + 7)
            next
        }
        { print }
    ' "${source}" >"${dest}.new"
    chmod 0644 "${dest}.new"
    mv -f -- "${dest}.new" "${dest}"
}

# Freshly installed entries and icons stay invisible until the shell's caches
# are rebuilt; a stale icon cache can even shadow icons that are already there.
refresh_desktop_caches() {
    if [[ -n "${DESTDIR}" ]]; then
        printf 'Staged install (DESTDIR set); skipping desktop cache refresh.\n'
        return 0
    fi
    if command -v desktop-file-validate >/dev/null 2>&1; then
        run_optional desktop-file-validate "${SHARE_DIR}/applications/${APP_ID}.desktop"
    fi
    if command -v update-desktop-database >/dev/null 2>&1; then
        run_optional_public update-desktop-database "${SHARE_DIR}/applications"
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        run_optional_public gtk-update-icon-cache --force --ignore-theme-index --quiet \
            "${SHARE_DIR}/icons/hicolor"
    fi
}

require_command() {
    if command -v "$1" >/dev/null 2>&1; then
        return
    fi
    ((DRY_RUN == 1)) || die "required command not found: $1"
}

while (($# > 0)); do
    case "$1" in
        --prefix)
            (($# >= 2)) || die "--prefix requires a path"
            PREFIX="$2"
            PREFIX_EXPLICIT=1
            shift 2
            ;;
        --prefix=*)
            PREFIX="${1#*=}"
            PREFIX_EXPLICIT=1
            shift
            ;;
        --bin-dir)
            (($# >= 2)) || die "--bin-dir requires a path"
            BIN_DIR="$2"
            shift 2
            ;;
        --bin-dir=*)
            BIN_DIR="${1#*=}"
            shift
            ;;
        --backend)
            (($# >= 2)) || die "--backend requires auto, nix, or cargo"
            BACKEND="$2"
            shift 2
            ;;
        --backend=*)
            BACKEND="${1#*=}"
            shift
            ;;
        --no-config)
            INSTALL_CONFIG=0
            shift
            ;;
        --no-desktop)
            INSTALL_DESKTOP=0
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
[[ -n "${PREFIX}" ]] || die "prefix must not be empty"
[[ "${PREFIX}" == /* ]] || die "--prefix must be an absolute path"
if [[ -z "${BIN_DIR}" ]]; then
    if ((PREFIX_EXPLICIT == 1)); then
        BIN_DIR="${PREFIX}/bin"
    else
        BIN_DIR="${HOME_DIR}/.cargo/bin"
    fi
fi
[[ "${BIN_DIR}" == /* ]] || die "--bin-dir must be an absolute path"
if [[ -n "${DESTDIR}" ]]; then
    [[ "${DESTDIR}" == /* ]] || die "DESTDIR must be an absolute path"
    DESTDIR="${DESTDIR%/}"
fi

case "${BACKEND}" in
    auto)
        if command -v nix >/dev/null 2>&1; then
            BACKEND="nix"
        else
            BACKEND="cargo"
        fi
        ;;
    nix|cargo) ;;
    *) die "invalid backend '${BACKEND}'; expected auto, nix, or cargo" ;;
esac

TARGET_DIR="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}"
if [[ "${TARGET_DIR}" != /* ]]; then
    TARGET_DIR="${REPO_ROOT}/${TARGET_DIR}"
fi
export CARGO_TARGET_DIR="${TARGET_DIR}"

printf 'Building jterm4 with %s...\n' "${BACKEND}"
case "${BACKEND}" in
    nix)
        require_command nix
        run_in_repo nix develop --command cargo build --release --locked
        ;;
    cargo)
        require_command cargo
        run_in_repo cargo build --release --locked
        ;;
esac

BINARY="${TARGET_DIR}/release/jterm4"
if ((DRY_RUN == 0)) && [[ ! -x "${BINARY}" ]]; then
    die "release binary was not produced at ${BINARY}"
fi

require_command install
STAGED_BIN_DIR="${DESTDIR}${BIN_DIR}"
run install -d -m 0755 "${STAGED_BIN_DIR}"
run install -m 0755 "${BINARY}" "${STAGED_BIN_DIR}/jterm4"
run install -m 0755 \
    "${REPO_ROOT}/scripts/support-bundle.sh" \
    "${STAGED_BIN_DIR}/jterm4-support-bundle"

SHARE_DIR="${DESTDIR}${PREFIX}/share"
ASSET_DIR="${SHARE_DIR}/jterm4"
run install -d -m 0755 \
    "${ASSET_DIR}/shell-integration" \
    "${ASSET_DIR}/workflows" \
    "${ASSET_DIR}/notebooks"
run install -m 0644 \
    "${REPO_ROOT}/scripts/shell-integration/README.md" \
    "${REPO_ROOT}/scripts/shell-integration/jterm4.bash" \
    "${REPO_ROOT}/scripts/shell-integration/jterm4.zsh" \
    "${REPO_ROOT}/scripts/shell-integration/jterm4.fish" \
    "${REPO_ROOT}/scripts/shell-integration/jterm4.ps1" \
    "${ASSET_DIR}/shell-integration/"
run install -m 0644 \
    "${REPO_ROOT}/scripts/workflows/git-feature.yaml" \
    "${REPO_ROOT}/scripts/workflows/find-large-files.yaml" \
    "${REPO_ROOT}/scripts/workflows/git-rebase-interactive.yaml" \
    "${REPO_ROOT}/scripts/workflows/ssh-tunnel.yaml" \
    "${REPO_ROOT}/scripts/workflows/docker-tail-logs.yaml" \
    "${REPO_ROOT}/scripts/workflows/kill-port.yaml" \
    "${ASSET_DIR}/workflows/"
run install -m 0644 \
    "${REPO_ROOT}/scripts/notebooks/welcome.jtnb.md" \
    "${ASSET_DIR}/notebooks/welcome.jtnb.md"

if ((INSTALL_DESKTOP == 1)); then
    install_desktop_entry "${REPO_ROOT}/data/${APP_ID}.desktop" \
        "${SHARE_DIR}/applications/${APP_ID}.desktop"
    run install -Dm0644 "${REPO_ROOT}/data/${APP_ID}.metainfo.xml" \
        "${SHARE_DIR}/metainfo/${APP_ID}.metainfo.xml"
    run install -Dm0644 "${REPO_ROOT}/data/${APP_ID}.svg" \
        "${SHARE_DIR}/icons/hicolor/scalable/apps/${APP_ID}.svg"
    run install -Dm0644 "${REPO_ROOT}/data/${APP_ID}-128.png" \
        "${SHARE_DIR}/icons/hicolor/128x128/apps/${APP_ID}.png"
    run install -Dm0644 "${REPO_ROOT}/data/${APP_ID}-256.png" \
        "${SHARE_DIR}/icons/hicolor/256x256/apps/${APP_ID}.png"
    refresh_desktop_caches
fi

CONFIG_HOME="${XDG_CONFIG_HOME:-${HOME_DIR}/.config}"
[[ "${CONFIG_HOME}" == /* ]] || die "XDG_CONFIG_HOME must be an absolute path"
CONFIG_DIR="${CONFIG_HOME}/jterm4"
STAGED_CONFIG_DIR="${DESTDIR}${CONFIG_DIR}"
if ((INSTALL_CONFIG == 1)); then
    run install -d -m 0700 "${STAGED_CONFIG_DIR}"
    if [[ ! -e "${STAGED_CONFIG_DIR}/config.toml" ]]; then
        run install -m 0600 "${REPO_ROOT}/config.toml.example" "${STAGED_CONFIG_DIR}/config.toml"
    else
        printf 'Keeping existing config: %s\n' "${CONFIG_DIR}/config.toml"
    fi
fi

printf 'Installed jterm4 to %s\n' "${BIN_DIR}/jterm4"
printf 'Installed support tool to %s\n' "${BIN_DIR}/jterm4-support-bundle"
printf 'Installed runtime assets under %s/share/jterm4\n' "${PREFIX}"
if ((INSTALL_DESKTOP == 1)); then
    printf 'Installed desktop integration under %s/share\n' "${PREFIX}"
    printf 'Launcher entry: %s (Exec=%s)\n' \
        "${SHARE_DIR}/applications/${APP_ID}.desktop" "$(desktop_exec_path)"
fi
if [[ -n "${DESTDIR}" ]]; then
    printf 'Staged file: %s\n' "${STAGED_BIN_DIR}/jterm4"
fi
if [[ -z "${DESTDIR}" ]]; then
    if ! bin_dir_on_path; then
        printf '\nNote: %s is not in PATH; the launcher entry uses the absolute path,\n' \
            "${BIN_DIR}"
        printf 'but shells will not find jterm4 until you add it, for example:\n'
        printf "  echo 'export PATH=\"%s:\$PATH\"' >>~/.profile\n" "${BIN_DIR}"
    fi
    SHADOWING_BIN="$(command -v jterm4 2>/dev/null || true)"
    if [[ -n "${SHADOWING_BIN}" && "${SHADOWING_BIN}" != "${BIN_DIR}/jterm4" ]]; then
        printf '\nNote: typing `jterm4` still runs %s, an older copy earlier in PATH.\n' \
            "${SHADOWING_BIN}"
        printf 'Remove it, or put %s ahead of it in PATH.\n' "${BIN_DIR}"
        printf 'The launcher entry is unaffected: it runs %s directly.\n' \
            "${BIN_DIR}/jterm4"
    fi
fi
printf 'Validate with: %s --doctor\n' "${BIN_DIR}/jterm4"
