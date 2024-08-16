//! Self-learning store: episode memory + capability weights (outer-layer durable).

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnEpisode {
    pub id: String,
    pub at: DateTime<Utc>,
    pub query: String,
    pub skill: Option<String>,
    pub plugin: Option<String>,
    pub tool: String,
    pub ok: bool,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LearnState {
    pub weights: HashMap<String, f32>,
    pub episodes: Vec<LearnEpisode>,
    /// Capability co-activation counts for CTM-style sync priors.
    pub sync_pairs: HashMap<String, u32>,
}

pub struct LearnStore {
    path: PathBuf,
    state: RwLock<LearnState>,
    dirty: AtomicBool,
}

impl LearnStore {
    pub fn open(outer_home: &Path) -> Self {
        let dir = outer_home.join("learn");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("state.json");
        let state: LearnState = fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let meta = outer_home.join("meta");
        let _ = fs::create_dir_all(&meta);
        if let Ok(text) = serde_json::to_string(&state.weights) {
            let _ = fs::write(meta.join("learn-weights.json"), text);
        }
        Self {
            path,
            state: RwLock::new(state),
            dirty: AtomicBool::new(false),
        }
    }

    pub fn persist(&self) {
        let state = self.state.read().clone();
        if let Ok(text) = serde_json::to_string(&state) {
            let _ = fs::write(&self.path, text);
        }
        if let Some(parent) = self.path.parent() {
            if let Some(outer) = parent.parent() {
                let meta = outer.join("meta/learn-weights.json");
                if let Ok(text) = serde_json::to_string(&state.weights) {
                    let _ = fs::write(meta, text);
                }
            }
        }
        self.dirty.store(false, Ordering::Relaxed);
    }

    pub fn flush_dirty(&self) {
        if self.dirty.swap(false, Ordering::AcqRel) {
            self.persist();
        }
    }

    pub fn record_tool_outcome(
        &self,
        query: &str,
        tool: &str,
        ok: bool,
        note: impl Into<String>,
    ) {
        self.record_tool_outcome_detailed(query, tool, None, ok, note);
    }

    pub fn record_tool_outcome_detailed(
        &self,
        query: &str,
        tool: &str,
        skill_hint: Option<&str>,
        ok: bool,
        note: impl Into<String>,
    ) {
        let (mut skill, plugin) = classify_tool(tool);
        if tool == "skill_load" {
            skill = skill_hint.map(|s| s.to_string());
        }
        let mut state = self.state.write();
        let episode = LearnEpisode {
            id: Uuid::new_v4().to_string(),
            at: Utc::now(),
            query: query.chars().take(240).collect(),
            skill: skill.clone(),
            plugin: plugin.clone(),
            tool: tool.to_string(),
            ok,
            note: note.into(),
        };
        state.episodes.push(episode);
        if state.episodes.len() > 500 {
            let drain = state.episodes.len() - 500;
            state.episodes.drain(0..drain);
        }

        let delta = if ok { 0.35 } else { -0.25 };
        if let Some(s) = &skill {
            bump(&mut state.weights, &format!("skill:{s}"), delta);
        }
        if let Some(p) = &plugin {
            bump(&mut state.weights, &format!("plugin:{p}"), delta);
        }
        bump(&mut state.weights, &format!("tool:{tool}"), delta * 0.5);

        if ok {
            if let (Some(s), Some(p)) = (skill, plugin) {
                let key = sync_key(&s, &p);
                *state.sync_pairs.entry(key).or_insert(0) += 1;
            }
        }
        drop(state);
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn weights(&self) -> HashMap<String, f32> {
        self.state.read().weights.clone()
    }

    pub fn recall(&self, query: &str, limit: usize) -> Vec<LearnEpisode> {
        let q = query.to_lowercase();
        let tokens: Vec<_> = q
            .split_whitespace()
            .filter(|t| t.len() > 2)
            .take(12)
            .collect();
        if tokens.is_empty() {
            return Vec::new();
        }
        // Scan newest-first, stop once we have enough strong hits (cap work).
        let mut scored: Vec<(f32, LearnEpisode)> = Vec::with_capacity(limit * 2);
        let episodes = self.state.read();
        for ep in episodes.episodes.iter().rev().take(120) {
            let mut score = 0.0_f32;
            let qlow = ep.query.to_lowercase();
            for t in &tokens {
                if qlow.contains(t) || ep.tool.contains(t) {
                    score += 1.0;
                }
                if let Some(s) = &ep.skill {
                    if s.contains(t) {
                        score += 0.5;
                    }
                }
                if let Some(p) = &ep.plugin {
                    if p.contains(t) {
                        score += 0.5;
                    }
                }
            }
            if ep.ok {
                score += 0.5;
            }
            if score > 0.0 {
                scored.push((score, ep.clone()));
            }
            if scored.len() >= limit * 4 {
                break;
            }
        }
        drop(episodes);
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored
            .into_iter()
            .take(limit.max(1))
            .map(|(_, e)| e)
            .collect()
    }

    pub fn sync_priors(&self) -> HashMap<String, u32> {
        self.state.read().sync_pairs.clone()
    }

    pub fn prompt_section(&self, query: &str) -> String {
        let recalled = self.recall(query, 3);
        if recalled.is_empty() {
            return "No prior learned episodes for this query.".into();
        }
        let mut out = String::from("Self-learning recall (prior successful/failed paths):\n");
        for ep in recalled {
            out.push_str(&format!(
                "- [{}] tool={} skill={:?} plugin={:?} :: {}\n",
                if ep.ok { "ok" } else { "fail" },
                ep.tool,
                ep.skill,
                ep.plugin,
                ep.note.chars().take(120).collect::<String>()
            ));
        }
        out.push_str("Prefer repeating ok paths; avoid fail paths unless the user insists.");
        out
    }
}

fn bump(map: &mut HashMap<String, f32>, key: &str, delta: f32) {
    let e = map.entry(key.to_string()).or_insert(0.0);
    *e = (*e + delta).clamp(-5.0, 10.0);
}

fn classify_tool(tool: &str) -> (Option<String>, Option<String>) {
    if tool == "skill_load" || tool == "skill_search" || tool == "skill_recommend" {
        return (None, None);
    }
    if let Some(rest) = tool.strip_prefix("plugin.") {
        let id = rest.split('.').next().unwrap_or(rest);
        return (None, Some(id.to_string()));
    }
    (None, None)
}

fn sync_key(a: &str, b: &str) -> String {
    if a <= b {
        format!("{a}|{b}")
    } else {
        format!("{b}|{a}")
    }
}
