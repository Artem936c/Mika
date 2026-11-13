//! Screenshots through GDI and text recognition through `Windows.Media.Ocr`.

use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::BitmapDecoder;
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, SetStretchBltMode, StretchBlt, BITMAPINFO, BITMAPINFOHEADER,
    CAPTUREBLT, DIB_RGB_COLORS, HALFTONE, HBITMAP, HDC, SRCCOPY,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Com::COINIT_MULTITHREADED;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, GetWindowRect, IsIconic, SM_CXSCREEN, SM_CYSCREEN,
};

use super::with_com;
use crate::sys::{OcrWord, Screen, Shot};

pub struct GdiScreen;

/// Also draws windows that use GPU composition (browsers, Electron).
const PW_RENDERFULLCONTENT: u32 = 2;

/// Size after shrinking so that the longer side is at most `max_side`; 0 keeps the size.
pub fn fit(width: i32, height: i32, max_side: u32) -> (i32, i32, f32) {
    let longest = width.max(height);
    if max_side == 0 || longest <= max_side as i32 {
        return (width, height, 1.0);
    }
    let factor = longest as f32 / max_side as f32;
    (
        ((width as f32 / factor).round() as i32).max(1),
        ((height as f32 / factor).round() as i32).max(1),
        factor,
    )
}

/// Frees GDI objects when the capture ends, on success or on error.
struct Gdi {
    screen: HDC,
    dcs: Vec<HDC>,
    bitmaps: Vec<HBITMAP>,
}

impl Drop for Gdi {
    fn drop(&mut self) {
        // SAFETY: every handle here was created by this capture and is no longer selected.
        unsafe {
            for dc in &self.dcs {
                let _ = DeleteDC(*dc);
            }
            for b in &self.bitmaps {
                let _ = DeleteObject((*b).into());
            }
            ReleaseDC(None, self.screen);
        }
    }
}

