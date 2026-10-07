//! Codex-aligned slash command surface for the TUI.

use crate::keymap::{KEYMAP_DEFAULTS, KEYMAP_HELP};
use crate::theme::CellKind;
use dsh_core::{
    clear_api_key, ApprovalPolicy, GoalSpec, PermissionMode, RetryPolicy, Runtime, SandboxMode,
    Session, TaskRecord, TaskState, APPROVAL_HELP, DEFAULT_FLAGS, PERMISSION_HELP, PERSONALITIES,
    PETS, SANDBOX_HELP, STATUSLINE_FIELDS, THEMES, TITLE_FIELDS,
};
use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct UiLine {
    pub kind: CellKind,
    pub text: String,
    pub header: Option<String>,
    pub running: bool,
    pub ok: Option<bool>,
}

pub struct SlashCtx<'a> {
    pub runtime: &'a Arc<Runtime>,
    pub session: &'a Arc<RwLock<Session>>,
    pub lines: &'a mut Vec<UiLine>,
    pub session_id: &'a mut String,
    pub model: &'a mut String,
    pub cwd: &'a str,
    pub show_thinking: &'a mut bool,
    pub sidebar: &'a mut bool,
    pub has_api_key: &'a mut bool,
    pub status: &'a mut String,
    pub skill_names: &'a [String],
    pub plugin_names: &'a [String],
    pub should_quit: &'a mut bool,
}

pub enum SlashEffect {
    None,
    Quit,
    /// Replace the active session handle (caller rebuilds UI).
    SwitchSession(Arc<RwLock<Session>>),
    /// Inject a synthetic user turn after the slash handler returns.
    QueuePrompt(String),
}

/// Formatted help. Topics: ``, `keys`, `session`, `agent`, `ui`, `setup`, `all`.
pub fn help_topic(topic: &str) -> String {
    let t = topic.trim().to_lowercase();
    match t.as_str() {
        "" | "overview" | "index" => help_overview(),
        "keys" | "key" | "keymap" | "shortcuts" => KEYMAP_HELP.to_string(),
        "session" | "chat" => section_session(),
        "agent" | "model" | "tools" => section_agent(),
        "ui" | "display" => section_ui(),
        "setup" | "config" | "auth" => section_setup(),
        "all" | "full" => {
            let mut out = String::new();
            out.push_str(&help_overview());
            out.push_str("\n\n");
            out.push_str(&section_session());
            out.push_str("\n\n");
            out.push_str(&section_agent());
            out.push_str("\n\n");
            out.push_str(&section_ui());
            out.push_str("\n\n");
            out.push_str(&section_setup());
            out.push_str("\n\n");
            out.push_str(KEYMAP_HELP);
            out
        }
        other => format!("Unknown help topic `{other}`.\n\n{}", help_overview()),
    }
}

fn help_overview() -> String {
    let mut s = String::new();
    s.push_str("dsh-rust  ·  slash command help\n");
    s.push_str("────────────────────────────────────────\n\n");
    s.push_str("Usage\n");
    s.push_str("  /help              this overview\n");
    s.push_str("  /help <topic>      detailed section\n");
    s.push_str("  /help all          everything\n");
    s.push_str("  Tab                autocomplete after /\n\n");
    s.push_str("Topics\n");
    s.push_str("  session   new / resume / fork / clear …\n");
    s.push_str("  agent     model / permissions / plan …\n");
    s.push_str("  ui        vim / theme / statusline …\n");
    s.push_str("  setup     apikey / init / logout …\n");
    s.push_str("  keys      keyboard shortcuts\n\n");
    s.push_str("Quick start\n");
    s.push_str("  1. /apikey <KEY>          set DeepSeek API key\n");
    s.push_str("  2. type a message         Enter to send\n");
    s.push_str("  3. /permissions auto      tool access preset\n");
    s.push_str("  4. /status                see current config\n\n");
    s.push_str("Composer prefixes\n");
    s.push_str("  !command     run shell in background (/ps to list)\n");
    s.push_str("  @path        attach a file path to the next prompt\n");
    s.push_str("  /cmd         slash command (Tab to complete)\n");
    s
}

fn row(cmd: &str, desc: &str) -> String {
    format!("  {cmd:<28} {desc}\n")
}

fn section_session() -> String {
    let mut s = String::new();
    s.push_str("SESSION\n");
    s.push_str("────────────────────────────────────────\n");
    s.push_str(&row("/new", "Start a new chat (same CLI)"));
    s.push_str(&row("/clear", "Clear view + start a new chat"));
    s.push_str(&row("/resume [id|--last]", "Resume a saved session"));
    s.push_str(&row("/fork", "Branch current chat into a new id"));
    s.push_str(&row("/rename <name>", "Name the current session"));
    s.push_str(&row("/compact", "Summarize history to free context"));
    s.push_str(&row("/copy", "Copy latest assistant output"));
    s.push_str(&row("/session", "Show current session id"));
    s.push_str(&row("/archive", "Archive session and exit"));
    s.push_str(&row("/delete", "Delete session and exit"));
    s.push_str(&row("/quit  |  /exit", "Leave the TUI"));
    s
}

fn section_agent() -> String {
    let mut s = String::new();
    s.push_str("AGENT\n");
    s.push_str("────────────────────────────────────────\n");
    s.push_str(&row("/model [name]", "Show or switch the active model"));
    s.push_str(&row(
        "/permissions [mode]",
        "read-only | auto | full-access",
    ));
    s.push_str(&row("/approval [policy]", "never | on-request | untrusted"));
    s.push_str(&row(
        "/sandbox [mode]",
        "read-only | workspace-write | danger-full-access",
    ));
    s.push_str(&row(
        "/security-research [on|off]",
        "Suppress generic cyber-safety refusal boilerplate",
    ));
    s.push_str(&row("/approvals", "Alias of /permissions"));
    s.push_str(&row("/approve", "Retry last permission-denied tool"));
    s.push_str(&row("/thinking", "Toggle thinking display in TUI"));
    s.push_str(&row("/fast", "Toggle fast mode (disable thinking)"));
    s.push_str(&row("/plan [msg]", "Ask for an execution plan first"));
    s.push_str(&row("/review [msg]", "Review the working tree"));
    s.push_str(&row("/goal …", "set|edit|verify|pause|resume|view|clear"));
    s.push_str(&row("/personality [name]", "Response style preset"));
    s.push_str(&row("/mention <path>", "Point the agent at a path"));
    s.push_str(&row("/ide", "Pull local IDE/workspace hints"));
    s.push_str(&row("/side|/btw [msg]", "Ephemeral side question"));
    s.push_str(&row("/ps", "List background shell jobs"));
    s.push_str(&row("/stop [id]", "Stop background jobs"));
    s.push_str(&row("/status", "Model, perms, session, tools…"));
    s.push_str(&row("/diff", "Git status + diff --stat"));
    s.push_str(&row("/mcp [verbose]", "List MCP servers / tools"));
    s.push_str(&row("/skills", "List discovered skills"));
    s.push_str(&row("/plugins|/apps", "List plugins / connectors"));
    s.push_str(&row("/agent|/subagents", "List session threads"));
    s.push_str(&row("/hooks", "Lifecycle hooks status"));
    s.push_str(&row("/memories …", "Memory inject/generate on|off"));
    s.push_str(&row("/usage", "Local usage / learn summary"));
    s.push_str(&row("/debug-config", "Dump layered config"));
    s
}

fn section_ui() -> String {
    let mut s = String::new();
    s.push_str("UI\n");
    s.push_str("────────────────────────────────────────\n");
    s.push_str(&row("/vim", "Toggle vim NORMAL/INSERT composer"));
    s.push_str(&row("/raw", "Toggle raw (unformatted) transcript"));
    s.push_str(&row("/sidebar", "Toggle context sidebar"));
    s.push_str(&row("/legend", "Color category legend"));
    s.push_str(&row("/keymap", "Keyboard shortcuts + bindings"));
    s.push_str(&row("/theme [name]", "Syntax / transcript theme"));
    s.push_str(&row("/statusline [fields…]", "Footer fields"));
    s.push_str(&row("/title [fields…]", "Window/tab title fields"));
    s.push_str(&row("/pets|/pet [name]", "Ambient pet (or none)"));
    s.push_str(&row("/experimental …", "Feature / experimental flags"));
    s
}

