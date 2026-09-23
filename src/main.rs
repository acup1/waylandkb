mod accessibility;
mod backend;
mod blur;
mod control;
mod input_method;
mod layout;
#[cfg(test)]
mod live_tests;
mod system_layout;
mod tray;
mod uinput;
mod visibility;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use backend::{InputBackend, Modifiers, VirtualKeyboardBackend};
use clap::Parser;
use control::{VisibilityCommand, start_socket};
use gtk::gdk::Display;
use gtk::glib::{self, ControlFlow, timeout_add_local, timeout_add_local_once};
use gtk::prelude::*;
use gtk::{Application, ApplicationWindow, Box as GtkBox, Button, CssProvider, Grid, Orientation};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use layout::{Action, Key, Language, LayoutPage, Modifier};

const SHIFT_DOUBLE_TAP: Duration = Duration::from_millis(350);
const KEY_REPEAT_DELAY: Duration = Duration::from_millis(450);
const KEY_REPEAT_INTERVAL: Duration = Duration::from_millis(65);

#[derive(Parser, Debug)]
#[command(version, about = "Split on-screen keyboard layer for Wayland")]
struct Args {
    /// Log automatic visibility decisions without logging any field contents.
    #[arg(long)]
    debug_visibility: bool,
    /// Keep the keyboard visible at startup.
    #[arg(long)]
    always_visible: bool,

    /// Bottom margin in pixels.
    #[arg(long, default_value_t = 10)]
    bottom_margin: i32,

    /// Layer-shell namespace used by compositors for blur/window rules.
    #[arg(long, default_value = "waylandkb")]
    namespace: String,

    /// Unix socket accepting `show`, `hide`, and `toggle` commands.
    #[arg(long, default_value = "/tmp/waylandkb.sock")]
    control_socket: PathBuf,
}

#[derive(Debug)]
struct KeyboardState {
    language: Language,
    page: LayoutPage,
    modifiers: Modifiers,
    last_shift_tap: Option<Instant>,
    held_modifiers: [u8; 4],
    used_modifiers: [bool; 4],
}

impl KeyboardState {
    fn new(language: Language) -> Self {
        Self {
            language,
            page: LayoutPage::Letters,
            modifiers: Modifiers::default(),
            last_shift_tap: None,
            held_modifiers: [0; 4],
            used_modifiers: [false; 4],
        }
    }

    fn modifier_active(&self, modifier: Modifier) -> bool {
        let modifiers = self.effective_modifiers();
        match modifier {
            Modifier::Ctrl => modifiers.ctrl,
            Modifier::Super => modifiers.super_key,
            Modifier::Alt => modifiers.alt,
            Modifier::Shift => modifiers.shift || modifiers.caps_lock,
        }
    }

    fn effective_modifiers(&self) -> Modifiers {
        Modifiers {
            ctrl: self.modifiers.ctrl || self.held_modifiers[Modifier::Ctrl as usize] > 0,
            super_key: self.modifiers.super_key
                || self.held_modifiers[Modifier::Super as usize] > 0,
            alt: self.modifiers.alt || self.held_modifiers[Modifier::Alt as usize] > 0,
            shift: self.modifiers.shift || self.held_modifiers[Modifier::Shift as usize] > 0,
            caps_lock: self.modifiers.caps_lock,
        }
    }

    fn press_modifier(&mut self, modifier: Modifier) {
        let index = modifier as usize;
        if self.held_modifiers[index] == 0 {
            self.used_modifiers[index] = false;
        }
        self.held_modifiers[index] += 1;
    }

    fn release_modifier(&mut self, modifier: Modifier, cancelled: bool, now: Instant) {
        let index = modifier as usize;
        if self.held_modifiers[index] == 0 {
            return;
        }
        self.held_modifiers[index] -= 1;
        if cancelled {
            self.used_modifiers[index] = true;
        }
        if self.held_modifiers[index] == 0 && !self.used_modifiers[index] {
            self.toggle_modifier(modifier, now);
        }
    }

    fn use_modifiers(&mut self) -> Modifiers {
        for (held, used) in self.held_modifiers.iter().zip(&mut self.used_modifiers) {
            if *held > 0 {
                *used = true;
            }
        }
        self.effective_modifiers()
    }

