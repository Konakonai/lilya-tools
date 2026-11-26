{
  description = "Personal standalone plugins and scripts consumed by nixos-config";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};

      kestrel = pkgs.rustPlatform.buildRustPackage {
        pname = "kestrel";
        version = "0.1.0";
        src = ./kestrel;
        cargoLock.lockFile = ./kestrel/Cargo.lock;

        nativeBuildInputs = [
          pkgs.pkg-config
          pkgs.wrapGAppsHook4
          pkgs.copyDesktopItems
        ];
        # nixpkgs' Rust builds link libsqlite3-sys against the system SQLite
        # even with rusqlite's `bundled` feature, so provide it.
        buildInputs = [
          pkgs.gtk4
          pkgs.libadwaita
          pkgs.sqlite
        ];

        desktopItems = [
          (pkgs.makeDesktopItem {
            name = "io.github.konakonai.Kestrel";
            desktopName = "Kestrel";
            genericName = "AI Agent Usage";
            comment = "Token usage, cost, speed and quota for local AI coding agents";
            exec = "kestrel";
            icon = "io.github.konakonai.Kestrel";
            categories = [
              "Development"
              "Utility"
            ];
            startupWMClass = "io.github.konakonai.Kestrel";
          })
        ];

        postInstall = ''
          install -Dm644 data/io.github.konakonai.Kestrel.svg \
            "$out/share/icons/hicolor/scalable/apps/io.github.konakonai.Kestrel.svg"
        '';

        meta = {
          description = "GTK4 monitor for Claude Code, Codex, Antigravity, Pi and Grok token usage, cost, speed and quota";
          mainProgram = "kestrel";
          platforms = [ "x86_64-linux" ];
        };
      };
    in
    {
      packages.${system} = {
        inherit kestrel;
        default = kestrel;
      };

      devShells.${system}.kestrel = pkgs.mkShell {
        inputsFrom = [ kestrel ];
        packages = [
          pkgs.clippy
          pkgs.rustfmt
          pkgs.rust-analyzer
        ];
      };

      checks.${system} = {
        inherit kestrel;

        music-discover =
          pkgs.runCommand "music-discover-tests"
            {
              nativeBuildInputs = [ pkgs.python3 ];
            }
            ''
              python3 -B -m unittest discover -s ${self}/music-discover/tests -v
              touch "$out"
            '';
      };

      formatter.${system} = pkgs.nixfmt-tree;
    };
}
