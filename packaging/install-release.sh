#!/usr/bin/env bash
# Install a prebuilt forge release bundle for the current user.

set -Eeuo pipefail
umask 077

APP_ID="io.github.beamiter.forge"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
HOME_DIR="${HOME:-}"
PREFIX="${HOME_DIR}/.local"
BIN_DIR="${PREFIX}/bin"
SHARE_DIR="${PREFIX}/share"
CONFIG_HOME="${XDG_CONFIG_HOME:-${HOME_DIR}/.config}"
CONFIG_DIR="${CONFIG_HOME}/forge"
ASSET_DIR="${SHARE_DIR}/forge"
DOC_DIR="${SHARE_DIR}/doc/forge"
CONFIG_SOURCE="${SCRIPT_DIR}/share/doc/forge/config.toml.example"
BINARY_SOURCE="${SCRIPT_DIR}/bin/forge"
SUPPORT_SOURCE="${SCRIPT_DIR}/bin/forge-support-bundle"
DESKTOP_SOURCE="${SCRIPT_DIR}/share/applications/${APP_ID}.desktop"
METAINFO_SOURCE="${SCRIPT_DIR}/share/metainfo/${APP_ID}.metainfo.xml"
SVG_SOURCE="${SCRIPT_DIR}/share/icons/hicolor/scalable/apps/${APP_ID}.svg"
PNG_128_SOURCE="${SCRIPT_DIR}/share/icons/hicolor/128x128/apps/${APP_ID}.png"
PNG_256_SOURCE="${SCRIPT_DIR}/share/icons/hicolor/256x256/apps/${APP_ID}.png"
SHELL_SOURCE_DIR="${SCRIPT_DIR}/share/forge/shell-integration"
WORKFLOW_SOURCE_DIR="${SCRIPT_DIR}/share/forge/workflows"
WORKFLOW_HELPER_SOURCE="${SCRIPT_DIR}/libexec/install-workflow-assets.sh"
NOTEBOOK_SOURCE="${SCRIPT_DIR}/share/forge/notebooks/welcome.jtnb.md"
README_SOURCE="${SCRIPT_DIR}/share/doc/forge/README.md"
LOCK_SOURCE="${SCRIPT_DIR}/share/doc/forge/Cargo.lock"
BUILDINFO_SOURCE="${SCRIPT_DIR}/share/doc/forge/BUILDINFO"
LICENSE_MIT_SOURCE="${SCRIPT_DIR}/share/doc/forge/LICENSE-MIT"
LICENSE_APACHE_SOURCE="${SCRIPT_DIR}/share/doc/forge/LICENSE-APACHE"
INSTALL_TEMP=""

die() {
    printf 'forge release install: %s\n' "$*" >&2
    exit 1
}

require_source_file() {
    local source="$1"
    [[ -f "${source}" && -r "${source}" && ! -L "${source}" ]] \
        || die "required release source is not a readable regular file: ${source}"
}

cleanup_install_temp() {
    if [[ -n "${INSTALL_TEMP:-}" ]]; then
        rm -f -- "${INSTALL_TEMP}"
        INSTALL_TEMP=""
    fi
}

trap cleanup_install_temp EXIT

# Stage beside the destination and publish with one rename. Reinstalling over
# a final symlink replaces the link itself, and interruption before the rename
# leaves the previous executable intact.
install_file_atomic() {
    local mode="$1" source="$2" dest="$3" directory basename
    directory="${dest%/*}"
    basename="${dest##*/}"
    install -d -m 0755 -- "${directory}"
    INSTALL_TEMP="$(mktemp "${directory}/.${basename}.install.XXXXXX")" \
        || die "cannot create temporary file beside ${dest}"
    install -m "${mode}" -- "${source}" "${INSTALL_TEMP}" \
        || die "cannot stage ${dest}"
    mv -fT -- "${INSTALL_TEMP}" "${dest}" \
        || die "cannot atomically replace ${dest}"
    INSTALL_TEMP=""
}

