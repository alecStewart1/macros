# Vendored block 0.1.6

The crates.io release of `block` 0.1.6 (MIT, by Steven Sheldon), wired in through
`[patch.crates-io]` in the root `Cargo.toml`. It's abandoned upstream — 0.1.6 is
the last release — yet gpui, cocoa, metal and core-video all depend on it.

Local changes, each marked with a `macros patch:` comment:

1. **Future-incompatibility fix** (`src/lib.rs`): `Class`, the type of the
   `_NSConcreteStackBlock` extern static, is an opaque zero-sized `#[repr(C)]`
   struct instead of an uninhabited `enum Class { }`. rustc warns that a static
   of an uninhabited type will become a hard error (rust-lang/rust#74840). Only
   the static's address is used, so behavior is unchanged.
2. **Explicit `extern "C"` ABI** (`src/lib.rs`): a bare `extern` / `extern fn`
   is deprecated and, now that the crate builds as a path dependency, its
   warnings would show in every build. `"C"` is what a bare `extern` meant.
3. **Dev-dependency removed** (`Cargo.toml`): `objc_test_utils` points at a
   path the published package doesn't include.

Drop the patch entry if a fixed `block` release ever appears.
