use std::path::Path;

use ratatui::style::Color;

/// How much color the terminal gets. Detected once at startup.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Truecolor,
    Ansi,
    Mono,
}

/// `NO_COLOR` wins; otherwise `COLORTERM` decides truecolor versus 16 color.
pub fn detect_color_mode() -> ColorMode {
    if std::env::var_os("NO_COLOR").is_some() {
        return ColorMode::Mono;
    }
    match std::env::var("COLORTERM") {
        Ok(v) if v.contains("truecolor") || v.contains("24bit") => ColorMode::Truecolor,
        _ => ColorMode::Ansi,
    }
}

/// A resolved palette: each semantic role is already degraded to the active
/// color mode, so the view asks for a role and never thinks about color depth.
#[derive(Clone, Copy)]
pub struct Theme {
    pub name: &'static str,
    pub text: Color,
    pub dim: Color,
    pub self_: Color,
    pub peer: Color,
    pub success: Color,
    pub warn: Color,
    pub error: Color,
    pub accent: Color,
    pub status_fg: Color,
    pub status_bg: Color,
    pub border: Color,
}

pub const NAMES: &[&str] = &[
    "catppuccin-mocha",
    "gruvbox-dark",
    "solarized-dark",
    "solarized-light",
    "tokyo-night",
];

pub fn default_theme(mode: ColorMode) -> Theme {
    catppuccin_mocha(mode)
}

pub fn by_name(name: &str, mode: ColorMode) -> Option<Theme> {
    match name {
        "catppuccin-mocha" => Some(catppuccin_mocha(mode)),
        "gruvbox-dark" => Some(gruvbox_dark(mode)),
        "solarized-dark" => Some(solarized_dark(mode)),
        "solarized-light" => Some(solarized_light(mode)),
        "tokyo-night" => Some(tokyo_night(mode)),
        _ => None,
    }
}

