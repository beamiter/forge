# Native reference workspace

Forge binds the shared `jterm_core::reference_shell` layout and
`reference_icons` vector paths to native GTK widgets. The notebook, PTYs,
shell editor, session restoration, terminal selection and existing retained
canvas remain the owners of their data and input. The reconstruction worker
did not receive the reference JPEG pixels; the parent task owns comparison
against the original and final visual acceptance.

## Layout and rendering

At a **1356 × 704 logical client area**, the toolbar is 42 pixels high. The
rail is 55 pixels wide. Navigation occupies 236 pixels plus a one-pixel
divider; the central region starts at x292. Its native content is 757 pixels
wide plus a one-pixel divider; the 306-pixel inspector starts at x1050. The
conversation header is 49 pixels high. The inspector collapses before the
navigation as width decreases. CSS binds a light central canvas to the dark
navigation, toolbar and inspector; native terminal output retains its own
surface and configured font.

The shared stage-six geometry uses 32-pixel Recent rows, 30-pixel Spaces,
a 120-pixel unavailable-model slot, and one controls factory for Block and
Chat. The outer composer stays at y613 with height80 at the reference size;
its 28-pixel control row is inset eight pixels from the composer bottom.
Block exposes a quiet noneditable focus surface; Chat keeps its original
live editor and immediate generation-bound controls.

The reference font is unknown. Application controls use DejaVu Sans and code
uses DejaVu Sans Mono for a reproducible local baseline. Terminal font and
zoom remain configured by the user. All shared line icons are drawn from
24-unit paths by Cairo; they are not substituted with font glyphs.

The Block/Chat buttons select the presentation for newly created/restored
local panes. Existing running panes retain their PTY and presentation. Public
Block uses the existing continuous native terminal backend. Legacy `block`,
`vte` and `unified` configuration/session values remain readable. Managed
remote panes deliberately retain their legacy block backend because its
metadata/history participates in reconnection and restoration. That remote
presentation exception and live conversion of existing panes remain work to
resolve before declaring complete mode parity.

## Data ownership

| Component | Production source / behavior |
| --- | --- |
| Recent sessions | Existing notebook/tab model and tab filtering |
| Global path and Context | Active terminal directory, sanitized and hidden by the existing private-title policy; title changes refresh it immediately |
| Chat command bubble | Observed real shell command and existing native VTE snapshot |
| Chat Shell result | Existing real output VTE, with an explicit Shell role header; no Assistant execution claim |
| Agent Tasks / chats | Existing feature-gated task and chat panels in the inspector |
| Files | Existing file browser moved to the inspector |
| Terminal composer tool | Focuses the existing live terminal; does not submit or synthesize input |
| Send / Stop in Chat | Existing generation-bound immediate controls and IME guards |
| Block composer | Same surrounding shell; input stays in the real grid, unsupported Send is disabled |
| Attach / Image / Code | Real disabled controls, with a readable muted label and an unavailable tooltip |
| Rich diff Copy | Copies the displayed diff to the native clipboard |
| Rich diff Apply | Disabled until a real authorized edit target is connected |
| Cross-application Spaces | Explicit unavailable placeholders; no program is launched |

The central rich component renderer accepts the shared typed
`reference_conversation::Conversation` model. Structured Assistant text,
task steps, diffs and inline terminal parts currently have the shared visual
fixture adapter; they are **not inferred from stdout**. Connecting actual
agent conversation storage to those central rich components remains separate
work. Existing real agent/task functionality continues through its inspector
pages.

## Explicit visual fixture

`JTERM_REFERENCE_FIXTURE=1` adds the shared native component sample to the
existing Chat scroll document and populates the inspector with the shared
sample state. The central header visibly says **Visual acceptance fixture ·
sample data**; task metadata says Fixture/offline/not connected, and terminal
sample text states that it did not execute a process. The five sample Recent
rows are not inserted into actual session history.

The same sample drives real GTK user bubbles, role/avatar rows, proportional
headings/body text, fixed-gap metadata, task rows, selectable code/diff lines,
Copy and disabled Apply controls, and inline terminal components. The real
live VTE and composer remain connected below the document. Production mode
does not populate those fixture task states.

