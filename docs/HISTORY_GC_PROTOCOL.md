# F02: conservative history GC planning, stage A

## Status and non-goals

`src/history_gc.rs` is a crate-private, deterministic **model-only dry-run
planner**. It accepts an explicit in-memory snapshot and returns conditional
candidates and preservation reasons. It performs no filesystem I/O, discovers no
configuration paths, acquires no locks, and has no CLI, startup, autosave or
background hook. It does not move, rename, repair or delete files. No new
runtime dependency is required.

There is currently no producer of verified ownership manifests or complete
cross-process reference snapshots. Consequently this change does **not** expose
usable production GC, establish reclaimable disk space, or finish F02. A test
fixture marked complete is an assertion about that fixture, not proof of a real
namespace. A zero-candidate result is not a successful cleanup.

## Small model API

`plan(Snapshot)` returns one decision per distinct owned/observed basename and a
fixed-order list of global blockers. Decisions use native `OsString` ordering;
non-UTF-8 names remain distinct. There is no filename-pattern or age inference.

- `ownership_namespace`, `inventory_namespace`, `reference_namespace`: retained
  directory identities (device/inode). Missing ownership/reference identity or
  any mismatch blocks the entire plan. `reference_namespace` binds both durable
  references and live/pending reservations; neither may come from another root.
- `manifest_version`: only `Some(1)` is understood. Missing and other versions
  block the entire plan. This is a model version, not a committed disk format.
- `owned`: explicit previously recorded ownership of an exact basename and
  `FileIdentity`. This must never be synthesized from directory name matching.
- `observed`: the current identity, or `None` for unsafe/unsupported/failed
  observations. A present identity requires a no-follow regular file owned by
  the current user with exactly one hard link. Symlinks, hardlinks, devices,
  FIFOs and unreadable files must not receive an identity.
- `FileIdentity`: device, inode, byte length, nanosecond change time and a full
  content digest. The planner only compares evidence; it does not compute or
  authenticate it. The future collector must use a consistent digest algorithm
  (planned SHA-256) and observe the complete bytes without a race. Metadata or a
  digest alone is not ownership authority.
- `references`: `None` means missing/incomplete reference knowledge and blocks
  every candidate. `Some([])` is only valid after proving the complete durable
  reference set empty, including recovery sources.
- `live_or_pending`: every live pane and producer/publication/save reservation,
  including queued work that has not yet created a file or window snapshot.
- `inventory_complete`: false on truncated enumeration, missing namespaces,
  unreadable inventory, or incomplete ownership, live-pane, producer or queued-save
  enumeration. An unobserved live/pending slice must never mean known empty. This blocks all
  candidates, including apparently unrelated files.
- `legacy_or_unknown_references`: true if old/nonparticipating processes or
  unsupported/unknown reference state prevent proof of completeness.

All inputs describe **one validated directory namespace and one coherent
snapshot**. The caller must bind manifest and observations to the same retained
directory identity and supply bounded collections. No caller meeting those
preconditions exists in this stage. Invalid reference/pending basenames make
reference knowledge incomplete; invalid owned/observed names are preserved.
Duplicate ownership or observation rows are ambiguous even if identical and
are preserved. Different names claiming the same device/inode are also preserved,
even if the other identity fields differ or one claim is a missing old file. Unknown, missing, unsafe and identity-changed files survive.

A candidate requires all global preconditions, exactly one ownership row and
one safe observation with identical identity, and absence from both durable
references and live/pending reservations. Reasons have deterministic precedence:
global blockers, invalid name, duplicate conflict, cross-name physical alias,
unowned/missing/unsafe,
identity mismatch, durable reference, live/pending reference, then candidate.
A reason describes the first applicable condition, not an exhaustive diagnosis.
Candidate is a conditional model result, never an execution capability.

## Existing persistence boundaries that must remain intact