/// Pixels as RGB, the picture size, the shrink factor and the screen position
/// of the top left corner.
#[allow(clippy::type_complexity)]
fn grab(
    window: Option<u64>,
    max_side: u32,
    from_screen: bool,
) -> Result<(Vec<u8>, u32, u32, f32, (i32, i32)), String> {
    super::physical_pixels();
    // SAFETY: GDI calls on handles owned by `Gdi`; buffers outlive the calls that fill them.
    unsafe {
        let hwnd = window.map(|id| HWND(id as usize as *mut core::ffi::c_void));
        let (width, height, origin) = match hwnd {
            Some(h) => {
                if IsIconic(h).as_bool() {
                    return Err("окно свёрнуто: сначала разверни его".into());
                }
                let mut r = RECT::default();
                GetWindowRect(h, &mut r).map_err(|_| "окно закрыто или недоступно".to_string())?;
                (r.right - r.left, r.bottom - r.top, (r.left, r.top))
            }
            // The main monitor: it starts at (0, 0) of the screen.
            None => (
                GetSystemMetrics(SM_CXSCREEN),
                GetSystemMetrics(SM_CYSCREEN),
                (0, 0),
            ),
        };
        if width <= 0 || height <= 0 {
            return Err("нечего снимать: у окна нет размера".into());
        }
        let screen = GetDC(None);
        if screen.is_invalid() {
            return Err("экран недоступен: возможно, сеанс заблокирован или отключён".into());
        }
        let mut gdi = Gdi {
            screen,
            dcs: Vec::new(),
            bitmaps: Vec::new(),
        };
        let full_dc = CreateCompatibleDC(Some(screen));
        gdi.dcs.push(full_dc);
        let full = CreateCompatibleBitmap(screen, width, height);
        if full.is_invalid() {
            return Err("не хватило памяти на снимок".into());
        }
        gdi.bitmaps.push(full);
        let old_full = SelectObject(full_dc, full.into());

        // A window drawn by itself may differ from what the screen shows; a
        // window in front is copied from the screen, exactly where clicks land.
        let printed = !from_screen
            && hwnd.is_some_and(|h| {
                PrintWindow(h, full_dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).as_bool()
            });
        if !printed {
            BitBlt(
                full_dc,
                0,
                0,
                width,
                height,
                Some(screen),
                origin.0,
                origin.1,
                SRCCOPY | CAPTUREBLT,
            )
            .map_err(|_| "Windows не дал снять экран".to_string())?;
        }

        let (out_w, out_h, factor) = fit(width, height, max_side);
        let result = if factor > 1.0 {
            let small_dc = CreateCompatibleDC(Some(screen));
            gdi.dcs.push(small_dc);
            let small = CreateCompatibleBitmap(screen, out_w, out_h);
            if small.is_invalid() {
                return Err("не хватило памяти на снимок".into());
            }
            gdi.bitmaps.push(small);
            let old_small = SelectObject(small_dc, small.into());
            SetStretchBltMode(small_dc, HALFTONE);
            let _ = StretchBlt(
                small_dc,
                0,
                0,
                out_w,
                out_h,
                Some(full_dc),
                0,
                0,
                width,
                height,
                SRCCOPY,
            );
            SelectObject(small_dc, old_small);
            small
        } else {
            full
        };
        // GetDIBits needs the bitmap out of every device context.
        SelectObject(full_dc, old_full);

        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: out_w,
                // Negative height: rows come top first.
                biHeight: -out_h,
                biPlanes: 1,
                biBitCount: 32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bgra = vec![0u8; out_w as usize * out_h as usize * 4];
        let rows = GetDIBits(
            screen,
            result,
            0,
            out_h as u32,
            Some(bgra.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        if rows == 0 {
            return Err("Windows не отдал пиксели снимка".into());
        }
        let mut rgb = Vec::with_capacity(out_w as usize * out_h as usize * 3);
        for [b, g, r, _] in bgra.as_chunks::<4>().0 {
            rgb.extend_from_slice(&[*r, *g, *b]);
        }
        Ok((rgb, out_w as u32, out_h as u32, factor, origin))
    }
}

pub fn encode_png(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(rgb).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

pub(super) fn ocr_engine() -> Result<OcrEngine, String> {
    if let Ok(engine) = OcrEngine::TryCreateFromUserProfileLanguages() {
        return Ok(engine);
    }
    for tag in ["ru", "en-US"] {
        if let Ok(lang) = Language::CreateLanguage(&HSTRING::from(tag)) {
            if let Ok(engine) = OcrEngine::TryCreateFromLanguage(&lang) {
                return Ok(engine);
            }
        }
    }
    Err("в Windows не установлен язык для распознавания текста: добавь его в «Параметры → Время и язык → Язык» (компонент «Оптическое распознавание символов»)".into())
}

fn recognize(png: Vec<u8>) -> windows::core::Result<Result<String, String>> {
    let result = match ocr_result(png)? {
        Ok(r) => r,
        Err(e) => return Ok(Err(e)),
    };
    let mut lines = Vec::new();
    for line in result.Lines()? {
        lines.push(line.Text()?.to_string());
    }
    Ok(Ok(lines.join("\n")))
}

fn recognize_words(png: Vec<u8>) -> windows::core::Result<Result<Vec<OcrWord>, String>> {
    let result = match ocr_result(png)? {
        Ok(r) => r,
        Err(e) => return Ok(Err(e)),
    };
    let mut out = Vec::new();
    for (i, line) in result.Lines()?.into_iter().enumerate() {
        for word in line.Words()? {
            let r = word.BoundingRect()?;
            out.push(OcrWord {
                text: word.Text()?.to_string(),
                line: i as u32,
                x: r.X,
                y: r.Y,
                w: r.Width,
                h: r.Height,
            });
        }
    }
    Ok(Ok(out))
}

fn ocr_result(
    png: Vec<u8>,
) -> windows::core::Result<Result<windows::Media::Ocr::OcrResult, String>> {
    let stream = InMemoryRandomAccessStream::new()?;
    let writer = DataWriter::CreateDataWriter(&stream)?;
    writer.WriteBytes(&png)?;
    writer.StoreAsync()?.join()?;
    writer.FlushAsync()?.join()?;
    writer.DetachStream()?;
    stream.Seek(0)?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?.join()?;
    let bitmap = decoder.GetSoftwareBitmapAsync()?.join()?;
    let engine = match ocr_engine() {
        Ok(e) => e,
        Err(e) => return Ok(Err(e)),
    };
    let limit = OcrEngine::MaxImageDimension()? as i32;
    if bitmap.PixelWidth()? > limit || bitmap.PixelHeight()? > limit {
        return Ok(Err(format!(
            "снимок больше {limit} пикселей по стороне, распознавание его не берёт"
        )));
    }
    Ok(Ok(engine.RecognizeAsync(&bitmap)?.join()?))
}

impl Screen for GdiScreen {
    fn capture(&self, window: Option<u64>, max_side: u32) -> Result<Shot, String> {
        let (rgb, width, height, factor, origin) = grab(window, max_side, false)?;
        Ok(Shot {
            png: encode_png(&rgb, width, height)?,
            width,
            height,
            factor,
            origin,
        })
    }

    fn capture_visible(&self, window: u64, max_side: u32) -> Result<Shot, String> {
        let (rgb, width, height, factor, origin) = grab(Some(window), max_side, true)?;
        Ok(Shot {
            png: encode_png(&rgb, width, height)?,
            width,
            height,
            factor,
            origin,
        })
    }

    fn ocr(&self, png: &[u8]) -> Result<String, String> {
        let png = png.to_vec();
        with_com(COINIT_MULTITHREADED, move || {
            recognize(png)
                .map_err(|e| format!("распознавание текста не сработало: {}", e.message()))?
        })
    }

    fn ocr_words(&self, png: &[u8]) -> Result<Vec<OcrWord>, String> {
        let png = png.to_vec();
        with_com(COINIT_MULTITHREADED, move || {
            recognize_words(png)
                .map_err(|e| format!("распознавание текста не сработало: {}", e.message()))?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_small_pictures_and_shrinks_large_ones() {
        assert_eq!(fit(800, 600, 1600), (800, 600, 1.0));
        assert_eq!(fit(3200, 1800, 1600), (1600, 900, 2.0));
        assert_eq!(fit(1000, 4000, 1600), (400, 1600, 2.5));
        assert_eq!(fit(5000, 5000, 0), (5000, 5000, 1.0));
    }

    #[test]
    #[ignore = "looks at the screen and the mixer of this computer"]
    fn live_words_with_boxes_and_the_mixer() {
        use crate::sys::Volume;
        // A picture from MIKA_OCR_PNG, or the screen.
        let (png, width) = match std::env::var("MIKA_OCR_PNG") {
            Ok(path) => {
                let png = std::fs::read(path).unwrap();
                let pic = crate::grid::decode(&png).unwrap();
                (png, pic.width)
            }
            Err(_) => {
                let shot = GdiScreen.capture(None, 0).unwrap();
                (shot.png, shot.width)
            }
        };
        let words = GdiScreen.ocr_words(&png).unwrap();
        println!("picture width {width}, words {}", words.len());
        for w in words.iter().take(8) {
            println!("{:?}", w);
        }
        assert!(words
            .iter()
            .all(|w| w.x >= 0.0 && w.x + w.w <= width as f32 + 1.0));
        let found = crate::grid::find_label(&words, "Сохранить как");
        println!("label: {found:?}");
        let apps = super::super::audio::CoreAudioVolume.apps();
        println!("mixer: {apps:?}");
        assert!(apps.is_ok());
    }

    #[test]
    fn png_has_signature_and_size() {
        let png = encode_png(&[255, 0, 0, 0, 255, 0], 2, 1).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        // IHDR: width and height as big-endian u32 right after the chunk header.
        assert_eq!(&png[16..24], &[0, 0, 0, 2, 0, 0, 0, 1]);
    }
}
