#!/usr/bin/env bash
# Validate install/uninstall path symmetry and perform a real private DESTDIR
# round trip from a prebuilt fixture without invoking Cargo or Nix.

set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
INSTALLER="${SCRIPT_DIR}/install.sh"
UNINSTALLER="${SCRIPT_DIR}/uninstall.sh"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/forge-install-paths.XXXXXX")"
TEST_HOME="${TEST_ROOT}/home"
TEST_PATH="/usr/bin:/bin"

trap 'rm -rf -- "${TEST_ROOT}"' EXIT
mkdir -p "${TEST_HOME}"

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

assert_contains() {
    local label="$1" output="$2" expected="$3"
    [[ "${output}" == *"${expected}"* ]] \
        || fail "${label} did not contain ${expected@Q}"
}

assert_not_contains() {
    local label="$1" output="$2" unexpected="$3"
    [[ "${output}" != *"${unexpected}"* ]] \
        || fail "${label} unexpectedly contained ${unexpected@Q}"
}

assert_regular_file() {
    local label="$1" path="$2"
    [[ -f "${path}" ]] || fail "${label} is not a regular file: ${path}"
}

assert_mode() {
    local label="$1" path="$2" expected="$3" actual
    actual="$(stat -c '%a' -- "${path}")"
    [[ "${actual}" == "${expected}" ]] \
        || fail "${label} mode was ${actual}, expected ${expected}: ${path}"
}

install_dry_run() {
    local destdir="$1"
    shift
    env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${destdir}" \
        CARGO_TARGET_DIR= "${INSTALLER}" --dry-run "$@"
}

uninstall_dry_run() {
    local destdir="$1"
    shift
    env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${destdir}" \
        "${UNINSTALLER}" --dry-run "$@"
}

# The historical source-install default is ~/.cargo/bin. Install and uninstall
# must agree, while an explicit prefix switches both to PREFIX/bin.
default_install="$(install_dry_run "")"
mkdir -p "${TEST_HOME}/.cargo/bin"
touch "${TEST_HOME}/.cargo/bin/forge"
default_uninstall="$(uninstall_dry_run "")"
assert_contains "default install" "${default_install}" \
    "Installed forge to ${TEST_HOME}/.cargo/bin/forge"
assert_contains "default uninstall" "${default_uninstall}" \
    "${TEST_HOME}/.cargo/bin/forge"

custom_prefix="${TEST_ROOT}/prefix"
mkdir -p "${custom_prefix}/bin"
touch "${custom_prefix}/bin/forge"
assert_contains "explicit-prefix install" \
    "$(install_dry_run "" --prefix "${custom_prefix}")" \
    "Installed forge to ${custom_prefix}/bin/forge"
assert_contains "explicit-prefix uninstall" \
    "$(uninstall_dry_run "" --prefix "${custom_prefix}")" \
    "${custom_prefix}/bin/forge"

custom_bin="${TEST_ROOT}/custom-bin"
mkdir -p "${custom_bin}"
touch "${custom_bin}/forge"
assert_contains "custom-bin install" \
    "$(install_dry_run "" --bin-dir "${custom_bin}")" \
    "Installed forge to ${custom_bin}/forge"
assert_contains "custom-bin uninstall" \
    "$(uninstall_dry_run "" --bin-dir "${custom_bin}")" \
    "${custom_bin}/forge"

root_stage="$(install_dry_run / --prefix /opt/forge-root)"
assert_contains "root DESTDIR cache policy" "${root_stage}" \
    "Staged install (DESTDIR set); skipping desktop cache refresh."
assert_contains "root DESTDIR summary" "${root_stage}" \
    "Staged file: /opt/forge-root/bin/forge"

for bad_path in '/opt/forge/../escape' '/opt/forge/'$'bad\npath'; do
    if install_dry_run "${TEST_ROOT}/stage" --prefix "${bad_path}" >/dev/null 2>&1; then
        fail "installer accepted unsafe prefix ${bad_path@Q}"
    fi
    if uninstall_dry_run "${TEST_ROOT}/stage" --prefix "${bad_path}" >/dev/null 2>&1; then
        fail "uninstaller accepted unsafe prefix ${bad_path@Q}"
    fi
