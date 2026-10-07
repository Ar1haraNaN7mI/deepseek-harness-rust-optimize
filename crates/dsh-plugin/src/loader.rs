use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub tools: Vec<PluginToolDecl>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginToolDecl {
    pub name: String,
    pub description: String,
    #[serde(default = "default_params")]
    pub parameters: serde_json::Value,
    /// Rhai function name to invoke, defaults to `name`.
    #[serde(default)]
    pub rhai_fn: Option<String>,
}

fn default_params() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "input": { "type": "string" }
        }
    })
}

#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    pub root: PathBuf,
    pub entry_script: Option<String>,
}

pub fn load_plugin_dir(root: &Path) -> Result<LoadedPlugin> {
    let manifest_path = find_manifest(root)?;
    let text = fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: PluginManifest =
        if manifest_path.file_name().and_then(|s| s.to_str()) == Some("package.json") {
            adapt_package_json(&text)?
        } else if manifest_path.extension().and_then(|s| s.to_str()) == Some("json") {
            serde_json::from_str(&text)?
        } else {
            serde_yaml::from_str(&text)?
        };

    if manifest.id.trim().is_empty() {
        bail!("plugin id required");
    }
    if !is_valid_id(&manifest.id) {
        bail!("invalid plugin id: {}", manifest.id);
    }
    for relative in manifest.entry.iter().chain(manifest.skills.iter()) {
        let path = Path::new(relative);
        if path.is_absolute()
            || path.components().any(|component| {
                !matches!(
                    component,
                    std::path::Component::Normal(_) | std::path::Component::CurDir
                )
            })
        {
            bail!("plugin entry and skill paths must stay inside the plugin directory: {relative}");
        }
        let resolved = root.join(path);
        if resolved.exists() && !resolved.canonicalize()?.starts_with(root.canonicalize()?) {
            bail!("plugin resource resolves outside its directory: {relative}");
        }
    }

    let entry_script = if let Some(entry) = &manifest.entry {
        let path = root.join(entry);
        if path.exists() {
            Some(
                fs::read_to_string(&path)
                    .with_context(|| format!("read entry {}", path.display()))?,
            )
        } else {
            None
        }
    } else {
        let default = root.join("main.rhai");
        if default.exists() {
            Some(fs::read_to_string(default)?)
        } else {
            None
        }
    };

    Ok(LoadedPlugin {
        manifest,
        root: root.to_path_buf(),
        entry_script,
    })
}

/// Resolve skill paths declared by a plugin (relative to plugin root).
pub fn plugin_skill_paths(plugin: &LoadedPlugin) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for rel in &plugin.manifest.skills {
        let p = plugin.root.join(rel);
        if p.is_file() {
            out.push(p);
        } else if p.is_dir() {
            let skill_md = p.join("SKILL.md");
            if skill_md.exists() {
                out.push(skill_md);
            }
        }
    }
    // Convention: <plugin>/skills/**/SKILL.md
    let skills_dir = plugin.root.join("skills");
    if skills_dir.is_dir() {
        if let Ok(walker) = walkdir::WalkDir::new(&skills_dir)
            .max_depth(3)
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
        {
            for entry in walker {
                let path = entry.path();
                if path.file_name().and_then(|s| s.to_str()) == Some("SKILL.md") {
                    out.push(path.to_path_buf());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn find_manifest(root: &Path) -> Result<PathBuf> {
    for name in ["plugin.yml", "plugin.yaml", "plugin.json", "dsh.plugin.yml"] {
        let p = root.join(name);
        if p.exists() {
            return Ok(p);
        }
    }
    // DeepSeek npm-style package.json with dsh field — adapt skills/tools lightly.
    let pkg = root.join("package.json");
    if pkg.exists() {
        return Ok(pkg);
    }
    bail!("no plugin.yml/plugin.json in {}", root.display())
}

fn adapt_package_json(text: &str) -> Result<PluginManifest> {
    let value: serde_json::Value = serde_json::from_str(text)?;
    let name = value
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("adapted-plugin")
        .trim_start_matches('@')
        .replace('/', "-");
    let id = sanitize_id(&name);
    let description = value
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("Adapted from package.json (DeepSeek plugin compatibility shim)")
        .to_string();
    let version = value
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0")
        .to_string();
    let mut tools = Vec::new();
    if let Some(arr) = value.pointer("/dsh/tools").and_then(|v| v.as_array()) {
        for t in arr {
            if let Some(n) = t.get("name").and_then(|v| v.as_str()) {
                tools.push(PluginToolDecl {
                    name: n.to_string(),
                    description: t
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("adapted tool")
                        .to_string(),
                    parameters: t.get("parameters").cloned().unwrap_or_else(default_params),
                    rhai_fn: None,
                });
            }
        }
    }
    let mut skills = Vec::new();
    if let Some(arr) = value.pointer("/dsh/skills").and_then(|v| v.as_array()) {
        for s in arr {
            if let Some(n) = s.as_str() {
                skills.push(n.to_string());
            }
        }
    }
    Ok(PluginManifest {
        id,
        name,
        version,
        description,
        entry: None,
        tools,
        skills,
        permissions: vec![],
        tags: vec!["adapted".into(), "deepseek-plugin".into()],
    })
}

fn sanitize_id(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Atomic install: copy to temp → validate → rename into place.
pub fn install_plugin_from_path(src: &Path, dest_root: &Path) -> Result<PathBuf> {
    let loaded = load_plugin_dir(src)?;
    fs::create_dir_all(dest_root)?;
    let source = src.canonicalize()?;
    let destination = dest_root.canonicalize()?;
    if destination.starts_with(&source) {
        bail!("plugin install destination must not be inside its source directory");
    }

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let staging = dest_root.join(format!(".staging-{}-{}", loaded.manifest.id, stamp));
    fs::create_dir(&staging)?;
    if let Err(error) = copy_dir(src, &staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }

    // Validate staged copy before swapping.
    let staged = load_plugin_dir(&staging)?;
    if staged.manifest.id != loaded.manifest.id {
        let _ = fs::remove_dir_all(&staging);
        bail!("staged plugin id mismatch");
    }

    let dest = dest_root.join(&loaded.manifest.id);
    let backup = dest_root.join(format!(".backup-{}-{}", loaded.manifest.id, stamp));

    if dest.exists() {
        if backup.exists() {
            fs::remove_dir_all(&backup)?;
        }
        fs::rename(&dest, &backup).with_context(|| {
            format!(
                "backup existing plugin {} → {}",
                dest.display(),
                backup.display()
            )
        })?;
    }

    match fs::rename(&staging, &dest) {
        Ok(()) => {
            let _ = fs::remove_dir_all(&backup);
            // Final validate
            let _ = load_plugin_dir(&dest)?;
            Ok(dest)
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&staging);
            if backup.exists() && !dest.exists() {
                let _ = fs::rename(&backup, &dest);
            }
            Err(e).with_context(|| format!("atomic install rename into {}", dest.display()))
        }
    }
}

fn copy_dir(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    for entry in walkdir::WalkDir::new(src) {
        let entry = entry?;
        if entry.path_is_symlink() {
            bail!(
                "plugin installation does not copy symbolic links: {}",
                entry.path().display()
            );
        }
        let rel = entry.path().strip_prefix(src)?;
        let target = dest.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
