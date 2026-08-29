# Engineering handoff

Updated: 2026-08-29 (AI chat store upstreamed to jterm_core; AI correction
and snapshot-line guards)

This working tree contains the nine-round "Evolve ASCII organism" series
(`d6fb8b4..00a099e`), the continued pass (`fa5c947`), the recovery-vigil
layer, and the current five-round debugging-vigil evolution: the experimental
Block-pane organism grew from an event reflex into a continuous life
simulation with utility-selected behavior, agent attribution, interpolated
spatial motion, motion restraint, a persistent per-repo build-duration
baseline, and a five-part embodiment pass covering visible growth, output
rhythm, semantic transitions, repo territory, and attention arbitration.
Every historical round followed the
same loop — implement, unit-test, sync `docs/USER_GUIDE.md`, run a
three-agent adversarial verification workflow, fix confirmed findings,
commit, push. The verification pass caught one to two real defects almost
every round (attribution races, a Drop panic on a fired glib source, a
saturation hole in a validate invariant); do not skip it.

## Completed since the previous handoff

- **AI chat store upstreaming (2026-08-29)**: `src/ui/ai_chat_store.rs` is a
  75-line shim (it was 1,536) over `jterm_core::ai::chat_store`, the union of
  the four terminals' private copies of the same multi-chat state machine —
  anvil 786, forge 1,536, ember 903, frost 791, 4,016 lines over the shared
  `jterm_core::ai::ConversationSnapshot` schema and not one line of toolkit
  code, diverged in both directions so that no copy was correct on its own.
  Core's module is 1,888 lines with 47 tests; forge's 27 store tests went
  upstream with the behavior they pinned, and the shim keeps a single test for
  the single decision it still owns. `src/` is 1,175 lines lighter, the largest
  deletion of the four repos.

  What forge sent up: the library-wide 8 MiB live-history budget with real
  compaction — the per-chat 100-turn and per-reply 256 KiB caps bound a chat,
  nothing but this bounds a 50-chat library; `snapshot_for_persistence`, which
  compacts *before* serialising and then syncs the truncation markers back into
  the live chats; typed `ArchiveOutcome`/`DeleteOutcome`; and draft merges that
  report what they dropped. The two budgets stay deliberately unequal — live
  turn text caps at 8 MiB, persistence at 4 MiB under an 8 MiB encoded-JSON
  ceiling — so `ConversationSnapshot::from_chats` still compacts on the way out
  and reports it through its flag; `publish_persisted_conversation`
  (`ai_panel.rs:1350`) consumes both halves exactly as before. What forge took
  back: in-store streaming, `summaries_filtered`, the prefix rule that makes
  draft recovery idempotent, and an at-capacity archive guard forge's own copy
  did not have.

  The panel's streaming accumulator is gone with it. `StreamProgress` owned the
  partial reply *and* the 256 KiB reply cap; the store now owns both, and
  `MAX_LIVE_ASSISTANT_MESSAGE_BYTES` has no forge caller at all — the cap exists
  once. What is left is `stream_render(partial, shown, attached)`
  (`ai_panel.rs:87`), a pure decision over the store's own partial: `shown` is
  read before `push_delta`, so `Append` carries only the bytes the transcript
  tail does not hold yet, and `push_delta`'s `Some(false)` (owner chat not
  visible) or `None` (cancelled, superseded, deleted) draws nothing. The
  invariant this buys: `render_active_chat` (`:1103`) now rebuilds the in-flight
  partial from the store and re-attaches `stream_display`, so a chat switched
  away and back shows what has already arrived instead of an empty gap until the
  next fragment — which may never come, if the stream has gone quiet — and
  `complete_success` clears the partial, so the finished reply cannot be drawn
  twice. Fragments dropped by the bounded UI queue are still healed by the
  authoritative final response, unchanged.

  Only the construction-time policy stays local, because it is a panel property
  and not a store property: `BUSY_POLICY = BusyChatPolicy::Allow`
  (`ai_chat_store.rs:32`). Archiving or deleting a chat with a request in flight
  is the one place the four apps genuinely disagree; forge proceeds because
  every such path cancels first (the Delete dialog cancels before removing the
  chat, `cancel_all_requests` cancels the whole map at teardown), and the
  cancelled request's late reply is still discarded on its epoch. Anvil, ember
  and frost pass `Refuse`, which is core's default. Retry recovery split the
  same way: forge keeps calling the busy-refusing `recover_retry_payload`, and
  needs nothing from `recover_retry_payload_detaching` (anvil's shutdown path)
  because `cancel_all_requests` has already cancelled the request before it
  recovers the payload. `delete_active` now returns a `Result`; deletion cannot
  be refused under `Allow`, but the panel reports a refusal instead of silently
  dropping the user's Delete, so a future policy change surfaces (`:955`).

  Three behavior changes reached the user. Archiving the last un-archived chat
  with all 50 slots occupied is now refused with a visible "Chat limit reached"
  status (`:888`) — forge's own copy set `archived = true` and then took the
  `else if !self.at_capacity()` branch, returning `Ok` with a library in which
  every chat was archived and no writable chat existed; core checks and refuses
  before it mutates. Library previews now arrive sanitised from the store
  (`chat_preview` runs `review_input::safe_inline_display`), so the row builder
  dropped its own subtitle filter and keeps the filter only on the title, which
  a restored snapshot supplies; the search query therefore matches the sanitised
  text, i.e. what is actually on screen. Forge was not exposed to the spoofing
  hole this closed for three of the four copies — it sanitised at display — but
  the filtering it hand-rolled in the panel now lives in `summaries_filtered`
  (`:1001`) with identical trim/lowercase/substring semantics, so the four
  libraries cannot drift. The draft-merge prefix rule changes nothing in forge
  today, because the panel holds at most one recoverable payload per chat (a
  starting request removes the stored retry payload); it removes the dependency
  of correctness on that call-site discipline.

  Not done, and blocking a commit: `Cargo.toml` still pins `jterm_core`
  `1f5f0fb`, which does not contain `ai::chat_store`. This tree compiles only
  through the temporary `[patch]` in `~/.cargo/config.toml` that points
  `jterm_core` and `jagent` at the local checkouts — that patch is also why the
  only `Cargo.lock` change is the loss of both `source = "git+…"` lines. Before
  this round can be committed the pin must move to the core commit that carries
  `chat_store`, `Cargo.lock` must be regenerated without the patch, and
  `flake.nix`'s `outputHashes` entry for `jterm_core-0.2.0` updated to match.

- **AI correction guards and snapshot-line quarantine (2026-08-29)**: the
  family's adversarial audit confirmed three holes in
  `src/ui/command_correction.rs`, all of them cases where forge's list was
  shorter than its siblings'. `adds_new_control_syntax` tested one substring at
  a time over single-character markers, and `"&&"` contains `"&"`: the scan
  could not tell `a & b` from `a && b`, so `ls | grep foo || rm -rf ~/work` was
  accepted as a correction for `ls | grep foo` because `|` was already present
  and `||` was never on the list. It now compares marker *sets* —
  `syntax_markers` (`:1639`) over `["&&", "||", ";", "|", "&", ">", "<", "$(",
  "`"]` — and refuses any marker the original does not contain, while a
  candidate that reuses only the original's own operators is still allowed.
  `adds_remote_execution` (`:1661`) was missing `mosh`, which opens an
  interactive session on a host the user never typed exactly as `ssh` does.
  `classify_failure` (`:1197`) hand-rolled an emptiness/control scan instead of
  calling `jterm_core::review_input::validate`, which additionally refuses
  visual spoofing: a command carrying U+202E was classified, embedded in the
  correction prompt sent to the configured provider, and rendered in the review
  card's "original" slot. The 16 KiB `MAX_COMMAND_BYTES` bound stays on top of
  the shared gate, because a correction request is not a bulk review insertion.
  The same pass widened classification to match the siblings: exit 127 is the
  POSIX command-not-found status, so an unrecognised shell wording now falls
  back to the command's first executable (resolved before the tool-suggestion
  branches, so an explicit suggestion can name the missing executable), and
  "no such subcommand" joined both unknown-token lists. Five new tests cover
  the chained-command refusal together with the marker-set allowance that must
  survive it, `mosh`, the spoofed command (which must also stay unclassified
  for `should_request_correction` while its unspoofed shape still classifies),
  exit 127 with and without a privilege prefix, and the cargo wording.

  `src/state.rs` had a quieter version of the same shape: `parse_ai_conversation`
  returned an `Option`, so "this snapshot has no AI line" and "this snapshot's
  AI line is unusable here" were the same answer. The window has already claimed
  those bytes by renaming a `window-*.state` onto its own `.active` name, and
  the next autosave writes a payload with no `ai_conversation=` line over
  exactly that path — so a duplicated, oversized or malformed line, or a chat
  library written by a newer forge pinning a newer snapshot schema, was silently
  and permanently deleted. The parser now answers
  `AiConversationLine::{Absent, Parsed, Rejected}` (`:1149`) and
  `restore_ai_conversation` (`:1215`) quarantines the file on `Rejected`, the
  same treatment `load_tabs_state`'s unreadable-read arm already applied.
  Quarantine moves the `.active` file aside even though its tab lines were fine;
  tab recovery is unaffected because `parse_tabs_state` reads the contents
  already in memory, and the next save recreates the snapshot. Two regressions
  pin both directions — an unusable line is moved aside and the tabs still
  restore; an absent or valid line leaves the file where it is.

