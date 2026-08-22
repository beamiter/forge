# Forge upgrade rounds

This ledger records the independently testable increments in the current
upgrade pass. It is intentionally tied to behavior and regression evidence,
not commits.

Rounds 1–10 record the preceding pass; this pass's additional twenty rounds
are numbered 11–30.

1. **Install/uninstall symmetry** — no-argument uninstall now targets the same
   historical `~/.cargo/bin` location as no-argument install; explicit prefixes
   still select `PREFIX/bin`.
2. **Runtime path boundary** — prefix and binary paths reject empty values,
   controls, relative paths, and lexical parent components without rejecting
   spaces, Unicode, or harmless `.` components.
3. **Staging/XDG preflight** — `DESTDIR` and active XDG config paths are checked
   before building or writing; purge validates both recursive-removal roots
   before deleting installed files.
4. **Root staging semantics** — an explicitly supplied `DESTDIR=/` remains a
   staged install, so cache refresh and host-PATH advice are not run by mistake.
5. **Unambiguous options** — explicit empty `--prefix`, `--bin-dir`, and
   `--binary` arguments fail instead of silently selecting a default.
6. **Build-free packaging** — `--binary PATH` installs a release/CI artifact
   through the same asset, desktop, config, and DESTDIR pipeline without Cargo
   or Nix.
7. **Pinned prebuilt source** — symlink input is rejected and the opened
   descriptor's device/inode is checked before copying through `/proc/self/fd`.
8. **Atomic executable replacement** — the binary is staged beside its target,
   mode `0755` is applied before same-filesystem rename; before that commit
   point EXIT cleanup removes the temp and leaves the prior executable intact.
9. **Portable desktop command** — `Exec` and `TryExec` now use their distinct
   escaping layers, preserve action arguments, reject ambiguous `%`/`=` paths,
   and atomically replace the desktop file.
10. **Configuration preservation and contract tests** — dangling config
    symlinks count as existing user configuration; the path suite covers real
    prebuilt staging, modes, hostile destination/source symlinks, escaping,
    root staging, cleanup, and uninstall symmetry.
11. **Repository-source preflight** — every support, shell, workflow, notebook,
    desktop, metadata, icon, and optional config source is checked before a
    build or destination mutation.
12. **Unambiguous artifact mode** — an explicit build backend can no longer be
    combined with `--binary`, avoiding an option that appears honored but is
    silently irrelevant.
13. **Non-empty artifact promise** — the pinned descriptor must contain at
    least one byte, and the contract proves rejection leaves the old target.
14. **Atomic support tool** — `forge-support-bundle` now uses a mode-0755
    same-directory temp and rename commit.
15. **Atomic shell integrations** — each documented shell file is committed
    independently with explicit public mode instead of copied in place.
16. **Frozen workflow manifest** — exactly the six shipped workflows are
    preflighted and atomically installed, with no source-tree glob drift.
17. **Atomic notebook asset** — the welcome notebook follows the same
    temp/rename boundary and cannot be partially replaced.
18. **Desktop structure contract** — at least one canonical `Exec`, exactly one
    canonical `TryExec`, and no alternate command line are required before the
    generated entry is renamed into place.
19. **Atomic public metadata/icons** — AppStream, SVG, and both PNG sizes retain
    mode 0644 and replace hostile destination symlinks without following them.
20. **True config no-clobber** — first-run config is staged beside the target
    and published by atomic hard-link create-new, not check-then-copy.
21. **Concurrent config winner preservation** — an injected `ln` race proves a
    competing creator wins, is retained verbatim, and leaves no private temp.
22. **Packaging ancestor boundary** — a non-root DESTDIR is lexically normalized
    before install/uninstall inspect its full existing component chain; disguised
    root links and recursive purge roots fail before ordinary files, while host
    prefixes remain compatible. This is not a concurrent-mutation guarantee.
23. **Unset-PATH safety** — post-install advice treats a missing PATH as empty
    rather than aborting under `set -u`.
24. **Single remote execution gate** — host target, user, shell, session,
    artifact, visual formatting, per-field bytes, and total argv bytes are
    checked together.
25. **Structured SSH option semantics** — `ssh_args` accepts OpenSSH option
    operands but rejects a second destination or premature `--`.
26. **Final argv revalidation** — fresh connections, restore, and reconnect use
    the checked builder immediately before a terminal spawn.
27. **128-profile activation gate** — indexed actions and name-based workspace
    restore fail closed at and above 128, even if a runtime-mutated vector is
    longer.
28. **Remote-filesystem spawn gate** — list/stat/cat/put/tar/untar all build
    through a checked probe argv; invalid runtime objects never reach spawn.
29. **Bounded safe remote UI** — picker and context rows use safe inline labels
    and cap executable rows without mutating configuration.
30. **Consumer regression evidence** — config and remote-fs tests exercise
    spoofing, semantic argv confusion, high indexes, and pre-spawn rejection.

Verification: `bash scripts/test-install-paths.sh`, `bash -n
scripts/{install,uninstall,test-install-paths}.sh`, and the repository-wide Rust
quality gates listed in `README.md`.