The existing application-level organism presence ledger is retained. A
single native surface per admitted owner draws the shared 64px vector
reconstruction from `reference_organism::SHAPES`. This is a reviewable trace
from the supervising task's measurements, not the original design asset.
Actual reaction speech has a native dismiss control and actual awake/resting
state. A bounded native viewport read rejects occupied terminal cells before
showing or moving the complete graphical/speech footprint. Insufficient space
hides it without enlarging a pane. Legacy reducer poses remain semantic data
and are not painted over terminal cells.

The existing application-level organism presence ledger is retained. Rich
messages create no companion. An organism can render only in the original
safe terminal space; it is not forced over dense text or composer controls.

## Verification

The task-local SDK uses already present Nix GTK 4.20.3, VTE 0.82.3 and
libadwaita 1.8.4. `native-sdk.env`, `native-rustc-wrapper.sh` and
`native-runtime.sh` in the task root isolate that SDK/runtime. System GTK
4.6.9 and installed applications are not changed.

```sh
source /path/to/isolated-sdk/native-sdk.env
CARGO_TARGET_DIR=/path/to/forge-build-cache \
  cargo build --release --offline
```

`scripts/qa-reference-shell.py` captures actual native Block and Chat windows,
executes a named fixture in the real PTY when testing shell rendering, verifies
keyboard input by a newly created proof file, and captures wide/narrow windows.
Each QA shell uses a private HISTFILE as well as private XDG directories.
`--rich-fixture` selects the explicit typed native component scene instead of
presenting printf output as rich UI. Captures record Git HEAD, dirty source
hash, binary hash, actual GTK geometry and scale, outer X11 bounds, and the
client crop. On X11 the 1366 × 714 outer surface includes a five-pixel CSD
resize shadow around a 1356 × 704 client; only that shadow is cropped.

`scripts/qa-reference-wayland.py` uses a private headless Sway/Pixman desktop
at real 1.25 and 1.5 output scales. It records compositor scale separately from
GTK's buffer scale; font DPI changes are not represented as display scaling.
`scripts/qa-reference-native-tests.py` runs the affected existing GTK/PTY
assertions in independent private displays and XDG trees.

The existing strict wrap/resize test also loads the production reference CSS.
Its width, alignment, complete-command and row-height assertions remain.
The command snapshot measures wrapped height from its actual allocated width
and reflows its native read-only grid before allocation, so narrowing and
expanding do not leave only the tail of a long command visible.
Metadata assertions now require the shared six-pixel gap and check allocated
child gaps. The fold/unfold exact-height assertion is retained: unchanged
master `336f513c1831` also reproduces its 286-versus-315 failure under the same
isolated SDK. With the added Shell role, the same 29-pixel fold mismatch moves to
304-versus-333. The cause was reserving live-editor rows inside a standalone
preview scroller; only the actual `block-scroll` canvas now reserves them.
The strict native test measures eight stable current-generation paint frames
and passes 333→137→333 with all eight output lines retained. Actual pane fold
coverage independently measures the mapped 16px Shell header and verifies
parked metadata, native height requests and history-reading intent. Background Shell
output reserves the role height even though it has no command-bubble gap.
Restore, undo, filtering and bookmark visibility translate undecorated
estimates exactly once before writing document heights.

Current results and remaining failures belong in the task's
evidence JSON/logs; this document does not claim pixel acceptance or universal
test success.

This source checkpoint is saved to master for recovery. Original-reference
acceptance remains pending; no application installation, dependency downgrade
or restart of the user's daily programs is part of this change.


## Native Tools and focus

The shared six-row dark Tools menu routes Terminal, Command palette and
Search canvas to existing callbacks. Attach/Image/Code stay disabled. The
menu keeps the existing composer and input owners. A native capture driver
records closed/open/closed pixels, actual focus targets, document anchors,
adjustments and follow intent; it requires genuine popup visibility rather
than treating a focus ring as an opened menu. The popup temporarily suppresses GtkViewport's independent focus reveal
through focus return, then restores the native history keyboard-focus policy.

Earlier test shells omitted HISTFILE; a metadata-only audit cannot establish
whether the host history was written because those traces lacked open/fd
path evidence. That outcome remains uncertain. Current harnesses explicitly
use private HISTFILE and private XDG trees. No host history content was read,
deleted or rewritten by the audit. Wayland cold first-key loss reproduces on
exact master336 and remains unresolved; GTK IMContext tests do not substitute
for system ibus/fcitx protocol coverage.
