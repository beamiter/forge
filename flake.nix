{
  description = "forge — a session-aware GTK4 terminal with structured command blocks";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachSystem
      [
        "x86_64-linux"
        "aarch64-linux"
      ]
      (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          manifest = builtins.fromTOML (builtins.readFile ./Cargo.toml);
          appId = "io.github.beamiter.forge";

          package = pkgs.rustPlatform.buildRustPackage {
            pname = manifest.package.name;
            version = manifest.package.version;
            src = self;

            cargoLock = {
              lockFile = ./Cargo.lock;
              # Git dependencies are not covered by Cargo.lock checksums, so
              # Nix needs an explicit hash per revision. Update these whenever
              # the jagent / jterm_core pins in Cargo.lock change.
              outputHashes = {
                "jagent-0.5.0" = "sha256-UuHxaZTR9gQB27E2d2iyjCaICBADmecC6E/GNyvCJIE=";
                # jterm_core rev 9e79a5bf0d905575863def4d0e77f74a1f533638
                "jterm_core-0.1.0" = "sha256-+72eS2tNLK3KXstAUup1R5l0jWCAYahNN/kFWj6uxc4=";
              };
            };
            strictDeps = true;

            nativeBuildInputs = with pkgs; [
              pkg-config
              wrapGAppsHook4
            ];

            buildInputs = with pkgs; [
              gtk4
              libadwaita
              vte-gtk4
              pcre2
              fcitx5-gtk
            ];

            FCITX5_GTK_PATH = "${pkgs.fcitx5-gtk}/lib/gtk-4.0";

            # GTK behavior is tested under a real display in CI. Running the
            # suite in the Nix build sandbox would produce display failures.
            doCheck = false;

            postInstall = ''
              install -Dm644 data/${appId}.desktop \
                "$out/share/applications/${appId}.desktop"
              install -Dm644 data/${appId}.metainfo.xml \
                "$out/share/metainfo/${appId}.metainfo.xml"
              install -Dm644 data/${appId}.svg \
                "$out/share/icons/hicolor/scalable/apps/${appId}.svg"
              install -Dm644 data/${appId}-128.png \
                "$out/share/icons/hicolor/128x128/apps/${appId}.png"
              install -Dm644 data/${appId}-256.png \
                "$out/share/icons/hicolor/256x256/apps/${appId}.png"
              install -Dm644 config.toml.example \
                "$out/share/doc/forge/config.toml.example"
              install -Dm644 README.md "$out/share/doc/forge/README.md"
              install -Dm644 Cargo.lock "$out/share/doc/forge/Cargo.lock"
              install -Dm755 scripts/support-bundle.sh \
                "$out/bin/forge-support-bundle"

              install -d "$out/share/forge/shell-integration"
              install -m644 scripts/shell-integration/README.md \
                scripts/shell-integration/forge.* \
                "$out/share/forge/shell-integration/"
              install -d "$out/share/forge/workflows"
              install -m644 scripts/workflows/*.yaml \
                "$out/share/forge/workflows/"
              install -Dm644 scripts/notebooks/welcome.jtnb.md \
                "$out/share/forge/notebooks/welcome.jtnb.md"
            '';

            preFixup = ''
              gappsWrapperArgs+=(
                --set-default FORGE_WORKFLOW_DIR "$out/share/forge/workflows"
                --set-default FORGE_ASSET_DIR "$out/share/forge"
              )
            '';

            meta = with pkgs.lib; {
              description = manifest.package.description;
              homepage = manifest.package.repository;
              mainProgram = "forge";
              platforms = platforms.linux;
            };
          };
        in
        {
          packages.default = package;
          apps.default = flake-utils.lib.mkApp { drv = package; };
          checks.package = package;
          formatter = pkgs.nixfmt-rfc-style;

          devShells.default = pkgs.mkShell {
            inputsFrom = [ package ];
            packages = with pkgs; [
              cargo
              rustc
              rustfmt
              clippy
              cargo-audit
              cargo-watch
              shellcheck

              gtk4
              glib
              pkg-config
              libadwaita
              vte
              vte-gtk4
              pcre2
              fcitx5-gtk

              glade
              cambalache
              xdotool
              jq
              valgrind
              strace
              patchelf
              fuse
              fakeroot
              openssl
            ];

            shellHook = ''
              export GSETTINGS_SCHEMA_DIR="${pkgs.gtk4}/share/gsettings-schemas/:${pkgs.glib}/share/gsettings-schemas/"
              export RUST_BACKTRACE=1
              export GTK_IM_MODULE="''${GTK_IM_MODULE:-fcitx}"
              export XMODIFIERS="''${XMODIFIERS:-@im=fcitx}"
              export QT_IM_MODULE="''${QT_IM_MODULE:-fcitx}"
              export GTK_PATH="${pkgs.fcitx5-gtk}/lib/gtk-4.0''${GTK_PATH:+:$GTK_PATH}"
              export FCITX5_GTK_PATH="${pkgs.fcitx5-gtk}/lib/gtk-4.0"
              echo "forge development environment ready. Run 'make verify'."
            '';
          };
        }
      );
}
