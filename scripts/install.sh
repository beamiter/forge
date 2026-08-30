#!/usr/bin/env bash
# Install forge and its Linux desktop integration from a source checkout.

set -Eeuo pipefail
umask 077

APP_ID="io.github.beamiter.forge"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
HOME_DIR="${HOME:-}"
DESTDIR="${DESTDIR:-}"
DESTDIR_ACTIVE=0
if [[ -n "${DESTDIR}" ]]; then
    DESTDIR_ACTIVE=1
fi
PREFIX="${HOME_DIR}/.local"
BIN_DIR=""
PREFIX_EXPLICIT=0
BACKEND="auto"
BACKEND_EXPLICIT=0
PREBUILT_BINARY=""
PREBUILT_FD=""
INSTALL_CONFIG=1
INSTALL_DESKTOP=1
DRY_RUN=0
INSTALL_TEMP=""

usage() {
    cat <<'USAGE'
Usage: ./scripts/install.sh [options]

Options:
  --prefix PATH          Runtime prefix (default: ~/.local)
  --bin-dir PATH         Runtime binary directory (default: ~/.cargo/bin;
                         with --prefix, defaults to PREFIX/bin)
  --backend auto|nix|cargo
                         Build backend (default: auto; prefers Nix)
  --binary PATH          Install a prebuilt forge binary instead of building
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
    printf 'forge install: %s\n' "$*" >&2
    exit 1
}

cleanup_install_temp() {
    if [[ -n "${INSTALL_TEMP:-}" ]]; then
        rm -f -- "${INSTALL_TEMP}"
        INSTALL_TEMP=""
    fi
}

trap cleanup_install_temp EXIT

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
        "$@" || printf 'forge install: warning: %s failed (non-fatal)\n' "$1" >&2
    fi
}

