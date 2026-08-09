# Engineering handoff

Updated: 2026-08-09

This baseline lands the nine-round "Evolve ASCII organism" series
(`d6fb8b4..00a099e`): the experimental Block-pane organism grew from an
event reflex into a continuous life simulation with utility-selected
behavior, agent attribution, interpolated spatial motion, motion restraint,
and a persistent per-repo build-duration baseline. Every round followed the
same loop — implement, unit-test, sync `docs/USER_GUIDE.md`, run a
three-agent adversarial verification workflow, fix confirmed findings,
commit, push. The verification pass caught one to two real defects almost
every round (attribution races, a Drop panic on a fired glib source, a
saturation hole in a validate invariant); do not skip it.

## Completed since the previous handoff

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

Hard lines the series preserved, verified every round: perception only
ever widens by enums, counters, or geometry scalars — command text, output,
and keystrokes never cross the organism boundary; the disk schema stays
version 1 with `#[serde(default)]` incremental migration (old files load;
newer files fail closed in older binaries, matching the
baseline/observations precedent); the three rendering invariants (O(1)
typing retreat, immediate alt-screen yield, fail-closed sizing) are
untouched; reaction intensity is a function of history, not of the
stimulus; big celebrations and speech belong to the human's own commands.

## Remaining boundaries

### Organism roadmap (long-term, design settled in review)

- **Flaky-test insight**: derive a same-day failure→success flip count in
  `replay_observations`; one `#[serde(default)]` saturating counter each on
  `DailyStats` and `StatsBaseline` so compaction stays correct. ≥3 flips
  with open-failure depth ≤1 reads as flaky: one Quiet sentence on success,
  no pose escalation — the human should suspect the test, not themselves.
- **Circadian clock**: 8×3-hour `u16 activity_buckets` per `DailyStats`
  (count-only, accumulated in `apply_event` outside the replay, validated
  and re-proven against the 512 KiB budget); infer habitual working hours
  from recent day records; lower the tick's energy target outside them,
  greet the first in-hours command with「早。」.
- **Growth stages**: `days_seen` and `lifetime_recoveries` saturating
  `u32`s on `DiskMemory` — the `days` window truncates at 64 records, so
  the organism structurally has no past older than 64 days without them.
  Derive juvenile (<7 days) / adult / seasoned (≥60 days + recovery
  threshold) stages: sprite scale and ever-terser speech (a seasoned
  `CelebrateBig` says only「嗯。」). Value accrues over months.
- **Single presence across panes**: only the focused pane shows the full
  body; unfocused panes keep at most the sticky micro-form; focus changes
  (content-free pulses from the pane-focus flow) drive walk-out/walk-in
  frames over the existing interpolator; a background pane's failure earns
  a `GlanceAside` from the focused body —「隔壁那格红了」— never a popup,
  never a focus grab. Largest remaining item; depends on nothing else.

### Consolidate the app-owned helper runner

Carried forward unchanged: Forge still has app-local trusted-helper,
notification, command-correction, and jsh installer runners while the
shared core has the stronger WNOWAIT/process-group/deadline contract.
Migrate only after core exposes an opaque runner rather than a mutable raw
`Command`, preserving Forge's Flatpak host-namespace rules and tests.

## Release checks

```text
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features --no-fail-fast
cargo test --locked --doc
cargo clippy --locked --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features
```
