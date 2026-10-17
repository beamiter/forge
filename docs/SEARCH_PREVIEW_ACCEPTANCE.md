# Retained-text exact search preview

## Scope

The Find bar's **Preview** opens an independent read-only dialog for the current
pane's retained completed-record text. The capture is a borrowed backend record
walk followed by bounded display copies. No live VTE extraction or raw PTY replay
is performed. Existing native Find is unchanged; this feature does not prove or
repair native VTE occurrence, selection, live starting row, scrollback or reflow
semantics.

The same immutable display strings are searched and painted. Every hit identifies
its snapshot, document/record field and UTF-8 range; its GTK offsets count Unicode
scalar values rather than bytes, terminal cells or graphemes. The active hit uses
a TextTag. A private TextMark scrolls after GTK validates the updated layout;
no selection/insertion mark is used for navigation. No VTE cursor, selection or
clipboard API participates. GTK documents why `scroll_to_iter` can be premature
immediately after buffer changes:
[TextView scrolling](https://docs.gtk.org/gtk4/method.TextView.scroll_to_iter.html).

A snapshot covers only retained sources. Missing Unified prompts or evicted output
are counted as unavailable, never fabricated as empty searchable sources. Partial
command/output text is marked; retained output may be a tail or a prefix. The
reported exact count is within this declared scope, not a complete command log.
Controls/bidi formatting characters are escaped before matching, with no Unicode
normalization. Searches are line-local (LF), field-local, non-overlapping and
case-insensitive by default. The Regex checkbox selects Rust regex syntax; inline
flags retain their Rust meaning. Potentially empty matches are rejected, including
optional/empty repetitions and standalone assertions. Consuming anchors work.
Lookaround/backreferences are unsupported. Visual TextView wrapping is irrelevant
to the stored ranges.

## Resource and failure contract

- Query: 8 KiB before the worker-owned String copy. GTK reads use bounded
  `chars()` prefixes, not unbounded `text()`: up to 8193 scalars for the preview
  entry, or 8195 for a Find seed with two slash delimiters and an oversize
  sentinel. These transient UTF-8 copies are therefore at most 32780 bytes.
  The GtkText delegate, when present, also caps the entry at 8193 scalars so
  oversized pastes cannot silently become valid shorter queries.
- Snapshot: independently capped at 1 MiB aggregate source bytes and 1 MiB
  aggregate escaped display bytes; at most 4096 records and 4096 fields.
  Source length is checked before conversion/copy; each escaped character is
  charged before append. Failed construction poisons the snapshot, and capture
  never publishes it. No unbounded ANSI conversion is used.
- Scan: at most 64 KiB per logical line and 10000 non-overlapping hits.
  A line or match overflow discards the entire request, including earlier hits.
  An exactly-full hit budget can still be complete because the remaining bounded
  text is examined for an additional hit. Compiled regex and DFA cache limits
  are each 256 KiB; query length also bounds regex-syntax parsing input.
- No full byte-to-character map is allocated. Each fixed-size Hit holds its
  two byte endpoints and two scalar endpoints; offsets are computed in one
  monotonic pass. Documents and hit vectors have independently bounded counts.
- These are logical payload/metadata limits, **not a 1 MiB process RSS limit**.
  Existing source records, String/Vec capacity, regex parsing/compilation,
  allocator overhead and GTK TextBuffer/layout storage have separate costs.
  The preview reuses one TextBuffer containing at most one bounded document.
- The 100 ms target and cancellation are cooperative checks before/after
  compile/scan operations and between hits/lines. An individual regex operation
  or compilation cannot be preempted. Closing immediately revokes publication;
  it does not promise immediate thread termination or memory reclamation.
  A long line is rejected before matching if it exceeds 64 KiB. The workload
  remains input/metadata bounded even when a check observes a late deadline.

Each dialog owns one worker, one latest-request slot and one latest-reply slot.
Query edits replace pending work and revoke previous result generations before
publishing anything. Snapshot ids and query generations never wrap. The owner is
a weak reference to the captured pane, whose widget must remain rooted. Closed,
detached or destroyed panes cannot be replaced by whichever pane is now focused.
Dialogs keep frozen content when the source changes; close/reopen explicitly
creates a new content revision. No stale range is reinterpreted against new text.

## Verification levels

`block_view::search_preview::tests` covers same-line A/B/C forward/reverse/wrap,
Unicode byte/scalar offsets, combining marks without normalization, field/LF
boundaries, regex restrictions, source/display/line/match/query budgets, compile
size failure, cancellation, poisoned captures, generation exhaustion and one-worker
latest-query/stop/drop behavior. These are display-free model tests.

`python3 scripts/check-search-preview-shape.py` checks the real button/dialog
wiring, record capture, bounded entry reads, stale-owner/reply gate and absence
of terminal search/selection/clipboard calls. This is a source-shape guard, not
an executed UI interaction test.

The ignored hosted-only test
`ui::search_preview::tests::exact_preview_tags_follow_unicode_ranges_without_native_selection`
uses real GtkTextBuffer/TextView and the production `paint_hit` helper. It checks
that each Unicode-aware range paints only the requested hit in both directions,
without creating a TextBuffer selection, with the private scroll mark at the
current scalar offset, across requested window sizes. It also checks one-mark
reuse across document replacement, deletion on clear, and fresh mark creation
for a later result. It does
not prove complete dialog typing, pane-switch/close interactions, physical keys,
PRIMARY ownership observation or native VTE selection. The owner/generation
behavior currently has model plus source-wiring coverage.

The existing hosted GTK job explicitly runs both the model group and the exact
ignored tag test; missing test names fail before execution. Display tests must
not be run in a restricted/headless executor. Native build, actual GTK execution
and complete application interaction remain unverified until their corresponding
hosted checks run. Existing lifecycle and Chat failure gates are unchanged.


## Production dialog lifecycle fixture (hosted only)

`ui::search_preview::dialog_tests::production_dialog_is_frozen_stale_safe_and_primary_neutral`
uses the same `present_exact_preview` builder called by the application entry,
real nonexecuting TermView PTYs and completed records, the real worker, actual GTK
changed/toggled/clicked/closed signals, and the production 25 ms reply poll.
It checks repeat Next/Previous wrapping, rejected regex, old-query publication,
hiding the source while another pane is visible, removing it, frozen-versus-new
capture, normal/force close and parent destruction. Test-only rendezvous blocks
one real worker after its generation check but before publication, then waits
after storing the stale reply until the actual UI gate has rejected it; both waits
are bounded by five-second timeouts and does not inject fabricated matches. Retirement checks
worker exit and the absence of further timer polls.

Retained-tail metadata is explicitly synthetic fixture metadata on a real
completed record, not a test of terminal truncation mechanics. PRIMARY checks
an owned synthetic GDK content provider and zero changed signals on an isolated
Xvfb display. It never reads a user's clipboard and is not an external-process
selection-owner interoperability test. GTK signals are driven programmatically;
physical keyboard, full UiState tab actions and complete application interaction
remain outside this fixture. The application entry wiring remains source-guarded.

libadwaita emits `closed` at sheet closing start, so the existing handler retires
the worker synchronously; `close-attempt` is not a universal closing signal (it
is for denied close attempts when can-close is false). No speculative replacement
of that lifecycle was made. This remains retained completed-text preview coverage,
not native VTE selection or completion of original F01.

A source must still belong to a realized root window. GTK can keep a root object
after parent destruction; merely having a root is insufficient. Root unrealize
conservatively retires the preview, even if temporary; reopen it afterward.
Hiding a pane within a realized window does not retire its frozen snapshot.
The hosted fixture also hides/unmaps and shows the root without unrealizing it;
actual window-manager minimization is not claimed by the isolated Xvfb test.
