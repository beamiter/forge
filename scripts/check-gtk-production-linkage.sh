#!/usr/bin/env bash
set -euo pipefail

binary=${1:?Provide the Forge production ELF}
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$root"

if grep -qE '(^vte4[[:space:]]*=|^name = "vte4(-sys)?")' Cargo.toml Cargo.lock; then
    printf '%s\n' 'The Forge production manifest or lock contains libvte bindings.' >&2
    exit 1
fi
graph=$(cargo tree --locked --offline --edges normal,build --prefix none)
if grep -qE '^vte4(-sys)? ' <<<"$graph"; then
    printf '%s\n' 'The Forge production dependency graph includes libvte bindings.' >&2
    exit 1
fi
dynamic=$(readelf --dynamic -- "$binary")
imports=$(nm --dynamic --undefined-only -- "$binary")
if grep -Eiq 'NEEDED.*libvte' <<<"$dynamic" || grep -Eq '[[:space:]]vte_' <<<"$imports"; then
    printf '%s\n' 'The Forge production binary contains a libvte dependency or import.' >&2
    exit 1
fi
printf '%s\n' 'Forge production manifest, lock, Cargo graph and ELF are free of libvte.'
