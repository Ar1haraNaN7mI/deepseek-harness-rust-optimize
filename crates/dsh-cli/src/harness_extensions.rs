//! Real local extension management, shared by authenticated web and stdio RPC.
use crate::app_server::RpcFailure;
use dsh_core::Runtime;
use dsh_plugin::PluginRegistry;
use dsh_skill::SkillCatalog;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

type RpcResult = Result<Value, RpcFailure>;
static MUTATIONS: OnceLock<parking_lot::Mutex<()>> = OnceLock::new();

fn internal(error: impl std::fmt::Display) -> RpcFailure {
    RpcFailure::internal(error.to_string())
}
fn required<'a>(params: &'a Value, field: &str) -> Result<&'a str, RpcFailure> {
    params
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| RpcFailure::invalid_params(format!("{field} must be a nonempty string")))
}
fn catalogs(runtime: &Runtime) -> Result<(Arc<PluginRegistry>, Arc<SkillCatalog>), RpcFailure> {
    let plugins = runtime
        .plugins
        .read()
        .clone()
        .ok_or_else(|| internal("DSH plugin registry is unavailable"))?;
    let skills = runtime
        .skills
        .read()
        .clone()
        .ok_or_else(|| internal("DSH skill catalog is unavailable"))?;
    Ok((plugins, skills))
}

pub(crate) fn snapshot(runtime: &Runtime) -> RpcResult {
    let (plugins, skills) = catalogs(runtime)?;
    let skills = skills
        .management_list()
        .map_err(internal)?
        .into_iter()
        .map(|(summary, enabled)| {
            let mut item = serde_json::to_value(summary).expect("skill summary serializes");
            item["enabled"] = json!(enabled);
            item
        })
        .collect::<Vec<_>>();
    let plugins = plugins
        .management_list(&runtime.outer_home.join("plugins"))
        .map_err(internal)?;
    Ok(json!({"plugins":plugins,"skills":skills,"install_root":runtime.outer_home.join("plugins")}))
}

