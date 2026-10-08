//! Native computer inspection and an explicit operator JSON session.
use anyhow::Result;
use dsh_core::Runtime;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::watch;

pub(crate) async fn run(runtime: Arc<Runtime>, action: crate::ComputerCmd) -> Result<()> {
    let (cancel, rx) = watch::channel(false);
    let signal = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = cancel.send(true);
        }
    });
    let result = match action {
        crate::ComputerCmd::Status => Ok(runtime.computer_status()),
        crate::ComputerCmd::Enable => runtime.set_computer_enabled(true),
        crate::ComputerCmd::Disable => runtime.set_computer_enabled(false),
        crate::ComputerCmd::Windows => runtime.computer.windows(rx.clone()).await,
        crate::ComputerCmd::Observe { window_id } => {
            runtime
                .computer
                .observe(&window_id, false, rx.clone())
                .await
        }
        crate::ComputerCmd::Session => {
            let result = session(&runtime, rx).await;
            signal.abort();
            return result;
        }
    };
    signal.abort();
    println!("{}", serde_json::to_string_pretty(&result?)?);
    Ok(())
}

async fn session(runtime: &Runtime, mut cancel: watch::Receiver<bool>) -> Result<()> {
    eprintln!("DSH native computer session. JSON actions: list_windows, snapshot, invoke, set_value, click, type_text, key, scroll. Start by observing a window; snapshots expire after 60 seconds and each action consumes one. EOF or Ctrl+C exits.");
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    loop {
        let line = tokio::select! {
            value=input.next_line()=>value?,
            _=cancel.changed()=>break,
        };
        let Some(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        let result = match serde_json::from_str::<Value>(&line) {
            Ok(args) => dispatch(runtime, args, cancel.clone()).await,
            Err(error) => Err(error.into()),
        };
        let output = match result {
            Ok(value) => json!({"result":value}),
            Err(error) => json!({"error":{"message":error.to_string()}}),
        };
        println!("{}", serde_json::to_string(&output)?);
    }
    Ok(())
}

async fn dispatch(runtime: &Runtime, args: Value, cancel: watch::Receiver<bool>) -> Result<Value> {
    match args.get("action").and_then(Value::as_str) {
        Some("list_windows") => runtime.computer.windows(cancel).await,
        Some("snapshot") => {
            let window_id = args
                .get("window_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("window_id is required"))?;
            runtime.computer.observe(window_id, false, cancel).await
        }
        _ => runtime.computer_operator_action(args, cancel).await,
    }
}
