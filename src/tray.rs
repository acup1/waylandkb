//! StatusNotifierItem and DBusMenu on GTK's existing session-bus connection.
//! No GTK3/AppIndicator dependency or compositor-specific IPC is needed.

use std::collections::HashMap;
use std::rc::Rc;

use anyhow::{Context, Result, ensure};
use gtk::gdk_pixbuf::{InterpType, Pixbuf};
use gtk::gio;
use gtk::glib::{Variant, variant::ObjectPath, variant::ToVariant};

const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";
const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_INTERFACE: &str = "com.canonical.dbusmenu";
const MENU_PATH: &str = "/StatusNotifierItem/Menu";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM_XML: &str = include_str!("tray_item.xml");
const MENU_XML: &str = include_str!("tray_menu.xml");
const AUTO_SHOW_ID: i32 = 5;
const MENU_ITEMS: [i32; 5] = [1, 2, AUTO_SHOW_ID, 3, 4];

type Properties = HashMap<String, Variant>;
type MenuLayout = (i32, Properties, Vec<Variant>);
type MenuEvent = (i32, String, Variant, u32);
type IconPixmaps = Vec<(i32, i32, Vec<u8>)>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayAction {
    Toggle,
    Show,
    Hide,
    Quit,
    ToggleAutoShow,
}

pub struct Tray {
    connection: gio::DBusConnection,
    objects: Vec<gio::RegistrationId>,
    unwatch: Option<Box<dyn FnOnce()>>,
}

impl Tray {
    pub fn new(
        connection: &gio::DBusConnection,
        auto_show_enabled: impl Fn() -> bool + 'static,
        action: impl Fn(TrayAction) + 'static,
    ) -> Result<Self> {
        let icons = keyboard_icons().context("could not load the built-in tray icon")?;
        let mut tray = Self {
            connection: connection.clone(),
            objects: Vec::new(),
            unwatch: None,
        };
        let action = Rc::new(action);
        let activate = action.clone();
        let item_info = gio::DBusNodeInfo::for_xml(ITEM_XML)?
            .lookup_interface(ITEM_INTERFACE)
            .context("missing StatusNotifierItem interface")?;
        tray.objects.push(
            connection
                .register_object(ITEM_PATH, &item_info)
                .property(move |_, _, _, _, name| {
                    item_property(name, &icons).expect("declared tray property")
                })
                .method_call(move |_, _, _, _, method, _, invocation| {
                    invocation.return_value(Some(&().to_variant()));
                    if matches!(method, "Activate" | "SecondaryActivate") {
                        activate(TrayAction::Toggle);
                    }
                    // The host renders our exported Menu on right click. Scroll and
                    // activation tokens do not change keyboard focus or visibility.
                })
                .build()?,
        );
        let menu_info = gio::DBusNodeInfo::for_xml(MENU_XML)?
            .lookup_interface(MENU_INTERFACE)
            .context("missing DBusMenu interface")?;
        tray.objects.push(
            connection
                .register_object(MENU_PATH, &menu_info)
                .property(|_, _, _, _, name| menu_property(name).expect("declared menu property"))
                .method_call(move |connection, _, _, _, method, parameters, invocation| {
                    match menu_request(method, &parameters, auto_show_enabled()) {
                        Ok((reply, actions)) => {
                            invocation.return_value(Some(&reply));
                            for event in actions {
                                action(event);
                                if event == TrayAction::ToggleAutoShow
                                    && let Err(error) = connection.emit_signal(
                                        None,
                                        MENU_PATH,
                                        MENU_INTERFACE,
                                        "ItemsPropertiesUpdated",
                                        Some(&auto_show_update(auto_show_enabled())),
                                    )
                                {
                                    eprintln!(
                                        "tray: could not update auto-show checkmark: {error}"
                                    );
                                }
                            }
                        }
                        Err(message) => invocation
                            .return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", message),
                    }
                })
                .build()?,
        );
        // The panel may start after us or restart while both keyboard windows are hidden.
        let watcher = gio::bus_watch_name_on_connection(
            connection,
            WATCHER,
            gio::BusNameWatcherFlags::NONE,
            |connection, _, owner| {
                connection.call(
                    Some(owner),
                    WATCHER_PATH,
                    WATCHER,
                    "RegisterStatusNotifierItem",
                    Some(&(ITEM_PATH,).to_variant()),
                    None,
                    gio::DBusCallFlags::NONE,
                    5_000,
                    gio::Cancellable::NONE,
                    |result| match result {
                        Ok(_) => eprintln!("tray: registered keyboard icon"),
                        Err(error) => eprintln!("tray: could not register icon: {error}"),
                    },
                );
            },
            |_, _| {
                eprintln!(
                    "tray: no StatusNotifier host; auto-show and control socket remain available"
                );
            },
        );
        // gio 0.20 exposes two different WatcherId types under the same name.
        // Keep the matching unregister operation with the inferred watch ID.
        tray.unwatch = Some(Box::new(move || gio::bus_unwatch_name(watcher)));
        Ok(tray)
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if let Some(unwatch) = self.unwatch.take() {
            unwatch();
        }
        for object in self.objects.drain(..) {
            let _ = self.connection.unregister_object(object);
        }
    }
}

