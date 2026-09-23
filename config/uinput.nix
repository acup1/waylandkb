# Optional NixOS module. Import into the system configuration, not the dev shell.
{ pkgs, ... }:
{
  boot.kernelModules = [ "uinput" ];
  services.udev.packages = [
    (pkgs.writeTextDir "lib/udev/rules.d/70-waylandkb-uinput.rules" ''
      SUBSYSTEM=="misc", KERNEL=="uinput", TAG+="uaccess", OPTIONS+="static_node=uinput"
    '')
  ];
}
