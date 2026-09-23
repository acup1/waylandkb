use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use crate::layout::Language;

pub fn current_language() -> Option<Language> {
    if let Some(language) = language_from_override_file() {
        return Some(language);
    }

    let desktop = current_desktop();

    // Prefer the active desktop's native source. Besides being more accurate,
    // this avoids repeatedly starting IPC clients for compositors that merely
    // happen to be installed on the machine.
    if desktop.contains("driftwm")
        && let Some(language) = driftwm_language()
    {
        return Some(language);
    }
    if (desktop.contains("niri") || env::var_os("NIRI_SOCKET").is_some())
        && let Some(language) = niri_language()
    {
        return Some(language);
    }
    if (desktop.contains("hyprland") || env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some())
        && let Some(language) = hyprland_language()
    {
        return Some(language);
    }
    if (desktop.contains("sway") || env::var_os("SWAYSOCK").is_some())
        && let Some(language) = sway_language()
    {
        return Some(language);
    }
    if (desktop.contains("kde") || desktop.contains("plasma"))
        && let Some(language) = kde_language()
    {
        return Some(language);
    }
    if desktop.contains("gnome")
        && let Some(language) = gnome_language()
    {
        return Some(language);
    }

    // Input-method frameworks are compositor-independent and are useful on
    // smaller compositors without a layout IPC API.
    if input_method_is("ibus")
        && let Some(language) = ibus_language()
    {
        return Some(language);
    }
    if input_method_is("fcitx")
        && let Some(language) = fcitx_language()
    {
        return Some(language);
    }

    // Looking for driftwm's state file is cheap and also works when a session
    // launcher did not export XDG_CURRENT_DESKTOP.
    driftwm_language()
        .or_else(x11_language)
        .or_else(language_from_environment)
}

pub fn start_monitor(initial: Language) -> Receiver<Language> {
    let (sender, receiver) = mpsc::channel();

    thread::spawn(move || {
        let mut last = initial;
        loop {
            thread::sleep(Duration::from_millis(400));
            if let Some(language) = current_language() {
                last = language;
            }

            // Sending the last known value also acts as a shutdown heartbeat:
            // once the GTK receiver is gone, this worker exits by itself.
            if sender.send(last).is_err() {
                break;
            }
        }
    });

    receiver
}

fn hyprland_language() -> Option<Language> {
    let output = command_output("hyprctl", &["-j", "devices"])?;
    parse_layout_devices(&output, "active_keymap", true)
}

fn driftwm_language() -> Option<Language> {
    let state_path = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)?
        .join("driftwm/state");
    let state = fs::read_to_string(state_path).ok()?;
    parse_driftwm_state(&state)
}

fn parse_driftwm_state(state: &str) -> Option<Language> {
    state_value(state, "layout_short")
        .and_then(Language::from_layout_name)
        .or_else(|| state_value(state, "layout").and_then(Language::from_layout_name))
}

fn state_value<'a>(state: &'a str, key: &str) -> Option<&'a str> {
    state.lines().find_map(|line| {
        let (candidate, value) = line.split_once('=')?;
        (candidate.trim() == key).then(|| value.trim())
    })
}

fn niri_language() -> Option<Language> {
    let output = command_output("niri", &["msg", "--json", "keyboard-layouts"])?;
    parse_niri_layouts(&output)
}

fn parse_niri_layouts(json: &str) -> Option<Language> {
    let index = json_usize_field(json, "current_idx")?;
    let names = json_string_array_field(json, "names")?;
    names
        .get(index)
        .and_then(|name| Language::from_layout_name(name))
}

fn sway_language() -> Option<Language> {
    let output = command_output("swaymsg", &["-r", "-t", "get_inputs"])?;
    parse_layout_devices(&output, "xkb_active_layout_name", false)
}

fn kde_language() -> Option<Language> {
    ["qdbus6", "qdbus"]
        .into_iter()
        .find_map(|program| {
            command_output(
                program,
                &[
                    "org.kde.keyboard",
                    "/Layouts",
                    "org.kde.KeyboardLayouts.getCurrentLayout",
                ],
            )
        })
        .and_then(|layout| Language::from_layout_name(&layout))
}

