//! Paleta del tema. Identidad propia inspirada en terminales dark modernas.

use ratatui::style::Color;

#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub border: Color,
    pub border_active: Color,
    pub user: Color,
    pub assistant: Color,
    pub tool: Color,
    pub accent: Color,
    pub muted: Color,
    pub error: Color,
    pub code_bg: Color,
    pub diff_add: Color,
    pub diff_del: Color,
}

pub const DARK: Theme = Theme {
    bg: Color::Rgb(16, 18, 24),
    fg: Color::Rgb(220, 224, 232),
    border: Color::Rgb(60, 66, 82),
    border_active: Color::Rgb(120, 170, 255),
    user: Color::Rgb(130, 200, 255),
    assistant: Color::Rgb(235, 235, 240),
    tool: Color::Rgb(190, 160, 255),
    accent: Color::Rgb(110, 231, 183),
    muted: Color::Rgb(110, 118, 135),
    error: Color::Rgb(255, 110, 110),
    code_bg: Color::Rgb(28, 32, 42),
    diff_add: Color::Rgb(120, 220, 140),
    diff_del: Color::Rgb(240, 120, 120),
};
