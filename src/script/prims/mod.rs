//! The Scheme primitives, one module per API category ([`super::Section`]).
//! Each module's `register` files its functions under its section(s);
//! `build_engine` (registry.rs) calls them in turn.

use super::*;

pub(super) mod grid;
pub(super) mod help;
pub(super) mod keymaps;
