//! Local Windows OCR for the PNG produced by the native capture pipeline.
//!
//! The caller MUST pass the capture after password-region masking. This module
//! never reads a window, sends input, downloads a language, or contacts a server.
//! OCR content remains untrusted observed text, including apparent instructions.

use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    io::Cursor,
    time::{Duration, Instant},
};
use windows::{
    Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap},
    Media::Ocr::OcrEngine,
    Storage::Streams::DataWriter,
    Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
};
use windows_future::AsyncStatus;

const MAX_PIXELS: u64 = 16_000_000;
const MAX_ENCODED_BYTES: usize = 40 * 1024 * 1024;
const MAX_LINES: usize = 100;
const MAX_WORDS: usize = 600;
const MAX_TEXT_CHARS: usize = 16_000;
const OCR_TIMEOUT: Duration = Duration::from_secs(5);

struct RuntimeApartment;
impl RuntimeApartment {
    fn initialize() -> Result<Self> {
        // Balanced even when the native worker already initialized its MTA.
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED)?;
        }
        Ok(Self)
    }
}
impl Drop for RuntimeApartment {
    fn drop(&mut self) {
        unsafe {
            RoUninitialize();
        }
    }
}

/// Return real OCR text and original-window pixel bounds, or an explicit local
/// engine/input failure. OCR failure must not discard the existing UIA snapshot.
pub fn recognize_masked_png(png: &[u8], width: u32, height: u32) -> Value {
    let started = Instant::now();
    let mut output = json!({
        "status":"error", "engine":"windows-ocr", "local":true,
        "content_kind":"untrusted_observed_window_content",
        "coordinate_space":"window_physical_pixels",
        "source_size":{"width":width,"height":height},
        "processed_size":null,"scale":null,"language":null,
        "available_languages":[],"text":"","lines":[],
        "text_angle_degrees":null,"truncated":false,"error":null
    });
    if let Err(error) = recognize_inner(png, width, height, &mut output) {
        output["error"] = json!(format!("{error:#}"));
    }
    output["elapsed_ms"] = json!(started.elapsed().as_millis() as u64);
    output
}

