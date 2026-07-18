{
  description = "Git Repository Manager";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "grm";
          version = "0.1.1";
          src = ./.;

          cargoLock.lockFile = ./Cargo.lock;

          buildInputs = [ pkgs.pkg-config pkgs.zlib ]
            ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.libiconv ];

          meta = with pkgs.lib; {
            description = "Git Repository Manager";
            homepage = "https://github.com/futsuuu/grm";
            license = licenses.mit;
            maintainers = [ "futsuuu" ];
            mainProgram = "grm";
          };
        };

        packages.grm = self.packages.${system}.default;

        apps.default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/grm";
        };
      }
    );
}
