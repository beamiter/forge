# Engineering handoff

Updated: 2026-08-29 (workflow subsystem upstreamed to jterm_core::workflows;
the family-wide unfilled-argument guard closed, forge's silent TOML coercion
removed, the bundled library reconciled with its three siblings)

This working tree contains the nine-round "Evolve ASCII organism" series
(`d6fb8b4..00a099e`), the continued pass (`fa5c947`), the recovery-vigil
layer, and the current five-round debugging-vigil evolution: the experimental
Block-pane organism grew from an event reflex into a continuous life
simulation with utility-selected behavior, agent attribution, interpolated
spatial motion, motion restraint, a persistent per-repo build-duration
baseline, and a five-part embodiment pass covering visible growth, output
rhythm, semantic transitions, repo territory, and attention arbitration.
Every historical round followed the
same loop — implement, unit-test, sync `docs/USER_GUIDE.md`, run a
three-agent adversarial verification workflow, fix confirmed findings,
commit, push. The verification pass caught one to two real defects almost
every round (attribution races, a Drop panic on a fired glib source, a
saturation hole in a validate invariant); do not skip it.

## Completed since the previous handoff

- **Review follow-ups on the viewport and Git-metadata work (current working
  tree)**: three defects an adversarial pass found in the two entries below,
  fixed in the same tree.

  The differential visibility pass visited `new_visible ∪ visible`
  (`block_view/mod.rs:10693`). That is exact while `visible` still names the
  cards GTK is laying out in full, which a scroll tick preserves and nothing
  else does. Two shapes broke it. A card mounted into the list — a command
  finishing below the viewport, an undo, a history load — arrives
  un-virtualized from `FinishedBlock::new`, so it is in neither set and nothing
  ever virtualizes it: it keeps its whole output allocated while the document
  holds the font-metric estimate it was appended with, and the pixel→card map
  drifts by the difference for the rest of the session. And
  `refresh_filtered_layout` (`:15806`) and `restore_deleted_blocks` (`:15518`)
  clear `visible` on purpose, because after their rewrite the old indices
  describe nothing — which left whatever was on screen before the filter toggle
  rendered forever, one screenful per toggle. The obligation to sweep every
  card now rides on `BlockDocumentIndex::take_full_sweep_due`
  (`:10290`), set by every rebuild — and a rebuild is already what a length
  change or an explicit stale mark forces, which is exactly the set of events
  that can invalidate `visible`. The two clearing callers say it themselves
  through `require_full_sweep` (`:10301`) rather than leaning on that
  coincidence. A scroll frame moves no card and marks nothing stale, so it
  stays differential. The pass now takes `&[C: VirtualizableCard]`
  (`:10674`) instead of `&[FinishedBlock]`, which is what finally lets the
  transition be tested: every widget test in this crate is `#[ignore]`d for want
  of a display, so before this seam the one function that decides what stays
  laid out was pinned by nothing that runs in the gate.

  The bottom bar invalidated Git state for `current_pane_leaf()`
  (`ui/bottom_bar.rs:106`) — the active leaf of the *visible* notebook page —
  even though `connect_bottom_bar_block_status` is installed once per pane. A
  command finishing in a background tab or a sibling split therefore marked
  some unrelated directory, and its own pane kept `invalidated == false` and a
  seconds-old `refreshed_at`, so `probe_is_due` stayed false and the bar showed
  the old branch for up to the full 30 s TTL — the 1 Hz probe that used to
  paper over this is precisely what the cache replaced. The handler now
  resolves the directory from a weak handle to the view it is installed on,
  through the same `pane_working_directory` the bar itself reads, so the key it
  marks is the key the next repaint looks up.

  `worker_loop` wrote `invalidated: false` on every completion, so an
  `invalidate` that arrived while `jterm_core::git_meta::read` was running was
  discarded and the change it reported waited out the TTL. Running two commands
  in a row on a large or FUSE-backed checkout is the ordinary way to hit that.
  A `CacheEntry` now carries a monotonic `invalidations` count and the
  `probed_at_invalidations` generation its stored answer was queued for
  (`git_meta.rs:38`); `invalidated()` is the two disagreeing, and a probe can
  only ever clear the generation it was actually asked about. The request
  carries that generation, read as late as possible but still before the worker
  can start Git, so a report landing in the gap costs one redundant probe
  rather than an answer that claims to cover a change it never saw.

  That covered every path with an entry to mark, and the *first* probe of a
  directory has none: `invalidate` only bumped an existing `CacheEntry`, and
  the entry is created by the worker when Git answers. A pane that has just
  opened, or has just changed directory, sits in exactly that window, and a
  cold checkout is the slowest probe there is — so the likeliest instance of
  the race was also the one still dropping its report, and the answer that
  landed afterwards was served for the whole TTL. `invalidate`
  (`git_meta.rs:132`) now records the report on an entry that holds no answer,
  which `worker_loop` carries forward like any other. `refreshed_at` became
  `Option<Instant>` (`:48`) so such an entry says plainly that nothing has
  answered for it instead of claiming a probe it never had, and both writers
  share one `insert_bounded` (`:198`) so a bare report cannot introduce a cache
  key outside the 256-entry ceiling.

- **Core repin `9f94f77` / jagent `bdc8023`, and journaled output bound to a
  lifecycle token (current working tree)**: `CompletedExecution` no longer
  carries a bare `id`. It carries an `ExecutionLifecycle`, a private-field
  capability whose only constructor takes one complete `CommandMeta` and
  returns `None` unless `id`, `session_id`, `seq` and `started_at_ms` all
  arrived on the *same* OSC 133 `C` packet — which is the only mark core's
  parser now accepts the three identity slots on. So `PendingCommandMeta`
  (`src/block_view/mod.rs:1083`) mints the token at `C` and nowhere else:
  `merge_command_end` is untouched and still refuses to let a `D` packet
  replace the id, and a `D` whose id disagrees now clears the lifecycle as well
  through one `forget_correlated_identity` (`:1213`), because at that point
  the token names some other execution's record. `journal_execution_id` became
  `journal_lifecycle` (`:1205`); both of the provenance filters it carried are
  preserved and now applied to the token's own id in
  `journal_lifecycle_provenance_admits` (`:1236`), which is the string that
  would actually reach disk. Those two filters are: the per-pane
  shell-integration secret (`pty.rs`, `scripts/shell-integration/forge.*` mint
  ids as `<token>-<seq>`) must never be written into a durable file every app
  in this family reads back; and an id jsh did not mint (`jsh-` prefix) has no
  journal Start to attach to. Journaled output therefore now requires a
  complete jsh lifecycle envelope: forge's own shims emit `id=` alone, so
  panes running under `forge.bash`/`.zsh`/`.fish` contribute no output events
  at all, which is the correct answer rather than a regression — they never had
  a jsh journal to contribute to.

  Repinned together: `Cargo.toml` (both `jterm_core` and the direct `jagent`
  pin, which must move as one or cargo resolves two revisions of one package),
  `Cargo.lock` (regenerated by building, no `path`/`[patch]` residue),
  `deny.toml`'s two `allow-git` revs, and `flake.nix`'s two `outputHashes`
  with their rev comments. `CompletionFacts` gained a lifetime and its `output`
  is now `&'a str`, so `ui/command_correction.rs:330` borrows instead of moving
  a whole finished block into the gate. jagent's classifier grew from 32 to 54
  warning classes and reaches the approval cards for free — forge keeps no
  duplicate danger list to retire (`src/agent.rs` is a pure re-export of
  `jterm_core::agent::is_dangerous`, and the two call sites at
  `ui/agent_panel.rs:986` and `ui/command_review.rs:351` use it directly). The
  two new fixture affordances are taken up as well: `CorrectionCandidate::
  for_tests` finally covers the correction card's *verified* branch — the one
  where the primary button says Run and executes — including that an edit to
  the field moves it back to insert-for-review, and
  `CorrectionPolicy::probe_thread_name()` pins the reader thread's name against
  a literal rather than against the constant that was just handed in.

- **Config edits survive an external reload (current working tree)**: the
  config-file watcher debounces 200 ms (`main.rs:1980`) while a settings edit
  waits 250 ms (`CONFIG_PERSIST_DEBOUNCE`) or 400 ms for a font step. A reload
  landing inside that window replaced the whole `Config`, the pending edit
  with it, and the debounced write then persisted the *reloaded* snapshot — the
  user's change disappeared with nothing on screen about it. `ConfigDirtyEpoch`
  (`ui/config_apply.rs:113`) counts UI-originated edits on the GTK thread
  against an atomic high-water mark the persistence worker raises with
  `fetch_max` when a write actually commits, so an edit made while a write was
  in flight stays dirty and a late older write cannot un-save a newer one.
  `decide_config_reload` (`:73`) turns that into Skip / Apply / Conflict, with
  Skip outranking Conflict because bytes this window already accounts for are
  not an external change. A conflict raises a modal whose close and Escape both
  resolve to keeping the unsaved edit — discarding the user's work is the
  destructive half and must be asked for out loud — and whose destructive
  answer cancels both debounce generations and the queued font-zoom sweep
  before adopting the file, so no armed timer can write the discarded snapshot
  back afterwards.

- **The GTK thread no longer waits on the config revision mutex (current
  working tree)**: `save_config_with_path` held `config.persistence_revision`
  across `ConfigFileLock::acquire`'s two-second spin, the backup rotation and
  every `fsync`, while `live_config_revision` took the same mutex on every
  reload decision. `RevisionBinding` (`config_store.rs:1134`) splits the read
  and the publish out of `write_config_under_lock`, and `save_config_bound`
  takes the *file* lock first, then reads the expected revision inside it.
  Writers stay serialized exactly as before — by the advisory lock, which is
  what actually orders them — while the slot is held only for the two moves.
  Reading `expected` after the lock is what keeps that safe: the revision this
  save compares against is the one the previous writer published, not one read
  seconds earlier. The UI side reads with `try_lock`; contention defers the
  decision by 20 ms up to four times rather than guessing, because "unknown"
  reads as "the file moved" and would apply a reload nobody proved was safe.

- **A reload that cannot be trusted keeps the live settings (current working
  tree)**: `reload_config` reads the file twice — once to take its revision and
  check its syntax, once inside `load_config`, which is the read that produces
  the `Config`. `load_file_config` answers a read error with
  `FileConfig::default()`, so a failed or raced second read replaced the theme,
  the keybindings, the remote hosts and everything else with defaults and said
  nothing. `reload_read_failure` (`ui/config_apply.rs:98`) refuses the reload
  when the loader recorded an error or when the revision it loaded is not the
  one this reload validated, and says so in the same dialog the other reload
  refusals use.

- **Block viewport resolution is a tree descent, not a walk (current working
  tree)**: resolving the strict and hysteresis windows walked `block_data` from
  card zero on every scroll tick, and `apply_visible_indices` then swept every
  finished card — with `max_visible_blocks` admitting 100 000, one 60 Hz frame
  cost as much as the whole retained session. `BlockDocumentIndex`
  (`block_view/mod.rs:10163`) keeps the same `block_document_height` values in
  a Fenwick tree; both viewport edges reduce to one primitive,
  `card_at_document_y`, and zero-height cards (the ones a pane filter removed
  from the document) can never be its answer because they do not move the
  prefix. The index is a cache with two rules that between them cover every
  writer: a structural mutation changes the list's length, which `reconcile`
  answers with a rebuild; a height written without a length change either
  patches the tree in the same statement (`set_height`, used by the visibility
  appliers, the only writers that run per frame) or calls `mark_stale` (the
  filter, collapse, density and refit paths, none of which run per frame).
  `apply_visible_indices_with_measurement` now visits only
  `new_visible ∪ visible`; cards in neither set are already virtualized at the
  height the document records, because `set_virtualized` returns early with
  that exact value when the state does not change. Measured at 50 000 cards,
  scrolled to the bottom: 102 µs per from-zero walk against 52 ns per indexed
  strict+loose resolution.

