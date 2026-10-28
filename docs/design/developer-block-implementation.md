# Developer Block native presentation

Reference: `block-reference-v2.json`. The Forge executor viewed the actual
1672×941 PNG before implementation. Tokens reconstruct a raster; typography
and text in that image are not authoritative runtime data.

New public Block selections use the existing `BlockBackend`. Explicit legacy
Unified configuration remains Unified, retaining its continuous VTE and zone
history. Parsing remains compatible. A pane's runtime mode is pinned at its
construction/reload boundary: choosing a future presentation never replaces
or restarts an existing PTY. Managed remote Block retains its original backend.

Completed records remain `FinishedBlock` native, read-only command/output VTEs
without per-card PTYs. The command VTE moves into its native header, keeping
selection/search/source identity; the original More/action callbacks remain.
Status and elapsed fields use observed exit/duration values. No descriptions
are invented. Running headers use accepted command-start/finish events and a
monotonic elapsed clock. The active terminal remains the only PTY input owner.

The gutter draws logical line starts from the same ANSI-replayed output model
used for native fit. Soft-wrapped continuations have no number. It reads native
adjustment/columns and caches by display generation and columns; it inserts no
bytes into output, native selection or clipboard text. The inter-card gap is
inside the measured outer wrapper, avoiding an unindexed external margin.

Batch 1 changes only the Block center/card projection. Chat and legacy Unified
remain separate. The old global Block footer is still the existing focus-only
surface in this checkpoint; a distinct follow-up will move the one active VTE
clip into a compact dock with correct running/foreground/IME authority. This
checkpoint does not claim final composer or visual acceptance.

Main VTE font family/size and scale remain user-owned. No reference estimate
silently changes the default 14pt setting. Explicit fixture fonts belong in
capture manifests. UI metadata follows the existing independent UI font role
scaler; running command text follows Main. Stop, search, program output, native
selection, resize and retained-scroll behavior require real native validation.

Post-stage-1 review: the line map now reuses `output_display_text` on raw
ANSI-bearing bytes before replay, preserving additional leading/trailing blank
rows exactly as the native snapshot does. Focused tests cover boundary CRLF,
ANSI-before-newline, retained blank lines and wide-glyph wrapping. Live count
refresh coalesces at idle after model mutations and includes the real current
command; background output is excluded. Acceptance telemetry enumerates actual
mapped renderers rather than assuming viewport-index cache membership.

Batch 2 gives new Block the existing generation-bound dock mechanics through
an explicit `docked_input` adapter. The semantic Chat class, messages and light
palette remain Chat-only. The same active VTE clip moves between the compact
prompt and running card; no terminal, PTY or editor is duplicated. While a
command owns input, a passive footer hint replaces the prompt and Stop targets
the observed foreground generation. The old focus-only global footer is hidden
for both real docks. Explicit legacy Unified retains its previous presentation.

The developer PTY grid reserves a constant header/border budget in prompt and
running seats. Short card visibility is independent of this physical grid;
output line count does not resize the child. Native evidence must compare the
same PID/TTY, TIOCGWINSZ, output and completion markers before claiming that
transition passes. Running native prompt/echo remains visible until an output
boundary can be verified safely; no guessed row cropping is applied.

Native phase-2 proof (`6f6890d`, frozen `955bec5c…`) preserves the same Bash
PID/TTY and 27×90 physical rows/columns at 1672×941 through prompt, command
start and output. The actual foreground command finishes with exit0 and a new
Ready prompt. A resize to 1356×704 revealed a one-column difference between
running and prompt seats; the follow-up moves the common inset onto the single
clip and removes seat-specific parent padding. Completed card bodies now own
the rounded outcome border and their internal 10px gap, both measured by the
outer wrapper. Native unit checks additionally exercise four-line input, the
same VTE/anchor identity, actual slave winsize and height-only resizing.

A paired multiline Ctrl+C probe on frozen pre-dock `98bdac8/96b7f050…` and
`6f6890d/955bec5c…` proves that both write byte03 to their private PTY and Bash
receives SIGINT. Both show a fresh empty prompt while the existing readiness
guards remain HasInput (`dirty=true`, `synced=true`, empty shadow). This is an
observed pre-dock readiness failure, retained in the paired evidence. It is not
worked around by treating HasInput as Ready or weakening submission guards.

Follow-up after the publishable c36 checkpoint: new Block alone uses a one-row
visible prompt floor, growing for all native drawn lines and completion UI.
Chat retains its two-row policy. A proven native cursor-row rewind below the
old origin now translates the absolute cursor high-water into the independently
measured displayed row domain, preserving its relative extent. This affects
only Block's visual projection: no PTY size, VTE feed/reset, bytes or history
identity changes. Its native resize and default-font geometry are pending
verification on the corresponding new binary. The existing card context menu
also receives a scoped dark sans-serif UI style, retaining original actions.

The 3e9 native capture proves the compact prompt uses one actual 27px row at
Main14pt, and a short running surface remains 127px after 1672→1356 resize.
Physical sizes remain 27×89 then 18×60, with a real completed command and Ready
prompt. Targeted native five and pure developer four/mode one also pass on
frozen lib-test5a50454a (source770, app3e9), without relabeling earlier results.

Real pointer Copy/F10/More-ShowOutput now all execute in the 3e9 action driver,
with exact six-row native restoration. The same trace exposed a model bug:
the menu changed only the widget, leaving the collapsed estimated height122
beside actual expanded254. The follow-up `FoldLayoutSync` updates the exact
record/index/visibility at the menu action and samples its real allocation on
a bounded second frame, including selected-card chrome. It neither changes
follow intent nor focuses the terminal. A new native More-button test requires
actual height == model == document span after both fold and expansion, exact
output/source preservation and unchanged reading intent. This follow-up is
not covered by the preceding 3e9 test result until its own artifact is run.

Native fallback remains a separate gate from shell-reported command metadata.
At cf15941, the default-font/source-last control passes exact commands with
Bash history enabled; disabling history in that private fixture makes the
first post-resize short command become the truncation sentinel. New Block's
fixed physical grid was still using the legacy compact-grid row-delta anchor
policy. The next candidate shares one fixed-grid decision between backend
capture and verified submission, with coordinate-only diagnostics. It keeps
all capture bounds, exactness provenance and approval/input guards. Real
metadata-disabled initial/restore/running/edit cases must pass before this is
called a fix; existing history-enabled results do not satisfy that fallback.
