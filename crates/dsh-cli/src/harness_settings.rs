//! Settings and conversation management for the authenticated control plane.
//! Read responses deliberately whitelist runtime metadata, never credentials.

use crate::app_server::{resolve_session, RpcFailure, SessionTurnGuard};
use anyhow::{Context, Result};
use dsh_core::{
    EventStore, McpTransport, Runtime, Session, SessionEvent, SettingsPatch, TaskState,
};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

type RpcResult = std::result::Result<Value, RpcFailure>;

pub(crate) fn dispatch(runtime: &Arc<Runtime>, method: &str, params: &Value) -> Option<RpcResult> {
    Some(match method {
        "settings/get" => snapshot(runtime).map_err(internal),
        "settings/update" => update_settings(runtime, params),
        "memory/list" => {
            let state = runtime.learn.list();
            Ok(
                json!({"total":state.episodes.len(),"feedback_total":state.feedback.len(),
                "memories":state.episodes,"feedback":state.feedback}),
            )
        }
        "memory/delete" => delete_memory(runtime, params),
        "memory/clear" => {
            let before = runtime.learn.list();
            runtime.learn.clear().map_err(internal).map(|()| {
                clear_memory_prompt(runtime);
                json!({"deleted_episodes":before.episodes.len(),"deleted_feedback":before.feedback.len()})
            })
        }
        "sessions/archive" | "sessions/unarchive" | "sessions/delete" | "sessions/rename" => {
            mutate_session(runtime, method, params)
        }
        "sessions/archive_all" | "sessions/delete_all" => mutate_all(runtime, method),
        "sessions/export" => export_sessions(runtime, params),
        _ => return None,
    })
}

fn internal(error: impl std::fmt::Display) -> RpcFailure {
    RpcFailure::internal(error.to_string())
}

fn required_string<'a>(params: &'a Value, key: &str) -> std::result::Result<&'a str, RpcFailure> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| RpcFailure::invalid_params(format!("{key} must be a nonempty string")))
}

fn update_settings(runtime: &Runtime, params: &Value) -> RpcResult {
    let patch = params
        .get("patch")
        .filter(|value| value.is_object())
        .ok_or_else(|| RpcFailure::invalid_params("patch must be an object"))?;
    let patch: SettingsPatch = serde_json::from_value(patch.clone())
        .map_err(|error| RpcFailure::invalid_params(error.to_string()))?;
    runtime.update_settings(patch).map_err(internal)?;
    snapshot(runtime).map_err(internal)
}

fn delete_memory(runtime: &Runtime, params: &Value) -> RpcResult {
    let id = required_string(params, "id")?;
    let kind = match params.get("kind") {
        None => "episode",
        Some(Value::String(value)) if value == "episode" || value == "feedback" => value,
        _ => {
            return Err(RpcFailure::invalid_params(
                "kind must be episode or feedback",
            ))
        }
    };
    let deleted = if kind == "episode" {
        runtime.learn.delete_episode(id)
    } else {
        runtime.learn.delete_feedback(id)
    }
    .map_err(internal)?;
    if deleted {
        clear_memory_prompt(runtime);
    }
    Ok(json!({"deleted":deleted,"id":id,"kind":kind}))
}

fn clear_memory_prompt(runtime: &Runtime) {
    let mut prompt = runtime.prompt.write();
    prompt.set_section("learn", "");
    prompt.set_section("continuous_thought", "");
}

