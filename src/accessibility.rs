//! Focus metadata only: never request text, selections, names or passwords.
//! AT-SPI covers widgets which use IBus/X11 instead of Wayland text-input.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{Context, Result};
use gio::prelude::*;
use glib::variant::ToVariant;
use gtk::{gio, glib};

use crate::input_method::TextInputVisibility;

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const CALL_TIMEOUT_MS: i32 = 300;
const STATE_EDITABLE: u32 = 7;
const STATE_DEFUNCT: u32 = 6;
const STATE_READ_ONLY: u32 = 43;
const ROLE_TERMINAL: u32 = 60;

pub fn start_monitor() -> Receiver<TextInputVisibility> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let context = glib::MainContext::new();
        let result = context.with_thread_default(|| monitor(sender, &context));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!("AT-SPI focus detection unavailable: {error:#}"),
            Err(error) => eprintln!("AT-SPI event loop unavailable: {error}"),
        }
    });
    receiver
}

fn call(
    connection: &gio::DBusConnection,
    bus: &str,
    path: &str,
    interface: &str,
    method: &str,
    args: Option<&glib::Variant>,
) -> Result<glib::Variant> {
    Ok(connection.call_sync(
        Some(bus),
        path,
        interface,
        method,
        args,
        None,
        gio::DBusCallFlags::NONE,
        CALL_TIMEOUT_MS,
        None::<&gio::Cancellable>,
    )?)
}

fn monitor(sender: Sender<TextInputVisibility>, context: &glib::MainContext) -> Result<()> {
    let session = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>)?;
    let reply = call(
        &session,
        "org.a11y.Bus",
        "/org/a11y/bus",
        "org.a11y.Bus",
        "GetAddress",
        None,
    )?;
    let (address,) = reply
        .get::<(String,)>()
        .context("invalid AT-SPI bus address")?;
    let connection = gio::DBusConnection::for_address_sync(
        &address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
            | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None,
        None::<&gio::Cancellable>,
    )?;
    connection.set_exit_on_close(false);
    let focus = Rc::new(RefCell::new(FocusState::default()));

    let event_focus = focus.clone();
    let event_sender = sender.clone();
    let terminal_apps = Rc::new(RefCell::new(HashMap::new()));
    let focused_terminal_apps = terminal_apps.clone();
    connection.signal_subscribe(
        None,
        Some("org.a11y.atspi.Event.Object"),
        Some("StateChanged"),
        None,
        Some("focused"),
        gio::DBusSignalFlags::NONE,
        move |connection, bus, path, _, _, parameters| {
            let Some(enabled) = parameters.try_child_get::<i32>(1).ok().flatten() else {
                return;
            };
            let pid = application_pid(connection, bus);
            if pid == Some(std::process::id()) {
                return;
            }
            let terminal_app = *focused_terminal_apps
                .borrow_mut()
                .entry(bus.to_string())
                .or_insert_with(|| pid.is_some_and(application_is_terminal));
            let target = (bus.to_string(), path.to_string());
            let input =
                enabled != 0 && is_text_input(connection, bus, path, terminal_app).unwrap_or(false);
            if let Some(visibility) = event_focus.borrow_mut().update(target, enabled != 0, input) {
                let _ = event_sender.send(visibility);
            }
        },
    );
    // Some custom GL terminal canvases do not publish focused=true at all.
    // Their window activation is still a reliable input target. Subsequent
    // focus on an actual toolbar button overrides this fallback with Hide.
    let event_focus = focus.clone();
    let event_sender = sender.clone();
    connection.signal_subscribe(
        None,
        Some("org.a11y.atspi.Event.Window"),
        Some("Activate"),
        None,
        None,
        gio::DBusSignalFlags::NONE,
        move |connection, bus, path, _, _, _| {
            let pid = application_pid(connection, bus);
            if pid == Some(std::process::id()) {
                return;
            }
            let terminal_app = *terminal_apps
                .borrow_mut()
                .entry(bus.to_string())
                .or_insert_with(|| pid.is_some_and(application_is_terminal));
            if terminal_app {
                event_focus
                    .borrow_mut()
                    .update((bus.to_string(), path.to_string()), true, true);
                let _ = event_sender.send(TextInputVisibility::Show);
            }
        },
    );
    // Some toolkits keep a widget's focused bit set in an inactive window.
    let event_focus = focus.clone();
    let event_sender = sender.clone();
    connection.signal_subscribe(
        None,
        Some("org.a11y.atspi.Event.Window"),
        Some("Deactivate"),
        None,
        None,
        gio::DBusSignalFlags::NONE,
        move |_, bus, _, _, _, _| {
            if event_focus.borrow_mut().deactivate(bus) {
                let _ = event_sender.send(TextInputVisibility::Hide);
            }
        },
    );
    let event_focus = focus;
    let event_sender = sender.clone();
    connection.signal_subscribe(
        Some("org.freedesktop.DBus"),
        Some("org.freedesktop.DBus"),
        Some("NameOwnerChanged"),
        Some("/org/freedesktop/DBus"),
        None,
        gio::DBusSignalFlags::NONE,
        move |_, _, _, _, _, parameters| {
            if let Some((name, _, owner)) = parameters.get::<(String, String, String)>()
                && owner.is_empty()
                && event_focus.borrow_mut().deactivate(&name)
            {
                let _ = event_sender.send(TextInputVisibility::Hide);
            }
        },
    );

    for event in [
        "object:state-changed:focused",
        "window:activate",
        "window:deactivate",
    ] {
        call(
            &connection,
            "org.a11y.atspi.Registry",
            "/org/a11y/atspi/registry",
            "org.a11y.atspi.Registry",
            "RegisterEvent",
            Some(&(event, Vec::<String>::new(), "").to_variant()),
        )?;
    }
    // Browsers only export accessibility when an assistive client requests it.
    // Do not enable ScreenReaderEnabled: this is an OSK, not a screen reader.
    if let Err(error) = call(
        &session,
        "org.a11y.Bus",
        "/org/a11y/bus",
        "org.freedesktop.DBus.Properties",
        "Set",
        Some(&("org.a11y.Status", "IsEnabled", true.to_variant()).to_variant()),
    ) {
        eprintln!("could not enable accessibility; some browsers may not report focus: {error:#}");
    }
    eprintln!("automatic visibility: AT-SPI focus monitoring enabled (no text is read)");
    let main_loop = glib::MainLoop::new(Some(context), false);
    let disconnected_loop = main_loop.clone();
    connection.connect_closed(move |_, _, _| disconnected_loop.quit());
    main_loop.run();
    let _ = sender.send(TextInputVisibility::Hide);
    Ok(())
}

