# Five-repository host CI contract

The checked-in example candidate manifest is deliberately `frozen: false`.
It is not a delivery receipt. Anvil's Mission integration is published, but the
final candidate set must wait for all required exact-head gates and a review of
the native evidence. The manual job requires explicit frozen invocation data.

## What this job actually verifies

On an ordinary GitHub-hosted Ubuntu 24.04 runner, use only these public repositories:
`beamiter/jterm_core`, `beamiter/anvil`, `beamiter/forge`, `beamiter/ember`,
`beamiter/frost`. The harness pins published core
`e8bd7a2b3edd48d7a0f8f0304fa3d8c864dab40e` and carries its own Cargo.lock.

The job reads the exact frozen five master refs and each predeclared exact-head
master workflow before checkout. The base contract requires 20 workflows, including core Security. All gates require push runs except three fixed paths-filtered workflows:
Core `security.yml`, Anvil `link-recovery.yml` and Forge `flatpak.yml`. These require an explicit
`workflow_dispatch` at that same master SHA when no push run exists. Anvil
link-recovery native lanes remain SKIPPED and are not accepted as native recovery
evidence; the Forge Flatpak bundle and smoke job must actually succeed. Missing, pending,
failed, cancelled and skipped
workflows fail closed. A newer failed run cannot be hidden by an older pass.
All frontend native Mission modules must exist at the published candidate.

It then builds the production shared core and runs a real isolated session host
against all five real Git checkouts, not generated Git evidence:

- Save one revisioned Mission and read that same identity through four independent
  protocol clients. These clients are explicitly NOT the four native application UIs.
- Inspect and explicitly adopt five clean candidates. Require actual local
  fingerprints, correct pins, exact dated public master refs and ancestry receipts.
- Read the same incomplete Mission through four `MissionWorkbench` instances.
  Each must retain Needs-you and refuse historical candidate adoption.
- Reject stale compare-and-swap only when the pinned core returns its exact
  revision-conflict error and an independent client reads the unchanged full plan
  and report identity. Transport errors, timeouts and changed readbacks fail the
  test instead of counting as CAS protection. Invalidate an in-flight inspection after refresh;
  invalidate evidence after changing the acceptance contract.
- In a NEW disposable local clone only, detect dirty input, a committed wrong
  core pin, an unpublished local commit and a missing checkout. Verify partial
  integration is distinguished from complete integration. No fixture is pushed.
- Restart only this isolated no-runtime-jobs host; retain the exact saved plan
  and dated receipts while revoking success/adoption until fresh inspection.
- Verify no task/runtime rows were created and all five positive source checkouts
  remain clean. Re-read final public refs and required workflow results.

The built-in required `validation` check is deliberately retained. No provider is
launched, no task/run identity or validation receipt is invented, and no result
can be `Completed`. A successful job produces `host_contract: passed` and
`five_repository_delivery: not_completed` with the remaining scopes explicit.

## Safety and bounded scope

- Manual `workflow_dispatch` only. `contents: read`, no secrets, no permission
  expansion, no persistent credentials, no privileged container or system edits.
- Pinned official checkout/artifact actions and Rust 1.86.0. The job explicitly
  selects `+1.86.0` for rustc and every Cargo operation, also sets
  `RUSTUP_TOOLCHAIN=1.86.0`, and asserts the recorded compiler/Cargo versions.
  A frontend's `rust-toolchain.toml` cannot silently select stable instead.
  The job never changes
  production Git trust, ownership, process leases, socket or test policies.
- Only five exact repository identities, full lowercase SHAs and a SHA256 of the
  ordered candidate set are accepted. API requests are bounded public GETs;
  redirects are rejected. No APIs create refs, issues, releases or deployments.
- Input JSON is data passed through a quoted environment variable, never embedded
  into shell code or evaluated. Checkout paths are generated under RUNNER_TEMP.
