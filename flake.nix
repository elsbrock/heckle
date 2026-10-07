{
  description = "heckle: live screen + webcam commentator";

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
        name = "heckle";
        desktopName = "Heckle";
        genericName = "Screen commentator";
        comment = "Live documentary-style commentary on your screen and webcam";
        exec = "heckle";
        icon = "heckle";
        terminal = false;
        categories = [ "AudioVideo" "Audio" ];
        keywords = [ "heckle" "commentary" "speech" "webcam" ];
        actions = {
          poke = { name = "Narrate now"; exec = "heckle poke"; };
          stop = { name = "Stop speaking"; exec = "heckle stop"; };
          toggle = { name = "Toggle on/off"; exec = "heckle toggle"; };
        };
      };

      # The compiled binary alone; wrapping and desktop files live in `heckle` below so that
      # touching them does not recompile everything.
      heckleBin = pkgs.rustPlatform.buildRustPackage {
        pname = "heckle-unwrapped";
        version = "0.1.0";
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;

        nativeBuildInputs = [ pkgs.pkg-config ];

        # sherpa-onnx-sys downloads its libraries in build.rs; hand it the pinned archive.
        SHERPA_ONNX_ARCHIVE_DIR = pkgs.linkFarm "sherpa-onnx-archives" [
          { name = sherpaArchive; path = sherpaLibs; }
        ];

        meta.mainProgram = "heckle";
      };

      heckle = pkgs.runCommand "heckle-${heckleBin.version}"
        {
          nativeBuildInputs = [ pkgs.makeWrapper ];
          meta = {
            description = "Live documentary-style commentator for your screen and webcam";
            mainProgram = "heckle";
            license = lib.licenses.mit;
            platforms = [ system ];
          };
        } ''
        mkdir -p $out/share/applications $out/share/icons/hicolor/scalable/apps
        makeWrapper ${heckleBin}/bin/heckle $out/bin/heckle \
          --prefix PATH : ${lib.makeBinPath runtimeDeps} \
          --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : ${lib.makeSearchPathOutput "out" "lib/gstreamer-1.0" runtimeDeps} \
          --set-default HECKLE_VOICE_DIR ${kokoroModel}
        install -Dm644 ${./assets/heckle.svg} \
          $out/share/icons/hicolor/scalable/apps/heckle.svg
        install -Dm644 ${desktopItem}/share/applications/heckle.desktop \
          $out/share/applications/heckle.desktop
      '';
    in
    {
      packages.${system} = {
        default = heckle;
        inherit heckle;
        kokoro-model = kokoroModel;
      };

      apps.${system}.default = {
        type = "app";
        program = lib.getExe heckle;
      };

      homeManagerModules.default = import ./nix/hm-module.nix self;

      overlays.default = final: _prev: { heckle = self.packages.${final.system}.default; };

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
