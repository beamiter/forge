# Native Chat terminal mode

`forge --mode chat --no-restore` opens the opt-in conversation layout. The
persistent setting is `terminal_mode = "chat"` (or `FORGE_MODE=chat`). Preferences
→ Terminal Backend → Chat applies to new/restored local panes. Existing panes
keep their selected renderer and live PTY. Existing defaults, Block, VTE,
Unified and managed-remote Block behavior are unchanged.

This is a terminal presentation, with no LLM dependency. Completed shell
commands appear in right-aligned blue capsules; their real ANSI-preserving
output appears in wide left-aligned rounded cards with the original exit,
elapsed-time, copy, review, collapse and search actions. Directory, time and
duration stay in separate compact pills on the left; the collapse control stays
at the far right. Short command capsules fit their content and remain right
aligned through window and font changes; wrapped commands grow vertically.
Log output retains the pane's reading width.

## Input ownership

The docked multiline composer is the same live VTE and long-lived PTY used by
Block mode. The shell continues to own editing, history, completion, multiline
continuations, IME, password entry and stdin. There is no second textbox, command
replay, one-shot exec or additional input journal. The arrow invokes the existing
native Enter accounting after exactly one immediate kernel write. Two accepted
clicks mean two Enter presses, never another copy of the command. Busy writers,
active paste/Alt frames, unknown ownership and enhanced key protocols reject the
optional button without queueing or retrying; use the native keyboard instead.
Shell-specific continuation/paste behavior stays native.

When a command runs, the same terminal moves to the active result region and
continues accepting stdin, including hidden input and REPL input. The composer
is not editable while that process owns the terminal. A running status and Stop
control remain at the bottom. Stop binds to the displayed accepted-command
identity, including pointer/key presses that span a lifecycle change. Replaying
held completion/start events cannot redirect an old Stop to the next command. Native delayed/accessibility activation freezes the same origin before its animation; all five native activation keys share this protection. Send also retains its originating prompt generation, so a pending animation cannot submit a replacement prompt.
Both Chat Stop controls retain that command generation and foreground process
group until a guarded, nonblocking, single-byte Ctrl-C write succeeds. Pending
or in-flight native input causes a refusal, never a delayed interrupt of a newer
job. Only Chat enables nonblocking PTY I/O; the ordinary keyboard FIFO stays in
order and Block/Unified keep their original fd mode. Completion and
interrupted/failed/unknown outcomes use the original lifecycle metadata.

An alternate-screen application receives the original full native terminal.
Chat input/status chrome yields to it. Exit restores the running result or
composer according to actual shell lifecycle. Missing/unreliable integration
uses the same terminal in passthrough until a real integrated prompt returns.
No fallback replaces the PTY or changes the saved mode.

The ASCII companion, when enabled, stays attached to the same live terminal
clip and inhabits the composer. Its original visibility, content-free activity
signals and alternate-screen rules remain in effect.

## Required automated coverage

- Configuration/CLI parse and persistence round trip for `chat`; defaults and
  existing values unchanged; no `chatbot`, `capsule` or `ai` aliases.
- Content-free state adapter: startup, ready prompt, running, completion gap,
  raw fallback, alternate screen and recovery.
- Real nonexecuting PTY write spy: one Send means one CR; continuation with
  pending input can Send again; running/alternate/raw states do not submit.
  Closed writers, a deterministic full queue, and a separately admitted held
  Alt escape must not be reported as accepted Enter input.
- Presented production Chat controls: held D/A/B/C replay, identical successive
  commands, pointer/key press across a new command, and rejected Send admission.
  A stale Stop must not interrupt the next command or emit accepted-input feedback.
- GTK display gate: only the live clip reparents, anchor and VTE identity remain
  stable through repeated transitions, and a sibling Block pane is unaffected.
  Height-only window resizes at the prompt, in the alternate screen and in raw
  passthrough must update the actual PTY slave winsize without output or a
  forced geometry synchronization. A tall multiline composer must allow actual
  window shrink without clearing input, and a simultaneously mapped Block pane
  must retain focus across background Chat completion.
- Scoped CSS, Unicode command sizing and wide output invariants.
- Existing Block input, PTY, copy/search, lifecycle and history regression suite.

Run `cargo test --locked --all-targets`, strict Clippy and `make test-display`.
A successful headless test run does not validate the GTK display checks, which
are explicitly ignored outside the dedicated Xvfb gate.

## Native acceptance checklist

Run on X11 and Wayland where supported. These are acceptance steps, not a claim
that they have already been run on every platform.

1. Open Chat and Block panes together; switch preferences and create another
   pane. Confirm existing panes and running commands are not recreated.
2. Run `printf 'alpha\n'`, a failing command, a no-output command, `seq 1 300`,
   a wide log, CJK text and a multiline command. Check exact copy/search, ANSI
   colors, scroll positions, right command capsules and full-width results.
3. At a shell continuation (PS2), submit successive lines using keyboard and
   the arrow. Try rapid repeated activation. Each activation contributes one
   Enter and no command text is copied/replayed. Rejected Closed/full-queue writes
   must show refusal feedback without accepted-input or submission state changes.
4. During `sleep 5` use Stop, and during `read -r value` enter stdin. Test a
   password prompt; it must remain hidden and must not create a password bubble.
5. Run vim/top, exit and repeat. Check input, mouse, selection and grid size,
   with no stale capsule or composer covering the application.
6. Use a shell without integration and later enable it. Native passthrough
   stays usable; a valid prompt restores Chat on the same process.
7. Scroll old results while the composer stays docked; copy/select/search,
   collapse/expand, restore history, undo clear and theme/font changes. Check
   360-pixel splits and large fonts for clipped controls or phantom gaps.
8. Focus a different pane, let this pane complete, and return. Reparenting must
   not steal background focus. ASCII remains inside the actual clipped input
   area, and paint/selection/pointer positions share GTK geometry.
