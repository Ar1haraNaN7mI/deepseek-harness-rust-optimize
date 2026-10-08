//! Self-contained Win32 test window. No external/user app is controlled.
#![allow(dead_code)] // Shared by integration tests and the interactive QA example.
use anyhow::{ensure, Context, Result};
use std::{sync::mpsc, thread::JoinHandle};
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
};

#[derive(Clone, Copy)]
pub struct Handles {
    pub window: isize,
    pub edit: isize,
    pub password: isize,
    pub button: isize,
    pub counter: isize,
    pub multiline: isize,
    pub canvas: isize,
}
pub struct Fixture {
    pub handles: Handles,
    thread: Option<JoinHandle<()>>,
}
impl Fixture {
    pub fn start(title: &str) -> Result<Self> {
        let title = title.to_owned();
        let (sender, receiver) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("dsh-computer-fixture".into())
            .spawn(move || {
                let result = run(&title, &sender);
                if let Err(error) = result {
                    let _ = sender.try_send(Err(error.to_string()));
                }
            })?;
        let handles = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .context("Fixture did not start")?
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            handles,
            thread: Some(thread),
        })
    }
    pub fn text(&self, handle: isize) -> String {
        let mut text = [0u16; 8192];
        let n = unsafe { GetWindowTextW(HWND(handle as _), &mut text) };
        String::from_utf16_lossy(&text[..n.max(0) as usize])
    }
    pub fn move_window(&self) {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(self.handles.window as _)),
                WM_APP + 1,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
    pub fn close(&mut self) {
        if let Some(thread) = self.thread.take() {
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(self.handles.window as _)),
                    WM_CLOSE,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
            let _ = thread.join();
        }
    }
    pub fn wait(mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.close();
    }
}

