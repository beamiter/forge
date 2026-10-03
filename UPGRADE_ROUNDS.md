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

70. **Lifecycle-bound journal output on core `9f94f77`** — the repin (core,
    the direct `jagent` pin, `Cargo.lock`, `deny.toml`, `flake.nix`) lands with
    `CompletedExecution`'s bare id replaced by an `ExecutionLifecycle` minted
    only from a complete `C`-mark envelope. `PendingCommandMeta` captures it at
    `C`, a mismatching `D` clears it with the id, and both provenance filters
    from the old predicate now apply to the token's own id: the per-pane shell
    secret never reaches a durable shared file, and a non-`jsh` id never spends
    a writer slot. Tests cover each of the four identity slots individually,
    a `D` packet that tries to mint or replace the token, and a complete
    envelope keyed by the pane secret. Core's two new fixture affordances are
    taken up in the same round: the correction card's verified direct-run
    branch is covered for the first time, and the probe reader thread's name is
    checked through the engine's accessor against a literal.

71. **Config edits that survive the watcher** — a dirty epoch counted on the
    GTK thread against an atomic mark the persistence worker raises with
    `fetch_max`, a Skip/Apply/Conflict decision over it and the file revision,
    and a conflict dialog whose default and Escape both keep the unsaved edit.
    The destructive answer cancels both debounce generations and the queued
    font sweep first. The 200 ms/250 ms race is covered as state transitions,
    with no sleeping.

72. **Config persistence off the UI thread's lock** — the revision slot is read
    and published inside the advisory file lock and held only for those moves,
    so the two-second lock spin and every fsync happen outside it; writers stay
    serialized by the lock that actually orders them. The regression holds the
    slot and proves the save reaches the file lock anyway, through the existing
    read-only lock probe; it fails in two seconds against the old ordering.

73. **A reload that cannot be trusted changes nothing** — the second read's
    recorded error and its revision are both checked against the revision this
    reload validated before any setting is replaced, so a failed or raced read
    no longer resets theme, keybindings and remote hosts to defaults silently.

74. **Logarithmic Block viewport resolution** — a Fenwick prefix index over the
    card document, both viewport edges reduced to one descent, point-patched by
    the visibility appliers and rebuilt only on a length change or an explicit
    stale mark from the non-frame writers. Visibility now touches the union of
    the outgoing and incoming sets instead of every finished card. A counting
    test bounds the probes at 1k/10k/100k, a property test checks the descent
    against the from-zero walk it replaced over generated documents, and the
    microbenchmark reports 102 µs against 52 ns at 50 000 cards.

75. **Git metadata that never blocks and never idles** — the Agent panel's
    probe and prompt assembly moved into the request thread, and the strip's
    cache gained a TTL plus explicit invalidation at command completion, so an
    idle window stops forking one `git status` per second.

76. **No exact record for a self-contradictory packet** — a `C` mark carrying
    both a command prefix and `cmd_truncated=1` can no longer resolve to
    `ShellReported`, so the flag that unlocks agent replay is never set for
    half a command line.

77. **Private staging for remote directory downloads** — extraction moved out
    of the destination's parent into a `0700` process-owned directory, with a
    single-expected-top-level check (name and real directory, not a link)
    before an atomic publish, and a `Drop` guard that removes staging on every
    path. The regression covers the honest tree, a smuggled sibling dotfile, a
    renamed top level, a symlinked one, and an empty archive.

78. **A Block-history failure that waits for an answer** — the one fail-closed
    persistence operation routes to a persistent bar with an explicit Retry
    instead of an eight-second toast, and Retry reloads the panes whose load
    failed while re-saving the rest.

79. **Zone history through the shared bounded reader** — `stat`-then-read
    replaced by `snapshot_file::read_bounded`, which enforces the ceiling on
    the descriptor it reads and refuses fifos, devices, hard-linked files and
    files another user can write.


80. **Review follow-ups on rounds 74 and 75** — three defects the adversarial
    pass found in the same working tree, fixed in place. The differential
    visibility pass (74) only ever visited the union of the outgoing and
    incoming visible sets, which is exact for a scroll tick and wrong for
    everything else: a card mounted by a finished command or an undo arrives
    un-virtualized and is named by neither set, and the filter and undo paths
    hand the pass an emptied set on purpose. Either way the cards involved
    stayed laid out in full at heights the document no longer recorded. The
    obligation to sweep every card now travels on `BlockDocumentIndex`, which
    already had to rebuild for exactly those reasons, so the per-frame case
    stays differential and nothing else does. The pass moved behind a
    `VirtualizableCard` seam so that transition is finally pinned by tests that
    run without a display. In (75), a finished command invalidated the *focused*
    pane's directory rather than the pane that ran it, so a command in a
    background tab or a sibling split left its own pane serving a stale branch
    for the whole 30 s TTL; the handler now resolves the directory from the view
    it is installed on. Also in (75), the probe worker cleared the invalidation
    flag unconditionally, discarding any report that arrived while Git was
    running — two commands in a row on a slow checkout was enough — so a probe
    now answers only for the generation it was queued at.

81. **A report about a directory Git has not answered for yet** — the
    generation counter added in (80) lived on the cache entry, and the first
    probe of a directory has no entry until the worker creates one, so an
    `invalidate` racing that probe was still dropped. A pane that has just
    opened or just changed directory is always in that window, and a cold
    checkout is the slowest probe there is, which made the likeliest instance
    of the race the one still losing its report for the full 30 s TTL.
    Reporting now creates an answerless entry to hold the count, `refreshed_at`
    became optional so such an entry cannot claim a probe it never had, and one
    bounded insert is shared by both writers so a report cannot grow the cache
    past its ceiling.

82. **The ASCII organism moves into `jterm_core`** — `organism`,
    `organism_memory` and `organism_attention` (9,004 lines and their 119
    tests) are deleted from forge and consumed from core at pin `fa256d6`;
    forge's copy is the one that went up, so the reducer, the memory schema and
    the attention arbiter behave exactly as before. The 20 `crate::organism*`
    call sites were rewritten to `jterm_core::organism*` rather than shimmed,
    because forge keeps a local `ui::organism` and a same-named alias for a core
    module would have made the two indistinguishable at the call site.
    `src/ui/organism.rs` is unchanged. Two narrowings came with the move:
    `OrganismMemory::load` now takes the app's path (core will not guess one,
    because a wrong path there fails silently), and `CircadianProfile` is built
    from a window start instead of a raw bucket mask, so the values whose
    `session_day` used to panic are no longer expressible. The one step no
    compiler error catches is the durability lane: `app::run` registers
    `OrganismLane` — forge's persistence worker — beside `identity::init` and
    before the first load, and two tests pin it, one driving a real write
    through the registered lane to disk and one reading forge's own source to
    prove startup still installs it. Without the registration nothing breaks
    and nothing is lost; core falls back to a writer thread of its own, outside
    forge's coalescing, admission bound and shutdown drain, and says so only in
    a log line.

83. **Metadata-only palette scan budget** — empty-query cross-block browse with
    active metadata filters now shares the 8 MiB / 48 ms scan budget and sets
    `scan_incomplete` when the walk stops early instead of silently omitting
    later records.

84. **Pattern-search scan budget regression** — non-empty cross-block palette
    queries already consumed the shared byte budget while walking command and
    output lines; a focused unit regression now proves the pattern path sets
    `scan_incomplete` when the budget stops mid-record instead of returning a
    silently truncated hit list.

85. **Hit-cap versus scan-incomplete contract** — reaching `max_hits` stops the
    walk early but must not set `scan_incomplete`; only byte/time budget
    exhaustion may disclose an incomplete scan. A focused regression pins that
    distinction for pattern search.
86. **Metadata filter hit-cap contract** — the metadata-only palette path now
    mirrors the pattern-search rule: stopping at `max_hits` must not set
    `scan_incomplete`; only scan-budget exhaustion may disclose truncation.
87. **Cross-block hit cap across surfaces** — pattern search stops once
    `max_hits` is reached on the command surface instead of spending the
    remainder of the cap on output from the same record.
88. **Excluded records spend pattern-search budget** — filter-excluded records
    consume the shared scan byte budget during pattern search, matching
    metadata browse, so a long filtered walk discloses `scan_incomplete`
    instead of examining unbounded history for free.
89. **Organism vigil-tier UI contract** — Full-motion bridge selection mirrors
    `VisualTransition::between` for the vigil-tier arcs (second-failure settle
    and idle Failure→Stuck / Recovery→Cautious); Calm/Static still snap.
    Behavior itself lands with core `fbfcafa` (pending push/repin from
    `33093da`).
90. **Persisted earlier-output notice** — `BlockData.output_notice` carries the
    finished-card loss notice (including "Earlier output not retained") across
    history save/restore and clear undo, matching anvil's round-50
    `output_head_dropped` contract. Pre-notice frames still decode with the
    notice off; `history_keeps_the_output_notice_and_reads_the_schema_before_it`
    pins both directions.
91. **Resumable cross-block search idle continuation** — budget stops return a
    `CrossBlockSearchCursor`; the dialog runs `glib::idle_add_local` slices
    cancelled by search generation. Unit regressions pin resume cursors and the
    generation cancel helper.
92. **Organism error-hold heal settle UI contract** — Full motion mirrors
    SitNearError/InspectError→Recovery/Cautious/Stuck and Cautious→Recovery
    once core recognizes those bridges (pending push/repin). Calm/Static snap.
93. **Shared finished-block output notice** — notice strings and known-set parse
    come from `jterm_core::output_notice` (pending core push/repin). History
    still stores `Option<String>` behind the known-set gate.
94. **Organism celebrate-hold relapse UI contract** — Full motion mirrors
    Celebrate/CelebrateBig→Failure/Stuck once core recognizes those bridges
    (pending push/repin). Calm/Static snap.
95. **Organism failure-push + error-hold success UI contract** — Full motion
    mirrors GuardFailure/Stuck→RestAfterPush and
    Inspect/SitNear→Celebrate{,Big} once core recognizes those bridges
    (pending push/repin). Calm/Static snap.
