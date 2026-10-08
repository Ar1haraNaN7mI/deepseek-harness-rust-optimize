//! DEEP DIVE: an original cinematic department-identification sequence.
//! Motion is decorative; identity and loaded inventories come from a real snapshot.

use crate::startup_audio::{Cue, StartupAudio};
use crate::terminal::TerminalGuard;
use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use dsh_core::{StartupProfile, StartupSection};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        canvas::{Canvas, Context, Line as CanvasLine, Points},
        Block, Paragraph,
    },
    Frame, Terminal,
};
use std::{
    f64::consts::TAU,
    io::{stdin, stdout, IsTerminal, Stdout},
    sync::OnceLock,
    time::{Duration, Instant},
};

const DURATIONS: [f64; 6] = [2.1, 1.8, 2.2, 3.4, 1.8, 2.5];
const TOTAL_DURATION: f64 = 13.8;
const TITLES: [&str; 6] = [
    "DSH 启动",
    "本地工作区接入",
    "操作员资料",
    "本机已加载清单",
    "启动清单就绪",
    "欢迎进入 DSH",
];
const SUBTITLES: [&str; 6] = [
    "DSH STARTUP",
    "WORKSPACE CONTEXT",
    "OPERATOR PROFILE",
    "LOADED INVENTORY",
    "STARTUP INVENTORY READY",
    "WELCOME TO DSH",
];

/// Immutable data captured before playback. An absent inventory is deliberately
/// distinct from a successfully loaded, empty inventory.
#[derive(Debug, Clone, Default)]
pub struct StartupContext {
    pub profile: StartupProfile,
    pub skill_names: Vec<String>,
    pub plugin_names: Vec<String>,
    pub inventory_loaded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Continue,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    None,
    Confirm,
    Select(usize),
    Previous,
    Next,
    Skip,
    Quit,
    Mute,
}

fn action(key: KeyEvent) -> Action {
    if key.kind != KeyEventKind::Press {
        return Action::None;
    }
    match key.code {
        KeyCode::Char('c' | 'C') if key.modifiers.contains(KeyModifiers::CONTROL) => Action::Quit,
        KeyCode::Esc => Action::Skip,
        KeyCode::Enter | KeyCode::Char(' ') => Action::Confirm,
        KeyCode::Char('1') => Action::Select(0),
        KeyCode::Char('2') => Action::Select(1),
        KeyCode::Char('3') => Action::Select(2),
        KeyCode::Left => Action::Previous,
        KeyCode::Right => Action::Next,
        KeyCode::Char('m' | 'M') => Action::Mute,
        _ => Action::None,
    }
}

struct Sequence {
    phase: usize,
    elapsed: f64,
    phase_age: f64,
    ambient: f64,
    interactive: bool,
    playing: bool,
    focus: usize,
    pulse_age: f64,
    pulse_origin: (f64, f64),
}

impl Sequence {
    fn new(interactive: bool) -> Self {
        Self {
            phase: 0,
            elapsed: 0.0,
            phase_age: 0.0,
            ambient: 0.0,
            interactive,
            playing: !interactive,
            focus: 0,
            pulse_age: 10.0,
            pulse_origin: (0.5, 0.5),
        }
    }
    fn waiting(&self) -> bool {
        self.interactive && matches!(self.phase, 0 | 2 | 4) && !self.playing
    }
    fn finish_phase(&mut self) {
        self.phase = (self.phase + 1).min(DURATIONS.len());
        self.elapsed = 0.0;
        self.phase_age = 0.0;
        self.playing = !(self.interactive && matches!(self.phase, 0 | 2 | 4));
    }
    fn confirm(&mut self, origin: (f64, f64)) {
        self.pulse_origin = (origin.0.clamp(0.0, 1.0), origin.1.clamp(0.0, 1.0));
        self.pulse_age = 0.0;
        if self.waiting() {
            self.playing = true;
        }
    }
    fn select(&mut self, focus: usize) {
        if focus < 3 {
            self.focus = focus;
            self.pulse_age = 0.0;
            self.pulse_origin = (0.5, 0.5);
        }
    }
    fn move_selection(&mut self, forward: bool) {
        self.select((self.focus + if forward { 1 } else { 2 }) % 3);
    }
    fn tick(&mut self, dt: f64) {
        let dt = dt.max(0.0);
        self.ambient += dt;
        self.phase_age += dt;
        self.pulse_age += dt;
        if self.phase >= DURATIONS.len() || self.waiting() {
            return;
        }
        self.elapsed += dt;
        while self.phase < DURATIONS.len() && self.elapsed >= DURATIONS[self.phase] {
            let remainder = self.elapsed - DURATIONS[self.phase];
            self.finish_phase();
            if self.waiting() || self.phase >= DURATIONS.len() {
                break;
            }
            self.elapsed = remainder;
            self.phase_age = remainder;
        }
    }
    fn tick_with_narration(&mut self, dt: f64, speaking: bool) {
        if speaking
            && !self.waiting()
            && self.phase < DURATIONS.len()
            && self.elapsed + dt >= DURATIONS[self.phase]
        {
            self.ambient += dt.max(0.0);
            self.phase_age += dt.max(0.0);
            self.pulse_age += dt.max(0.0);
            self.elapsed = DURATIONS[self.phase];
        } else {
            self.tick(dt);
        }
    }
    fn progress(&self) -> f64 {
        (DURATIONS[..self.phase.min(DURATIONS.len())]
            .iter()
            .sum::<f64>()
            + self.elapsed)
            / TOTAL_DURATION
    }
}

fn mouse_confirms(mouse: MouseEvent) -> bool {
    mouse.kind == MouseEventKind::Down(MouseButton::Left)
}

fn truthy(value: Option<String>) -> bool {
    value.is_some_and(|s| {
        !matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no"
        )
    })
}

pub(crate) fn available() -> bool {
    stdin().is_terminal()
        && stdout().is_terminal()
        && std::env::var("TERM").as_deref() != Ok("dumb")
        && std::env::var_os("NO_COLOR").is_none()
        && !truthy(std::env::var("CI").ok())
        && !truthy(std::env::var("DSH_NO_STARTUP").ok())
}

/// Replay without constructing an agent runtime or making any network requests.
pub async fn preview_startup(config: StartupSection) -> Result<()> {
    preview_startup_with_context(config, StartupContext::default()).await
}

/// Preview real data supplied by a caller without loading or executing plugins.
pub async fn preview_startup_with_context(
    config: StartupSection,
    context: StartupContext,
) -> Result<()> {
    if !config.enabled {
        println!("DSH startup preview disabled (--no-startup).");
        return Ok(());
    }
    if !available() {
        println!("DSH / DEEP DIVE — startup preview needs an interactive color terminal.");
        return Ok(());
    }
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    play(&mut terminal, &config, &context).await?;
    Ok(())
}

