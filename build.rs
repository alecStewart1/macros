//! Build script. Two jobs:
//!
//! 1. Compile the vendored Mermaid tree-sitter parser (`grammars/tree-sitter-mermaid`)
//!    from C and link it in, so `src/highlight.rs` can bind `tree_sitter_mermaid()`
//!    as a `LanguageFn`. Every other grammar arrives prebuilt via a crate; Mermaid
//!    has no crate exposing the modern API against tree-sitter 0.26, so we vendor
//!    its `parser.c` (no external scanner) and compile it here.
//!
//! 2. Windows-only: embed the application icon and version metadata into
//!    `macros.exe` via a compiled resource (.res), so the installed Start-menu /
//!    taskbar shortcut shows a real icon. The WiX installer (`wix/main.wxs`) sets
//!    NO `Shortcut/@Icon` on purpose — it lets the shell resolve the icon from the
//!    exe, whose path is stable across upgrades (an explicit installer-cached icon
//!    path goes stale on MajorUpgrade). The `winresource` crate is gated to
//!    `cfg(windows)` build-dependencies in Cargo.toml, so it isn't compiled when
//!    targeting macOS/Linux.

fn main() {
    // Expose the target triple to the running binary as `BUILD_TARGET`. The `cc`
    // crate needs a target to locate a toolchain, but it reads `$TARGET` from the
    // build environment, which isn't present at runtime — so we bake it in for
    // the Windows grammar compiler (src/grammar.rs). Mirrors Helix's BUILD_TARGET.
    if let Ok(target) = std::env::var("TARGET") {
        println!("cargo:rustc-env=BUILD_TARGET={target}");
    }

    compile_mermaid_grammar();

    #[cfg(windows)]
    {
        // Only re-run when the icon changes.
        println!("cargo:rerun-if-changed=assets/brand/icon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/brand/icon.ico");
        // Best-effort: a missing toolchain/icon must not hard-fail the build of
        // the editor itself. CI ships the .ico (scripts/build_icons.sh), so in
        // a release build this always succeeds.
        if let Err(e) = res.compile() {
            println!("cargo:warning=winresource: failed to embed icon: {e}");
        }
    }
}

/// Compile the vendored Mermaid grammar's `parser.c` into a static lib linked
/// into the binary. The grammar has no external scanner, so this is the single
/// translation unit plus its bundled `tree_sitter` headers.
fn compile_mermaid_grammar() {
    let dir = "grammars/tree-sitter-mermaid/src";
    println!("cargo:rerun-if-changed={dir}/parser.c");
    cc::Build::new()
        .include(dir)
        .file(format!("{dir}/parser.c"))
        // tree-sitter's generated parser.c trips these on -Wall; they're benign.
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .warnings(false)
        .compile("tree-sitter-mermaid");
}
