#!/usr/bin/env bash
# Copy every bundled workflow format accepted by jterm_core into one package
# asset directory. This helper is for trusted build/release inputs; user-authored
# workflow directories are never passed here.

set -Eeuo pipefail
umask 077

usage() {
    printf 'Usage: %s SOURCE_DIR DEST_DIR\n' "${0##*/}"
    printf '       %s --check SOURCE_DIR\n' "${0##*/}"
}

die() {
    printf 'forge workflow assets: %s\n' "$*" >&2
    exit 1
}

CHECK_ONLY=0
if (($# == 2)) && [[ "$1" == --check ]]; then
    CHECK_ONLY=1
    SOURCE_DIR="$2"
    DEST_DIR=""
elif (($# == 2)); then
    SOURCE_DIR="$1"
    DEST_DIR="$2"
else
    usage >&2
    exit 2
fi
INSTALL_TEMP=""

cleanup() {
    if [[ -n "${INSTALL_TEMP}" ]]; then
        rm -f -- "${INSTALL_TEMP}"
    fi
}
trap cleanup EXIT

[[ -d "${SOURCE_DIR}" && ! -L "${SOURCE_DIR}" ]] \
    || die "source is not a real directory: ${SOURCE_DIR}"
if ((CHECK_ONLY == 0)); then
    [[ -n "${DEST_DIR}" ]] || die "destination must not be empty"
fi

# This is the same extension set as jterm_core::workflows::is_workflow_file.
# Nullglob makes an empty library fail explicitly instead of handing install a
# literal `*.toml` path.
shopt -s nullglob
WORKFLOW_SOURCES=(
    "${SOURCE_DIR}/"*.toml
    "${SOURCE_DIR}/"*.yaml
    "${SOURCE_DIR}/"*.yml
)
shopt -u nullglob
((${#WORKFLOW_SOURCES[@]} > 0)) \
    || die "no .toml, .yaml, or .yml workflows found in ${SOURCE_DIR}"

for source in "${WORKFLOW_SOURCES[@]}"; do
    [[ -f "${source}" && -r "${source}" && ! -L "${source}" ]] \
        || die "workflow is not a readable regular file: ${source}"
done

((CHECK_ONLY == 0)) || exit 0

install -d -m 0755 -- "${DEST_DIR}"
for source in "${WORKFLOW_SOURCES[@]}"; do
    basename="${source##*/}"
    INSTALL_TEMP="$(mktemp "${DEST_DIR}/.${basename}.install.XXXXXX")" \
        || die "cannot stage ${DEST_DIR}/${basename}"
    install -m 0644 -- "${source}" "${INSTALL_TEMP}" \
        || die "cannot copy ${source}"
    mv -fT -- "${INSTALL_TEMP}" "${DEST_DIR}/${basename}" \
        || die "cannot publish ${DEST_DIR}/${basename}"
    INSTALL_TEMP=""
done