# Like run_optional, but relaxes this script's restrictive umask: the desktop
# and icon caches are generated files that every user of a shared prefix has to
# be able to read, unlike the config we deliberately keep owner-only.
run_optional_public() {
    print_command "$@"
    if ((DRY_RUN == 0)); then
        (umask 022 && "$@") \
            || printf 'forge install: warning: %s failed (non-fatal)\n' "$1" >&2
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

# Collapse repeated separators and lexical `.` components without resolving
# symlinks. `..` and control bytes were already rejected above.
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

# Check the whole existing DESTDIR chain from `/`, not just its final spelling.
# This is a point-in-time preflight; callers must not treat it as race-proof.
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

require_source_file() {
    local source="$1"
    [[ ! -L "${source}" && -f "${source}" && -r "${source}" ]] \
        || die "required install source is not a readable regular file: ${source}"
}

# Only caller-controlled packaging roots receive this stricter policy. Normal
# host prefixes may legitimately contain compatibility symlinks.
validate_staging_target() {
    local target="$1" suffix current component
    local -a components=()
    ((DESTDIR_ACTIVE == 1)) || return 0
    [[ -n "${DESTDIR}" ]] || return 0
    validate_destdir_root
    suffix="${target#"${DESTDIR}"}"
    suffix="${suffix#/}"
    current="${DESTDIR}"
    IFS='/' read -r -a components <<<"${suffix}"
    for component in "${components[@]}"; do
        [[ -n "${component}" && "${component}" != . ]] || continue
        current="${current}/${component}"
        [[ ! -L "${current}" ]] \
            || die "staged install path contains a symbolic-link ancestor: ${current}"
        [[ -e "${current}" ]] || break
    done
}

install_file_atomic() {
    local mode="$1" source="$2" dest="$3" directory basename
    printf '  install -m %q %q %q && mv -fT -- %q %q\n' \
        "${mode}" "${source}" "${dest}.<temporary>" "${dest}.<temporary>" "${dest}"
    ((DRY_RUN == 0)) || return 0
    directory="${dest%/*}"
    basename="${dest##*/}"
    install -d -m 0755 "${directory}"
    INSTALL_TEMP="$(mktemp "${directory}/.${basename}.install.XXXXXX")" \
        || die "cannot create temporary file beside ${dest}"
    if ! install -m "${mode}" "${source}" "${INSTALL_TEMP}"; then
        cleanup_install_temp
        die "cannot stage ${dest}"
    fi
    if ! mv -fT -- "${INSTALL_TEMP}" "${dest}"; then
        cleanup_install_temp
        die "cannot atomically replace ${dest}"
    fi
    INSTALL_TEMP=""
}

# Publish a first-run config without a check-then-copy race. The temporary and
# destination are in the same directory, so link(2) is atomic and cannot cross
# filesystems; EEXIST means a concurrent writer won and must be preserved.
install_config_if_absent() {
    local source="$1" dest="$2" runtime_dest="$3" directory basename
    if [[ -e "${dest}" || -L "${dest}" ]]; then
        printf 'Keeping existing config: %s\n' "${runtime_dest}"
        return 0
    fi
    printf '  install -m 0600 %q %q && ln -- %q %q\n' \
        "${source}" "${dest}.<temporary>" "${dest}.<temporary>" "${dest}"
    ((DRY_RUN == 0)) || return 0
    directory="${dest%/*}"
    basename="${dest##*/}"
    INSTALL_TEMP="$(mktemp "${directory}/.${basename}.install.XXXXXX")" \
        || die "cannot create temporary config beside ${dest}"
    if ! install -m 0600 "${source}" "${INSTALL_TEMP}"; then
        cleanup_install_temp
        die "cannot stage initial config for ${dest}"
    fi
    if ln -- "${INSTALL_TEMP}" "${dest}" 2>/dev/null; then
        cleanup_install_temp
        return 0
    fi
    if [[ -e "${dest}" || -L "${dest}" ]]; then
        cleanup_install_temp
        printf 'Keeping concurrently created config: %s\n' "${runtime_dest}"
        return 0
    fi
    cleanup_install_temp
    die "cannot atomically create initial config at ${dest}"
}

install_binary_atomic() {
    local source="$1" dest="$2"
    printf '  install -m 0755 %q %q && mv -fT -- %q %q\n' \
        "${source}" "${dest}.<temporary>" "${dest}.<temporary>" "${dest}"
    ((DRY_RUN == 0)) || return 0
    INSTALL_TEMP="$(mktemp "${dest}.install.XXXXXX")" \
        || die "cannot create temporary binary beside ${dest}"
    if ! install -m 0755 "${source}" "${INSTALL_TEMP}"; then
        cleanup_install_temp
        die "cannot stage binary for ${dest}"
    fi
    if ! mv -fT -- "${INSTALL_TEMP}" "${dest}"; then
        cleanup_install_temp
        die "cannot atomically replace ${dest}"
    fi
    INSTALL_TEMP=""
}

pin_prebuilt_binary() {
    local requested="$1" fd_path fd_identity path_identity
    [[ -d /proc/self/fd && -r /proc/self/fd ]] \
        || die "cannot pin prebuilt binary: /proc/self/fd is unavailable"
    [[ ! -L "${requested}" ]] \
        || die "prebuilt binary must not be a symbolic link: ${requested}"
    [[ -f "${requested}" ]] \
        || die "prebuilt binary is not a regular file: ${requested}"
    [[ -r "${requested}" ]] \
        || die "prebuilt binary is not readable: ${requested}"
    exec {PREBUILT_FD}<"${requested}" \
        || die "cannot open prebuilt binary: ${requested}"
    fd_path="/proc/self/fd/${PREBUILT_FD}"
    [[ -e "${fd_path}" ]] \
        || die "cannot pin prebuilt binary: /proc/self/fd is unavailable"
    [[ -f "${fd_path}" ]] \
        || die "opened prebuilt binary is not a regular file: ${requested}"
    [[ -s "${fd_path}" ]] \
        || die "prebuilt binary must not be empty: ${requested}"
    fd_identity="$(stat -Lc '%d:%i' -- "${fd_path}")" \
        || die "cannot identify opened prebuilt binary (GNU stat required): ${requested}"
    [[ ! -L "${requested}" && -f "${requested}" ]] \
        || die "prebuilt binary changed while being opened: ${requested}"
    path_identity="$(stat -Lc '%d:%i' -- "${requested}")" \
        || die "cannot identify prebuilt binary (GNU stat required): ${requested}"
    [[ ! -L "${requested}" && "${path_identity}" == "${fd_identity}" ]] \
        || die "prebuilt binary changed while being opened: ${requested}"
    BINARY="${fd_path}"
}

bin_dir_on_path() {
    case ":${PATH:-}:" in
        *":${BIN_DIR}:"*) return 0 ;;
        *) return 1 ;;
    esac
}

# A desktop session fixes its PATH at login, so an entry that only says
# `Exec=forge` fails TryExec and is hidden from the launcher whenever the
# binary lives in a per-user bin dir that PATH does not list. Point the entry at
# the real path unless the target is a system bin dir that is always on PATH.
desktop_exec_path() {
    case "${BIN_DIR}" in
        /usr/bin | /usr/local/bin | /bin) printf 'forge' ;;
        *) printf '%s/forge' "${BIN_DIR}" ;;
    esac
}

