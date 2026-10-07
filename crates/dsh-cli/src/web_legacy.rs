//! Confirmed-restart fallback for local servers predating the shutdown API.
//! The caller must identify the legacy Harness and obtain confirmation first.

use anyhow::Result;

/// Stop only a verified local `dsh web` listener. This never stops a process
/// tree, a wildcard listener, or another command merely containing "web".
pub async fn stop_legacy(port: u16, expected_token: &str) -> Result<()> {
    anyhow::ensure!(port != 0, "a specific occupied port is required");
    #[cfg(windows)]
    {
        windows::stop(port, expected_token).await
    }
    #[cfg(not(windows))]
    {
        let _ = expected_token;
        anyhow::bail!(
            "This legacy server cannot be restarted automatically on this platform. Stop its dsh web process manually or choose another --port."
        )
    }
}

#[cfg(any(windows, test))]
fn is_web_command(arguments: &[String]) -> bool {
    // Conservative recognition of the existing global flags. Unknown options
    // are rejected instead of guessing whether their next value is a command.
    const VALUES: &[&str] = &[
        "--workspace",
        "--config",
        "--model",
        "--backend",
        "--model-optimization",
        "--model-size-b",
        "--permissions",
        "--sandbox",
        "--ask-for-approval",
        "--config-override",
        "--add-dir",
        "--cd",
        "--enable",
        "--disable",
    ];
    const SWITCHES: &[&str] = &[
        "--startup",
        "--no-startup",
        "--silent",
        "--yolo",
        "--dangerously-bypass-approvals-and-sandbox",
        "--search",
    ];
    let mut index = 1;
    while let Some(argument) = arguments.get(index) {
        if argument == "web" {
            return true;
        }
        if SWITCHES.contains(&argument.as_str()) {
            index += 1;
        } else if VALUES.contains(&argument.as_str())
            || matches!(argument.as_str(), "-m" | "-s" | "-a" | "-c" | "-C")
        {
            if arguments.get(index + 1).is_none() {
                return false;
            }
            index += 2;
        } else if argument
            .split_once('=')
            .is_some_and(|(name, _)| VALUES.contains(&name))
            || ["-m", "-s", "-a", "-c", "-C"]
                .iter()
                .any(|prefix| argument.starts_with(prefix) && argument.len() > prefix.len())
        {
            index += 1;
        } else {
            return false;
        }
    }
    false
}

#[cfg(windows)]
mod windows {
    use super::*;
    use anyhow::{bail, Context};
    use serde::Deserialize;
    use std::ffi::c_void;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    // Only the validated u16 port is inserted. No user paths/commands are ever
    // inserted into PowerShell source, and captured command lines stay private.
    const INSPECT: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$dshPort = __PORT__
try {
    $owners = @(Get-CimInstance -Namespace root/StandardCimv2 -ClassName MSFT_NetTCPConnection -Filter "LocalPort = $dshPort" |
        Where-Object { $_.LocalAddress -eq '127.0.0.1' -and $_.State -eq 2 } |
        Select-Object -ExpandProperty OwningProcess -Unique)
    if ($owners.Count -ne 1) { exit 2 }
    $ownerId = [uint32]$owners[0]
    $owner = Get-CimInstance -ClassName Win32_Process -Filter "ProcessId = $ownerId"
    if ($null -eq $owner -or [string]::IsNullOrWhiteSpace($owner.ExecutablePath) -or
        [string]::IsNullOrWhiteSpace($owner.CommandLine) -or $null -eq $owner.CreationDate) { exit 3 }
    [pscustomobject]@{
        pid = $ownerId
        executable = [string]$owner.ExecutablePath
        command_line = [string]$owner.CommandLine
        creation_filetime = [string]$owner.CreationDate.ToFileTimeUtc()
    } | ConvertTo-Json -Compress
} catch { exit 4 }
"#;

    #[derive(Deserialize, PartialEq, Eq)]
    struct Owner {
        pid: u32,
        executable: String,
        command_line: String,
        creation_filetime: String,
    }

