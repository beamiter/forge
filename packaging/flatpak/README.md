# Flatpak shared core

`cargo-sources.json` is generated from the application lockfile for offline
registry and Git dependencies. Shared `jterm_core` is an exact Git dependency
in `Cargo.lock`. The generated manifest fetches that revision and vendors it
with the registry dependencies, so the Flatpak build uses the locked sources
offline.

Regenerate source manifests with `scripts/update-flatpak-cargo-sources.sh
--update` after changing Cargo.lock. Cargo, Nix, Flatpak, and extracted production
fixtures now use the published Git source. `scripts/check-shared-core-pin.py`
rejects committed path overrides, mismatched test pins, stale lock identities,
and an alternate Flatpak or Nix core source.

The older `packaging/jterm-core-src.tar.gz`, revision and origin files are
historical snapshots, not the source used by the current builds. The compatibility
entrypoint `scripts/materialize-shared-core.sh` only checks the production pin
and runs `cargo fetch --locked`; it never extracts that archive or picks a sibling.
Use an explicit command-scoped Cargo override for local candidate experiments,
and do not report those results as exact production-pin validation.

Nix's full source-tree NAR hash is separate from an archive SHA-256. Repinning
requires updating the `jterm_core-0.2.0` output hash as well as the manifest,
lockfile, dependency policy and generated Flatpak sources.