pub(crate) async fn play(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    config: &StartupSection,
    context: &StartupContext,
) -> Result<Outcome> {
    if !config.enabled || !available() {
        return Ok(Outcome::Continue);
    }
    let speed = if config.speed.is_finite() {
        config.speed.clamp(0.25, 3.0) as f64
    } else {
        1.0
    };
    let mut seq = Sequence::new(config.interactive && !config.reduced_motion);
    if config.reduced_motion {
        seq.phase = 5;
        seq.playing = true;
    }
    let mut audio = StartupAudio::new(config.sound, config.volume);
    let mut last_phase = usize::MAX;
    let mut previous = Instant::now();
    loop {
        if seq.phase >= DURATIONS.len() {
            break;
        }
        if seq.playing && seq.phase != last_phase {
            audio.play_with_voice(
                match seq.phase {
                    0 => Cue::Ignite,
                    1 => Cue::Scan,
                    2 => Cue::Focus,
                    3 => Cue::Route,
                    4 => Cue::Resolve,
                    _ => Cue::Ready,
                },
                fixed_voice(seq.phase, context),
            );
            last_phase = seq.phase;
        }
        terminal.draw(|frame| draw(frame, &seq, config, context, audio.is_muted()))?;
        // Poll without blocking the async executor; redraw at at most 30 FPS.
        if event::poll(Duration::ZERO)? {
            match event::read()? {
                Event::Key(key) => match action(key) {
                    Action::Quit => return Ok(Outcome::Quit),
                    Action::Skip => break,
                    Action::Confirm => seq.confirm((0.5, 0.5)),
                    Action::Select(selection) => seq.select(selection),
                    Action::Previous => seq.move_selection(false),
                    Action::Next => seq.move_selection(true),
                    Action::Mute => {
                        audio.set_muted(!audio.is_muted());
                    }
                    Action::None => {}
                },
                Event::Resize(_, _) => {
                    terminal.autoresize()?;
                }
                Event::Mouse(mouse) if mouse_confirms(mouse) => {
                    let size = terminal.size()?;
                    seq.confirm((
                        f64::from(mouse.column) / f64::from(size.width.max(1)),
                        f64::from(mouse.row) / f64::from(size.height.max(1)),
                    ));
                }
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(33)).await;
        let now = Instant::now();
        seq.tick_with_narration(
            now.duration_since(previous).as_secs_f64() * speed,
            audio.is_speaking(),
        );
        previous = now;
    }
    audio.stop();
    Ok(Outcome::Continue)
}

#[derive(Clone, Copy)]
struct Palette {
    bg: Color,
    fg: Color,
    dim: Color,
    grid: Color,
    accent: Color,
}

impl Palette {
    fn new(theme: &str) -> Self {
        if theme == "light" {
            Self {
                bg: Color::Rgb(243, 247, 250),
                fg: Color::Rgb(25, 38, 49),
                dim: Color::Rgb(83, 103, 115),
                grid: Color::Rgb(180, 202, 211),
                accent: Color::Rgb(20, 114, 150),
            }
        } else {
            Self {
                bg: Color::Rgb(8, 12, 19),
                fg: Color::Rgb(234, 242, 249),
                dim: Color::Rgb(116, 139, 158),
                grid: Color::Rgb(33, 53, 70),
                accent: Color::Rgb(103, 211, 239),
            }
        }
    }
    fn text(self) -> Style {
        Style::default().bg(self.bg).fg(self.fg)
    }
    fn accent(self) -> Style {
        self.text().fg(self.accent)
    }
}

fn row(
    frame: &mut Frame,
    area: Rect,
    y: u16,
    text: impl Into<Line<'static>>,
    style: Style,
    centered: bool,
) {
    if y >= area.height || area.width == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(text.into())
            .style(style)
            .alignment(if centered {
                Alignment::Center
            } else {
                Alignment::Left
            }),
        Rect::new(area.x, area.y + y, area.width, 1),
    );
}

fn display_text(value: &str, columns: usize) -> String {
    let mut result = String::new();
    for character in value.chars().filter(|ch| !ch.is_control()) {
        let mut next = result.clone();
        next.push(character);
        if Line::from(next.as_str()).width() > columns {
            break;
        }
        result = next;
    }
    result
}

/// Decorative decoding of the actual profile. Width is preserved even while
/// Chinese characters are concealed, so surrounding labels never jump.
fn decoded_profile(value: &str, columns: usize, age: f64, row: usize, still: bool) -> String {
    let value = display_text(value, columns);
    let progress = ((age - row as f64 * 0.12) / 0.62).clamp(0.0, 1.0);
    if still || !progress.is_finite() || progress >= 1.0 {
        return value;
    }
    const GLYPHS: &[u8] = b"0123456789/+=<>[]#";
    let count = value.chars().count().max(1);
    let frame = (age.max(0.0) * 24.0) as usize;
    let mut output = String::new();
    for (index, character) in value.chars().enumerate() {
        let width = Line::from(character.to_string()).width();
        if character.is_whitespace()
            || width == 0
            || progress >= (index + 1) as f64 / count as f64
        {
            output.push(character);
        } else {
            for column in 0..width {
                let seed = frame
                    .wrapping_mul(13)
                    .wrapping_add(index.wrapping_mul(7))
                    .wrapping_add(row.wrapping_mul(11))
                    .wrapping_add(column.wrapping_mul(3));
                output.push(GLYPHS[seed % GLYPHS.len()] as char);
            }
        }
    }
    output
}

fn scene_title(phase: usize, context: &StartupContext) -> &'static str {
    if matches!(phase, 3 | 4) && !context.inventory_loaded {
        "本机清单未接入"
    } else {
        TITLES[phase]
    }
}

fn scene_subtitle(phase: usize, context: &StartupContext) -> &'static str {
    if matches!(phase, 3 | 4) && !context.inventory_loaded {
        "INVENTORY UNAVAILABLE"
    } else if phase == 3 {
        "LOADED SKILLS / PLUGINS"
    } else {
        SUBTITLES[phase]
    }
}

fn inventory_summary(context: &StartupContext) -> String {
    if context.inventory_loaded {
        format!(
            "SKILLS {}  /  PLUGINS {}  ·  已加载",
            context.skill_names.len(),
            context.plugin_names.len()
        )
    } else {
        "本机清单未接入 · 不显示估计数量".into()
    }
}

fn draw_inventory(frame: &mut Frame, area: Rect, context: &StartupContext, p: Palette) {
    row(frame, area, 0, inventory_summary(context), p.accent(), true);
    if !context.inventory_loaded {
        return;
    }
    if area.width < 76 || area.height < 12 {
        for (offset, names, label) in [
            (3, &context.skill_names, "SKILL"),
            (2, &context.plugin_names, "PLUGIN"),
        ] {
            let first = names.first().map(String::as_str).unwrap_or("(none loaded)");
            row(
                frame,
                area,
                area.height.saturating_sub(offset),
                display_text(&format!("{label} / {first}"), area.width as usize),
                p.text(),
                true,
            );
        }
        return;
    }
    let column_width = ((area.width - 46) / 2).min(32);
    let visible = area.height.saturating_sub(9).min(7) as usize;
    for (x, names, label) in [
        (area.x + 2, &context.skill_names, "SKILLS"),
        (
            area.right().saturating_sub(column_width + 2),
            &context.plugin_names,
            "PLUGINS",
        ),
    ] {
        let column = Rect::new(x, area.y + 4, column_width, (visible + 3) as u16);
        row(
            frame,
            column,
            0,
            format!("{label} / {}", names.len()),
            p.accent(),
            false,
        );
        if names.is_empty() {
            row(frame, column, 2, "(none loaded)", p.text().fg(p.dim), false);
        }
        for (index, name) in names.iter().take(visible).enumerate() {
            row(
                frame,
                column,
                index as u16 + 2,
                display_text(name, column_width as usize),
                p.text(),
                false,
            );
        }
        if names.len() > visible {
            row(
                frame,
                column,
                visible as u16 + 2,
                format!("+ {} more", names.len() - visible),
                p.text().fg(p.dim),
                false,
            );
        }
    }
    row(
        frame,
        area,
        area.height.saturating_sub(1),
        "启动前已完成挂载 · 动画仅展示清单",
        p.text().fg(p.dim),
        true,
    );
}

