# Forge ASCII organism prototype

This is the first executable harness from the “ASCII electronic organism”
design conversation. It validates a living state loop and a terminal body
inside Forge VTE before the body is wired into Forge's native Block overlay.

The prototype is deliberately:

- Rust and no-LLM: it performs no network requests.
- State-driven: eight bounded continuous values feed utility-based behavior.
- Event-driven: typing, build, failure, success, push, and idle are semantic
  events rather than frame-by-frame scripts in the life engine.
- Memory-aware: the manual mode stores bounded local-calendar-day/repository
  build stats using a single-writer lock and an owner-only, atomically replaced
  JSON file.
- Honest about its boundary: the executable is a VTE event harness. Its demo
  and hotkeys synthesize semantic events; it does not yet observe unrelated
  commands in another Forge pane.

## Run in Forge

```bash
./prototypes/ascii-organism/run-in-forge.sh
```

The script builds offline, opens a fresh Forge VTE window, and replays:

```text
sleep -> typing -> build -> repeated errors -> success -> git push -> memory
```

The UI explicitly labels itself `SYNTHETIC VTE HARNESS`; the displayed command
output is not captured from another pane. The scripted demo uses an explicitly
labelled synthetic “yesterday” record so
the final cross-day memory line is reproducible. It does not write that demo
record to disk.

The next native phase now lives in Forge itself behind
`ascii_organism_enabled = true` (or `FORGE_ASCII_ORGANISM_ENABLED=1`). It uses
real Block command lifecycle events and an inline widget above the live prompt.
This standalone harness remains useful for deterministic life-state and memory
experiments; its persistent/cross-day memory is not yet wired into the native
widget.

Keys inside the TUI:

```text
t typing   b build   f fail   s success   p push
i idle     r replay (demo only)   q/Esc/Ctrl-C quit
```

Manual persistent mode:

```bash
cargo run --offline --manifest-path prototypes/ascii-organism/Cargo.toml
```

Its default memory path is
`${XDG_STATE_HOME:-~/.local/state}/forge/ascii-organism.json`. Only aggregate
event counts/timings, a normalized repository-directory identifier, and a
session count are stored; raw keys, command text, and terminal output are not
recorded. A second persistent instance using the same path is refused rather
than losing updates.

## Verify without a GUI

```bash
cargo test --offline --manifest-path prototypes/ascii-organism/Cargo.toml
cargo run --offline --manifest-path prototypes/ascii-organism/Cargo.toml \
  -- --headless-demo
```

`run-in-forge.sh` prefers this checkout's `target/debug/forge`, falls back to an
installed `forge`, and accepts an explicit `FORGE_BIN` override.

The next native step is to feed authoritative cwd/repository identity and
persistent/cross-day memory into the inline widget, then evolve its body from a
prompt-adjacent card into a freely positioned overlay. The standalone life
engine in `src/life.rs` remains terminal/toolkit-independent.