    fn toggle_modifier(&mut self, modifier: Modifier, now: Instant) {
        match modifier {
            Modifier::Ctrl => self.modifiers.ctrl = !self.modifiers.ctrl,
            Modifier::Super => self.modifiers.super_key = !self.modifiers.super_key,
            Modifier::Alt => self.modifiers.alt = !self.modifiers.alt,
            Modifier::Shift if self.modifiers.caps_lock => {
                self.modifiers.caps_lock = false;
                self.modifiers.shift = false;
                self.last_shift_tap = None;
            }
            Modifier::Shift => {
                let is_double_tap = self
                    .last_shift_tap
                    .is_some_and(|last| now.duration_since(last) <= SHIFT_DOUBLE_TAP);
                if is_double_tap {
                    self.modifiers.caps_lock = true;
                    self.modifiers.shift = false;
                    self.last_shift_tap = None;
                } else {
                    self.modifiers.shift = !self.modifiers.shift;
                    self.last_shift_tap = Some(now);
                }
            }
        }
    }

    fn consume_one_shot_modifiers(&mut self) -> bool {
        let changed = self.modifiers.has_one_shot_modifier();
        self.modifiers.ctrl = false;
        self.modifiers.super_key = false;
        self.modifiers.alt = false;
        self.modifiers.shift = false;
        self.last_shift_tap = None;
        changed
    }
}

#[derive(Debug, Default)]
struct KeyPressTracker {
    held: bool,
    repeating: bool,
    modifiers: Modifiers,
    generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct KeyPressFinish {
    emit_tap: bool,
    was_held: bool,
}

#[derive(Clone, Copy)]
enum KeyboardSide {
    Left,
    Right,
}

impl KeyPressTracker {
    fn press(&mut self, modifiers: Modifiers) -> u64 {
        self.generation += 1;
        self.held = true;
        self.repeating = false;
        self.modifiers = modifiers;
        self.generation
    }

    fn begin_repeat(&mut self, generation: u64) -> Option<Modifiers> {
        if !self.held || generation != self.generation {
            return None;
        }
        self.repeating = true;
        Some(self.modifiers)
    }

    fn repeat_modifiers(&self, generation: u64) -> Option<Modifiers> {
        (self.held && self.generation == generation).then_some(self.modifiers)
    }

    fn finish(&mut self) -> KeyPressFinish {
        let result = KeyPressFinish {
            emit_tap: self.held && !self.repeating,
            was_held: self.held,
        };
        self.held = false;
        self.repeating = false;
        result
    }
}

type KeyButtons = Rc<RefCell<Vec<(glib::WeakRef<Button>, Action)>>>;

#[derive(Clone)]
struct KeyboardUi {
    left_window: ApplicationWindow,
    right_window: ApplicationWindow,
    left_panel: GtkBox,
    right_panel: GtkBox,
    state: Rc<RefCell<KeyboardState>>,
    backend: VirtualKeyboardBackend,
    buttons: KeyButtons,
    epoch: Rc<std::cell::Cell<u64>>,
}

#[derive(Clone)]
struct WeakKeyboardUi {
    left_window: glib::WeakRef<ApplicationWindow>,
    right_window: glib::WeakRef<ApplicationWindow>,
    left_panel: glib::WeakRef<GtkBox>,
    right_panel: glib::WeakRef<GtkBox>,
    state: Rc<RefCell<KeyboardState>>,
    backend: VirtualKeyboardBackend,
    buttons: KeyButtons,
    epoch: Rc<std::cell::Cell<u64>>,
}

impl KeyboardUi {
    fn downgrade(&self) -> WeakKeyboardUi {
        WeakKeyboardUi {
            left_window: self.left_window.downgrade(),
            right_window: self.right_window.downgrade(),
            left_panel: self.left_panel.downgrade(),
            right_panel: self.right_panel.downgrade(),
            state: self.state.clone(),
            backend: self.backend.clone(),
            buttons: self.buttons.clone(),
            epoch: self.epoch.clone(),
        }
    }

    fn render(&self) {
        self.cancel_presses();
        self.buttons.borrow_mut().clear();
        clear_panel(&self.left_panel);
        clear_panel(&self.right_panel);

        let state = self.state.borrow();
        for row in state.language.rows(state.page) {
            self.left_panel
                .append(&build_row(row.left, KeyboardSide::Left, self, &state));
            self.right_panel
                .append(&build_row(row.right, KeyboardSide::Right, self, &state));
        }
    }

