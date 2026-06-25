use std::collections::HashMap;

use gpui::{Global, Hsla, rgb, rgba};

/// A color theme: a palette mapping face names to colors.
///
/// "Faces" are the Emacs unit of styling — named display attributes that both
/// UI chrome (`background`, `cursor`, `region`, `mode-line-bg`, …) and syntax
/// tokens (`comment`, `string`, `keyword`, …) reference. Everything that draws
/// looks colors up by name, so swapping the palette restyles the whole editor.
///
/// The palette itself is defined in Scheme (`scheme/themes/*.scm`, applied via
/// `set-face`); this type only stores and looks up faces, with per-call
/// fallbacks for the brief bootstrap before the runtime loads. Held as a gpui
/// global (`cx.global::<Theme>()`), the same way Zed does theming.
#[derive(Default)]
pub struct Theme {
    faces: HashMap<String, Hsla>,
}

impl Global for Theme {}

impl Theme {
    /// Look up a face color, falling back when the face is undefined.
    pub fn color(&self, name: &str, fallback: Hsla) -> Hsla {
        self.faces.get(name).copied().unwrap_or(fallback)
    }

    pub fn background(&self) -> Hsla {
        self.color("background", rgb(0x1e1e1e).into())
    }

    pub fn foreground(&self) -> Hsla {
        self.color("foreground", rgb(0xd4d4d4).into())
    }

    /// Title-bar background. Defaults to the window background so the custom
    /// title-bar strip blends into the frame; set the `title-bar-bg` face in a
    /// theme to give it its own color.
    pub fn title_bar_bg(&self) -> Hsla {
        self.color("title-bar-bg", self.background())
    }

    /// Title-bar text color. Defaults to the dimmed foreground.
    pub fn title_bar_fg(&self) -> Hsla {
        self.color("title-bar-fg", self.color("dim", self.foreground()))
    }

    /// Set (or override) a face color — drives `set-face` from Steel.
    pub fn set(&mut self, name: impl Into<String>, color: Hsla) {
        self.faces.insert(name.into(), color);
    }
}

/// Parse a `#rrggbb` or `#rrggbbaa` color string into an `Hsla`.
pub fn parse_color(s: &str) -> Option<Hsla> {
    let hex = s.trim().trim_start_matches('#');
    match hex.len() {
        6 => u32::from_str_radix(hex, 16).ok().map(|v| rgb(v).into()),
        8 => u32::from_str_radix(hex, 16).ok().map(|v| rgba(v).into()),
        _ => None,
    }
}
