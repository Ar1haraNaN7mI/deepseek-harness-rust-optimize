//! Codex-inspired semantic colors and animation glyphs.

use ratatui::style::{Color, Modifier, Style};

/// Visual category for history cells (scanable like Codex tool rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellKind {
    User,
    Assistant,
    Thinking,
    Ctm,
    Terminal,
    Skill,
    Plugin,
    Plan,
    Filesystem,
    Web,
    Learn,
    System,
    Error,
    Tool,
}

impl CellKind {
    pub fn label(self) -> &'static str {
        match self {
            CellKind::User => "you",
            CellKind::Assistant => "dsh",
            CellKind::Thinking => "think",
            CellKind::Ctm => "ctm",
            CellKind::Terminal => "shell",
            CellKind::Skill => "skill",
            CellKind::Plugin => "plugin",
            CellKind::Plan => "plan",
            CellKind::Filesystem => "fs",
            CellKind::Web => "web",
            CellKind::Learn => "learn",
            CellKind::System => "sys",
            CellKind::Error => "error",
            CellKind::Tool => "tool",
        }
    }

    pub fn accent(self) -> Color {
        match self {
            CellKind::User => Color::Cyan,
            CellKind::Assistant => Color::Rgb(180, 190, 200),
            CellKind::Thinking => Color::DarkGray,
            CellKind::Ctm => Color::Magenta,
            CellKind::Terminal => Color::Rgb(230, 180, 80),
            CellKind::Skill => Color::Rgb(80, 200, 180),
            CellKind::Plugin => Color::Rgb(200, 120, 220),
            CellKind::Plan => Color::Rgb(100, 160, 255),
            CellKind::Filesystem => Color::Rgb(120, 200, 120),
            CellKind::Web => Color::Rgb(100, 180, 230),
            CellKind::Learn => Color::Rgb(255, 160, 90),
            CellKind::System => Color::DarkGray,
            CellKind::Error => Color::Red,
            CellKind::Tool => Color::Yellow,
        }
    }

    pub fn label_style(self) -> Style {
        Style::default()
            .fg(self.accent())
            .add_modifier(Modifier::BOLD)
    }

    pub fn body_style(self) -> Style {
        match self {
            CellKind::User => Style::default().fg(Color::Cyan),
            CellKind::Assistant => Style::default(),
            CellKind::Thinking => Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
            CellKind::Ctm => Style::default().fg(Color::Magenta).add_modifier(Modifier::DIM),
            CellKind::Error => Style::default().fg(Color::Red),
            CellKind::System => Style::default().fg(Color::DarkGray),
            _ => Style::default().fg(Color::Rgb(200, 200, 200)),
        }
    }

    pub fn bullet(self, ok: Option<bool>) -> (&'static str, Style) {
        let style = Style::default().fg(self.accent());
        match ok {
            Some(true) => ("✔", style.fg(Color::Green).add_modifier(Modifier::BOLD)),
            Some(false) => ("✖", style.fg(Color::Red).add_modifier(Modifier::BOLD)),
            None => ("•", style.add_modifier(Modifier::DIM)),
        }
    }
}

pub fn categorize_tool(name: &str) -> CellKind {
    let lower = name.to_lowercase();
    if lower == "shell" || lower.starts_with("bash") || lower.contains("terminal") {
        return CellKind::Terminal;
    }
    if lower.starts_with("skill_") || lower.starts_with("skill.") {
        return CellKind::Skill;
    }
    if lower.starts_with("plugin.") || lower.starts_with("plugin_") {
        return CellKind::Plugin;
    }
    if lower.starts_with("todo_") || lower.contains("plan") {
        return CellKind::Plan;
    }
    if matches!(
        lower.as_str(),
        "read_file"
            | "write_file"
            | "edit_file"
            | "apply_patch"
            | "list_dir"
            | "glob"
            | "grep"
    ) {
        return CellKind::Filesystem;
    }
    if lower == "web_fetch" || lower.starts_with("web_") {
        return CellKind::Web;
    }
    if lower.starts_with("learn_") {
        return CellKind::Learn;
    }
    CellKind::Tool
}

