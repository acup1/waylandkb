# No flake evaluation is needed here. Reuse the locked nixpkgs revision so
# installation on an older NixOS release still gets a compatible Rust/GTK stack.
{
  system ? builtins.currentSystem,
  pkgs ?
    let
      locked = (builtins.fromJSON (builtins.readFile ./flake.lock)).nodes.nixpkgs.locked;
    in
    import (builtins.fetchTarball {
      url = "https://github.com/${locked.owner}/${locked.repo}/archive/${locked.rev}.tar.gz";
      sha256 = locked.narHash;
    }) { inherit system; },
}:
pkgs.callPackage ./nix/package.nix { }
