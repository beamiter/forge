# Block density and shared shell geometry

This is a local follow-up to the new Block reference, not a claim of final
pixel acceptance. Existing command-capture/readiness failures remain recorded
in `native-command-capture-boundaries.md`; changing the default font does not
turn the explicit14pt F13 fixture into a smaller-font test.

## Default font contract

Unconfigured Main becomes `JetBrainsMono Nerd Font Mono 12px`: Pango absolute
logical pixels, not12pt and not a hidden scale multiplier. Unconfigured UI
remains independently14pt, with its existing independent zoom behavior.
Authored Chat heading/body/metadata roles remain20/12/10px at the new Main
nominal size; explicit Main point/pixel settings still scale content through
the actual native Pango context. The UI nominal baseline remains14pt.

Explicit Main env/TOML values and their units are retained. Explicit UI values
retain their units too. If UI size is omitted, it now resolves to14pt instead
of being derived from Main. Thus an old configuration containing only a custom
Main size or zoom no longer implicitly changes UI size or zoom. Missing UI
zoom resolves independently to1; explicit UI zoom is retained. This is the intentional
independent-default migration, including a new template that explicitly writes
the Main12px default. No existing user configuration file is rewritten.

Settings already retain the configured unit when editing size or family and
label Pixels/Points accordingly. That path is preserved. Native tests retain
explicit14pt/14px, fractional size and zoom checks; only assertions about the
actual nominal default change to12 absolute pixels.

Template synchronization is authored by the parent task while installer changes
are concurrent: parent template commit `f441e95` has the12px Main default and
documents optional independent UI14pt/scale1 fields. The UI keys remain
commented so their addition does not trigger an installer schema migration
that resets a custom font. The loader owns those missing-key defaults.
Installer/README synchronization is a separate parent commit.

## Shared geometry contract

The current contract is the original common shell for both Block and Chat:
titlebar42px, rail55px, navigation237px (center starts at292px), inspector306px,
center header49px, composer80px minimum with28px horizontal and11px bottom
insets. At1356×704 the composer is320/613/702/80. `developer_workspace` delegates
its outer metrics and Layout to `reference_shell`; there is no second active
shell geometry. The newer Block raster supplies only the dark command canvas:
Live36px, card inset12px and gap10px. Native Main remains12 logical pixels.

The earlier55/53/191/298 shell and68px dock below are historical validation
stages only, retained to preserve their input/caret evidence. They are superseded
as layout targets. Native content still grows naturally when configured fonts
or multiline input require it; nothing crops input to force the minimum.

The separately reviewed Chat visible-row floor changes from two to one. It
still grows for all native multiline/wrapped/completion content, bounded by
the viewport. It does not change PTY-grid calculation or create another input
owner. The canonical80px footer remains a minimum; larger explicit fonts and long
input may grow naturally. Four-line/long input caret, Enter, IME and physical
grid checks must run on the combined native geometry artifact before claiming
this floor change accepted.

Both adapters consume the canonical metrics through `developer_workspace`.
Native CSS accounts for border/padding inside every request. Search is362×28px
at top8, mode controls52×28px at top7, Inspector tabs49px and its outer inset14px.
Chat keeps its light canvas and Block its navy canvas; their outer geometry,
fixed28px controls, model120px and send28px slots are shared. Larger UI role
scales grow the same controls; smaller UI fonts retain minimum hit areas.

The 8eaf/font-only artifact was built against core84:27 pure checks and two
real Pango role/context checks passed. Its third native default-font comparison
still used a hardcoded14pt reference and failed7 versus11 cells. The next
artifact independently constructs the reference at absolute12px, keeping the
original cell, cursor, full-text and unchanged-config assertions. That old
failure remains archived. The More anchor fixture also gains two real history
cards so its exact87.25px point stays away from a clamp after folding at the
smaller font; its strict position, motion, height/index and text assertions
remain unchanged, with a new explicit non-clamping prerequisite.

## Native input follow-up

The combined geometry run exposed two concrete input constraints. Chat's
prompt seat had 30 more horizontal pixels than its history seat, so Enter
changed real kernel winsize from 150 to 146 columns. The retained history
anchor now keeps invariant horizontal chrome, and both seats size the same
native child to the narrower actual content width. No cached column count,
text replacement or oversized/clipped native surface is used. Actual window
resizes still update both allocations.

A long Chat draft also exposed fixed dock padding/border missing from the
physical-row budget. The budget now subtracts those measured style insets,
independently of current editor height. Native CPR/caret containment and real
Bash 4/80-line acceptance are separate checks; results are recorded with the
exact source and ELF, including failures. The first diagnostic cursor formula
incorrectly subtracted VTE's rebased adjustment from absolute ring cursor rows;
those old traces remain preserved and are not authoritative caret positions.