    fn cancel_presses(&self) {
        self.epoch.set(self.epoch.get() + 1);
        self.state.borrow_mut().held_modifiers = [0; 4];
        if let Err(error) = self.backend.sync_modifiers(Modifiers::default()) {
            eprintln!("{error:#}");
        }
    }

    fn refresh_modifiers(&self) {
        let state = self.state.borrow();
        // Keep widgets and touch sequences alive while fingers are down.
        for (button, action) in self.buttons.borrow().iter() {
            if let Action::Modifier(modifier) = action
                && let Some(button) = button.upgrade()
            {
                button.remove_css_class("latched");
                button.remove_css_class("locked");
                if state.modifier_active(*modifier) {
                    button.add_css_class(
                        if state.modifiers.caps_lock && *modifier == Modifier::Shift {
                            "locked"
                        } else {
                            "latched"
                        },
                    );
                }
                if *modifier == Modifier::Shift {
                    button.set_label(if state.modifiers.caps_lock {
                        "Caps"
                    } else {
                        "Shift"
                    });
                }
            }
        }
        if let Err(error) = self.backend.sync_modifiers(state.effective_modifiers()) {
            eprintln!("{error:#}");
        }
    }

    fn set_visible(&self, visible: bool) {
        if !visible {
            self.cancel_presses();
            self.state.borrow_mut().consume_one_shot_modifiers();
            self.refresh_modifiers();
        }
        for window in [&self.left_window, &self.right_window] {
            if visible {
                window.present();
            } else {
                window.hide();
            }
        }
    }

    fn is_visible(&self) -> bool {
        self.left_window.is_visible() || self.right_window.is_visible()
    }
}

impl WeakKeyboardUi {
    fn upgrade(&self) -> Option<KeyboardUi> {
        Some(KeyboardUi {
            left_window: self.left_window.upgrade()?,
            right_window: self.right_window.upgrade()?,
            left_panel: self.left_panel.upgrade()?,
            right_panel: self.right_panel.upgrade()?,
            state: self.state.clone(),
            backend: self.backend.clone(),
            buttons: self.buttons.clone(),
            epoch: self.epoch.clone(),
        })
    }
}

fn main() {
    let args = Args::parse();
    let app = Application::builder()
        .application_id("dev.waylandkb.Keyboard")
        .build();

    let ui = RefCell::new(None::<KeyboardUi>);
    app.connect_activate(move |app| {
        // Launching again restores the existing keyboard without duplicating
        // windows, focus monitors, sockets, or tray registrations.
        if let Some(ui) = ui.borrow().as_ref() {
            ui.set_visible(true);
            return;
        }
        *ui.borrow_mut() = Some(build_ui(app, &args));
    });
    app.run_with_args(&["waylandkb"]);
}

fn build_ui(app: &Application, args: &Args) -> KeyboardUi {
    install_css();

    let initial_language = system_layout::current_language().unwrap_or_else(|| {
        eprintln!("could not read the system keyboard layout; using English until it is detected");
        Language::English
    });
    let (left_window, left_panel) = build_surface(app, args, Edge::Left);
    let (right_window, right_panel) = build_surface(app, args, Edge::Right);
    let ui = KeyboardUi {
        left_window,
        right_window,
        left_panel,
        right_panel,
        state: Rc::new(RefCell::new(KeyboardState::new(initial_language))),
        backend: VirtualKeyboardBackend::default(),
        buttons: Rc::new(RefCell::new(Vec::new())),
        epoch: Rc::new(std::cell::Cell::new(0)),
    };

    ui.render();
    attach_tray(app, &ui);
    for window in [&ui.left_window, &ui.right_window] {
        let ui = ui.downgrade();
        window.connect_close_request(move |_| {
            if let Some(ui) = ui.upgrade() {
                ui.set_visible(false);
            }
            glib::Propagation::Stop
        });
    }
    attach_control_socket(&ui, &args.control_socket);
    attach_input_method_monitor(&ui, args.debug_visibility);
    attach_layout_monitor(&ui, initial_language);
    ui.set_visible(args.always_visible);
    ui
}

fn attach_tray(app: &Application, ui: &KeyboardUi) {
    let Some(connection) = app.dbus_connection() else {
        eprintln!("tray: session bus unavailable; auto-show and control socket remain available");
        return;
    };
    let ui = ui.downgrade();
    let weak_app = app.downgrade();
    let tray = match tray::Tray::new(&connection, move |action| {
        if let Some(ui) = ui.upgrade() {
            match action {
                tray::TrayAction::Toggle => ui.set_visible(!ui.is_visible()),
                tray::TrayAction::Show => ui.set_visible(true),
                tray::TrayAction::Hide | tray::TrayAction::Quit => ui.set_visible(false),
            }
        }
        if action == tray::TrayAction::Quit
            && let Some(app) = weak_app.upgrade()
        {
            app.quit();
        }
    }) {
        Ok(tray) => tray,
        Err(error) => {
            eprintln!("tray: {error:#}");
            return;
        }
    };
    let tray = RefCell::new(Some(tray));
    app.connect_shutdown(move |_| {
        tray.borrow_mut().take();
    });
}

fn build_surface(app: &Application, args: &Args, side: Edge) -> (ApplicationWindow, GtkBox) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("waylandkb")
        .decorated(false)
        .resizable(false)
        .build();

