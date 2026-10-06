//! Codex-inspired TUI: transcript cells + bottom composer + semantic category colors.

use crate::commands::{
    autocomplete_slash, handle_slash, matching_commands, SlashCtx, SlashEffect, UiLine,
};
use crate::keymap::{map_key, KeyAction};
use crate::theme::{
    accent_style, activity_bar, border_style, busy_border_color, categorize_tool,
    composer_title_style, cursor_block, dim_style, running_glyph, running_suffix, spinner_frame,
    stream_caret, CellKind,
};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use dsh_core::{AgentEvent, AgentHandle, AgentLoop, Runtime, Session};
use parking_lot::RwLock;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Terminal;
use std::io::{stdin, stdout, IsTerminal, Stdout};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub struct TuiOptions {
    pub model: String,
    pub cwd: String,
    pub show_thinking: bool,
    pub sidebar: bool,
    pub skill_names: Vec<String>,
    pub plugin_names: Vec<String>,
    pub session_id: Option<String>,
    pub has_api_key: bool,
    /// When set, auto-start a turn after the first draw (Codex positional prompt).
    pub initial_prompt: Option<String>,
    /// Per-invocation opt-in, including when the queued one-shot preference is off.
    pub startup_enabled: bool,
    /// Highest-priority per-invocation opt-out, including a queued one-shot intro.
    pub startup_disabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusPane {
    Transcript,
    Composer,
    Sidebar,
}

#[derive(Default, Clone, Copy)]
struct PaneLayout {
    transcript: Rect,
    composer: Rect,
    status: Rect,
    sidebar: Option<Rect>,
}

struct AppState {
    lines: Vec<UiLine>,
    input: String,
    /// Cursor position in Unicode characters (not bytes).
    cursor: usize,
    cursor_on: bool,
    last_blink: Instant,
    status: String,
    busy: bool,
    show_thinking: bool,
    sidebar: bool,
    model: String,
    cwd: String,
    skill_names: Vec<String>,
    plugin_names: Vec<String>,
    handle: Option<AgentHandle>,
    should_quit: bool,
    session_id: String,
    dirty: bool,
    spin_tick: u64,
    last_spin: Instant,
    has_api_key: bool,
    runtime: Arc<Runtime>,
    /// True while assistant text is actively streaming.
    streaming: bool,
    /// Previous user drafts for ↑ history.
    draft_history: Vec<String>,
    draft_index: Option<usize>,
    /// Codex: queue prompts/commands while a turn is running.
    pending_queue: Vec<String>,
    /// EscEsc: empty composer + double Esc forks from last user message.
    last_esc: Option<Instant>,
    /// Codex /vim — when true, composer starts in normal mode.
    vim_mode: bool,
    vim_insert: bool,
    raw_mode: bool,
    /// Waiting for y/n on AgentEvent::ApprovalNeeded.
    awaiting_approval: bool,
    /// Request id used to resolve the exact approval when multiple runs wait.
    awaiting_approval_request_id: Option<String>,
    /// Optional auto-send prompt after first draw.
    initial_prompt: Option<String>,
    /// @mention fuzzy file candidates.
    mention_candidates: Vec<String>,
    mention_index: usize,
    /// Slash popup selection index.
    slash_index: usize,
    /// Which pane receives scroll / keyboard focus (click to change).
    focus: FocusPane,
    /// First visible transcript line index (mouse-wheel / keys scroll this).
    transcript_scroll: usize,
    /// When true, keep pinned to the latest messages.
    stick_to_bottom: bool,
    /// Inner height of transcript viewport (rows), updated each draw.
    transcript_view_h: usize,
    /// Last computed pane geometry for hit-testing mouse clicks.
    panes: PaneLayout,
}

/// Resolve presentation only after terminal and session setup succeeds. Explicit
/// choices override the queued preference, but an eligible launch still consumes it.
fn startup_enabled_for_launch(
    outer_home: &Path,
    configured: bool,
    available: bool,
    requested: bool,
    disabled: bool,
) -> Result<bool> {
    if !available {
        return Ok(false);
    }
    let pending = dsh_core::take_next_startup(outer_home)?;
    Ok((requested || pending.unwrap_or(configured)) && !disabled)
}

#[cfg(test)]
mod startup_launch_tests {
    use super::startup_enabled_for_launch;
    use std::path::PathBuf;

    struct TemporaryHome(PathBuf);

    impl TemporaryHome {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!(
                "dsh-startup-launch-{}-{stamp}",
                std::process::id()
            )))
        }
    }

    impl Drop for TemporaryHome {
        fn drop(&mut self) {
            for name in ["startup-next.txt", ".startup-next.lock"] {
                let _ = std::fs::remove_file(self.0.join(name));
            }
            let _ = std::fs::remove_dir(&self.0);
        }
    }

    #[test]
    fn explicit_startup_choices_override_and_consume_the_pending_choice_once() {
        let home = TemporaryHome::new();
        for (configured, pending, requested, disabled, expected) in [
            (false, None, false, false, false),
            (true, None, false, false, true),
            (false, Some(true), false, false, true),
            (true, Some(false), false, false, false),
            (false, Some(false), true, false, true),
            (true, Some(true), false, true, false),
            (true, Some(true), true, true, false),
        ] {
            if let Some(pending) = pending {
                dsh_core::set_next_startup(&home.0, pending).unwrap();
            }
            let selected = startup_enabled_for_launch(
                &home.0, configured, true, requested, disabled,
            ).unwrap();
            assert_eq!(selected, expected);
            assert_eq!(dsh_core::take_next_startup(&home.0).unwrap(), None);
        }
    }

    #[test]
    fn startup_opt_in_does_not_consume_a_choice_for_an_ineligible_terminal() {
        let home = TemporaryHome::new();
        dsh_core::set_next_startup(&home.0, false).unwrap();
        assert!(!startup_enabled_for_launch(&home.0, true, false, true, false).unwrap());
        assert_eq!(dsh_core::take_next_startup(&home.0).unwrap(), Some(false));
    }
}

