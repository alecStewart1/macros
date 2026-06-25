//! Bundled assets (icons), embedded at compile time and served to GPUI's SVG
//! renderer. `window.paint_svg` resolves a path like `"icons/folder.svg"`
//! through this source. Icons are Lucide (MIT) — single-stroke, so they render
//! monochrome and tint with whatever color the element passes.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};
use include_dir::{Dir, include_dir};

static ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/assets");

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ASSETS.get_file(path).map(|f| Cow::Borrowed(f.contents())))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ASSETS
            .get_dir(path)
            .map(|dir| {
                dir.files()
                    .map(|f| SharedString::from(f.path().to_string_lossy().into_owned()))
                    .collect()
            })
            .unwrap_or_default())
    }
}