- **The Agent panel's Git probe left the GTK thread, and the strip's cache
  grew a TTL (current working tree)**: `request_model` called the bounded
  *waiting* reader on the GTK thread, so submitting a turn froze the panel for
  as long as `git` took — unbounded in practice on a FUSE or network checkout.
  The probe and the prompt assembly moved into the request thread that was
  about to wait on the network anyway, which keeps the freshness the model
  wants and costs the panel nothing. Separately, `read_cached_and_refresh`
  queued a probe on every call and the bottom bar calls it once a second, so an
  idle window forked one `git status` per second forever. Cache entries now
  carry `refreshed_at` and an `invalidated` flag: a finished command
  invalidates the focused pane's cwd — that is when Git's answer can actually
  move — and a 30-second ceiling exists only to notice a change made by another
  window or another terminal.

- **A truncated command packet can no longer be persisted as exact (current
  working tree)**: `cmdline_url=<prefix>;cmd_truncated=1` resolved to
  `CommandTextSource::ShellReported`, which is what makes `command_exact` true
  on the persisted record and unlocks agent replay of the text as written — so
  a self-contradictory packet handed the agent half a command line to re-run.
  `resolve_command_for_block` now checks the disclosure first. Core's parser
  drops `command` on this shape as of the pin this tree builds against
  (`jterm_core` `4d8c814`, not in the old pin), so the contradiction should no
  longer arrive; the check stays because these marks come off the PTY, where
  anything a foreground process printed is indistinguishable from the shell's
  own output.

- **Remote directory downloads extract into private staging (current working
  tree, security)**: a download of `~/proj` into `$HOME` piped an untrusted tar
  straight into the *parent* — `tar xf - -C $HOME` — and validated nothing. A
  hostile host returns `proj/...` plus ordinary relative siblings, `.bashrc`,
  `.ssh/authorized_keys`, `.config/forge/config.toml`, and they land. Nothing
  about those paths is exotic enough for tar to refuse them.
  `download_dir` now extracts into a `0700`, process-owned staging directory
  created beside the target with `create_dir` (so it cannot reuse anything
  already there), checks that the result is exactly one top-level entry, that
  it is named what was asked for, and that it is a real directory rather than a
  link pointing wherever the host chose — then publishes with a
  same-filesystem rename. Staging is removed by a `Drop` guard on every path:
  on failure it holds the partial extraction, on success an empty shell. The
  fix is deliberately local to forge; frost has the same bug and owns its own.

- **Block-history fail-closed states wait for an answer (current working
  tree)**: a history save that refuses — its revision moved, the load it must
  not overwrite failed, the volume is full, the lock never cleared — arrived as
  the same eight-second toast every routine persistence failure gets. The one
  class of failure that needs a decision was the class most likely to be
  missed. `persistence_failure_surface` (`ui/history_notice.rs`) routes that
  one operation to a persistent bar with a Retry, and `Retry` asks each Block
  pane for `retry_history_persistence`: a pane whose *load* failed restarts the
  load, because saving again would refuse again and must — that refusal is what
  stops an unreadable file from becoming a licence to overwrite what is really
  on disk — while every other pane simply saves again. Nothing reports success;
  the only honest signal is the next failure, which raises the bar through the
  same path.

- **Zone-history restore reads through the shared bounded reader (current
  working tree)**: `read_session` did a path-based `stat` and then a separate
  `read`, so it decided on one file and read whichever file the path named a
  moment later, and it believed the size it was told. It now calls
  `jterm_core::snapshot_file::read_bounded`, which checks the open descriptor,
  caps the read itself, and refuses a fifo (which the old path would have
  blocked the restoring thread on), a device, a hard-linked file, and one
  another user can write. Deliberate behaviour change: the oversize error kind
  moved from `InvalidData` to `FileTooLarge`; no caller branches on it,
  `restore_zone_history` only logs.