- **Remote Files transactional navigation and authority isolation (2026-08-29)**:
  root changes now stage a `NavigationIntent` carrying a monotonic revision,
  immutable filesystem target and cancellation token. The target listing must
  succeed and still match both the newest intent and live authority identity
  before location, execution overlay, root store, selector, snapshot and
  history commit together. Failure, cancellation, profile mutation and
  out-of-order completion leave the old root/store/selection/history usable.
  Choosing the still-committed location explicitly cancels a staged selector
  change, including the pre-list remote Home probe. That probe freezes the
  stable filesystem authority and rechecks it against live config before
  handing its path to navigation, so B's late Home cannot target C after an
  index reuse. Back/Forward retain at most 64 committed points per stack and mutate
  only after a successful listing; a newly committed branch clears Forward.
  Up, Home, Open Folder, cwd follow, profile remap and foreground-SSH follow
  enter the same transaction. The clickable root title provides a bounded,
  control/spoof-safe absolute POSIX path entry and exact ancestor breadcrumb
  targets (dialog UI, not a persistent inline entry).
  History locations are exact-profile remapped on config reload and unprovable
  entries are dropped. A committed filesystem-identity guard also makes a
  retained snapshot read-only if its numeric profile index is reinterpreted;
  scans, mutations and Open Terminal cannot send old paths to the replacement
  endpoint while fallback navigation is pending or has failed.

  Scan admission keeps the existing global eight fixed workers/64 queued jobs
  and weighted Root/Manual/Lazy service, but now keys work by stable filesystem
  authority, round-robins authorities, caps each remote at two running and 16
  pending scans, and caps Local at 48 pending. At global saturation the first
  Root/Manual request from a queue-absent authority displaces the newest,
  lowest-priority queued request from the most overrepresented other authority.
  A slow endpoint therefore cannot occupy the global pool or make another
  authority's first interactive navigation return `WouldBlock`. File
  mutations moved from one thread per request to four fixed workers plus a
  32-job hard queue. Per-authority cap/fairness and fs-op saturation have pure
  regressions.

  Each successful directory snapshot now stores wall completion time for UI
  age and monotonic completion time for a five-minute remote TTL. While Files
  is visible, a 30-second tick revalidates at most four visible materialized
  stale directories with SWR, coalescing pending revisions. Non-cancellation,
  non-backpressure failures are classified transient/persistent and receive
  per-authority/path exponential or 30-second cooldown (30-second cap);
  explicit tree-row Retry bypasses the current cooldown for exactly one
  attempt. Queue/list/reconcile time, enqueue depth and delta sizes are logged
  with slow thresholds. The child-store registry is weak so GTK-owned live
  subtrees preserve identity without the cache pinning evicted subtrees.
  Regressions cover transactional failure/stale answers/cancellation, bounded
  branching history, validator/breadcrumbs, authority isolation, cooldown and
  Retry bypass, monotonic TTL, timing thresholds and weak-cache reclamation.

- **Remote Files bounded scheduling and navigation (2026-08-29)**: the audit
  found that the former concurrency counter still created one waiting OS thread
  per scan, collapsed-but-cached directories were skipped by targeted refresh,
  raw remote diagnostics could reach UI text, retained snapshots had no age,
  and mutation/navigation selection semantics were incomplete. Directory scans
  now use eight fixed workers behind a 64-job hard queue. Weighted root/manual/
  lazy lanes prioritize navigation, guarantee lazy service, let high-priority
  requests preempt newest lazy work, and physically retire cancelled same-path
  revisions before they consume queue capacity. Pending state is completed only
  by the current revision. Pure pressure, preemption, fairness, cancellation and
  pending regressions pin these bounds.

  Stale-while-revalidate now records the last successful publication time and
  labels retained snapshots with relative age. Tree/UI errors use stable
  categories rather than raw SSH/probe stderr, with bounded control/spoof-safe
  labels. A collapsed materialized store remains refreshable; reconciliation of
  a vanished/retyped directory cancels and removes all descendant requests,
  stores, timestamps and selection intents. Ambiguous operation failures also
  re-list exact affected parents, while successful create/rename selects the new
  path after the winning reconciliation. Remote home parsing is strict UTF-8;
  the Files header adds Home, context menus add Open Folder, and ListView-scoped
  `Alt+Up`, `Alt+Home`, `Alt+Right` provide parent/home/enter navigation without
  claiming terminal or ordinary list keys. Late home answers are guarded by
  generation, location, overlay and root. Tests cover error redaction/snapshot
  age, store delta/subtree boundaries, selection restoration, shortcut
  modifiers and strict home parsing.

