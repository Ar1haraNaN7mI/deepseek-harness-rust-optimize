//! Codex-aligned keyboard shortcuts for the TUI.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Human-readable shortcut help shown by `/keymap` and `/help keys`.
pub const KEYMAP_HELP: &str = "\
KEYBOARD
────────────────────────────────────────
  Enter                 Send message
  Alt+Enter             Insert newline
  Esc                   Cancel running turn
  Esc Esc (empty)       Fork & edit last user message
  Ctrl+C                Quit  (/exit)
  Ctrl+L                Clear transcript view (keep session)
  Ctrl+N                New chat  (/new)
  Ctrl+O                Copy latest assistant output  (/copy)
  Ctrl+P                Permissions help  (/permissions)
  Ctrl+R                Browse draft history
  Ctrl+G                Goal help  (/goal)
  Tab                   Autocomplete slash command
  Tab (while busy)      Queue follow-up for next turn
  Enter (while busy)    Queue / inject follow-up
  ↑ / ↓                 Scroll transcript (when focused) / draft history
  PgUp / PgDn           Scroll transcript by page
  Mouse click           Focus transcript / composer / sidebar
  Mouse wheel           Scroll the pane under the cursor
  Home / End / ← / →    Move cursor in composer

COMPOSER PREFIXES
────────────────────────────────────────
  !command              Background shell  (see /ps /stop)
  @path                 Mention a file path in the prompt
  /command              Slash command  (Tab to complete)
";

/// Default binding table (inspect-only for now; remapping persists later).
pub const KEYMAP_DEFAULTS: &str = "\
DEFAULT BINDINGS
────────────────────────────────────────
  global.quit               = ctrl-c
  global.cancel             = esc
  global.clear_view         = ctrl-l
  global.new_chat           = ctrl-n
  global.copy_last          = ctrl-o
  global.permissions        = ctrl-p
  global.history_search     = ctrl-r
  global.goal_help          = ctrl-g
  composer.send             = enter
  composer.newline          = alt-enter
  composer.slash_complete   = tab
  composer.shell_prefix     = !
  composer.mention_prefix   = @
  transcript.scroll_up      = up
  transcript.scroll_down    = down
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Quit,
    Cancel,
    ClearView,
    NewChat,
    CopyLast,
    PermissionsHelp,
    HistorySearch,
    GoalHelp,
    Newline,
    Send,
    SlashComplete,
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    CursorLeft,
    CursorRight,
    CursorHome,
    CursorEnd,
    Backspace,
    Delete,
    InsertChar(char),
}

pub fn map_key(key: KeyEvent) -> Option<KeyAction> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    match key.code {
        KeyCode::Char('c') if ctrl => Some(KeyAction::Quit),
        KeyCode::Char('l') if ctrl => Some(KeyAction::ClearView),
        KeyCode::Char('n') if ctrl => Some(KeyAction::NewChat),
        KeyCode::Char('o') if ctrl => Some(KeyAction::CopyLast),
        KeyCode::Char('p') if ctrl => Some(KeyAction::PermissionsHelp),
        KeyCode::Char('r') if ctrl => Some(KeyAction::HistorySearch),
        KeyCode::Char('g') if ctrl => Some(KeyAction::GoalHelp),
        KeyCode::Esc => Some(KeyAction::Cancel),
        KeyCode::Enter if alt => Some(KeyAction::Newline),
        KeyCode::Enter => Some(KeyAction::Send),
        KeyCode::Tab => Some(KeyAction::SlashComplete),
        KeyCode::Backspace => Some(KeyAction::Backspace),
        KeyCode::Delete => Some(KeyAction::Delete),
        KeyCode::Left => Some(KeyAction::CursorLeft),
        KeyCode::Right => Some(KeyAction::CursorRight),
        KeyCode::Home => Some(KeyAction::CursorHome),
        KeyCode::End => Some(KeyAction::CursorEnd),
        KeyCode::Up => Some(KeyAction::ScrollUp),
        KeyCode::Down => Some(KeyAction::ScrollDown),
        KeyCode::PageUp => Some(KeyAction::PageUp),
        KeyCode::PageDown => Some(KeyAction::PageDown),
        KeyCode::Char(c) if !ctrl => Some(KeyAction::InsertChar(c)),
        _ => None,
    }
}
