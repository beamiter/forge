# PR19 synthetic GTK4 frame display fixture

This test-only executable does not link Forge or VTE or start any PTY. An
independent, locked Rust harness builds against the exact shared engine subtree
at core `5f665fe`. It constructs two synthetic frames and transfers them through
the shared publication Subscriber and Receiver, then serializes the display
projection to a private bounded fixture format consumed by the GTK4 renderer.

The GTK4 DrawingArea must be mapped and draw both a normal frame and the
alternate/resized replacement. Its realized GSK renderer renders the widget
paintable into a texture; this checks exact cell/background/cursor sentinels,
rasterized narrow and wide glyph presence, and the cleared previous column.
The Rust suite verifies interpretation of wide continuations, clipping into
a one-column frame, stale selection, and malformed rows. The two native
markers are separate from Rust test output and are required for success.

This fixture does not prove Forge's production GTK4 integration, launcher,
input, IME, clipboard, AT-SPI, terminal font/layout parity, physical output,
or a real TUI. GTK package builds and runner changes are not required; the
runner uses the existing hosted GTK job packages inside an owned bus/display.
