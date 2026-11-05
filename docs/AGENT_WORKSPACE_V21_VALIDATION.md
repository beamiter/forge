# Agent Workspace V2.1: Forge source and stability checkpoint

Date: 2026-11-05. This record distinguishes source checks, executable tests,
blocked checks and pending hosted display/packaging results. It is not a claim
that V2.1 or the five-repository Mission has passed all acceptance gates.

## Audited baseline

The starting master was `e115c34a5799bf1394f7df9eab3fe422493a4f39`, including
Eva's approved handoff composer. Its existing CI failures were inspected at:

- [CI](https://github.com/beamiter/forge/actions/runs/41791363009)
- [Flatpak](https://github.com/beamiter/forge/actions/runs/41791363034)
- [Native Vim pixel viewport](https://github.com/beamiter/forge/actions/runs/41791363082)

Rustfmt failed in four source files. Strict Clippy reported twelve findings.
The Vim workflow stopped at formatting, so that run did not test Vim behavior.
The prototype inherited the root `.cargo/config.toml` path override, which also
made the prototype dependency-policy lock check fail. Flatpak stopped before
building because its generated sources still described a Git core while the
lockfile described an unversioned path core.

The GTK input harness could not locate the declared Git core because the actual
build used the bundled sibling. The independent-font and search-preview fixtures
used a PTY child that exits immediately: the production reader correctly retired
it before later fixture bytes could form finished blocks. The fixtures now use
the existing `for_live_display_tests` owner, whose child lifetime and teardown
have a dedicated unit regression. No production child-exit guard, assertion,
timeout or test-selection requirement was weakened.

## One production source

Runtime and test dependencies now use the published shared-core commit
`f8566bb9cb6e9cca3258c8050c6dc4e9b96da8ff`. Cargo.lock records that exact Git
source. Nix, dependency policy and regenerated Flatpak sources agree. The global
path override is removed; the compatibility materialization entrypoint checks
the source contract and fetches the lock, never extracting an archive or choosing
a sibling tree. The older bundled snapshot is historical and is not a build input.

The source guard has five mutation tests, including path overrides, missing Git
lock identity, a stale Flatpak pin and an alternate Nix bundled source. It runs in
CI and `make verify`. The existing runtime source helper separately verified Cargo
metadata resolves the exact Git source.

Nix source NAR: `sha256-zEC7/WI43Y4CiPCq+OStmEVcFzEGtnRVEMe+zOS2FYo=`.
A recursive NAR serialization of the exact Git export produced this value. The
same procedure first reproduced the existing `10120c3` output hash
`sha256-pWlvnbcgGitlQ+Hm30ONIB0z6NgGGWu7sJvlH4UuZB0=`. This verifies the source
hash calculation, not a Nix application build.

## Executed in the assistant's isolated cloud workspace

Rust 1.99.1, Linux x86-64. No user desktop or existing Codex thread was used.

- `cargo fetch --locked`, production source guard, Cargo metadata and Rustfmt passed.
- Versioned host/process adapter manifests and tracked-text privacy check passed.
- Files follow/navigation: 30 tests; search lifecycle: 28; pane lifecycle: 10;
  tab selection: 7; real Linux PTY resize harness: 11. All passed.
- Real Linux PTY input harness: 7 passed against the exact pinned core modules.
- Native Pango font helper and independent-size policies: 23 passed. This is not
  a full GTK window/font regression.
- Search-preview shape: 4 passed; chat metadata oracles: 10; render oracles: 18.
- GTK runner orchestration: 11 scenarios passed against fake subprocesses and
  172 registered cases. No native display acceptance is implied by that result.
- The pinned Flatpak source generator passed both `--update` and a fresh `--check`.
- The ASCII organism prototype passed strict Clippy and all 16 tests with its
  unchanged committed lockfile, confirming the inherited-patch failure is fixed.

## Blocked and pending gates

`cargo check --locked --all-targets` was attempted and stopped in `glib-sys`:
`glib-2.0.pc` is absent. The cloud executor does not have GTK development packages,
and its system-package install path is restricted. Therefore the application
compile, strict Clippy, full suite, Rustdoc and release build must be checked by
the hosted CI for the published frontend commit.

The chat-shell oracle suite ran 20 tests, with 19 passing and the native AF_UNIX
socket creation blocked by the executor (`EPERM`). A permitted escalated attempt
had the same outcome. That failure is retained, not skipped or counted as a pass.

Hosted GTK font/search/Vim regressions, Flatpak build and smoke, and Nix build
remain pending until their exact-commit workflow results are recorded. Real IME
composition/candidate UI, physical hardware, Wayland, provider execution, remote
SSH, and a live model request surviving GUI closure were not verified here.

## Hosted checkpoint: c10869c

[CI run 41950429946](https://github.com/beamiter/forge/actions/runs/41950429946)
completed the production source/PTY input, native font groups, exact search
preview (including the previously timing-out production dialog), native Vim9,
top-tab geometry and same-execution metadata checks successfully. The unit test,
strict Clippy, Rustfmt, Rustdoc, release, dependency policy, shell, privacy,
prototype and Nix package jobs passed. The separate
[Native Vim](https://github.com/beamiter/forge/actions/runs/41950429875),
[native conversation](https://github.com/beamiter/forge/actions/runs/41950429820)
and [Flatpak](https://github.com/beamiter/forge/actions/runs/41950429848)
workflows also passed, including Flatpak installation and X11/Wayland smoke.
These exact-commit results do not certify later frontend revisions.

The main GTK job failed in the F13 synthetic-render fixture before capture zero:
`chat_render_tests.rs:132` asserted nonzero native clip/control bounds. Its
artifact shows the render fixture still used the immediately-exiting PTY child;
the asynchronous reader had retired the active prompt before rendering. That
fixture now uses the same owned live-display child as the font/search fixtures.
Its geometry, cursor, glyph, scrollbar and capture assertions remain unchanged.
The artifact digest was
`cfdccc2b17b212af554eedb196ad7ead20daad1931cf73686fbd6704288bb5fc`.

The plain real-Bash smoke and broad GTK/VTE runner were skipped by GitHub's default
success condition after that failure. They are **not** counted as passes. These
independent stages now use `!cancelled()` so an earlier failure still leaves the
job failed while allowing all native diagnostics to execute. Native verification
of the F13 fixture change and the broad regression set is pending its next run.

## Hosted checkpoint: 0ea0fbf

[CI run 41951959168](https://github.com/beamiter/forge/actions/runs/41951959168)
now passes the complete F13 synthetic fractional-scale rendering matrix,
confirming the live-display fixture correction. Flatpak, Nix, release, unit,
Clippy, Rustfmt, Rustdoc and the independent native workflows also passed.
The newly unblocked real-Bash keyboard/clipboard smoke fails: Bash accepted both
commands' bytes correctly, but the history-disabled pasted command's completed
capsule became `(command capture unavailable)` after resize, yielding a one-row
capsule instead of the keyboard command's four rows. The strict geometry check
is retained; production command-capture diagnosis remains open.

The broad GTK runner reached registration preflight and found one obsolete test
name. `successful_spawn_losing_close_and_exit_before_completion_retire_exact_ownership`
was renamed to `late_owned_outcomes_obey_controller_close_exit_and_reentry` in
`terminal::launch_owner::tests`; the script now names that actual owned-child
regression. No test was removed. The broad native set has still not executed
successfully and is not counted as accepted. Its new run also includes the
Mission editing regression.

The diagnostic artifact SHA256 is
`01f9aba5fdb2d688c463f7a4b459127d1db7f54cf0a111e815ac64a70db7f65a`.
