# Engineering handoff

Updated: 2026-08-08

This baseline exact-pins one shared jagent source through the hardened jterm_core,
and upgrades Agent approval, bounded AI conversations, PTY queues, persistence,
history, terminal parsing, notebook workflows, configuration, and UI safety.
Execution completions are now correlated by a checked one-shot identity and must
match their captured command, and the vendored jsh installer is resynced from the
hardened canonical copy. The wire generation is jagent 0.6.0 plus jterm_core
0.2.0: owning transcript types are serialize-only and provider envelopes are
bounded before JSON allocation.

## Completed since the previous handoff

- `[[remote_hosts]]` gained `deploy` ("off" by default, "persist", or
  "incognito"). With it on, a remote tab runs `jterm_core::jsh_remote`'s vendored
  `jsh-remote.sh`, which places a verified static jsh on the destination for the
  life of the session and removes it afterwards — so blocks, cwd tracking, exit
  codes and the Commands timeline work on a machine nobody prepared, without
  anything being installed there, without root, and without touching the
  destination's `.bashrc`, `.profile`, or login shell. `remote_shell` is ignored
  in that mode. An unrecognised spelling rejects the host and is reported by
  config validation; it deliberately does not fall back to "off", because the
  difference between the modes is whether the destination's `$HOME` is written
  to. `build_remote_argv` splits into `build_deployed_argv` (pure, given a
  launcher path) and the publish step, so the argument order is asserted in
  tests without writing into the real cache directory, and a failure to publish
  degrades to plain ssh rather than refusing the tab.

- `pending_command` carries a `PendingExecution { proposal_id, command,
  generation }`. The generation is checked and never reused, and a finished
  block whose captured VTE command differs from the approved one now consumes
  the approval *without* submitting an observation. Feeding the model the output
  of a command the user did not approve is worse than losing an observation,
  and that mismatch is exactly how a concurrent or external prompt write would
  surface.
- `scripts/install-jsh.sh` is resynced from the hardened canonical jsh copy:
  mandatory format-checked SHA-256, byte-bounded HTTPS-only downloads, validated
  version/target/base-URL grammars, archive members checked for links,
  traversal, and extra payload before extracting exactly the expected binary,
  private and symlink-safe atomic cache and staging files, and a deadline-bounded
  `--version` probe that writes to a file rather than a pipe. It now matches
  upstream commit `fd605616b56bd73265a3a6141c814938aa2859f9`: archive checks use
  explicit branches, and failed self-check rollback reports success only after
  the private temporary copy is chmodded and atomically renamed into place.
- Completed block outcomes now delegate to
  `jterm_core::block_contract::classify_completed` only after Forge resolves the
  final command through its metadata/screen fallback. Renderer and persistence
  types stay local; failed-only and exact-exit filters use the same four-way
  outcome, so commandless background output cannot become a failure merely
  because a producer attached a raw non-zero status. The `-1` compatibility
  sentinel remains confined to downstream `i32`-only presentation surfaces and
  is never passed to the classifier.
- `jterm_core` is pinned to
  `586d84739c490d74918778a31441040ed7a36a4b` and direct jagent to
  `f3b9b9a95d494619b0e623bd96afa70311f9ca26`; Cargo, Nix, and the twice-generated
  Flatpak source manifest all describe those exact trees.
- Agent snapshot restore now decodes once through jagent's allocation-aware
  schema and audits its immutable accessors directly. Forge's stricter
  contiguous-ID, final-pending, immediate-observation, turn-accounting, state,
  and anti-rebinding rules remain in front of live session restore; the former
  `AgentSnapshotAudit` ordinary-serde copy is gone.
- App-local non-streaming AI transport now keeps successful curl bodies as raw
  bytes through jagent's canonical 1 MiB response gate. HTTP errors may still
  be retained as bounded transport evidence, but only the first 2 KiB can enter
  diagnostic JSON parsing; exact-limit, limit-plus-one, and large-error tests
  pin those boundaries.

## Remaining boundaries

### Carry the execution generation into the completion callback

`connect_block_finished` still delivers `(command, exit_code, output)` with no
execution identity, so the generation stored beside an approval is one-shot but
not *verified* by the completion. Thread it through the block-finished path —
this becomes mandatory before supporting concurrent or external prompt writes,
where the captured command alone can no longer distinguish two executions.

## Release checks

```text
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features --no-fail-fast
cargo clippy --locked --all-targets --all-features -- -D warnings
```