- **Workflow subsystem upstreamed (2026-08-29)**: `src/workflows.rs` is a
  288-line policy shim over `jterm_core::workflows`, pinned at core rev
  `790d06a`. It was 801. The four terminals each carried the same TOML/YAML
  "saved command with parameters" library — anvil 772 + 144 + 248, forge 801,
  ember 867 + 284, frost 827 + 316 — and the shared module is 3,185 lines
  across five files with 73 tests. `src/` here is +532 / −885 over five files
  (`workflows.rs`, `ui/dialogs.rs`, `ui/command_palette.rs`, `palette.rs`,
  `cli.rs`).

  **These four apps read the same files out of the same directories.** That is
  what makes this surface different from the chat store or the correction
  engine: a difference in what one app *accepts* is a difference in what a
  user's file MEANS depending on which terminal opened it. The divergences
  were therefore never style.

  **The defect that was in all four.** `render()` is supposed to refuse an
  argument that declares no default and was left blank, and anvil, ember and
  frost each unit-test that guard. Every UI in the family pre-seeded each
  declared argument with `""` when it built the dialog, so it never fired:
  `kill -9 {pid}` inserted `kill -9 ` when the user tabbed past the field. A
  guard green in isolation and dead in practice — and this repo's
  `docs/USER_GUIDE.md` has promised it in writing since `7192e59`
  (2026-07-16, 280 commits ago): "未提供的必填参数不会静默执行". The contract is now stated once — an empty
  value is meaningful only if the file says so — and enforced in two places:
  in `render()`, which applies it to the values map itself so a caller that
  pre-seeds cannot seed past it, and in `ArgsForm`, which carries `Unset` vs
  `Supplied` in the type system. Emptying a *defaulted* field stays a
  deliberate empty value; emptying an *undefaulted* one is a missing value.

  forge could not have implemented that guard at all: its `WorkflowArg::default`
  was a `String`, so "no default" and "empty default" were the same value. Its
  YAML front-end actually deserialised `Option<String>` and then destroyed the
  information at `unwrap_or_default()`; the TOML front-end never had it. The
  shared schema is `Option<String>`.

  **forge was the outlier lineage**, and every claimed divergence reproduced in
  `HEAD:src/workflows.rs` before it was deleted:

  - *Type-wrong TOML was repaired, not refused.* The parser was hand-rolled
    over `toml::Table` with `as_str().unwrap_or("")` and
    `filter_map(Value::as_str)`. `default = 3000` — an unquoted port, the most
    natural authoring mistake on this surface — silently became the empty
    string and the file loaded; the dialog showed a blank Port field and Insert
    put `lsof -ti tcp: | xargs -r kill -TERM` at the prompt. `tags = ["net", 1]`
    dropped the bad element; an `[[args]]` entry with no `name` dropped the
    argument, leaving its placeholder in the inserted command. Both formats go
    through serde derive now, so all three reject the file with a message
    naming the problem. The other three already refused all three files.
  - *Zero-argument workflows skipped `render()`.* `workflow.command` went
    straight to the pane at both activation sites, so forge's own documented
    `{{ }}` literal-brace escape did not apply there and the template never
    crossed validation on that path. The bug lived in `ui/dialogs.rs` and
    `ui/command_palette.rs`, which is why it is invisible in a diff of
    `workflows.rs` alone.
  - *Placeholder names were not trimmed.* `&template[i + 2..close]` raw, so
    `{{ service }}` — how a mustache-convention shared library is written —
    rendered `{ service }` literally into the command.
  - *An unterminated `{{` advanced by two bytes.* `awk '{{print $1}' file`
    was rewritten to `awk '{print $1}' file`: a different, executable awk
    program. Core advances by one and re-scans, and computes each opener's
    matching `}}` by depth so a later pair's close cannot be claimed by an
    earlier `{{`.
  - *Descriptive errors were built and discarded.*
    `let Ok(contents) = read_bounded_workflow(&path) else { continue };` and
    `toml::from_str(..).ok()?` — an oversized, symlinked, non-UTF-8 or
    unparseable TOML file vanished from the palette with no log line at all.
    That silence is why the rest of this list went unnoticed for as long as it
    did. Every skip now logs `workflows: skipping <path>: <reason>`, both
    halves through `review_input::safe_inline_display`.
  - *The user tier could be CWD-relative.* `workflows_dir()` fell back to
    `std::env::var_os("HOME").unwrap_or_default()`, so with `HOME` unset forge
    scanned `./.config/forge/workflows`: clone a repository containing that
    directory, start forge inside it, and its files were the
    *highest-precedence* workflows. The tier is `glib::user_config_dir()` now
    and a non-absolute answer is a skipped tier, not a resolved one.
  - *The command template was never checked for visual spoofing.*
    `command_is_reviewable` ran only the length bound and
    `review_input::validate`; `contains_visual_spoofing` was applied to name,
    description and tags but not to the one field that reaches the prompt.

  **Policy is injected, not hardcoded, because each of these would silently
  change behaviour for two of the four apps.** `const APP: &str = "forge"`
  (the `FORGE_WORKFLOW_DIR` override is *derived* from it, so one app cannot
  read its own directory while honouring another's variable);
  `const LOAD_ORDER: LoadOrder = LoadOrder::ByName`, which has no `Default`
  and is pinned at every construction site — anvil and frost list in
  directory-precedence order, and in all four copies that difference was the
  presence or absence of one `sort_by` line; a `GlibDirs` `DirSources` impl,
  because anvil and forge ask glib and ember and frost ask the `dirs` crate,
  whose fallback chains differ exactly at the edges that matter; and
  `dev_root()` passed into the spec, because `env!("CARGO_MANIFEST_DIR")`
  resolves against the *compiling* crate and evaluating it inside jterm_core
  would point all four apps at `jterm_core/scripts/workflows` — with their
  bundled-library tests still green, asserting about a directory that does not
  exist. `SearchPathSpec::for_current_app` was refused in favour of
  `for_app("forge", ..)`: it reads the process identity, which answers `"jterm"`
  before `identity::init` and in every test binary.

  **Two changes made by hand after the migration**, both affecting every app
  equally, both present in this tree:

  - `scripts/workflows/docker-tail-logs.yaml` declared `default: ""` for its
    required `container` argument. Under the new contract that is an
    *explicitly declared* empty value, so the round's headline guard would not
    have fired on the example the apps ship — Insert would have produced
    `docker logs -f --tail 100 `. The line is gone; `container` is a required
    argument and the guard now fires on it, which
    `a_bundled_workflow_renders_from_its_declared_defaults_alone` observes.
  - forge's bundled library differed from the other three in 5 of its 6 files,
    and `find-large-files.yaml` differed substantively: under the same name
    "Find large files", forge shipped
    `find . -type f -printf '%s %p\n' | …` with one argument, the other three
    `find {{dir}} -type f -size +{{min_size}} …` with three. Dedup is
    name-keyed and first-wins, so one shared library resolved to two different
    commands — which defeats the point of a shared format. Tags diverged too
    (`[net, debug]` vs `[network, diagnostics]`, so `network` recalled
    ssh-tunnel in forge and nothing in its siblings). Verified after the fact:
    `diff -r` against anvil, ember and frost is clean in all three directions.

  **Three adversarial audits ran against the shared module before any app
  adopted it** and found nine defects in it, five serious. Two lenses
  independently caught `SearchPathSpec::for_current_app` resolving to the
  neutral `"jterm"` identity when `identity::init` had not run, which would
  have changed every directory read — in tests most of all, since they never
  call init. It returns `Option` now, and the test that had guarded it was
  itself vacuous: the old assertion held for `"jterm"` too, so it was green
  precisely when the bug was present. Do not skip that pass on the next
  extraction.

  Gates, measured in this tree under `nix develop`: `cargo fmt --check` clean,
  `cargo clippy --offline --locked --all-targets -- -D warnings` clean,
  `cargo test --offline --locked` 1,443 passing / 33 ignored (display-gated) /
  0 failing — re-run after `serde_yaml_ng` was dropped, and still green.
  `Cargo.toml` and `flake.nix` both move to core `790d06a` and
  `~/.cargo/config.toml` carries no `[patch]`, so `--locked` is against the
  pushed core rather than a local checkout — unlike the previous two rounds,
  this one is committable as it stands. `nix build` was not run, so
  `flake.nix`'s `outputHashes."jterm_core-0.2.0"` for `790d06a` is unverified
  here; check it before relying on the Nix path.

  User-visible consequences are in `CHANGELOG.md` (Changed, including a
  standalone upgrade note for the library shrink, and Security), in
  `README.md`, and in `docs/USER_GUIDE.md` §6 — the missing-value rule belongs
  in the guide because the guide already promised it.

  Not done:

  - anvil's `RefreshLatch` + background rescan was not adopted. Two of the
    three `load_all()` calls remain, one per palette open, on the GTK main
    loop (`ui/command_palette.rs`, `ui/dialogs.rs`); the third — a full
    five-tier rescan on *every activation*, having already walked the same
    path to build the list the user just picked from — is gone, resolved in
    the snapshot the palette was built from. `jterm_core::workflows::RefreshLatch`
    is exported and ready. forge is the one app in the family with a main loop
    to block, so this is worth doing.
  - (Was open, now closed while this handoff was being written.)
    `serde_yaml_ng` had exactly one user in this crate, the deleted YAML
    parser; nothing under `src/` or `tests/` mentions it and no lint fires on
    an unused dependency, so it survived the migration. It has since been
    dropped from `Cargo.toml` and `Cargo.lock` — the lock now shows it moving
    from forge's own dependency list into `jterm_core`'s, which is the shape
    the extraction should produce. The gate was re-run after that change and
    is still green.
  - forge's unified command palette (`palette.rs::gather`) still ranks
    workflows with its own fuzzy path alongside actions and history; only the
    standalone `Ctrl+Shift+M` overlay uses `WorkflowPicker`. That is fine — the
    two surfaces have different result budgets — but it means the two ways to
    reach a workflow in forge rank it differently.

  Claims in the migration report that do not survive this tree, recorded so
  the next reader does not chase them:

  - It reports `scripts/workflows/` as NOT reconciled, blocked by a
    permission classifier. It *is* reconciled here (all six files byte-identical
    to anvil, ember and frost) and `docker-tail-logs.yaml`'s `default: ""` is
    gone. Both were done by hand afterwards; see above.
  - It reports `Cargo.toml` left untouched and forge building only through a
    temporary `[patch]`. Neither holds: `Cargo.toml`, `Cargo.lock` and
    `flake.nix` all name `790d06a`, there is no `[patch]` in
    `~/.cargo/config.toml`, and the unused `serde_yaml_ng` line it flagged has
    also been removed. `--locked` therefore builds against the pushed core.
  - It reports `WorkflowPicker` as unused by forge. `ui/dialogs.rs` builds one
    (`PickerPolicy::new(15, true)`), which is a user-visible change the report
    does not list: the `Ctrl+Shift+M` overlay went from lowercased-substring
    filtering over every loaded row to skim fuzzy matching re-ranked by score
    and capped at 15 drawn rows. It is in `CHANGELOG.md`. The command template
    stays in forge's haystack, which is the knob the report describes.
  - It reports 1,445 tests. The measured number is 1,443 passing with 33
    display-gated ignored.
  - The round brief sizes `jterm_core::workflows` at 2,610 lines / 62 tests.
    At the pushed rev `790d06a` it is 3,185 lines / 73 tests across five files.
  - `ui/dialogs.rs`'s new comment says the old path "called `substitute`, which
    … validates neither its bindings nor its output". True of *core's*
    `substitute`, not of forge's deleted local one, which validated binding
    names and values and ran `review_input::validate` on the rendered text. The
    real gaps in the old dialog path were that it never re-validated the
    workflow and had no missing-value rule; the zero-argument path bypassed
    `substitute` entirely. Likewise "its only validation log was
    `workflows: invalid tag` with no filename" is half right: the field
    validator did log without a filename, but `command_is_reviewable` and the
    YAML parse-error arm both logged the sanitised path. What was fully silent
    was the bounded reader and the TOML parser.
  - The brief says `ArgsForm` lets a UI "disable Insert before the user sees an
    error". forge deliberately does not disable it: `missing()` is a superset
    of what `render()` will actually refuse (an argument the template never
    references does not block a render), so the hint is advisory and `render()`
    stays the single authority. Insert remains live and reports
    `missing values: …` in an alert.

  `UPGRADE_ROUNDS.md` was deliberately not extended. Its last entry is round
  53 and it has not been touched in 30 commits (`c1daab8`, 2026-08-25); the
  workspace-identity, agent-restore, Codex-task, chat-store and
  command-correction rounds all landed without adding to it. Continuing the
  numbering here would imply a ledger discipline this repo stopped keeping.

- **Command correction upstreamed (2026-08-29)**:
  `src/ui/command_correction.rs` is an 888-line shim (737 production, 151 test)
  over `jterm_core::command_correction`, pinned at core rev `badcce2`. It was
  2,148. The four terminals each carried a private copy of the same "that
  command failed, here is a fix" flow — anvil 1,817, forge 2,148, ember 2,335,
  frost 1,552 — 7,852 lines whose engine half held no toolkit code whatsoever,
  which is exactly why four copies were free to drift and did. Core's module is
  3,937 lines including its tests; the four apps shed 6,294 lines between them,
  `src/` here 1,248 (+437 / −1,685 across four files).

  What went up: classification, token extraction, typo ranking, the safety
  gate, the provider prompt, the strict-JSON reply parser, helper resolution,
  the bounded probes, the two-stage resolver and the request epoch machine.
  What stayed is genuinely forge's: the Notebook attachment layer that reaches
  nested split leaves, the 50 ms GLib poller that hands a worker result back to
  the main context, the inline card in the block conversation (inserted above
  the live prompt and styled like a finished block, not shown as a modal), and
  the tracked-submission path — the verified command kept present and
  insensitive until `CommandStart` proves its identity, with the organism
  assist-pulse revoked if that proof never arrives. No sibling has that last
  one, and it is the reason this shim is the largest of the four.

  **This surface decides whether a model-proposed command may be offered for
  execution, so the divergences were not style.** Three were live holes here:

  - *A third user's binary was a trusted system helper.* forge asked
    `mode & 0o022 != 0 || (uid == euid && mode & 0o200 != 0)` in
    `host.rs::trusted_system_executable`, and helper resolution reached
    candidates by scanning the user's own `PATH`. A `bash` owned by another
    account at mode 0755, earlier in `PATH` than `/usr/bin` on a shared build
    box, answered "not writable by me" to that predicate and was spawned
    automatically by any failed command — no prompt, no user action beyond
    mistyping. Clamping the *child's* `PATH` (which forge did, to
    `/usr/bin:/bin`) never helped: the helper is itself the hostile binary. The
    same expression inverts under euid 0, where `uid == euid` holds for every
    root-owned binary, so a forge in a container or under `sudo` refused every
    system helper and silently produced no APT- or PATH-verified correction at
    all. `jterm_core::helper::trusted_component` already answered both halves,
    and of the four only frost used it.
  - *A candidate could add a pipe into a shell.* `syntax_markers` asks only
    whether a marker is PRESENT, so against an original that already contains a
    pipe, appending `| sh` introduces no new marker and passes the superset
    check untouched. forge was the only copy that checked at all — and checked
    as four literal spellings, `["| sh", "|sh", "| bash", "|bash"]`, so
    `|  sh` (two spaces), `| /bin/sh`, `| zsh` and `| python3` walked past a
    guard defeated by the space bar. The shared rule splits the pipeline and
    compares the SET of interpreters its stages run, pinned by a test against
    jagent's own lexer so the family cannot fork it silently. It deliberately
    does not refuse every new stage name: `ls | gerp foo` → `ls | grep foo` is
    the commonest failure this surface exists for.
  - *The consent switch was not consulted.* forge ships
    `ai_share_command_context`, documents it as consent to attach command and
    terminal evidence to provider prompts, and requires it before a native
    Codex task may start — and then posted exactly that payload from this
    surface, the one with the largest payload of the lot, without asking. Of
    the four, only ember honoured it here.

  Three legitimate disagreements became construction-time policy with no
  `Default` where safety-relevant, following the `BusyChatPolicy` precedent
  from the chat-store round. `CorrectionPolicy::new` takes all three
  positionally, so there was no way to compile without answering:

  - `local_evidence_for` (`command_correction.rs:137`). Sandboxed, the process
    `PATH` describes the sandbox and says nothing about the host where Block
    commands run, so it is `LocalEvidence::Bridged` with forge's own
    `flatpak-spawn --host --watch-bus /bin/sh -c <launcher>` argv — forge is
    the only terminal that can produce host PATH evidence under Flatpak at all
    (anvil abandons both probe and walk there and so offers no PATH-verified
    correction). Native, it is `SameNamespace` + `HelperStrategy::TrustedPathScan`,
    which keeps forge's existing reach on a host whose helpers live outside
    `/usr/bin`; the scan tries the fixed system candidates first, so it is a
    superset of `FixedCandidates`, never a weakening. What changed there is the
    predicate, not the pathname list.
  - `context_sharing` (`:162`), built per request rather than at startup,
    because revoking consent must silence the provider fallback for the *next*
    failed command and not at the next restart.
  - `PROBE_THREAD_NAME` (`:71`), so a reader stuck on a descendant's pipe is
    attributable to forge in `ps`/`gdb`.

  One supporting change outside the shim, and it is the right shape rather than
  a convenience: `connect_block_finished_with_output` now carries the
  completion's `CompletionProvenance` as a sixth argument
  (`block_view/mod.rs:4554`). The core requires a `trusted_completion` answer
  and forge had no way to know it. Passing the enum rather than a pre-digested
  bool keeps the trust decision at the observer, in `completion_is_trusted`,
  where it is stated and tested; `agent_panel.rs` ignores it.

  The tests that duplicated the engine are gone with the engine — classification
  shapes, ranking, `replace_shell_word`, reply parsing, escalation/remote/chain
  rejection, output sampling, the probe bounds, the epoch machine, the timeout
  boundary. Five remain, pinning only what forge still owns: the bridge argv is
  byte-for-byte `crate::host`'s (`HOST_HELPER_LAUNCHER` is now
  `pub(crate)` for exactly this — one definition, two builders), the native
  PATH policy, the consent truth table, the provenance rule, and forge's
  `CompletionFacts` reaching the engine with each of its three suppressions
  stopping it on its own.

  Gates: forge 1,455 tests, `--locked` against the pushed core; anvil 1,453,
  ember 2,196, frost 1,006, jterm_core 714.

  **Three adversarial audits ran against the shared module before any app
  adopted it** and found eight defects in it — including that the merged pipe
  rule was still forge's four-spelling substring match, carried up verbatim by
  the merge. All eight were fixed with regression tests that fail when the fix
  is reverted. Do not skip that pass on the next extraction; the merge of four
  copies is where a weak one wins by accident.

  User-visible consequences are in `CHANGELOG.md` (Changed and Security) and in
  `README.md`; the short version is that with `ai_share_command_context` off —
  the default — no AI-suggested correction is offered any more, locally
  verified ones (APT index, executable PATH, the target's own suggestion) are
  unaffected, and a few friendly deterministic corrections (`apt install sud` →
  `apt install sudo`) are now refused because the gate that lets them through
  is the same one that read untrusted remote target output.

  Not done, deliberately:

  - `crate::host::helper_command` has one caller left, the CLI doctor
    (`cli.rs:562`). It no longer carries the weak predicate —
    `host::trusted_system_executable` now delegates to
    `jterm_core::helper::trusted_system_executable` and the local
    `writable_by_current_user` closure is deleted — so the doctor is on the
    corrected policy today. What remains is a second, thinner resolver
    (`trusted_helper_program`/`helper_command_for`) that should eventually be
    the shared one. It is user-invoked and non-automatic, which is why it is
    tolerable; the situation is recorded in `host.rs`'s module doc (`:13-32`)
    so it cannot be read as an oversight.
  - Two Flatpak bridge-launcher resolutions now coexist.
    `host::flatpak_spawn_program()` (`:80`) falls back to a `PATH` lookup for
    `flatpak-spawn`; the correction policy's `FLATPAK_SPAWN` deliberately does
    not, because a bridge that has to be *searched for* is not one this surface
    should spawn automatically. Both are correct for their callers, but they
    are not the same rule — know that before unifying them
    (`command_correction.rs:75-90`).
  - jterm_core is uncommitted and forge's pin is stale.
    `jterm_core/src/command_correction.rs` is untracked and jterm_core's
    `Cargo.toml` gained `fuzzy-matcher`; forge builds only through the
    temporary `[patch]` in `~/.cargo/config.toml`. `Cargo.toml`'s `rev` and
    `flake.nix`'s `outputHashes."jterm_core-0.2.0"` here are already written for
    `badcce2`, so `nix build` stays broken until that revision is actually
    pushed. Push core first.

  Two claims in the migration report do not survive the diff, recorded here so
  the next reader does not chase them. The weak `writable_by_current_user`
  predicate is *not* still in `host.rs`; it was deleted (see above). And
  `--safe-mode` does *not* run the correction monitor: `Config::safe_defaults()`
  sets `ai_enabled: false` and `command_correction_enabled: false`
  (`config.rs:1015-1021`), and `correction_monitor_enabled` requires both, so
  the monitor may be installed but can never start a request — and safe mode
  also forces `TerminalMode::Vte`, where there are no Block panes to attach to.
  forge already matches anvil here; nothing needs adding.

- **AI chat store upstreaming (2026-08-29)**: `src/ui/ai_chat_store.rs` is a
  75-line shim (it was 1,536) over `jterm_core::ai::chat_store`, the union of
  the four terminals' private copies of the same multi-chat state machine —
  anvil 786, forge 1,536, ember 903, frost 791, 4,016 lines over the shared
  `jterm_core::ai::ConversationSnapshot` schema and not one line of toolkit
  code, diverged in both directions so that no copy was correct on its own.
  Core's module is 1,888 lines with 47 tests; forge's 27 store tests went
  upstream with the behavior they pinned, and the shim keeps a single test for
  the single decision it still owns. `src/` is 1,175 lines lighter, the largest
  deletion of the four repos.

  What forge sent up: the library-wide 8 MiB live-history budget with real
  compaction — the per-chat 100-turn and per-reply 256 KiB caps bound a chat,
  nothing but this bounds a 50-chat library; `snapshot_for_persistence`, which
  compacts *before* serialising and then syncs the truncation markers back into
  the live chats; typed `ArchiveOutcome`/`DeleteOutcome`; and draft merges that
  report what they dropped. The two budgets stay deliberately unequal — live
  turn text caps at 8 MiB, persistence at 4 MiB under an 8 MiB encoded-JSON
  ceiling — so `ConversationSnapshot::from_chats` still compacts on the way out
  and reports it through its flag; `publish_persisted_conversation`
  (`ai_panel.rs:1350`) consumes both halves exactly as before. What forge took
  back: in-store streaming, `summaries_filtered`, the prefix rule that makes
  draft recovery idempotent, and an at-capacity archive guard forge's own copy
  did not have.

  The panel's streaming accumulator is gone with it. `StreamProgress` owned the
  partial reply *and* the 256 KiB reply cap; the store now owns both, and
  `MAX_LIVE_ASSISTANT_MESSAGE_BYTES` has no forge caller at all — the cap exists
  once. What is left is `stream_render(partial, shown, attached)`
  (`ai_panel.rs:87`), a pure decision over the store's own partial: `shown` is
  read before `push_delta`, so `Append` carries only the bytes the transcript
  tail does not hold yet, and `push_delta`'s `Some(false)` (owner chat not
  visible) or `None` (cancelled, superseded, deleted) draws nothing. The
  invariant this buys: `render_active_chat` (`:1103`) now rebuilds the in-flight
  partial from the store and re-attaches `stream_display`, so a chat switched
  away and back shows what has already arrived instead of an empty gap until the
  next fragment — which may never come, if the stream has gone quiet — and
  `complete_success` clears the partial, so the finished reply cannot be drawn
  twice. Fragments dropped by the bounded UI queue are still healed by the
  authoritative final response, unchanged.

  Only the construction-time policy stays local, because it is a panel property
  and not a store property: `BUSY_POLICY = BusyChatPolicy::Allow`
  (`ai_chat_store.rs:32`). Archiving or deleting a chat with a request in flight
  is the one place the four apps genuinely disagree; forge proceeds because
  every such path cancels first (the Delete dialog cancels before removing the
  chat, `cancel_all_requests` cancels the whole map at teardown), and the
  cancelled request's late reply is still discarded on its epoch. Anvil, ember
  and frost pass `Refuse`, which is core's default. Retry recovery split the
  same way: forge keeps calling the busy-refusing `recover_retry_payload`, and
  needs nothing from `recover_retry_payload_detaching` (anvil's shutdown path)
  because `cancel_all_requests` has already cancelled the request before it
  recovers the payload. `delete_active` now returns a `Result`; deletion cannot
  be refused under `Allow`, but the panel reports a refusal instead of silently
  dropping the user's Delete, so a future policy change surfaces (`:955`).

  Three behavior changes reached the user. Archiving the last un-archived chat
  with all 50 slots occupied is now refused with a visible "Chat limit reached"
  status (`:888`) — forge's own copy set `archived = true` and then took the
  `else if !self.at_capacity()` branch, returning `Ok` with a library in which
  every chat was archived and no writable chat existed; core checks and refuses
  before it mutates. Library previews now arrive sanitised from the store
  (`chat_preview` runs `review_input::safe_inline_display`), so the row builder
  dropped its own subtitle filter and keeps the filter only on the title, which
  a restored snapshot supplies; the search query therefore matches the sanitised
  text, i.e. what is actually on screen. Forge was not exposed to the spoofing
  hole this closed for three of the four copies — it sanitised at display — but
  the filtering it hand-rolled in the panel now lives in `summaries_filtered`
  (`:1001`) with identical trim/lowercase/substring semantics, so the four
  libraries cannot drift. The draft-merge prefix rule changes nothing in forge
  today, because the panel holds at most one recoverable payload per chat (a
  starting request removes the stored retry payload); it removes the dependency
  of correctness on that call-site discipline.

  Not done, and blocking a commit: `Cargo.toml` still pins `jterm_core`
  `1f5f0fb`, which does not contain `ai::chat_store`. This tree compiles only
  through the temporary `[patch]` in `~/.cargo/config.toml` that points
  `jterm_core` and `jagent` at the local checkouts — that patch is also why the
  only `Cargo.lock` change is the loss of both `source = "git+…"` lines. Before
  this round can be committed the pin must move to the core commit that carries
  `chat_store`, `Cargo.lock` must be regenerated without the patch, and
  `flake.nix`'s `outputHashes` entry for `jterm_core-0.2.0` updated to match.

- **AI correction guards and snapshot-line quarantine (2026-08-29)**: the
  family's adversarial audit confirmed three holes in
  `src/ui/command_correction.rs`, all of them cases where forge's list was
  shorter than its siblings'. `adds_new_control_syntax` tested one substring at
  a time over single-character markers, and `"&&"` contains `"&"`: the scan
  could not tell `a & b` from `a && b`, so `ls | grep foo || rm -rf ~/work` was
  accepted as a correction for `ls | grep foo` because `|` was already present
  and `||` was never on the list. It now compares marker *sets* —
  `syntax_markers` (`:1639`) over `["&&", "||", ";", "|", "&", ">", "<", "$(",
  "`"]` — and refuses any marker the original does not contain, while a
  candidate that reuses only the original's own operators is still allowed.
  `adds_remote_execution` (`:1661`) was missing `mosh`, which opens an
  interactive session on a host the user never typed exactly as `ssh` does.
  `classify_failure` (`:1197`) hand-rolled an emptiness/control scan instead of
  calling `jterm_core::review_input::validate`, which additionally refuses
  visual spoofing: a command carrying U+202E was classified, embedded in the
  correction prompt sent to the configured provider, and rendered in the review
  card's "original" slot. The 16 KiB `MAX_COMMAND_BYTES` bound stays on top of
  the shared gate, because a correction request is not a bulk review insertion.
  The same pass widened classification to match the siblings: exit 127 is the
  POSIX command-not-found status, so an unrecognised shell wording now falls
  back to the command's first executable (resolved before the tool-suggestion
  branches, so an explicit suggestion can name the missing executable), and
  "no such subcommand" joined both unknown-token lists. Five new tests cover
  the chained-command refusal together with the marker-set allowance that must
  survive it, `mosh`, the spoofed command (which must also stay unclassified
  for `should_request_correction` while its unspoofed shape still classifies),
  exit 127 with and without a privilege prefix, and the cargo wording.

  `src/state.rs` had a quieter version of the same shape: `parse_ai_conversation`
  returned an `Option`, so "this snapshot has no AI line" and "this snapshot's
  AI line is unusable here" were the same answer. The window has already claimed
  those bytes by renaming a `window-*.state` onto its own `.active` name, and
  the next autosave writes a payload with no `ai_conversation=` line over
  exactly that path — so a duplicated, oversized or malformed line, or a chat
  library written by a newer forge pinning a newer snapshot schema, was silently
  and permanently deleted. The parser now answers
  `AiConversationLine::{Absent, Parsed, Rejected}` (`:1149`) and
  `restore_ai_conversation` (`:1215`) quarantines the file on `Rejected`, the
  same treatment `load_tabs_state`'s unreadable-read arm already applied.
  Quarantine moves the `.active` file aside even though its tab lines were fine;
  tab recovery is unaffected because `parse_tabs_state` reads the contents
  already in memory, and the next save recreates the snapshot. Two regressions
  pin both directions — an unusable line is moved aside and the tabs still
  restore; an absent or valid line leaves the file where it is.