/// No API key, endpoint URL, MCP command, environment or request header is
/// included. These can embed secrets even when the outer config is harmless.
pub(crate) fn snapshot(runtime: &Runtime) -> Result<Value> {
    let settings = runtime.settings.read().clone();
    let mut public_settings = serde_json::to_value(&settings)?;
    // Connection details have a dedicated sanitized endpoint. Keep the broad
    // settings response credential-free even if another caller set a raw URL.
    public_settings.as_object_mut().unwrap().remove("base_url");
    let llm = runtime.llm.config();
    let mut session_count = 0_u64;
    let mut archived_count = 0_u64;
    let mut user_message_count = 0_u64;
    let mut assistant_message_count = 0_u64;
    let mut tool_call_count = 0_u64;
    let mut session_bytes = 0_u64;
    for id in runtime.sessions.try_list_ids()? {
        let session = runtime.sessions.get_or_load(&id)?;
        let session = session.read();
        session_count += 1;
        archived_count += u64::from(session.archived);
        for event in &session.events {
            match event {
                SessionEvent::UserMessage { .. } => user_message_count += 1,
                SessionEvent::AssistantMessage { .. } => assistant_message_count += 1,
                SessionEvent::ToolCall { .. } => tool_call_count += 1,
                _ => {}
            }
        }
        session_bytes += file_bytes(
            &runtime
                .outer_home
                .join("sessions")
                .join(format!("{id}.json")),
        )?;
    }
    let memory_bytes = file_bytes(&runtime.outer_home.join("learn/state.json"))?
        + file_bytes(&runtime.outer_home.join("meta/learn-weights.json"))?;
    let event_bytes = file_bytes(&runtime.outer_home.join("events/events.jsonl"))?;
    let mut plugins = runtime
        .plugins
        .read()
        .as_ref()
        .map(|registry| registry.list())
        .unwrap_or_default()
        .into_iter()
        .map(|plugin| {
            json!({
                "id":plugin["id"],"name":plugin["name"],"version":plugin["version"],
                "description":plugin["description"],"enabled":true,
                "tool_count":plugin["tools"].as_array().map_or(0, Vec::len)
            })
        })
        .collect::<Vec<_>>();
    plugins.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
    let servers = runtime.mcp.read().servers.iter().map(|server| json!({
        "name":server.name,"enabled":server.enabled,"status":"configured",
        "transport":match server.transport { McpTransport::Stdio { .. } => "stdio", McpTransport::Http { .. } => "http" }
    })).collect::<Vec<_>>();
    Ok(json!({
        "effective":{"model":llm.model,"thinking":llm.thinking,"backend":llm.backend.label(),
            "permissions":*runtime.permissions.read(),"sandbox":settings.sandbox,"approval":settings.approval,
            "memory_available":runtime.config.learn.enabled,
            "memory_inject":runtime.config.learn.enabled && settings.memory_inject,
            "memory_generate":runtime.config.learn.enabled && settings.memory_generate},
        "settings":public_settings,
        "account":{"kind":"local","credential_configured":runtime.llm.has_api_key(),"model_ready":runtime.llm.is_ready()},
        "storage":{"session_count":session_count,"archived_count":archived_count,
            "session_bytes":session_bytes,"memory_bytes":memory_bytes,"event_bytes":event_bytes,
            "total_bytes":session_bytes+memory_bytes+event_bytes},
        "usage":{"session_count":session_count,"user_message_count":user_message_count,
            "assistant_message_count":assistant_message_count,"message_count":user_message_count+assistant_message_count,
            "tool_call_count":tool_call_count,"event_count":runtime.events.latest_sequence(),"token_usage_available":false},
        "capabilities":{"plugin_toggle":runtime.plugins.read().is_some(),"mcp_connect":false,"cloud_account":false},
        "plugins":plugins,"mcp":{"servers":servers,"connection_supported":false}
    }))
}

fn file_bytes(path: &Path) -> Result<u64> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => {
            Err(error).with_context(|| format!("read storage metadata {}", path.display()))
        }
    }
}

fn mutation_guard(
    runtime: &Arc<Runtime>,
    id: &str,
) -> std::result::Result<SessionTurnGuard, RpcFailure> {
    let guard = SessionTurnGuard::acquire(runtime, id)?;
    // Include every task, not just the newest task belonging to a session.
    // Failed runs with retries remaining are still scheduler-owned work.
    for task in runtime
        .tasks
        .tasks()
        .into_iter()
        .filter(|task| task.session_id.as_deref() == Some(id))
    {
        let pending_retry = task.state == TaskState::Failed
            && runtime
                .tasks
                .latest_run_for_task(&task.id)
                .is_some_and(|run| run.attempt < task.retry_policy.max_attempts);
        if matches!(
            task.state,
            TaskState::Queued
                | TaskState::Running
                | TaskState::WaitingApproval
                | TaskState::WaitingEvent
        ) || pending_retry
            || runtime.is_task_reserved(&task.id)
            || runtime.active_agents.lock().contains_key(&task.id)
        {
            return Err(RpcFailure { code:-32003,
                message:"session has active or scheduled work; pause or cancel it before managing this conversation".into(),
                data:Some(json!({"session_id":id,"task_id":task.id})) });
        }
    }
    // A concurrent delete could have finished before we acquired the lease.
    runtime.sessions.get_or_load(id).map_err(internal)?;
    Ok(guard)
}