fn section_setup() -> String {
    let mut s = String::new();
    s.push_str("SETUP\n");
    s.push_str("────────────────────────────────────────\n");
    s.push_str(&row("/apikey <KEY>", "Save DeepSeek API key"));
    s.push_str(&row("/api-status", "Masked key status"));
    s.push_str(&row("/logout", "Clear stored API key"));
    s.push_str(&row("/init", "Write AGENTS.md scaffold"));
    s.push_str(&row("/import", "Import Claude-style project hints"));
    s.push_str(&row("/feedback", "Write a local diagnostic dump"));
    s.push_str(&row(
        "/setup-default-sandbox",
        "Reset sandbox / perms helper",
    ));
    s.push_str(&row(
        "/sandbox-add-read-dir <p>",
        "Extra readable directory",
    ));
    s.push_str("\nCLI equivalents\n");
    s.push_str("  dsh login                 dsh config set-api-key\n");
    s.push_str("  dsh doctor                diagnostics\n");
    s.push_str("  dsh resume [--last]       reopen a session\n");
    s.push_str("  dsh --help                full CLI command list\n");
    s
}

pub const SLASH_COMMANDS: &[&str] = &[
    "/help",
    "/quit",
    "/exit",
    "/clear",
    "/new",
    "/resume",
    "/fork",
    "/compact",
    "/copy",
    "/model",
    "/permissions",
    "/approval",
    "/sandbox",
    "/security-research",
    "/approvals",
    "/approve",
    "/status",
    "/diff",
    "/plan",
    "/review",
    "/init",
    "/mcp",
    "/keymap",
    "/thinking",
    "/fast",
    "/apikey",
    "/api-key",
    "/api-status",
    "/apistatus",
    "/skills",
    "/plugins",
    "/apps",
    "/session",
    "/legend",
    "/sidebar",
    "/vim",
    "/raw",
    "/rename",
    "/archive",
    "/delete",
    "/goal",
    "/personality",
    "/ps",
    "/stop",
    "/mention",
    "/ide",
    "/agent",
    "/subagents",
    "/hooks",
    "/memories",
    "/usage",
    "/statusline",
    "/theme",
    "/side",
    "/btw",
    "/logout",
    "/experimental",
    "/feedback",
    "/import",
    "/pets",
    "/pet",
    "/title",
    "/debug-config",
    "/setup-default-sandbox",
    "/sandbox-add-read-dir",
];

/// Tab-complete a partial slash command. Returns replacement text or None.
pub fn autocomplete_slash(input: &str) -> Option<String> {
    if !input.starts_with('/') || input.contains(char::is_whitespace) {
        return None;
    }
    let matches: Vec<&str> = SLASH_COMMANDS
        .iter()
        .copied()
        .filter(|c| c.starts_with(input))
        .collect();
    match matches.as_slice() {
        [one] => Some(format!("{one} ")),
        many if !many.is_empty() => {
            // Longest common prefix among matches.
            let mut prefix = many[0].to_string();
            for m in &many[1..] {
                while !m.starts_with(&prefix) {
                    prefix.pop();
                }
            }
            if prefix.len() > input.len() {
                Some(prefix)
            } else {
                None
            }
        }
        _ => None,
    }
}

pub fn matching_commands(prefix: &str) -> Vec<&'static str> {
    SLASH_COMMANDS
        .iter()
        .copied()
        .filter(|c| c.starts_with(prefix))
        .collect()
}