96. **Organism celebrate-hold push UI contract** — Full motion mirrors
    Celebrate/CelebrateBig→RestAfterPush once core recognizes those bridges
    (pending push/repin). Calm/Static snap.
97. **Organism WatchSettled fail + idle settles UI contract** — Full motion
    mirrors WatchSettled→Inspect/SitNear and
    Celebrate{,Big}/RestAfterPush→Idle once core recognizes those bridges
    (pending push/repin). Calm/Static snap.
98. **Organism WatchCommand/WatchAgent finish UI contract** — Full motion
    mirrors WatchCommand→Celebrate{,Big}/Inspect/SitNear/RestAfterPush and
    WatchAgent→Celebrate/Inspect/SitNear once core recognizes those bridges
    (pending push/repin). Calm/Static snap.
99. **Shared CrossBlockSearchCursor** — resume cursor / mid-record /
    generation-current predicate / cross-block budget constants come from
    `jterm_core::cross_block_search` (pending core push/repin). GTK idle and
    `CrossBlockSearchReport`/`FindScanBudget` stay local.
100. **Organism UnknownOutcome settle/overwrite UI contract** — Full motion
    mirrors UnknownOutcome→Idle and UnknownOutcome→InspectError once core
    recognizes those bridges (pending push/repin). GlanceAside stays live-only.
    Calm/Static snap.
101. **Organism UnknownOutcome success/sit overwrite UI contract** — Full motion
    mirrors UnknownOutcome→Celebrate{,Big} and UnknownOutcome→SitNearError once
    core recognizes those bridges (pending push/repin). Calm/Static snap.
102. **Organism WatchAgent/WatchSettled→RestAfterPush UI contract** — Full motion
    mirrors WatchAgent→RestAfterPush and WatchSettled→RestAfterPush once core
    recognizes those bridges (pending push/repin). Calm/Static snap.
103. **UnknownOutcome→Guard* Full-motion UI** — Full motion mirrors
     UnknownOutcome→GuardFailure/Stuck/Recovery/Cautious vigil settles once core
     recognizes those bridges; Find overlay scan caps come from core
     `FIND_OVERLAY_SCAN_*`.

104. **Find-overlay / finished-output test pins** — live-dump and aggregate
     FindScanBudget regressions use `FIND_OVERLAY_SCAN_{BYTE,TIME}_LIMIT`;
     finished-output notice tests pin `FINISHED_OUTPUT_*`. TODO clarifies
     CROSS_BLOCK_* vs FIND_OVERLAY_* budgets.

105. **Ambient VisualTransition N/A probe** — UI ambient pose
     (Explore/Sleep/Approach/Idle) does not route through
     `VisualTransition::between`; no ambient snap bridges to add.

106. **Close idle cross-block continue TODO** — mark the cancellable
     cross-block scan P2 done in the round-91 idle-continuation form;
     backfill ledger rounds 104–105. Live VTE↔PCRE2 alignment stays open.

107. **FindScanBudget constructor semantic pin** — palette
     `FindScanBudget::for_cross_block` uses shared `CROSS_BLOCK_SCAN_*`
     (8 MiB / 48 ms); live overlay `FindScanBudget::new` uses
     `FIND_OVERLAY_SCAN_*` (4 MiB / 12 ms). Unit test pins both constructors.

108. **Full-motion semantic_bridges catch-up** — Full-motion UI contract list
     mirrors every core `VisualTransition::between` pair (64 bridges), closing
     gaps left by the staged UnknownOutcome / Watch* / vigil rounds.

109. **WatchAgent→UnknownOutcome None + agent Celebrate UI** — Full motion
     pins WatchAgent→UnknownOutcome as None; agent-driven recovery stays
     Celebrate (never CelebrateBig), matching core quiet-nod contract.

110. **WatchCommand→UnknownOutcome None UI** — Full motion pins
     WatchCommand→UnknownOutcome as None (missing exit status snaps), pairing
     core intentional None beside WatchAgent→UnknownOutcome.

111. **Core tip STAGE_PREFIXES strace/scriptlive (len 60)** — path-patched
     local `jterm_core` peels `strace` / `scriptlive` (STAGE_PREFIXES 58→60)
     with `classify_command` see-through; Cargo manifests stay on the
     published pin (pending push/repin).

112. **FindScanBudget / Options / Report from core** — re-export
     `FindScanBudget`, `CrossBlockSearchOptions` / `CrossBlockSearchScope`, and
     hit-generic `CrossBlockSearchReport<CrossBlockHit>` from path-patched
     core; `CrossBlockHit` stays local (no anvil palette-chrome fields). GTK
     idle stays here. Pairs anvil round 76.

113. **Guard*/Watch*/Celebrate*/Glance intentional None UI** — Full motion
     pins WatchSettled→UnknownOutcome, Watch*→Guard*, Guard*→Celebrate*,
     Celebrate*→Watch*, Idle/Rest→Guard*, and GlanceAside as source/target
     (pairs anvil round 77; core audit wave).

114. **Core tip STAGE_PREFIXES systemd-cat/aa-exec (len 62)** — path-patched
     local `jterm_core` peels `systemd-cat` / `aa-exec` (STAGE_PREFIXES 60→62)
     with `classify_command` see-through (and full STAGE membership pin);
     Cargo manifests stay on the published pin (pending push/repin).

115. **Anvil Block-history sticky catch-up note** — anvil rounds 75 (Retry
     foundation), 78 (sticky chrome), 80 (labeled enqueue), and 81 (toast
     partition + ReloadFirst pins + Explicit Clear bypass of Failed-load)
     close sticky parity with forge round 78. Forge already routes
     ExplicitReplace past Failed-load in the worker; history_notice docs now
     name that Clear contract so the two trees stay aligned. No forge
     behavior change.

116. **Ambient / Typing VisualTransition N/A pin** — Full-motion UI pins that
     Idle/Explore/Sleep/Approach disposition exchanges and Typing-surface entry
     onto WatchCommand stay `VisualTransition::between` None (pairs core ambient
     N/A pin and anvil round 82). Round 105 documented Ambient N/A; this lands
     the regression.

117. **Core tip STAGE_PREFIXES systemd-inhibit (len 63)** — path-patched local
     `jterm_core` peels `systemd-inhibit` (STAGE_PREFIXES 62→63) with
     `classify_command` see-through; Cargo manifests stay on the published pin
     (pending push/repin). Catch-up after ambient round 116. Pairs anvil round 83.

118. **Core tip STAGE_PREFIXES systemd-socket-activate (len 64)** — path-patched
     local `jterm_core` peels `systemd-socket-activate` (STAGE_PREFIXES 63→64)
     with `classify_command` see-through and membership pin; Cargo manifests
     stay on the published pin (pending push/repin). Pairs anvil round 84.

119. **Clear-vigil Idle Full-motion UI contract** — Full motion mirrors
     core Inspect/SitNear→Idle and Guard*→Idle bridges (`between()` 64→70).
     Pairs anvil round 85; core clear-vigil Idle survey wave.

120. **Error-hold/Unknown→Rest + Watch*→Idle UI contract** — Full motion
     mirrors core Inspect/SitNear/Unknown→RestAfterPush and Watch*→Idle
     (`between()` 70→76). STAGE docs rounds 117–118 stay membership-only
     (len 63/64); the 70 count was round 119, this round catches 76.
     Pairs anvil round 86.

121. **Find overlay continue bookmark-empty status** — `pending_scan_continue`
     idle slices reuse `overlay_scan_status` (and close bumps
     `search_generation`) so a finished Bookmarked scan that added no hits
     keeps the bookmark-empty copy. Pairs anvil round 87.

122. **Core tip STAGE_PREFIXES daemonize/setlock/s6-setuidgid (len 67)** —
     path-patched local `jterm_core` peels `daemonize` / `setlock` /
     `s6-setuidgid` (STAGE_PREFIXES 64→67) with `classify_command` see-through
     (including `--` before the s6 account) and leftover pin for `s6-envdir` /
     `s6-log` / runit helpers; Cargo manifests stay on the published pin
     (pending push/repin). Pairs anvil round 88.

123. **Celebrate*/Rest/GuardRecovery hold-overwrite UI contract** — Full motion
     mirrors core Celebrate*/RestAfterPush/GuardRecovery finish overwrites
     (`between()` 76→90). STAGE docs round 122 stays membership-only (len 67);
     the 76 count was round 120, this round catches 90. Rest/GuardRecovery→Watch*
     stay None. Pairs anvil round 89; core hold-overwrite survey wave.

124. **Find stale-bookmark empty-reason pin** — bookmark ids absent from the
     current retained records list are `NoRetainedBookmarks`, not a metadata
     or query miss. Pairs anvil round 90 (empty-query browser `None`).

125. **GuardFailure/Stuck/Cautious→error-hold None UI** — Full motion pins
     Failure/Stuck/Cautious→Inspect/Sit/Unknown as None beside Recovery's
     finish-overwrite bridges. Pairs anvil round 91.

126. **Core tip fail-closed deepen (STAGE nest + transparency; len 68)** —
     path-patched local `jterm_core` / `jagent`: timeout/nice nests around
     `daemonize`/`setlock`/`s6-setuidgid`, jagent peels optional `--` before
     the setuidgid account, membership + DISPATCHES pin `len == 68` (wave-23
     triple + `gnome-session-inhibit`), and transparency partition documents
     STAGE names outside `select_execution_wrappers_mode` plus intentional
     PIPE-only `unshare`/`nsenter`. Cargo manifests stay on the published pin
     (pending push/repin). Pairs anvil round 92.


127. **Find empty-query browser pin** — Bookmarked empty-reason stays `None`
     when an empty query still has eligible scoped text (browser, not
     QueryNoMatches). Dedicated pin beside round 124 stale ids. Pairs anvil
     round 93 (stale ids) / 90 (empty-query).

128. **Error/unknown→Watch* None UI** — Full motion pins Inspect/SitNear/
     Unknown→Watch* as None beside Celebrate*/Rest/Recovery→Watch*. Pairs anvil
     round 94; core `error_and_unknown_holds_never_bridge_to_watch_poses`.

129. **Find mixed stale+live bookmark empty-reason** — stale ids beside a live
     bookmark do not collapse to `NoRetainedBookmarks`; empty query stays a
     browser and a live query miss stays `QueryNoMatches`. Pairs anvil round 96.

