//! Auditable cloud-artifact providers for local and remote app-server use.
//!
//! Artifacts are deliberately plain JSON files under the outer layer.  They
//! can be inspected, copied between machines, imported from a raw patch, and
//! applied through the same guarded `FsService` path used by agent tools.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use dsh_app_client::{ReconnectPolicy, ReconnectingTcpAppServerClient};
use dsh_core::{EventEnvelope, EventSource, EventStore, JsonlEventStore, PermissionMode, Runtime};
use dsh_fs::{FsService, PathGuard, PathGuardConfig};
use dsh_protocol::CLOUD_ARTIFACT_SCHEMA_VERSION;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

pub use dsh_protocol::CloudArtifact;
pub const ARTIFACT_SCHEMA_VERSION: u16 = CLOUD_ARTIFACT_SCHEMA_VERSION;
pub const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct ApplyOutcome {
    pub artifact_id: String,
    pub sha256: String,
    pub dry_run: bool,
    pub workspace: String,
    pub files: Vec<String>,
    pub hunks: usize,
    pub operations: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
}

/// Async provider boundary for local and remote artifact backends.
#[async_trait]
pub trait CloudProvider: Send + Sync {
    async fn list(&self) -> Result<Vec<CloudArtifact>>;
    async fn load(&self, workspace: &Path, query: &str) -> Result<CloudArtifact>;
    async fn import(&self, workspace: &Path, source_path: &Path) -> Result<CloudArtifact>;
}

#[derive(Debug, Clone)]
pub struct LocalCloudProvider {
    outer_home: PathBuf,
}

impl LocalCloudProvider {
    pub fn new(outer_home: impl Into<PathBuf>) -> Self {
        Self {
            outer_home: outer_home.into(),
        }
    }
}

#[async_trait]
impl CloudProvider for LocalCloudProvider {
    async fn list(&self) -> Result<Vec<CloudArtifact>> {
        list_artifacts(&self.outer_home)
    }

    async fn load(&self, workspace: &Path, query: &str) -> Result<CloudArtifact> {
        load_artifact(&self.outer_home, workspace, query)
    }

    async fn import(&self, workspace: &Path, source_path: &Path) -> Result<CloudArtifact> {
        import_artifact(&self.outer_home, workspace, source_path)
    }
}

/// Remote artifact provider backed by a reconnecting app-server TCP client.
/// Import sends patch contents, so the source file only needs to exist on the
/// caller side; apply still runs through the caller's local PathGuard.
pub struct RemoteCloudProvider {
    client: Arc<AsyncMutex<ReconnectingTcpAppServerClient>>,
}

impl RemoteCloudProvider {
    pub async fn connect(address: impl Into<String>) -> Result<Self> {
        Self::connect_with_policy(address, ReconnectPolicy::default()).await
    }

    pub async fn connect_with_policy(
        address: impl Into<String>,
        policy: ReconnectPolicy,
    ) -> Result<Self> {
        let client = ReconnectingTcpAppServerClient::connect(address, policy)
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))?;
        Ok(Self {
            client: Arc::new(AsyncMutex::new(client)),
        })
    }
}

#[async_trait]
impl CloudProvider for RemoteCloudProvider {
    async fn list(&self) -> Result<Vec<CloudArtifact>> {
        self.client
            .lock()
            .await
            .cloud_list()
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))
    }

    async fn load(&self, workspace: &Path, query: &str) -> Result<CloudArtifact> {
        self.client
            .lock()
            .await
            .cloud_get(&workspace_identity(workspace), query)
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))
    }

    async fn import(&self, workspace: &Path, source_path: &Path) -> Result<CloudArtifact> {
        let text = fs::read_to_string(source_path)
            .with_context(|| format!("read cloud artifact source {}", source_path.display()))?;
        if text.len() > MAX_ARTIFACT_BYTES {
            bail!("cloud artifact source exceeds {} bytes", MAX_ARTIFACT_BYTES);
        }
        self.client
            .lock()
            .await
            .cloud_import(
                &workspace_identity(workspace),
                &source_path.display().to_string(),
                &text,
            )
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))
    }
}

pub fn artifacts_dir(outer_home: &Path) -> PathBuf {
    outer_home.join("cloud").join("artifacts")
}

pub fn artifact_path(outer_home: &Path, id: &str) -> Result<PathBuf> {
    if !is_safe_id(id) {
        bail!("invalid artifact id `{id}`");
    }
    Ok(artifacts_dir(outer_home).join(format!("{id}.json")))
}

