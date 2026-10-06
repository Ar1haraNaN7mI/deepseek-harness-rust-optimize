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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillLoadStatus {
    Loaded,
    Error,
    Skipped,
}

/// A completed discovery/mount operation. Paths identify the actual local source.
#[derive(Debug, Clone)]
pub struct SkillLoadEvent {
    pub name: String,
    pub path: PathBuf,
    pub source: SkillSource,
    pub status: SkillLoadStatus,
    pub message: Option<String>,
}

impl SkillLoadEvent {
    fn from_record(record: &SkillRecord, status: SkillLoadStatus, message: Option<String>) -> Self {
        Self {
            name: record.summary.name.clone(),
            path: record.summary.path.clone(),
            source: record.summary.source.clone(),
            status,
            message,
        }
    }
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
        self.discover_observed(workspace_root, outer_home, bundled_root, |_| {});
    }

    /// Discover the same roots and use the same precedence as `discover`, while
    /// reporting parse errors, duplicate decisions and committed catalog entries.
    pub fn discover_observed(
        &self,
        workspace_root: &Path,
        outer_home: &Path,
        bundled_root: Option<&Path>,
        mut observe: impl FnMut(SkillLoadEvent),
    ) {
        let git_root =
            find_git_root(workspace_root).unwrap_or_else(|| workspace_root.to_path_buf());
        let mut roots = vec![
            (git_root.join(".dsh/skills"), SkillSource::ProjectDsh),
            (git_root.join(".agents/skills"), SkillSource::ProjectAgents),
            (
                workspace_root.join(".dsh-rust/skills"),
                SkillSource::OuterWorkspace,
            ),
            (outer_home.join("skills"), SkillSource::OuterHome),
        ];
        if let Some(home) = dirs::home_dir() {
            roots.push((home.join(".agents/skills"), SkillSource::OpenaiUser));
        }
        if let Some(bundled) = bundled_root {
            roots.push((bundled.to_path_buf(), SkillSource::Bundled));
        }
        self.discover_roots_observed(&roots, &mut observe);
    }

    fn discover_roots_observed(
        &self,
        roots: &[(PathBuf, SkillSource)],
        observe: &mut dyn FnMut(SkillLoadEvent),
    ) {
        let mut found = Vec::new();
        for (root, source) in roots {
            scan_skill_root(root, source.clone(), &mut found, observe);
        }

        let mut map = HashMap::new();
        let mut skipped = Vec::new();
        // Preserve the original later-root-wins precedence. Directory entries
        // within a root are now sorted, making same-root duplicate ties stable.
        for record in found.into_iter().rev() {
            let name = record.summary.name.clone();
            if let Some(winner) = map.get(&name) {
                let winner: &SkillRecord = winner;
                skipped.push(SkillLoadEvent::from_record(
                    &record,
                    SkillLoadStatus::Skipped,
                    Some(format!(
                        "duplicate name; retained {}",
                        winner.summary.path.display()
                    )),
                ));
            } else {
                map.insert(name, record);
            }
        }

        let mut enriched = HashMap::new();
        let mut winners: Vec<_> = map.into_iter().collect();
        winners.sort_by(|a, b| a.0.cmp(&b.0));
        let mut loaded = Vec::with_capacity(winners.len());
        for (name, mut record) in winners {
            let meta = load_or_create_meta(&self.meta_dir, &record);
            if record.summary.tags.is_empty() {
                record.summary.tags = meta.tags;
            }
            if record.summary.examples.is_empty() {
                record.summary.examples = meta.examples;
            }
            loaded.push(SkillLoadEvent::from_record(
                &record,
                SkillLoadStatus::Loaded,
                None,
            ));
            enriched.insert(name, record);
        }
        *self.records.write() = enriched;
        self.body_cache.write().clear();
        self.bump();
        skipped.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
        for event in skipped.into_iter().chain(loaded) {
            observe(event);
        }
    }

    /// Mount a skill path from a plugin (or other outer source) without full rediscovery.
    pub fn mount_path(&self, path: &Path, source: SkillSource) -> anyhow::Result<String> {
        self.mount_path_observed(path, source, |_| {})
    }

    pub fn mount_path_observed(
        &self,
        path: &Path,
        source: SkillSource,
        mut observe: impl FnMut(SkillLoadEvent),
    ) -> anyhow::Result<String> {
        let record = match parse_skill_file(path, source.clone()) {
            Ok(record) => record,
            Err(error) => {
                observe(skill_error(path, source, format!("{error:#}")));
                return Err(error);
            }
        };
        report_frontmatter_diagnostic(&record, &mut observe);
        let name = record.summary.name.clone();
        let loaded = SkillLoadEvent::from_record(&record, SkillLoadStatus::Loaded, None);
        let replaced = self.records.write().insert(name.clone(), record);
        self.body_cache.write().remove(&name);
        self.bump();
        if let Some(previous) = replaced {
            observe(SkillLoadEvent::from_record(
                &previous,
                SkillLoadStatus::Skipped,
                Some(format!("replaced by mounted skill {}", path.display())),
            ));
        }
        observe(loaded);
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
        let base = records.get(name).cloned()?;
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

fn skill_error(path: &Path, source: SkillSource, message: String) -> SkillLoadEvent {
    let display = if path.file_name().and_then(|s| s.to_str()) == Some("SKILL.md") {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    SkillLoadEvent {
        name: display
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        path: path.to_path_buf(),
        source,
        status: SkillLoadStatus::Error,
        message: Some(message),
    }
}

// The existing parser intentionally falls back to defaults for malformed YAML.
// Keep that compatibility while making the fallback visible to an observer.
fn report_frontmatter_diagnostic(record: &SkillRecord, observe: &mut dyn FnMut(SkillLoadEvent)) {
    // Match parse.rs's accepted field types through the YAML deserializer itself:
    // scalar numbers/bools can be strings there, unlike deserializing a Value.
    #[derive(Deserialize)]
    struct FrontmatterDiagnostic {
        #[serde(rename = "name")]
        _name: Option<String>,
        #[serde(rename = "description")]
        _description: Option<String>,
        #[serde(rename = "whenToUse")]
        _when_to_use: Option<String>,
        #[serde(default, rename = "tags")]
        _tags: Vec<String>,
        #[serde(default, rename = "allowed-tools")]
        _allowed_tools: Option<String>,
    }
    let Ok(text) = std::fs::read_to_string(&record.summary.path) else {
        return;
    };
    let trimmed = text.trim_start_matches('\u{feff}');
    let Some(rest) = trimmed.strip_prefix("---") else {
        return;
    };
    let diagnostic = match rest.find("\n---") {
        None => {
            Some("unterminated frontmatter; loaded as body by the compatibility parser".to_string())
        }
        Some(end) if rest[..end].trim().is_empty() => None,
        Some(end) => match serde_yaml::from_str::<FrontmatterDiagnostic>(&rest[..end]) {
            Err(error) => Some(format!(
                "invalid frontmatter; default metadata used: {error}"
            )),
            Ok(_) => None,
        },
    };
    if let Some(message) = diagnostic {
        observe(SkillLoadEvent::from_record(
            record,
            SkillLoadStatus::Error,
            Some(message),
        ));
    }
}

fn scan_skill_root(
    root: &Path,
    source: SkillSource,
    out: &mut Vec<SkillRecord>,
    observe: &mut dyn FnMut(SkillLoadEvent),
) {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            observe(skill_error(
                root,
                source,
                format!("read skill directory {}: {error}", root.display()),
            ));
            return;
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => paths.push(entry.path()),
            Err(error) => observe(skill_error(
                root,
                source.clone(),
                format!("read skill directory entry: {error}"),
            )),
        }
    }
    paths.sort();
    for path in paths {
        let candidate = if path.is_dir() {
            let skill_md = path.join("SKILL.md");
            match std::fs::metadata(&skill_md) {
                Ok(_) => Some(skill_md),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    observe(skill_error(&skill_md, source.clone(), error.to_string()));
                    None
                }
            }
        } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
            Some(path)
        } else {
            None
        };
        if let Some(path) = candidate {
            match parse_skill_file(&path, source.clone()) {
                Ok(record) => {
                    report_frontmatter_diagnostic(&record, observe);
                    out.push(record);
                }
                Err(error) => observe(skill_error(&path, source.clone(), format!("{error:#}"))),
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

#[cfg(test)]
mod observed_tests {
    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let name = format!(
                "dsh-skill-observed-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
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
                .starts_with("dsh-skill-observed-"));
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_and_empty_roots_are_empty_without_errors() {
        let fixture = Fixture::new();
        let empty = fixture.0.join("empty");
        std::fs::create_dir(&empty).unwrap();
        let catalog = SkillCatalog::new(fixture.0.join("meta"));
        let mut events = Vec::new();
        catalog.discover_roots_observed(
            &[
                (empty, SkillSource::ProjectDsh),
                (fixture.0.join("missing"), SkillSource::OuterHome),
            ],
            &mut |event| events.push(event),
        );
        assert!(catalog.list().is_empty());
        assert!(events.is_empty());
    }

    #[test]
    fn duplicate_precedence_is_preserved_and_observed_after_commit() {
        let fixture = Fixture::new();
        let earlier = fixture.write("earlier/one.md", "---\nname: shared\n---\nearlier");
        let later = fixture.write("later/two.md", "---\nname: shared\n---\nlater");
        let catalog = SkillCatalog::new(fixture.0.join("meta"));
        let mut events = Vec::new();
        catalog.discover_roots_observed(
            &[
                (fixture.0.join("earlier"), SkillSource::ProjectDsh),
                (fixture.0.join("later"), SkillSource::OuterHome),
            ],
            &mut |event| {
                if event.status == SkillLoadStatus::Loaded {
                    assert_eq!(catalog.get(&event.name).unwrap().summary.path, event.path);
                }
                events.push(event);
            },
        );
        assert_eq!(catalog.get("shared").unwrap().summary.path, later);
        assert!(events
            .iter()
            .any(|event| event.path == earlier && event.status == SkillLoadStatus::Skipped));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.status == SkillLoadStatus::Loaded)
                .count(),
            1
        );

        let mounted = fixture.write("plugin/SKILL.md", "---\nname: shared\n---\nplugin");
        events.clear();
        catalog
            .mount_path_observed(&mounted, SkillSource::Plugin, |event| {
                assert_eq!(catalog.get("shared").unwrap().summary.path, mounted);
                events.push(event);
            })
            .unwrap();
        assert_eq!(events[0].status, SkillLoadStatus::Skipped);
        assert_eq!(events[0].path, later);
        assert_eq!(events[1].status, SkillLoadStatus::Loaded);
        assert_eq!(events[1].path, mounted);
    }

    #[test]
    fn malformed_frontmatter_is_visible_without_changing_legacy_fallback() {
        let fixture = Fixture::new();
        fixture.write("skills/empty.md", "");
        let malformed = fixture.write("skills/malformed.md", "---\nname: [\n---\nbody");
        let wrong_type = fixture.write("skills/wrong-type.md", "---\nname: [list]\n---\nbody");
        let invalid_name = fixture.write("skills/invalid.md", "---\nname: BAD name!\n---\nbody");
        let valid = fixture.write(
            "skills/valid.md",
            "---\nname: valid\ndescription: 123\ntags: [123]\n---\nbody",
        );
        let catalog = SkillCatalog::new(fixture.0.join("meta"));
        let mut events = Vec::new();
        catalog.discover_roots_observed(
            &[(fixture.0.join("skills"), SkillSource::ProjectDsh)],
            &mut |event| events.push(event),
        );
        assert!(catalog.get("empty").is_some());
        assert!(catalog.get("malformed").is_some());
        assert!(catalog.get("wrong-type").is_some());
        assert!(catalog.get("invalid").is_none());
        for path in [malformed, wrong_type, invalid_name] {
            assert!(events
                .iter()
                .any(|event| event.path == path && event.status == SkillLoadStatus::Error));
        }
        assert!(events
            .iter()
            .all(|event| event.path != valid || event.status != SkillLoadStatus::Error));
        assert_eq!(catalog.get("valid").unwrap().summary.description, "123");
        let names: Vec<_> = catalog.list().into_iter().map(|skill| skill.name).collect();
        assert_eq!(names, ["empty", "malformed", "valid", "wrong-type"]);
    }
}