130. **Core tip STAGE_PREFIXES uclampset/gamemoderun (len 70)** — path-patched
     local core teaches util-linux util clamp + GameMode env launcher peels;
     GuardFailure/Stuck/Cautious→Celebrate* None survey pin; `between()` stays
     90. Manifests stay on published pins. Pairs anvil round 97.

131. **Sticky near-miss Block-history labels** — padded / cased / truncated
     `"Save Block history"` strings stay on the toast surface (exact worker
     label only). Pairs anvil round 95.

132. **InspectError→SitNearError Full-motion (90→91)** — Full-motion
     `semantic_bridges` mirrors core Inspect hold second-failure overwrite.
     Pairs anvil round 98.

133. **Failure/Stuck/Cautious→Watch* + tier overwrite None UI** — Full motion
     pins Failure/Stuck/Cautious→Watch* beside Rest/Recovery→Watch*, and
     SitNear→Inspect / Celebrate↔CelebrateBig as None. Pairs anvil round 99.

134. **File-tree permission vs missing scan errors** — public Error-row copy
     distinguishes PermissionDenied from NotFound; empty directories stay
     success listings. Closes TODO.md P2 file-tree Retry/toast leftover.
     Pin `directory_scan_errors_distinguish_permission_from_missing`. Pairs
     anvil round 100. STAGE 70 tip already ledgered at round 130.

135. **Sticky near-miss whitespace/invisible labels** — tab / CR / NBSP / ZWSP /
     BOM / VT variants of `"Save Block history"` stay toast-only. Catch-up
     beside anvil rounds 100–101.
136. **File-op permission vs missing** — `public_file_operation_error_message`
     keeps PermissionDenied distinct from NotFound beside round 134 directory
     scan. Pairs anvil 102 FS copy pin.
137. **Find MetadataMismatch with stale extras** — live bookmark failing
     metadata filters stays MetadataMismatch when stale ids remain. Pairs
     anvil 103.
138. **Ambient→Watch* None UI** — Idle/Explore/Sleep/Approach never bridge to
     WatchCommand/Agent/Settled under Full motion (Watch*→Idle animates).
     Pairs anvil 104.

139. **Ambient→Guard*/Celebrate* None UI** — Explore/Sleep/Approach never
     bridge to GuardFailure/Stuck/Recovery/Cautious or Celebrate/CelebrateBig
     under Full motion (Idle/Rest→Guard* already pinned). Pairs anvil 105.
140. **Find NoRetainedTextInScope with stale extras** — Output-scope live
     bookmark without retained output stays NoRetainedTextInScope when stale
     ids remain. Pairs anvil 106.
141. **Sticky near-miss soft-hyphen/WJ/bidi labels** — soft hyphen / word
     joiner / LRM / ZWNJ / figure-space variants of `"Save Block history"`
     stay toast-only beside round 135. Pairs anvil 107.

142. **CelebrateBig finish arcs Full-motion UI** — Full motion pins all fifteen
     CelebrateBig inbound/outbound Some bridges and WatchAgent→CelebrateBig
     None (pairs core tip and anvil round 108).

143. **output_notice Truncated/Partly history round-trip** — Block-history
     restore keeps TextTruncated and PartlyRetained through the known-set gate
     beside EarlierNotRetained. Pairs anvil round 109 Earlier-only disk pin.

144. **Find Hit optional exit_code/duration/cwd + outcome suffix** — forge
     `CrossBlockHit` carries optional palette chrome; BackendRecordRef exposes
     cwd; palette rows show anvil-parity outcome suffixes without breaking
     ActionRow layout. Closes the Full Find Hit schema survey leftover. Pairs
     anvil round 110 / core Hit pin retarget.

145. **Sticky ReloadFirst chains labeled save** — Retry on Failed-load latches
     `retry_save_after_history_load`, reloads, then enqueues `"Save Block
     history"` on Loaded (or early sync restore). Sync load-queue Failed
     clears the latch and returns Err so the sticky bar re-shows. Pairs
     anvil 111.

146. **Find Hit chrome parity pins** — palette `hit_outcome_label` covers
     exit:148 suspended and `hit_outcome_class(None)` beside 130/137. Pairs
     anvil 112.

147. **semantic_bridges list len == 91 + CelebrateBig fifteen lockstep** —
     Full-motion contract list asserts length against core `between()` 91;
     CelebrateBig Some arcs stay fifteen; Celebrate↔CelebrateBig Nones join
     the finish-arc UI pin. Pairs anvil 113.

148. **Ambient→Inspect/Sit/Unknown/Rest None UI** — Explore/Sleep/Approach
     never bridge to error/unknown holds or RestAfterPush under Full motion.
     Pairs anvil 114 / core ambient hold/rest pin.

149. **Find Command-scope NoRetainedTextInScope + stale** — whitespace-only
     command with retained output stays NoRetainedTextInScope under Command
     scope when stale ids remain. Pairs anvil 115.

150. **Sticky line/paragraph separators + notice near-miss restore gate** —
     U+2028/U+2029 `"Save Block history"` stay toast-only; padded/cased
     Truncated/Partly/Earlier strings stay outside the known-set remount gate.
     Pairs anvil 116.

151. **Inspect/SitNear→UnknownOutcome Full-motion (91→93)** — Full-motion
     `semantic_bridges` mirrors core error-hold missing-exit overwrites.
     CelebrateBig fifteen lockstep recounts against `between()` **93**.
     Pairs anvil 117.

152. **Idle→hold/cele/rest None UI** — Idle never bridges to Inspect/Sit/
     Unknown/Celebrate{,Big}/RestAfterPush under Full motion (live finishes
     via Watch*). Pairs anvil 118 / core Idle pin.

153. **CrossBlock continue cancel scheduled-ahead + gen-0** — palette idle
     continue pins scheduled generation ahead of live and gen-0 finished
     (no resume) as cancel. Pairs anvil 119 / core cancel edge.

154. **Sticky near-miss NNBSP/MMSP/invisible-math labels** — U+202F / U+205F /
     U+2062 / U+2063 / U+2064 variants of `"Save Block history"` stay toast-only
     beside round 150. Pairs anvil 120.

155. **Find All-scope NoRetainedTextInScope + stale** — whitespace-only command
     and output stay NoRetainedTextInScope under All scope when stale ids remain.
     Pairs anvil 121.

156. **Sticky near-miss ZWJ/RLM/FA/ideo/NEL labels** — U+200D / U+200F /
     U+2061 / U+3000 / U+0085 variants of `"Save Block history"` stay toast-only
     beside round 154. Pairs anvil 122.

157. **Find Command/Output-scope QueryNoMatches + stale** — live bookmark whose
     scoped text misses the query stays QueryNoMatches under Command and Output
     when stale ids remain (All-scope query miss already pinned). Pairs anvil 123.

158. **CrossBlock cancel ahead-without-resume + wrapping finished** — scheduled
     generation ahead of live cancels even with an empty resume; wrapping
     MAX→0 bump with a finished cursor cancels the same way. Pairs anvil 124.

159. **output_notice CR/NBSP/ZWSP/BOM/soft-hyphen near-miss gate** — CR / NBSP /
     ZWSP / BOM / soft-hyphen padded Truncated/Partly/Earlier strings stay
     outside the known-set remount gate beside round 150. Pairs anvil STAGE tip.

160. **Core tip STAGE_PREFIXES openvt (len 71)** — path-patched local core
     teaches kbd `openvt` VT launcher peels (`-c`/`--console` meta) plus
     timeout/nice nest classify; Inspect/Sit→Unknown `between()` **93** already
     ledgered. Manifests stay on published pins. Pairs anvil 125.

161. **WatchSettled finish arcs Full-motion (6 Some)** — Full motion pins all
     six WatchSettled→Celebrate{,Big}/Inspect/Sit/Rest/Idle bridges and keeps
     UnknownOutcome intentional None. Pairs anvil 126 / core
     `watch_settled_finish_arcs_cover_pass_fail_rest_and_idle`.

162. **CrossBlock cancel live wrapping ahead of scheduled** — live generation
     wrapping ahead of scheduled (`0` vs `u64::MAX`) cancels with or without a
     resume — reverse of the MAX→0 schedule bump. Pairs anvil 127 / core cancel
     edge.

163. **Core tip PATH wave-30 ctl/notify leftovers** — path-patched local core
     keeps `systemctl` / `busctl` / `journalctl` / `timedatectl` / `resolvectl`
     / `systemd-notify` / `systemd-mount` / `chvt` / `aa-status` out of STAGE
     beside openvt 71. Manifests stay on published pins. Pairs anvil 128.

164. **Sticky near-miss punct/thin/hair/ALM/bidi-isolate labels** — U+2008 /
     U+2009 / U+200A / U+061C / U+2066–U+2069 variants of `"Save Block history"`
     stay toast-only beside rounds 156/154. Pairs anvil 129. Leaves 161–163 for
     WatchSettled/CrossBlock/PATH wave-30 cohort.

165. **Find Command/Output empty-query browser + stale** — empty query under
     Command and Output scopes stays `None` (browser) when stale ids remain
     beside a live scoped bookmark (All-scope empty-query already pinned).
     Pairs anvil 130.

166. **output_notice ZWJ/NNBSP/LS/NEL/ideo/FA near-miss gate** — ZWJ / NNBSP /
     line-separator / NEL / ideographic-space / function-application padded
     Truncated/Partly/Earlier strings stay outside the known-set remount gate
     beside rounds 159/150. Pairs anvil STAGE tip.

167. **Sticky near-miss en/em/quad-space + RLI/FSI labels** — U+2000–U+2006 /
     U+2067 / U+2068 variants of `"Save Block history"` stay toast-only beside
     rounds 164/156. Pairs anvil 131.

168. **Find All-scope empty-query browser + stale** — empty query under All
     scope stays `None` (browser) when stale ids remain beside a live scoped
     bookmark (Command/Output empty+stale already pinned). Pairs anvil 132.

