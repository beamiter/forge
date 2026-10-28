# Native Block v2 checkpoint

This local checkpoint reconstructs the new Block reference's center using the
existing native card backend. It is a buildable intermediate result, not final
pixel or interaction acceptance. Chat and the shared shell geometry remain
separate. Explicit legacy Unified remains continuous; an existing pane is not
hot-converted and its PTY is not restarted.

## Exact artifacts

- App source: `a7caab2e2c13fecdc56c0a8479446c08006f2f3f`.
- Shared core: `84c96ddda17f34091957aff19ea2004e85a418a2`.
- Release SHA256: `476b07d44b6bfad69d2c155b8b578c02a8585cd4a124538c54cd1e42e969b72a`.
- Check passed in 5.48s; release build passed in 1m43s. Both used the existing
  isolated GTK SDK, existing Forge target, offline Cargo and one build job.
- Lib-test source is the same app source plus only `#[cfg(test)]` loading of
  the production workspace CSS in `developer_dock_tests`. That test-only
  change is included in this checkpoint; app code is unchanged.
- Frozen lib-test SHA256:
  `294098c5cc662e3562bfa456bec133d5663ca8b46fca9595c2f5bbb8c4b2b383`.
  Cargo's exact lib artifact was frozen with patch SHA256
  `06a54be979c8efd95ca1614fdd48b94b857fc484e03f28475446d087e56f72a7`.
  The lib-test build passed in 3m40s. The earlier accidentally selected bin
  harness contained zero tests and is retained as a failed runner setup.

All evidence paths below are relative to the task workspace, outside source.
No older screenshots, binaries or Library records were overwritten.

## Executed verification

`evidence/forge/developer-block-v2-a7caab2-tests/native-lib/` records five
individually executed native tests, all passing:

1. New Block: one actual VTE and document anchor, four-line input, actual slave
   TIOCGWINSZ across prompt/running seats, height-only resize and return.
2. Real-pane visible, parked and filtered fold geometry, preserving reading
   intent and the original strict height assertions.
3. Existing Chat Enter/completion transitions and narrow/font-scaled limits.
4. Native IMContext commit/cancel with independent PTYs.
5. IMContext composition retains Send authority.

The last two use native IMContext injection. They do not establish a complete
physical ibus/fcitx engine round trip. Two gutter normalization/wide-wrap unit
tests and the public Block/Chat versus explicit Unified compatibility test also
passed on the exact lib artifact. No full-suite or historical baseline claims
are made from this targeted set.

`evidence/forge/developer-block-v2-a7caab2-native/` contains real 1672×941 and
1356×704 Block PNGs and `manifest.json`: actual successful ls, failed cat exit1
and a running foreground function. Main is the actual 14pt setting, UI14pt,
DPR1. At 1672 the same shell PID3133319/TTY has 27×89 physical rows/columns
through prompt, command start and output. After a real resize to 1356, both
running and the next Ready prompt have 18×60. The command finishes with a real
exit0 record; it is not a printed status fixture.

`evidence/forge/developer-block-v2-a7caab2-actions/` shows a real More menu,
Copy Output and bound F10 collapse. Reopening More exposes Show Output, but
the final pointer click timed out in that driver and does not prove restoration.
Widget-level native fold restoration passed separately. Clipboard
contains exactly the six real filenames, without ANSI or gutter numbers. These
checks establish action operation, not final menu styling or scrolled-anchor
acceptance for that menu.

## Unfinished acceptance

- Running output still includes native prompt/echo and has no separate live
  gutter. There is no unverified row crop, replacement output VTE or second PTY.
- A real resize currently latches a short running card to full visible height
  when libvte renumbers absolute ring rows; the child grid is correct. The
  projection/high-water coordinate reconciliation still needs correction.
- Block currently reuses the shared two-row composer floor. Default Main14pt
  makes its empty prompt taller than the 68px reference minimum. A Block-only
  one-row visible floor is pending; Chat's existing floor must remain unchanged.
- More/selection controls retain older visual density. Long-output native
  gutter inner-scroll and additional selection/focus checks are pending.
- Live conversion of an existing pane between Block and Chat is not provided;
  the toolbar preference applies to newly created/restored panes as documented.
- Final 1:1 visual acceptance is outstanding.

The multiline Ctrl+C readiness gate is separately unresolved. Private paired
traces for frozen stage1 Block98, docked Block6f and explicit Unified98 all show
PTY byte03 delivered, Bash SIGINT, a fresh empty prompt, native focus and stable
physical grid, yet HasInput remains (`AwaitingCommand`, dirty/synced true,
empty shadow). All three emit A then B after interrupt. This establishes the
failure before the new dock and in both backends at the same source; it does
not establish the released master baseline. Evidence is in
`evidence/forge/developer-block-v2-cancel-pair/`, including lifecycle-only
summaries with authentication material omitted. The failure is retained;
submission guards are not cleared or treated as Ready to make tests pass.