/// Fixed English recordings address OPERATOR rather than reading private profile
/// or inventory data aloud. Assets live in the crate so packaged builds stay offline.
fn fixed_voice(phase: usize, context: &StartupContext) -> Option<&'static [u8]> {
    if matches!(phase, 3 | 4) && !context.inventory_loaded {
        return Some(include_bytes!("../assets/voice/load-unavailable.wav"));
    }
    Some(match phase {
        0 => include_bytes!("../assets/voice/phase-0.wav"),
        1 => include_bytes!("../assets/voice/phase-1.wav"),
        2 => include_bytes!("../assets/voice/phase-2.wav"),
        // Normal TUI boot has already loaded the catalogs before animation.
        3 => include_bytes!("../assets/voice/phase-3-mounted.wav"),
        4 => include_bytes!("../assets/voice/phase-4.wav"),
        5 => include_bytes!("../assets/voice/phase-5.wav"),
        _ => return None,
    })
}

fn draw(
    frame: &mut Frame,
    seq: &Sequence,
    config: &StartupSection,
    context: &StartupContext,
    muted: bool,
) {
    let area = frame.area();
    let p = Palette::new(&config.theme);
    frame.render_widget(Block::default().style(p.text()), area);
    if area.width < 4 || area.height < 3 {
        return;
    }
    let phase = seq.phase.min(5);
    let inner = Rect::new(
        area.x + 2,
        area.y,
        area.width.saturating_sub(4),
        area.height,
    );
    let cue = if seq.waiting() {
        match phase {
            0 => "点击任意位置 / ENTER  开启序列",
            2 => "点击任意位置 / ENTER  查看本机清单",
            _ => "点击任意位置 / ENTER  完成接入",
        }
    } else {
        "点击 / ENTER  注入能量脉冲"
    };
    if area.width < 50 || area.height < 16 {
        row(frame, inner, 0, "D S H / DEEP DIVE", p.accent(), true);
        row(
            frame,
            inner,
            2,
            format!("0{} / 06  {}", phase + 1, scene_title(phase, context)),
            p.text(),
            true,
        );
        row(
            frame,
            inner,
            4,
            scene_subtitle(phase, context),
            p.accent(),
            true,
        );
        if phase == 3 && area.height >= 12 {
            row(frame, inner, 6, inventory_summary(context), p.text(), true);
        }
        row(
            frame,
            inner,
            area.height.saturating_sub(4),
            cue,
            p.text(),
            true,
        );
        row(
            frame,
            inner,
            area.height.saturating_sub(2),
            "ESC skip  M sound  CTRL+C exit",
            p.text().fg(p.dim),
            true,
        );
        return;
    }

    row(
        frame,
        inner,
        0,
        "D S H  /  DEEP DIVE",
        p.accent().add_modifier(Modifier::BOLD),
        false,
    );
    let status = Rect::new(
        inner.x + inner.width.saturating_sub(28),
        inner.y,
        inner.width.min(28),
        1,
    );
    row(
        frame,
        status,
        0,
        format!(
            "FOCUS 0{}  /  {}",
            seq.focus + 1,
            ["SIGNAL", "IDENTITY", "READOUT"][seq.focus]
        ),
        p.text().fg(p.dim),
        false,
    );
    let graphic = Rect::new(
        area.x,
        area.y + 2,
        area.width,
        area.height.saturating_sub(7),
    );
    draw_cinematic(frame, graphic, seq, config.reduced_motion, p, context);
    if phase == 3 {
        draw_inventory(frame, graphic, context, p);
    }

    row(
        frame,
        inner,
        area.height - 5,
        format!(
            "0{}  /  {}   {}",
            phase + 1,
            scene_title(phase, context),
            scene_subtitle(phase, context)
        ),
        p.text().add_modifier(Modifier::BOLD),
        true,
    );
    row(
        frame,
        inner,
        area.height - 3,
        if config.reduced_motion {
            "DSH / WELCOME"
        } else {
            cue
        },
        p.accent(),
        true,
    );
    let mut spans = Vec::new();
    for index in 0..6 {
        spans.push(Span::styled(
            if index <= phase { "━━ " } else { "── " },
            if index == phase {
                p.accent()
            } else {
                p.text().fg(p.grid)
            },
        ));
    }
    spans.push(Span::styled(
        format!(
            " {:03.0}%  ESC skip  M {}  CTRL+C exit  ·  示意序列",
            if config.reduced_motion {
                100.0
            } else {
                seq.progress() * 100.0
            },
            if muted { "unmute" } else { "mute" }
        ),
        p.text().fg(p.dim),
    ));
    row(
        frame,
        inner,
        area.height - 1,
        Line::from(spans),
        p.text(),
        true,
    );
}

fn tint(base: Color, target: Color, amount: f64) -> Color {
    match (base, target) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => {
            let t = amount.clamp(0.0, 1.0);
            let channel = |a: u8, b: u8| (f64::from(a) + (f64::from(b) - f64::from(a)) * t) as u8;
            Color::Rgb(channel(ar, br), channel(ag, bg), channel(ab, bb))
        }
        _ => target,
    }
}

fn line(ctx: &mut Context<'_>, from: (f64, f64), to: (f64, f64), color: Color) {
    ctx.draw(&CanvasLine {
        x1: from.0,
        y1: from.1,
        x2: to.0,
        y2: to.1,
        color,
    });
}

fn ring(
    ctx: &mut Context<'_>,
    center: (f64, f64),
    radius: f64,
    rotation: f64,
    arc: f64,
    color: Color,
) {
    let points: Vec<_> = (0..180)
        .map(|index| {
            let angle = rotation + index as f64 / 179.0 * arc;
            (
                center.0 + angle.cos() * radius,
                center.1 + angle.sin() * radius,
            )
        })
        .collect();
    ctx.draw(&Points {
        coords: &points,
        color,
    });
}

// Generated from the HD preview's original DELTA CIRCUIT department seal. Its
// five parts are the left D track, right H track, S base, DSH engraving and scan
// strata, in screen-y-down coordinates. The embedded asset supports packaging.

fn inside_polygon(point: (f64, f64), polygon: &[(f64, f64)]) -> bool {
    let mut inside = false;
    let mut previous = polygon[polygon.len() - 1];
    for &current in polygon {
        if (current.1 > point.1) != (previous.1 > point.1)
            && point.0
                < (previous.0 - current.0) * (point.1 - current.1) / (previous.1 - current.1)
                    + current.0
        {
            inside = !inside;
        }
        previous = current;
    }
    inside
}