Block `HistoryFileLock` takes the directory's exclusive flock plus the
persistent per-file lock around a revisioned save. The ordinary Block load has
no lifetime read lease. Unified zone reads and saves use a short directory
flock; decoding and the rest of a pane's lifetime are outside it. Neither lock
proves that another process no longer needs the file.

Window state uses `.active` and `.state`, plus legacy `tabs.state` and preserved
corrupt/incomplete sources. These sources may retain references even after a
process exits. The restore parser's bounded or repaired output is not a complete
reference inventory. The window snapshots also do not bind old session IDs to
an exact historical configured history root. Block and Unified zone documents
must be represented separately, even for the same session ID.

Existing legacy-history migration removes an exactly validated consumed shared
source; existing window-state retention has its own cleanup. Neither is
ownership GC. This stage changes neither behavior. In particular,
`prune_stale_session_histories` remains test-only and
`saving_one_session_never_prunes_an_old_sibling` remains unchanged. A grace
period or PID disappearance cannot replace ownership/reference proof.

## Prerequisites for a later recoverable executor

1. Establish producer registration before any history publication or reference
   acquisition. Persist pending ownership/reference before publishing content;
   commit only after required file and directory syncs. Interrupted pending
   work remains protected.
2. Establish one cross-process protocol and lock order covering producers,
   reader reference acquisition, restore/claim, state save/finalize and GC.
   Include queued writes and live panes. Existing write-transaction flock is
   insufficient. Legacy/nonparticipating namespaces remain blocked unless a
   separate isolation proof makes them irrelevant.
3. Retire references only after the replacement state is durably committed.
   Preserve crash-recovery references; a vanished PID does not retire them.
   Fail closed on every unknown version, malformed state, partial parse,
   truncated scan, uncertain recovery journal and inaccessible namespace.
4. Reuse this planner for preview and execution under the same coherent
   snapshot protocol. Bind a reviewed plan to manifest/reference generations
   and file identities. Before execution, revalidate the entire plan under the
   protocol barrier; any change rejects it before mutation. Do not silently
   execute a subset or enlarged set. Stage A does not implement this binding.
5. Implement only recoverable quarantine: durable prepare journal, same-device
   no-replace rename into a private quarantine, sync both directories, then
   durable commit. Define per-file and interrupted multi-file batch recovery.
   There is no promise of all-or-nothing multi-file rename. Interrupted work
   must remain recoverable and explicitly reported, never reported as success.
6. Restore without replacing an occupied destination. Preserve both copies and
   report conflicts. Do not introduce permanent unlink or timed quarantine
   deletion as part of this phase.

## Verification

The production model includes `history_gc_tests.rs`, runnable without GTK or
any external crate:

```sh
rustc --edition=2021 -D warnings --test src/history_gc.rs -o /tmp/forge-history-gc-tests
/tmp/forge-history-gc-tests
rustfmt --edition 2021 --check src/history_gc.rs src/history_gc_tests.rs
```

Synthetic in-memory tests cover complete explicit ownership, every global
blocker, live/pending reservations, each identity component, unsupported file
observations, duplicate conflicts, unsafe basenames, missing files, deterministic
ordering, removal/addition of references and exact non-UTF-8 name handling, namespace mismatches and cross-name inode conflicts.
They do not claim that symlinks or locks were exercised against a filesystem;
those are future collector/executor responsibilities.

Before any production executor ships, add actual synthetic-directory,
multi-process and crash-injection tests: quiet live panes, unpublished new
panes, queued saves, state claim/restore races, legacy processes, every journal
and fsync interruption, parent/entry substitution, hardlinks/FIFOs/symlinks,
scan limits, unknown versions, history-root changes, no-replace restore
conflicts, repeat recovery, and exact preview/execution set agreement under a
stable snapshot. Every uncertain state must preserve bytes. Keep the existing
no-sibling-prune regression. GUI, F08 lifecycle and F13 acceptance changes are
outside this work.
