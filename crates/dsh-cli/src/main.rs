mod app_server;
mod cloud;
mod mcp_server;
mod startup_inventory;
mod startup_web;

use cloud::CloudProvider;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, shells};
use dsh_core::{
    api_key_status, check_command, clear_api_key, load_features, load_mcp, load_policy_file,
    load_settings, merge_policies, register_builtin_tools, register_learn_tools,
    resolve_api_key_for_backend, save_api_key, save_features, save_mcp, save_settings, strictest,
    AgentEvent, AgentEventContext, AgentLoop, AppConfig, ApprovalPolicy, EventStore,
    ModelOptimizationMode, PermissionMode, Runtime, SandboxMode, Session, TaskRecord, TaskState,
    APPROVAL_HELP, PERMISSION_HELP, SANDBOX_HELP,
};
use dsh_fs::{FsService, PathGuard, PathGuardConfig};
use dsh_llm::{DeepSeekClient, LlmBackend};
use dsh_plugin::{install_plugin_from_path, register_plugin_tools, PluginRegistry};
use dsh_skill::{register_skill_tools, SkillCatalog};
use dsh_tools::ToolRegistry;
use dsh_tui::{run_tui, TuiOptions};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "dsh",
    about = "dsh-rust — OpenAI-compatible agent harness (Codex-style TUI + CLI)",
    long_about = "\
dsh-rust is a two-layer coding agent for hosted and local OpenAI-compatible models.

  dsh                 interactive TUI (default)
  dsh \"fix bugs\"     TUI and auto-send prompt
  dsh exec <prompt>   one-shot / CI run  (alias: e)
  dsh exec-resume     resume session non-interactively
  dsh resume --last   continue last session
  dsh doctor          local diagnostics
  dsh login           save API key
  dsh startup         preview the terminal startup sequence
  dsh --startup       play startup, then enter the interactive TUI
  dsh web             serve the local Harness web app

Inside the TUI, type /help for formatted slash-command help.
Global flags: -m/--model, -s/--sandbox, -a/--ask-for-approval, -c/--config-override,
  --add-dir, -C/--cd, --yolo, --enable/--disable, --search, --permissions, --workspace,
  --startup, --no-startup, --silent
"
)]
struct Cli {
    #[arg(long, global = true)]
    workspace: Option<PathBuf>,

    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Play the startup animation for this interactive TUI or web invocation
    #[arg(long, global = true, conflicts_with = "no_startup")]
    startup: bool,

    /// Skip the startup animation for this invocation
    #[arg(long, global = true, conflicts_with = "startup")]
    no_startup: bool,

    /// Mute startup sound for this invocation
    #[arg(long, global = true)]
    silent: bool,

    /// Override model for this invocation (e.g. deepseek-v4-flash)
    #[arg(short = 'm', long, global = true)]
    model: Option<String>,

    /// OpenAI-compatible backend preset (deepseek|ollama|llama_cpp|vllm|sglang|litellm|localai|tgi|mlx_lm|lm_studio)
    #[arg(long, global = true)]
    backend: Option<String>,

    /// Force model optimization branch: auto|small|standard|off
    #[arg(long = "model-optimization", global = true)]
    model_optimization: Option<String>,

    /// Explicit model parameter count in billions (overrides model-id inference)
    #[arg(long = "model-size-b", global = true)]
    model_size_b: Option<f32>,

    /// Override permission mode for this invocation (read-only|auto|full-access)
    #[arg(long, global = true)]
    permissions: Option<String>,

    /// Sandbox mode: read-only | workspace-write | danger-full-access
    #[arg(short = 's', long, global = true)]
    sandbox: Option<String>,

    /// Approval policy: never | on-request | untrusted
    #[arg(short = 'a', long = "ask-for-approval", global = true)]
    ask_for_approval: Option<String>,

    /// Override config key=value (repeatable). Known keys: model, backend, thinking, permissions
    #[arg(short = 'c', long = "config-override", global = true)]
    config_override: Vec<String>,

    /// Extra writable/readable dirs (repeatable)
    #[arg(long = "add-dir", global = true)]
    add_dir: Vec<PathBuf>,

    /// Change working directory before boot
    #[arg(short = 'C', long = "cd", global = true)]
    cd: Option<PathBuf>,

    /// Bypass approvals and use danger-full-access sandbox
    #[arg(long, global = true)]
    yolo: bool,

    /// Alias of --yolo
    #[arg(long = "dangerously-bypass-approvals-and-sandbox", global = true)]
    dangerously_bypass: bool,

    /// Enable feature flag(s) for this invocation
    #[arg(long, global = true)]
    enable: Vec<String>,

    /// Disable feature flag(s) for this invocation
    #[arg(long, global = true)]
    disable: Vec<String>,

    /// Enable live web search for this session
    #[arg(long, global = true)]
    search: bool,

    /// Optional prompt when no subcommand — opens TUI and auto-sends
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

impl Cli {
    fn validate_startup_flags(&self) -> std::result::Result<(), clap::Error> {
        // Clap validates conflicts within each command scope before propagating
        // globals, so also check flags split across a subcommand boundary.
        if self.startup && self.no_startup {
            return Err(<Self as clap::CommandFactory>::command().error(
                clap::error::ErrorKind::ArgumentConflict,
                "--startup cannot be used with --no-startup",
            ));
        }
        Ok(())
    }
}

#[derive(Subcommand, Debug, Clone)]
enum Commands {
    /// Preview the startup animation without creating a session or starting agents
    Startup {
        #[command(subcommand)]
        action: Option<StartupCmd>,
        #[arg(long, value_parser = ["dark", "light"])]
        theme: Option<String>,
        /// Playback speed multiplier (0.25..3.0)
        #[arg(long, value_parser = parse_startup_speed)]
        speed: Option<f32>,
        /// Confirm three checkpoints with any left click, Enter, or Space (default)
        #[arg(long, conflicts_with = "auto_play")]
        interactive: bool,
        /// Play all six phases automatically without interaction
        #[arg(long = "auto", conflicts_with = "interactive")]
        auto_play: bool,
        /// Show a static startup frame
        #[arg(long)]
        reduced_motion: bool,
    },
    /// Serve the local Harness web app with real sessions, skills and plugins
    Web {
        #[arg(long, default_value_t = 8770)]
        port: u16,
        /// Override the built frontend asset directory
        #[arg(long)]
        assets: Option<PathBuf>,
    },

