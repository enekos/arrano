{
  description = "Eagle view over your GitHub PRs: lazygit-style TUI on top of gh, with Claude Code reviews";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      manifest = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package;
    in
    {
      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "arrano";
          version = manifest.version;
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [ pkgs.makeWrapper ];
          postFixup = ''
            wrapProgram $out/bin/arrano --suffix PATH : ${pkgs.lib.makeBinPath [ pkgs.gh pkgs.git ]}
          '';
          meta = {
            description = manifest.description;
            homepage = "https://github.com/enekos/arrano";
            license = pkgs.lib.licenses.mit;
            mainProgram = "arrano";
          };
        };
      });
    };
}
