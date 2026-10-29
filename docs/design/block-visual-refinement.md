# Block reference visual refinement

This bounded local batch uses the self-viewed native reference
`Forge Developer Terminal Workspace.png` (1672×941, SHA256
`dc81db85112b6804424a2cdbbd94e232ca2335be2e0b36a620756f6725e26bf1`).
It is not final1:1 acceptance and does not implement running-output separation.

The active outer geometry now comes from the original common1356×704 shell:
42px titlebar,55px rail,237px navigation,306px Inspector,49px header and80px
composer inset28px/bottom11px. The earlier68px/1672 captures documented below
remain historical evidence; they are not a second pending layout. The Block
raster continues to define only the dark native command cards. Current
canonical geometry and fixed control bounds require new same-source screenshots.

## Final surface pass

The separate `work/forge-ui-final-20261028` branch starts at the stable
`f20aa105` product. The uncommitted native-feed experiment and the withdrawn
source-registration guard are preserved in task evidence and are excluded.

The common Inspector already matches Anvil's authored title11px/600 and
section8px/700 roles, so those typography tokens are unchanged. The Block status badge uses the same
12px native Check/Close/History vectors,4px inner gap,10px/700 label and quiet
green/red/blue fills as Anvil, driven by observed outcomes. Neutral or unknown
outcomes do not receive a success symbol. Header surfaces gain a subtle navy
gradient and failed terminal icons receive a muted red tint. Ordinary card
borders remain navy. The existing Block More button uses the shared16px
horizontal ellipsis vector; its action owner and signals are retained. Chat
messages, terminal fonts, input, footer and all PTY paths are unchanged. Native
screenshots and geometry checks are required before this pass can be marked
validated. Running echo separation and live output gutters remain unfinished
functionality.

The populated Current Step card now centers its state symbol and two-line
content inside the existing59px frame. Only the `details=true` renderer branch
changes: title and detail stay on one line with end ellipsis and their original
complete tooltips. Queue rows, message steps and all Inspector outer bounds
retain their existing behavior.

This UI-only pass builds on `f20aa105`; its fresh app ELF is
`1f090b6e11c7e08480522a00623174217e8050b6c90be38beb3ed54174ae6598`.
Format, offline locked check and release passed. Real Block commands exercised
success, failure and Running states; native focus, same shell/TTY/grid and
short Inspector scrolling checks passed. Chat1672/1356/1000/700 logical widths
kept the original font/geometry assertions and actual keyboard proof.
The actual command-header pixel bands remain40px for all three outcomes;
the full native PNGs include CSD and are not guessed client crops.

The first More test opened the genuine menu but failed before Copy when
full-screen OCR did not recognize its small text. That PNG, TSV and failed
driver remain intact. A bounded menu ROI oracle then passed the exact six-file
clipboard assertion, real F10 collapse, native More Show Output, exact restored
allocation/model height and700px real-state checks. No product behavior or
assertion was weakened for the oracle correction.

The unchanged eight-case long-label test was freshly compiled into test ELF
`cffa6095509e670a67d4845485e24a4188b58772ad81b4089565542b9eb168a1`
and passed with eight actual component PNGs. This test executable is distinct
from the full app. Its original single-line/ellipsis/full-tooltip,22px heading,
non-overlap,199px pane, five24px queue rows and short-window reachability
assertions remain intact. Current evidence is under
`evidence/forge/ui-final-surfaces/current-step-centered` and
`evidence/forge/ui-final-surfaces/block-interactions-oracle-v2`.

These are bounded UI checks, not final1:1 acceptance or a physical Fcitx/Wayland
IME claim. The saved native-feed experiment, withdrawn lifetime guard and their
old failures are excluded from this UI branch. Nothing in this pass implements
running echo separation or live output gutters.

The comparison baseline is runtimecd9/coref791/ELF2fd290, the Block and Chat
1672×941 native PNGs in `evidence/forge/density-v3-final-native`. Their major
bounds remain fixed:55px titlebar, centerx244, inspectorx1374/w298,42px center
header, composer(256,865,1105,68), Tools(612,897,73,28). Existing Main12px,
independent UI14pt, input owner, PTY and reading/follow rules are not changed.

## Stage1 — ordinary native command cards

Both completed and failed card bodies use the existing1px navy frame
`#293c57` on `#0e1621`. Outcome color stays in the real green/red compact
status badge. Running keeps its blue1px edge and glow. Hover, native keyboard
focus, card selection and text-selection paint remain distinguishable without
changing any margin, padding, height, font or terminal sizing property.

The optional existing geometry probe gains actual footer descendant button
bounds, because the baseline recorded only the composer and Tools. This is
read-only resize telemetry; it does not change the footer and must not be used
to relabel old screenshots. In particular, suggested shared footer metrics
were not all consumed by the baseline factory: actual values require capture.

## Stage2 — Inspector section mapping

Approximate reference pixel regions, measured from the actual PNG; source
font and antialiasing are unknown, so these are calibration targets, not
editable design tokens:

| Section | Reference x/y/width/height | Baseline native bounds |
|---|---|---|
| Metadata frame |1396/180/253/132 |unframed1395/182/257/95 |
| Current Step card |1396/359/253/59 |1395/304/257/45 |
| Empty Task Queue card |1396/466/253/58 |text-only1395/381/257/11 |
| Terminal section |starts near669, separate top divider |inside one full-height slab |
| Terminal pane |1396/708/253/198 |1395/779/257/140 |