fn badge_parts() -> &'static [Vec<(f64, f64)>; 5] {
    static PARTS: OnceLock<[Vec<(f64, f64)>; 5]> = OnceLock::new();
    PARTS.get_or_init(|| {
        let definition: serde_json::Value =
            serde_json::from_str(include_str!("../assets/dsh-emblem.json"))
                .expect("embedded DELTA CIRCUIT geometry must be valid JSON");
        let contours: [Vec<Vec<(f64, f64)>>; 5] =
            serde_json::from_value(definition["parts"].clone())
                .expect("embedded DELTA CIRCUIT geometry must contain five contour groups");
        std::array::from_fn(|part| {
            let mut points = Vec::new();
            // Sampling centers avoids asymmetric polygon-boundary inclusion.
            for y in (-110..110).step_by(2) {
                for x in (-110..110).step_by(2) {
                    let point = (x as f64 + 1.0, y as f64 + 1.0);
                    // SVG evenodd fill preserves cut-through tracks and the D counter.
                    if contours[part].iter().fold(false, |inside, contour| {
                        inside ^ inside_polygon(point, contour)
                    }) {
                        points.push(point);
                    }
                }
            }
            points
        })
    })
}

fn transformed_badge(center: (f64, f64), scale: f64, separation: f64) -> [Vec<(f64, f64)>; 5] {
    std::array::from_fn(|part| {
        let direction = if part == 0 || part == 2 || part == 4 {
            -1.0
        } else {
            1.0
        };
        let offset_x = direction * separation * if part < 2 { 100.0 } else { 145.0 };
        let offset_y = if part < 2 {
            direction * separation * 18.0
        } else {
            0.0
        };
        badge_parts()[part]
            .iter()
            .map(|&(x, y)| {
                (
                    center.0 + x * scale + offset_x,
                    center.1 - y * scale + offset_y,
                )
            })
            .collect()
    })
}

