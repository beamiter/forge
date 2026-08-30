#!/usr/bin/env bash
# Build a release archive around a fixture executable, then prove that every
# loader-supported bundled workflow survives package, install, and uninstall.

set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/forge-release-assets.XXXXXX")"
trap 'rm -rf -- "${TEST_ROOT}"' EXIT

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

assert_file() {
    [[ -f "$2" ]] || fail "$1 is not a regular file: $2"
}

assert_absent() {
    [[ ! -e "$2" && ! -L "$2" ]] || fail "$1 still exists: $2"
}

shopt -s nullglob
WORKFLOW_SOURCES=(
    "${PROJECT_ROOT}/scripts/workflows/"*.toml
    "${PROJECT_ROOT}/scripts/workflows/"*.yaml
    "${PROJECT_ROOT}/scripts/workflows/"*.yml
)
shopt -u nullglob
((${#WORKFLOW_SOURCES[@]} >= 6)) \
    || fail "expected at least six bundled workflow fixtures"
bash "${SCRIPT_DIR}/install-workflow-assets.sh" --check \
    "${PROJECT_ROOT}/scripts/workflows"

# Candidate validation is a preflight: an empty library or a symlinked member
# must fail before creating the destination tree.
empty_source="${TEST_ROOT}/empty-source"
empty_dest="${TEST_ROOT}/empty-dest"
mkdir -p "${empty_source}"
if bash "${SCRIPT_DIR}/install-workflow-assets.sh" \
    "${empty_source}" "${empty_dest}" >/dev/null 2>&1; then
    fail "workflow helper accepted an empty source directory"
fi
assert_absent "destination after empty-source rejection" "${empty_dest}"

linked_source="${TEST_ROOT}/linked-source"
linked_dest="${TEST_ROOT}/linked-dest"
mkdir -p "${linked_source}"
ln -s -- "${WORKFLOW_SOURCES[0]}" "${linked_source}/linked.yaml"
if bash "${SCRIPT_DIR}/install-workflow-assets.sh" \
    "${linked_source}" "${linked_dest}" >/dev/null 2>&1; then
    fail "workflow helper accepted a symlinked source"
fi
assert_absent "destination after symlink-source rejection" "${linked_dest}"

fixture="${TEST_ROOT}/forge"
printf '#!/bin/sh\nprintf "forge release fixture\\n"\n' >"${fixture}"
chmod 0755 "${fixture}"

version="0.0.0-assets-test"
target="test-linux"
dist="${TEST_ROOT}/dist"
env DIST_DIR="${dist}" VERSION="${version}" TARGET="${target}" \
    SOURCE_DATE_EPOCH=1700000000 \
    bash "${SCRIPT_DIR}/package-release.sh" "${fixture}" >/dev/null

archive="${dist}/forge-${version}-${target}.tar.gz"
assert_file "release archive" "${archive}"
tar -xzf "${archive}" -C "${TEST_ROOT}"
bundle="${TEST_ROOT}/forge-${version}-${target}"
assert_file "bundled workflow helper" \
    "${bundle}/libexec/install-workflow-assets.sh"

packaged_dir="${bundle}/share/forge/workflows"
shopt -s nullglob
PACKAGED_WORKFLOWS=(
    "${packaged_dir}/"*.toml
    "${packaged_dir}/"*.yaml
    "${packaged_dir}/"*.yml
)
shopt -u nullglob
((${#PACKAGED_WORKFLOWS[@]} == ${#WORKFLOW_SOURCES[@]})) \
    || fail "release archive workflow count differs from its source library"
for source in "${WORKFLOW_SOURCES[@]}"; do
    packaged="${packaged_dir}/${source##*/}"
    assert_file "packaged workflow" "${packaged}"
    cmp -- "${source}" "${packaged}" \
        || fail "packaged workflow differs from ${source}"
    [[ "$(stat -c '%a' -- "${packaged}")" == 644 ]] \
        || fail "packaged workflow mode is not 0644: ${packaged}"
done

# Publishing a known asset replaces a hostile final symlink rather than
# following it. The helper's build-root callers are trusted, but the release
# installer also runs against a user's existing asset directory.
symlink_dest="${TEST_ROOT}/symlink-dest"
symlink_victim="${TEST_ROOT}/symlink-victim"
mkdir -p "${symlink_dest}"
printf 'victim\n' >"${symlink_victim}"
ln -s -- "${symlink_victim}" \
    "${symlink_dest}/${WORKFLOW_SOURCES[0]##*/}"
bash "${SCRIPT_DIR}/install-workflow-assets.sh" \
    "${PROJECT_ROOT}/scripts/workflows" "${symlink_dest}"
[[ ! -L "${symlink_dest}/${WORKFLOW_SOURCES[0]##*/}" ]] \
    || fail "workflow helper retained a destination symlink"
[[ "$(<"${symlink_victim}")" == victim ]] \
    || fail "workflow helper followed a destination symlink"

home="${TEST_ROOT}/home"
config_home="${TEST_ROOT}/config"
mkdir -p "${home}"
env HOME="${home}" XDG_CONFIG_HOME="${config_home}" \
    PATH=/usr/bin:/bin USER=forge-assets-test \
    bash "${bundle}/install.sh" >/dev/null

installed_dir="${home}/.local/share/forge/workflows"
for source in "${WORKFLOW_SOURCES[@]}"; do
    installed="${installed_dir}/${source##*/}"
    assert_file "installed release workflow" "${installed}"
    cmp -- "${source}" "${installed}" \
        || fail "installed release workflow differs from ${source}"
done

custom="${installed_dir}/custom-user-workflow.yaml"
printf 'name: Custom\ncommand: echo custom\n' >"${custom}"
env HOME="${home}" XDG_CONFIG_HOME="${config_home}" PATH=/usr/bin:/bin \
    bash "${bundle}/uninstall.sh" --prefix "${home}/.local" >/dev/null
for source in "${WORKFLOW_SOURCES[@]}"; do
    assert_absent "owned release workflow" "${installed_dir}/${source##*/}"
done
assert_file "adjacent user workflow" "${custom}"

printf 'release workflow asset contract: ok\n'
