use crate::loader::LoadedPlugin;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginMeta {
    pub id: String,
    pub tags: Vec<String>,
    pub examples: Vec<String>,
    pub content_hash: String,
}

pub fn auto_tag_plugin(plugin: &LoadedPlugin, meta_dir: &Path) -> PluginMeta {
    let mut tags = plugin.manifest.tags.clone();
    let blob = format!(
        "{} {} {}",
        plugin.manifest.description,
        plugin
            .manifest
            .tools
            .iter()
            .map(|t| t.description.clone())
            .collect::<Vec<_>>()
            .join(" "),
        plugin.entry_script.clone().unwrap_or_default()
    );
    if tags.is_empty() {
        tags = infer_tags(&blob);
    }
    let examples = synthesize_examples(plugin);
    let hash = {
        let mut hasher = Sha256::new();
        hasher.update(blob.as_bytes());
        hex::encode(hasher.finalize())
    };
    let meta = PluginMeta {
        id: plugin.manifest.id.clone(),
        tags,
        examples,
        content_hash: hash,
    };
    let _ = std::fs::create_dir_all(meta_dir);
    let path = meta_dir.join(format!("plugin-{}.json", plugin.manifest.id));
    if let Ok(text) = serde_json::to_string_pretty(&meta) {
        let _ = std::fs::write(path, text);
    }
    meta
}

fn infer_tags(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let vocab = [
        ("echo", "utility"),
        ("http", "http"),
        ("file", "filesystem"),
        ("shell", "shell"),
        ("git", "git"),
        ("search", "search"),
        ("web", "web"),
        ("test", "testing"),
    ];
    let mut tags = vec!["plugin".into()];
    for (n, t) in vocab {
        if lower.contains(n) && !tags.contains(&t.to_string()) {
            tags.push(t.into());
        }
    }
    tags
}

fn synthesize_examples(plugin: &LoadedPlugin) -> Vec<String> {
    let mut examples = Vec::new();
    examples.push(format!(
        "Call plugin tools under namespace plugin.{}.*",
        plugin.manifest.id
    ));
    for tool in plugin.manifest.tools.iter().take(3) {
        examples.push(format!(
            "Use plugin.{}.{} — {}",
            plugin.manifest.id, tool.name, tool.description
        ));
    }
    if examples.len() == 1 {
        examples.push(format!(
            "Install/reload plugin `{}` then invoke its tools",
            plugin.manifest.id
        ));
    }
    examples
}
