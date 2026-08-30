# forge relocatable Linux bundle

This archive contains a prebuilt `forge` binary plus its desktop metadata,
shell integrations, example workflows, documented configuration, and welcome
notebook. It installs into the current user's `~/.local` prefix; root access is
not required, and an existing `config.toml` is never overwritten.

## License

forge is dual-licensed under `MIT OR Apache-2.0`. The archive includes both
canonical license texts under `share/doc/forge/`.

## Runtime requirements

This is not a statically linked or self-contained portable application. A
compatible graphical Linux system with GTK 4, libadwaita, GTK4 VTE, and PCRE2
runtime libraries is required. Optional integrations include `notify-send`,
OpenSSH, Git, and a configured AI provider.

## Verify, extract, and install

From the directory containing the archive and checksum:

```bash
sha256sum --check forge-*.tar.gz.sha256
tar -xzf forge-*.tar.gz
cd forge-*/
./install.sh
```

The extracted `./uninstall.sh` uses the same `~/.local` default as the bundle
installer, removes the binary and installed assets, and preserves configuration
and state by default. Add `--purge-config` only when those user files should
also be removed; explicit `--prefix` / `--bin-dir` overrides are forwarded.
First-run configuration publication is atomic: an existing file or symlink and
a configuration created concurrently with installation are never overwritten.
Binary and support-tool upgrades are also staged beside their destinations and
renamed atomically, so an interruption keeps the previous executable intact.
The desktop entry is validated, safely quotes non-standard HOME paths, and is
published by the same no-symlink-following rename pattern.

After installation:

```bash
forge --doctor
forge --doctor --json
forge --check-config
forge --safe-mode
forge
```

For support, `forge-support-bundle [OUTPUT_DIRECTORY]` creates a
privacy-preserving archive without network access. Review it before sharing.
