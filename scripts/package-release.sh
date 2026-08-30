#!/usr/bin/env bash
# Assemble a deterministic, relocatable user-local Linux release bundle.

set -Eeuo pipefail
umask 022

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
cd "${PROJECT_ROOT}"

BINARY="${1:-target/release/forge}"
DIST_DIR="${DIST_DIR:-${PROJECT_ROOT}/target/dist}"
VERSION="${VERSION:-$(awk -F ' *= *' '$1 == "version" { gsub(/"/, "", $2); print $2; exit }' Cargo.toml)}"
TARGET="${TARGET:-$(rustc -vV | sed -n 's/^host: //p')}"
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || date +%s)}"

[[ -n "${VERSION}" ]] || { printf 'Error: cannot read Cargo version.\n' >&2; exit 1; }
[[ -n "${TARGET}" ]] || { printf 'Error: cannot determine Rust target.\n' >&2; exit 1; }
[[ -x "${BINARY}" ]] || {
    printf 'Error: release binary not found or not executable: %s\n' "${BINARY}" >&2
    printf "Run 'make package' outside a Nix shell, or pass an explicit system-linked release binary.\n" >&2
    exit 1
}

APP_ID="io.github.beamiter.forge"
PACKAGE_NAME="forge-${VERSION}-${TARGET}"
ARCHIVE_NAME="${PACKAGE_NAME}.tar.gz"
STAGE_DIR="$(mktemp -d)"
PACKAGE_ROOT="${STAGE_DIR}/${PACKAGE_NAME}"
trap 'rm -rf -- "${STAGE_DIR}"' EXIT

install -Dm0755 "${BINARY}" "${PACKAGE_ROOT}/bin/forge"
install -Dm0755 scripts/support-bundle.sh "${PACKAGE_ROOT}/bin/forge-support-bundle"
install -Dm0755 packaging/install-release.sh "${PACKAGE_ROOT}/install.sh"
install -Dm0755 packaging/uninstall-release.sh "${PACKAGE_ROOT}/uninstall.sh"
install -Dm0755 scripts/uninstall.sh "${PACKAGE_ROOT}/libexec/uninstall.sh"
install -Dm0755 scripts/install-workflow-assets.sh \
    "${PACKAGE_ROOT}/libexec/install-workflow-assets.sh"
install -Dm0644 packaging/RELEASE_README.md "${PACKAGE_ROOT}/README.txt"
printf '%s\n' "${VERSION}" >"${PACKAGE_ROOT}/VERSION"

install -Dm0644 "data/${APP_ID}.desktop" \
    "${PACKAGE_ROOT}/share/applications/${APP_ID}.desktop"
install -Dm0644 "data/${APP_ID}.metainfo.xml" \
    "${PACKAGE_ROOT}/share/metainfo/${APP_ID}.metainfo.xml"
install -Dm0644 "data/${APP_ID}.svg" \
    "${PACKAGE_ROOT}/share/icons/hicolor/scalable/apps/${APP_ID}.svg"
for size in 128 256; do
    install -Dm0644 "data/${APP_ID}-${size}.png" \
        "${PACKAGE_ROOT}/share/icons/hicolor/${size}x${size}/apps/${APP_ID}.png"
done

install -Dm0644 README.md "${PACKAGE_ROOT}/share/doc/forge/README.md"
install -Dm0644 LICENSE-MIT "${PACKAGE_ROOT}/share/doc/forge/LICENSE-MIT"
install -Dm0644 LICENSE-APACHE "${PACKAGE_ROOT}/share/doc/forge/LICENSE-APACHE"
install -Dm0644 config.toml.example \
    "${PACKAGE_ROOT}/share/doc/forge/config.toml.example"
install -Dm0644 Cargo.lock "${PACKAGE_ROOT}/share/doc/forge/Cargo.lock"
install -d "${PACKAGE_ROOT}/share/doc/forge"
{
    printf 'version=%s\n' "${VERSION}"
    printf 'target=%s\n' "${TARGET}"
    printf 'source_date_epoch=%s\n' "${SOURCE_DATE_EPOCH}"
    printf 'git_commit=%s\n' "$(git rev-parse HEAD 2>/dev/null || printf unknown)"
    printf 'rustc=%s\n' "$(rustc --version)"
} >"${PACKAGE_ROOT}/share/doc/forge/BUILDINFO"

install -d "${PACKAGE_ROOT}/share/forge/shell-integration"
install -m 0644 scripts/shell-integration/README.md scripts/shell-integration/forge.* \
    "${PACKAGE_ROOT}/share/forge/shell-integration/"
bash scripts/install-workflow-assets.sh scripts/workflows \
    "${PACKAGE_ROOT}/share/forge/workflows"
install -Dm0644 scripts/notebooks/welcome.jtnb.md \
    "${PACKAGE_ROOT}/share/forge/notebooks/welcome.jtnb.md"

mkdir -p "${DIST_DIR}"
rm -f -- "${DIST_DIR}/${ARCHIVE_NAME}" "${DIST_DIR}/${ARCHIVE_NAME}.sha256"
tar --sort=name --mtime="@${SOURCE_DATE_EPOCH}" --owner=0 --group=0 --numeric-owner \
    -C "${STAGE_DIR}" -cf - "${PACKAGE_NAME}" \
    | gzip -n -9 >"${DIST_DIR}/${ARCHIVE_NAME}"
(
    cd "${DIST_DIR}"
    sha256sum "${ARCHIVE_NAME}" >"${ARCHIVE_NAME}.sha256"
)

printf 'Created %s\n' "${DIST_DIR}/${ARCHIVE_NAME}"
printf 'Created %s\n' "${DIST_DIR}/${ARCHIVE_NAME}.sha256"
