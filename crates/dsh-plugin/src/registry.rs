use crate::loader::{install_plugin_from_path, load_plugin_dir, plugin_skill_paths, LoadedPlugin};
use crate::meta::auto_tag_plugin;
use crate::runtime::{HostBridge, PluginSandbox};
use async_trait::async_trait;
use dsh_skill::{SkillCatalog, SkillLoadEvent, SkillSource};
use dsh_tools::{ToolContext, ToolDefinition, ToolError, ToolHandler, ToolMetadata, ToolRegistry};
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

/// Completed operations from the actual plugin registry and attached skill catalog.
#[derive(Debug, Clone)]
pub enum PluginLoadEvent {
    Loaded {
        id: String,
        name: String,
        path: PathBuf,
    },
    Error {
        name: String,
        path: PathBuf,
        message: String,
    },
    Skipped {
        name: String,
        path: PathBuf,
        message: String,
    },
    Skill(SkillLoadEvent),
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
        self.discover_and_load_observed(roots, |_| {});
    }

    /// Observe real parse/compile/mount results without starting a watcher or
    /// executing any plugin tool. Earlier roots retain their existing priority.
    pub fn discover_and_load_observed(
        &self,
        roots: &[PathBuf],
        mut observe: impl FnMut(PluginLoadEvent),
    ) {
        *self.watch_roots.write() = roots.to_vec();
        for root in roots {
            let entries = match std::fs::read_dir(root) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    observe(plugin_error(
                        root,
                        format!("read plugin directory {}: {error}", root.display()),
                    ));
                    continue;
                }
            };
            let mut paths = Vec::new();
            for entry in entries {
                match entry {
                    Ok(entry) => paths.push(entry.path()),
                    Err(error) => observe(plugin_error(
                        root,
                        format!("read plugin directory entry: {error}"),
                    )),
                }
            }
            paths.sort();
            for path in paths {
                if path.is_dir() {
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or_default();
                    if name.starts_with('.') {
                        observe(PluginLoadEvent::Skipped {
                            name: name.into(),
                            path: path.clone(),
                            message: "hidden plugin directory ignored".into(),
                        });
                        continue;
                    }
                    // Higher-priority roots are scanned first; skip duplicate ids.
                    let loaded = match load_plugin_dir(&path) {
                        Ok(plugin) => plugin,
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "skip plugin mount"
                            );
                            observe(plugin_error(&path, format!("{e:#}")));
                            continue;
                        }
                    };
                    if self.plugins.read().contains_key(&loaded.manifest.id) {
                        observe(PluginLoadEvent::Skipped {
                            name: loaded.manifest.name.clone(),
                            path: path.clone(),
                            message: format!(
                                "duplicate plugin id {}; retained earlier mount",
                                loaded.manifest.id
                            ),
                        });
                        continue;
                    }
                    match self.mount_loaded_observed(loaded, &mut observe) {
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
        self.mount_dir_observed(path, |_| {})
    }

    pub fn mount_dir_observed(
        &self,
        path: &Path,
        mut observe: impl FnMut(PluginLoadEvent),
    ) -> anyhow::Result<String> {
        let loaded = load_plugin_dir(path).map_err(|error| {
            observe(plugin_error(path, format!("{error:#}")));
            error
        })?;
        self.mount_loaded_observed(loaded, &mut observe)
    }

    fn mount_loaded_observed(
        &self,
        loaded: LoadedPlugin,
        observe: &mut dyn FnMut(PluginLoadEvent),
    ) -> anyhow::Result<String> {
        if let Some(script) = &loaded.entry_script {
            self.sandbox.validate_script(script).map_err(|e| {
                let error =
                    anyhow::anyhow!("plugin {} Rhai validate failed: {e}", loaded.manifest.id);
                observe(PluginLoadEvent::Error {
                    name: loaded.manifest.name.clone(),
                    path: loaded.root.clone(),
                    message: error.to_string(),
                });
                error
            })?;
        } else if let Some(entry) = &loaded.manifest.entry {
            observe(PluginLoadEvent::Error {
                name: loaded.manifest.name.clone(),
                path: loaded.root.join(entry),
                message: "declared entry is missing; compatibility mount has no executable script"
                    .into(),
            });
        }
        let id = loaded.manifest.id.clone();
        let name = loaded.manifest.name.clone();
        let path = loaded.root.clone();
        let meta = auto_tag_plugin(&loaded, &self.meta_dir);
        self.tools.unregister_plugin(&id);
        self.register_tools(&loaded).map_err(|error| {
            observe(PluginLoadEvent::Error {
                name: name.clone(),
                path: path.clone(),
                message: format!("{error:#}"),
            });
            error
        })?;
        self.mount_plugin_skills_observed(&loaded, observe);
        self.metas.write().insert(id.clone(), meta);
        self.plugins.write().insert(id.clone(), loaded);
        observe(PluginLoadEvent::Loaded {
            id: id.clone(),
            name,
            path,
        });
        Ok(id)
    }

    fn mount_plugin_skills_observed(
        &self,
        plugin: &LoadedPlugin,
        observe: &mut dyn FnMut(PluginLoadEvent),
    ) {
        let Some(catalog) = self.skills.read().clone() else {
            return;
        };
        // The resolver keeps its tolerant mounting semantics. Report declared
        // paths it cannot resolve instead of silently hiding these local issues.
        for declared in &plugin.manifest.skills {
            let path = plugin.root.join(declared);
            let resolved = if path.is_dir() {
                path.join("SKILL.md")
            } else {
                path
            };
            if !resolved.is_file() {
                observe(PluginLoadEvent::Skill(SkillLoadEvent {
                    name: declared.clone(),
                    path: resolved,
                    source: SkillSource::Plugin,
                    status: dsh_skill::SkillLoadStatus::Error,
                    message: Some(format!(
                        "plugin {} declared skill path could not be resolved",
                        plugin.manifest.id
                    )),
                }));
            }
        }
        let skills_root = plugin.root.join("skills");
        if skills_root.is_dir() {
            for error in walkdir::WalkDir::new(&skills_root)
                .max_depth(3)
                .into_iter()
                .filter_map(Result::err)
            {
                observe(PluginLoadEvent::Skill(SkillLoadEvent {
                    name: plugin.manifest.id.clone(),
                    path: error.path().unwrap_or(&skills_root).to_path_buf(),
                    source: SkillSource::Plugin,
                    status: dsh_skill::SkillLoadStatus::Error,
                    message: Some(format!("plugin skill discovery failed: {error}")),
                }));
            }
        }
        for path in plugin_skill_paths(plugin) {
            match catalog.mount_path_observed(&path, SkillSource::Plugin, |event| {
                observe(PluginLoadEvent::Skill(event))
            }) {
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
                                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
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
                    metadata: ToolMetadata::plugin_default(),
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

fn plugin_error(path: &Path, message: String) -> PluginLoadEvent {
    PluginLoadEvent::Error {
        name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        path: path.to_path_buf(),
        message,
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

#[cfg(test)]
mod observed_tests {
    use super::*;
    use dsh_skill::SkillLoadStatus;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let name = format!(
                "dsh-plugin-observed-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let path = std::env::temp_dir().join(name);
            std::fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }

        fn write(&self, relative: &str, body: &str) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, body).unwrap();
            path
        }

        fn plugin(&self, relative: &str, id: &str, script: &str) -> PathBuf {
            self.write(
                &format!("{relative}/plugin.json"),
                &json!({
                    "id": id, "name": relative, "version": "1.0.0", "entry": "main.rhai",
                    "tools": [{"name":"echo", "description":"fixture echo"}]
                })
                .to_string(),
            );
            self.write(&format!("{relative}/main.rhai"), script);
            self.0.join(relative)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let temp = std::env::temp_dir().canonicalize().unwrap();
            assert_eq!(self.0.parent(), Some(temp.as_path()));
            assert!(self
                .0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("dsh-plugin-observed-"));
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_and_empty_roots_do_not_seed_plugins_or_start_watching() {
        let fixture = Fixture::new();
        let empty = fixture.0.join("empty");
        std::fs::create_dir(&empty).unwrap();
        let registry = PluginRegistry::new(Arc::new(ToolRegistry::new()), fixture.0.join("meta"));
        let mut events = Vec::new();
        registry.discover_and_load_observed(&[empty, fixture.0.join("missing")], |event| {
            events.push(event)
        });
        assert!(events.is_empty());
        assert!(registry.ids().is_empty());
        assert!(registry.tools.definitions().is_empty());
        assert!(registry.watcher.inner.read().is_none());
        assert!(!fixture.0.join("missing").exists());
    }

    #[test]
    fn only_compiles_scripts_and_keeps_first_duplicate_mount() {
        let fixture = Fixture::new();
        let first = fixture.plugin(
            "first/a",
            "shared",
            "throw \"must not execute\"; fn echo(args) { args }",
        );
        let duplicate = fixture.plugin("later/b", "shared", "fn echo(args) { args }");
        let invalid = fixture.plugin("first/invalid", "invalid", "fn echo( {");
        fixture.write("first/malformed/plugin.json", "{");
        let registry = PluginRegistry::new(Arc::new(ToolRegistry::new()), fixture.0.join("meta"));
        let mut events = Vec::new();
        registry.discover_and_load_observed(
            &[fixture.0.join("first"), fixture.0.join("later")],
            |event| {
                if let PluginLoadEvent::Loaded { id, .. } = &event {
                    assert!(registry.ids().contains(id));
                    assert!(registry
                        .tools
                        .definitions()
                        .iter()
                        .any(|tool| tool.plugin_id.as_ref() == Some(id)));
                }
                events.push(event);
            },
        );
        assert_eq!(registry.ids(), ["shared"]);
        assert_eq!(registry.plugins.read()["shared"].root, first);
        assert!(events.iter().any(
            |event| matches!(event, PluginLoadEvent::Skipped {path,..} if *path == duplicate)
        ));
        assert!(events
            .iter()
            .any(|event| matches!(event, PluginLoadEvent::Error {path,..} if *path == invalid)));
        assert!(events.iter().any(
            |event| matches!(event, PluginLoadEvent::Error {path,..} if path.ends_with("malformed"))
        ));
        assert_eq!(registry.tools.definitions().len(), 1);
        assert!(registry.watcher.inner.read().is_none());
    }

    #[test]
    fn attached_skill_mounts_and_failures_are_observed() {
        let fixture = Fixture::new();
        let previous = fixture.write("initial.md", "---\nname: shared\n---\ninitial");
        fixture.write(
            "plugins/pack/plugin.json",
            &json!({
                "id":"pack", "name":"Pack", "version":"1.0.0", "entry":"absent.rhai",
                "skills":["skills/shared", "missing.md"]
            })
            .to_string(),
        );
        let mounted = fixture.write(
            "plugins/pack/skills/shared/SKILL.md",
            "---\nname: shared\n---\nmounted",
        );
        let invalid = fixture.write(
            "plugins/pack/skills/invalid/SKILL.md",
            "---\nname: INVALID!\n---\nbody",
        );
        let skills = Arc::new(SkillCatalog::new(fixture.0.join("meta")));
        skills
            .mount_path(&previous, SkillSource::ProjectDsh)
            .unwrap();
        let registry = PluginRegistry::new(Arc::new(ToolRegistry::new()), fixture.0.join("meta"));
        registry.attach_skills(skills.clone());
        let mut events = Vec::new();
        registry
            .discover_and_load_observed(&[fixture.0.join("plugins")], |event| events.push(event));
        assert_eq!(registry.ids(), ["pack"]);
        assert_eq!(skills.get("shared").unwrap().summary.path, mounted);
        assert!(events.iter().any(|event| matches!(event, PluginLoadEvent::Skill(e) if e.path == previous && e.status == SkillLoadStatus::Skipped)));
        for path in [invalid, fixture.0.join("plugins/pack/missing.md")] {
            assert!(events.iter().any(|event| matches!(event, PluginLoadEvent::Skill(e) if e.path == path && e.status == SkillLoadStatus::Error)));
        }
        assert!(events.iter().any(|event| matches!(event, PluginLoadEvent::Error {path,..} if path.ends_with("absent.rhai"))));
        assert!(matches!(events.last(), Some(PluginLoadEvent::Loaded {id,..}) if id == "pack"));
    }
}
