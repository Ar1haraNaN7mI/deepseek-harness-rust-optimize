//! Core kernel: session log, agent loop, system prompt, CTM thought, self-learning.
//! This layer is immutable at runtime — agents may only mutate the outer layer.

mod agent_loop;
pub mod approvals;
pub mod bg;
pub mod builtin_tools;
mod config;
pub mod credentials;
pub mod ctm;
pub mod execpolicy;
pub mod features;
pub mod hooks;
pub mod learn;
pub mod learn_tools;
pub mod mcp;
pub mod permissions;
pub mod policy;
mod session;
pub mod settings;
mod system_prompt;

pub use agent_loop::{AgentEvent, AgentHandle, AgentLoop};
pub use approvals::{ApprovalQueue, DeniedAction};
pub use bg::{BgJobInfo, BgTerminals};
pub use builtin_tools::register_builtin_tools;
pub use config::AppConfig;
pub use credentials::{
    api_key_status, clear_api_key, load_api_key, resolve_api_key, save_api_key,
};
pub use ctm::{ContinuousThought, CtmConfig, ThoughtSnapshot};
pub use execpolicy::{
    check_command, load_policy_file, merge_policies, strictest, ExecCheckResult, ExecDecision,
    ExecPolicyFile, ExecRule,
};
pub use features::{load_features, save_features, FeatureFlags, DEFAULT_FLAGS};
pub use hooks::{hooks_path, load_hooks, save_hooks, HookEntry, HooksConfig};
pub use learn::LearnStore;
pub use learn_tools::register_learn_tools;
pub use mcp::{load_mcp, save_mcp, McpConfig, McpServer, McpTransport};
pub use permissions::{PermissionMode, PERMISSION_HELP};
pub use policy::{
    ApprovalPolicy, SandboxMode, APPROVAL_HELP, SANDBOX_HELP,
};
pub use session::{Session, SessionEvent, SessionStore};
pub use settings::{
    load_settings, personality_prompt, save_settings, SessionSettings, PERSONALITIES, PETS,
    STATUSLINE_FIELDS, THEMES, TITLE_FIELDS,
};
pub use system_prompt::SystemPromptBuilder;

use anyhow::Result;
use dsh_llm::DeepSeekClient;
use dsh_plugin::PluginRegistry;
use dsh_skill::SkillCatalog;
use dsh_tools::{DefaultPipeline, ToolRegistry};
use parking_lot::Mutex;
use parking_lot::RwLock;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, watch};

/// Shared runtime state owned by the core kernel.
pub struct Runtime {
    pub config: AppConfig,
    pub workspace_root: PathBuf,
    pub outer_home: PathBuf,
    pub workspace_outer: PathBuf,
    pub llm: Arc<DeepSeekClient>,
    pub tools: Arc<ToolRegistry>,
    pub pipeline: Arc<DefaultPipeline>,
    pub sessions: Arc<SessionStore>,
    pub prompt: Arc<RwLock<SystemPromptBuilder>>,
    pub learn: Arc<LearnStore>,
    pub ctm: Arc<ContinuousThought>,
    pub skills: RwLock<Option<Arc<SkillCatalog>>>,
    pub plugins: RwLock<Option<Arc<PluginRegistry>>>,
    pub permissions: RwLock<PermissionMode>,
    pub settings: RwLock<SessionSettings>,
    pub features: RwLock<FeatureFlags>,
    pub mcp: RwLock<McpConfig>,
    pub hooks: RwLock<HooksConfig>,
    pub bg: Arc<BgTerminals>,
    pub approvals: Arc<ApprovalQueue>,
    /// Pending human approval reply (Codex on-request).
    approval_waiter: Mutex<Option<oneshot::Sender<bool>>>,
}