    window.add_css_class("keyboard-surface");
    window.init_layer_shell();
    window.set_namespace(&args.namespace);
    window.set_layer(Layer::Overlay);
    window.set_anchor(Edge::Bottom, true);
    window.set_anchor(side, true);
    window.set_margin(Edge::Bottom, args.bottom_margin);
    // Pointer clicks reach the buttons, while keyboard focus stays in the app
    // into which the virtual keyboard should inject the selected character.
    window.set_keyboard_mode(KeyboardMode::None);
    blur::attach(&window);

    let panel = GtkBox::new(Orientation::Vertical, 4);
    panel.add_css_class("keyboard-half");
    panel.set_overflow(gtk::Overflow::Hidden);
    window.set_child(Some(&panel));

    (window, panel)
}

fn attach_control_socket(ui: &KeyboardUi, path: &Path) {
    let receiver = match start_socket(path) {
        Ok(receiver) => receiver,
        Err(error) => {
            eprintln!("{error:#}");
            return;
        }
    };

    let ui = ui.clone();
    timeout_add_local(Duration::from_millis(60), move || {
        while let Ok(command) = receiver.try_recv() {
            match command {
                VisibilityCommand::Show => ui.set_visible(true),
                VisibilityCommand::Hide => ui.set_visible(false),
                VisibilityCommand::Toggle => ui.set_visible(!ui.is_visible()),
            }
        }

        ControlFlow::Continue
    });
}

fn attach_layout_monitor(ui: &KeyboardUi, initial: Language) {
    let receiver = system_layout::start_monitor(initial);
    let ui = ui.clone();
    timeout_add_local(Duration::from_millis(100), move || {
        let mut newest = None;
        while let Ok(language) = receiver.try_recv() {
            newest = Some(language);
        }

        if let Some(language) = newest {
            let changed = {
                let mut state = ui.state.borrow_mut();
                let changed = state.language != language;
                state.language = language;
                changed
            };
            if changed {
                ui.render();
            }
        }

        ControlFlow::Continue
    });
}

fn attach_input_method_monitor(ui: &KeyboardUi, debug: bool) {
    let receivers = [
        input_method::start_monitor(),
        accessibility::start_monitor(),
    ];
    let mut visibility = visibility::AutoVisibility::default();
    let ui = ui.clone();
    timeout_add_local(Duration::from_millis(60), move || {
        let now = Instant::now();
        for (source, receiver) in receivers.iter().enumerate() {
            while let Ok(event) = receiver.try_recv() {
                if debug {
                    eprintln!("focus source {}: {event:?}", ["Wayland", "AT-SPI"][source]);
                }
                visibility.update(source, event, now);
            }
        }
        if let Some(visible) = visibility.poll(now) {
            if debug {
                eprintln!("automatic visibility: {visible}");
            }
            ui.set_visible(visible);
        }

        ControlFlow::Continue
    });
}

fn clear_panel(panel: &GtkBox) {
    while let Some(child) = panel.first_child() {
        panel.remove(&child);
    }
}

