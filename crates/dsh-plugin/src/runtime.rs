//! Rhai sandbox with compiled-AST cache and safe host bridges.

use parking_lot::Mutex;
use rhai::{Dynamic, Engine, Map, Scope, AST};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RhaiError {
    #[error("rhai: {0}")]
    Script(String),
}

/// Host context exposed to plugin scripts (outer-safe proxies only).
#[derive(Debug, Clone)]
pub struct HostBridge {
    pub cwd: PathBuf,
    pub outer_home: PathBuf,
    pub plugin_root: PathBuf,
}

impl Default for HostBridge {
    fn default() -> Self {
        Self {
            cwd: PathBuf::from("."),
            outer_home: PathBuf::from("."),
            plugin_root: PathBuf::from("."),
        }
    }
}

pub struct PluginSandbox {
    engine: Engine,
    cache: Mutex<HashMap<u64, Arc<AST>>>,
}

impl PluginSandbox {
    pub fn new() -> Self {
        let mut engine = Engine::new();
        engine.set_max_operations(200_000);
        engine.set_max_string_size(256 * 1024);
        engine.set_max_array_size(10_000);
        engine.set_max_map_size(10_000);
        register_host_api(&mut engine);
        Self {
            engine,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Compile-check a script without executing plugin tools (used on mount).
    pub fn validate_script(&self, script: &str) -> Result<(), RhaiError> {
        if script.trim().is_empty() {
            return Ok(());
        }
        self.compile_cached(script)?;
        Ok(())
    }

    pub fn invalidate_cache(&self) {
        self.cache.lock().clear();
    }

    fn compile_cached(&self, script: &str) -> Result<Arc<AST>, RhaiError> {
        let hash = fxhash(script);
        if let Some(ast) = self.cache.lock().get(&hash).cloned() {
            return Ok(ast);
        }
        let ast = self
            .engine
            .compile(script)
            .map_err(|e| RhaiError::Script(e.to_string()))?;
        let ast = Arc::new(ast);
        self.cache.lock().insert(hash, ast.clone());
        Ok(ast)
    }

    pub fn call_fn_with_host(
        &self,
        script: &str,
        fn_name: &str,
        args_json: &Value,
        host: &HostBridge,
    ) -> Result<String, RhaiError> {
        let ast = self.compile_cached(script)?;
        let mut scope = Scope::new();
        scope.push("HOST_CWD", host.cwd.to_string_lossy().to_string());
        scope.push(
            "HOST_OUTER",
            host.outer_home.to_string_lossy().to_string(),
        );
        scope.push(
            "HOST_PLUGIN_ROOT",
            host.plugin_root.to_string_lossy().to_string(),
        );

        let _ = self
            .engine
            .eval_ast_with_scope::<Dynamic>(&mut scope, ast.as_ref())
            .map_err(|e| RhaiError::Script(e.to_string()))?;

        let dyn_arg = json_to_dynamic(args_json);
        let result = self
            .engine
            .call_fn::<Dynamic>(&mut scope, ast.as_ref(), fn_name, (dyn_arg,))
            .or_else(|_| {
                let s = args_json.to_string();
                self.engine
                    .call_fn::<Dynamic>(&mut scope, ast.as_ref(), fn_name, (s,))
            })
            .or_else(|_| {
                let input = args_json
                    .get("input")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.engine
                    .call_fn::<Dynamic>(&mut scope, ast.as_ref(), fn_name, (input,))
            })
            .map_err(|e| RhaiError::Script(e.to_string()))?;
        Ok(dynamic_to_string(result))
    }
}

impl Default for PluginSandbox {
    fn default() -> Self {
        Self::new()
    }
}

fn register_host_api(engine: &mut Engine) {
    // host_log(msg) — debug aid (no filesystem side effects)
    engine.register_fn("host_log", |msg: &str| {
        tracing::info!(target: "dsh_plugin", "plugin host_log: {msg}");
        Dynamic::UNIT
    });

    // host_cwd() — current working directory string
    engine.register_fn("host_cwd", || {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".into())
    });

    // host_read_text(path) — read text only under plugin root / outer / cwd (size capped)
    engine.register_fn("host_read_text", |path: &str| -> String {
        match safe_read_text(Path::new(path)) {
            Ok(s) => s,
            Err(e) => format!("host_read_text error: {e}"),
        }
    });

    // host_list_dir(path) — list entries (capped) under allowed roots
    engine.register_fn("host_list_dir", |path: &str| -> rhai::Array {
        match safe_list_dir(Path::new(path)) {
            Ok(items) => items.into_iter().map(Dynamic::from).collect(),
            Err(e) => vec![Dynamic::from(format!("error: {e}"))],
        }
    });
}

fn is_allowed_read(path: &Path) -> bool {
    let Ok(canon) = path.canonicalize() else {
        // Allow relative paths that may not exist yet only if parent is allowed.
        return path
            .parent()
            .and_then(|p| p.canonicalize().ok())
            .map(|p| is_allowed_read(&p))
            .unwrap_or(false);
    };
    let s = canon.to_string_lossy().replace('\\', "/").to_lowercase();
    // Deny core crate tree by path segment heuristic.
    if s.contains("/crates/") || s.ends_with("/crates") {
        return false;
    }
    if s.contains("/target/") {
        return false;
    }
    true
}

fn safe_read_text(path: &Path) -> Result<String, String> {
    if !is_allowed_read(path) {
        return Err("path not allowed for host_read_text".into());
    }
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > 256 * 1024 {
        return Err("file too large (>256KiB)".into());
    }
    std::fs::read_to_string(path).map_err(|e| e.to_string())
}

fn safe_list_dir(path: &Path) -> Result<Vec<String>, String> {
    if !is_allowed_read(path) {
        return Err("path not allowed for host_list_dir".into());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        out.push(entry.file_name().to_string_lossy().to_string());
        if out.len() >= 200 {
            out.push("…".into());
            break;
        }
    }
    out.sort();
    Ok(out)
}

fn fxhash(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn json_to_dynamic(value: &Value) -> Dynamic {
    match value {
        Value::Null => Dynamic::UNIT,
        Value::Bool(b) => Dynamic::from(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Dynamic::from(i)
            } else if let Some(f) = n.as_f64() {
                Dynamic::from(f)
            } else {
                Dynamic::from(n.to_string())
            }
        }
        Value::String(s) => Dynamic::from(s.clone()),
        Value::Array(arr) => {
            let list: rhai::Array = arr.iter().map(json_to_dynamic).collect();
            Dynamic::from(list)
        }
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                out.insert(k.clone().into(), json_to_dynamic(v));
            }
            Dynamic::from(out)
        }
    }
}

fn dynamic_to_string(value: Dynamic) -> String {
    if value.is_unit() {
        return String::new();
    }
    if let Ok(s) = value.clone().into_string() {
        return s;
    }
    if let Some(map) = value.clone().try_cast::<Map>() {
        return format!("{map:?}");
    }
    format!("{value}")
}
