{
  description = "Dogpatch household e-paper dashboard";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    {
      packages = nixpkgs.lib.genAttrs [
        "aarch64-linux"
        "aarch64-darwin"
      ] (system: {
        default = nixpkgs.legacyPackages.${system}.callPackage ./nix/package.nix { };
      });

      nixosModules.default = import ./nix/module.nix;
    };
}
