//! Colour themes for pretty mode.
//!
//! Colours are ROLES, not hues — `accent` is the spinner/cursor/prompt arrow,
//! `brand` the g3 name, `h1..h4` the heading rainbow — so a theme is just a
//! table of RGBs. Chosen once per process from `G3_THEME`:
//!
//!   mocha (default) · catppuccin   Catppuccin Mocha — warm, pink/mauve/peach
//!   tokyo-night · tokyonight · tn  Tokyo Night — what the butler CLI uses
//!
//! Default is Mocha on purpose: Dhanji wants g3 to look distinct from butler.

use std::sync::OnceLock;

pub type Rgb = (u8, u8, u8);

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub name: &'static str,
    pub fg: Rgb,
    pub sub: Rgb,
    pub dim: Rgb,
    pub faint: Rgb,
    pub brand: Rgb,
    pub accent: Rgb,
    pub info: Rgb,
    pub ok: Rgb,
    pub warn: Rgb,
    pub slow: Rgb,
    pub err: Rgb,
    pub h1: Rgb,
    pub h2: Rgb,
    pub h3: Rgb,
    pub h4: Rgb,
    pub code: Rgb,
    pub code_bg: Rgb,
    pub link: Rgb,
}

/// Catppuccin Mocha — https://catppuccin.com/palette (Mocha)
pub const MOCHA: Palette = Palette {
    name: "mocha",
    fg: (205, 214, 244),      // text
    sub: (166, 173, 200),     // subtext0
    dim: (127, 132, 156),     // overlay1
    faint: (88, 91, 112),     // surface2
    brand: (203, 166, 247),   // mauve
    accent: (245, 194, 231),  // pink
    info: (137, 220, 235),    // sky
    ok: (166, 227, 161),      // green
    warn: (249, 226, 175),    // yellow
    slow: (250, 179, 135),    // peach
    err: (243, 139, 168),     // red
    h1: (203, 166, 247),      // mauve
    h2: (250, 179, 135),      // peach
    h3: (249, 226, 175),      // yellow
    h4: (166, 227, 161),      // green
    code: (250, 179, 135),    // peach
    code_bg: (49, 50, 68),    // surface0
    link: (116, 199, 236),    // sapphire
};

/// Tokyo Night — matches the butler terminal client on charm.
pub const TOKYO_NIGHT: Palette = Palette {
    name: "tokyo-night",
    fg: (192, 202, 245),
    sub: (154, 165, 206),
    dim: (86, 95, 137),
    faint: (59, 66, 97),
    brand: (122, 162, 247),
    accent: (187, 154, 247),
    info: (125, 207, 255),
    ok: (158, 206, 106),
    warn: (224, 175, 104),
    slow: (255, 158, 100),
    err: (247, 118, 142),
    h1: (122, 162, 247),
    h2: (187, 154, 247),
    h3: (125, 207, 255),
    h4: (158, 206, 106),
    code: (255, 158, 100),
    code_bg: (41, 46, 66),
    link: (125, 207, 255),
};

pub const THEMES: &[&Palette] = &[&MOCHA, &TOKYO_NIGHT];

/// `G3_THEME` value → palette. Unknown/empty → Mocha (never an error: a typo
/// in an env var must not break the UI).
pub fn palette_for(name: Option<&str>) -> &'static Palette {
    match name.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("tokyo-night") | Some("tokyonight") | Some("tokyo") | Some("tn") => &TOKYO_NIGHT,
        _ => &MOCHA,
    }
}

fn fg(c: Rgb) -> String {
    format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2)
}

fn bg(c: Rgb) -> String {
    format!("\x1b[48;2;{};{};{}m", c.0, c.1, c.2)
}