fn gnome_language() -> Option<Language> {
    let current = command_output(
        "gsettings",
        &["get", "org.gnome.desktop.input-sources", "current"],
    )
    .and_then(|value| value.split_whitespace().last()?.parse::<usize>().ok());
    let sources = command_output(
        "gsettings",
        &["get", "org.gnome.desktop.input-sources", "sources"],
    )?;
    let layouts = parse_gsettings_sources(&sources);

    current
        .and_then(|index| layouts.get(index))
        .and_then(|layout| Language::from_layout_name(layout))
        .or_else(|| {
            command_output(
                "gsettings",
                &["get", "org.gnome.desktop.input-sources", "mru-sources"],
            )
            .and_then(|value| parse_gsettings_sources(&value).into_iter().next())
            .and_then(|layout| Language::from_layout_name(&layout))
        })
}

fn parse_gsettings_sources(value: &str) -> Vec<String> {
    let quoted: Vec<_> = value
        .split('\'')
        .enumerate()
        .filter(|(index, _)| index % 2 == 1)
        .map(|(_, part)| part.to_owned())
        .collect();

    quoted
        .chunks_exact(2)
        .filter(|pair| pair[0] == "xkb")
        .map(|pair| pair[1].clone())
        .collect()
}

fn ibus_language() -> Option<Language> {
    let engine = command_output("ibus", &["engine"])?;
    let layout = engine
        .trim()
        .strip_prefix("xkb:")
        .and_then(|value| value.split(':').next())
        .unwrap_or(engine.trim());
    Language::from_layout_name(layout)
}

fn fcitx_language() -> Option<Language> {
    let input_method = command_output("fcitx5-remote", &["-n"])?;
    let layout = input_method
        .trim()
        .strip_prefix("keyboard-")
        .unwrap_or(input_method.trim());
    Language::from_layout_name(layout)
}

fn x11_language() -> Option<Language> {
    env::var_os("DISPLAY")?;
    let layout = command_output("xkb-switch", &["-p"])?;
    Language::from_layout_name(&layout)
}

fn command_output(program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(arguments)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn current_desktop() -> String {
    [
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "DESKTOP_SESSION",
    ]
    .into_iter()
    .filter_map(|name| env::var(name).ok())
    .collect::<Vec<_>>()
    .join(":")
    .to_lowercase()
}

fn input_method_is(name: &str) -> bool {
    ["GTK_IM_MODULE", "QT_IM_MODULE", "XMODIFIERS"]
        .into_iter()
        .filter_map(|variable| env::var(variable).ok())
        .any(|value| value.to_lowercase().contains(name))
}

fn language_from_override_file() -> Option<Language> {
    let path = env::var_os("WAYLANDKB_LAYOUT_FILE")?;
    language_from_file(Path::new(&path))
}

fn language_from_file(path: &Path) -> Option<Language> {
    let value = fs::read_to_string(path).ok()?;
    Language::from_layout_name(&value)
}

fn language_from_environment() -> Option<Language> {
    let layout = env::var("XKB_DEFAULT_LAYOUT").ok()?;
    (!layout.contains(','))
        .then(|| Language::from_layout_name(&layout))
        .flatten()
}

fn parse_layout_devices(json: &str, layout_field: &str, prefer_main: bool) -> Option<Language> {
    let marker = format!("\"{layout_field}\"");
    let ranges = json_object_ranges(json);
    let mut fallback = None;

    for (position, _) in json.match_indices(&marker) {
        let object = ranges
            .iter()
            .filter(|(start, end)| *start < position && position < *end)
            .min_by_key(|(start, end)| end - start)
            .map(|(start, end)| &json[*start..=*end])?;
        let language = json_string_field(object, layout_field)
            .and_then(|name| Language::from_layout_name(&name));

        if prefer_main && json_bool_field(object, "main") == Some(true) && language.is_some() {
            return language;
        }
        fallback = fallback.or(language);
    }

    fallback
}

fn json_object_ranges(json: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escaped = false;

    for (index, byte) in json.bytes().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }

        match byte {
            b'"' => in_string = true,
            b'{' => stack.push(index),
            b'}' => {
                if let Some(start) = stack.pop() {
                    ranges.push((start, index));
                }
            }
            _ => {}
        }
    }

    ranges
}

