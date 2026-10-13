//! Face and theme primitives: colors, fonts, underline / bold / wavy, and
//! theme loading.

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, ctx: &EngineShared) {
    let EngineShared { queue, .. } = ctx;
    reg.section(Section::Faces);
    {
        // `(theme-names)` → every bundled theme name (the stem of each
        // scheme/themes/*.scm). The `load-theme` wrapper in core.scm reads this to
        // complete theme names — in the M-x prompt and the in-buffer argument CAPF.
        reg.register_fn("theme-names", || -> Vec<String> {
            crate::runtime::theme_names()
        });
    }
    register_binary(reg, queue, "set-face", EditorCommand::SetFace);
    // `(set-face-background name color)` — the background painted behind
    // overlay text using `name` (plot heatmaps' `plot-heat-N-bg`).
    register_binary(
        reg,
        queue,
        "set-face-background",
        EditorCommand::SetFaceBackground,
    );
    // `(set-face-font name family)` — the font family for text under `name`
    // (an installed or bundled family, e.g. "Macros Plot").
    register_binary(reg, queue, "set-face-font", EditorCommand::SetFaceFont);
    register_binary(
        reg,
        queue,
        "set-face-underline",
        EditorCommand::SetFaceUnderline,
    );
    register_binary(reg, queue, "set-face-bold", EditorCommand::SetFaceBold);
    register_binary(reg, queue, "set-face-wavy", EditorCommand::SetFaceWavy);
    {
        // `load-theme-impl` accepts a string or a symbol — `(load-theme "x")` and
        // `(load-theme 'x)` both reach here via the core.scm wrapper. Any other
        // value is stringified, so a bad arg echoes "No theme named …" rather than
        // failing to type-check. The wrapper adds the no-arg interactive prompt.
        register_unary(reg, queue, "load-theme-impl", |Name(name)| {
            EditorCommand::LoadTheme(name)
        });
    }
}