done
if install_dry_run "${TEST_ROOT}/stage/../escape" --prefix /opt/forge \
    >"${TEST_ROOT}/bad-destdir.log" 2>&1; then
    fail "installer accepted a DESTDIR parent component"
fi
assert_contains "DESTDIR parent diagnostic" "$(<"${TEST_ROOT}/bad-destdir.log")" \
    "DESTDIR must not contain '..' path components"

for command in install_dry_run uninstall_dry_run; do
    if "${command}" "" --bin-dir= >"${TEST_ROOT}/empty-bin.log" 2>&1; then
        fail "${command} accepted an empty --bin-dir"
    fi
    assert_contains "empty bin diagnostic" "$(<"${TEST_ROOT}/empty-bin.log")" \
        "--bin-dir must not be empty"
done

unicode_prefix='/opt/./锻造 terminal'
assert_contains "Unicode and dot path" \
    "$(install_dry_run "${TEST_ROOT}/stage" --prefix "${unicode_prefix}")" \
    "Installed forge to ${unicode_prefix}/bin/forge"

prebuilt_dir="${TEST_ROOT}/prebuilt"
prebuilt_binary="${prebuilt_dir}/forge"
stage="${TEST_ROOT}/roundtrip-stage"
runtime_prefix='/opt/forge release \dir $'
runtime_bin="${runtime_prefix}/bin"
runtime_share="${runtime_prefix}/share"
config_home="/etc/forge contract"
app_id="io.github.beamiter.forge"
mkdir -p "${prebuilt_dir}"
printf '#!/bin/sh\nprintf "forge fixture\\n"\n' >"${prebuilt_binary}"
chmod 0600 "${prebuilt_binary}"

install_output="$(
    env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${stage}" \
        XDG_CONFIG_HOME="${config_home}" "${INSTALLER}" \
        --binary "${prebuilt_binary}" --prefix "${runtime_prefix}" 2>&1
)"
assert_contains "prebuilt selected" "${install_output}" \
    "Using prebuilt forge binary: ${prebuilt_binary}"
assert_not_contains "prebuilt skips build" "${install_output}" "Building forge"

installed_binary="${stage}${runtime_bin}/forge"
installed_support="${stage}${runtime_bin}/forge-support-bundle"
installed_desktop="${stage}${runtime_share}/applications/${app_id}.desktop"
installed_metainfo="${stage}${runtime_share}/metainfo/${app_id}.metainfo.xml"
installed_asset="${stage}${runtime_share}/forge/notebooks/welcome.jtnb.md"
installed_config="${stage}${config_home}/forge/config.toml"
for file in "${installed_binary}" "${installed_support}" "${installed_desktop}" \
    "${installed_asset}" "${installed_config}"; do
    assert_regular_file "staged output" "${file}"
done
cmp -- "${prebuilt_binary}" "${installed_binary}" \
    || fail "installed binary differs from fixture"
assert_mode "binary" "${installed_binary}" 755
assert_mode "desktop" "${installed_desktop}" 644
assert_mode "config" "${installed_config}" 600

expected_exec='Exec="/opt/forge release \\\\dir \\$/bin/forge"'
[[ "$(grep -Fxc "${expected_exec}" "${installed_desktop}")" == 2 ]] \
    || fail "desktop Exec paths were not safely rewritten"
expected_try_exec='TryExec=/opt/forge release \\dir $/bin/forge'
grep -Fxq "${expected_try_exec}" "${installed_desktop}" \
    || fail "desktop TryExec path was not safely rewritten"
if grep -Fq "${stage}" "${installed_desktop}"; then
    fail "desktop entry leaked DESTDIR into its runtime path"
fi
if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate "${installed_desktop}"
fi

# Public resources are committed with rename, so a hostile destination link is
# replaced rather than followed and its target remains untouched.
asset_victim="${TEST_ROOT}/asset-must-not-change"
printf 'asset victim\n' >"${asset_victim}"
rm -f -- "${installed_metainfo}"
ln -s -- "${asset_victim}" "${installed_metainfo}"
env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${stage}" \
    XDG_CONFIG_HOME="${config_home}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" --prefix "${runtime_prefix}" >/dev/null
[[ ! -L "${installed_metainfo}" ]] \
    || fail "metainfo destination symlink survived reinstall"
[[ "$(<"${asset_victim}")" == 'asset victim' ]] \
    || fail "public asset install followed destination symlink"

