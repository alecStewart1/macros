# Vendored gpui 0.2.2

This is the crates.io release of `gpui` 0.2.2 (Apache-2.0, see
`LICENSE-APACHE`), wired in through `[patch.crates-io]` in the root
`Cargo.toml`. We stay on 0.2.2 because git `main` doesn't rasterize text in our
build. Every local change is marked with a `macros patch:` comment:

1. **No vertical subpixel glyph positioning on macOS** (`src/text_system.rs`,
   `SUBPIXEL_VARIANTS_Y = 1`). With 4 variants, a baseline that falls between
   pixel rows spreads horizontal strokes over two rows, so text looks blurry
   on 1x displays. Linux and Windows already used 1.
2. **Glyph contrast and gamma correction in the Metal shader**
   (`src/platform/mac/shaders.metal`, `monochrome_sprite_fragment`). This
   ports `apply_contrast_and_gamma_correction` from
   `src/platform/blade/shaders.wgsl`, with that renderer's defaults hard-coded
   (gamma 1.8, enhanced contrast 1.0).
3. **Warning fix** (`src/taffy.rs:312`): adds `f32` literal suffixes. This is
   only visible now that gpui builds as a path dependency.
4. **`stacksafe` 0.1 → 1.0** (`Cargo.toml`): 0.1's macro crate depends on
   `proc-macro-error2`, which triggers a future-incompatibility warning (it
   re-exports the private `proc_macro` crate) on every build. 1.0 dropped that
   dependency; the `StackSafe` / `#[stacksafe]` API gpui uses is unchanged.

To move to a newer gpui, drop the `[patch.crates-io]` entry. Then check
whether upstream already includes these fixes before you re-vendor.
