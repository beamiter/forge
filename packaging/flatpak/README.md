# Flatpak shared core

`cargo-sources.json` is generated from the application lockfile for offline
registry and Git dependencies. Shared `jterm_core` is an exact Git dependency
in `Cargo.lock`. The generated manifest fetches that revision and vendors it
with the registry dependencies, so the Flatpak build uses the locked sources
offline.

Regenerate source manifests with `scripts/update-flatpak-cargo-sources.sh
--update` after changing Cargo.lock. Regenerate the bundled core snapshot with
`scripts/package-shared-core.sh /path/to/exact-pinned-core` after shared-core
changes. The source directory must contain the exact pinned commit without
local edits. Update `packaging/jterm-core-revision` and
`packaging/jterm-core-origin.json` alongside the archive and `.sha256` file.
The archive retains the same source revision for isolated path-based consumers;
the current Flatpak build uses its generated Git source entry.
A staged release can be checked against a local commit before publication; the
origin record states that boundary explicitly. Push that exact core commit
before remote builds acquire the new dependency. The source archive SHA-256
and the full source tree NAR hash used by Nix are separate values.