    async fn inspect(port: u16) -> Result<Owner> {
        let system_root = std::env::var_os("SystemRoot")
            .context("Windows system directory is unavailable; stop the old server manually")?;
        let powershell =
            PathBuf::from(system_root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut command = tokio::process::Command::new(powershell);
        command
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
            ])
            .arg(INSPECT.replace("__PORT__", &port.to_string()))
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(12), command.output())
            .await
            .context("Timed out identifying the legacy listener; stop it manually or choose another --port")?
            .context("Could not inspect the legacy listener; stop it manually or choose another --port")?;
        // Do not forward PowerShell output/errors: they can contain private
        // command-line options, configuration paths, or environment values.
        anyhow::ensure!(output.status.success(),
            "Could not uniquely verify the 127.0.0.1:{port} listener; no process was stopped. Stop the old server manually or choose another --port.");
        serde_json::from_slice(&output.stdout).map_err(|_| {
            anyhow::anyhow!("Could not read the legacy listener identity; no process was stopped")
        })
    }

    fn verify_dsh(owner: &Owner) -> Result<()> {
        let is_dsh = Path::new(&owner.executable)
            .file_name()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("dsh.exe"));
        anyhow::ensure!(
            is_dsh && owner.pid != 0 && owner.pid != std::process::id(),
            "The listener is not a verified separate dsh.exe process; no process was stopped"
        );
        let arguments = split_command_line(&owner.command_line)?;
        let command_is_dsh = arguments
            .first()
            .and_then(|arg| Path::new(arg).file_name())
            .is_some_and(|name| {
                let name = name.to_string_lossy();
                name.eq_ignore_ascii_case("dsh.exe") || name.eq_ignore_ascii_case("dsh")
            });
        anyhow::ensure!(
            command_is_dsh && is_web_command(&arguments),
            "The listener is not a verified dsh web command; no process was stopped"
        );
        Ok(())
    }

    async fn verify_service_token(port: u16, expected_token: &str) -> Result<()> {
        anyhow::ensure!(
            !expected_token.is_empty(),
            "The confirmed Harness identity is missing"
        );
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()?;
        let mut response = client
            .get(format!("http://127.0.0.1:{port}/api/harness/bootstrap"))
            .send()
            .await
            .context("Could not revalidate the confirmed legacy Harness; no process was stopped")?;
        anyhow::ensure!(
            response.status() == reqwest::StatusCode::OK,
            "The legacy Harness identity changed; no process was stopped"
        );
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            anyhow::ensure!(
                body.len() + chunk.len() <= 2 * 1024 * 1024,
                "The legacy Harness identity response is too large; no process was stopped"
            );
            body.extend_from_slice(&chunk);
        }
        let value: serde_json::Value = serde_json::from_slice(&body).map_err(|_| {
            anyhow::anyhow!("The legacy Harness identity is invalid; no process was stopped")
        })?;
        anyhow::ensure!(value["token"].as_str() == Some(expected_token),
            "The service changed after confirmation; no process was stopped. Retry the restart request.");
        Ok(())
    }

    pub(super) async fn stop(port: u16, expected_token: &str) -> Result<()> {
        let original = inspect(port).await?;
        verify_dsh(&original)?;
        // Retain the actual process handle before rechecking the listener. A
        // recycled PID cannot redirect termination to a replacement process.
        let process = Process::open(original.pid)?;
        process.verify_identity(&original)?;
        let current = inspect(port).await?;
        anyhow::ensure!(current == original,
            "The legacy listener changed during verification; no process was stopped. Retry the restart request.");
        process.verify_identity(&current)?;
        // Tie the retained process to the service the user actually confirmed,
        // including a replacement between the caller's probe and our first CIM query.
        verify_service_token(port, expected_token).await?;
        process.terminate()?;
        for _ in 0..50 {
            if process.exited() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        bail!("The verified old server has not exited yet; wait before retrying or choose another --port")
    }

    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn GetProcessTimes(
            process: *mut c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn QueryFullProcessImageNameW(
            process: *mut c_void,
            flags: u32,
            image: *mut u16,
            size: *mut u32,
        ) -> i32;
        fn TerminateProcess(process: *mut c_void, exit_code: u32) -> i32;
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn CommandLineToArgvW(command: *const u16, count: *mut i32) -> *mut *mut u16;
    }

    struct Process(OwnedHandle);

    impl Process {
        fn open(pid: u32) -> Result<Self> {
            // QUERY_LIMITED_INFORMATION | SYNCHRONIZE | TERMINATE, same user
            // permissions only; this helper never elevates or enables debug rights.
            let handle = unsafe { OpenProcess(0x1000 | 0x0010_0000 | 0x0001, 0, pid) };
            anyhow::ensure!(!handle.is_null(),
                "Could not open the verified legacy process; stop it manually or choose another --port");
            // SAFETY: OpenProcess returned a unique owning handle.
            Ok(Self(unsafe { OwnedHandle::from_raw_handle(handle) }))
        }

        fn verify_identity(&self, owner: &Owner) -> Result<()> {
            let mut image = vec![0u16; 32_768];
            let mut image_length = image.len() as u32;
            let mut creation = FileTime::default();
            let mut exit = FileTime::default();
            let mut kernel = FileTime::default();
            let mut user = FileTime::default();
            // SAFETY: all output buffers are writable for the advertised lengths
            // and the owning handle remains open throughout both calls.
            let available = unsafe {
                QueryFullProcessImageNameW(
                    self.0.as_raw_handle(),
                    0,
                    image.as_mut_ptr(),
                    &mut image_length,
                ) != 0
                    && GetProcessTimes(
                        self.0.as_raw_handle(),
                        &mut creation,
                        &mut exit,
                        &mut kernel,
                        &mut user,
                    ) != 0
            };
            anyhow::ensure!(
                available && !self.exited(),
                "The verified legacy process is no longer available; no process was stopped"
            );
            let executable = String::from_utf16_lossy(&image[..image_length as usize]);
            let actual_creation = (u64::from(creation.high) << 32) | u64::from(creation.low);
            let expected_creation = owner.creation_filetime.parse::<u64>().unwrap_or(0);
            // WMI datetime preserves microseconds; FILETIME has 100 ns precision.
            anyhow::ensure!(
                executable.eq_ignore_ascii_case(&owner.executable)
                    && expected_creation != 0
                    && actual_creation / 10 == expected_creation / 10,
                "The legacy process identity changed; no process was stopped"
            );
            Ok(())
        }

        fn exited(&self) -> bool {
            // SAFETY: the owning handle is valid; a zero timeout never blocks.
            unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) == 0 }
        }

        fn terminate(&self) -> Result<()> {
            // SAFETY: this exact handle passed image, creation-time, command and
            // immediate listener checks. No PID lookup or tree traversal occurs.
            anyhow::ensure!(unsafe { TerminateProcess(self.0.as_raw_handle(), 0) } != 0,
                "Could not stop the verified legacy server; stop it manually or choose another --port");
            Ok(())
        }
    }

    fn split_command_line(command: &str) -> Result<Vec<String>> {
        anyhow::ensure!(
            !command.is_empty() && !command.contains('\0'),
            "Invalid legacy command line"
        );
        let mut wide: Vec<u16> = command.encode_utf16().collect();
        wide.push(0);
        let mut count = 0;
        // SAFETY: the UTF-16 input has a trailing NUL and count is writable.
        let arguments = unsafe { CommandLineToArgvW(wide.as_ptr(), &mut count) };
        anyhow::ensure!(
            !arguments.is_null(),
            "Could not verify the legacy command line"
        );
        let mut result = Vec::new();
        for index in 0..count {
            // SAFETY: the API guarantees count pointers to NUL-terminated UTF-16
            // strings in one allocation, which stays alive until LocalFree below.
            let value = unsafe {
                let pointer = *arguments.add(index as usize);
                let mut length = 0;
                while *pointer.add(length) != 0 {
                    length += 1;
                }
                String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length))
            };
            result.push(value);
        }
        // SAFETY: this allocation came from CommandLineToArgvW and is freed once.
        unsafe {
            LocalFree(arguments.cast());
        }
        Ok(result)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[tokio::test]
        async fn final_http_identity_must_match_the_confirmed_server() {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            for (body, accepted) in [
                (r#"{"token":"confirmed-instance"}"#, true),
                (r#"{"token":"replacement-instance"}"#, false),
                (r#"{"unrelated":true}"#, false),
            ] {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let port = listener.local_addr().unwrap().port();
                let server = tokio::spawn(async move {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut request = Vec::new();
                    while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                        let mut bytes = [0u8; 1024];
                        let count = socket.read(&mut bytes).await.unwrap();
                        assert!(count > 0);
                        request.extend_from_slice(&bytes[..count]);
                    }
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                });
                assert_eq!(
                    verify_service_token(port, "confirmed-instance")
                        .await
                        .is_ok(),
                    accepted
                );
                server.await.unwrap();
            }
        }

        #[test]
        fn quoted_paths_are_parsed_without_mistaking_values_for_web() {
            let valid = split_command_line(r#""C:\Program Files\DSH\dsh.exe" --workspace "C:\projects\my app" --silent web --port 8770"#).unwrap();
            assert!(is_web_command(&valid));
            let other =
                split_command_line(r#"dsh.exe --workspace "C:\projects\web" mcp --name web"#)
                    .unwrap();
            assert!(!is_web_command(&other));
        }

        #[tokio::test]
        async fn a_non_dsh_listener_is_rejected_without_stopping_it() {
            let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let owner = inspect(port).await.unwrap();
            assert_eq!(owner.pid, std::process::id());
            // Read-only comparison also verifies CIM/Win32 timestamp agreement.
            Process::open(owner.pid)
                .unwrap()
                .verify_identity(&owner)
                .unwrap();
            assert!(stop_legacy(port, "test-token-never-used-for-non-dsh")
                .await
                .unwrap_err()
                .to_string()
                .contains("not a verified separate dsh.exe"));
            assert!(std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).is_ok());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(args: &[&str]) -> bool {
        is_web_command(
            &args
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn only_the_actual_web_subcommand_is_accepted() {
        for args in [
            vec!["dsh.exe", "web"],
            vec!["dsh.exe", "--config", "web", "--silent", "web"],
            vec![
                "dsh.exe",
                "--workspace=web",
                "-mmodel",
                "-C",
                "workspace",
                "web",
            ],
        ] {
            assert!(command(&args), "{args:?}");
        }
        for args in [
            vec!["dsh.exe", "mcp", "web"],
            vec!["dsh.exe", "--config", "web", "mcp"],
            vec!["dsh.exe", "--", "web"],
            vec!["dsh.exe", "startup", "web"],
            vec!["dsh.exe", "--unknown", "web"],
            vec!["dsh.exe", "--workspace"],
        ] {
            assert!(!command(&args), "{args:?}");
        }
    }

    #[tokio::test]
    async fn port_zero_is_never_a_stop_target() {
        assert!(stop_legacy(0, "unused")
            .await
            .unwrap_err()
            .to_string()
            .contains("specific occupied port"));
    }
}
