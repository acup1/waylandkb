{
  lib,
  rustPlatform,
  pkg-config,
  wrapGAppsHook4,
  copyDesktopItems,
  makeDesktopItem,
  gtk4,
  gtk4-layer-shell,
  wayland,
  glib,
  gsettings-desktop-schemas,
}:
rustPlatform.buildRustPackage {
  pname = "waylandkb";
  version = (builtins.fromTOML (builtins.readFile ../Cargo.toml)).package.version;

  # Keep build output, screenshots and local configuration out of the source.
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      ../assets
      ../config
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [
    pkg-config
    wrapGAppsHook4
    copyDesktopItems
  ];
  buildInputs = [
    gtk4
    gtk4-layer-shell
    wayland
    gsettings-desktop-schemas
  ];

  desktopItems = [
    (makeDesktopItem {
      name = "dev.waylandkb.Keyboard";
      desktopName = "waylandkb";
      genericName = "On-screen keyboard";
      comment = "Split on-screen keyboard for Wayland";
      exec = "waylandkb";
      icon = "waylandkb";
      categories = [
        "Utility"
        "Accessibility"
      ];
      terminal = false;
      startupNotify = false;
    })
  ];

  postInstall = ''
    install -Dm644 assets/waylandkb.svg "$out/share/icons/hicolor/scalable/apps/waylandkb.svg"
    install -Dm644 assets/waylandkb.png "$out/share/icons/hicolor/128x128/apps/waylandkb.png"
  '';

  preFixup = ''
    # gsettings is used to detect the active GNOME layout. Compositor-specific
    # optional tools (hyprctl, swaymsg, niri, etc.) remain on the session PATH.
    gappsWrapperArgs+=(--prefix PATH : ${lib.makeBinPath [ glib ]})
  '';

  # Only non-interactive unit tests run in the sandbox; desktop tests are ignored.
  doCheck = true;
  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    "$out/bin/waylandkb" --version
    "$out/bin/waylandkb" --help > /dev/null
    test -s "$out/share/applications/dev.waylandkb.Keyboard.desktop"
    test -s "$out/share/icons/hicolor/scalable/apps/waylandkb.svg"
    runHook postInstallCheck
  '';

  meta = {
    description = "Split on-screen keyboard for Wayland with system layout tracking and a tray icon";
    homepage = "https://github.com/acup1/waylandkb";
    license = lib.licenses.mit;
    mainProgram = "waylandkb";
    platforms = lib.platforms.linux;
  };
}
