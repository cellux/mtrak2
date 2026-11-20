//! Terminal user interface and command layer for mtrak.
//!
//! The TUI is isolated from the core engine: key handling is mapped to actions
//! that mutate an [`app::App`] state, which can be tested without a terminal.
//! Only [`ui`] knows about `ratatui`.

#![forbid(unsafe_code)]

pub mod app;
pub mod clipboard;
pub mod command;
pub mod editor;
pub mod key;
pub mod ui;
pub mod undo;

pub use app::{
    App, BrushHeight, BrushWidth, LuaCommand, MacroSource, Message, Mode, Rect, Selection,
};
pub use command::{parse as parse_command, Command};
pub use key::{Action, Code, Key};
