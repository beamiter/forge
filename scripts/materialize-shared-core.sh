#!/usr/bin/env bash
# Keep the historical CI entrypoint, but never substitute an unversioned sibling
# or archived candidate for the committed production Git source.
set -euo pipefail
repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
if (($#)); then
  printf 'Production builds use Cargo.lock; sibling materialization is no longer supported.\n' >&2
  exit 2
fi
python3 "$repo_dir/scripts/check-shared-core-pin.py"
(cd "$repo_dir" && cargo fetch --locked)
printf 'Using Cargo-locked shared core; no sibling checkout created.\n'
