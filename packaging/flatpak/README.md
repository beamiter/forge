# Flatpak shared core

`cargo-sources.json` is generated from the application lockfile for offline
registry and Git dependencies. Shared `jterm_core` is a local path dependency:
the build materializes the SHA-256-verified `packaging/jterm-core-src.tar.gz`
snapshot under `.shared/jterm_core` and adjusts the path in the temporary build
manifest. It does not fetch an obsolete Git version of shared core.

Regenerate source manifests with `scripts/update-flatpak-cargo-sources.sh
--update` after changing Cargo.lock. Regenerate the bundled core snapshot with
`scripts/package-shared-core.sh` after shared-core changes.
