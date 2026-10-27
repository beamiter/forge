# Complete isolated GTK regression results

`scripts/test-gtk-display.sh` checks required tools and verifies that every
explicitly listed test exists before executing any display-backed case. These
setup checks remain fail-fast.

Each registered case retains its fresh process, Xvfb display, session bus,
accessibility/IME environment, and exact Cargo test filter. A failed case is
recorded and later cases still execute. This prevents an unrelated early failure
from hiding later Chat layout and input regressions. The final report includes
passing/total counts and every failed case's original exit status. The runner
exits with the **first original nonzero status**: later successes or optional
crash diagnostics cannot turn the job green. Cases are not retried by this change.

The existing bounded optional F08 diagnostic replay is unchanged. Existing
case/runtime limits and shell signal handling are unchanged; this does not add
an unlimited retry or alter CI timeouts. A cancelled overall job may still lack
results for later cases and must not be reported as a complete run.

Run `python3 scripts/test-gtk-display-contract.py` to exercise the actual runner
with fake tools on a closed PATH. It checks all-success, early/multiple failures,
child timeout/signal statuses, diagnostic failure preservation, preflight
failures, environment isolation and foreground-group cancellation. This test
uses no GTK, D-Bus, Xvfb, GDB, or real Cargo. Its pass validates shell control
flow only, **not native GUI behavior**.
