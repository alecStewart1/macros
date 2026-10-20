use std::collections::HashMap;

use gpui::{Global, Hsla};

use crate::color_compat::{rgb, rgba};

/// A color theme: a palette mapping face names to their display attributes.
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
    faces: HashMap<String, Face>,
}

/// One face's attributes. Every attribute is independent: a face may have
/// only a background, or only bold, and each `set-face-*` primitive touches
/// just its own field. `load-theme` resets the faces themes own (see
/// `runtime::theme_face_names`); other faces keep their attributes.
#[derive(Clone, Debug, Default, PartialEq)]
struct Face {
    /// Foreground color (`set-face`). Without one, `color()` falls back to the
    /// caller's default — so a bold-only face is "default foreground, bold".
    fg: Option<Hsla>,
    /// Background color (`set-face-background`), painted behind overlay text
    /// that uses the face; a face with ONLY a background leaves the text color
    /// it lands on unchanged (so it can layer under a foreground face).
    bg: Option<Hsla>,
    /// Font family (`set-face-font`): text under the face shapes with this
    /// family instead of the editor font — the plot faces use the bundled
    /// "Macros Plot" glyph font so chart columns stay on the editor's grid.
    font: Option<String>,
    bold: bool,
    /// `set-face-underline`.
    underline: bool,
    /// `set-face-wavy` (spellcheck squiggles). Kept apart from `underline`
    /// because the two toggle independently: wavy implies an underline, and
    /// turning wavy back off leaves a straight one if that was also set.
    wavy: bool,
}

/// How a face's text is underlined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Underline {
    Straight,
    /// A squiggle (spellcheck).
    Wavy,
}

impl Global for Theme {}

impl Theme {
    /// Drop every face whose name `owned` says belongs to a theme, keeping the
    /// rest (faces a module or the user's config set, e.g. `diff-added`).
    pub fn clear_faces(&mut self, owned: impl Fn(&str) -> bool) {
        self.faces.retain(|name, _| !owned(name));
    }

    fn face(&self, name: &str) -> Option<&Face> {
        self.faces.get(name)
    }

    fn face_mut(&mut self, name: impl Into<String>) -> &mut Face {
        self.faces.entry(name.into()).or_default()
    }

    /// Look up a face color, falling back when the face is undefined.
    pub fn color(&self, name: &str, fallback: Hsla) -> Hsla {
        self.face(name).and_then(|f| f.fg).unwrap_or(fallback)
    }

    pub fn background(&self) -> Hsla {
        self.color("background", rgb(0x1e1e1e))
    }

    pub fn foreground(&self) -> Hsla {
        self.color("foreground", rgb(0xd4d4d4))
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
        self.face_mut(name).fg = Some(color);
    }

    /// Whether `name` has a foreground color of its own (vs. a caller default).
    pub fn has_color(&self, name: &str) -> bool {
        self.face(name).is_some_and(|f| f.fg.is_some())
    }

    pub fn set_background(&mut self, name: impl Into<String>, color: Hsla) {
        self.face_mut(name).bg = Some(color);
    }

    /// The face's background color, if one was set.
    pub fn background_of(&self, name: &str) -> Option<Hsla> {
        self.face(name).and_then(|f| f.bg)
    }

    pub fn set_font(&mut self, name: impl Into<String>, family: impl Into<String>) {
        self.face_mut(name).font = Some(family.into());
    }

    /// The face's font family, if one was set.
    pub fn font_of(&self, name: &str) -> Option<&str> {
        self.face(name).and_then(|f| f.font.as_deref())
    }

    /// Turn a face's underline attribute on or off — drives `set-face-underline`
    /// from Steel.
    pub fn set_underline(&mut self, name: impl Into<String>, on: bool) {
        self.face_mut(name).underline = on;
    }

    /// The underline style for `name`, if it's underlined at all.
    pub fn underline(&self, name: &str) -> Option<Underline> {
        match self.face(name) {
            Some(f) if f.wavy => Some(Underline::Wavy),
            Some(f) if f.underline => Some(Underline::Straight),
            _ => None,
        }
    }

    /// Turn a face's wavy-underline attribute on or off — drives
    /// `set-face-wavy` from Steel.
    pub fn set_wavy(&mut self, name: impl Into<String>, on: bool) {
        self.face_mut(name).wavy = on;
    }

    /// Whether text in `name` should be bold.
    pub fn is_bold(&self, name: &str) -> bool {
        self.face(name).is_some_and(|f| f.bold)
    }

    /// Turn a face's bold attribute on or off — drives `set-face-bold` from
    /// Steel.
    pub fn set_bold(&mut self, name: impl Into<String>, on: bool) {
        self.face_mut(name).bold = on;
    }
}

/// Parse a `#rrggbb` or `#rrggbbaa` color string into an `Hsla`.
pub fn parse_color(s: &str) -> Option<Hsla> {
    let hex = s.trim().trim_start_matches('#');
    match hex.len() {
        6 => u32::from_str_radix(hex, 16).ok().map(rgb),
        8 => u32::from_str_radix(hex, 16).ok().map(rgba),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn face_attributes_are_independent() {
        let mut theme = Theme::default();
        let red: Hsla = rgb(0xff0000);
        let fallback: Hsla = rgb(0x00ff00);
        theme.set_bold("b", true);
        // A bold-only face has no color of its own.
        assert!(theme.is_bold("b") && !theme.has_color("b"));
        assert_eq!(theme.color("b", fallback), fallback);
        theme.set("b", red);
        assert_eq!(theme.color("b", fallback), red);
        assert!(theme.is_bold("b"));
        assert_eq!(theme.background_of("b"), None);
        assert_eq!(theme.font_of("missing"), None);
    }

    #[test]
    fn wavy_and_straight_underlines_toggle_independently() {
        let mut theme = Theme::default();
        assert_eq!(theme.underline("u"), None);
        theme.set_underline("u", true);
        assert_eq!(theme.underline("u"), Some(Underline::Straight));
        theme.set_wavy("u", true);
        assert_eq!(theme.underline("u"), Some(Underline::Wavy));
        // Turning wavy off again leaves the straight underline.
        theme.set_wavy("u", false);
        assert_eq!(theme.underline("u"), Some(Underline::Straight));
        theme.set_underline("u", false);
        assert_eq!(theme.underline("u"), None);
        // Wavy alone still underlines.
        theme.set_wavy("w", true);
        assert_eq!(theme.underline("w"), Some(Underline::Wavy));
    }
}
