{ pkgs ? import <nixpkgs> { } }:

pkgs.mkShell {
  packages = with pkgs; [
    cargo
    clippy
    rustc
    rustfmt
    netcat-openbsd
  ];

  nativeBuildInputs = with pkgs; [
    pkg-config
  ];

  buildInputs = with pkgs; [
    cairo
    gdk-pixbuf
    glib
    graphene
    gtk4
    gtk4-layer-shell
    pango
  ];
}