pub fn load_name(config_dir: &Path) -> Option<String> {
    std::fs::read_to_string(config_dir.join("theme"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn save_name(config_dir: &Path, name: &str) -> std::io::Result<()> {
    std::fs::write(config_dir.join("theme"), name)
}

/// Map an RGB triple to a `Color` for the active mode: the literal RGB on a
/// truecolor terminal, the nearest of the 16 ANSI colors otherwise, or no color
/// at all under `NO_COLOR`.
fn col(mode: ColorMode, r: u8, g: u8, b: u8) -> Color {
    match mode {
        ColorMode::Truecolor => Color::Rgb(r, g, b),
        ColorMode::Ansi => nearest_ansi(r, g, b),
        ColorMode::Mono => Color::Reset,
    }
}

const ANSI16: &[(u8, u8, u8, Color)] = &[
    (0, 0, 0, Color::Black),
    (205, 0, 0, Color::Red),
    (0, 205, 0, Color::Green),
    (205, 205, 0, Color::Yellow),
    (0, 0, 238, Color::Blue),
    (205, 0, 205, Color::Magenta),
    (0, 205, 205, Color::Cyan),
    (229, 229, 229, Color::Gray),
    (127, 127, 127, Color::DarkGray),
    (255, 0, 0, Color::LightRed),
    (0, 255, 0, Color::LightGreen),
    (255, 255, 0, Color::LightYellow),
    (92, 92, 255, Color::LightBlue),
    (255, 0, 255, Color::LightMagenta),
    (0, 255, 255, Color::LightCyan),
    (255, 255, 255, Color::White),
];

fn nearest_ansi(r: u8, g: u8, b: u8) -> Color {
    let mut best = Color::Reset;
    let mut best_dist = i32::MAX;
    for (cr, cg, cb, color) in ANSI16 {
        let dr = r as i32 - *cr as i32;
        let dg = g as i32 - *cg as i32;
        let db = b as i32 - *cb as i32;
        let dist = dr * dr + dg * dg + db * db;
        if dist < best_dist {
            best_dist = dist;
            best = *color;
        }
    }
    best
}

fn catppuccin_mocha(m: ColorMode) -> Theme {
    Theme {
        name: "catppuccin-mocha",
        text: col(m, 0xcd, 0xd6, 0xf4),
        dim: col(m, 0x6c, 0x70, 0x86),
        self_: col(m, 0x89, 0xb4, 0xfa),
        peer: col(m, 0xcb, 0xa6, 0xf7),
        success: col(m, 0xa6, 0xe3, 0xa1),
        warn: col(m, 0xf9, 0xe2, 0xaf),
        error: col(m, 0xf3, 0x8b, 0xa8),
        accent: col(m, 0xcb, 0xa6, 0xf7),
        status_fg: col(m, 0xcd, 0xd6, 0xf4),
        status_bg: col(m, 0x31, 0x32, 0x44),
        border: col(m, 0x45, 0x47, 0x5a),
    }
}

fn gruvbox_dark(m: ColorMode) -> Theme {
    Theme {
        name: "gruvbox-dark",
        text: col(m, 0xeb, 0xdb, 0xb2),
        dim: col(m, 0x92, 0x83, 0x74),
        self_: col(m, 0x83, 0xa5, 0x98),
        peer: col(m, 0xd3, 0x86, 0x9b),
        success: col(m, 0xb8, 0xbb, 0x26),
        warn: col(m, 0xfa, 0xbd, 0x2f),
        error: col(m, 0xfb, 0x49, 0x34),
        accent: col(m, 0xfe, 0x80, 0x19),
        status_fg: col(m, 0xeb, 0xdb, 0xb2),
        status_bg: col(m, 0x3c, 0x38, 0x36),
        border: col(m, 0x50, 0x49, 0x45),
    }
}

fn solarized_dark(m: ColorMode) -> Theme {
    Theme {
        name: "solarized-dark",
        text: col(m, 0x83, 0x94, 0x96),
        dim: col(m, 0x58, 0x6e, 0x75),
        self_: col(m, 0x26, 0x8b, 0xd2),
        peer: col(m, 0x2a, 0xa1, 0x98),
        success: col(m, 0x85, 0x99, 0x00),
        warn: col(m, 0xb5, 0x89, 0x00),
        error: col(m, 0xdc, 0x32, 0x2f),
        accent: col(m, 0x6c, 0x71, 0xc4),
        status_fg: col(m, 0x93, 0xa1, 0xa1),
        status_bg: col(m, 0x07, 0x36, 0x42),
        border: col(m, 0x58, 0x6e, 0x75),
    }
}

fn solarized_light(m: ColorMode) -> Theme {
    Theme {
        name: "solarized-light",
        text: col(m, 0x65, 0x7b, 0x83),
        dim: col(m, 0x93, 0xa1, 0xa1),
        self_: col(m, 0x26, 0x8b, 0xd2),
        peer: col(m, 0x2a, 0xa1, 0x98),
        success: col(m, 0x85, 0x99, 0x00),
        warn: col(m, 0xb5, 0x89, 0x00),
        error: col(m, 0xdc, 0x32, 0x2f),
        accent: col(m, 0x6c, 0x71, 0xc4),
        status_fg: col(m, 0x58, 0x6e, 0x75),
        status_bg: col(m, 0xee, 0xe8, 0xd5),
        border: col(m, 0x93, 0xa1, 0xa1),
    }
}

fn tokyo_night(m: ColorMode) -> Theme {
    Theme {
        name: "tokyo-night",
        text: col(m, 0xc0, 0xca, 0xf5),
        dim: col(m, 0x56, 0x5f, 0x89),
        self_: col(m, 0x7a, 0xa2, 0xf7),
        peer: col(m, 0x7d, 0xcf, 0xff),
        success: col(m, 0x9e, 0xce, 0x6a),
        warn: col(m, 0xe0, 0xaf, 0x68),
        error: col(m, 0xf7, 0x76, 0x8e),
        accent: col(m, 0xbb, 0x9a, 0xf7),
        status_fg: col(m, 0xc0, 0xca, 0xf5),
        status_bg: col(m, 0x24, 0x28, 0x3b),
        border: col(m, 0x3b, 0x42, 0x61),
    }
}