169. **output_notice punct/quad-space/RLI near-miss gate** — punct/thin/hair /
     en/em/three-per-em / RLI-padded Truncated/Partly/Earlier strings stay
     outside the known-set remount gate beside rounds 166/159. Pairs anvil
     sticky/find tip.

170. **Sticky near-miss Hangul-filler/Braille/format-control labels** — U+115F /
     U+1160 / U+3164 / U+FFA0 / U+2800 / U+206A / U+206F variants of
     `"Save Block history"` stay toast-only beside rounds 167/164. Pairs anvil
     133.

171. **Find Command/Output whitespace-query QueryNoMatches + stale** —
     whitespace-only `" \t "` under Command and Output stays QueryNoMatches when
     stale ids remain (empty-query browser already pinned). Pairs anvil 134.

172. **output_notice Hangul-filler/Braille/format-control near-miss gate** —
     Hangul fillers / Braille blank / U+206A–U+206F padded Truncated/Partly/
     Earlier strings stay outside the known-set remount gate beside rounds
     169/166. Pairs anvil sticky tip.

173. **Watch*→Guard* Full-motion None** — Full motion pins WatchCommand/Agent/
     Settled→Guard* intentional None beside WatchSettled finish Somes. Pairs
     anvil 135 / core `watch_poses_never_bridge_to_repo_vigil_guards`.

174. **Watch*→ambient Full-motion None + semantic_bridges WatchSettled note** —
     Full motion pins Watch*→Explore/Sleep/Approach None; `semantic_bridges`
     comment notes WatchSettled finish six sit inside between() **93**. Pairs
     anvil 136 / core `watch_poses_never_bridge_to_ambient_utility`.

175. **Sticky near-miss Ogham/MVS/CGJ/VS/Khmer labels** — U+1680 / U+180E /
     U+034F / U+FE00 / U+FE0E / U+FE0F / U+17B4 / U+17B5 variants of
     "Save Block history" stay toast-only beside rounds 170/167. Pairs anvil
     137.

176. **Find All-scope whitespace-query QueryNoMatches + stale** —
     whitespace-only `" \t "` under All stays QueryNoMatches when stale ids
     remain (Command/Output whitespace already pinned). Pairs anvil 138.

177. **output_notice Ogham/MVS/CGJ/VS/Khmer near-miss gate** — Ogham space /
     MVS / CGJ / VS / Khmer-inherent padded Truncated/Partly/Earlier strings
     stay outside the known-set remount gate beside rounds 172/169. Pairs
     anvil sticky tip.

178. **Guard*→Celebrate* Full-motion None** — Full motion pins
     GuardFailure/Stuck/Recovery/Cautious→Celebrate{,Big} intentional None.
     Pairs anvil 139 / core `repo_vigil_guards_never_bridge_to_celebrate`.

179. **Celebrate*→Watch* Full-motion None** — Full motion pins
     Celebrate{,Big}→WatchCommand/Agent/Settled intentional None. Pairs anvil
     140 / core `celebrate_holds_never_bridge_to_watch_poses`.

180. **Hold/rest→ambient Full-motion None** — Full motion pins Celebrate{,Big}/
     Inspect/Sit/Unknown/Rest→Explore/Sleep/Approach intentional None. Pairs
     anvil 141 / core `hold_and_rest_poses_never_bridge_to_ambient_utility`.

181. **Guard*→ambient Full-motion None** — Full motion pins
     GuardFailure/Stuck/Recovery/Cautious→Explore/Sleep/Approach intentional
     None. Pairs anvil 142 / core `repo_vigil_guards_never_bridge_to_ambient_utility`.

182. **Watch* mode-switch Full-motion None** — Full motion pins
     WatchCommand↔WatchAgent↔WatchSettled intentional None (live SurfaceMode
     remaps). Pairs anvil 143 / core `watch_pose_mode_switches_have_no_visual_transition`.

183. **CrossBlock cancel finished at wrap gen** — palette idle continuation
     drops `MAX,MAX` finished walks (no resume) beside MAX→0 schedule bump.
     Pairs anvil 144 / core cancel edge.

184. **Sticky Retry continues after sync refusal** — `retry_block_history`
     re-shows on Err but does not break/return before later leaves retry.
     Pairs anvil 145.

185. **Sticky near-miss Mongolian FVS / mid ZWNBSP / interlinear labels** —
     U+180B–U+180D / mid U+FEFF / U+FFF9–U+FFFB variants of "Save Block history"
     stay toast-only beside rounds 175/170. Pairs anvil 146.

186. **Find Command/Output/All NBSP/ZWSP-query QueryNoMatches + stale** —
     NBSP / ZWSP-only queries stay QueryNoMatches when stale ids remain (ASCII
     whitespace already pinned). Pairs anvil 147.

187. **output_notice Mongolian FVS / mid ZWNBSP / interlinear near-miss gate** —
     FVS / mid ZWNBSP / interlinear-padded Truncated/Partly/Earlier strings stay
     outside the known-set remount gate beside rounds 177/172. Pairs anvil sticky tip.

188. **Idle/Rest→Guard* Full-motion None** — Full motion pins Idle/RestAfterPush→
     GuardFailure/Stuck/Recovery/Cautious intentional None (shipped beside
     hold/ambient pins). Pairs anvil 148 / core
     `idle_and_rest_never_bridge_to_repo_vigil_guards`.

189. **Rest/Guard*→Watch* Full-motion None** — Full motion pins RestAfterPush/
     Guard*→WatchCommand/Agent/Settled intentional None. Pairs anvil 149 / core
     `rest_and_repo_vigil_never_bridge_to_watch_poses`.

190. **Inspect/Sit/Unknown→Watch* Full-motion None** — Full motion pins
     InspectError/SitNearError/UnknownOutcome→Watch* intentional None. Pairs
     anvil 150 / core `error_and_unknown_holds_never_bridge_to_watch_poses`.

191. **GuardFailure/Stuck/Cautious→holds Full-motion None** — Full motion pins
     those vigil poses→Inspect/Sit/Unknown intentional None (Recovery alone
     animates). Pairs anvil 151 / core
     `failure_stuck_cautious_never_bridge_to_error_or_unknown_holds`.


192. **Sticky near-miss bidi embeddings / deprecated format labels** —
     U+202A–U+202E / U+206B–U+206E variants of "Save Block history" stay
     toast-only beside rounds 185/175. Pairs anvil 152.

193. **output_notice bidi embedding / deprecated-format near-miss gate** —
     U+202A–U+202E / U+206B–U+206E-padded Truncated/Partly/Earlier strings stay
     outside the known-set remount gate beside rounds 187/177. Pairs anvil sticky tip.

194. **Find WJ/figure/soft-hyphen/bidi-query QueryNoMatches + stale** —
     U+2060 / U+2007 / U+00AD / U+202A / U+202E-only queries stay
     QueryNoMatches under Command/Output/All when stale ids remain (NBSP/ZWSP
     already pinned). Pairs anvil 153.

195. **Ambient disposition exchange Full-motion None completeness** — Full
     motion pins Idle↔Sleep/Approach, Explore↔Approach, Approach↔Sleep
     intentional None beside the partial ambient table. Pairs anvil 154 /
     core `ambient_disposition_exchanges_have_no_visual_transition`.

196. **Sticky near-miss mid-range VS / Mongolian FVS4 labels** —
     U+FE01 / U+FE0D / U+180F variants of "Save Block history" stay toast-only
     beside rounds 192/185. Pairs anvil 155.

197. **output_notice mid-range VS / Mongolian FVS4 near-miss gate** —
     U+FE01 / U+FE0D / U+180F-padded Truncated/Partly/Earlier strings stay
     outside the known-set remount gate beside rounds 193/187. Pairs anvil sticky tip.

198. **Find ZWNJ/ZWJ/LRM/RLM/ALM-query QueryNoMatches + stale** —
     U+200C / U+200D / U+200E / U+200F / U+061C-only queries stay
     QueryNoMatches under Command/Output/All when stale ids remain (WJ/figure/
     bidi already pinned). Pairs anvil 156.

199. **GlanceAside↔ambient Full-motion None completeness** — Full motion pins
     GlanceAside↔Explore/Sleep/Approach intentional None beside the prior
     GlanceAside overwrite table. Pairs anvil 157 / between() 93.

200. **CrossBlock cancel near-wrap bump** — palette idle continuation drops
     MAX-1→MAX generation bumps (with or without resume) and keeps a live
     resume at matching MAX. Pairs anvil 158 / core cancel edge.

201. **CrossBlock cancel scheduled-ahead near-wrap** — scheduled MAX vs live
     MAX-1 cancels with or without resume (speculative gen / rewound live
     beside the MAX-1→MAX bump). Pairs anvil 159 / core cancel edge.

202. **GuardFailure/Stuck/Cautious→Celebrate* Full-motion None** — Full motion
     pins those vigil poses→Celebrate{,Big} intentional None (success finishes
     via Watch*; Recovery→Celebrate already covered in round 178). Pairs anvil
     160 / core `failure_stuck_cautious_never_bridge_to_celebrate_holds`.

203. **Sticky interior mid-range VS / Mongolian nirugu labels** —
     U+FE02 / U+FE0C / U+180A variants of "Save Block history" stay toast-only
     beside rounds 196/192. Pairs anvil 161.

204. **output_notice interior mid-range VS / Mongolian nirugu near-miss gate** —
     U+FE02 / U+FE0C / U+180A-padded Truncated/Partly/Earlier strings stay
     outside the known-set remount gate beside rounds 197/193. Pairs anvil sticky tip.

205. **Find Hangul/Braille/ideo/Ogham-query QueryNoMatches + stale** —
     U+115F / U+3164 / U+2800 / U+3000 / U+1680-only queries stay
     QueryNoMatches under Command/Output/All when stale ids remain (marks/WJ
     already pinned). Pairs anvil 162.

206. **WatchAgent→CelebrateBig + Watch*→Unknown Full-motion None** — Full
     motion pins WatchAgent→CelebrateBig and WatchCommand/Agent/Settled→
     UnknownOutcome intentional None. Pairs anvil 163 / core watch_* None pins.