fn draw_cinematic(
    frame: &mut Frame,
    area: Rect,
    seq: &Sequence,
    still: bool,
    p: Palette,
    context: &StartupContext,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let phase = seq.phase.min(5);
    let progress = if still {
        1.0
    } else {
        (seq.elapsed / DURATIONS[phase]).clamp(0.0, 1.0)
    };
    let time = if still { 0.0 } else { seq.ambient };
    let y_max = 110.0 * f64::from(area.height) * 2.0 / f64::from(area.width);
    let base_radius = 78.0_f64.min(y_max * 0.86);
    let finale = if phase == 5 {
        ease(progress * 1.6)
    } else {
        0.0
    };
    let entrance = if still || seq.waiting() {
        1.0
    } else {
        ease(progress * 3.6)
    };
    let impact = if still || seq.waiting() {
        0.0
    } else {
        (progress * 9.0).sin() * (-progress * 6.0).exp()
    };
    let center = (if phase == 2 { -base_radius * 0.20 } else { 0.0 }, 0.0);
    let radius = base_radius
        * (0.93 + 0.07 * entrance + 0.11 * impact)
        * (1.0 - 0.08 * finale)
        * (1.0 + (time * 3.6).sin() * 0.025);
    let phase_rotation = seq.focus as f64 * TAU / 3.0;
    let separation = if phase == 0 && !seq.waiting() && !still {
        1.0 - ease(progress * 3.0)
    } else {
        0.0
    };
    let badge_scale =
        radius / 100.0 * (0.66 + finale * 0.06) * (0.74 + 0.26 * entrance + impact * 0.16);
    let emblem = transformed_badge(center, badge_scale, separation);
    let ghost = transformed_badge(center, badge_scale, 0.0);
    let cell_width = 220.0 / f64::from(area.width);
    let cell_height = y_max * 2.0 / f64::from(area.height);
    let native_engraving = separation <= f64::EPSILON
        && cell_width * 3.0 <= badge_scale * 42.0
        && cell_height <= badge_scale * 22.0;
    let canvas = Canvas::default()
        .background_color(p.bg)
        .marker(Marker::Braille)
        .x_bounds([-110.0, 110.0])
        .y_bounds([-y_max, y_max])
        .paint(|ctx| {
            // Registration marks and sparse perspective rails span the whole frame.
            for side in [-1.0, 1.0] {
                for index in 0..13 {
                    let y = (index as f64 / 12.0 - 0.5) * y_max * 1.6;
                    let length = if index % 3 == 0 { 8.0 } else { 3.0 };
                    line(ctx, (side * 105.0, y), (side * (105.0 - length), y), p.grid);
                }
                for y in [-y_max * 0.87, y_max * 0.87] {
                    line(ctx, (side * 96.0, y), (side * 82.0, y), p.dim);
                    line(
                        ctx,
                        (side * 96.0, y),
                        (side * 96.0, y - y.signum() * 6.0),
                        p.dim,
                    );
                }
            }

            let angular_speed = if seq.waiting() {
                0.95
            } else {
                2.2 + phase as f64 * 0.16
            };
            let sprint = if still || seq.waiting() {
                0.0
            } else {
                ease(progress * 4.0) * 1.4
            };
            ring(
                ctx,
                center,
                radius * 1.07,
                time * angular_speed + sprint + phase_rotation,
                TAU * 0.73,
                p.accent,
            );
            ring(
                ctx,
                center,
                radius * 1.17,
                -time * angular_speed * 1.31 - sprint - phase_rotation,
                TAU * 0.46,
                p.fg,
            );
            ring(ctx, center, radius * 0.91, -time * 0.8, TAU, p.grid);
            for trail in 1..4 {
                ring(
                    ctx,
                    center,
                    radius * (1.07 + trail as f64 * 0.025),
                    time * angular_speed + sprint + phase_rotation - trail as f64 * 0.12,
                    TAU * 0.17,
                    tint(p.bg, p.accent, 0.48 / trail as f64),
                );
            }
            for index in 0..90 {
                let angle = index as f64 / 90.0 * TAU + time * 0.24;
                let inside = if index % 5 == 0 { 0.97 } else { 1.01 };
                line(
                    ctx,
                    (
                        center.0 + radius * inside * angle.cos(),
                        center.1 + radius * inside * angle.sin(),
                    ),
                    (
                        center.0 + radius * 1.035 * angle.cos(),
                        center.1 + radius * 1.035 * angle.sin(),
                    ),
                    if index % 5 == 0 { p.dim } else { p.grid },
                );
            }

            // Inflow stays alive at checkpoints; context intake accelerates the field.
            if phase < 5 {
                let particle_speed = if phase == 1 {
                    1.35
                } else if seq.waiting() {
                    0.22
                } else {
                    0.58
                };
                let mut particles = Vec::new();
                let mut bright = Vec::new();
                for index in 0..145 {
                    let angle = index as f64 * 2.399963 + phase_rotation * 0.2;
                    let travel = (time * particle_speed + index as f64 * 0.137).fract();
                    let far = 100.0 + (index % 7) as f64 * 8.0;
                    let distance = radius * 0.34 + (far - radius * 0.34) * (1.0 - travel).powi(2);
                    let point = (
                        distance * angle.cos(),
                        distance * angle.sin() * y_max / 70.0,
                    );
                    if index % 3 == seq.focus {
                        bright.push(point);
                        let trail = if seq.waiting() {
                            3.0
                        } else {
                            9.0 + (index % 5) as f64 * 1.8
                        };
                        line(
                            ctx,
                            point,
                            (
                                (distance + trail) * angle.cos(),
                                (distance + trail) * angle.sin() * y_max / 70.0,
                            ),
                            if phase == 1 || phase == 3 {
                                p.dim
                            } else {
                                p.grid
                            },
                        );
                    } else {
                        particles.push(point);
                    }
                    if phase == 1 && index % 17 == 0 {
                        let token = ["fn", "01", "{}", "ctx", "[]", "<> "][(index / 17) % 6];
                        ctx.print(point.0, point.1, Span::styled(token, p.text().fg(p.dim)));
                    }
                }
                ctx.draw(&Points {
                    coords: &particles,
                    color: p.grid,
                });
                ctx.draw(&Points {
                    coords: &bright,
                    color: if phase == 1 { p.fg } else { p.accent },
                });
            }

            match phase {
                0 => {
                    let aperture = if seq.waiting() {
                        0.70 + 0.10 * (time * 3.8).sin()
                    } else {
                        0.55 + 0.45 * ease(progress)
                    };
                    for index in 0..12 {
                        let angle = index as f64 * TAU / 12.0 + time * 0.37;
                        line(
                            ctx,
                            (angle.cos() * radius * 1.35, angle.sin() * radius * 1.35),
                            (
                                angle.cos() * radius * aperture,
                                angle.sin() * radius * aperture,
                            ),
                            p.grid,
                        );
                    }
                    line(ctx, (-102.0, 0.0), (-radius * 1.3, 0.0), p.accent);
                    line(ctx, (radius * 1.3, 0.0), (102.0, 0.0), p.accent);
                }
                1 => {
                    for index in 0..6 {
                        let angle = index as f64 * TAU / 6.0 + phase_rotation * 0.12;
                        let start = (angle.cos() * 106.0, angle.sin() * y_max * 0.94);
                        let end = (angle.cos() * radius * 0.44, angle.sin() * radius * 0.44);
                        line(ctx, start, end, p.grid);
                        let t = (time * 2.8 + index as f64 * 0.17).fract();
                        line(
                            ctx,
                            (
                                start.0 + (end.0 - start.0) * t,
                                start.1 + (end.1 - start.1) * t,
                            ),
                            (
                                start.0 + (end.0 - start.0) * (t + 0.14).min(1.0),
                                start.1 + (end.1 - start.1) * (t + 0.14).min(1.0),
                            ),
                            p.accent,
                        );
                    }
                }
                2 => {
                    // A flat technical identity plate: brackets, microtype and scan bars.
                    // Gates pause the sequence, but decoding still settles so
                    // the operator can read their profile before confirming.
                    let decode_age = seq.phase_age.max(seq.elapsed);
                    let x = radius * (1.55 + (1.0 - entrance) * 0.6);
                    let y = radius * 0.78;
                    for side in [-1.0, 1.0] {
                        line(ctx, (side * x, -y), (side * x, y), p.dim);
                        line(ctx, (side * x, y), (side * (x - 10.0), y), p.fg);
                        line(ctx, (side * x, -y), (side * (x - 10.0), -y), p.fg);
                    }
                    ctx.print(
                        -x + 6.0,
                        y - 4.0,
                        Span::styled(
                            format!(
                                "OPERATOR / {}",
                                decoded_profile(&context.profile.username, 28, decode_age, 0, still)
                            ),
                            p.accent(),
                        ),
                    );
                    ctx.print(
                        -x + 6.0,
                        -y - 5.0,
                        Span::styled(
                            format!(
                                "ID {} / {}",
                                decoded_profile(&context.profile.badge_id, 24, decode_age, 1, still),
                                decoded_profile("LOCAL PROFILE", 24, decode_age, 2, still)
                            ),
                            p.text().fg(p.dim),
                        ),
                    );
                    for index in 0..7 {
                        let xx = x - 24.0 + index as f64 * 2.4;
                        let height = 4.0 + ((index + seq.focus) % 3) as f64 * 2.0;
                        line(ctx, (xx, y - 15.0), (xx, y - 15.0 + height), p.accent);
                    }
                    let scan = -y + (time * 0.72).fract() * y * 2.0;
                    line(ctx, (-x + 1.0, scan), (-radius * 1.16, scan), p.accent);
                    line(ctx, (radius * 1.16, scan), (x - 1.0, scan), p.accent);
                }
                3 => {
                    // Hexadecimal rain and horizontal reading slats, without a 3D core.
                    for side in [-1.0, 1.0] {
                        for column in 0..2 {
                            let x = if side < 0.0 {
                                -102.0 + column as f64 * 21.0
                            } else {
                                63.0 + column as f64 * 21.0
                            };
                            for row in 0..9 {
                                let y = y_max * 0.85
                                    - (time * (26.0 + column as f64 * 7.0) + row as f64 * 11.0)
                                        .rem_euclid(y_max * 1.7);
                                let value = ((time * 40.0) as u32)
                                    .wrapping_mul(0x9e3779b9)
                                    .wrapping_add((row * 0xabc1 + column * 0x62f3) as u32);
                                ctx.print(
                                    x,
                                    y,
                                    Span::styled(
                                        format!("{:06X}", value & 0xffffff),
                                        p.text().fg(if row % 3 == seq.focus {
                                            p.dim
                                        } else {
                                            p.grid
                                        }),
                                    ),
                                );
                            }
                        }
                    }
                    for index in 0..9 {
                        let y = (index as f64 / 8.0 - 0.5) * radius * 1.7;
                        let fill = (progress * 1.5 - index as f64 * 0.06).clamp(0.0, 1.0);
                        line(
                            ctx,
                            (-radius * 1.4, y),
                            (-radius * 1.4 + radius * 0.25 * fill, y),
                            p.accent,
                        );
                        line(
                            ctx,
                            (radius * 1.15, y),
                            (radius * 1.15 + radius * 0.25 * fill, y),
                            p.accent,
                        );
                    }
                }
                4 => {
                    let tool_names = [
                        "SKILLS",
                        "PLUGINS",
                        if context.inventory_loaded {
                            "READY"
                        } else {
                            "N/A"
                        },
                    ];
                    for (index, tool) in tool_names.iter().enumerate() {
                        let angle = index as f64 * TAU / 3.0 + 0.45 + phase_rotation * 0.1;
                        let node = (angle.cos() * radius * 1.32, angle.sin() * radius * 1.32);
                        ring(
                            ctx,
                            node,
                            3.6,
                            time * 1.6,
                            TAU,
                            if index == seq.focus { p.fg } else { p.accent },
                        );
                        ctx.print(
                            node.0 + if node.0 < 0.0 { -20.0 } else { 6.0 },
                            node.1 + 1.0,
                            Span::styled(*tool, p.text().fg(p.dim)),
                        );
                        line(ctx, node, (node.0 * 0.35, node.1 * 0.35), p.grid);
                        let travel = (time * 2.5 + index as f64 / 3.0).fract();
                        let feedback = (node.0 * (1.0 - travel), node.1 * (1.0 - travel));
                        ring(ctx, feedback, 1.3, 0.0, TAU, p.fg);
                    }
                    ring(
                        ctx,
                        (0.0, 0.0),
                        radius * 1.31,
                        -time * 3.8,
                        TAU * 0.32,
                        p.accent,
                    );
                }
                _ => {
                    let greeting =
                        format!("WELCOME, {}", display_text(&context.profile.username, 20));
                    ctx.print(
                        -(Line::from(greeting.as_str()).width() as f64) * cell_width * 0.5,
                        radius * 0.88,
                        Span::styled(greeting, p.accent()),
                    );
                    ctx.print(
                        -radius * 0.77,
                        -radius * 0.86,
                        Span::styled("D S H / ENGINEERING", p.text().fg(p.dim)),
                    );
                    line(
                        ctx,
                        (-radius * 0.78, -radius * 0.70),
                        (radius * 0.78, -radius * 0.70),
                        p.accent,
                    );
                }
            }

            if phase != 5 {
                ctx.print(
                    center.0 - radius * 0.55,
                    -radius * 0.74,
                    Span::styled("D S H / DIVISION 06", p.text().fg(p.dim)),
                );
            }

            // Fill the shared five-part mark densely; pixels are solid ink after assembly.
            for (part, points) in emblem.iter().enumerate() {
                if part == 3 && native_engraving {
                    continue;
                }
                ctx.draw(&Points {
                    coords: &ghost[part],
                    color: tint(p.bg, p.fg, 0.13),
                });
                if separation > 0.015 {
                    for trail in 1..3 {
                        let offset = if part == 0 || part == 2 || part == 4 {
                            -1.0
                        } else {
                            1.0
                        };
                        let trail_points: Vec<_> = points
                            .iter()
                            .map(|&(x, y)| {
                                (x + offset * trail as f64 * (4.0 + separation * 8.0), y)
                            })
                            .collect();
                        ctx.draw(&Points {
                            coords: &trail_points,
                            color: tint(p.bg, p.accent, 0.16 / trail as f64),
                        });
                    }
                }
                ctx.draw(&Points {
                    coords: points,
                    color: if seq.waiting() && phase == 0 {
                        tint(p.bg, p.fg, 0.83 + 0.10 * (time * 3.6).sin())
                    } else {
                        p.fg
                    },
                });
            }
            if !still {
                let scan = center.1 - radius + (time * 0.78).fract() * radius * 2.0;
                let lit: Vec<_> = emblem
                    .iter()
                    .enumerate()
                    .filter(|(part, _)| *part != 3 || !native_engraving)
                    .flat_map(|(_, points)| points)
                    .copied()
                    .filter(|(_, y)| (*y - scan).abs() < radius * 0.13)
                    .collect();
                ctx.draw(&Points {
                    coords: &lit,
                    color: p.accent,
                });
                line(ctx, (-104.0, scan), (-radius * 1.25, scan), p.grid);
                line(ctx, (radius * 1.25, scan), (104.0, scan), p.grid);
                if !seq.waiting() && seq.elapsed < 0.42 {
                    let sweep = ease(seq.elapsed / 0.42);
                    let x = -110.0 + 220.0 * sweep;
                    line(
                        ctx,
                        (x, -y_max),
                        (x - 18.0, y_max),
                        tint(p.bg, p.fg, (1.0 - sweep) * 0.65),
                    );
                    ring(
                        ctx,
                        center,
                        radius * (0.6 + sweep * 1.4),
                        0.0,
                        TAU,
                        tint(p.bg, p.accent, (1.0 - sweep) * 0.8),
                    );
                }
            }
            if !still && seq.pulse_age < 0.72 {
                let origin = (
                    (seq.pulse_origin.0 * 2.0 - 1.0) * 110.0,
                    (1.0 - seq.pulse_origin.1 * 2.0) * y_max,
                );
                let pulse = seq.pulse_age / 0.72;
                let pulse_radius = 3.0 + ease(pulse) * base_radius * 2.6;
                ring(
                    ctx,
                    origin,
                    pulse_radius,
                    0.0,
                    TAU,
                    tint(p.bg, p.accent, 1.0 - pulse),
                );
                ring(
                    ctx,
                    origin,
                    pulse_radius * 0.92,
                    0.0,
                    TAU,
                    tint(p.bg, p.fg, (1.0 - pulse) * 0.9),
                );
                for ray in 0..12 {
                    let angle = ray as f64 * TAU / 12.0 + phase_rotation;
                    let inner = pulse_radius * 0.83;
                    let outer = pulse_radius + 10.0 * (1.0 - pulse);
                    line(
                        ctx,
                        (
                            origin.0 + angle.cos() * inner,
                            origin.1 + angle.sin() * inner,
                        ),
                        (
                            origin.0 + angle.cos() * outer,
                            origin.1 + angle.sin() * outer,
                        ),
                        tint(p.bg, p.fg, (1.0 - pulse) * 0.85),
                    );
                }
            }
            // The vector engraving is below Braille resolution on normal terminals.
            // Fit an actual three-cell label inside the triangle only after assembly.
            if native_engraving {
                ctx.print(
                    center.0 - cell_width * 1.5,
                    center.1 - badge_scale * 17.0,
                    Span::styled("DSH", p.text().add_modifier(Modifier::BOLD)),
                );
            }
        });
    frame.render_widget(canvas, area);
}

