//! Ratatui terminal UI for dsh-rust (Codex-inspired transcript + category colors).

mod app;
mod commands;
mod keymap;
mod theme;

pub use app::{run_tui, TuiOptions};
