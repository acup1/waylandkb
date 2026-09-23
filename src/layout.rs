use crate::backend::KeyAction;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Language {
    English,
    Russian,
}

impl Language {
    pub fn from_layout_name(name: &str) -> Option<Self> {
        let name = name.trim().to_lowercase();
        if name == "ru" || name.starts_with("ru ") || name.contains("russian") {
            Some(Self::Russian)
        } else if matches!(name.as_str(), "us" | "en" | "gb")
            || name.starts_with("en ")
            || name.contains("english")
        {
            Some(Self::English)
        } else {
            None
        }
    }

    pub const fn rows(self, page: LayoutPage) -> &'static [SplitRow] {
        match page {
            LayoutPage::Letters => match self {
                Self::English => ENGLISH_ROWS,
                Self::Russian => RUSSIAN_ROWS,
            },
            LayoutPage::Symbols => SYMBOL_ROWS,
            LayoutPage::Functions => FUNCTION_ROWS,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LayoutPage {
    #[default]
    Letters,
    Symbols,
    Functions,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Modifier {
    Ctrl,
    Super,
    Alt,
    Shift,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Input(KeyAction),
    Modifier(Modifier),
    SwitchPage(LayoutPage),
    Hide,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Key {
    pub label: &'static str,
    pub action: Action,
    pub width: i32,
}

#[derive(Clone, Copy, Debug)]
pub struct SplitRow {
    pub left: &'static [Key],
    pub right: &'static [Key],
}

const fn text(label: &'static str, physical_key: &'static str) -> Key {
    text_value(label, label, physical_key, 0, 1)
}

const fn russian_text(label: &'static str, physical_key: &'static str) -> Key {
    text_value(label, label, physical_key, 1, 1)
}

const fn text_value(
    label: &'static str,
    value: &'static str,
    physical_key: &'static str,
    group: u32,
    width: i32,
) -> Key {
    Key {
        label,
        action: Action::Input(KeyAction::Text {
            value,
            physical_key,
            group,
        }),
        width,
    }
}

const fn symbol(label: &'static str, physical_key: &'static str, shifted: bool) -> Key {
    Key {
        label,
        action: Action::Input(KeyAction::Symbol {
            value: label,
            physical_key,
            shifted,
        }),
        width: 1,
    }
}

const fn input_key(label: &'static str, name: &'static str, width: i32) -> Key {
    Key {
        label,
        action: Action::Input(KeyAction::Key(name)),
        width,
    }
}

const fn modifier(label: &'static str, modifier: Modifier) -> Key {
    Key {
        label,
        action: Action::Modifier(modifier),
        width: 1,
    }
}

const fn switch_page(label: &'static str, page: LayoutPage) -> Key {
    Key {
        label,
        action: Action::SwitchPage(page),
        width: 1,
    }
}

const fn hide() -> Key {
    Key {
        label: "⌄",
        action: Action::Hide,
        width: 1,
    }
}

const ENGLISH_BOTTOM_ROW: SplitRow = SplitRow {
    left: &[
        switch_page("?123", LayoutPage::Symbols),
        switch_page("Fn", LayoutPage::Functions),
        modifier("Ctrl", Modifier::Ctrl),
        modifier("Super", Modifier::Super),
        modifier("Alt", Modifier::Alt),
        text_value("space", " ", "space", 0, 2),
    ],
    right: &[
        text_value("space", " ", "space", 0, 2),
        modifier("Shift", Modifier::Shift),
        modifier("Alt", Modifier::Alt),
        modifier("Super", Modifier::Super),
        modifier("Ctrl", Modifier::Ctrl),
        hide(),
    ],
};

const RUSSIAN_BOTTOM_ROW: SplitRow = SplitRow {
    left: &[
        switch_page("?123", LayoutPage::Symbols),
        switch_page("Fn", LayoutPage::Functions),
        modifier("Ctrl", Modifier::Ctrl),
        modifier("Super", Modifier::Super),
        modifier("Alt", Modifier::Alt),
        text_value("пробел", " ", "space", 1, 2),
    ],
    right: &[
        text_value("пробел", " ", "space", 1, 2),
        modifier("Shift", Modifier::Shift),
        modifier("Alt", Modifier::Alt),
        modifier("Super", Modifier::Super),
        modifier("Ctrl", Modifier::Ctrl),
        hide(),
    ],
};

const SYMBOL_BOTTOM_ROW: SplitRow = SplitRow {
    left: &[
        switch_page("ABC", LayoutPage::Letters),
        switch_page("Fn", LayoutPage::Functions),
        modifier("Ctrl", Modifier::Ctrl),
        modifier("Super", Modifier::Super),
        modifier("Alt", Modifier::Alt),
        text_value("space", " ", "space", 0, 2),
    ],
    right: &[
        text_value("space", " ", "space", 0, 2),
        modifier("Shift", Modifier::Shift),
        modifier("Alt", Modifier::Alt),
        modifier("Super", Modifier::Super),
        modifier("Ctrl", Modifier::Ctrl),
        hide(),
    ],
};

const ENGLISH_ROWS: &[SplitRow] = &[
    SplitRow {
        left: &[
            text("q", "q"),
            text("w", "w"),
            text("e", "e"),
            text("r", "r"),
            text("t", "t"),
        ],
        right: &[
            text("y", "y"),
            text("u", "u"),
            text("i", "i"),
            text("o", "o"),
            text("p", "p"),
        ],
    },
    SplitRow {
        left: &[
            text("a", "a"),
            text("s", "s"),
            text("d", "d"),
            text("f", "f"),
            text("g", "g"),
        ],
        right: &[
            text("h", "h"),
            text("j", "j"),
            text("k", "k"),
            text("l", "l"),
            input_key("⏎", "Return", 1),
        ],
    },
    SplitRow {
        left: &[
            modifier("Shift", Modifier::Shift),
            text("z", "z"),
            text("x", "x"),
            text("c", "c"),
            text("v", "v"),
        ],
        right: &[
            text("b", "b"),
            text("n", "n"),
            text("m", "m"),
            input_key("⌫", "BackSpace", 2),
        ],
    },
    ENGLISH_BOTTOM_ROW,
];

const RUSSIAN_ROWS: &[SplitRow] = &[
    SplitRow {
        left: &[
            russian_text("й", "q"),
            russian_text("ц", "w"),
            russian_text("у", "e"),
            russian_text("к", "r"),
            russian_text("е", "t"),
            russian_text("н", "y"),
        ],
        right: &[
            russian_text("г", "u"),
            russian_text("ш", "i"),
            russian_text("щ", "o"),
            russian_text("з", "p"),
            russian_text("х", "bracketleft"),
            russian_text("ъ", "bracketright"),
        ],
    },
    SplitRow {
        left: &[
            russian_text("ф", "a"),
            russian_text("ы", "s"),
            russian_text("в", "d"),
            russian_text("а", "f"),
            russian_text("п", "g"),
            russian_text("р", "h"),
        ],
        right: &[
            russian_text("о", "j"),
            russian_text("л", "k"),
            russian_text("д", "l"),
            russian_text("ж", "semicolon"),
            russian_text("э", "apostrophe"),
            input_key("⏎", "Return", 1),
        ],
    },
    SplitRow {
        left: &[
            modifier("Shift", Modifier::Shift),
            russian_text("я", "z"),
            russian_text("ч", "x"),
            russian_text("с", "c"),
            russian_text("м", "v"),
            russian_text("и", "b"),
        ],
        right: &[
            russian_text("т", "n"),
            russian_text("ь", "m"),
            russian_text("б", "comma"),
            russian_text("ю", "period"),
            input_key("⌫", "BackSpace", 2),
        ],
    },
    RUSSIAN_BOTTOM_ROW,
];

const SYMBOL_ROWS: &[SplitRow] = &[
    SplitRow {
        left: &[
            symbol("1", "1", false),
            symbol("2", "2", false),
            symbol("3", "3", false),
            symbol("4", "4", false),
            symbol("5", "5", false),
        ],
        right: &[
            symbol("6", "6", false),
            symbol("7", "7", false),
            symbol("8", "8", false),
            symbol("9", "9", false),
            symbol("0", "0", false),
        ],
    },
    SplitRow {
        left: &[
            symbol("@", "2", true),
            symbol("#", "3", true),
            symbol("$", "4", true),
            symbol("%", "5", true),
            symbol("&", "7", true),
        ],
        right: &[
            symbol("-", "minus", false),
            symbol("+", "equal", true),
            symbol("(", "9", true),
            symbol(")", "0", true),
            input_key("⌫", "BackSpace", 1),
        ],
    },
    SplitRow {
        left: &[
            symbol(",", "comma", false),
            symbol(".", "period", false),
            symbol(":", "semicolon", true),
            symbol(";", "semicolon", false),
            symbol("/", "slash", false),
        ],
        right: &[
            symbol("!", "1", true),
            symbol("?", "slash", true),
            symbol("'", "apostrophe", false),
            symbol("\"", "apostrophe", true),
            input_key("⏎", "Return", 1),
        ],
    },
    SYMBOL_BOTTOM_ROW,
];

const FUNCTION_ROWS: &[SplitRow] = &[
    SplitRow {
        left: &[
            input_key("Esc", "Escape", 1),
            input_key("Tab", "Tab", 1),
            input_key("F1", "F1", 1),
            input_key("F2", "F2", 1),
            input_key("F3", "F3", 1),
        ],
        right: &[
            input_key("F4", "F4", 1),
            input_key("F5", "F5", 1),
            input_key("F6", "F6", 1),
            input_key("F7", "F7", 1),
            input_key("F8", "F8", 1),
        ],
    },
    SplitRow {
        left: &[
            input_key("F9", "F9", 1),
            input_key("F10", "F10", 1),
            input_key("F11", "F11", 1),
            input_key("F12", "F12", 1),
            input_key("Ins", "Insert", 1),
        ],
        right: &[
            input_key("Home", "Home", 1),
            input_key("↑", "Up", 1),
            input_key("End", "End", 1),
            input_key("PgUp", "PageUp", 1),
            input_key("Del", "Delete", 1),
        ],
    },
    SplitRow {
        left: &[
            symbol("`", "grave", false),
            symbol("\\", "backslash", false),
            symbol("[", "bracketleft", false),
            symbol("]", "bracketright", false),
            symbol("=", "equal", false),
        ],
        right: &[
            input_key("←", "Left", 1),
            input_key("↓", "Down", 1),
            input_key("→", "Right", 1),
            input_key("PgDn", "PageDown", 1),
            input_key("⏎", "Return", 1),
        ],
    },
    SplitRow {
        left: &[
            switch_page("ABC", LayoutPage::Letters),
            switch_page("?123", LayoutPage::Symbols),
            modifier("Ctrl", Modifier::Ctrl),
            modifier("Super", Modifier::Super),
            modifier("Alt", Modifier::Alt),
            text_value("space", " ", "space", 0, 2),
        ],
        right: &[
            text_value("space", " ", "space", 0, 2),
            modifier("Shift", Modifier::Shift),
            modifier("Alt", Modifier::Alt),
            modifier("Super", Modifier::Super),
            modifier("Ctrl", Modifier::Ctrl),
            hide(),
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn all_keys(language: Language, page: LayoutPage) -> impl Iterator<Item = &'static Key> {
        language
            .rows(page)
            .iter()
            .flat_map(|row| row.left.iter().chain(row.right))
    }

    fn half_width(rows: &[SplitRow], left: bool) -> i32 {
        rows.iter()
            .map(|row| {
                let keys = if left { row.left } else { row.right };
                keys.iter().map(|key| key.width).sum()
            })
            .max()
            .unwrap_or_default()
    }

    #[test]
    fn system_layout_names_are_recognized() {
        assert_eq!(
            Language::from_layout_name("English (US)"),
            Some(Language::English)
        );
        assert_eq!(
            Language::from_layout_name("Russian"),
            Some(Language::Russian)
        );
        assert_eq!(Language::from_layout_name("ru"), Some(Language::Russian));
        assert_eq!(Language::from_layout_name("German"), None);
    }

    #[test]
    fn both_letter_layouts_contain_controls() {
        for language in [Language::English, Language::Russian] {
            let keys: Vec<_> = all_keys(language, LayoutPage::Letters).collect();
            for modifier in [
                Modifier::Ctrl,
                Modifier::Super,
                Modifier::Alt,
                Modifier::Shift,
            ] {
                assert!(
                    keys.iter()
                        .any(|key| key.action == Action::Modifier(modifier))
                );
            }
            assert!(keys.iter().any(|key| key.action == Action::Hide));
            assert!(
                keys.iter()
                    .any(|key| key.action == Action::SwitchPage(LayoutPage::Symbols))
            );
        }
    }

    #[test]
    fn both_halves_have_the_same_maximum_width() {
        for language in [Language::English, Language::Russian] {
            for page in [
                LayoutPage::Letters,
                LayoutPage::Symbols,
                LayoutPage::Functions,
            ] {
                let rows = language.rows(page);
                assert_eq!(half_width(rows, true), half_width(rows, false));
            }
        }
    }

    #[test]
    fn punctuation_is_kept_off_the_letter_page() {
        for language in [Language::English, Language::Russian] {
            assert!(
                !all_keys(language, LayoutPage::Letters)
                    .any(|key| matches!(key.action, Action::Input(KeyAction::Symbol { .. })))
            );
        }
        assert!(
            all_keys(Language::English, LayoutPage::Symbols).any(|key| matches!(
                key.action,
                Action::Input(KeyAction::Symbol { value: "?", .. })
            ))
        );
    }

    #[test]
    fn russian_shortcut_keys_follow_qwerty_positions() {
        assert!(all_keys(Language::Russian, LayoutPage::Letters).any(|key| {
            key.action
                == Action::Input(KeyAction::Text {
                    value: "с",
                    physical_key: "c",
                    group: 1,
                })
        }));
    }
}