# Reinstall over hostile destination symlinks. The binary symlink is replaced
# without touching its target; a dangling config symlink is preserved.
victim="${TEST_ROOT}/must-not-change"
printf 'victim\n' >"${victim}"
rm -f -- "${installed_binary}" "${installed_config}"
ln -s -- "${victim}" "${installed_binary}"
ln -s -- missing-config "${installed_config}"
env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${stage}" \
    XDG_CONFIG_HOME="${config_home}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" --prefix "${runtime_prefix}" --no-desktop >/dev/null
[[ ! -L "${installed_binary}" ]] || fail "binary destination symlink survived reinstall"
[[ "$(<"${victim}")" == victim ]] || fail "binary install followed destination symlink"
[[ -L "${installed_config}" ]] || fail "installer replaced a dangling config symlink"

shopt -s nullglob
binary_temps=("${installed_binary}.install."*)
desktop_temps=("${stage}${runtime_share}/applications/.${app_id}.desktop.install."*)
config_temps=("${stage}${config_home}/forge/.config.toml.install."*)
shopt -u nullglob
(( ${#binary_temps[@]} == 0 )) || fail "binary temporary files remain"
(( ${#desktop_temps[@]} == 0 )) || fail "desktop temporary files remain"
(( ${#config_temps[@]} == 0 )) || fail "config temporary files remain"

env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${stage}" \
    XDG_CONFIG_HOME="${config_home}" "${UNINSTALLER}" \
    --prefix "${runtime_prefix}" >/dev/null
[[ ! -e "${installed_binary}" && ! -L "${installed_binary}" ]] \
    || fail "uninstaller left binary"
[[ ! -e "${installed_desktop}" && ! -L "${installed_desktop}" ]] \
    || fail "uninstaller left desktop entry"
[[ -L "${installed_config}" ]] || fail "uninstaller removed preserved configuration"

interrupt_tools="${TEST_ROOT}/interrupt-tools"
interrupt_stage="${TEST_ROOT}/interrupt-stage"
interrupt_prefix="/opt/forge-interrupt"
interrupt_binary="${interrupt_stage}${interrupt_prefix}/bin/forge"
mkdir -p "${interrupt_tools}" "$(dirname -- "${interrupt_binary}")"
printf 'old interrupt forge\n' >"${interrupt_binary}"
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
    env HOME="${TEST_HOME}" PATH="${interrupt_tools}:${TEST_PATH}" \
        DESTDIR="${interrupt_stage}" "${INSTALLER}" --binary "${prebuilt_binary}" \
        --prefix "${interrupt_prefix}" --no-config --no-desktop
} >"${TEST_ROOT}/interrupt.log" 2>&1; then
    fail "interrupted installer unexpectedly succeeded"
fi
[[ "$(<"${interrupt_binary}")" == 'old interrupt forge' ]] \
    || fail "pre-rename interruption replaced the old binary"
shopt -s nullglob
interrupt_temps=("${interrupt_binary}.install."*)
shopt -u nullglob
(( ${#interrupt_temps[@]} == 0 )) \
    || fail "pre-rename interruption left a binary temporary"

prebuilt_symlink="${prebuilt_dir}/forge-link"
ln -s -- "${prebuilt_binary}" "${prebuilt_symlink}"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${stage}" \
    "${INSTALLER}" --binary "${prebuilt_symlink}" --prefix /opt/forge \
    --no-config --no-desktop >"${TEST_ROOT}/symlink-source.log" 2>&1; then
    fail "installer accepted a symlinked prebuilt binary"
fi
assert_contains "symlink source diagnostic" "$(<"${TEST_ROOT}/symlink-source.log")" \
    "prebuilt binary must not be a symbolic link"

if install_dry_run "" --binary= >"${TEST_ROOT}/empty-binary.log" 2>&1; then
    fail "installer accepted an empty --binary"
fi
assert_contains "empty binary diagnostic" "$(<"${TEST_ROOT}/empty-binary.log")" \
    "--binary must not be empty"

empty_prebuilt="${prebuilt_dir}/forge-empty"
: >"${empty_prebuilt}"
empty_stage="${TEST_ROOT}/empty-prebuilt-stage"
empty_prefix="/opt/forge-empty"
empty_sentinel="${empty_stage}${empty_prefix}/bin/forge"
mkdir -p "$(dirname -- "${empty_sentinel}")"
printf 'old empty forge\n' >"${empty_sentinel}"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${empty_stage}" \
    "${INSTALLER}" --binary "${empty_prebuilt}" --prefix "${empty_prefix}" \
    --no-config --no-desktop >"${TEST_ROOT}/empty-prebuilt.log" 2>&1; then
    fail "installer accepted a zero-byte prebuilt binary"
fi
assert_contains "zero-byte prebuilt diagnostic" \
    "$(<"${TEST_ROOT}/empty-prebuilt.log")" "prebuilt binary must not be empty"
[[ "$(<"${empty_sentinel}")" == 'old empty forge' ]] \
    || fail "zero-byte preflight replaced the old binary"

if install_dry_run "" --binary "${prebuilt_binary}" --backend cargo \
    >"${TEST_ROOT}/backend-binary.log" 2>&1; then
    fail "installer accepted both --backend and --binary"
fi
assert_contains "backend/binary diagnostic" "$(<"${TEST_ROOT}/backend-binary.log")" \
    "--backend cannot be combined with --binary"

# Packaging roots are caller controlled: an existing symlink ancestor must be
# rejected before it can redirect any staged write outside DESTDIR.
ancestor_stage="${TEST_ROOT}/ancestor-stage"
ancestor_victim="${TEST_ROOT}/ancestor-victim"
mkdir -p "${ancestor_stage}" "${ancestor_victim}"
ln -s -- "${ancestor_victim}" "${ancestor_stage}/opt"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${ancestor_stage}" \
    "${INSTALLER}" --binary "${prebuilt_binary}" --prefix /opt/forge \
    --no-config --no-desktop >"${TEST_ROOT}/ancestor.log" 2>&1; then
    fail "installer accepted a symlink ancestor beneath DESTDIR"
fi
assert_contains "staged symlink diagnostic" "$(<"${TEST_ROOT}/ancestor.log")" \
    "staged install path contains a symbolic-link ancestor"
[[ -z "$(find "${ancestor_victim}" -mindepth 1 -print -quit)" ]] \
    || fail "staged install escaped through a symlink ancestor"

# Force another writer to win immediately before link(2). The first-run config
# must preserve that file and leave no private staging name behind.
race_tools="${TEST_ROOT}/config-race-tools"
race_stage="${TEST_ROOT}/config-race-stage"
race_prefix="/opt/forge-config-race"
race_config_home="/etc/forge-config-race"
race_config="${race_stage}${race_config_home}/forge/config.toml"
mkdir -p "${race_tools}"
# shellcheck disable=SC2016
printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'destination=""' \
    'for argument do destination="${argument}"; done' \
    'printf "concurrent config\\n" >"${destination}"' \
    'exec /usr/bin/ln "$@"' \
    >"${race_tools}/ln"
chmod 0755 "${race_tools}/ln"
env HOME="${TEST_HOME}" PATH="${race_tools}:${TEST_PATH}" DESTDIR="${race_stage}" \
    XDG_CONFIG_HOME="${race_config_home}" "${INSTALLER}" \
    --binary "${prebuilt_binary}" --prefix "${race_prefix}" --no-desktop \
    >"${TEST_ROOT}/config-race.log" 2>&1
[[ "$(<"${race_config}")" == 'concurrent config' ]] \
    || fail "initial config publication overwrote a concurrent writer"
assert_contains "config race diagnostic" "$(<"${TEST_ROOT}/config-race.log")" \
    "Keeping concurrently created config"
shopt -s nullglob
race_config_temps=("${race_config%/*}/.config.toml.install."*)
shopt -u nullglob
(( ${#race_config_temps[@]} == 0 )) \
    || fail "config race left a temporary file"

# Desktop-path validation is a preflight: a bad launcher path must not replace
# an already installed executable before reporting the error.
invalid_stage="${TEST_ROOT}/invalid-desktop-stage"
invalid_prefix='/opt/forge=invalid'
sentinel_binary="${invalid_stage}${invalid_prefix}/bin/forge"
mkdir -p "$(dirname -- "${sentinel_binary}")"
printf 'old forge\n' >"${sentinel_binary}"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${invalid_stage}" \
    "${INSTALLER}" --binary "${prebuilt_binary}" --prefix "${invalid_prefix}" \
    --no-config >"${TEST_ROOT}/desktop-preflight.log" 2>&1; then
    fail "installer accepted an invalid desktop executable path"
fi
[[ "$(<"${sentinel_binary}")" == 'old forge' ]] \
    || fail "desktop preflight failure replaced the old binary"

# XDG paths are validated before any write, but an unused config override does
# not make a --no-config packaging run fail.
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${stage}" \
    XDG_CONFIG_HOME='/etc/forge/../escape' "${INSTALLER}" --dry-run \
    --binary "${prebuilt_binary}" --prefix /opt/forge >/dev/null 2>&1; then
    fail "installer accepted an escaping XDG_CONFIG_HOME"
fi
env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${stage}" \
    XDG_CONFIG_HOME='/etc/forge/../escape' "${INSTALLER}" --dry-run \
    --binary "${prebuilt_binary}" --prefix /opt/forge --no-config >/dev/null

invalid_xdg_prefix="/opt/forge-invalid-xdg"
xdg_sentinel="${stage}${invalid_xdg_prefix}/bin/forge"
mkdir -p "$(dirname -- "${xdg_sentinel}")"
printf 'old xdg forge\n' >"${xdg_sentinel}"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${stage}" \
    XDG_CONFIG_HOME='/etc/forge/../escape' "${INSTALLER}" \
    --binary "${prebuilt_binary}" --prefix "${invalid_xdg_prefix}" --no-desktop \
    >"${TEST_ROOT}/xdg-preflight.log" 2>&1; then
    fail "installer accepted an escaping XDG_CONFIG_HOME"
fi
[[ "$(<"${xdg_sentinel}")" == 'old xdg forge' ]] \
    || fail "XDG preflight failure replaced the old binary"

# Recursive purge roots are preflighted before ordinary installed files are
# touched, so an unsafe XDG override cannot cause a partial uninstall.
purge_stage="${TEST_ROOT}/purge-stage"
purge_prefix="/opt/forge-purge"
mkdir -p "${purge_stage}${purge_prefix}/bin"
touch "${purge_stage}${purge_prefix}/bin/forge"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${purge_stage}" \
    XDG_STATE_HOME='/var/lib/forge/../escape' "${UNINSTALLER}" \
    --prefix "${purge_prefix}" --purge-config >/dev/null 2>&1; then
    fail "purge accepted an escaping XDG_STATE_HOME"
fi
assert_regular_file "binary after rejected purge" \
    "${purge_stage}${purge_prefix}/bin/forge"

# A package tree may be inspected after an untrusted build step.  Uninstall
# must not follow an ancestor link and remove a same-named file outside
# DESTDIR; rejecting the final file symlink itself is unnecessary.
uninstall_link_stage="${TEST_ROOT}/uninstall-link-stage"
uninstall_link_victim="${TEST_ROOT}/uninstall-link-victim"
uninstall_link_prefix="/opt/forge-uninstall-link"
mkdir -p "${uninstall_link_stage}" \
    "${uninstall_link_victim}/forge-uninstall-link/bin"
printf 'outside forge\n' \
    >"${uninstall_link_victim}/forge-uninstall-link/bin/forge"
ln -s -- "${uninstall_link_victim}" "${uninstall_link_stage}/opt"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" \
    DESTDIR="${uninstall_link_stage}" "${UNINSTALLER}" \
    --prefix "${uninstall_link_prefix}" >"${TEST_ROOT}/uninstall-link.log" 2>&1; then
    fail "uninstaller followed a symbolic-link ancestor below DESTDIR"
fi
assert_contains "uninstall ancestor diagnostic" \
    "$(<"${TEST_ROOT}/uninstall-link.log")" \
    "staged uninstall path contains a symbolic-link ancestor"
[[ "$(<"${uninstall_link_victim}/forge-uninstall-link/bin/forge")" == \
    'outside forge' ]] || fail "uninstaller removed a file outside DESTDIR"

# Recursive purge roots receive the same preflight before the ordinary binary
# removal, so an unsafe state ancestor cannot cause either escape or partial
# uninstall.
purge_link_stage="${TEST_ROOT}/purge-link-stage"
purge_link_victim="${TEST_ROOT}/purge-link-victim"
purge_link_prefix="/opt/forge-purge-link"
mkdir -p "${purge_link_stage}${purge_link_prefix}/bin" \
    "${purge_link_stage}/var" "${purge_link_victim}/state/forge"
printf 'installed forge\n' >"${purge_link_stage}${purge_link_prefix}/bin/forge"
printf 'outside state\n' >"${purge_link_victim}/state/forge/sentinel"
ln -s -- "${purge_link_victim}/state" "${purge_link_stage}/var/state"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${purge_link_stage}" \
    XDG_STATE_HOME=/var/state "${UNINSTALLER}" \
    --prefix "${purge_link_prefix}" --purge-config \
    >"${TEST_ROOT}/purge-link.log" 2>&1; then
    fail "purge followed a symbolic-link ancestor below DESTDIR"
fi
assert_regular_file "binary after rejected symlink purge" \
    "${purge_link_stage}${purge_link_prefix}/bin/forge"
[[ "$(<"${purge_link_victim}/state/forge/sentinel")" == 'outside state' ]] \
    || fail "purge removed state outside DESTDIR"

# DESTDIR itself may be disguised as `link/.` or `link//`: both spellings used
# to make `-L "$DESTDIR"` follow the link before testing it. Normalize first,
# then inspect the complete root chain before any install or removal.
root_link="${TEST_ROOT}/destdir-root-link"
root_victim="${TEST_ROOT}/destdir-root-victim"
root_prefix="/opt/forge-destdir-root"
root_binary="${root_victim}${root_prefix}/bin/forge"
mkdir -p "${root_victim}"
ln -s -- "${root_victim}" "${root_link}"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${root_link}/." \
    "${INSTALLER}" --binary "${prebuilt_binary}" --prefix "${root_prefix}" \
    --no-config --no-desktop >"${TEST_ROOT}/root-link-install.log" 2>&1; then
    fail "installer accepted a symlinked DESTDIR root disguised with /."
fi
assert_contains "symlinked DESTDIR install diagnostic" \
    "$(<"${TEST_ROOT}/root-link-install.log")" \
    "DESTDIR path contains a symbolic-link component"
[[ -z "$(find "${root_victim}" -mindepth 1 -print -quit)" ]] \
    || fail "symlinked DESTDIR install wrote outside its staging boundary"

mkdir -p "$(dirname -- "${root_binary}")"
printf 'outside root forge\n' >"${root_binary}"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${root_link}//" \
    "${UNINSTALLER}" --prefix "${root_prefix}" \
    >"${TEST_ROOT}/root-link-uninstall.log" 2>&1; then
    fail "uninstaller accepted a symlinked DESTDIR root with trailing separators"
fi
assert_contains "symlinked DESTDIR uninstall diagnostic" \
    "$(<"${TEST_ROOT}/root-link-uninstall.log")" \
    "DESTDIR path contains a symbolic-link component"
[[ "$(<"${root_binary}")" == 'outside root forge' ]] \
    || fail "symlinked DESTDIR uninstall removed an outside binary"

root_state="${root_victim}/var/lib/forge-root/forge/sentinel"
root_config="${root_victim}/etc/forge-root/forge/sentinel"
mkdir -p "$(dirname -- "${root_state}")" "$(dirname -- "${root_config}")"
printf 'outside root state\n' >"${root_state}"
printf 'outside root config\n' >"${root_config}"
if env HOME="${TEST_HOME}" PATH="${TEST_PATH}" DESTDIR="${root_link}/./" \
    XDG_STATE_HOME=/var/lib/forge-root XDG_CONFIG_HOME=/etc/forge-root \
    "${UNINSTALLER}" --prefix "${root_prefix}" --purge-config \
    >"${TEST_ROOT}/root-link-purge.log" 2>&1; then
    fail "purge accepted a symlinked DESTDIR root"
fi
assert_contains "symlinked DESTDIR purge diagnostic" \
    "$(<"${TEST_ROOT}/root-link-purge.log")" \
    "DESTDIR path contains a symbolic-link component"
assert_regular_file "binary after rejected root-symlink purge" "${root_binary}"
[[ "$(<"${root_state}")" == 'outside root state' ]] \
    || fail "root-symlink purge removed outside state"
[[ "$(<"${root_config}")" == 'outside root config' ]] \
    || fail "root-symlink purge removed outside config"

printf 'install/uninstall path contract: ok\n'
