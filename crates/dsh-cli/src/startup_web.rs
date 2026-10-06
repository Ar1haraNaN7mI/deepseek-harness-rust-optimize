//! Same-origin loopback hosts for the startup preview and the actual Harness runtime.
use anyhow::{bail, Context, Result};
use dsh_core::{
    load_startup_profile, save_startup_profile, set_next_startup, EventStore, Runtime,
    StartupProfile,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex, Semaphore},
};

struct Host {
    workspace: PathBuf,
    outer_home: PathBuf,
    roots: Vec<PathBuf>,
    authority: String,
    token: String,
    sound: bool,
    inventory: Mutex<Option<crate::startup_inventory::LoadedInventory>>,
    runtime: Option<Arc<Runtime>>,
    assets: Option<PathBuf>,
    startup_override: Option<bool>,
}

pub async fn serve(
    workspace: PathBuf,
    outer_home: PathBuf,
    roots: Vec<PathBuf>,
    port: u16,
    sound: bool,
) -> Result<()> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .context("bind startup preview (choose another --port if it is in use)")?;
    let authority = listener.local_addr()?.to_string();
    println!("DSH local startup: http://{authority}/startup-preview.html");
    println!("Workspace: {}\nCtrl+C to stop. Skills/plugins are loaded only when the loading scene starts.", workspace.display());
    let host = Arc::new(Host {
        workspace,
        outer_home,
        roots,
        authority,
        token: uuid::Uuid::new_v4().to_string(),
        sound,
        inventory: Mutex::new(None),
        runtime: None,
        assets: None,
        startup_override: None,
    });
    accept_connections(listener, host).await
}

/// Attach a web frontend to the already booted CLI runtime. This does not load
/// a second agent, start another scheduler, or consume the next-CLI preference.
pub async fn serve_harness(
    runtime: Arc<Runtime>,
    port: u16,
    assets: Option<PathBuf>,
    startup_override: Option<bool>,
) -> Result<()> {
    let cwd = std::env::current_dir().context("locate the current directory for Harness assets")?;
    let executable = std::env::current_exe().ok();
    let assets = resolve_harness_assets(assets.as_deref(), executable.as_deref(), &cwd)?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .context("bind Harness web host (choose another --port if it is in use)")?;
    let authority = listener.local_addr()?.to_string();
    println!("DSH Harness: http://{authority}/");
    println!(
        "Workspace: {}\nCtrl+C to stop.",
        runtime.workspace_root.display()
    );
    let host = Arc::new(Host {
        workspace: runtime.workspace_root.clone(),
        outer_home: runtime.outer_home.clone(),
        roots: vec![],
        authority,
        token: uuid::Uuid::new_v4().to_string(),
        sound: runtime.config.tui.startup.sound,
        inventory: Mutex::new(None),
        runtime: Some(runtime),
        assets: Some(assets),
        startup_override,
    });
    accept_connections(listener, host).await
}

fn validate_harness_assets(path: &Path) -> Result<PathBuf> {
    let assets = path
        .canonicalize()
        .with_context(|| format!("Harness assets unavailable at {}", path.display()))?;
    anyhow::ensure!(
        assets.is_dir() && assets.join("index.html").is_file(),
        "Harness assets at {} must be a directory containing index.html",
        path.display()
    );
    Ok(assets)
}

fn resolve_harness_assets(
    explicit: Option<&Path>,
    executable: Option<&Path>,
    cwd: &Path,
) -> Result<PathBuf> {
    // An explicit override must fail visibly, even when other assets exist.
    if let Some(path) = explicit {
        return validate_harness_assets(&cwd.join(path));
    }

    let mut candidates = Vec::new();
    if let Some(root) = executable.and_then(Path::parent).and_then(Path::parent) {
        // Cargo-style install: <root>/bin/dsh and <root>/share/dsh/web.
        candidates.push(root.join("share/dsh/web"));
    }
    candidates.push(cwd.join("web/dist"));

    let mut failures = Vec::new();
    for candidate in candidates {
        match validate_harness_assets(&candidate) {
            Ok(assets) => return Ok(assets),
            Err(error) => failures.push(format!("{error:#}")),
        }
    }
    bail!(
        "Harness frontend was not found. Install the frontend alongside dsh, run npm --prefix web run build in the repository, or use --assets. Checked:\n{}",
        failures.join("\n")
    )
}

async fn accept_connections(listener: TcpListener, host: Arc<Host>) -> Result<()> {
    let permits = Arc::new(Semaphore::new(64));
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (socket, _) = accepted?;
                let Ok(permit) = permits.clone().try_acquire_owned() else { drop(socket); continue; };
                let host = host.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(error) = tokio::time::timeout(Duration::from_secs(130), handle(socket, host)).await { tracing::debug!(%error, "web connection timed out"); }
                });
            }
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