fn build_row(keys: &[Key], side: KeyboardSide, ui: &KeyboardUi, state: &KeyboardState) -> Grid {
    let grid = Grid::builder()
        .column_spacing(4)
        .row_spacing(4)
        .halign(match side {
            KeyboardSide::Left => gtk::Align::Start,
            KeyboardSide::Right => gtk::Align::End,
        })
        .build();

    let mut column = 0;
    for key in keys {
        let label = match key.action {
            Action::Modifier(Modifier::Shift) if state.modifiers.caps_lock => "Caps",
            Action::SwitchPage(LayoutPage::Letters) if state.language == Language::Russian => "АБВ",
            _ => key.label,
        };
        let button = Button::with_label(label);
        button.set_focusable(false);
        ui.buttons
            .borrow_mut()
            .push((button.downgrade(), key.action));
        button.add_css_class("key");
        if let Action::Modifier(modifier) = key.action
            && state.modifier_active(modifier)
        {
            button.add_css_class(
                if state.modifiers.caps_lock && modifier == Modifier::Shift {
                    "locked"
                } else {
                    "latched"
                },
            );
        }
        button.set_hexpand(false);
        if key.action == Action::Hide {
            button.add_css_class("hide-key");
            button.set_tooltip_text(Some("Скрыть клавиатуру"));
            button.set_halign(gtk::Align::End);
            button.set_valign(gtk::Align::End);
            button.set_size_request(30, 30);
        } else {
            button.set_size_request(44 * key.width, 40);
        }

        attach_key_gesture(&button, key.action, ui);

        grid.attach(&button, column, 0, key.width, 1);
        column += key.width;
    }

    grid
}

fn attach_key_gesture(button: &Button, action: Action, ui: &KeyboardUi) {
    let gesture = gtk::GestureClick::builder().button(1).build();
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    let tracker = Rc::new(RefCell::new(KeyPressTracker::default()));
    let press_epoch = Rc::new(std::cell::Cell::new(0));

    let pressed_tracker = tracker.clone();
    let pressed_ui = ui.downgrade();
    let pressed_button = button.downgrade();
    let pressed_epoch = press_epoch.clone();
    gesture.connect_pressed(move |gesture, _, _, _| {
        let Some(ui) = pressed_ui.upgrade() else {
            return;
        };
        gesture.set_state(gtk::EventSequenceState::Claimed);
        pressed_epoch.set(ui.epoch.get());
        let modifiers = if matches!(action, Action::Input(_)) {
            ui.state.borrow_mut().use_modifiers()
        } else {
            ui.state.borrow().effective_modifiers()
        };
        let generation = pressed_tracker.borrow_mut().press(modifiers);

        if let Action::Modifier(modifier) = action {
            ui.state.borrow_mut().press_modifier(modifier);
            ui.refresh_modifiers();
            return;
        }

        let Action::Input(input_action) = action else {
            return;
        };
        send_input(&ui, &input_action, modifiers);

        let delay_tracker = pressed_tracker.clone();
        let delay_ui = pressed_ui.clone();
        let delay_button = pressed_button.clone();
        let epoch = ui.epoch.get();
        timeout_add_local_once(KEY_REPEAT_DELAY, move || {
            if delay_button.upgrade().is_none() {
                return;
            }
            let Some(_) = delay_tracker.borrow_mut().begin_repeat(generation) else {
                return;
            };
            let Some(ui) = delay_ui.upgrade() else {
                return;
            };
            if ui.epoch.get() != epoch {
                return;
            }
            let modifiers = ui.state.borrow().effective_modifiers();
            send_repeated_input(&ui, action, modifiers);

            let repeat_tracker = delay_tracker.clone();
            let repeat_ui = delay_ui.clone();
            let repeat_button = delay_button.clone();
            timeout_add_local(KEY_REPEAT_INTERVAL, move || {
                if repeat_button.upgrade().is_none() {
                    return ControlFlow::Break;
                }
                let Some(_) = repeat_tracker.borrow().repeat_modifiers(generation) else {
                    return ControlFlow::Break;
                };
                let Some(ui) = repeat_ui.upgrade() else {
                    return ControlFlow::Break;
                };
                if ui.epoch.get() != epoch {
                    return ControlFlow::Break;
                }
                let modifiers = ui.state.borrow().effective_modifiers();
                send_repeated_input(&ui, action, modifiers);
                ControlFlow::Continue
            });
        });
    });

    let released_tracker = tracker.clone();
    let released_ui = ui.downgrade();
    let released_epoch = press_epoch.clone();
    gesture.connect_released(move |_, _, _, _| {
        let finish = released_tracker.borrow_mut().finish();
        let Some(ui) = released_ui.upgrade() else {
            return;
        };
        if ui.epoch.get() != released_epoch.get() {
            return;
        }
        match action {
            Action::Input(_) if finish.was_held => {
                ui.state.borrow_mut().consume_one_shot_modifiers();
                ui.refresh_modifiers();
            }
            Action::Modifier(modifier) if finish.was_held => {
                ui.state
                    .borrow_mut()
                    .release_modifier(modifier, false, Instant::now());
                ui.refresh_modifiers();
            }
            _ if finish.emit_tap => activate_action(&ui, action),
            _ => {}
        }
    });

    // GestureClick::stopped means its double-click time/distance threshold
    // expired, NOT that the finger was lifted. Only cancel ends a sequence.
    let cancelled_tracker = tracker;
    let cancelled_ui = ui.downgrade();
    gesture.connect_cancel(move |_, _| {
        let finish = cancelled_tracker.borrow_mut().finish();
        let Some(ui) = cancelled_ui.upgrade() else {
            return;
        };
        if !finish.was_held || ui.epoch.get() != press_epoch.get() {
            return;
        }
        match action {
            Action::Input(_) => {
                ui.state.borrow_mut().consume_one_shot_modifiers();
            }
            Action::Modifier(modifier) => {
                ui.state
                    .borrow_mut()
                    .release_modifier(modifier, true, Instant::now());
            }
            _ => {}
        }
        ui.refresh_modifiers();
    });

    button.add_controller(gesture);
}