- **Remote Files transactional navigation and authority isolation (2026-08-29)**:
  root changes now stage a `NavigationIntent` carrying a monotonic revision,
  immutable filesystem target and cancellation token. The target listing must
  succeed and still match both the newest intent and live authority identity
  before location, execution overlay, root store, selector, snapshot and
  history commit together. Failure, cancellation, profile mutation and
  out-of-order completion leave the old root/store/selection/history usable.
  Choosing the still-committed location explicitly cancels a staged selector
  change, including the pre-list remote Home probe. That probe freezes the
  stable filesystem authority and rechecks it against live config before
  handing its path to navigation, so B's late Home cannot target C after an
  index reuse. Back/Forward retain at most 64 committed points per stack and mutate
  only after a successful listing; a newly committed branch clears Forward.
  Up, Home, Open Folder, cwd follow, profile remap and foreground-SSH follow
  enter the same transaction. The clickable root title provides a bounded,
  control/spoof-safe absolute POSIX path entry and exact ancestor breadcrumb
  targets (dialog UI, not a persistent inline entry).
  History locations are exact-profile remapped on config reload and unprovable
  entries are dropped. A committed filesystem-identity guard also makes a
  retained snapshot read-only if its numeric profile index is reinterpreted;
  scans, mutations and Open Terminal cannot send old paths to the replacement
  endpoint while fallback navigation is pending or has failed.

  Scan admission keeps the existing global eight fixed workers/64 queued jobs
  and weighted Root/Manual/Lazy service, but now keys work by stable filesystem
  authority, round-robins authorities, caps each remote at two running and 16
  pending scans, and caps Local at 48 pending. At global saturation the first
  Root/Manual request from a queue-absent authority displaces the newest,
  lowest-priority queued request from the most overrepresented other authority.
  A slow endpoint therefore cannot occupy the global pool or make another
  authority's first interactive navigation return `WouldBlock`. File
  mutations moved from one thread per request to four fixed workers plus a
  32-job hard queue. Per-authority cap/fairness and fs-op saturation have pure
  regressions.

  Each successful directory snapshot now stores wall completion time for UI
  age and monotonic completion time for a five-minute remote TTL. While Files
  is visible, a 30-second tick revalidates at most four visible materialized
  stale directories with SWR, coalescing pending revisions. Non-cancellation,
  non-backpressure failures are classified transient/persistent and receive
  per-authority/path exponential or 30-second cooldown (30-second cap);
  explicit tree-row Retry bypasses the current cooldown for exactly one
  attempt. Queue/list/reconcile time, enqueue depth and delta sizes are logged
  with slow thresholds. The child-store registry is weak so GTK-owned live
  subtrees preserve identity without the cache pinning evicted subtrees.
  Regressions cover transactional failure/stale answers/cancellation, bounded
  branching history, validator/breadcrumbs, authority isolation, cooldown and
  Retry bypass, monotonic TTL, timing thresholds and weak-cache reclamation.