- Source audit checks each checkout's root Cargo.toml, recursively traversing its
  ordinary/dev/build, target-specific and workspace dependency tables. It rejects
  core aliases, inheritance and differing pins, and requires the canonical exact
  production root dependency. It does not claim to scan arbitrary workspace-member
  manifests. It also rejects core patches, path/replace/source overrides,
  duplicate/unqualified core lock sources, config symlinks and changed adapter
  checksums. Bundle paths cannot escape the repository. Python guards remain
  active under `PYTHONOPTIMIZE=1`.
- Uploaded artifacts contain only bounded evidence/logs, not checkout directories,
  SQLite state, home directories, environment dumps or credential files.
- Existing per-repository gates are not deleted, skipped or weakened. CI workflow
  success is a prerequisite receipt, not proof that every script's native semantic
  assertion passed. Final candidate freeze still requires reviewing native logs.

## Placement and freeze sequence

The implementation lives in Forge's `.github/workflows/five-repository-host.yml`
and `scripts/mission-acceptance/`. Core does not change just to host this job:
that would unnecessarily change the pin required by all four frontends.

1. Review the implementation and let the selected frontend's normal gates pass
   after its authorized publication. Finish all final exact-head gates first. Run core Security, Anvil link-recovery and Forge Flatpak manually on master if their
   paths-filtered push workflows did not run. Record Anvil link-recovery source/build
   scope and native SKIPPED status without substituting another gate.
2. Read all five current master SHAs, native evidence and exact-head workflow
   results. Prepare a separate JSON input using the example's schema. Only after
   the final set is reviewed should that input say `frozen: true`.
3. Compute its digest with `collect.digest_set(manifest)`. Keep the manifest as
   invocation data, NOT a commit that contains its own repository's future SHA.
   Otherwise the five-repository manifest would introduce a self-reference cycle.
4. Dispatch the reviewed workflow on master with `candidate_set_json` and
   `candidate_set_sha256`. The collector checks that the workflow's own
   GITHUB_REPOSITORY/GITHUB_SHA is a member of this exact frozen set.
5. Inspect the resulting artifact and exact job conclusion. Any source/ref change
   requires a new candidate set, not reuse of old evidence. Do not accept only a
   top-level green UI badge when the scoped receipt is absent.

The optional checked-in `frozen-candidates.json` is a disabled example only. The
workflow uses its explicit dispatch input. Default local invocation with the
example stops before any network request or output directory creation.

## Local verification and recovery status

The former unpublished PR4 integration was not available in the replacement
cloud checkout. Its source was semantically reconstructed from the published
base and documented contract. This is not a byte-identical restoration, and
historical test counts are not presented as new results. New tests, source hashes
and logs must be recorded before publication. No real five-repository host run
or existing-task validation was performed during recovery.

New recovery checks passed with Rust 1.86.0: 21 host/reducer tests plus 23
preflight tests, strict all-target Clippy, locked build and rustfmt. The 56 Python
contract tests passed both normally and under PYTHONOPTIMIZE=1. Three executable
negative-state checks returned blocked without creating runtime directories.
These are local contract tests, not a real host or five-repository acceptance.

The harness now also compiles the actual Forge Mission evidence reducer in its
unit-test target, without GTK, using the same exact core dependency. The optional
read-only continuation executable has its own input, identity, race and evidence
tests. Both are covered by the harness Cargo operations; the root application's
RustSec/deny audits do not automatically cover this independent lockfile.

See [VALIDATION_CONTINUATION.md](VALIDATION_CONTINUATION.md) for the existing-host
read-only preflight and [acceptance-cards.json](acceptance-cards.json) for the
explicitly not-run T02/T09/T10 experiments. The manual CI workflow does not start
this preflight or invent the real tasks it requires.

This host contract is one step toward the original V3 acceptance. Four actual
native applications sharing a visible Mission, real hosted task validation,
native review navigation and the final complete delivery remain separate work.