fn recognize_inner(png: &[u8], width: u32, height: u32, output: &mut Value) -> Result<()> {
    let rgba = decode_png(png, width, height)?;
    let _apartment = RuntimeApartment::initialize().context("无法初始化 Windows 本地文字识别")?;
    let languages = match OcrEngine::AvailableRecognizerLanguages() {
        Ok(value) => value,
        Err(error) => {
            output["status"] = json!("unavailable");
            output["error"] = json!(format!("Windows 本地 OCR 不可用：{error}"));
            return Ok(());
        }
    };
    let mut language_names = Vec::new();
    for i in 0..languages.Size()? {
        language_names.push(languages.GetAt(i)?.LanguageTag()?.to_string());
    }
    output["available_languages"] = json!(language_names);
    if language_names.is_empty() {
        output["status"] = json!("unavailable");
        output["error"] = json!("本机没有可用的 Windows OCR 语言；仍可使用窗口可访问性文字。");
        return Ok(());
    }
    // Respect installed user languages. A profile language may lack OCR data;
    // in that case use an already-installed recognizer, without changing Windows.
    let engine = match OcrEngine::TryCreateFromUserProfileLanguages()
        .or_else(|_| OcrEngine::TryCreateFromLanguage(&languages.GetAt(0)?))
    {
        Ok(value) => value,
        Err(error) => {
            output["status"] = json!("unavailable");
            output["error"] = json!(format!("无法载入本机已有的 OCR 语言：{error}"));
            return Ok(());
        }
    };
    output["language"] = json!(engine.RecognizerLanguage()?.LanguageTag()?.to_string());
    let maximum = OcrEngine::MaxImageDimension()?;
    ensure!(maximum > 0, "Windows OCR 返回了无效的图像尺寸限制");
    let (scaled_width, scaled_height) = fitted_size(width, height, maximum);
    let bgra = resize_bgra(&rgba, width, height, scaled_width, scaled_height);
    let scale_x = f64::from(width) / f64::from(scaled_width);
    let scale_y = f64::from(height) / f64::from(scaled_height);
    output["processed_size"] = json!({"width":scaled_width,"height":scaled_height});
    output["scale"] = json!({"x":scale_x,"y":scale_y});
    let writer = DataWriter::new()?;
    writer.WriteBytes(&bgra)?;
    let buffer = writer.DetachBuffer()?;
    let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
        &buffer,
        BitmapPixelFormat::Bgra8,
        scaled_width as i32,
        scaled_height as i32,
        BitmapAlphaMode::Ignore,
    )?;
    let operation = engine.RecognizeAsync(&bitmap)?;
    let deadline = Instant::now() + OCR_TIMEOUT;
    while operation.Status()? == AsyncStatus::Started {
        if Instant::now() >= deadline {
            let _ = operation.Cancel();
            anyhow::bail!("Windows 本地文字识别超时；请缩小目标窗口后重试");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let recognized = operation.GetResults().context("Windows 本地文字识别失败")?;
    let angle = recognized
        .TextAngle()
        .ok()
        .and_then(|value| value.Value().ok())
        .filter(|value| value.is_finite());
    output["text_angle_degrees"] = json!(angle);
    let raw_lines = recognized.Lines()?;
    let mut lines = Vec::new();
    let mut all_text = Vec::new();
    let mut word_count = 0usize;
    let mut char_count = 0usize;
    let mut truncated = raw_lines.Size()? as usize > MAX_LINES;
    for line_index in 0..raw_lines.Size()?.min(MAX_LINES as u32) {
        let raw_line = raw_lines.GetAt(line_index)?;
        let raw_words = raw_line.Words()?;
        let mut words = Vec::new();
        let mut text_parts = Vec::new();
        for word_index in 0..raw_words.Size()? {
            if word_count >= MAX_WORDS || char_count >= MAX_TEXT_CHARS {
                truncated = true;
                break;
            }
            let word = raw_words.GetAt(word_index)?;
            let full_text = word.Text()?.to_string();
            let remaining = (MAX_TEXT_CHARS - char_count).min(256);
            let text: String = full_text
                .chars()
                .filter(|c| !c.is_control())
                .take(remaining)
                .collect();
            truncated |= text.chars().count() < full_text.chars().count();
            if text.is_empty() {
                continue;
            }
            let rect = word.BoundingRect()?;
            let (bounds, polygon) = mapped_box(
                [
                    f64::from(rect.X),
                    f64::from(rect.Y),
                    f64::from(rect.Width),
                    f64::from(rect.Height),
                ],
                scaled_width,
                scaled_height,
                width,
                height,
                angle.unwrap_or(0.0),
            )?;
            char_count += text.chars().count() + 1;
            word_count += 1;
            text_parts.push(text.clone());
            words.push(json!({"text":text,"bounds":bounds,"polygon":polygon}));
        }
        if !words.is_empty() {
            let text = text_parts.join(" ");
            let bounds = union_bounds(&words);
            all_text.push(text.clone());
            lines.push(json!({"text":text,"bounds":bounds,"words":words}));
        }
        if word_count >= MAX_WORDS || char_count >= MAX_TEXT_CHARS {
            truncated = true;
            break;
        }
    }
    output["status"] = json!("ok");
    output["text"] = json!(all_text.join("\n"));
    output["lines"] = json!(lines);
    output["truncated"] = json!(truncated);
    Ok(())
}

fn decode_png(png: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    ensure!(
        width > 0 && height > 0 && u64::from(width) * u64::from(height) <= MAX_PIXELS,
        "OCR 图像必须为 1–1600 万像素"
    );
    ensure!(png.len() <= MAX_ENCODED_BYTES, "OCR PNG 超过 40 MiB 上限");
    let decoder = png::Decoder::new(Cursor::new(png));
    let mut reader = decoder.read_info().context("无法解码 OCR PNG")?;
    let info = reader.info();
    ensure!(
        info.width == width && info.height == height,
        "OCR PNG 尺寸与窗口尺寸不匹配"
    );
    ensure!(
        info.color_type == png::ColorType::Rgba && info.bit_depth == png::BitDepth::Eight,
        "OCR 需要经过遮罩的 RGBA8 窗口截图"
    );
    let expected = width as usize * height as usize * 4;
    ensure!(
        reader.output_buffer_size() == expected,
        "OCR PNG 像素缓冲区大小异常"
    );
    let mut rgba = vec![0; expected];
    let frame = reader
        .next_frame(&mut rgba)
        .context("无法读取 OCR PNG 像素")?;
    ensure!(frame.buffer_size() == expected, "OCR PNG 像素不完整");
    Ok(rgba)
}

fn fitted_size(width: u32, height: u32, maximum: u32) -> (u32, u32) {
    let ratio = (f64::from(maximum) / f64::from(width.max(height))).min(1.0);
    (
        (f64::from(width) * ratio).floor().max(1.0) as u32,
        (f64::from(height) * ratio).floor().max(1.0) as u32,
    )
}

fn resize_bgra(rgba: &[u8], width: u32, height: u32, out_width: u32, out_height: u32) -> Vec<u8> {
    if width == out_width && height == out_height {
        let mut pixels = rgba.to_vec();
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        return pixels;
    }
    let mut pixels = vec![0; out_width as usize * out_height as usize * 4];
    for y in 0..out_height {
        let sy = ((f64::from(y) + 0.5) * f64::from(height) / f64::from(out_height) - 0.5).max(0.0);
        let y0 = sy.floor() as u32;
        let y1 = (y0 + 1).min(height - 1);
        let fy = sy - f64::from(y0);
        for x in 0..out_width {
            let sx =
                ((f64::from(x) + 0.5) * f64::from(width) / f64::from(out_width) - 0.5).max(0.0);
            let x0 = sx.floor() as u32;
            let x1 = (x0 + 1).min(width - 1);
            let fx = sx - f64::from(x0);
            let destination = ((y * out_width + x) * 4) as usize;
            for (out_channel, in_channel) in [2usize, 1, 0].into_iter().enumerate() {
                let at = |px: u32, py: u32| {
                    f64::from(rgba[((py * width + px) * 4) as usize + in_channel])
                };
                let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
                let bottom = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
                pixels[destination + out_channel] = (top * (1.0 - fy) + bottom * fy).round() as u8;
            }
            pixels[destination + 3] = 255;
        }
    }
    pixels
}

/// Windows OCR rectangles are measured on the deskewed image. Rotate their
/// corners clockwise around the processed image center, then undo resizing.
fn mapped_box(
    rect: [f64; 4],
    processed_width: u32,
    processed_height: u32,
    width: u32,
    height: u32,
    angle: f64,
) -> Result<(Value, Vec<Value>)> {
    ensure!(
        rect.iter().all(|value| value.is_finite()) && rect[2] >= 0.0 && rect[3] >= 0.0,
        "OCR 返回了无效坐标"
    );
    let [x, y, w, h] = rect;
    let (cx, cy) = (
        f64::from(processed_width) / 2.0,
        f64::from(processed_height) / 2.0,
    );
    let (sin, cos) = angle.to_radians().sin_cos();
    let (scale_x, scale_y) = (
        f64::from(width) / f64::from(processed_width),
        f64::from(height) / f64::from(processed_height),
    );
    let points: Vec<(f64, f64)> = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
        .into_iter()
        .map(|(px, py)| {
            (
                ((cx + (px - cx) * cos - (py - cy) * sin) * scale_x).clamp(0.0, f64::from(width)),
                ((cy + (px - cx) * sin + (py - cy) * cos) * scale_y).clamp(0.0, f64::from(height)),
            )
        })
        .collect();
    let left = points.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let top = points.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let right = points.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
    let bottom = points.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    Ok((
        json!({"x":left,"y":top,"width":right-left,"height":bottom-top}),
        points
            .into_iter()
            .map(|(x, y)| json!({"x":x,"y":y}))
            .collect(),
    ))
}

fn union_bounds(words: &[Value]) -> Value {
    let left = words
        .iter()
        .map(|w| w["bounds"]["x"].as_f64().unwrap_or(0.0))
        .fold(f64::INFINITY, f64::min);
    let top = words
        .iter()
        .map(|w| w["bounds"]["y"].as_f64().unwrap_or(0.0))
        .fold(f64::INFINITY, f64::min);
    let right = words
        .iter()
        .map(|w| {
            w["bounds"]["x"].as_f64().unwrap_or(0.0) + w["bounds"]["width"].as_f64().unwrap_or(0.0)
        })
        .fold(0.0, f64::max);
    let bottom = words
        .iter()
        .map(|w| {
            w["bounds"]["y"].as_f64().unwrap_or(0.0) + w["bounds"]["height"].as_f64().unwrap_or(0.0)
        })
        .fold(0.0, f64::max);
    json!({"x":left,"y":top,"width":right-left,"height":bottom-top})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaling_and_rotation_return_original_window_coordinates() {
        assert_eq!(fitted_size(4000, 2000, 2000), (2000, 1000));
        let (bounds, polygon) =
            mapped_box([10.0, 20.0, 30.0, 10.0], 100, 50, 200, 100, 0.0).unwrap();
        assert_eq!(
            bounds,
            json!({"x":20.0,"y":40.0,"width":60.0,"height":20.0})
        );
        assert_eq!(polygon[2], json!({"x":80.0,"y":60.0}));
        let (rotated, _) = mapped_box([10.0, 20.0, 30.0, 10.0], 100, 100, 200, 200, 90.0).unwrap();
        assert!((rotated["x"].as_f64().unwrap() - 140.0).abs() < 0.0001);
        assert!((rotated["y"].as_f64().unwrap() - 20.0).abs() < 0.0001);
        assert!((rotated["width"].as_f64().unwrap() - 20.0).abs() < 0.0001);
        assert!((rotated["height"].as_f64().unwrap() - 60.0).abs() < 0.0001);
    }

    #[test]
    fn invalid_or_oversize_images_fail_explicitly_without_an_ocr_request() {
        let result = recognize_masked_png(b"not png", 100, 80);
        assert_eq!(result["status"], "error");
        assert!(result["error"].as_str().unwrap().contains("PNG"));
        let result = recognize_masked_png(&[], 20000, 20000);
        assert_eq!(result["status"], "error");
        assert!(result["lines"].as_array().unwrap().is_empty());
    }

    #[test]
    fn windows_ocr_reads_real_offscreen_fixture_or_reports_missing_system_support() {
        let png = fixture_png();
        let result = recognize_masked_png(&png, 760, 140);
        eprintln!(
            "local OCR fixture: {}",
            json!({"status":result["status"],"language":result["language"],"available_languages":result["available_languages"],"text":result["text"],"error":result["error"]})
        );
        if result["status"] == "unavailable" {
            assert!(!result["error"].as_str().unwrap_or_default().is_empty());
            return;
        }
        assert_eq!(result["status"], "ok", "{result}");
        let text = result["text"].as_str().unwrap();
        assert!(
            text.contains("4729") && text.to_ascii_uppercase().contains("OPERATOR"),
            "{result}"
        );
        assert!(!result["lines"].as_array().unwrap().is_empty());
        assert_eq!(result["coordinate_space"], "window_physical_pixels");
        assert_eq!(result["content_kind"], "untrusted_observed_window_content");
    }

    fn fixture_png() -> Vec<u8> {
        use windows::{
            core::w,
            Win32::{Foundation::COLORREF, Graphics::Gdi::*},
        };
        unsafe {
            let dc = CreateCompatibleDC(None);
            assert!(!dc.0.is_null());
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: 760,
                    biHeight: -140,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut raw = std::ptr::null_mut();
            let bitmap =
                CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut raw, None, 0).unwrap();
            let old_bitmap = SelectObject(dc, bitmap.into());
            let pixels = std::slice::from_raw_parts_mut(raw as *mut u8, 760 * 140 * 4);
            pixels.fill(255);
            let font = CreateFontW(
                -42,
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                ANTIALIASED_QUALITY,
                0,
                w!("Arial"),
            );
            assert!(!font.0.is_null());
            let old_font = SelectObject(dc, font.into());
            SetTextColor(dc, COLORREF(0));
            SetBkMode(dc, TRANSPARENT);
            assert!(TextOutW(
                dc,
                24,
                42,
                &"DSH OPERATOR 4729".encode_utf16().collect::<Vec<_>>()
            )
            .as_bool());
            let _ = GdiFlush();
            let mut rgba = pixels.to_vec();
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.swap(0, 2);
                pixel[3] = 255;
            }
            SelectObject(dc, old_font);
            SelectObject(dc, old_bitmap);
            let _ = DeleteObject(font.into());
            let _ = DeleteObject(bitmap.into());
            let _ = DeleteDC(dc);
            let mut png = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut png, 760, 140);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                encoder
                    .write_header()
                    .unwrap()
                    .write_image_data(&rgba)
                    .unwrap();
            }
            png
        }
    }
}
