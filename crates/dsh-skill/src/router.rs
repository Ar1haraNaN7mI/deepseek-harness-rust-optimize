//! Relevance routing for skills — BM25-ish scoring + learn weights + CTM hints.

use crate::catalog::{SkillCatalog, SkillSummary};
use crate::meta::SkillMeta;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct RankedSkill {
    pub summary: SkillSummary,
    pub score: f32,
    pub reasons: Vec<String>,
}

/// Score skills against a free-text query.
pub fn rank_skills(
    catalog: &SkillCatalog,
    query: &str,
    learn_weights: &HashMap<String, f32>,
    limit: usize,
) -> Vec<RankedSkill> {
    let q = normalize(query);
    let tokens = tokenize(&q);
    let docs: Vec<SkillSummary> = catalog.list();
    let df = catalog.doc_freqs();
    let n_docs = docs.len().max(1) as f32;

    let mut ranked = Vec::new();
    for summary in docs {
        let mut score = 0.0_f32;
        let mut reasons = Vec::new();

        let name = normalize(&summary.name);
        let desc = normalize(&summary.description);
        let tags_joined = summary.tags.iter().map(|t| normalize(t)).collect::<Vec<_>>();
        let examples_joined = summary
            .examples
            .iter()
            .map(|e| normalize(e))
            .collect::<Vec<_>>();
        let wtu = summary
            .when_to_use
            .as_ref()
            .map(|s| normalize(s))
            .unwrap_or_default();

        if name == q || q.contains(&name) || name.contains(&q) {
            score += 10.0;
            reasons.push("name-match".into());
        }

        for t in &tokens {
            let idf = ((n_docs - df.get(t).copied().unwrap_or(0) as f32 + 0.5)
                / (df.get(t).copied().unwrap_or(0) as f32 + 0.5))
                .ln_1p()
                .max(0.0);

            if name.contains(t) {
                score += 3.5 * (1.0 + idf);
                reasons.push(format!("name:{t}"));
            }
            if desc.contains(t) {
                score += 1.6 * (1.0 + idf * 0.5);
            }
            for tag in &tags_joined {
                if tag == t || tag.contains(t) {
                    score += 2.8 * (1.0 + idf * 0.3);
                    reasons.push(format!("tag:{tag}"));
                }
            }
            for ex in &examples_joined {
                if ex.contains(t) {
                    score += 1.4;
                }
            }
            if !wtu.is_empty() && wtu.contains(t) {
                score += 2.2;
            }
        }

        // Resource richness small bonus (scripts imply executable workflows)
        if !summary.resources.scripts.is_empty() {
            score += 0.4;
            reasons.push("has-scripts".into());
        }
        if !summary.resources.references.is_empty() {
            score += 0.2;
        }

        let key = format!("skill:{}", summary.name);
        if let Some(w) = learn_weights.get(&key) {
            score += w * 2.4;
            if *w > 0.1 {
                reasons.push(format!("learned:+{w:.2}"));
            }
        }

        // Soft floor so empty catalogs still return something useful for generic queries
        if score <= 0.0 && tokens.is_empty() {
            score = 0.1;
        }

        if score > 0.0 {
            reasons.truncate(6);
            ranked.push(RankedSkill {
                summary,
                score,
                reasons,
            });
        }
    }

    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if ranked.is_empty() {
        // Fallback: return first N by name for discoverability
        for s in catalog.list().into_iter().take(limit.max(1)) {
            ranked.push(RankedSkill {
                summary: s,
                score: 0.05,
                reasons: vec!["fallback".into()],
            });
        }
    }
    ranked.truncate(limit.max(1));
    ranked
}

pub fn prompt_topk_section(ranked: &[RankedSkill]) -> String {
    if ranked.is_empty() {
        return "No strongly relevant skills for this query. Use skill_search if needed.".into();
    }
    let mut out = String::from(
        "Relevant skills for this turn (skill_load before following; check resources):\n",
    );
    for r in ranked {
        out.push_str(&format!(
            "- {} (score={:.1}) — {} [{}]\n",
            r.summary.name,
            r.score,
            r.summary.description,
            r.summary.tags.join(", ")
        ));
        if !r.summary.examples.is_empty() {
            out.push_str(&format!(
                "  e.g. {}\n",
                r.summary.examples.first().cloned().unwrap_or_default()
            ));
        }
        if !r.summary.resources.is_empty() {
            out.push_str(&format!(
                "  resources: scripts={} refs={} assets={}\n",
                r.summary.resources.scripts.len(),
                r.summary.resources.references.len(),
                r.summary.resources.assets.len()
            ));
        }
    }
    out.push_str(
        "Routing: skill_search → skill_load(name) → follow body; use plugin.* only when skill requires it.",
    );
    out
}

pub fn load_learn_weights(meta_dir: &Path) -> HashMap<String, f32> {
    let path = meta_dir.join("learn-weights.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_learn_weights(meta_dir: &Path, weights: &HashMap<String, f32>) {
    let _ = std::fs::create_dir_all(meta_dir);
    let path = meta_dir.join("learn-weights.json");
    if let Ok(text) = serde_json::to_string_pretty(weights) {
        let _ = std::fs::write(path, text);
    }
}

pub fn bump_weight(meta_dir: &Path, key: &str, delta: f32) {
    let mut w = load_learn_weights(meta_dir);
    let entry = w.entry(key.to_string()).or_insert(0.0);
    *entry = (*entry + delta).clamp(-5.0, 10.0);
    save_learn_weights(meta_dir, &w);
}

fn normalize(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn tokenize(s: &str) -> Vec<String> {
    s.split_whitespace()
        .filter(|t| t.len() > 1)
        .map(|t| t.to_string())
        .collect()
}

pub fn ensure_tags_from_meta(summary: &mut SkillSummary, meta: &SkillMeta) {
    if summary.tags.is_empty() {
        summary.tags = meta.tags.clone();
    }
    if summary.examples.is_empty() {
        summary.examples = meta.examples.clone();
    }
}