fn ease(t: f64) -> f64 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn visible_text(buffer: &ratatui::buffer::Buffer) -> String {
        let mut output = String::new();
        for row in buffer.content.chunks(buffer.area.width.max(1) as usize) {
            let mut x = 0;
            while x < row.len() {
                let symbol = row[x].symbol();
                output.push_str(symbol);
                x += Line::from(symbol).width().max(1);
            }
            output.push('\n');
        }
        output
    }

    #[test]
    fn native_narration_reviews_loaded_catalogs_and_does_not_claim_missing_resources_loaded() {
        let available = StartupContext {
            inventory_loaded: true,
            ..Default::default()
        };
        assert_eq!(
            fixed_voice(3, &available),
            Some(include_bytes!("../assets/voice/phase-3-mounted.wav").as_slice())
        );
        for phase in [3, 4] {
            assert_eq!(
                fixed_voice(phase, &StartupContext::default()),
                Some(include_bytes!("../assets/voice/load-unavailable.wav").as_slice())
            );
        }
        for phase in 0..6 {
            assert!(fixed_voice(phase, &available).is_some());
        }
        assert!(fixed_voice(6, &available).is_none());
    }

    #[test]
    fn profile_decoding_preserves_unicode_width_and_resolves_all_three_values() {
        for (row, value, columns) in [
            (0, "星海研究员 CatShark", 28),
            (1, "部门-七 / DSH-0001", 24),
            (2, "LOCAL PROFILE", 24),
        ] {
            let expected = display_text(value, columns);
            assert_ne!(decoded_profile(value, columns, 0.0, row, false), expected);
            for age in [0.0, 0.15, 0.35, 0.65, 0.9] {
                let decoded = decoded_profile(value, columns, age, row, false);
                assert_eq!(
                    Line::from(decoded.as_str()).width(),
                    Line::from(expected.as_str()).width()
                );
                assert_eq!(decoded, decoded_profile(value, columns, age, row, false));
            }
            assert_eq!(decoded_profile(value, columns, 0.9, row, false), expected);
            assert_eq!(decoded_profile(value, columns, 0.0, row, true), expected);
        }
    }

    #[test]
    fn identity_decodes_at_a_gate_then_resets_for_the_next_phase() {
        let context = StartupContext {
            profile: StartupProfile {
                username: "星海研究员".into(),
                badge_id: "部门-七".into(),
            },
            ..Default::default()
        };
        let config = StartupSection::default();
        let mut seq = Sequence {
            phase: 2,
            ..Sequence::new(true)
        };
        let mut terminal = Terminal::new(TestBackend::new(110, 34)).unwrap();
        let before = terminal
            .draw(|f| draw(f, &seq, &config, &context, true))
            .unwrap();
        assert!(!visible_text(before.buffer).contains("星海研究员"));
        seq.tick(0.9);
        assert_eq!(
            seq.elapsed, 0.0,
            "decoding must not release the interaction gate"
        );
        let after = terminal
            .draw(|f| draw(f, &seq, &config, &context, true))
            .unwrap();
        let clear = visible_text(after.buffer).replace(' ', "");
        for expected in ["星海研究员", "部门-七", "LOCALPROFILE"] {
            assert!(clear.contains(expected), "missing decoded value: {expected}");
        }
        seq.confirm((0.5, 0.5));
        seq.tick(DURATIONS[2] + 0.2);
        assert_eq!(seq.phase, 3);
        assert!((seq.phase_age - 0.2).abs() < 1e-9);
        seq.tick(DURATIONS[3]);
        assert_eq!(seq.phase, 4);
        assert_eq!(seq.phase_age, 0.0);
    }

    #[test]
    fn real_inventory_and_unicode_identity_render_without_inventing_entries() {
        let config = StartupSection::default();
        let context = StartupContext {
            profile: StartupProfile {
                username: "星海研究员".into(),
                badge_id: "部门-七".into(),
            },
            skill_names: vec!["真实技能".into(), "review-rust".into()],
            plugin_names: vec!["本机插件 (local)".into()],
            inventory_loaded: true,
        };
        let mut terminal = Terminal::new(TestBackend::new(110, 34)).unwrap();
        for (phase, expected) in [
            (2, vec!["星海研究员", "部门-七"]),
            (
                3,
                vec![
                    "SKILLS 2",
                    "PLUGINS 1",
                    "真实技能",
                    "review-rust",
                    "本机插件 (local)",
                    "启动前已完成挂载",
                ],
            ),
        ] {
            let seq = Sequence {
                phase,
                elapsed: 1.0,
                ..Sequence::new(false)
            };
            let completed = terminal
                .draw(|f| draw(f, &seq, &config, &context, true))
                .unwrap();
            let text = visible_text(completed.buffer);
            for expected in expected {
                assert!(
                    text.replace(' ', "").contains(&expected.replace(' ', "")),
                    "missing {expected}"
                );
            }
        }
    }

    #[test]
    fn empty_loaded_inventory_is_distinct_from_unavailable_inventory() {
        let config = StartupSection::default();
        let seq = Sequence {
            phase: 3,
            elapsed: 1.0,
            ..Sequence::new(false)
        };
        for inventory_loaded in [true, false] {
            let context = StartupContext {
                inventory_loaded,
                ..Default::default()
            };
            for (width, height) in [(53, 18), (80, 24), (110, 34)] {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                let completed = terminal
                    .draw(|f| draw(f, &seq, &config, &context, true))
                    .unwrap();
                let text = completed
                    .buffer
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>();
                if inventory_loaded {
                    assert!(text.contains("SKILLS 0"));
                    assert!(text.contains("PLUGINS 0"));
                    assert!(text.contains("(none loaded)"));
                } else {
                    assert!(text.contains("INVENTORY UNAVAILABLE"));
                    assert!(!text.contains("SKILLS 0"));
                    let seq = Sequence {
                        phase: 4,
                        elapsed: 1.0,
                        ..Sequence::new(false)
                    };
                    let completed = terminal
                        .draw(|f| draw(f, &seq, &config, &context, true))
                        .unwrap();
                    let text = visible_text(completed.buffer);
                    assert!(text.contains("INVENTORY UNAVAILABLE"));
                    assert!(!text.contains("READY"));
                }
            }
        }
        assert_eq!(display_text("猫鲨\u{1b}\n", 4), "猫鲨");
    }

    #[test]
    fn delta_circuit_preserves_three_edge_tracks_and_engraved_letter_counters() {
        let parts = badge_parts();
        assert!(parts.iter().all(|part| !part.is_empty()));
        // The three large edge tracks, all three engraved letters and scan strata survive.
        for (part, ink) in [
            (0, (-11.0, -41.0)),
            (1, (11.0, -41.0)),
            (2, (1.0, 81.0)),
            (3, (-23.0, 27.0)),
            (3, (1.0, 27.0)),
            (3, (17.0, 27.0)),
            (4, (1.0, -11.0)),
        ] {
            assert!(parts[part].contains(&ink), "missing ink at {ink:?}");
        }
        // Left D cut, both S grooves, central D counter and H's upper opening.
        for hole in [
            (-41.0, 9.0),
            (1.0, 55.0),
            (1.0, 71.0),
            (-19.0, 27.0),
            (17.0, 21.0),
        ] {
            assert!(
                !parts.iter().any(|part| part.contains(&hole)),
                "filled hole at {hole:?}"
            );
        }
    }

    #[test]
    fn native_engraving_waits_for_assembly_and_fits_the_inner_triangle() {
        let config = StartupSection::default();
        for (width, height, fits) in [(110, 34, true), (80, 24, true), (53, 18, false)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            for (elapsed, assembled) in [(0.10, false), (1.20, true)] {
                let seq = Sequence {
                    elapsed,
                    ambient: elapsed,
                    ..Sequence::new(false)
                };
                let completed = terminal
                    .draw(|f| draw(f, &seq, &config, &StartupContext::default(), true))
                    .unwrap();
                let graphic: String = completed
                    .buffer
                    .content
                    .chunks(width as usize)
                    .skip(2)
                    .take(height as usize - 7)
                    .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
                    .collect::<Vec<_>>()
                    .join("\n");
                assert_eq!(
                    graphic.contains("DSH"),
                    assembled && fits,
                    "engraving at {width}x{height}, elapsed {elapsed}"
                );
            }
        }
    }

    #[test]
    fn silent_sequence_completes_in_thirteen_point_eight_seconds() {
        assert!((DURATIONS.iter().sum::<f64>() - TOTAL_DURATION).abs() < 1e-9);
        let mut seq = Sequence::new(false);
        seq.tick(TOTAL_DURATION - 0.01);
        assert_eq!(seq.phase, 5);
        seq.tick(0.011);
        assert_eq!(seq.phase, 6);
        assert!((seq.progress() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn narration_holds_only_the_phase_boundary_and_keeps_ambient_motion_alive() {
        let mut seq = Sequence::new(false);
        seq.tick_with_narration(10.0, true);
        assert_eq!(seq.phase, 0);
        assert_eq!(seq.elapsed, DURATIONS[0]);
        assert_eq!(seq.ambient, 10.0);
        seq.confirm((0.2, 0.8));
        seq.tick_with_narration(1.0, true);
        assert_eq!(seq.phase, 0);
        assert_eq!(seq.ambient, 11.0);
        seq.tick_with_narration(0.1, false);
        assert_eq!(seq.phase, 1);
        assert!((seq.elapsed - 0.1).abs() < 1e-9);
    }

    #[test]
    fn three_gates_release_pairs_of_continuous_scenes() {
        let mut seq = Sequence::new(true);
        for gate in [0, 2, 4] {
            assert_eq!(seq.phase, gate);
            assert!(seq.waiting());
            let ambient = seq.ambient;
            seq.tick(10.0);
            assert_eq!(seq.elapsed, 0.0);
            assert!(seq.ambient > ambient);
            seq.confirm((0.1, 0.9));
            assert!(!seq.waiting());
            seq.tick(DURATIONS[gate] + 0.01);
            assert_eq!(seq.phase, gate + 1);
            assert!(!seq.waiting());
            seq.tick(DURATIONS[gate + 1]);
        }
        assert_eq!(seq.phase, 6);
    }

    #[test]
    fn confirmations_while_playing_emit_pulses_without_skipping() {
        let mut seq = Sequence::new(true);
        seq.confirm((0.5, 0.5));
        seq.tick(1.0);
        seq.confirm((0.0, 1.0));
        assert_eq!(seq.phase, 0);
        assert_eq!(seq.elapsed, 1.0);
        assert_eq!(seq.pulse_age, 0.0);
        assert_eq!(seq.pulse_origin, (0.0, 1.0));
        seq.select(2);
        assert_eq!(seq.focus, 2);
        seq.move_selection(true);
        assert_eq!(seq.focus, 0);
        seq.move_selection(false);
        assert_eq!(seq.focus, 2);
        assert_eq!(seq.elapsed, 1.0);
    }

    #[test]
    fn mouse_down_anywhere_confirms_but_release_drag_and_scroll_do_not() {
        for (column, row) in [(0, 0), (109, 33), (55, 16)] {
            let event = |kind| MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            };
            assert!(mouse_confirms(event(MouseEventKind::Down(
                MouseButton::Left
            ))));
            for kind in [
                MouseEventKind::Up(MouseButton::Left),
                MouseEventKind::Drag(MouseButton::Left),
                MouseEventKind::Moved,
                MouseEventKind::ScrollDown,
                MouseEventKind::Down(MouseButton::Right),
            ] {
                assert!(!mouse_confirms(event(kind)));
            }
        }
    }

    #[test]
    fn keyboard_controls_ignore_repeats_and_releases() {
        assert_eq!(
            action(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Action::Skip
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Quit
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE)),
            Action::Mute
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::Confirm
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
            Action::Confirm
        );
        for kind in [KeyEventKind::Repeat, KeyEventKind::Release] {
            for code in [KeyCode::Enter, KeyCode::Char('2'), KeyCode::Right] {
                assert_eq!(
                    action(KeyEvent::new_with_kind(code, KeyModifiers::NONE, kind)),
                    Action::None
                );
            }
        }
    }

    #[test]
    fn six_scenes_render_across_themes_and_tiny_terminal_sizes() {
        for theme in ["dark", "light"] {
            let config = StartupSection {
                theme: theme.into(),
                ..Default::default()
            };
            for (width, height) in [
                (0, 0),
                (1, 1),
                (12, 5),
                (53, 18),
                (80, 24),
                (110, 34),
                (160, 48),
            ] {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                for phase in 0..6 {
                    let seq = Sequence {
                        phase,
                        elapsed: DURATIONS[phase] * 0.7,
                        ambient: DURATIONS[..phase].iter().sum::<f64>() + DURATIONS[phase] * 0.7,
                        ..Sequence::new(false)
                    };
                    let completed = terminal
                        .draw(|f| draw(f, &seq, &config, &StartupContext::default(), true))
                        .unwrap();
                    if width >= 80 {
                        let text: String = completed
                            .buffer
                            .content
                            .iter()
                            .map(|cell| cell.symbol())
                            .collect();
                        assert!(text.contains(scene_subtitle(phase, &StartupContext::default())));
                        assert!(text.contains("ESC skip"));
                        assert!(text.replace(' ', "").contains("示意序列"));
                    }
                }
            }
        }
    }

    #[test]
    fn waiting_is_visually_alive_and_focus_changes_the_field() {
        let config = StartupSection::default();
        let mut terminal = Terminal::new(TestBackend::new(110, 34)).unwrap();
        for phase in [0, 2, 4] {
            let mut seq = Sequence {
                phase,
                ..Sequence::new(true)
            };
            let before = terminal
                .draw(|f| draw(f, &seq, &config, &StartupContext::default(), true))
                .unwrap()
                .buffer
                .clone();
            seq.tick(0.7);
            let after = terminal
                .draw(|f| draw(f, &seq, &config, &StartupContext::default(), true))
                .unwrap()
                .buffer
                .clone();
            assert_eq!(seq.elapsed, 0.0);
            assert_ne!(before, after);
            seq.select(2);
            let focus = terminal
                .draw(|f| draw(f, &seq, &config, &StartupContext::default(), true))
                .unwrap()
                .buffer
                .clone();
            assert_ne!(after, focus);
        }
    }

    #[test]
    fn reduced_motion_has_no_visual_time_dependence() {
        let config = StartupSection {
            reduced_motion: true,
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(110, 34)).unwrap();
        let mut seq = Sequence {
            phase: 5,
            ..Sequence::new(false)
        };
        let before = terminal
            .draw(|f| draw(f, &seq, &config, &StartupContext::default(), true))
            .unwrap()
            .buffer
            .clone();
        seq.elapsed = 0.9;
        seq.ambient = 8.0;
        seq.pulse_age = 0.2;
        let after = terminal
            .draw(|f| draw(f, &seq, &config, &StartupContext::default(), true))
            .unwrap()
            .buffer
            .clone();
        assert_eq!(before, after);
    }

    /// Export fresh frames, including correct hidden cells after CJK glyphs.
    #[test]
    #[ignore = "writes target/startup-frames.json for scripts/render_startup_preview.py"]
    fn export_preview_frames() {
        let mut terminal = Terminal::new(TestBackend::new(110, 34)).unwrap();
        let config = StartupSection::default();
        let mut frames = Vec::new();
        for index in 0..=69 {
            let time = (index as f64 * 0.2).min(TOTAL_DURATION - 0.001);
            let mut seq = Sequence::new(false);
            seq.tick(time);
            let completed = terminal
                .draw(|f| draw(f, &seq, &config, &StartupContext::default(), true))
                .unwrap();
            let cells: Vec<_> = completed
                .buffer
                .content
                .iter()
                .map(|cell| {
                    let rgb = |color: Color| match color {
                        Color::Rgb(r, g, b) => [r, g, b],
                        _ => [234, 242, 249],
                    };
                    serde_json::json!([cell.symbol(), rgb(cell.fg), rgb(cell.bg)])
                })
                .collect();
            frames.push(cells);
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/startup-frames.json");
        std::fs::write(path, serde_json::to_vec(&serde_json::json!({"width":110,"height":34,"frame_duration_ms":200,"frames":frames})).unwrap()).unwrap();
    }
}
