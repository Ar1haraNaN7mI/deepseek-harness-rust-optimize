//! Builtin filesystem, search, shell, web, and todo tools protected by PathGuard.

use async_trait::async_trait;
use dsh_fs::FsService;
use dsh_tools::{ToolContext, ToolDefinition, ToolError, ToolHandler, ToolRegistry};
use parking_lot::RwLock;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;

pub fn register_builtin_tools(registry: &ToolRegistry, fs: Arc<FsService>) {
    let todos = Arc::new(TodoStore::default());
    registry.register(Arc::new(ReadFileTool { fs: fs.clone() }));
    registry.register(Arc::new(WriteFileTool { fs: fs.clone() }));
    registry.register(Arc::new(EditFileTool { fs: fs.clone() }));
    registry.register(Arc::new(ApplyPatchTool { fs: fs.clone() }));
    registry.register(Arc::new(ListDirTool { fs: fs.clone() }));
    registry.register(Arc::new(GlobTool { fs: fs.clone() }));
    registry.register(Arc::new(GrepTool { fs: fs.clone() }));
    registry.register(Arc::new(ShellTool));
    registry.register(Arc::new(WebFetchTool));
    registry.register(Arc::new(TodoWriteTool {
        store: todos.clone(),
    }));
    registry.register(Arc::new(TodoReadTool { store: todos }));
}

fn resolve_path(ctx: &ToolContext, path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        ctx.cwd.join(p)
    }
}

fn cancelled(ctx: &ToolContext) -> bool {
    *ctx.cancel.borrow()
}

struct ReadFileTool {
    fs: Arc<FsService>,
}

#[async_trait]
impl ToolHandler for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "read_file",
            "Read a UTF-8 text file with optional 1-based offset and line limit (default 400 lines).",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "offset": { "type": "integer", "description": "1-based start line" },
                    "limit": { "type": "integer", "description": "max lines to return" }
                },
                "required": ["path"]
            }),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<String, ToolError> {
        if cancelled(ctx) {
            return Err(ToolError::Message("cancelled".into()));
        }
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Message("path required".into()))?;
        let offset = args
            .get("offset")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        let resolved = resolve_path(ctx, path);
        self.fs
            .read_range(&resolved, offset, limit)
            .map_err(|e| ToolError::Message(e.to_string()))
    }
}

struct WriteFileTool {
    fs: Arc<FsService>,
}

#[async_trait]
impl ToolHandler for WriteFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "write_file",
            "Write a UTF-8 text file. PathGuard denies writes to core crates/Cargo/target.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<String, ToolError> {
        if cancelled(ctx) {
            return Err(ToolError::Message("cancelled".into()));
        }
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Message("path required".into()))?;
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Message("content required".into()))?;
        let resolved = resolve_path(ctx, path);
        self.fs
            .write_text(&resolved, content)
            .map_err(|e| ToolError::Message(e.to_string()))?;
        Ok(format!("wrote {}", resolved.display()))
    }
}

struct EditFileTool {
    fs: Arc<FsService>,
}

#[async_trait]
impl ToolHandler for EditFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "edit_file",
            "Replace text in a file. PathGuard denies core kernel paths.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean" }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<String, ToolError> {
        if cancelled(ctx) {
            return Err(ToolError::Message("cancelled".into()));
        }
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Message("path required".into()))?;
        let old = args
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Message("old_string required".into()))?;
        let new = args
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Message("new_string required".into()))?;
        let replace_all = args
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let resolved = resolve_path(ctx, path);
        self.fs
            .edit_replace(&resolved, old, new, replace_all)
            .map_err(|e| ToolError::Message(e.to_string()))
    }
}

struct ApplyPatchTool {
    fs: Arc<FsService>,
}

#[async_trait]
impl ToolHandler for ApplyPatchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "apply_patch",
            "Apply multi-hunk SEARCH/REPLACE patch. Supports *** Update/Add/Delete File headers.",
            json!({
                "type": "object",
                "properties": {
                    "patch": { "type": "string" }
                },
                "required": ["patch"]
            }),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<String, ToolError> {
        if cancelled(ctx) {
            return Err(ToolError::Message("cancelled".into()));
        }
        let patch = args
            .get("patch")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Message("patch required".into()))?;
        // Resolve relative paths inside patch against cwd by rewriting headers.
        let rewritten = rewrite_patch_paths(patch, &ctx.cwd);
        self.fs
            .apply_patch(&rewritten)
            .map_err(|e| ToolError::Message(e.to_string()))
    }
}

