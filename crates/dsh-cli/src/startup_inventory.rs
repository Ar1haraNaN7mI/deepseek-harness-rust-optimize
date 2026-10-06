//! Real local discovery for startup previews. This creates isolated registries,
//! compiles plugin scripts, and mounts their skill/tool definitions. It never
//! executes a tool, seeds examples, or starts a runtime, scheduler, or watcher.

use dsh_plugin::{PluginLoadEvent, PluginRegistry};
use dsh_skill::{SkillCatalog, SkillLoadEvent, SkillLoadStatus};
use dsh_tools::ToolRegistry;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

pub struct LoadedInventory {
    pub skills: Arc<SkillCatalog>,
    pub plugins: Arc<PluginRegistry>,
}

pub fn load(
    workspace: &Path,
    outer_home: &Path,
    plugin_roots: &[PathBuf],
    mut emit: impl FnMut(Value),
) -> LoadedInventory {
    let started = Instant::now();
    let mut issues = Vec::new();
    emit(json!({ "type": "stage", "stage": "skills", "status": "loading" }));
    let skills = Arc::new(SkillCatalog::new(outer_home.join("meta")));
    let bundled = workspace.join("outer/bundled-skills");
    skills.discover_observed(workspace, outer_home, Some(&bundled), |event| {
        emit_skill(event, &mut issues, &mut emit);
    });
    emit(json!({ "type": "stage", "stage": "skills", "status": "complete" }));

    emit(json!({ "type": "stage", "stage": "plugins", "status": "loading" }));
    let tools = Arc::new(ToolRegistry::new());
    let plugins = Arc::new(PluginRegistry::new(tools.clone(), outer_home.join("meta")));
    plugins.attach_skills(skills.clone());
    plugins.discover_and_load_observed(plugin_roots, |event| match event {
        PluginLoadEvent::Loaded { name, path, .. } => {
            emit_item("plugin", name, path, "loaded", None, &mut issues, &mut emit);
        }
        PluginLoadEvent::Error {
            name,
            path,
            message,
        } => {
            emit_item(
                "plugin",
                name,
                path,
                "error",
                Some(message),
                &mut issues,
                &mut emit,
            );
        }
        PluginLoadEvent::Skipped {
            name,
            path,
            message,
        } => {
            emit_item(
                "plugin",
                name,
                path,
                "skipped",
                Some(message),
                &mut issues,
                &mut emit,
            );
        }
        PluginLoadEvent::Skill(event) => emit_skill(event, &mut issues, &mut emit),
    });
    emit(json!({ "type": "stage", "stage": "plugins", "status": "complete" }));

    // A plugin can replace or add skills. Read the final catalog only after all
    // real plugin mounts, and count registered tools rather than declarations.
    let loaded_skills: Vec<_> = skills
        .list()
        .into_iter()
        .map(|skill| {
            json!({
                "name": skill.name,
                "source": display_path(&skill.path),
                "status": "loaded",
            })
        })
        .collect();
    let definitions = tools.definitions();
    let loaded_plugins: Vec<_> = plugins.routing_summaries().into_iter().map(|plugin| {
        let tool_count = definitions.iter()
            .filter(|tool| tool.plugin_id.as_deref() == Some(plugin.id.as_str())).count();
        json!({ "id": plugin.id, "name": plugin.name, "tool_count": tool_count, "status": "loaded" })
    }).collect();
    issues.sort_by(|a, b| {
        ["kind", "name", "message"]
            .into_iter()
            .map(|key| a[key].as_str().cmp(&b[key].as_str()))
            .find(|order| !order.is_eq())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    issues.dedup();
    emit(json!({
        "type": "complete", "skills": loaded_skills, "plugins": loaded_plugins,
        "issues": issues, "elapsed_ms": started.elapsed().as_millis(),
    }));
    LoadedInventory { skills, plugins }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn emit_skill(event: SkillLoadEvent, issues: &mut Vec<Value>, emit: &mut dyn FnMut(Value)) {
    let status = match event.status {
        SkillLoadStatus::Loaded => "loaded",
        SkillLoadStatus::Error => "error",
        SkillLoadStatus::Skipped => "skipped",
    };
    emit_item(
        "skill",
        event.name,
        event.path,
        status,
        event.message,
        issues,
        emit,
    );
}

fn emit_item(
    kind: &str,
    name: String,
    path: PathBuf,
    status: &str,
    message: Option<String>,
    issues: &mut Vec<Value>,
    emit: &mut dyn FnMut(Value),
) {
    let source = display_path(&path);
    let mut item =
        json!({ "type": "item", "kind": kind, "name": name, "source": source, "status": status });
    if let Some(message) = message {
        if status == "error" {
            issues.push(
                json!({ "kind": kind, "name": name, "message": format!("{source}: {message}") }),
            );
        }
        item["message"] = Value::String(message);
    }
    emit(item);
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("dsh-startup-inventory-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }

        fn write(&self, relative: &str, body: &str) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, body).unwrap();
            path
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
                .starts_with("dsh-startup-inventory-"));
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn final_event_matches_retained_registries_after_plugin_skill_mounts() {
        let fixture = Fixture::new();
        let unique = format!("inventory-{}", uuid::Uuid::new_v4());
        let body = format!("---\nname: {unique}\n---\nfixture");
        let previous = fixture.write("workspace/.dsh/skills/initial.md", &body);
        fixture.write("outer/plugins/pack/plugin.json", &json!({
            "id":"inventory-pack", "name":"Inventory Pack", "version":"1.0.0",
            "entry":"main.rhai", "skills":["skill.md", "absent.md"],
            "tools":[{"name":"echo", "description":"first"}, {"name":"echo", "description":"replaced"}]
        }).to_string());
        fixture.write(
            "outer/plugins/pack/main.rhai",
            "throw \"must not execute\"; fn echo(args) { args }",
        );
        let mounted = fixture.write("outer/plugins/pack/skill.md", &body);
        fixture.write("outer/plugins/invalid/plugin.json", &json!({
            "id":"inventory-invalid", "name":"Inventory Invalid", "version":"1.0.0", "entry":"main.rhai"
        }).to_string());
        fixture.write("outer/plugins/invalid/main.rhai", "fn missing_brace( {");
        fixture.write(
            "later/duplicate/plugin.json",
            &json!({
                "id":"inventory-pack", "name":"Duplicate", "version":"2.0.0"
            })
            .to_string(),
        );
        let mut events = Vec::new();
        let loaded = load(
            &fixture.0.join("workspace"),
            &fixture.0.join("outer"),
            &[
                fixture.0.join("outer/plugins"),
                fixture.0.join("later"),
                fixture.0.join("missing"),
            ],
            |event| events.push(event),
        );
        assert_eq!(loaded.skills.get(&unique).unwrap().summary.path, mounted);
        assert_eq!(loaded.plugins.ids(), ["inventory-pack"]);
        let complete = events.last().unwrap();
        assert_eq!(complete["type"], "complete");
        assert!(complete["elapsed_ms"].as_u64().is_some());
        assert_eq!(
            complete["plugins"],
            json!([{
                "id":"inventory-pack", "name":"Inventory Pack", "tool_count":1, "status":"loaded"
            }])
        );
        let skills = complete["skills"].as_array().unwrap();
        assert_eq!(skills.len(), loaded.skills.list().len());
        assert!(skills
            .windows(2)
            .all(|pair| pair[0]["name"].as_str() <= pair[1]["name"].as_str()));
        let final_skill = skills.iter().find(|skill| skill["name"] == unique).unwrap();
        assert_eq!(final_skill["source"], display_path(&mounted));
        assert!(events.iter().any(|event| event["type"] == "item"
            && event["source"] == display_path(&previous)
            && event["status"] == "skipped"));
        assert!(events.iter().any(|event| event["kind"] == "plugin"
            && event["name"] == "Duplicate"
            && event["status"] == "skipped"));
        let issues = complete["issues"].as_array().unwrap();
        assert!(issues
            .iter()
            .any(|issue| issue["name"] == "Inventory Invalid" && issue["kind"] == "plugin"));
        assert!(issues
            .iter()
            .any(|issue| issue["name"] == "absent.md" && issue["kind"] == "skill"));
        let stages: Vec<_> = events
            .iter()
            .filter(|event| event["type"] == "stage")
            .collect();
        assert_eq!(
            stages,
            [
                &json!({"type":"stage", "stage":"skills", "status":"loading"}),
                &json!({"type":"stage", "stage":"skills", "status":"complete"}),
                &json!({"type":"stage", "stage":"plugins", "status":"loading"}),
                &json!({"type":"stage", "stage":"plugins", "status":"complete"}),
            ]
        );
        assert!(!fixture.0.join("missing").exists());
    }
}
