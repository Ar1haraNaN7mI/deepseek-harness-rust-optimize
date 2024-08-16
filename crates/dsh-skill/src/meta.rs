use crate::catalog::SkillRecord;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillMeta {
    pub name: String,
    pub content_hash: String,
    pub tags: Vec<String>,
    pub examples: Vec<String>,
    pub format: String,
}

pub fn load_or_create_meta(meta_dir: &Path, record: &SkillRecord) -> SkillMeta {
    let path = meta_dir.join(format!("skill-{}.json", record.summary.name));
    let hash = content_hash(&record.body);
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(existing) = serde_json::from_str::<SkillMeta>(&text) {
            if existing.content_hash == hash {
                return existing;
            }
        }
    }
    auto_tag_and_examples(record, meta_dir)
}

pub fn auto_tag_and_examples(record: &SkillRecord, meta_dir: &Path) -> SkillMeta {
    let mut tags = record.summary.tags.clone();
    if tags.is_empty() {
        tags = infer_tags(&record.summary.description, &record.body);
    }
    let mut examples = record.summary.examples.clone();
    if examples.is_empty() {
        examples = synthesize_examples(&record.summary.name, &record.summary.description);
    }
    let meta = SkillMeta {
        name: record.summary.name.clone(),
        content_hash: content_hash(&record.body),
        tags,
        examples,
        format: format!("{:?}", record.format),
    };
    let _ = std::fs::create_dir_all(meta_dir);
    let path = meta_dir.join(format!("skill-{}.json", record.summary.name));
    if let Ok(text) = serde_json::to_string_pretty(&meta) {
        let _ = std::fs::write(path, text);
    }
    meta
}

fn content_hash(body: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    hex::encode(hasher.finalize())
}

fn infer_tags(description: &str, body: &str) -> Vec<String> {
    let text = format!("{description}\n{body}").to_lowercase();
    let vocab = [
        ("git", "git"),
        ("test", "testing"),
        ("rust", "rust"),
        ("python", "python"),
        ("docker", "docker"),
        ("http", "http"),
        ("api", "api"),
        ("refactor", "refactor"),
        ("debug", "debug"),
        ("security", "security"),
        ("plugin", "plugin"),
        ("skill", "skill"),
        ("tui", "tui"),
        ("shell", "shell"),
        ("file", "filesystem"),
        ("web", "web"),
        ("docs", "docs"),
        ("ci", "ci"),
    ];
    let mut tags = Vec::new();
    for (needle, tag) in vocab {
        if text.contains(needle) && !tags.contains(&tag.to_string()) {
            tags.push(tag.to_string());
        }
    }
    if tags.is_empty() {
        tags.push("general".into());
    }
    tags.into_iter().take(8).collect()
}

fn synthesize_examples(name: &str, description: &str) -> Vec<String> {
    vec![
        format!("Use the `{name}` skill when: {description}"),
        format!("Load skill `{name}` then follow its steps."),
        format!("Ask: help me with {} workflow", name.replace('-', " ")),
    ]
}