pub async fn run_tui(runtime: Arc<Runtime>, opts: TuiOptions) -> Result<()> {
    anyhow::ensure!(stdin().is_terminal() && stdout().is_terminal(), "TUI requires an interactive terminal; use `dsh exec <prompt>` for redirected input/output.");
    let _terminal_guard = crate::terminal::TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let settings = runtime.settings.read().clone();
    let show_thinking = settings.show_thinking.unwrap_or(opts.show_thinking);
    let sidebar = settings.sidebar.unwrap_or(opts.sidebar);

    let session = if let Some(id) = &opts.session_id {
        runtime.sessions.get_or_load(id)?
    } else {
        runtime.sessions.create()
    };
    let session_id = session.read().id.clone();
    let mut startup = runtime.config.tui.startup.clone();
    startup.enabled = startup_enabled_for_launch(
        &runtime.outer_home,
        startup.enabled,
        crate::startup::available(),
        opts.startup_enabled,
        opts.startup_disabled,
    )?;
    let startup_context = if startup.enabled && crate::startup::available() {
        // Snapshot already mounted inventories; animation never runs discovery or plugins.
        let skills = runtime.skills.read().clone();
        let plugins = runtime.plugins.read().clone();
        let profile = dsh_core::load_startup_profile(&runtime.outer_home).unwrap_or_else(|error| {
            tracing::warn!(%error, "startup profile unavailable; using local defaults");
            dsh_core::StartupProfile::default()
        });
        crate::StartupContext {
            profile,
            inventory_loaded: skills.is_some() && plugins.is_some(),
            skill_names: skills
                .as_ref()
                .map(|catalog| catalog.list().into_iter().map(|s| s.name).collect())
                .unwrap_or_default(),
            plugin_names: plugins
                .as_ref()
                .map(|registry| {
                    registry
                        .routing_summaries()
                        .into_iter()
                        .map(|p| {
                            if p.name == p.id {
                                p.name
                            } else {
                                format!("{} ({})", p.name, p.id)
                            }
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    } else {
        crate::StartupContext::default()
    };
    if crate::startup::play(&mut terminal, &startup, &startup_context).await?
        == crate::startup::Outcome::Quit
    {
        return Ok(());
    }
    let agent = AgentLoop::new(runtime.clone());
    let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(256);

    let perm = *runtime.permissions.read();
    let mut seed_lines = vec![
        UiLine {
            kind: CellKind::System,
            text: "Welcome to dsh-rust — DeepSeek coding agent (Codex-style TUI)".into(),
            header: Some("dsh".into()),
            running: false,
            ok: None,
        },
        UiLine {
            kind: CellKind::System,
            text: format!(
                "session {} · {} · click a pane to focus · mouse wheel scrolls chat",
                &session_id[..8.min(session_id.len())],
                perm.label()
            ),
            header: None,
            running: false,
            ok: None,
        },
        UiLine {
            kind: CellKind::System,
            text: "Tips: Enter send · Alt+Enter newline · Esc cancel · /help · /keymap".into(),
            header: None,
            running: false,
            ok: None,
        },
    ];
    if !opts.has_api_key {
        seed_lines.push(UiLine {
            kind: CellKind::Error,
            text: "No API key yet. Paste one with:  /apikey sk-...".into(),
            header: Some("setup".into()),
            running: false,
            ok: Some(false),
        });
        seed_lines.push(UiLine {
            kind: CellKind::System,
            text: "Or from a terminal:  dsh login   (saves to ~/.dsh-rust/credentials.env)".into(),
            header: Some("setup".into()),
            running: false,
            ok: None,
        });
    } else {
        seed_lines.push(UiLine {
            kind: CellKind::System,
            text: "Ready — type a task below, or try /plan /review /status".into(),
            header: Some("ready".into()),
            running: false,
            ok: Some(true),
        });
    }
    for line in session.read().transcript_lines() {
        seed_lines.push(classify_transcript_seed(&line));
    }

    let mut app = AppState {
        lines: seed_lines,
        input: String::new(),
        cursor: 0,
        cursor_on: true,
        last_blink: Instant::now(),
        status: if opts.has_api_key {
            format!("{} · {} · {}", opts.model, perm.label(), opts.cwd)
        } else {
            "API key missing — /apikey <KEY>".into()
        },
        busy: false,
        show_thinking,
        sidebar,
        model: opts.model,
        cwd: opts.cwd,
        skill_names: opts.skill_names,
        plugin_names: opts.plugin_names,
        handle: None,
        should_quit: false,
        session_id,
        dirty: true,
        spin_tick: 0,
        last_spin: Instant::now(),
        has_api_key: opts.has_api_key,
        runtime: runtime.clone(),
        streaming: false,
        draft_history: Vec::new(),
        draft_index: None,
        pending_queue: Vec::new(),
        last_esc: None,
        vim_mode: settings.vim_mode,
        vim_insert: !settings.vim_mode,
        raw_mode: settings.raw_mode,
        awaiting_approval: false,
        awaiting_approval_request_id: None,
        initial_prompt: opts.initial_prompt,
        mention_candidates: Vec::new(),
        mention_index: 0,
        slash_index: 0,
        focus: FocusPane::Composer,
        transcript_scroll: 0,
        stick_to_bottom: true,
        transcript_view_h: 10,
        panes: PaneLayout::default(),
    };

    let result = run_loop(
        &mut terminal,
        &mut app,
        &agent,
        session,
        event_tx,
        &mut event_rx,
    )
    .await;

    result
}

fn classify_transcript_seed(line: &str) -> UiLine {
    if let Some(rest) = line.strip_prefix("You: ") {
        return UiLine {
            kind: CellKind::User,
            text: rest.to_string(),
            header: None,
            running: false,
            ok: None,
        };
    }
    if let Some(rest) = line.strip_prefix("Assistant: ") {
        return UiLine {
            kind: CellKind::Assistant,
            text: rest.to_string(),
            header: None,
            running: false,
            ok: None,
        };
    }
    if let Some(rest) = line.strip_prefix("→ tool ") {
        let name = rest.split('(').next().unwrap_or(rest);
        let kind = categorize_tool(name);
        return UiLine {
            kind,
            text: rest.to_string(),
            header: Some(kind.label().into()),
            running: false,
            ok: None,
        };
    }
    if let Some(rest) = line.strip_prefix("← ") {
        let name = rest.split([' ', ':']).next().unwrap_or(rest);
        let kind = categorize_tool(name);
        let ok = !rest.contains("[err]");
        return UiLine {
            kind,
            text: rest.to_string(),
            header: Some(kind.label().into()),
            running: false,
            ok: Some(ok),
        };
    }
    UiLine {
        kind: CellKind::System,
        text: line.to_string(),
        header: None,
        running: false,
        ok: None,
    }
}

fn rebuild_from_session(app: &mut AppState, session: &Arc<RwLock<Session>>) {
    let id = session.read().id.clone();
    app.session_id = id.clone();
    let perm = *app.runtime.permissions.read();
    app.lines.clear();
    app.lines.push(UiLine {
        kind: CellKind::System,
        text: format!(
            "session {} · perms {} · /help · /keymap",
            &id[..8.min(id.len())],
            perm.label()
        ),
        header: Some("dsh-rust".into()),
        running: false,
        ok: None,
    });
    for line in session.read().transcript_lines() {
        app.lines.push(classify_transcript_seed(&line));
    }
    app.stick_to_bottom = true;
    clamp_transcript_scroll(app);
    app.status = format!("{} · {} · {}", app.model, perm.label(), app.cwd);
    app.dirty = true;
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut AppState,
    agent: &AgentLoop,
    mut session: Arc<RwLock<Session>>,
    event_tx: mpsc::Sender<AgentEvent>,
    event_rx: &mut mpsc::Receiver<AgentEvent>,
) -> Result<()> {
    let mut first_draw = true;
    loop {
        if app.should_quit {
            break;
        }

        let mut got_events = false;
        while let Ok(ev) = event_rx.try_recv() {
            apply_event(app, ev);
            got_events = true;
        }
        if got_events {
            app.dirty = true;
        }

        let any_running = app.lines.iter().any(|l| l.running);
        let animating = app.busy || app.streaming || any_running;
        if app.last_blink.elapsed() >= Duration::from_millis(530) {
            app.cursor_on = !app.cursor_on;
            app.last_blink = Instant::now();
            app.dirty = true;
        }
        if animating && app.last_spin.elapsed() >= Duration::from_millis(70) {
            app.spin_tick = app.spin_tick.wrapping_add(1);
            app.last_spin = Instant::now();
            app.dirty = true;
        }

        if app.dirty {
            terminal.draw(|f| draw(f, app))?;
            app.dirty = false;
        }

        if first_draw {
            first_draw = false;
            if let Some(prompt) = app.initial_prompt.take() {
                let prompt = prompt.trim().to_string();
                if !prompt.is_empty() {
                    if let Err(e) = start_turn(app, agent, &session, &event_tx, prompt).await {
                        app.lines.push(UiLine {
                            kind: CellKind::Error,
                            text: format!("{e}"),
                            header: Some("error".into()),
                            running: false,
                            ok: Some(false),
                        });
                        app.dirty = true;
                    }
                }
            }
        }

        if event::poll(Duration::from_millis(33))? {
            match event::read()? {
                Event::Mouse(mouse) => {
                    app.dirty = true;
                    handle_mouse(app, mouse);
                }
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    app.dirty = true;
                    app.cursor_on = true;
                    app.last_blink = Instant::now();

                    if app.awaiting_approval {
                        match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                                if let Some(request_id) = app.awaiting_approval_request_id.take() {
                                    app.runtime.resolve_approval_request(&request_id, true);
                                } else {
                                    app.runtime.resolve_approval(true);
                                }
                                app.awaiting_approval = false;
                                app.status = "tool approved".into();
                                app.lines.push(UiLine {
                                    kind: CellKind::System,
                                    text: "approved".into(),
                                    header: Some("approval".into()),
                                    running: false,
                                    ok: Some(true),
                                });
                            }
                            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                                if let Some(request_id) = app.awaiting_approval_request_id.take() {
                                    app.runtime.resolve_approval_request(&request_id, false);
                                } else {
                                    app.runtime.resolve_approval(false);
                                }
                                app.awaiting_approval = false;
                                app.status = "tool denied".into();
                                app.lines.push(UiLine {
                                    kind: CellKind::Error,
                                    text: "denied".into(),
                                    header: Some("approval".into()),
                                    running: false,
                                    ok: Some(false),
                                });
                            }
                            _ => {}
                        }
                        continue;
                    }

                    let Some(action) = map_key(key) else {
                        continue;
                    };
                    match action {
                        KeyAction::Quit => break,
                        KeyAction::Cancel => {
                            // Esc while browsing transcript → return to composer (friendlier UX).
                            if !app.busy
                                && app.focus == FocusPane::Transcript
                                && app.input.is_empty()
                            {
                                app.focus = FocusPane::Composer;
                                app.status = "composer focused".into();
                                continue;
                            }
                            // EscEsc with empty composer: fork from last user message (Codex).
                            if app.input.is_empty() {
                                let now = Instant::now();
                                let double = app
                                    .last_esc
                                    .map(|t| now.duration_since(t) < Duration::from_millis(600))
                                    .unwrap_or(false);
                                app.last_esc = Some(now);
                                if double {
                                    let forked_opt = session.read().fork_from_last_user();
                                    if let Some((forked, text)) = forked_opt {
                                        let s = app.runtime.sessions.insert(forked);
                                        session = s;
                                        rebuild_from_session(app, &session);
                                        app.input = text;
                                        app.cursor = app.input.chars().count();
                                        app.focus = FocusPane::Composer;
                                        app.status =
                                            "forked from last user message · edit and Enter".into();
                                        app.last_esc = None;
                                        continue;
                                    }
                                }
                            } else if app.vim_mode && app.vim_insert {
                                app.vim_insert = false;
                                app.status = "vim NORMAL".into();
                                continue;
                            }
                            if let Some(h) = &app.handle {
                                h.cancel();
                            }
                            app.status = "cancel requested · EscEsc empty = edit last".into();
                        }
                        KeyAction::ClearView => {
                            // Ctrl+L: clear view only (Codex) — keep session.
                            app.lines.clear();
                            app.lines.push(UiLine {
                                kind: CellKind::System,
                                text: format!(
                                    "view cleared · session {} still active",
                                    &app.session_id[..8.min(app.session_id.len())]
                                ),
                                header: Some("ui".into()),
                                running: false,
                                ok: None,
                            });
                        }
                        KeyAction::NewChat => {
                            let effect = dispatch_slash(app, &session, "/new");
                            if let SlashEffect::SwitchSession(s) = effect {
                                session = s;
                                rebuild_from_session(app, &session);
                            }
                        }
                        KeyAction::CopyLast => {
                            let _ = dispatch_slash(app, &session, "/copy");
                        }
                        KeyAction::PermissionsHelp => {
                            let _ = dispatch_slash(app, &session, "/permissions");
                        }
                        KeyAction::HistorySearch => {
                            // Ctrl+R: jump into draft history (Codex-style history search entry).
                            if !app.draft_history.is_empty() {
                                let next = app.draft_history.len().saturating_sub(1);
                                app.draft_index = Some(next);
                                app.input = app.draft_history[next].clone();
                                app.cursor = app.input.chars().count();
                                app.status = "history search · ↑/↓ to browse".into();
                            } else {
                                app.status = "history search · (empty)".into();
                            }
                        }
                        KeyAction::GoalHelp => {
                            let _ = dispatch_slash(app, &session, "/goal");
                        }
                        KeyAction::Newline => insert_char(app, '\n'),
                        KeyAction::Send => {
                            if in_mention_mode(app) && !app.mention_candidates.is_empty() {
                                insert_mention_selection(app);
                                continue;
                            }
                            if in_slash_mode(app) {
                                let matches = matching_commands(app.input.trim());
                                if let Some(cmd) = matches.get(app.slash_index).copied() {
                                    app.input = format!("{cmd} ");
                                    app.cursor = app.input.chars().count();
                                    app.slash_index = 0;
                                    continue;
                                }
                            }
                            let text = app.input.trim().to_string();
                            if text.is_empty() {
                                continue;
                            }
                            // While busy: Enter injects; Tab queues (handled separately).
                            if app.busy {
                                app.pending_queue.push(text.clone());
                                app.input.clear();
                                app.cursor = 0;
                                app.status = format!(
                                    "queued ({}) · will run after current turn",
                                    app.pending_queue.len()
                                );
                                app.lines.push(UiLine {
                                    kind: CellKind::System,
                                    text: format!(
                                        "queued: {}",
                                        text.chars().take(120).collect::<String>()
                                    ),
                                    header: Some("queue".into()),
                                    running: false,
                                    ok: None,
                                });
                                continue;
                            }
                            app.input.clear();
                            app.cursor = 0;
                            app.draft_index = None;
                            app.last_esc = None;
                            app.mention_candidates.clear();
                            if !text.starts_with('/') && !text.starts_with('!') {
                                app.draft_history.push(text.clone());
                            }
                            // !cmd — local shell under current permissions (Codex).
                            if let Some(cmd) = text.strip_prefix('!').map(str::trim) {
                                if cmd.is_empty() {
                                    continue;
                                }
                                match app.runtime.bg.spawn(cmd, Path::new(&app.cwd)) {
                                    Ok(id) => {
                                        app.lines.push(UiLine {
                                            kind: CellKind::Terminal,
                                            text: format!("bg #{id} started: {cmd}"),
                                            header: Some("shell".into()),
                                            running: false,
                                            ok: Some(true),
                                        });
                                    }
                                    Err(e) => app.lines.push(UiLine {
                                        kind: CellKind::Error,
                                        text: format!("shell failed: {e}"),
                                        header: Some("shell".into()),
                                        running: false,
                                        ok: Some(false),
                                    }),
                                }
                                continue;
                            }
                            // @path — mention file into prompt (Codex).
                            let text = if let Some(rest) = text.strip_prefix('@') {
                                let path = rest.trim();
                                if path.is_empty() {
                                    text
                                } else {
                                    format!("Please inspect this file/path: {path}\n\n(Attached via @mention)")
                                }
                            } else {
                                text
                            };
                            if text.starts_with('/') {
                                let effect = dispatch_slash(app, &session, &text);
                                match effect {
                                    SlashEffect::Quit => break,
                                    SlashEffect::SwitchSession(s) => {
                                        session = s;
                                        if text.starts_with("/clear") {
                                            // keep current cleared banner
                                        } else {
                                            rebuild_from_session(app, &session);
                                        }
                                    }
                                    SlashEffect::QueuePrompt(prompt) => {
                                        if let Err(e) =
                                            start_turn(app, agent, &session, &event_tx, prompt)
                                                .await
                                        {
                                            app.lines.push(UiLine {
                                                kind: CellKind::Error,
                                                text: format!("{e}"),
                                                header: Some("error".into()),
                                                running: false,
                                                ok: Some(false),
                                            });
                                        }
                                    }
                                    SlashEffect::None => {}
                                }
                                continue;
                            }
                            if let Err(e) = start_turn(app, agent, &session, &event_tx, text).await
                            {
                                app.lines.push(UiLine {
                                    kind: CellKind::Error,
                                    text: format!("{e}"),
                                    header: Some("error".into()),
                                    running: false,
                                    ok: Some(false),
                                });
                            }
                        }
                        KeyAction::SlashComplete => {
                            // Tab while busy queues the composer contents (Codex).
                            if app.busy && !app.input.trim().is_empty() {
                                let text = app.input.trim().to_string();
                                app.pending_queue.push(text.clone());
                                app.input.clear();
                                app.cursor = 0;
                                app.status = format!("queued ({})", app.pending_queue.len());
                                app.lines.push(UiLine {
                                    kind: CellKind::System,
                                    text: format!("queued: {text}"),
                                    header: Some("queue".into()),
                                    running: false,
                                    ok: None,
                                });
                                continue;
                            }
                            if in_mention_mode(app) && !app.mention_candidates.is_empty() {
                                insert_mention_selection(app);
                                continue;
                            }
                            let before = app.input.clone();
                            if let Some(completed) = autocomplete_slash(&before) {
                                app.input = completed;
                                app.cursor = app.input.chars().count();
                            } else {
                                let matches = matching_commands(before.trim());
                                if let Some(cmd) = matches.get(app.slash_index).copied() {
                                    app.input = format!("{cmd} ");
                                    app.cursor = app.input.chars().count();
                                    app.slash_index = 0;
                                } else if !matches.is_empty() {
                                    app.lines.push(UiLine {
                                        kind: CellKind::System,
                                        text: matches.join("  "),
                                        header: Some("slash".into()),
                                        running: false,
                                        ok: None,
                                    });
                                }
                            }
                        }
                        KeyAction::Backspace => {
                            delete_before_cursor(app);
                            refresh_completions(app);
                        }
                        KeyAction::Delete => {
                            delete_at_cursor(app);
                            refresh_completions(app);
                        }
                        KeyAction::CursorLeft => {
                            if app.cursor > 0 {
                                app.cursor -= 1;
                            }
                        }
                        KeyAction::CursorRight => {
                            let len = app.input.chars().count();
                            if app.cursor < len {
                                app.cursor += 1;
                            }
                        }
                        KeyAction::CursorHome => app.cursor = 0,
                        KeyAction::CursorEnd => app.cursor = app.input.chars().count(),
                        KeyAction::InsertChar(c) => {
                            app.draft_index = None;
                            // Minimal vim: normal mode hjkl / i / a / 0 / $
                            if app.vim_mode && !app.vim_insert && !app.busy {
                                match c {
                                    'i' => {
                                        app.vim_insert = true;
                                        app.status = "vim INSERT".into();
                                    }
                                    'a' => {
                                        app.vim_insert = true;
                                        let len = app.input.chars().count();
                                        if app.cursor < len {
                                            app.cursor += 1;
                                        }
                                        app.status = "vim INSERT".into();
                                    }
                                    'h' if app.cursor > 0 => app.cursor -= 1,
                                    'h' => {}
                                    'l' => {
                                        let len = app.input.chars().count();
                                        if app.cursor < len {
                                            app.cursor += 1;
                                        }
                                    }
                                    '0' => app.cursor = 0,
                                    '$' => app.cursor = app.input.chars().count(),
                                    _ => {}
                                }
                                continue;
                            }
                            insert_char(app, c);
                            refresh_completions(app);
                        }
                        KeyAction::ScrollUp => {
                            if in_mention_mode(app) && !app.mention_candidates.is_empty() {
                                if app.mention_index > 0 {
                                    app.mention_index -= 1;
                                }
                                continue;
                            }
                            if in_slash_mode(app) {
                                if app.slash_index > 0 {
                                    app.slash_index -= 1;
                                }
                                continue;
                            }
                            // Transcript focus (or empty composer with no draft hist): scroll chat.
                            if app.focus == FocusPane::Transcript
                                || (app.focus == FocusPane::Composer
                                    && app.input.is_empty()
                                    && app.draft_index.is_none()
                                    && app.draft_history.is_empty())
                            {
                                scroll_transcript(app, -1);
                                continue;
                            }
                            if app.focus == FocusPane::Composer
                                && (app.input.is_empty() || app.draft_index.is_some())
                                && !app.draft_history.is_empty()
                            {
                                let next = match app.draft_index {
                                    None => app.draft_history.len().saturating_sub(1),
                                    Some(0) => 0,
                                    Some(i) => i.saturating_sub(1),
                                };
                                app.draft_index = Some(next);
                                app.input = app.draft_history[next].clone();
                                app.cursor = app.input.chars().count();
                                continue;
                            }
                            scroll_transcript(app, -1);
                        }
                        KeyAction::ScrollDown => {
                            if in_mention_mode(app) && !app.mention_candidates.is_empty() {
                                let max = app.mention_candidates.len().saturating_sub(1);
                                if app.mention_index < max {
                                    app.mention_index += 1;
                                }
                                continue;
                            }
                            if in_slash_mode(app) {
                                let n = matching_commands(app.input.trim()).len();
                                if n > 0 && app.slash_index + 1 < n {
                                    app.slash_index += 1;
                                }
                                continue;
                            }
                            if app.focus == FocusPane::Composer {
                                if let Some(i) = app.draft_index {
                                    if i + 1 >= app.draft_history.len() {
                                        app.draft_index = None;
                                        app.input.clear();
                                        app.cursor = 0;
                                    } else {
                                        app.draft_index = Some(i + 1);
                                        app.input = app.draft_history[i + 1].clone();
                                        app.cursor = app.input.chars().count();
                                    }
                                    continue;
                                }
                            }
                            scroll_transcript(app, 1);
                        }
                        KeyAction::PageUp => {
                            scroll_transcript(app, -(app.transcript_view_h.max(1) as i32));
                            app.focus = FocusPane::Transcript;
                        }
                        KeyAction::PageDown => {
                            scroll_transcript(app, app.transcript_view_h.max(1) as i32);
                            app.focus = FocusPane::Transcript;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn dispatch_slash(app: &mut AppState, session: &Arc<RwLock<Session>>, text: &str) -> SlashEffect {
    let mut ctx = SlashCtx {
        runtime: &app.runtime,
        session,
        lines: &mut app.lines,
        session_id: &mut app.session_id,
        model: &mut app.model,
        cwd: &app.cwd,
        show_thinking: &mut app.show_thinking,
        sidebar: &mut app.sidebar,
        has_api_key: &mut app.has_api_key,
        status: &mut app.status,
        skill_names: &app.skill_names,
        plugin_names: &app.plugin_names,
        should_quit: &mut app.should_quit,
    };
    handle_slash(&mut ctx, text)
}

async fn start_turn(
    app: &mut AppState,
    agent: &AgentLoop,
    session: &Arc<RwLock<Session>>,
    event_tx: &mpsc::Sender<AgentEvent>,
    text: String,
) -> Result<()> {
    if !app.has_api_key {
        app.lines.push(UiLine {
            kind: CellKind::Error,
            text: "Set an API key first: /apikey <KEY>".into(),
            header: Some("setup".into()),
            running: false,
            ok: Some(false),
        });
        return Ok(());
    }
    app.lines.push(UiLine {
        kind: CellKind::User,
        text: text.clone(),
        header: None,
        running: false,
        ok: None,
    });
    app.busy = true;
    app.streaming = false;
    app.status = format!("working · {}", app.session_id);
    let handle = agent
        .run_turn(session.clone(), text, event_tx.clone())
        .await?;
    app.handle = Some(handle);
    Ok(())
}

fn insert_char(app: &mut AppState, c: char) {
    app.focus = FocusPane::Composer;
    let mut chars: Vec<char> = app.input.chars().collect();
    let idx = app.cursor.min(chars.len());
    chars.insert(idx, c);
    app.input = chars.into_iter().collect();
    app.cursor = idx + 1;
}

fn delete_before_cursor(app: &mut AppState) {
    if app.cursor == 0 {
        return;
    }
    let mut chars: Vec<char> = app.input.chars().collect();
    let idx = app.cursor - 1;
    if idx < chars.len() {
        chars.remove(idx);
        app.input = chars.into_iter().collect();
        app.cursor = idx;
    }
}

fn delete_at_cursor(app: &mut AppState) {
    let mut chars: Vec<char> = app.input.chars().collect();
    if app.cursor < chars.len() {
        chars.remove(app.cursor);
        app.input = chars.into_iter().collect();
    }
}

fn visible_line_count(app: &AppState) -> usize {
    app.lines
        .iter()
        .filter(|l| app.show_thinking || !matches!(l.kind, CellKind::Thinking))
        .count()
}

fn max_transcript_scroll(app: &AppState) -> usize {
    visible_line_count(app).saturating_sub(app.transcript_view_h.max(1))
}

fn clamp_transcript_scroll(app: &mut AppState) {
    let max = max_transcript_scroll(app);
    if app.stick_to_bottom || app.transcript_scroll > max {
        app.transcript_scroll = max;
    }
}

fn scroll_transcript(app: &mut AppState, delta: i32) {
    let max = max_transcript_scroll(app) as i32;
    let next = (app.transcript_scroll as i32 + delta).clamp(0, max) as usize;
    app.transcript_scroll = next;
    app.stick_to_bottom = next >= max as usize;
    app.focus = FocusPane::Transcript;
    app.status = if app.stick_to_bottom {
        "transcript · following latest".into()
    } else {
        format!(
            "transcript · {}/{}  (click composer to type)",
            app.transcript_scroll + 1,
            visible_line_count(app).max(1)
        )
    };
}

fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

fn handle_mouse(app: &mut AppState, mouse: crossterm::event::MouseEvent) {
    let col = mouse.column;
    let row = mouse.row;
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if rect_contains(app.panes.transcript, col, row) {
                app.focus = FocusPane::Transcript;
                app.status = "transcript focused · wheel or ↑↓ to scroll".into();
            } else if rect_contains(app.panes.composer, col, row) {
                app.focus = FocusPane::Composer;
                app.status = "composer focused · type to chat".into();
            } else if app
                .panes
                .sidebar
                .is_some_and(|s| rect_contains(s, col, row))
            {
                app.focus = FocusPane::Sidebar;
                app.status = "sidebar focused".into();
            }
        }
        MouseEventKind::ScrollUp
            if rect_contains(app.panes.transcript, col, row)
                || app.focus == FocusPane::Transcript =>
        {
            scroll_transcript(app, -3);
        }
        MouseEventKind::ScrollDown
            if rect_contains(app.panes.transcript, col, row)
                || app.focus == FocusPane::Transcript =>
        {
            scroll_transcript(app, 3);
        }
        _ => {}
    }
}

fn pin_transcript_if_following(app: &mut AppState) {
    if app.stick_to_bottom {
        clamp_transcript_scroll(app);
    }
}

fn apply_event(app: &mut AppState, ev: AgentEvent) {
    match ev {
        AgentEvent::TextDelta(t) => {
            app.streaming = true;
            if let Some(last) = app.lines.last_mut() {
                if matches!(last.kind, CellKind::Assistant) && !last.running {
                    last.text.push_str(&t);
                    pin_transcript_if_following(app);
                    return;
                }
            }
            app.lines.push(UiLine {
                kind: CellKind::Assistant,
                text: t,
                header: None,
                running: false,
                ok: None,
            });
            pin_transcript_if_following(app);
        }
        AgentEvent::ReasoningDelta(t) => {
            if !app.show_thinking {
                return;
            }
            app.streaming = true;
            if let Some(last) = app.lines.last_mut() {
                if matches!(last.kind, CellKind::Thinking) {
                    last.text.push_str(&t);
                    return;
                }
            }
            app.lines.push(UiLine {
                kind: CellKind::Thinking,
                text: t,
                header: Some("think".into()),
                running: false,
                ok: None,
            });
        }
        AgentEvent::ThoughtTick { t, note } => {
            app.lines.push(UiLine {
                kind: CellKind::Ctm,
                text: note,
                header: Some(format!("ctm:{t}")),
                running: false,
                ok: None,
            });
        }
        AgentEvent::ToolStarted { name, call_id } => {
            app.streaming = false;
            let kind = categorize_tool(&name);
            app.lines.push(UiLine {
                kind,
                text: format!("{name}  ·  {call_id}"),
                header: Some(kind.label().into()),
                running: true,
                ok: None,
            });
        }
        AgentEvent::ToolFinished {
            name, ok, preview, ..
        } => {
            let kind = categorize_tool(&name);
            if let Some(last) = app
                .lines
                .iter_mut()
                .rev()
                .find(|l| l.running && l.kind == kind)
            {
                last.running = false;
                last.ok = Some(ok);
                last.text = format!("{name}: {preview}");
                last.header = Some(kind.label().into());
                return;
            }
            app.lines.push(UiLine {
                kind,
                text: format!("{name}: {preview}"),
                header: Some(kind.label().into()),
                running: false,
                ok: Some(ok),
            });
        }
        AgentEvent::Error(e) => {
            app.lines.push(UiLine {
                kind: CellKind::Error,
                text: e,
                header: Some("error".into()),
                running: false,
                ok: Some(false),
            });
            app.busy = false;
            app.streaming = false;
            app.awaiting_approval = false;
            app.awaiting_approval_request_id = None;
            app.status = "error".into();
        }
        AgentEvent::Done | AgentEvent::TurnEnded(_) => {
            for line in &mut app.lines {
                line.running = false;
            }
            app.busy = false;
            app.streaming = false;
            app.awaiting_approval = false;
            app.awaiting_approval_request_id = None;
            let perm = *app.runtime.permissions.read();
            app.status = format!("{} · {} · {}", app.model, perm.label(), app.cwd);
            app.handle = None;
            // Drain one queued item into the composer for the next turn.
            if let Some(next) = app.pending_queue.first().cloned() {
                app.pending_queue.remove(0);
                app.input = next;
                app.cursor = app.input.chars().count();
                app.status = format!(
                    "dequeued · {} remaining · press Enter",
                    app.pending_queue.len()
                );
            }
        }
        AgentEvent::TurnStarted(_) => {
            app.busy = true;
            app.streaming = false;
        }
        AgentEvent::ApprovalNeeded {
            request_id,
            call_id,
            name,
            summary,
        } => {
            app.awaiting_approval = true;
            app.awaiting_approval_request_id = Some(request_id);
            app.lines.push(UiLine {
                kind: CellKind::Error,
                text: format!("approve tool {name}? [y/n]  {summary}  ({call_id})"),
                header: Some("approval".into()),
                running: false,
                ok: None,
            });
            app.status = format!("approve {name}? y/n");
        }
    }
    pin_transcript_if_following(app);
}

fn draw(f: &mut ratatui::Frame, app: &mut AppState) {
    let root = if app.sidebar {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(78), Constraint::Percentage(22)])
            .split(f.area())
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(100)])
            .split(f.area())
    };

    let input_h = composer_height(&app.input);
    let main = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(input_h),
            Constraint::Length(1),
        ])
        .split(root[0]);

    app.panes.transcript = main[0];
    app.panes.composer = main[1];
    app.panes.status = main[2];
    app.panes.sidebar = if app.sidebar && root.len() > 1 {
        Some(root[1])
    } else {
        None
    };

    f.render_widget(Clear, main[0]);
    draw_transcript(f, main[0], app);
    if in_slash_mode(app) {
        draw_slash_popup(f, main[0], app);
    } else if in_mention_mode(app) && !app.mention_candidates.is_empty() {
        draw_mention_popup(f, main[0], app);
    }
    draw_composer(f, main[1], app);
    draw_status(f, main[2], app);

    if app.sidebar && root.len() > 1 {
        draw_sidebar(f, root[1], app);
    }
}

