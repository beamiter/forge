# Engineering handoff

Updated: 2026-08-08

This baseline exact-pins one shared jagent source through the hardened jterm_core,
and upgrades Agent approval, bounded AI conversations, PTY queues, persistence,
history, terminal parsing, notebook workflows, configuration, and UI safety.
Execution completions are now correlated by a checked one-shot identity and must
match their captured command, and the vendored jsh installer is resynced from the
hardened canonical copy.

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
  `--version` probe that writes to a file rather than a pipe.
- Completed block outcomes now delegate to
  `jterm_core::block_contract::classify_completed` only after Forge resolves the
  final command through its metadata/screen fallback. Renderer and persistence
  types stay local; failed-only and exact-exit filters use the same four-way
  outcome, so commandless background output cannot become a failure merely
  because a producer attached a raw non-zero status. The `-1` compatibility
  sentinel remains confined to downstream `i32`-only presentation surfaces and
  is never passed to the classifier.
- `jterm_core` is pinned to
  `9e79a5bf0d905575863def4d0e77f74a1f533638`; the Nix fixed-output hash and the
  Flatpak Cargo source manifest were regenerated for that exact tree while the
  direct jagent revision stayed unchanged.

## Remaining boundaries

### Decode Agent snapshots once, with budgets and semantics together

The file is capped at 256 KiB and production restore performs strict transcript
and state auditing, but the current path first constructs `AgentSessionSnapshot`
and then constructs a second audit representation. The upstream jagent now
decodes snapshots through bounded seeds that stop before turn 129 and charge
per-field and cumulative bytes while decoding; once the pinned revision is
advanced, replace both local passes with that single counted decode and audit
the decoded value directly.

Keep raw `AgentSession` restore private to the hardened wrapper, or make every
public snapshot reader perform the same audit so future callers cannot bypass
production semantics.

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
