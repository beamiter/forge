# Engineering handoff

Updated: 2026-08-01

This baseline exact-pins one shared jagent source through the hardened jterm_core,
and upgrades Agent approval, bounded AI conversations, PTY queues, persistence,
history, terminal parsing, notebook workflows, configuration, and UI safety.

## Remaining boundaries

### Decode Agent snapshots once, with budgets and semantics together

The file is capped at 256 KiB and production restore performs strict transcript and
state auditing, but the current path first constructs `AgentSessionSnapshot` and then
constructs a second audit representation. Replace both with one counted visitor that
stops before turn 129 and charges per-field and cumulative bytes during decoding.

Keep raw `AgentSession` restore private to the hardened wrapper, or make every public
snapshot reader perform the same audit so future callers cannot bypass production
semantics.

### Correlate execution completion with a one-shot generation

`pending_command` currently carries only `(ProposalId, command)` and associates the
next completed foreground block with that approval. Add an independent checked
execution generation and require captured VTE metadata to match before submitting an
observation. This becomes mandatory before supporting concurrent/external prompt
writes.

### Finish the vendored installer trust chain

After the canonical jsh installer is hardened for mandatory checksums, bounded safe
archive extraction, strict URL/version/target grammar, private atomic cache files,
and bounded version probing, resync `scripts/install-jsh.sh` and its acceptance tests.

### Keep the Flatpak Cargo source manifest synchronized

Whenever a git dependency revision changes in `Cargo.lock`, rerun the exact pinned
`flatpak-cargo-generator.py` revision from `.github/workflows/flatpak.yml` with
network access and commit the resulting `packaging/flatpak/cargo-sources.json`.
The current dependency repin still needs that regeneration; local execution was
blocked by unavailable external network access, so the generated manifest was not
hand-edited or approximated.

## Release checks

```text
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features --no-fail-fast
cargo clippy --locked --all-targets --all-features -- -D warnings
```
