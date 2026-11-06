# Existing-task validation continuation

This optional executable is a read-only preflight, separate from the five-repository
host CI contract. It never starts a host, provider, task, validation or acceptance,
imports CI evidence, obtains a control lease, or marks delivery complete.

## Preconditions and explicit input

Use only a real, already-running production host with a saved Mission and real
linked hosted task runs. Create or control those tasks only through the ordinary
authorized application workflow. Do not seed SQLite, invent task IDs, attach a
fake/failing provider, or remove the validation requirement to manufacture a pass.

Copy `validation-input.example.json` to a private invocation file. After all five
published candidates, their source pins and required CI have been reviewed, enter
the actual full SHAs, immutable baselines, absolute checkout paths, saved Mission
ID/revision and existing host state path. Then explicitly set `frozen: true`.
Never put private runtime identities or paths in a repository commit.

The schema accepts exactly the five beamiter repositories, unique paths, full
lowercase 40-character commits, compiled core `4ffa774138ee89dc2916ce21786c45e411196ca4`,
and no unknown keys. Input must be a regular file at most 64 KiB. Symlinks and
FIFOs are rejected using a no-follow, nonblocking open and descriptor metadata.
The saved Mission must match every candidate, path, baseline, canonical repository
identity, master target and frontend core pin. Only the `local` environment and
the sole required `validation` check are supported. Other environments and check
kinds fail closed instead of being upgraded from local evidence.

Only a canonical state path shorter than 70 bytes is accepted. Its existing
state, runtime and lease directories must be private and its same-user host socket
must exist. The actual core layout is `STATE/run/leases` and `STATE/run/host.sock`.
Long roots use a core-private runtime derivation and are deliberately unsupported.
Do not relocate a running host or change permissions to satisfy the preflight.

## Run and interpret

Build with the checked-in lock and explicit Rust 1.86.0:

```
cargo +1.86.0 build --locked --manifest-path scripts/mission-acceptance/harness/Cargo.toml
scripts/mission-acceptance/harness/target/debug/mission-validation-continuation /private/input.json
```

Use the actual `CARGO_TARGET_DIR` path instead if one was configured. The executable
prints a bounded JSON summary without recipes, logs, diffs, prompts or environment
variables. Host transport errors are fixed reason codes, not raw remote text.

- `blocked` / exit 1 or 2: an identity, capability, input, runtime or evidence guard
  failed, or a repository has no real linked task. Investigate through the existing UI.
- `action_required` / exit 2: the exact idle task has an available original recipe,
  but current passed validation after this acceptance contract is missing. Review
  and run that task's existing validation action only with the required authorization,
  then rerun this read-only preflight. This program does not issue that action.
- `current_local_validation_observed` / exit 0: existing validation receipts matched
  this point-in-time capture. This is **not** Mission completion, remote publication,
  four-native-UI acceptance, CI evidence ingestion or five-repository delivery.

The request allowlist consists solely of `ReadMission`, `InspectNative` and
`InspectResult`. Each task is inspected before and after its result. Conversation,
TaskId, run, event sequence, immutable baseline, checkout, original recipe, idle
and redaction state must agree. A retained completed-validation terminal descriptor
is history, not a busy flag. An already accepted task can still expose a current
Passed receipt. The host must advertise can-start-validation only when a new
validation action would be needed; otherwise that continuation is blocked. The worktree must be clean at the frozen
HEAD. The receipt must be Passed, unchanged, bound to the same fingerprint and
original command/shell, and started after the Mission's evidence boundary. This
conservative preflight currently supports recipes run at the worktree root; a
subdirectory recipe is rejected, not relabelled as a pass. The production core
receipt constructor remains authoritative for receipt validity.

At most 16 linked tasks and 180 seconds total are allowed. A 70-second request
bound is capped by the one overall deadline, including first and last Mission
reads. The entire saved Mission is reread and compared, not only its revision.
A later edit or process change revokes the usefulness of this observation.

Protocol v2 cannot attest the running host binary's source SHA. The output says
`host_binary_core_provenance: not_verified_by_protocol`; separately verify its
trusted build identity before using the observation in an experiment.

The program sends no mutating protocol operation, but does not promise zero
filesystem side effects: the production client can recreate a runtime directory
that disappears after preflight, and the host can perform its normal abandoned-
request cleanup. No security, lease, ownership or Git-trust guard is relaxed.

## Recovery validation boundary

This source was semantically reconstructed after the former cloud checkout became
unavailable. It is not claimed byte-identical to the old unpublished candidate.
Only newly executed tests and their new source hashes may be used as evidence.
Pure fault-injection tests use labelled synthetic structs, not invented real host
receipts. No real hosted-task validation was run during this reconstruction.

See `acceptance-cards.json` for the still-unrun T02, T09 and T10 experiments.
