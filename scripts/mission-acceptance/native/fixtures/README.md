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

## Public Mission panel replay

`frost-mission-panel.png` is the exact public panel crop at `(680, 90)` with size
`520 x 750` from `native-reader/frost/018-refresh.png` in run
[42245525353, attempt 1](https://github.com/beamiter/forge/actions/runs/42245525353).
Artifact `13517852843` has ZIP SHA256
`a4809cf82770410ce978484622b4631dd860be4ef2e7442e59b3af58733e79d2`.
The crop excludes terminal, hostname and sidebar pixels. Its displayed Mission
is the isolated public acceptance fixture, not a user's task.

`frost-mission-controls.tsv` and `frost-mission-fields.tsv` retain 81 and 69
verbatim OCR word rows respectively, wholly inside this crop, with original
full-window coordinates. Only line endings are normalized to LF; no recognized
string, numeric field or confidence was corrected.
Tests can add a black surround and virtual page offset as scaffolding; that is
not a screenshot of another native run. Replay verifies only strict recognition
and measured input placement. The original native experiment remains failed.
