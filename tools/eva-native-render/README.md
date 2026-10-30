# Native offscreen Eva text fixture

This tiny harness imports **the same `src/eva_companion_paint.rs`** used by Forge's GTK label. Anvil's matching source is identical. It is intentionally independent of GTK initialization, Unix sockets and a desktop display.

```sh
# From the Forge checkout, with Rust and Pango/Cairo development libraries
# installed and their pkg-config/runtime loader paths configured.
mkdir -p qa/eva-native
cargo test --manifest-path tools/eva-native-render/Cargo.toml
cargo run --quiet --manifest-path tools/eva-native-render/Cargo.toml > qa/eva-native/frames.json
XDG_CACHE_HOME="$PWD/qa/eva-native/.cache" python3 tools/eva-native-render/render.py qa/eva-native/frames.json qa/eva-native
```

The native `libpango`, `libpangocairo` and `libcairo` libraries shape and draw all 8 skins × 13 poses at 96/144/192 DPI (100/150/200%). The JSON records measured pixel extents, provenance, exact enum, seed, elapsed time, frame index and candidate/input/approval rectangles. Generated PNGs cover all104 poses at96DPI and all8 idle skins at144/192DPI. Raw JSON/per-frame PNGs are rebuildable and ignored by Git; numeric measurements and the native contact sheet are shipped. `native-contact-sheet.png` is actual native output, not the design illustration.

This verifies offscreen glyph colors, measurable bounds and non-overlapping admission examples. It does **not** verify GTK window composition, live callback timing, SIGSTOP pixel retirement, Unix IPC, real IME, a GPU, or a microphone. The live app additionally uses a GTK snapshot clip widget whose measured allocation and clip are the admitted native rectangle.

The build script extracts the actual `safe_edge` function from the production GTK owner. The second test replays all312 measured native extents through that selector with the real protocol Rect implementation, checks exact admitted size, input/approval exclusion, and full-coverage hide. It is not a reimplemented geometry stub.