pub fn handle_slash(ctx: &mut SlashCtx<'_>, text: &str) -> SlashEffect {
    let (cmd, rest) = match text.split_once(char::is_whitespace) {
        Some((c, r)) => (c, r.trim()),
        None => (text, ""),
    };
    let cmd = cmd.to_lowercase();

    match cmd.as_str() {
        "/help" | "/?" => {
            let body = help_topic(rest);
            push_help_blocks(ctx, &body);
            SlashEffect::None
        }
        "/quit" | "/exit" => {
            *ctx.should_quit = true;
            SlashEffect::Quit
        }
        "/keymap" => {
            if rest.is_empty() || rest == "list" || rest == "show" {
                push_help_blocks(ctx, &format!("{KEYMAP_HELP}\n\n{KEYMAP_DEFAULTS}"));
            } else if rest == "reset" {
                push_sys(
                    ctx,
                    "keymap",
                    "custom keymaps not persisted yet — defaults restored (built-in)",
                );
            } else {
                push_help_blocks(
                    ctx,
                    &format!("usage: /keymap [list|reset]\n\n{KEYMAP_HELP}"),
                );
            }
            SlashEffect::None
        }
        "/apikey" | "/api-key" => {
            if rest.is_empty() {
                push_err(ctx, "setup", "usage: /apikey sk-...");
                return SlashEffect::None;
            }
            match dsh_core::save_api_key(&ctx.runtime.outer_home, rest) {
                Ok(path) => {
                    ctx.runtime.llm.set_api_key(rest);
                    *ctx.has_api_key = true;
                    *ctx.status = format!("{} · {}", ctx.model, ctx.cwd);
                    push_ok(
                        ctx,
                        "setup",
                        format!(
                            "API key saved → {} ({})",
                            path.display(),
                            dsh_core::api_key_status(&ctx.runtime.outer_home)
                        ),
                    );
                }
                Err(e) => push_err(ctx, "setup", format!("failed to save API key: {e}")),
            }
            SlashEffect::None
        }
        "/api-status" | "/apistatus" => {
            push_sys(
                ctx,
                "setup",
                dsh_core::api_key_status(&ctx.runtime.outer_home),
            );
            SlashEffect::None
        }
        "/logout" => match clear_api_key(&ctx.runtime.outer_home) {
            Ok(()) => {
                ctx.runtime.llm.clear_api_key();
                *ctx.has_api_key = false;
                push_ok(ctx, "setup", "API key cleared");
                SlashEffect::None
            }
            Err(e) => {
                push_err(ctx, "setup", format!("logout failed: {e}"));
                SlashEffect::None
            }
        },
        "/skills" => {
            push_kind(
                ctx,
                CellKind::Skill,
                "skills",
                if ctx.skill_names.is_empty() {
                    "(none)".into()
                } else {
                    ctx.skill_names.join(", ")
                },
                None,
            );
            SlashEffect::None
        }
        "/plugins" | "/apps" => {
            let label = if cmd == "/apps" { "apps" } else { "plugins" };
            let body = if ctx.plugin_names.is_empty() {
                "(none) — plugins act as app connectors".into()
            } else {
                format!(
                    "{} connector(s):\n{}",
                    ctx.plugin_names.len(),
                    ctx.plugin_names
                        .iter()
                        .map(|n| format!("  · {n}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            };
            push_kind(ctx, CellKind::Plugin, label, body, None);
            SlashEffect::None
        }
        "/session" => {
            let s = ctx.session.read();
            let name = s.display_name();
            let goal = s
                .goal
                .as_deref()
                .map(|g| {
                    if s.goal_paused {
                        format!("{g} (paused)")
                    } else {
                        g.to_string()
                    }
                })
                .unwrap_or_else(|| "(none)".into());
            push_sys(
                ctx,
                "session",
                format!(
                    "id: {}\nname: {}\narchived: {}\ngoal: {}\nevents: {}",
                    s.id,
                    name,
                    s.archived,
                    goal,
                    s.events.len()
                ),
            );
            SlashEffect::None
        }
        "/legend" => {
            push_sys(
                ctx,
                "colors",
                "shell=amber  skill=teal  plugin=violet  plan=blue  fs=green  web=sky  learn=orange  ctm=magenta",
            );
            SlashEffect::None
        }
        "/thinking" => {
            *ctx.show_thinking = !*ctx.show_thinking;
            {
                let mut s = ctx.runtime.settings.write();
                s.show_thinking = Some(*ctx.show_thinking);
            }
            let _ = ctx.runtime.persist_settings();
            push_kind(
                ctx,
                CellKind::Thinking,
                "think",
                format!("show_thinking={}", *ctx.show_thinking),
                None,
            );
            SlashEffect::None
        }
        "/sidebar" => {
            *ctx.sidebar = !*ctx.sidebar;
            {
                let mut s = ctx.runtime.settings.write();
                s.sidebar = Some(*ctx.sidebar);
            }
            let _ = ctx.runtime.persist_settings();
            push_sys(ctx, "ui", format!("sidebar={}", *ctx.sidebar));
            SlashEffect::None
        }
        "/vim" => {
            let next = {
                let mut s = ctx.runtime.settings.write();
                s.vim_mode = !s.vim_mode;
                s.vim_mode
            };
            {
                let mut f = ctx.runtime.features.write();
                f.set("vim_mode", next);
            }
            let _ = ctx.runtime.persist_settings();
            let _ = ctx.runtime.persist_features();
            push_sys(ctx, "vim", format!("vim_mode={}", next));
            SlashEffect::None
        }
        "/raw" => {
            let next = {
                let mut s = ctx.runtime.settings.write();
                s.raw_mode = !s.raw_mode;
                s.raw_mode
            };
            {
                let mut f = ctx.runtime.features.write();
                f.set("raw_scrollback", next);
            }
            let _ = ctx.runtime.persist_settings();
            let _ = ctx.runtime.persist_features();
            push_sys(ctx, "raw", format!("raw_mode={}", next));
            SlashEffect::None
        }
        "/fast" => {
            // Fast mode: disable thinking for lower latency.
            let thinking_on = ctx
                .runtime
                .settings
                .read()
                .thinking
                .unwrap_or(ctx.runtime.llm.config().thinking);
            let next_thinking = !thinking_on;
            ctx.runtime.llm.set_thinking(next_thinking);
            {
                let mut s = ctx.runtime.settings.write();
                s.thinking = Some(next_thinking);
            }
            let _ = ctx.runtime.persist_settings();
            let fast_on = !next_thinking;
            push_sys(
                ctx,
                "fast",
                format!(
                    "fast mode {} (thinking={})",
                    if fast_on { "on" } else { "off" },
                    next_thinking
                ),
            );
            SlashEffect::None
        }
        "/model" => {
            if rest.is_empty() {
                push_sys(
                    ctx,
                    "model",
                    format!(
                        "active model: {}\nusage: /model <name>  (e.g. deepseek-chat, deepseek-reasoner)",
                        ctx.model
                    ),
                );
                return SlashEffect::None;
            }
            let name = rest.to_string();
            ctx.runtime.set_model(&name);
            *ctx.model = name.clone();
            {
                let mut s = ctx.runtime.settings.write();
                s.model = Some(name.clone());
            }
            let _ = ctx.runtime.persist_settings();
            *ctx.status = format!("{} · {}", ctx.model, ctx.cwd);
            push_sys(ctx, "model", format!("model set to {name}"));
            SlashEffect::None
        }
        "/permissions" | "/approvals" => {
            if rest.is_empty() {
                let mode = *ctx.runtime.permissions.read();
                push_sys(
                    ctx,
                    "permissions",
                    format!(
                        "current: {} — {}\n{}\nAlso: /approval  /sandbox\n{}",
                        mode.label(),
                        mode.description(),
                        PERMISSION_HELP,
                        "See /approval and /sandbox for Codex-aligned policies."
                    ),
                );
                return SlashEffect::None;
            }
            match PermissionMode::parse(rest) {
                Some(mode) => match ctx.runtime.set_permissions(mode) {
                    Ok(()) => {
                        *ctx.status = format!("{} · {} · {}", ctx.model, mode.label(), ctx.cwd);
                        push_ok(
                            ctx,
                            "permissions",
                            format!("permissions → {} ({})", mode.label(), mode.description()),
                        );
                    }
                    Err(e) => push_err(ctx, "permissions", format!("failed: {e}")),
                },
                None => push_err(
                    ctx,
                    "permissions",
                    format!(
                        "unknown mode `{rest}`\n{PERMISSION_HELP}\nAlso try /approval or /sandbox"
                    ),
                ),
            }
            SlashEffect::None
        }
        "/security-research" | "/security" => {
            if rest.is_empty() {
                let enabled = ctx.runtime.settings.read().security_research_mode;
                push_sys(
                    ctx,
                    "security-research",
                    format!(
                        "security research mode: {}\nusage: /security-research on|off",
                        if enabled { "on" } else { "off" }
                    ),
                );
                return SlashEffect::None;
            }
            let enabled = match rest.to_ascii_lowercase().as_str() {
                "on" | "true" | "yes" | "1" | "enable" | "enabled" => true,
                "off" | "false" | "no" | "0" | "disable" | "disabled" => false,
                _ => {
                    push_err(ctx, "security-research", "usage: /security-research on|off");
                    return SlashEffect::None;
                }
            };
            match ctx.runtime.set_security_research_mode(enabled) {
                Ok(()) => push_ok(
                    ctx,
                    "security-research",
                    format!(
                        "security research mode → {}",
                        if enabled { "on" } else { "off" }
                    ),
                ),
                Err(err) => push_err(ctx, "security-research", format!("failed: {err}")),
            }
            SlashEffect::None
        }
        "/approval" => {
            if rest.is_empty() {
                let policy = ctx.runtime.settings.read().approval;
                push_sys(
                    ctx,
                    "approval",
                    format!("current: {}\n{APPROVAL_HELP}", policy.label()),
                );
                return SlashEffect::None;
            }
            match ApprovalPolicy::parse(rest) {
                Some(policy) => match ctx.runtime.set_approval(policy) {
                    Ok(()) => push_ok(ctx, "approval", format!("approval → {}", policy.label())),
                    Err(e) => push_err(ctx, "approval", format!("failed: {e}")),
                },
                None => push_err(
                    ctx,
                    "approval",
                    format!("unknown policy `{rest}`\n{APPROVAL_HELP}"),
                ),
            }
            SlashEffect::None
        }
        "/sandbox" => {
            if rest.is_empty() {
                let mode = ctx.runtime.settings.read().sandbox;
                push_sys(
                    ctx,
                    "sandbox",
                    format!("current: {}\n{SANDBOX_HELP}", mode.label()),
                );
                return SlashEffect::None;
            }
            match SandboxMode::parse(rest) {
                Some(mode) => match ctx.runtime.set_sandbox(mode) {
                    Ok(()) => {
                        let perm = *ctx.runtime.permissions.read();
                        *ctx.status = format!("{} · {} · {}", ctx.model, perm.label(), ctx.cwd);
                        push_ok(
                            ctx,
                            "sandbox",
                            format!("sandbox → {} (permissions={})", mode.label(), perm.label()),
                        );
                    }
                    Err(e) => push_err(ctx, "sandbox", format!("failed: {e}")),
                },
                None => push_err(
                    ctx,
                    "sandbox",
                    format!("unknown mode `{rest}`\n{SANDBOX_HELP}"),
                ),
            }
            SlashEffect::None
        }
        "/approve" => match ctx.runtime.approvals.take_last() {
            Some(denied) => {
                let args = serde_json::to_string(&denied.arguments).unwrap_or_else(|_| "{}".into());
                let prompt = format!(
                    "Please retry the previously denied tool call with my approval.\n\
                     tool: {}\ncall_id: {}\nreason was: {}\narguments: {}\n\
                     Re-invoke the tool now (do not ask again).",
                    denied.name, denied.call_id, denied.reason, args
                );
                push_ok(
                    ctx,
                    "approve",
                    format!("approving retry of `{}` ({})", denied.name, denied.call_id),
                );
                SlashEffect::QueuePrompt(prompt)
            }
            None => {
                push_sys(
                    ctx,
                    "approve",
                    "nothing to approve — no recent denied tool action",
                );
                SlashEffect::None
            }
        },
        "/status" => {
            let perm = *ctx.runtime.permissions.read();
            let settings = ctx.runtime.settings.read().clone();
            let thinking = settings
                .thinking
                .unwrap_or(ctx.runtime.llm.config().thinking);
            let profile = ctx.runtime.model_profile();
            let llm_config = ctx.runtime.llm.config();
            let events = ctx.session.read().events.len();
            let tools = ctx.runtime.tools.names();
            push_sys(
                ctx,
                "status",
                format!(
                    "session: {}\nmodel: {}\nbackend: {}\nllm-ready: {}  fallbacks: {}\noptimization: {}\npermissions: {} ({})\nsecurity-research: {}\nthinking: {}\nvim: {}  raw: {}\npersonality: {}\napi: {}\ncwd: {}\nevents: {}\nskills: {} · plugins: {}\ntools: {}",
                    ctx.session_id,
                    ctx.model,
                    llm_config.backend.label(),
                    ctx.runtime.llm.is_ready(),
                    llm_config.fallbacks.len(),
                    profile.summary(),
                    perm.label(),
                    perm.description(),
                    if settings.security_research_mode { "on" } else { "off" },
                    thinking,
                    settings.vim_mode,
                    settings.raw_mode,
                    settings.personality.as_deref().unwrap_or("default"),
                    dsh_core::api_key_status(&ctx.runtime.outer_home),
                    ctx.cwd,
                    events,
                    ctx.skill_names.len(),
                    ctx.plugin_names.len(),
                    tools.len()
                ),
            );
            SlashEffect::None
        }
        "/diff" => {
            let out = git_diff(Path::new(ctx.cwd));
            push_sys(ctx, "diff", out);
            SlashEffect::None
        }
        "/mcp" => {
            let verbose = rest == "verbose";
            let mcp_body = ctx.runtime.mcp.read().list_summary(verbose);
            let names = ctx.runtime.tools.names();
            let body = if verbose {
                format!(
                    "{mcp_body}\n\nRegistered tools ({}):\n{}",
                    names.len(),
                    names.join("\n")
                )
            } else {
                format!(
                    "{mcp_body}\n{} tools registered — `/mcp verbose` for names",
                    names.len()
                )
            };
            push_sys(ctx, "mcp", body);
            SlashEffect::None
        }
        "/init" => match write_agents_md(Path::new(ctx.cwd)) {
            Ok(path) => {
                push_ok(ctx, "init", format!("wrote {}", path.display()));
                SlashEffect::None
            }
            Err(e) => {
                push_err(ctx, "init", format!("{e}"));
                SlashEffect::None
            }
        },
        "/copy" => {
            let text = latest_assistant(ctx);
            match copy_clipboard(&text) {
                Ok(()) if text.is_empty() => {
                    push_err(ctx, "copy", "no assistant output to copy");
                }
                Ok(()) => push_ok(
                    ctx,
                    "copy",
                    format!("copied {} chars to clipboard", text.chars().count()),
                ),
                Err(e) => push_err(ctx, "copy", format!("clipboard failed: {e}")),
            }
            SlashEffect::None
        }
        "/compact" => {
            let note = ctx.session.write().compact(48);
            ctx.runtime.sessions.persist_now(ctx.session);
            push_sys(ctx, "compact", note.chars().take(800).collect::<String>());
            SlashEffect::None
        }
        "/new" => {
            let session = ctx.runtime.sessions.create();
            *ctx.session_id = session.read().id.clone();
            push_sys(
                ctx,
                "session",
                format!("new session {}", short_id(ctx.session_id)),
            );
            SlashEffect::SwitchSession(session)
        }
        "/clear" => {
            // Codex: clear view + start fresh chat.
            let session = ctx.runtime.sessions.create();
            *ctx.session_id = session.read().id.clone();
            ctx.lines.clear();
            push_sys(
                ctx,
                "session",
                format!(
                    "cleared · new session {} · Esc cancel · /help",
                    short_id(ctx.session_id)
                ),
            );
            SlashEffect::SwitchSession(session)
        }
        "/fork" => {
            let forked = ctx.session.read().fork_clone();
            let session = ctx.runtime.sessions.insert(forked);
            *ctx.session_id = session.read().id.clone();
            push_sys(
                ctx,
                "session",
                format!("forked → {}", short_id(ctx.session_id)),
            );
            SlashEffect::SwitchSession(session)
        }
        "/resume" => {
            let ids = ctx.runtime.sessions.list_ids();
            if rest.is_empty() || rest == "list" {
                if ids.is_empty() {
                    push_sys(ctx, "resume", "no saved sessions");
                } else {
                    let listing = ids
                        .iter()
                        .rev()
                        .take(20)
                        .map(|id| format!("  {id}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    push_sys(
                        ctx,
                        "resume",
                        format!(
                            "sessions (newest last):\n{listing}\nusage: /resume <id> | /resume --last"
                        ),
                    );
                }
                return SlashEffect::None;
            }
            let target = if rest == "--last" || rest == "last" {
                ids.last().cloned()
            } else {
                let prefix = rest.to_string();
                ids.iter()
                    .find(|id| id.starts_with(&prefix) || id == &&prefix)
                    .cloned()
                    .or(Some(prefix))
            };
            let Some(id) = target else {
                push_err(ctx, "resume", "no session to resume");
                return SlashEffect::None;
            };
            match ctx.runtime.sessions.get_or_load(&id) {
                Ok(session) => {
                    *ctx.session_id = session.read().id.clone();
                    push_sys(
                        ctx,
                        "resume",
                        format!("resumed {}", short_id(ctx.session_id)),
                    );
                    SlashEffect::SwitchSession(session)
                }
                Err(e) => {
                    push_err(ctx, "resume", format!("failed to load {id}: {e}"));
                    SlashEffect::None
                }
            }
        }
        "/rename" => {
            if rest.is_empty() {
                push_err(ctx, "rename", "usage: /rename <name>");
                return SlashEffect::None;
            }
            {
                let mut s = ctx.session.write();
                s.rename(rest);
            }
            ctx.runtime.sessions.persist_now(ctx.session);
            push_ok(ctx, "rename", format!("session renamed to `{rest}`"));
            SlashEffect::None
        }
        "/archive" => {
            let id = ctx.session_id.clone();
            match ctx.runtime.sessions.set_archived(&id, true) {
                Ok(()) => {
                    push_ok(
                        ctx,
                        "archive",
                        format!("archived {} — quitting session", short_id(&id)),
                    );
                    *ctx.should_quit = true;
                    SlashEffect::Quit
                }
                Err(e) => {
                    push_err(ctx, "archive", format!("failed: {e}"));
                    SlashEffect::None
                }
            }
        }
        "/delete" => {
            let id = ctx.session_id.clone();
            match ctx.runtime.sessions.delete(&id) {
                Ok(()) => {
                    push_ok(
                        ctx,
                        "delete",
                        format!("deleted {} — quitting", short_id(&id)),
                    );
                    *ctx.should_quit = true;
                    SlashEffect::Quit
                }
                Err(e) => {
                    push_err(ctx, "delete", format!("failed: {e}"));
                    SlashEffect::None
                }
            }
        }
        "/goal" => handle_goal(ctx, rest),
        "/personality" => {
            if rest.is_empty() {
                let current = ctx
                    .runtime
                    .settings
                    .read()
                    .personality
                    .clone()
                    .unwrap_or_else(|| "default".into());
                push_sys(
                    ctx,
                    "personality",
                    format!(
                        "current: {current}\navailable: {}\nusage: /personality <name>",
                        PERSONALITIES.join(", ")
                    ),
                );
                return SlashEffect::None;
            }
            let name = rest.to_lowercase();
            if !PERSONALITIES.iter().any(|p| *p == name) {
                push_err(
                    ctx,
                    "personality",
                    format!(
                        "unknown `{rest}` — choose one of: {}",
                        PERSONALITIES.join(", ")
                    ),
                );
                return SlashEffect::None;
            }
            match ctx.runtime.set_personality(&name) {
                Ok(()) => {
                    ctx.session.write().personality = Some(name.clone());
                    ctx.runtime.sessions.persist_now(ctx.session);
                    push_ok(ctx, "personality", format!("personality → {name}"));
                }
                Err(e) => push_err(ctx, "personality", format!("failed: {e}")),
            }
            SlashEffect::None
        }
        "/ps" => {
            ctx.runtime.bg.refresh();
            push_sys(ctx, "ps", ctx.runtime.bg.format_ps());
            SlashEffect::None
        }
        "/stop" => {
            if rest.is_empty() {
                let n = ctx.runtime.bg.stop_all();
                push_ok(ctx, "stop", format!("stopped {n} background job(s)"));
            } else {
                match rest.parse::<u64>() {
                    Ok(id) => {
                        if ctx.runtime.bg.stop(id) {
                            push_ok(ctx, "stop", format!("stopped job #{id}"));
                        } else {
                            push_err(ctx, "stop", format!("no running job #{id}"));
                        }
                    }
                    Err(_) => push_err(ctx, "stop", "usage: /stop [id]  (omit id to stop all)"),
                }
            }
            SlashEffect::None
        }
        "/mention" => {
            if rest.is_empty() {
                push_err(ctx, "mention", "usage: /mention <path>");
                return SlashEffect::None;
            }
            let path = rest.to_string();
            push_sys(ctx, "mention", format!("attached path: {path}"));
            SlashEffect::QueuePrompt(format!(
                "Please read and consider the file at `{path}` in the current workspace. Summarize relevant context and wait for further instructions."
            ))
        }
        "/ide" => {
            let note = scan_ide_context(Path::new(ctx.cwd));
            push_sys(ctx, "ide", note);
            SlashEffect::None
        }
        "/agent" | "/subagents" => {
            let summaries = ctx.runtime.sessions.list_summaries(true);
            if summaries.is_empty() {
                push_sys(ctx, "agent", "no agent threads (sessions)");
            } else {
                let listing = summaries
                    .iter()
                    .rev()
                    .take(30)
                    .map(|(id, name, archived)| {
                        let mark = if *archived { " [archived]" } else { "" };
                        format!("  {}  {name}{mark}", short_id(id))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                push_sys(
                    ctx,
                    "agent",
                    format!("agent threads (sessions):\n{listing}\nuse /resume <id> to switch"),
                );
            }
            SlashEffect::None
        }
        "/hooks" => {
            let (sub, arg) = match rest.split_once(char::is_whitespace) {
                Some((s, a)) => (s.to_lowercase(), a.trim()),
                None => (rest.to_lowercase(), ""),
            };
            match sub.as_str() {
                "" | "list" | "status" => {
                    push_sys(ctx, "hooks", ctx.runtime.hooks.read().summary());
                }
                "trust" => {
                    {
                        let mut h = ctx.runtime.hooks.write();
                        h.trust_all();
                    }
                    match ctx.runtime.persist_hooks() {
                        Ok(()) => push_ok(ctx, "hooks", "all hooks marked trusted"),
                        Err(e) => push_err(ctx, "hooks", format!("persist failed: {e}")),
                    }
                }
                "enable" => {
                    if arg.is_empty() {
                        push_err(ctx, "hooks", "usage: /hooks enable <name>");
                    } else {
                        let ok = {
                            let mut h = ctx.runtime.hooks.write();
                            h.set_enabled(arg, true)
                        };
                        if ok {
                            let _ = ctx.runtime.persist_hooks();
                            push_ok(ctx, "hooks", format!("enabled `{arg}`"));
                        } else {
                            push_err(ctx, "hooks", format!("hook not found: {arg}"));
                        }
                    }
                }
                "disable" => {
                    if arg.is_empty() {
                        push_err(ctx, "hooks", "usage: /hooks disable <name>");
                    } else {
                        let ok = {
                            let mut h = ctx.runtime.hooks.write();
                            h.set_enabled(arg, false)
                        };
                        if ok {
                            let _ = ctx.runtime.persist_hooks();
                            push_ok(ctx, "hooks", format!("disabled `{arg}`"));
                        } else {
                            push_err(ctx, "hooks", format!("hook not found: {arg}"));
                        }
                    }
                }
                _ => push_err(
                    ctx,
                    "hooks",
                    "usage: /hooks [list|trust|enable <name>|disable <name>]",
                ),
            }
            SlashEffect::None
        }
        "/memories" => handle_memories(ctx, rest),
        "/usage" => {
            let weights = ctx.runtime.learn.weights();
            let events = ctx.session.read().events.len();
            let mut weight_lines: Vec<String> = weights
                .iter()
                .map(|(k, v)| format!("  {k}: {v:.3}"))
                .collect();
            weight_lines.sort();
            if weight_lines.len() > 20 {
                weight_lines.truncate(20);
                weight_lines.push("  …".into());
            }
            let body = if weight_lines.is_empty() {
                format!("session events: {events}\nlearn weights: (none yet)")
            } else {
                format!(
                    "session events: {events}\nlearn weights (usage proxy):\n{}",
                    weight_lines.join("\n")
                )
            };
            push_sys(ctx, "usage", body);
            SlashEffect::None
        }
        "/statusline" => {
            if rest.is_empty() {
                let fields = ctx.runtime.settings.read().statusline.clone();
                push_sys(
                    ctx,
                    "statusline",
                    format!(
                        "current: {}\navailable: {}\nusage: /statusline <field> [field...]",
                        fields.join(" "),
                        STATUSLINE_FIELDS.join(", ")
                    ),
                );
                return SlashEffect::None;
            }
            let fields: Vec<String> = rest.split_whitespace().map(|s| s.to_string()).collect();
            let unknown: Vec<&str> = fields
                .iter()
                .map(|s| s.as_str())
                .filter(|f| !STATUSLINE_FIELDS.contains(f))
                .collect();
            if !unknown.is_empty() {
                push_err(
                    ctx,
                    "statusline",
                    format!(
                        "unknown fields: {}\navailable: {}",
                        unknown.join(", "),
                        STATUSLINE_FIELDS.join(", ")
                    ),
                );
                return SlashEffect::None;
            }
            {
                let mut s = ctx.runtime.settings.write();
                s.statusline = fields.clone();
            }
            let _ = ctx.runtime.persist_settings();
            push_ok(
                ctx,
                "statusline",
                format!("statusline → {}", fields.join(" ")),
            );
            SlashEffect::None
        }
        "/theme" => {
            if rest.is_empty() {
                let current = ctx
                    .runtime
                    .settings
                    .read()
                    .theme
                    .clone()
                    .unwrap_or_else(|| "default".into());
                push_sys(
                    ctx,
                    "theme",
                    format!(
                        "current: {current}\navailable: {}\nusage: /theme <name>",
                        THEMES.join(", ")
                    ),
                );
                return SlashEffect::None;
            }
            let name = rest.to_lowercase();
            if !THEMES.iter().any(|t| *t == name) {
                push_err(
                    ctx,
                    "theme",
                    format!("unknown `{rest}` — choose: {}", THEMES.join(", ")),
                );
                return SlashEffect::None;
            }
            {
                let mut s = ctx.runtime.settings.write();
                s.theme = Some(name.clone());
            }
            let _ = ctx.runtime.persist_settings();
            push_ok(ctx, "theme", format!("theme → {name}"));
            SlashEffect::None
        }
        "/title" => {
            if rest.is_empty() {
                let fields = ctx.runtime.settings.read().title_fields.clone();
                push_sys(
                    ctx,
                    "title",
                    format!(
                        "current: {}\navailable: {}\nusage: /title <field> [field...]",
                        fields.join(" "),
                        TITLE_FIELDS.join(", ")
                    ),
                );
                return SlashEffect::None;
            }
            let fields: Vec<String> = rest.split_whitespace().map(|s| s.to_string()).collect();
            let unknown: Vec<&str> = fields
                .iter()
                .map(|s| s.as_str())
                .filter(|f| !TITLE_FIELDS.contains(f))
                .collect();
            if !unknown.is_empty() {
                push_err(
                    ctx,
                    "title",
                    format!(
                        "unknown fields: {}\navailable: {}",
                        unknown.join(", "),
                        TITLE_FIELDS.join(", ")
                    ),
                );
                return SlashEffect::None;
            }
            {
                let mut s = ctx.runtime.settings.write();
                s.title_fields = fields.clone();
            }
            let _ = ctx.runtime.persist_settings();
            push_ok(ctx, "title", format!("title → {}", fields.join(" ")));
            SlashEffect::None
        }
        "/pets" | "/pet" => {
            if rest.is_empty() {
                let current = ctx
                    .runtime
                    .settings
                    .read()
                    .pet
                    .clone()
                    .unwrap_or_else(|| "none".into());
                push_sys(
                    ctx,
                    "pets",
                    format!(
                        "current: {current}\navailable: {}\nusage: /pet <name>",
                        PETS.join(", ")
                    ),
                );
                return SlashEffect::None;
            }
            let name = rest.to_lowercase();
            if !PETS.iter().any(|p| *p == name) {
                push_err(
                    ctx,
                    "pets",
                    format!("unknown `{rest}` — choose: {}", PETS.join(", ")),
                );
                return SlashEffect::None;
            }
            {
                let mut s = ctx.runtime.settings.write();
                s.pet = Some(name.clone());
            }
            {
                let mut f = ctx.runtime.features.write();
                f.set("pets", name != "none");
            }
            let _ = ctx.runtime.persist_settings();
            let _ = ctx.runtime.persist_features();
            push_ok(ctx, "pets", format!("pet → {name}"));
            SlashEffect::None
        }
        "/side" | "/btw" => {
            if rest.is_empty() {
                push_err(ctx, "side", "usage: /side|/btw <message>");
                return SlashEffect::None;
            }
            let tag = if cmd == "/btw" { "btw" } else { "side" };
            SlashEffect::QueuePrompt(format!("[{tag}] {rest}"))
        }
        "/experimental" => handle_experimental(ctx, rest),
        "/feedback" => match write_feedback_dump(ctx) {
            Ok(path) => {
                push_ok(
                    ctx,
                    "feedback",
                    format!("wrote diagnostic dump → {}", path.display()),
                );
                SlashEffect::None
            }
            Err(e) => {
                push_err(ctx, "feedback", format!("failed: {e}"));
                SlashEffect::None
            }
        },
        "/import" => {
            let note = import_claude_hints(Path::new(ctx.cwd));
            push_sys(ctx, "import", note);
            SlashEffect::None
        }
        "/debug-config" => {
            let dump = debug_config_dump(ctx);
            push_sys(ctx, "debug-config", dump);
            SlashEffect::None
        }
        "/setup-default-sandbox" => {
            #[cfg(windows)]
            {
                push_sys(
                    ctx,
                    "sandbox",
                    "Windows: OS sandbox is limited. Setting permissions to `auto` (ask for risky tools).",
                );
            }
            #[cfg(not(windows))]
            {
                push_sys(
                    ctx,
                    "sandbox",
                    "Setting permissions to `auto` as the default sandbox profile.",
                );
            }
            match ctx.runtime.set_permissions(PermissionMode::Auto) {
                Ok(()) => {
                    let mode = *ctx.runtime.permissions.read();
                    *ctx.status = format!("{} · {} · {}", ctx.model, mode.label(), ctx.cwd);
                    push_ok(
                        ctx,
                        "sandbox",
                        format!("permissions → {} ({})", mode.label(), mode.description()),
                    );
                }
                Err(e) => push_err(ctx, "sandbox", format!("failed: {e}")),
            }
            SlashEffect::None
        }
        "/sandbox-add-read-dir" => {
            if rest.is_empty() {
                push_err(ctx, "sandbox", "usage: /sandbox-add-read-dir <path>");
                return SlashEffect::None;
            }
            let path = rest.to_string();
            {
                let mut s = ctx.runtime.settings.write();
                if !s.extra_read_dirs.iter().any(|p| p == &path) {
                    s.extra_read_dirs.push(path.clone());
                }
            }
            let _ = ctx.runtime.persist_settings();
            let dirs = ctx.runtime.settings.read().extra_read_dirs.clone();
            push_ok(
                ctx,
                "sandbox",
                format!(
                    "added read dir `{path}`\nextra_read_dirs:\n{}",
                    dirs.iter()
                        .map(|d| format!("  · {d}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            );
            SlashEffect::None
        }
        "/plan" => {
            let prompt = if rest.is_empty() {
                "Switch to plan mode: propose a concrete execution plan for the current task/repo before making edits. List steps, risks, and files you would touch.".to_string()
            } else {
                format!("[plan mode] {rest}")
            };
            SlashEffect::QueuePrompt(prompt)
        }
        "/review" => {
            let prompt = if rest.is_empty() {
                "Review the current working tree: summarize changes, flag risks, and suggest fixes. Use tools to inspect git status/diff.".to_string()
            } else {
                format!("[review] {rest}")
            };
            SlashEffect::QueuePrompt(prompt)
        }
        other => {
            push_err(
                ctx,
                "error",
                format!("unknown command: {other} — try /help"),
            );
            SlashEffect::None
        }
    }
}

fn handle_goal(ctx: &mut SlashCtx<'_>, rest: &str) -> SlashEffect {
    let (sub, arg) = match rest.split_once(char::is_whitespace) {
        Some((s, a)) => (s.to_lowercase(), a.trim()),
        None => (rest.to_lowercase(), ""),
    };

    match sub.as_str() {
        "" | "view" | "show" => {
            let s = ctx.session.read();
            match &s.goal {
                Some(g) if s.goal_paused => {
                    push_sys(ctx, "goal", format!("goal (paused): {g}"));
                }
                Some(g) => push_sys(ctx, "goal", format!("goal: {g}")),
                None => push_sys(ctx, "goal", "no goal set — /goal set <text>"),
            }
            SlashEffect::None
        }
        "set" | "edit" => {
            if arg.is_empty() {
                push_err(ctx, "goal", format!("usage: /goal {sub} <text>"));
                return SlashEffect::None;
            }
            if let Err(err) = create_goal_task(ctx, arg) {
                push_err(ctx, "goal", format!("failed to create durable task: {err}"));
                return SlashEffect::None;
            }
            {
                let mut s = ctx.session.write();
                s.goal = Some(arg.to_string());
                s.goal_paused = false;
            }
            ctx.runtime.sessions.persist_now(ctx.session);
            push_ok(ctx, "goal", format!("goal set: {arg}"));
            SlashEffect::None
        }
        "verify" => {
            let Some(task) = ctx.runtime.tasks.task_for_session(&ctx.session.read().id) else {
                push_err(ctx, "goal", "set a goal before adding verification");
                return SlashEffect::None;
            };
            if matches!(task.state, TaskState::Completed | TaskState::Cancelled) {
                push_err(
                    ctx,
                    "goal",
                    "terminal tasks cannot change verification criteria",
                );
                return SlashEffect::None;
            }
            let result = if arg.eq_ignore_ascii_case("clear") {
                ctx.runtime
                    .tasks
                    .update_task(&task.id, |task| task.goal.verification.clear())
            } else if arg.is_empty() {
                push_err(
                    ctx,
                    "goal",
                    "usage: /goal verify <criterion> (or /goal verify clear)",
                );
                return SlashEffect::None;
            } else {
                ctx.runtime.tasks.update_task(&task.id, |task| {
                    if !task.goal.verification.iter().any(|value| value == arg) {
                        task.goal.verification.push(arg.to_string());
                    }
                })
            };
            match result {
                Ok(updated) => push_ok(
                    ctx,
                    "goal",
                    format!("verification criteria: {}", updated.goal.verification.len()),
                ),
                Err(err) => push_err(ctx, "goal", format!("failed to update verification: {err}")),
            }
            SlashEffect::None
        }
        "pause" => {
            if ctx.session.read().goal.is_none() {
                push_err(ctx, "goal", "no goal to pause");
                return SlashEffect::None;
            }
            let task = ctx.runtime.tasks.task_for_session(&ctx.session.read().id);
            if let Some(task) = task {
                if matches!(
                    task.state,
                    TaskState::Queued
                        | TaskState::Running
                        | TaskState::WaitingApproval
                        | TaskState::WaitingEvent
                ) {
                    let _ = ctx.runtime.pause_active_task(&task.id);
                    if let Err(err) = ctx
                        .runtime
                        .tasks
                        .transition_task(&task.id, TaskState::Paused)
                    {
                        push_err(ctx, "goal", format!("failed to pause durable task: {err}"));
                        return SlashEffect::None;
                    }
                }
            }
            {
                let mut s = ctx.session.write();
                s.goal_paused = true;
            }
            ctx.runtime.sessions.persist_now(ctx.session);
            push_ok(ctx, "goal", "goal paused");
            SlashEffect::None
        }
        "resume" => {
            if ctx.session.read().goal.is_none() {
                push_err(ctx, "goal", "no goal to resume");
                return SlashEffect::None;
            }
            let task = ctx.runtime.tasks.task_for_session(&ctx.session.read().id);
            if let Some(task) = task {
                if matches!(
                    task.state,
                    TaskState::Paused | TaskState::WaitingApproval | TaskState::WaitingEvent
                ) {
                    if let Err(err) = ctx
                        .runtime
                        .tasks
                        .transition_task(&task.id, TaskState::Queued)
                    {
                        push_err(ctx, "goal", format!("failed to queue durable task: {err}"));
                        return SlashEffect::None;
                    }
                }
            }
            {
                let mut s = ctx.session.write();
                s.goal_paused = false;
            }
            ctx.runtime.sessions.persist_now(ctx.session);
            push_ok(ctx, "goal", "goal resumed");
            SlashEffect::None
        }
        "clear" => {
            if let Some(task) = ctx.runtime.tasks.task_for_session(&ctx.session.read().id) {
                if matches!(
                    task.state,
                    TaskState::Queued | TaskState::Paused | TaskState::Failed
                ) {
                    let _ = ctx.runtime.cancel_active_task(&task.id);
                    let _ = ctx
                        .runtime
                        .tasks
                        .transition_task(&task.id, TaskState::Cancelled);
                }
            }
            {
                let mut s = ctx.session.write();
                s.goal = None;
                s.goal_paused = false;
            }
            ctx.runtime.sessions.persist_now(ctx.session);
            push_ok(ctx, "goal", "goal cleared");
            SlashEffect::None
        }
        // Treat bare text as set when first token is not a known subcommand.
        other
            if !rest.is_empty()
                && arg.is_empty()
                && !matches!(
                    other,
                    "set" | "edit" | "pause" | "resume" | "view" | "show" | "clear"
                ) =>
        {
            if let Err(err) = create_goal_task(ctx, rest) {
                push_err(ctx, "goal", format!("failed to create durable task: {err}"));
                return SlashEffect::None;
            }
            {
                let mut s = ctx.session.write();
                s.goal = Some(rest.to_string());
                s.goal_paused = false;
            }
            ctx.runtime.sessions.persist_now(ctx.session);
            push_ok(ctx, "goal", format!("goal set: {rest}"));
            SlashEffect::None
        }
        _ => {
            push_err(
                ctx,
                "goal",
                "usage: /goal [set|edit|pause|resume|view|clear] [text]",
            );
            SlashEffect::None
        }
    }
}

fn create_goal_task(ctx: &SlashCtx<'_>, goal: &str) -> anyhow::Result<TaskRecord> {
    let session_id = ctx.session.read().id.clone();
    let previous = ctx.runtime.tasks.task_for_session(&session_id);
    let mut task = TaskRecord::new_with_retry_policy(
        GoalSpec::new(goal),
        RetryPolicy {
            max_attempts: ctx.runtime.config.agent.retry_max_attempts,
            backoff_secs: ctx.runtime.config.agent.retry_backoff_secs,
            max_backoff_secs: ctx.runtime.config.agent.retry_max_backoff_secs,
        },
    )
    .map_err(anyhow::Error::msg)?;
    task.session_id = Some(session_id.clone());
    task.parent_task_id = previous.as_ref().map(|previous| previous.id.clone());
    let created = ctx.runtime.tasks.create_task(task)?;
    if let Some(previous) = previous {
        if !matches!(previous.state, TaskState::Completed | TaskState::Cancelled) {
            let _ = ctx.runtime.cancel_active_task(&previous.id);
            if let Err(error) = ctx
                .runtime
                .tasks
                .transition_task(&previous.id, TaskState::Cancelled)
            {
                tracing::debug!(
                    error = %error,
                    task_id = %previous.id,
                    "previous goal could not be superseded"
                );
            }
            let _ = ctx.runtime.record_system_event(
                "task.superseded",
                &serde_json::json!({
                    "task_id": created.id,
                    "parent_task_id": previous.id,
                }),
            );
        }
    }
    Ok(created)
}

fn handle_memories(ctx: &mut SlashCtx<'_>, rest: &str) -> SlashEffect {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    match parts.as_slice() {
        [] | ["status"] => {
            let s = ctx.runtime.settings.read();
            push_sys(
                ctx,
                "memories",
                format!(
                    "memory_inject={}  memory_generate={}\nusage: /memories [on|off|status]\n       /memories inject|generate [on|off]",
                    s.memory_inject, s.memory_generate
                ),
            );
        }
        ["on"] => {
            {
                let mut s = ctx.runtime.settings.write();
                s.memory_inject = true;
                s.memory_generate = true;
            }
            let _ = ctx.runtime.persist_settings();
            push_ok(ctx, "memories", "memory_inject=on  memory_generate=on");
        }
        ["off"] => {
            {
                let mut s = ctx.runtime.settings.write();
                s.memory_inject = false;
                s.memory_generate = false;
            }
            let _ = ctx.runtime.persist_settings();
            push_ok(ctx, "memories", "memory_inject=off  memory_generate=off");
        }
        ["inject", state] | ["generate", state] => {
            let on = matches!(*state, "on" | "true" | "1");
            let off = matches!(*state, "off" | "false" | "0");
            if !on && !off {
                push_err(ctx, "memories", "use on|off");
                return SlashEffect::None;
            }
            let which = parts[0];
            {
                let mut s = ctx.runtime.settings.write();
                if which == "inject" {
                    s.memory_inject = on;
                } else {
                    s.memory_generate = on;
                }
            }
            let _ = ctx.runtime.persist_settings();
            push_ok(
                ctx,
                "memories",
                format!("memory_{which}={}", if on { "on" } else { "off" }),
            );
        }
        _ => {
            push_err(
                ctx,
                "memories",
                "usage: /memories [on|off|status] | /memories inject|generate [on|off]",
            );
        }
    }
    SlashEffect::None
}

fn handle_experimental(ctx: &mut SlashCtx<'_>, rest: &str) -> SlashEffect {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    match parts.as_slice() {
        [] | ["list"] => {
            let settings_exp = ctx.runtime.settings.read().experimental.clone();
            let flags = ctx.runtime.features.read().list();
            let flag_lines = flags
                .iter()
                .map(|(k, v)| format!("  {k}={}", if *v { "on" } else { "off" }))
                .collect::<Vec<_>>()
                .join("\n");
            push_sys(
                ctx,
                "experimental",
                format!(
                    "settings.experimental:\n  network_proxy={}\n  prevent_sleep={}\nfeature flags:\n{flag_lines}\nusage: /experimental <flag> on|off | /experimental list",
                    settings_exp.network_proxy, settings_exp.prevent_sleep
                ),
            );
        }
        [flag, state] => {
            let on = matches!(*state, "on" | "true" | "1");
            let off = matches!(*state, "off" | "false" | "0");
            if !on && !off {
                push_err(ctx, "experimental", "state must be on|off");
                return SlashEffect::None;
            }
            let known = DEFAULT_FLAGS.iter().any(|(k, _)| *k == *flag)
                || matches!(*flag, "network_proxy" | "prevent_sleep");
            if !known {
                push_err(
                    ctx,
                    "experimental",
                    format!("unknown flag `{flag}` — try /experimental list"),
                );
                return SlashEffect::None;
            }
            match *flag {
                "network_proxy" | "experimental_network_proxy" => {
                    {
                        let mut s = ctx.runtime.settings.write();
                        s.experimental.network_proxy = on;
                    }
                    {
                        let mut f = ctx.runtime.features.write();
                        f.set("experimental_network_proxy", on);
                    }
                }
                "prevent_sleep" => {
                    {
                        let mut s = ctx.runtime.settings.write();
                        s.experimental.prevent_sleep = on;
                    }
                    {
                        let mut f = ctx.runtime.features.write();
                        f.set("prevent_sleep", on);
                    }
                }
                other => {
                    let mut f = ctx.runtime.features.write();
                    f.set(other, on);
                }
            }
            let _ = ctx.runtime.persist_settings();
            let _ = ctx.runtime.persist_features();
            push_ok(
                ctx,
                "experimental",
                format!("{flag}={}", if on { "on" } else { "off" }),
            );
        }
        _ => {
            push_err(
                ctx,
                "experimental",
                "usage: /experimental [flag on|off|list]",
            );
        }
    }
    SlashEffect::None
}

fn push_sys(ctx: &mut SlashCtx<'_>, header: &str, text: impl Into<String>) {
    push_kind(ctx, CellKind::System, header, text, None);
}

/// Push help as one line per row so the transcript keeps column alignment.
fn push_help_blocks(ctx: &mut SlashCtx<'_>, body: &str) {
    let mut first = true;
    for block in body.split("\n\n") {
        let block = block.trim_end();
        if block.is_empty() {
            continue;
        }
        if !first {
            ctx.lines.push(UiLine {
                kind: CellKind::System,
                text: String::new(),
                header: None,
                running: false,
                ok: None,
            });
        }
        first = false;
        for (i, line) in block.lines().enumerate() {
            ctx.lines.push(UiLine {
                kind: CellKind::System,
                text: line.to_string(),
                header: if i == 0 { Some("help".into()) } else { None },
                running: false,
                ok: None,
            });
        }
    }
}

fn push_ok(ctx: &mut SlashCtx<'_>, header: &str, text: impl Into<String>) {
    push_kind(ctx, CellKind::System, header, text, Some(true));
}

fn push_err(ctx: &mut SlashCtx<'_>, header: &str, text: impl Into<String>) {
    push_kind(ctx, CellKind::Error, header, text, Some(false));
}

fn push_kind(
    ctx: &mut SlashCtx<'_>,
    kind: CellKind,
    header: &str,
    text: impl Into<String>,
    ok: Option<bool>,
) {
    ctx.lines.push(UiLine {
        kind,
        text: text.into(),
        header: Some(header.into()),
        running: false,
        ok,
    });
}

fn short_id(id: &str) -> &str {
    &id[..8.min(id.len())]
}

fn latest_assistant(ctx: &SlashCtx<'_>) -> String {
    for line in ctx.lines.iter().rev() {
        if matches!(line.kind, CellKind::Assistant) && !line.text.is_empty() {
            return line.text.clone();
        }
    }
    for event in ctx.session.read().events.iter().rev() {
        if let dsh_core::SessionEvent::AssistantMessage { text, .. } = event {
            if !text.is_empty() {
                return text.clone();
            }
        }
    }
    String::new()
}

fn copy_clipboard(text: &str) -> anyhow::Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    #[cfg(windows)]
    {
        use std::io::Write;
        use std::process::Stdio;
        let mut child = Command::new("cmd")
            .args(["/C", "clip"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes())?;
        }
        let status = child.wait()?;
        if !status.success() {
            anyhow::bail!("clip exited with {status}");
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        use std::io::Write;
        use std::process::Stdio;
        let mut child = Command::new("pbcopy")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .or_else(|_| {
                Command::new("xclip")
                    .args(["-selection", "clipboard"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
            })?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes())?;
        }
        let status = child.wait()?;
        if !status.success() {
            anyhow::bail!("clipboard helper exited with {status}");
        }
        Ok(())
    }
}

fn git_diff(cwd: &Path) -> String {
    let tracked = Command::new("git")
        .args(["diff", "--stat"])
        .current_dir(cwd)
        .output();
    let untracked = Command::new("git")
        .args(["status", "--short"])
        .current_dir(cwd)
        .output();
    match (tracked, untracked) {
        (Ok(d), Ok(s)) => {
            let diff = String::from_utf8_lossy(&d.stdout);
            let status = String::from_utf8_lossy(&s.stdout);
            if diff.trim().is_empty() && status.trim().is_empty() {
                "(clean working tree)".into()
            } else {
                format!("status:\n{status}\ndiff --stat:\n{diff}")
            }
        }
        (Err(e), _) | (_, Err(e)) => format!("git unavailable: {e}"),
    }
}

fn write_agents_md(cwd: &Path) -> anyhow::Result<std::path::PathBuf> {
    let path = cwd.join("AGENTS.md");
    if path.exists() {
        anyhow::bail!("{} already exists", path.display());
    }
    let body = r#"# AGENTS.md

## Project
Describe the repository purpose and architecture in a few sentences.

## Commands
- Build:
- Test:
- Lint:

## Conventions
- Prefer small, focused changes.
- Do not commit secrets.

## Notes for agents
- Read existing docs before changing public APIs.
"#;
    std::fs::write(&path, body)?;
    Ok(path)
}

fn scan_ide_context(cwd: &Path) -> String {
    let candidates = [
        ".vscode",
        ".idea",
        ".cursor",
        ".vs",
        "AGENTS.md",
        ".editorconfig",
    ];
    let mut found = Vec::new();
    for name in candidates {
        let p = cwd.join(name);
        if p.exists() {
            found.push(format!("  · {}", p.display()));
        }
    }
    if found.is_empty() {
        "No common IDE context files found (.vscode / .idea / .cursor / AGENTS.md).\n\
         Open the project in your editor, or place AGENTS.md / .editorconfig for agent context."
            .into()
    } else {
        format!(
            "IDE / editor context detected:\n{}\n\
             Tip: keep AGENTS.md updated so the agent shares project conventions.",
            found.join("\n")
        )
    }
}

fn import_claude_hints(cwd: &Path) -> String {
    let claude_md = cwd.join("CLAUDE.md");
    let claude_dir = cwd.join(".claude");
    let agents = cwd.join("AGENTS.md");
    let mut lines = Vec::new();
    if claude_md.exists() {
        lines.push(format!("found CLAUDE.md → {}", claude_md.display()));
    }
    if claude_dir.exists() {
        lines.push(format!("found .claude/ → {}", claude_dir.display()));
    }
    if agents.exists() {
        lines.push(format!("AGENTS.md already present → {}", agents.display()));
    } else {
        lines.push(
            "AGENTS.md missing — run /init to create one, or copy hints from CLAUDE.md manually."
                .into(),
        );
    }
    if !claude_md.exists() && !claude_dir.exists() {
        lines.insert(0, "no .claude or CLAUDE.md found in workspace".into());
    } else {
        lines.push(
            "import status: detected Claude-style project files (read-only note; no auto-merge)."
                .into(),
        );
    }
    lines.join("\n")
}

fn write_feedback_dump(ctx: &SlashCtx<'_>) -> anyhow::Result<PathBuf> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = ctx.runtime.outer_home.join(format!("feedback-{ts}.txt"));
    let body = debug_config_dump(ctx);
    let header = format!(
        "dsh-rust feedback dump\nsession: {}\nmodel: {}\ncwd: {}\napi: {}\n\n",
        ctx.session_id,
        ctx.model,
        ctx.cwd,
        dsh_core::api_key_status(&ctx.runtime.outer_home)
    );
    std::fs::create_dir_all(&ctx.runtime.outer_home)?;
    std::fs::write(&path, format!("{header}{body}"))?;
    Ok(path)
}

fn debug_config_dump(ctx: &SlashCtx<'_>) -> String {
    let settings = ctx.runtime.settings.read().clone();
    let features = ctx.runtime.features.read().list();
    let mcp = ctx.runtime.mcp.read().list_summary(true);
    let perm = *ctx.runtime.permissions.read();
    let flag_lines = features
        .iter()
        .map(|(k, v)| format!("  {k}={}", if *v { "on" } else { "off" }))
        .collect::<Vec<_>>()
        .join("\n");
    let settings_dump = format!(
        "permissions={}\nmodel={:?}\nthinking={:?}\nsidebar={:?}\nshow_thinking={:?}\n\
         personality={:?}\nvim_mode={}\nraw_mode={}\nmemory_inject={}\nmemory_generate={}\n\
         statusline={:?}\ntitle_fields={:?}\ntheme={:?}\npet={:?}\n\
         experimental.network_proxy={}\nexperimental.prevent_sleep={}\nsecurity_research_mode={}\nextra_read_dirs={:?}",
        settings.permissions.label(),
        settings.model,
        settings.thinking,
        settings.sidebar,
        settings.show_thinking,
        settings.personality,
        settings.vim_mode,
        settings.raw_mode,
        settings.memory_inject,
        settings.memory_generate,
        settings.statusline,
        settings.title_fields,
        settings.theme,
        settings.pet,
        settings.experimental.network_proxy,
        settings.experimental.prevent_sleep,
        settings.security_research_mode,
        settings.extra_read_dirs,
    );
    format!(
        "permissions: {} ({})\n\
         workspace_root: {}\n\
         outer_home: {}\n\n\
         --- settings ---\n{settings_dump}\n\
         --- features ---\n{flag_lines}\n\n\
         --- mcp ---\n{mcp}",
        perm.label(),
        perm.description(),
        ctx.runtime.workspace_root.display(),
        ctx.runtime.outer_home.display(),
    )
}