desktop_exec_value() {
    local remaining="$1" escaped="" character
    if [[ "${remaining}" == forge ]]; then
        printf 'forge'
        return
    fi
    while [[ -n "${remaining}" ]]; do
        character="${remaining:0:1}"
        remaining="${remaining:1}"
        case "${character}" in
            \\) escaped="${escaped}\\\\\\\\" ;;
            '"') escaped+='\"' ;;
            '`') escaped+='\`' ;;
            '$') escaped+='\\$' ;;
            *) escaped+="${character}" ;;
        esac
    done
    printf '"%s"' "${escaped}"
}

desktop_try_exec_value() {
    local remaining="$1" escaped="" character
    while [[ -n "${remaining}" ]]; do
        character="${remaining:0:1}"
        remaining="${remaining:1}"
        case "${character}" in
            \\) escaped="${escaped}\\\\" ;;
            *) escaped+="${character}" ;;
        esac
    done
    printf '%s' "${escaped}"
}

validate_desktop_exec_path() {
    local path="$1"
    [[ "${path}" != *'='* ]] \
        || die "desktop executable path must not contain '=': ${path}"
    [[ "${path}" != *'%'* ]] \
        || die "desktop executable path must not contain '%': ${path}"
    if [[ "${path}" =~ [[:cntrl:]] ]]; then
        die "desktop executable path must not contain control characters"
    fi
}

install_desktop_entry() {
    local source="$1" dest="$2" exec_path exec_value try_exec_value desktop_dir
    exec_path="$(desktop_exec_path)"
    validate_desktop_exec_path "${exec_path}"
    exec_value="$(desktop_exec_value "${exec_path}")"
    try_exec_value="$(desktop_try_exec_value "${exec_path}")"
    printf '  install -Dm0644 (Exec=%s) %q %q\n' "${exec_path}" "${source}" "${dest}"
    ((DRY_RUN == 0)) || return 0
    desktop_dir="${dest%/*}"
    install -d -m 0755 "${desktop_dir}"
    INSTALL_TEMP="$(mktemp "${desktop_dir}/.${APP_ID}.desktop.install.XXXXXX")" \
        || die "cannot create temporary desktop entry beside ${dest}"
    if ! FORGE_DESKTOP_EXEC_VALUE="${exec_value}" \
        FORGE_DESKTOP_TRY_EXEC_VALUE="${try_exec_value}" \
        awk '
        BEGIN { exec_count = 0; try_exec_count = 0 }
        /^Exec=forge([[:space:]]|$)/ {
            exec_count++
            eq = index($0, "=")
            print substr($0, 1, eq) ENVIRON["FORGE_DESKTOP_EXEC_VALUE"] \
                substr($0, eq + 6)
            next
        }
        /^TryExec=forge([[:space:]]|$)/ {
            try_exec_count++
            eq = index($0, "=")
            print substr($0, 1, eq) ENVIRON["FORGE_DESKTOP_TRY_EXEC_VALUE"] \
                substr($0, eq + 6)
            next
        }
        /^Exec=/ { exit 45 }
        /^TryExec=/ { exit 46 }
        { print }
        END {
            if (exec_count < 1 || try_exec_count != 1) exit 44
        }
    ' "${source}" >"${INSTALL_TEMP}" \
        || ! chmod 0644 "${INSTALL_TEMP}" \
        || ! mv -fT -- "${INSTALL_TEMP}" "${dest}"; then
        cleanup_install_temp
        die "cannot atomically install desktop entry at ${dest}"
    fi
    INSTALL_TEMP=""
}