    /// Interactive TUI (default) — boots even without API key
    Tui {
        #[arg(long)]
        session: Option<String>,
    },
    /// One-shot headless task
    Headless {
        prompt: String,
        #[arg(long)]
        session: Option<String>,
    },
    /// Run a one-shot task (same as headless)
    #[command(visible_alias = "e")]
    Exec {
        prompt: String,
        #[arg(long)]
        session: Option<String>,
        /// Emit NDJSON agent events on stdout
        #[arg(long)]
        json: bool,
        /// Write the final assistant message to this file
        #[arg(long)]
        last_message_file: Option<PathBuf>,
    },
    /// Resume a session non-interactively (Codex `exec resume`)
    #[command(name = "exec-resume", allow_missing_positional = true)]
    ExecResume {
        /// Session id (or unique prefix)
        id: Option<String>,
        /// Resume the most recent session
        #[arg(long)]
        last: bool,
        /// Prompt to send after resume
        prompt: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        last_message_file: Option<PathBuf>,
    },
    /// Fork a session and open it in the TUI
    Fork {
        /// Session id (or unique prefix / name)
        id: Option<String>,
        /// Fork the most recent active session
        #[arg(long)]
        last: bool,
    },
    /// Archive a session
    Archive { session: String },
    /// Unarchive a session
    Unarchive { session: String },
    /// Delete a session
    Delete {
        session: String,
        #[arg(long)]
        force: bool,
    },
    /// Persist the active LLM API key (alias of `config set-api-key`)
    Login {
        /// API key value (or omit to read from stdin)
        key: Option<String>,
    },
    /// Clear stored API key
    Logout,
    /// Manage MCP server registry
    Mcp {
        #[command(subcommand)]
        action: McpCmd,
    },
    /// Manage feature flags
    Features {
        #[command(subcommand)]
        action: FeaturesCmd,
    },
    /// Print diagnostic report (paths, credentials, features, mcp, …)
    Doctor,
    /// Print shell completion script
    Completion {
        /// Shell: bash, zsh, fish, or powershell
        shell: String,
    },
    /// Review working tree / commit via a headless agent turn
    Review {
        /// Focus on uncommitted changes (default when no scope flags)
        #[arg(long)]
        uncommitted: bool,
        /// Diff against this base ref (e.g. main)
        #[arg(long)]
        base: Option<String>,
        /// Review a specific commit SHA
        #[arg(long)]
        commit: Option<String>,
        /// Optional extra review instructions
        prompt: Option<String>,
    },
    /// How to update dsh-rust (no self-update binary)
    Update,
    /// Run a shell command (stdout/stderr printed; PathGuard does not wrap OS exec)
    Sandbox {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    /// Manage local, auditable Codex-compatible cloud artifacts.
    Cloud {
        #[command(subcommand)]
        action: Option<CloudCmd>,
    },
    /// JSON-RPC 2.0 app-server over stdio or TCP.
    #[command(name = "app-server")]
    AppServer {
        #[arg(long)]
        listen: Option<String>,
    },
    /// Long-running remote-control compatible supervisor.
    #[command(name = "remote-control")]
    RemoteControl {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Open desktop app (stub — use TUI)
    App,
    /// Run the durable scheduler until Ctrl+C.
    Daemon {
        /// Status output interval in seconds.
        #[arg(long, default_value_t = 5)]
        interval: u64,
        /// Emit one JSON status object per interval.
        #[arg(long)]
        json: bool,
    },
    /// Read durable protocol events from the local event log.
    Events {
        /// Return events with sequence greater than this value.
        #[arg(long, default_value_t = 0)]
        after: u64,
        /// Maximum number of events to print.
        #[arg(long, default_value_t = 500)]
        limit: usize,
        /// Emit one JSON array instead of pretty text.
        #[arg(long)]
        json: bool,
        /// Keep waiting for new events until Ctrl+C.
        #[arg(long)]
        follow: bool,
        /// Long-poll interval used by --follow.
        #[arg(long, default_value_t = 30_000)]
        wait_ms: u64,
    },
    /// Run dsh as an MCP server over stdio
    #[command(name = "mcp-server")]
    McpServer,
    /// Apply a local cloud artifact through PathGuard.
    Apply {
        /// Artifact id, stored artifact JSON path, or raw patch path.
        artifact: Option<String>,
        /// Validate and show the plan without writing files.
        #[arg(long)]
        dry_run: bool,
        /// Emit a machine-readable JSON result.
        #[arg(long)]
        json: bool,
        /// Optional TCP app-server address used to fetch the artifact.
        #[arg(long, env = "DSH_APP_SERVER")]
        server: Option<String>,
    },
    /// Check execpolicy rule files
    Execpolicy {
        #[command(subcommand)]
        action: ExecpolicyCmd,
    },
    /// Debug helpers (models list, prompt projection)
    Debug {
        #[command(subcommand)]
        action: DebugCmd,
    },
    /// Configure credentials and show status
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
    /// Resume a saved interactive session in the TUI
    Resume {
        /// Session id (or unique prefix)
        id: Option<String>,
        /// Resume the most recent session
        #[arg(long)]
        last: bool,
        /// List sessions instead of opening the TUI
        #[arg(long)]
        all: bool,
    },
    Plugin {
        #[command(subcommand)]
        action: PluginCmd,
    },
    Skill {
        #[command(subcommand)]
        action: SkillCmd,
    },
    /// List or inspect persisted sessions
    Session {
        #[command(subcommand)]
        action: SessionCmd,
    },
    /// Inspect and resume durable long-running tasks.
    Task {
        #[command(subcommand)]
        action: TaskCmd,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum StartupCmd {
    /// Set a one-use choice for the next interactive CLI startup
    Next {
        #[arg(value_parser = ["on", "off"])]
        mode: String,
    },
    /// Serve the immersive HTML startup with real local catalogs (no agent session)
    Web {
        #[arg(long, default_value_t = 8769)]
        port: u16,
    },
    /// Show or persist the identity shared by the web and terminal startup
    Profile {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        badge: Option<String>,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum CloudCmd {
    /// List imported artifacts.
    List {
        #[arg(long)]
        json: bool,
        /// Optional TCP app-server address (for example 127.0.0.1:4567).
        #[arg(long, env = "DSH_APP_SERVER")]
        server: Option<String>,
    },
    /// Show one artifact, including its patch and integrity digest.
    Show {
        id: String,
        #[arg(long)]
        json: bool,
        /// Optional TCP app-server address (for example 127.0.0.1:4567).
        #[arg(long, env = "DSH_APP_SERVER")]
        server: Option<String>,
    },
    /// Import a raw SEARCH/REPLACE patch or artifact JSON document.
    Import {
        path: PathBuf,
        #[arg(long)]
        json: bool,
        /// Optional TCP app-server address (for example 127.0.0.1:4567).
        #[arg(long, env = "DSH_APP_SERVER")]
        server: Option<String>,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum ConfigCmd {
    /// Show whether an API key is configured (masked)
    Status,
    /// Persist the active LLM API key under ~/.dsh-rust/credentials.env
    SetApiKey {
        /// API key value (or omit to read from stdin)
        key: Option<String>,
    },
    /// Remove stored API key
    ClearApiKey,
    /// Show or set permission mode (read-only|auto|full-access)
    Permissions { mode: Option<String> },
    /// Show or set default model
    Model { name: Option<String> },
    /// Show or set authorized security-research prompt mode (on|off).
    SecurityResearch { mode: Option<String> },
}

#[derive(Subcommand, Debug, Clone)]
enum SessionCmd {
    /// List sessions (omit archived unless --all)
    List {
        #[arg(long)]
        all: bool,
    },
    Show {
        id: String,
    },
    Archive {
        session: String,
    },
    Unarchive {
        session: String,
    },
    Delete {
        session: String,
        #[arg(long)]
        force: bool,
    },
    Rename {
        id: String,
        name: String,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum TaskCmd {
    /// List durable tasks (terminal tasks are hidden unless --all).
    List {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show one task and its execution attempts.
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Resume a queued, paused, or failed task.
    Resume {
        id: String,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        last_message_file: Option<PathBuf>,
    },
    /// Pause a task before its next run.
    Pause { id: String },
    /// Add or clear a declarative completion verification criterion.
    Verify {
        id: String,
        criterion: Option<String>,
        #[arg(long)]
        clear: bool,
    },
    /// Cancel a task that has not reached a terminal state.
    Cancel { id: String },
}

#[derive(Subcommand, Debug, Clone)]
enum McpCmd {
    /// List configured MCP servers
    List,
    /// Add a stdio MCP server: dsh mcp add <name> -- <cmd> [args...]
    Add {
        name: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    /// Add an HTTP MCP server
    AddHttp { name: String, url: String },
    /// Remove an MCP server by name
    Remove { name: String },
}

#[derive(Subcommand, Debug, Clone)]
enum FeaturesCmd {
    /// List feature flags
    List,
    Enable {
        name: String,
    },
    Disable {
        name: String,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum PluginCmd {
    List,
    Add {
        path: PathBuf,
    },
    Reload,
    /// Manage plugin marketplace sources
    Marketplace {
        #[command(subcommand)]
        action: MarketplaceCmd,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum MarketplaceCmd {
    /// Add a marketplace source (git URL or path)
    Add { source: String },
    /// List configured marketplaces
    List,
    /// Remove a marketplace by name
    Remove { name: String },
}

#[derive(Subcommand, Debug, Clone)]
enum ExecpolicyCmd {
    /// Evaluate a command against rule files
    Check {
        /// Rule file path(s) (toml or json)
        #[arg(long)]
        rules: Vec<PathBuf>,
        /// Pretty-print JSON result
        #[arg(long)]
        pretty: bool,
        /// Command tokens to check
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum DebugCmd {
    /// Print known LLM model ids as JSON
    Models,
    /// Print OpenAI-compatible local backend presets as JSON
    Backends,
    /// Explain the active <70B optimization branch
    #[command(name = "model-profile")]
    ModelProfile,
    /// Build system prompt + derive_messages for an empty session
    #[command(name = "prompt-input")]
    PromptInput { prompt: Option<String> },
}

#[derive(Subcommand, Debug, Clone)]
enum SkillCmd {
    List,
    Load { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct MarketplacesFile {
    #[serde(default)]
    marketplaces: Vec<MarketplaceEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MarketplaceEntry {
    name: String,
    source: String,
}

struct Boot {
    runtime: Arc<Runtime>,
    skills: Arc<SkillCatalog>,
    plugins: Arc<PluginRegistry>,
}

async fn cloud_provider(outer_home: &Path, server: Option<&str>) -> Result<Box<dyn CloudProvider>> {
    if let Some(server) = server {
        Ok(Box::new(cloud::RemoteCloudProvider::connect(server).await?))
    } else {
        Ok(Box::new(cloud::LocalCloudProvider::new(
            outer_home.to_path_buf(),
        )))
    }
}

fn main() -> Result<()> {
    load_dotenv_files();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    // Parse outside the async dispatcher so Clap's temporary command builders
    // do not share its large poll stack frame on the Windows main thread.
    let cli = Cli::parse();
    if let Err(error) = cli.validate_startup_flags() {
        error.exit();
    }
    if let Some(ref cd) = cli.cd {
        std::env::set_current_dir(cd)
            .map_err(|e| anyhow::anyhow!("failed to cd to {}: {e}", cd.display()))?;
    }
    if let Some(Commands::Completion { ref shell }) = cli.command {
        return print_completion(shell);
    }
    run(cli)
}

fn print_completion(shell: &str) -> Result<()> {
    let mut cmd = Cli::command();
    let mut out = std::io::stdout();
    match shell.to_lowercase().as_str() {
        "bash" => generate(shells::Bash, &mut cmd, "dsh", &mut out),
        "zsh" => generate(shells::Zsh, &mut cmd, "dsh", &mut out),
        "fish" => generate(shells::Fish, &mut cmd, "dsh", &mut out),
        "powershell" | "pwsh" => generate(shells::PowerShell, &mut cmd, "dsh", &mut out),
        other => anyhow::bail!("unsupported shell `{other}` (expected bash|zsh|fish|powershell)"),
    }
    Ok(())
}

#[tokio::main]
async fn run(cli: Cli) -> Result<()> {
    let workspace = cli
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let config_path = cli.config.clone();

    match cli.command.clone() {
        Some(Commands::Startup {
            action,
            theme,
            speed,
            interactive,
            auto_play,
            reduced_motion,
        }) => {
            let mut config = load_app_config(&workspace, config_path.as_ref())?;
            let outer_home = config.resolve_outer_home()?;
            let roots = plugin_roots(&outer_home, &workspace.join(&config.paths.workspace_outer), &workspace);
            match action {
                Some(StartupCmd::Next { mode }) => {
                    dsh_core::set_next_startup(&outer_home, mode == "on")?;
                    println!("next interactive startup: {mode} (one use)");
                    return Ok(());
                }
                Some(StartupCmd::Web { port }) => {
                    return startup_web::serve(workspace, outer_home, roots, port, !cli.silent && config.tui.startup.sound).await;
                }
                Some(StartupCmd::Profile { name, badge }) => {
                    let mut profile = dsh_core::load_startup_profile(&outer_home)?;
                    let changed = name.is_some() || badge.is_some();
                    if let Some(name) = name { profile.username = name; }
                    if let Some(badge) = badge { profile.badge_id = badge; }
                    if changed { dsh_core::save_startup_profile(&outer_home, &profile)?; }
                    println!("{}", serde_json::to_string_pretty(&dsh_core::load_startup_profile(&outer_home)?)?);
                    return Ok(());
                }
                None => (),
            }
            apply_startup_overrides(&mut config, &cli);
            config.tui.startup.enabled = !cli.no_startup;
            if let Some(theme) = theme {
                config.tui.startup.theme = theme;
            }
            if let Some(speed) = speed {
                config.tui.startup.speed = speed;
            }
            if interactive {
                config.tui.startup.interactive = true;
            }
            if auto_play {
                config.tui.startup.interactive = false;
            }
            if reduced_motion {
                config.tui.startup.reduced_motion = true;
            }
            config.tui.startup.validate()?;
            let loaded = startup_inventory::load(&workspace, &outer_home, &roots, |_| {});
            let context = dsh_tui::StartupContext {
                profile: dsh_core::load_startup_profile(&outer_home)?,
                skill_names: loaded.skills.list().into_iter().map(|s| s.name).collect(),
                plugin_names: loaded.plugins.routing_summaries().into_iter().map(|p| p.name).collect(),
                inventory_loaded: true,
            };
            dsh_tui::preview_startup_with_context(config.tui.startup, context).await?;
        }
        Some(Commands::Web { port, assets }) => {
            let boot = boot_tui(&workspace, config_path.as_ref(), &cli)?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            let startup_override = if cli.no_startup {
                Some(false)
            } else if cli.startup {
                Some(true)
            } else {
                None
            };
            startup_web::serve_harness(boot.runtime, port, assets, startup_override).await?;
        }
        None => {
            let boot = boot_tui(&workspace, config_path.as_ref(), &cli)?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            let has_key = llm_ready(&boot.runtime);
            let opts = TuiOptions {
                startup_enabled: cli.startup,
                startup_disabled: cli.no_startup,
                model: boot.runtime.llm.config().model.clone(),
                cwd: workspace.display().to_string(),
                show_thinking: boot.runtime.config.tui.show_thinking,
                sidebar: boot.runtime.config.tui.sidebar,
                skill_names: boot.skills.list().into_iter().map(|s| s.name).collect(),
                plugin_names: plugin_ids(&boot.plugins),
                session_id: None,
                has_api_key: has_key,
                initial_prompt: cli.prompt.clone(),
            };
            run_tui(boot.runtime, opts).await?;
        }
        Some(Commands::Tui { session }) => {
            let boot = boot_tui(&workspace, config_path.as_ref(), &cli)?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            let has_key = llm_ready(&boot.runtime);
            let opts = TuiOptions {
                startup_enabled: cli.startup,
                startup_disabled: cli.no_startup,
                model: boot.runtime.llm.config().model.clone(),
                cwd: workspace.display().to_string(),
                show_thinking: boot.runtime.config.tui.show_thinking,
                sidebar: boot.runtime.config.tui.sidebar,
                skill_names: boot.skills.list().into_iter().map(|s| s.name).collect(),
                plugin_names: plugin_ids(&boot.plugins),
                session_id: session,
                has_api_key: has_key,
                initial_prompt: cli.prompt.clone(),
            };
            run_tui(boot.runtime, opts).await?;
        }
        Some(Commands::Headless { prompt, session }) => {
            let boot = boot_full(&workspace, config_path.as_ref())?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            require_api_key(&boot.runtime)?;
            run_headless(boot.runtime, prompt, session, false, None, None).await?;
        }
        Some(Commands::Exec {
            prompt,
            session,
            json,
            last_message_file,
        }) => {
            let boot = boot_full(&workspace, config_path.as_ref())?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            require_api_key(&boot.runtime)?;
            run_headless(boot.runtime, prompt, session, json, last_message_file, None).await?;
        }
        Some(Commands::ExecResume {
            id,
            last,
            prompt,
            json,
            last_message_file,
        }) => {
            let boot = boot_full(&workspace, config_path.as_ref())?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            require_api_key(&boot.runtime)?;
            let store = &boot.runtime.sessions;
            let session_id = if last || id.is_none() {
                store
                    .latest_active_id()
                    .or_else(|| store.latest_id())
                    .ok_or_else(|| anyhow::anyhow!("no sessions to resume. Start with: dsh"))?
            } else {
                let q = id.as_deref().unwrap_or("");
                store.resolve(q)?.read().id.clone()
            };
            run_headless(
                boot.runtime,
                prompt,
                Some(session_id),
                json,
                last_message_file,
                None,
            )
            .await?;
        }
        Some(Commands::Fork { id, last }) => {
            let boot = boot_tui(&workspace, config_path.as_ref(), &cli)?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            let store = &boot.runtime.sessions;
            let source_id = if last || id.is_none() {
                store
                    .latest_active_id()
                    .or_else(|| store.latest_id())
                    .ok_or_else(|| anyhow::anyhow!("no sessions to fork. Start with: dsh"))?
            } else {
                let q = id.as_deref().unwrap_or("");
                store.resolve(q)?.read().id.clone()
            };
            let forked = store.resolve(&source_id)?.read().fork_clone();
            let session = store.insert(forked);
            let session_id = session.read().id.clone();
            eprintln!("forked {source_id} → {session_id}");
            let has_key = llm_ready(&boot.runtime);
            let opts = TuiOptions {
                startup_enabled: cli.startup,
                startup_disabled: cli.no_startup,
                model: boot.runtime.llm.config().model.clone(),
                cwd: workspace.display().to_string(),
                show_thinking: boot.runtime.config.tui.show_thinking,
                sidebar: boot.runtime.config.tui.sidebar,
                skill_names: boot.skills.list().into_iter().map(|s| s.name).collect(),
                plugin_names: plugin_ids(&boot.plugins),
                session_id: Some(session_id),
                has_api_key: has_key,
                initial_prompt: None,
            };
            run_tui(boot.runtime, opts).await?;
        }
        Some(Commands::Archive { session }) => {
            let boot = boot_full(&workspace, config_path.as_ref())?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            boot.runtime.sessions.set_archived(&session, true)?;
            println!("archived {session}");
        }
        Some(Commands::Unarchive { session }) => {
            let boot = boot_full(&workspace, config_path.as_ref())?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            boot.runtime.sessions.set_archived(&session, false)?;
            println!("unarchived {session}");
        }
        Some(Commands::Delete { session, force }) => {
            let boot = boot_full(&workspace, config_path.as_ref())?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            delete_session(&boot.runtime.sessions, &session, force)?;
        }
        Some(Commands::Login { key }) => {
            let (_config, outer_home, _workspace_outer) =
                load_paths(&workspace, config_path.as_ref())?;
            set_api_key_interactive(&outer_home, key)?;
        }
        Some(Commands::Logout) => {
            let (_config, outer_home, _workspace_outer) =
                load_paths(&workspace, config_path.as_ref())?;
            clear_api_key(&outer_home)?;
            println!("cleared API key");
            println!("{}", api_key_status(&outer_home));
        }
        Some(Commands::Mcp { action }) => {
            let (_config, outer_home, _workspace_outer) =
                load_paths(&workspace, config_path.as_ref())?;
            let mut mcp = load_mcp(&outer_home);
            match action {
                McpCmd::List => {
                    println!("{}", mcp.list_summary(true));
                }
                McpCmd::Add { name, command } => {
                    let mut parts = command.into_iter();
                    let cmd = parts.next().ok_or_else(|| {
                        anyhow::anyhow!("usage: dsh mcp add <name> -- <cmd> [args...]")
                    })?;
                    let args: Vec<String> = parts.collect();
                    mcp.add_stdio(&name, cmd, args);
                    let path = save_mcp(&outer_home, &mcp)?;
                    println!("added MCP server `{name}` → {}", path.display());
                }
                McpCmd::AddHttp { name, url } => {
                    mcp.add_http(&name, &url);
                    let path = save_mcp(&outer_home, &mcp)?;
                    println!("added MCP HTTP server `{name}` → {}", path.display());
                }
                McpCmd::Remove { name } => {
                    if !mcp.remove(&name) {
                        anyhow::bail!("MCP server not found: {name}");
                    }
                    let path = save_mcp(&outer_home, &mcp)?;
                    println!("removed MCP server `{name}` → {}", path.display());
                }
            }
        }
        Some(Commands::Features { action }) => {
            let (_config, outer_home, _workspace_outer) =
                load_paths(&workspace, config_path.as_ref())?;
            let mut features = load_features(&outer_home);
            match action {
                FeaturesCmd::List => {
                    for (name, enabled) in features.list() {
                        let state = if enabled { "on" } else { "off" };
                        println!("  [{state}] {name}");
                    }
                }
                FeaturesCmd::Enable { name } => {
                    features.set(&name, true);
                    let path = save_features(&outer_home, &features)?;
                    println!("enabled `{name}` → {}", path.display());
                }
                FeaturesCmd::Disable { name } => {
                    features.set(&name, false);
                    let path = save_features(&outer_home, &features)?;
                    println!("disabled `{name}` → {}", path.display());
                }
            }
        }
        Some(Commands::Doctor) => {
            let (config, outer_home, workspace_outer) =
                load_paths(&workspace, config_path.as_ref())?;
            print_doctor_report(&workspace, &config, &outer_home, &workspace_outer)?;
        }
        Some(Commands::Completion { .. }) => unreachable!("completion is handled before runtime startup"),
        Some(Commands::Review {
            uncommitted,
            base,
            commit,
            prompt,
        }) => {
            let boot = boot_full(&workspace, config_path.as_ref())?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            require_api_key(&boot.runtime)?;
            let review_prompt = build_review_prompt(
                uncommitted,
                base.as_deref(),
                commit.as_deref(),
                prompt.as_deref(),
            );
            run_headless(boot.runtime, review_prompt, None, false, None, None).await?;
        }
        Some(Commands::Update) => {
            println!("dsh-rust does not self-update.");
            println!("Update with one of:");
            println!("  cargo install --path crates/dsh-cli --force");
            println!("  git pull");
            println!("  cargo build -p dsh-cli --release");
            println!("Or rebuild from your clone after fetching the latest commits.");
        }
        Some(Commands::Sandbox { command }) => {
            let (_config, _outer_home, _workspace_outer) =
                load_paths(&workspace, config_path.as_ref())?;
            eprintln!(
                "note: sandbox CLI exec uses the OS process; PathGuard still applies to agent fs/edit tools, not raw OS exec"
            );
            run_sandbox_command(&workspace, &command)?;
        }
        Some(Commands::Cloud { action }) => {
            let (_config, outer_home, _workspace_outer) =
                load_paths(&workspace, config_path.as_ref())?;
            match action {
                None => {
                    println!("dsh cloud manages local artifacts; use: dsh cloud list|show|import");
                }
                Some(CloudCmd::List { json, server }) => {
                    let provider = cloud_provider(&outer_home, server.as_deref()).await?;
                    let artifacts = provider.list().await?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&artifacts)?);
                    } else if artifacts.is_empty() {
                        println!("(no cloud artifacts)");
                    } else {
                        for artifact in artifacts {
                            println!(
                                "{}\t{}\t{}",
                                artifact.id,
                                artifact.created_at,
                                artifact.source.replace(['\r', '\n'], " ")
                            );
                        }
                    }
                }
                Some(CloudCmd::Show { id, json, server }) => {
                    let provider = cloud_provider(&outer_home, server.as_deref()).await?;
                    let artifact = provider.load(&workspace, &id).await?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&artifact)?);
                    } else {
                        println!("id: {}", artifact.id);
                        println!("created_at: {}", artifact.created_at);
                        println!("source: {}", artifact.source);
                        println!("workspace: {}", artifact.workspace);
                        println!("sha256: {}", artifact.sha256);
                        println!("patch:\n{}", artifact.patch);
                    }
                }
                Some(CloudCmd::Import { path, json, server }) => {
                    let remote = server.is_some();
                    let provider = cloud_provider(&outer_home, server.as_deref()).await?;
                    let artifact = provider.import(&workspace, &path).await;
                    match artifact {
                        Ok(artifact) => {
                            if !remote {
                                let _ = cloud::record_event_at(
                                    &outer_home,
                                    "cloud.artifact.imported",
                                    serde_json::json!({
                                        "artifact_id": artifact.id,
                                        "sha256": artifact.sha256,
                                        "source": artifact.source,
                                        "workspace": artifact.workspace,
                                    }),
                                );
                            }
                            if json {
                                println!("{}", serde_json::to_string_pretty(&artifact)?);
                            } else {
                                if remote {
                                    println!("imported {} via app-server", artifact.id);
                                } else {
                                    println!(
                                        "imported {} → {}",
                                        artifact.id,
                                        cloud::artifact_path(&outer_home, &artifact.id)?.display()
                                    );
                                }
                                println!("sha256: {}", artifact.sha256);
                            }
                        }
                        Err(err) => {
                            if !remote {
                                let _ = cloud::record_event_at(
                                    &outer_home,
                                    "cloud.artifact.rejected",
                                    serde_json::json!({
                                        "source": path.display().to_string(),
                                        "workspace": workspace.display().to_string(),
                                        "error": err.to_string(),
                                    }),
                                );
                            }
                            return Err(err);
                        }
                    }
                }
            }
        }
        Some(Commands::AppServer { listen }) => {
            let boot = boot_full(&workspace, config_path.as_ref())?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            app_server::run_app_server(boot.runtime, listen).await?;
        }
        Some(Commands::RemoteControl { args }) => {
            let mut interval = 5_u64;
            let mut json = false;
            let mut iter = args.into_iter();
            while let Some(arg) = iter.next() {
                if arg == "--json" {
                    json = true;
                } else if let Some(value) = arg.strip_prefix("--interval=") {
                    interval = value.parse().unwrap_or(5);
                } else if arg == "--interval" {
                    interval = iter
                        .next()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(5);
                }
            }
            let boot = boot_full(&workspace, config_path.as_ref())?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            run_daemon(boot.runtime, interval, json).await?;
        }
        Some(Commands::App) => {
            let boot = boot_tui(&workspace, config_path.as_ref(), &cli)?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            let opts = TuiOptions {
                startup_enabled: cli.startup,
                startup_disabled: cli.no_startup,
                model: boot.runtime.llm.config().model.clone(),
                cwd: workspace.display().to_string(),
                show_thinking: boot.runtime.config.tui.show_thinking,
                sidebar: boot.runtime.config.tui.sidebar,
                skill_names: boot.skills.list().into_iter().map(|s| s.name).collect(),
                plugin_names: plugin_ids(&boot.plugins),
                session_id: None,
                has_api_key: llm_ready(&boot.runtime),
                initial_prompt: cli.prompt.clone(),
            };
            run_tui(boot.runtime, opts).await?;
        }
        Some(Commands::Daemon { interval, json }) => {
            let boot = boot_full(&workspace, config_path.as_ref())?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            run_daemon(boot.runtime, interval, json).await?;
        }
        Some(Commands::Events {
            after,
            limit,
            json,
            follow,
            wait_ms,
        }) => {
            let boot = boot_full(&workspace, config_path.as_ref())?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            if follow {
                run_event_follow(boot.runtime, after, limit, json, wait_ms).await?;
            } else {
                let events = read_events_after(&boot.runtime, after, limit)?;
                print_events(&events, json)?;
            }
        }
        Some(Commands::McpServer) => {
            let boot = boot_full(&workspace, config_path.as_ref())?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            mcp_server::run_mcp_server(boot.runtime).await?;
        }
        Some(Commands::Apply {
            artifact,
            dry_run,
            json,
            server,
        }) => {
            let Some(artifact) = artifact else {
                println!("usage: dsh apply <artifact-id-or-path> [--dry-run] [--json]");
                return Ok(());
            };
            let boot = boot_full(&workspace, config_path.as_ref())?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            let provider = cloud_provider(&boot.runtime.outer_home, server.as_deref()).await?;
            let outcome = cloud::apply_artifact_with_provider(
                &boot.runtime,
                provider.as_ref(),
                &artifact,
                dry_run,
            )
            .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&outcome)?);
            } else if dry_run {
                println!("validated {} (dry-run)", outcome.artifact_id);
                println!("files: {}  hunks: {}", outcome.files.len(), outcome.hunks);
                for operation in outcome.operations {
                    println!("  {operation}");
                }
            } else {
                println!("applied {}", outcome.artifact_id);
                if let Some(report) = outcome.report {
                    println!("{report}");
                }
            }
        }
        Some(Commands::Execpolicy { action }) => match action {
            ExecpolicyCmd::Check {
                rules,
                pretty,
                command,
            } => {
                let joined = command.join(" ");
                let mut policies = Vec::new();
                let mut per_file = Vec::new();
                for path in &rules {
                    let policy = load_policy_file(path)?;
                    let result = check_command(&policy, &joined);
                    per_file.push(serde_json::json!({
                        "file": path.display().to_string(),
                        "decision": result.decision,
                        "matched_rule": result.matched_rule,
                        "reason": result.reason,
                    }));
                    policies.push(policy);
                }
                let merged = merge_policies(&policies);
                let merged_result = check_command(&merged, &joined);
                let decision = if per_file.is_empty() {
                    merged_result.decision
                } else {
                    let results: Vec<_> =
                        policies.iter().map(|p| check_command(p, &joined)).collect();
                    strictest(&results)
                };
                let out = serde_json::json!({
                    "command": joined,
                    "decision": decision,
                    "merged": {
                        "decision": merged_result.decision,
                        "matched_rule": merged_result.matched_rule,
                        "reason": merged_result.reason,
                    },
                    "files": per_file,
                });
                if pretty {
                    println!("{}", serde_json::to_string_pretty(&out)?);
                } else {
                    println!("{out}");
                }
            }
        },
        Some(Commands::Debug { action }) => match action {
            DebugCmd::Models => {
                let models = [
                    "deepseek-v4-pro",
                    "deepseek-v4-flash",
                    "deepseek-chat",
                    "deepseek-reasoner",
                    "qwen2.5:14b",
                    "llama-3.1-8b-instruct",
                    "mistral-7b-instruct",
                ];
                println!("{}", serde_json::to_string(&models)?);
            }
            DebugCmd::Backends => {
                let backends: Vec<_> = LlmBackend::ALL
                    .into_iter()
                    .map(|backend| {
                        serde_json::json!({
                            "id": backend.id(),
                            "label": backend.label(),
                            "default_base_url": backend.default_base_url(),
                            "requires_api_key": backend.requires_api_key(),
                            "local": backend.is_local(),
                            "tool_calling_template_dependent": backend.tool_calling_is_template_dependent(),
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&backends)?);
            }
            DebugCmd::ModelProfile => {
                let boot = boot_full(&workspace, config_path.as_ref())?;
                apply_cli_overrides(&boot.runtime, &cli)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&boot.runtime.model_profile())?
                );
            }
            DebugCmd::PromptInput { prompt } => {
                let boot = boot_full(&workspace, config_path.as_ref())?;
                apply_cli_overrides(&boot.runtime, &cli)?;
                boot.runtime.sync_model_optimization();
                let system = boot.runtime.prompt.read().render();
                let mut session = Session::new();
                if let Some(p) = prompt {
                    if !p.trim().is_empty() {
                        session.append(dsh_core::SessionEvent::UserMessage {
                            id: uuid::Uuid::new_v4().to_string(),
                            text: p,
                            at: chrono::Utc::now(),
                        });
                    }
                }
                let messages = session.derive_messages(&system);
                println!("{}", serde_json::to_string_pretty(&messages)?);
            }
        },
        Some(Commands::Resume { id, last, all }) => {
            let boot = boot_tui(&workspace, config_path.as_ref(), &cli)?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            let store = &boot.runtime.sessions;
            let ids = store.list_ids();
            if all {
                for sid in &ids {
                    println!("{sid}");
                }
                return Ok(());
            }
            let session_id = if last || id.is_none() {
                store.latest_id().or_else(|| ids.last().cloned())
            } else {
                let prefix = id.as_deref().unwrap_or("");
                ids.iter()
                    .find(|s| s.as_str() == prefix || s.starts_with(prefix))
                    .cloned()
                    .or_else(|| id.clone())
            };
            let Some(session_id) = session_id else {
                anyhow::bail!("no sessions to resume. Start with: dsh");
            };
            let _ = store.get_or_load(&session_id)?;
            let has_key = llm_ready(&boot.runtime);
            let opts = TuiOptions {
                startup_enabled: cli.startup,
                startup_disabled: cli.no_startup,
                model: boot.runtime.llm.config().model.clone(),
                cwd: workspace.display().to_string(),
                show_thinking: boot.runtime.config.tui.show_thinking,
                sidebar: boot.runtime.config.tui.sidebar,
                skill_names: boot.skills.list().into_iter().map(|s| s.name).collect(),
                plugin_names: plugin_ids(&boot.plugins),
                session_id: Some(session_id),
                has_api_key: has_key,
                initial_prompt: None,
            };
            run_tui(boot.runtime, opts).await?;
        }
        Some(Commands::Config { action }) => {
            let (_config, outer_home, _workspace_outer) =
                load_paths(&workspace, config_path.as_ref())?;
            match action {
                ConfigCmd::Status => {
                    let settings = load_settings(&outer_home);
                    println!("{}", api_key_status(&outer_home));
                    println!(
                        "credentials file: {}",
                        outer_home.join("credentials.env").display()
                    );
                    println!("permissions: {}", settings.permissions.label());
                    println!(
                        "security research mode: {}",
                        if settings.security_research_mode {
                            "on"
                        } else {
                            "off"
                        }
                    );
                    if let Some(m) = settings.model {
                        println!("model: {m}");
                    }
                }
                ConfigCmd::SetApiKey { key } => {
                    set_api_key_interactive(&outer_home, key)?;
                }
                ConfigCmd::ClearApiKey => {
                    clear_api_key(&outer_home)?;
                    println!("cleared API key");
                    println!("{}", api_key_status(&outer_home));
                }
                ConfigCmd::Permissions { mode } => {
                    let mut settings = load_settings(&outer_home);
                    match mode {
                        None => {
                            println!(
                                "permissions: {} — {}",
                                settings.permissions.label(),
                                settings.permissions.description()
                            );
                            print!("{PERMISSION_HELP}");
                        }
                        Some(m) => {
                            let Some(p) = PermissionMode::parse(&m) else {
                                anyhow::bail!("unknown mode `{m}`\n{PERMISSION_HELP}");
                            };
                            settings.permissions = p;
                            let path = save_settings(&outer_home, &settings)?;
                            println!(
                                "permissions → {} ({})\nsaved {}",
                                p.label(),
                                p.description(),
                                path.display()
                            );
                        }
                    }
                }
                ConfigCmd::Model { name } => {
                    let mut settings = load_settings(&outer_home);
                    match name {
                        None => match settings.model {
                            Some(m) => println!("model: {m}"),
                            None => println!("model: (default from config.toml)"),
                        },
                        Some(m) => {
                            settings.model = Some(m.clone());
                            let path = save_settings(&outer_home, &settings)?;
                            println!("model → {m}\nsaved {}", path.display());
                        }
                    }
                }
                ConfigCmd::SecurityResearch { mode } => {
                    let mut settings = load_settings(&outer_home);
                    match mode {
                        None => println!(
                            "security research mode: {}",
                            if settings.security_research_mode {
                                "on"
                            } else {
                                "off"
                            }
                        ),
                        Some(value) => {
                            let enabled = match value.trim().to_ascii_lowercase().as_str() {
                                "on" | "true" | "yes" | "1" | "enable" | "enabled" => true,
                                "off" | "false" | "no" | "0" | "disable" | "disabled" => false,
                                _ => anyhow::bail!(
                                    "unknown security research mode `{value}` (expected on|off)"
                                ),
                            };
                            settings.security_research_mode = enabled;
                            let path = save_settings(&outer_home, &settings)?;
                            println!(
                                "security research mode → {}\nsaved {}",
                                if enabled { "on" } else { "off" },
                                path.display()
                            );
                        }
                    }
                }
            }
        }
        Some(Commands::Plugin { action }) => {
            let (config, outer_home, workspace_outer) =
                load_paths(&workspace, config_path.as_ref())?;
            let tools = Arc::new(ToolRegistry::new());
            let skills = Arc::new(SkillCatalog::new(outer_home.join("meta")));
            let bundled = workspace.join("outer/bundled-skills");
            skills.discover(
                &workspace,
                &outer_home,
                if bundled.exists() {
                    Some(bundled.as_path())
                } else {
                    None
                },
            );
            let plugins = Arc::new(PluginRegistry::new(tools.clone(), outer_home.join("meta")));
            plugins.attach_skills(skills.clone());
            let roots = plugin_roots(&outer_home, &workspace_outer, &workspace);
            seed_example_plugin(&workspace, &outer_home.join("plugins"))?;
            plugins.discover_and_load(&roots);
            let _ = &config;
            match action {
                PluginCmd::List => {
                    println!("{}", serde_json::to_string_pretty(&plugins.list())?);
                }
                PluginCmd::Add { path } => {
                    let dest = outer_home.join("plugins");
                    let id = plugins.install_from_path(&path, &dest)?;
                    println!("installed {id}");
                }
                PluginCmd::Reload => {
                    plugins.reload_all(&roots);
                    println!("reloaded");
                    println!("{}", serde_json::to_string_pretty(&plugins.list())?);
                }
                PluginCmd::Marketplace { action } => {
                    handle_marketplace(&outer_home, action)?;
                }
            }
        }
        Some(Commands::Skill { action }) => {
            let (_config, outer_home, workspace_outer) =
                load_paths(&workspace, config_path.as_ref())?;
            let skills = Arc::new(SkillCatalog::new(outer_home.join("meta")));
            let bundled = workspace.join("outer/bundled-skills");
            skills.discover(
                &workspace,
                &outer_home,
                if bundled.exists() {
                    Some(bundled.as_path())
                } else {
                    None
                },
            );
            let tools = Arc::new(ToolRegistry::new());
            let plugins = Arc::new(PluginRegistry::new(tools, outer_home.join("meta")));
            plugins.attach_skills(skills.clone());
            let roots = plugin_roots(&outer_home, &workspace_outer, &workspace);
            seed_example_plugin(&workspace, &outer_home.join("plugins"))?;
            plugins.discover_and_load(&roots);
            match action {
                SkillCmd::List => {
                    println!("{}", serde_json::to_string_pretty(&skills.list())?);
                }
                SkillCmd::Load { name } => {
                    let Some(rec) = skills.get(&name) else {
                        anyhow::bail!("skill not found: {name}");
                    };
                    println!("{}", rec.body);
                }
            }
        }
        Some(Commands::Session { action }) => {
            let (_config, outer_home, _workspace_outer) =
                load_paths(&workspace, config_path.as_ref())?;
            let store = dsh_core::SessionStore::with_dir(outer_home.join("sessions"));
            match action {
                SessionCmd::List { all } => {
                    for (id, name, archived) in store.list_summaries(all) {
                        let flag = if archived { " [archived]" } else { "" };
                        println!("{id}\t{name}{flag}");
                    }
                }
                SessionCmd::Show { id } => {
                    let session = store.resolve(&id)?;
                    for line in session.read().transcript_lines() {
                        println!("{line}");
                    }
                }
                SessionCmd::Archive { session } => {
                    store.set_archived(&session, true)?;
                    println!("archived {session}");
                }
                SessionCmd::Unarchive { session } => {
                    store.set_archived(&session, false)?;
                    println!("unarchived {session}");
                }
                SessionCmd::Delete { session, force } => {
                    delete_session(&store, &session, force)?;
                }
                SessionCmd::Rename { id, name } => {
                    store.rename(&id, &name)?;
                    println!("renamed {id} → {name}");
                }
            }
        }
        Some(Commands::Task { action }) => {
            let boot = boot_full(&workspace, config_path.as_ref())?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            match action {
                TaskCmd::List { all, json } => {
                    let mut tasks = boot.runtime.tasks.tasks();
                    if !all {
                        tasks.retain(|task| {
                            !matches!(task.state, TaskState::Completed | TaskState::Cancelled)
                        });
                    }
                    if json {
                        println!("{}", serde_json::to_string_pretty(&tasks)?);
                    } else if tasks.is_empty() {
                        println!("(no tasks)");
                    } else {
                        for task in tasks {
                            println!(
                                "{}\t{:?}\t{}",
                                task.id,
                                task.state,
                                task.goal.outcome.replace(['\r', '\n'], " ")
                            );
                        }
                    }
                }
                TaskCmd::Show { id, json } => {
                    let task = resolve_task(&boot.runtime, &id)?;
                    let runs = boot
                        .runtime
                        .tasks
                        .runs()
                        .into_iter()
                        .filter(|run| run.task_id == task.id)
                        .collect::<Vec<_>>();
                    let checkpoint = boot.runtime.tasks.latest_checkpoint(&task.id);
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "task": task,
                                "runs": runs,
                                "latest_checkpoint": checkpoint,
                            }))?
                        );
                    } else {
                        println!("id: {}", task.id);
                        println!("state: {:?}", task.state);
                        println!("goal: {}", task.goal.outcome);
                        println!(
                            "session: {}",
                            task.session_id.as_deref().unwrap_or("(none)")
                        );
                        println!("runs: {}", runs.len());
                        if let Some(run) = runs.last() {
                            println!(
                                "latest run: {} attempt={} state={:?}",
                                run.id, run.attempt, run.state
                            );
                        }
                        if let Some(checkpoint) = checkpoint {
                            println!(
                                "checkpoint: {} step={} event_sequence={}",
                                checkpoint.id, checkpoint.step_index, checkpoint.event_sequence
                            );
                        }
                    }
                }
                TaskCmd::Resume {
                    id,
                    prompt,
                    json,
                    last_message_file,
                } => {
                    let task = resolve_task(&boot.runtime, &id)?;
                    if matches!(task.state, TaskState::Completed | TaskState::Cancelled) {
                        anyhow::bail!("task {} is terminal ({:?})", task.id, task.state);
                    }
                    let session_id = task
                        .session_id
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("task {} has no session", task.id))?;
                    let runtime = boot.runtime;
                    let session = runtime.sessions.get_or_load(&session_id)?;
                    if session.read().goal_paused {
                        session.write().goal_paused = false;
                        runtime.sessions.persist_now(&session);
                    }
                    require_api_key(&runtime)?;
                    run_headless(
                        runtime,
                        prompt.unwrap_or(task.goal.outcome),
                        Some(session_id),
                        json,
                        last_message_file,
                        Some(task.id),
                    )
                    .await?;
                }
                TaskCmd::Pause { id } => {
                    let task = resolve_task(&boot.runtime, &id)?;
                    if matches!(
                        task.state,
                        TaskState::Queued
                            | TaskState::Running
                            | TaskState::WaitingApproval
                            | TaskState::WaitingEvent
                    ) {
                        let _ = boot.runtime.pause_active_task(&task.id);
                        boot.runtime
                            .tasks
                            .transition_task(&task.id, TaskState::Paused)?;
                    }
                    println!("paused {}", task.id);
                }
                TaskCmd::Verify {
                    id,
                    criterion,
                    clear,
                } => {
                    let task = resolve_task(&boot.runtime, &id)?;
                    if matches!(task.state, TaskState::Completed | TaskState::Cancelled) {
                        anyhow::bail!("terminal tasks cannot change verification criteria");
                    }
                    if clear {
                        boot.runtime
                            .tasks
                            .update_task(&task.id, |task| task.goal.verification.clear())?;
                    } else {
                        let criterion = criterion
                            .filter(|value| !value.trim().is_empty())
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "provide a criterion or use --clear (e.g. file_exists:dist/app)"
                                )
                            })?;
                        boot.runtime.tasks.update_task(&task.id, |task| {
                            if !task
                                .goal
                                .verification
                                .iter()
                                .any(|value| value == &criterion)
                            {
                                task.goal.verification.push(criterion.clone());
                            }
                        })?;
                    }
                    let updated = boot.runtime.tasks.task(&task.id).expect("task remains");
                    println!(
                        "verification criteria for {}: {}",
                        task.id,
                        updated.goal.verification.len()
                    );
                }
                TaskCmd::Cancel { id } => {
                    let task = resolve_task(&boot.runtime, &id)?;
                    if !matches!(task.state, TaskState::Completed | TaskState::Cancelled) {
                        let _ = boot.runtime.cancel_active_task(&task.id);
                        boot.runtime
                            .tasks
                            .transition_task(&task.id, TaskState::Cancelled)?;
                    }
                    println!("cancelled {}", task.id);
                }
            }
        }
    }

    Ok(())
}

fn apply_cli_overrides(runtime: &Runtime, cli: &Cli) -> Result<()> {
    for ov in &cli.config_override {
        let Some((k, v)) = ov.split_once('=') else {
            anyhow::bail!("invalid --config-override `{ov}` (expected key=value)");
        };
        let key = k.trim();
        let val = v.trim();
        match key {
            "model" => {
                runtime.set_model(val.to_string());
                runtime.settings.write().model = Some(val.to_string());
            }
            "backend" => {
                let backend: LlmBackend = val.parse().map_err(anyhow::Error::msg)?;
                runtime.set_backend(backend);
            }
            "thinking" => {
                let on = matches!(val.to_lowercase().as_str(), "1" | "true" | "yes" | "on");
                runtime.llm.set_thinking(on);
                runtime.settings.write().thinking = Some(on);
            }
            "permissions" => {
                let Some(mode) = PermissionMode::parse(val) else {
                    anyhow::bail!("unknown permissions `{val}`\n{PERMISSION_HELP}");
                };
                *runtime.permissions.write() = mode;
                runtime.settings.write().permissions = mode;
            }
            other => {
                anyhow::bail!(
                    "unknown config key `{other}` (known: model, backend, thinking, permissions)"
                )
            }
        }
    }

    if let Some(m) = &cli.model {
        runtime.set_model(m.clone());
        runtime.settings.write().model = Some(m.clone());
    }
    if let Some(backend) = &cli.backend {
        runtime.set_backend(backend.parse().map_err(anyhow::Error::msg)?);
    }
    if let Some(mode) = &cli.model_optimization {
        let mode: ModelOptimizationMode = mode.parse().map_err(anyhow::Error::msg)?;
        std::env::set_var("DSH_MODEL_OPTIMIZATION", mode.label());
    }
    if let Some(size) = cli.model_size_b {
        if !size.is_finite() || size <= 0.0 {
            anyhow::bail!("--model-size-b must be a positive finite number");
        }
        std::env::set_var("DSH_MODEL_SIZE_B", size.to_string());
    }
    if cli.model_optimization.is_some() || cli.model_size_b.is_some() {
        runtime.sync_model_optimization();
    }
    if let Some(p) = &cli.permissions {
        let Some(mode) = PermissionMode::parse(p) else {
            anyhow::bail!("unknown mode `{p}`\n{PERMISSION_HELP}");
        };
        *runtime.permissions.write() = mode;
        runtime.settings.write().permissions = mode;
    }
    if let Some(s) = &cli.sandbox {
        let Some(mode) = SandboxMode::parse(s) else {
            anyhow::bail!("unknown sandbox `{s}`\n{SANDBOX_HELP}");
        };
        runtime.set_sandbox(mode)?;
    }
    if let Some(a) = &cli.ask_for_approval {
        let Some(policy) = ApprovalPolicy::parse(a) else {
            anyhow::bail!("unknown approval `{a}`\n{APPROVAL_HELP}");
        };
        runtime.set_approval(policy)?;
    }

    if !cli.add_dir.is_empty() {
        let mut s = runtime.settings.write();
        for d in &cli.add_dir {
            let p = d.display().to_string();
            if !s.add_dirs.iter().any(|x| x == &p) {
                s.add_dirs.push(p);
            }
        }
    }

    {
        let mut features = runtime.features.write();
        for name in &cli.enable {
            features.set(name, true);
        }
        for name in &cli.disable {
            features.set(name, false);
        }
    }

    if cli.search {
        runtime.settings.write().web_search_live = true;
        runtime.features.write().set("web_search", true);
    }

    if cli.yolo || cli.dangerously_bypass {
        runtime.set_sandbox(SandboxMode::DangerFullAccess)?;
        runtime.set_approval(ApprovalPolicy::Never)?;
    }

    Ok(())
}

fn marketplaces_path(outer_home: &Path) -> PathBuf {
    outer_home.join("marketplaces.toml")
}

fn load_marketplaces(outer_home: &Path) -> MarketplacesFile {
    std::fs::read_to_string(marketplaces_path(outer_home))
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_marketplaces(outer_home: &Path, file: &MarketplacesFile) -> Result<PathBuf> {
    std::fs::create_dir_all(outer_home)?;
    let path = marketplaces_path(outer_home);
    let text = toml::to_string_pretty(file)?;
    std::fs::write(&path, text)?;
    Ok(path)
}

fn marketplace_name_from_source(source: &str) -> String {
    let trimmed = source.trim_end_matches('/').trim_end_matches(".git");
    Path::new(trimmed)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("marketplace")
        .to_string()
}

fn handle_marketplace(outer_home: &Path, action: MarketplaceCmd) -> Result<()> {
    let mut file = load_marketplaces(outer_home);
    match action {
        MarketplaceCmd::Add { source } => {
            let name = marketplace_name_from_source(&source);
            if file.marketplaces.iter().any(|m| m.name == name) {
                anyhow::bail!("marketplace `{name}` already exists");
            }
            file.marketplaces.push(MarketplaceEntry {
                name: name.clone(),
                source: source.clone(),
            });
            let path = save_marketplaces(outer_home, &file)?;
            println!("added marketplace `{name}` ({source}) → {}", path.display());
        }
        MarketplaceCmd::List => {
            if file.marketplaces.is_empty() {
                println!("(no marketplaces)");
            } else {
                for m in &file.marketplaces {
                    println!("{}\t{}", m.name, m.source);
                }
            }
        }
        MarketplaceCmd::Remove { name } => {
            let before = file.marketplaces.len();
            file.marketplaces.retain(|m| m.name != name);
            if file.marketplaces.len() == before {
                anyhow::bail!("marketplace not found: {name}");
            }
            let path = save_marketplaces(outer_home, &file)?;
            println!("removed marketplace `{name}` → {}", path.display());
        }
    }
    Ok(())
}

fn require_api_key(runtime: &Runtime) -> Result<()> {
    let config = runtime.llm.config();
    if !runtime.llm.is_ready() {
        anyhow::bail!(
            "no usable LLM endpoint configured for {}. Run: dsh login  (or configure a local fallback in [llm.fallbacks])\n{}",
            config.backend.label(),
            api_key_status(&runtime.outer_home)
        );
    }
    Ok(())
}

fn llm_ready(runtime: &Runtime) -> bool {
    runtime.llm.is_ready()
}

fn set_api_key_interactive(outer_home: &Path, key: Option<String>) -> Result<()> {
    let key = match key {
        Some(k) => k,
        None => {
            eprint!("paste LLM API key: ");
            let mut buf = String::new();
            std::io::stdin().read_line(&mut buf)?;
            buf.trim().to_string()
        }
    };
    let path = save_api_key(outer_home, &key)?;
    println!("saved API key → {}", path.display());
    println!("{}", api_key_status(outer_home));
    Ok(())
}

fn delete_session(store: &dsh_core::SessionStore, session: &str, force: bool) -> Result<()> {
    if !force {
        anyhow::bail!("refusing to delete `{session}` without --force");
    }
    let id = store.resolve(session)?.read().id.clone();
    store.delete(&id)?;
    println!("deleted {id}");
    Ok(())
}

fn resolve_task(runtime: &Runtime, query: &str) -> Result<TaskRecord> {
    let tasks = runtime.tasks.tasks();
    if let Some(task) = tasks.iter().find(|task| task.id == query) {
        return Ok(task.clone());
    }
    let matches: Vec<_> = tasks
        .iter()
        .filter(|task| task.id.starts_with(query))
        .collect();
    match matches.as_slice() {
        [task] => Ok((*task).clone()),
        [] => anyhow::bail!("task not found: {query}"),
        _ => anyhow::bail!("task prefix is ambiguous: {query}"),
    }
}

fn build_review_prompt(
    uncommitted: bool,
    base: Option<&str>,
    commit: Option<&str>,
    extra: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    if let Some(sha) = commit {
        parts.push(format!(
            "Review commit {sha}: summarize the change, flag risks, and suggest fixes. Use git show/diff tools."
        ));
    } else if let Some(b) = base {
        parts.push(format!(
            "Review the diff against base `{b}`: summarize changes, flag risks, and suggest fixes. Use git status/diff tools."
        ));
    } else if uncommitted || (base.is_none() && commit.is_none()) {
        parts.push(
            "Review the current working tree: summarize uncommitted changes, flag risks, and suggest fixes. Use tools to inspect git status/diff."
                .into(),
        );
    }
    if let Some(p) = extra {
        if !p.trim().is_empty() {
            parts.push(format!("[review] {p}"));
        }
    }
    parts.join("\n")
}

fn print_doctor_report(
    workspace: &Path,
    config: &AppConfig,
    outer_home: &Path,
    workspace_outer: &Path,
) -> Result<()> {
    println!("dsh-rust doctor");
    println!("===============");
    println!("workspace:        {}", workspace.display());
    println!("outer_home:       {}", outer_home.display());
    println!("workspace_outer:  {}", workspace_outer.display());
    println!(
        "credentials:      {}",
        outer_home.join("credentials.env").display()
    );
    println!("api key:          {}", api_key_status(outer_home));

    let settings = load_settings(outer_home);
    println!(
        "permissions:      {} — {}",
        settings.permissions.label(),
        settings.permissions.description()
    );
    match &settings.model {
        Some(m) => println!("model (settings): {m}"),
        None => println!("model (settings): (default) {}", config.llm.model),
    }
    let effective_llm = config.to_llm_config(String::new());
    let backend = effective_llm.backend;
    println!(
        "llm backend:       {} ({})",
        backend.label(),
        effective_llm.base_url
    );
    println!("llm fallbacks:     {}", effective_llm.fallbacks.len());
    let profile = dsh_core::ModelProfile::for_model(
        settings
            .model
            .as_deref()
            .unwrap_or(effective_llm.model.as_str()),
        &config.llm.optimization,
    );
    println!("model optimization: {}", profile.summary());
    if let Some(t) = settings.thinking {
        println!("thinking:         {t}");
    }

    let features = load_features(outer_home);
    println!("features:");
    for (name, enabled) in features.list() {
        let state = if enabled { "on" } else { "off" };
        println!("  [{state}] {name}");
    }

    let mcp = load_mcp(outer_home);
    println!("{}", mcp.list_summary(false));

    let store = dsh_core::SessionStore::with_dir(outer_home.join("sessions"));
    let active = store.list_summaries(false).len();
    let all = store.list_summaries(true).len();
    println!("sessions:         {active} active / {all} total");

    let git = StdCommand::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(workspace)
        .output();
    match git {
        Ok(o) if o.status.success() => {
            let branch = String::from_utf8_lossy(&o.stdout).trim().to_string();
            println!("git branch:       {branch}");
        }
        _ => println!("git branch:       (not a git repo or git unavailable)"),
    }

    let rustc = StdCommand::new("rustc").arg("--version").output();
    match rustc {
        Ok(o) if o.status.success() => {
            println!(
                "rustc:            {}",
                String::from_utf8_lossy(&o.stdout).trim()
            );
        }
        _ => println!("rustc:            (unavailable)"),
    }

    Ok(())
}

fn run_sandbox_command(cwd: &Path, command: &[String]) -> Result<()> {
    if command.is_empty() {
        anyhow::bail!("sandbox requires a command");
    }
    // Prefer a foreground OS process so stdout/stderr stream immediately.
    // BgTerminals remains available for in-agent background jobs.
    let (prog, args) = command.split_first().expect("non-empty");
    let output = StdCommand::new(prog).args(args).current_dir(cwd).output()?;
    let mut stdout = std::io::stdout();
    stdout.write_all(&output.stdout)?;
    let mut stderr = std::io::stderr();
    stderr.write_all(&output.stderr)?;
    if !output.status.success() {
        let code = output.status.code().unwrap_or(1);
        anyhow::bail!("sandbox command exited with status {code}");
    }
    Ok(())
}

fn load_dotenv_files() {
    let _ = dotenvy::dotenv();
    if let Ok(cwd) = std::env::current_dir() {
        let _ = dotenvy::from_path(cwd.join(".env"));
    }
    if let Some(home) = dirs::home_dir() {
        let _ = dotenvy::from_path(home.join(".dsh-rust").join("credentials.env"));
        let _ = dotenvy::from_path(home.join(".dsh-rust").join(".env"));
    }
}

fn load_app_config(workspace: &Path, config_path: Option<&PathBuf>) -> Result<AppConfig> {
    if let Some(path) = config_path {
        AppConfig::load(path)
    } else {
        AppConfig::load_default(workspace)
    }
}

fn apply_startup_overrides(config: &mut AppConfig, cli: &Cli) {
    if cli.startup {
        config.tui.startup.enabled = true;
    }
    if cli.no_startup {
        config.tui.startup.enabled = false;
    }
    if cli.silent {
        config.tui.startup.sound = false;
    }
}

fn parse_startup_speed(value: &str) -> std::result::Result<f32, String> {
    let speed: f32 = value
        .parse()
        .map_err(|_| "speed must be a number between 0.25 and 3.0".to_string())?;
    if !speed.is_finite() || !(0.25..=3.0).contains(&speed) {
        return Err("speed must be a number between 0.25 and 3.0".into());
    }
    Ok(speed)
}

fn load_paths(
    workspace: &Path,
    config_path: Option<&PathBuf>,
) -> Result<(AppConfig, PathBuf, PathBuf)> {
    let config = load_app_config(workspace, config_path)?;
    let outer_home = config.resolve_outer_home()?;
    let workspace_outer = workspace.join(&config.paths.workspace_outer);
    std::fs::create_dir_all(outer_home.join("plugins"))?;
    std::fs::create_dir_all(outer_home.join("skills"))?;
    std::fs::create_dir_all(outer_home.join("meta"))?;
    Ok((config, outer_home, workspace_outer))
}

fn plugin_roots(outer_home: &Path, workspace_outer: &Path, workspace: &Path) -> Vec<PathBuf> {
    vec![
        outer_home.join("plugins"),
        workspace_outer.join("plugins"),
        workspace.join("outer/plugins"),
    ]
}

fn plugin_ids(plugins: &PluginRegistry) -> Vec<String> {
    plugins
        .list()
        .into_iter()
        .filter_map(|v| v.get("id").and_then(|x| x.as_str()).map(|s| s.to_string()))
        .collect()
}

async fn run_daemon(runtime: Arc<Runtime>, interval: u64, json: bool) -> Result<()> {
    let cadence = std::time::Duration::from_secs(interval.max(1));
    let mut ticker = tokio::time::interval(cadence);
    eprintln!(
        "dsh daemon running (poll={}s, scheduler enabled={}); press Ctrl+C to stop",
        interval.max(1),
        runtime.scheduler.snapshot().enabled
    );
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let snapshot = runtime.scheduler.snapshot();
                if json {
                    println!("{}", serde_json::to_string(&snapshot)?);
                } else {
                    println!(
                        "scheduler started={} active={} ticks={} scheduled={} retries={} recovered={} errors={}",
                        snapshot.started,
                        snapshot.active_runs,
                        snapshot.tick_count,
                        snapshot.scheduled_runs,
                        snapshot.retry_promotions,
                        snapshot.recovered_runs,
                        snapshot.errors,
                    );
                }
                std::io::stdout().flush()?;
            }
            result = &mut ctrl_c => {
                result?;
                break;
            }
        }
    }
    runtime.stop_scheduler_and_wait().await;
    Ok(())
}

fn read_events_after(
    runtime: &Runtime,
    sequence: u64,
    limit: usize,
) -> Result<Vec<dsh_core::EventEnvelope>> {
    let limit = limit.clamp(1, 5000);
    runtime.events.read_after_limit(sequence, limit)
}

fn print_events(events: &[dsh_core::EventEnvelope], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(events)?);
    } else {
        for event in events {
            println!(
                "#{:>6} {} source={:?} task={} run={}",
                event.sequence,
                event.event_type,
                event.source,
                event.task_id.as_deref().unwrap_or("-"),
                event.run_id.as_deref().unwrap_or("-")
            );
        }
    }
    std::io::stdout().flush()?;
    Ok(())
}

