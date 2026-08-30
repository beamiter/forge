# Forge upgrade rounds

This ledger records the independently testable increments in the current
upgrade pass. It is intentionally tied to behavior and regression evidence,
not commits.

Rounds 1–10 record the preceding pass; this pass's additional forty-three rounds
are numbered 11–53.

1. **Install/uninstall symmetry** — no-argument uninstall now targets the same
   historical `~/.cargo/bin` location as no-argument install; explicit prefixes
   still select `PREFIX/bin`.
2. **Runtime path boundary** — prefix and binary paths reject empty values,
   controls, relative paths, and lexical parent components without rejecting
   spaces, Unicode, or harmless `.` components.
3. **Staging/XDG preflight** — `DESTDIR` and active XDG config paths are checked
   before building or writing; purge validates both recursive-removal roots
   before deleting installed files.
4. **Root staging semantics** — an explicitly supplied `DESTDIR=/` remains a
   staged install, so cache refresh and host-PATH advice are not run by mistake.
5. **Unambiguous options** — explicit empty `--prefix`, `--bin-dir`, and
   `--binary` arguments fail instead of silently selecting a default.
6. **Build-free packaging** — `--binary PATH` installs a release/CI artifact
   through the same asset, desktop, config, and DESTDIR pipeline without Cargo
   or Nix.
7. **Pinned prebuilt source** — symlink input is rejected and the opened
   descriptor's device/inode is checked before copying through `/proc/self/fd`.
8. **Atomic executable replacement** — the binary is staged beside its target,
   mode `0755` is applied before same-filesystem rename; before that commit
   point EXIT cleanup removes the temp and leaves the prior executable intact.
9. **Portable desktop command** — `Exec` and `TryExec` now use their distinct
   escaping layers, preserve action arguments, reject ambiguous `%`/`=` paths,
   and atomically replace the desktop file.
10. **Configuration preservation and contract tests** — dangling config
    symlinks count as existing user configuration; the path suite covers real
    prebuilt staging, modes, hostile destination/source symlinks, escaping,
    root staging, cleanup, and uninstall symmetry.
11. **Repository-source preflight** — every support, shell, workflow, notebook,
    desktop, metadata, icon, and optional config source is checked before a
    build or destination mutation.
12. **Unambiguous artifact mode** — an explicit build backend can no longer be
    combined with `--binary`, avoiding an option that appears honored but is
    silently irrelevant.
13. **Non-empty artifact promise** — the pinned descriptor must contain at
    least one byte, and the contract proves rejection leaves the old target.
14. **Atomic support tool** — `forge-support-bundle` now uses a mode-0755
    same-directory temp and rename commit.
15. **Atomic shell integrations** — each documented shell file is committed
    independently with explicit public mode instead of copied in place.
16. **Frozen workflow manifest** — exactly the six shipped workflows are
    preflighted and atomically installed, with no source-tree glob drift.
17. **Atomic notebook asset** — the welcome notebook follows the same
    temp/rename boundary and cannot be partially replaced.
18. **Desktop structure contract** — at least one canonical `Exec`, exactly one
    canonical `TryExec`, and no alternate command line are required before the
    generated entry is renamed into place.
19. **Atomic public metadata/icons** — AppStream, SVG, and both PNG sizes retain
    mode 0644 and replace hostile destination symlinks without following them.
20. **True config no-clobber** — first-run config is staged beside the target
    and published by atomic hard-link create-new, not check-then-copy.
21. **Concurrent config winner preservation** — an injected `ln` race proves a
    competing creator wins, is retained verbatim, and leaves no private temp.
22. **Packaging ancestor boundary** — a non-root DESTDIR is lexically normalized
    before install/uninstall inspect its full existing component chain; disguised
    root links and recursive purge roots fail before ordinary files, while host
    prefixes remain compatible. This is not a concurrent-mutation guarantee.
23. **Unset-PATH safety** — post-install advice treats a missing PATH as empty
    rather than aborting under `set -u`.
24. **Single remote execution gate** — host target, user, shell, session,
    artifact, visual formatting, per-field bytes, and total argv bytes are
    checked together.