desktop_exec_value() {
    local remaining="$1" escaped="" character
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
    local source="$1" dest="$2" exec_path exec_value try_exec_value directory basename
    exec_path="${BIN_DIR}/forge"
    exec_value="$(desktop_exec_value "${exec_path}")"
    try_exec_value="$(desktop_try_exec_value "${exec_path}")"
    directory="${dest%/*}"
    basename="${dest##*/}"
    install -d -m 0755 -- "${directory}"
    INSTALL_TEMP="$(mktemp "${directory}/.${basename}.install.XXXXXX")" \
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

# Publish first-run configuration without a check-then-copy race. The private
# temporary and destination share a directory, so link(2) is atomic; EEXIST
# means an existing file, symlink, or concurrent writer wins and is preserved.
install_config_if_absent() {
    local source="$1" dest="$2" directory basename
    if [[ -e "${dest}" || -L "${dest}" ]]; then
        printf 'Keeping existing configuration: %s\n' "${dest}"
        return 0
    fi
    directory="${dest%/*}"
    basename="${dest##*/}"
    install -d -m 0700 -- "${directory}"
    INSTALL_TEMP="$(mktemp "${directory}/.${basename}.install.XXXXXX")" \
        || die "cannot create temporary configuration beside ${dest}"
    install -m 0600 -- "${source}" "${INSTALL_TEMP}" \
        || die "cannot stage initial configuration for ${dest}"
    if ln -- "${INSTALL_TEMP}" "${dest}" 2>/dev/null; then
        cleanup_install_temp
        printf 'Created %s\n' "${dest}"
        return 0
    fi
    if [[ -e "${dest}" || -L "${dest}" ]]; then
        cleanup_install_temp
        printf 'Keeping concurrently created configuration: %s\n' "${dest}"
        return 0
    fi
    cleanup_install_temp
    die "cannot atomically create initial configuration at ${dest}"
}

[[ -n "${HOME_DIR}" ]] || die "HOME is not set"
[[ "${CONFIG_HOME}" == /* ]] || die "XDG_CONFIG_HOME must be an absolute path"
[[ -f "${BINARY_SOURCE}" && -r "${BINARY_SOURCE}" && \
    -x "${BINARY_SOURCE}" && -s "${BINARY_SOURCE}" && ! -L "${BINARY_SOURCE}" ]] \
    || die "${BINARY_SOURCE} is not a non-empty executable regular file"
[[ -f "${SUPPORT_SOURCE}" && -r "${SUPPORT_SOURCE}" && \
    -x "${SUPPORT_SOURCE}" && -s "${SUPPORT_SOURCE}" && ! -L "${SUPPORT_SOURCE}" ]] \
    || die "${SUPPORT_SOURCE} is not a non-empty executable regular file"
[[ -f "${CONFIG_SOURCE}" && -r "${CONFIG_SOURCE}" && ! -L "${CONFIG_SOURCE}" ]] \
    || die "configuration template is not a readable regular file: ${CONFIG_SOURCE}"
[[ -f "${DESKTOP_SOURCE}" && -r "${DESKTOP_SOURCE}" && ! -L "${DESKTOP_SOURCE}" ]] \
    || die "desktop template is not a readable regular file: ${DESKTOP_SOURCE}"
for source in \
    "${METAINFO_SOURCE}" \
    "${SVG_SOURCE}" \
    "${PNG_128_SOURCE}" \
    "${PNG_256_SOURCE}" \
    "${WORKFLOW_HELPER_SOURCE}" \
    "${NOTEBOOK_SOURCE}" \
    "${README_SOURCE}" \
    "${LOCK_SOURCE}" \
    "${BUILDINFO_SOURCE}" \
    "${LICENSE_MIT_SOURCE}" \
    "${LICENSE_APACHE_SOURCE}"; do
    require_source_file "${source}"
done
[[ -x "${WORKFLOW_HELPER_SOURCE}" ]] \
    || die "workflow asset helper is not executable: ${WORKFLOW_HELPER_SOURCE}"
shopt -s nullglob
SHELL_SOURCES=(
    "${SHELL_SOURCE_DIR}/README.md"
    "${SHELL_SOURCE_DIR}/"forge.*
)
shopt -u nullglob
((${#SHELL_SOURCES[@]} >= 5)) \
    || die "release bundle is missing shell integration assets"
for source in "${SHELL_SOURCES[@]}"; do
    require_source_file "${source}"
done
bash "${WORKFLOW_HELPER_SOURCE}" --check "${WORKFLOW_SOURCE_DIR}"
validate_desktop_exec_path "${BIN_DIR}/forge"

printf 'Installing forge for %s...\n' "${USER:-the current user}"
install_file_atomic 0755 "${BINARY_SOURCE}" "${BIN_DIR}/forge"
install_file_atomic 0755 "${SUPPORT_SOURCE}" "${BIN_DIR}/forge-support-bundle"

install_config_if_absent "${CONFIG_SOURCE}" "${CONFIG_DIR}/config.toml"

# A desktop session fixes its PATH at login, so `Exec=forge` fails TryExec and
# hides the launcher entry whenever ${BIN_DIR} is missing from that PATH. This
# bundle always installs per-user, so point the entry at the safely escaped
# absolute path and publish the rewritten template atomically.
install_desktop_entry "${DESKTOP_SOURCE}" \
    "${SHARE_DIR}/applications/${APP_ID}.desktop"
install_file_atomic 0644 "${METAINFO_SOURCE}" \
    "${SHARE_DIR}/metainfo/${APP_ID}.metainfo.xml"
install_file_atomic 0644 "${SVG_SOURCE}" \
    "${SHARE_DIR}/icons/hicolor/scalable/apps/${APP_ID}.svg"
install_file_atomic 0644 "${PNG_128_SOURCE}" \
    "${SHARE_DIR}/icons/hicolor/128x128/apps/${APP_ID}.png"
install_file_atomic 0644 "${PNG_256_SOURCE}" \
    "${SHARE_DIR}/icons/hicolor/256x256/apps/${APP_ID}.png"

for source in "${SHELL_SOURCES[@]}"; do
    install_file_atomic 0644 "${source}" \
        "${ASSET_DIR}/shell-integration/${source##*/}"
done
bash "${WORKFLOW_HELPER_SOURCE}" \
    "${WORKFLOW_SOURCE_DIR}" "${ASSET_DIR}/workflows"
install_file_atomic 0644 "${NOTEBOOK_SOURCE}" \
    "${ASSET_DIR}/notebooks/welcome.jtnb.md"

install_file_atomic 0644 "${README_SOURCE}" "${DOC_DIR}/README.md"
install_file_atomic 0644 "${CONFIG_SOURCE}" "${DOC_DIR}/config.toml.example"
install_file_atomic 0644 "${LOCK_SOURCE}" "${DOC_DIR}/Cargo.lock"
install_file_atomic 0644 "${BUILDINFO_SOURCE}" "${DOC_DIR}/BUILDINFO"
install_file_atomic 0644 "${LICENSE_MIT_SOURCE}" "${DOC_DIR}/LICENSE-MIT"
install_file_atomic 0644 "${LICENSE_APACHE_SOURCE}" "${DOC_DIR}/LICENSE-APACHE"

if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate "${SHARE_DIR}/applications/${APP_ID}.desktop" || true
fi
# The caches below are generated files the desktop shell reads back, so they run
# under a relaxed umask instead of the owner-only one this script installs with.
if command -v update-desktop-database >/dev/null 2>&1; then
    (umask 022 && update-desktop-database "${SHARE_DIR}/applications") >/dev/null 2>&1 || true
fi
# A stale icon cache shadows the icons installed above, so always rebuild it.
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    (umask 022 && gtk-update-icon-cache --force --ignore-theme-index --quiet \
        "${SHARE_DIR}/icons/hicolor") >/dev/null 2>&1 || true
fi

printf '\nforge installation complete.\n'
printf '  Binary:            %s\n' "${BIN_DIR}/forge"
printf '  Support bundle:    %s\n' "${BIN_DIR}/forge-support-bundle"
printf '  Configuration:     %s\n' "${CONFIG_DIR}/config.toml"
printf '  Runtime assets:    %s\n' "${ASSET_DIR}"
printf '\nMake sure %s is in PATH, then run forge --doctor.\n' "${BIN_DIR}"
