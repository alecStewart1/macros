//! The Scheme primitives, one module per API category ([`super::Section`]).
//! Each module's `register` files its functions under its section(s);
//! `build_engine` (registry.rs) calls them in turn.

use super::*;

pub(super) mod editing;
pub(super) mod faces;
pub(super) mod grid;
pub(super) mod help;
pub(super) mod keymaps;
pub(super) mod languages;
pub(super) mod modal;
pub(super) mod plotting;
pub(super) mod prompts;
pub(super) mod spell;
pub(super) mod text;
pub(super) mod tree_sitter;
