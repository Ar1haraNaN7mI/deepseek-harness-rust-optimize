//! Share a matching local service; only tear down a service created by this app.
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::net::TcpListener;

enum Service {
    Shared { authority: String },
    Owned(TcpListener),
}

fn same_workspace(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

async fn prepare(
    port: Option<u16>,
    remembered: Option<u16>,
    workspace: &Path,
    allow_reuse: bool,
) -> Result<Service> {
    let preferred = port.or(remembered).unwrap_or(8770);
    match TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, preferred)).await {
        Ok(listener) => return Ok(Service::Owned(listener)),
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => (),
        Err(error) => return Err(error).context("bind desktop Harness service"),
    }
    let authority = format!("127.0.0.1:{preferred}");
    if allow_reuse {
        if let Ok(existing) =
            crate::web_launch::probe(&crate::web_launch::client()?, &authority).await
        {
            if same_workspace(Path::new(&existing.workspace), workspace) {
                return Ok(Service::Shared { authority });
            }
        }
    }
    anyhow::ensure!(
        port.is_none(),
        "端口 {preferred} 不能复用当前工作区。请省略 --port 自动选择，或使用 --port 0。"
    );
    Ok(Service::Owned(
        TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?,
    ))
}

pub async fn launch(
    cli: &crate::Cli,
    workspace: &Path,
    config_path: Option<&PathBuf>,
    port: Option<u16>,
    assets: Option<PathBuf>,
    smoke_test: bool,
) -> Result<()> {
    anyhow::ensure!(
        cfg!(windows),
        "The native desktop client currently supports Windows; use `dsh web` on this platform."
    );
    anyhow::ensure!(
        workspace.is_dir(),
        "工作区路径必须是目录：{}",
        workspace.display()
    );
    let config = crate::load_app_config(workspace, config_path)?;
    let data_directory = config.resolve_outer_home()?.join("desktop/webview");
    let identity = format!(
        "{}|{}",
        workspace.canonicalize()?.display(),
        std::path::absolute(&data_directory)?.display()
    )
    .to_lowercase();
    let instance_key = hex::encode(Sha256::digest(identity.as_bytes()));
    let _instance = match dsh_desktop::acquire_instance(&instance_key)? {
        dsh_desktop::Instance::Primary(guard) => guard,
        dsh_desktop::Instance::Existing => {
            println!("已打开该工作区的 DSH 桌面窗口。");
            return Ok(());
        }
    };
    let port_file = data_directory
        .parent()
        .context("desktop data directory missing")?
        .join("ports")
        .join(format!("{instance_key}.port"));
    let remembered = std::fs::read_to_string(&port_file)
        .ok()
        .and_then(|text| text.trim().parse::<u16>().ok())
        .filter(|port| *port != 0);
    let startup = if cli.no_startup {
        Some(false)
    } else if cli.startup {
        Some(true)
    } else {
        None
    };
    // Explicit runtime overrides require an owned runtime so they cannot be
    // silently discarded or mutate a service another window is using.
    let allow_reuse = assets.is_none()
        && cli.model.is_none()
        && cli.backend.is_none()
        && cli.config.is_none()
        && cli.config_override.is_empty()
        && !cli.silent
        && cli.permissions.is_none()
        && cli.sandbox.is_none()
        && cli.ask_for_approval.is_none()
        && cli.add_dir.is_empty()
        && !cli.yolo
        && !cli.dangerously_bypass
        && cli.enable.is_empty()
        && cli.disable.is_empty()
        && !cli.search
        && cli.model_optimization.is_none()
        && cli.model_size_b.is_none();
    let selected = prepare(port, remembered, workspace, allow_reuse).await?;
    let (authority, mut owned) = match selected {
        Service::Shared { authority } => {
            println!("DSH desktop: reusing Harness at http://{authority}/");
            (authority, None)
        }
        Service::Owned(listener) => {
            crate::startup_web::validate_frontend_assets(assets.as_deref())?;
            let authority = listener.local_addr()?.to_string();
            let boot = crate::boot_tui(workspace, config_path, cli)?;
            crate::apply_cli_overrides(&boot.runtime, cli)?;
            let handle = tokio::spawn(crate::startup_web::serve_harness(
                boot.runtime,
                listener,
                assets,
                startup,
                false,
            ));
            (authority, Some(handle))
        }
    };
    let client = crate::web_launch::client()?;
    let ready = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Ok(service) = crate::web_launch::probe(&client, &authority).await {
                return service;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let service = match ready {
        Ok(service) => service,
        Err(_) => {
            if let Some(handle) = owned.take() {
                handle.abort();
            }
            anyhow::bail!("Desktop Harness service did not become ready");
        }
    };
    // A stable origin preserves theme, pet and other origin-scoped preferences.
    // Explicit --port choices (including --port 0) do not replace this affinity.
    if port.is_none() {
        if let Some(directory) = port_file.parent() {
            std::fs::create_dir_all(directory)?;
        }
        std::fs::write(
            &port_file,
            authority
                .rsplit_once(':')
                .context("desktop service port missing")?
                .1,
        )?;
    }
    let result = dsh_desktop::run(dsh_desktop::DesktopOptions {
        instance_key,
        url: crate::web_launch::launch_url(&authority, startup),
        data_directory,
        workspace_label: workspace
            .file_name()
            .unwrap_or(workspace.as_os_str())
            .to_string_lossy()
            .into_owned(),
        smoke_test,
    });
    let was_owned = owned.is_some();
    if let Some(mut handle) = owned {
        let port = authority
            .rsplit_once(':')
            .context("desktop service port missing")?
            .1
            .parse()?;
        if let Err(error) = crate::web_launch::stop(&client, &authority, port, &service).await {
            // We may abort only our own in-process task; a port's new occupant
            // is never shut down by an obsolete desktop window.
            tracing::warn!(%error, "Desktop service stopped or changed before window close");
        }
        match tokio::time::timeout(Duration::from_secs(3), &mut handle).await {
            Ok(joined) => {
                joined??;
            }
            Err(_) => handle.abort(),
        }
    }
    let outcome = result?;
    if outcome.smoke_test {
        println!(
            "{}",
            serde_json::json!({"desktop":"native-webview2", "page_loaded":outcome.page_loaded,"frontend_ready":outcome.frontend_ready,"owned_service":was_owned,"closed":true})
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workspace_matching_resolves_relative_components_and_missing_paths() {
        let current = std::env::current_dir().unwrap();
        assert!(same_workspace(&current, &current.join(".")));
        assert!(!same_workspace(
            &current,
            &current.join("does-not-exist-desktop-test")
        ));
    }
    #[tokio::test]
    async fn occupied_unrelated_port_is_not_reused_or_closed() {
        let occupied = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = occupied.local_addr().unwrap().port();
        assert!(
            prepare(Some(port), None, &std::env::current_dir().unwrap(), false)
                .await
                .is_err()
        );
        assert!(TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .is_err());
        assert!(matches!(
            prepare(Some(0), None, &std::env::current_dir().unwrap(), false)
                .await
                .unwrap(),
            Service::Owned(_)
        ));
    }

    #[tokio::test]
    async fn remembered_origin_is_rebound_before_the_default_port() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let Service::Owned(listener) =
            prepare(None, Some(port), &std::env::current_dir().unwrap(), false)
                .await
                .unwrap()
        else {
            panic!("expected owned listener");
        };
        assert_eq!(listener.local_addr().unwrap().port(), port);
    }
}
