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

Production packages use only `shared-terminal`. The Flatpak runtime and Nix
package do not provision VTE. Explicit legacy oracle builds require separately
installed VTE development libraries. The offline Cargo source manifest may still
vendor optional legacy crates present in Cargo.lock; vendoring does not enable
their Cargo feature.

The source manifest and Nix Git output hash match exact core
`06bbe8c45bab2a51525116f2f4dabb4efc3e9c7e`. Its private candidate tree was
verified locally using command-scoped Git URL redirection. Public availability
has not been asserted; public package fetches require that exact revision to be
available from their configured Git source. The production receipt records the
independently verified NAR hash without claiming publication.
