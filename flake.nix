{
  description = "narrator: live screen + webcam commentator";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (s: f nixpkgs.legacyPackages.${s});
    in {
      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo rustc clippy rustfmt rust-analyzer pkg-config
            # webcam: gst-launch + the pipewiresrc plugin
            gst_all_1.gstreamer gst_all_1.gst-plugins-base gst_all_1.gst-plugins-good pipewire
          ];
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        };
      });
    };
}