fn mutate_session(runtime: &Arc<Runtime>, method: &str, params: &Value) -> RpcResult {
    let session = resolve_session(runtime, required_string(params, "id")?)?;
    let id = session.read().id.clone();
    let _guard = mutation_guard(runtime, &id)?;
    if method == "sessions/rename" {
        let name = required_string(params, "name")?.trim();
        if name.chars().count() > 100 || name.chars().any(char::is_control) {
            return Err(RpcFailure::invalid_params(
                "name must contain 1 to 100 printable characters",
            ));
        }
        runtime.sessions.rename(&id, name).map_err(internal)?;
        Ok(json!({"id":id,"name":name}))
    } else if method == "sessions/delete" {
        runtime.sessions.delete(&id).map_err(internal)?;
        Ok(json!({"id":id,"deleted":true}))
    } else {
        let archived = method == "sessions/archive";
        runtime
            .sessions
            .set_archived(&id, archived)
            .map_err(internal)?;
        Ok(json!({"id":id,"archived":archived}))
    }
}

fn mutate_all(runtime: &Arc<Runtime>, method: &str) -> RpcResult {
    let ids = runtime.sessions.try_list_ids().map_err(internal)?;
    // Reserve and validate the whole selection before the first disk mutation.
    let _guards = ids
        .iter()
        .map(|id| mutation_guard(runtime, id))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut completed = Vec::new();
    for id in &ids {
        let result = if method == "sessions/delete_all" {
            runtime.sessions.delete(id)
        } else {
            runtime.sessions.set_archived(id, true)
        };
        if let Err(error) = result {
            return Err(RpcFailure {
                code: -32603,
                message: error.to_string(),
                data: Some(json!({"completed_ids":completed,"failed_id":id})),
            });
        }
        completed.push(id.clone());
    }
    Ok(json!({"count":completed.len(),"ids":completed}))
}

fn export_sessions(runtime: &Runtime, params: &Value) -> RpcResult {
    let ids = match params.get("id") {
        None | Some(Value::Null) => runtime.sessions.try_list_ids().map_err(internal)?,
        Some(Value::String(id)) if !id.trim().is_empty() => {
            vec![resolve_session(runtime, id)?.read().id.clone()]
        }
        _ => return Err(RpcFailure::invalid_params("id must be a nonempty string")),
    };
    let format = match params.get("format") {
        None => "json",
        Some(Value::String(value)) if value == "json" || value == "markdown" => value,
        _ => {
            return Err(RpcFailure::invalid_params(
                "format must be json or markdown",
            ))
        }
    };
    let mut sessions = Vec::with_capacity(ids.len());
    for id in ids {
        let mut session = runtime
            .sessions
            .get_or_load(&id)
            .map_err(internal)?
            .read()
            .clone();
        session.events.retain(|event| {
            !matches!(
                event,
                SessionEvent::AssistantChunk { .. } | SessionEvent::ReasoningChunk { .. }
            )
        });
        sessions.push(session);
    }
    let now = chrono::Utc::now();
    let (mime, extension, content) = if format == "json" {
        (
            "application/json",
            "json",
            serde_json::to_string_pretty(&json!({
                "schema_version":1,"exported_at":now,"sessions":sessions
            }))
            .map_err(internal)?,
        )
    } else {
        ("text/markdown", "md", markdown_export(&sessions))
    };
    Ok(
        json!({"filename":format!("dsh-conversations-{}.{}",now.format("%Y%m%d-%H%M%S"),extension),
        "mime":mime,"content":content,"session_count":sessions.len()}),
    )
}

