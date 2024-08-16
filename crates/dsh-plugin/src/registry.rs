use crate::loader::{
    install_plugin_from_path, load_plugin_dir, plugin_skill_paths, LoadedPlugin,
};
use crate::meta::auto_tag_plugin;
use crate::runtime::{HostBridge, PluginSandbox};
use async_trait::async_trait;
use dsh_skill::{SkillCatalog, SkillSource};
use dsh_tools::{ToolContext, ToolDefinition, ToolError, ToolHandler, ToolRegistry};
use parking_lot::RwLock;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct RoutingSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tools: Vec<String>,
    pub tags: Vec<String>,
}

pub struct PluginRegistry {
    plugins: RwLock<HashMap<String, LoadedPlugin>>,
    metas: RwLock<HashMap<String, crate::meta::PluginMeta>>,
    tools: Arc<ToolRegistry>,
    meta_dir: PathBuf,
    sandbox: Arc<PluginSandbox>,
    skills: RwLock<Option<Arc<SkillCatalog>>>,
    watch_roots: RwLock<Vec<PathBuf>>,
    watcher: MutexWatcher,
}

/// Thin wrap so we can optionally hold a notify watcher without exposing the type widely.
struct MutexWatcher {
    inner: RwLock<Option<notify::RecommendedWatcher>>,
}