async fn run_event_follow(
    runtime: Arc<Runtime>,
    after: u64,
    limit: usize,
    json: bool,
    wait_ms: u64,
) -> Result<()> {
    let mut sequence = after;
    let wait = std::time::Duration::from_millis(wait_ms.clamp(50, 120_000));
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    eprintln!("following events after sequence {sequence}; press Ctrl+C to stop");
    loop {
        let events = read_events_after(&runtime, sequence, limit)?;
        if !events.is_empty() {
            sequence = events
                .last()
                .map(|event| event.sequence)
                .unwrap_or(sequence);
            print_events(&events, json)?;
            continue;
        }
        let notified = runtime.event_notify.notified();
        tokio::select! {
            result = &mut ctrl_c => {
                result?;
                break;
            }
            _ = notified => {}
            _ = tokio::time::sleep(wait) => {}
        }
    }
    runtime.stop_scheduler_and_wait().await;
    Ok(())
}

fn boot_full(workspace: &Path, config_path: Option<&PathBuf>) -> Result<Boot> {
    let (config, outer_home, workspace_outer) = load_paths(workspace, config_path)?;
    boot_from_config(workspace, config, outer_home, workspace_outer)
}

fn boot_tui(workspace: &Path, config_path: Option<&PathBuf>, cli: &Cli) -> Result<Boot> {
    let (mut config, outer_home, workspace_outer) = load_paths(workspace, config_path)?;
    apply_startup_overrides(&mut config, cli);
    boot_from_config(workspace, config, outer_home, workspace_outer)
}

