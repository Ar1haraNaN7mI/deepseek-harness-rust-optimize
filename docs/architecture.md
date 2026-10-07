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
| Durable app control / event cursor | `dsh-cli` app-server `events/read_after` + `events/wait`, `dsh-app-client::EventCursor` |

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

Long-running consumers use the durable sequence cursor rather than keeping
process-local state: `events/read_after` returns replayable batches,
`events/wait` long-polls for the next batch, and `dsh events --follow` provides
the local CLI equivalent. A restart resumes from the last delivered sequence.

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
| `dsh-llm` | OpenAI-compatible streaming client and backend presets |
| `dsh-tools` | Tool registry + pipeline (generation cache) |
| `dsh-fs` | FS + PathGuard |
| `dsh-skill` | Dual-format skills + router + DF cache |
| `dsh-plugin` | Outer plugins + Rhai host + hot-reload |
| `dsh-tui` | Ratatui UI (dirty redraw) |
| `dsh-cli` | `dsh` binary |
| `dsh-protocol` | Versioned task/run/checkpoint/event contracts |
| `dsh-app-client` | Async JSON-RPC client over TCP/stdio with durable event cursor |

`dsh-core::model_profile` adds a provider-independent `<70B` policy layer:
model-size inference, bounded/compacted tool schemas, context projection,
compact tool results, serialized tool calls, and per-request token/thinking
overrides. `dsh-llm` keeps the same
OpenAI-compatible wire contract for DeepSeek, Ollama, llama.cpp, LM Studio, vLLM,
SGLang, LiteLLM, LocalAI, TGI, MLX-LM, and generic compatible servers. Ordered endpoint
fallback and provider-specific request fields stay in `dsh-llm`, while the
full tool schema remains in core for preflight validation; see
`docs/small-models.md`.

## Cloud artifacts and remote recovery

Cloud artifacts are versioned, content-digested patch documents stored under
the outer layer. The same `CloudProvider` boundary serves the local provider
and `RemoteCloudProvider`; applying an artifact always goes through the local
`PathGuard`, permission mode, and audit event log.

Before applying, the local side compares the artifact's canonical workspace
binding with the target workspace. A valid content digest alone does not
authorize applying a patch to a different checkout; import it in that checkout
first when using a remote provider.

The app-server exposes `cloud/list`, `cloud/get`, and `cloud/import`. Remote
clients resolve `cloud/get` by a restricted artifact id only, never by an
arbitrary server filesystem path. `dsh cloud ... --server HOST:PORT` and
`dsh apply ... --server HOST:PORT` use this provider boundary without moving
the caller's workspace write authority to the server.

`ReconnectingTcpAppServerClient` re-runs `initialize` after a transport drop
and retries only idempotent reads (`ping`, task/event reads, approvals/tools
lists, and cloud reads). Requests that can create work, resolve approval,
import artifacts, or otherwise mutate state are surfaced after a failed
transport instead of being blindly replayed. `EventCursor` advances only to
the last delivered sequence, so a daemon can reconnect or restart without
skipping durable events.

## Authorized security-research mode

The default model prompt treats in-scope security research as ordinary technical
work and suppresses generic refusal/legal boilerplate. This is a prompt-level
presentation setting only: permissions, approvals, sandbox, PathGuard, and the
append-only audit log remain authoritative. Toggle it with
`dsh config security-research on|off` or TUI `/security-research on|off`.