- **Remote Files resilient refresh state (2026-08-29)**: initial child/root
  listings now render `Loading…` inside the tree; stale-while-revalidate appends
  `Refreshing…` after the last-good children, and failure replaces only that
  transient row with bounded `Error: …` plus a focusable, accessible `Retry`
  button. Successful reconciliation removes the state row and retains surviving
  object identity/expansion. Per-path scan revisions now own cancellation tokens:
  issuing a newer revision cancels the older token, queued workers recheck after
  acquiring their scan slot and immediately before enumeration/spawn, and a
  running remote list uses the existing process-group watchdog kill path. Bare
  `F5` is captured only within the Files ListView to refresh the current root;
  modified F5 and every F5 while terminal focus is outside Files propagate.
  Regressions cover loading/refresh/error preservation, retryability, queued and
  pre-spawn cancellation, running process-group cancellation, and the strict F5
  modifier matrix.

- **Remote Files bounded listing reconciliation (2026-08-29)**: probe protocol
  v4 gives remote `list` the client-owned `MAX_DIRECTORY_ENTRIES + 1` hard
  limit and stops its loop at that boundary. The extra complete pair is a
  conservative truncation sentinel; only the first 4096 valid rows are kept,
  and root/manual refreshes disclose the bounded prefix. Remote wire names must
  now be valid UTF-8 basenames, with duplicate names/resolved paths rejected,
  so a lossy label can never become an unaddressable operation path. The probe
  classifies `-L` before `-d`, making every symlink (including links to dirs)
  non-expandable. In-place refresh now explicitly restores only surviving
  selected paths, clears any drop-hover widget before ListView recycling, and
  revalidates menu, rename/create and delete-confirmation row identities before
  delayed work starts. Parser, argv, real-script limit/symlink, selection and
  delayed-row regressions cover the protocol and reconciliation boundaries.

