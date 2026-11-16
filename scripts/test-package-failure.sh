#!/usr/bin/env bash
# Exercise the real packager with a non-executed binary fixture.
set -Eeuo pipefail
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(mktemp -d)"
trap 'rm -rf -- "${ROOT}"' EXIT
mkdir -p "${ROOT}/bin" "${ROOT}/dist with spaces"
printf '#!/bin/sh\nexit 0\n' >"${ROOT}/binary"
chmod +x "${ROOT}/binary"
# Only BUILDINFO needs rustc for this packaging-only regression. This is not a
# Rust build test; CI separately builds and packages the production executable.
printf '#!/bin/sh\nprintf "rustc packaging-test-stub\\n"\n' >"${ROOT}/bin/rustc"
chmod +x "${ROOT}/bin/rustc"
export DIST_DIR="${ROOT}/dist with spaces" VERSION=0.0.0-failure-test TARGET=test-linux
export SOURCE_DATE_EPOCH=1700000000
export PATH="${ROOT}/bin:${PATH}"
archive="${DIST_DIR}/forge-${VERSION}-${TARGET}.tar.gz"
package() { bash "${SCRIPT_DIR}/package-release.sh" "${ROOT}/binary"; }
assert_clean() {
    local remaining
    remaining="$(find "${DIST_DIR}" -maxdepth 1 -name '.forge-package.*' -print)"
    [[ -z "${remaining}" ]]
}
package >"${ROOT}/initial.log"
cp -- "${archive}" "${ROOT}/expected.tar.gz"
cp -- "${archive}.sha256" "${ROOT}/expected.sha256"
# Fail after writing partial output, not merely before starting the command.
for tool in tar gzip sha256sum; do
    printf '#!/bin/sh\nprintf "partial output\\n"\nexit 73\n' >"${ROOT}/bin/${tool}"
    chmod +x "${ROOT}/bin/${tool}"
    if package >"${ROOT}/${tool}.log" 2>&1; then
        printf 'FAIL: %s failure was accepted\n' "${tool}" >&2
        exit 1
    fi
    cmp -- "${archive}" "${ROOT}/expected.tar.gz"
    cmp -- "${archive}.sha256" "${ROOT}/expected.sha256"
    assert_clean
    rm -- "${ROOT}/bin/${tool}"
done
# A later successful rebuild still replaces both outputs and hashes their final
# name (rather than a temporary path). Changing the fixture changes the archive.
printf '# replacement bytes\n' >>"${ROOT}/binary"
package >"${ROOT}/replacement.log"
if cmp -s -- "${archive}" "${ROOT}/expected.tar.gz"; then
    printf 'FAIL: successful replacement kept the old archive\n' >&2
    exit 1
fi
(cd "${DIST_DIR}" && sha256sum --check "${archive##*/}.sha256")
tar -tzf "${archive}" >/dev/null
assert_clean
printf 'PASS: packaging failures preserve old archive/checksum and recovery succeeds\n'
