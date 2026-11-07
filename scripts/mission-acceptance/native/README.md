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

The latest executed round, [42312173963, attempt 1](https://github.com/beamiter/forge/actions/runs/42312173963),
**failed native acceptance**. Its five production builds and 21 prerequisite
checks passed. The final independent host check passed with the whole saved plan
unchanged, both native/conversation catalogs empty, no Tasks, and every required
validation still `not_run`. The recorded frontend binaries matched the same-run
production build artifacts. No complete native inspection/refresh/reopen sequence
passed.

Anvil and Forge opened real windows, but the observer stopped with an AT-SPI
`NotSupported` error before Mission input. GTK 4.14 does not implement general
Text.GetRangeExtents for these widgets. The old diagnostic did not identify the
failing node, so this is a verified compatibility defect, not proof of that
particular exception's call site. Ember selected the existing Mission and read
its exact identity/revision, five repository candidates and four core pins.
Its fixed-distance rewind then left Inspect above the viewport and searched in
the wrong direction. Inspect, refresh, close and reopen were not executed.
Frost opened the plan and really scrolled, but a single adjacent pixel merged
with its scrollbar thumb and made the strict shape check reject it. Its ID crop
also failed the unchanged OCR confidence threshold. Identity and subsequent
inspection phases were not accepted.

The earlier [42245525353](https://github.com/beamiter/forge/actions/runs/42245525353)
and [42228324473](https://github.com/beamiter/forge/actions/runs/42228324473) rounds
remain failed. Their observations must not be combined with later rounds to
claim a completed sequence. Offline pixel replay and pure observer regression
tests do not replace another exact-head native run. Frost displays its stable
Mission ID, but the ID/revision and repository-bound fields must still be
observed in every required stage.

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

AT-SPI requests keep introspection disabled and use explicit protocol types:
unsigned SCREEN coordinates for Component.GetExtents and Text.GetRangeExtents,
signed text offsets for Text.GetText and Text.GetRangeExtents. Signature tests
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
