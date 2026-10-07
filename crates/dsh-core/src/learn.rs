//! Self-learning store: episode memory + capability weights (outer-layer durable).

use anyhow::{Context, Result};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnFeedback {
    pub id: String,
    pub at: DateTime<Utc>,
    pub task_id: String,
    pub run_id: String,
    pub goal: String,
    pub attempt: u32,
    pub ok: bool,
    pub note: String,
    /// Retain exact reinforcement provenance so deleting an episode also
    /// removes its later feedback-derived routing influence.
    #[serde(default)]
    pub recalled_episode_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LearnState {
    #[serde(default)]
    pub weights: HashMap<String, f32>,
    #[serde(default)]
    pub episodes: Vec<LearnEpisode>,
    /// Capability co-activation counts for CTM-style sync priors.
    #[serde(default)]
    pub sync_pairs: HashMap<String, u32>,
    /// Durable task/run outcomes used to bias future routing.
    #[serde(default)]
    pub feedback: Vec<LearnFeedback>,
}

pub struct LearnStore {
    path: PathBuf,
    state: RwLock<LearnState>,
    dirty: AtomicBool,
    retrieval_enabled: AtomicBool,
    generation_enabled: AtomicBool,
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
            retrieval_enabled: AtomicBool::new(true),
            generation_enabled: AtomicBool::new(true),
        }
    }

    pub fn set_enabled(&self, retrieval: bool, generation: bool) {
        self.retrieval_enabled.store(retrieval, Ordering::Release);
        self.generation_enabled.store(generation, Ordering::Release);
    }

    /// Management access remains available when model recall is disabled.
    pub fn list(&self) -> LearnState {
        self.state.read().clone()
    }

    pub fn delete_episode(&self, id: &str) -> Result<bool> {
        self.delete_record(id, true)
    }

    pub fn delete_feedback(&self, id: &str) -> Result<bool> {
        self.delete_record(id, false)
    }

    fn delete_record(&self, id: &str, episode: bool) -> Result<bool> {
        let mut state = self.state.write();
        let mut next = state.clone();
        let removed = if episode {
            next.episodes.retain(|record| record.id != id);
            for feedback in &mut next.feedback {
                if let Some(ids) = &mut feedback.recalled_episode_ids {
                    ids.retain(|value| value != id);
                }
            }
            next.episodes.len() != state.episodes.len()
        } else {
            next.feedback.retain(|record| record.id != id);
            next.feedback.len() != state.feedback.len()
        };
        if !removed {
            return Ok(false);
        }
        rebuild_derived(&mut next);
        self.persist_state(&next, &state)?;
        *state = next;
        self.dirty.store(false, Ordering::Release);
        Ok(true)
    }

    /// Remove episodes, task feedback, routing weights, and sync priors together.
    pub fn clear(&self) -> Result<()> {
        let mut state = self.state.write();
        let empty = LearnState::default();
        self.persist_state(&empty, &state)?;
        *state = empty;
        self.dirty.store(false, Ordering::Release);
        Ok(())
    }

    fn persist_state(&self, next: &LearnState, previous: &LearnState) -> Result<()> {
        let weights_path = self
            .path
            .parent()
            .and_then(Path::parent)
            .context("Learning store has no outer home")?
            .join("meta/learn-weights.json");
        let state_bytes = serde_json::to_vec(next)?;
        let weights_bytes = serde_json::to_vec(&next.weights)?;
        // The state file is authoritative. Publish the derived cache first,
        // restoring it on a failed authoritative write before reporting failure.
        crate::settings::atomic_write(&weights_path, &weights_bytes)?;
        if let Err(error) = crate::settings::atomic_write(&self.path, &state_bytes) {
            crate::settings::atomic_write(&weights_path, &serde_json::to_vec(&previous.weights)?)
                .with_context(|| {
                format!("{error:#}; failed to restore the prior learning-weight cache")
            })?;
            return Err(error);
        }
        Ok(())
    }

    pub fn try_persist(&self) -> Result<()> {
        // Keep recording/deletion serialized through both durable files.
        let state = self.state.write();
        self.persist_state(&state, &state)?;
        self.dirty.store(false, Ordering::Release);
        Ok(())
    }

    pub fn persist(&self) {
        if let Err(error) = self.try_persist() {
            self.dirty.store(true, Ordering::Release);
            tracing::warn!(%error, "Failed to persist learning memory");
        }
    }

    pub fn flush_dirty(&self) {
        if self.dirty.load(Ordering::Acquire) {
            self.persist();
        }
    }

    pub fn record_tool_outcome(&self, query: &str, tool: &str, ok: bool, note: impl Into<String>) {
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
        if !self.generation_enabled.load(Ordering::Acquire) {
            return;
        }
        let (mut skill, plugin) = classify_tool(tool);
        if tool == "skill_load" {
            skill = skill_hint.map(|s| s.to_string());
        }
        let mut state = self.state.write();
        if !self.generation_enabled.load(Ordering::Acquire) {
            return;
        }
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
        reinforce_episode(&mut state, &episode);
        state.episodes.push(episode);
        if state.episodes.len() > 500 {
            let drain = state.episodes.len() - 500;
            state.episodes.drain(0..drain);
        }

        drop(state);
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Record a durable task outcome and feed it back into routing weights.
    /// Recent episodes matching the goal receive a smaller reinforcement so
    /// successful paths become easier to rediscover while failures cool them.
    pub fn record_task_outcome(
        &self,
        task_id: &str,
        run_id: &str,
        goal: &str,
        attempt: u32,
        ok: bool,
        note: impl Into<String>,
    ) {
        if !self.generation_enabled.load(Ordering::Acquire) {
            return;
        }
        let note = note.into();
        let mut state = self.state.write();
        if !self.generation_enabled.load(Ordering::Acquire) {
            return;
        }
        let recalled = if self.retrieval_enabled.load(Ordering::Acquire) {
            recall_episodes(&state.episodes, goal, 8)
        } else {
            Vec::new()
        };
        let feedback = LearnFeedback {
            id: Uuid::new_v4().to_string(),
            at: Utc::now(),
            task_id: task_id.to_string(),
            run_id: run_id.to_string(),
            goal: goal.chars().take(240).collect(),
            attempt: attempt.max(1),
            ok,
            note: note.chars().take(500).collect(),
            recalled_episode_ids: Some(recalled.iter().map(|episode| episode.id.clone()).collect()),
        };
        reinforce_feedback(&mut state.weights, &feedback, &recalled);
        state.feedback.push(feedback);
        if state.feedback.len() > 500 {
            let drain = state.feedback.len() - 500;
            state.feedback.drain(0..drain);
        }
        drop(state);
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn feedback(&self) -> Vec<LearnFeedback> {
        self.state.read().feedback.clone()
    }

    pub fn weights(&self) -> HashMap<String, f32> {
        if !self.retrieval_enabled.load(Ordering::Acquire) {
            return HashMap::new();
        }
        self.state.read().weights.clone()
    }

    pub fn recall(&self, query: &str, limit: usize) -> Vec<LearnEpisode> {
        if !self.retrieval_enabled.load(Ordering::Acquire) {
            return Vec::new();
        }
        recall_episodes(&self.state.read().episodes, query, limit)
    }

    pub fn sync_priors(&self) -> HashMap<String, u32> {
        if !self.retrieval_enabled.load(Ordering::Acquire) {
            return HashMap::new();
        }
        self.state.read().sync_pairs.clone()
    }

    pub fn prompt_section(&self, query: &str) -> String {
        if !self.retrieval_enabled.load(Ordering::Acquire) {
            return String::new();
        }
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

fn recall_episodes(episodes: &[LearnEpisode], query: &str, limit: usize) -> Vec<LearnEpisode> {
    let limit = limit.clamp(1, 500);
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
    for ep in episodes.iter().rev().take(120) {
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
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored
        .into_iter()
        .take(limit.max(1))
        .map(|(_, e)| e)
        .collect()
}

fn reinforce_episode(state: &mut LearnState, episode: &LearnEpisode) {
    let delta = if episode.ok { 0.35 } else { -0.25 };
    if let Some(skill) = &episode.skill {
        bump(&mut state.weights, &format!("skill:{skill}"), delta);
    }
    if let Some(plugin) = &episode.plugin {
        bump(&mut state.weights, &format!("plugin:{plugin}"), delta);
    }
    bump(
        &mut state.weights,
        &format!("tool:{}", episode.tool),
        delta * 0.5,
    );
    if episode.ok {
        if let (Some(skill), Some(plugin)) = (&episode.skill, &episode.plugin) {
            *state.sync_pairs.entry(sync_key(skill, plugin)).or_insert(0) += 1;
        }
    }
}

fn reinforce_feedback(
    weights: &mut HashMap<String, f32>,
    feedback: &LearnFeedback,
    recalled: &[LearnEpisode],
) {
    let delta = if feedback.ok { 0.5 } else { -0.35 };
    for episode in recalled {
        bump(weights, &format!("tool:{}", episode.tool), delta * 0.5);
        if let Some(skill) = &episode.skill {
            bump(weights, &format!("skill:{skill}"), delta * 0.5);
        }
        if let Some(plugin) = &episode.plugin {
            bump(weights, &format!("plugin:{plugin}"), delta * 0.5);
        }
    }
    for token in feedback
        .goal
        .split_whitespace()
        .map(|token| token.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '_'))
        .filter(|token| token.len() >= 3)
        .take(8)
    {
        bump(weights, &format!("goal:{token}"), delta * 0.25);
    }
}

fn rebuild_derived(state: &mut LearnState) {
    // Replay only retained records in timestamp order. Older aggregate history
    // without retained records cannot survive a user's memory deletion.
    let mut timeline: Vec<_> = state
        .episodes
        .iter()
        .enumerate()
        .map(|(index, record)| (record.at, false, index))
        .chain(
            state
                .feedback
                .iter()
                .enumerate()
                .map(|(index, record)| (record.at, true, index)),
        )
        .collect();
    timeline.sort();
    let mut derived = LearnState::default();
    for (_, feedback, index) in timeline {
        if feedback {
            let record = &state.feedback[index];
            let recalled = match &record.recalled_episode_ids {
                Some(ids) => ids
                    .iter()
                    .filter_map(|id| {
                        derived
                            .episodes
                            .iter()
                            .find(|episode| &episode.id == id)
                            .cloned()
                    })
                    .collect(),
                None => recall_episodes(&derived.episodes, &record.goal, 8),
            };
            reinforce_feedback(&mut derived.weights, record, &recalled);
        } else {
            let record = &state.episodes[index];
            reinforce_episode(&mut derived, record);
            derived.episodes.push(record.clone());
        }
    }
    state.weights = derived.weights;
    state.sync_pairs = derived.sync_pairs;
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

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("dsh-learn-fixture-{}", Uuid::new_v4()));
            fs::create_dir_all(&root).unwrap();
            Self(root.canonicalize().unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            assert_eq!(
                self.0.parent(),
                Some(std::env::temp_dir().canonicalize().unwrap().as_path())
            );
            assert!(self
                .0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("dsh-learn-fixture-"));
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn memory_flags_gate_model_reads_and_generation_independently() {
        let fixture = Fixture::new();
        let store = LearnStore::open(&fixture.0);
        store.record_tool_outcome(
            "remember project",
            "plugin.private.run",
            true,
            "saved memory",
        );
        store.set_enabled(false, false);
        assert!(store.recall("remember project", 5).is_empty());
        assert!(store.weights().is_empty());
        assert!(store.sync_priors().is_empty());
        assert!(store.prompt_section("remember project").is_empty());
        store.record_tool_outcome("discarded", "shell", true, "ignored");
        store.record_task_outcome("task", "run", "discarded", 1, true, "ignored");
        assert_eq!(store.list().episodes.len(), 1);
        assert!(store.list().feedback.is_empty());
        store.set_enabled(false, true);
        store.record_task_outcome(
            "task",
            "run",
            "new outcome",
            1,
            true,
            "recorded without recall",
        );
        assert_eq!(store.feedback()[0].recalled_episode_ids, Some(vec![]));
        store.set_enabled(true, false);
        assert_eq!(store.recall("remember project", 5).len(), 1);
        assert!(!store.weights().is_empty());
    }

    #[test]
    fn deleting_records_removes_derived_influence_and_clear_is_durable() {
        let fixture = Fixture::new();
        let store = LearnStore::open(&fixture.0);
        store.record_tool_outcome("deploy private", "plugin.private.run", true, "remove this");
        let removed = store.list().episodes[0].id.clone();
        store.record_tool_outcome("deploy kept", "read_file", true, "keep this");
        store.record_task_outcome("task", "run", "deploy outcome", 1, true, "verified");
        let feedback = store.feedback()[0].id.clone();
        store.try_persist().unwrap();
        assert!(store.delete_episode(&removed).unwrap());
        assert!(!store.delete_episode(&removed).unwrap());
        let after = store.list();
        assert_eq!(after.episodes.len(), 1);
        assert!(!after.weights.keys().any(|key| key.contains("private")));
        assert!(!after.feedback[0]
            .recalled_episode_ids
            .as_ref()
            .unwrap()
            .contains(&removed));
        let reopened = LearnStore::open(&fixture.0);
        assert_eq!(reopened.list().episodes.len(), 1);
        let cached: HashMap<String, f32> =
            serde_json::from_slice(&fs::read(fixture.0.join("meta/learn-weights.json")).unwrap())
                .unwrap();
        assert_eq!(cached, after.weights);
        assert!(store.delete_feedback(&feedback).unwrap());
        assert!(!store.weights().contains_key("goal:outcome"));
        store.clear().unwrap();
        let empty = LearnStore::open(&fixture.0).list();
        assert!(
            empty.episodes.is_empty()
                && empty.feedback.is_empty()
                && empty.weights.is_empty()
                && empty.sync_pairs.is_empty()
        );
        assert_eq!(
            fs::read_to_string(fixture.0.join("meta/learn-weights.json")).unwrap(),
            "{}"
        );
    }

    #[test]
    fn failed_deletion_keeps_live_records_and_restores_weight_cache() {
        let fixture = Fixture::new();
        let store = LearnStore::open(&fixture.0);
        store.record_tool_outcome("retain me", "shell", true, "still present");
        store.try_persist().unwrap();
        let before = store.list();
        fs::rename(&store.path, fixture.0.join("saved-state.json")).unwrap();
        fs::create_dir(&store.path).unwrap();
        assert!(store.delete_episode(&before.episodes[0].id).is_err());
        assert_eq!(store.list().episodes.len(), 1);
        let cached: HashMap<String, f32> =
            serde_json::from_slice(&fs::read(fixture.0.join("meta/learn-weights.json")).unwrap())
                .unwrap();
        assert_eq!(cached, before.weights);
        assert!(store.clear().is_err());
        fs::remove_dir(&store.path).unwrap();
        fs::rename(fixture.0.join("saved-state.json"), &store.path).unwrap();
        let cache = fixture.0.join("meta/learn-weights.json");
        fs::remove_file(&cache).unwrap();
        fs::create_dir(&cache).unwrap();
        assert!(store.clear().is_err());
        let persisted: LearnState =
            serde_json::from_slice(&fs::read(&store.path).unwrap()).unwrap();
        assert_eq!(persisted.episodes.len(), 1);
        assert_eq!(store.list().episodes.len(), 1);
    }

    #[test]
    fn task_feedback_is_durable_and_reinforces_matching_paths() {
        let root = std::env::temp_dir().join(format!("dsh-learn-{}", Uuid::new_v4()));
        let store = LearnStore::open(&root);
        store.record_tool_outcome("ship feature", "skill_load", true, "loaded");
        store.record_tool_outcome_detailed(
            "ship feature",
            "plugin.echo.run",
            Some("release"),
            true,
            "ran",
        );
        let before = store.weights();
        store.record_task_outcome("task-1", "run-1", "ship feature", 1, true, "verified");
        assert_eq!(store.feedback().len(), 1);
        assert!(store.weights().get("goal:ship").copied().unwrap_or(0.0) > 0.0);
        assert!(store.weights().len() >= before.len());
        let _ = fs::remove_dir_all(root);
    }
}
