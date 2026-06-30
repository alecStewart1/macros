//! Build script. Two jobs:
//!
//! 1. Compile the vendored tree-sitter parsers under `grammars/` from C and link
//!    them in, so `src/highlight.rs` can bind their `tree_sitter_<name>()` entry
//!    points as `LanguageFn`s. Every other grammar arrives prebuilt via a crate;
//!    these have no crate exposing the modern API against tree-sitter 0.26, so we
//!    vendor their C sources and compile them here. Mermaid is a lone `parser.c`;
//!    Racket additionally has a small external `scanner.c` (here-string bodies).
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

    // Mermaid: parser only. Racket: parser + external scanner.
    compile_grammar(
        "tree-sitter-mermaid",
        "grammars/tree-sitter-mermaid/src",
        false,
    );
    compile_grammar(
        "tree-sitter-racket",
        "grammars/tree-sitter-racket/src",
        true,
    );

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

/// Compile a vendored grammar's C sources into a static lib linked into the
/// binary: `parser.c` (always) plus `scanner.c` when `has_scanner`, alongside the
/// grammar's bundled `tree_sitter` headers. `name` is both the cc output lib name
/// and matches the `tree_sitter_<…>()` entry point bound in `src/highlight.rs`.
fn compile_grammar(name: &str, dir: &str, has_scanner: bool) {
    let mut build = cc::Build::new();
    build.include(dir).file(format!("{dir}/parser.c"));
    println!("cargo:rerun-if-changed={dir}/parser.c");
    if has_scanner {
        build.file(format!("{dir}/scanner.c"));
        println!("cargo:rerun-if-changed={dir}/scanner.c");
    }
    build
        // tree-sitter's generated parser.c trips these on -Wall; they're benign.
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .warnings(false)
        .compile(name);
}
