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
installed_binary="${home}/.local/bin/forge"
installed_support="${home}/.local/bin/forge-support-bundle"
installed_config="${config_home}/forge/config.toml"
assert_file "installed release binary" "${installed_binary}"
assert_file "installed release support tool" "${installed_support}"
assert_file "installed release config" "${installed_config}"
cmp -- "${bundle}/bin/forge" "${installed_binary}" \
    || fail "installed release binary differs from the bundle"
cmp -- "${bundle}/bin/forge-support-bundle" "${installed_support}" \
    || fail "installed release support tool differs from the bundle"
[[ "$(stat -c '%a' -- "${installed_binary}")" == 755 ]] \
    || fail "installed release binary mode is not 0755"
[[ "$(stat -c '%a' -- "${installed_support}")" == 755 ]] \
    || fail "installed release support mode is not 0755"
[[ "$(stat -c '%a' -- "${installed_config}")" == 600 ]] \
    || fail "first-run release config mode is not 0600"
for source in "${WORKFLOW_SOURCES[@]}"; do
    installed="${installed_dir}/${source##*/}"
    assert_file "installed release workflow" "${installed}"
    cmp -- "${source}" "${installed}" \
        || fail "installed release workflow differs from ${source}"
done

custom="${installed_dir}/custom-user-workflow.yaml"
printf 'name: Custom\ncommand: echo custom\n' >"${custom}"
env HOME="${home}" XDG_CONFIG_HOME="${config_home}" PATH=/usr/bin:/bin \
    bash "${bundle}/uninstall.sh" >/dev/null
assert_absent "release binary under its default prefix" "${installed_binary}"
assert_absent "release support tool under its default prefix" "${installed_support}"
assert_file "release config preserved by default" "${installed_config}"
for source in "${WORKFLOW_SOURCES[@]}"; do
    assert_absent "owned release workflow" "${installed_dir}/${source##*/}"
done
assert_file "adjacent user workflow" "${custom}"

# A caller can still override the wrapper's default: forwarding happens after
# the injected prefix, so the shared parser's ordinary last-option semantics
# select the explicit value.
override_prefix="${TEST_ROOT}/override-prefix"
mkdir -p "${override_prefix}/bin"
touch "${override_prefix}/bin/forge"
override_dry_run="$(
    env HOME="${home}" PATH=/usr/bin:/bin \
        bash "${bundle}/uninstall.sh" --prefix "${override_prefix}" --dry-run
)"
[[ "${override_dry_run}" == *"${override_prefix}/bin/forge"* ]] \
    || fail "release uninstaller did not forward an explicit prefix"

# Force another writer to win exactly at link(2). The release installer must
# preserve that configuration and clean its private staging name.
race_home="${TEST_ROOT}/race-home"
race_config_home="${TEST_ROOT}/race-config"
race_config="${race_config_home}/forge/config.toml"
race_tools="${TEST_ROOT}/race-tools"
mkdir -p "${race_home}" "${race_tools}"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'destination=""' \
    'for argument do destination="${argument}"; done' \
    'printf "concurrent release config\n" >"${destination}"' \
    'exec /usr/bin/ln "$@"' \
    >"${race_tools}/ln"
chmod 0755 "${race_tools}/ln"
env HOME="${race_home}" XDG_CONFIG_HOME="${race_config_home}" \
    PATH="${race_tools}:/usr/bin:/bin" USER=forge-assets-test \
    bash "${bundle}/install.sh" >"${TEST_ROOT}/race-install.log"
[[ "$(<"${race_config}")" == 'concurrent release config' ]] \
    || fail "release installer overwrote a concurrent configuration"
[[ "$(<"${TEST_ROOT}/race-install.log")" == \
    *"Keeping concurrently created configuration"* ]] \
    || fail "release installer did not report the concurrent configuration"
