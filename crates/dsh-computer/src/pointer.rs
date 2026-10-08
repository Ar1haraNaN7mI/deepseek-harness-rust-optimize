//! A DSH-owned desktop cursor layer. It never changes the OS cursor or focus.
use anyhow::{ensure, Context, Result};
use std::sync::{mpsc, Mutex};
use windows::{
    core::w,
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM},
        Graphics::Gdi::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2},
            WindowsAndMessaging::*,
        },
    },
};

pub(crate) const CLASS: &str = "DSHIndependentDesktopPointer";
const WIDTH: i32 = 72;
const HEIGHT: i32 = 52;

struct Window {
    hwnd: usize,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            let _ = PostMessageW(Some(HWND(self.hwnd as _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Default)]
pub(crate) struct DesktopPointer {
    window: Mutex<Option<Window>>,
}
impl DesktopPointer {
    pub(crate) fn show(&self, point: POINT) -> Result<()> {
        let mut window = self
            .window
            .lock()
            .map_err(|_| anyhow::anyhow!("DSH cursor lock failed"))?;
        if window.is_none() {
            let (sender, receiver) = mpsc::sync_channel(1);
            let thread = std::thread::Builder::new()
                .name("dsh-desktop-pointer".into())
                .spawn(move || {
                    let result = run(&sender);
                    if let Err(error) = result {
                        let _ = sender.try_send(Err(error.to_string()));
                    }
                })?;
            let hwnd = receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .context("DSH cursor window did not start")?
                .map_err(anyhow::Error::msg)?;
            *window = Some(Window {
                hwnd,
                thread: Some(thread),
            });
        }
        let hwnd = HWND(window.as_ref().unwrap().hwnd as _);
        // These coordinates locate only our overlay. SWP_NOACTIVATE is mandatory.
        unsafe {
            SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                point.x,
                point.y,
                WIDTH,
                HEIGHT,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )?;
            SetTimer(Some(hwnd), 1, 5000, None);
        }
        Ok(())
    }
    pub(crate) fn hide(&self) {
        if let Ok(window) = self.window.lock() {
            if let Some(window) = window.as_ref() {
                unsafe {
                    let _ = ShowWindow(HWND(window.hwnd as _), SW_HIDE);
                }
            }
        }
    }
    #[cfg(test)]
    pub(crate) fn handle(&self) -> Option<HWND> {
        self.window
            .lock()
            .ok()?
            .as_ref()
            .map(|window| HWND(window.hwnd as _))
    }
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_TIMER => {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
                let _ = KillTimer(Some(hwnd), 1);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn run(sender: &mpsc::SyncSender<Result<usize, String>>) -> Result<()> {
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: instance,
            lpszClassName: w!("DSHIndependentDesktopPointer"),
            ..Default::default()
        });
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("DSHIndependentDesktopPointer"),
            w!("DSH independent cursor"),
            WS_POPUP,
            0,
            0,
            WIDTH,
            HEIGHT,
            None,
            None,
            Some(instance),
            None,
        )?;
        if let Err(error) = paint(hwnd) {
            let _ = DestroyWindow(hwnd);
            return Err(error);
        }
        if sender.send(Ok(hwnd.0 as usize)).is_err() {
            let _ = DestroyWindow(hwnd);
            return Ok(());
        }
        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0);
            ensure!(result.0 >= 0, "DSH cursor message loop failed");
            if result.0 == 0 {
                break;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

fn polygon(x: f32, y: f32, points: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let mut last = points.len() - 1;
    for (index, &(px, py)) in points.iter().enumerate() {
        let (lx, ly) = points[last];
        if (py > y) != (ly > y) && x < (lx - px) * (y - py) / (ly - py) + px {
            inside = !inside;
        }
        last = index;
    }
    inside
}

fn pixels() -> Vec<u8> {
    let mut pixels = vec![0; (WIDTH * HEIGHT * 4) as usize];
    let outline = [
        (0., 0.),
        (0., 34.),
        (9., 27.),
        (17., 43.),
        (25., 39.),
        (17., 23.),
        (30., 22.),
    ];
    let inner = [
        (3., 7.),
        (3., 28.),
        (10., 22.),
        (18., 38.),
        (21., 37.),
        (12., 20.),
        (23., 19.),
    ];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let color = if polygon(x as f32 + 0.5, y as f32 + 0.5, &inner) {
                [40, 164, 255, 255]
            } else if polygon(x as f32 + 0.5, y as f32 + 0.5, &outline) {
                [23, 27, 29, 255]
            } else if (29..69).contains(&x) && (29..48).contains(&y) {
                [23, 27, 29, 245]
            } else {
                [0, 0, 0, 0]
            };
            let offset = ((y * WIDTH + x) * 4) as usize;
            pixels[offset..offset + 4].copy_from_slice(&color);
        }
    }
    // Tiny custom pixel lettering keeps the overlay independent of installed fonts.
    let glyphs = [
        [30u8, 17, 17, 17, 17, 17, 30],
        [31, 16, 16, 31, 1, 1, 31],
        [17, 17, 17, 31, 17, 17, 17],
    ];
    for (letter, rows) in glyphs.iter().enumerate() {
        for (row, bits) in rows.iter().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) != 0 {
                    let x = 35 + letter * 10 + column;
                    let y = 35 + row;
                    let offset = (y * WIDTH as usize + x) * 4;
                    pixels[offset..offset + 4].copy_from_slice(&[233, 238, 239, 255]);
                }
            }
        }
    }
    pixels
}

fn paint(hwnd: HWND) -> Result<()> {
    unsafe {
        let dc = CreateCompatibleDC(None);
        ensure!(!dc.0.is_null(), "Create DSH cursor surface failed");
        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: WIDTH,
            biHeight: -HEIGHT,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut data = std::ptr::null_mut();
        let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut data, None, 0) {
            Ok(bitmap) => bitmap,
            Err(error) => {
                let _ = DeleteDC(dc);
                return Err(error.into());
            }
        };
        let prior = SelectObject(dc, bitmap.into());
        let pixels = pixels();
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), data.cast::<u8>(), pixels.len());
        let size = SIZE {
            cx: WIDTH,
            cy: HEIGHT,
        };
        let origin = POINT::default();
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let result = UpdateLayeredWindow(
            hwnd,
            None,
            None,
            Some(&size),
            Some(dc),
            Some(&origin),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        SelectObject(dc, prior);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(dc);
        result?;
    }
    Ok(())
}