The shared Block/Chat inspector will retain the existing column and tabs. Its
large slab paint will become a quiet background with independent metadata,
Current Step, empty queue and terminal sections. Honest idle values remain
Not selected/No task; the opt-in typed fixture remains visibly marked. A real
fixture progress row may occupy additional room. No fake tasks, model or
terminal output are introduced. The output section remains bottom anchored
with natural fill between task content and terminal; no empty hard spacer is
added. Final actual bounds and native proof will be recorded before acceptance.

The implemented idle flow removes the empty progress row. Metadata retains
19px label/value rows with3px row gaps inside a132px framed section. Current
Step and an empty queue receive separate59px/58px cards. Existing task and
fixture queues still render their actual rows. A task with a nonzero total
keeps its truthful progress row and therefore shifts the following sections
naturally; idle pixel targets are not imposed on populated task data.

The terminal section uses a divider, heading and199px pane at the bottom of
the same flexible vertical layout. Its empty native label remains selectable
and says “No task output”; no synthetic process output is supplied. The
reference slab becomes a transparent grouping surface while the content
frames supply hierarchy. All type sizes, global columns, canvas, composer
and input state stay unchanged. Initial native capture at1672×941 measured metadata1396/180/253/132,
Current1396/359/253/59 and empty queue1396/466/253/58. The populated fixture
keeps its nonzero progress and all five24px queue rows; its terminal pane is
1396/708/253/199. The short1356×704 Inspector scroll reveals the fifth queue
row and full terminal pane without enlarging the window. The explicit
Search-close → readonly native output → Terminal action focus check passed
with the same shell/TTY; the earlier modal-open failure remains archived.

The first empty-state pane measured638/269 because its centered child made
the terminal section inherit vertical expansion. That initial ELF and PNG
remain in `stage2-inspector`. Explicitly disabling expansion on the section
leaves spare room with the existing flexible fill rather than adding a
spacer. Only the idle badge is made nonwrapping with ellipsis and its existing
full tooltip. Corrected native capture is recorded separately.

Stage1 native evidence is in `evidence/forge/visual-refinement/stage1-cards`.
The Chat4-width geometry/default-font and real-keyboard checks passed. The
first Block focus challenge opened the global Search modal and then tried
to click the underlying Terminal action without dismissing it. That attempt
failed and is retained; it is not evidence that the unobstructed action is
broken or that native focus passed. Card captures and the real command
status/grid measurements precede that challenge and keep their own source.

## Native extreme-label verification

A single ignored test in the existing test-only geometry module calls the
actual `reference_conversation::inspector`, vector icons, production CSS and
`role_font_css(1,1)`. It adds no product model override or rendering path:

```
ui::tab_geometry_tests::inspector_extreme_labels_keep_one_line_and_reachable_full_tooltips
```

The eight native cases combine ordinary text, a517-character title,
a541-character status, and both long fields at298×844 and298×607. They assert
one Pango line, actual ellipsis for every long field, an exact full-text
`tooltip_text` value,253×22px heading bounds, visible icon/title/status with
no overlap, and text fitting inside its content allocation. The normal
85px badge is not ellipsized; long cases retain at least that status width
(actual long-status116px, both-long114px). No status label is removed to
make the title fit.

The same actual widget tree retains a199px terminal pane and five24px queue
rows. At607px high its native scroll document is805px, page607px, and bottom
position198px. The fifth queue row and full pane fit inside the real native
viewport at the bottom; returning to the top makes both full-text tooltips
reachable again. Native component PNGs intentionally show the top again to
inspect long labels. These component fixtures are distinct from the full
application's d11e786/ELF36e9e609 Block/Chat screenshots and real Bash checks.
Tooltip text preservation is tested through GTK; no separate hover-popup
visual claim is made.

The final single native test passed in2.45s with all eight case assertions
and actual component PNGs. Its exact Cargo test artifact was frozen with
SHA256 `66d200ddfed5baac6af7af1e96522b311b0526be46cdbe78384eadc445c6ba5a`.
Provenance and results are in
`evidence/forge/visual-refinement/stage3-single-line/same-crate-long-labels`.

Earlier standalone rustc E0463 failures remain archived. The first same-crate
run had an invalid mapping wait comparing GTK's253px content width against
an outer-box threshold. The second incorrectly compared a197px content pane
against its199px CSS outer target. Both failed test sources, original logs
and frozen binaries remain preserved. The final audited test uses actual
outer bounds for22/253/199/24px visual targets, keeps Pango content getters
for text layout, and uses the native adjustment page size for reachability.
No product code or expected visual dimensions were changed to pass it.

Fresh configurations render the shared shell at opacity1.0 so semantic surface colors are not dimmed by a transparent top-level window. Explicit file/environment opacity and saved user preferences retain their existing priority and are not migrated or overwritten.

The original sidebar controller and fixed49px bottom status remain the outer box. Its navigation/recent/spaces document now has one native vertical viewport, so short windows retain exact requested size and all rows remain reachable by scrolling. This one-time shell arrangement does not rebuild session widgets or change the PTY/history viewport.