25. **Structured SSH option semantics** — `ssh_args` accepts OpenSSH option
    operands but rejects a second destination or premature `--`.
26. **Final argv revalidation** — fresh connections, restore, and reconnect use
    the checked builder immediately before a terminal spawn.
27. **128-profile activation gate** — indexed actions and name-based workspace
    restore fail closed at and above 128, even if a runtime-mutated vector is
    longer.
28. **Remote-filesystem spawn gate** — list/stat/cat/put/tar/untar all build
    through a checked probe argv; invalid runtime objects never reach spawn.
29. **Bounded safe remote UI** — picker and context rows use safe inline labels
    and cap executable rows without mutating configuration.
30. **Consumer regression evidence** — config and remote-fs tests exercise
    spoofing, semantic argv confusion, high indexes, and pre-spawn rejection.
31. **Search/filter state identity** — exact cross-block occurrence jumps and
    retained-query rebuilds fail closed when a card render changes; filtered
    zero-height cards stay absent from viewport, virtualization, and marker
    geometry, while bookmark mutations reconcile the active filter. Cargo and
    Nix consume the same published hardened-core revision.
32. **Live density propagation** — `block_compact` reloads update existing
    finished cards and the live cell in place, then perform one layout and PTY
    geometry synchronization per affected pane.
33. **Fresh bounded branch chips** — repeated cards share a 64-entry
    `cwd → HEAD` locator LRU and reread HEAD safely for every card, so branch
    switches are immediate; only negative lookups live for 200 milliseconds.
34. **Nonblocking safe-mode feedback** — memory-only setting changes use one
    deduplicated toast instead of an alert dialog that interrupts the settings
    workflow.
35. **Pinned local and display gates** — verification and security targets
    enter the flake toolchain, while explicit GTK/VTE tests share an isolated
    D-Bus/Xvfb runner between CI, `make verify`, and `make test-display`.
36. **Single-snapshot OSC ownership** — prompt-start and command-end each
    sample the PTY foreground owner once, rejecting foreign child-process C/D
    markers without a second probe racing to a different answer.
37. **Composable result states** — outcome owns the stripe/wash, selection owns
    its accent ring, hover owns elevation, and compound failed-selection rules
    retain all three signals at once.
38. **Recoverable Block first use** — after the RawFallback grace period, direct
    interactive bash/zsh/fish/pwsh panes show a docked, copyable integration fix;
    jsh, one-shot, remote and wrapped argv stay silent, while a late OSC marker
    removes the notice in place.
39. **Visible lifecycle provenance** — recovered, inferred and incomplete
    completions wear a dedicated accessible header chip across live, history and
    undo rebuilds; healthy and background records remain uncluttered.
40. **Truthful quick actions** — command copy, output copy and prompt insertion
    use three distinct semantic icons instead of two identical copy glyphs and a
    misleading rerun glyph.
41. **Selection-owned re-run refusal** — Ctrl+Enter remains consumed while a
    Block selection exists even when execution is refused, and history commands
    rewritten by control/paste-marker sanitization stay insert-only.
42. **One-shot Block orientation** — an empty Block pane exposes card selection,
    context actions, and cross-block search without measuring or intercepting the
    live surface; a completion or restored history dismisses it permanently,
    while Unified/VTE and inline-notice ownership remain untouched.
43. **Bounded card-shell ownership** — every WidgetPool release tears down the
    old VTE subtree, controllers and tooltip before either pooling or dropping
    the shell, keeping evicted scrollback and stale callbacks inside the same
    completed-block memory boundary on clear, retention and pool-full paths.
44. **Capability-shaped selection affordance** — the selected-card hint exposes
    direct run only for one byte-identical foreground command, while multi,
    background and sanitized selections retain only the safe actions they can
    honor; destructive Delete remains available and undoable but is deliberately
    not advertised, and the same key surface survives finished-header focus.
45. **Verified history execution** — keyboard and context-menu re-run share the
    settled-anchor, empty-suffix, clean-editor, no-Agent and foreground-shell
    proof boundary; every refusal is consumed before the live VTE can submit a
    different line, then the command is inserted first and CR is admitted only
    after VTE renders the exact stable text.
