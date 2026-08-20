//! Themes (FR-39): semantic color palettes selectable via config
//! (`theme = "claude-dark"`), the `--theme` CLI flag, or the command palette.
//!
//! `dark`, `light`, and `high-contrast` use ANSI colors so they inherit the
//! user's terminal scheme; the branded palettes use truecolor RGB.

use std::sync::RwLock;

use ratatui::style::Color;

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Focused borders, brand chip, headings.
    pub accent: Color,
    pub border_dim: Color,
    pub text: Color,
    pub dim: Color,
    /// Selection background (focused / unfocused).
    pub sel_bg: Color,
    pub sel_bg_dim: Color,
    // JSON syntax roles.
    pub key: Color,
    pub string: Color,
    pub number: Color,
    pub keyword: Color, // bool / null
    pub object_id: Color,
    pub date: Color,
    pub marker: Color, // fold arrows
    // Status roles.
    pub ok: Color,
    pub warn: Color,
    pub error: Color,
    /// Text on accent/error chips.
    pub badge_fg: Color,
}

/// Default: ANSI dark (inherits the terminal's own palette).
pub const DARK: Theme = Theme {
    accent: Color::Cyan,
    border_dim: Color::DarkGray,
    text: Color::White,
    dim: Color::DarkGray,
    sel_bg: Color::Blue,
    sel_bg_dim: Color::DarkGray,
    key: Color::Cyan,
    string: Color::Green,
    number: Color::Yellow,
    keyword: Color::Magenta,
    object_id: Color::LightMagenta,
    date: Color::Blue,
    marker: Color::Yellow,
    ok: Color::Green,
    warn: Color::Yellow,
    error: Color::Red,
    badge_fg: Color::Black,
};

/// For light terminal backgrounds: darker foregrounds, warm highlight.
pub const LIGHT: Theme = Theme {
    accent: Color::Blue,
    border_dim: Color::Gray,
    text: Color::Black,
    dim: Color::DarkGray,
    sel_bg: Color::Rgb(205, 225, 250),
    sel_bg_dim: Color::Rgb(226, 226, 226),
    key: Color::Rgb(20, 90, 160),
    string: Color::Rgb(60, 120, 50),
    number: Color::Rgb(160, 105, 0),
    keyword: Color::Rgb(150, 60, 140),
    object_id: Color::Rgb(115, 70, 170),
    date: Color::Rgb(30, 100, 150),
    marker: Color::Rgb(180, 110, 0),
    ok: Color::Rgb(0, 125, 60),
    warn: Color::Rgb(165, 110, 0),
    error: Color::Rgb(190, 40, 40),
    badge_fg: Color::White,
};

/// Warm Anthropic-inspired palette on dark terminals: terracotta accent,
/// ivory text, sage/amber syntax.
pub const CLAUDE_DARK: Theme = Theme {
    accent: Color::Rgb(217, 119, 87),
    border_dim: Color::Rgb(94, 92, 85),
    text: Color::Rgb(240, 238, 229),
    dim: Color::Rgb(146, 143, 132),
    sel_bg: Color::Rgb(72, 56, 47),
    sel_bg_dim: Color::Rgb(52, 51, 47),
    key: Color::Rgb(206, 165, 121),
    string: Color::Rgb(162, 182, 128),
    number: Color::Rgb(230, 185, 111),
    keyword: Color::Rgb(193, 143, 178),
    object_id: Color::Rgb(173, 148, 217),
    date: Color::Rgb(133, 168, 202),
    marker: Color::Rgb(217, 119, 87),
    ok: Color::Rgb(162, 182, 128),
    warn: Color::Rgb(230, 185, 111),
    error: Color::Rgb(214, 106, 106),
    badge_fg: Color::Rgb(25, 25, 21),
};

/// The same warmth for light terminals (ivory background assumed).
pub const CLAUDE_LIGHT: Theme = Theme {
    accent: Color::Rgb(202, 100, 66),
    border_dim: Color::Rgb(190, 187, 178),
    text: Color::Rgb(40, 39, 35),
    dim: Color::Rgb(128, 126, 117),
    sel_bg: Color::Rgb(238, 221, 204),
    sel_bg_dim: Color::Rgb(230, 228, 220),
    key: Color::Rgb(146, 100, 54),
    string: Color::Rgb(92, 122, 62),
    number: Color::Rgb(172, 120, 28),
    keyword: Color::Rgb(150, 90, 140),
    object_id: Color::Rgb(115, 85, 170),
    date: Color::Rgb(60, 110, 160),
    marker: Color::Rgb(202, 100, 66),
    ok: Color::Rgb(70, 130, 80),
    warn: Color::Rgb(180, 125, 30),
    error: Color::Rgb(190, 60, 60),
    badge_fg: Color::Rgb(250, 249, 245),
};

