//! Compatibility shims for gpui-ce 0.2.2's palette-based color types.
//!
//! gpui-ce replaced gpui's hand-rolled `Hsla`/`Rgba` with the `palette`
//! crate's `Alpha<Hsl, f32>` / `Alpha<Rgb, f32>`: the `.a` field became
//! `.alpha`, `From<Rgba>` conversions were dropped in favor of explicit
//! `rgb_to_hsla`/`hsla_to_rgba`, and the `.opacity()` builder went away.
//! These shims restore the old call shapes so call sites don't change;
//! see the gpui-ce swap commit for context.

use gpui::Hsla;

/// Hex `0xRRGGBB` → [`Hsla`], replacing `gpui::rgb(..).into()`.
pub fn rgb(hex: u32) -> Hsla {
    gpui::rgb_to_hsla(gpui::rgb(hex))
}

/// Hex `0xRRGGBBAA` → [`Hsla`], replacing `gpui::rgba(..).into()`.
pub fn rgba(hex: u32) -> Hsla {
    gpui::rgb_to_hsla(gpui::rgba(hex))
}
