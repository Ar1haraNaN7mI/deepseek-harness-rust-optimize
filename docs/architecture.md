# dsh-rust architecture

Aligned with [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) concepts (Cordis-like seams, session log, turn/step loop, system-prompt sections, capability tools) while enforcing a hard **core / outer** split. CTM and self-learning are Rust-side control layers on top of that skeleton.

## Official mapping

| Official (`dsh`) | dsh-rust |
|---|---|
| `ctx.sessions` append-only log + `deriveMessages()` | `Session` / `SessionStore` + `derive_messages` |
| `ctx.systemPrompt` section assembly | `SystemPromptBuilder` (cached sections) |
| `ctx.tools` + tools waterfall | `ToolRegistry` + `ToolPipeline` |
| `ctx.agentLoop` / `agent/pre-step` | `AgentLoop` + `prepare_turn_cognition` |
| `llm/stream` → `assistant/chunk*` → `assistant/message` | Stream UI via live `AgentEvent`; durable `AssistantMessage` once assembled |
| `tool/call*` → execute → `tool/result*` | Same durable events; persist debounced |
| Capability seams (`fs`, LLM adapter) | `dsh-fs` PathGuard, `dsh-llm` DeepSeek V4 |

**Model-visible means logged.** Chunks are live/UI fidelity only and are stripped from disk snapshots; the durable assistant message + tools reconstruct model history.

## Layers

```text
┌─────────────────────────────────────────────┐
│ TUI / CLI (live AgentEvent consumer)        │
├─────────────────────────────────────────────┤
│ agent/pre-step: CTM + single-pass routing   │
├─────────────────────────────────────────────┤
│ Outer (writable): plugins / skills / learn  │
├─────────────────────────────────────────────┤
│ Core: loop · session · tools · LLM · guard  │
└─────────────────────────────────────────────┘
```

## Turn flow (synced with official)

```text
turn/start
  claim user/message (durable)
  agent/pre-step ≈ prepare_turn_cognition
    ensure CTM channels → think → boost weights → rank once
    batch system-prompt sections (skills/plugins/learn/ctm)
  step/start
    assemble prompt + cached tool schemas
    llm/stream → live TextDelta* (no per-token session write)
    assistant/message (durable)
    tool/call* → execute → learn(dirty) → ctm.observe → tool/result*
    step/end → flush dirty session/learn
  … more steps if tools owe another request
turn/end → persist_now
```

## Latency / load controls (without slowing stream)

| Hotspot | Mitigation |
|---|---|
| Double skill/plugin rank before TTFB | Single rank after CTM boosts |
| Per-token session lock + UUID + pretty JSON | Live events only; durable message once |
| Persist after every tool | Dirty mark + flush between steps / turn end |
| Learn pretty-write every outcome | Dirty flag + `flush_dirty` |
| CTM O(N²) sync every tick | Sync once at end on top-K channels; HashSet boosts; ring history |
| Tool schema rebuild every step | Generation-cached definitions |
| Skill DF / plugin JSON+WalkDir rank | Cached DF; `routing_summaries()` |
| TUI redraw every 50ms | Dirty-flag redraw; coalesce stream events |

## Skills / Plugins

1. CTM recommendations boost in-memory learn weights for the turn  
2. One BM25-ish `skill` / lightweight `plugin` rank  
3. `skill_load` progressive body (mtime cache)  
4. Plugin `skills[]` mounted as `SkillSource::Plugin`  
5. Atomic install + Rhai validate + notify hot-reload (scoped)

## CTM (software analogue)

Internal ticks, private NLM-style updates, dual sync latents, adaptive halt on confidence ∧ low entropy. Not a trained neural CTM.

## Crates

| Crate | Role |
|---|---|
| `dsh-core` | Session, agent loop, CTM, learn, system prompt |
| `dsh-llm` | DeepSeek V4 streaming client |
| `dsh-tools` | Tool registry + pipeline (generation cache) |
| `dsh-fs` | FS + PathGuard |
| `dsh-skill` | Dual-format skills + router + DF cache |
| `dsh-plugin` | Outer plugins + Rhai host + hot-reload |
| `dsh-tui` | Ratatui UI (dirty redraw) |
| `dsh-cli` | `dsh` binary |
