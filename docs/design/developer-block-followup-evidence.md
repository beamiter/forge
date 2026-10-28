# Block follow-up evidence boundaries

The published checkpoint remains `c36fb1b` plus its documentation correction,
integrated by the parent task as master `bad4a93`. Later work described here is
local. None of these results establishes final pixel or full interaction
acceptance.

## More restoration

Local `cf15941410db40ac510995f6ebcf8e3d5fcb6b01` fixes the original More
Hide/Show metadata gap. Its frozen app SHA256 is
`c8c7eea648c93d78ecd76683141ee8e7a32bd22c937bc8228a665832ba02cab8`;
core is `84c96ddda17f34091957aff19ea2004e85a418a2`. The separate frozen
lib-test SHA256 is
`efb4a3c7e2e32b0d7ff09f1ebc57f33aea7a0b1060badc603e8a4f9d63cb953a`.

Both the real More-action allocation test and the original visible/parked
fold-geometry test passed on that exact lib artifact. Real pointer interaction
also copied exactly six filenames, folded with F10 and restored through More
Show Output. After restoration, six native output rows remain, and the selected
card's actual height and retained estimated height both equal 254px. The
document span is independently checked by the native test. The earlier a7
pointer timeout remains archived and is not relabeled as passing.

Evidence, relative to the task workspace:

- `evidence/forge/developer-block-v2-cf15941-tests/native/`
- `evidence/forge/developer-block-v2-cf15941-actions/manifest.json`
- `evidence/forge/developer-block-v2-cf15941-actions/more-menu.png`
- `evidence/forge/developer-block-v2-cf15941-actions/action-3.png`
- `evidence/forge/developer-block-v2-cf15941-native/manifest.json`

These first tests preserved the reading-intent flag but did not prove a
nonzero history anchor. The additional four-card fractional-scroll case
exposed a movement from 87.25px to 87.01808297168606px after Hide. That failure
is retained with test artifact `2135a66d…` under
`evidence/forge/developer-block-v2-more-reading/native/`. The follow-up reuses
the existing Tools popup focus-reveal guard for More. It suppresses GTK's
independent reveal only during the popup's focus exchange, then restores the
native policy; it does not write the adjustment or reset follow intent.
The follow-up passes the exact same anchor assertion through popup open,
Hide, reopen and Show, and preserves the complete follow/motion state. The
original fold-geometry test also passes. These results use the frozen test
artifact `b7d58034ed6fab0ba648cd3fb11340adc88f7be01f58e09c4f3b7fbd2459cdd8`,
compiled from `a9743f1` plus source patch SHA256
`9d9c59ada18b42b5ead0a4ba44ab891c7cc37f0772946034c1a1ffeade73e5ab`.
The build completed in 3m37s. Exact provenance and logs are in
`evidence/forge-more-reading-scoped-artifact.json` and
`evidence/forge/developer-block-v2-more-reading-scoped/native/`.

## Multiline interrupt control

The pre-dock Block98 and explicit Unified98 controls were not old-release
proof. A further control now runs the frozen, previously published-equivalent
app source `1c9a00e01a222ca853a6f937faecf8a4613a63b0`, binary SHA256
`fe86bece2d398b4daacaf2025637d07c738e841b896d775d7db7dd2212a04c2c`.
Its existing build-source record maps it to publication `c0019e4`. At that
source, public Block uses the continuous Unified surface.

The same four-line draft and real Ctrl+C reproduce `HasInput` on this old app.
The private PTY trace proves byte03 delivery, Bash SIGINT and subsequent A/B.
The same shell's physical size stays 27 rows by 92 columns. A subsequent real
keyboard probe produces C/D and writes the exact `cancel-probe-ok` stdout.
The old native screenshot remains stale and no history record appears, so
this result does not claim old visual cleanup or history correctness.

The integration script extracted from that old source is byte-identical to
the current script, SHA256
`f15eb9bd7f39b6386cdf1a9ca8e92b46b1fec76978b39a4786f4ad103d65d5fa`.
Private trace summaries are under
`evidence/forge/developer-block-v2-cancel-release-control/sanitized-result.json`.
Only lifecycle letters and boolean diagnostics are exported; capability
material and host history are not read into the report.

Current source explains the retained failure: commits after an accepted LF
taint `pending_typeahead`; PromptEnd consumes that flag but retains dirty/synced
state through `next_prompt_shadow_state(true)`. The current shadow is cleared,
whereas the old control retains a nonempty shadow. The matching status therefore
does not establish identical individual flags or one universal root cause.
No dirty/typeahead flag is cleared to turn the failed readiness gate green.

## Native command fallback remains open