pub(crate) fn dispatch(runtime: &Arc<Runtime>, method: &str, params: &Value) -> Option<RpcResult> {
    if method == "extensions/list" {
        return Some(snapshot(runtime));
    }
    if method == "skills/read" {
        return Some((|| {
            let (_, skills) = catalogs(runtime)?;
            let name = required(params, "name")?;
            let record = skills
                .inspect(name)
                .ok_or_else(|| RpcFailure::invalid_params(format!("unknown skill: {name}")))?;
            // Read disk so a deleted/unreadable skill never masquerades as a successful stale read.
            let content = std::fs::read_to_string(&record.summary.path).map_err(internal)?;
            Ok(json!({"name":name,"path":record.summary.path,"content":content}))
        })());
    }
    if !matches!(
        method,
        "plugins/install"
            | "plugins/enable"
            | "plugins/disable"
            | "plugins/uninstall"
            | "skills/enable"
            | "skills/disable"
            | "extensions/reload"
    ) {
        return None;
    }
    Some((|| {
        let _guard = MUTATIONS.get_or_init(Default::default).lock();
        let (plugins, skills) = catalogs(runtime)?;
        let mut warnings = Vec::new();
        let operation = (|| -> Result<(), RpcFailure> {
            match method {
                "plugins/install" => {
                    let path = PathBuf::from(required(params, "path")?);
                    if !path.is_absolute() {
                        return Err(RpcFailure::invalid_params(
                            "use an absolute local plugin directory path",
                        ));
                    }
                    plugins
                        .install_from_path(&path, &runtime.outer_home.join("plugins"))
                        .map_err(internal)?;
                }
                "plugins/enable" | "plugins/disable" => plugins
                    .set_enabled(required(params, "id")?, method == "plugins/enable")
                    .map_err(internal)?,
                "plugins/uninstall" => plugins
                    .uninstall(required(params, "id")?, &runtime.outer_home.join("plugins"))
                    .map_err(internal)?,
                "skills/enable" | "skills/disable" => skills
                    .set_enabled(required(params, "name")?, method == "skills/enable")
                    .map_err(internal)?,
                "extensions/reload" => {
                    skills.discover_observed(
                        &runtime.workspace_root,
                        &runtime.outer_home,
                        Some(&runtime.workspace_root.join("outer/bundled-skills")),
                        |event| {
                            if event.status == dsh_skill::SkillLoadStatus::Error {
                                warnings.push(format!(
                                    "{}: {}",
                                    event.path.display(),
                                    event.message.unwrap_or_default()
                                ));
                            }
                        },
                    );
                    plugins.reload_all(&plugins.roots());
                }
                _ => unreachable!(),
            }
            Ok(())
        })();
        // Replace any bootstrap prompt cache immediately; per-turn routing also
        // consults the same live registries and their generation counters.
        let mut prompt = runtime.prompt.write();
        prompt.set_section("skills", skills.catalog_prompt_section());
        prompt.set_section("plugins", plugins.catalog_prompt_section());
        drop(prompt);
        operation?;
        let mut value = snapshot(runtime)?;
        value["warnings"] = json!(warnings);
        Ok(value)
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_core::AppConfig;
    use dsh_llm::DeepSeekClient;
    use dsh_tools::ToolRegistry;

    struct Fixture {
        root: PathBuf,
        runtime: Option<Arc<Runtime>>,
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("dsh-extensions-test-{}", uuid::Uuid::new_v4()));
            let mut config = AppConfig::builtin_default();
            config.paths.outer_home = root.join("outer").display().to_string();
            config.agent.scheduler_enabled = false;
            config.ctm.enabled = false;
            let llm = DeepSeekClient::new(config.to_llm_config("fixture-key".into())).unwrap();
            let tools = Arc::new(ToolRegistry::new());
            let runtime = Runtime::bootstrap(config, root.clone(), llm, tools.clone()).unwrap();
            let skills = Arc::new(SkillCatalog::new(runtime.outer_home.join("meta")));
            let plugins = Arc::new(PluginRegistry::new(tools, runtime.outer_home.join("meta")));
            plugins.attach_skills(skills.clone());
            plugins.discover_and_load(&[runtime.outer_home.join("plugins")]);
            runtime.attach_skills(skills);
            runtime.attach_plugins(plugins);
            let source = root.join("package");
            std::fs::create_dir_all(source.join("skills/helper")).unwrap();
            std::fs::write(source.join("plugin.json"), json!({"id":"fixture","name":"Fixture","version":"1.0","entry":"main.rhai","tools":[{"name":"echo","description":"fixture"}]}).to_string()).unwrap();
            std::fs::write(
                source.join("main.rhai"),
                "fn echo(args) { \"fixture tool\" }",
            )
            .unwrap();
            std::fs::write(source.join("skills/helper/SKILL.md"), "---\nname: fixture-helper\ndescription: Use fixture tools.\n---\nReal instructions.").unwrap();
            Self {
                root,
                runtime: Some(runtime),
            }
        }
        fn runtime(&self) -> &Arc<Runtime> {
            self.runtime.as_ref().unwrap()
        }
        fn call(&self, method: &str, params: Value) -> RpcResult {
            dispatch(self.runtime(), method, &params).unwrap()
        }
        fn install(&self) -> Value {
            self.call("plugins/install", json!({"path":self.root.join("package")}))
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.runtime.take();
            let root = self.root.canonicalize().unwrap();
            let temp = std::env::temp_dir().canonicalize().unwrap();
            assert_eq!(root.parent(), Some(temp.as_path()));
            assert!(root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("dsh-extensions-test-"));
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[tokio::test]
    async fn rpc_changes_real_tools_skills_and_persistent_activation() {
        let fixture = Fixture::new();
        let installed = fixture.install();
        assert_eq!(installed["plugins"][0]["mounted"], true);
        assert!(fixture.runtime().tools.get("plugin.fixture.echo").is_some());
        assert_eq!(installed["skills"][0]["name"], "fixture-helper");
        let disabled = fixture
            .call("plugins/disable", json!({"id":"fixture"}))
            .unwrap();
        assert_eq!(disabled["plugins"][0]["enabled"], false);
        assert_eq!(disabled["skills"].as_array().unwrap().len(), 0);
        assert!(fixture.runtime().tools.get("plugin.fixture.echo").is_none());
        let persisted = std::fs::read_to_string(
            fixture
                .runtime()
                .outer_home
                .join("meta/plugins-disabled.json"),
        )
        .unwrap();
        assert!(persisted.contains("fixture"));
        fixture
            .call("plugins/enable", json!({"id":"fixture"}))
            .unwrap();
        fixture
            .call("skills/disable", json!({"name":"fixture-helper"}))
            .unwrap();
        assert!(fixture
            .runtime()
            .skills
            .read()
            .as_ref()
            .unwrap()
            .get("fixture-helper")
            .is_none());
        assert!(fixture
            .call("skills/read", json!({"name":"fixture-helper"}))
            .unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("Real instructions."));
        fixture
            .call("plugins/uninstall", json!({"id":"fixture"}))
            .unwrap();
        assert!(!fixture
            .runtime()
            .outer_home
            .join("plugins/fixture")
            .exists());
        assert!(fixture.root.join("package/plugin.json").exists());
    }

    #[tokio::test]
    async fn invalid_paths_and_unknown_extensions_return_errors() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture
                .call("plugins/install", json!({"path":"relative/path"}))
                .unwrap_err()
                .code,
            -32602
        );
        assert!(fixture
            .call(
                "plugins/install",
                json!({"path":fixture.root.join("missing")})
            )
            .is_err());
        assert!(fixture
            .call("plugins/enable", json!({"id":"unknown"}))
            .is_err());
        assert!(fixture
            .call("skills/disable", json!({"name":"unknown"}))
            .is_err());
        assert!(fixture
            .call("skills/read", json!({"name":"unknown"}))
            .is_err());
        assert!(fixture.runtime().tools.names().is_empty());
    }
}