pub fn list_artifacts(outer_home: &Path) -> Result<Vec<CloudArtifact>> {
    let root = artifacts_dir(outer_home);
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut artifacts = Vec::new();
    for entry in fs::read_dir(&root).with_context(|| format!("read {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let artifact = read_stored_artifact(&path)?;
        artifacts.push(artifact);
    }
    artifacts.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(artifacts)
}

/// Import a raw SEARCH/REPLACE patch or an existing artifact JSON document.
/// A fresh id and digest are assigned so the imported file is independently
/// auditable even when its source was copied from another machine.
pub fn import_artifact(
    outer_home: &Path,
    workspace: &Path,
    source_path: &Path,
) -> Result<CloudArtifact> {
    let text = fs::read_to_string(source_path)
        .with_context(|| format!("read cloud artifact source {}", source_path.display()))?;
    if text.len() > MAX_ARTIFACT_BYTES {
        bail!("cloud artifact source exceeds {} bytes", MAX_ARTIFACT_BYTES);
    }
    import_artifact_text(
        outer_home,
        workspace,
        source_path.display().to_string(),
        text,
    )
}

/// Import artifact contents received over a remote transport.
pub fn import_artifact_text(
    outer_home: &Path,
    workspace: &Path,
    source_name: impl Into<String>,
    text: String,
) -> Result<CloudArtifact> {
    let source_name = source_name.into();
    if text.len() > MAX_ARTIFACT_BYTES {
        bail!("cloud artifact source exceeds {} bytes", MAX_ARTIFACT_BYTES);
    }
    let (patch, source) = match serde_json::from_str::<CloudArtifact>(&text) {
        Ok(document) if !document.patch.trim().is_empty() => {
            if document.schema_version > ARTIFACT_SCHEMA_VERSION {
                bail!(
                    "artifact uses unsupported schema version {} (supported {})",
                    document.schema_version,
                    ARTIFACT_SCHEMA_VERSION
                );
            }
            let source = if document.source.trim().is_empty() {
                source_name.clone()
            } else {
                document.source
            };
            (document.patch, source)
        }
        _ => (text, source_name),
    };
    if patch.trim().is_empty() {
        bail!("cloud artifact patch is empty");
    }

    let artifact = new_artifact(source, patch, workspace);
    persist_artifact(outer_home, &artifact)?;
    Ok(artifact)
}

pub fn load_artifact(outer_home: &Path, workspace: &Path, query: &str) -> Result<CloudArtifact> {
    let candidate = PathBuf::from(query);
    if candidate.is_file() {
        return read_artifact_document(&candidate, workspace);
    }
    let path = artifact_path(outer_home, query)?;
    if !path.is_file() {
        bail!("cloud artifact not found: {query}");
    }
    read_stored_artifact(&path)
}

/// Remote transports resolve ids only; arbitrary server-local paths are not
/// accepted through the app-server boundary.
pub fn load_artifact_id(outer_home: &Path, id: &str) -> Result<CloudArtifact> {
    let path = artifact_path(outer_home, id)?;
    if !path.is_file() {
        bail!("cloud artifact not found: {id}");
    }
    read_stored_artifact(&path)
}

pub fn persist_artifact(outer_home: &Path, artifact: &CloudArtifact) -> Result<PathBuf> {
    if artifact.schema_version > ARTIFACT_SCHEMA_VERSION {
        bail!(
            "artifact uses unsupported schema version {} (supported {})",
            artifact.schema_version,
            ARTIFACT_SCHEMA_VERSION
        );
    }
    artifact
        .validate_metadata()
        .map_err(anyhow::Error::msg)
        .context("validate cloud artifact metadata")?;
    let path = artifact_path(outer_home, &artifact.id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!(
        "json.tmp-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    let encoded = serde_json::to_vec_pretty(artifact)?;
    fs::write(&temp, encoded).with_context(|| format!("write {}", temp.display()))?;
    if let Err(first_error) = fs::rename(&temp, &path) {
        if cfg!(windows) && path.is_file() {
            fs::remove_file(&path)
                .with_context(|| format!("remove old artifact {}", path.display()))?;
            fs::rename(&temp, &path)
                .with_context(|| format!("persist {} after replace", path.display()))?;
        } else {
            let _ = fs::remove_file(&temp);
            return Err(first_error).with_context(|| format!("persist {}", path.display()));
        }
    }
    Ok(path)
}

pub async fn apply_artifact_with_provider(
    runtime: &Runtime,
    provider: &dyn CloudProvider,
    query: &str,
    dry_run: bool,
) -> Result<ApplyOutcome> {
    let artifact = provider.load(&runtime.workspace_root, query).await?;
    let result = apply_loaded_artifact(runtime, &artifact, dry_run);
    if let Err(err) = &result {
        let _ = record_runtime_event(
            runtime,
            "cloud.artifact.rejected",
            json!({
                "artifact_id": artifact.id,
                "sha256": artifact.sha256,
                "query": query,
                "error": err.to_string(),
            }),
        );
    }
    result
}

fn apply_loaded_artifact(
    runtime: &Runtime,
    artifact: &CloudArtifact,
    dry_run: bool,
) -> Result<ApplyOutcome> {
    verify_integrity(artifact)?;
    ensure_workspace_binding(artifact, &runtime.workspace_root)?;
    if *runtime.permissions.read() == PermissionMode::ReadOnly {
        bail!("cloud apply is blocked by read-only permissions; use --permissions auto");
    }

    let patch = rewrite_patch_paths(&artifact.patch, &runtime.workspace_root)?;
    let fs = guarded_fs(runtime)?;
    let preview = fs.preview_patch(&patch)?;
    let report = if dry_run {
        let _ = record_runtime_event(
            runtime,
            "cloud.artifact.previewed",
            json!({
                "artifact_id": artifact.id,
                "sha256": artifact.sha256,
                "files": preview.files,
                "hunks": preview.hunks,
            }),
        );
        None
    } else {
        let report = fs.apply_patch(&patch)?;
        if let Err(err) = record_runtime_event(
            runtime,
            "cloud.artifact.applied",
            json!({
                "artifact_id": artifact.id,
                "sha256": artifact.sha256,
                "files": preview.files,
                "hunks": preview.hunks,
                "workspace": artifact.workspace,
            }),
        ) {
            tracing::warn!(error = %err, artifact_id = %artifact.id, "cloud apply succeeded but audit event failed");
        }
        Some(report)
    };

    Ok(ApplyOutcome {
        artifact_id: artifact.id.clone(),
        sha256: artifact.sha256.clone(),
        dry_run,
        workspace: runtime.workspace_root.display().to_string(),
        files: preview.files,
        hunks: preview.hunks,
        operations: preview.operations,
        report,
    })
}

pub fn record_event_at(
    outer_home: &Path,
    event_type: &str,
    payload: serde_json::Value,
) -> Result<EventEnvelope> {
    let events = JsonlEventStore::open(outer_home.join("events/events.jsonl"))?;
    events.append(EventEnvelope::new(event_type, payload).with_source(EventSource::External))
}

fn record_runtime_event(
    runtime: &Runtime,
    event_type: &str,
    payload: serde_json::Value,
) -> Result<EventEnvelope> {
    runtime.record_event(EventEnvelope::new(event_type, payload).with_source(EventSource::External))
}

fn read_artifact_document(path: &Path, workspace: &Path) -> Result<CloudArtifact> {
    let text =
        fs::read_to_string(path).with_context(|| format!("read artifact {}", path.display()))?;
    if let Ok(mut artifact) = serde_json::from_str::<CloudArtifact>(&text) {
        if !artifact.patch.trim().is_empty() {
            validate_schema(artifact.schema_version, path)?;
            if artifact.id.is_empty() {
                artifact.id = format!("transient-{}", Uuid::new_v4());
            }
            if artifact.source.is_empty() {
                artifact.source = path.display().to_string();
            }
            if artifact.workspace.is_empty() && workspace != Path::new(".") {
                artifact.workspace = workspace_identity(workspace);
            }
            return Ok(artifact);
        }
    }
    if text.trim().is_empty() {
        bail!("cloud artifact patch is empty: {}", path.display());
    }
    Ok(new_artifact(path.display().to_string(), text, workspace))
}

fn read_stored_artifact(path: &Path) -> Result<CloudArtifact> {
    let text =
        fs::read_to_string(path).with_context(|| format!("read artifact {}", path.display()))?;
    let artifact: CloudArtifact = serde_json::from_str(&text)
        .with_context(|| format!("parse stored artifact {}", path.display()))?;
    if artifact.id.is_empty() || artifact.patch.trim().is_empty() {
        bail!("stored artifact {} is missing id or patch", path.display());
    }
    if path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem != artifact.id)
    {
        bail!(
            "stored artifact id does not match file name: {}",
            path.display()
        );
    }
    validate_schema(artifact.schema_version, path)?;
    Ok(artifact)
}

fn validate_schema(schema_version: u16, path: &Path) -> Result<()> {
    if schema_version > ARTIFACT_SCHEMA_VERSION {
        bail!(
            "artifact {} uses unsupported schema version {} (supported {})",
            path.display(),
            schema_version,
            ARTIFACT_SCHEMA_VERSION
        );
    }
    Ok(())
}

fn new_artifact(source: String, patch: String, workspace: &Path) -> CloudArtifact {
    CloudArtifact {
        schema_version: ARTIFACT_SCHEMA_VERSION,
        id: format!("art-{}", Uuid::new_v4()),
        created_at: Utc::now(),
        source,
        sha256: sha256_hex(&patch),
        patch,
        workspace: workspace_identity(workspace),
    }
}

fn verify_integrity(artifact: &CloudArtifact) -> Result<()> {
    artifact
        .validate_metadata()
        .map_err(anyhow::Error::msg)
        .context("validate cloud artifact metadata")?;
    if artifact.sha256.trim().is_empty() {
        bail!("artifact {} has no sha256 integrity field", artifact.id);
    }
    let actual = sha256_hex(&artifact.patch);
    if !actual.eq_ignore_ascii_case(&artifact.sha256) {
        bail!(
            "artifact {} sha256 mismatch: recorded {}, computed {}",
            artifact.id,
            artifact.sha256,
            actual
        );
    }
    Ok(())
}

/// Artifacts are portable documents, but applying one is intentionally bound
/// to the workspace identity captured at import time. Without this check a
/// valid patch copied from one checkout could silently mutate another checkout
/// when the artifact id is reused through a remote provider.
fn ensure_workspace_binding(artifact: &CloudArtifact, workspace: &Path) -> Result<()> {
    let bound = artifact.workspace.trim();
    if bound.is_empty() {
        bail!("artifact {} has no workspace binding", artifact.id);
    }
    let expected = workspace_identity(workspace);
    let bound_path = PathBuf::from(bound);
    let actual = fs::canonicalize(&bound_path)
        .unwrap_or(bound_path)
        .display()
        .to_string();
    let matches = if cfg!(windows) {
        actual.eq_ignore_ascii_case(&expected)
    } else {
        actual == expected
    };
    if !matches {
        bail!(
            "artifact {} is bound to workspace `{bound}`, not `{expected}`",
            artifact.id
        );
    }
    Ok(())
}

fn guarded_fs(runtime: &Runtime) -> Result<FsService> {
    let guard = PathGuard::new(PathGuardConfig {
        workspace_root: runtime.workspace_root.clone(),
        outer_home: runtime.outer_home.clone(),
        workspace_outer: runtime.workspace_outer.clone(),
        deny_core_writes: runtime.config.guard.deny_core_writes,
        deny_patterns: runtime.config.guard.deny_patterns.clone(),
    })?;
    Ok(FsService::new(guard))
}

fn rewrite_patch_paths(patch: &str, workspace: &Path) -> Result<String> {
    let mut out = String::new();
    for line in patch.lines() {
        let header = line
            .strip_prefix("*** Update File:")
            .map(|rest| ("*** Update File:", rest))
            .or_else(|| {
                line.strip_prefix("*** Add File:")
                    .map(|rest| ("*** Add File:", rest))
            })
            .or_else(|| {
                line.strip_prefix("*** Delete File:")
                    .map(|rest| ("*** Delete File:", rest))
            });
        if let Some((prefix, rest)) = header {
            let raw = rest.trim();
            if raw.is_empty() {
                bail!("patch file header is empty");
            }
            let path = PathBuf::from(raw);
            let resolved = if path.is_absolute() {
                path
            } else {
                workspace.join(path)
            };
            out.push_str(&format!("{prefix} {}\n", resolved.display()));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    Ok(out)
}

fn workspace_identity(workspace: &Path) -> String {
    fs::canonicalize(workspace)
        .unwrap_or_else(|_| workspace.to_path_buf())
        .display()
        .to_string()
}

fn sha256_hex(value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(value.as_bytes());
    hex::encode(digest.finalize())
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_core::AppConfig;
    use dsh_llm::DeepSeekClient;
    use dsh_tools::ToolRegistry;
    use std::sync::Arc;

    fn temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dsh-cloud-{label}-{}", Uuid::new_v4()))
    }

    #[test]
    fn import_round_trips_and_records_digest() {
        let root = temp_dir("import");
        fs::create_dir_all(&root).expect("root");
        let source = root.join("change.patch");
        fs::write(
            &source,
            "*** Add File: result.txt\n<<<<<<< SEARCH\n=======\nready\n>>>>>>> REPLACE\n",
        )
        .expect("patch");
        let home = root.join("home");
        let artifact = import_artifact(&home, &root, &source).expect("import");
        assert!(artifact.id.starts_with("art-"));
        assert_eq!(
            load_artifact(&home, &root, &artifact.id)
                .expect("load")
                .sha256,
            artifact.sha256
        );
        assert_eq!(list_artifacts(&home).expect("list").len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn import_rejects_future_artifact_schema() {
        let root = temp_dir("future-schema");
        fs::create_dir_all(&root).expect("root");
        let document = serde_json::json!({
            "schema_version": ARTIFACT_SCHEMA_VERSION + 1,
            "patch": "*** Add File: future.txt\n<<<<<<< SEARCH\n=======\nfuture\n>>>>>>> REPLACE\n"
        });
        let error = import_artifact_text(
            &root.join("home"),
            &root,
            "future.json",
            document.to_string(),
        )
        .expect_err("future schema must be rejected");
        assert!(error.to_string().contains("unsupported schema version"));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn apply_honors_guard_and_dry_run() {
        let root = temp_dir("apply");
        let home = root.join("home");
        fs::create_dir_all(&root).expect("root");
        let mut config = AppConfig::builtin_default();
        config.paths.outer_home = home.display().to_string();
        config.agent.scheduler_enabled = false;
        let llm = DeepSeekClient::new(config.to_llm_config(String::new())).expect("llm");
        let runtime = Runtime::bootstrap(config, root.clone(), llm, Arc::new(ToolRegistry::new()))
            .expect("runtime");
        let source = root.join("change.patch");
        fs::write(
            &source,
            "*** Add File: result.txt\n<<<<<<< SEARCH\n=======\nready\n>>>>>>> REPLACE\n",
        )
        .expect("patch");
        let artifact = import_artifact(&home, &root, &source).expect("import");
        let provider = LocalCloudProvider::new(home.clone());
        let preview = apply_artifact_with_provider(&runtime, &provider, &artifact.id, true)
            .await
            .expect("preview");
        assert!(preview.dry_run);
        assert!(!root.join("result.txt").exists());
        let applied = apply_artifact_with_provider(&runtime, &provider, &artifact.id, false)
            .await
            .expect("apply");
        assert!(!applied.dry_run);
        assert_eq!(
            fs::read_to_string(root.join("result.txt")).expect("result"),
            "ready"
        );

        let mut tampered = artifact.clone();
        tampered.id = "art-tampered".into();
        tampered.patch = tampered.patch.replace("ready", "tampered");
        persist_artifact(&home, &tampered).expect("tampered artifact");
        assert!(
            apply_artifact_with_provider(&runtime, &provider, &tampered.id, false)
                .await
                .is_err()
        );
        assert!(runtime
            .events
            .read_all()
            .expect("events")
            .iter()
            .any(|event| event.event_type == "cloud.artifact.rejected"));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn apply_rejects_an_artifact_bound_to_another_workspace() {
        let root = temp_dir("binding-root");
        let other = temp_dir("binding-other");
        fs::create_dir_all(&root).expect("root");
        fs::create_dir_all(&other).expect("other");
        let home = root.join("outer");
        let mut config = AppConfig::builtin_default();
        config.paths.outer_home = home.display().to_string();
        config.agent.scheduler_enabled = false;
        let llm = DeepSeekClient::new(config.to_llm_config(String::new())).expect("llm");
        let runtime = Runtime::bootstrap(config, root.clone(), llm, Arc::new(ToolRegistry::new()))
            .expect("runtime");
        let patch = "*** Add File: bound.txt\n<<<<<<< SEARCH\n=======\nbound\n>>>>>>> REPLACE\n";
        let mut artifact = new_artifact("binding.patch".into(), patch.into(), &other);
        artifact.id = format!("art-{}", Uuid::new_v4());
        persist_artifact(&home, &artifact).expect("persist");
        let provider = LocalCloudProvider::new(home.clone());
        let result = apply_artifact_with_provider(&runtime, &provider, &artifact.id, true).await;
        assert!(result.is_err());
        assert!(!root.join("bound.txt").exists());
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(other);
    }
}