fn application_pid(connection: &gio::DBusConnection, bus: &str) -> Option<u32> {
    call(
        connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "GetConnectionUnixProcessID",
        Some(&(bus,).to_variant()),
    )
    .ok()
    .and_then(|reply| reply.get::<(u32,)>())
    .map(|(pid,)| pid)
}

fn is_text_input(
    connection: &gio::DBusConnection,
    bus: &str,
    path: &str,
    terminal_app: bool,
) -> Result<bool> {
    let role = call(connection, bus, path, ACCESSIBLE, "GetRole", None)?
        .get::<(u32,)>()
        .context("invalid accessible role")?
        .0;
    let state = call(connection, bus, path, ACCESSIBLE, "GetState", None)?
        .get::<(Vec<u32>,)>()
        .context("invalid accessible state")?
        .0;
    Ok(accepts_input(role, &state, terminal_app))
}

fn accepts_input(role: u32, state: &[u32], terminal_app: bool) -> bool {
    let has = |flag| {
        state
            .get((flag / 32) as usize)
            .is_some_and(|word| *word & (1u32 << (flag % 32)) != 0)
    };
    if has(STATE_DEFUNCT) {
        return false;
    }
    // GPU terminals can expose their focused canvas as a generic panel rather
    // than Editable/Terminal (e.g. Ghostty). Use the freedesktop application
    // category for that fallback, not a hardcoded list of terminal names.
    role == ROLE_TERMINAL
        || (terminal_app && matches!(role, 6 | 18 | 39 | 67))
        || (!has(STATE_READ_ONLY) && has(STATE_EDITABLE))
}

fn executable_name(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .trim_start_matches('.')
        .trim_end_matches("-wrapped")
}

