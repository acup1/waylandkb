//! Opt-in desktop integration tests. Never run as part of ordinary cargo test.
//! Shortcut tests send input only while the dedicated test window has focus.
//! Screenshot tests capture only a generated backdrop and our keyboard panels.

use super::*;
use gtk::gio;

fn pump_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(4);
    while !condition() {
        assert!(Instant::now() < deadline, "desktop event timed out");
        while glib::MainContext::default().pending() {
            glib::MainContext::default().iteration(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn gesture(ui: &KeyboardUi, action: Action) -> gtk::GestureClick {
    let button = ui
        .buttons
        .borrow()
        .iter()
        .find_map(|(button, candidate)| (*candidate == action).then(|| button.upgrade()).flatten())
        .expect("visible key");
    let controllers = button.observe_controllers();
    (0..controllers.n_items())
        .find_map(|i| {
            controllers
                .item(i)?
                .downcast::<gtk::GestureClick>()
                .ok()
                .filter(|gesture| gesture.propagation_phase() == gtk::PropagationPhase::Capture)
        })
        .expect("key gesture")
}

fn press(gesture: &gtk::GestureClick) {
    gesture.emit_by_name::<()>("pressed", &[&1i32, &1.0f64, &1.0f64]);
}

fn release(gesture: &gtk::GestureClick) {
    gesture.emit_by_name::<()>("released", &[&1i32, &1.0f64, &1.0f64]);
}

fn tap(ui: &KeyboardUi, action: Action, receiver: &ApplicationWindow) {
    assert!(
        receiver.is_active(),
        "refusing to inject into another application"
    );
    let key = gesture(ui, action);
    press(&key);
    release(&key);
}

#[test]
#[ignore = "requires a desktop, writable /dev/uinput and explicit WAYLANDKB_LIVE_TEST=1"]
fn desktop_shortcuts_from_ui_reach_application_and_system_layout() {
    assert_eq!(std::env::var("WAYLANDKB_LIVE_TEST").as_deref(), Ok("1"));
    gtk::init().unwrap();
    let app = Application::builder()
        .application_id("dev.waylandkb.ShortcutTest")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    let receiver = ApplicationWindow::builder()
        .application(&app)
        .title("waylandkb — shortcut test")
        .default_width(600)
        .default_height(160)
        .build();
    let entry = gtk::Entry::new();
    receiver.set_child(Some(&entry));
    let received = Rc::new(RefCell::new(Vec::new()));
    let events = received.clone();
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    controller.connect_key_pressed(move |_, key, code, mods| {
        eprintln!("receiver: {key:?}, code={code}, modifiers={mods:?}");
        events.borrow_mut().push((key, code, mods));
        glib::Propagation::Proceed
    });
    receiver.add_controller(controller);
    let shortcut_count = Rc::new(std::cell::Cell::new(0));
    let shortcuts = gtk::ShortcutController::new();
    for trigger in ["<Control>c", "<Control><Shift>c"] {
        let count = shortcut_count.clone();
        shortcuts.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string(trigger),
            Some(gtk::CallbackAction::new(move |_, _| {
                count.set(count.get() + 1);
                glib::Propagation::Stop
            })),
        ));
    }
    shortcuts.set_propagation_phase(gtk::PropagationPhase::Capture);
    receiver.add_controller(shortcuts);
    let initial = system_layout::current_language().expect("read system layout");
    let ui = KeyboardUi {
        left_window: ApplicationWindow::builder().application(&app).build(),
        right_window: ApplicationWindow::builder().application(&app).build(),
        left_panel: GtkBox::new(Orientation::Vertical, 0),
        right_panel: GtkBox::new(Orientation::Vertical, 0),
        state: Rc::new(RefCell::new(KeyboardState::new(initial))),
        backend: VirtualKeyboardBackend::default(),
        buttons: Rc::new(RefCell::new(Vec::new())),
        epoch: Rc::new(std::cell::Cell::new(0)),
    };
    ui.render();
    receiver.present();
    entry.grab_focus();
    pump_until(|| receiver.is_active());
    // Device discovery takes time; the backend was initialized before focus.
    std::thread::sleep(Duration::from_millis(600));

    let c = |language| {
        Action::Input(backend::KeyAction::Text {
            value: if language == Language::Russian {
                "с"
            } else {
                "c"
            },
            physical_key: "c",
            group: u32::from(language == Language::Russian),
        })
    };
    let space = |language| {
        Action::Input(backend::KeyAction::Text {
            value: " ",
            physical_key: "space",
            group: u32::from(language == Language::Russian),
        })
    };
    tap(&ui, Action::Modifier(Modifier::Ctrl), &receiver);
    tap(&ui, c(initial), &receiver);
    pump_until(|| shortcut_count.get() == 1);
    eprintln!("PASS: GTK Ctrl+C action activated through UI and /dev/uinput");

    // Real overlap: Ctrl is not released before Shift and C are pressed.
    let ctrl = gesture(&ui, Action::Modifier(Modifier::Ctrl));
    press(&ctrl);
    // GTK emits this when its click timer expires while the finger is down.
    ctrl.emit_by_name::<()>("stopped", &[]);
    tap(&ui, Action::Modifier(Modifier::Shift), &receiver);
    tap(&ui, c(initial), &receiver);
    release(&ctrl);
    pump_until(|| shortcut_count.get() == 2);
    eprintln!("PASS: held Ctrl + tapped Shift + C activated GTK Ctrl+Shift+C");

    // No compositor-specific switching command: use the user's Super+Space.
    tap(&ui, Action::Modifier(Modifier::Super), &receiver);
    tap(&ui, space(initial), &receiver);
    pump_until(|| system_layout::current_language().is_some_and(|layout| layout != initial));
    let switched = system_layout::current_language().unwrap();
    eprintln!("PASS: Super+Space changed system layout {initial:?} -> {switched:?}");
    ui.state.borrow_mut().language = switched;
    ui.render();
    tap(&ui, Action::Modifier(Modifier::Ctrl), &receiver);
    tap(&ui, c(switched), &receiver);
    // Restore the user's layout before asserting the other-layout shortcut.
    tap(&ui, Action::Modifier(Modifier::Super), &receiver);
    tap(&ui, space(switched), &receiver);
    pump_until(|| system_layout::current_language() == Some(initial));
    pump_until(|| shortcut_count.get() == 3);
    eprintln!("PASS: GTK Ctrl+C also activated with system layout {switched:?}");

    ui.state.borrow_mut().language = Language::Russian;
    ui.render();
    entry.set_text("");
    tap(&ui, c(Language::Russian), &receiver);
    pump_until(|| entry.text() == "с");
    eprintln!("PASS: Russian text still arrives as Cyrillic after shortcuts");
    ui.cancel_presses();
    receiver.close();
}

#[test]
#[ignore = "requires a Wayland desktop; briefly displays a test background and keyboard"]
fn desktop_keyboard_background_is_translucent_and_rounded() {
    assert_eq!(std::env::var("WAYLANDKB_LIVE_TEST").as_deref(), Ok("1"));
    gtk::init().unwrap();
    install_css();
    let app = Application::builder()
        .application_id("dev.waylandkb.VisualTest")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    let background = ApplicationWindow::builder()
        .application(&app)
        .title("waylandkb transparency test")
        .build();
    let pattern = gtk::DrawingArea::new();
    pattern.set_draw_func(|_, cr, width, height| {
        for y in (0..height).step_by(8) {
            for x in (0..width).step_by(8) {
                if (x / 8 + y / 8) % 2 == 0 {
                    cr.set_source_rgb(0.15, 0.35, 0.65);
                } else {
                    cr.set_source_rgb(0.95, 0.65, 0.25);
                }
                cr.rectangle(f64::from(x), f64::from(y), 8.0, 8.0);
                cr.fill().unwrap();
            }
        }
    });
    background.set_child(Some(&pattern));
    background.fullscreen();
    background.present();
    let args = Args::parse_from(["waylandkb", "--namespace", "waylandkb-visual-test"]);
    let (left_window, left_panel) = build_surface(&app, &args, Edge::Left);
    let (right_window, right_panel) = build_surface(&app, &args, Edge::Right);
    let ui = KeyboardUi {
        left_window,
        right_window,
        left_panel,
        right_panel,
        state: Rc::new(RefCell::new(KeyboardState::new(Language::English))),
        backend: VirtualKeyboardBackend::default(),
        buttons: Rc::new(RefCell::new(Vec::new())),
        epoch: Rc::new(std::cell::Cell::new(0)),
    };
    ui.render();
    ui.set_visible(true);
    let started = Instant::now();
    pump_until(|| started.elapsed() >= Duration::from_millis(800));
    let window = &ui.left_window;
    let (width, height) = (window.width(), window.height());
    assert!(width > 100 && height > 100);
    let snapshot = gtk::Snapshot::new();
    gtk::WidgetPaintable::new(Some(window)).snapshot(&snapshot, width as f64, height as f64);
    let node = snapshot.to_node().unwrap();
    let texture = window.renderer().unwrap().render_texture(
        &node,
        Some(&gtk::graphene::Rect::new(
            0.0,
            0.0,
            width as f32,
            height as f32,
        )),
    );
    let stride = texture.width() as usize * 4;
    let mut pixels = vec![0u8; stride * texture.height() as usize];
    texture.download(&mut pixels, stride);
    assert_eq!(
        pixels[3], 0,
        "the outer rounded corner must be fully transparent"
    );
    let alpha = pixels[5 * stride + width as usize / 2 * 4 + 3];
    assert!(
        (20..128).contains(&alpha),
        "panel alpha is {alpha}, expected translucent glass"
    );
    eprintln!("PASS: actual GTK render has transparent corner and panel alpha={alpha}/255");
    // Save only the test panel + generated checkerboard, never user content.
    let surface = window.surface().unwrap();
    let monitor = surface
        .display()
        .monitor_at_surface(&surface)
        .unwrap()
        .geometry();
    let area = format!(
        "{},{} {}x{}",
        monitor.x(),
        monitor.y() + monitor.height() - args.bottom_margin - height,
        width,
        height
    );
    let status = std::process::Command::new("grim")
        .args(["-g", &area, "/tmp/waylandkb-blur-test.png"])
        .status();
    eprintln!("test screenshot: {status:?}, area={area}");
    // Exercise the hide/show lifecycle; no duplicate effect or dead surface ID.
    ui.set_visible(false);
    ui.set_visible(true);
    let started = Instant::now();
    pump_until(|| started.elapsed() >= Duration::from_millis(200));
    ui.left_window.close();
    ui.right_window.close();
    background.close();
}

#[test]
#[ignore = "requires WAYLANDKB_LIVE_TEST=1 and grim; displays a generated backdrop and keyboard"]
fn desktop_readme_screenshots() {
    assert_eq!(std::env::var("WAYLANDKB_LIVE_TEST").as_deref(), Ok("1"));
    gtk::init().unwrap();
    install_css();
    let app = Application::builder()
        .application_id("dev.waylandkb.ReadmeScreenshots")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    let background = ApplicationWindow::builder()
        .application(&app)
        .title("waylandkb screenshot backdrop")
        .decorated(false)
        .default_height(320)
        .build();
    background.init_layer_shell();
    background.set_namespace("waylandkb-screenshot-background");
    background.set_layer(Layer::Overlay);
    background.set_keyboard_mode(KeyboardMode::None);
    for edge in [Edge::Bottom, Edge::Left, Edge::Right] {
        background.set_anchor(edge, true);
    }
    let scene = gtk::DrawingArea::new();
    scene.set_content_height(320);
    scene.set_draw_func(|_, cr, width, height| {
        let (width, height) = (f64::from(width), f64::from(height));
        let gradient = gtk::cairo::LinearGradient::new(0.0, 0.0, width, height);
        gradient.add_color_stop_rgb(0.0, 0.035, 0.11, 0.20);
        gradient.add_color_stop_rgb(0.5, 0.06, 0.08, 0.15);
        gradient.add_color_stop_rgb(1.0, 0.17, 0.08, 0.25);
        cr.set_source(&gradient).unwrap();
        cr.paint().unwrap();
        for (x, red, green, blue) in [(0.12, 0.12, 0.53, 0.8), (0.9, 0.48, 0.2, 0.78)] {
            let glow =
                gtk::cairo::RadialGradient::new(width * x, height, 0.0, width * x, height, 420.0);
            glow.add_color_stop_rgba(0.0, red, green, blue, 0.45);
            glow.add_color_stop_rgba(1.0, red, green, blue, 0.0);
            cr.set_source(&glow).unwrap();
            cr.paint().unwrap();
        }
        cr.set_source_rgba(0.68, 0.79, 1.0, 0.13);
        cr.set_line_width(1.5);
        for offset in [0.0, 28.0, 56.0] {
            cr.move_to(0.0, height * 0.5 + offset);
            cr.curve_to(
                width * 0.3,
                -height + offset,
                width * 0.65,
                height * 2.0 + offset,
                width,
                height * 0.25 + offset,
            );
            cr.stroke().unwrap();
        }
    });
    background.set_child(Some(&scene));
    background.present();
    pump_until(|| background.is_mapped() && background.width() > 100);
    let args = Args::parse_from(["waylandkb", "--namespace", "waylandkb"]);
    let (left_window, left_panel) = build_surface(&app, &args, Edge::Left);
    let (right_window, right_panel) = build_surface(&app, &args, Edge::Right);
    let monitor = background
        .surface()
        .unwrap()
        .display()
        .monitor_at_surface(&background.surface().unwrap())
        .unwrap();
    left_window.set_monitor(&monitor);
    right_window.set_monitor(&monitor);
    let ui = KeyboardUi {
        left_window,
        right_window,
        left_panel,
        right_panel,
        state: Rc::new(RefCell::new(KeyboardState::new(Language::English))),
        backend: VirtualKeyboardBackend::default(),
        buttons: Rc::new(RefCell::new(Vec::new())),
        epoch: Rc::new(std::cell::Cell::new(0)),
    };
    let output = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/images");
    std::fs::create_dir_all(&output).unwrap();
    for (language, page, filename) in [
        (Language::English, LayoutPage::Letters, "keyboard-en.png"),
        (Language::Russian, LayoutPage::Letters, "keyboard-ru.png"),
        (
            Language::English,
            LayoutPage::Symbols,
            "keyboard-symbols.png",
        ),
        (
            Language::English,
            LayoutPage::Functions,
            "keyboard-functions.png",
        ),
    ] {
        {
            let mut state = ui.state.borrow_mut();
            state.language = language;
            state.page = page;
        }
        ui.render();
        ui.set_visible(true);
        let started = Instant::now();
        pump_until(|| started.elapsed() >= Duration::from_millis(700));
        let geometry = monitor.geometry();
        assert_eq!(background.width(), geometry.width());
        assert!(
            ui.left_window.height().max(ui.right_window.height()) + args.bottom_margin
                < background.height()
        );
        assert!(ui.left_window.width() + ui.right_window.width() < background.width());
        // Crop strictly inside the opaque generated backdrop. Do not capture
        // the desktop, tray, notifications, cursor, or other user windows.
        let area = format!(
            "{},{} {}x{}",
            geometry.x(),
            geometry.y() + geometry.height() - background.height(),
            background.width(),
            background.height()
        );
        let status = std::process::Command::new("grim")
            .args(["-g", &area, "-s", "1"])
            .arg(output.join(filename))
            .status()
            .expect("grim is required for README screenshots");
        assert!(status.success(), "screenshot failed: {filename}");
        eprintln!("saved {}", output.join(filename).display());
    }
    ui.set_visible(false);
    ui.left_window.close();
    ui.right_window.close();
    background.close();
}