fn markdown_export(sessions: &[Session]) -> String {
    let mut output = String::from("# DSH conversations\n\n");
    for session in sessions {
        output.push_str(&format!(
            "## {}\n\nSession: {}\nArchived: {}\n\n",
            session.display_name(),
            session.id,
            session.archived
        ));
        for event in &session.events {
            match event {
                SessionEvent::UserMessage { text, .. } => {
                    output.push_str(&format!("### User\n\n{text}\n\n"))
                }
                SessionEvent::AssistantMessage {
                    text, reasoning, ..
                } => {
                    if let Some(reasoning) = reasoning {
                        output.push_str(&format!("### Assistant reasoning\n\n{reasoning}\n\n"));
                    }
                    output.push_str(&format!("### Assistant\n\n{text}\n\n"));
                }
                SessionEvent::ToolCall {
                    name, arguments, ..
                } => output.push_str(&format!("### Tool call: {name}\n\n{arguments}\n\n")),
                SessionEvent::ToolResult {
                    name, ok, content, ..
                } => output.push_str(&format!("### Tool result: {name} ({ok})\n\n{content}\n\n")),
                SessionEvent::SystemNote { text, .. } => {
                    output.push_str(&format!("### System note\n\n{text}\n\n"))
                }
                _ => {}
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_core::{AppConfig, GoalSpec, SessionStore, TaskRecord};
    use dsh_llm::DeepSeekClient;
    use dsh_tools::ToolRegistry;
    use std::path::PathBuf;

    struct Fixture {
        root: PathBuf,
        runtime: Option<Arc<Runtime>>,
    }
    impl Fixture {
        fn new() -> Self {
            Self::with_learning(true)
        }
        fn with_learning(enabled: bool) -> Self {
            let root =
                std::env::temp_dir().join(format!("dsh-harness-settings-{}", uuid::Uuid::new_v4()));
            let mut config = AppConfig::builtin_default();
            config.paths.outer_home = root.join("outer").display().to_string();
            config.agent.scheduler_enabled = false;
            config.ctm.enabled = false;
            config.learn.enabled = enabled;
            let llm = DeepSeekClient::new(config.to_llm_config("private-test-key".into())).unwrap();
            let runtime =
                Runtime::bootstrap(config, root.clone(), llm, Arc::new(ToolRegistry::new()))
                    .unwrap();
            Self {
                root,
                runtime: Some(runtime),
            }
        }
        fn runtime(&self) -> &Arc<Runtime> {
            self.runtime.as_ref().unwrap()
        }
        fn call(&self, method: &str, params: Value) -> RpcResult {
            dispatch(self.runtime(), method, &params).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.runtime.take();
            let temp = std::env::temp_dir().canonicalize().unwrap();
            let root = self.root.canonicalize().unwrap();
            assert_eq!(root.parent(), Some(temp.as_path()));
            assert!(root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("dsh-harness-settings-"));
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn settings_save_returns_live_persisted_values_without_connection_secrets() {
        let fixture = Fixture::new();
        let runtime = fixture.runtime();
        runtime
            .llm
            .set_base_url("https://private-user:private-password@example.invalid");
        runtime.mcp.write().add_http(
            "configured-example",
            "https://example.invalid?token=private-token",
        );
        let value = fixture.call("settings/update", json!({"patch":{
            "model":"test-model","thinking":false,"custom_instructions":"用中文回答。\n保留事实。",
            "personality":"concise","characteristics":{"warmth":"more"},"memory_inject":false,
            "sandbox":"read-only","approval":"untrusted"
        }})).unwrap();
        assert_eq!(value["effective"]["model"], "test-model");
        assert_eq!(value["effective"]["thinking"], false);
        assert_eq!(value["effective"]["permissions"], "read-only");
        assert_eq!(value["mcp"]["servers"][0]["status"], "configured");
        assert_eq!(value["capabilities"]["plugin_toggle"], false);
        assert!(!value.to_string().contains("private-"));
        let persisted = dsh_core::load_settings(&runtime.outer_home);
        assert_eq!(persisted.custom_instructions, "用中文回答。\n保留事实。");
        assert!(!persisted.memory_inject);
        assert!(fixture
            .call("settings/update", json!({"patch":{"api_key":"forbidden"}}))
            .is_err());
        assert!(fixture
            .call(
                "settings/update",
                json!({"patch":{"personality":"unknown"}})
            )
            .is_err());
    }

    #[tokio::test]
    async fn general_settings_never_serialize_connection_urls() {
        let fixture = Fixture::new();
        fixture.runtime().settings.write().base_url = Some("https://private-user:private-password@example.invalid?key=private-token".into());
        let value = fixture.call("settings/get", json!({})).unwrap();
        assert!(value["settings"].get("base_url").is_none());
        assert!(!value.to_string().contains("private-"));
    }

    #[tokio::test]
    async fn settings_save_failure_does_not_claim_success() {
        let fixture = Fixture::new();
        std::fs::create_dir(fixture.runtime().outer_home.join("settings.toml")).unwrap();
        assert!(fixture
            .call(
                "settings/update",
                json!({"patch":{"custom_instructions":"unsaved"}})
            )
            .is_err());
        assert_eq!(fixture.runtime().settings.read().custom_instructions, "");
    }

    #[tokio::test]
    async fn globally_disabled_memory_is_reported_as_unavailable_despite_saved_preferences() {
        let fixture = Fixture::with_learning(false);
        let value = fixture
            .call(
                "settings/update",
                json!({"patch":{"memory_inject":true,"memory_generate":true}}),
            )
            .unwrap();
        assert_eq!(value["settings"]["memory_inject"], true);
        assert_eq!(value["effective"]["memory_available"], false);
        assert_eq!(value["effective"]["memory_inject"], false);
        assert_eq!(value["effective"]["memory_generate"], false);
        fixture
            .runtime()
            .learn
            .record_tool_outcome("disabled", "read_file", true, "unused");
        assert_eq!(fixture.call("memory/list", json!({})).unwrap()["total"], 0);
    }

    #[tokio::test]
    async fn an_unreadable_session_directory_is_not_reported_as_an_empty_store() {
        let fixture = Fixture::new();
        let directory = fixture.runtime().outer_home.join("sessions");
        std::fs::remove_dir(&directory).unwrap();
        std::fs::write(&directory, "not a directory").unwrap();
        for method in [
            "settings/get",
            "sessions/archive_all",
            "sessions/delete_all",
            "sessions/export",
        ] {
            assert!(fixture.call(method, json!({})).is_err(), "{method}");
        }
    }

    #[tokio::test]
    async fn memory_management_remains_available_with_recall_off_and_persists_deletions() {
        let fixture = Fixture::new();
        let runtime = fixture.runtime();
        runtime
            .learn
            .record_tool_outcome("remember fixture", "read_file", true, "observed");
        runtime
            .learn
            .record_task_outcome("task", "run", "remember fixture", 1, true, "verified");
        runtime.learn.try_persist().unwrap();
        fixture
            .call("settings/update", json!({"patch":{"memory_inject":false}}))
            .unwrap();
        let list = fixture.call("memory/list", json!({})).unwrap();
        assert_eq!(list["total"], 1);
        assert_eq!(list["feedback_total"], 1);
        assert_eq!(list["memories"][0]["note"], "observed");
        assert_eq!(
            fixture
                .call("memory/delete", json!({"id":list["memories"][0]["id"]}))
                .unwrap()["deleted"],
            true
        );
        assert!(dsh_core::LearnStore::open(&runtime.outer_home)
            .list()
            .episodes
            .is_empty());
        let clear = fixture.call("memory/clear", json!({})).unwrap();
        assert_eq!(clear["deleted_feedback"], 1);
        let reopened = dsh_core::LearnStore::open(&runtime.outer_home).list();
        assert!(
            reopened.feedback.is_empty()
                && reopened.weights.is_empty()
                && reopened.sync_pairs.is_empty()
        );
        assert!(fixture
            .call("memory/delete", json!({"id":"missing","kind":"invalid"}))
            .is_err());
    }

    #[tokio::test]
    async fn conversation_management_and_exports_use_full_durable_content() {
        let fixture = Fixture::new();
        let runtime = fixture.runtime();
        let session = runtime.sessions.create();
        let id = session.read().id.clone();
        let at = chrono::Utc::now();
        session.write().append(SessionEvent::UserMessage {
            id: "user".into(),
            text: "完整提问".into(),
            at,
        });
        session.write().append(SessionEvent::AssistantMessage {
            id: "assistant".into(),
            text: "完整回复".into(),
            reasoning: Some("reasoning".into()),
            at,
        });
        session.write().append(SessionEvent::AssistantChunk {
            id: "chunk".into(),
            text: "transient duplicate".into(),
            at,
        });
        session.write().append(SessionEvent::ToolCall {
            id: "call".into(),
            call_id: "call-1".into(),
            name: "read_file".into(),
            arguments: json!({"path":"example"}),
            at,
        });
        let full_result = "long tool result ".repeat(70);
        session.write().append(SessionEvent::ToolResult {
            id: "result".into(),
            call_id: "call-1".into(),
            name: "read_file".into(),
            ok: true,
            content: full_result.clone(),
            at,
        });
        runtime.sessions.persist_now_result(&session).unwrap();
        fixture
            .call("sessions/rename", json!({"id":id,"name":"中文对话"}))
            .unwrap();
        fixture.call("sessions/archive", json!({"id":id})).unwrap();
        let reopened = SessionStore::with_dir(runtime.outer_home.join("sessions"));
        assert!(reopened.get_or_load(&id).unwrap().read().archived);
        assert_eq!(
            reopened.get_or_load(&id).unwrap().read().name.as_deref(),
            Some("中文对话")
        );
        let stats = fixture.call("settings/get", json!({})).unwrap();
        assert_eq!(stats["storage"]["archived_count"], 1);
        assert_eq!(stats["usage"]["message_count"], 2);
        assert_eq!(stats["usage"]["tool_call_count"], 1);
        assert!(stats["storage"]["session_bytes"].as_u64().unwrap() > 0);
        for format in ["json", "markdown"] {
            let exported = fixture
                .call("sessions/export", json!({"id":id,"format":format}))
                .unwrap();
            let text = exported["content"].as_str().unwrap();
            assert!(text.contains(&full_result));
            assert!(!text.contains("transient duplicate"));
            assert_eq!(exported["session_count"], 1);
        }
        fixture
            .call("sessions/unarchive", json!({"id":id}))
            .unwrap();
        assert!(!session.read().archived);
        fixture.call("sessions/delete", json!({"id":id})).unwrap();
        assert!(runtime.sessions.get_or_load(&id).is_err());
        assert!(runtime.outer_home.join("events/events.jsonl").exists());
        assert!(fixture
            .call("sessions/delete", json!({"id":"../outside"}))
            .is_err());
    }

    #[tokio::test]
    async fn busy_sessions_block_single_and_bulk_mutations_before_any_changes() {
        let fixture = Fixture::new();
        let runtime = fixture.runtime();
        let idle = runtime.sessions.create();
        let busy = runtime.sessions.create();
        let id = busy.read().id.clone();
        let mut task = TaskRecord::new(GoalSpec::new("pending work")).unwrap();
        task.session_id = Some(id.clone());
        let task = runtime.tasks.create_task(task).unwrap();
        for method in [
            "sessions/archive",
            "sessions/unarchive",
            "sessions/delete",
            "sessions/rename",
        ] {
            assert_eq!(
                fixture
                    .call(method, json!({"id":id,"name":"blocked"}))
                    .unwrap_err()
                    .code,
                -32003
            );
        }
        for method in ["sessions/archive_all", "sessions/delete_all"] {
            assert_eq!(fixture.call(method, json!({})).unwrap_err().code, -32003);
            assert!(!idle.read().archived);
            assert_eq!(runtime.sessions.list_ids().len(), 2);
        }
        runtime
            .tasks
            .transition_task(&task.id, TaskState::Cancelled)
            .unwrap();
        let guard = SessionTurnGuard::acquire(runtime, &id).unwrap();
        assert_eq!(
            fixture
                .call("sessions/delete", json!({"id":id}))
                .unwrap_err()
                .code,
            -32003
        );
        drop(guard);
        assert_eq!(
            fixture.call("sessions/archive_all", json!({})).unwrap()["count"],
            2
        );
        assert_eq!(
            fixture.call("sessions/delete_all", json!({})).unwrap()["count"],
            2
        );
        assert!(runtime.sessions.list_ids().is_empty());
    }

    #[tokio::test]
    async fn bulk_disk_failure_reports_completed_subset() {
        let fixture = Fixture::new();
        let runtime = fixture.runtime();
        runtime.sessions.create();
        runtime.sessions.create();
        let ids = runtime.sessions.list_ids();
        let failing = runtime
            .outer_home
            .join("sessions")
            .join(format!("{}.json", ids[1]));
        std::fs::remove_file(&failing).unwrap();
        std::fs::create_dir(&failing).unwrap();
        let error = fixture.call("sessions/delete_all", json!({})).unwrap_err();
        assert_eq!(
            error.data.as_ref().unwrap()["completed_ids"],
            json!([ids[0]])
        );
        assert!(runtime.sessions.get(&ids[1]).is_some());
    }

    #[tokio::test]
    async fn model_facing_catalog_tools_follow_live_memory_preferences_instead_of_disk_cache() {
        let fixture = Fixture::new();
        let runtime = fixture.runtime();
        let skill_path = fixture.root.join("fixture-skill.md");
        std::fs::write(
            &skill_path,
            "---\nname: fixture-skill\ndescription: Example procedure\n---\nUse this procedure.",
        )
        .unwrap();
        let skills = Arc::new(dsh_skill::SkillCatalog::new(
            runtime.outer_home.join("meta"),
        ));
        skills
            .mount_path(&skill_path, dsh_skill::SkillSource::Runtime)
            .unwrap();
        let plugin_dir = fixture.root.join("fixture-plugin");
        std::fs::create_dir(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.json"),
            json!({"id":"fixture-plugin","name":"Example plugin","version":"1.0.0"}).to_string(),
        )
        .unwrap();
        let plugins = Arc::new(dsh_plugin::PluginRegistry::new(
            runtime.tools.clone(),
            runtime.outer_home.join("meta"),
        ));
        plugins.mount_dir(&plugin_dir).unwrap();
        let learning = runtime.learn.clone();
        let weights: dsh_skill::LearnWeightProvider = Arc::new(move || learning.weights());
        dsh_skill::register_skill_tools_with_weights(&runtime.tools, skills, weights.clone());
        dsh_plugin::register_plugin_tools_with_weights(&runtime.tools, plugins, weights);
        for _ in 0..8 {
            runtime.learn.record_tool_outcome_detailed(
                "fixture",
                "skill_load",
                Some("fixture-skill"),
                true,
                "loaded",
            );
            runtime.learn.record_tool_outcome(
                "fixture",
                "plugin.fixture-plugin.example",
                true,
                "worked",
            );
        }
        runtime.learn.try_persist().unwrap();
        assert!(
            std::fs::read_to_string(runtime.outer_home.join("meta/learn-weights.json"))
                .unwrap()
                .contains("fixture-skill")
        );
        let (_sender, cancel) = tokio::sync::watch::channel(false);
        let ctx = dsh_tools::ToolContext {
            cwd: fixture.root.clone(),
            outer_home: runtime.outer_home.clone(),
            workspace_outer: runtime.workspace_outer.clone(),
            cancel,
        };
        let args = json!({"query":"unrelateduniquequery"});
        let mut previous_enabled = Vec::new();
        for enabled in [true, false, true] {
            fixture
                .call(
                    "settings/update",
                    json!({"patch":{"memory_inject":enabled,"memory_generate":false}}),
                )
                .unwrap();
            let mut outputs = Vec::new();
            for tool in ["skill_search", "skill_recommend", "plugin_search"] {
                outputs.push(
                    runtime
                        .tools
                        .get(tool)
                        .unwrap()
                        .call(args.clone(), &ctx)
                        .await
                        .unwrap(),
                );
            }
            assert_eq!(outputs[0].contains("learned:+"), enabled);
            assert_eq!(outputs[2].contains("learned:+"), enabled);
            if enabled && previous_enabled.is_empty() {
                previous_enabled = outputs;
            } else if enabled {
                assert_eq!(outputs, previous_enabled);
            } else {
                assert_ne!(outputs[1], previous_enabled[1]);
            }
        }
        let before = runtime.learn.list().episodes.len();
        runtime
            .tools
            .get("skill_load")
            .unwrap()
            .call(json!({"name":"fixture-skill"}), &ctx)
            .await
            .unwrap();
        runtime.learn.record_tool_outcome_detailed(
            "fixture",
            "skill_load",
            Some("fixture-skill"),
            true,
            "disabled generation",
        );
        assert_eq!(runtime.learn.list().episodes.len(), before);
    }
}