fn item_property(name: &str, icons: &IconPixmaps) -> Option<Variant> {
    let empty_icons: Vec<(i32, i32, Vec<u8>)> = Vec::new();
    Some(match name {
        "Category" => "ApplicationStatus".to_variant(),
        "Id" => "waylandkb".to_variant(),
        "Title" => "Экранная клавиатура".to_variant(),
        // Hidden panels must not make the tray icon disappear.
        "Status" => "Active".to_variant(),
        "WindowId" => 0_i32.to_variant(),
        // Hosts prefer IconName to IconPixmap, even when that theme icon is
        // missing. An empty name explicitly selects our embedded artwork.
        "IconName" => "".to_variant(),
        "IconPixmap" => icons.to_variant(),
        "OverlayIconPixmap" | "AttentionIconPixmap" => empty_icons.to_variant(),
        "IconThemePath" | "OverlayIconName" | "AttentionIconName" | "AttentionMovieName" => {
            "".to_variant()
        }
        "ItemIsMenu" => false.to_variant(),
        "Menu" => ObjectPath::try_from(MENU_PATH).ok()?.to_variant(),
        "ToolTip" => (
            "",
            icons,
            "Экранная клавиатура",
            "Нажмите, чтобы показать или скрыть клавиатуру",
        )
            .to_variant(),
        _ => return None,
    })
}

fn keyboard_icons() -> Result<IconPixmaps> {
    // PNG is embedded so neither a theme nor a runtime SVG loader is required.
    // Keep the editable SVG and its generated PNG together in assets/.
    let source = Pixbuf::from_read(std::io::Cursor::new(include_bytes!(
        "../assets/waylandkb.png"
    )))?;
    [16, 22, 24, 32, 48, 64]
        .into_iter()
        .map(|size| {
            let image = source
                .scale_simple(size, size, InterpType::Hyper)
                .context("could not resize tray icon")?;
            icon_pixmap(&image)
        })
        .collect()
}

fn icon_pixmap(image: &Pixbuf) -> Result<(i32, i32, Vec<u8>)> {
    ensure!(
        image.has_alpha() && image.n_channels() == 4 && image.bits_per_sample() == 8,
        "tray artwork must be RGBA8"
    );
    let (width, height) = (image.width(), image.height());
    let rgba = image.read_pixel_bytes();
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    // GdkPixbuf rows may be padded. SNI requires tightly packed, network-order
    // ARGB rather than RGBA or native-endian Cairo pixels.
    for row in rgba.chunks(image.rowstride() as usize) {
        for pixel in row[..width as usize * 4].chunks_exact(4) {
            pixels.extend_from_slice(&[pixel[3], pixel[0], pixel[1], pixel[2]]);
        }
    }
    Ok((width, height, pixels))
}

