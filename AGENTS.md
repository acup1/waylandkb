# Repository Guidelines

## Project Structure & Module Organization

This repository is a Rust Wayland on-screen keyboard prototype. Keep code organized by responsibility:

- `src/main.rs` builds the GTK4 layer-shell UI.
- `src/layout.rs` defines the split keyboard rows and key actions.
- `src/backend.rs` routes text through `zwp_virtual_keyboard_v1` and shortcuts through uinput.
- `src/uinput.rs` owns the persistent Linux virtual keyboard used for system shortcuts.
- `src/blur.rs` requests compositor blur through `ext_background_effect_v1`.
- `src/control.rs` owns external visibility commands over the Unix socket.
- `src/tray.rs` exports the StatusNotifierItem and DBusMenu over the session bus; `src/tray_*.xml` describe those interfaces.
- `src/input_method.rs` tracks focused text inputs through `zwp_input_method_v2`.
- `src/accessibility.rs` observes AT-SPI focus metadata and terminal desktop categories; never read field contents for auto-show.
- `src/visibility.rs` combines independent focus sources and debounces hiding.
- `src/system_layout.rs` tracks the compositor's active keyboard layout.
- `config/` stores compositor snippets, the virtual keyboard XKB map, and an optional NixOS uinput module.
- `assets/` contains the editable tray SVG and its generated PNG, embedded in the binary; regeneration is documented in `README.md`.
- `docs/images/` stores README screenshots of real keyboard panels over a generated background, without user desktop content.
- `nix/package.nix` is the shared Nix package; `flake.nix` exports packages/apps and the optional uinput module, while `default.nix` provides non-flake installation using the locked nixpkgs revision.
- Module-level tests cover key mapping and interaction state; `src/live_tests.rs` contains opt-in desktop integration tests.

Keep `.agents/` and `.codex/` intact. Do not place application code inside those directories.

## Build, Test, and Development Commands

Use Cargo for local development:

- `cargo fmt`: format Rust code.
- `cargo check`: type-check the project without producing a release binary.
- `cargo test`: run non-interactive unit tests.
- `cargo clippy --all-targets -- -D warnings`: lint application and tests.
- `cargo run -- --always-visible`: run the keyboard as a visible layer-shell overlay.
- `printf show | nc -U /tmp/waylandkb.sock`: show a running hidden keyboard.

On NixOS, run these commands inside `nix develop` or `nix-shell`. GTK4, layer-shell development libraries, and `pkg-config` must be available on the host.

Nix packaging: use `nix build` (flakes) or `nix-build` (without flakes). Both build the same package, run non-interactive tests, install icons/a desktop entry, and wrap GTK runtime dependencies. `nix flake check --no-build` validates the outputs; `nix run . -- --always-visible` runs the packaged app. README installation examples must use `environment.systemPackages`, not `cargo install`. Do not run `nixos-rebuild switch` on the user's system to test packaging.

## Coding Style & Naming Conventions

Use `rustfmt` defaults. Rust modules, functions, and variables use `snake_case`; types and traits use `PascalCase`; constants use `SCREAMING_SNAKE_CASE`. Keep GTK widget construction close to `src/main.rs` unless it becomes reusable.

## Testing Guidelines

Place tests under `tests/` or inside module-level `#[cfg(test)]` blocks. Test names should describe behavior, for example `split_layout_contains_backspace`.

New behavior should include tests for the expected path and at least one edge case. Run `cargo test` once tests exist.

The ignored desktop test is explicitly opt-in: `WAYLANDKB_LIVE_TEST=1 GDK_BACKEND=wayland cargo test desktop_shortcuts_from_ui -- --ignored --nocapture --test-threads=1`.
It requires writable `/dev/uinput` and two system layouts switched by Super+Space. It opens a test receiving window and temporarily changes/restores the layout. Do not run it as an unattended unit test or while someone is interacting with other windows.

The visual test is also opt-in: `WAYLANDKB_LIVE_TEST=1 GDK_BACKEND=wayland cargo test desktop_keyboard_background -- --ignored --nocapture --test-threads=1`. It briefly shows a fullscreen generated checkerboard behind real keyboard panels, verifies rendered alpha, and optionally captures just the test panel with `grim`.

Regenerate README images with `WAYLANDKB_LIVE_TEST=1 GDK_BACKEND=wayland cargo test desktop_readme_screenshots -- --ignored --nocapture --test-threads=1`. It requires `grim` and briefly shows a generated backdrop behind real keyboard panels, capturing only that region. Close other keyboard instances first and inspect every image for user content before publishing.

## Commit & Pull Request Guidelines

Use short, imperative commit subjects such as `Add layout parser` or `Fix config validation`. The project is published at `git@github.com:acup1/waylandkb.git`; keep build output and local agent state out of commits.

Pull requests should include a concise summary, verification steps, linked issues when relevant, and screenshots or terminal output for user-visible changes.

## Agent-Specific Instructions

Before making changes, inspect the current tree and preserve user-created files. Avoid destructive Git commands unless explicitly requested. Update this guide whenever build commands, test commands, or repository layout become concrete.
