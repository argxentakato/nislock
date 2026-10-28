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
    in
    {
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