fn boot_from_config(
    workspace: &Path,
    config: AppConfig,
    outer_home: PathBuf,
    workspace_outer: PathBuf,
) -> Result<Boot> {

    // Optional at boot — empty key is OK; configure later via CLI/TUI.
    let llm_config = config.to_llm_config(String::new());
    let api_key = resolve_api_key_for_backend(&outer_home, llm_config.backend);
    let llm = DeepSeekClient::new(dsh_llm::LlmConfig {
        api_key,
        ..llm_config
    })?;

    let guard = PathGuard::new(PathGuardConfig {
        workspace_root: workspace.to_path_buf(),
        outer_home: outer_home.clone(),
        workspace_outer: workspace_outer.clone(),
        deny_core_writes: config.guard.deny_core_writes,
        deny_patterns: config.guard.deny_patterns.clone(),
    })?;
    let fs = Arc::new(FsService::new(guard));

    let tools = Arc::new(ToolRegistry::new());
    register_builtin_tools(&tools, fs);

    let runtime = Runtime::bootstrap(config, workspace.to_path_buf(), llm, tools.clone())?;
    register_learn_tools(&tools, runtime.learn.clone());

    let skills = Arc::new(SkillCatalog::new(runtime.outer_home.join("meta")));
    let bundled = workspace.join("outer/bundled-skills");
    skills.discover(
        workspace,
        &runtime.outer_home,
        if bundled.exists() {
            Some(bundled.as_path())
        } else {
            None
        },
    );
    register_skill_tools(&tools, skills.clone());
    runtime.attach_skills(skills.clone());

    let plugins = Arc::new(PluginRegistry::new(
        tools.clone(),
        runtime.outer_home.join("meta"),
    ));
    plugins.attach_skills(skills.clone());
    let roots = plugin_roots(&runtime.outer_home, &runtime.workspace_outer, workspace);
    seed_example_plugin(workspace, &runtime.outer_home.join("plugins"))?;
    plugins.discover_and_load(&roots);
    plugins.start_hot_reload();
    register_plugin_tools(&tools, plugins.clone());
    runtime.attach_plugins(plugins.clone());

    let skill_names: Vec<String> = skills.list().into_iter().map(|s| s.name).collect();
    let pids = plugin_ids(&plugins);
    runtime.ctm.ensure_channels(&skill_names, &pids);

    {
        let weights = runtime.learn.weights();
        let ranked_skills = dsh_skill::rank_skills(
            &skills,
            "general coding agent skills plugins architecture",
            &weights,
            runtime.config.agent.skill_prompt_topk,
        );
        let ranked_plugins = dsh_plugin::rank_plugins(
            &plugins,
            "general utility plugin tools",
            &weights,
            runtime.config.agent.plugin_prompt_topk,
        );
        let mut prompt = runtime.prompt.write();
        prompt.set_section("skills", dsh_skill::prompt_topk_section(&ranked_skills));
        prompt.set_section("plugins", dsh_plugin::prompt_topk_section(&ranked_plugins));
        prompt.set_section("learn", runtime.learn.prompt_section("bootstrap"));
    }

    runtime.start_scheduler();

    Ok(Boot {
        runtime,
        skills,
        plugins,
    })
}