- **Remote Files latest-wins refresh (2026-08-29)**: every root, lazy child
  and explicit/operation-triggered directory scan now carries a per-directory
  revision in addition to the existing tree generation, location and execution
  overlay snapshot. A newer request for the same remote path rejects an older
  completion, so a delayed expansion can no longer overwrite a fresher manual
  or post-mutation listing. Context-menu Refresh now targets the clicked
  directory (or a file's parent) rather than always re-listing the tree root.
  Successful refreshes still use the minimal in-place store diff, preserving
  surviving row identity, loaded descendants and expansion; failures leave the
  visible snapshot intact. Pure regressions cover out-of-order revisions and
  child-directory action targeting.

- **Files hidden-entry policy (2026-08-29)**: a focusable, accessible eye
  toggle now hides dot-prefixed rows by default and reveals them through the
  existing `FilterListModel`. It never rescans, changes location authority, or
  discards `TreeListRow` expansion identity; the hidden policy and loaded-name
  query compose in one predicate while the selection model remains stable.

- **Foreground SSH → Remote Files 4.6 (2026-08-27)**: the existing single
  window heartbeat now observes the active leaf's real foreground process tree
  through `jterm_core::process::observed_ssh_command`, so Block, Unified and
  VTE recognize direct SSH and provenance-checked jsh upgrade launchers without
  trusting terminal text or OSC lifecycle completeness. The shared parser
  rejects remote commands and side-effecting/unreplayable options.
  Forge prefers one validated configured filesystem authority, otherwise
  embeds an immutable `FsLocation::Transient(RemoteHostConfig)` marked
  temporary in the selector. Remote home discovery stages off-thread while
  the existing tree remains intact; publication requires the same monotonic
  follow token, active pane/session/foreground target, tab-focus generation,
  file-operation intent/count, tree generation, location/root, exact config
  remap and freshly recomputed transport uniqueness. Success reveals Files without
  persisting a preference, failure offers Retry, unsupported SSH offers the
  profile selector, and SSH exit never forces an already-open tree to Local.
  Explicit and provenance-derived ControlPaths live only in immutable execution
  overlays (probe, scan, operation, clipboard and both transfer endpoints),
  while stable saved/transient identity ignores them. Equivalent saved and
  temporary endpoints paste through direct copy/rename instead of a relay.
  Transient locations use the same final Forge execution gate; their terminal
  bridge is plain interactive SSH with no assumed remote jsh command. Long
  endpoints use bounded middle elision in the selector while their sanitized
  full value remains available as a tooltip.

- **Files Remote Bridge 4.5 (2026-08-27)**: the Files header now has a
  keyboard-focusable, explicitly labelled terminal action. Local opens at the
  visible tree root; an SSH/Docker location opens the same validated managed
  profile at its remote-shell default directory. Remote home discovery moved
  from GTK to the bounded file-op worker, with generation/location rejection
  for late answers and a visible Local fallback on failure. Index-backed tree
  and clipboard locations are reconciled across Settings edits and config
  reload by exact full-profile identity: unique reorders remap; removal,
  replacement, or ambiguity fails closed to Local so old rows can never be
  applied to a different target. Dropdown rebuild notifications are suppressed;
  menu/name/delete intents revalidate their originating generation/location;
  and operation completions refresh only the generation/location they started
  from. Every Copy/Cut has a monotonic intent token: delayed Paste resolves the
  token through the live reconciled clipboard, and a slow cut can retire only
  its own intent (even when a newer intent has identical paths, or the same
  profile moved index). Managed-tab OSC 7 following first distinguishes the
  focused local/remote split leaf, then requires one validated current profile
  match instead of trusting a display name; only restored tabs carry the
  explicit saved-session identity exception through reconnect. Pure regressions
  cover reorder, removal, replacement, invalid/ambiguous profiles, Local
  identity, stale delayed actions, clipboard ABA, and managed-tab replacement.

- **Block Search 4.4 (2026-08-26)**: the capture-phase picker key router now
  confirms only when focus belongs to the query editor or a result row. Every
  other focused widget receives `Return`/`KP_Enter` normally — including
  Refresh/Reset, scope and filter controls, row bookmark stars, and
  `AdwHeaderBar`'s implicit Close button — instead of jumping and closing on
  an unrelated selected result. Query/list confirmation and Shift+Enter
  advance semantics remain unchanged; pure routing and DISPLAY-backed GTK
  focus-classification regressions cover both sides of the allowlist.

- **Block Search 4.3 (2026-08-26)**: exact VTE occurrence jumps now roll back
  transactionally when any native step fails, removing both the search regex
  and VTE's partial selection so an unavailable target cannot leave a wrong
  match highlighted. A real DISPLAY-backed VTE regression covers two
  successful steps followed by a failed third occurrence.

- **Block Search 4.2 (2026-08-26)**: Cross Block Search now has a Reset-aware
  Bookmarked metadata toggle that composes with Failed/Slow/Background before
  scope and the 500-hit cap, including empty-query browsing. A pane-local,
  runtime-only `BookmarkState` centralizes membership and a monotonic revision;
  both Block and Unified results can toggle it from an accessible row star or
  selected-hit `Ctrl+Shift+B`. The physical-keycode latch suppresses repeat and
  modifier-release leakage, while successful toggles preserve selection and
  rebuild all duplicate rows. Block reuses its card toggle so star/CSS and an
  active Bookmarked card filter stay synchronized. Unified prunes only record
  ids actually retired by `record_unified_zone`; snapshot/chrome eviction does
  not remove membership. Nothing is serialized or restored.

- **Block Search 4.1 (2026-08-26)**: the GTK metadata row now includes a
  process-local, Reset-aware Background filter. Block records use
  `BlockData::is_background()` while Unified records use their explicit
  metadata bit; the search adapter normalizes background command, exit status,
  and duration to absent, making command-lifecycle predicates mutually
  exclusive even for contradictory legacy fields. Empty-query Cmd produces no
  synthetic row; All/Out use only the first meaningful line of real retained
  output, and filter plus scope eligibility precede the result cap. The compact
  rows remain inside real automatic horizontal overflow with unchanged Tab
  order. Cross Block close paths now retain the single dialog-slot claim until
  the owning `closed` callback, which also identity-guards memory persistence;
  a fast close/release/open sequence cannot create or later lose a replacement.

- **Block Search 4.0 (2026-08-26)**: the window capture controller now routes
  the configured `block:search` action through a hardware-keycode latch shared
  with the in-dialog fallback. The opener toggles once, repeats are consumed
  through physical release even if chord modifiers are released mid-press, a
  fresh press still closes, and window deactivation clears a release that the
  compositor may have dropped. The GTK title bar now
  keeps only Refresh and Reset; matching/scope and metadata filters occupy two
  compact rows inside explicit automatic horizontal overflow, so theme/font
  growth in narrow windows remains scroll- and Tab-reachable without clipping and
  still leaves a dedicated metadata row for Background parity. Manual refresh
  sets and explicitly announces `Refreshing blocks…`, crosses one complete
  painted frame, then runs
  the synchronous selection-preserving rebuild on the next tick. A newer
  intent or click cancels the superseded frame callback, and query refocus plus
  the existing F5 modifier/release semantics remain unchanged.

- **Block Search metadata parity (2026-08-26)**: Forge now exposes the
  existing Failed and Slow record predicates in the GTK palette, persists them
  with the other process-local search intent, and clears them through Reset.
  Either filter works with an empty text query by yielding one representative
  row per eligible retained block; predicates run before the 500-hit cap, and
  filter-only activation navigates without installing an empty VTE matcher.

- **Block Search 3.9 (2026-08-26)**: the GTK search header now exposes a
  pointer-accessible refresh button with an accessible action name and `F5`
  shortcut. Clicking it, or pressing unmodified F5, synchronizes the automatic
  version probe, cancels pending debounce, and immediately performs the same
  selection-preserving rebuild before returning focus to the query. F5 with
  Ctrl/Shift/Alt/Super/Hyper/Meta continues propagating unchanged. A physical
  F5 press is latched until release, so auto-repeat cannot rebuild repeatedly
  and releasing a chord modifier while F5 remains held cannot trigger refresh;
  leaving the dialog focus domain resets the latch if GTK drops the release.

- **Block Search 3.3 (2026-08-26)**: Shift+Enter now reveals a live terminal
  hit, keeps the GTK palette open, restores query focus, and advances only
  after that successful reveal. Snapshot-only hits still open their snapshot;
  unavailable hits retain selection and diagnostics instead of fake-stepping.

- **Block Search 3.2 (2026-08-26)**: result navigation now wraps with arrows,
  jumps to either edge with Home/End, pages ten rows with PageUp/PageDown, and
  scrolls the selected GTK row into view without stealing query focus. The
  accessible status reports the current position as well as total/cap state.

- **Block Search 3.1 (2026-08-26)**: `All / Cmd / Out` scopes now restrict
  cross-block scanning before the 500-hit cap, so an excluded surface cannot
  starve the requested one. The GTK dropdown and `Ctrl+O` cycle rescan the
  retained records without changing activated VTE highlight semantics.

- **Block Search 3.0 (2026-08-26)**: cross-block search now composes `Aa`
  case sensitivity, bounded regex, and Unicode whole-word matching. The GTK
  result scan and activated VTE/PCRE2 highlight carry one typed options value,
  so a row cannot be found case-sensitively or as a whole word and then jump
  under the old case-insensitive substring semantics. `Ctrl+I`, `Ctrl+R`, and
  `Ctrl+W` keep every control keyboard reachable while the query retains focus.

- **Core-owned Agent claim durability (2026-08-25)**: Forge now pins
  `jterm_core` `21437ba` and its matching `jagent` `a462ec8`. Core durably
  syncs retirement of the public Agent snapshot name before exposing a live
  session and owns post-consumption cleanup. Forge therefore removed its
  redundant post-`Restored` directory-sync failure gate, which could otherwise
  discard a session after core had already consumed its only snapshot. The
  compatibility re-exports for the legacy read/remove/best-effort claim helpers
  remain in one narrowly scoped `#[allow(deprecated)]` use so downstream source
  paths do not break before a major release; Forge's own panel uses only the
  typed, durability-owning claim path. The
  direct and transitive `jagent` pins remain identical; encoded provider,
  streaming, text-action, and native-tool JSON now reject duplicate members.

- **Shared workspace pane identity (2026-08-25)**: restored pane `sid` values
  now use `jterm_core`'s exact 1..=128-byte ASCII `[A-Za-z0-9_-]` contract.
  Valid 128-byte identities survive intact; dotted, Unicode, control-bearing,
  and oversized snapshot values are regenerated before pane routing.

- **Search/filter correctness and core repin (2026-08-24)**: card search
  surfaces now validate their render stamp before every outcome, including an
  already-selected one-hit edge, and palette occurrence jumps fail closed when
  the exact match cannot be reached within 4096 native steps. Filtered cards
  remain zero-height through viewport, virtualization, and failure-marker
  calculations; bookmark mutation reconciles an active Bookmarked filter from
  both menu and keyboard paths. The app pins published `jterm_core` `21437ba`;
  its transitive `jagent` `a462ec8` is also the direct Forge pin, avoiding
  duplicate crate identities.

- **Exactly-once command lifecycle closure (2026-08-21)**: Block and Unified
  now share one observer-side `C -> finish` latch. An accepted `D` consumes it
  with `shell_reported` evidence; if `D` is lost, only a foreground-shell `A`
  consumes it with `boundary_inferred`/`degraded` evidence and `None` for exit
  status and duration. The inferred fan-out runs before that same `A` finalizes
  the backend record, preserving the normal `D -> A` ordering. Repeated `A`,
  background output, prompt-owned alternate screens, and RIS cannot mint a
  finish without an accepted `C`; RIS remains invalidation, not completion.
  The running-command display copy, engine-owned command identity, and Agent
  correlation remain available for prompt-trust rollback and later
  Block/Unified finalization.

- **Inherited-environment freeze (2026-08-15, fourth round)**: the local
  `src/child_env.rs` is deleted and both spawn paths run on
  `jterm_core::child_env`'s frozen launch snapshot.
  `capture_inherited_environment()` is the first statement of `app::run`
  (before `identity::init` and `cli::handle_early_args`; every `set_var` —
  the FORGE_* flags and the input-method writes — runs strictly after, and a
  capture failure is fatal per core's contract). `pty.rs` builds the
  block-mode child block with `envp_from_captured`; `terminal.rs` uses
  `vte_envv_from_captured` and ORs `VTE_SPAWN_NO_PARENT_ENVV_BITS` into the
  spawn flags so libvte cannot re-merge the live environment. A non-UTF-8
  inherited variable makes the strict conversion fail; that path rebuilds
  the envv from the frozen block with the offending entries scrubbed and
  keeps the flag, rather than falling back to the live environment. Coverage
  is scoped to terminal/PTY children: notebook cell workers and the
  flatpak-spawn bridge still start from the live environment (pre-existing).
  Adversarial review verified the capture ordering across every entry path
  and caught the scrub-fallback misdiagnosis plus two overstated comments;
  one accepted test weakness remains — the spawn tests tolerate an
  `AlreadyExists` capture race, so the test binary's frozen snapshot can
  contain another test's env mutation (no assertion depends on it).

- **Deduplication round 3 (2026-08-15)**: repinned to `04f6328` and deleted
  the last two diverged local modules. `src/snapshot_file.rs` is gone (all
  callers on `jterm_core::snapshot_file`, including the now-public
  `read_bounded_private`). `src/command_history.rs` is gone: core upstreamed
  `read_recent_with_status`/`RecentHistory`, and the module's extra
  absolute-path check was redundant with the config layer's
  `normalize_history_path`. Ctrl+Click link opening now delegates to
  `jterm_core::link::is_openable_url` — a deliberate tightening (2 KiB cap,
  userinfo and backslash refused; all three `open_uri` callers pass untrusted
  child output, so nothing legitimate breaks; a >2 KiB URL still highlights
  but is refused with a log line, the same silent-refusal UX as before). The
  doctor's Flatpak probe runs through `helper::bounded_command_output`, so
  `command_status_with_timeout` left the host shim. Adversarial review caught
  two stale comments (fixed) and noted: core's history validation used the
  narrower spoof set until forge's `review_input` was upstreamed in round 4
  (display-side `safe_inline_display` still sanitizes with the wider set), and
  core's
  `command_history::prepare_path` preflight is available but not yet adopted
  by either app.

- **Deduplication round 4 (2026-08-15)**: repinned to `592d663` and deleted
  `src/review_input.rs`, `src/pty_input.rs`, and `src/execution_journal.rs` —
  core upstreamed the widened spoof table, `safe_inline_display`/
  `safe_multiline_display`, `AdmittedInput`/`admitted_input`, and
  `output_capture_enabled`. Mechanical renames: `contains_visual_spoof` →
  `contains_visual_spoofing`, `contains_noncontrol_visual_spoof` →
  `contains_noncontrol_visual_spoofing`, `is_visual_spoof_character` →
  `is_visual_spoofing_character`. All `valid_jsh_id` call sites moved to
  `execution_journal::is_valid_jsh_session_id`; core's execution-id validator
  (which additionally allows `.`) stays private, and jsh-generated execution
  ids are dot-free, so the two OSC 133 correlation sites
  (`block_view/mod.rs`) keep the session grammar — same acceptance as before.
  Core's `InputGuard` dropped `Clone`/`Copy`; the PTY writer's rollback now
  reconstructs the guard from its one-bit frame state
  (`pty.rs::input_guard_with_frame`) instead of copying it.

- **AI module unification round (2026-08-15, second half)**: `src/ai/` is
  gone, replaced by a thin shim over `jterm_core::ai`; see "AI module: done"
  below. Adversarial review confirmed symbol coverage, the endpoint-safety
  gate ordering, snapshot decode compatibility, and the stronger credential
  staging; it found no defects beyond the stale-handoff lines this update
  fixes.

- **Architecture unification round (2026-08-15)**: the app-owned helper
  runners and the five verbatim duplicate modules (`kitty_graphics`,
  `notebook_text`, `notify`, `core_keybindings`, `atomic_file`) migrated onto
  `jterm_core`; details and the adopted tightenings are under "Remaining
  boundaries" below. A three-agent adversarial review of the migration caught
  and fixed a correction-worker hang on supervised probe-failure paths, two
  helper-construction tests that no longer exercised the function under test,
  a dead `JSH_LOOKUP_PATH` export, and a stale vendored-script leftover.

- **Agent hardening adoption round (2026-08-15/16)**: `src/agent.rs` is a
  pure re-export of `jterm_core::agent`; the panel's hand-rolled claim
  machinery (~860 lines of raw `openat`/`renameat2` audit code) is replaced
  by `try_claim_session_file` under a new `PrivateParentLock` in
  `config_store.rs`, and snapshot writes take the same lock. Behavior
  changes: `AwaitingObservation` checkpoints restore as `Ready` with an
  explicit unknown-result note instead of being silently discarded; invalid
  evidence quarantines under core's `.claimed-*` name; live model replies
  auto-reject unsafe proposals inside the session. Adversarial review then
  drove a core hardening round (pin `cf0dd2c`): the dropped forge audit
  rules (pending-must-be-final-turn, approved-requires-observation,
  turn-counter arithmetic, final-turn/state matching) are now enforced by
  core's pre-restore validation for both apps, and claimed snapshots are
  read with `read_bounded_private` (a group-readable snapshot quarantines as
  tampering). Accepted trade-off, reviewed and kept: snapshot load/save now
  serialize with config saves through the shared directory flock, so a
  contended `persist()` at window close can stall the GTK thread up to the
  2s lock timeout and then skip the write with a `log::warn!` (the old
  lock-free write could race a claim instead). Forge's own
  `task_epoch`/proposal-id plumbing already covers stale-callback rejection,
  so core's `AgentSessionEpoch` remains additive and unadopted.

- **Parser unification round (2026-08-15)**: repinned to `73c1411` and
  deleted `src/parser.rs` — core upstreamed the parser verbatim (only
  `crate::` → `jterm_core::` path prefixes and rustfmt line wraps differ),
  including `ParserEvent::AgentIntegrationReady`/`EraseScrollback`/
  `HardReset`, strict ST-only APC/DCS/PM/SOS termination with
  abort-and-reprocess, SOS/RIS handling, and `is_erase_scrollback`.
  `src/lib.rs` keeps a `pub mod parser { pub use jterm_core::parser::*; }`
  re-export (the `exit_status`/`redact` pattern), so every `crate::parser::…`
  and `forge::parser::…` path — `block_view/mod.rs`,
  `tests/regression_parser.rs` — compiles unchanged, and the exhaustive
  `ParserEvent` match in `block_view/mod.rs` already covered the three new
  events. Zero behavior change; all release checks pass.

Nine organism evolution rounds, in order:

- **Inner life** (`d6fb8b4`): habituation (the Nth clean pass of the day
  celebrates at `1/(1+prior/4)` strength) and sensitization (first crack
  after ≥5 clean runs stings more); repo familiarity gradient
  (`RepoArrival` from remembered day records — shy in unknown checkouts,
  「回来了。」in well-known ones, keyed on resolved repo identity so a
  root/cwd key flap cannot re-fire it); session-local any-command rough
  streak (≥3 consecutive non-zero exits → silent `SitNearError`);
  pane-scoped content-free accept/dismiss pulses from the correction card;
  fixed-width sticky-header micro-poses.
- **Continuous life** (`14ef832`): the prototype's `LifeState::tick`
  homeostasis ported into the native reducer, driven by a window-shared
  `OrganismActivity` aggregate and tick clock that hands each wall-clock
  slice out exactly once however many pane bodies exist; forced micro-rest
  below 0.15 energy keeps exhaustion self-limiting; the agent-lost recovery
  path returns its running-command slot.
- **Body language** (`36d0bd5`): multi-frame sprite sets (gait, tail
  flicks, sparkle blinks, dozing) with per-set stable bounding boxes so the
  fail-closed fit check never flaps; `BodyLanguage` quantizes the
  continuous state (drowsy/tense/listless) into ambient poses and wander
  tempo; `Calm` tempo reproduces the original 80-second cycle frame for
  frame.
- **Autonomous mind** (`5cde9fc`): utility-scored ambient dispositions
  (Sleep/Explore/Approach/Idle) with incumbent inertia, per-body xorshift
  jitter, and hold timers; sleeping feeds rest back into the tick — gated
  on no command running anywhere so one sleeping body cannot recharge the
  shared mind mid-build.
- **Agent awareness** (`74991f2`): `TermView::agent_command_active()`
  exposes the identity-verified agent generation as one bool;
  agent-driven commands get the crouched `WatchAgent` pose and quiet
  half-strength nods — `CelebrateBig`, speech, and the human's confidence
  stay reserved for commands the human typed; coarse `AgentPulse` phases
  feed social need/attachment (giving Approach its niche);
  correlation-loss events resolve one main-loop turn later so an
  authoritative finish always wins with attribution intact.
- **Spatial continuity** (`853b9a9`): `approach()` interpolation — the
  body walks to its next pose in whole-cell steps (quarter of remaining
  distance per frame) instead of teleporting; every hide path (typing
  retreat, alt-screen, fail-closed, resize/font reflow via a surface
  signature) forgets the standing spot so reappearance snaps — it never
  walks while invisible; the watching pose perches above the output growth
  edge, following `cursor_row` (a content-free geometry scalar) down.
- **Motion restraint** (`4339e77`): `ascii_organism_motion =
  "full"|"calm"|"static"` defaulting to the desktop animation preference;
  the frame driver moved off the GTK frame clock (a tick callback forces it
  to run at full rate forever) onto a self-rescheduling glib timeout — ≤10
  wakes/s active, a 0.9s heartbeat after a minute of window rest, full
  cadence within one beat of any input. The fired-source slot is cleared
  before every other guard: a runtime outliving its pane must never let
  `Drop` remove a dead source (glib panics).
- **Long-command companionship** (`00a099e`): a command past ten seconds
  counts accompaniment time on its card ("· 2m 30s in", ten-second steps
  so the accessible status region is not narrated every second); past a
  minute the watcher settles into a lying vigil (`WatchSettled`).
  Successful build wall times accumulate into two saturating per-repo/day
  scalars (`build_duration_sum_ms`/`build_duration_count`) over the
  established `#[serde(default)]` migration path — capped at six hours per
  sample, exactly-once per event id, deliberately outside the observation
  replay so compaction cannot touch them; after three samples a build 2×
  off its repo baseline (and >10s off) earns one quiet sentence.

The continued pass adds six more coherent layers:

- **Flaky-test memory**: replay derives same-day failure→success flips across
  retained and compacted observations. Once the day has at least three flips,
  a current one-failure recovery says only「像是偶发的。」and explicitly normalizes a
  stale cross-window `CelebrateBig` to the ordinary celebration. The new
  daily/baseline counter has strict legacy-presence migration, duplicate-field
  rejection, bounded validation, and never repairs other corrupt summaries.
- **Safer perception and space**: native reducer entry points now accept only
  `CommandKind`; command strings stop at the UI classifier. Build pace compares
  against history snapshotted before the current event and before record
  eviction. Watching/reacting bodies prefer the blank band below the output
  cursor and hide if a complete sprite cannot fit; pose dimensions join the
  surface signature so interpolation cannot cross the scrollbar gutter after
  a width change. Inline pose width is fixed, and calm/static run on the low
  frequency heartbeat even while active. Reaction holds are semantic rather
  than one fixed eight seconds (quiet pass 1.8s through repeated-error sit
  10s), while any new lifecycle event still interrupts immediately. Command
  reactions and live-only cross-pane cues each reset to their canonical first
  frame without disturbing the continuous ambient wander phase.
- **Learned circadian rhythm**: each repo/day stores eight bounded three-hour
  completion buckets with event-time local day/bucket frozen together. The
  window-shared mind infers one concentrated circular nine-hour work session
  from the previous 28 days (minimum three active days/six samples, strict
  majority) and eases waking energy toward different in-hours/off-hours
  targets. Unlearned behavior is exactly the old drift. The first human command
  in each window-local work session gets a time-appropriate quiet greeting
  (daytime「早。」, evening「来了。」), unless a more specific repo-home line
  already exists; night sessions spanning midnight greet once, Agent/outside
  commands do not consume it, and failed cross-window refreshes retry instead
  of blessing stale state.
- **Lifetime growth**: top-level `days_seen` and `lifetime_recoveries` counters
  sit outside the evicting repo/day records. A 64-day sorted ledger plus a
  compaction cursor deduplicates work days across repos and makes late closed
  history growth-neutral; recovery episodes advance from replayed flip-count
  deltas, so out-of-order insertion remains correct while its daily ordering
  is retained. Evicting a record with build history closes that date prefix;
  later events there stay valid but cannot count the same lifetime episode
  twice. Legacy v1 data rebuilds a strict lower bound only when all three
  fields are absent. The shared badge
  names juvenile (<7 days), adult, and seasoned (≥60 days and ≥12 recoveries);
  a seasoned human `CelebrateBig` keeps its behavior/tone and says only「嗯。」.
  Celebration frames also retain the cat's ears instead of switching to the
  old human-shaped figure.
- **Single focused presence**: pane reducers, inline cards, and sticky forms
  remain local, but a window-shared monotonic-token/weak-registration arbiter
  lets only the genuinely focused local Block pane own the spatial body. It
  resolves the current page with `focused_leaf()` (never fallback
  `active_leaf()`), gates on window activity, hides every old desired surface
  before refreshing a new owner, and registers Static panes so they can revoke
  a prior body without rendering one. Notebook switching synchronously revokes
  the old owner inside the pre-commit signal, then lets the tab idle resolve
  the new page; focus-widget/window signals, post-switch reconciliation, and
  the existing one-second pane poll keep close/zoom/tab
  topology self-healing without strong-reference cycles. A non-zero
  authoritative finish in a registered background pane now sends only a typed,
  content-free failure pulse to that owner: an actually visible idle body
  briefly `GlanceAside`s without changing either pane's reducer/card/sticky
  state. Busy, hidden, Static, alternate-screen, owner-local, unknown, and
  successful outcomes drop the pulse rather than queueing stale attention.
  Only an active, primary-screen Full owner keeps the 100 ms animation
  cadence; non-owner, alternate-screen, Calm, Static, and resting runtimes use
  the 900 ms heartbeat. Focus transfer safely rearms the new owner without
  ever creating a second pending GLib source.
- **Recovery vigil**: the existing day/repo `recovered_pending_push` fact now
  remains a visible intention after the celebration settles. The body guards
  a fixed-width `[ok]` pose beside the prompt (with matching inline/sticky
  forms) until a successful push closes the loop; clean builds, unrelated
  commands, failed pushes, and push dry-runs preserve it, while a new build
  failure or local-day rollover clears it. Leaving the checkout releases the
  pane-local pose without erasing memory, so same-day re-entry can restore it;
  raw cwd signals handle ordinary path exits immediately, while nested-repo
  identity is resolved at the next semantic command. The reducer mirrors
  memory's preserve-until-push semantics, then reconciles the memory layer's
  post-replay truth and broadcasts it only to same-window panes already bound
  to the exact repo/day. The low-frequency frame clock notices midnight even
  without another command. Exhaustion still overrides the intention with real
  sleep and a separate wake threshold prevents boundary flutter. Command
  classification unwraps a bounded chain of common wrappers; a push whose
  option tail was truncated fails closed instead of hiding a late dry-run.
  No schema field was added.
- **Five-round debugging vigil (current working tree)**: recovery now has its
  missing front half. One or two unresolved repo/day build failures settle to
  a fixed-width `[!]` `GuardFailure`; three or more settle to the lower,
  silent `[!!]` `GuardStuck`. These idle intentions anchor only in a complete
  blank band below the terminal output edge and fail closed to the inline card
  when the sprite cannot fit; recovery/cautious recovery stay beside the
  prompt. A third same-day failure→success flip turns pending recovery into
  `[?]` `GuardCautious` without claiming the current test is flaky. A typed
  `RepoWorkState { open_failures, recovered_pending_push,
  failure_success_flips }` is the content-free post-replay truth shared with
  exact same-repo/day panes; `MemoryInsight.open_failures` deliberately
  remains the event-position depth used by the immediate reaction when
  `event_order_exact` is true; compacted-prefix late events are explicitly
  fail-neutral. Duplicate, unknown, failed-push, late, and out-of-order paths
  all return the final work snapshot; visible replay descriptions are rebuilt
  from that event position instead of retaining stale sensitization or count
  text, and final open work vetoes stale recovery/push closure speech. The
  source pane reconciles even a
  queue-rejected preview, but only an event admitted to disk/retry ownership
  broadcasts to siblings; admission is decided under the queue lock and can no
  longer race a fast worker acknowledgement. Start and finish each freeze one
  wall-clock sample for context, reducer, day, bucket, and event construction;
  finish refreshes first so every context event is an ordered predecessor of
  that sample.
  Every pane performs its own midnight retirement even though only one pane
  advances shared physiology. A durable vigil cancels an already-running
  cross-pane `GlanceAside`; forced vigil sleep retains output clearance and an
  owner-only logical rest claim lets Static/fail-closed bodies cross the wake
  hysteresis without multiplying recovery; sync, context exit, midnight, and
  focus transfer reconcile that claim immediately. Non-Git loops are
  conservatively scoped to exact raw cwd. Identity-preserving Git global
  options such as `--no-pager` and `-c` can still reach a real push, while
  `-C`/`--git-dir` and bounded-parser truncation fail closed. Accepted human
  input now hides the live body for the
  whole 900 ms retreat window and returns by snap, so typing never triggers a
  distracting run. No schema field or content-bearing perception was added.
- **Five-part embodiment pass (current working tree)**: lifetime stage now has
  a visible phenotype as well as a badge — large eyes and quicker micro-motion
  for juvenile, the existing adult silhouette, and a notched ear with slower
  cadence for seasoned. A composable render context keeps semantic behavior,
  quantized `BodyLanguage`, `GrowthStage`, and content-free `OutputRhythm`
  independent instead of multiplying reducer states. Every phenotype variant
  in a pose family retains the exact bounding box, and semantic reaction marks
  remain canonical. Output rhythm exists only in memory: the most recent three
  pulses inside roughly 1.2 seconds read as busy, roughly three quiet seconds
  (including a command with no output yet) as waiting,
  and returning output gets a roughly 0.9-second resume acknowledgement;
  existing commands past 60 seconds still use `WatchSettled`. None of those
  states inspects bytes or infers a command result.

  Full motion connects selected semantic boundaries with four fixed-envelope
  frames: error reaction→failure vigil, celebration→recovery or
  cautious guard, settled watch→celebration, and recovery guard→post-push rest.
  Calm snaps and Static stays card-only. A newer event, typing retreat,
  alternate-screen yield, and fail-closed sizing always preempt transitions.
  A process-local stable hash of an already canonical repo identity chooses
  the preferred nest side and route; an unfamiliar repo receives one short
  post-settle exploration when no higher-priority vigil conflicts, otherwise
  that exploration is dropped rather than replayed later. It never displays the path, enters the reducer, or
  adds a persisted field. Finally, a window/session-local attention arbiter
  admits failure/vigil before closure/recovery/push, then long-command changes,
  then greetings/insights. An admitted expression owns a shared focus window
  and starts its cue-local cooldown; suppressed expression is dropped, never
  queued, while durable repo facts remain intact.

Hard lines the series preserved, verified every round: perception only
ever widens by enums, counters, or geometry scalars — command text, output,
and keystrokes never cross the reducer or persistence boundary; the disk schema
stays version 1 with `#[serde(default)]` incremental migration (old files load;
newer files fail closed in older binaries, matching the
baseline/observations precedent); the three rendering invariants (O(1)
typing retreat, immediate alt-screen yield, fail-closed sizing) are
untouched; reaction intensity is a function of history, not of the
stimulus; big celebrations and speech belong to the human's own commands.

## Remaining boundaries

### Helper-runner consolidation: done

Forge now pins `jterm_core`
`21437ba6f0cb85e74d4ce2a03ef1857de2c55d9d`. The app-owned helper
runners are migrated. `src/host.rs` is now a thin shim
over `jterm_core::host` (`pub use` + `APP_ID` + `interactive_bash_path`) whose
only local code is `pub(crate) helper_command`, kept for the two callers
still outside the core's scope (the CLI doctor's command construction and
the command-correction probes — the doctor's bounded run itself goes through
`jterm_core::helper::bounded_command_output`); `src/git_meta.rs` is a
re-export plus the forge-only
`read_cached_and_refresh` UI cache worker; `src/jsh_install.rs` is a pure
re-export; `src/ui/command_correction.rs` probes run on the now-public
`jterm_core::supervised::SupervisedChild`. Intentional tightenings adopted
from core: helper resolution requires a canonical, non-user/group-writable
target with a `PATH=/usr/bin:/bin` child clamp (note: this also rejects
group-writable root-owned dirs such as Debian's `/usr/local/bin` 2775 — a
deliberate core policy, shared with anvil), `install_argv` execs `/bin/sh`
under core's `RUN_WRAPPER`, and script staging uses core's `VendoredScript`.
On supervised probe/reap failure paths the correction worker detaches its
output reader instead of joining it, because a disarmed (unsignalled) group
can hold the pipe open forever. `scripts/install-jsh.sh` was deleted (the
vendored copy lives in core). The verbatim modules `kitty_graphics`,
`notebook_text`, `notify`, `core_keybindings`, and `atomic_file` were deleted
in favor of the core equivalents.

### AI module: done

`src/ai/mod.rs` + `src/ai/conversation.rs` (~5400 lines) are replaced by a
160-line `src/ai.rs` shim over `jterm_core::ai`: pure re-exports plus one
local `client_from_config` that gates on `config::ai_base_url_is_safe` before
constructing the client, compensating for core's `AiClient::new` not
validating the endpoint at construction (core fails closed at request-build
time instead). Core's conversation decoder is byte-compatible with forge's
on-disk snapshots (v1 legacy and v2, identical budgets), and its credential
staging (`atomic_file::write_atomic`) and supervised curl transport are
strictly stronger than the deleted nonce-temp/kill-and-reap code. Known
diagnostic-only quirk: a hand-constructed Config with an unrecognized
provider string now reports the endpoint-safety error instead of "unknown AI
provider" (unreachable through normal config load, which normalizes unknown
providers). `config::ai_api_key_file_env_override` (forge's own resolver,
with its absolute-path filter) remains untested — pre-existing gap. The host
shim's `helper_command` lost its biggest consumer (curl) but stays for the
doctor and correction probes.

