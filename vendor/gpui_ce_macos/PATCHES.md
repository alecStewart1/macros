# Vendored gpui_ce_macos 0.1.0

This is the crates.io release of `gpui_ce_macos` 0.1.0 (Apache-2.0, see
`LICENSE-APACHE`), wired in through `[patch.crates-io]` in the root
`Cargo.toml`. Every local change is marked with a `macros patch:` comment:

1. **Glyph contrast and gamma correction in the Metal shader**
   (`src/shaders.metal`, `monochrome_sprite_fragment`). Ports
   `apply_contrast_and_gamma_correction` from gpui 0.2.2's
   `src/platform/blade/shaders.wgsl`, with that renderer's defaults
   hard-coded (gamma 1.8, enhanced contrast 1.0). Without it, CoreGraphics'
   raw coverage blended linearly renders text thin and washed out — worst
   for light-on-dark at 1x displays. gpui-ce 0.1.0 just does
   `color.a *= sample.a`.

Carried over from the previously vendored gpui 0.2.2 (patch #2 there;
patch #1 of that set, `SUBPIXEL_VARIANTS_Y = 1`, is already in gpui-ce).

On Windows/Linux this crate is target-gated out, so a change here is only
compiled on macOS — verify text rendering there after touching it.
