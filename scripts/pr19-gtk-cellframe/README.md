# PR19 GTK4 CellFrame display fixture

Run `bash scripts/pr19-gtk-cellframe/run.sh ../jterm_core` from Forge. The runner
exports the committed engine subtree from core `7e30d95` and builds the independent
fixture with its locked dependencies. For both cases the Rust producer sends
`CellFrame` snapshots through the shared publication Subscriber and Receiver,
then writes their paint projection in a bounded fixture format. A mapped GTK4
DrawingArea draws each frame in an isolated X11 session. Its realized GSK renderer
renders the widget paintable into a texture for cell, glyph and cursor checks.

- The synthetic case separates wide-continuation cell backgrounds from glyph
  geometry. Unit tests also cover one-column clipping, malformed rows and stale
  selection anchors.
- The real PTY case builds Aether session source `5598a27` against the same engine
  candidate through a fixture-only Cargo override. It launches the existing shell
  host, captures styled, wide-character output, resizes the PTY, enters the
  alternate screen, and waits for shell exit and output drain. The GTK fixture
  checks both published frames, including the cleared column after resize.

## Forge app candidate

From Forge, build the candidate against the local family sources and run it
under an isolated X11 display and session bus. The journey uses PyGObject AT-SPI,
`xdotool` and `xclip` to exercise the app's actual tab controls.

```sh
python3 ../jterm_core/scripts/family-cargo.py --forge-shared-terminal forge build --release --bin forge
dbus-run-session -- xvfb-run -a /usr/bin/python3 ../jterm_core/crates/jterm_gtk_view/tests/native.py forge target/release/forge
```

The candidate adds **New shared terminal tab** beside the existing new-tab
button. Each tab uses Forge's owned PTY and displays frames from `jterm_engine`
through a native GTK text view. The shell uses `TERM=dumb`. This tab supports
live keyboard input, screen selection, copy, paste and close. Closing the app
retires the tab alongside the other owned children. The tab is scoped to the
current window lifetime.

The candidate journey covers two independent shells, output while hidden,
reported exit status, PTY resize and paste followed by explicit Enter. Full TUI,
scrollback and reflow, Wayland and Chinese IME composition
require their own acceptance.
