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

## Command capture after reflow

The history-disabled Bash artifact has an exact append-only application draft at
stage 6, but VTE reflow moved the command's end above its old prompt anchor.
Refusing that native extraction range is correct. Treating the refusal as literal
`(command capture unavailable)` text prevented the existing empty-capture resolver
from using that exact draft. The reader now allows that existing fallback only
for a nonempty exact shadow with no explicit shell truncation. Edited or
post-submit typeahead shadows still fail closed. The result remains screen-derived,
not shell-reported or eligible for exact replay. Reviewed/Agent submissions still
require their independent byte-exact command identity and cannot gain approval
from this fallback.

Regression coverage includes rejected-range reader dispatch for both exact states
and the inexact state, unavailable reviewed Agent identity (lost event and cleared
generation), explicit truncation, empty shadows, and the existing edited-input
identity cases. The 22 non-GTK source-contract tests passed locally against the
final shared pin; the full ReaderHarness and real Bash/GTK acceptance remain for
hosted CI. The original native geometry assertion has not been relaxed.

The first Mission native build found a missing GTK optional-cast trait import,
fixed in `d0fd5e8`. Its next hosted Clippy run found one needless borrow in the
native Mission test; that test expression has been corrected without allowing
warnings. These findings are not native acceptance of the Mission UI.

## Hosted checkpoint: d9df27f

