# Four native frontends, one saved Mission

This is a separate acceptance experiment from `five-repository-host-v1`.
The existing host contract runs four protocol clients, not four rendered apps.
This experiment must run the actual Anvil, Forge, Ember and Frost binaries and
observe their visible Mission surfaces: exact identity/revision, repository-bound
candidates and core pins, current missing-validation reasons, and refresh/reopen. A passing build or a successful host
read alone cannot pass native acceptance.

## Explicit limits

Real Agent/provider validation was skipped by the user. Do not install, log in
to, or start a provider. Do not create Task refs, run validation, accept work,
commit or push repository changes, or assert five-repository delivery complete.
T02/T09/T10 real-agent cards stay unverified. This experiment only checks a
shared Mission and retained missing-validation evidence through native UIs.

Use fresh private app homes/configurations, no inherited credentials, no startup
commands and no session restore. Disable AI/Eva according to each application's
actual supported configuration. Ordinary application terminal shells are not
provider execution. Never type a command into those shells.

## Data and executable identity

Use the existing collector's frozen five-repository input and digest checks,
exact public master/CI receipts and source checks. Every native binary and the
independent `jterm-sessiond` must come from this same workflow's exact checked-out
sources, with separate source and executable SHA256 receipts. All four frontends
must pin the compiled core. Publication of new acceptance code changes Forge's
HEAD, so a new freeze and exact-head prerequisite CI are needed before dispatch.
An old accepted snapshot cannot approve new workflow code.

All clients receive one short canonical private `JTERM_WORKSPACE_STATE_DIR`.
An independently built production `jterm-sessiond --jterm-session-host` owns it.
Record and monitor PID, Linux process birth tick, executable and private socket. The helper also
binds its actual protocol request streams to that PID using Linux SO_PEERCRED.
If that process dies or changes, stop the round; frontend auto-start replacement
is not evidence that the original host survived.

## Setup and checks

`mission-native-state seed ROOT STATE PID HOST_BIN RECEIPT` requires an already
running empty isolated host. It declares the exact five acceptance targets using the real production Mission
API, then independently inspects the actual five Git checkouts/public refs and
requires all clean source/publication gates. Declared targets are not evidence. This is explicit test setup, not
a claim that a user created the plan in a native UI. The saved plan has zero Task
refs and retains its required validation check for every repository.

`mission-native-state check ROOT STATE PID HOST_BIN RECEIPT OUTPUT` connects only
to the existing host. It reads and checks the whole saved plan, the exact catalog,
the empty native/conversation catalogs and retained missing validation. It never
inspects repositories, saves, adopts, starts processes, or accepts work. Its
`passed` status is explicitly a host-state result, not a native UI result.

The native reader may only open Missions, select the existing exact plan, read
its visible identity/candidate fields and reasons, explicitly Inspect (read-only source observation),
Refresh and reopen the surface/application. It may not Create, Save, Freeze,
Adopt, Link, Start or Validate. Capture genuine public pointer/keyboard actions,
owned-window/pid evidence, screenshots and read-only accessibility/OCR output.
A database/API snapshot cannot substitute for required visible evidence. Other
values such as clipped checkout paths or wrapped goals are supplementary only,
and a bare occurrence of a hash is not proof of a specific repository field.

Refresh or reopening revokes fresh inspection authority. Do not force old
validation-reason text to remain after evidence expires. An explicit new source
inspection may restore current source observations, never validation completion.

## Status and result scope

The latest executed round, [42228324473, attempt 1](https://github.com/beamiter/forge/actions/runs/42228324473),
**failed native acceptance**. Its five production builds and 21 prerequisite
checks passed. The final independent host check passed with the full saved plan
unchanged and both native/conversation catalogs empty. No complete native
Mission inspection, refresh or reopen sequence passed.

The first Anvil/Forge captures were still blank before accessibility registration;
Ember reached the selected Mission before its accessibility tree changed during
observation; Frost's visible Missions entry had a middle dot read as a hyphen by
OCR. These are retained failures, not passes inferred from source or screenshots.
Pure observer regression tests do not replace another exact-head native run.
Frost now displays the stable Mission ID, but its exact identity/revision and
repository-bound fields must still be observed in every required stage.

Results must distinguish:

- production API setup and final host-state integrity;
- each real frontend's exact visible checks and normal close/reopen;
- Forge has no separately tested Mission Back control; whole-app reopen is required;
- Frost OCR does not prove the Freeze button is disabled; that stays unverified;
- source/CI rechecks;
- provider execution: `skipped_by_user`;
- hosted Task validation: `not_run`;
- five-repository delivery: `not_completed`.

Only the specific native checks actually executed may be marked passed. Preserve
failed or unexecuted phases and their reasons; never fill missing observations
with assertions from source code or an earlier CI run.

## Observation readiness

X11 window mapping is not proof of a painted frame or an accessible tree.
Each observation has at most 20 attempts within 15 seconds, also bounded by the
unchanged 300-second per-app deadline. An unregistered accessibility root, a
validated temporary tree update or a still-flat first frame discards that whole
attempt. A retry captures a new screenshot and rechecks the host, process,
window and public tree. It never reuses a discarded attempt's image for a later tree.
Ambiguous roots/windows, foreign or cyclic edges, changed ownership, malformed
data and tool/protocol failures remain hard errors.

Frost's small middle dot may be read as a hyphen only in the public Missions
sidebar badge and its neighboring Needs-you badge anchor. This is a
control-specific OCR rule, not a correction of Mission
IDs, candidate hashes, core pins or other text. The whole high-confidence line,
sidebar context, actual word boxes and two matching observations must identify
one control before pointer input is allowed.
