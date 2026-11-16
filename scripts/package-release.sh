#!/usr/bin/env bash
# Assemble a deterministic, relocatable user-local Linux release bundle.

set -Eeuo pipefail
umask 022
export LC_ALL=C

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
cd "${PROJECT_ROOT}"

BINARY="${1:-target/release/forge}"
DIST_DIR="${DIST_DIR:-${PROJECT_ROOT}/target/dist}"
VERSION="${VERSION:-$(awk -F ' *= *' '$1 == "version" { gsub(/"/, "", $2); print $2; exit }' Cargo.toml)}"
# TARGET may be supplied when packaging a prebuilt binary without Rust installed.
TARGET_EXPLICIT=1
if [[ -z "${TARGET:-}" ]]; then
    TARGET_EXPLICIT=0
    if ! RUSTC_VERBOSE="$(rustc -vV)"; then
        printf 'Error: cannot determine Rust target; set TARGET for a prebuilt binary.\n' >&2
        exit 1
    fi
    TARGET="$(sed -n 's/^host: //p' <<<"${RUSTC_VERBOSE}")"
fi
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || date +%s)}"

[[ -n "${VERSION}" ]] || { printf 'Error: cannot read Cargo version.\n' >&2; exit 1; }
[[ -n "${TARGET}" ]] || { printf 'Error: cannot determine Rust target.\n' >&2; exit 1; }
# VERSION and TARGET become path components and line-oriented BUILDINFO values.
# Reject separators/control bytes before creating any staging or output paths.
# Preserve semver build metadata and ordinary Rust target triples.
for field in VERSION TARGET; do
    value="${!field}"
    if [[ "${value}" == *[!a-zA-Z0-9._+-]* ]]; then
        printf 'Error: %s must contain only ASCII letters, digits, dot, underscore, plus or hyphen.\n' "${field}" >&2
        exit 1
    fi
done
if [[ "${SOURCE_DATE_EPOCH}" == *[!0-9]* ]]; then
    printf 'Error: SOURCE_DATE_EPOCH must be a nonnegative integer.\n' >&2
    exit 1
fi
# This records the packaging environment, not the compiler used for BINARY.
# A failed probe is permitted for an explicitly targeted prebuilt binary, but
# successful output must be one nonempty, control-free BUILDINFO value.
# Map NUL to a rejected control byte before Bash can discard it. pipefail
# preserves a failed rustc status through this byte filter.
if ! PACKAGE_RUSTC="$(rustc --version | tr '\000' '\r')"; then
    if [[ "${TARGET_EXPLICIT}" -eq 0 ]]; then
        printf 'Error: cannot read packaging rustc version without explicit TARGET.\n' >&2
        exit 1
    fi
    PACKAGE_RUSTC=unknown
    printf 'Warning: packaging rustc unavailable; BUILDINFO rustc=unknown (not binary provenance).\n' >&2
elif [[ -z "${PACKAGE_RUSTC}" || "${PACKAGE_RUSTC}" == *[[:cntrl:]]* ]]; then
    printf 'Error: rustc --version must return one nonempty line without control characters.\n' >&2
    exit 1
fi

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
ARCHIVE_STAGE=""
cleanup() {
    rm -rf -- "${STAGE_DIR}"
    if [[ -n "${ARCHIVE_STAGE}" ]]; then
        rm -rf -- "${ARCHIVE_STAGE}"
    fi
}
trap cleanup EXIT

install -Dm0755 "${BINARY}" "${PACKAGE_ROOT}/bin/forge"
install -Dm0755 scripts/support-bundle.sh "${PACKAGE_ROOT}/bin/forge-support-bundle"
install -Dm0755 packaging/install-release.sh "${PACKAGE_ROOT}/install.sh"
install -Dm0644 scripts/install-config.sh "${PACKAGE_ROOT}/libexec/install-config.sh"
install -Dm0644 scripts/migrate-legacy-binary.sh "${PACKAGE_ROOT}/libexec/migrate-legacy-binary.sh"
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
    printf 'rustc=%s\n' "${PACKAGE_RUSTC}"
} >"${PACKAGE_ROOT}/share/doc/forge/BUILDINFO"

install -d "${PACKAGE_ROOT}/share/forge/shell-integration"
install -m 0644 scripts/shell-integration/README.md scripts/shell-integration/forge.* \
    "${PACKAGE_ROOT}/share/forge/shell-integration/"
bash scripts/install-workflow-assets.sh scripts/workflows \
    "${PACKAGE_ROOT}/share/forge/workflows"
install -Dm0644 scripts/notebooks/welcome.jtnb.md \
    "${PACKAGE_ROOT}/share/forge/notebooks/welcome.jtnb.md"

mkdir -p "${DIST_DIR}"
# Build both outputs before replacing either old artifact. Staging inside the
# destination keeps each final rename on the same filesystem. The two renames
# are not a transaction: this protects build/hash failures, not power loss or
# concurrent readers between publication steps.
ARCHIVE_STAGE="$(mktemp -d "${DIST_DIR}/.forge-package.XXXXXX")"
tar --sort=name --mtime="@${SOURCE_DATE_EPOCH}" --owner=0 --group=0 --numeric-owner \
    -C "${STAGE_DIR}" -cf - "${PACKAGE_NAME}" \
    | gzip -n -9 >"${ARCHIVE_STAGE}/${ARCHIVE_NAME}"
(
    cd "${ARCHIVE_STAGE}"
    sha256sum "${ARCHIVE_NAME}" >"${ARCHIVE_NAME}.sha256"
)

mv -fT -- "${ARCHIVE_STAGE}/${ARCHIVE_NAME}" "${DIST_DIR}/${ARCHIVE_NAME}"
mv -fT -- "${ARCHIVE_STAGE}/${ARCHIVE_NAME}.sha256" "${DIST_DIR}/${ARCHIVE_NAME}.sha256"

printf 'Created %s\n' "${DIST_DIR}/${ARCHIVE_NAME}"
printf 'Created %s\n' "${DIST_DIR}/${ARCHIVE_NAME}.sha256"