impl Runtime {
    pub fn bootstrap(
        config: AppConfig,
        workspace_root: PathBuf,
        llm: DeepSeekClient,
        tools: Arc<ToolRegistry>,
    ) -> Result<Arc<Self>> {
        let outer_home = config.resolve_outer_home()?;
        let workspace_outer = workspace_root.join(&config.paths.workspace_outer);
        std::fs::create_dir_all(&outer_home)?;
        std::fs::create_dir_all(outer_home.join("skills"))?;
        std::fs::create_dir_all(outer_home.join("plugins"))?;
        std::fs::create_dir_all(outer_home.join("meta"))?;
        std::fs::create_dir_all(outer_home.join("learn"))?;
        std::fs::create_dir_all(outer_home.join("sessions"))?;
        std::fs::create_dir_all(outer_home.join("rules"))?;
        std::fs::create_dir_all(&workspace_outer)?;
        std::fs::create_dir_all(workspace_outer.join("skills"))?;
        std::fs::create_dir_all(workspace_outer.join("plugins"))?;

        let pipeline = Arc::new(DefaultPipeline::new(tools.clone()));
        let sessions = Arc::new(SessionStore::with_dir(outer_home.join("sessions")));
        let prompt = Arc::new(RwLock::new(SystemPromptBuilder::default_sections()));
        let learn = Arc::new(LearnStore::open(&outer_home));
        let ctm = Arc::new(ContinuousThought::new(config.ctm.clone(), learn.clone()));
        let settings = load_settings(&outer_home);
        let permissions = settings.permissions;
        let features = load_features(&outer_home);
        let mcp = load_mcp(&outer_home);
        let hooks = load_hooks(&outer_home);

        let runtime = Arc::new(Self {
            config,
            workspace_root,
            outer_home,
            workspace_outer,
            llm: Arc::new(llm),
            tools,
            pipeline,
            sessions,
            prompt,
            learn,
            ctm,
            skills: RwLock::new(None),
            plugins: RwLock::new(None),
            permissions: RwLock::new(permissions),
            settings: RwLock::new(settings.clone()),
            features: RwLock::new(features),
            mcp: RwLock::new(mcp),
            hooks: RwLock::new(hooks),
            bg: Arc::new(BgTerminals::new()),
            approvals: Arc::new(ApprovalQueue::new()),
            approval_waiter: Mutex::new(None),
        });

        if let Some(model) = settings.model {
            runtime.llm.set_model(model);
        }
        if let Some(thinking) = settings.thinking {
            runtime.llm.set_thinking(thinking);
        }
        if let Some(p) = &settings.personality {
            runtime
                .prompt
                .write()
                .set_section("personality", personality_prompt(p).to_string());
        }

        // Align permission mode with sandbox setting when sandbox was persisted.
        let _ = settings.sandbox;

        Ok(runtime)
    }

    pub fn attach_skills(&self, skills: Arc<SkillCatalog>) {
        *self.skills.write() = Some(skills);
    }

    pub fn attach_plugins(&self, plugins: Arc<PluginRegistry>) {
        *self.plugins.write() = Some(plugins);
    }

    pub fn set_permissions(&self, mode: PermissionMode) -> anyhow::Result<()> {
        *self.permissions.write() = mode;
        let mut s = self.settings.write();
        s.permissions = mode;
        save_settings(&self.outer_home, &s)?;
        Ok(())
    }

    pub fn set_approval(&self, policy: ApprovalPolicy) -> anyhow::Result<()> {
        let mut s = self.settings.write();
        s.approval = policy;
        save_settings(&self.outer_home, &s)?;
        Ok(())
    }

    pub fn set_sandbox(&self, mode: SandboxMode) -> anyhow::Result<()> {
        *self.permissions.write() = mode.to_permission();
        let mut s = self.settings.write();
        s.sandbox = mode;
        s.permissions = mode.to_permission();
        save_settings(&self.outer_home, &s)?;
        Ok(())
    }

    pub fn persist_settings(&self) -> anyhow::Result<()> {
        let s = self.settings.read().clone();
        save_settings(&self.outer_home, &s)?;
        Ok(())
    }

    pub fn persist_features(&self) -> anyhow::Result<()> {
        let f = self.features.read().clone();
        save_features(&self.outer_home, &f)?;
        Ok(())
    }

    pub fn persist_mcp(&self) -> anyhow::Result<()> {
        let m = self.mcp.read().clone();
        save_mcp(&self.outer_home, &m)?;
        Ok(())
    }

    pub fn persist_hooks(&self) -> anyhow::Result<()> {
        let h = self.hooks.read().clone();
        save_hooks(&self.outer_home, &h)?;
        Ok(())
    }

    pub fn set_personality(&self, name: &str) -> anyhow::Result<()> {
        {
            let mut s = self.settings.write();
            s.personality = Some(name.to_string());
        }
        self.prompt
            .write()
            .set_section("personality", personality_prompt(name).to_string());
        self.persist_settings()?;
        Ok(())
    }

    /// Resolve a pending approval from the TUI (y/n).
    pub fn resolve_approval(&self, allow: bool) {
        if let Some(tx) = self.approval_waiter.lock().take() {
            let _ = tx.send(allow);
        }
    }

    /// Ask the UI to approve a tool; returns false if denied / cancelled / timed out.
    pub async fn request_tool_approval(
        &self,
        event_tx: &mpsc::Sender<AgentEvent>,
        call_id: &str,
        name: &str,
        summary: &str,
        cancel_rx: &mut watch::Receiver<bool>,
    ) -> bool {
        let (tx, rx) = oneshot::channel();
        *self.approval_waiter.lock() = Some(tx);
        let _ = event_tx
            .send(AgentEvent::ApprovalNeeded {
                call_id: call_id.to_string(),
                name: name.to_string(),
                summary: summary.to_string(),
            })
            .await;

        tokio::select! {
            reply = rx => reply.unwrap_or(false),
            _ = async {
                loop {
                    if cancel_rx.changed().await.is_err() {
                        return;
                    }
                    if *cancel_rx.borrow() {
                        return;
                    }
                }
            } => {
                let _ = self.approval_waiter.lock().take();
                false
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(300)) => {
                let _ = self.approval_waiter.lock().take();
                false
            }
        }
    }
}