shopt -s nullglob
RACE_CONFIG_TEMPS=("${race_config%/*}/.config.toml.install."*)
shopt -u nullglob
((${#RACE_CONFIG_TEMPS[@]} == 0)) \
    || fail "release config race left a temporary file"

# A dangling configuration link is still an existing user choice; do not
# follow it or replace it during a reinstall.
linked_home="${TEST_ROOT}/linked-home"
linked_config_home="${TEST_ROOT}/linked-config"
linked_config="${linked_config_home}/forge/config.toml"
mkdir -p "${linked_home}" "${linked_config%/*}"
ln -s -- missing-user-config "${linked_config}"
env HOME="${linked_home}" XDG_CONFIG_HOME="${linked_config_home}" \
    PATH=/usr/bin:/bin USER=forge-assets-test \
    bash "${bundle}/install.sh" >/dev/null
[[ -L "${linked_config}" ]] \
    || fail "release installer replaced a dangling configuration symlink"
assert_absent "dangling configuration target" \
    "${linked_config%/*}/missing-user-config"

# Final executable symlinks are replaced, never followed into another file.
atomic_home="${TEST_ROOT}/atomic-home"
atomic_config_home="${TEST_ROOT}/atomic-config"
atomic_bin="${atomic_home}/.local/bin"
binary_victim="${TEST_ROOT}/binary-victim"
support_victim="${TEST_ROOT}/support-victim"
mkdir -p "${atomic_bin}"
printf 'binary victim\n' >"${binary_victim}"
printf 'support victim\n' >"${support_victim}"
ln -s -- "${binary_victim}" "${atomic_bin}/forge"
ln -s -- "${support_victim}" "${atomic_bin}/forge-support-bundle"
env HOME="${atomic_home}" XDG_CONFIG_HOME="${atomic_config_home}" \
    PATH=/usr/bin:/bin USER=forge-assets-test \
    bash "${bundle}/install.sh" >/dev/null
[[ ! -L "${atomic_bin}/forge" && ! -L "${atomic_bin}/forge-support-bundle" ]] \
    || fail "release installer retained an executable destination symlink"
[[ "$(<"${binary_victim}")" == 'binary victim' ]] \
    || fail "release installer followed the binary destination symlink"
[[ "$(<"${support_victim}")" == 'support victim' ]] \
    || fail "release installer followed the support destination symlink"

# Kill the installer after it stages the first executable but before rename.
# The old binary must survive byte-for-byte and the EXIT trap must clean temp.
interrupt_home="${TEST_ROOT}/interrupt-home"
interrupt_config_home="${TEST_ROOT}/interrupt-config"
interrupt_bin="${interrupt_home}/.local/bin"
interrupt_tools="${TEST_ROOT}/interrupt-tools"
mkdir -p "${interrupt_bin}" "${interrupt_tools}"
printf 'old release forge\n' >"${interrupt_bin}/forge"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'last=""' \
    'for argument do last="${argument}"; done' \
    '/usr/bin/install "$@"' \
    'case "${last}" in *.install.*) kill -TERM "${PPID}" ;; esac' \
    >"${interrupt_tools}/install"
chmod 0755 "${interrupt_tools}/install"
if {
    env HOME="${interrupt_home}" XDG_CONFIG_HOME="${interrupt_config_home}" \
        PATH="${interrupt_tools}:/usr/bin:/bin" USER=forge-assets-test \
        bash "${bundle}/install.sh"
} >"${TEST_ROOT}/interrupt-install.log" 2>&1; then
    fail "interrupted release installer unexpectedly succeeded"
fi
[[ "$(<"${interrupt_bin}/forge")" == 'old release forge' ]] \
    || fail "interrupted release install replaced the old binary"
shopt -s nullglob
INTERRUPT_TEMPS=("${interrupt_bin}/.forge.install."*)
shopt -u nullglob
((${#INTERRUPT_TEMPS[@]} == 0)) \
    || fail "interrupted release install left a binary temporary"

printf 'release workflow asset contract: ok\n'