207. **CrossBlock cancel finished at near-wrap gen** — palette idle continuation
     drops MAX-1,MAX-1 finished walks (no resume) beside the MAX-1→MAX bump.
     Pairs anvil 164 / core cancel edge `continue_idle_resume_edges_drop_stale_or_finished_walks`.

208. **Sticky closer mid-range VS / Mongolian Todo soft-hyphen labels** —
     U+FE03 / U+FE0B / U+1806 variants of "Save Block history" stay toast-only
     beside rounds 203/196. Pairs anvil 165.

209. **output_notice closer mid-range VS / Mongolian Todo soft-hyphen gate** —
     U+FE03 / U+FE0B / U+1806-padded Truncated/Partly/Earlier strings stay
     outside the known-set remount gate beside rounds 204/197. Pairs anvil sticky tip.

210. **Find FVS/MVS/VS/Khmer/CGJ-query QueryNoMatches + stale** —
     U+180B / U+180E / U+FE00 / U+17B4 / U+034F-only queries stay
     QueryNoMatches under Command/Output/All when stale ids remain (Hangul/
     ideo/Ogham already pinned). Pairs anvil 166.

211. **CrossBlock cancel near-near-wrap bump** — palette idle continuation
     drops MAX-2→MAX-1 generation bumps (with or without resume), scheduled
     ahead MAX-1 vs MAX-2, and finished walks at MAX-2. Pairs anvil 167 /
     core cancel edge.

212. **Sticky Retry skips missing Block view without aborting** — `continue`
     past notebook pages/leaves without a Block view so non-Block chrome
     cannot starve later leaves of `retry_history_persistence`. Pairs anvil 168.

213. **Sticky Retry hides before walk / stays quiet on Ok** — optimistic
     `set_visible(false)` precedes the notebook walk; only Err re-raises via
     `show_block_history_failure`. Pairs anvil 169.

214. **SitNear/Celebrate tier overwrite Full-motion None** — Full motion pins
     SitNear→Inspect and Celebrate↔CelebrateBig intentional None. Pairs anvil
     170 / core `sit_near_and_celebrate_tier_overwrites_stay_none`.

215. **Sticky inner mid-range VS / Mongolian syllable-boundary labels** —
     U+FE04 / U+FE0A / U+1807 variants of "Save Block history" stay toast-only
     beside rounds 208/203. Pairs anvil 171.

216. **output_notice inner mid-range VS / Mongolian syllable-boundary gate** —
     U+FE04 / U+FE0A / U+1807-padded Truncated/Partly/Earlier strings stay
     outside the known-set remount gate beside rounds 209/204. Pairs anvil sticky tip.

217. **Find FE03/Todo soft-hyphen-query QueryNoMatches + stale** —
     U+FE03 / U+FE0B / U+1806-only queries stay QueryNoMatches under
     Command/Output/All when stale ids remain (FVS/Khmer already pinned).
     Pairs anvil 172.

218. **CrossBlock cancel near-near-near-wrap bump** — palette idle continuation
     drops MAX-3→MAX-2 generation bumps (with or without resume), scheduled
     ahead MAX-2 vs MAX-3, and finished walks at MAX-3. Pairs anvil 173 /
     core cancel edge.

219. **Celebrate*→UnknownOutcome Full-motion bridges** — Full motion animates
     Celebrate{,Big}→UnknownOutcome; Calm/Static snap. Pairs anvil 174 / core
     `CelebrateToUnknownOutcome` / `CelebrateBigToUnknownOutcome`.

220. **Sticky nesting mid-range VS / Mongolian Manchu-comma labels** —
     U+FE05 / U+FE09 / U+1808 variants of "Save Block history" stay toast-only
     beside rounds 215/208. Pairs anvil 175.

221. **output_notice nesting mid-range VS / Mongolian Manchu-comma gate** —
     U+FE05 / U+FE09 / U+1808-padded Truncated/Partly/Earlier strings stay
     outside the known-set remount gate beside rounds 216/209. Pairs anvil sticky tip.

222. **Find FE04/syllable-boundary-query QueryNoMatches + stale** —
     U+FE04 / U+FE0A / U+1807-only queries stay QueryNoMatches under
     Command/Output/All when stale ids remain (FE03/Todo already pinned).
     Pairs anvil 176.

223. **CrossBlock cancel near-near-near-near-wrap bump** — palette idle
     continuation drops MAX-4→MAX-3 generation bumps (with or without resume),
     scheduled ahead MAX-3 vs MAX-4, and finished walks at MAX-4. Pairs anvil
     177 / core cancel edge.

224. **RestAfterPush→UnknownOutcome Full-motion bridge** — Full motion animates
     RestAfterPush→UnknownOutcome; Calm/Static snap. Pairs anvil 178 / core
     `RestAfterPushToUnknownOutcome` beside Celebrate*→Unknown inside between()
     93.

225. **Sticky deeper nesting mid-range VS / Mongolian Manchu full-stop labels** —
     U+FE06 / U+FE08 / U+1809 variants of "Save Block history" stay toast-only
     beside rounds 220/215. Pairs anvil 179.

226. **output_notice deeper nesting VS / Mongolian Manchu full-stop gate** —
     U+FE06 / U+FE08 / U+1809-padded Truncated/Partly/Earlier strings stay
     outside the known-set remount gate beside rounds 221/216. Pairs anvil sticky tip.

227. **CrossBlock cancel near-near-near-near-near-wrap bump** — palette idle
     continuation drops MAX-5→MAX-4 generation bumps (with or without resume),
     scheduled ahead MAX-4 vs MAX-5, and finished walks at MAX-5. Pairs anvil
     180 / core cancel edge.

228. **Celebrate/Rest→UnknownOutcome UI-bridge membership + Calm/Static snaps** —
     semantic_bridges len 93 lists Celebrate{,Big}→UnknownOutcome and
     RestAfterPush→UnknownOutcome beside core between() 93; Calm/Static snap in
     the Full-motion bridge loop. Pairs anvil 181.

229. **Find FE05/Manchu-comma-query QueryNoMatches + stale** —
     U+FE05 / U+FE09 / U+1808-only queries stay QueryNoMatches under
     Command/Output/All when stale ids remain (FE04/syllable already pinned).
     Pairs anvil 182.

230. **SitNear/Inspect→UnknownOutcome Full-motion bridges** — Full motion
     animates SitNearError/InspectError→UnknownOutcome; Calm/Static snap.
     Pairs anvil 183 / core SitNear/Inspect→Unknown inside between() 93 beside
     Celebrate*/Rest→Unknown.

231. **Sticky center mid-range VS / Mongolian birga labels** —
     U+FE07 / U+1800 variants of "Save Block history" stay toast-only
     beside rounds 225/220. Pairs anvil 184.

232. **output_notice center mid-range VS / Mongolian birga gate** —
     U+FE07 / U+1800-padded Truncated/Partly/Earlier strings stay
     outside the known-set remount gate beside rounds 226/221. Pairs anvil sticky tip.

233. **CrossBlock cancel near-near-near-near-near-near-wrap bump** — palette idle
     continuation drops MAX-6→MAX-5 generation bumps (with or without resume),
     scheduled ahead MAX-5 vs MAX-6, and finished walks at MAX-6. Pairs anvil
     185 / core cancel edge.

234. **GuardRecovery→UnknownOutcome Full-motion bridge** — Full motion animates
     GuardRecovery→UnknownOutcome; Calm/Static snap. Pairs anvil 186 / core
     `GuardRecoveryToUnknownOutcome` beside Celebrate*/Rest/SitNear/Inspect→
     Unknown inside between() 93.

235. **Find FE06/Manchu-full-stop-query QueryNoMatches + stale** —
     U+FE06 / U+FE08 / U+1809-only queries stay QueryNoMatches under
     Command/Output/All when stale ids remain (FE05/Manchu-comma already pinned).
     Pairs anvil 187.

236. **Sticky Mongolian ellipsis label** —
     U+1801 variant of "Save Block history" stays toast-only beside FE07/birga
     (round 231). Pairs anvil 188.

237. **output_notice Mongolian ellipsis gate** —
     U+1801-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky 1801. Pairs anvil sticky tip.

238. **CrossBlock cancel near-near-near-near-near-near-near-wrap bump** — palette idle
     continuation drops MAX-7→MAX-6 generation bumps (with or without resume),
     scheduled ahead MAX-6 vs MAX-7, and finished walks at MAX-7. Pairs anvil
     189 / core cancel edge.

239. **GuardRecovery→UnknownOutcome Full-motion UI bridge sync** — semantic_bridges
     membership + Calm/Static snaps list GuardRecovery→Unknown beside
     Celebrate*/Rest pins (dedicated Full-motion hold already at round 234).
     Pairs anvil 190 / core `GuardRecoveryToUnknownOutcome` inside between() 93.

240. **Find FE07/birga-query QueryNoMatches + stale** —
     U+FE07 / U+1800-only queries stay QueryNoMatches under
     Command/Output/All when stale ids remain (FE06/Manchu-full-stop already pinned).
     Pairs anvil 191.

241. **Sticky Mongolian comma label** —
     U+1802 variant of "Save Block history" stays toast-only beside ellipsis
     (round 236). Pairs anvil 192.

242. **output_notice Mongolian comma gate** —
     U+1802-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky 1802. Pairs anvil sticky tip.

243. **CrossBlock cancel near-near-near-near-near-near-near-near-wrap bump** — palette idle
     continuation drops MAX-8→MAX-7 generation bumps (with or without resume),
     scheduled ahead MAX-7 vs MAX-8, and finished walks at MAX-8. Pairs anvil
     193 / core cancel edge.

244. **UnknownOutcome→GuardRecovery Full-motion bridge** — Full motion animates
     UnknownOutcome→GuardRecovery; Calm/Static snap + semantic_bridges
     membership sync. Pairs anvil 194 / core `UnknownOutcomeToGuardRecovery`
     reverse of GuardRecovery→Unknown inside between() 93.

245. **Find 1801/ellipsis-query QueryNoMatches + stale** —
     U+1801-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FE07/birga already pinned). Pairs anvil 195.

246. **Sticky Mongolian full stop label** —
     U+1803 variant of "Save Block history" stays toast-only beside comma
     (round 241). Pairs anvil 196.

