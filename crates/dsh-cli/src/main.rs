mod mcp_server;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, shells};
use dsh_core::{
    api_key_status, check_command, clear_api_key, load_features, load_mcp, load_policy_file,
    load_settings, merge_policies, register_builtin_tools, register_learn_tools, resolve_api_key,
    save_api_key, save_features, save_mcp, save_settings, strictest, AgentEvent, AgentLoop,
    AppConfig, ApprovalPolicy, PermissionMode, Runtime, SandboxMode, Session, APPROVAL_HELP,
    PERMISSION_HELP, SANDBOX_HELP,
};
use dsh_fs::{FsService, PathGuard, PathGuardConfig};
use dsh_llm::DeepSeekClient;
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
    about = "dsh-rust — DeepSeek agent harness (Codex-style TUI + CLI)",
    long_about = "\
dsh-rust is a two-layer DeepSeek coding agent.

  dsh                 interactive TUI (default)
  dsh \"fix bugs\"     TUI and auto-send prompt
  dsh exec <prompt>   one-shot / CI run  (alias: e)
  dsh exec-resume     resume session non-interactively
  dsh resume --last   continue last session
  dsh doctor          local diagnostics
  dsh login           save API key

Inside the TUI, type /help for formatted slash-command help.
Global flags: -m/--model, -s/--sandbox, -a/--ask-for-approval, -c/--config-override,
  --add-dir, -C/--cd, --yolo, --enable/--disable, --search, --permissions, --workspace
"
)]
struct Cli {
    #[arg(long, global = true)]
    workspace: Option<PathBuf>,

    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Override model for this invocation (e.g. deepseek-v4-flash)
    #[arg(short = 'm', long, global = true)]
    model: Option<String>,

    /// Override permission mode for this invocation (read-only|auto|full-access)
    #[arg(long, global = true)]
    permissions: Option<String>,

    /// Sandbox mode: read-only | workspace-write | danger-full-access
    #[arg(short = 's', long, global = true)]
    sandbox: Option<String>,

    /// Approval policy: never | on-request | untrusted
    #[arg(short = 'a', long = "ask-for-approval", global = true)]
    ask_for_approval: Option<String>,