fn seed_example_plugin(workspace: &Path, dest_root: &Path) -> Result<()> {
    let src = workspace.join("outer/plugins/echo-plugin");
    let dest = dest_root.join("echo");
    if !src.exists() {
        return Ok(());
    }
    let needs_seed = !dest.exists() || !dest.join("skills").exists();
    if needs_seed {
        let _ = install_plugin_from_path(&src, dest_root)?;
    }
    Ok(())
}

async fn run_headless(
    runtime: Arc<Runtime>,
    prompt: String,
    session_id: Option<String>,
    json: bool,
    last_message_file: Option<PathBuf>,
    task_id: Option<String>,
) -> Result<()> {
    let session = if let Some(id) = session_id {
        runtime.sessions.get_or_load(&id)?
    } else {
        runtime.sessions.create()
    };
    if !json {
        eprintln!("session={}", session.read().id);
    }
    let event_runtime = runtime.clone();
    let agent = AgentLoop::new(runtime);
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let requested_task_id = task_id.clone();
    let handle = if let Some(task_id) = task_id {
        agent.run_task(task_id, session.clone(), prompt, tx).await?
    } else {
        agent.run_turn(session.clone(), prompt, tx).await?
    };
    let mut last_message = String::new();
    let mut protocol_context = AgentEventContext::for_session(session.read().id.clone());
    protocol_context.task_id = requested_task_id;
    while let Some(ev) = rx.recv().await {
        if json {
            if let AgentEvent::TurnStarted(id) = &ev {
                protocol_context.turn_id = Some(id.clone());
                // A requested task id is authoritative. Falling back to the
                // session's latest task is only appropriate for a plain
                // interactive turn; otherwise a concurrently updated task in
                // the same session could steal event correlation.
                if protocol_context.task_id.is_none() {
                    if let Some(task) = event_runtime.tasks.task_for_session(&session.read().id) {
                        protocol_context.task_id = Some(task.id.clone());
                    }
                }
                if let Some(task_id) = protocol_context.task_id.as_deref() {
                    event_runtime.register_agent_handle(task_id.to_string(), handle.clone());
                    protocol_context.run_id = event_runtime
                        .tasks
                        .latest_run_for_task(task_id)
                        .map(|run| run.id);
                }
            }
            let envelope = ev.to_protocol_event(&protocol_context);
            let envelope = match event_runtime.record_event(envelope) {
                Ok(persisted) => persisted,
                Err(err) => {
                    eprintln!("failed to persist JSON event: {err}");
                    ev.to_protocol_event(&protocol_context)
                }
            };
            emit_ndjson(&envelope);
            if let AgentEvent::TextDelta(t) = &ev {
                last_message.push_str(t);
            }
            if matches!(ev, AgentEvent::Done) {
                if let Some(task_id) = protocol_context.task_id.as_deref() {
                    event_runtime.clear_agent_handle(task_id);
                }
                break;
            }
            continue;
        }
        match ev {
            AgentEvent::TextDelta(t) => {
                last_message.push_str(&t);
                print!("{t}");
            }
            AgentEvent::ThoughtTick { t, note } => eprintln!("[ctm:{t}] {note}"),
            AgentEvent::ToolStarted { name, .. } => eprintln!("\n[tool] {name}"),
            AgentEvent::ToolFinished {
                name, ok, preview, ..
            } => eprintln!("[tool:{name}] ok={ok} {preview}"),
            AgentEvent::Error(e) => eprintln!("error: {e}"),
            AgentEvent::Done => break,
            _ => {}
        }
    }
    if let Some(task_id) = protocol_context.task_id.as_deref() {
        event_runtime.clear_agent_handle(task_id);
    }
    if !json {
        println!();
    }
    if let Some(path) = last_message_file {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(&path, &last_message)?;
        if !json {
            eprintln!("wrote last message → {}", path.display());
        }
    }
    Ok(())
}