247. **output_notice Mongolian full stop gate** —
     U+1803-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky 1803. Pairs anvil sticky tip.

248. **CrossBlock cancel near-near-near-near-near-near-near-near-near-wrap bump** — palette idle
     continuation drops MAX-9→MAX-8 generation bumps (with or without resume),
     scheduled ahead MAX-8 vs MAX-9, and finished walks at MAX-9. Pairs anvil
     197 / core cancel edge.

249. **UnknownOutcome→GuardCautious Full-motion bridge** — Full motion animates
     UnknownOutcome→GuardCautious; Calm/Static snap + semantic_bridges
     membership sync. Unknown↔GuardRecovery already synced in rounds 239/244.
     Pairs anvil 198 / core `UnknownOutcomeToGuardCautious` inside between() 93.
250. **Find 1802/comma-query QueryNoMatches + stale** —
     U+1802-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (1801/ellipsis already pinned). Pairs anvil 199.

251. **Sticky Mongolian colon label** —
     U+1804 variant of "Save Block history" stays toast-only beside full stop
     (round 246). Pairs anvil 200.

252. **output_notice Mongolian colon gate** —
     U+1804-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky 1804. Pairs anvil sticky tip.

253. **CrossBlock cancel near-near-near-near-near-near-near-near-near-near-wrap bump** — palette idle
     continuation drops MAX-10→MAX-9 generation bumps (with or without resume),
     scheduled ahead MAX-9 vs MAX-10, and finished walks at MAX-10. Pairs anvil
     201 / core cancel edge.

254. **UnknownOutcome→GuardStuck Full-motion bridge** — Full motion animates
     UnknownOutcome→GuardStuck; Calm/Static snap + semantic_bridges
     membership sync. Unknown→GuardCautious already synced in round 249.
     Pairs anvil 202 / core `UnknownOutcomeToGuardStuck` inside between() 93.

255. **Find 1803/full-stop-query QueryNoMatches + stale** —
     U+1803-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (1802/comma already pinned). Pairs anvil 203.

256. **Sticky Mongolian four dots label** —
     U+1805 variant of "Save Block history" stays toast-only beside colon
     (round 251). Pairs anvil 204.

257. **output_notice Mongolian four dots gate** —
     U+1805-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky 1805. Pairs anvil sticky tip.

258. **CrossBlock cancel near-near-near-near-near-near-near-near-near-near-near-wrap bump** — palette idle
     continuation drops MAX-11→MAX-10 generation bumps (with or without resume),
     scheduled ahead MAX-10 vs MAX-11, and finished walks at MAX-11. Pairs anvil
     205 / core cancel edge.

259. **UnknownOutcome→GuardFailure Full-motion bridge** — Full motion animates
     UnknownOutcome→GuardFailure; Calm/Static snap + semantic_bridges
     membership sync. Unknown→GuardStuck already synced in round 254.
     Pairs anvil 206 / core `UnknownOutcomeToGuardFailure` inside between() 93.

260. **Find 1804/colon-query QueryNoMatches + stale** —
     U+1804-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (1803/full-stop already pinned). Pairs anvil 207.

261. **Sticky fullwidth colon label** —
     U+FF1A variant of "Save Block history" stays toast-only beside Mongolian
     four dots (round 256). Pairs anvil 208.

262. **output_notice fullwidth colon gate** —
     U+FF1A-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky FF1A. Pairs anvil sticky tip.

263. **CrossBlock cancel near-near-near-near-near-near-near-near-near-near-near-near-wrap bump** — palette idle
     continuation drops MAX-12→MAX-11 generation bumps (with or without resume),
     scheduled ahead MAX-11 vs MAX-12, and finished walks at MAX-12. Pairs anvil
     209 / core cancel edge.

264. **UnknownOutcome→Idle Full-motion bridge** — Full motion animates
     UnknownOutcome→Idle; Calm/Static snap + semantic_bridges membership sync.
     Unknown→GuardFailure already synced in round 259. Pairs anvil 210 / core
     `UnknownOutcomeToIdle` inside between() 93.

265. **Find 1805/four-dots-query QueryNoMatches + stale** —
     U+1805-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (1804/colon already pinned). Pairs anvil 211.

266. **Sticky fullwidth semicolon label** —
     U+FF1B variant of "Save Block history" stays toast-only beside fullwidth
     colon (round 261). Pairs anvil 212.

267. **output_notice fullwidth semicolon gate** —
     U+FF1B-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky FF1B. Pairs anvil sticky tip.

268. **CrossBlock cancel near-near-near-near-near-near-near-near-near-near-near-near-near-wrap bump** — palette idle
     continuation drops MAX-13→MAX-12 generation bumps (with or without resume),
     scheduled ahead MAX-12 vs MAX-13, and finished walks at MAX-13. Pairs anvil
     213 / core cancel edge.

269. **UnknownOutcome→Celebrate Full-motion bridge** — Full motion animates
     UnknownOutcome→Celebrate; Calm/Static snap + semantic_bridges
     membership sync. Unknown→Idle already synced in round 264.
     Pairs anvil 214 / core `UnknownOutcomeToCelebrate` inside between() 93.

270. **Find FF1A/fullwidth-colon-query QueryNoMatches + stale** —
     U+FF1A-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (1805/four-dots already pinned). Pairs anvil 215.

271. **Sticky fullwidth less-than label** —
     U+FF1C variant of "Save Block history" stays toast-only beside fullwidth
     semicolon (round 266). Pairs anvil 216.

272. **output_notice fullwidth less-than gate** —
     U+FF1C-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky FF1C. Pairs anvil sticky tip.

273. **CrossBlock cancel near-near-near-near-near-near-near-near-near-near-near-near-near-near-wrap bump** — palette idle
     continuation drops MAX-14→MAX-13 generation bumps (with or without resume),
     scheduled ahead MAX-13 vs MAX-14, and finished walks at MAX-14. Pairs anvil
     217 / core cancel edge.

274. **UnknownOutcome→CelebrateBig Full-motion bridge** — Full motion animates
     UnknownOutcome→CelebrateBig; Calm/Static snap + semantic_bridges
     membership sync. Unknown→Celebrate already synced in round 269.
     Pairs anvil 218 / core `UnknownOutcomeToCelebrateBig` inside between() 93.

275. **Find FF1B/fullwidth-semicolon-query QueryNoMatches + stale** —
     U+FF1B-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF1A/fullwidth-colon already pinned). Pairs anvil 219.

276. **Sticky fullwidth equals label** —
     U+FF1D variant of "Save Block history" stays toast-only beside fullwidth
     less-than (round 271). Pairs anvil 220.

277. **output_notice fullwidth equals gate** —
     U+FF1D-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky FF1D. Pairs anvil sticky tip.

278. **CrossBlock cancel near-near-near-near-near-near-near-near-near-near-near-near-near-near-near-wrap bump** — palette idle
     continuation drops MAX-15→MAX-14 generation bumps (with or without resume),
     scheduled ahead MAX-14 vs MAX-15, and finished walks at MAX-15. Pairs anvil
     221 / core cancel edge.

279. **UnknownOutcome→SitNearError Full-motion bridge** — Full motion animates
     UnknownOutcome→SitNearError; Calm/Static snap + semantic_bridges
     membership sync. Unknown→CelebrateBig already synced in round 274.
     Pairs anvil 222 / core `UnknownOutcomeToSitNearError` inside between() 93.

280. **Find FF1C/fullwidth-less-than-query QueryNoMatches + stale** —
     U+FF1C-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF1B/fullwidth-semicolon already pinned). Pairs anvil 223.

281. **Sticky fullwidth greater-than label** —
     U+FF1E variant of "Save Block history" stays toast-only beside fullwidth
     equals (round 276). Pairs anvil 224.

282. **output_notice fullwidth greater-than gate** —
     U+FF1E-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky FF1E. Pairs anvil sticky tip.

283. **CrossBlock cancel near-near-near-near-near-near-near-near-near-near-near-near-near-near-near-near-wrap bump** — palette idle
     continuation drops MAX-16→MAX-15 generation bumps (with or without resume),
     scheduled ahead MAX-15 vs MAX-16, and finished walks at MAX-16. Pairs anvil
     225 / core cancel edge.

284. **UnknownOutcome→InspectError Full-motion bridge** — Full motion animates
     UnknownOutcome→InspectError; Calm/Static snap + semantic_bridges
     membership sync. Unknown→SitNearError already synced in round 279.
     Pairs anvil 226 / core `UnknownOutcomeToInspectError` inside between() 93.

285. **Find FF1D/fullwidth-equals-query QueryNoMatches + stale** —
     U+FF1D-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF1C/fullwidth-less-than already pinned). Pairs anvil 227.

286. **Sticky fullwidth question-mark label** —
     U+FF1F variant of "Save Block history" stays toast-only beside fullwidth
     greater-than (round 281). Pairs anvil 228.

287. **output_notice fullwidth question-mark gate** —
     U+FF1F-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky FF1F. Pairs anvil sticky tip.

288. **CrossBlock cancel MAX-17→MAX-16 wrap bump** — palette idle continuation
     drops MAX-17→MAX-16 generation bumps (with or without resume), scheduled
     ahead MAX-16 vs MAX-17, and finished walks at MAX-17. Pairs anvil 229 /
     core cancel edge.

289. **UnknownOutcome→RestAfterPush Full-motion bridge** — Full motion animates
     UnknownOutcome→RestAfterPush; Calm/Static snap + semantic_bridges
     membership sync. Unknown→InspectError already synced in round 284.
     Pairs anvil 230 / core `UnknownOutcomeToRestAfterPush` inside between() 93.

290. **Find FF1E/fullwidth-greater-than-query QueryNoMatches + stale** —
     U+FF1E-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF1D/fullwidth-equals already pinned). Pairs anvil 231.

291. **Sticky fullwidth commercial-at label** —
     U+FF20 variant of "Save Block history" stays toast-only beside fullwidth
     question mark (round 286). Pairs anvil 232.

292. **output_notice fullwidth commercial-at gate** —
     U+FF20-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky FF20. Pairs anvil sticky tip.

