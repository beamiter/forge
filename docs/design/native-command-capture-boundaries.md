# Native command capture: measured capability boundary

This is a limited acceptance matrix, not a blanket claim that native fallback
or F13 passes. Source and artifact identities are kept distinct from the
published checkpoint. Current local app source is `211e131`; later test/docs
commits do not change that app binary. Core remains `84c96dd`.

| Path / condition | Actual evidence | Outcome and limit |
|---|---|---|
| Bundled Bash history metadata, ordinary actual commands | c090 app `a28fbc3…`; actual ls exit0, failed cat exit1, running command and exit0, same PID/TTY | Exact visible commands and native statuses pass. This does not prove native screen capture or the full long/edited metadata lane. |
| History disabled, clean prompt resized before typing | 211 app `21da1038…`; unchanged source-last fixture, default font, original three commands at1672→1356→1672; GUI hooks off and no Ready query | All three exact commands and exit0/7/0 pass. Physical grid23×89→15×60→23×89; shared native anchors update only under the existing empty-editor resize proof. |
| Native child-origin prefill after B, then real resize | test `55b234c8…`, source211 plus native negative-test patch `4ce236…` | Pass: shared anchor, saved rows and consumed epoch do not absorb prefill; HasInput remains, text remains once, no command card is fabricated. This is a private PTY protocol fixture, not external Bash/readline prefill coverage. |
| History disabled, keyboard editing, draft resized800→480→800 before Enter | current test55b and checkpoint-equivalent test294 on the same runtime and unchanged F13 driver | Both fail identically: expected191chars/279UTF-8bytes, capsule127chars beginning inside the command,3rows81px. Actual Bash-accepted literal remains exact. The command's screen boundary is not preserved through dirty width reflow. |
| Same history-off dirty reflow, actual bracketed clipboard paste | same matched F13 pair | Both fail identically: sentinel40chars/1row31px. Bash accepts the exact literal. This is not evidence that the actual279-byte command exceeds the256KiB command limit. |
| Wide glyph at a Readline redraw boundary | current bare native extraction contrasts with an ordered parser/title fence | Controlled streams with different input-space meaning can yield identical text, HTML, cursor and per-row extraction through69→40→69 reflow. These observables do not support deleting a padding-looking space. |
| Four-line draft followed by real Ctrl+C | old published-equivalent app1c9/fe86 and newer Block controls, private PTY traces | Byte03/SIGINT and A/B are real; subsequent keyboard execution is real, but HasInput persists. Individual old/new flags differ. No dirty/typeahead state is cleared to claim readiness success. |

The original strict same-length keyboard/paste metric assertion remains
unchanged. Neither path is forced to a common placeholder or a fixed height.
Typed shadow is not used to repair these edited captures, and no command
prefix is guessed from matching text.

## Matched F13 comparison

The app/fixture baseline is the frozen a7/c36-equivalent lib artifact
`294098c5cc662e3562bfa456bec133d5663ca8b46fca9595c2f5bbb8c4b2b383`.
The current lib artifact is
`55b234c8a2e5942e222e319169886d82f04bd9f12b6862faeee2142bc18d8ccb`.
The F13 Rust and Python fixture source diff is empty. Both run on this host's
private Sway, Nix VTE0.82/Adwaita1.8, same font and native keyboard/clipboard
driver. The loader bridge and headless compositor flag are the only harness
adaptations. Both complete cleanup without errors and fail the same original
stage7 assertion.

Their actual physical rows stay21 while columns change69→40→69. Keyboard
capsules have the same matching runs at expected character offsets55 and112;
paste capsules have the same sentinel. This matched case finds no additional
F13 regression from the limited empty-prompt repair. It does not prove all
library versions, all baselines or all command shapes behave identically.
The published CI Adwaita1.5 artifact has a different keyboard capture (192chars,
one extra space), and remains separately attributed.

Evidence relative to the task workspace:

- `evidence/forge/developer-block-v2-prompt-resize-guard/f13-pair-comparison.json`
- `evidence/forge/developer-block-v2-prompt-resize-guard/f13-history-off/`
- `evidence/forge/developer-block-v2-prompt-resize-guard/f13-checkpoint-pair/`
- `evidence/forge/developer-block-v2-prompt-resize-guard/native-public-extraction-ambiguity.json`

## What is established, and what is unknown

The pre-Enter native captures establish that dirty width reflow changes ring
rows even when physical row count is constant. The invalid capture bound and
partial keyboard label establish a lost command boundary. They do not establish
which particular asynchronous feed/layout transition first invalidated that
saved boundary. A C/feed race is a hypothesis, not a proven cause here.

The controlled Readline pair without an input space and the pair with a genuine
input space have different raw feed hashes, yet identical observed text, HTML,
cursor and per-row text at all three widths. The simpler EL-padding versus
genuine-space pair also matches at9→6→9. Other controls retain observable
differences, so this is a specific ambiguity, not a claim that all whitespace
is unknowable. It also does not prove that VTE internals or a different
integration could never expose stronger evidence. The current exercised APIs
and retained observations do not carry enough information for safe repair.

The smallest next diagnostic is a test-only extension to the existing strict
F13 fixture: record the B anchor, native allocation generation, contents
generation, resize epoch and current cursor at each width and at accepted C.
Keep this coordinate/epoch evidence separate from authentication bytes. Compare
the same frozen baseline and candidate; do not change expected input or metrics.
A native boundary mechanism that survives width reflow needs its own explicit
contract and negative prefill/recall tests before implementation is accepted.

If stronger provenance is unavailable, preserve an unsupported-capture boundary.
A future structured reason can distinguish unavailable/invalid native range
from a truly over-limit command. Changing only the displayed placeholder would
not fix capture and is deliberately not done in this batch.
