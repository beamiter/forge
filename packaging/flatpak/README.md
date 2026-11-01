# Flatpak shared core

`cargo-sources.json` is generated from the application lockfile for offline
registry and Git dependencies. Shared `jterm_core` is an exact Git dependency
in `Cargo.lock`. The generated manifest fetches that revision and vendors it
with the registry dependencies, so the Flatpak build uses the locked sources
offline.

Regenerate source manifests with `scripts/update-flatpak-cargo-sources.sh
--update` after changing Cargo.lock. Regenerate the bundled core snapshot with
`scripts/package-shared-core.sh /path/to/exact-published-core` after shared-core
changes. The source directory must contain the exact published commit without
local edits. Update `packaging/jterm-core-revision` and
`packaging/jterm-core-origin.json` alongside the archive and `.sha256` file.
The archive retains the same source revision for isolated path-based consumers;
the current Flatpak build uses its generated Git source entry.