fn activate_action(ui: &KeyboardUi, action: Action) {
    match action {
        Action::Input(action) => {
            let modifiers = ui.state.borrow_mut().use_modifiers();
            send_input(ui, &action, modifiers);
            ui.state.borrow_mut().consume_one_shot_modifiers();
            ui.refresh_modifiers();
        }
        Action::Modifier(modifier) => {
            ui.state
                .borrow_mut()
                .toggle_modifier(modifier, Instant::now());
            ui.refresh_modifiers();
        }
        Action::SwitchPage(page) => {
            ui.state.borrow_mut().page = page;
            ui.render();
        }
        Action::Hide => ui.set_visible(false),
    }
}

fn send_repeated_input(ui: &KeyboardUi, action: Action, modifiers: Modifiers) {
    if let Action::Input(action) = action {
        send_input(ui, &action, modifiers);
    }
}

fn send_input(ui: &KeyboardUi, action: &backend::KeyAction, modifiers: Modifiers) {
    if let Err(error) = ui.backend.send(action, modifiers) {
        eprintln!("{error:#}");
    }
}

fn install_css() {
    let provider = CssProvider::new();
    provider.load_from_data(
        r#"
        window.keyboard-surface,
        window.keyboard-surface.background {
            background-color: transparent;
            background-image: none;
            box-shadow: none;
        }

        .keyboard-half {
            padding: 10px 12px;
            background-color: rgba(18, 20, 24, 0.24);
            background-image: none;
            background-clip: padding-box;
            border-radius: 16px;
            border: 1px solid rgba(255, 255, 255, 0.18);
            box-shadow: none;
        }

        button.key {
            min-width: 44px;
            min-height: 40px;
            padding: 0 8px;
            border-radius: 8px;
            background: rgba(28, 32, 40, 0.35);
            color: #f4f6fa;
            font: 600 16px sans-serif;
            border: 1px solid rgba(255, 255, 255, 0.22);
        }

        button.key:hover {
            background: rgba(70, 80, 96, 0.50);
        }

        button.key:active,
        button.key.latched {
            background: rgba(152, 188, 255, 0.94);
        }

        button.key.locked {
            background: rgba(116, 164, 255, 0.98);
            box-shadow: inset 0 -3px rgba(30, 76, 170, 0.45);
        }

        button.key.hide-key {
            min-width: 28px;
            min-height: 28px;
            padding: 0;
            border-radius: 8px;
            font-size: 14px;
            color: rgba(244, 246, 250, 0.90);
        }
        "#,
    );

    if let Some(display) = Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            // User-installed GTK themes often live in gtk-4.0/gtk.css and
            // override APPLICATION priority, painting an opaque .background.
            // Our selectors affect only this application's keyboard widgets.
            gtk::STYLE_PROVIDER_PRIORITY_USER + 1,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_is_one_shot() {
        let now = Instant::now();
        let mut state = KeyboardState::new(Language::English);
        state.toggle_modifier(Modifier::Shift, now);
        assert!(state.modifiers.shift);
        assert!(state.consume_one_shot_modifiers());
        assert!(!state.modifiers.shift);
    }

    #[test]
    fn double_tap_shift_enables_persistent_caps_lock() {
        let now = Instant::now();
        let mut state = KeyboardState::new(Language::English);
        state.toggle_modifier(Modifier::Shift, now);
        state.toggle_modifier(Modifier::Shift, now + Duration::from_millis(200));
        assert!(state.modifiers.caps_lock);
        assert!(!state.modifiers.shift);
        assert!(!state.consume_one_shot_modifiers());
        assert!(state.modifiers.caps_lock);
    }

    #[test]
    fn pressing_shift_turns_caps_lock_off() {
        let now = Instant::now();
        let mut state = KeyboardState::new(Language::Russian);
        state.toggle_modifier(Modifier::Shift, now);
        state.toggle_modifier(Modifier::Shift, now + Duration::from_millis(100));
        state.toggle_modifier(Modifier::Shift, now + Duration::from_millis(200));
        assert_eq!(state.modifiers, Modifiers::default());
    }

    #[test]
    fn short_press_emits_one_tap() {
        let mut tracker = KeyPressTracker::default();
        tracker.press(Modifiers::default());
        let finish = tracker.finish();
        assert!(finish.emit_tap);
        assert!(finish.was_held);
        assert!(tracker.repeat_modifiers(tracker.generation).is_none());
    }

    #[test]
    fn long_press_enters_repeat_until_release() {
        let modifiers = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        let mut tracker = KeyPressTracker::default();
        let generation = tracker.press(modifiers);
        assert_eq!(tracker.begin_repeat(generation), Some(modifiers));
        assert_eq!(tracker.repeat_modifiers(generation), Some(modifiers));
        let finish = tracker.finish();
        assert!(!finish.emit_tap);
        assert!(tracker.repeat_modifiers(generation).is_none());
    }

    #[test]
    fn held_ctrl_applies_before_release_and_does_not_latch_after_use() {
        let mut state = KeyboardState::new(Language::English);
        state.press_modifier(Modifier::Ctrl);
        assert!(state.use_modifiers().ctrl);
        state.consume_one_shot_modifiers();
        assert!(state.effective_modifiers().ctrl);
        state.release_modifier(Modifier::Ctrl, false, Instant::now());
        assert!(!state.effective_modifiers().ctrl);
    }

    #[test]
    fn tapped_ctrl_and_shift_accumulate_for_one_chord() {
        let mut state = KeyboardState::new(Language::Russian);
        for modifier in [Modifier::Ctrl, Modifier::Shift] {
            state.press_modifier(modifier);
            state.release_modifier(modifier, false, Instant::now());
        }
        let mods = state.use_modifiers();
        assert!(mods.ctrl && mods.shift);
        state.consume_one_shot_modifiers();
        assert_eq!(state.effective_modifiers(), Modifiers::default());
    }

    #[test]
    fn cancelling_modifier_does_not_latch_it() {
        let mut state = KeyboardState::new(Language::English);
        state.press_modifier(Modifier::Super);
        state.release_modifier(Modifier::Super, true, Instant::now());
        assert!(!state.effective_modifiers().super_key);
    }

    #[test]
    fn releasing_one_of_two_ctrl_buttons_keeps_the_other_held() {
        let mut state = KeyboardState::new(Language::English);
        state.press_modifier(Modifier::Ctrl);
        state.press_modifier(Modifier::Ctrl);
        state.use_modifiers();
        state.release_modifier(Modifier::Ctrl, false, Instant::now());
        assert!(state.effective_modifiers().ctrl);
        state.release_modifier(Modifier::Ctrl, false, Instant::now());
        assert!(!state.effective_modifiers().ctrl);
    }

    #[test]
    fn previous_tap_timer_cannot_repeat_a_new_press() {
        let mut tracker = KeyPressTracker::default();
        let old = tracker.press(Modifiers::default());
        tracker.finish();
        let current = tracker.press(Modifiers::default());
        assert!(tracker.begin_repeat(old).is_none());
        assert!(tracker.begin_repeat(current).is_some());
    }
}