struct Counter {
    label: HWND,
    count: u32,
}
#[derive(Default)]
struct CanvasState {
    clicks: usize,
    text: String,
    scroll: isize,
    keys: usize,
    delay_next_char_ms: u64,
}
unsafe extern "system" fn canvas_procedure(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let data = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut CanvasState;
    if msg == WM_CREATE {
        unsafe {
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                Box::into_raw(Box::<CanvasState>::default()) as isize,
            );
        }
        return LRESULT(0);
    }
    if msg == WM_DESTROY {
        if !data.is_null() {
            unsafe {
                drop(Box::from_raw(data));
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
        }
        return LRESULT(0);
    }
    if !data.is_null() {
        let state = unsafe { &mut *data };
        match msg {
            WM_LBUTTONUP => state.clicks += 1,
            WM_CHAR => {
                if let Some(c) = char::from_u32(wparam.0 as u32) {
                    state.text.push(c);
                }
                let delay = std::mem::take(&mut state.delay_next_char_ms);
                if delay > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(delay));
                }
            }
            WM_KEYDOWN => state.keys += 1,
            WM_MOUSEWHEEL => state.scroll += ((wparam.0 >> 16) as i16 as isize) / 120,
            message if message == WM_APP + 11 => return LRESULT(state.clicks as isize),
            message if message == WM_APP + 12 => {
                return LRESULT(state.text.chars().count() as isize)
            }
            message if message == WM_APP + 13 => return LRESULT(state.scroll),
            message if message == WM_APP + 14 => return LRESULT(state.keys as isize),
            message if message == WM_APP + 15 => {
                state.delay_next_char_ms = (wparam.0 as u64).min(1000);
                return LRESULT(0);
            }
            WM_PAINT | WM_PRINTCLIENT => {
                unsafe {
                    let mut paint = PAINTSTRUCT::default();
                    let dc = if msg == WM_PAINT {
                        BeginPaint(hwnd, &mut paint)
                    } else {
                        HDC(wparam.0 as _)
                    };
                    let mut bounds = Default::default();
                    let _ = GetClientRect(hwnd, &mut bounds);
                    FillRect(dc, &bounds, GetSysColorBrush(COLOR_WINDOW));
                    let font = CreateFontW(
                        24,
                        0,
                        0,
                        0,
                        700,
                        0,
                        0,
                        0,
                        DEFAULT_CHARSET,
                        OUT_DEFAULT_PRECIS,
                        CLIP_DEFAULT_PRECIS,
                        DEFAULT_QUALITY,
                        DEFAULT_PITCH.0 as u32,
                        w!("Segoe UI"),
                    );
                    let prior = SelectObject(dc, font.into());
                    SetBkMode(dc, TRANSPARENT);
                    let text = format!(
                        "VISUAL TARGET 42\nCanvas clicks: {}  Scroll: {}\nTyped: {}",
                        state.clicks, state.scroll, state.text
                    );
                    let mut text: Vec<u16> = text.encode_utf16().collect();
                    let mut area = windows::Win32::Foundation::RECT {
                        left: 12,
                        top: 8,
                        right: 530,
                        bottom: 158,
                    };
                    DrawTextW(dc, &mut text, &mut area, DT_LEFT | DT_TOP);
                    SelectObject(dc, prior);
                    let _ = DeleteObject(font.into());
                    if msg == WM_PAINT {
                        let _ = EndPaint(hwnd, &paint);
                    }
                }
                return LRESULT(0);
            }
            _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, true);
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
unsafe extern "system" fn procedure(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_COMMAND if wparam.0 & 0xffff == 101 && wparam.0 >> 16 == 0 => {
            let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Counter;
            if !pointer.is_null() {
                let counter = unsafe { &mut *pointer };
                counter.count += 1;
                let text: Vec<_> = format!("Counter: {}", counter.count)
                    .encode_utf16()
                    .chain(Some(0))
                    .collect();
                unsafe {
                    let _ = SetWindowTextW(counter.label, PCWSTR(text.as_ptr()));
                }
            }
            LRESULT(0)
        }
        message if message == WM_APP + 1 => {
            let mut rect = Default::default();
            unsafe {
                if GetWindowRect(hwnd, &mut rect).is_ok() {
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        rect.left + 30,
                        rect.top + 20,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER,
                    );
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Counter;
            if !pointer.is_null() {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    drop(Box::from_raw(pointer));
                }
            }
            unsafe {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn run(title: &str, sender: &mpsc::SyncSender<Result<Handles, String>>) -> Result<()> {
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: instance,
            lpszClassName: w!("DSHComputerTestFixture"),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            ..Default::default()
        };
        // An identical class can already have been registered by a previous test.
        RegisterClassW(&class);
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(canvas_procedure),
            hInstance: instance,
            lpszClassName: w!("DSHComputerCanvasFixture"),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            ..Default::default()
        });
        let title: Vec<_> = title.encode_utf16().chain(Some(0)).collect();
        let window = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("DSHComputerTestFixture"),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            100,
            120,
            620,
            690,
            None,
            None,
            Some(instance),
            None,
        )?;
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("DSH isolated computer-use test"),
            WS_CHILD | WS_VISIBLE,
            24,
            20,
            540,
            26,
            Some(window),
            None,
            Some(instance),
            None,
        )?;
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("Text input"),
            WS_CHILD | WS_VISIBLE,
            24,
            64,
            120,
            24,
            Some(window),
            None,
            Some(instance),
            None,
        )?;
        let edit = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            w!("EDIT"),
            w!("initial"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            150,
            60,
            380,
            30,
            Some(window),
            Some(HMENU(100 as _)),
            Some(instance),
            None,
        )?;
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("Password fixture"),
            WS_CHILD | WS_VISIBLE,
            24,
            112,
            120,
            24,
            Some(window),
            None,
            Some(instance),
            None,
        )?;
        let password = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            w!("EDIT"),
            w!("fixture-secret-never-export"),
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE((ES_PASSWORD | ES_AUTOHSCROLL) as u32),
            150,
            108,
            380,
            30,
            Some(window),
            Some(HMENU(103 as _)),
            Some(instance),
            None,
        )?;
        let button = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("Increment counter"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            24,
            170,
            240,
            42,
            Some(window),
            Some(HMENU(101 as _)),
            Some(instance),
            None,
        )?;
        let counter = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("Counter: 0"),
            WS_CHILD | WS_VISIBLE,
            300,
            182,
            240,
            30,
            Some(window),
            Some(HMENU(102 as _)),
            Some(instance),
            None,
        )?;
        let lines: Vec<u16> = (0..60)
            .map(|index| format!("Background scroll line {index}\r\n"))
            .collect::<String>()
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let multiline = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            w!("EDIT"),
            PCWSTR(lines.as_ptr()),
            WS_CHILD
                | WS_VISIBLE
                | WS_VSCROLL
                | WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL) as u32),
            24,
            242,
            540,
            160,
            Some(window),
            Some(HMENU(104 as _)),
            Some(instance),
            None,
        )?;
        let canvas = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            w!("DSHComputerCanvasFixture"),
            w!(""),
            WS_CHILD | WS_VISIBLE,
            24,
            430,
            540,
            170,
            Some(window),
            Some(HMENU(105 as _)),
            Some(instance),
            None,
        )?;
        SetWindowLongPtrW(
            window,
            GWLP_USERDATA,
            Box::into_raw(Box::new(Counter {
                label: counter,
                count: 0,
            })) as isize,
        );
        let _ = ShowWindow(window, SW_SHOWNORMAL);
        let _ = UpdateWindow(window);
        let _ = SetForegroundWindow(window);
        sender
            .send(Ok(Handles {
                window: window.0 as isize,
                edit: edit.0 as isize,
                password: password.0 as isize,
                button: button.0 as isize,
                counter: counter.0 as isize,
                multiline: multiline.0 as isize,
                canvas: canvas.0 as isize,
            }))
            .map_err(|_| anyhow::anyhow!("Fixture parent disconnected"))?;
        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0);
            ensure!(result.0 >= 0, "Fixture message loop failed");
            if result.0 == 0 {
                break;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        Ok(())
    }
}