- **Remote Files bounded scheduling and navigation (2026-08-29)**: the audit
  found that the former concurrency counter still created one waiting OS thread
  per scan, collapsed-but-cached directories were skipped by targeted refresh,
  raw remote diagnostics could reach UI text, retained snapshots had no age,
  and mutation/navigation selection semantics were incomplete. Directory scans
  now use eight fixed workers behind a 64-job hard queue. Weighted root/manual/
  lazy lanes prioritize navigation, guarantee lazy service, let high-priority
  requests preempt newest lazy work, and physically retire cancelled same-path
  revisions before they consume queue capacity. Pending state is completed only
  by the current revision. Pure pressure, preemption, fairness, cancellation and
  pending regressions pin these bounds.

  Stale-while-revalidate now records the last successful publication time and
  labels retained snapshots with relative age. Tree/UI errors use stable
  categories rather than raw SSH/probe stderr, with bounded control/spoof-safe
  labels. A collapsed materialized store remains refreshable; reconciliation of
  a vanished/retyped directory cancels and removes all descendant requests,
  stores, timestamps and selection intents. Ambiguous operation failures also
  re-list exact affected parents, while successful create/rename selects the new
  path after the winning reconciliation. Remote home parsing is strict UTF-8;
  the Files header adds Home, context menus add Open Folder, and ListView-scoped
  `Alt+Up`, `Alt+Home`, `Alt+Right` provide parent/home/enter navigation without
  claiming terminal or ordinary list keys. Late home answers are guarded by
  generation, location, overlay and root. Tests cover error redaction/snapshot
  age, store delta/subtree boundaries, selection restoration, shortcut
  modifiers and strict home parsing.

- **Remote Files resilient refresh state (2026-08-29)**: initial child/root
  listings now render `Loading…` inside the tree; stale-while-revalidate appends
  `Refreshing…` after the last-good children, and failure replaces only that
  transient row with bounded `Error: …` plus a focusable, accessible `Retry`
  button. Successful reconciliation removes the state row and retains surviving
  object identity/expansion. Per-path scan revisions now own cancellation tokens:
  issuing a newer revision cancels the older token, queued workers recheck after
  acquiring their scan slot and immediately before enumeration/spawn, and a
  running remote list uses the existing process-group watchdog kill path. Bare
  `F5` is captured only within the Files ListView to refresh the current root;
  modified F5 and every F5 while terminal focus is outside Files propagate.
  Regressions cover loading/refresh/error preservation, retryability, queued and
  pre-spawn cancellation, running process-group cancellation, and the strict F5
  modifier matrix.

- **Remote Files bounded listing reconciliation (2026-08-29)**: probe protocol
  v4 gives remote `list` the client-owned `MAX_DIRECTORY_ENTRIES + 1` hard
  limit and stops its loop at that boundary. The extra complete pair is a
  conservative truncation sentinel; only the first 4096 valid rows are kept,
  and root/manual refreshes disclose the bounded prefix. Remote wire names must
  now be valid UTF-8 basenames, with duplicate names/resolved paths rejected,
  so a lossy label can never become an unaddressable operation path. The probe
  classifies `-L` before `-d`, making every symlink (including links to dirs)
  non-expandable. In-place refresh now explicitly restores only surviving
  selected paths, clears any drop-hover widget before ListView recycling, and
  revalidates menu, rename/create and delete-confirmation row identities before
  delayed work starts. Parser, argv, real-script limit/symlink, selection and
  delayed-row regressions cover the protocol and reconciliation boundaries.

