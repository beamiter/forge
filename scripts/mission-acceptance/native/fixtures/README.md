# Frost sidebar OCR replay

`frost-sidebar-open.tsv` contains only the original level-5 word rows for the
public Agent Workspace heading and its decorative collapse icon (block 10), All / Running / Needs-you
filter strip and Missions entry (blocks 23–26) of the real
`native-reader/frost/010-open.tsv` failure artifact captured
on 2026-11-06. It retains the original two-times-scale pixel boxes, confidence,
line identity and text; no words or coordinates were synthesized. It excludes
the terminal, host names, other captured content and the full artifact.

The original screenshot was visually inspected: it paints
`Missions · 0 need review`, but OCR reads the small middle dot as `-`, with
89.711044 confidence. The five-word observed box after the original scale-2
conversion is `[71, 274, 158, 11]`. The owned window was `[200, 175, 1200, 850]`,
so the corresponding screen box is `[271, 449, 158, 11]`.

Original artifact SHA-256 digests:

- `010-open.png`: `566392aa715ab774cb5b3334357ef0eea7821de7c71f883344913d215f02915c`
- `010-open.tsv`: `e4aafa80a19a68f512bbd499cb8aca498d5c22bcc57e366eb6e5146e77c447b8`
- `010-open.json`: `f7213beb59c9ba03dbdbc52feae187a5d3b56c6cc2a8841ffd88edc2152c3ee2`

Replay and mutated negative cases are pure observer tests. They do not prove
that a new native GUI run, Mission read, or end-to-end acceptance passed.
