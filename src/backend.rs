use std::cell::RefCell;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::AsFd;
use std::rc::Rc;

use crate::uinput::UinputKeyboard;

use anyhow::{Context, Result};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

const MOD_SHIFT: u32 = 1;
const MOD_CTRL: u32 = 4;
const MOD_ALT: u32 = 8;
const MOD_LOGO: u32 = 64;

const KEY_LEFT_CTRL: u32 = 29;
const KEY_LEFT_SHIFT: u32 = 42;
const KEY_LEFT_ALT: u32 = 56;
const KEY_LEFT_META: u32 = 125;

// A complete evdev keymap keeps the keycodes sent to the focused application
// identical to a physical keyboard. This matters on compositors which consume
// the virtual keyboard's keymap internally but keep the seat keymap for clients.
const KEYMAP: &str = include_str!("../config/virtual-keyboard.xkb");

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub super_key: bool,
    pub alt: bool,
    pub shift: bool,
    pub caps_lock: bool,
}

impl Modifiers {
    pub const fn has_shortcut_modifier(self) -> bool {
        self.ctrl || self.super_key || self.alt
    }

    pub const fn has_one_shot_modifier(self) -> bool {
        self.ctrl || self.super_key || self.alt || self.shift
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyAction {
    Text {
        value: &'static str,
        physical_key: &'static str,
        group: u32,
    },
    Symbol {
        value: &'static str,
        physical_key: &'static str,
        shifted: bool,
    },
    Key(&'static str),
}

pub trait InputBackend: Clone + 'static {
    fn send(&self, action: &KeyAction, modifiers: Modifiers) -> Result<()>;
    fn sync_modifiers(&self, modifiers: Modifiers) -> Result<()>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct KeyStroke {
    key_code: u32,
    depressed_modifiers: u32,
    group: u32,
}

impl KeyStroke {
    fn new(action: &KeyAction, modifiers: Modifiers) -> Result<Self> {
        let (physical_key, group, shift_mode, display_value) = match action {
            KeyAction::Text {
                value,
                physical_key,
                group,
            } => (*physical_key, *group, ShiftMode::Text, *value),
            KeyAction::Symbol {
                value,
                physical_key,
                shifted,
            } => (*physical_key, 0, ShiftMode::Symbol(*shifted), *value),
            KeyAction::Key(key) => (*key, 0, ShiftMode::NamedKey, *key),
        };
        let key_code = evdev_key_code(physical_key).with_context(|| {
            format!("unsupported virtual keyboard key `{physical_key}` for `{display_value}`")
        })?;

        // Shortcuts are conventionally resolved by their QWERTY position even
        // while the visible system layout is non-Latin. Keeping group 1 here
        // turns Ctrl+с into Ctrl+Cyrillic_es, which most applications and
        // compositor bindings do not recognize as Ctrl+C.
        let group = if shift_mode == ShiftMode::Text && modifiers.has_shortcut_modifier() {
            0
        } else {
            group
        };

        let effective_shift = match shift_mode {
            ShiftMode::Text if !modifiers.has_shortcut_modifier() => {
                modifiers.shift ^ modifiers.caps_lock
            }
            ShiftMode::Text | ShiftMode::NamedKey => modifiers.shift,
            ShiftMode::Symbol(shifted) => shifted || modifiers.shift,
        };
        let depressed_modifiers = (modifiers.ctrl as u32 * MOD_CTRL)
            | (modifiers.super_key as u32 * MOD_LOGO)
            | (modifiers.alt as u32 * MOD_ALT)
            | (effective_shift as u32 * MOD_SHIFT);

        Ok(Self {
            key_code,
            depressed_modifiers,
            group,
        })
    }

    fn modifier_key_codes(self) -> Vec<u32> {
        [
            (MOD_CTRL, KEY_LEFT_CTRL),
            (MOD_SHIFT, KEY_LEFT_SHIFT),
            (MOD_ALT, KEY_LEFT_ALT),
            (MOD_LOGO, KEY_LEFT_META),
        ]
        .into_iter()
        .filter_map(|(mask, key_code)| (self.depressed_modifiers & mask != 0).then_some(key_code))
        .collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShiftMode {
    Text,
    Symbol(bool),
    NamedKey,
}

#[derive(Clone, Debug)]
pub struct VirtualKeyboardBackend {
    shortcuts: Rc<RefCell<Option<UinputKeyboard>>>,
}

impl Default for VirtualKeyboardBackend {
    fn default() -> Self {
        let shortcuts = match UinputKeyboard::new() {
            Ok(keyboard) => {
                eprintln!("shortcuts: using /dev/uinput (system keyboard input path)");
                Some(keyboard)
            }
            Err(error) => {
                eprintln!(
                    "{error:#}; falling back to Wayland. System shortcuts/layout switching may not work."
                );
                None
            }
        };
        Self {
            shortcuts: Rc::new(RefCell::new(shortcuts)),
        }
    }
}

impl InputBackend for VirtualKeyboardBackend {
    fn send(&self, action: &KeyAction, modifiers: Modifiers) -> Result<()> {
        let stroke = KeyStroke::new(action, modifiers)?;
        if (modifiers.has_shortcut_modifier() || matches!(action, KeyAction::Key(_)))
            && let Some(keyboard) = self.shortcuts.borrow_mut().as_mut()
        {
            return keyboard.send(stroke.key_code, &stroke.modifier_key_codes());
        }
        send_key_stroke(stroke)
    }

    fn sync_modifiers(&self, modifiers: Modifiers) -> Result<()> {
        if let Some(keyboard) = self.shortcuts.borrow_mut().as_mut() {
            // Send real transitions as soon as the UI modifier changes, not
            // just with a main key. This also permits modifier-only bindings.
            let stroke = KeyStroke::new(&KeyAction::Key("space"), modifiers)?;
            keyboard.set_modifiers(&stroke.modifier_key_codes())?;
        }
        Ok(())
    }
}

fn send_key_stroke(stroke: KeyStroke) -> Result<()> {
    let connection = Connection::connect_to_env().context("failed to connect to Wayland")?;
    let (globals, mut event_queue) = registry_queue_init::<WaylandState>(&connection)
        .context("failed to read Wayland globals")?;
    let queue_handle = event_queue.handle();
    let seat: wl_seat::WlSeat = globals
        .bind(&queue_handle, 1..=1, ())
        .context("Wayland compositor did not expose a seat")?;
    let manager: ZwpVirtualKeyboardManagerV1 = globals
        .bind(&queue_handle, 1..=1, ())
        .context("Wayland compositor does not support zwp_virtual_keyboard_v1")?;
    let keyboard = manager.create_virtual_keyboard(&seat, &queue_handle, ());

    let mut keymap_file = tempfile::tempfile().context("failed to create keymap file")?;
    keymap_file
        .write_all(KEYMAP.as_bytes())
        .context("failed to write virtual keyboard keymap")?;
    keymap_file
        .write_all(&[0])
        .context("failed to terminate virtual keyboard keymap")?;
    keymap_file
        .seek(SeekFrom::Start(0))
        .context("failed to rewind virtual keyboard keymap")?;

    keyboard.keymap(1, keymap_file.as_fd(), (KEYMAP.len() + 1) as u32);
    // Some compositors and clients only update shortcut state when they also
    // see the physical modifier key events. Send a complete chord while still
    // keeping the protocol modifier mask authoritative for the main key.
    let modifier_key_codes = stroke.modifier_key_codes();
    for key_code in &modifier_key_codes {
        keyboard.key(0, *key_code, 1);
    }
    keyboard.modifiers(stroke.depressed_modifiers, 0, 0, stroke.group);
    keyboard.key(0, stroke.key_code, 1);
    keyboard.key(0, stroke.key_code, 0);
    for key_code in modifier_key_codes.iter().rev() {
        keyboard.key(0, *key_code, 0);
    }
    keyboard.modifiers(0, 0, 0, stroke.group);

    // Keep the keymap fd and protocol objects alive until the compositor has
    // processed the complete press/release sequence.
    event_queue
        .roundtrip(&mut WaylandState)
        .context("failed to send virtual keyboard event")?;
    keyboard.destroy();
    connection
        .flush()
        .context("failed to flush Wayland events")?;
    Ok(())
}

struct WaylandState;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for WaylandState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(WaylandState: ignore wl_seat::WlSeat);
delegate_noop!(WaylandState: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(WaylandState: ignore ZwpVirtualKeyboardV1);

fn evdev_key_code(name: &str) -> Option<u32> {
    Some(match name.to_ascii_lowercase().as_str() {
        "escape" => 1,
        "1" => 2,
        "2" => 3,
        "3" => 4,
        "4" => 5,
        "5" => 6,
        "6" => 7,
        "7" => 8,
        "8" => 9,
        "9" => 10,
        "0" => 11,
        "minus" => 12,
        "equal" => 13,
        "backspace" => 14,
        "tab" => 15,
        "q" => 16,
        "w" => 17,
        "e" => 18,
        "r" => 19,
        "t" => 20,
        "y" => 21,
        "u" => 22,
        "i" => 23,
        "o" => 24,
        "p" => 25,
        "bracketleft" => 26,
        "bracketright" => 27,
        "return" => 28,
        "a" => 30,
        "s" => 31,
        "d" => 32,
        "f" => 33,
        "g" => 34,
        "h" => 35,
        "j" => 36,
        "k" => 37,
        "l" => 38,
        "semicolon" => 39,
        "apostrophe" => 40,
        "grave" => 41,
        "backslash" => 43,
        "z" => 44,
        "x" => 45,
        "c" => 46,
        "v" => 47,
        "b" => 48,
        "n" => 49,
        "m" => 50,
        "comma" => 51,
        "period" => 52,
        "slash" => 53,
        "space" => 57,
        "f1" => 59,
        "f2" => 60,
        "f3" => 61,
        "f4" => 62,
        "f5" => 63,
        "f6" => 64,
        "f7" => 65,
        "f8" => 66,
        "f9" => 67,
        "f10" => 68,
        "f11" => 87,
        "f12" => 88,
        "home" => 102,
        "up" => 103,
        "pageup" => 104,
        "left" => 105,
        "right" => 106,
        "end" => 107,
        "down" => 108,
        "pagedown" => 109,
        "insert" => 110,
        "delete" => 111,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CYRILLIC_C: KeyAction = KeyAction::Text {
        value: "с",
        physical_key: "c",
        group: 1,
    };

    #[test]
    fn russian_character_uses_physical_key_and_russian_group() {
        assert_eq!(
            KeyStroke::new(&CYRILLIC_C, Modifiers::default()).unwrap(),
            KeyStroke {
                key_code: 46,
                depressed_modifiers: 0,
                group: 1,
            }
        );
    }

    #[test]
    fn russian_ctrl_shortcut_uses_latin_group_and_physical_position() {
        assert_eq!(
            KeyStroke::new(
                &CYRILLIC_C,
                Modifiers {
                    ctrl: true,
                    ..Modifiers::default()
                }
            )
            .unwrap(),
            KeyStroke {
                key_code: 46,
                depressed_modifiers: MOD_CTRL,
                group: 0,
            }
        );
    }

    #[test]
    fn modifier_key_events_form_a_complete_physical_chord() {
        let stroke = KeyStroke::new(
            &CYRILLIC_C,
            Modifiers {
                ctrl: true,
                super_key: true,
                alt: true,
                shift: true,
                ..Modifiers::default()
            },
        )
        .unwrap();

        assert_eq!(
            stroke.modifier_key_codes(),
            vec![KEY_LEFT_CTRL, KEY_LEFT_SHIFT, KEY_LEFT_ALT, KEY_LEFT_META]
        );
    }

    #[test]
    fn shift_uppercases_text_with_a_real_modifier() {
        let stroke = KeyStroke::new(
            &CYRILLIC_C,
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        )
        .unwrap();
        assert_eq!(stroke.depressed_modifiers, MOD_SHIFT);
    }

    #[test]
    fn caps_lock_and_shift_cancel_for_text_case() {
        let stroke = KeyStroke::new(
            &CYRILLIC_C,
            Modifiers {
                shift: true,
                caps_lock: true,
                ..Modifiers::default()
            },
        )
        .unwrap();
        assert_eq!(stroke.depressed_modifiers, 0);
    }

    #[test]
    fn named_key_receives_active_modifiers() {
        let stroke = KeyStroke::new(
            &KeyAction::Key("Tab"),
            Modifiers {
                alt: true,
                shift: true,
                ..Modifiers::default()
            },
        )
        .unwrap();
        assert_eq!(stroke.key_code, 15);
        assert_eq!(stroke.depressed_modifiers, MOD_ALT | MOD_SHIFT);
    }

    #[test]
    fn super_space_is_a_complete_physical_chord() {
        let space = KeyAction::Text {
            value: " ",
            physical_key: "space",
            group: 0,
        };
        let stroke = KeyStroke::new(
            &space,
            Modifiers {
                super_key: true,
                ..Modifiers::default()
            },
        )
        .unwrap();
        assert_eq!(stroke.key_code, 57);
        assert_eq!(stroke.depressed_modifiers, MOD_LOGO);
    }

    #[test]
    fn unknown_key_is_rejected() {
        let error = KeyStroke::new(&KeyAction::Key("not-a-key"), Modifiers::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("not-a-key"));
    }

    #[test]
    fn shifted_symbol_ignores_caps_lock() {
        let symbol = KeyAction::Symbol {
            value: "?",
            physical_key: "slash",
            shifted: true,
        };
        let stroke = KeyStroke::new(
            &symbol,
            Modifiers {
                caps_lock: true,
                ..Modifiers::default()
            },
        )
        .unwrap();

        assert_eq!(stroke.key_code, 53);
        assert_eq!(stroke.depressed_modifiers, MOD_SHIFT);
        assert_eq!(stroke.group, 0);
    }

    #[test]
    fn all_visible_keys_support_every_modifier_combination() {
        use crate::layout::{Action, Language, LayoutPage};
        for language in [Language::English, Language::Russian] {
            for page in [
                LayoutPage::Letters,
                LayoutPage::Symbols,
                LayoutPage::Functions,
            ] {
                for key in language
                    .rows(page)
                    .iter()
                    .flat_map(|row| row.left.iter().chain(row.right))
                {
                    let Action::Input(action) = key.action else {
                        continue;
                    };
                    for bits in 0..16 {
                        let mods = Modifiers {
                            ctrl: bits & 1 != 0,
                            super_key: bits & 2 != 0,
                            alt: bits & 4 != 0,
                            shift: bits & 8 != 0,
                            caps_lock: false,
                        };
                        let stroke = KeyStroke::new(&action, mods).unwrap();
                        if mods.ctrl {
                            assert_ne!(stroke.depressed_modifiers & MOD_CTRL, 0);
                        }
                        if mods.super_key {
                            assert_ne!(stroke.depressed_modifiers & MOD_LOGO, 0);
                        }
                        if mods.alt {
                            assert_ne!(stroke.depressed_modifiers & MOD_ALT, 0);
                        }
                        if mods.shift {
                            assert_ne!(stroke.depressed_modifiers & MOD_SHIFT, 0);
                        }
                    }
                }
            }
        }
    }
}