293. **CrossBlock cancel MAX-18→MAX-17 wrap bump** — palette idle continuation
     drops MAX-18→MAX-17 generation bumps (with or without resume), scheduled
     ahead MAX-17 vs MAX-18, and finished walks at MAX-18. Pairs anvil 233 /
     core cancel edge.

294. **GuardCautious→Idle Full-motion bridge** — Full motion animates
     GuardCautious→Idle; Calm/Static snap + semantic_bridges membership
     sync. Unknown→RestAfterPush already synced in round 289. Pairs anvil
     234 / core `GuardCautiousToIdle` inside between() 93.

295. **Find FF1F/fullwidth-question-mark-query QueryNoMatches + stale** —
     U+FF1F-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF1E/fullwidth-greater-than already pinned). Pairs anvil 235.

296. **Sticky fullwidth latin-A label** —
     U+FF21 variant of "Save Block history" stays toast-only beside fullwidth
     commercial at (round 291). Pairs anvil 236.

297. **output_notice fullwidth latin-A gate** —
     U+FF21-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky FF21. Pairs anvil sticky tip.

298. **CrossBlock cancel MAX-19→MAX-18 wrap bump** — palette idle continuation
     drops MAX-19→MAX-18 generation bumps (with or without resume), scheduled
     ahead MAX-18 vs MAX-19, and finished walks at MAX-19. Pairs anvil 237 /
     core cancel edge.

299. **GuardFailure→Idle Full-motion bridge** — Full motion animates
     GuardFailure→Idle; Calm/Static snap + semantic_bridges membership
     sync. GuardCautious→Idle already synced in round 294. Pairs anvil 238 /
     core `GuardFailureToIdle` inside between() 93.

300. **Find FF20/fullwidth-commercial-at-query QueryNoMatches + stale** —
     U+FF20-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF1F/fullwidth-question-mark already pinned). Pairs anvil 239.

301. **Sticky fullwidth latin-B label** —
     U+FF22 variant of "Save Block history" stays toast-only beside fullwidth
     latin A (round 296). Pairs anvil 240.

302. **output_notice fullwidth latin-B gate** —
     U+FF22-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky FF22. Pairs anvil sticky tip.

303. **CrossBlock cancel MAX-20→MAX-19 wrap bump** — palette idle continuation
     drops MAX-20→MAX-19 generation bumps (with or without resume), scheduled
     ahead MAX-19 vs MAX-20, and finished walks at MAX-20. Pairs anvil 241 /
     core cancel edge.

304. **GuardStuck→Idle Full-motion bridge** — Full motion animates
     GuardStuck→Idle; Calm/Static snap + semantic_bridges membership
     sync. GuardFailure→Idle already synced in round 299. Pairs anvil 242 /
     core `GuardStuckToIdle` inside between() 93.

305. **Find FF21/fullwidth-latin-A-query QueryNoMatches + stale** —
     U+FF21-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF20/fullwidth-commercial-at already pinned). Pairs anvil 243.

306. **Sticky fullwidth latin-C label** —
     U+FF23 variant of "Save Block history" stays toast-only beside fullwidth
     latin B (round 301). Pairs anvil 244.

307. **output_notice fullwidth latin-C gate** —
     U+FF23-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky FF23. Pairs anvil sticky tip.

308. **CrossBlock cancel MAX-21→MAX-20 wrap bump** — palette idle continuation
     drops MAX-21→MAX-20 generation bumps (with or without resume), scheduled
     ahead MAX-20 vs MAX-21, and finished walks at MAX-21. Pairs anvil 245 /
     core cancel edge.

309. **GuardRecovery→Idle Full-motion bridge** — Full motion animates
     GuardRecovery→Idle; Calm/Static snap + semantic_bridges membership
     sync. GuardStuck→Idle already synced in round 304. Pairs anvil 246 /
     core `GuardRecoveryToIdle` inside between() 93.

310. **Find FF22/fullwidth-latin-B-query QueryNoMatches + stale** —
     U+FF22-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF21/fullwidth-latin-A already pinned). Pairs anvil 247.

311. **Sticky fullwidth latin-D label** —
     U+FF24 variant of "Save Block history" stays toast-only beside fullwidth
     latin C (round 306). Pairs anvil 248.

312. **output_notice fullwidth latin-D gate** —
     U+FF24-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky FF24. Pairs anvil sticky tip.

313. **CrossBlock cancel MAX-22→MAX-21 wrap bump** — palette idle continuation
     drops MAX-22→MAX-21 generation bumps (with or without resume), scheduled
     ahead MAX-21 vs MAX-22, and finished walks at MAX-22. Pairs anvil 249 /
     core cancel edge.

314. **Celebrate→Idle Full-motion bridge** — Full motion animates
     Celebrate→Idle; Calm/Static snap + semantic_bridges membership sync.
     GuardRecovery→Idle already synced in round 309. Pairs anvil 250 /
     core `CelebrateToIdle` inside between() 93.

315. **Find FF23/fullwidth-latin-C-query QueryNoMatches + stale** —
     U+FF23-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF22/fullwidth-latin-B already pinned). Pairs anvil 251.

316. **Sticky fullwidth latin-E label** —
     U+FF25 variant of "Save Block history" stays toast-only beside fullwidth
     latin D (round 311). Pairs anvil 252.

317. **output_notice fullwidth latin-E gate** —
     U+FF25-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky FF25. Pairs anvil sticky tip.

318. **CrossBlock cancel MAX-23→MAX-22 wrap bump** — palette idle continuation
     drops MAX-23→MAX-22 generation bumps (with or without resume), scheduled
     ahead MAX-22 vs MAX-23, and finished walks at MAX-23. Pairs anvil 253 /
     core cancel edge.

319. **CelebrateBig→Idle Full-motion bridge** — Full motion animates
     CelebrateBig→Idle; Calm/Static snap + semantic_bridges membership
     sync. Celebrate→Idle already synced in round 314. Pairs anvil 254 /
     core `CelebrateBigToIdle` inside between() 93.

320. **Find FF24/fullwidth-latin-D-query QueryNoMatches + stale** —
     U+FF24-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF23/fullwidth-latin-C already pinned). Pairs anvil 255.

321. **Sticky fullwidth latin-F label** —
     U+FF26 variant of "Save Block history" stays toast-only beside fullwidth
     latin E (round 316). Pairs anvil 256.

322. **output_notice fullwidth latin-F gate** —
     U+FF26-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky FF26. Pairs anvil sticky tip.

323. **CrossBlock cancel MAX-24→MAX-23 wrap bump** — palette idle continuation
     drops MAX-24→MAX-23 generation bumps (with or without resume), scheduled
     ahead MAX-23 vs MAX-24, and finished walks at MAX-24. Pairs anvil 257 /
     core cancel edge.

324. **RestAfterPush→Idle Full-motion bridge** — Full motion animates
     RestAfterPush→Idle; Calm/Static snap + semantic_bridges membership
     sync. CelebrateBig→Idle already synced in round 319. Pairs anvil 258 /
     core `RestAfterPushToIdle` inside between() 93.

325. **Find FF25/fullwidth-latin-E-query QueryNoMatches + stale** —
     U+FF25-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF24/fullwidth-latin-D already pinned). Pairs anvil 259.

326. **Sticky fullwidth latin-G label** —
     U+FF27 variant of "Save Block history" stays toast-only beside fullwidth
     latin F (round 321). Pairs anvil 260.

327. **output_notice fullwidth latin-G gate** —
     U+FF27-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky FF27. Pairs anvil sticky tip.

328. **CrossBlock cancel MAX-25→MAX-24 wrap bump** — palette idle continuation
     drops MAX-25→MAX-24 generation bumps (with or without resume), scheduled
     ahead MAX-24 vs MAX-25, and finished walks at MAX-25. Pairs anvil 261 /
     core cancel edge.

329. **InspectError→Idle Full-motion bridge** — Full motion animates
     InspectError→Idle; Calm/Static snap + semantic_bridges membership
     sync. RestAfterPush→Idle already synced in round 324. Pairs anvil 262 /
     core `InspectErrorToIdle` inside between() 93.

330. **Find FF26/fullwidth-latin-F-query QueryNoMatches + stale** —
     U+FF26-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF25/fullwidth-latin-E already pinned). Pairs anvil 263.

331. **Sticky fullwidth latin-H label** —
     U+FF28 variant of "Save Block history" stays toast-only beside fullwidth
     latin G (round 326). Pairs anvil 264.

332. **output_notice fullwidth latin-H gate** —
     U+FF28-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky FF28. Pairs anvil sticky tip.

333. **CrossBlock cancel MAX-26→MAX-25 wrap bump** — palette idle continuation
     drops MAX-26→MAX-25 generation bumps (with or without resume), scheduled
     ahead MAX-25 vs MAX-26, and finished walks at MAX-26. Pairs anvil 265 /
     core cancel edge.

334. **SitNearError→Idle Full-motion bridge** — Full motion animates
     SitNearError→Idle; Calm/Static snap + semantic_bridges membership
     sync. InspectError→Idle already synced in round 329. Pairs anvil 266 /
     core `SitNearErrorToIdle` inside between() 93.

335. **Find FF27/fullwidth-latin-G-query QueryNoMatches + stale** —
     U+FF27-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF26/fullwidth-latin-F already pinned). Pairs anvil 267.

336. **Sticky fullwidth latin-I label** —
     U+FF29 variant of "Save Block history" stays toast-only beside fullwidth
     latin H (round 331). Pairs anvil 268.

337. **output_notice fullwidth latin-I gate** —
     U+FF29-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky FF29. Pairs anvil sticky tip.

338. **CrossBlock cancel MAX-27→MAX-26 wrap bump** — palette idle continuation
     drops MAX-27→MAX-26 generation bumps (with or without resume), scheduled
     ahead MAX-26 vs MAX-27, and finished walks at MAX-27. Pairs anvil 269 /
     core cancel edge.

339. **WatchCommand→Idle Full-motion bridge** — Full motion animates
     WatchCommand→Idle; Calm/Static snap + semantic_bridges membership
     sync. SitNearError→Idle already synced in round 334. Pairs anvil 270 /
     core `WatchCommandToIdle` inside between() 93.

