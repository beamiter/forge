#!/usr/bin/env bash
# Retain the historical ~/.cargo/bin entry as a link to the canonical install.
# The caller validates the destination and only invokes this for ~/.local/bin.
migrate_legacy_binary() (
    set -Eeuo pipefail
    umask 077
    local old="$1" canonical="$2" fd backup temp="" identity
    [[ -e "${old}" || -L "${old}" ]] || return 0
    if [[ -L "${old}" && "$(readlink -- "${old}")" == "${canonical}" ]]; then
        return 0
    fi
    [[ -L "${old}" || -f "${old}" ]] || die "legacy binary is not a file: ${old}"
    printf 'Migrate legacy binary %q -> %q (preserving a backup)\n' "${old}" "${canonical}"
    ((DRY_RUN == 0)) || return 0
    exec {fd}<"${old%/*}"
    local bound="/proc/self/fd/${fd}/${old##*/}"
    identity="$(stat -c '%d:%i:%s:%y:%z' -- "${bound}")"
    backup="$(mktemp -d "/proc/self/fd/${fd}/.${old##*/}.backup.XXXXXX")"
    # Preserve a link as a link; never write into a Nix store or other referent.
    cp -P -- "${bound}" "${backup}/${old##*/}"
    printf 'Saved previous binary: %s/%s/%s\n' "${old%/*}" "${backup##*/}" "${old##*/}"
    temp="${backup}/replacement"
    ln -s -- "${canonical}" "${temp}"
    [[ "$(stat -c '%d:%i:%s:%y:%z' -- "${bound}")" == "${identity}" ]] \
        || die "legacy binary changed during migration; backup retained"
    mv -fT -- "${temp}" "${bound}"
)