fn composer_height(input: &str) -> u16 {
    let lines = input.chars().filter(|c| *c == '\n').count() + 1;
    (lines as u16 + 2).clamp(3, 8)
}

fn draw_transcript(f: &mut ratatui::Frame, area: Rect, app: &mut AppState) {
    let tick = app.spin_tick;
    let streaming = app.streaming;

    // Inner rows available for messages (exclude border).
    app.transcript_view_h = area.height.saturating_sub(2) as usize;
    let total = visible_line_count(app);
    if app.stick_to_bottom {
        clamp_transcript_scroll(app);
    } else {
        let max_off = total.saturating_sub(app.transcript_view_h.max(1));
        if app.transcript_scroll > max_off {
            app.transcript_scroll = max_off;
        }
    }

    let view_h = app.transcript_view_h.max(1);
    let start = if total == 0 {
        0
    } else {
        app.transcript_scroll.min(total.saturating_sub(1))
    };
    let end = (start + view_h).min(total);

    let visible: Vec<&UiLine> = app
        .lines
        .iter()
        .filter(|l| app.show_thinking || !matches!(l.kind, CellKind::Thinking))
        .collect();
    let window = if visible.is_empty() {
        &[][..]
    } else {
        &visible[start..end]
    };

    let last_global = total.saturating_sub(1);
    let items: Vec<ListItem> = window
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let global_idx = start + i;
            if app.raw_mode {
                ListItem::new(Line::from(Span::raw(format!(
                    "{}{}",
                    l.header
                        .as_ref()
                        .map(|h| format!("[{h}] "))
                        .unwrap_or_default(),
                    l.text
                ))))
            } else {
                ListItem::new(render_cell(l, tick, streaming && global_idx == last_global))
            }
        })
        .collect();

    let focused = app.focus == FocusPane::Transcript;
    let pos = if total == 0 {
        "0/0".into()
    } else {
        format!("{}/{}", (start + 1).min(total), total)
    };
    let title = if app.busy {
        let spin = spinner_frame(tick);
        let bar = activity_bar(tick, 10);
        Line::from(vec![
            Span::styled(format!(" {spin} "), Style::default().fg(Color::Yellow)),
            Span::styled("dsh ", accent_style()),
            Span::styled(
                format!("· {bar} · {pos} "),
                Style::default().fg(Color::Yellow),
            ),
            if focused {
                Span::styled("● focused ", Style::default().fg(Color::Cyan))
            } else {
                Span::styled("click to focus · wheel scroll ", dim_style())
            },
        ])
    } else {
        Line::from(vec![
            Span::styled(" dsh ", accent_style()),
            Span::styled(format!("· transcript {pos} "), dim_style()),
            if focused {
                Span::styled(
                    "● focused · wheel/↑↓ scroll ",
                    Style::default().fg(Color::Cyan),
                )
            } else {
                Span::styled("click to focus · wheel scroll ", dim_style())
            },
        ])
    };

    let border = if focused {
        Style::default().fg(Color::Cyan)
    } else if app.busy {
        Style::default().fg(busy_border_color(tick))
    } else {
        border_style()
    };

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border)
            .title(title),
    );
    f.render_widget(list, area);
}

