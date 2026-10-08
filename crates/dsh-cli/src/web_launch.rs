//! Resolve an occupied Harness port before constructing another Runtime.
use anyhow::{bail, Context, Result};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use std::{
    io::{BufRead, Write},
    path::Path,
    time::Duration,
};
use tokio::net::TcpListener;

pub(crate) struct ExistingService {
    pub(crate) workspace: String,
    token: String,
    instance_id: Option<String>,
}

#[derive(Debug, PartialEq)]
enum Choice {
    Open,
    Restart,
}

fn confirm(reader: &mut impl BufRead, output: &mut impl Write, prompt: &str) -> Result<bool> {
    loop {
        write!(output, "{prompt} [y/N]: ")?;
        output.flush()?;
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            bail!("已取消：输入已关闭。");
        }
        match line.trim().to_lowercase().as_str() {
            "y" | "yes" | "是" => return Ok(true),
            "" | "n" | "no" | "否" => return Ok(false),
            _ => writeln!(output, "请输入 y（是）或 n（否）。")?,
        }
    }
}

fn choose(reader: &mut impl BufRead, output: &mut impl Write) -> Result<Choice> {
    loop {
        if confirm(reader, output, "是否直接打开已有 Harness 窗口？")? {
            return Ok(Choice::Open);
        }
        if confirm(reader, output, "是否关闭已有服务，并在当前工作区重新启动？")?
        {
            return Ok(Choice::Restart);
        }
        writeln!(output, "返回启动选项（Ctrl+C 可退出）。")?;
    }
}

pub fn launch_url(authority: &str, startup: Option<bool>) -> String {
    let suffix = match startup {
        Some(true) => "?dsh-startup=on",
        Some(false) => "?dsh-startup=off",
        None => "",
    };
    format!("http://{authority}/{suffix}")
}

pub(crate) fn client() -> Result<Client> {
    Ok(Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()?)
}

async fn limited_json(mut response: reqwest::Response) -> Result<Value> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            bytes.len() + chunk.len() <= 2 * 1024 * 1024,
            "Local service response is too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes).context("Local service did not return Harness JSON")?)
}

pub(crate) async fn probe(client: &Client, authority: &str) -> Result<ExistingService> {
    let base = format!("http://{authority}");
    let response = client
        .get(format!("{base}/api/harness/bootstrap"))
        .send()
        .await?;
    anyhow::ensure!(
        response.status() == StatusCode::OK,
        "该端口上的服务不是可识别的 DSH Harness，请使用 --port 选择其他端口。"
    );
    let bootstrap = limited_json(response).await?;
    anyhow::ensure!(
        bootstrap["inventory_mode"] == "mounted"
            && bootstrap["skills"].is_array()
            && bootstrap["plugins"].is_array()
            && bootstrap["sessions"].is_array()
            && bootstrap["startup"].is_object(),
        "该端口返回了其他服务的数据，未对其执行任何关闭操作。"
    );
    let token = bootstrap["token"]
        .as_str()
        .filter(|s| uuid::Uuid::parse_str(s).is_ok())
        .context("Harness identity token is invalid")?
        .to_owned();
    let workspace = bootstrap["workspace"]
        .as_str()
        .context("Harness workspace is missing")?
        .to_owned();
    let response = client
        .get(format!("{base}/api/harness/service"))
        .header("X-DSH-Token", &token)
        .send()
        .await?;
    let instance_id = if response.status() == StatusCode::NOT_FOUND {
        None // Older DSH versions did not have a graceful shutdown endpoint.
    } else {
        anyhow::ensure!(
            response.status() == StatusCode::OK,
            "无法验证已有 Harness 服务。"
        );
        let service = limited_json(response).await?;
        anyhow::ensure!(
            service["service"] == "dsh-harness"
                && service["protocol_version"] == 1
                && service["can_shutdown"] == true,
            "不支持此 Harness 服务的控制协议。"
        );
        Some(
            service["instance_id"]
                .as_str()
                .filter(|s| uuid::Uuid::parse_str(s).is_ok())
                .context("Harness instance identity is invalid")?
                .to_owned(),
        )
    };
    Ok(ExistingService {
        workspace,
        token,
        instance_id,
    })
}

pub(crate) async fn stop(
    client: &Client,
    authority: &str,
    port: u16,
    previous: &ExistingService,
) -> Result<()> {
    let current = probe(client, authority)
        .await
        .context("服务状态已变化，请重新运行 dsh web")?;
    anyhow::ensure!(
        current.token == previous.token && current.instance_id == previous.instance_id,
        "该端口的服务已经变化，未关闭新服务。请重新运行 dsh web。"
    );
    if let Some(instance_id) = &current.instance_id {
        let response = client
            .post(format!("http://{authority}/api/harness/shutdown"))
            .header("X-DSH-Token", &current.token)
            .json(&json!({"instance_id":instance_id}))
            .send()
            .await?;
        anyhow::ensure!(
            response.status() == StatusCode::OK,
            "已有 Harness 拒绝了关闭请求。"
        );
        let ack = limited_json(response).await?;
        anyhow::ensure!(
            ack["accepted"] == true && ack["instance_id"] == *instance_id,
            "Harness shutdown was not acknowledged"
        );
    } else {
        crate::web_legacy::stop_legacy(port, &current.token).await?;
    }
    Ok(())
}

