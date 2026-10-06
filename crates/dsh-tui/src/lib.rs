//! Ratatui terminal UI for dsh-rust (Codex-inspired transcript + category colors).

mod app;
mod commands;
mod keymap;
mod startup;
mod startup_audio;
mod terminal;
mod theme;

pub use app::{run_tui, TuiOptions};
pub use startup::{preview_startup, preview_startup_with_context, StartupContext};