/// Termius-style: deep navy with electric blue/teal accents.
pub const TERMIUS: Theme = Theme {
    accent: Color::Rgb(24, 164, 255),
    border_dim: Color::Rgb(62, 80, 102),
    text: Color::Rgb(220, 230, 242),
    dim: Color::Rgb(122, 142, 166),
    sel_bg: Color::Rgb(30, 58, 92),
    sel_bg_dim: Color::Rgb(38, 48, 64),
    key: Color::Rgb(99, 179, 237),
    string: Color::Rgb(72, 207, 148),
    number: Color::Rgb(255, 203, 107),
    keyword: Color::Rgb(199, 146, 234),
    object_id: Color::Rgb(214, 145, 235),
    date: Color::Rgb(86, 182, 255),
    marker: Color::Rgb(255, 203, 107),
    ok: Color::Rgb(72, 207, 148),
    warn: Color::Rgb(255, 203, 107),
    error: Color::Rgb(255, 99, 99),
    badge_fg: Color::Rgb(10, 20, 35),
};

/// Maximum-legibility ANSI (NFR-9).
pub const HIGH_CONTRAST: Theme = Theme {
    accent: Color::LightCyan,
    border_dim: Color::Gray,
    text: Color::White,
    dim: Color::Gray,
    sel_bg: Color::Blue,
    sel_bg_dim: Color::DarkGray,
    key: Color::LightCyan,
    string: Color::LightGreen,
    number: Color::LightYellow,
    keyword: Color::LightMagenta,
    object_id: Color::LightMagenta,
    date: Color::LightBlue,
    marker: Color::LightYellow,
    ok: Color::LightGreen,
    warn: Color::LightYellow,
    error: Color::LightRed,
    badge_fg: Color::Black,
};

pub const NAMES: [&str; 6] = [
    "dark",
    "light",
    "claude-dark",
    "claude-light",
    "termius",
    "high-contrast",
];

pub fn by_name(name: &str) -> Option<Theme> {
    match name {
        "dark" => Some(DARK),
        "light" => Some(LIGHT),
        "claude-dark" | "claude" => Some(CLAUDE_DARK),
        "claude-light" => Some(CLAUDE_LIGHT),
        "termius" => Some(TERMIUS),
        "high-contrast" => Some(HIGH_CONTRAST),
        _ => None,
    }
}

static CURRENT: RwLock<Theme> = RwLock::new(DARK);

/// Set the active theme by name. Returns false for unknown names.
pub fn set_by_name(name: &str) -> bool {
    match by_name(name) {
        Some(theme) => {
            *CURRENT.write().unwrap() = theme;
            true
        }
        None => false,
    }
}

pub fn current() -> Theme {
    *CURRENT.read().unwrap()
}

// Role accessors used throughout the renderer.
pub fn accent() -> Color {
    current().accent
}
pub fn border_dim() -> Color {
    current().border_dim
}
pub fn text() -> Color {
    current().text
}
pub fn dim() -> Color {
    current().dim
}
pub fn sel_bg() -> Color {
    current().sel_bg
}
pub fn sel_bg_dim() -> Color {
    current().sel_bg_dim
}
pub fn key() -> Color {
    current().key
}
pub fn string() -> Color {
    current().string
}
pub fn number() -> Color {
    current().number
}
pub fn keyword() -> Color {
    current().keyword
}
pub fn object_id() -> Color {
    current().object_id
}
pub fn date() -> Color {
    current().date
}
pub fn marker() -> Color {
    current().marker
}
pub fn ok() -> Color {
    current().ok
}
pub fn warn() -> Color {
    current().warn
}
pub fn error() -> Color {
    current().error
}
pub fn badge_fg() -> Color {
    current().badge_fg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_names_resolve() {
        for name in NAMES {
            assert!(by_name(name).is_some(), "{name}");
        }
        assert!(by_name("claude").is_some(), "alias");
        assert!(by_name("nope").is_none());
    }

    #[test]
    fn set_by_name_switches() {
        assert!(set_by_name("termius"));
        assert!(!set_by_name("bogus"));
        set_by_name("dark");
    }
}
