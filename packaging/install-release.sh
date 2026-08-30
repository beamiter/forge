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
INSTALL_TEMP=""

die() {
    printf 'forge release install: %s\n' "$*" >&2
    exit 1
}

cleanup_install_temp() {
    if [[ -n "${INSTALL_TEMP:-}" ]]; then
        rm -f -- "${INSTALL_TEMP}"
        INSTALL_TEMP=""
    fi
}

trap cleanup_install_temp EXIT

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
[[ -x "${SCRIPT_DIR}/bin/forge" ]] \
    || die "${SCRIPT_DIR}/bin/forge is missing or not executable"
[[ -f "${CONFIG_SOURCE}" && -r "${CONFIG_SOURCE}" && ! -L "${CONFIG_SOURCE}" ]] \
    || die "configuration template is not a readable regular file: ${CONFIG_SOURCE}"

printf 'Installing forge for %s...\n' "${USER:-the current user}"
install -Dm0755 "${SCRIPT_DIR}/bin/forge" "${BIN_DIR}/forge"
install -Dm0755 "${SCRIPT_DIR}/bin/forge-support-bundle" \
    "${BIN_DIR}/forge-support-bundle"

install_config_if_absent "${CONFIG_SOURCE}" "${CONFIG_DIR}/config.toml"

# A desktop session fixes its PATH at login, so `Exec=forge` fails TryExec and
# hides the launcher entry whenever ${BIN_DIR} is missing from that PATH. This
# bundle always installs per-user, so point the entry at the absolute path.
install -d -m 0755 "${SHARE_DIR}/applications"
awk -v exec_path="${BIN_DIR}/forge" '
    /^Exec=forge([[:space:]]|$)/ || /^TryExec=forge([[:space:]]|$)/ {
        eq = index($0, "=")
        print substr($0, 1, eq) exec_path substr($0, eq + 7)
        next
    }
    { print }
' "${SCRIPT_DIR}/share/applications/${APP_ID}.desktop" \
    >"${SHARE_DIR}/applications/${APP_ID}.desktop.new"
chmod 0644 "${SHARE_DIR}/applications/${APP_ID}.desktop.new"
mv -f -- "${SHARE_DIR}/applications/${APP_ID}.desktop.new" \
    "${SHARE_DIR}/applications/${APP_ID}.desktop"
install -Dm0644 "${SCRIPT_DIR}/share/metainfo/${APP_ID}.metainfo.xml" \
    "${SHARE_DIR}/metainfo/${APP_ID}.metainfo.xml"
install -Dm0644 "${SCRIPT_DIR}/share/icons/hicolor/scalable/apps/${APP_ID}.svg" \
    "${SHARE_DIR}/icons/hicolor/scalable/apps/${APP_ID}.svg"
for size in 128 256; do
    install -Dm0644 \
        "${SCRIPT_DIR}/share/icons/hicolor/${size}x${size}/apps/${APP_ID}.png" \
        "${SHARE_DIR}/icons/hicolor/${size}x${size}/apps/${APP_ID}.png"
done

install -d -m 0755 "${ASSET_DIR}/shell-integration" "${ASSET_DIR}/workflows"
install -m 0644 "${SCRIPT_DIR}/share/forge/shell-integration/README.md" \
    "${SCRIPT_DIR}"/share/forge/shell-integration/forge.* \
    "${ASSET_DIR}/shell-integration/"
bash "${SCRIPT_DIR}/libexec/install-workflow-assets.sh" \
    "${SCRIPT_DIR}/share/forge/workflows" "${ASSET_DIR}/workflows"
install -Dm0644 "${SCRIPT_DIR}/share/forge/notebooks/welcome.jtnb.md" \
    "${ASSET_DIR}/notebooks/welcome.jtnb.md"

install -Dm0644 "${SCRIPT_DIR}/share/doc/forge/README.md" "${DOC_DIR}/README.md"
install -Dm0644 "${SCRIPT_DIR}/share/doc/forge/Cargo.lock" "${DOC_DIR}/Cargo.lock"
install -Dm0644 "${SCRIPT_DIR}/share/doc/forge/BUILDINFO" "${DOC_DIR}/BUILDINFO"

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