### Follow-up migrations (next rounds)

- Forge-ahead local modules to upstream into core rather than delete: none
  left. `ui::ai_chat_store` was the last one and went up this round as
  `jterm_core::ai::chat_store` (1,536 lines down to a 75-line policy shim; see
  the entry above), joining `parser` (OSC 7771, erase-scrollback/hard-reset
  barriers, strict ST termination), upstreamed and deleted locally at pin
  `73c1411`, and, earlier at pin `592d663`, `review_input` (safe-display
  helpers, wider spoof set), `pty_input` (`AdmittedInput`) and
  `execution_journal` (`output_capture_enabled`).
- Core-ahead modules: `chat_store` lands with two facilities forge
  deliberately declines — `BusyChatPolicy::Refuse`, which is core's default
  and the anvil/ember/frost archive/delete semantics, and
  `recover_retry_payload_detaching`, anvil's shutdown path — because forge's
  panel cancels the in-flight request before it mutates or recovers anything.
  Core's additive `AgentSessionEpoch` also remains unused (forge's own
  `task_epoch` plumbing covers the same ground) on the adopted hardened
  `jterm_core::agent` session wrapper, and `child_env`'s
  inherited-environment freeze stays wired (`app::run` captures first,
  `pty.rs` uses `envp_from_captured`, and the VTE spawn pairs
  `vte_envv_from_captured` with `VTE_SPAWN_NO_PARENT_ENVV`).
- Pin bookkeeping, which this round leaves open: the two "done" sections above
  quote the pin current at their own round (`21437ba`), and `Cargo.toml` pins
  `1f5f0fb` today — neither carries `ai::chat_store`, so the working tree
  builds only through a temporary local `[patch]`. Bumping the pin to the core
  commit that carries the module also means regenerating `Cargo.lock` without
  that patch (its two dropped `source = "git+…"` lines are the patch's only
  trace) and refreshing `flake.nix`'s `outputHashes` entry for
  `jterm_core-0.2.0`.
- `src/pty.rs:1967` and `src/state.rs:2118` spawn `sh` directly, outside the
  helper-runner contract — both inside `#[cfg(test)]` helpers, and the line
  references this bullet used to carry (`1579`/`1953`) had drifted;
  `src/notebook.rs` long-lived terminal children keep their own wait logic by
  design (`SupervisedChild` scopes itself to short-lived helpers).

## Release checks

```text
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features --no-fail-fast
cargo test --locked --doc
cargo clippy --locked --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features
```