/// A palette rendered to ready-to-use escape strings, plus the remap table
/// that recolours StreamingMarkdownFormatter's (Catppuccin Macchiato) output.
pub struct Theme {
    pub p: &'static Palette,
    pub fg: String,
    pub sub: String,
    pub dim: String,
    pub faint: String,
    pub brand: String,
    pub accent: String,
    pub info: String,
    pub ok: String,
    pub warn: String,
    pub slow: String,
    pub err: String,
    pub code_bg: String,
    pub bold: &'static str,
    pub italic: &'static str,
    pub strike: &'static str,
    pub reset: &'static str,
    pub remap: Vec<(&'static str, String)>,
}

impl Theme {
    pub fn new(p: &'static Palette) -> Self {
        // Source colours are what streaming_markdown.rs emits; each appears in
        // exactly one construct there (pinned by a test). Headings gain marks.
        let remap = vec![
            ("\x1b[1m\x1b[38;2;198;160;246m", format!("\x1b[1m{}▌ ", fg(p.h1))), // H1 mauve
            ("\x1b[38;2;138;173;244m", format!("\x1b[1m{}▍ ", fg(p.h2))),        // H2 blue
            ("\x1b[38;2;183;189;248m", format!("\x1b[1m{}▏ ", fg(p.h3))),        // H3 lavender
            ("\x1b[38;2;139;213;202m", format!("\x1b[1m{}", fg(p.h4))),          // H4 teal
            ("\x1b[38;2;165;173;203m", fg(p.sub)),                                // H5/6 subtext1
            ("\x1b[38;2;145;215;227m", fg(p.fg)),                                 // italic sky
            ("\x1b[38;2;125;196;228m", fg(p.fg)),                                 // bold sapphire
            ("\x1b[38;2;245;169;127m", format!("{}{}", fg(p.code), bg(p.code_bg))), // inline code peach
            ("\x1b[38;2;166;218;149m", fg(p.link)),                               // links green
            ("\x1b[38;2;128;135;162m", fg(p.dim)),                                // rules overlay1
        ];
        Self {
            p,
            fg: fg(p.fg),
            sub: fg(p.sub),
            dim: fg(p.dim),
            faint: fg(p.faint),
            brand: fg(p.brand),
            accent: fg(p.accent),
            info: fg(p.info),
            ok: fg(p.ok),
            warn: fg(p.warn),
            slow: fg(p.slow),
            err: fg(p.err),
            code_bg: bg(p.code_bg),
            bold: "\x1b[1m",
            italic: "\x1b[3m",
            strike: "\x1b[9m",
            reset: "\x1b[0m",
            remap,
        }
    }
}

static THEME: OnceLock<Theme> = OnceLock::new();

/// The process theme (from `G3_THEME`, read once).
pub fn th() -> &'static Theme {
    THEME.get_or_init(|| Theme::new(palette_for(std::env::var("G3_THEME").ok().as_deref())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_mocha_and_tn_is_selectable() {
        assert_eq!(palette_for(None).name, "mocha");
        assert_eq!(palette_for(Some("")).name, "mocha");
        assert_eq!(palette_for(Some("nonsense")).name, "mocha");
        assert_eq!(palette_for(Some("catppuccin")).name, "mocha");
        assert_eq!(palette_for(Some("Tokyo-Night")).name, "tokyo-night");
        assert_eq!(palette_for(Some("tn")).name, "tokyo-night");
    }

    #[test]
    fn mocha_is_distinguishable_from_butler() {
        // accent drives spinner/cursor/arrow: the most visible colour
        assert_ne!(MOCHA.accent, TOKYO_NIGHT.accent);
        assert_ne!(MOCHA.h2, TOKYO_NIGHT.h2);
        assert_ne!(MOCHA.brand, TOKYO_NIGHT.brand);
    }

    #[test]
    fn remap_targets_follow_the_palette() {
        let m = Theme::new(&MOCHA);
        let t = Theme::new(&TOKYO_NIGHT);
        assert!(m.remap[1].1.contains("250;179;135"), "mocha H2 is peach");
        assert!(t.remap[1].1.contains("187;154;247"), "tokyo H2 is purple");
        assert_eq!(m.remap.iter().map(|r| r.0).collect::<Vec<_>>(), t.remap.iter().map(|r| r.0).collect::<Vec<_>>());
    }
}