async fn read_request(socket: &mut TcpStream) -> Result<Request> {
    let mut bytes = Vec::new();
    let boundary = loop {
        let mut chunk = [0u8; 2048];
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            bail!("incomplete request");
        }
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if bytes.len() > 16384 {
            bail!("request headers too large");
        }
    };
    if boundary > 16384 {
        bail!("request headers too large");
    }
    let header = std::str::from_utf8(&bytes[..boundary])?;
    let mut lines = header.split("\r\n");
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let method = first.next().context("missing method")?.to_string();
    let path = first.next().context("missing path")?.to_string();
    let mut headers = HashMap::new();
    for line in lines.filter(|l| !l.is_empty()) {
        let (name, value) = line.split_once(':').context("invalid header")?;
        if headers
            .insert(name.to_ascii_lowercase(), value.trim().to_string())
            .is_some()
        {
            bail!("duplicate header");
        }
    }
    if headers.contains_key("transfer-encoding") {
        bail!("chunked requests are unsupported");
    }
    let length = headers
        .get("content-length")
        .map(|v| v.parse::<usize>())
        .transpose()?
        .unwrap_or(0);
    if length > 1024 * 1024 {
        bail!("request body too large");
    }
    while bytes.len() < boundary + length {
        let mut chunk = [0u8; 2048];
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            bail!("incomplete body");
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
    Ok(Request {
        method,
        path,
        headers,
        body: bytes[boundary..boundary + length].to_vec(),
    })
}

async fn response(socket: &mut TcpStream, code: u16, kind: &str, body: &[u8]) -> Result<()> {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let head = format!("HTTP/1.1 {code} {reason}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nCross-Origin-Resource-Policy: same-origin\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n", body.len());
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(body).await?;
    socket.shutdown().await?;
    Ok(())
}

async fn json_response(socket: &mut TcpStream, code: u16, value: Value) -> Result<()> {
    response(
        socket,
        code,
        "application/json; charset=utf-8",
        &serde_json::to_vec(&value)?,
    )
    .await
}

fn allowed(request: &Request, host: &Host) -> bool {
    let Some(authority) = request.headers.get("host") else {
        return false;
    };
    let localhost = format!(
        "localhost:{}",
        host.authority.rsplit(':').next().unwrap_or_default()
    );
    if authority != &host.authority && authority != &localhost {
        return false;
    }
    if request
        .headers
        .get("origin")
        .is_some_and(|o| o != &format!("http://{authority}"))
    {
        return false;
    }
    if request
        .headers
        .get("sec-fetch-site")
        .is_some_and(|s| s != "same-origin" && s != "none")
    {
        return false;
    }
    request.method != "POST" || request.headers.get("x-dsh-token") == Some(&host.token)
}

fn asset(path: &str) -> Option<(&'static str, &'static str)> {
    match path.split('?').next().unwrap_or(path) {
        "/" | "/startup-preview.html" => Some((
            "text/html; charset=utf-8",
            include_str!("../../../docs/startup-preview.html"),
        )),
        "/startup-preview.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../../../docs/startup-preview.js"),
        )),
        "/startup-sequence.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../../../docs/startup-sequence.js"),
        )),
        "/startup-emblem.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../../../docs/startup-emblem.js"),
        )),
        "/startup-visuals.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../../../docs/startup-visuals.js"),
        )),
        "/startup-local.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../../../docs/startup-local.js"),
        )),
        "/startup-voice.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../../../docs/startup-voice.js"),
        )),
        "/startup-embed.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("../../../docs/startup-embed.js"),
        )),
        _ => None,
    }
}

fn audio_asset(path: &str) -> Option<&'static [u8]> {
    Some(match path {
        "/assets/voice/phase-0.wav" => include_bytes!("../../../docs/assets/voice/phase-0.wav"),
        "/assets/voice/phase-1.wav" => include_bytes!("../../../docs/assets/voice/phase-1.wav"),
        "/assets/voice/phase-2.wav" => include_bytes!("../../../docs/assets/voice/phase-2.wav"),
        "/assets/voice/phase-3.wav" => include_bytes!("../../../docs/assets/voice/phase-3.wav"),
        "/assets/voice/phase-3-mounted.wav" => {
            include_bytes!("../../../docs/assets/voice/phase-3-mounted.wav")
        }
        "/assets/voice/phase-4.wav" => include_bytes!("../../../docs/assets/voice/phase-4.wav"),
        "/assets/voice/phase-5.wav" => include_bytes!("../../../docs/assets/voice/phase-5.wav"),
        "/assets/voice/load-warning.wav" => {
            include_bytes!("../../../docs/assets/voice/load-warning.wav")
        }
        "/assets/voice/load-unavailable.wav" => {
            include_bytes!("../../../docs/assets/voice/load-unavailable.wav")
        }
        _ => return None,
    })
}