46. **No hidden widget state across lifetimes** — pooled card shells restore
    visibility after filter/alt-screen hiding, while a dismissed shell-integration
    notice leaves only weak handles in its late-marker watch instead of retaining
    the removed GTK subtree.
47. **Selection-key truth boundary** — a visible selection owns plain Enter as
    well as Ctrl+Enter; a busy, dirty, unsafe or unsupported recall now rings and
    stops instead of submitting unrelated live-editor contents, and its hint says
    up front that prompt readiness is required.
48. **Lossless batch recall and accessible chrome** — multi-card recall refuses
    shells whose lack of bracketed paste would silently keep only the first
    command, while unmodified Return/Space continue to activate a focused GTK
    header button and only the explicit Ctrl+Enter chord reaches Block re-run.
49. **Surface-aware orientation** — first-use guidance suspends for alternate-
    screen ownership and returns afterward, so the overlay can never cover the
    first full-screen TUI merely because no command card has completed yet.
50. **Trusted integration repair dismissal** — rejected lifecycle markers cannot
    latch the shell-integration notice as healthy, and one-shot or rc-bypassing
    bash/zsh/fish/PowerShell argv receive no default-profile instruction that
    cannot repair their current session.
51. **Input-aware, truthful Block guidance** — first accepted human input retires
    the one-shot orientation overlay before it can cover a long initial command;
    selection hints report the selected count and distinguish recall from recall
    all without claiming prompt readiness, while refused Enter and Ctrl+Enter
    briefly expose the actual reason before restoring the available actions.
52. **Verified history insertion** — every Block recall entry point now proves
    the live editor is visibly empty at the settled PromptEnd anchor before it
    writes `Ctrl+U` or history bytes; dirty shadows, moved cursors, unknown
    suffixes and in-flight reviewed submissions fail with zero PTY output, and
    context-menu sensitivity comes from that same proof.
53. **Generation-owned selection feedback** — repeated refusal flashes refresh
    their full lifetime and only the newest status can restore the steady action
    legend; faded quick actions also leave GTK pointer targeting, eliminating a
    transparent header dead zone for touch and no-hover input.

Verification: `bash scripts/test-install-paths.sh`, `bash -n
scripts/{install,uninstall,test-install-paths}.sh`, and the repository-wide Rust
quality gates listed in `README.md`.

54. **Reversible workflow arguments** — every parameter row exposes **Reset**,
    backed by the shared `ArgsForm::clear` contract rather than by assigning an
    empty string. A defaulted row returns to its declared value; an undefaulted
    row becomes genuinely unset and is named by the existing required-value
    hint. The app-level regression pins both branches.

55. **One locked security graph** — the local security entry point now passes
    `--locked` to cargo-deny and names `Cargo.lock` explicitly for cargo-audit,
    matching CI and the already-locked metadata/tree checks. A security run can
    no longer resolve or inspect a dependency graph other than the one being
    shipped.

56. **Reset survives GTK signal echo** — resetting an undefaulted workflow row
    now suppresses the synchronous `EntryRow::changed` echo while its widget is
    updated. The shared form therefore stays genuinely `Unset` instead of being
    immediately rewritten as `Supplied("")`; ordinary user edits still cross
    the callback. A mutation-sensitive app-level test covers both branches.

57. **Smaller direct dependency contract** — unused direct edges for
    `once_cell`, `bytecheck`, and `gdk4` leave the app manifest. The crates stay
    transitively locked where core, rkyv, and GTK need them; GTK's `v4_14`
    feature already propagates the matching GDK API level. Forge no longer
    claims three APIs that no source, test, example, or build script imports.

58. **Lock-exact Flatpak source graph** — the committed offline source manifest
    is regenerated from the shipping `Cargo.lock`, including jagent at
    `ab7552d`, jterm_core at `f60c507`, and every updated crates.io checksum.
    The documented maintenance boundary now forbids hand edits and points to
    the same pinned generator and hash-locked Python environment that CI uses
    for its byte-for-byte gate.