fn is_terminal_desktop(info: &gio::DesktopAppInfo, executable: &Path) -> bool {
    info.categories().is_some_and(|categories| {
        categories
            .split(';')
            .any(|value| value == "TerminalEmulator")
    }) && executable_name(&info.executable()) == executable_name(executable)
}

fn application_is_terminal(pid: u32) -> bool {
    let Ok(executable) = std::fs::read_link(format!("/proc/{pid}/exe")) else {
        return false;
    };
    if gio::AppInfo::all()
        .into_iter()
        .filter_map(|info| info.downcast::<gio::DesktopAppInfo>().ok())
        .any(|info| is_terminal_desktop(&info, &executable))
    {
        return true;
    }
    // Nix packages can be launched outside XDG_DATA_DIRS; consult their own
    // installed desktop metadata too, without reading process arguments/text.
    let Some(prefix) = executable.parent().and_then(Path::parent) else {
        return false;
    };
    std::fs::read_dir(prefix.join("share/applications"))
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| gio::DesktopAppInfo::from_filename(entry.path()))
        .any(|info| is_terminal_desktop(&info, &executable))
}

#[derive(Default)]
struct FocusState {
    target: Option<(String, String)>,
}

impl FocusState {
    fn update(
        &mut self,
        target: (String, String),
        focused: bool,
        input: bool,
    ) -> Option<TextInputVisibility> {
        if focused {
            self.target = Some(target);
            Some(if input {
                TextInputVisibility::Show
            } else {
                TextInputVisibility::Hide
            })
        } else if self.target.as_ref() == Some(&target) {
            self.target = None;
            Some(TextInputVisibility::Hide)
        } else {
            None
        }
    }

    fn deactivate(&mut self, bus: &str) -> bool {
        if self.target.as_ref().is_some_and(|(owner, _)| owner == bus) {
            self.target = None;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editable_web_fields_and_terminals_are_inputs() {
        assert!(accepts_input(79, &[1 << STATE_EDITABLE, 0], false));
        assert!(accepts_input(40, &[1 << STATE_EDITABLE, 0], false));
        assert!(accepts_input(ROLE_TERMINAL, &[0, 0], false));
    }

    #[test]
    fn read_only_text_buttons_and_dead_widgets_are_not_inputs() {
        assert!(!accepts_input(61, &[0, 0], false));
        assert!(!accepts_input(43, &[0, 0], false));
        assert!(!accepts_input(
            79,
            &[1 << STATE_EDITABLE, 1 << (STATE_READ_ONLY - 32)],
            false
        ));
        assert!(!accepts_input(
            ROLE_TERMINAL,
            &[1 << STATE_DEFUNCT, 0],
            false
        ));
    }

    #[test]
    fn terminal_canvas_fallback_does_not_match_other_apps_or_toolbar_buttons() {
        assert!(accepts_input(39, &[1124075520, 0], true));
        assert!(!accepts_input(39, &[1124075520, 0], false));
        assert!(!accepts_input(43, &[1124075520, 0], true));
        assert!(accepts_input(
            ROLE_TERMINAL,
            &[0, 1 << (STATE_READ_ONLY - 32)],
            false
        ));
    }

    #[test]
    fn executable_wrappers_match_desktop_metadata() {
        assert_eq!(
            executable_name(Path::new("/pkg/bin/.ghostty-wrapped")),
            "ghostty"
        );
        assert_eq!(executable_name(Path::new("/usr/bin/foot")), "foot");
    }

    #[test]
    #[ignore = "requires WAYLANDKB_TEST_TERMINAL_PID pointing to a running desktop terminal"]
    fn desktop_terminal_is_recognized_by_installed_metadata() {
        let pid = std::env::var("WAYLANDKB_TEST_TERMINAL_PID")
            .unwrap()
            .parse()
            .unwrap();
        assert!(application_is_terminal(pid));
    }

    #[test]
    fn delayed_blur_of_old_field_does_not_hide_new_focus() {
        let mut focus = FocusState::default();
        let old = ("app".to_string(), "/old".to_string());
        let new = ("app".to_string(), "/new".to_string());
        focus.update(old.clone(), true, true);
        focus.update(new.clone(), true, true);
        assert_eq!(focus.update(old, false, false), None);
        assert_eq!(
            focus.update(new, false, false),
            Some(TextInputVisibility::Hide)
        );
    }
}
