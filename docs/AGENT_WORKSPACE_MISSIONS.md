# Mission workbench: Forge integration

## What is available

**＋ New → Mission workbench · linked delivery** opens a native GTK list and
editor over the shared session host's `MissionWorkbench`. Opening, reading,
refreshing and inspecting never start a provider, validation command, commit,
push or integration. Existing task controls remain responsible for execution.
There is no new scheduler, daemon, remote runner or conversation identity.

Create a five-repository plan by supplying a title, acceptance goal and absolute
parent directory containing `jterm_core`, `anvil`, `forge`, `ember` and `frost`.
The JSON editor exposes the existing versioned plan contract: checkout paths,
baselines, frozen candidate commits, target refs, required core pins, checks,
environment and exact task/run references. Edit paths when checkouts are not
siblings. **Save plan with revision check** only persists that contract.

Select an existing hosted task in the workspace, then open a Mission and choose
**Link the last selected hosted task and run**. The shared controller reads its
actual result identity before saving a reference. Saved links display their run
IDs and open the exact conversation's existing inspector. An inspector can show
a newer run: compare its current capture with the saved run ID before using it.
A Mission link does not resume, duplicate or rerun the linked task.

## Evidence and unfinished work

**Inspect saved repository candidates** captures host-owned local, dependency,
remote-ref and validation facts. **Freeze inspected clean commits as candidates**
is a separate explicit plan edit, enabled only after a fresh explicit inspection.
Refresh, reopening, and plan changes revoke that permission even while dated
observations remain readable. Reports show candidate and observed HEAD,
fingerprint, dirty state, manifest/lock pins, per-repository committed/pushed/
integrated/validated gates, and each check's environment, status and receipt ID.
Dated remote observations include exact repository, ref, HEAD and capture time.
They remain labelled observations when current gates expire.

Missions appear in workspace navigation with stable Mission IDs, project scope
and Needs-you attention. Their goal and repository names are searchable with positive free-text terms.
Structured conversation fields (such as `kind:api` or `attention:failed`) and
negated text exclude Mission rows rather than inventing conversation facts; the
dedicated Mission catalog remains available. The
catalog keeps existing GTK button identities across updates. Reading a saved
report or refreshing expires its current gates in the shared controller; a saved
success is not proof that a checkout or remote ref is still unchanged. Missing,
stale, blocked, failed and not-run checks remain visible.

The Mission workbench now lists the shared core's specific Needs-you reasons,
including validation failure/block/staleness, missing candidates, mismatched
dependencies and unverified publication. Navigation rows expose the leading
reason. The selected Mission's GTK reason rows retain stable identities; the
full current entry, including its receipt and detail, is checked before a click
can capture evidence. Unsaved edits disable these actions and clear old captures.

Selecting a reason shows its candidate, check, environment and receipt. When
the shared core can unambiguously bind that receipt to a linked task run, Forge
makes one read-only result request and displays the validation log and captured
worktree diff on this page. The conversation, task, run and exact receipt must
match; a newer run or event is not silently substituted. Output truncation,
failed/stale validation and capture errors remain explicit. A reason without
an exact run still shows its saved details, without inventing an evidence link.
The diff is labelled as a current worktree capture with its time and fingerprint,
not as the historical validation input. Each visible log/diff is capped at
256 KiB on a UTF-8 boundary and labelled when truncated.
No action starts a provider, runs validation, changes a plan or publishes code.

## Editing and recovery boundaries

The editor checks Mission ID and revision before saving. A save acknowledgement
can replace only the submitted snapshot, never newer local edits made while it
was pending. Conflicts and uncertain write outcomes retain the draft. The shared
controller reads back uncertain outcomes instead of replaying writes. If a newer
host revision arrives while local edits remain, copy the draft before explicitly
discarding/reloading to compare and reapply it. Unsaved edits block switching to
another Mission and cannot be presented as current delivery evidence.

The host persists accepted plans and references; reopening can read them again.
Unsaved editor text is retained while changing views within this window, but is
not a durable cross-process draft. Closing a window does not authorize replaying
any task or plan write.

## Source and verification

The production/test dependency is the public shared-core commit
`4ffa774138ee89dc2916ce21786c45e411196ca4`. Cargo.lock, dependency policy,
Flatpak's generated sources, Nix's source hash and adapter compatibility records
all name that source. Its NAR is
`sha256-CZinWRNrlmclrkzh9GbiKs/SEGraa5xlIL1GyIIljuk=`. The global sibling path
patch and historical archive are not build inputs.

Local Rustfmt, five production-source mutation tests, seven real Linux PTY input
tests, adapter manifests and fresh Flatpak source-generation checks passed.
Twelve non-GTK tests (plan identity/revision, dated report rendering, conservative
Mission search, and existing task receipt/selection behavior) passed against the
actual locked public core in a temporary harness that includes the production
helpers and review module. That harness does not compile the GTK surface.
Local full-app Clippy was attempted and stopped at missing `glib-2.0.pc`; no
native acceptance is inferred from the headless helper results.
The Needs-you follow-up additionally passed six production-source evidence
tests and strict helper Clippy against the published attention API revision.
Fifteen source-pin mutation tests pass at Python optimization levels 0, 1 and 2,
including actual CLI refusal checks. Cargo metadata resolves exactly the
published core, both adapter bundles retain their checksums, and Flatpak sources
were regenerated with the pinned generator. These checks do not compile GTK.
The added GTK regression exercises pending-save edits, stale-revision rejection,
retained rows, attention/evidence state, narrow minimum width and callback
release, plus retained reason rows, rejected stale callbacks and draft-driven
capture invalidation. It is explicitly registered in `scripts/test-gtk-display.sh` and runs in
an isolated process/display, not the ordinary headless test process.

This integration is not a claim that the five-repository delivery is completed.
Hosted exact-commit GTK/build/package results must be checked after publication.
Real IME candidate UI, physical hardware, external-provider execution, real SSH
and a model request surviving GUI closure still require separate acceptance.
See [V2.1 validation](AGENT_WORKSPACE_V21_VALIDATION.md) for earlier failures,
confirmed exact-commit results and the distinction between executed and skipped
native stages.