fn render_cell(line: &UiLine, tick: u64, show_stream_caret: bool) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();

    if line.running {
        let glyph = running_glyph(line.kind, tick);
        spans.push(Span::styled(
            format!("{glyph} "),
            Style::default()
                .fg(line.kind.accent())
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        let (bullet, bullet_style) = line.kind.bullet(line.ok);
        spans.push(Span::styled(format!("{bullet} "), bullet_style));
    }

    match line.kind {
        CellKind::User => {
            spans.push(Span::styled(
                "❯ ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(line.text.clone(), line.kind.body_style()));
        }
        CellKind::Assistant => {
            spans.push(Span::styled(line.text.clone(), line.kind.body_style()));
            if show_stream_caret {
                let (ch, style) = stream_caret(tick, true);
                spans.push(Span::styled(ch.to_string(), style));
            }
        }
        CellKind::Thinking => {
            let wave = running_glyph(CellKind::Thinking, tick);
            spans.push(Span::styled(
                format!("{wave} think "),
                line.kind.label_style().add_modifier(Modifier::DIM),
            ));
            spans.push(Span::styled(line.text.clone(), line.kind.body_style()));
            if show_stream_caret {
                let (ch, style) = stream_caret(tick, true);
                spans.push(Span::styled(ch.to_string(), style));
            }
        }
        other => {
            if let Some(label) = &line.header {
                spans.push(Span::styled(format!("{label} "), other.label_style()));
            } else if !matches!(other, CellKind::System) {
                spans.push(Span::styled(
                    format!("{} ", other.label()),
                    other.label_style(),
                ));
            } else {
                // Continuation / formatted help lines — keep column alignment.
                spans.push(Span::raw("      "));
            }
            let mut body = line.text.clone();
            if line.running {
                body.push_str(&running_suffix(tick));
            }
            spans.push(Span::styled(
                body,
                Style::default()
                    .fg(if line.running {
                        other.accent()
                    } else if line.text.starts_with("──")
                        || line
                            .text
                            .chars()
                            .all(|c| c.is_ascii_uppercase() || c == ' ')
                            && line.text.len() < 24
                            && !line.text.is_empty()
                    {
                        Color::Cyan
                    } else {
                        Color::Rgb(190, 190, 190)
                    })
                    .add_modifier(if line.running {
                        Modifier::empty()
                    } else if line
                        .text
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c == ' ')
                        && line.text.len() < 24
                        && !line.text.is_empty()
                    {
                        Modifier::BOLD
                    } else {
                        Modifier::DIM
                    }),
            ));
        }
    }

    Line::from(spans)
}

fn draw_composer(f: &mut ratatui::Frame, area: Rect, app: &AppState) {
    let spin = spinner_frame(app.spin_tick);
    let focused = app.focus == FocusPane::Composer;
    let title = if app.busy {
        let bar = activity_bar(app.spin_tick, 8);
        Line::from(vec![
            Span::styled(format!(" {spin} "), Style::default().fg(Color::Yellow)),
            Span::styled("working ", composer_title_style(true)),
            Span::styled(format!("{bar} "), Style::default().fg(Color::Yellow)),
            Span::styled("Esc cancel ", dim_style()),
        ])
    } else {
        Line::from(vec![
            Span::styled(" › ", accent_style()),
            Span::styled("message ", composer_title_style(false)),
            if focused {
                Span::styled(
                    "· ● focused · Enter send ",
                    Style::default().fg(Color::Cyan),
                )
            } else {
                Span::styled("· click to focus · Enter send ", dim_style())
            },
        ])
    };

    let prompt = composer_line(app);

    let p = Paragraph::new(prompt)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(if app.busy {
                    Style::default().fg(busy_border_color(app.spin_tick))
                } else if focused {
                    Style::default().fg(Color::Cyan)
                } else if app.cursor_on {
                    Style::default().fg(Color::Rgb(40, 100, 120))
                } else {
                    Style::default().fg(Color::Rgb(40, 80, 90))
                })
                .title(title),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn composer_line(app: &AppState) -> Line<'static> {
    let mut spans = vec![Span::styled("› ", accent_style())];
    let (cursor_ch, cursor_style) = cursor_block(app.cursor_on && !app.busy);

    if app.input.is_empty() && !app.busy {
        spans.push(Span::styled(cursor_ch.to_string(), cursor_style));
        let hint = if !app.has_api_key {
            " /apikey sk-...  then describe your task"
        } else if app.focus != FocusPane::Composer {
            " click here to type · or press Esc"
        } else {
            " Ask anything…  (/help for commands)"
        };
        spans.push(Span::styled(
            hint,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ));
        return Line::from(spans);
    }

    if app.busy && app.input.is_empty() {
        spans.push(Span::styled(
            format!(
                "{spin} waiting for model…",
                spin = spinner_frame(app.spin_tick)
            ),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::ITALIC),
        ));
        return Line::from(spans);
    }

    let chars: Vec<char> = app.input.chars().collect();
    let cursor = app.cursor.min(chars.len());
    let before: String = chars[..cursor].iter().collect();
    let after: String = chars[cursor..].iter().collect();

    if !before.is_empty() {
        spans.push(Span::raw(before));
    }
    if !app.busy {
        spans.push(Span::styled(cursor_ch.to_string(), cursor_style));
    }
    if !after.is_empty() {
        spans.push(Span::raw(after));
    } else if app.busy {
        spans.push(Span::styled(
            format!(" {}", spinner_frame(app.spin_tick)),
            Style::default().fg(Color::Yellow),
        ));
    }

    Line::from(spans)
}

fn draw_status(f: &mut ratatui::Frame, area: Rect, app: &AppState) {
    let sid = &app.session_id[..8.min(app.session_id.len())];
    let perm = *app.runtime.permissions.read();
    let focus = match app.focus {
        FocusPane::Transcript => "chat",
        FocusPane::Composer => "input",
        FocusPane::Sidebar => "side",
    };
    let cwd_short = shorten_path(&app.cwd, 28);
    let mut spans = vec![
        Span::styled(" ", dim_style()),
        Span::styled(app.model.clone(), accent_style()),
        Span::styled(" · ", dim_style()),
        Span::styled(perm.label().to_string(), Style::default().fg(Color::Yellow)),
        Span::styled(" · ", dim_style()),
        Span::styled(format!("focus:{focus}"), Style::default().fg(Color::Cyan)),
        Span::styled(" · ", dim_style()),
        Span::styled(cwd_short, dim_style()),
        Span::styled(" · ", dim_style()),
        Span::styled(sid.to_string(), dim_style()),
        Span::styled(" · ", dim_style()),
    ];
    if app.busy || app.streaming {
        let spin = spinner_frame(app.spin_tick);
        spans.push(Span::styled(
            format!("{spin} "),
            Style::default().fg(Color::Yellow),
        ));
        spans.push(Span::styled(
            activity_bar(app.spin_tick, 8),
            Style::default().fg(Color::Yellow),
        ));
        spans.push(Span::styled(" ", dim_style()));
    }
    spans.push(Span::styled(app.status.clone(), dim_style()));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn shorten_path(path: &str, max: usize) -> String {
    let chars: Vec<char> = path.chars().collect();
    if chars.len() <= max {
        return path.to_string();
    }
    let keep = max.saturating_sub(1);
    let tail: String = chars[chars.len().saturating_sub(keep)..].iter().collect();
    format!("…{tail}")
}

fn draw_sidebar(f: &mut ratatui::Frame, area: Rect, app: &AppState) {
    let perm = *app.runtime.permissions.read();
    let mut lines = vec![
        Line::from(Span::styled(" outer ", accent_style())),
        Line::from(Span::styled(format!(" {}", app.model), dim_style())),
        Line::from(Span::styled(
            format!(" perms {}", perm.label()),
            Style::default().fg(Color::Yellow),
        )),
        Line::from(""),
        Line::from(Span::styled(
            " legend ",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        legend_line(CellKind::Terminal),
        legend_line(CellKind::Skill),
        legend_line(CellKind::Plugin),
        legend_line(CellKind::Plan),
        legend_line(CellKind::Filesystem),
        legend_line(CellKind::Web),
        legend_line(CellKind::Learn),
        legend_line(CellKind::Ctm),
        Line::from(""),
        Line::from(Span::styled(" skills ", CellKind::Skill.label_style())),
    ];
    for s in app.skill_names.iter().take(10) {
        lines.push(Line::from(vec![
            Span::styled(" · ", CellKind::Skill.label_style()),
            Span::raw(s.clone()),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " plugins ",
        CellKind::Plugin.label_style(),
    )));
    for p in app.plugin_names.iter().take(10) {
        lines.push(Line::from(vec![
            Span::styled(" · ", CellKind::Plugin.label_style()),
            Span::raw(p.clone()),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " tips ",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(" click pane = focus", dim_style())));
    lines.push(Line::from(Span::styled(
        " wheel = scroll chat",
        dim_style(),
    )));
    lines.push(Line::from(Span::styled(" /help = commands", dim_style())));
    lines.push(Line::from(Span::styled(
        " Esc = back to input",
        dim_style(),
    )));

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style())
            .title(Line::from(Span::styled(" context ", dim_style()))),
    );
    f.render_widget(para, area);
}

fn legend_line(kind: CellKind) -> Line<'static> {
    Line::from(vec![
        Span::styled(" • ", Style::default().fg(kind.accent())),
        Span::styled(kind.label().to_string(), kind.label_style()),
    ])
}

fn in_slash_mode(app: &AppState) -> bool {
    let t = app.input.trim_end();
    t.starts_with('/') && !t.contains(char::is_whitespace)
}

fn in_mention_mode(app: &AppState) -> bool {
    app.input.starts_with('@') && !app.input[1..].contains(char::is_whitespace)
}

fn refresh_completions(app: &mut AppState) {
    if in_mention_mode(app) {
        let filter = app.input.strip_prefix('@').unwrap_or("").to_lowercase();
        app.mention_candidates = fuzzy_cwd_files(&app.cwd, &filter, 40);
        if app.mention_index >= app.mention_candidates.len() {
            app.mention_index = 0;
        }
    } else {
        app.mention_candidates.clear();
        app.mention_index = 0;
    }
    if in_slash_mode(app) {
        let n = matching_commands(app.input.trim()).len();
        if app.slash_index >= n {
            app.slash_index = 0;
        }
    } else {
        app.slash_index = 0;
    }
}

fn insert_mention_selection(app: &mut AppState) {
    if let Some(path) = app.mention_candidates.get(app.mention_index).cloned() {
        app.input = format!("@{path} ");
        app.cursor = app.input.chars().count();
        app.mention_candidates.clear();
        app.mention_index = 0;
    }
}

fn fuzzy_cwd_files(cwd: &str, filter: &str, max: usize) -> Vec<String> {
    let root = Path::new(cwd);
    let mut out = Vec::new();
    let walker = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !(name == ".git"
                || name == "target"
                || name == "node_modules"
                || name == ".dsh-build2"
                || name.starts_with("target"))
        });
    for entry in walker.flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let s = rel.to_string_lossy().replace('\\', "/");
        if filter.is_empty() || s.to_lowercase().contains(filter) {
            out.push(s);
            if out.len() >= max {
                break;
            }
        }
    }
    out.sort();
    out.truncate(max);
    out
}

fn draw_slash_popup(f: &mut ratatui::Frame, area: Rect, app: &AppState) {
    let matches = matching_commands(app.input.trim());
    if matches.is_empty() {
        return;
    }
    let height = (matches.len() as u16).clamp(1, 8).saturating_add(2);
    let width = area.width.clamp(20, 48);
    let popup = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(area.height.saturating_sub(height)),
        width,
        height,
    };
    let items: Vec<ListItem> = matches
        .iter()
        .enumerate()
        .map(|(i, cmd)| {
            let style = if i == app.slash_index {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Cyan)
            };
            ListItem::new(Line::from(Span::styled(format!(" {cmd}"), style)))
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" slash "),
    );
    f.render_widget(Clear, popup);
    f.render_widget(list, popup);
}

fn draw_mention_popup(f: &mut ratatui::Frame, area: Rect, app: &AppState) {
    let height = (app.mention_candidates.len() as u16)
        .clamp(1, 8)
        .saturating_add(2);
    let width = area.width.clamp(24, 64);
    let popup = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(area.height.saturating_sub(height)),
        width,
        height,
    };
    let items: Vec<ListItem> = app
        .mention_candidates
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let style = if i == app.mention_index {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Yellow)
            };
            ListItem::new(Line::from(Span::styled(format!(" @{path}"), style)))
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(" @files "),
    );
    f.render_widget(Clear, popup);
    f.render_widget(list, popup);
}
