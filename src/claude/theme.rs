//! Claude Code's colour theme (`theme` in its global config), for the colour
//! it draws inline code in: peekme's boxes draw it the same.

use std::path::PathBuf;

use crate::color::Color;

/// Claude Code's colour of inline code (its "permission" colour) in `theme`;
/// "auto" and custom themes get the dark one.
pub fn code_color(theme: Option<&str>) -> Color {
    match theme.unwrap_or("dark") {
        "light" => Color::Rgb(87, 105, 247),
        "light-daltonized" => Color::Rgb(51, 102, 255),
        "dark-daltonized" => Color::Rgb(153, 204, 255),
        "light-ansi" => Color::Palette(4),
        "dark-ansi" => Color::Palette(12),
        _ => Color::Rgb(177, 185, 249),
    }
}

/// The theme set in `$CLAUDE_CONFIG_DIR/.claude.json`, else `~/.claude.json`.
pub fn theme() -> Option<String> {
    let path = match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d).join(".claude.json"),
        None => crate::launch::home()?.join(".claude.json"),
    };
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    v.get("theme")?.as_str().map(str::to_string)
}
