//! Minimal MCP server over stdio (JSON-RPC 2.0, one JSON object per line).

use anyhow::Result;
use dsh_core::Runtime;
use dsh_tools::{ToolCall, ToolContext, ToolPipeline};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::sync::Arc;
use tokio::sync::watch;
use uuid::Uuid;

/// Serve MCP over stdin/stdout until EOF or fatal error.
pub async fn run_mcp_server(runtime: Arc<Runtime>) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let reader = stdin.lock();

    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                write_line(
                    &mut stdout,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": { "code": -32700, "message": format!("parse error: {e}") }
                    }),
                )?;
                continue;
            }
        };

        // Notifications have no id (or null) and expect no response.
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));

        match method {
            "notifications/initialized" | "initialized" => {
                // No response for notifications.
                continue;
            }
            "initialize" => {
                let result = json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": { "listChanged": false }
                    },
                    "serverInfo": {
                        "name": "dsh-rust",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                });
                write_result(&mut stdout, id, result)?;
            }
            "ping" => {
                write_result(&mut stdout, id, json!({}))?;
            }
            "tools/list" => {
                let tools: Vec<Value> = runtime
                    .tools
                    .definitions()
                    .into_iter()
                    .map(|d| {
                        json!({
                            "name": d.name,
                            "description": d.description,
                            "inputSchema": d.parameters
                        })
                    })
                    .collect();
                write_result(&mut stdout, id, json!({ "tools": tools }))?;
            }
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
                if name.is_empty() {
                    write_error(&mut stdout, id, -32602, "missing tool name")?;
                    continue;
                }
                let (_cancel_tx, cancel_rx) = watch::channel(false);
                let call = ToolCall {
                    id: Uuid::new_v4().to_string(),
                    name: name.clone(),
                    arguments,
                };
                let ctx = ToolContext {
                    cwd: runtime.workspace_root.clone(),
                    outer_home: runtime.outer_home.clone(),
                    workspace_outer: runtime.workspace_outer.clone(),
                    cancel: cancel_rx,
                };
                let result = runtime.pipeline.execute(&call, &ctx).await;
                let content = json!([{
                    "type": "text",
                    "text": result.content
                }]);
                write_result(
                    &mut stdout,
                    id,
                    json!({
                        "content": content,
                        "isError": !result.ok
                    }),
                )?;
            }
            "" => {
                // Could be a response — ignore.
            }
            other => {
                if id.is_some() {
                    write_error(
                        &mut stdout,
                        id,
                        -32601,
                        &format!("method not found: {other}"),
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn write_result(out: &mut impl Write, id: Option<Value>, result: Value) -> Result<()> {
    let mut body = json!({
        "jsonrpc": "2.0",
        "result": result
    });
    if let Some(id) = id {
        body["id"] = id;
    } else {
        body["id"] = Value::Null;
    }
    write_line(out, &body)
}

fn write_error(out: &mut impl Write, id: Option<Value>, code: i64, message: &str) -> Result<()> {
    let mut body = json!({
        "jsonrpc": "2.0",
        "error": { "code": code, "message": message }
    });
    if let Some(id) = id {
        body["id"] = id;
    } else {
        body["id"] = Value::Null;
    }
    write_line(out, &body)
}

fn write_line(out: &mut impl Write, value: &Value) -> Result<()> {
    writeln!(out, "{value}")?;
    out.flush()?;
    Ok(())
}