fn emit_ndjson(event: &dsh_protocol::EventEnvelope) {
    match serde_json::to_string(event) {
        Ok(value) => println!("{value}"),
        Err(err) => eprintln!("failed to encode JSON event: {err}"),
    }
}

#[cfg(test)]
mod startup_cli_tests {
    use super::*;

    #[test]
    fn exec_resume_accepts_the_required_prompt_with_or_without_a_session() {
        let cli = Cli::try_parse_from(["dsh", "exec-resume", "--last", "continue"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::ExecResume { id: None, last: true, ref prompt, .. }) if prompt == "continue"));
        let cli = Cli::try_parse_from(["dsh", "exec-resume", "session-id", "continue"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::ExecResume { id: Some(ref id), last: false, ref prompt, .. }) if id == "session-id" && prompt == "continue"));
    }

    #[test]
    fn startup_opt_in_is_global_and_preserves_the_selected_command() {
        for arguments in [
            vec!["dsh", "--startup"],
            vec!["dsh", "tui", "--startup"],
            vec!["dsh", "resume", "--last", "--startup"],
            vec!["dsh", "fork", "--last", "--startup"],
            vec!["dsh", "app", "--startup"],
            vec!["dsh", "web", "--startup"],
            vec!["dsh", "exec", "hello", "--startup"],
            vec!["dsh", "startup", "--startup"],
        ] {
            let cli = Cli::try_parse_from(&arguments).unwrap();
            assert!(cli.startup, "{arguments:?}");
            assert!(!cli.no_startup);
            let mut config = AppConfig::builtin_default();
            apply_startup_overrides(&mut config, &cli);
            assert!(config.tui.startup.enabled);
            assert!(config.tui.startup.sound);
        }
        let cli = Cli::try_parse_from(["dsh", "resume", "--last", "--startup"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Resume { last: true, .. })));
        let cli = Cli::try_parse_from(["dsh", "--startup", "exec", "hello"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Exec { .. })));
        let cli = Cli::try_parse_from(["dsh", "--startup", "startup"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Startup { .. })));
    }

    #[test]
    fn startup_opt_in_is_optional_and_conflicts_with_opt_out() {
        let cli = Cli::try_parse_from(["dsh"]).unwrap();
        let mut config = AppConfig::builtin_default();
        apply_startup_overrides(&mut config, &cli);
        assert!(!cli.startup);
        assert!(!config.tui.startup.enabled);
        for arguments in [
            vec!["dsh", "--startup", "--no-startup"],
            vec!["dsh", "--startup", "tui", "--no-startup"],
            vec!["dsh", "--no-startup", "tui", "--startup"],
            vec!["dsh", "resume", "--startup", "--no-startup"],
            vec!["dsh", "web", "--startup", "--no-startup"],
        ] {
            let error = Cli::try_parse_from(arguments)
                .and_then(|cli| cli.validate_startup_flags())
                .unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        }
        let cli = Cli::try_parse_from(["dsh", "--startup", "--silent"]).unwrap();
        apply_startup_overrides(&mut config, &cli);
        assert!(config.tui.startup.enabled);
        assert!(!config.tui.startup.sound);
    }

    #[test]
    fn harness_web_command_accepts_assets_and_global_startup_preferences() {
        let cli = Cli::try_parse_from(["dsh", "web"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Web { port: 8770, assets: None })));
        let cli = Cli::try_parse_from([
            "dsh", "web", "--port", "8870", "--assets", "web/dist", "--startup", "--silent",
        ]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Web { port: 8870, assets: Some(ref path) }) if path == Path::new("web/dist")));
        let mut config = AppConfig::builtin_default();
        apply_startup_overrides(&mut config, &cli);
        assert!(config.tui.startup.enabled);
        assert!(!config.tui.startup.sound);
    }

    #[test]
    fn local_web_and_persistent_profile_commands_parse() {
        let cli = Cli::try_parse_from(["dsh", "startup", "web", "--port", "8877"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Startup { action: Some(StartupCmd::Web { port: 8877 }), .. })));
        let cli = Cli::try_parse_from(["dsh", "startup", "profile", "--name", "CatShark"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Startup { action: Some(StartupCmd::Profile { name: Some(ref name), .. }), .. }) if name == "CatShark"));
    }

    #[test]
    fn startup_preview_accepts_local_options_and_global_silent() {
        let cli = Cli::try_parse_from([
            "dsh",
            "startup",
            "--theme",
            "light",
            "--speed",
            "1.5",
            "--interactive",
            "--silent",
        ])
        .unwrap();
        assert!(cli.silent);
        match cli.command {
            Some(Commands::Startup {
                action,
                theme,
                speed,
                interactive,
                auto_play,
                reduced_motion,
            }) => {
                assert!(action.is_none());
                assert_eq!(theme.as_deref(), Some("light"));
                assert_eq!(speed, Some(1.5));
                assert!(interactive);
                assert!(!auto_play);
                assert!(!reduced_motion);
            }
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn global_flags_disable_only_startup_presentation() {
        let cli = Cli::try_parse_from(["dsh", "--no-startup", "--silent", "resume", "--last"])
            .unwrap();
        let mut config = AppConfig::builtin_default();
        apply_startup_overrides(&mut config, &cli);
        assert!(!config.tui.startup.enabled);
        assert!(!config.tui.startup.sound);
        assert_eq!(config.tui.startup.speed, 1.0);
        assert!(matches!(
            cli.command,
            Some(Commands::Resume { last: true, .. })
        ));
    }

    #[test]
    fn preview_rejects_invalid_speed_and_theme() {
        for value in ["0", "NaN", "inf", "4"] {
            assert!(Cli::try_parse_from(["dsh", "startup", "--speed", value]).is_err());
        }
        assert!(Cli::try_parse_from(["dsh", "startup", "--theme", "unknown"]).is_err());
    }

    #[test]
    fn next_startup_control_parses_on_and_off() {
        for mode in ["on", "off"] {
            let cli = Cli::try_parse_from(["dsh", "startup", "next", mode]).unwrap();
            match cli.command {
                Some(Commands::Startup {
                    action: Some(StartupCmd::Next { mode: actual }),
                    ..
                }) => {
                    assert_eq!(actual, mode);
                }
                command => panic!("unexpected command: {command:?}"),
            }
        }
        assert!(Cli::try_parse_from(["dsh", "startup", "next", "invalid"]).is_err());
    }

    #[test]
    fn automatic_preview_is_explicit_and_conflicts_with_interactive() {
        let cli = Cli::try_parse_from(["dsh", "startup", "--auto"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Startup { auto_play: true, interactive: false, .. })));
        assert!(Cli::try_parse_from(["dsh", "startup", "--auto", "--interactive"]).is_err());
        assert!(AppConfig::builtin_default().tui.startup.interactive);
    }
}