impl PluginRegistry {
    pub fn new(tools: Arc<ToolRegistry>, meta_dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&meta_dir);
        Self {
            plugins: RwLock::new(HashMap::new()),
            metas: RwLock::new(HashMap::new()),
            tools,
            meta_dir,
            sandbox: Arc::new(PluginSandbox::new()),
            skills: RwLock::new(None),
            watch_roots: RwLock::new(Vec::new()),
            watcher: MutexWatcher {
                inner: RwLock::new(None),
            },
        }
    }

    pub fn attach_skills(&self, catalog: Arc<SkillCatalog>) {
        *self.skills.write() = Some(catalog);
    }

    pub fn discover_and_load(&self, roots: &[PathBuf]) {
        *self.watch_roots.write() = roots.to_vec();
        for root in roots {
            if !root.exists() {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or_default();
                    if name.starts_with('.') {
                        continue;
                    }
                    // Higher-priority roots are scanned first; skip duplicate ids.
                    let provisional = match load_plugin_dir(&path) {
                        Ok(p) => p.manifest.id,
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "skip plugin mount"
                            );
                            continue;
                        }
                    };
                    if self.plugins.read().contains_key(&provisional) {
                        continue;
                    }
                    match self.mount_dir(&path) {
                        Ok(id) => tracing::info!(plugin = %id, "mounted plugin"),
                        Err(e) => tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "skip plugin mount"
                        ),
                    }
                }
            }
        }
    }

    pub fn mount_dir(&self, path: &Path) -> anyhow::Result<String> {
        let loaded = load_plugin_dir(path)?;
        if let Some(script) = &loaded.entry_script {
            self.sandbox
                .validate_script(script)
                .map_err(|e| anyhow::anyhow!("plugin {} Rhai validate failed: {e}", loaded.manifest.id))?;
        }
        let id = loaded.manifest.id.clone();
        let meta = auto_tag_plugin(&loaded, &self.meta_dir);
        self.tools.unregister_plugin(&id);
        self.register_tools(&loaded)?;
        self.mount_plugin_skills(&loaded);
        self.metas.write().insert(id.clone(), meta);
        self.plugins.write().insert(id.clone(), loaded);
        Ok(id)
    }

    fn mount_plugin_skills(&self, plugin: &LoadedPlugin) {
        let Some(catalog) = self.skills.read().clone() else {
            return;
        };
        for path in plugin_skill_paths(plugin) {
            match catalog.mount_path(&path, SkillSource::Plugin) {
                Ok(name) => tracing::info!(
                    plugin = %plugin.manifest.id,
                    skill = %name,
                    "mounted plugin skill"
                ),
                Err(e) => tracing::warn!(
                    plugin = %plugin.manifest.id,
                    path = %path.display(),
                    error = %e,
                    "plugin skill mount failed"
                ),
            }
        }
    }

    pub fn install_from_path(&self, src: &Path, dest_root: &Path) -> anyhow::Result<String> {
        let dest = install_plugin_from_path(src, dest_root)?;
        self.mount_dir(&dest)
    }

    pub fn unload(&self, id: &str) -> bool {
        self.tools.unregister_plugin(id);
        self.metas.write().remove(id);
        self.plugins.write().remove(id).is_some()
    }

    pub fn reload_all(&self, roots: &[PathBuf]) {
        self.sandbox.invalidate_cache();
        let ids: Vec<String> = self.plugins.read().keys().cloned().collect();
        for id in ids {
            self.unload(&id);
        }
        self.discover_and_load(roots);
    }

    /// Start a background hot-reload watcher on plugin roots (debounced).
    pub fn start_hot_reload(self: &Arc<Self>) {
        use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
        use std::sync::mpsc;

        let roots = self.watch_roots.read().clone();
        if roots.is_empty() {
            return;
        }

        let (tx, rx) = mpsc::channel();
        let mut watcher = match RecommendedWatcher::new(
            move |res| {
                let _ = tx.send(res);
            },
            notify::Config::default().with_poll_interval(Duration::from_secs(2)),
        ) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(error = %e, "plugin hot-reload watcher unavailable");
                return;
            }
        };

        for root in &roots {
            if root.exists() {
                if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
                    tracing::warn!(path = %root.display(), error = %e, "watch failed");
                }
            }
        }
        *self.watcher.inner.write() = Some(watcher);

        let registry = Arc::clone(self);
        std::thread::Builder::new()
            .name("dsh-plugin-watch".into())
            .spawn(move || {
                let mut dirty = false;
                loop {
                    match rx.recv_timeout(Duration::from_millis(800)) {
                        Ok(Ok(event)) => {
                            let meaningful = matches!(
                                event.kind,
                                EventKind::Create(_)
                                    | EventKind::Modify(_)
                                    | EventKind::Remove(_)
                            );
                            if meaningful {
                                // Ignore staging/backup dirs
                                let skip = event.paths.iter().any(|p| {
                                    p.components().any(|c| {
                                        let s = c.as_os_str().to_string_lossy();
                                        s.starts_with(".staging-") || s.starts_with(".backup-")
                                    })
                                });
                                if !skip {
                                    dirty = true;
                                }
                            }
                        }
                        Ok(Err(e)) => tracing::warn!(error = %e, "notify error"),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            if dirty {
                                dirty = false;
                                let roots = registry.watch_roots.read().clone();
                                tracing::info!("hot-reloading plugins after filesystem change");
                                registry.reload_all(&roots);
                            }
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .ok();
    }

    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<_> = self.plugins.read().keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Lightweight routing view — no JSON rebuild, no skills WalkDir.
    pub fn routing_summaries(&self) -> Vec<RoutingSummary> {
        let plugins = self.plugins.read();
        let metas = self.metas.read();
        let mut out = Vec::with_capacity(plugins.len());
        for p in plugins.values() {
            let meta = metas.get(&p.manifest.id);
            out.push(RoutingSummary {
                id: p.manifest.id.clone(),
                name: p.manifest.name.clone(),
                description: p.manifest.description.clone(),
                tools: p.manifest.tools.iter().map(|t| t.name.clone()).collect(),
                tags: meta
                    .map(|m| m.tags.clone())
                    .unwrap_or_else(|| p.manifest.tags.clone()),
            });
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub fn list(&self) -> Vec<serde_json::Value> {
        let plugins = self.plugins.read();
        let metas = self.metas.read();
        plugins
            .values()
            .map(|p| {
                let meta = metas.get(&p.manifest.id);
                let skill_paths: Vec<String> = plugin_skill_paths(p)
                    .into_iter()
                    .map(|x| x.to_string_lossy().replace('\\', "/"))
                    .collect();
                json!({
                    "id": p.manifest.id,
                    "name": p.manifest.name,
                    "version": p.manifest.version,
                    "description": p.manifest.description,
                    "tools": p.manifest.tools.iter().map(|t| &t.name).collect::<Vec<_>>(),
                    "skills": p.manifest.skills,
                    "skill_paths": skill_paths,
                    "tags": meta.map(|m| m.tags.clone()).unwrap_or_default(),
                    "examples": meta.map(|m| m.examples.clone()).unwrap_or_default(),
                    "root": p.root,
                })
            })
            .collect()
    }

    pub fn catalog_prompt_section(&self) -> String {
        let list = self.list();
        if list.is_empty() {
            return "No outer plugins mounted.".into();
        }
        let mut out = String::from("Mounted outer plugins:\n");
        for item in list {
            out.push_str(&format!(
                "- {} ({}) — {} tags={:?}\n",
                item["id"], item["version"], item["description"], item["tags"]
            ));
            if let Some(examples) = item["examples"].as_array() {
                for ex in examples.iter().take(2) {
                    out.push_str(&format!("  example: {}\n", ex));
                }
            }
        }
        out.push_str("Invoke tools as plugin.<id>.<tool>.");
        out
    }

    fn register_tools(&self, plugin: &LoadedPlugin) -> anyhow::Result<()> {
        let script = plugin.entry_script.clone().unwrap_or_default();
        for tool in &plugin.manifest.tools {
            let full_name = format!("plugin.{}.{}", plugin.manifest.id, tool.name);
            let rhai_fn = tool.rhai_fn.clone().unwrap_or_else(|| tool.name.clone());
            let handler = Arc::new(PluginToolHandler {
                def: ToolDefinition {
                    name: full_name,
                    description: format!("[plugin:{}] {}", plugin.manifest.id, tool.description),
                    parameters: tool.parameters.clone(),
                    plugin_id: Some(plugin.manifest.id.clone()),
                    tags: {
                        let mut t = plugin.manifest.tags.clone();
                        t.push("plugin".into());
                        t
                    },
                },
                script: script.clone(),
                rhai_fn,
                sandbox: self.sandbox.clone(),
                plugin_root: plugin.root.clone(),
            });
            self.tools.register(handler);
        }
        Ok(())
    }
}

struct PluginToolHandler {
    def: ToolDefinition,
    script: String,
    rhai_fn: String,
    sandbox: Arc<PluginSandbox>,
    plugin_root: PathBuf,
}

#[async_trait]
impl ToolHandler for PluginToolHandler {
    fn definition(&self) -> ToolDefinition {
        self.def.clone()
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<String, ToolError> {
        if self.script.trim().is_empty() {
            return Ok(format!(
                "plugin tool {} has no Rhai entry; args={}",
                self.def.name, args
            ));
        }
        let script = self.script.clone();
        let fn_name = self.rhai_fn.clone();
        let args = args.clone();
        let sandbox = self.sandbox.clone();
        let host = HostBridge {
            cwd: ctx.cwd.clone(),
            outer_home: ctx.outer_home.clone(),
            plugin_root: self.plugin_root.clone(),
        };
        tokio::task::spawn_blocking(move || {
            sandbox.call_fn_with_host(&script, &fn_name, &args, &host)
        })
        .await
        .map_err(|e| ToolError::Message(e.to_string()))?
        .map_err(|e| ToolError::Message(e.to_string()))
    }
}
