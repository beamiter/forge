# Logical Find cursor properties (F01 component)

Run `python3 scripts/test-find-cursor-properties.py` with an existing Rust
compiler. It compiles only the current cursor types, four production functions
and the original render-stamp type alias. No Cargo dependency, display, VTE,
PTY, clipboard or service is used. The same new property module is included
by normal Cargo tests through `src/block_view/find.rs`.

## Additional coverage

The test enumerates all 340 domains with one to four surfaces and one to four
occurrences per surface. Every legal cursor is checked against a separately
expanded list of `(record ID, command/output surface, occurrence)` identities.
Both directions and exact/capped sets yield 12,520 navigation comparisons.
The return flag is checked at each surface edge, including a one-surface wrap
where the surface index is unchanged. Capped endpoints stay on their known
edge instead of wrapping into unknown occurrences.

Each domain is checked under every cyclic rotation of its surfaces. Resumption must recover
the same record/surface/occurrence at its new index and global position. Making
that target live, removing it, or choosing an occurrence past its new count
must fail. A different command/output field or the old vector index cannot
substitute for the missing identity.

For one to eight hits, every native logical cursor position, target position
(including one-past-end), direction, completeness state and command/output surface kind is checked against
missing or changed render stamps: 11,360 stale-stamp rejection assertions. Of
these, 3,020 have an underlying permitted action and independently exercise
the guard; the remaining inputs already lack a permitted action. Each of the four
stamp fields is changed independently. The reserved active-live identity
`is_live && block_id == 0` remains the only bypass for the finished-card stamp;
both a finished surface with ID zero and a live surface with a nonzero ID are
checked as non-bypasses. This tests the render-provenance guard, with the
underlying action used only to verify pass-through when provenance is valid.
It is not an independent oracle for native action semantics. The caller's
separate live-content invalidation is outside this pure matrix.

## Boundaries

These tests strengthen the existing logical cursor model. They do not prove
that the model's occurrence count matches native VTE selection. The existing
native characterization already records different same-line/soft-wrap cursor
behavior in forward and backward directions. That original F01 gap remains.
The retained-text preview's immutable snapshot/generation/owner tests remain a
separate implemented component; this corpus does not relabel them as new work.

All generated inputs satisfy the production admission invariant of positive,
bounded counts and internally consistent cursors/totals. Arbitrarily corrupted
private structures and integer-sized synthetic counts are not supported input
claims. Real render replay, native selection ownership, live content changes,
signal lifetime, UI wiring and GTK behavior require their existing native
fixtures. No production behavior is changed by this test-only patch.
