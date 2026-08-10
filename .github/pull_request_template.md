## Summary

<!-- Explain the user-visible or architectural change. -->

## Validation

- [ ] `make verify`
- [ ] `make security` (or explain why an environment-dependent check was not run)
- [ ] Relevant Wayland/X11 and VTE/Block smoke checks completed

## Safety and compatibility

- [ ] No credentials, private hosts, personal paths, or captured terminal output were committed
- [ ] PTY/process cleanup and persisted-state behavior were considered
- [ ] Configuration changes preserve existing defaults or document migration
- [ ] User-facing behavior and `CHANGELOG.md` were updated where appropriate
