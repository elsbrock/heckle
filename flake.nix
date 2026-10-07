{
  description = "narrator: live screen + webcam commentator";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      # sherpa-onnx's prebuilt static libraries are pinned below for x86_64 Linux only.
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      inherit (pkgs) lib;

      # Everything the binary shells out to: gst-launch (+ the pipewiresrc/jpeg/waylandsink
      # plugins) for the camera, pw-cat for audio and pw-dump for camera discovery.
      runtimeDeps = with pkgs; [
        gst_all_1.gstreamer
        gst_all_1.gst-plugins-base
        gst_all_1.gst-plugins-good
        gst_all_1.gst-plugins-bad
        pipewire
      ];

      sherpaVersion = "1.13.8";
      sherpaArchive = "sherpa-onnx-v${sherpaVersion}-linux-x64-static-lib.tar.bz2";
      sherpaLibs = pkgs.fetchurl {
        url = "https://github.com/k2-fsa/sherpa-onnx/releases/download/v${sherpaVersion}/${sherpaArchive}";
        hash = "sha256-4f3FtnUw4VdB74l/pf//KXBW878MbYKaJ6+SJaTEtaY=";
      };

      kokoroModel = pkgs.runCommand "kokoro-en-v0_19"
        {
          src = pkgs.fetchurl {
            url = "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-en-v0_19.tar.bz2";
            hash = "sha256-kSgEhVoEdF+nejC+VFs/ml0VxNZtsAuIy81JId9gWsc=";
          };
          nativeBuildInputs = [ pkgs.gnutar pkgs.bzip2 ];
        } ''
        mkdir -p $out
        tar -xjf $src -C $out --strip-components=1
      '';

      desktopItem = pkgs.makeDesktopItem {
        name = "narrator";
        desktopName = "Narrator";
        genericName = "Screen commentator";
        comment = "Live documentary-style commentary on your screen and webcam";
        exec = "narrator";
        icon = "narrator";
        terminal = false;
        categories = [ "AudioVideo" "Audio" ];
        keywords = [ "narrator" "commentary" "speech" "webcam" ];
        actions = {
          poke = { name = "Narrate now"; exec = "narrator poke"; };
          stop = { name = "Stop speaking"; exec = "narrator stop"; };
          toggle = { name = "Toggle on/off"; exec = "narrator toggle"; };
        };
      };

      # The compiled binary alone; wrapping and desktop files live in `narrator` below so that
      # touching them does not recompile everything.
      narratorBin = pkgs.rustPlatform.buildRustPackage {
        pname = "narrator-unwrapped";
        version = "0.1.0";
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;

        nativeBuildInputs = [ pkgs.pkg-config ];

        # sherpa-onnx-sys downloads its libraries in build.rs; hand it the pinned archive.
        SHERPA_ONNX_ARCHIVE_DIR = pkgs.linkFarm "sherpa-onnx-archives" [
          { name = sherpaArchive; path = sherpaLibs; }
        ];

        meta.mainProgram = "narrator";
      };

      narrator = pkgs.runCommand "narrator-${narratorBin.version}"
        {
          nativeBuildInputs = [ pkgs.makeWrapper ];
          meta = {
            description = "Live documentary-style commentator for your screen and webcam";
            mainProgram = "narrator";
            platforms = [ system ];
          };
        } ''
        mkdir -p $out/share/applications $out/share/icons/hicolor/scalable/apps
        makeWrapper ${narratorBin}/bin/narrator $out/bin/narrator \
          --prefix PATH : ${lib.makeBinPath runtimeDeps} \
          --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : ${lib.makeSearchPathOutput "out" "lib/gstreamer-1.0" runtimeDeps} \
          --set-default NARRATOR_VOICE_DIR ${kokoroModel}
        install -Dm644 ${./assets/narrator.svg} \
          $out/share/icons/hicolor/scalable/apps/narrator.svg
        install -Dm644 ${desktopItem}/share/applications/narrator.desktop \
          $out/share/applications/narrator.desktop
      '';
    in
    {
      packages.${system} = {
        default = narrator;
        inherit narrator;
        kokoro-model = kokoroModel;
      };

      apps.${system}.default = {
        type = "app";
        program = lib.getExe narrator;
      };

      homeManagerModules.default = import ./nix/hm-module.nix self;

      overlays.default = final: _prev: { narrator = self.packages.${final.system}.default; };

      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [
          cargo
          rustc
          clippy
          rustfmt
          rust-analyzer
          pkg-config
        ] ++ runtimeDeps;
        RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
      };
    };
}