- **Remote Files latest-wins refresh (2026-08-29)**: every root, lazy child
  and explicit/operation-triggered directory scan now carries a per-directory
  revision in addition to the existing tree generation, location and execution
  overlay snapshot. A newer request for the same remote path rejects an older
  completion, so a delayed expansion can no longer overwrite a fresher manual
  or post-mutation listing. Context-menu Refresh now targets the clicked
  directory (or a file's parent) rather than always re-listing the tree root.
  Successful refreshes still use the minimal in-place store diff, preserving
  surviving row identity, loaded descendants and expansion; failures leave the
  visible snapshot intact. Pure regressions cover out-of-order revisions and
  child-directory action targeting.

- **Files hidden-entry policy (2026-08-29)**: a focusable, accessible eye
  toggle now hides dot-prefixed rows by default and reveals them through the
  existing `FilterListModel`. It never rescans, changes location authority, or
  discards `TreeListRow` expansion identity; the hidden policy and loaded-name
  query compose in one predicate while the selection model remains stable.

- **Foreground SSH → Remote Files 4.6 (2026-08-27)**: the existing single
  window heartbeat now observes the active leaf's real foreground process tree
  through `jterm_core::process::observed_ssh_command`, so Block, Unified and
  VTE recognize direct SSH and provenance-checked jsh upgrade launchers without
  trusting terminal text or OSC lifecycle completeness. The shared parser
  rejects remote commands and side-effecting/unreplayable options.
  Forge prefers one validated configured filesystem authority, otherwise
  embeds an immutable `FsLocation::Transient(RemoteHostConfig)` marked
  temporary in the selector. Remote home discovery stages off-thread while
  the existing tree remains intact; publication requires the same monotonic
  follow token, active pane/session/foreground target, tab-focus generation,
  file-operation intent/count, tree generation, location/root, exact config
  remap and freshly recomputed transport uniqueness. Success reveals Files without
  persisting a preference, failure offers Retry, unsupported SSH offers the
  profile selector, and SSH exit never forces an already-open tree to Local.
  Explicit and provenance-derived ControlPaths live only in immutable execution
  overlays (probe, scan, operation, clipboard and both transfer endpoints),
  while stable saved/transient identity ignores them. Equivalent saved and
  temporary endpoints paste through direct copy/rename instead of a relay.
  Transient locations use the same final Forge execution gate; their terminal
  bridge is plain interactive SSH with no assumed remote jsh command. Long
  endpoints use bounded middle elision in the selector while their sanitized
  full value remains available as a tooltip.

- **Files Remote Bridge 4.5 (2026-08-27)**: the Files header now has a
  keyboard-focusable, explicitly labelled terminal action. Local opens at the
  visible tree root; an SSH/Docker location opens the same validated managed
  profile at its remote-shell default directory. Remote home discovery moved
  from GTK to the bounded file-op worker, with generation/location rejection
  for late answers and a visible Local fallback on failure. Index-backed tree
  and clipboard locations are reconciled across Settings edits and config
  reload by exact full-profile identity: unique reorders remap; removal,
  replacement, or ambiguity fails closed to Local so old rows can never be
  applied to a different target. Dropdown rebuild notifications are suppressed;
  menu/name/delete intents revalidate their originating generation/location;
  and operation completions refresh only the generation/location they started
  from. Every Copy/Cut has a monotonic intent token: delayed Paste resolves the
  token through the live reconciled clipboard, and a slow cut can retire only
  its own intent (even when a newer intent has identical paths, or the same
  profile moved index). Managed-tab OSC 7 following first distinguishes the
  focused local/remote split leaf, then requires one validated current profile
  match instead of trusting a display name; only restored tabs carry the
  explicit saved-session identity exception through reconnect. Pure regressions
  cover reorder, removal, replacement, invalid/ambiguous profiles, Local
  identity, stale delayed actions, clipboard ABA, and managed-tab replacement.

- **Block Search 4.4 (2026-08-26)**: the capture-phase picker key router now
  confirms only when focus belongs to the query editor or a result row. Every
  other focused widget receives `Return`/`KP_Enter` normally — including
  Refresh/Reset, scope and filter controls, row bookmark stars, and
  `AdwHeaderBar`'s implicit Close button — instead of jumping and closing on
  an unrelated selected result. Query/list confirmation and Shift+Enter
  advance semantics remain unchanged; pure routing and DISPLAY-backed GTK
  focus-classification regressions cover both sides of the allowlist.

- **Block Search 4.3 (2026-08-26)**: exact VTE occurrence jumps now roll back
  transactionally when any native step fails, removing both the search regex
  and VTE's partial selection so an unavailable target cannot leave a wrong
  match highlighted. A real DISPLAY-backed VTE regression covers two
  successful steps followed by a failed third occurrence.

- **Block Search 4.2 (2026-08-26)**: Cross Block Search now has a Reset-aware
  Bookmarked metadata toggle that composes with Failed/Slow/Background before
  scope and the 500-hit cap, including empty-query browsing. A pane-local,
  runtime-only `BookmarkState` centralizes membership and a monotonic revision;
  both Block and Unified results can toggle it from an accessible row star or
  selected-hit `Ctrl+Shift+B`. The physical-keycode latch suppresses repeat and
  modifier-release leakage, while successful toggles preserve selection and
  rebuild all duplicate rows. Block reuses its card toggle so star/CSS and an
  active Bookmarked card filter stay synchronized. Unified prunes only record
  ids actually retired by `record_unified_zone`; snapshot/chrome eviction does
  not remove membership. Nothing is serialized or restored.

- **Block Search 4.1 (2026-08-26)**: the GTK metadata row now includes a
  process-local, Reset-aware Background filter. Block records use
  `BlockData::is_background()` while Unified records use their explicit
  metadata bit; the search adapter normalizes background command, exit status,
  and duration to absent, making command-lifecycle predicates mutually
  exclusive even for contradictory legacy fields. Empty-query Cmd produces no
  synthetic row; All/Out use only the first meaningful line of real retained
  output, and filter plus scope eligibility precede the result cap. The compact
  rows remain inside real automatic horizontal overflow with unchanged Tab
  order. Cross Block close paths now retain the single dialog-slot claim until
  the owning `closed` callback, which also identity-guards memory persistence;
  a fast close/release/open sequence cannot create or later lose a replacement.

- **Block Search 4.0 (2026-08-26)**: the window capture controller now routes
  the configured `block:search` action through a hardware-keycode latch shared
  with the in-dialog fallback. The opener toggles once, repeats are consumed
  through physical release even if chord modifiers are released mid-press, a
  fresh press still closes, and window deactivation clears a release that the
  compositor may have dropped. The GTK title bar now
  keeps only Refresh and Reset; matching/scope and metadata filters occupy two
  compact rows inside explicit automatic horizontal overflow, so theme/font
  growth in narrow windows remains scroll- and Tab-reachable without clipping and
  still leaves a dedicated metadata row for Background parity. Manual refresh
  sets and explicitly announces `Refreshing blocks…`, crosses one complete
  painted frame, then runs
  the synchronous selection-preserving rebuild on the next tick. A newer
  intent or click cancels the superseded frame callback, and query refocus plus
  the existing F5 modifier/release semantics remain unchanged.

- **Block Search metadata parity (2026-08-26)**: Forge now exposes the
  existing Failed and Slow record predicates in the GTK palette, persists them
  with the other process-local search intent, and clears them through Reset.
  Either filter works with an empty text query by yielding one representative
  row per eligible retained block; predicates run before the 500-hit cap, and
  filter-only activation navigates without installing an empty VTE matcher.

- **Block Search 3.9 (2026-08-26)**: the GTK search header now exposes a
  pointer-accessible refresh button with an accessible action name and `F5`
  shortcut. Clicking it, or pressing unmodified F5, synchronizes the automatic
  version probe, cancels pending debounce, and immediately performs the same
  selection-preserving rebuild before returning focus to the query. F5 with
  Ctrl/Shift/Alt/Super/Hyper/Meta continues propagating unchanged. A physical
  F5 press is latched until release, so auto-repeat cannot rebuild repeatedly
  and releasing a chord modifier while F5 remains held cannot trigger refresh;
  leaving the dialog focus domain resets the latch if GTK drops the release.

- **Block Search 3.3 (2026-08-26)**: Shift+Enter now reveals a live terminal
  hit, keeps the GTK palette open, restores query focus, and advances only
  after that successful reveal. Snapshot-only hits still open their snapshot;
  unavailable hits retain selection and diagnostics instead of fake-stepping.

- **Block Search 3.2 (2026-08-26)**: result navigation now wraps with arrows,
  jumps to either edge with Home/End, pages ten rows with PageUp/PageDown, and
  scrolls the selected GTK row into view without stealing query focus. The
  accessible status reports the current position as well as total/cap state.

- **Block Search 3.1 (2026-08-26)**: `All / Cmd / Out` scopes now restrict
  cross-block scanning before the 500-hit cap, so an excluded surface cannot
  starve the requested one. The GTK dropdown and `Ctrl+O` cycle rescan the
  retained records without changing activated VTE highlight semantics.

- **Block Search 3.0 (2026-08-26)**: cross-block search now composes `Aa`
  case sensitivity, bounded regex, and Unicode whole-word matching. The GTK
  result scan and activated VTE/PCRE2 highlight carry one typed options value,
  so a row cannot be found case-sensitively or as a whole word and then jump
  under the old case-insensitive substring semantics. `Ctrl+I`, `Ctrl+R`, and
  `Ctrl+W` keep every control keyboard reachable while the query retains focus.

- **Core-owned Agent claim durability (2026-08-25)**: Forge now pins
  `jterm_core` `21437ba` and its matching `jagent` `a462ec8`. Core durably
  syncs retirement of the public Agent snapshot name before exposing a live
  session and owns post-consumption cleanup. Forge therefore removed its
  redundant post-`Restored` directory-sync failure gate, which could otherwise
  discard a session after core had already consumed its only snapshot. The
  compatibility re-exports for the legacy read/remove/best-effort claim helpers
  remain in one narrowly scoped `#[allow(deprecated)]` use so downstream source
  paths do not break before a major release; Forge's own panel uses only the
  typed, durability-owning claim path. The
  direct and transitive `jagent` pins remain identical; encoded provider,
  streaming, text-action, and native-tool JSON now reject duplicate members.

- **Shared workspace pane identity (2026-08-25)**: restored pane `sid` values
  now use `jterm_core`'s exact 1..=128-byte ASCII `[A-Za-z0-9_-]` contract.
  Valid 128-byte identities survive intact; dotted, Unicode, control-bearing,
  and oversized snapshot values are regenerated before pane routing.

- **Search/filter correctness and core repin (2026-08-24)**: card search
  surfaces now validate their render stamp before every outcome, including an
  already-selected one-hit edge, and palette occurrence jumps fail closed when
  the exact match cannot be reached within 4096 native steps. Filtered cards
  remain zero-height through viewport, virtualization, and failure-marker
  calculations; bookmark mutation reconciles an active Bookmarked filter from
  both menu and keyboard paths. The app pins published `jterm_core` `21437ba`;
  its transitive `jagent` `a462ec8` is also the direct Forge pin, avoiding
  duplicate crate identities.

- **Exactly-once command lifecycle closure (2026-08-21)**: Block and Unified
  now share one observer-side `C -> finish` latch. An accepted `D` consumes it
  with `shell_reported` evidence; if `D` is lost, only a foreground-shell `A`
  consumes it with `boundary_inferred`/`degraded` evidence and `None` for exit
  status and duration. The inferred fan-out runs before that same `A` finalizes
  the backend record, preserving the normal `D -> A` ordering. Repeated `A`,
  background output, prompt-owned alternate screens, and RIS cannot mint a
  finish without an accepted `C`; RIS remains invalidation, not completion.
  The running-command display copy, engine-owned command identity, and Agent
  correlation remain available for prompt-trust rollback and later
  Block/Unified finalization.

- **Inherited-environment freeze (2026-08-15, fourth round)**: the local
  `src/child_env.rs` is deleted and both spawn paths run on
  `jterm_core::child_env`'s frozen launch snapshot.
  `capture_inherited_environment()` is the first statement of `app::run`
  (before `identity::init` and `cli::handle_early_args`; every `set_var` —
  the FORGE_* flags and the input-method writes — runs strictly after, and a
  capture failure is fatal per core's contract). `pty.rs` builds the
  block-mode child block with `envp_from_captured`; `terminal.rs` uses
  `vte_envv_from_captured` and ORs `VTE_SPAWN_NO_PARENT_ENVV_BITS` into the
  spawn flags so libvte cannot re-merge the live environment. A non-UTF-8
  inherited variable makes the strict conversion fail; that path rebuilds
  the envv from the frozen block with the offending entries scrubbed and
  keeps the flag, rather than falling back to the live environment. Coverage
  is scoped to terminal/PTY children: notebook cell workers and the
  flatpak-spawn bridge still start from the live environment (pre-existing).
  Adversarial review verified the capture ordering across every entry path
  and caught the scrub-fallback misdiagnosis plus two overstated comments;
  one accepted test weakness remains — the spawn tests tolerate an
  `AlreadyExists` capture race, so the test binary's frozen snapshot can
  contain another test's env mutation (no assertion depends on it).

- **Deduplication round 3 (2026-08-15)**: repinned to `04f6328` and deleted
  the last two diverged local modules. `src/snapshot_file.rs` is gone (all
  callers on `jterm_core::snapshot_file`, including the now-public
  `read_bounded_private`). `src/command_history.rs` is gone: core upstreamed
  `read_recent_with_status`/`RecentHistory`, and the module's extra
  absolute-path check was redundant with the config layer's
  `normalize_history_path`. Ctrl+Click link opening now delegates to
  `jterm_core::link::is_openable_url` — a deliberate tightening (2 KiB cap,
  userinfo and backslash refused; all three `open_uri` callers pass untrusted
  child output, so nothing legitimate breaks; a >2 KiB URL still highlights
  but is refused with a log line, the same silent-refusal UX as before). The
  doctor's Flatpak probe runs through `helper::bounded_command_output`, so
  `command_status_with_timeout` left the host shim. Adversarial review caught
  two stale comments (fixed) and noted: core's history validation used the
  narrower spoof set until forge's `review_input` was upstreamed in round 4
  (display-side `safe_inline_display` still sanitizes with the wider set), and
  core's
  `command_history::prepare_path` preflight is available but not yet adopted
  by either app.

- **Deduplication round 4 (2026-08-15)**: repinned to `592d663` and deleted
  `src/review_input.rs`, `src/pty_input.rs`, and `src/execution_journal.rs` —
  core upstreamed the widened spoof table, `safe_inline_display`/
  `safe_multiline_display`, `AdmittedInput`/`admitted_input`, and
  `output_capture_enabled`. Mechanical renames: `contains_visual_spoof` →
  `contains_visual_spoofing`, `contains_noncontrol_visual_spoof` →
  `contains_noncontrol_visual_spoofing`, `is_visual_spoof_character` →
  `is_visual_spoofing_character`. All `valid_jsh_id` call sites moved to
  `execution_journal::is_valid_jsh_session_id`; core's execution-id validator
  (which additionally allows `.`) stays private, and jsh-generated execution
  ids are dot-free, so the two OSC 133 correlation sites
  (`block_view/mod.rs`) keep the session grammar — same acceptance as before.
  Core's `InputGuard` dropped `Clone`/`Copy`; the PTY writer's rollback now
  reconstructs the guard from its one-bit frame state
  (`pty.rs::input_guard_with_frame`) instead of copying it.

- **AI module unification round (2026-08-15, second half)**: `src/ai/` is
  gone, replaced by a thin shim over `jterm_core::ai`; see "AI module: done"
  below. Adversarial review confirmed symbol coverage, the endpoint-safety
  gate ordering, snapshot decode compatibility, and the stronger credential
  staging; it found no defects beyond the stale-handoff lines this update
  fixes.

- **Architecture unification round (2026-08-15)**: the app-owned helper
  runners and the five verbatim duplicate modules (`kitty_graphics`,
  `notebook_text`, `notify`, `core_keybindings`, `atomic_file`) migrated onto
  `jterm_core`; details and the adopted tightenings are under "Remaining
  boundaries" below. A three-agent adversarial review of the migration caught
  and fixed a correction-worker hang on supervised probe-failure paths, two
  helper-construction tests that no longer exercised the function under test,
  a dead `JSH_LOOKUP_PATH` export, and a stale vendored-script leftover.

- **Agent hardening adoption round (2026-08-15/16)**: `src/agent.rs` is a
  pure re-export of `jterm_core::agent`; the panel's hand-rolled claim
  machinery (~860 lines of raw `openat`/`renameat2` audit code) is replaced
  by `try_claim_session_file` under a new `PrivateParentLock` in
  `config_store.rs`, and snapshot writes take the same lock. Behavior
  changes: `AwaitingObservation` checkpoints restore as `Ready` with an
  explicit unknown-result note instead of being silently discarded; invalid
  evidence quarantines under core's `.claimed-*` name; live model replies
  auto-reject unsafe proposals inside the session. Adversarial review then
  drove a core hardening round (pin `cf0dd2c`): the dropped forge audit
  rules (pending-must-be-final-turn, approved-requires-observation,
  turn-counter arithmetic, final-turn/state matching) are now enforced by
  core's pre-restore validation for both apps, and claimed snapshots are
  read with `read_bounded_private` (a group-readable snapshot quarantines as
  tampering). Accepted trade-off, reviewed and kept: snapshot load/save now
  serialize with config saves through the shared directory flock, so a
  contended `persist()` at window close can stall the GTK thread up to the
  2s lock timeout and then skip the write with a `log::warn!` (the old
  lock-free write could race a claim instead). Forge's own
  `task_epoch`/proposal-id plumbing already covers stale-callback rejection,
  so core's `AgentSessionEpoch` remains additive and unadopted.

- **Parser unification round (2026-08-15)**: repinned to `73c1411` and
  deleted `src/parser.rs` — core upstreamed the parser verbatim (only
  `crate::` → `jterm_core::` path prefixes and rustfmt line wraps differ),
  including `ParserEvent::AgentIntegrationReady`/`EraseScrollback`/
  `HardReset`, strict ST-only APC/DCS/PM/SOS termination with
  abort-and-reprocess, SOS/RIS handling, and `is_erase_scrollback`.
  `src/lib.rs` keeps a `pub mod parser { pub use jterm_core::parser::*; }`
  re-export (the `exit_status`/`redact` pattern), so every `crate::parser::…`
  and `forge::parser::…` path — `block_view/mod.rs`,
  `tests/regression_parser.rs` — compiles unchanged, and the exhaustive
  `ParserEvent` match in `block_view/mod.rs` already covered the three new
  events. Zero behavior change; all release checks pass.

Nine organism evolution rounds, in order:

- **Inner life** (`d6fb8b4`): habituation (the Nth clean pass of the day
  celebrates at `1/(1+prior/4)` strength) and sensitization (first crack
  after ≥5 clean runs stings more); repo familiarity gradient
  (`RepoArrival` from remembered day records — shy in unknown checkouts,
  「回来了。」in well-known ones, keyed on resolved repo identity so a
  root/cwd key flap cannot re-fire it); session-local any-command rough
  streak (≥3 consecutive non-zero exits → silent `SitNearError`);
  pane-scoped content-free accept/dismiss pulses from the correction card;
  fixed-width sticky-header micro-poses.
- **Continuous life** (`14ef832`): the prototype's `LifeState::tick`
  homeostasis ported into the native reducer, driven by a window-shared
  `OrganismActivity` aggregate and tick clock that hands each wall-clock
  slice out exactly once however many pane bodies exist; forced micro-rest
  below 0.15 energy keeps exhaustion self-limiting; the agent-lost recovery
  path returns its running-command slot.
- **Body language** (`36d0bd5`): multi-frame sprite sets (gait, tail
  flicks, sparkle blinks, dozing) with per-set stable bounding boxes so the
  fail-closed fit check never flaps; `BodyLanguage` quantizes the
  continuous state (drowsy/tense/listless) into ambient poses and wander
  tempo; `Calm` tempo reproduces the original 80-second cycle frame for
  frame.
- **Autonomous mind** (`5cde9fc`): utility-scored ambient dispositions
  (Sleep/Explore/Approach/Idle) with incumbent inertia, per-body xorshift
  jitter, and hold timers; sleeping feeds rest back into the tick — gated
  on no command running anywhere so one sleeping body cannot recharge the
  shared mind mid-build.
- **Agent awareness** (`74991f2`): `TermView::agent_command_active()`
  exposes the identity-verified agent generation as one bool;
  agent-driven commands get the crouched `WatchAgent` pose and quiet
  half-strength nods — `CelebrateBig`, speech, and the human's confidence
  stay reserved for commands the human typed; coarse `AgentPulse` phases
  feed social need/attachment (giving Approach its niche);
  correlation-loss events resolve one main-loop turn later so an
  authoritative finish always wins with attribution intact.
- **Spatial continuity** (`853b9a9`): `approach()` interpolation — the
  body walks to its next pose in whole-cell steps (quarter of remaining
  distance per frame) instead of teleporting; every hide path (typing
  retreat, alt-screen, fail-closed, resize/font reflow via a surface
  signature) forgets the standing spot so reappearance snaps — it never
  walks while invisible; the watching pose perches above the output growth
  edge, following `cursor_row` (a content-free geometry scalar) down.
- **Motion restraint** (`4339e77`): `ascii_organism_motion =
  "full"|"calm"|"static"` defaulting to the desktop animation preference;
  the frame driver moved off the GTK frame clock (a tick callback forces it
  to run at full rate forever) onto a self-rescheduling glib timeout — ≤10
  wakes/s active, a 0.9s heartbeat after a minute of window rest, full
  cadence within one beat of any input. The fired-source slot is cleared
  before every other guard: a runtime outliving its pane must never let
  `Drop` remove a dead source (glib panics).
- **Long-command companionship** (`00a099e`): a command past ten seconds
  counts accompaniment time on its card ("· 2m 30s in", ten-second steps
  so the accessible status region is not narrated every second); past a
  minute the watcher settles into a lying vigil (`WatchSettled`).
  Successful build wall times accumulate into two saturating per-repo/day
  scalars (`build_duration_sum_ms`/`build_duration_count`) over the
  established `#[serde(default)]` migration path — capped at six hours per
  sample, exactly-once per event id, deliberately outside the observation
  replay so compaction cannot touch them; after three samples a build 2×
  off its repo baseline (and >10s off) earns one quiet sentence.

The continued pass adds six more coherent layers:

- **Flaky-test memory**: replay derives same-day failure→success flips across
  retained and compacted observations. Once the day has at least three flips,
  a current one-failure recovery says only「像是偶发的。」and explicitly normalizes a
  stale cross-window `CelebrateBig` to the ordinary celebration. The new
  daily/baseline counter has strict legacy-presence migration, duplicate-field
  rejection, bounded validation, and never repairs other corrupt summaries.
- **Safer perception and space**: native reducer entry points now accept only
  `CommandKind`; command strings stop at the UI classifier. Build pace compares
  against history snapshotted before the current event and before record
  eviction. Watching/reacting bodies prefer the blank band below the output
  cursor and hide if a complete sprite cannot fit; pose dimensions join the
  surface signature so interpolation cannot cross the scrollbar gutter after
  a width change. Inline pose width is fixed, and calm/static run on the low
  frequency heartbeat even while active. Reaction holds are semantic rather
  than one fixed eight seconds (quiet pass 1.8s through repeated-error sit
  10s), while any new lifecycle event still interrupts immediately. Command
  reactions and live-only cross-pane cues each reset to their canonical first
  frame without disturbing the continuous ambient wander phase.
- **Learned circadian rhythm**: each repo/day stores eight bounded three-hour
  completion buckets with event-time local day/bucket frozen together. The
  window-shared mind infers one concentrated circular nine-hour work session
  from the previous 28 days (minimum three active days/six samples, strict
  majority) and eases waking energy toward different in-hours/off-hours
  targets. Unlearned behavior is exactly the old drift. The first human command
  in each window-local work session gets a time-appropriate quiet greeting
  (daytime「早。」, evening「来了。」), unless a more specific repo-home line
  already exists; night sessions spanning midnight greet once, Agent/outside
  commands do not consume it, and failed cross-window refreshes retry instead
  of blessing stale state.
- **Lifetime growth**: top-level `days_seen` and `lifetime_recoveries` counters
  sit outside the evicting repo/day records. A 64-day sorted ledger plus a
  compaction cursor deduplicates work days across repos and makes late closed
  history growth-neutral; recovery episodes advance from replayed flip-count
  deltas, so out-of-order insertion remains correct while its daily ordering
  is retained. Evicting a record with build history closes that date prefix;
  later events there stay valid but cannot count the same lifetime episode
  twice. Legacy v1 data rebuilds a strict lower bound only when all three
  fields are absent. The shared badge
  names juvenile (<7 days), adult, and seasoned (≥60 days and ≥12 recoveries);
  a seasoned human `CelebrateBig` keeps its behavior/tone and says only「嗯。」.
  Celebration frames also retain the cat's ears instead of switching to the
  old human-shaped figure.
- **Single focused presence**: pane reducers, inline cards, and sticky forms
  remain local, but a window-shared monotonic-token/weak-registration arbiter
  lets only the genuinely focused local Block pane own the spatial body. It
  resolves the current page with `focused_leaf()` (never fallback
  `active_leaf()`), gates on window activity, hides every old desired surface
  before refreshing a new owner, and registers Static panes so they can revoke
  a prior body without rendering one. Notebook switching synchronously revokes
  the old owner inside the pre-commit signal, then lets the tab idle resolve
  the new page; focus-widget/window signals, post-switch reconciliation, and
  the existing one-second pane poll keep close/zoom/tab
  topology self-healing without strong-reference cycles. A non-zero
  authoritative finish in a registered background pane now sends only a typed,
  content-free failure pulse to that owner: an actually visible idle body
  briefly `GlanceAside`s without changing either pane's reducer/card/sticky
  state. Busy, hidden, Static, alternate-screen, owner-local, unknown, and
  successful outcomes drop the pulse rather than queueing stale attention.
  Only an active, primary-screen Full owner keeps the 100 ms animation
  cadence; non-owner, alternate-screen, Calm, Static, and resting runtimes use
  the 900 ms heartbeat. Focus transfer safely rearms the new owner without
  ever creating a second pending GLib source.
- **Recovery vigil**: the existing day/repo `recovered_pending_push` fact now
  remains a visible intention after the celebration settles. The body guards
  a fixed-width `[ok]` pose beside the prompt (with matching inline/sticky
  forms) until a successful push closes the loop; clean builds, unrelated
  commands, failed pushes, and push dry-runs preserve it, while a new build
  failure or local-day rollover clears it. Leaving the checkout releases the
  pane-local pose without erasing memory, so same-day re-entry can restore it;
  raw cwd signals handle ordinary path exits immediately, while nested-repo
  identity is resolved at the next semantic command. The reducer mirrors
  memory's preserve-until-push semantics, then reconciles the memory layer's
  post-replay truth and broadcasts it only to same-window panes already bound
  to the exact repo/day. The low-frequency frame clock notices midnight even
  without another command. Exhaustion still overrides the intention with real
  sleep and a separate wake threshold prevents boundary flutter. Command
  classification unwraps a bounded chain of common wrappers; a push whose
  option tail was truncated fails closed instead of hiding a late dry-run.
  No schema field was added.
- **Five-round debugging vigil (current working tree)**: recovery now has its
  missing front half. One or two unresolved repo/day build failures settle to
  a fixed-width `[!]` `GuardFailure`; three or more settle to the lower,
  silent `[!!]` `GuardStuck`. These idle intentions anchor only in a complete
  blank band below the terminal output edge and fail closed to the inline card
  when the sprite cannot fit; recovery/cautious recovery stay beside the
  prompt. A third same-day failure→success flip turns pending recovery into
  `[?]` `GuardCautious` without claiming the current test is flaky. A typed
  `RepoWorkState { open_failures, recovered_pending_push,
  failure_success_flips }` is the content-free post-replay truth shared with
  exact same-repo/day panes; `MemoryInsight.open_failures` deliberately
  remains the event-position depth used by the immediate reaction when
  `event_order_exact` is true; compacted-prefix late events are explicitly
  fail-neutral. Duplicate, unknown, failed-push, late, and out-of-order paths
  all return the final work snapshot; visible replay descriptions are rebuilt
  from that event position instead of retaining stale sensitization or count
  text, and final open work vetoes stale recovery/push closure speech. The
  source pane reconciles even a
  queue-rejected preview, but only an event admitted to disk/retry ownership
  broadcasts to siblings; admission is decided under the queue lock and can no
  longer race a fast worker acknowledgement. Start and finish each freeze one
  wall-clock sample for context, reducer, day, bucket, and event construction;
  finish refreshes first so every context event is an ordered predecessor of
  that sample.
  Every pane performs its own midnight retirement even though only one pane
  advances shared physiology. A durable vigil cancels an already-running
  cross-pane `GlanceAside`; forced vigil sleep retains output clearance and an
  owner-only logical rest claim lets Static/fail-closed bodies cross the wake
  hysteresis without multiplying recovery; sync, context exit, midnight, and
  focus transfer reconcile that claim immediately. Non-Git loops are
  conservatively scoped to exact raw cwd. Identity-preserving Git global
  options such as `--no-pager` and `-c` can still reach a real push, while
  `-C`/`--git-dir` and bounded-parser truncation fail closed. Accepted human
  input now hides the live body for the
  whole 900 ms retreat window and returns by snap, so typing never triggers a
  distracting run. No schema field or content-bearing perception was added.
- **Five-part embodiment pass (current working tree)**: lifetime stage now has
  a visible phenotype as well as a badge — large eyes and quicker micro-motion
  for juvenile, the existing adult silhouette, and a notched ear with slower
  cadence for seasoned. A composable render context keeps semantic behavior,
  quantized `BodyLanguage`, `GrowthStage`, and content-free `OutputRhythm`
  independent instead of multiplying reducer states. Every phenotype variant
  in a pose family retains the exact bounding box, and semantic reaction marks
  remain canonical. Output rhythm exists only in memory: the most recent three
  pulses inside roughly 1.2 seconds read as busy, roughly three quiet seconds
  (including a command with no output yet) as waiting,
  and returning output gets a roughly 0.9-second resume acknowledgement;
  existing commands past 60 seconds still use `WatchSettled`. None of those
  states inspects bytes or infers a command result.

  Full motion connects selected semantic boundaries with four fixed-envelope
  frames: error reaction→failure vigil, celebration→recovery or
  cautious guard, settled watch→celebration, and recovery guard→post-push rest.
  Calm snaps and Static stays card-only. A newer event, typing retreat,
  alternate-screen yield, and fail-closed sizing always preempt transitions.
  A process-local stable hash of an already canonical repo identity chooses
  the preferred nest side and route; an unfamiliar repo receives one short
  post-settle exploration when no higher-priority vigil conflicts, otherwise
  that exploration is dropped rather than replayed later. It never displays the path, enters the reducer, or
  adds a persisted field. Finally, a window/session-local attention arbiter
  admits failure/vigil before closure/recovery/push, then long-command changes,
  then greetings/insights. An admitted expression owns a shared focus window
  and starts its cue-local cooldown; suppressed expression is dropped, never
  queued, while durable repo facts remain intact.

Hard lines the series preserved, verified every round: perception only
ever widens by enums, counters, or geometry scalars — command text, output,
and keystrokes never cross the reducer or persistence boundary; the disk schema
stays version 1 with `#[serde(default)]` incremental migration (old files load;
newer files fail closed in older binaries, matching the
baseline/observations precedent); the three rendering invariants (O(1)
typing retreat, immediate alt-screen yield, fail-closed sizing) are
untouched; reaction intensity is a function of history, not of the
stimulus; big celebrations and speech belong to the human's own commands.

## Remaining boundaries

### Helper-runner consolidation: done

Forge now pins `jterm_core`
`21437ba6f0cb85e74d4ce2a03ef1857de2c55d9d`. The app-owned helper
runners are migrated. `src/host.rs` is now a thin shim
over `jterm_core::host` (`pub use` + `APP_ID` + `interactive_bash_path`) whose
only local code is `pub(crate) helper_command`, kept for the two callers
still outside the core's scope (the CLI doctor's command construction and
the command-correction probes — the doctor's bounded run itself goes through
`jterm_core::helper::bounded_command_output`); `src/git_meta.rs` is a
re-export plus the forge-only
`read_cached_and_refresh` UI cache worker; `src/jsh_install.rs` is a pure
re-export; `src/ui/command_correction.rs` probes run on the now-public
`jterm_core::supervised::SupervisedChild`. Intentional tightenings adopted
from core: helper resolution requires a canonical, non-user/group-writable
target with a `PATH=/usr/bin:/bin` child clamp (note: this also rejects
group-writable root-owned dirs such as Debian's `/usr/local/bin` 2775 — a
deliberate core policy, shared with anvil), `install_argv` execs `/bin/sh`
under core's `RUN_WRAPPER`, and script staging uses core's `VendoredScript`.
On supervised probe/reap failure paths the correction worker detaches its
output reader instead of joining it, because a disarmed (unsignalled) group
can hold the pipe open forever. `scripts/install-jsh.sh` was deleted (the
vendored copy lives in core). The verbatim modules `kitty_graphics`,
`notebook_text`, `notify`, `core_keybindings`, and `atomic_file` were deleted
in favor of the core equivalents.

### AI module: done

`src/ai/mod.rs` + `src/ai/conversation.rs` (~5400 lines) are replaced by a
160-line `src/ai.rs` shim over `jterm_core::ai`: pure re-exports plus one
local `client_from_config` that gates on `config::ai_base_url_is_safe` before
constructing the client, compensating for core's `AiClient::new` not
validating the endpoint at construction (core fails closed at request-build
time instead). Core's conversation decoder is byte-compatible with forge's
on-disk snapshots (v1 legacy and v2, identical budgets), and its credential
staging (`atomic_file::write_atomic`) and supervised curl transport are
strictly stronger than the deleted nonce-temp/kill-and-reap code. Known
diagnostic-only quirk: a hand-constructed Config with an unrecognized
provider string now reports the endpoint-safety error instead of "unknown AI
provider" (unreachable through normal config load, which normalizes unknown
providers). `config::ai_api_key_file_env_override` (forge's own resolver,
with its absolute-path filter) remains untested — pre-existing gap. The host
shim's `helper_command` lost its biggest consumer (curl) but stays for the
doctor and correction probes.