History-enabled Bash command metadata and screen-derived fallback are separate
paths. The history-enabled default-font control passes, but cannot prove native
capture. On `cf15941`, the private history-disabled first post-resize command
becomes the truncation sentinel. Candidate `a9743f1` shares a fixed-grid anchor
policy between both native capture and verified submission: the first grow
case passes, but the following shrink still fails with a saved row5 versus
current cursor row1. Those failures remain archived. This candidate is not
called a complete capture fix.

The next local candidate runs the already-existing empty-editor resize proof
from the bounded geometry settle pass and before a human native commit. It
requires the coarse Ready gates, settled B anchor, a changed local PTY resize
epoch, the exact nonempty native prompt prefix, an empty native suffix and an
unchanged accepted-input generation. Only shared anchor coordinates/rows and
the consumed resize epoch change. No input guard, PTY byte, editor content,
focus or follow state changes. Dirty-draft reflow intentionally declines this
repair and remains a separate gate; this is not a claim that F13 is fixed.

That limited candidate is local `211e131ac8d87ddbd567832409c5efe2a888464c`,
release SHA256 `21da1038c681f8a3b0b3a6a97dbf40d582a1547e8cdbf7bd2251ce2cdade82b2`.
Check passed in 4.45s and release in 1m35s. The unchanged private history-off,
source-last, default-font three-command control now passes exact command and
exit0/7/0 assertions through grow/shrink/restore. GUI readiness/geometry hooks
are disabled and the driver never asks Ready. The same shell/TTY has physical
23×89, 15×60 and 23×89 grids. Original cf/a974 failures remain archived.
See `evidence/forge/developer-block-v2-211e131-native-fallback/manifest.json`.

Frozen test artifact
`55b234c8a2e5942e222e319169886d82f04bd9f12b6862faeee2142bc18d8ccb`
is that app source plus only test patch SHA256
`4ce23600e7b0e27f46c3412ead228c249ad6c921431d0c683c7d40445bae2764`.
Its native negative case puts child-origin text after B without local typed
input, performs a real resize and requires a new physical PTY epoch. The anchor,
saved rows and consumed epoch remain unchanged, the native prefill remains
exactly once and status stays HasInput. This uses a private `for_tests` PTY
protocol fixture, not a claim about an external Bash prefill implementation.
The same artifact also passes the exact More history-anchor test, physical dock
grid test, original fold geometry and both existing pure prefix/reflow guards.
Logs are under `evidence/forge/developer-block-v2-prompt-resize-guard/`.

The published GTK F13 history-disabled lane also fails. Its artifact shows
correct Bash-accepted bytes but a keyboard capsule with one extra wide-wrap
space and a pasted capsule containing the truncation sentinel. The 4-row versus
1-row metric difference follows those different strings. The strict metric
assertion must remain: fixed heights, history-enabled metadata, prefix guesses
or typed-shadow replacement would conceal the capture failure.

The unchanged history-off F13 test was also actually rerun on the latter
artifact, on private Sway with the existing Nix VTE0.82/Adwaita1.8 runtime.
Only the runtime-loader bridge and headless Sway flag were adapted. All
original stages, expected commands, focus checks and assertions remain.
It still fails at stage7: keyboard capsule127chars/3rows81px versus paste
sentinel40chars/1row31px, while both real Bash-accepted literals are exact.
The keyboard label begins within the command, not at its first character;
this is not evidence for a safe prefix completion. At the same physical
21-row grid, the width changes69→40→69 and the ring rows move. This exposes
the unclosed dirty-draft reflow anchor case. A C/native-feed race has not been
established as the cause by this observation. The full failure and native
stage images remain under
`evidence/forge/developer-block-v2-prompt-resize-guard/f13-history-off/`.

## Font provenance

Main14pt screenshots with explicit DejaVu Sans Mono and UI14pt are configured
fixtures, not complete default-font evidence. Their old
`Main_changed_for_reference:false` field means only that nominal Main size was
not reduced to imitate the reference. It does not mean font family and UI
settings were left at safe defaults. Default-font captures must record resolved
Pango family, size units, cell metrics, role scale and DPR separately.

Actual default-font screenshots are now captured on local `c090e01`, frozen
app `a28fbc3bc0296e1da903552372d86b8fd2616583185366a85e5b67c707af9928`.
The private config contains only the fixture shell and disabled update checks:
no Main/UI font or scale override. Requested and resolved Pango Main are
JetBrainsMono Nerd Font Mono14, size14336units and nonabsolute (points),
font scale1, GTK scale1/DPR1, cell11×29. Safe defaults resolve Chrome size14
(not None) with role scale1. Real success/failure/running captures at1672×941
and1356×704, plus same-PTY transition checks, pass. They remain attributed to
c090, not later anchor changes. See
`evidence/forge/developer-block-v2-default-native/default-font-source-summary.json`.
