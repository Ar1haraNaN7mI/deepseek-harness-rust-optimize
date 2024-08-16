//! CTM-inspired Continuous Thought controller (optimized).
//!
//! Closer software mapping of Continuous Thought Machines:
//! - Internal ticks with adaptive halt (confidence + entropy)
//! - Synapse: merge observation + query attention into pre-activations
//! - Private NLM-style update per neuron kind (skill/plugin/builtin/meta)
//! - Dual sync latents: action (routing) and output (prompt focus)
//! - Inactive-channel decay keeps dynamics from saturating

use crate::learn::LearnStore;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtmConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_max_ticks")]
    pub max_ticks: usize,
    #[serde(default = "default_min_ticks")]
    pub min_ticks: usize,
    #[serde(default = "default_history_len")]
    pub history_len: usize,
    #[serde(default = "default_halt")]
    pub halt_confidence: f32,
    #[serde(default = "default_entropy_halt")]
    pub halt_entropy: f32,
    #[serde(default = "default_decay")]
    pub inactive_decay: f32,
}

fn default_true() -> bool {
    true
}
fn default_max_ticks() -> usize {
    6
}
fn default_min_ticks() -> usize {
    2
}
fn default_history_len() -> usize {
    12
}
fn default_halt() -> f32 {
    0.78
}
fn default_entropy_halt() -> f32 {
    1.15
}
fn default_decay() -> f32 {
    0.92
}

