{
  description = "nislock — gamepad-driven unlock screen for Steam Deck (NixOS)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { self
    , nixpkgs,
    }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      workspaceManifest = builtins.fromTOML (builtins.readFile ./Cargo.toml);
    in
    {
      packages = forAllSystems (
        pkgs:
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "nislock";
            version = workspaceManifest.workspace.package.version;
            src = pkgs.lib.cleanSource ./.;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "-p" "nislock" ];
            nativeBuildInputs = [
              pkgs.pkg-config
              pkgs.rustPlatform.bindgenHook
            ];
            buildInputs = [
              pkgs.pam
              pkgs.libxkbcommon
            ];
            meta = {
              description = "Gamepad-driven unlock screen for Steam Deck";
              homepage = "https://github.com/argxentakato/nislock";
              license = pkgs.lib.licenses.agpl3Only;
              platforms = pkgs.lib.platforms.linux;
              mainProgram = "nislock";
            };
          };
        }
      );

      devShells = forAllSystems (pkgs: {
        # The Rust toolchain is NOT vendored here: rustup (stable) is used as
        # usual. This shell only supplies the native C dependencies needed by
        # sctk/pam crates and cargo test wiring.
        default = pkgs.mkShell {
          packages = [
            pkgs.pkg-config
            pkgs.clang # bindgen for pam-sys
            pkgs.libxkbcommon
            pkgs.pam
            pkgs.libinput # handy while iterating on evdev rules
          ];
          shellHook = ''
            export LIBCLANG_PATH="${pkgs.libclang.lib}/lib"
          '';
        };
      });
    };
}
