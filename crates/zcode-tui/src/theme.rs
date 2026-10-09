//! Paletas de la TUI. Tres temas dark probados; Ctrl+T cicla entre ellos
//! y la elección persiste en ~/.config/zcode-tui/theme.txt.

use ratatui::style::Color;

#[derive(Clone, Copy)]
pub struct Theme {
    pub name: &'static str,
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
    pub warning: Color,
    pub link: Color,
    pub code_bg: Color,
    pub diff_add: Color,
    pub diff_del: Color,
    pub diff_ctx: Color,
}

pub const DARK: Theme = Theme {
    name: "dark",
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
    warning: Color::Rgb(255, 200, 110),
    link: Color::Rgb(120, 170, 255),
    code_bg: Color::Rgb(28, 32, 42),
    diff_add: Color::Rgb(120, 220, 140),
    diff_del: Color::Rgb(240, 120, 120),
    diff_ctx: Color::Rgb(90, 98, 115),
};

pub const TOKYO_NIGHT: Theme = Theme {
    name: "tokyo-night",
    bg: Color::Rgb(26, 27, 38),
    fg: Color::Rgb(192, 202, 245),
    border: Color::Rgb(65, 72, 104),
    border_active: Color::Rgb(125, 207, 255),
    user: Color::Rgb(125, 207, 255),
    assistant: Color::Rgb(197, 205, 245),
    tool: Color::Rgb(187, 154, 247),
    accent: Color::Rgb(115, 218, 202),
    muted: Color::Rgb(86, 95, 137),
    error: Color::Rgb(247, 118, 142),
    warning: Color::Rgb(224, 175, 104),
    link: Color::Rgb(125, 207, 255),
    code_bg: Color::Rgb(31, 35, 53),
    diff_add: Color::Rgb(158, 230, 187),
    diff_del: Color::Rgb(247, 118, 142),
    diff_ctx: Color::Rgb(86, 95, 137),
};

pub const GRUVBOX: Theme = Theme {
    name: "gruvbox",
    bg: Color::Rgb(40, 40, 40),
    fg: Color::Rgb(235, 219, 178),
    border: Color::Rgb(124, 111, 100),
    border_active: Color::Rgb(250, 189, 47),
    user: Color::Rgb(131, 165, 152),
    assistant: Color::Rgb(235, 219, 178),
    tool: Color::Rgb(211, 134, 155),
    accent: Color::Rgb(184, 187, 38),
    muted: Color::Rgb(146, 131, 116),
    error: Color::Rgb(251, 73, 52),
    warning: Color::Rgb(250, 189, 47),
    link: Color::Rgb(131, 165, 152),
    code_bg: Color::Rgb(50, 48, 47),
    diff_add: Color::Rgb(184, 187, 38),
    diff_del: Color::Rgb(251, 73, 52),
    diff_ctx: Color::Rgb(146, 131, 116),
};

pub const THEMES: [Theme; 3] = [DARK, TOKYO_NIGHT, GRUVBOX];

pub fn next(current: Theme) -> Theme {
    let idx = THEMES
        .iter()
        .position(|t| t.name == current.name)
        .map(|i| (i + 1) % THEMES.len())
        .unwrap_or(0);
    THEMES[idx]
}

/// Ruta del archivo que recuerda el tema elegido.
fn theme_file_path() -> std::path::PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            std::path::PathBuf::from(home).join(".config")
        });
    base.join("zcode-tui/theme.txt")
}

pub fn load() -> Theme {
    let saved = std::fs::read_to_string(theme_file_path()).unwrap_or_default();
    THEMES
        .iter()
        .find(|t| t.name == saved.trim())
        .copied()
        .unwrap_or(DARK)
}

pub fn save(theme: Theme) {
    let path = theme_file_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, theme.name);
}