pub async fn prepare(
    port: u16,
    workspace: &Path,
    startup: Option<bool>,
    interactive: bool,
    open_window: bool,
    validate_replacement: impl Fn() -> Result<()>,
) -> Result<Option<TcpListener>> {
    let address = (std::net::Ipv4Addr::LOCALHOST, port);
    match TcpListener::bind(address).await {
        Ok(listener) => {
            validate_replacement()?;
            return Ok(Some(listener));
        }
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => (),
        Err(error) => return Err(error).context("bind Harness web host"),
    }
    let authority = format!("127.0.0.1:{port}");
    let client = client()?;
    let existing = probe(&client, &authority)
        .await
        .context("端口已被占用，无法确认可复用的 Harness；可用 --port 指定其他端口")?;
    println!(
        "发现已运行的 DSH Harness：http://{authority}/\n已有工作区：{}\n当前工作区：{}",
        existing.workspace,
        workspace.display()
    );
    anyhow::ensure!(
        interactive,
        "非交互模式不会询问或关闭已有服务。请在终端重新运行以选择打开/重启，或指定其他 --port。"
    );
    let choice = choose(&mut std::io::stdin().lock(), &mut std::io::stdout().lock())?;
    match choice {
        Choice::Open => {
            let current = probe(&client, &authority).await?;
            anyhow::ensure!(
                current.token == existing.token,
                "服务已变化，请重新运行 dsh web。"
            );
            let url = launch_url(&authority, startup);
            if open_window {
                open_browser(&url)?;
            }
            println!("使用已有 Harness：{url}");
            Ok(None)
        }
        Choice::Restart => {
            validate_replacement()?;
            stop(&client, &authority, port, &existing).await?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                match TcpListener::bind(address).await {
                    Ok(listener) => {
                        println!("原服务已关闭，正在启动当前工作区。");
                        return Ok(Some(listener));
                    }
                    Err(error)
                        if error.kind() == std::io::ErrorKind::AddrInUse
                            && tokio::time::Instant::now() < deadline =>
                    {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    Err(error) => {
                        return Err(error).context("旧服务尚未释放端口，请稍后重试；未关闭其他进程")
                    }
                }
            }
        }
    }
}

pub fn open_browser(url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url)?;
    anyhow::ensure!(
        parsed.scheme() == "http"
            && parsed.host_str() == Some("127.0.0.1")
            && parsed.port_or_known_default().is_some()
            && parsed.username().is_empty()
            && parsed.password().is_none(),
        "Only a local Harness URL can be opened"
    );
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "shell32")]
        extern "system" {
            fn ShellExecuteW(
                window: *mut std::ffi::c_void,
                operation: *const u16,
                file: *const u16,
                parameters: *const u16,
                directory: *const u16,
                show: i32,
            ) -> isize;
        }
        let wide = |text: &str| {
            std::ffi::OsStr::new(text)
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>()
        };
        let action = wide("open");
        let file = wide(url);
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                action.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
            )
        };
        anyhow::ensure!(
            result > 32,
            "无法打开默认浏览器（Windows 错误 {result}），请手动打开 {url}"
        );
    }
    #[cfg(not(windows))]
    {
        let program = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let mut child = std::process::Command::new(program)
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("无法打开默认浏览器，请手动打开 {url}"))?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn choices_open_restart_repeat_and_eof() {
        for (answers, expected, questions) in [
            ("y\n", Choice::Open, 1),
            ("n\ny\n", Choice::Restart, 2),
            ("n\nn\ny\n", Choice::Open, 3),
        ] {
            let mut output = Vec::new();
            assert_eq!(
                choose(&mut answers.as_bytes(), &mut output).unwrap(),
                expected
            );
            assert_eq!(
                String::from_utf8(output).unwrap().matches("[y/N]").count(),
                questions
            );
        }
        assert!(choose(&mut "n\n".as_bytes(), &mut Vec::new()).is_err());
    }
    #[test]
    fn invalid_answers_do_not_restart_and_defaults_are_no() {
        let mut output = Vec::new();
        assert_eq!(
            choose(&mut "invalid\n\n\nYES\n".as_bytes(), &mut output).unwrap(),
            Choice::Open
        );
        assert!(String::from_utf8(output).unwrap().contains("请输入"));
    }
    #[test]
    fn invocation_startup_flags_are_carried_in_the_opened_url() {
        assert_eq!(
            launch_url("127.0.0.1:8770", Some(true)),
            "http://127.0.0.1:8770/?dsh-startup=on"
        );
        assert_eq!(
            launch_url("127.0.0.1:8770", Some(false)),
            "http://127.0.0.1:8770/?dsh-startup=off"
        );
        assert_eq!(launch_url("127.0.0.1:8770", None), "http://127.0.0.1:8770/");
    }
    #[test]
    fn browser_opener_rejects_remote_and_non_http_targets() {
        for target in [
            "https://example.com",
            "file:///tmp/test",
            "http://127.0.0.1.evil:8770/",
            "http://user@127.0.0.1:8770/",
        ] {
            assert!(open_browser(target).is_err());
        }
    }
}