pub fn border_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub fn accent_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

pub fn dim_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub fn composer_title_style(busy: bool) -> Style {
    if busy {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        accent_style()
    }
}

/// Braille spinner — general activity.
pub const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Arc spinner — shell / terminal.
pub const SPINNER_ARC: &[&str] = &["◐", "◓", "◑", "◒"];

/// Pulse blocks — filesystem / plan.
pub const SPINNER_BLOCKS: &[&str] = &["▖", "▘", "▝", "▗"];

/// Orbit dots — plugins / skills.
pub const SPINNER_ORBIT: &[&str] = &["●○○", "○●○", "○○●", "○●○"];

/// Wave bar — streaming / thinking.
pub const SPINNER_WAVE: &[&str] = &["▁▃▅", "▃▅▇", "▅▇▅", "▇▅▃", "▅▃▁", "▃▁▃"];

pub fn spinner_frame(tick: u64) -> &'static str {
    SPINNER[(tick as usize) % SPINNER.len()]
}

/// Category-aware running glyph (prettier than one shared spinner).
pub fn running_glyph(kind: CellKind, tick: u64) -> &'static str {
    let i = tick as usize;
    match kind {
        CellKind::Terminal => SPINNER_ARC[i % SPINNER_ARC.len()],
        CellKind::Filesystem | CellKind::Plan => SPINNER_BLOCKS[i % SPINNER_BLOCKS.len()],
        CellKind::Skill | CellKind::Plugin | CellKind::Learn => {
            SPINNER_ORBIT[i % SPINNER_ORBIT.len()]
        }
        CellKind::Web | CellKind::Ctm => SPINNER_WAVE[i % SPINNER_WAVE.len()],
        CellKind::Thinking => SPINNER_WAVE[i % SPINNER_WAVE.len()],
        _ => SPINNER[i % SPINNER.len()],
    }
}

/// Animated trailing activity for streaming assistant / thinking.
pub fn stream_caret(tick: u64, on: bool) -> (&'static str, Style) {
    if !on {
        return (
            " ",
            Style::default(),
        );
    }
    let frames = ["▍", "▍", "▍", " "];
    let ch = frames[(tick as usize / 2) % frames.len()];
    (
        ch,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
}

/// Horizontal activity meter for status / busy title.
pub fn activity_bar(tick: u64, width: usize) -> String {
    let width = width.clamp(6, 16);
    let pos = (tick as usize) % (width.saturating_mul(2).saturating_sub(2).max(1));
    let head = if pos < width { pos } else { width * 2 - 2 - pos };
    let mut out = String::with_capacity(width);
    for i in 0..width {
        if i == head {
            out.push('━');
        } else if i + 1 == head || i == head + 1 {
            out.push('─');
        } else {
            out.push('·');
        }
    }
    out
}

/// Soft border pulse between cyan and yellow while busy.
pub fn busy_border_color(tick: u64) -> Color {
    match tick % 6 {
        0 | 1 => Color::Yellow,
        2 | 3 => Color::Rgb(255, 200, 80),
        _ => Color::Cyan,
    }
}

/// Blinking block cursor glyph.
pub fn cursor_block(visible: bool) -> (&'static str, Style) {
    if visible {
        (
            "▌",
            Style::default()
                .fg(Color::Cyan)
                .bg(Color::Rgb(30, 50, 70))
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (
            "▏",
            Style::default().fg(Color::Rgb(60, 80, 100)),
        )
    }
}

pub fn running_suffix(tick: u64) -> String {
    let dots = [".  ", ".. ", "...", " ..", "  .", "   "];
    dots[(tick as usize / 2) % dots.len()].to_string()
}
