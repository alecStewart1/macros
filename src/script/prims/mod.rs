//! The Scheme primitives, one module per API category ([`super::Section`]).
//! Each module's `register` files its functions under its section(s);
//! `build_engine` (registry.rs) calls them in turn.

use super::*;

pub(super) mod buffers;
pub(super) mod decorations;
pub(super) mod editing;
pub(super) mod faces;
pub(super) mod files;
pub(super) mod grid;
pub(super) mod help;
pub(super) mod hooks;
pub(super) mod keymaps;
pub(super) mod languages;
pub(super) mod lsp;
pub(super) mod modal;
pub(super) mod options;
pub(super) mod plotting;
pub(super) mod prompts;
pub(super) mod special_buffers;
pub(super) mod spell;
pub(super) mod text;
pub(super) mod tree_sitter;
