use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SkillSource {
    ProjectDsh,
    ProjectAgents,
    OuterHome,
    OuterWorkspace,
    OpenaiUser,
    Bundled,
    Runtime,
    Plugin,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillResources {
    #[serde(default)]
    pub scripts: Vec<String>,
    #[serde(default)]
    pub references: Vec<String>,
    #[serde(default)]
    pub assets: Vec<String>,
}

impl SkillResources {
    pub fn is_empty(&self) -> bool {
        self.scripts.is_empty() && self.references.is_empty() && self.assets.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    pub source: SkillSource,
    pub path: PathBuf,
    pub tags: Vec<String>,
    pub examples: Vec<String>,
    #[serde(default)]
    pub resources: SkillResources,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRecord {
    pub summary: SkillSummary,
    pub body: String,
    pub format: SkillFormat,
    #[serde(default)]
    pub resources: SkillResources,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SkillFormat {
    OpenaiSkillMd,
    DeepseekDsh,
}

use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use crate::meta::load_or_create_meta;
use crate::parse::parse_skill_file;

struct CachedBody {
    mtime: SystemTime,
    body: String,
}

pub struct SkillCatalog {
    records: RwLock<HashMap<String, SkillRecord>>,
    body_cache: RwLock<HashMap<String, CachedBody>>,
    df_cache: RwLock<Option<(u64, HashMap<String, usize>)>>,
    generation: AtomicU64,
    meta_dir: PathBuf,
}

impl SkillCatalog {
    pub fn new(meta_dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&meta_dir);
        Self {
            records: RwLock::new(HashMap::new()),
            body_cache: RwLock::new(HashMap::new()),
            df_cache: RwLock::new(None),
            generation: AtomicU64::new(0),
            meta_dir,
        }
    }

    fn bump(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        *self.df_cache.write() = None;
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    pub fn discover(&self, workspace_root: &Path, outer_home: &Path, bundled_root: Option<&Path>) {
        let mut found = Vec::new();
        let git_root = find_git_root(workspace_root).unwrap_or_else(|| workspace_root.to_path_buf());

        scan_skill_root(&git_root.join(".dsh/skills"), SkillSource::ProjectDsh, &mut found);
        scan_skill_root(
            &git_root.join(".agents/skills"),
            SkillSource::ProjectAgents,
            &mut found,
        );
        scan_skill_root(
            &workspace_root.join(".dsh-rust/skills"),
            SkillSource::OuterWorkspace,
            &mut found,
        );
        scan_skill_root(&outer_home.join("skills"), SkillSource::OuterHome, &mut found);
        if let Some(home) = dirs::home_dir() {
            scan_skill_root(&home.join(".agents/skills"), SkillSource::OpenaiUser, &mut found);
        }
        if let Some(bundled) = bundled_root {
            scan_skill_root(bundled, SkillSource::Bundled, &mut found);
        }

        let mut map = HashMap::new();
        for record in found.into_iter().rev() {
            let name = record.summary.name.clone();
            map.entry(name).or_insert(record);
        }

        let mut enriched = HashMap::new();
        for (name, mut record) in map {
            let meta = load_or_create_meta(&self.meta_dir, &record);
            if record.summary.tags.is_empty() {
                record.summary.tags = meta.tags;
            }
            if record.summary.examples.is_empty() {
                record.summary.examples = meta.examples;
            }
            enriched.insert(name, record);
        }
        *self.records.write() = enriched;
        self.body_cache.write().clear();
        self.bump();
    }

    /// Mount a skill path from a plugin (or other outer source) without full rediscovery.
    pub fn mount_path(&self, path: &Path, source: SkillSource) -> anyhow::Result<String> {
        let record = parse_skill_file(path, source)?;
        let name = record.summary.name.clone();
        self.records.write().insert(name.clone(), record);
        self.body_cache.write().remove(&name);
        self.bump();
        Ok(name)
    }

    pub fn list(&self) -> Vec<SkillSummary> {
        let mut list: Vec<_> = self
            .records
            .read()
            .values()
            .map(|r| r.summary.clone())
            .collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    }

    pub fn doc_freqs(&self) -> HashMap<String, usize> {
        let gen = self.generation();
        if let Some((g, df)) = self.df_cache.read().as_ref() {
            if *g == gen {
                return df.clone();
            }
        }
        let docs = self.list();
        let df = compute_doc_freqs(&docs);
        *self.df_cache.write() = Some((gen, df.clone()));
        df
    }

    pub fn get(&self, name: &str) -> Option<SkillRecord> {
        let records = self.records.read();
        let Some(base) = records.get(name).cloned() else {
            return None;
        };
        drop(records);

        // Refresh body from disk if mtime changed (progressive disclosure cache).
        let path = base.summary.path.clone();
        let mtime = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        {
            let cache = self.body_cache.read();
            if let Some(c) = cache.get(name) {
                if c.mtime == mtime {
                    let mut out = base;
                    out.body = c.body.clone();
                    return Some(out);
                }
            }
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            let body = if let Ok((_, b)) = split_body_only(&text) {
                b
            } else {
                text
            };
            self.body_cache.write().insert(
                name.to_string(),
                CachedBody {
                    mtime,
                    body: body.clone(),
                },
            );
            let mut out = base;
            out.body = body;
            Some(out)
        } else {
            Some(base)
        }
    }

    pub fn catalog_prompt_section(&self) -> String {
        let list = self.list();
        if list.is_empty() {
            return "No skills discovered.".into();
        }
        let mut out = String::from("Available skills (name — description) [tags]:\n");
        for s in list {
            out.push_str(&format!(
                "- {} — {} [{}]\n",
                s.name,
                s.description,
                s.tags.join(", ")
            ));
        }
        out.push_str("Use skill_load(name) to load full instructions.");
        out
    }

    pub fn meta_dir(&self) -> &Path {
        &self.meta_dir
    }
}

fn split_body_only(text: &str) -> anyhow::Result<((), String)> {
    let trimmed = text.trim_start_matches('\u{feff}');
    if !trimmed.starts_with("---") {
        return Ok(((), trimmed.to_string()));
    }
    let rest = &trimmed[3..];
    let Some(end) = rest.find("\n---") else {
        return Ok(((), trimmed.to_string()));
    };
    Ok(((), rest[end + 4..].trim_start_matches('\n').to_string()))
}

fn scan_skill_root(root: &Path, source: SkillSource, out: &mut Vec<SkillRecord>) {
    if !root.exists() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let skill_md = path.join("SKILL.md");
            if skill_md.exists() {
                if let Ok(record) = parse_skill_file(&skill_md, source.clone()) {
                    out.push(record);
                }
            }
        } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
            if let Ok(record) = parse_skill_file(&path, source.clone()) {
                out.push(record);
            }
        }
    }
}

fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        if cur.join(".git").exists() {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}

fn compute_doc_freqs(docs: &[SkillSummary]) -> HashMap<String, usize> {
    let mut df = HashMap::new();
    for s in docs {
        let mut seen = std::collections::HashSet::new();
        let blob = format!(
            "{} {} {} {}",
            s.name,
            s.description,
            s.tags.join(" "),
            s.examples.join(" ")
        )
        .to_lowercase();
        for t in blob.split(|c: char| !c.is_alphanumeric()) {
            if t.len() > 1 && seen.insert(t.to_string()) {
                *df.entry(t.to_string()).or_insert(0) += 1;
            }
        }
    }
    df
}