# Freshly installed entries and icons stay invisible until the shell's caches
# are rebuilt; a stale icon cache can even shadow icons that are already there.
refresh_desktop_caches() {
    if ((DESTDIR_ACTIVE == 1)); then
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
        --backend)
            (($# >= 2)) || die "--backend requires auto, nix, or cargo"
            BACKEND="$2"
            BACKEND_EXPLICIT=1
            shift 2
            ;;
        --backend=*)
            BACKEND="${1#*=}"
            BACKEND_EXPLICIT=1
            shift
            ;;
        --binary)
            (($# >= 2)) || die "--binary requires a path"
            PREBUILT_BINARY="$2"
            [[ -n "${PREBUILT_BINARY}" ]] || die "--binary must not be empty"
            shift 2
            ;;
        --binary=*)
            PREBUILT_BINARY="${1#*=}"
            [[ -n "${PREBUILT_BINARY}" ]] || die "--binary must not be empty"
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

CONFIG_HOME="${XDG_CONFIG_HOME:-${HOME_DIR}/.config}"
if ((INSTALL_CONFIG == 1)); then
    validate_absolute_path "XDG_CONFIG_HOME" "${CONFIG_HOME}"
fi

# Install every file format accepted by the shared workflow loader. Nullglob
# turns an empty source directory into an explicit preflight failure instead
# of copying a literal wildcard. Keeping this candidate rule beside the
# bundled-library Rust test means a new valid example cannot be parsed in a
# checkout yet silently omitted from a native install.
shopt -s nullglob
WORKFLOW_SOURCES=(
    "${REPO_ROOT}/scripts/workflows/"*.toml
    "${REPO_ROOT}/scripts/workflows/"*.yaml
    "${REPO_ROOT}/scripts/workflows/"*.yml
)
shopt -u nullglob
((${#WORKFLOW_SOURCES[@]} > 0)) \
    || die "no bundled workflow sources found under ${REPO_ROOT}/scripts/workflows"
for source in "${WORKFLOW_SOURCES[@]}"; do
    require_source_file "${source}"
done

STAGED_BIN_DIR="${DESTDIR}${BIN_DIR}"
SHARE_DIR="${DESTDIR}${PREFIX}/share"
ASSET_DIR="${SHARE_DIR}/forge"
CONFIG_DIR="${CONFIG_HOME}/forge"
STAGED_CONFIG_DIR="${DESTDIR}${CONFIG_DIR}"

validate_staging_target "${STAGED_BIN_DIR}"
validate_staging_target "${SHARE_DIR}"
if ((INSTALL_CONFIG == 1)); then
    validate_staging_target "${STAGED_CONFIG_DIR}"
fi

# All repository assets are checked before a build or destination mutation.
for source in \
    "${REPO_ROOT}/scripts/support-bundle.sh" \
    "${REPO_ROOT}/scripts/shell-integration/README.md" \
    "${REPO_ROOT}/scripts/shell-integration/forge.bash" \
    "${REPO_ROOT}/scripts/shell-integration/forge.zsh" \
    "${REPO_ROOT}/scripts/shell-integration/forge.fish" \
    "${REPO_ROOT}/scripts/shell-integration/forge.ps1" \
    "${REPO_ROOT}/scripts/notebooks/welcome.jtnb.md"; do
    require_source_file "${source}"
done
if ((INSTALL_DESKTOP == 1)); then
    require_source_file "${REPO_ROOT}/data/${APP_ID}.desktop"
    require_source_file "${REPO_ROOT}/data/${APP_ID}.metainfo.xml"
    require_source_file "${REPO_ROOT}/data/${APP_ID}.svg"
    require_source_file "${REPO_ROOT}/data/${APP_ID}-128.png"
    require_source_file "${REPO_ROOT}/data/${APP_ID}-256.png"
fi
if ((INSTALL_CONFIG == 1)); then
    require_source_file "${REPO_ROOT}/config.toml.example"
fi

require_command install
require_command mktemp
require_command mv
require_command rm
if ((INSTALL_CONFIG == 1)); then
    require_command ln
fi
if ((INSTALL_DESKTOP == 1)); then
    require_command awk
    require_command chmod
    validate_desktop_exec_path "$(desktop_exec_path)"
fi

if [[ -n "${PREBUILT_BINARY}" && ${BACKEND_EXPLICIT} -eq 1 ]]; then
    die "--backend cannot be combined with --binary"
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

if [[ -n "${PREBUILT_BINARY}" ]]; then
    BINARY="${PREBUILT_BINARY}"
    printf 'Using prebuilt forge binary: %s\n' "${BINARY}"
    if ((DRY_RUN == 0)); then
        require_command stat
        pin_prebuilt_binary "${PREBUILT_BINARY}"
    fi
else
    TARGET_DIR="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}"
    if [[ "${TARGET_DIR}" != /* ]]; then
        TARGET_DIR="${REPO_ROOT}/${TARGET_DIR}"
    fi
    export CARGO_TARGET_DIR="${TARGET_DIR}"

    printf 'Building forge with %s...\n' "${BACKEND}"
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

    BINARY="${TARGET_DIR}/release/forge"
    if ((DRY_RUN == 0)) && [[ ! -x "${BINARY}" ]]; then
        die "release binary was not produced at ${BINARY}"
    fi
fi

run install -d -m 0755 "${STAGED_BIN_DIR}"
install_binary_atomic "${BINARY}" "${STAGED_BIN_DIR}/forge"
if [[ -n "${PREBUILT_FD}" ]]; then
    exec {PREBUILT_FD}<&-
fi
install_file_atomic 0755 "${REPO_ROOT}/scripts/support-bundle.sh" \
    "${STAGED_BIN_DIR}/forge-support-bundle"

run install -d -m 0755 \
    "${ASSET_DIR}/shell-integration" \
    "${ASSET_DIR}/workflows" \
    "${ASSET_DIR}/notebooks"
for source in \
    "${REPO_ROOT}/scripts/shell-integration/README.md" \
    "${REPO_ROOT}/scripts/shell-integration/forge.bash" \
    "${REPO_ROOT}/scripts/shell-integration/forge.zsh" \
    "${REPO_ROOT}/scripts/shell-integration/forge.fish" \
    "${REPO_ROOT}/scripts/shell-integration/forge.ps1"; do
    install_file_atomic 0644 "${source}" \
        "${ASSET_DIR}/shell-integration/${source##*/}"
done
for source in "${WORKFLOW_SOURCES[@]}"; do
    install_file_atomic 0644 "${source}" "${ASSET_DIR}/workflows/${source##*/}"
done
install_file_atomic 0644 "${REPO_ROOT}/scripts/notebooks/welcome.jtnb.md" \
    "${ASSET_DIR}/notebooks/welcome.jtnb.md"

if ((INSTALL_DESKTOP == 1)); then
    install_desktop_entry "${REPO_ROOT}/data/${APP_ID}.desktop" \
        "${SHARE_DIR}/applications/${APP_ID}.desktop"
    # Launcher left by installs from before the jterm4 -> forge rename; left in
    # place it shows up as a second "jterm4" entry beside the new one.
    run rm -f -- "${SHARE_DIR}/applications/io.github.beamiter.jterm4.desktop"
    install_file_atomic 0644 "${REPO_ROOT}/data/${APP_ID}.metainfo.xml" \
        "${SHARE_DIR}/metainfo/${APP_ID}.metainfo.xml"
    install_file_atomic 0644 "${REPO_ROOT}/data/${APP_ID}.svg" \
        "${SHARE_DIR}/icons/hicolor/scalable/apps/${APP_ID}.svg"
    install_file_atomic 0644 "${REPO_ROOT}/data/${APP_ID}-128.png" \
        "${SHARE_DIR}/icons/hicolor/128x128/apps/${APP_ID}.png"
    install_file_atomic 0644 "${REPO_ROOT}/data/${APP_ID}-256.png" \
        "${SHARE_DIR}/icons/hicolor/256x256/apps/${APP_ID}.png"
    refresh_desktop_caches
fi

if ((INSTALL_CONFIG == 1)); then
    run install -d -m 0700 "${STAGED_CONFIG_DIR}"
    install_config_if_absent "${REPO_ROOT}/config.toml.example" \
        "${STAGED_CONFIG_DIR}/config.toml" "${CONFIG_DIR}/config.toml"
fi

printf 'Installed forge to %s\n' "${BIN_DIR}/forge"
printf 'Installed support tool to %s\n' "${BIN_DIR}/forge-support-bundle"
printf 'Installed runtime assets under %s/share/forge\n' "${PREFIX}"
if ((INSTALL_DESKTOP == 1)); then
    printf 'Installed desktop integration under %s/share\n' "${PREFIX}"
    printf 'Launcher entry: %s (Exec=%s)\n' \
        "${SHARE_DIR}/applications/${APP_ID}.desktop" "$(desktop_exec_path)"
fi
if ((DESTDIR_ACTIVE == 1)); then
    printf 'Staged file: %s\n' "${STAGED_BIN_DIR}/forge"
fi
if ((DESTDIR_ACTIVE == 0)); then
    if ! bin_dir_on_path; then
        printf '\nNote: %s is not in PATH; the launcher entry uses the absolute path,\n' \
            "${BIN_DIR}"
        printf 'but shells will not find forge until you add it, for example:\n'
        printf "  echo 'export PATH=\"%s:\$PATH\"' >>~/.profile\n" "${BIN_DIR}"
    fi
    SHADOWING_BIN="$(command -v forge 2>/dev/null || true)"
    if [[ -n "${SHADOWING_BIN}" && "${SHADOWING_BIN}" != "${BIN_DIR}/forge" ]]; then
        printf '\nNote: typing %s still runs %s, an older copy earlier in PATH.\n' \
            'forge' "${SHADOWING_BIN}"
        printf 'Remove it, or put %s ahead of it in PATH.\n' "${BIN_DIR}"
        printf 'The launcher entry is unaffected: it runs %s directly.\n' \
            "${BIN_DIR}/forge"
    fi
fi
printf 'Validate with: %s --doctor\n' "${BIN_DIR}/forge"