[CI run 41955538580](https://github.com/beamiter/forge/actions/runs/41955538580)
passed the real Bash keyboard/clipboard matrix with the unchanged geometry
assertion, confirming the rejected-range fallback fix. Its PTY/Pango/Vim,
synthetic F13, shell metadata/provenance, independent font, top-tab geometry and
exact-search stages all passed. The main test binary passed 2,285 ordinary tests;
240 display/host/manual tests remained explicitly ignored there and are not
counted as native passes. Strict Clippy, Rustfmt, Rustdoc, release, Nix and the
other non-GTK CI jobs passed. The separate
[Native Vim](https://github.com/beamiter/forge/actions/runs/41955538615),
[native conversation](https://github.com/beamiter/forge/actions/runs/41955538518)
and [Flatpak](https://github.com/beamiter/forge/actions/runs/41955538543)
workflows passed, including Flatpak build/install and X11/Wayland smoke.

The broad GTK runner finally passed registration and ran the Mission editing
regression successfully. It started 140 of 173 named cases: 102 passed, 37 failed,
and the current case was interrupted at the job's 20-minute limit; 33 had not
started. Those failures and unexecuted cases remain open acceptance gates. The
artifact SHA256 is
`f9add3b2f5a8d19cc45a664d21fe9d5162ac71d8805c472f632fd1fe54a15871`.

Several display fixtures still installed the real asynchronous reader over an
immediately-exiting inert child. Their later prompt replay was then correctly
rejected as belonging to a retired process. The display-only fixtures now retain
the owned inert child until teardown, as the already-passing font/F13 fixtures do.
Synchronous dispatch/commit harnesses and tests of actual exit behavior remain
separate. The complete runner has a 35-minute job budget and a 120-second deadline
per native case, so a stuck case fails explicitly without hiding the remaining
cases. Timeout/signal status and the first failure remain fatal. This scheduling
change alone does not resolve or waive any of the 37 assertions.

The newly reached cases also exposed production issues unrelated to PTY capture:
workflow controls could act during the dialog's closing animation, and batch
session edits looked up pending organization drafts using an obsolete identity.
The fixes retire callbacks when close starts and check the same full session
identity as the draft editor. Cwd recovery is an explicit ordinary-local retry
policy only: generic, task and remote launches continue to reject an unavailable
required directory. Closed-pane cleanup checks reaping of the original child,
not a UI callback that the closed-pane guard intentionally suppresses.

Other fixture expectations are aligned with existing contracts rather than
production behavior being changed to satisfy them: the shared navigation minimum,
real allocated Tab targets, ten-second elapsed labels and a static but still
visible organism. Pixel comparisons wait for stable geometry without forcing a
full redraw; their unchanged pixel assertions still decide success. Native
verification of these additional fixes remains pending the next exact-commit run.

## Hosted checkpoint: 737c88e

[CI run 41959890897](https://github.com/beamiter/forge/actions/runs/41959890897)
completed all 173 named GTK cases: 167 passed and six failed. None was skipped
or cut off by the job deadline. All preceding PTY/Pango/Vim, F13 synthetic and
real-Bash, metadata/provenance, font, top-tab and exact-search stages passed.
The other CI jobs and independent Vim/conversation workflows passed. The
[Flatpak run](https://github.com/beamiter/forge/actions/runs/41959890960) passed
source verification, build, install and both X11/Wayland launch checks.
The GTK artifact SHA256 is
`e6c36ac9b49c6dc711fe0ef7839d41c75e449a9c012606fda28d2bc5727a1533`.

The six remaining cases cover workflow dialog release, approval pagination,
incremental organism paint, Block's alternate-screen round trip, failed VTE
launch diagnostics and Unified navigation. Their follow-up corrections retain
all six tests and their acceptance assertions:

- Block's hidden dock continues to contribute its measured footer budget when
  an integrated prompt/running card enters fullscreen. The controls remain
  hidden by their parent. A direct CLI with no prior dock, Chat and Unified keep
  their distinct geometry policies. The child must not receive a seat-only
  resize on Block's 1049h/l round trip.
- Launch diagnostics name only the original executable plus the sanitized
  failure cause, never its arguments or environment.
- Paint comparison waits for the actual scroll controller to retire before
  requiring stable geometry and exact incremental/full-expose pixels. Unified
  records its normal grid only after the closing Find entry is unmapped.
- Dialog and approval fixtures use allocated hosts. Dialog release separately
  checks immediate callback retirement, focus return, animation-time unparenting
  and final weak-reference release. Their original time budgets remain intact.

These corrections still need the next exact-commit native run. In particular,
the former dialog failure did not identify which widget retained ownership;
the staged release diagnostics distinguish native host ownership from a signal
cycle instead of asserting a production leak without evidence.

## Hosted checkpoint: 6722978

[CI run 41964166559](https://github.com/beamiter/forge/actions/runs/41964166559)
completed 173/173 native cases with 172 passes. The Block round trip now passes
both exact native/child grid checks and the unchanged resize epoch, excluding
an intermediate resize that later returns to the same size. Unified navigation,
workflow dialog release, approval pagination and failed-launch task retirement
also pass. All earlier GTK matrix stages, other CI jobs, independent native
workflows and [Flatpak](https://github.com/beamiter/forge/actions/runs/41964166579)
passed. The Mission native regression passes again.

The sole remaining gate is the incremental organism paint oracle. Geometry and
the real scroll controller now stay identical, but its complete old/new damage
envelope also includes an unrelated native overlay scrollbar. All 115 differing
pixels are in that fading scrollbar, outside the current card. A second full
expose changes another 65 pixels including the focused entry's blinking caret,
so even the two full-reference captures are not temporally identical. The
original zero-difference assertion correctly remains failed. The downloaded
artifact was verified against SHA256
`0856d424ad426bd405f33a81e714e25edab8078e24b4b6345a7025cffa40d850`.

The paint fixture now makes only this surrounding chrome stationary before its
first map/focus: the card's ancestor uses a non-overlay scrollbar, and the entry
cursor does not blink. A scoped guard restores both properties immediately after
the paint oracle, before the remaining lifecycle checks. GTK animations and the
real canvas scroll controller stay enabled; neither the damage region nor the
zero-pixel tolerance changes. GTK 4.14.5's `remove_indicator` cancels the overlay
fade sources, while `GtkText` checks the blink policy when installing its focus
tick. This isolates unrelated temporal changes, not a production repaint fix.
The ordinary scrollbar has a different width allocation; this oracle verifies
the card's incremental paint under that real layout, not overlay scrollbar
animation. Restoration also waits for settled layout before later hit testing.
The next exact-commit native run must still prove the original pixel assertion.

## Hosted checkpoint: 9d09ff0

[CI run 41967605701](https://github.com/beamiter/forge/actions/runs/41967605701)
passed all six strict incremental/full-expose paint stages, the unchanged
snapshot-reuse assertion and restored-layout barrier. The same long organism
case then reached its previously unexecuted tooltip checks and failed its first
native target query. Overall the 173-case gate remains 172 passed / one failed;
all other CI jobs, native workflows and Flatpak passed again.

The new Block/Chat canvas places a transparent `DrawingArea` above the whole
canvas solely to observe allocation. `can_target(false)` excludes normal input
picking but not the tooltip's deliberate `NON_TARGETABLE` pick. GTK's default
`contains` therefore let this nonpainting probe hide the real label beneath it.
The probe now subclasses `DrawingArea` with an empty hit region, retaining its
native allocation and resize signal. The existing native Chat case also checks
that it maps, reports allocation/resizes and never becomes a non-targetable pick
result. Tooltip ownership, pointer-state checks and the external decorative
overlay rejection are unchanged. The next exact-commit native run is required
before treating the remaining long case as passed.

## Hosted checkpoint: 0fe2a7c

[CI run 41970947524](https://github.com/beamiter/forge/actions/runs/41970947524)
passed the resize observer's native allocation/pick regression and the long
organism case's tooltip ownership, actual overlay obstruction, drag suppression,
settings/timer and horizontal clipping checks. The 173-case gate still has
172 passes: the long case then entered its 1,000-cycle lifecycle soak and crashed
on its first explicit PTY detach. GDB identifies `vte_terminal_set_pty` called
from `lifecycle_tests.rs`. All other CI jobs, native workflows and Flatpak passed.

The runner uses VTE 0.76.0. In that version, `Widget::set_pty(NULL)` calls
`_vte_pty_get_impl(NULL)`, which dereferences the null argument. The fixture now
uses normal terminal/root disposal, whose VTE destructor calls
`unset_pty(false)` to disconnect both I/O watches and clear pending I/O. It also
requires the terminal and root weak references to expire, in addition to the
eight adapter widgets, so avoiding the null setter cannot hide a retained live
terminal. The 1,000 cycles, stage/time budgets, callback/timer retirement and
before/after slave no-write assertions remain unchanged. No production null
PTY setter exists in Forge. The next exact-commit run must finish this soak
before the native gate can be considered passed.