fn mounted_inventory(runtime: &Runtime) -> Value {
    let started = std::time::Instant::now();
    let skills = runtime.skills.read().clone();
    let plugins = runtime.plugins.read().clone();
    let mut issues = Vec::new();
    if skills.is_none() {
        issues.push(json!({"kind":"skill","name":"catalog","message":"Skill catalog is not attached to this runtime"}));
    }
    if plugins.is_none() {
        issues.push(json!({"kind":"plugin","name":"registry","message":"Plugin registry is not attached to this runtime"}));
    }
    let skill_items: Vec<_> = skills.map(|catalog| catalog.list()).unwrap_or_default().into_iter().map(|skill| json!({
        "name":skill.name,"description":skill.description,"source":skill.path.to_string_lossy().replace('\\', "/"),"status":"loaded",
    })).collect();
    let definitions = runtime.tools.definitions();
    let plugin_items: Vec<_> = plugins
        .map(|registry| registry.routing_summaries())
        .unwrap_or_default()
        .into_iter()
        .map(|plugin| {
            let tool_count = definitions
                .iter()
                .filter(|tool| tool.plugin_id.as_deref() == Some(plugin.id.as_str()))
                .count();
            json!({"id":plugin.id,"name":plugin.name,"tool_count":tool_count,"status":"loaded"})
        })
        .collect();
    json!({"type":"complete","skills":skill_items,"plugins":plugin_items,"issues":issues,"elapsed_ms":started.elapsed().as_millis(),"inventory_mode":"mounted"})
}

fn peek_next_startup(outer_home: &Path) -> Result<Option<bool>> {
    // The writer replaces this marker atomically. Reading it must never claim
    // or delete it; only a successfully prepared interactive CLI may consume it.
    match std::fs::read_to_string(outer_home.join("startup-next.txt")) {
        Ok(text) => match text.trim() {
            "on" => Ok(Some(true)),
            "off" => Ok(Some(false)),
            _ => bail!("invalid next startup preference"),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("read next startup preference"),
    }
}

fn bootstrap(host: &Host, runtime: &Runtime) -> Result<Value> {
    let profile = load_startup_profile(&host.outer_home)?;
    let model = runtime.llm.config();
    let inventory = mounted_inventory(runtime);
    let startup = &runtime.config.tui.startup;
    Ok(json!({
        "token":host.token,"workspace":host.workspace,"profile":profile,
        "model":{"name":model.model,"backend":model.backend.label(),"ready":runtime.llm.is_ready(),"local":model.backend.is_local()},
        "status":if runtime.llm.is_ready() {"ready"} else {"missing_credentials"},
        "startup":{"enabled":startup.enabled,"override_enabled":host.startup_override,"next_enabled":peek_next_startup(&host.outer_home)?,"sound":startup.sound,"interactive":startup.interactive,"reduced_motion":startup.reduced_motion},
        "inventory_mode":"mounted","skills":inventory["skills"],"plugins":inventory["plugins"],"issues":inventory["issues"],
        "sessions":crate::app_server::session_summaries(runtime, false),"latest_sequence":runtime.events.latest_sequence(),
    }))
}

fn decoded_asset_path(raw: &str) -> Result<String> {
    let raw = raw.split('?').next().unwrap_or(raw);
    let mut bytes = Vec::with_capacity(raw.len());
    let mut chars = raw.as_bytes().iter().copied();
    while let Some(byte) = chars.next() {
        if byte == b'%' {
            let high = chars
                .next()
                .and_then(|c| (c as char).to_digit(16))
                .context("invalid path encoding")?;
            let low = chars
                .next()
                .and_then(|c| (c as char).to_digit(16))
                .context("invalid path encoding")?;
            bytes.push((high * 16 + low) as u8);
        } else {
            bytes.push(byte);
        }
    }
    let path = String::from_utf8(bytes).context("invalid path encoding")?;
    anyhow::ensure!(
        path.starts_with('/')
            && !path.contains(['\\', ':', '%'])
            && !path.chars().any(char::is_control),
        "invalid asset path"
    );
    anyhow::ensure!(
        !path.split('/').any(|part| part.starts_with('.')),
        "hidden and relative paths are not assets"
    );
    Ok(path)
}

fn static_asset(root: &Path, raw: &str) -> Result<Option<(String, Vec<u8>)>> {
    let path = decoded_asset_path(raw)?;
    let relative = path.trim_start_matches('/');
    let candidate = root.join(if relative.is_empty() {
        "index.html"
    } else {
        relative
    });
    let candidate = match candidate.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("resolve static asset"),
    };
    anyhow::ensure!(
        candidate.starts_with(root) && candidate.is_file(),
        "asset is outside the static directory"
    );
    anyhow::ensure!(
        candidate.metadata()?.len() <= 32 * 1024 * 1024,
        "asset exceeds size limit"
    );
    let mime = match candidate
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    };
    Ok(Some((mime.into(), std::fs::read(candidate)?)))
}

