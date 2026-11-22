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
    in
    {
      checks.${system}.music-discover =
        pkgs.runCommand "music-discover-tests"
          {
            nativeBuildInputs = [ pkgs.python3 ];
          }
          ''
            python3 -B -m unittest discover -s ${self}/music-discover/tests -v
            touch "$out"
          '';

      formatter.${system} = pkgs.nixfmt-tree;
    };
}