fn rewrite_patch_paths(patch: &str, cwd: &Path) -> String {
    let mut out = String::new();
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("*** Update File:") {
            let p = PathBuf::from(rest.trim());
            let abs = if p.is_absolute() { p } else { cwd.join(p) };
            out.push_str(&format!("*** Update File: {}\n", abs.display()));
        } else if let Some(rest) = line.strip_prefix("*** Add File:") {
            let p = PathBuf::from(rest.trim());
            let abs = if p.is_absolute() { p } else { cwd.join(p) };
            out.push_str(&format!("*** Add File: {}\n", abs.display()));
        } else if let Some(rest) = line.strip_prefix("*** Delete File:") {
            let p = PathBuf::from(rest.trim());
            let abs = if p.is_absolute() { p } else { cwd.join(p) };
            out.push_str(&format!("*** Delete File: {}\n", abs.display()));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

struct ListDirTool {
    fs: Arc<FsService>,
}

#[async_trait]
impl ToolHandler for ListDirTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "list_dir",
            "List directory entries (non-recursive).",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<String, ToolError> {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let resolved = resolve_path(ctx, path);
        let entries = self
            .fs
            .list_dir(&resolved)
            .map_err(|e| ToolError::Message(e.to_string()))?;
        Ok(entries
            .into_iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

struct GlobTool {
    fs: Arc<FsService>,
}

#[async_trait]
impl ToolHandler for GlobTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "glob",
            "Find files under a root matching a glob pattern (e.g. **/*.rs).",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "root": { "type": "string" },
                    "max": { "type": "integer" }
                },
                "required": ["pattern"]
            }),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<String, ToolError> {
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Message("pattern required".into()))?;
        let root = args
            .get("root")
            .and_then(|v| v.as_str())
            .map(|p| resolve_path(ctx, p))
            .unwrap_or_else(|| ctx.cwd.clone());
        let max = args.get("max").and_then(|v| v.as_u64()).unwrap_or(200) as usize;
        let hits = self
            .fs
            .glob(&root, pattern, max)
            .map_err(|e| ToolError::Message(e.to_string()))?;
        if hits.is_empty() {
            return Ok("(no files)".into());
        }
        Ok(hits
            .into_iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

struct GrepTool {
    fs: Arc<FsService>,
}

#[async_trait]
impl ToolHandler for GrepTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "grep",
            "Search file contents with a regex. Optional glob filter (e.g. *.rs).",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "root": { "type": "string" },
                    "glob": { "type": "string" },
                    "case_insensitive": { "type": "boolean" },
                    "max_hits": { "type": "integer" }
                },
                "required": ["pattern"]
            }),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<String, ToolError> {
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Message("pattern required".into()))?;
        let root = args
            .get("root")
            .and_then(|v| v.as_str())
            .map(|p| resolve_path(ctx, p))
            .unwrap_or_else(|| ctx.cwd.clone());
        let glob = args.get("glob").and_then(|v| v.as_str());
        let ci = args
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let max = args.get("max_hits").and_then(|v| v.as_u64()).unwrap_or(80) as usize;
        self.fs
            .grep(&root, pattern, glob, max, ci)
            .map_err(|e| ToolError::Message(e.to_string()))
    }
}

struct ShellTool;

#[async_trait]
impl ToolHandler for ShellTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "shell",
            "Run a shell command in the workspace cwd. Honors cancel. Do not mutate core crates.",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" }
                },
                "required": ["command"]
            }),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<String, ToolError> {
        if cancelled(ctx) {
            return Err(ToolError::Message("cancelled".into()));
        }
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Message("command required".into()))?;
        if looks_like_core_mutation(command) {
            return Err(ToolError::Message(
                "shell blocked: refuses mutating core crates/Cargo".into(),
            ));
        }
        run_shell_cancellable(ctx, command).await
    }
}

