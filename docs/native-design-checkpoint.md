# Recoverable native design checkpoint

This master checkpoint preserves the current native GUI reconstruction. It is not final 1:1 reference acceptance or a claim that all tests pass.

The UI source checkpoint is `34fe4eed9e099eff6402459164a470bd98323e9d`. The bundled core is published as `c0fcbd2e32a727c053c1e6e26261b81b5b89d66a` (same complete tree as the recorded local source `84c96ddda17f34091957aff19ea2004e85a418a2`); its source archive includes the required identity assets and has been checked against the committed core bytes. Documentation-only commits do not change that compiled UI source.

The queue-spacing release and actual Block/Chat native allocations were checked. Eleven strict native gates belong to the earlier c2a6b08/core d992 checkpoint; they are not relabeled as a new full-suite run. The Wayland Chat cold first-key focus failure also reproduces on the previous master.

Structured Assistant/task/diff components use an explicitly labelled visual fixture until their production event binding is complete. Unavailable Attach/Image/Code/Apply/model actions remain visibly disabled. Native PTY and retained canvas/input ownership are preserved.

Original-reference typography, surfaces and cross-backend pixel acceptance remain open. Software headless checks do not certify physical desktop IME integration or hardware frame pacing.
