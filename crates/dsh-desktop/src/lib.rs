//! A native window around DSH's authenticated loopback application.
//! No generic shell/filesystem IPC is exposed to web content.
#[cfg(windows)]
use anyhow::Context;
use anyhow::Result;
use std::path::PathBuf;
#[cfg(windows)]
use std::time::Duration;
#[cfg(any(windows, test))]
use url::Url;

pub struct DesktopOptions {
    pub instance_key: String,
    pub url: String,
    pub data_directory: PathBuf,
    pub workspace_label: String,
    /// Diagnostic mode opens a real window, waits for a real page load, then exits.
    pub smoke_test: bool,
}

pub enum Instance {
    Primary(InstanceGuard),
    Existing,
}

pub struct InstanceGuard {
    #[cfg(windows)]
    handle: isize,
}

#[cfg(windows)]
mod instance_api {
    #[link(name = "kernel32")]
    extern "system" {
        pub fn CreateMutexW(
            attributes: *const std::ffi::c_void,
            owner: i32,
            name: *const u16,
        ) -> isize;
        pub fn WaitForSingleObject(handle: isize, timeout: u32) -> u32;
        pub fn ReleaseMutex(handle: isize) -> i32;
        pub fn CloseHandle(handle: isize) -> i32;
    }
    #[link(name = "user32")]
    extern "system" {
        pub fn FindWindowW(class: *const u16, title: *const u16) -> isize;
        pub fn IsIconic(window: isize) -> i32;
        pub fn ShowWindow(window: isize, command: i32) -> i32;
        pub fn SetForegroundWindow(window: isize) -> i32;
    }
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            instance_api::ReleaseMutex(self.handle);
            instance_api::CloseHandle(self.handle);
        }
    }
}

/// Serialize desktop ownership per workspace/profile. A second launch focuses
/// the owner window, so it can never borrow a server that another window closes.
#[cfg(windows)]
pub fn acquire_instance(key: &str) -> Result<Instance> {
    use std::os::windows::ffi::OsStrExt;
    use std::time::Instant;
    let wide = |value: &str| {
        std::ffi::OsStr::new(value)
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>()
    };
    let name = wide(&format!("Local\\DSH.Harness.{key}"));
    let handle = unsafe { instance_api::CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    anyhow::ensure!(handle != 0, "Cannot create DSH desktop instance lock");
    let status = unsafe { instance_api::WaitForSingleObject(handle, 0) };
    if matches!(status, 0 | 0x80) {
        return Ok(Instance::Primary(InstanceGuard { handle }));
    }
    unsafe {
        instance_api::CloseHandle(handle);
    }
    anyhow::ensure!(status == 0x102, "Cannot acquire DSH desktop instance lock");
    let class = wide(&format!("DSH.Harness.{key}"));
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let window = unsafe { instance_api::FindWindowW(class.as_ptr(), std::ptr::null()) };
        if window != 0 {
            unsafe {
                if instance_api::IsIconic(window) != 0 {
                    instance_api::ShowWindow(window, 9);
                }
                instance_api::SetForegroundWindow(window);
            }
            return Ok(Instance::Existing);
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    anyhow::bail!(
        "DSH desktop is already starting for this workspace. Please try again in a moment."
    )
}

#[cfg(not(windows))]
pub fn acquire_instance(_key: &str) -> Result<Instance> {
    anyhow::bail!("The native desktop client currently supports Windows")
}

#[derive(Debug)]
pub struct DesktopOutcome {
    pub page_loaded: bool,
    pub frontend_ready: bool,
    pub smoke_test: bool,
}

#[cfg(any(windows, test))]
fn local_origin(input: &str) -> Result<String> {
    let url = Url::parse(input)?;
    anyhow::ensure!(
        url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.port().is_some()
            && url.username().is_empty()
            && url.password().is_none(),
        "Desktop Harness requires an authenticated loopback service"
    );
    Ok(url.origin().ascii_serialization())
}

#[cfg(any(windows, test))]
fn same_origin(input: &str, origin: &str) -> bool {
    Url::parse(input).is_ok_and(|url| url.origin().ascii_serialization() == origin)
}

#[cfg(windows)]
pub fn run(options: DesktopOptions) -> Result<DesktopOutcome> {
    use std::{cell::Cell, rc::Rc, time::Instant};
    use tao::{
        dpi::LogicalSize,
        event::{Event, WindowEvent},
        event_loop::{ControlFlow, EventLoop},
        platform::run_return::EventLoopExtRunReturn,
        platform::windows::WindowBuilderExtWindows,
        window::{Icon, WindowBuilder},
    };
    use wry::{NewWindowResponse, PageLoadEvent, WebContext, WebViewBuilder};

    let origin = local_origin(&options.url)?;
    if options.smoke_test { eprintln!("Desktop check: create window"); }
    std::fs::create_dir_all(&options.data_directory)?;
    let mut event_loop = EventLoop::new();
    let size = event_loop.primary_monitor().map(|monitor| {
        let size = monitor.size().to_logical::<f64>(monitor.scale_factor());
        LogicalSize::new((size.width * 0.85).min(1360.0), (size.height * 0.80).min(900.0))
    }).unwrap_or_else(|| LogicalSize::new(1100.0, 720.0));
    let window = WindowBuilder::new()
        .with_window_classname(format!("DSH.Harness.{}", options.instance_key))
        .with_title(format!("DSH Harness · {}", options.workspace_label))
        .with_window_icon(
            Icon::from_rgba(include_bytes!("../assets/icon.rgba").to_vec(), 128, 128).ok(),
        )
        .with_inner_size(size)
        .with_min_inner_size(LogicalSize::new(720.0, 480.0))
        .with_maximized(true)
        .build(&event_loop)
        .context("create DSH desktop window")?;
    let loaded = Rc::new(Cell::new(None));
    let loaded_callback = loaded.clone();
    let ready = Rc::new(Cell::new(None));
    let ready_callback = ready.clone();
    let ipc_origin = origin.clone();
    let page_origin = origin.clone();
    let mut context = WebContext::new(Some(options.data_directory));
    if options.smoke_test { eprintln!("Desktop check: create WebView2"); }
    let webview = WebViewBuilder::new_with_web_context(&mut context)
        .with_url(&options.url)
        .with_background_color((10, 14, 16, 255))
        .with_clipboard(true)
        .with_devtools(cfg!(debug_assertions))
        .with_initialization_script(
            "Object.defineProperty(window, '__DSH_DESKTOP__', {value: true});",
        )
        .with_ipc_handler(move |request| {
            // A readiness notification is the entire native IPC surface; it
            // neither accepts commands nor returns credentials to JavaScript.
            if same_origin(&request.uri().to_string(), &ipc_origin)
                && request.body() == "{\"event\":\"dsh-ready\",\"version\":1}"
            {
                ready_callback.set(Some(Instant::now()));
            }
        })
        .with_navigation_handler(move |url| {
            if same_origin(&url, &origin) {
                return true;
            }
            open_external(&url);
            false
        })
        .with_new_window_req_handler(|url, _| {
            open_external(&url);
            NewWindowResponse::Deny
        })
        .with_on_page_load_handler(move |event, url| {
            if matches!(event, PageLoadEvent::Finished) && same_origin(&url, &page_origin) {
                loaded_callback.set(Some(Instant::now()));
            }
        })
        .build(&window)
        .context("create WebView2; install Microsoft Edge WebView2 Runtime if it is missing")?;

    if options.smoke_test { eprintln!("Desktop check: run native event loop"); }

    let started = Instant::now();
    let timer_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let timer = if options.smoke_test {
        let stop = timer_stop.clone();
        let proxy = event_loop.create_proxy();
        Some(std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));
                if proxy.send_event(()).is_err() { break; }
            }
        }))
    } else { None };
    // run_return is necessary to release the backend owned by this window after
    // the native event loop exits. Tokio's worker threads remain active throughout.
    event_loop.run_return(|event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
        }
        if options.smoke_test
            && (ready
                .get()
                .is_some_and(|at| at.elapsed() > Duration::from_secs(2))
                || started.elapsed() > Duration::from_secs(20))
        {
            *control_flow = ControlFlow::Exit;
        }
        let _ = (&window, &webview, &context);
    });
    timer_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(timer) = timer { let _ = timer.join(); }
    let page_loaded = loaded.get().is_some();
    let frontend_ready = ready.get().is_some();
    if options.smoke_test { eprintln!("Desktop check: native event loop finished"); }
    anyhow::ensure!(
        !options.smoke_test || (page_loaded && frontend_ready),
        "Native WebView did not load the real Harness frontend and bootstrap within 20 seconds"
    );
    Ok(DesktopOutcome {
        page_loaded,
        frontend_ready,
        smoke_test: options.smoke_test,
    })
}

