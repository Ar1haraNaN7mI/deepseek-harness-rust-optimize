use crate::catalog::{SkillFormat, SkillRecord, SkillResources, SkillSource, SkillSummary};
use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize, Default)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
    #[serde(rename = "whenToUse")]
    when_to_use: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default, rename = "allowed-tools")]
    allowed_tools: Option<String>,
}

pub fn parse_skill_file(path: &Path, source: SkillSource) -> Result<SkillRecord> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read skill {}", path.display()))?;
    let (fm, body) = split_frontmatter(&text)?;
    let dir_name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("skill");
    let file_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(dir_name);

    let name = fm.name.unwrap_or_else(|| {
        if path.file_name().and_then(|s| s.to_str()) == Some("SKILL.md") {
            sanitize_name(dir_name)
        } else {
            sanitize_name(file_stem)
        }
    });
    if !is_valid_skill_name(&name) {
        bail!("invalid skill name: {name}");
    }
    let description = fm
        .description
        .unwrap_or_else(|| first_paragraph(&body).unwrap_or_else(|| name.clone()));

    let format = if path.file_name().and_then(|s| s.to_str()) == Some("SKILL.md")
        || source == SkillSource::ProjectAgents
        || source == SkillSource::OpenaiUser
    {
        SkillFormat::OpenaiSkillMd
    } else {
        SkillFormat::DeepseekDsh
    };

    let resources = scan_resources(path);
    let mut tags = fm.tags;
    if let Some(allowed) = fm.allowed_tools {
        for t in allowed.split_whitespace() {
            let tag = format!("tool:{t}");
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
    }

    Ok(SkillRecord {
        summary: SkillSummary {
            name,
            description,
            when_to_use: fm.when_to_use,
            source,
            path: path.to_path_buf(),
            tags,
            examples: extract_examples(&body),
            resources: resources.clone(),
        },
        body,
        format,
        resources,
    })
}

fn scan_resources(skill_md: &Path) -> SkillResources {
    let Some(dir) = skill_md.parent() else {
        return SkillResources::default();
    };
    // Flat .md skills have no resource dirs beside them typically.
    if skill_md.file_name().and_then(|s| s.to_str()) != Some("SKILL.md") {
        return SkillResources::default();
    }
    SkillResources {
        scripts: list_rel(dir, "scripts"),
        references: list_rel(dir, "references"),
        assets: list_rel(dir, "assets"),
    }
}

fn list_rel(dir: &Path, sub: &str) -> Vec<String> {
    let root = dir.join(sub);
    if !root.exists() {
        return vec![];
    }
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file() {
                if let Ok(rel) = p.strip_prefix(dir) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    out.sort();
    out
}

fn split_frontmatter(text: &str) -> Result<(Frontmatter, String)> {
    let trimmed = text.trim_start_matches('\u{feff}');
    if !trimmed.starts_with("---") {
        return Ok((Frontmatter::default(), trimmed.to_string()));
    }
    let rest = &trimmed[3..];
    let Some(end) = rest.find("\n---") else {
        return Ok((Frontmatter::default(), trimmed.to_string()));
    };
    let yaml = &rest[..end];
    let body = rest[end + 4..].trim_start_matches('\n').to_string();
    let fm: Frontmatter = serde_yaml::from_str(yaml).unwrap_or_default();
    Ok((fm, body))
}

fn sanitize_name(raw: &str) -> String {
    let lower = raw.to_lowercase();
    let re = Regex::new(r"[^a-z0-9]+").unwrap();
    let s = re.replace_all(&lower, "-");
    s.trim_matches('-').to_string()
}

fn is_valid_skill_name(name: &str) -> bool {
    Regex::new(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
        .unwrap()
        .is_match(name)
}

fn first_paragraph(body: &str) -> Option<String> {
    for line in body.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        return Some(t.chars().take(200).collect());
    }
    None
}

fn extract_examples(body: &str) -> Vec<String> {
    let mut examples = Vec::new();
    let lower = body.to_lowercase();
    for heading in ["## examples", "## example", "## usage", "## when to use"] {
        let Some(idx) = find_heading(&lower, heading) else {
            continue;
        };
        let slice = &body[idx..];
        for line in slice.lines().skip(1).take(14) {
            let t = line.trim();
            if t.starts_with("## ") {
                break;
            }
            if t.starts_with('-') || t.starts_with('*') {
                let item = t.trim_start_matches(['-', '*', ' ']).to_string();
                if !examples.contains(&item) {
                    examples.push(item);
                }
            } else if !t.is_empty() && !t.starts_with('#') && !t.starts_with('`') {
                let item: String = t.chars().take(120).collect();
                if !examples.contains(&item) {
                    examples.push(item);
                }
            }
            if examples.len() >= 5 {
                break;
            }
        }
        if examples.len() >= 5 {
            break;
        }
    }
    examples.into_iter().take(5).collect()
}

fn find_heading(lower: &str, heading: &str) -> Option<usize> {
    let mut start = 0;
    while let Some(rel) = lower[start..].find(heading) {
        let idx = start + rel;
        let after = idx + heading.len();
        let ok_boundary = lower
            .as_bytes()
            .get(after)
            .map(|b| matches!(*b, b'\n' | b'\r' | b' ' | b'\t'))
            .unwrap_or(true);
        if ok_boundary {
            return Some(idx);
        }
        start = after;
    }
    None
}