fn menu_property(name: &str) -> Option<Variant> {
    Some(match name {
        "Version" => 4_u32.to_variant(),
        "TextDirection" => "ltr".to_variant(),
        "Status" => "normal".to_variant(),
        "IconThemePath" => Vec::<String>::new().to_variant(),
        _ => return None,
    })
}

fn menu_properties(id: i32, names: &[String], auto_show: bool) -> Option<Properties> {
    let mut props = Properties::new();
    match id {
        0 => {
            props.insert("children-display".into(), "submenu".to_variant());
        }
        1 | 2 | 4 | AUTO_SHOW_ID => {
            let label = match id {
                1 => "Показать клавиатуру",
                2 => "Скрыть клавиатуру",
                AUTO_SHOW_ID => "Автоматически показывать клавиатуру",
                _ => "Выход",
            };
            props.insert("label".into(), label.to_variant());
            props.insert("enabled".into(), true.to_variant());
            props.insert("visible".into(), true.to_variant());
            if id == AUTO_SHOW_ID {
                props.insert("toggle-type".into(), "checkmark".to_variant());
                props.insert("toggle-state".into(), i32::from(auto_show).to_variant());
            }
        }
        3 => {
            props.insert("type".into(), "separator".to_variant());
        }
        _ => return None,
    }
    if !names.is_empty() {
        props.retain(|name, _| names.contains(name));
    }
    Some(props)
}

fn menu_layout(id: i32, depth: i32, names: &[String], auto_show: bool) -> Option<MenuLayout> {
    let props = menu_properties(id, names, auto_show)?;
    let children = if id == 0 && depth != 0 {
        MENU_ITEMS
            .into_iter()
            .filter_map(|child| menu_layout(child, 0, names, auto_show))
            .map(|layout| layout.to_variant())
            .collect()
    } else {
        Vec::new()
    };
    Some((id, props, children))
}

fn valid_menu_id(id: i32) -> bool {
    id == 0 || MENU_ITEMS.contains(&id)
}

fn auto_show_update(enabled: bool) -> Variant {
    let props = Properties::from([("toggle-state".into(), i32::from(enabled).to_variant())]);
    (
        vec![(AUTO_SHOW_ID, props)],
        Vec::<(i32, Vec<String>)>::new(),
    )
        .to_variant()
}

fn menu_event(id: i32, event: &str) -> Result<Option<TrayAction>, &'static str> {
    if !valid_menu_id(id) {
        return Err("Unknown menu item");
    }
    Ok(if event == "clicked" {
        match id {
            1 => Some(TrayAction::Show),
            2 => Some(TrayAction::Hide),
            4 => Some(TrayAction::Quit),
            AUTO_SHOW_ID => Some(TrayAction::ToggleAutoShow),
            _ => None,
        }
    } else {
        None
    })
}