fn json_string_field(object: &str, field: &str) -> Option<String> {
    let marker = format!("\"{field}\"");
    let value = object
        .split_once(&marker)?
        .1
        .split_once(':')?
        .1
        .trim_start();
    let value = value.strip_prefix('"')?;
    let mut result = String::new();
    let mut escaped = false;

    for character in value.chars() {
        if escaped {
            result.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            return Some(result);
        } else {
            result.push(character);
        }
    }

    None
}

fn json_bool_field(object: &str, field: &str) -> Option<bool> {
    let marker = format!("\"{field}\"");
    let value = object
        .split_once(&marker)?
        .1
        .split_once(':')?
        .1
        .trim_start();
    if value.starts_with("true") {
        Some(true)
    } else if value.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

fn json_usize_field(json: &str, field: &str) -> Option<usize> {
    let marker = format!("\"{field}\"");
    json.split_once(&marker)?
        .1
        .split_once(':')?
        .1
        .trim_start()
        .split(|character: char| !character.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

fn json_string_array_field(json: &str, field: &str) -> Option<Vec<String>> {
    let marker = format!("\"{field}\"");
    let value = json.split_once(&marker)?.1.split_once(':')?.1.trim_start();
    let value = value.strip_prefix('[')?;
    let mut strings = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut escaped = false;

    for character in value.chars() {
        if in_string {
            if escaped {
                current.push(character);
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                strings.push(std::mem::take(&mut current));
                in_string = false;
            } else {
                current.push(character);
            }
        } else if character == '"' {
            in_string = true;
        } else if character == ']' {
            return Some(strings);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyprland_prefers_main_keyboard() {
        let json = r#"{
            "keyboards": [
                {"name": "virtual", "active_keymap": "English (US)", "main": false},
                {"name": "built-in", "active_keymap": "Russian", "main": true}
            ]
        }"#;
        assert_eq!(
            parse_layout_devices(json, "active_keymap", true),
            Some(Language::Russian)
        );
    }

    #[test]
    fn sway_layout_is_detected() {
        let json = r#"[
            {"identifier": "keyboard", "xkb_active_layout_name": "English (US)"}
        ]"#;
        assert_eq!(
            parse_layout_devices(json, "xkb_active_layout_name", false),
            Some(Language::English)
        );
    }

    #[test]
    fn braces_inside_strings_do_not_break_object_detection() {
        let json =
            r#"{"keyboards":[{"name":"odd } name", "active_keymap":"Russian", "main":true}]}"#;
        assert_eq!(
            parse_layout_devices(json, "active_keymap", true),
            Some(Language::Russian)
        );
    }

    #[test]
    fn driftwm_prefers_short_layout_name() {
        let state = "layout=Russian\nlayout_short=ru\nworkspace=1\n";
        assert_eq!(parse_driftwm_state(state), Some(Language::Russian));
    }

    #[test]
    fn driftwm_uses_long_layout_name_as_fallback() {
        let state = "layout=English (US)\n";
        assert_eq!(parse_driftwm_state(state), Some(Language::English));
    }

    #[test]
    fn niri_uses_current_layout_index() {
        let json = r#"{"names":["English (US)","Russian"],"current_idx":1}"#;
        assert_eq!(parse_niri_layouts(json), Some(Language::Russian));
    }

    #[test]
    fn gnome_sources_keep_only_xkb_layouts() {
        let value = "[('xkb', 'us'), ('ibus', 'typing-booster'), ('xkb', 'ru')]";
        assert_eq!(parse_gsettings_sources(value), ["us", "ru"]);
    }
}