impl Default for CtmConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_ticks: 6,
            min_ticks: 2,
            history_len: 12,
            halt_confidence: 0.78,
            halt_entropy: 1.15,
            inactive_decay: 0.92,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CapabilityNeuron {
    pub id: String,
    pub kind: NeuronKind,
    /// Pre-activation history A_t (incoming signals).
    pub pre_history: Vec<f32>,
    /// Post-activation history Z_t (NLM outputs).
    pub post_history: Vec<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeuronKind {
    Skill,
    Plugin,
    Builtin,
    Meta,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThoughtTick {
    pub t: usize,
    pub focus: Vec<String>,
    pub confidence: f32,
    pub entropy: f32,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThoughtSnapshot {
    pub ticks: Vec<ThoughtTick>,
    pub halted_early: bool,
    pub sync_action: Vec<(String, String, f32)>,
    pub sync_output: Vec<(String, String, f32)>,
    pub recommended_skills: Vec<String>,
    pub recommended_plugins: Vec<String>,
    pub recommended_builtins: Vec<String>,
    pub prompt_section: String,
}

pub struct ContinuousThought {
    config: CtmConfig,
    neurons: RwLock<Vec<CapabilityNeuron>>,
    learn: Arc<LearnStore>,
    /// Last observation vector used as synapse input o_t.
    last_observation: RwLock<HashMap<String, f32>>,
}

impl ContinuousThought {
    pub fn new(config: CtmConfig, learn: Arc<LearnStore>) -> Self {
        let mut neurons = vec![
            neuron("meta.route", NeuronKind::Meta, 0.25),
            neuron("meta.learn", NeuronKind::Meta, 0.15),
            neuron("builtin.fs", NeuronKind::Builtin, 0.12),
            neuron("builtin.search", NeuronKind::Builtin, 0.12),
            neuron("builtin.shell", NeuronKind::Builtin, 0.1),
            neuron("builtin.web", NeuronKind::Builtin, 0.08),
            neuron("builtin.todo", NeuronKind::Builtin, 0.08),
        ];
        for (k, w) in learn.weights() {
            if let Some(id) = k.strip_prefix("skill:") {
                neurons.push(neuron(
                    &format!("skill.{id}"),
                    NeuronKind::Skill,
                    (w / 10.0).clamp(0.0, 1.0),
                ));
            } else if let Some(id) = k.strip_prefix("plugin:") {
                neurons.push(neuron(
                    &format!("plugin.{id}"),
                    NeuronKind::Plugin,
                    (w / 10.0).clamp(0.0, 1.0),
                ));
            }
        }
        Self {
            config,
            neurons: RwLock::new(neurons),
            learn,
            last_observation: RwLock::new(HashMap::new()),
        }
    }

    pub fn ensure_channels(&self, skill_names: &[String], plugin_ids: &[String]) {
        let mut neurons = self.neurons.write();
        for s in skill_names {
            let id = format!("skill.{s}");
            if !neurons.iter().any(|n| n.id == id) {
                neurons.push(neuron(&id, NeuronKind::Skill, 0.05));
            }
        }
        for p in plugin_ids {
            let id = format!("plugin.{p}");
            if !neurons.iter().any(|n| n.id == id) {
                neurons.push(neuron(&id, NeuronKind::Plugin, 0.05));
            }
        }
    }

    pub fn think(
        &self,
        query: &str,
        ranked_skills: &[String],
        ranked_plugins: &[String],
    ) -> ThoughtSnapshot {
        if !self.config.enabled {
            return ThoughtSnapshot {
                ticks: vec![],
                halted_early: true,
                sync_action: vec![],
                sync_output: vec![],
                recommended_skills: ranked_skills.to_vec(),
                recommended_plugins: ranked_plugins.to_vec(),
                recommended_builtins: default_builtins_for(query),
                prompt_section: String::new(),
            };
        }

        let complexity = estimate_complexity(query);
        let budget = ((self.config.min_ticks as f32)
            + complexity * (self.config.max_ticks - self.config.min_ticks) as f32)
            .round() as usize;
        let budget = budget.clamp(self.config.min_ticks, self.config.max_ticks);
        let query_attn = query_attention(query);

        // Precompute ranked id sets — avoid format! in the hot neuron×tick loop.
        let skill_boost: HashMap<String, f32> = ranked_skills
            .iter()
            .enumerate()
            .map(|(i, s)| (format!("skill.{s}"), 0.28 * (1.0 - i as f32 * 0.05)))
            .collect();
        let plugin_boost: HashMap<String, f32> = ranked_plugins
            .iter()
            .enumerate()
            .map(|(i, p)| (format!("plugin.{p}"), 0.28 * (1.0 - i as f32 * 0.05)))
            .collect();
        let skill_set: std::collections::HashSet<&str> =
            skill_boost.keys().map(|s| s.as_str()).collect();
        let plugin_set: std::collections::HashSet<&str> =
            plugin_boost.keys().map(|s| s.as_str()).collect();

        let mut ticks = Vec::with_capacity(budget);
        let mut halted_early = false;
        let obs_snapshot = self.last_observation.read().clone();

        for t in 1..=budget {
            {
                let mut neurons = self.neurons.write();
                for n in neurons.iter_mut() {
                    let z = *n.post_history.last().unwrap_or(&0.05);
                    let o = obs_snapshot.get(&n.id).copied().unwrap_or(0.0);
                    let mut pre = 0.55 * z + 0.35 * o;

                    if let Some(b) = skill_boost.get(&n.id) {
                        pre += *b;
                    } else if let Some(b) = plugin_boost.get(&n.id) {
                        pre += *b;
                    }

                    let attn = attention_for(&n.id, n.kind, &query_attn);
                    pre += attn;

                    if attn < 0.05
                        && !skill_set.contains(n.id.as_str())
                        && !plugin_set.contains(n.id.as_str())
                        && o < 0.05
                    {
                        pre *= self.config.inactive_decay;
                    }

                    push_hist(&mut n.pre_history, pre, self.config.history_len);
                    let post = nlm_update(n.kind, &n.pre_history);
                    push_hist(&mut n.post_history, post, self.config.history_len);
                }
            }

            let focus = self.top_neurons(5);
            let probs = softmax_acts(&focus);
            let confidence = probs.first().copied().unwrap_or(0.0);
            let entropy = shannon_entropy(&probs);
            let note = format!(
                "t={t} H={entropy:.2} conf={confidence:.2} focus={}",
                focus
                    .iter()
                    .zip(probs.iter())
                    .map(|((id, _), p)| format!("{id}:{p:.2}"))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            ticks.push(ThoughtTick {
                t,
                focus: focus.into_iter().map(|(id, _)| id).collect(),
                confidence,
                entropy,
                note,
            });

            let halt = t >= self.config.min_ticks
                && confidence >= self.config.halt_confidence
                && entropy <= self.config.halt_entropy;
            if halt {
                halted_early = true;
                break;
            }
        }

        // Sync once at end (not per tick) — sample top-K to keep O(K²) small.
        let sync_action = self.synchronization(SyncKind::Action);
        let sync_output = self.synchronization(SyncKind::Output);
        let (rec_skills, rec_plugins, rec_builtins) = self.recommendations();
        let prompt_section = self.render_prompt(
            &ticks,
            &sync_action,
            &sync_output,
            &rec_skills,
            &rec_plugins,
            &rec_builtins,
            halted_early,
        );
        ThoughtSnapshot {
            ticks,
            halted_early,
            sync_action,
            sync_output,
            recommended_skills: rec_skills,
            recommended_plugins: rec_plugins,
            recommended_builtins: rec_builtins,
            prompt_section,
        }
    }

    pub fn observe_tool(&self, tool: &str, ok: bool) {
        let boost = if ok { 0.22 } else { -0.12 };
        let mut obs = self.last_observation.write();
        obs.clear();

        let mut neurons = self.neurons.write();
        for n in neurons.iter_mut() {
            let hit = tool_hits_neuron(tool, &n.id, n.kind);
            if hit {
                let last = *n.post_history.last().unwrap_or(&0.1);
                let next = (last + boost).clamp(0.0, 1.0);
                push_hist(&mut n.post_history, next, self.config.history_len);
                obs.insert(n.id.clone(), next);
            }
        }

        // Write soft sync prior when skill_load + later plugin succeed in close succession
        // (learn store already records detailed episodes).
        let _ = &self.learn;
    }

    pub fn observe_skill_load(&self, skill_name: &str, ok: bool) {
        self.observe_tool(&format!("skill_load:{skill_name}"), ok);
        let mut neurons = self.neurons.write();
        let id = format!("skill.{skill_name}");
        if let Some(n) = neurons.iter_mut().find(|n| n.id == id) {
            let last = *n.post_history.last().unwrap_or(&0.1);
            let next = (last + if ok { 0.3 } else { -0.15 }).clamp(0.0, 1.0);
            push_hist(&mut n.post_history, next, self.config.history_len);
        } else if ok {
            neurons.push(neuron(&id, NeuronKind::Skill, 0.35));
        }
    }

    fn recommendations(&self) -> (Vec<String>, Vec<String>, Vec<String>) {
        let top = self.top_neurons(8);
        let mut skills = Vec::new();
        let mut plugins = Vec::new();
        let mut builtins = Vec::new();
        for (id, _) in top {
            if let Some(s) = id.strip_prefix("skill.") {
                skills.push(s.to_string());
            } else if let Some(p) = id.strip_prefix("plugin.") {
                plugins.push(p.to_string());
            } else if id.starts_with("builtin.") {
                builtins.push(id);
            }
        }
        (skills, plugins, builtins)
    }

    fn top_neurons(&self, k: usize) -> Vec<(String, f32)> {
        let mut v: Vec<_> = self
            .neurons
            .read()
            .iter()
            .map(|n| (n.id.clone(), *n.post_history.last().unwrap_or(&0.0)))
            .collect();
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        v.truncate(k);
        v
    }

    fn synchronization(&self, kind: SyncKind) -> Vec<(String, String, f32)> {
        // Only correlate the strongest channels (cap K) to avoid O(N²) blow-ups.
        let mut candidates: Vec<(String, NeuronKind, Vec<f32>, f32)> = self
            .neurons
            .read()
            .iter()
            .filter(|n| match kind {
                SyncKind::Action => {
                    matches!(n.kind, NeuronKind::Skill | NeuronKind::Plugin | NeuronKind::Meta)
                }
                SyncKind::Output => !matches!(n.kind, NeuronKind::Meta),
            })
            .map(|n| {
                let last = *n.post_history.last().unwrap_or(&0.0);
                (n.id.clone(), n.kind, n.post_history.clone(), last)
            })
            .collect();
        candidates.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
        candidates.truncate(12);

        let mut pairs = Vec::new();
        for i in 0..candidates.len() {
            for j in (i + 1)..candidates.len() {
                let c = corr(&candidates[i].2, &candidates[j].2);
                if c > 0.12 {
                    pairs.push((candidates[i].0.clone(), candidates[j].0.clone(), c));
                }
            }
        }

        if matches!(kind, SyncKind::Action) {
            for (key, count) in self.learn.sync_priors() {
                let mut parts = key.split('|');
                if let (Some(a), Some(b)) = (parts.next(), parts.next()) {
                    let bonus = (count as f32 / 8.0).min(0.55);
                    pairs.push((format!("skill.{a}"), format!("plugin.{b}"), 0.25 + bonus));
                }
            }
        }

        pairs.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        pairs.truncate(8);
        pairs
    }

    fn render_prompt(
        &self,
        ticks: &[ThoughtTick],
        sync_action: &[(String, String, f32)],
        sync_output: &[(String, String, f32)],
        skills: &[String],
        plugins: &[String],
        builtins: &[String],
        halted_early: bool,
    ) -> String {
        let mut out = String::from("## continuous_thought (CTM)\n");
        out.push_str(&format!(
            "ticks={} halted_early={halted_early}. Prefer focus channels; avoid unrelated tools.\n",
            ticks.len()
        ));
        if let Some(last) = ticks.last() {
            out.push_str(&format!(
                "Focus: {} | confidence={:.2} entropy={:.2}\n",
                last.focus.join(", "),
                last.confidence,
                last.entropy
            ));
        }
        if !skills.is_empty() {
            out.push_str(&format!(
                "Recommended skills (load first): {}\n",
                skills.join(", ")
            ));
        }
        if !plugins.is_empty() {
            out.push_str(&format!(
                "Recommended plugins: {}\n",
                plugins
                    .iter()
                    .map(|p| format!("plugin.{p}.*"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !builtins.is_empty() {
            out.push_str(&format!("Recommended builtins: {}\n", builtins.join(", ")));
        }
        if !sync_action.is_empty() {
            out.push_str("Action-sync (routing):\n");
            for (a, b, c) in sync_action.iter().take(3) {
                out.push_str(&format!("- {a} ↔ {b} ({c:.2})\n"));
            }
        }
        if !sync_output.is_empty() {
            out.push_str("Output-sync:\n");
            for (a, b, c) in sync_output.iter().take(2) {
                out.push_str(&format!("- {a} ↔ {b} ({c:.2})\n"));
            }
        }
        out.push_str(
            "Policy: skill_recommend/skill_load → act; plugin_search before plugin.*; grep/glob before broad shell.\n",
        );
        out
    }
}

#[derive(Clone, Copy)]
enum SyncKind {
    Action,
    Output,
}

fn neuron(id: &str, kind: NeuronKind, init: f32) -> CapabilityNeuron {
    CapabilityNeuron {
        id: id.to_string(),
        kind,
        pre_history: vec![init],
        post_history: vec![init],
    }
}

fn push_hist(hist: &mut Vec<f32>, v: f32, max: usize) {
    let v = v.clamp(0.0, 1.0);
    if hist.len() < max {
        hist.push(v);
        return;
    }
    // Ring-style shift without per-element remove(0) realloc churn when at capacity.
    if max == 0 {
        return;
    }
    hist.copy_within(1.., 0);
    let last = hist.len() - 1;
    hist[last] = v;
}

fn nlm_update(kind: NeuronKind, pre: &[f32]) -> f32 {
    let n = pre.len().min(8);
    if n == 0 {
        return 0.05;
    }
    let window = &pre[pre.len() - n..];
    let mean = window.iter().sum::<f32>() / n as f32;
    let last = *window.last().unwrap_or(&mean);
    let trend = last - window.first().copied().unwrap_or(last);
    let (a, b, c) = match kind {
        NeuronKind::Skill => (0.55, 0.30, 0.15),
        NeuronKind::Plugin => (0.50, 0.35, 0.15),
        NeuronKind::Builtin => (0.45, 0.40, 0.15),
        NeuronKind::Meta => (0.40, 0.25, 0.35),
    };
    // Squashing NLM
    let raw = a * last + b * mean + c * (0.5 + trend);
    (1.0 / (1.0 + (-4.0 * (raw - 0.5)).exp())).clamp(0.0, 1.0)
}

struct QueryAttn {
    tokens: Vec<String>,
    wants_search: bool,
    wants_files: bool,
    wants_shell: bool,
    wants_web: bool,
    wants_plugin: bool,
    wants_skill: bool,
}

fn query_attention(query: &str) -> QueryAttn {
    let lower = query.to_lowercase();
    let tokens = lower
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .filter(|t| t.len() > 1)
        .map(|t| t.to_string())
        .collect();
    QueryAttn {
        wants_search: contains_any(&lower, &["find", "search", "grep", "where", "locate"]),
        wants_files: contains_any(&lower, &["file", "read", "edit", "patch", "refactor", "code"]),
        wants_shell: contains_any(&lower, &["run", "build", "test", "cargo", "npm", "shell"]),
        wants_web: contains_any(&lower, &["http", "url", "docs", "web", "fetch", "api"]),
        wants_plugin: contains_any(&lower, &["plugin", "extend", "outer"]),
        wants_skill: contains_any(&lower, &["skill", "workflow", "how to", "playbook"]),
        tokens,
    }
}

fn attention_for(id: &str, kind: NeuronKind, q: &QueryAttn) -> f32 {
    let mut a: f32 = 0.0;
    match kind {
        NeuronKind::Builtin if id == "builtin.search" && q.wants_search => a += 0.35,
        NeuronKind::Builtin if id == "builtin.fs" && q.wants_files => a += 0.3,
        NeuronKind::Builtin if id == "builtin.shell" && q.wants_shell => a += 0.3,
        NeuronKind::Builtin if id == "builtin.web" && q.wants_web => a += 0.35,
        NeuronKind::Builtin if id == "builtin.todo" && q.tokens.iter().any(|t| t == "plan" || t == "todo") => {
            a += 0.25
        }
        NeuronKind::Plugin if q.wants_plugin => a += 0.2,
        NeuronKind::Skill if q.wants_skill => a += 0.2,
        NeuronKind::Meta if id == "meta.route" => a += 0.1,
        _ => {}
    }
    for t in &q.tokens {
        if t.len() > 2 && id.to_lowercase().contains(t) {
            a += 0.12;
        }
    }
    a.min(0.55)
}

fn tool_hits_neuron(tool: &str, id: &str, kind: NeuronKind) -> bool {
    if let Some(rest) = tool.strip_prefix("skill_load:") {
        return id == format!("skill.{rest}");
    }
    match kind {
        NeuronKind::Plugin => id
            .strip_prefix("plugin.")
            .map(|p| tool.starts_with(&format!("plugin.{p}.")))
            .unwrap_or(false),
        NeuronKind::Skill => {
            tool.starts_with("skill_") || tool.starts_with(&format!("skill_load"))
        }
        NeuronKind::Builtin => match id {
            "builtin.fs" => {
                matches!(
                    tool,
                    "read_file"
                        | "write_file"
                        | "edit_file"
                        | "list_dir"
                        | "apply_patch"
                )
            }
            "builtin.search" => matches!(tool, "grep" | "glob"),
            "builtin.shell" => tool == "shell",
            "builtin.web" => tool == "web_fetch",
            "builtin.todo" => tool.starts_with("todo_"),
            _ => false,
        },
        NeuronKind::Meta => tool.starts_with("learn_") || tool.starts_with("skill_search"),
    }
}

fn default_builtins_for(query: &str) -> Vec<String> {
    let q = query_attention(query);
    let mut v = Vec::new();
    if q.wants_search {
        v.push("builtin.search".into());
    }
    if q.wants_files {
        v.push("builtin.fs".into());
    }
    if q.wants_shell {
        v.push("builtin.shell".into());
    }
    if q.wants_web {
        v.push("builtin.web".into());
    }
    if v.is_empty() {
        v.push("builtin.fs".into());
    }
    v
}

fn estimate_complexity(query: &str) -> f32 {
    let len = query.chars().count() as f32;
    let mut c = (len / 160.0).clamp(0.2, 1.0);
    let lower = query.to_lowercase();
    for kw in [
        "refactor",
        "architect",
        "debug",
        "multi",
        "plugin",
        "skill",
        "evolve",
        "optimize",
        "migrate",
        "implement",
        "fix",
    ] {
        if lower.contains(kw) {
            c = (c + 0.1).min(1.0);
        }
    }
    c
}

fn contains_any(hay: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| hay.contains(n))
}

fn corr(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    if n < 2 {
        return 0.0;
    }
    let a = &a[a.len() - n..];
    let b = &b[b.len() - n..];
    let mean_a = a.iter().sum::<f32>() / n as f32;
    let mean_b = b.iter().sum::<f32>() / n as f32;
    let mut num = 0.0;
    let mut da = 0.0;
    let mut db = 0.0;
    for i in 0..n {
        let x = a[i] - mean_a;
        let y = b[i] - mean_b;
        num += x * y;
        da += x * x;
        db += y * y;
    }
    let den = (da * db).sqrt();
    if den < 1e-6 {
        0.0
    } else {
        (num / den).clamp(0.0, 1.0)
    }
}

fn softmax_acts(focus: &[(String, f32)]) -> Vec<f32> {
    if focus.is_empty() {
        return vec![];
    }
    let max = focus
        .iter()
        .map(|(_, a)| *a)
        .fold(f32::NEG_INFINITY, f32::max);
    let exps: Vec<f32> = focus.iter().map(|(_, a)| (a - max).exp()).collect();
    let sum: f32 = exps.iter().sum::<f32>().max(1e-6);
    exps.into_iter().map(|e| e / sum).collect()
}

fn shannon_entropy(probs: &[f32]) -> f32 {
    let mut h = 0.0;
    for p in probs {
        if *p > 1e-8 {
            h -= p * p.ln();
        }
    }
    h
}
