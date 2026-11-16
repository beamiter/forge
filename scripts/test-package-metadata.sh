#!/usr/bin/env bash
# Real packager, non-executed binary fixture; not a Rust build or GUI test.
set -Eeuo pipefail
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(mktemp -d)"
trap 'rm -rf -- "${ROOT}"' EXIT
mkdir -p "${ROOT}/bin" "${ROOT}/tmp" "${ROOT}/dist with spaces"
printf '#!/bin/sh\nexit 0\n' >"${ROOT}/binary"
chmod +x "${ROOT}/binary"
printf '#!/bin/sh\nprintf "rustc packaging-test-stub\\n"\n' >"${ROOT}/bin/rustc"
chmod +x "${ROOT}/bin/rustc"
export PATH="${ROOT}/bin:${PATH}" TMPDIR="${ROOT}/tmp"
export DIST_DIR="${ROOT}/dist with spaces"
export VERSION=1.2.3-rc.1+meta_2 TARGET=x86_64-unknown-linux-gnu SOURCE_DATE_EPOCH=1700000000
package() { bash "${SCRIPT_DIR}/package-release.sh" "${ROOT}/binary"; }
package >"${ROOT}/valid.log"
archive="${DIST_DIR}/forge-${VERSION}-${TARGET}.tar.gz"
(cd "${DIST_DIR}" && sha256sum --check "${archive##*/}.sha256")
tar -xOf "${archive}" "forge-${VERSION}-${TARGET}/share/doc/forge/BUILDINFO" >"${ROOT}/buildinfo"
grep -Fx "version=${VERSION}" "${ROOT}/buildinfo"
grep -Fx "target=${TARGET}" "${ROOT}/buildinfo"
grep -Fx "source_date_epoch=${SOURCE_DATE_EPOCH}" "${ROOT}/buildinfo"
[[ "$(wc -l <"${ROOT}/buildinfo")" -eq 5 ]]
cp -- "${archive}" "${ROOT}/expected.tar.gz"
cp -- "${archive}.sha256" "${ROOT}/expected.sha256"
# Any attempt to stage invalid metadata is a failure, even if packaging later
# fails for another reason. This also makes the old-code negative control safe.
cat >"${ROOT}/bin/mktemp" <<'STUB'
#!/bin/sh
printf 'staging reached\n' >"${METADATA_STAGE_MARKER}"
exit 79
STUB
chmod +x "${ROOT}/bin/mktemp"
export METADATA_STAGE_MARKER="${ROOT}/staging-reached"
reject() {
    local field="$1" value="$2"
    rm -f -- "${METADATA_STAGE_MARKER}"
    if env "${field}=${value}" bash "${SCRIPT_DIR}/package-release.sh" "${ROOT}/binary" >"${ROOT}/invalid.log" 2>&1; then
        printf 'FAIL: invalid %s accepted\n' "${field}" >&2
        exit 1
    fi
    [[ ! -e "${METADATA_STAGE_MARKER}" ]]
    grep -F "Error: ${field} must" "${ROOT}/invalid.log" >/dev/null
    cmp -- "${archive}" "${ROOT}/expected.tar.gz"
    cmp -- "${archive}.sha256" "${ROOT}/expected.sha256"
}
for field in VERSION TARGET; do
    for bad in 'x/../../outside' 'x\outside' $'x\ninjected=1' $'x\r' $'x\t' 'x y' 'é'; do
        reject "${field}" "${bad}"
    done
done
for bad in -1 1.5 '1 second' $'1\ninjected=1' 1/2; do
    reject SOURCE_DATE_EPOCH "${bad}"
done
rm -- "${ROOT}/bin/mktemp"
SOURCE_DATE_EPOCH=0 package >"${ROOT}/zero.log"
(cd "${DIST_DIR}" && sha256sum --check "${archive##*/}.sha256")
printf 'PASS: metadata rejected before staging; valid semver/target and zero epoch accepted\n'