### Not done this round, with the reason

- **A provably safe GC for per-session Block-history files** (TODO P2). Still
  open, and still correct not to guess: `prune_stale_session_histories` is
  `#[cfg(test)]` precisely because deleting by mtime kills the file of a pane
  that is open but quiet, and its revisioned next save then fails closed after
  the data is already gone. The shape the fix needs is a keep-set built from
  the window-state manifest (`~/.config/forge/windows/window-*.state`, plus the
  `.ready` generation) unioned with this process's live `session_ids`, matched
  against candidate filenames by mapping each known session id *forward*
  through `sanitize_session_component` — never by parsing a filename back into
  an id. It must abort entirely if any state file fails to parse or the
  directory listing is truncated, because an incomplete keep-set is
  indistinguishable from an empty one. The unresolved part is the cross-process
  race: a second forge that has already written a pane's history but not yet
  its window-state file owns a file no keep-set knows about. That wants either
  a startup ordering guarantee (the active state file created before any
  history save) or an explicitly-documented grace floor, and neither is
  something to decide in the margin of another change.

- **Moving the cross-block search scan to a cancellable worker** (TODO P2).
  `cross_block_search_in_scope` (`block_view/find.rs:1449`) regex-scans every
  retained record's command and output on the GTK thread with no time or byte
  budget; the `max_hits` cap bounds the *results*, not the scan, so a query
  that matches nothing still walks the whole retained history. The records are
  borrowed out of the pane's `RefCell`s, so a real thread would have to copy up
  to the full retained history to use them — the tractable shape is a resumable
  slice (`start_at` cursor plus a `FindScanBudget`-style deadline, which
  `find_in_blocks` already has for its own scan) driven from
  `glib::idle_add_local` and cancelled by the search generation the dialog
  already keeps. That is dialog surgery in `ui/dialogs.rs`'s 200-line rebuild
  closure, and a half-applied version that bounds the scan without driving the
  continuation would silently truncate results, which is worse than being slow.
  Left whole for the next round.

