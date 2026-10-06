//! Restore the console even when setup, drawing, or an awaited operation fails.

use crossterm::{
    cursor::{Hide, Show},
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    style::ResetColor,
    terminal::{
        disable_raw_mode, enable_raw_mode, is_raw_mode_enabled, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use std::io::{self, stdout};

pub(crate) struct TerminalGuard {
    restore_raw: bool,
}

impl TerminalGuard {
    pub(crate) fn enter() -> io::Result<Self> {
        let restore_raw = !is_raw_mode_enabled()?;
        enable_raw_mode()?;
        // Construct before changing the screen so partial setup failures also restore it.
        let guard = Self { restore_raw };
        execute!(stdout(), EnterAlternateScreen, Hide, EnableMouseCapture)?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(
            stdout(),
            DisableMouseCapture,
            ResetColor,
            LeaveAlternateScreen,
            Show
        );
        if self.restore_raw {
            let _ = disable_raw_mode();
        }
    }
}
