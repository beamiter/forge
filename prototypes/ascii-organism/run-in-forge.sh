#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/../.." && pwd)"

cargo build --offline --manifest-path "${script_dir}/Cargo.toml"
forge_bin="${FORGE_BIN:-${repo_root}/target/debug/forge}"
if [[ ! -x "${forge_bin}" ]]; then
    forge_bin="$(command -v forge || true)"
fi
if [[ -z "${forge_bin}" ]]; then
    echo "forge executable not found; build the repository or set FORGE_BIN" >&2
    exit 1
fi

exec "${forge_bin}" --mode vte --no-restore \
    --working-directory "${script_dir}" \
    --execute "${script_dir}/target/debug/forge-ascii-organism" --demo --speed 1.5
