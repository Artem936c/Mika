//! Screenshots through GDI and text recognition through `Windows.Media.Ocr`.

use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::BitmapDecoder;
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, SetStretchBltMode, StretchBlt, BITMAPINFO, BITMAPINFOHEADER,
    CAPTUREBLT, DIB_RGB_COLORS, HALFTONE, HBITMAP, HDC, SRCCOPY,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Com::COINIT_MULTITHREADED;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetSystemMetrics, GetWindowDisplayAffinity, GetWindowLongW, GetWindowRect,
    IsIconic, IsWindowVisible, SetWindowDisplayAffinity, WindowFromPoint, GA_ROOT, GWL_EXSTYLE,
    SM_CXSCREEN, SM_CYSCREEN, WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
};

use super::with_com;
use super::wnd::{process_id, top_level};
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

/// Keeps this process's windows (the island, the avatar, the click frame) out
/// of screen captures while it lives; the monitor still shows them. A picture
/// for clicking must show the window under them, not Mika's own text.
struct OwnWindowsHidden {
    hidden: Vec<HWND>,
    /// Visible windows of this process that Windows would not leave out: the
    /// picture shows them, so they are painted over.
    shown: Vec<HWND>,
}

impl OwnWindowsHidden {
    fn new() -> Self {
        // SAFETY: plain query.
        let own = unsafe { GetCurrentProcessId() };
        let (mut hidden, mut shown) = (Vec::new(), Vec::new());
        for h in top_level() {
            if process_id(h) != own {
                continue;
            }
            // SAFETY: plain calls on a window of this process; a closed one just fails.
            unsafe {
                if !IsWindowVisible(h).as_bool() {
                    continue;
                }
                let mut now = 0u32;
                if GetWindowDisplayAffinity(h, &mut now).is_err() {
                    shown.push(h);
                    continue;
                }
                // Already kept out of captures, or shown black in them.
                if now != WDA_NONE.0 {
                    continue;
                }
                match SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE) {
                    Ok(()) => hidden.push(h),
                    Err(e) => {
                        log::warn!(
                            "own window stays in the capture, painted over: {}",
                            e.message()
                        );
                        shown.push(h);
                    }
                }
            }
        }
        if !hidden.is_empty() {
            // The next composed frame leaves them out.
            std::thread::sleep(std::time::Duration::from_millis(60));
        }
        Self { hidden, shown }
    }
}

impl Drop for OwnWindowsHidden {
    fn drop(&mut self) {
        for h in &self.hidden {
            // SAFETY: undoes the change made in `new` on a window of this process.
            let _ = unsafe { SetWindowDisplayAffinity(*h, WDA_NONE) };
        }
    }
}

/// Whether a window of this process with these extended styles would take a
/// click Mika makes where it lies. Her topmost windows let it through: the
/// avatar and the click frame always, the island at the spot of the click,
/// which the app keeps clear while she clicks (`Event::UiTarget`). Her
/// ordinary window (the island turned into a window) takes it.
fn takes_own_click(ex_style: u32) -> bool {
    ex_style & (WS_EX_TRANSPARENT.0 | WS_EX_TOPMOST.0) == 0
}

/// Side of the squares checked for a cover, picture pixels.
const COVER_CELL: u32 = 8;
/// Colour of the parts of a window that lie under another program's window.
const COVER_GREY: u8 = 128;

/// Paints grey every square of the picture whose middle `covered` names as
/// someone else's. Returns how many squares were painted.
fn paint_covered(
    rgb: &mut [u8],
    width: u32,
    height: u32,
    mut covered: impl FnMut(u32, u32) -> bool,
) -> usize {
    let mut painted = 0;
    for y0 in (0..height).step_by(COVER_CELL as usize) {
        for x0 in (0..width).step_by(COVER_CELL as usize) {
            let (x1, y1) = ((x0 + COVER_CELL).min(width), (y0 + COVER_CELL).min(height));
            if !covered((x0 + x1) / 2, (y0 + y1) / 2) {
                continue;
            }
            painted += 1;
            for y in y0..y1 {
                let row = ((y * width + x0) * 3) as usize;
                rgb[row..row + ((x1 - x0) * 3) as usize].fill(COVER_GREY);
            }
        }
    }
    painted
}

fn overlaps(a: &RECT, b: &RECT) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

