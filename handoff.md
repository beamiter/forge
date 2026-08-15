# Engineering handoff

Updated: 2026-08-15 (AI module round)

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
  two stale comments (fixed) and noted: core's history validation uses the
  narrower spoof set until forge's `review_input` is upstreamed (display-side
  `safe_inline_display` still sanitizes with the wider set), and core's
  `command_history::prepare_path` preflight is available but not yet adopted
  by either app.

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

Both apps now pin `jterm_core`
`04f63283090591d9ad88500224e848dbb69b1f61`. The app-owned helper
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

- Forge-ahead local modules to upstream into core rather than delete:
  `review_input` (safe-display helpers, `valid_jsh_id`, wider spoof set),
  `pty_input` (`AdmittedInput`), `execution_journal` (`output_capture_enabled`),
  and `parser` (OSC 7771, erase-scrollback/hard-reset barriers, strict ST
  termination).
- Core-ahead modules not yet adopted: `child_env`'s inherited-environment
  freeze and the hardened `jterm_core::agent` session wrapper.
- `src/pty.rs:1545` and `src/state.rs:1953` spawn `sh` directly, outside the
  helper-runner contract; `src/notebook.rs` long-lived terminal children keep
  their own wait logic by design (`SupervisedChild` scopes itself to
  short-lived helpers).

## Release checks

```text
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features --no-fail-fast
cargo test --locked --doc
cargo clippy --locked --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features
```