59. **One Flatpak source-generation entry point** — local maintenance and CI
    now share a documented `--check`/`--update` script instead of duplicating a
    fragile command sequence in workflow YAML. The script owns the generator
    commit, verifies its SHA-256 before execution, installs only hash-locked
    Python wheels, updates atomically, and can retain CI's mismatch artifact.

60. **One fail-closed security entry point** — local `--all` and CI's
    `--policy`, `--audit`, and `--shell` modes now share the same implementation.
    Both committed lockfiles are proven locked and audited; cargo-audit warnings
    are errors, so a future unsound, unmaintained, notice, or yanked advisory
    cannot leave a green job. Shell discovery also owns Bash parsing as well as
    ShellCheck, eliminating the last duplicated workflow logic.

61. **Fixed and exhaustive validation baseline** — the last moving
    `ubuntu-latest` job now names Ubuntu 24.04 like the rest of CI. Main,
    prototype, release, and AI acceptance test commands continue after an
    individual target failure, and every main-crate command includes all
    targets, so one early failure or omitted example cannot hide another.

62. **XDG-safe default asset discovery** — a custom `XDG_DATA_HOME` no longer
    hides the workflow examples and welcome Notebook installed by the
    no-argument installer under `~/.local/share/forge`. One deduplicated,
    absolute compatibility tier sits after active user data and before system
    data for both consumers; explicit non-default prefixes retain their
    documented environment overrides.

63. **Loader-complete workflow packaging** — the native installer discovers
    every bundled `.toml`, `.yaml`, and `.yml` example accepted by the shared
    loader instead of maintaining a six-name copy. The real DESTDIR contract
    byte-compares and mode-checks the complete candidate set, proves
    `--no-desktop` still ships it, and makes the deliberately narrow uninstaller
    preserve adjacent user workflows while removing every owned example.

64. **One workflow asset boundary for every release channel** — Nix, Flatpak,
    the relocatable archive builder, and that archive's installer now invoke
    one bounded helper that copies all loader-supported extensions atomically.
    A fixture-binary regression really packages, extracts, installs, and
    uninstalls the archive, byte-checking the complete library and preserving an
    adjacent user workflow; Flatpak also rebuilds when the helper or examples
    change.

65. **Symmetric release-bundle uninstall defaults** — the extracted
    `uninstall.sh` now injects the same `~/.local` prefix that its sibling
    installer owns, then forwards explicit user overrides to the hardened
    shared uninstaller. The archive E2E calls the documented no-argument path
    and proves both the binary and support tool disappear along with owned
    assets, while configuration and adjacent user workflows remain.

66. **Race-free release configuration ownership** — first-run bundle install
    stages a private `0600` config beside its destination and publishes with an
    atomic hard link. Existing files, dangling symlinks, and a writer that wins
    after the initial check are preserved; controlled-link and symlink E2Es
    prove both branches and require temporary cleanup.

67. **Atomic release executable upgrades** — the bundle preflights both
    executable sources, stages each beside its destination, and commits with
    `mv -T`. Hostile final symlinks are replaced without touching their targets;
    a signal-injected E2E kills installation between stage and rename and proves
    the prior binary survives byte-for-byte with no temporary residue.

68. **Atomic, path-safe release desktop entry** — the release installer
    validates its template and executable path before touching an installed
    binary, escapes Desktop Entry `Exec`/`TryExec` values for non-standard HOME
    paths, counts the fields it rewrites, and publishes from a random adjacent
    temporary. E2Es cover a spaced HOME, invalid metacharacter preflight, and a
    hostile final symlink whose outside target must remain unchanged.

69. **Preflighted atomic release resources** — every metainfo, icon, shell,
    workflow, Notebook, and documentation source is verified before executable
    replacement; public destinations use adjacent `0644` staging and rename.
    The installed docs now retain the archive's config example and both license
    texts, with symmetric uninstall ownership. The E2E byte-compares and
    mode-checks the whole set, then proves a symlinked source fails before an
    existing binary changes.
