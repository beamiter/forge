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

The current prerequisite contract requires 22 exact-head workflows: the base
20 now include core `security.yml`, plus the existing Forge/Frost security gates.
The historical rounds below used 21 and are not relabelled. Core Security accepts
only exact-master push or explicit dispatch; all other event restrictions remain.

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

The latest executed round, [42348116404, attempt 1](https://github.com/beamiter/forge/actions/runs/42348116404),
**failed native acceptance** on exact Forge `e4c1228`. Its five production builds
and 21 prerequisite checks passed. The final independent host check passed with
the whole saved plan unchanged, both native/conversation catalogs empty, no
Tasks, and every required validation still `not_run`. Source, installed binary,
process and shared-host identities were checked against the same-run artifacts.
The [final artifact](https://github.com/beamiter/forge/actions/runs/42348116404/artifacts/13564932113)
contains 740 members; its ZIP SHA256 is
`555c1c2e339d63e3b2393a43bdfd0a02efecaf3f9edc41b8730dbcdbdd4df55c`.

- Anvil and Forge opened real windows but found no unique eligible settled open
  control. Neither received any input or opened the Mission surface. The captured
  GTK controls were sensitive and visible without the ENABLED state bit; this
  is distinct from the earlier SCREEN-coordinate and Text capability failures.
- Ember opened and selected the existing Mission, expanded all five repositories
  and completed one explicit Inspect. Both initial and inspected scans bound the
  exact ID/revision, five candidate SHAs and four core pins. The inspected scan
  also retained missing-validation evidence for all five repositories. During
  the subsequent upward scroll toward Refresh, a Text range-extents response
  was rejected as malformed. That failed frame did not record the raw response,
  so its exact geometry cause remains unconfirmed.
- Frost opened and selected the existing Mission but did not complete its initial
  scan or establish an identity binding. Repeated full-frame observation consumed
  almost the entire 300-second application budget, and the final scrollbar-mask
  command timed out with 2.795 seconds remaining. This is not an OCR-confidence
  pass or a completed inspection.

No frontend clicked Refresh, completed normal close or reopened in this round.
The separate real GTK observer fixture passed in ordinary e4c CI; that result
cannot replace any missing application-level checks above.

The earlier [42312173963](https://github.com/beamiter/forge/actions/runs/42312173963),
[42245525353](https://github.com/beamiter/forge/actions/runs/42245525353),
[42228324473](https://github.com/beamiter/forge/actions/runs/42228324473) and the
first formal round remain failed. Their observations must not be combined with
later rounds to claim a completed sequence. Offline pixel replay and pure
observer regression tests do not replace another exact-head native run. Frost
displays its stable Mission ID, but the ID/revision and repository-bound fields
must still be observed in every required stage.

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

## Current observer corrections and verification limits

The GTK control path uses the pinned frontends' actual public state semantics:
Anvil and Forge require SENSITIVE and non-defunct state through their complete
owned widget ancestry. Only the validated application root is exempt. Ember
continues to require both ENABLED and SENSITIVE. Exact allowed labels, supported
button roles, full visibility, stable repeated observations and owned-window
bindings remain mandatory. This does not make an absent ENABLED bit proof that
Freeze is disabled. The real GTK control fixture runs in a separate window so
it cannot alter the existing wrapped-text and OCR geometry fixture.

AccessKit's exact signed-i32 Text range sentinel `(-1,-1,-1,-1)` denotes
unavailable bounds, not a visible or invisible rectangle. Such a range supplies
no visible-text or field proof and does not enter the NotSupported-only OCR
fallback. Other malformed replies still fail with bounded raw-reply diagnostics.
This compatibility correction is based on the pinned AccessKit implementation;
the old failed frame did not retain its reply, so it is not a reproduced cause
or proof that a later native round will pass.

Frost's full-frame scrollbar mask retains the original sRGB Q16 floating-point
comparisons and connected-component geometry. The optimized RGB16 walk keeps
host/deadline checks, rejects malformed extraction and unexpected transparency,
and does not increase the 300-second application budget or observation budget.
Offline comparisons against the actual failed-round frames establish pixel and
component equivalence, not native interaction coverage. OCR, scrolling and the
rest of the application sequence still need a complete exact-head run.

## Observation readiness

The separate ordinary-CI GTK compatibility fixture at
[4c3e4e7 / run 42319455807](https://github.com/beamiter/forge/actions/runs/42319455807)
started and painted a real GTK 4.14.5 window, but failed its first visible-label
assertion. All observed Component SCREEN origins were `(0, 0)`, so the strict
OCR clips did not contain the displayed controls. This is a failed standalone
observer test, not a passed frontend or formal native round. The same screenshot
also showed that `max-width-chars` alone did not make the wrapping fixture
narrow; its later width assertion was not reached.

[GTK 4.14.5's Component implementation](https://github.com/GNOME/gtk/blob/4.14.5/gtk/a11y/gtkatspicomponent.c#L106-L153)
returns zero SCREEN origins, while WINDOW coordinates accumulate actual
accessible-parent bounds. When every Component SCREEN origin is zero, the
observer requires the reader's freshly observed X11 client XID, PID, title and
rectangle. A unique direct top-level frame must match both the observed title
and client dimensions in the same PID-owned accessibility tree. Only that
frame's subtree is translated from WINDOW coordinates using the captured client
origin. Other clients retain raw diagnostic geometry but cannot supply visible
text or input targets. Ambiguous matches, missing bindings, dimension conflicts
and unsupported nested native dialog/window boundaries fail closed. Nested
in-client FRAME containers retain their clipping boundaries.

Raw SCREEN and WINDOW extents, the chosen anchor and the converted rectangles
are recorded. Both raw coordinate sets, the complete tree, node properties,
text, owner and PID are rechecked; the reader separately rechecks XID, title,
PID and client geometry after observation. No decoration offset, scaling or
expected text is used to invent a position. Implementations with nonzero SCREEN
origins keep the existing SCREEN path. Local mock regressions do not prove this
compatibility path works in real GTK; the ordinary fixture and subsequent
formal round must independently pass.

AT-SPI requests keep introspection disabled and use explicit protocol types:
unsigned SCREEN or bound WINDOW coordinates for Component.GetExtents and
Text.GetRangeExtents, signed text offsets for Text.GetText and
Text.GetRangeExtents. Signature tests
use a typed mock when dbus-python is unavailable locally; they are not a real
D-Bus or native-application pass. Protocol errors remain hard failures, with one
explicit capability fallback: only the actual Text.GetRangeExtents call's exact
D-Bus NotSupported error can use same-frame OCR. Other calls, proxy construction
and other errors cannot enter that fallback. Read-only call diagnostics retain
the owner, node path, method, typed arguments and error name.

Fallback OCR never receives expected field text. It uses one fixed grayscale
300% PSM 11 capture per frame and keeps actual word boxes. Every contributing
word must have confidence at least 80 and its complete box must lie inside the
node's observed clip. Complete logical lines must match the node's public text,
allowing only leading/trailing and repeated horizontal whitespace normalization.
It never repairs characters or joins values across frames. Partial or missing
proof remains `unknown_text`, not an empty successful observation. Automatic
visual wrapping of a logical line is not reconstructed; it can still leave
status or candidate text unknown and fail the native contract. Tree, owner,
text, geometry and state checks still apply. Required status evidence must come
from its own Mission control, not another body or terminal string.

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

Frost also preserves full-frame OCR while reading measured public button and
Mission-header crops from the same owned screenshot. The header must contain
the actual Mission ID and revision between the catalog and Title; body text
cannot substitute. Crops never repair identifiers or hashes, lower confidence
80, or choose an ID because it matches the expected plan. Conflicting valid
high-confidence IDs reject the frame. The preserved pre-crop OCR copy is an
additional rejection-only channel for busy, stale or error notices; it cannot
grant identity evidence. Base OCR still supplies public sidebar/catalog anchors.
Refresh and Back require the complete observed toolbar, and Inspect remains
bound to its environment-to-attention band. These pointer targets need two equal
current observations and the existing ownership/deadline checks.

Frost's measured ID crop uses a fixed grayscale 300% PSM 6 pass, without expected
ID guidance. The confidence-80 and conflicting-ID rejection rules stay unchanged.
The blue scrollbar mask is separate from the grayscale button mask. It must
prove a unique high-fill thumb and its observed longer track inside the Mission
scope, with stable geometry in two frames; an adjacent non-blue pixel cannot
silently relax the shape threshold.

Frost wheel input uses the center of a unique observed Mission scrollbar thumb,
not the window's outside gutter. Two current captures must agree on the thumb
and owned window. Cached panel bounds are bound to process birth and window and
are cleared on close, relaunch or a discarded observation. Real-pixel replay
proves recognition and coordinate placement only; actual scrolling and the
complete inspection/refresh/reopen sequence require a later native experiment.


Ember navigation uses bounded direction and measured progress instead of a fixed
rewind distance. A clipped control can inform scroll direction but cannot become
a pointer target. Input still requires two matching visible, enabled, sensitive
observations in the owned Mission scope. Its `Close window` header button must
be the unique direct child of the validated nested Mission frame and lie wholly
inside that frame's header strip; unrelated controls are not made visible.

## Ordinary observer compatibility gate

The ordinary CI job `Native observer compatibility` runs pure contracts normally
and with Python optimization, then a standalone GTK 4.14 fixture in private
X11/D-Bus and XDG directories. The fixture exercises Label, read-only Entry,
TextView, clipped hidden text and a narrow automatically wrapped label, using
the delivered Reader OCR callback. It
records real NotSupported capability diagnostics, checks visible text geometry,
rejects the clipped sentinel, and retains the wrapped logical line as unknown.
It does not launch any terminal application,
production host, provider or real Task and sends no input to the fixture.

The real GTK test is opt-in with `JTERM_GTK_GEOMETRY_SMOKE=1`; ordinary local
discovery reports it as skipped. Explicit opt-in with missing dependencies or
the wrong GTK version fails. CI preserves only fixture JSON/PNG/version evidence,
not private homes or host state. A pass would cover this isolated compatibility
fixture only, never the four-application Mission contract. Local tests on a
machine without GTK are not a real GTK pass.


## Native run 42383439132: observed failure, not acceptance

The exact Forge `4965b2cca714596ff4606bc347e247192c7eb616` attempt passed
preflight and all five production builds. The four-reader runtime failed; it
did not establish four-application acceptance or real Task/provider delivery.
Artifact `13579410747` is 70,404,217 bytes, SHA-256
`a7df71d7c525bbd3f9bcf87fac1a24c0097c9e860698becce2b4ca7d52493b3b`.
Its independent host check passed: the complete saved plan stayed unchanged and
both conversation catalogs remained empty.

Anvil and Forge opened their Mission surfaces but did not select the saved row.
Ember proved identity, all five candidates and four common core pins, Inspect,
Refresh and the disabled post-refresh freeze control. It closed and reopened
the Mission surface, then could not reselect the offscreen saved row. Whole-app
restart was not reached. Frost selected the plan, but its exact ID/hash OCR was
below confidence 80, so it correctly did not Inspect. Its fixed rewind stopped
midway down the content, leaving Refresh offscreen. These partial observations
are not a passed contract.

The navigation corrections do not weaken these evidence requirements:
- Ember saved-row selection uses the existing owned, bounded navigation path.
  A genuinely absent initial catalog may wait up to ten frames within the same
  24-step budget. Disabled, foreign, duplicate or disappearing-after-scroll
  matches still fail; pointer input still needs two equal visible observations.
- Frost rewinds by at most 24 batches of four upward wheel events. Each batch
  uses the current unique measured thumb/track, stable in two frames and bound
  to the preceding observation. The distance from the track top must strictly
  shrink; success requires exact top equality and a second matching frame.
  Horizontal geometry and track stay fixed. The observed one-pixel raster
  rounding of thumb height is bounded across the whole seek, not accumulated.
  This is navigation only, never identity, candidate, pin or validation proof.
- X11 client coordinates come from strict `xwininfo` output, not guessed
  decoration offsets. The measured border is added to its absolute outer
  origin. Window/root identity, map state, dimensions, complete root containment
  and the before/after binding are checked. Existing xdotool PID, active-window
  and dimension checks remain; its legacy X/Y are retained only diagnostically.
  Only local screen 0 is accepted, matching the isolated input/capture server.
  Pointer input rechecks the complete second-frame window/title/root/client
  binding; a small move that leaves the old point inside the window still fails.
  This avoids the old xdotool reparented-window offset calculation.

The ordinary compatibility fixture additionally creates two synthetic X11
windows with nonzero parent/child offsets and borders. It checks the measured
client origin and saves a separate receipt. It is not a terminal application,
host or provider, and does not upgrade the failed native result.

The GTK mixed content-origin/border-size issue and Frost low-confidence
identifier/hash pixels remain separate from the navigation corrections.
No clipping tolerance, confidence reduction, punctuation repair, expected-value
OCR dictionary, cross-frame text joining or fabricated validation is introduced.
A later native experiment still needs separately reviewed frozen heads and all
22 current prerequisite checks.
