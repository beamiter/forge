# Contributing to forge

## Development setup

The reproducible path is the repository's Nix shell:

```bash
nix develop
cargo run
```

A native Cargo build also works after installing GTK4, libadwaita, VTE GTK4, PCRE2, and `pkg-config` development packages. The repository toolchain file installs stable Rust with rustfmt and Clippy.

`jagent` and `jterm_core` are pinned Git dependencies with an explicit crate `version` plus an exact `rev`. Keep those in sync with `Cargo.lock`, `deny.toml`, and the matching `cargoLock.outputHashes` entry in `flake.nix`; otherwise `cargo deny check`, `nix develop`, and `nix build` will fail. When repinning either revision, temporarily set the corresponding output hash to `pkgs.lib.fakeHash`, run `nix build .#default`, and copy the `got:` hash from the mismatch error.

## Required checks

Run the same gates as CI before opening a pull request:

```bash
make verify
make security
```

`make verify` runs formatting, tests, strict Clippy, Rustdoc, the release build,
shell syntax checks, and the tracked-text privacy guard. `make security` runs the
locked dependency audit, `cargo deny` source/license policy, duplicate-dependency
report, and ShellCheck policy; it requires `cargo-audit`, `cargo-deny`, and
`shellcheck`. Run `make privacy` alone for the fast privacy check. It rejects
only known personal identifiers; neutral placeholders and RFC 5737 documentation
addresses remain valid examples.

For packaging changes, also run `desktop-file-validate`, `appstreamcli validate --no-net`, regenerate `packaging/flatpak/cargo-sources.json`, build the Flatpak manifest, execute `scripts/smoke-flatpak.sh` inside a D-Bus session, and verify the archive produced by `make package` with its SHA-256 file.

For UI changes, smoke-test both Wayland and X11 when practical, VTE and Block modes, CJK input, tab closing, process cleanup, and session restoration. Changes to Block rendering should also follow `docs/BLOCK_MODE_ACCEPTANCE.md`.

## Design expectations

Keep GTK work on the main thread and filesystem/process work off it. Preserve explicit PTY ownership, generation/cancellation checks for asynchronous UI results, atomic persistence, and backwards-compatible configuration defaults. Add focused unit tests for pure parsing, state transitions, quoting, and boundary conditions.

Never commit tokens, private hostnames, personal paths, captured terminal output, or real configuration files. Use placeholders in documentation and tests. Report vulnerabilities through `SECURITY.md`, not a public proof of concept.

## Licensing

forge is distributed under the `MIT OR Apache-2.0` dual license. Unless a
contribution is conspicuously marked otherwise before it is accepted, submitting
it for inclusion means that you license that contribution under the same terms,
without additional restrictions. Contributors must have the right to submit the
code, documentation, tests, and assets they provide.

## Pull requests

Prefer reviewable commits with a clear user-visible rationale. Update `README.md`, the user guide, architecture notes, and `CHANGELOG.md` when behavior changes. A pull request should describe residual risk and any manual checks that cannot run in CI.
