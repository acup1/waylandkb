{
  description = "Wayland split on-screen keyboard";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          waylandkb = pkgs.callPackage ./nix/package.nix { };
        in
        {
          inherit waylandkb;
          default = waylandkb;
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/waylandkb";
          meta.description = self.packages.${system}.default.meta.description;
        };
      });

      # Installing the package alone must not grant virtual-input permissions.
      nixosModules.uinput = import ./config/uinput.nix;

      checks = forAllSystems (system: {
        package = self.packages.${system}.default;
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        {
          default = import ./shell.nix { inherit pkgs; };
        }
      );
    };
}