### Follow-up migrations (next rounds)

- Forge-ahead local modules to upstream into core rather than delete: none
  left. `ui::ai_chat_store` was the last one and went up this round as
  `jterm_core::ai::chat_store` (1,536 lines down to a 75-line policy shim; see
  the entry above), joining `parser` (OSC 7771, erase-scrollback/hard-reset
  barriers, strict ST termination), upstreamed and deleted locally at pin
  `73c1411`, and, earlier at pin `592d663`, `review_input` (safe-display
  helpers, wider spoof set), `pty_input` (`AdmittedInput`) and
  `execution_journal` (`output_capture_enabled`).
- Core-ahead modules: `chat_store` lands with two facilities forge
  deliberately declines — `BusyChatPolicy::Refuse`, which is core's default
  and the anvil/ember/frost archive/delete semantics, and
  `recover_retry_payload_detaching`, anvil's shutdown path — because forge's
  panel cancels the in-flight request before it mutates or recovers anything.
  Core's additive `AgentSessionEpoch` also remains unused (forge's own
  `task_epoch` plumbing covers the same ground) on the adopted hardened
  `jterm_core::agent` session wrapper, and `child_env`'s
  inherited-environment freeze stays wired (`app::run` captures first,
  `pty.rs` uses `envp_from_captured`, and the VTE spawn pairs
  `vte_envv_from_captured` with `VTE_SPAWN_NO_PARENT_ENVV`).
- Pin bookkeeping: closed. The two "done" sections above quote the pin current
  at their own round (`21437ba`); `Cargo.toml` now pins `9f94f77` with
  `jagent` at `bdc8023`, `Cargo.lock` was regenerated by building with no
  `path`/`[patch]` residue, and both `flake.nix` `outputHashes` and both
  `deny.toml` `allow-git` revs moved with them.
- `src/pty.rs:1967` and `src/state.rs:2118` spawn `sh` directly, outside the
  helper-runner contract — both inside `#[cfg(test)]` helpers, and the line
  references this bullet used to carry (`1579`/`1953`) had drifted;
  `src/notebook.rs` long-lived terminal children keep their own wait logic by
  design (`SupervisedChild` scopes itself to short-lived helpers).

## Release checks

```text
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features --no-fail-fast
cargo test --locked --doc
cargo clippy --locked --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features
```
