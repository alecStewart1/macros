//! The Steel scripting layer, split by concern:
//!   * [`commands`] — the [`EditorCommand`] queue vocabulary and its payloads.
//!   * [`state`] — shared editor state published to the engine thread
//!     (snapshots, markers, options).
//!   * [`gix`] — pure git backends (log/diff/status/…) with no Steel coupling.
//!   * [`engine`] — the [`ScriptEngine`] handle and its dedicated thread.
//!   * [`registry`] — `build_engine`: every registered Scheme bridge function.
//!   * [`convert`] — SteelVal parsing/formatting helpers.

mod commands;
mod convert;
mod engine;
mod gix;
mod registry;
mod state;

pub use commands::*;
pub(crate) use convert::*;
pub use engine::*;
pub(crate) use gix::*;
pub(crate) use registry::*;
pub use state::*;

#[cfg(test)]
mod tests;