fn holds(r: &RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

/// Whether the point of the target is covered: by a window of Mika's that is
/// in the way, or by another program's window that takes the click there
/// (`theirs_at`, asked only inside `others`).
fn covered_at(
    p: POINT,
    mine: &[RECT],
    others: &[RECT],
    theirs_at: impl FnOnce(POINT) -> bool,
) -> bool {
    mine.iter().any(|r| holds(r, p)) || (others.iter().any(|r| holds(r, p)) && theirs_at(p))
}

/// Paints over the parts of `target` that other windows cover on the screen:
/// a click there lands in them, and their content is neither the target's text
/// to click nor the target's picture for the vision model. Other programs'
/// windows that let clicks through (hidden panels, transparent overlays) and
/// the target's own pop-ups do not count. Mika's windows count when the
/// picture shows them (`shown`) or when they would take the click.
fn paint_over_others(
    target: HWND,
    rgb: &mut [u8],
    (width, height): (u32, u32),
    factor: f32,
    origin: (i32, i32),
    shown: &[HWND],
) {
    // SAFETY: plain queries on window handles; a closed window just fails.
    unsafe {
        let own = GetCurrentProcessId();
        let theirs = process_id(target);
        let mut area = RECT::default();
        if GetWindowRect(target, &mut area).is_err() {
            return;
        }
        let (mut mine, mut others) = (Vec::new(), Vec::new());
        for h in top_level() {
            if h == target {
                break;
            }
            let pid = process_id(h);
            if pid == theirs || !IsWindowVisible(h).as_bool() {
                continue;
            }
            let mut r = RECT::default();
            if GetWindowRect(h, &mut r).is_err() || !overlaps(&r, &area) {
                continue;
            }
            if pid != own {
                others.push(r);
            } else if shown.contains(&h) || takes_own_click(GetWindowLongW(h, GWL_EXSTYLE) as u32) {
                mine.push(r);
            }
        }
        if mine.is_empty() && others.is_empty() {
            return;
        }
        let painted = paint_covered(rgb, width, height, |x, y| {
            let p = POINT {
                x: origin.0 + (x as f32 * factor) as i32,
                y: origin.1 + (y as f32 * factor) as i32,
            };
            covered_at(p, &mine, &others, |p| {
                let root = GetAncestor(WindowFromPoint(p), GA_ROOT);
                let pid = process_id(root);
                root != target && pid != theirs && pid != own
            })
        });
        if painted > 0 {
            log::debug!("capture: {painted} squares under other windows painted over");
        }
    }
}

/// Pixels as RGB, the picture size, the shrink factor and the screen position
/// of the top left corner. Taken `from_screen`, the picture leaves out Mika's
/// windows and paints over other programs' windows lying on the target.
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
        let mut shown = Vec::new();
        if !printed {
            let hidden = from_screen.then(OwnWindowsHidden::new);
            if let Some(h) = &hidden {
                shown.clone_from(&h.shown);
            }
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
        if let (true, Some(h)) = (from_screen, hwnd) {
            paint_over_others(
                h,
                &mut rgb,
                (out_w as u32, out_h as u32),
                factor,
                origin,
                &shown,
            );
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
    use windows::Win32::UI::WindowsAndMessaging::WS_EX_LAYERED;

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
    fn squares_under_other_windows_are_painted_over() {
        // 20×10: the right half belongs to another window.
        let (w, h) = (20u32, 10u32);
        let mut rgb = vec![0u8; (w * h * 3) as usize];
        let mut asked = Vec::new();
        let painted = paint_covered(&mut rgb, w, h, |x, y| {
            asked.push((x, y));
            x >= 10
        });
        // Squares of 8: columns 0–7, 8–15, 16–19; rows 0–7, 8–9.
        assert_eq!(
            asked,
            vec![(4, 4), (12, 4), (18, 4), (4, 9), (12, 9), (18, 9)]
        );
        assert_eq!(painted, 4);
        let px = |x: u32, y: u32| rgb[((y * w + x) * 3) as usize];
        assert_eq!(
            (px(7, 0), px(8, 0), px(19, 9), px(0, 9)),
            (0, COVER_GREY, COVER_GREY, 0)
        );
        let r = |l, t, rr, b| RECT {
            left: l,
            top: t,
            right: rr,
            bottom: b,
        };
        assert!(overlaps(&r(0, 0, 10, 10), &r(9, 9, 20, 20)));
        assert!(!overlaps(&r(0, 0, 10, 10), &r(10, 0, 20, 10)));
        assert!(holds(&r(0, 0, 10, 10), POINT { x: 0, y: 9 }));
        assert!(!holds(&r(0, 0, 10, 10), POINT { x: 10, y: 5 }));
    }

    #[test]
    fn mikas_windows_in_the_way_are_painted_over_too() {
        let r = |l, t, rr, b| RECT {
            left: l,
            top: t,
            right: rr,
            bottom: b,
        };
        let at = |x, y| POINT { x, y };
        // Her window in the way covers the point whoever takes the click.
        let mine = [r(0, 0, 100, 50)];
        assert!(covered_at(at(10, 10), &mine, &[], |_| unreachable!()));
        assert!(!covered_at(at(10, 60), &mine, &[], |_| unreachable!()));
        // Another program's window covers it only where it takes the click.
        let others = [r(0, 0, 100, 100)];
        assert!(covered_at(at(10, 60), &mine, &others, |_| true));
        assert!(!covered_at(at(10, 60), &mine, &others, |_| false));
        assert!(!covered_at(at(200, 60), &mine, &others, |_| unreachable!()));

        // Her ordinary window takes the click; topmost or click-through ones
        // (the island, the avatar, the frame) let it through.
        assert!(takes_own_click(0));
        assert!(takes_own_click(WS_EX_LAYERED.0));
        assert!(!takes_own_click(WS_EX_TOPMOST.0 | WS_EX_LAYERED.0));
        assert!(!takes_own_click(WS_EX_TRANSPARENT.0 | WS_EX_LAYERED.0));
        assert!(!takes_own_click(
            WS_EX_TOPMOST.0 | WS_EX_TRANSPARENT.0 | WS_EX_LAYERED.0
        ));
    }

    #[test]
    fn png_has_signature_and_size() {
        let png = encode_png(&[255, 0, 0, 0, 255, 0], 2, 1).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        // IHDR: width and height as big-endian u32 right after the chunk header.
        assert_eq!(&png[16..24], &[0, 0, 0, 2, 0, 0, 0, 1]);
    }
}