    /// Override config key=value (repeatable). Known keys: model, thinking, permissions
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

#[derive(Subcommand, Debug, Clone)]
enum Commands {
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
    #[command(name = "exec-resume")]
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
    /// Persist DeepSeek API key (alias of `config set-api-key`)
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
    /// Codex cloud chats (not applicable — DeepSeek local harness)
    Cloud {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Codex app-server (protocol bridge stub)
    #[command(name = "app-server")]
    AppServer {
        #[arg(long)]
        listen: Option<String>,
    },
    /// Remote control daemon (stub)
    #[command(name = "remote-control")]
    RemoteControl {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Open desktop app (stub — use TUI)
    App,
    /// Run dsh as an MCP server over stdio
    #[command(name = "mcp-server")]
    McpServer,
    /// Apply cloud diff locally (stub)
    Apply,
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
}

#[derive(Subcommand, Debug, Clone)]
enum ConfigCmd {
    /// Show whether an API key is configured (masked)
    Status,
    /// Persist DeepSeek API key under ~/.dsh-rust/credentials.env
    SetApiKey {
        /// API key value (or omit to read from stdin)
        key: Option<String>,
    },
    /// Remove stored API key
    ClearApiKey,
    /// Show or set permission mode (read-only|auto|full-access)
    Permissions {
        mode: Option<String>,
    },
    /// Show or set default model
    Model {
        name: Option<String>,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum SessionCmd {
    /// List sessions (omit archived unless --all)
    List {
        #[arg(long)]
        all: bool,
    },
    Show { id: String },
    Archive { session: String },
    Unarchive { session: String },
    Delete {
        session: String,
        #[arg(long)]
        force: bool,
    },
    Rename { id: String, name: String },
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
    Enable { name: String },
    Disable { name: String },
}

#[derive(Subcommand, Debug, Clone)]
enum PluginCmd {
    List,
    Add { path: PathBuf },
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
    /// Build system prompt + derive_messages for an empty session
    #[command(name = "prompt-input")]
    PromptInput {
        prompt: Option<String>,
    },
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

#[tokio::main]
async fn main() -> Result<()> {
    load_dotenv_files();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .with_target(false)
        .init();

    let cli = Cli::parse();

    if let Some(ref cd) = cli.cd {
        std::env::set_current_dir(cd)
            .map_err(|e| anyhow::anyhow!("failed to cd to {}: {e}", cd.display()))?;
    }

    let workspace = cli
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let config_path = cli.config.clone();

    match cli.command.clone() {
        None => {
            let boot = boot_full(&workspace, config_path.as_ref())?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            let has_key = boot.runtime.llm.has_api_key();
            let opts = TuiOptions {
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
            let boot = boot_full(&workspace, config_path.as_ref())?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            let has_key = boot.runtime.llm.has_api_key();
            let opts = TuiOptions {
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
            run_headless(boot.runtime, prompt, session, false, None).await?;
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
            run_headless(boot.runtime, prompt, session, json, last_message_file).await?;
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
            )
            .await?;
        }
        Some(Commands::Fork { id, last }) => {
            let boot = boot_full(&workspace, config_path.as_ref())?;
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
            let has_key = boot.runtime.llm.has_api_key();
            let opts = TuiOptions {
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
                    let cmd = parts
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("usage: dsh mcp add <name> -- <cmd> [args...]"))?;
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
        Some(Commands::Completion { shell }) => {
            let mut cmd = Cli::command();
            let bin = "dsh";
            let mut out = std::io::stdout();
            match shell.to_lowercase().as_str() {
                "bash" => generate(shells::Bash, &mut cmd, bin, &mut out),
                "zsh" => generate(shells::Zsh, &mut cmd, bin, &mut out),
                "fish" => generate(shells::Fish, &mut cmd, bin, &mut out),
                "powershell" | "pwsh" => generate(shells::PowerShell, &mut cmd, bin, &mut out),
                other => anyhow::bail!(
                    "unsupported shell `{other}` (expected bash|zsh|fish|powershell)"
                ),
            }
        }
        Some(Commands::Review {
            uncommitted,
            base,
            commit,
            prompt,
        }) => {
            let boot = boot_full(&workspace, config_path.as_ref())?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            require_api_key(&boot.runtime)?;
            let review_prompt =
                build_review_prompt(uncommitted, base.as_deref(), commit.as_deref(), prompt.as_deref());
            run_headless(boot.runtime, review_prompt, None, false, None).await?;
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
        Some(Commands::Cloud { args }) => {
            let _ = args;
            println!(
                "dsh cloud: not available — dsh-rust is a local DeepSeek harness (no Codex Cloud).\nUse: dsh exec / dsh resume / dsh review"
            );
        }
        Some(Commands::AppServer { listen }) => {
            println!(
                "dsh app-server: stub. Local protocol server is not shipped.\nRequested listen: {}\nUse the TUI (`dsh`) or `dsh exec` instead.",
                listen.unwrap_or_else(|| "stdio://".into())
            );
        }
        Some(Commands::RemoteControl { args }) => {
            let _ = args;
            println!(
                "dsh remote-control: stub. Pairing / daemon control is Codex-cloud specific.\nUse the local TUI instead."
            );
        }
        Some(Commands::App) => {
            println!("dsh app: no desktop app. Launch the TUI with `dsh` (or `dsh tui`).");
        }
        Some(Commands::McpServer) => {
            let boot = boot_full(&workspace, config_path.as_ref())?;
            apply_cli_overrides(&boot.runtime, &cli)?;
            mcp_server::run_mcp_server(boot.runtime).await?;
        }
        Some(Commands::Apply) => {
            println!(
                "dsh apply: Codex cloud diffs are not supported.\nApply local patches with git or ask the agent: dsh exec \"apply this patch…\""
            );
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
                    let results: Vec<_> = policies
                        .iter()
                        .map(|p| check_command(p, &joined))
                        .collect();
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
                ];
                println!("{}", serde_json::to_string(&models)?);
            }
            DebugCmd::PromptInput { prompt } => {
                let boot = boot_full(&workspace, config_path.as_ref())?;
                apply_cli_overrides(&boot.runtime, &cli)?;
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
            let boot = boot_full(&workspace, config_path.as_ref())?;
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
            let has_key = boot.runtime.llm.has_api_key();
            let opts = TuiOptions {
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
            let plugins = Arc::new(PluginRegistry::new(
                tools.clone(),
                outer_home.join("meta"),
            ));
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
                runtime.llm.set_model(val.to_string());
                runtime.settings.write().model = Some(val.to_string());
            }
            "thinking" => {
                let on = matches!(
                    val.to_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                );
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
            other => anyhow::bail!(
                "unknown config key `{other}` (known: model, thinking, permissions)"
            ),
        }
    }

    if let Some(m) = &cli.model {
        runtime.llm.set_model(m.clone());
        runtime.settings.write().model = Some(m.clone());
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
    if !runtime.llm.has_api_key() {
        anyhow::bail!(
            "no API key configured. Run: dsh login  (or: dsh config set-api-key <KEY>)\n{}",
            api_key_status(&runtime.outer_home)
        );
    }
    Ok(())
}

fn set_api_key_interactive(outer_home: &Path, key: Option<String>) -> Result<()> {
    let key = match key {
        Some(k) => k,
        None => {
            eprint!("paste DeepSeek API key: ");
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
        None => println!(
            "model (settings): (default) {}",
            config.llm.model
        ),
    }
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
    let output = StdCommand::new(prog)
        .args(args)
        .current_dir(cwd)
        .output()?;
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

fn load_paths(
    workspace: &Path,
    config_path: Option<&PathBuf>,
) -> Result<(AppConfig, PathBuf, PathBuf)> {
    let config = if let Some(p) = config_path {
        AppConfig::load(p)?
    } else {
        AppConfig::load_default(workspace)?
    };
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

fn boot_full(workspace: &PathBuf, config_path: Option<&PathBuf>) -> Result<Boot> {
    let (config, outer_home, workspace_outer) = load_paths(workspace, config_path)?;

    // Optional at boot — empty key is OK; configure later via CLI/TUI.
    let api_key = resolve_api_key(&outer_home);
    let llm = DeepSeekClient::new(config.to_llm_config(api_key))?;

    let guard = PathGuard::new(PathGuardConfig {
        workspace_root: workspace.clone(),
        outer_home: outer_home.clone(),
        workspace_outer: workspace_outer.clone(),
        deny_core_writes: config.guard.deny_core_writes,
        deny_patterns: config.guard.deny_patterns.clone(),
    })?;
    let fs = Arc::new(FsService::new(guard));

    let tools = Arc::new(ToolRegistry::new());
    register_builtin_tools(&tools, fs);

    let runtime = Runtime::bootstrap(config, workspace.clone(), llm, tools.clone())?;
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
    let roots = plugin_roots(
        &runtime.outer_home,
        &runtime.workspace_outer,
        workspace,
    );
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
) -> Result<()> {
    let session = if let Some(id) = session_id {
        runtime.sessions.get_or_load(&id)?
    } else {
        runtime.sessions.create()
    };
    if !json {
        eprintln!("session={}", session.read().id);
    }
    let agent = AgentLoop::new(runtime);
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let _handle = agent.run_turn(session.clone(), prompt, tx).await?;
    let mut last_message = String::new();
    while let Some(ev) = rx.recv().await {
        if json {
            emit_ndjson(&ev);
            if let AgentEvent::TextDelta(t) = &ev {
                last_message.push_str(t);
            }
            if matches!(ev, AgentEvent::Done) {
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
                name,
                ok,
                preview,
                ..
            } => eprintln!("[tool:{name}] ok={ok} {preview}"),
            AgentEvent::Error(e) => eprintln!("error: {e}"),
            AgentEvent::Done => break,
            _ => {}
        }
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

fn emit_ndjson(ev: &AgentEvent) {
    let value = match ev {
        AgentEvent::TurnStarted(id) => serde_json::json!({"type":"turn_started","id":id}),
        AgentEvent::TurnEnded(id) => serde_json::json!({"type":"turn_ended","id":id}),
        AgentEvent::TextDelta(t) => serde_json::json!({"type":"text_delta","text":t}),
        AgentEvent::ReasoningDelta(t) => serde_json::json!({"type":"reasoning_delta","text":t}),
        AgentEvent::ThoughtTick { t, note } => {
            serde_json::json!({"type":"thought_tick","t":t,"note":note})
        }
        AgentEvent::ToolStarted { name, call_id } => {
            serde_json::json!({"type":"tool_started","name":name,"call_id":call_id})
        }
        AgentEvent::ToolFinished {
            name,
            call_id,
            ok,
            preview,
        } => serde_json::json!({
            "type":"tool_finished",
            "name":name,
            "call_id":call_id,
            "ok":ok,
            "preview":preview
        }),
        AgentEvent::Error(e) => serde_json::json!({"type":"error","message":e}),
        AgentEvent::ApprovalNeeded {
            call_id,
            name,
            summary,
        } => serde_json::json!({
            "type":"approval_needed",
            "call_id":call_id,
            "name":name,
            "summary":summary
        }),
        AgentEvent::Done => serde_json::json!({"type":"done"}),
    };
    println!("{value}");
}