#[cfg(not(windows))]
pub fn run(_options: DesktopOptions) -> Result<DesktopOutcome> {
    anyhow::bail!(
        "The native desktop client currently supports Windows. Use `dsh web` on this platform."
    )
}

#[cfg(windows)]
fn open_external(input: &str) {
    use std::os::windows::ffi::OsStrExt;
    let Ok(url) = Url::parse(input) else {
        return;
    };
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return;
    }
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            window: isize,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> isize;
    }
    let wide = |value: &str| {
        std::ffi::OsStr::new(value)
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>()
    };
    let operation = wide("open");
    let target = wide(url.as_str());
    unsafe {
        ShellExecuteW(
            0,
            operation.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        );
    }
}

/// GUI entry-point errors must remain visible when there is no terminal.
pub fn show_error(message: &str) {
    show_dialog(message, true);
}

pub fn show_message(message: &str) {
    show_dialog(message, false);
}

fn show_dialog(message: &str, error: bool) {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "user32")]
        extern "system" {
            fn MessageBoxW(window: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
        }
        let wide = |value: &str| {
            std::ffi::OsStr::new(value)
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>()
        };
        let text = wide(message);
        let caption = wide("DSH Harness");
        unsafe {
            MessageBoxW(
                0,
                text.as_ptr(),
                caption.as_ptr(),
                if error { 0x10 } else { 0x40 },
            );
        }
    }
    #[cfg(not(windows))]
    eprintln!("{message}");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_only_accepts_the_exact_local_service_origin() {
        let origin = local_origin("http://127.0.0.1:8770/?dsh-startup=on").unwrap();
        assert!(same_origin(
            "http://127.0.0.1:8770/startup-preview.html",
            &origin
        ));
        for url in [
            "https://example.com",
            "file:///secret",
            "http://127.0.0.1.evil:8770/",
            "http://user@127.0.0.1:8770/",
            "http://127.0.0.1/",
        ] {
            assert!(local_origin(url).is_err());
        }
        assert!(!same_origin("http://127.0.0.1:8771/", &origin));
    }
}
