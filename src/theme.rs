//! Colour palette. Truecolor, tuned to look good on dark terminals while
//! never forcing a background colour (we respect the user's terminal theme).

use ratatui::style::{Color, Modifier, Style};

pub const ACCENT: Color = Color::Rgb(255, 138, 51); // PlatformIO-ish orange
pub const ACCENT2: Color = Color::Rgb(94, 206, 190); // teal
pub const FG: Color = Color::Rgb(222, 224, 235);
pub const MUTED: Color = Color::Rgb(122, 128, 150);
pub const BORDER: Color = Color::Rgb(70, 74, 96);
pub const SEL_BG: Color = Color::Rgb(46, 50, 72);
pub const OK: Color = Color::Rgb(126, 214, 126);
pub const ERR: Color = Color::Rgb(240, 108, 108);
pub const WARN: Color = Color::Rgb(240, 200, 100);
pub const STAR: Color = Color::Rgb(250, 210, 90);

pub const LOGO_GRADIENT: [Color; 6] = [
    Color::Rgb(255, 196, 92),
    Color::Rgb(255, 176, 76),
    Color::Rgb(255, 158, 64),
    Color::Rgb(255, 138, 51),
    Color::Rgb(246, 116, 46),
    Color::Rgb(232, 96, 44),
];

pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn selected() -> Style {
    Style::new().fg(ACCENT).bg(SEL_BG).add_modifier(Modifier::BOLD)
}