fn menu_request(
    method: &str,
    parameters: &Variant,
    auto_show: bool,
) -> Result<(Variant, Vec<TrayAction>), &'static str> {
    let mut actions = Vec::new();
    let reply = match method {
        "GetLayout" => {
            let (id, depth, names) = parameters
                .get::<(i32, i32, Vec<String>)>()
                .ok_or("Invalid layout arguments")?;
            let layout = menu_layout(id, depth, &names, auto_show).ok_or("Unknown menu item")?;
            (1_u32, layout).to_variant()
        }
        "GetGroupProperties" => {
            let (mut ids, names) = parameters
                .get::<(Vec<i32>, Vec<String>)>()
                .ok_or("Invalid property arguments")?;
            if ids.is_empty() {
                ids.push(0);
                ids.extend(MENU_ITEMS);
            }
            let props: Vec<_> = ids
                .into_iter()
                .filter_map(|id| menu_properties(id, &names, auto_show).map(|props| (id, props)))
                .collect();
            (props,).to_variant()
        }
        "GetProperty" => {
            let (id, name) = parameters
                .get::<(i32, String)>()
                .ok_or("Invalid property arguments")?;
            let property = menu_properties(id, &[], auto_show)
                .and_then(|mut props| props.remove(&name))
                .ok_or("Unknown menu item or property")?;
            (property,).to_variant()
        }
        "Event" => {
            let (id, event, _, _) = parameters
                .get::<MenuEvent>()
                .ok_or("Invalid event arguments")?;
            actions.extend(menu_event(id, &event)?);
            ().to_variant()
        }
        "EventGroup" => {
            let (events,) = parameters
                .get::<(Vec<MenuEvent>,)>()
                .ok_or("Invalid event arguments")?;
            let mut errors = Vec::new();
            for (id, event, _, _) in events {
                match menu_event(id, &event) {
                    Ok(action) => actions.extend(action),
                    Err(_) => errors.push(id),
                }
            }
            (errors,).to_variant()
        }
        "AboutToShow" => {
            let (id,) = parameters.get::<(i32,)>().ok_or("Invalid menu arguments")?;
            menu_properties(id, &[], auto_show).ok_or("Unknown menu item")?;
            (false,).to_variant()
        }
        "AboutToShowGroup" => {
            let (ids,) = parameters
                .get::<(Vec<i32>,)>()
                .ok_or("Invalid menu arguments")?;
            let errors: Vec<_> = ids.into_iter().filter(|id| !valid_menu_id(*id)).collect();
            (Vec::<i32>::new(), errors).to_variant()
        }
        _ => return Err("Unknown menu method"),
    };
    Ok((reply, actions))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_properties_match_declared_dbus_types() {
        let icons = keyboard_icons().unwrap();
        for (xml, interface) in [(ITEM_XML, ITEM_INTERFACE), (MENU_XML, MENU_INTERFACE)] {
            let info = gio::DBusNodeInfo::for_xml(xml)
                .unwrap()
                .lookup_interface(interface)
                .unwrap();
            // The static XML declarations put name before type.
            for declaration in xml.split("<property ").skip(1) {
                let attributes: Vec<_> = declaration.split('"').collect();
                let (name, signature) = (attributes[1], attributes[3]);
                assert!(info.lookup_property(name).is_some());
                let property = if interface == ITEM_INTERFACE {
                    item_property(name, &icons)
                } else {
                    menu_property(name)
                };
                assert_eq!(property.unwrap().type_().as_str(), signature, "{name}");
            }
        }
        assert_eq!(
            item_property("Status", &icons).unwrap().str(),
            Some("Active")
        );
        assert!(item_property("Unknown", &icons).is_none());
    }

    #[test]
    fn menu_layout_contains_show_hide_and_quit_as_variant_children() {
        let (reply, actions) = menu_request(
            "GetLayout",
            &(0, -1, Vec::<String>::new()).to_variant(),
            true,
        )
        .unwrap();
        assert_eq!(reply.type_().as_str(), "(u(ia{sv}av))");
        let (_, (_, props, children)) = reply.get::<(u32, MenuLayout)>().unwrap();
        assert_eq!(props["children-display"].str(), Some("submenu"));
        assert_eq!(children.len(), 5);
        for (child, label) in children.iter().zip([
            Some("Показать клавиатуру"),
            Some("Скрыть клавиатуру"),
            Some("Автоматически показывать клавиатуру"),
            None,
            Some("Выход"),
        ]) {
            let (_, props, _) = child.get::<MenuLayout>().unwrap();
            assert_eq!(props.get("label").and_then(Variant::str), label);
        }
        assert!(actions.is_empty());
    }

    #[test]
    fn menu_respects_depth_and_property_filters_and_rejects_unknown_items() {
        let (_, props, children) = menu_layout(0, 0, &[], true).unwrap();
        assert!(!props.is_empty());
        assert!(children.is_empty());
        let (_, props, children) = menu_layout(1, -1, &["label".into()], true).unwrap();
        assert_eq!(props.len(), 1);
        assert!(children.is_empty());
        assert!(menu_layout(99, -1, &[], true).is_none());
        assert!(menu_request("GetLayout", &().to_variant(), true).is_err());
    }

    #[test]
    fn only_clicked_actionable_menu_items_trigger_actions() {
        for (id, expected) in [
            (1, TrayAction::Show),
            (2, TrayAction::Hide),
            (4, TrayAction::Quit),
            (AUTO_SHOW_ID, TrayAction::ToggleAutoShow),
        ] {
            let (reply, actions) = menu_request(
                "Event",
                &(id, "clicked", 0_i32.to_variant(), 0_u32).to_variant(),
                true,
            )
            .unwrap();
            assert_eq!(reply.type_().as_str(), "()");
            assert_eq!(actions, vec![expected]);
        }
        assert_eq!(menu_event(4, "hovered"), Ok(None));
        assert_eq!(menu_event(AUTO_SHOW_ID, "hovered"), Ok(None));
        assert_eq!(menu_event(0, "opened"), Ok(None));
        assert_eq!(menu_event(3, "clicked"), Ok(None));
        assert!(menu_event(99, "clicked").is_err());
    }

    #[test]
    fn grouped_events_report_unknown_ids_without_losing_valid_actions() {
        let events = vec![
            (99, "clicked", 0_i32.to_variant(), 0_u32),
            (2, "clicked", 0_i32.to_variant(), 0_u32),
        ];
        let (reply, actions) = menu_request("EventGroup", &(events,).to_variant(), true).unwrap();
        assert_eq!(reply.get::<(Vec<i32>,)>(), Some((vec![99],)));
        assert_eq!(actions, vec![TrayAction::Hide]);
    }

    #[test]
    fn property_queries_and_open_notifications_do_not_trigger_actions() {
        let (reply, actions) = menu_request(
            "GetGroupProperties",
            &(vec![1, 99, 2], vec!["label"]).to_variant(),
            true,
        )
        .unwrap();
        assert_eq!(reply.type_().as_str(), "(a(ia{sv}))");
        let (props,) = reply.get::<(Vec<(i32, Properties)>,)>().unwrap();
        assert_eq!(props.len(), 2);
        assert_eq!(props[1].0, 2);
        assert_eq!(props[1].1.len(), 1);
        assert!(actions.is_empty());
        let (reply, _) = menu_request("GetProperty", &(4, "label").to_variant(), true).unwrap();
        assert_eq!(reply.type_().as_str(), "(v)");
        assert_eq!(reply.get::<(Variant,)>().unwrap().0.str(), Some("Выход"));
        assert!(menu_request("GetProperty", &(99, "label").to_variant(), true).is_err());
        let (reply, actions) =
            menu_request("AboutToShowGroup", &(vec![0, 1, 99],).to_variant(), true).unwrap();
        assert_eq!(
            reply.get::<(Vec<i32>, Vec<i32>)>(),
            Some((vec![], vec![99]))
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn auto_show_checkmark_matches_state_in_every_property_query() {
        for enabled in [true, false] {
            let (_, props, _) = menu_layout(AUTO_SHOW_ID, 0, &[], enabled).unwrap();
            assert_eq!(props["toggle-type"].str(), Some("checkmark"));
            assert_eq!(props["toggle-state"].get::<i32>(), Some(i32::from(enabled)));
            let (reply, actions) = menu_request(
                "GetProperty",
                &(AUTO_SHOW_ID, "toggle-state").to_variant(),
                enabled,
            )
            .unwrap();
            assert_eq!(
                reply.get::<(Variant,)>().unwrap().0.get::<i32>(),
                Some(i32::from(enabled))
            );
            assert!(actions.is_empty());
            let (reply, _) = menu_request(
                "GetGroupProperties",
                &(Vec::<i32>::new(), vec!["toggle-state"]).to_variant(),
                enabled,
            )
            .unwrap();
            let (items,) = reply.get::<(Vec<(i32, Properties)>,)>().unwrap();
            assert_eq!(
                items.iter().find(|(id, _)| *id == AUTO_SHOW_ID).unwrap().1["toggle-state"]
                    .get::<i32>(),
                Some(i32::from(enabled))
            );
        }
    }

    #[test]
    fn checkmark_update_signal_contains_new_state_and_no_removed_properties() {
        for enabled in [false, true] {
            let signal = auto_show_update(enabled);
            assert_eq!(signal.type_().as_str(), "(a(ia{sv})a(ias))");
            let (updated, removed) = signal
                .get::<(Vec<(i32, Properties)>, Vec<(i32, Vec<String>)>)>()
                .unwrap();
            assert_eq!(updated.len(), 1);
            assert_eq!(updated[0].0, AUTO_SHOW_ID);
            assert_eq!(
                updated[0].1["toggle-state"].get::<i32>(),
                Some(i32::from(enabled))
            );
            assert!(removed.is_empty());
        }
    }

    #[test]
    fn grouped_toggle_events_do_not_drop_other_valid_actions() {
        let events = vec![
            (AUTO_SHOW_ID, "clicked", 0_i32.to_variant(), 0_u32),
            (1, "clicked", 0_i32.to_variant(), 0_u32),
            (AUTO_SHOW_ID, "clicked", 0_i32.to_variant(), 0_u32),
        ];
        let (reply, actions) = menu_request("EventGroup", &(events,).to_variant(), true).unwrap();
        assert_eq!(reply.get::<(Vec<i32>,)>(), Some((vec![],)));
        assert_eq!(
            actions,
            vec![
                TrayAction::ToggleAutoShow,
                TrayAction::Show,
                TrayAction::ToggleAutoShow
            ]
        );
    }

    #[test]
    fn embedded_icon_is_colored_and_transparent_at_every_tray_size() {
        let icons = keyboard_icons().unwrap();
        assert_eq!(
            icons.iter().map(|icon| icon.0).collect::<Vec<_>>(),
            [16, 22, 24, 32, 48, 64]
        );
        for (width, height, pixels) in icons {
            assert_eq!(width, height);
            assert_eq!(pixels.len(), (width * height * 4) as usize);
            assert_eq!(&pixels[..4], &[0, 0, 0, 0]);
            let opaque: Vec<_> = pixels
                .chunks_exact(4)
                .filter(|pixel| pixel[0] == 255)
                .collect();
            assert!(opaque.len() > (width * height / 3) as usize);
            assert!(
                opaque
                    .iter()
                    .any(|pixel| pixel[3] > pixel[1].saturating_add(30))
            );
        }
    }

    #[test]
    fn icon_and_tooltip_select_embedded_pixels_instead_of_theme_lookup() {
        let icons = keyboard_icons().unwrap();
        assert_eq!(item_property("IconName", &icons).unwrap().str(), Some(""));
        assert_eq!(
            item_property("IconPixmap", &icons)
                .unwrap()
                .get::<IconPixmaps>(),
            Some(icons.clone())
        );
        let tooltip = item_property("ToolTip", &icons).unwrap();
        let (name, images, _, _) = tooltip
            .get::<(String, IconPixmaps, String, String)>()
            .unwrap();
        assert!(name.is_empty());
        assert_eq!(images, icons);
    }

    #[test]
    fn argb_conversion_preserves_alpha_and_skips_row_padding() {
        let bytes =
            gtk::glib::Bytes::from_owned(vec![10, 20, 30, 128, 0, 0, 0, 0, 40, 50, 60, 255]);
        let image = Pixbuf::from_bytes(&bytes, gtk::gdk_pixbuf::Colorspace::Rgb, true, 8, 1, 2, 8);
        assert_eq!(
            icon_pixmap(&image).unwrap(),
            (1, 2, vec![128, 10, 20, 30, 255, 40, 50, 60])
        );
        let rgb = Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, 1, 1).unwrap();
        assert!(icon_pixmap(&rgb).is_err());
    }
}