async fn run_shell_cancellable(ctx: &ToolContext, command: &str) -> Result<String, ToolError> {
    use tokio::io::AsyncReadExt;

    #[cfg(windows)]
    let mut child = Command::new("powershell")
        .args(["-NoProfile", "-Command", command])
        .current_dir(&ctx.cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| ToolError::Message(e.to_string()))?;

    #[cfg(not(windows))]
    let mut child = Command::new("bash")
        .args(["-lc", command])
        .current_dir(&ctx.cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| ToolError::Message(e.to_string()))?;

    let mut cancel = ctx.cancel.clone();
    loop {
        if *cancel.borrow() {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(ToolError::Message("shell cancelled".into()));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_end(&mut stdout).await;
                }
                if let Some(mut err) = child.stderr.take() {
                    let _ = err.read_to_end(&mut stderr).await;
                }
                let mut text = String::from_utf8_lossy(&stdout).to_string();
                if !stderr.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&String::from_utf8_lossy(&stderr));
                }
                text.push_str(&format!("\n[exit {}]", status.code().unwrap_or(-1)));
                return Ok(text);
            }
            Ok(None) => {
                tokio::select! {
                    _ = cancel.changed() => {}
                    _ = tokio::time::sleep(std::time::Duration::from_millis(40)) => {}
                }
            }
            Err(e) => return Err(ToolError::Message(e.to_string())),
        }
    }
}

fn looks_like_core_mutation(command: &str) -> bool {
    let lower = command.to_lowercase();
    let hits_core =
        lower.contains("crates/") || lower.contains("cargo.toml") || lower.contains("\\crates\\");
    let mutates = lower.contains("rm ")
        || lower.contains("del ")
        || lower.contains("remove-item")
        || lower.contains("move-item")
        || lower.contains('>')
        || lower.contains("set-content")
        || lower.contains("out-file");
    hits_core && mutates
}

struct WebFetchTool;

#[async_trait]
impl ToolHandler for WebFetchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "web_fetch",
            "Fetch a public URL and return truncated text/markdown-ish body (max ~32k chars).",
            json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string" }
                },
                "required": ["url"]
            }),
        )
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<String, ToolError> {
        if cancelled(ctx) {
            return Err(ToolError::Message("cancelled".into()));
        }
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::Message("url required".into()))?;
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(ToolError::Message("only http(s) URLs allowed".into()));
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| ToolError::Message(e.to_string()))?;
        let resp = client
            .get(url)
            .header("User-Agent", "dsh-rust/0.1")
            .send()
            .await
            .map_err(|e| ToolError::Message(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ToolError::Message(e.to_string()))?;
        let stripped = strip_html_light(&text);
        let truncated: String = stripped.chars().take(32_000).collect();
        Ok(format!("HTTP {status}\n{truncated}"))
    }
}

fn strip_html_light(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for c in input.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Default)]
struct TodoStore {
    items: RwLock<Vec<TodoItem>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct TodoItem {
    id: String,
    content: String,
    status: String,
}

struct TodoWriteTool {
    store: Arc<TodoStore>,
}

#[async_trait]
impl ToolHandler for TodoWriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "todo_write",
            "Replace the in-session todo list. status: pending|in_progress|completed|cancelled.",
            json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "content": { "type": "string" },
                                "status": { "type": "string" }
                            },
                            "required": ["id", "content", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        )
    }

    async fn call(&self, args: Value, _ctx: &ToolContext) -> Result<String, ToolError> {
        let todos = args
            .get("todos")
            .cloned()
            .ok_or_else(|| ToolError::Message("todos required".into()))?;
        let items: Vec<TodoItem> =
            serde_json::from_value(todos).map_err(|e| ToolError::Message(e.to_string()))?;
        *self.store.items.write() = items;
        Ok(serde_json::to_string_pretty(&*self.store.items.read()).unwrap_or_default())
    }
}

struct TodoReadTool {
    store: Arc<TodoStore>,
}

#[async_trait]
impl ToolHandler for TodoReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::builtin(
            "todo_read",
            "Read the current in-session todo list.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    async fn call(&self, _args: Value, _ctx: &ToolContext) -> Result<String, ToolError> {
        Ok(serde_json::to_string_pretty(&*self.store.items.read()).unwrap_or_default())
    }
}