async fn handle(mut socket: TcpStream, host: Arc<Host>) -> Result<()> {
    let request =
        match tokio::time::timeout(Duration::from_secs(10), read_request(&mut socket)).await {
            Ok(Ok(r)) => r,
            _ => return json_response(&mut socket, 400, json!({"error":"Invalid request"})).await,
        };
    if !allowed(&request, &host) {
        return json_response(
            &mut socket,
            403,
            json!({"error":"Local same-origin request required"}),
        )
        .await;
    }
    let path = request.path.split('?').next().unwrap_or(&request.path);
    match (request.method.as_str(), path) {
        ("GET", "/api/harness/bootstrap") if host.runtime.is_some() => {
            match bootstrap(&host, host.runtime.as_ref().unwrap()) {
                Ok(value) => json_response(&mut socket, 200, value).await,
                Err(error) => json_response(&mut socket, 503, json!({"error":{"message":error.to_string()}})).await,
            }
        }
        ("POST", "/api/harness/rpc") if host.runtime.is_some() => {
            let payload: Value = match serde_json::from_slice(&request.body) {
                Ok(value) => value,
                Err(_) => return json_response(&mut socket, 400, json!({"error":{"code":-32700,"message":"Invalid JSON request"}})).await,
            };
            let Some(method) = payload.get("method").and_then(Value::as_str).filter(|value| !value.trim().is_empty()) else {
                return json_response(&mut socket, 400, json!({"error":{"code":-32600,"message":"method must be a non-empty string"}})).await;
            };
            let params = payload.get("params").cloned().unwrap_or_else(|| json!({}));
            if matches!(method, "agent/turn" | "tasks/resume") && params.get("wait") == Some(&Value::Bool(true)) {
                return json_response(&mut socket, 200, json!({"error":{"code":-32602,"message":"Use wait=false and events/wait to follow web turns"}})).await;
            }
            let value = crate::app_server::dispatch_http(host.runtime.as_ref().unwrap().clone(), method, params).await;
            json_response(&mut socket, 200, value).await
        }
        ("POST", "/api/startup-next") => {
            let result = serde_json::from_slice::<Value>(&request.body).map_err(anyhow::Error::from).and_then(|value| {
                let enabled = value.get("enabled").and_then(Value::as_bool).context("enabled must be boolean")?;
                set_next_startup(&host.outer_home, enabled)?;
                Ok(enabled)
            });
            match result {
                Ok(enabled) => json_response(&mut socket, 200, json!({"enabled":enabled,"scope":"next_cli"})).await,
                Err(error) => json_response(&mut socket, 400, json!({"error":{"message":error.to_string()}})).await,
            }
        }
        ("GET", "/api/profile") => match load_startup_profile(&host.outer_home) {
            Ok(profile) => json_response(&mut socket, 200, json!({"username":profile.username,"badge_id":profile.badge_id,"token":host.token,"workspace":host.workspace,"mode":"local","sound":host.sound,"inventory_mode":if host.runtime.is_some() {"mounted"} else {"discover"}})).await,
            Err(e) => json_response(&mut socket, 503, json!({"error":e.to_string()})).await,
        },
        ("POST", "/api/profile") => {
            let result = serde_json::from_slice::<StartupProfile>(&request.body).map_err(anyhow::Error::from)
                .and_then(|profile| { save_startup_profile(&host.outer_home, &profile)?; load_startup_profile(&host.outer_home) });
            match result {
                Ok(profile) => json_response(&mut socket, 200, serde_json::to_value(profile)?).await,
                Err(e) => json_response(&mut socket, 400, json!({"error":e.to_string()})).await,
            }
        }
        ("POST", "/api/load") => {
            if let Some(runtime) = &host.runtime {
                let inventory = mounted_inventory(runtime);
                let mut bytes = serde_json::to_vec(&inventory)?;
                bytes.push(b'\n');
                return response(&mut socket, 200, "application/x-ndjson; charset=utf-8", &bytes).await;
            }
            // A cancelled browser stream does not interrupt synchronous catalog
            // work. A replay waits for it instead of turning overlap into failure.
            let mut inventory = host.inventory.lock().await;
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson; charset=utf-8\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n").await?;
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
            let loader_host = host.clone();
            let worker = tokio::task::spawn_blocking(move || crate::startup_inventory::load(&loader_host.workspace, &loader_host.outer_home, &loader_host.roots, |event| { let _ = tx.send(event); }));
            let mut connected = true;
            while let Some(event) = rx.recv().await {
                if connected {
                    let mut bytes = serde_json::to_vec(&event)?; bytes.push(b'\n');
                    connected = socket.write_all(&bytes).await.is_ok();
                }
            }
            // Keep actual mounted catalogs alive for the lifetime of this preview session.
            match worker.await {
                Ok(loaded) => *inventory = Some(loaded),
                Err(e) if connected => { let mut bytes = serde_json::to_vec(&json!({"type":"error","message":format!("Loader stopped: {e}")}))?; bytes.push(b'\n'); socket.write_all(&bytes).await?; }
                Err(_) => (),
            }
            if connected { socket.shutdown().await?; }
            Ok(())
        }
        ("GET", path) => {
            if path.starts_with("/api/") { return json_response(&mut socket, 404, json!({"error":"Not found"})).await; }
            if let Some(wav) = audio_asset(path) { return response(&mut socket, 200, "audio/wav", wav).await; }
            if let Some(root) = &host.assets {
                // The original startup resources remain embedded and available
                // even when the React asset directory is overridden.
                if path != "/" { if let Some((kind, body)) = asset(path) { return response(&mut socket, 200, kind, body.as_bytes()).await; } }
                match static_asset(root, &request.path) {
                    Ok(Some((kind, body))) => return response(&mut socket, 200, &kind, &body).await,
                    Err(_) => return json_response(&mut socket, 403, json!({"error":"Invalid asset path"})).await,
                    Ok(None) => return json_response(&mut socket, 404, json!({"error":"Not found"})).await,
                }
            }
            match asset(path) {
                Some((kind, body)) => response(&mut socket, 200, kind, body.as_bytes()).await,
                None => json_response(&mut socket, 404, json!({"error":"Not found"})).await,
            }
        },
        _ => json_response(&mut socket, 404, json!({"error":"Not found"})).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AssetFixture(PathBuf);

    impl AssetFixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("dsh-harness-assets-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            Self(root.canonicalize().unwrap())
        }

        fn frontend(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("index.html"), "<html>fixture</html>").unwrap();
            path.canonicalize().unwrap()
        }
    }

    impl Drop for AssetFixture {
        fn drop(&mut self) {
            assert_eq!(self.0.parent(), Some(std::env::temp_dir().canonicalize().unwrap().as_path()));
            assert!(self.0.file_name().unwrap().to_string_lossy().starts_with("dsh-harness-assets-"));
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn harness_assets_explicit_override_wins_and_is_relative_to_cwd() {
        let fixture = AssetFixture::new();
        fixture.frontend("installation/share/dsh/web");
        fixture.frontend("workspace/web/dist");
        let custom = fixture.frontend("workspace/custom");
        let executable = fixture.0.join("installation/bin/dsh.exe");
        let cwd = fixture.0.join("workspace");
        assert_eq!(
            resolve_harness_assets(Some(Path::new("custom")), Some(&executable), &cwd).unwrap(),
            custom
        );
        assert_eq!(
            resolve_harness_assets(Some(&custom), None, &cwd).unwrap(),
            custom
        );
    }

    #[test]
    fn harness_assets_installed_frontend_precedes_cwd_development_fallback() {
        let fixture = AssetFixture::new();
        let installed = fixture.frontend("installation/share/dsh/web");
        let development = fixture.frontend("workspace/web/dist");
        let executable = fixture.0.join("installation/bin/dsh.exe");
        let cwd = fixture.0.join("workspace");
        assert_eq!(resolve_harness_assets(None, Some(&executable), &cwd).unwrap(), installed);
        // The install is also usable from a workspace with no checkout/assets.
        assert_eq!(
            resolve_harness_assets(None, Some(&executable), &fixture.0.join("another-workspace")).unwrap(),
            installed
        );
        std::fs::remove_file(installed.join("index.html")).unwrap();
        assert_eq!(resolve_harness_assets(None, Some(&executable), &cwd).unwrap(), development);
        assert_eq!(resolve_harness_assets(None, None, &cwd).unwrap(), development);
    }

    #[test]
    fn harness_assets_missing_and_invalid_explicit_paths_fail_with_diagnostics() {
        let fixture = AssetFixture::new();
        let executable = fixture.0.join("installation/bin/dsh.exe");
        let cwd = fixture.0.join("workspace");
        let error = resolve_harness_assets(None, Some(&executable), &cwd).unwrap_err().to_string();
        assert!(error.contains(&fixture.0.join("installation/share/dsh/web").display().to_string()));
        assert!(error.contains(&cwd.join("web/dist").display().to_string()));
        assert!(error.contains("--assets"));

        fixture.frontend("installation/share/dsh/web");
        fixture.frontend("workspace/web/dist");
        let empty = cwd.join("empty");
        std::fs::create_dir(&empty).unwrap();
        for path in [Path::new("missing"), Path::new("empty")] {
            let error = resolve_harness_assets(Some(path), Some(&executable), &cwd).unwrap_err().to_string();
            assert!(error.contains(&cwd.join(path).display().to_string()));
        }
    }

    struct TestHost {
        host: Arc<Host>,
        task: tokio::task::JoinHandle<()>,
        root: PathBuf,
    }

    impl Drop for TestHost {
        fn drop(&mut self) {
            self.task.abort();
            let temp = std::env::temp_dir().canonicalize().unwrap();
            assert_eq!(self.root.parent(), Some(temp.as_path()));
            assert!(self
                .root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("dsh-harness-http-"));
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    async fn harness(llm_endpoint: Option<String>) -> TestHost {
        let root = std::env::temp_dir().join(format!("dsh-harness-http-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let mut config = dsh_core::AppConfig::builtin_default();
        config.paths.outer_home = root.join("outer").display().to_string();
        config.agent.scheduler_enabled = false;
        config.ctm.enabled = false;
        let mut llm_config = config.to_llm_config(String::new());
        if let Some(endpoint) = llm_endpoint {
            llm_config.base_url = endpoint;
            llm_config.api_key = "only-local-fixture-secret".into();
        }
        let llm = dsh_llm::DeepSeekClient::new(llm_config).unwrap();
        let tools = Arc::new(dsh_tools::ToolRegistry::new());
        let runtime = Runtime::bootstrap(config, root.clone(), llm, tools.clone()).unwrap();
        let skills = Arc::new(dsh_skill::SkillCatalog::new(root.join("meta")));
        let skill = root.join("http-fixture.md");
        std::fs::write(
            &skill,
            "---\nname: http-fixture\n---\nA real locally mounted fixture",
        )
        .unwrap();
        skills
            .mount_path(&skill, dsh_skill::SkillSource::Runtime)
            .unwrap();
        let plugin = root.join("plugin");
        std::fs::create_dir(&plugin).unwrap();
        std::fs::write(plugin.join("plugin.json"), r#"{"id":"fixture","name":"Fixture Plugin","version":"1","tools":[{"name":"echo","description":"real definition"}]}"#).unwrap();
        let plugins = Arc::new(dsh_plugin::PluginRegistry::new(tools, root.join("meta")));
        plugins.mount_dir(&plugin).unwrap();
        *runtime.skills.write() = Some(skills);
        *runtime.plugins.write() = Some(plugins);
        let assets = root.join("dist");
        std::fs::create_dir(&assets).unwrap();
        std::fs::write(
            assets.join("index.html"),
            "<html>Real Harness fixture</html>",
        )
        .unwrap();
        std::fs::write(root.join("secret.txt"), "not a web asset").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = Arc::new(Host {
            workspace: root.clone(),
            outer_home: runtime.outer_home.clone(),
            roots: vec![],
            authority: listener.local_addr().unwrap().to_string(),
            token: uuid::Uuid::new_v4().to_string(),
            sound: false,
            inventory: Mutex::new(None),
            runtime: Some(runtime),
            assets: Some(assets),
            startup_override: None,
        });
        let shared = host.clone();
        let task = tokio::spawn(async move {
            accept_connections(listener, shared).await.unwrap();
        });
        TestHost { host, task, root }
    }

    async fn http(
        host: &Host,
        method: &str,
        path: &str,
        payload: Value,
        token: bool,
    ) -> (u16, Value) {
        let mut socket = TcpStream::connect(&host.authority).await.unwrap();
        let body = if method == "POST" {
            serde_json::to_vec(&payload).unwrap()
        } else {
            vec![]
        };
        let auth = if token {
            format!("X-DSH-Token: {}\r\n", host.token)
        } else {
            String::new()
        };
        let headers = format!("{method} {path} HTTP/1.1\r\nHost: {}\r\nOrigin: http://{}\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",host.authority,host.authority,body.len());
        socket.write_all(headers.as_bytes()).await.unwrap();
        socket.write_all(&body).await.unwrap();
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), socket.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        let boundary = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        let headers = std::str::from_utf8(&bytes[..boundary]).unwrap();
        let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
        let body = serde_json::from_slice(&bytes[boundary..]).unwrap_or_else(|_| {
            Value::String(String::from_utf8_lossy(&bytes[boundary..]).into_owned())
        });
        (status, body)
    }

    async fn rpc(host: &Host, method: &str, params: Value) -> Value {
        let (status, value) = http(
            host,
            "POST",
            "/api/harness/rpc",
            json!({"method":method,"params":params}),
            true,
        )
        .await;
        assert_eq!(status, 200);
        value
    }

    #[tokio::test]
    async fn harness_http_exposes_real_state_and_rejects_unauthenticated_writes() {
        let server = harness(None).await;
        let host = &server.host;
        let (status, initial) =
            http(host, "GET", "/api/harness/bootstrap", Value::Null, false).await;
        assert_eq!(status, 200);
        assert_eq!(initial["skills"][0]["name"], "http-fixture");
        assert_eq!(initial["plugins"][0]["tool_count"], 1);
        assert_eq!(initial["status"], "missing_credentials");
        assert_eq!(initial["inventory_mode"], "mounted");
        let unauthenticated = http(
            host,
            "POST",
            "/api/harness/rpc",
            json!({"method":"sessions/create"}),
            false,
        )
        .await;
        assert_eq!(unauthenticated.0, 403);
        assert!(host
            .runtime
            .as_ref()
            .unwrap()
            .sessions
            .list_ids()
            .is_empty());
        let created = rpc(host, "sessions/create", json!({"name":"HTTP session"})).await;
        let id = created["result"]["session"]["id"].as_str().unwrap();
        let listed = rpc(host, "sessions/list", json!({})).await;
        assert_eq!(listed["result"]["sessions"][0]["id"], id);
        let rejected = rpc(
            host,
            "agent/turn",
            json!({"session_id":id,"prompt":"must not fabricate an answer","wait":false}),
        )
        .await;
        assert_eq!(rejected["error"]["code"], -32002);
        let fetched = rpc(host, "sessions/get", json!({"id":id})).await;
        assert!(fetched["result"]["session"]["events"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(rpc(host, "sessions/get", json!({"id":"../secret"}))
            .await
            .get("error")
            .is_some());
        let (status, profile) = http(host, "GET", "/api/profile", Value::Null, false).await;
        assert_eq!(status, 200);
        assert_eq!(profile["inventory_mode"], "mounted");
        let runtime = host.runtime.as_ref().unwrap();
        let generation = runtime.skills.read().as_ref().unwrap().generation();
        let (status, inventory) = http(host, "POST", "/api/load", json!({}), true).await;
        assert_eq!(status, 200);
        assert_eq!(inventory["skills"], initial["skills"]);
        assert_eq!(
            runtime.skills.read().as_ref().unwrap().generation(),
            generation
        );
        assert!(host.inventory.lock().await.is_none());
        let (status, preference) = http(
            host,
            "POST",
            "/api/startup-next",
            json!({"enabled":true}),
            true,
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(preference["scope"], "next_cli");
        let (_, again) = http(host, "GET", "/api/harness/bootstrap", Value::Null, false).await;
        assert_eq!(again["startup"]["next_enabled"], true);
        http(host, "POST", "/api/load", json!({}), true).await;
        assert_eq!(
            dsh_core::take_next_startup(&host.outer_home).unwrap(),
            Some(true)
        );
        assert_eq!(dsh_core::take_next_startup(&host.outer_home).unwrap(), None);
    }

    #[tokio::test]
    async fn harness_static_files_preserve_embedded_preview_and_block_traversal() {
        let server = harness(None).await;
        let (status, body) = http(&server.host, "GET", "/", Value::Null, false).await;
        assert_eq!(status, 200);
        assert!(body.as_str().unwrap().contains("Real Harness fixture"));
        for path in ["/startup-preview.html", "/startup-embed.js"] {
            assert_eq!(
                http(&server.host, "GET", path, Value::Null, false).await.0,
                200
            );
        }
        for path in [
            "/../secret.txt",
            "/%2e%2e/secret.txt",
            "/%252e%252e/secret.txt",
            "/a%5c..%5csecret.txt",
            "/C%3A/secret.txt",
        ] {
            assert_eq!(
                http(&server.host, "GET", path, Value::Null, false).await.0,
                403,
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn startup_override_is_distinct_from_configuration_and_next_cli() {
        let server = harness(None).await;
        set_next_startup(&server.host.outer_home, true).unwrap();
        let runtime = server.host.runtime.as_ref().unwrap();
        for override_enabled in [None, Some(false), Some(true)] {
            let host = Host {
                workspace: server.host.workspace.clone(),
                outer_home: server.host.outer_home.clone(),
                roots: vec![],
                authority: server.host.authority.clone(),
                token: server.host.token.clone(),
                sound: false,
                inventory: Mutex::new(None),
                runtime: Some(runtime.clone()),
                assets: None,
                startup_override: override_enabled,
            };
            let value = bootstrap(&host, runtime).unwrap();
            assert_eq!(
                value["startup"]["override_enabled"],
                json!(override_enabled)
            );
            assert_eq!(value["startup"]["enabled"], false);
            assert_eq!(value["startup"]["next_enabled"], true);
        }
        assert_eq!(
            peek_next_startup(&server.host.outer_home).unwrap(),
            Some(true)
        );
    }

    #[tokio::test]
    async fn harness_two_real_streamed_turns_are_correlated_and_persisted() {
        let mock = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", mock.local_addr().unwrap());
        let mock_task = tokio::spawn(async move {
            for suffix in ["one", "two"] {
                let (mut socket, _) = mock.accept().await.unwrap();
                let request = read_request(&mut socket).await.unwrap();
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert!(body["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|m| m["role"] == "user"));
                if suffix == "two" {
                    assert!(body.to_string().contains("reply one"));
                }
                // Keep the first real request pending while two tabs race to
                // submit the same session. Only one may enter the agent loop.
                tokio::time::sleep(Duration::from_millis(80)).await;
                let frames = [
                    format!(
                        "data: {}\n\n",
                        json!({"choices":[{"delta":{"content":"reply "},"finish_reason":null}]})
                    ),
                    format!(
                        "data: {}\n\n",
                        json!({"choices":[{"delta":{"content":suffix},"finish_reason":"stop"}]})
                    ),
                    "data: [DONE]\n\n".into(),
                ];
                let length: usize = frames.iter().map(String::len).sum();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
                for frame in frames {
                    socket.write_all(frame.as_bytes()).await.unwrap();
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        });
        let server = harness(Some(endpoint)).await;
        let (_, bootstrap) = http(
            &server.host,
            "GET",
            "/api/harness/bootstrap",
            Value::Null,
            false,
        )
        .await;
        assert!(!bootstrap.to_string().contains("only-local-fixture-secret"));
        assert_eq!(bootstrap["model"]["ready"], true);
        let created = rpc(
            &server.host,
            "sessions/create",
            json!({"name":"Streamed HTTP"}),
        )
        .await;
        let id = created["result"]["session"]["id"].as_str().unwrap();
        let mut cursor = bootstrap["latest_sequence"].as_u64().unwrap();
        let mut observed = Vec::new();
        for suffix in ["one", "two"] {
            let params =
                json!({"session_id":id,"prompt":format!("HTTP turn {suffix}"),"wait":false});
            if suffix == "one" {
                let (first, second) = tokio::join!(
                    rpc(&server.host, "agent/turn", params.clone()),
                    rpc(&server.host, "agent/turn", params)
                );
                let responses = [first, second];
                assert_eq!(
                    responses
                        .iter()
                        .filter(|value| value["result"]["accepted"] == true)
                        .count(),
                    1,
                    "{responses:?}"
                );
                assert_eq!(
                    responses
                        .iter()
                        .filter(|value| value["error"]["code"] == -32003)
                        .count(),
                    1,
                    "{responses:?}"
                );
            } else {
                let accepted = rpc(&server.host, "agent/turn", params).await;
                assert_eq!(accepted["result"]["accepted"], true, "{accepted}");
            }
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            let mut streamed = String::new();
            loop {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "timed out waiting for real agent events"
                );
                let batch = rpc(
                    &server.host,
                    "events/wait",
                    json!({"sequence":cursor,"limit":2,"timeout_ms":500}),
                )
                .await;
                let events = batch["result"]["events"].as_array().unwrap();
                let mut done = false;
                for event in events {
                    let sequence = event["sequence"].as_u64().unwrap();
                    assert!(sequence > cursor);
                    cursor = sequence;
                    if event["payload"]["session_id"] == id {
                        if event["event_type"] == "agent.text_delta" {
                            streamed.push_str(event["payload"]["text"].as_str().unwrap());
                        }
                        if event["event_type"] == "agent.done" {
                            done = true;
                        }
                        assert_ne!(event["event_type"], "agent.error", "{event}");
                    }
                    observed.push(event.clone());
                }
                if done {
                    break;
                }
            }
            assert_eq!(streamed, format!("reply {suffix}"));
        }
        mock_task.await.unwrap();
        let fetched = rpc(&server.host, "sessions/get", json!({"id":id})).await;
        let messages = fetched["result"]["session"]["events"].as_array().unwrap();
        for (kind, count) in [("user_message", 2), ("assistant_message", 2)] {
            assert_eq!(
                messages
                    .iter()
                    .filter(|event| event["type"] == kind)
                    .count(),
                count
            );
        }
        let disk = dsh_core::Session::load_json(
            &server
                .host
                .outer_home
                .join("sessions")
                .join(format!("{id}.json")),
        )
        .unwrap();
        assert_eq!(
            disk.events
                .iter()
                .filter(|event| matches!(event, dsh_core::SessionEvent::AssistantMessage { .. }))
                .count(),
            2
        );
        assert_eq!(
            observed
                .iter()
                .filter(|event| event["event_type"] == "agent.done")
                .count(),
            2
        );
    }
    #[test]
    fn local_requests_require_exact_host_origin_and_write_token() {
        let host = Host {
            workspace: PathBuf::new(),
            outer_home: PathBuf::new(),
            roots: vec![],
            authority: "127.0.0.1:8769".into(),
            token: "test-token".into(),
            sound: false,
            inventory: Mutex::new(None),
            runtime: None,
            assets: None,
            startup_override: None,
        };
        let mut r = Request {
            method: "GET".into(),
            path: "/api/profile".into(),
            headers: HashMap::from([("host".into(), host.authority.clone())]),
            body: vec![],
        };
        assert!(allowed(&r, &host));
        r.headers
            .insert("origin".into(), "https://example.com".into());
        assert!(!allowed(&r, &host));
        r.headers.remove("origin");
        r.method = "POST".into();
        assert!(!allowed(&r, &host));
        r.headers.insert("x-dsh-token".into(), host.token.clone());
        assert!(allowed(&r, &host));
        r.headers
            .insert("host".into(), "attacker.example:8769".into());
        assert!(!allowed(&r, &host));
    }
    #[test]
    fn assets_are_embedded_and_paths_allowlisted() {
        assert!(asset("/startup-preview.html?v=1").is_some());
        assert!(asset("/startup-local.js").is_some());
        assert!(asset("/../config/default.toml").is_none());
        assert!(asset("/api/profile").is_none());
        assert!(audio_asset("/assets/voice/phase-5.wav")
            .unwrap()
            .starts_with(b"RIFF"));
        assert!(audio_asset("/assets/voice/../../config/default.toml").is_none());
    }
}