340. **Find FF28/fullwidth-latin-H-query QueryNoMatches + stale** —
     U+FF28-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF27/fullwidth-latin-G already pinned). Pairs anvil 271.

341. **Sticky fullwidth latin-J label** —
     U+FF2A variant of "Save Block history" stays toast-only beside fullwidth
     latin I (round 336). Pairs anvil 272.

342. **output_notice fullwidth latin-J gate** —
     U+FF2A-padded Truncated/Partly/Earlier strings stay outside the known-set
     remount gate beside sticky FF2A. Pairs anvil sticky tip.

343. **CrossBlock cancel MAX-28→MAX-27 wrap bump** — palette idle continuation
     drops MAX-28→MAX-27 generation bumps (with or without resume), scheduled
     ahead MAX-27 vs MAX-28, and finished walks at MAX-28. Pairs anvil 273 /
     core cancel edge.

344. **WatchAgent→Idle Full-motion bridge** — Full motion animates
     WatchAgent→Idle; Calm/Static snap + semantic_bridges membership
     sync. WatchCommand→Idle already synced in round 339. Pairs anvil 274 /
     core `WatchAgentToIdle` inside between() 93.

345. **Find FF29/fullwidth-latin-I-query QueryNoMatches + stale** —
     U+FF29-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF28/fullwidth-latin-H already pinned). Pairs anvil 275.

346. **Sticky near-miss U+FF2B/fullwidth-latin-K** — persistence_failure_surface
     keeps "Save Block\u{ff2b}history" on Toast (FF2A/fullwidth-latin-J already
     pinned). Pairs anvil 276.

347. **output_notice remount gate U+FF2B/fullwidth-latin-K** — known_output_notice
     rejects Truncated/Partly/Earlier strings padded with U+FF2B (FF2A already
     pinned). Pairs anvil 277.

348. **CrossBlock cancel MAX-29→MAX-28 wrap bump** — palette idle continuation
     drops MAX-29→MAX-28 generation bumps (with or without resume), scheduled
     ahead MAX-28 vs MAX-29, and finished walks at MAX-29. Pairs anvil 278 /
     core cancel edge.

349. **WatchSettled→Idle Full-motion bridge** — Full motion animates
     WatchSettled→Idle; Calm/Static snap + semantic_bridges membership
     sync. WatchAgent→Idle already synced in round 344. Pairs anvil 279 /
     core `WatchSettledToIdle` inside between() 93.

350. **Find FF2A/fullwidth-latin-J-query QueryNoMatches + stale** —
     U+FF2A-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF29/fullwidth-latin-I already pinned). Pairs anvil 280.

351. **Sticky near-miss U+FF2C/fullwidth-latin-L** — persistence_failure_surface
     keeps "Save Block\u{ff2c}history" on Toast (FF2B/fullwidth-latin-K already
     pinned). Pairs anvil 281.

352. **output_notice remount gate U+FF2C/fullwidth-latin-L** — known_output_notice
     rejects Truncated/Partly/Earlier strings padded with U+FF2C (FF2B already
     pinned). Pairs anvil 282.

353. **CrossBlock cancel MAX-30→MAX-29 wrap bump** — palette idle continuation
     drops MAX-30→MAX-29 generation bumps (with or without resume), scheduled
     ahead MAX-29 vs MAX-30, and finished walks at MAX-30. Pairs anvil 283 /
     core cancel edge.

354. **WatchAgent→Celebrate Full-motion bridge** — Full motion animates
     WatchAgent→Celebrate; Calm/Static snap + semantic_bridges membership
     sync. WatchSettled→Idle already synced in round 349. Pairs anvil 284 /
     core `WatchAgentToCelebrate` inside between() 93.

355. **Find FF2B/fullwidth-latin-K-query QueryNoMatches + stale** —
     U+FF2B-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF2A/fullwidth-latin-J already pinned). Pairs anvil 285.

356. **Sticky near-miss U+FF2D/fullwidth-latin-M** — persistence_failure_surface
     keeps "Save Block\u{ff2d}history" on Toast (FF2C/fullwidth-latin-L already
     pinned). Pairs anvil 286.

357. **output_notice remount gate U+FF2D/fullwidth-latin-M** — known_output_notice
     rejects Truncated/Partly/Earlier strings padded with U+FF2D (FF2C already
     pinned). Pairs anvil 287.

358. **CrossBlock cancel MAX-31→MAX-30 wrap bump** — palette idle continuation
     drops MAX-31→MAX-30 generation bumps (with or without resume), scheduled
     ahead MAX-30 vs MAX-31, and finished walks at MAX-31. Pairs anvil 288 /
     core cancel edge.

359. **WatchCommand→Celebrate Full-motion bridge** — Full motion animates
     WatchCommand→Celebrate; Calm/Static snap + semantic_bridges membership
     sync. WatchAgent→Celebrate already synced in round 354. Pairs anvil 289 /
     core `WatchCommandToCelebrate` inside between() 93.

360. **Find FF2C/fullwidth-latin-L-query QueryNoMatches + stale** —
     U+FF2C-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF2B/fullwidth-latin-K already pinned). Pairs anvil 290.

361. **Sticky near-miss U+FF2E/fullwidth-latin-N** — persistence_failure_surface
     keeps "Save Block\u{ff2e}history" on Toast (FF2D/fullwidth-latin-M already
     pinned). Pairs anvil 291.

362. **output_notice remount gate U+FF2E/fullwidth-latin-N** — known_output_notice
     rejects Truncated/Partly/Earlier strings padded with U+FF2E (FF2D already
     pinned). Pairs anvil 292.

363. **CrossBlock cancel MAX-32→MAX-31 wrap bump** — palette idle continuation
     drops MAX-32→MAX-31 generation bumps (with or without resume), scheduled
     ahead MAX-31 vs MAX-32, and finished walks at MAX-32. Pairs anvil 293 /
     core cancel edge.

364. **WatchCommand→CelebrateBig Full-motion bridge** — Full motion animates
     WatchCommand→CelebrateBig; Calm/Static snap + semantic_bridges membership
     sync. WatchCommand→Celebrate already synced in round 359. Pairs anvil 294 /
     core `WatchCommandToCelebrateBig` inside between() 93.

365. **Find FF2D/fullwidth-latin-M-query QueryNoMatches + stale** —
     U+FF2D-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF2C/fullwidth-latin-L already pinned). Pairs anvil 295.

366. **Sticky near-miss U+FF2F/fullwidth-latin-O** — persistence_failure_surface
     keeps "Save Block\u{ff2f}history" on Toast (FF2E/fullwidth-latin-N already
     pinned). Pairs anvil 296.

367. **output_notice remount gate U+FF2F/fullwidth-latin-O** — known_output_notice
     rejects Truncated/Partly/Earlier strings padded with U+FF2F (FF2E already
     pinned). Pairs anvil 297.

368. **CrossBlock cancel MAX-33→MAX-32 wrap bump** — palette idle continuation
     drops MAX-33→MAX-32 generation bumps (with or without resume), scheduled
     ahead MAX-32 vs MAX-33, and finished walks at MAX-33. Pairs anvil 298 /
     core cancel edge.

369. **WatchCommand→InspectError Full-motion bridge** — Full motion animates
     WatchCommand→InspectError; Calm/Static snap + semantic_bridges membership
     sync. WatchCommand→CelebrateBig already synced in round 364. Pairs anvil
     299 / core `WatchCommandToInspectError` inside between() 93.

370. **Find FF2E/fullwidth-latin-N-query QueryNoMatches + stale** —
     U+FF2E-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF2D/fullwidth-latin-M already pinned). Pairs anvil 300.

371. **Sticky near-miss U+FF30/fullwidth-latin-P** — persistence_failure_surface
     keeps "Save Block\u{ff30}history" on Toast (FF2F/fullwidth-latin-O already
     pinned). Pairs anvil 301.

372. **output_notice remount gate U+FF30/fullwidth-latin-P** — known_output_notice
     rejects Truncated/Partly/Earlier strings padded with U+FF30 (FF2F already
     pinned). Pairs anvil 302.

373. **CrossBlock cancel MAX-34→MAX-33 wrap bump** — palette idle continuation
     drops MAX-34→MAX-33 generation bumps (with or without resume), scheduled
     ahead MAX-33 vs MAX-34, and finished walks at MAX-34. Pairs anvil 303 /
     core cancel edge.

374. **WatchCommand→SitNearError Full-motion bridge** — Full motion animates
     WatchCommand→SitNearError; Calm/Static snap + semantic_bridges membership
     sync. WatchCommand→InspectError already synced in round 369. Pairs anvil
     304 / core `WatchCommandToSitNearError` inside between() 93.

375. **Find FF2F/fullwidth-latin-O-query QueryNoMatches + stale** —
     U+FF2F-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF2E/fullwidth-latin-N already pinned). Pairs anvil 305.

376. **Sticky near-miss U+FF31/fullwidth-latin-Q** — persistence_failure_surface
     keeps "Save Block\u{ff31}history" on Toast (FF30/fullwidth-latin-P already
     pinned). Pairs anvil 306.

377. **output_notice remount gate U+FF31/fullwidth-latin-Q** — known_output_notice
     rejects Truncated/Partly/Earlier strings padded with U+FF31 (FF30 already
     pinned). Pairs anvil 307.

378. **CrossBlock cancel MAX-35→MAX-34 wrap bump** — palette idle continuation
     drops MAX-35→MAX-34 generation bumps (with or without resume), scheduled
     ahead MAX-34 vs MAX-35, and finished walks at MAX-35. Pairs anvil 308 /
     core cancel edge.

379. **WatchCommand→RestAfterPush Full-motion bridge** — Full motion animates
     WatchCommand→RestAfterPush; Calm/Static snap + semantic_bridges membership
     sync. WatchCommand→SitNearError already synced in round 374. Pairs anvil
     309 / core `WatchCommandToRestAfterPush` inside between() 93.

380. **Find FF30/fullwidth-latin-P-query QueryNoMatches + stale** —
     U+FF30-only queries stay QueryNoMatches under Command/Output/All when stale
     ids remain (FF2F/fullwidth-latin-O already pinned). Pairs anvil 310.

