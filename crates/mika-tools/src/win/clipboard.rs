use std::path::PathBuf;
use std::time::Duration;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};

use crate::sys::{Clipboard, SavedClip};

/// Standard clipboard format of a file list.
const CF_HDROP: u32 = 15;
/// Standard formats kept when she borrows the clipboard: text and its
/// language, a picture, files.
const KEPT: [u32; 5] = [
    13, // CF_UNICODETEXT
    16, // CF_LOCALE
    8,  // CF_DIB
    17, // CF_DIBV5
    CF_HDROP,
];
/// A bigger format is not kept.
const MAX_FORMAT_BYTES: usize = 128 << 20;

pub struct WinClipboard;

fn register(name: PCWSTR) -> u32 {
    // SAFETY: a constant wide string; registering an existing name returns its id.
    unsafe { RegisterClipboardFormatW(name) }
}

/// Named formats kept: the picture as PNG, rich text, and whether files were
/// copied or cut. Others are left out: a program may render dozens of them on
/// demand, slowly.
fn kept_named() -> [u32; 5] {
    [
        w!("PNG"),
        w!("HTML Format"),
        w!("Rich Text Format"),
        w!("Preferred DropEffect"),
        w!("Shell IDList Array"),
    ]
    .map(register)
}

/// Password managers mark their copies with these formats, so that clipboard
/// history and cloud clipboard leave them alone.
fn history_marks() -> [u32; 3] {
    [
        w!("ExcludeClipboardContentFromMonitorProcessing"),
        w!("Clipboard Viewer Ignore"),
        w!("CanIncludeInClipboardHistory"),
    ]
    .map(register)
}

/// The clipboard opened by this thread, closed when dropped. Another program
/// may hold it for a moment, so it is tried a few times.
struct Opened;

impl Opened {
    fn new() -> Result<Self, String> {
        for _ in 0..10 {
            // SAFETY: no pointers; `Drop` closes it.
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Ok(Opened);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Err("буфер обмена занят другой программой".into())
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        // SAFETY: opened by this thread in `new`.
        let _ = unsafe { CloseClipboard() };
    }
}

/// Bytes of one format of the open clipboard; `None` when it has none.
fn read(format: u32) -> Option<Vec<u8>> {
    // SAFETY: the clipboard is open and owns the handle; its memory is read
    // only between lock and unlock and within its size.
    unsafe {
        let mem = HGLOBAL(GetClipboardData(format).ok()?.0);
        let size = GlobalSize(mem);
        if size == 0 || size > MAX_FORMAT_BYTES {
            return None;
        }
        let ptr = GlobalLock(mem) as *const u8;
        if ptr.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(ptr, size).to_vec();
        let _ = GlobalUnlock(mem);
        Some(bytes)
    }
}

/// Hands a copy of the bytes to the open clipboard.
fn put(format: u32, bytes: &[u8]) -> Result<(), String> {
    // SAFETY: the memory is allocated, filled within its size and handed to
    // the clipboard, which owns it after a successful SetClipboardData.
    unsafe {
        let mem = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))
            .map_err(|_| "не хватило памяти".to_string())?;
        let ptr = GlobalLock(mem) as *mut u8;
        if ptr.is_null() {
            let _ = GlobalFree(Some(mem));
            return Err("не хватило памяти".into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        let _ = GlobalUnlock(mem);
        if SetClipboardData(format, Some(HANDLE(mem.0))).is_err() {
            let _ = GlobalFree(Some(mem));
            return Err("Windows не принял данные в буфер обмена".into());
        }
    }
    Ok(())
}

/// Empties the open clipboard: this program owns it then.
fn empty() -> Result<(), String> {
    // SAFETY: the clipboard is open.
    unsafe { EmptyClipboard() }.map_err(|_| "Windows не дал очистить буфер обмена".to_string())
}

/// The `CF_HDROP` block: a `DROPFILES` head (offset of the list, a point,
/// two flags; the last one says the names are UTF-16), then each path with a
/// zero, then one more zero.
pub fn drop_files(paths: &[PathBuf]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&20u32.to_le_bytes());
    out.extend_from_slice(&[0u8; 12]);
    out.extend_from_slice(&1u32.to_le_bytes());
    for p in paths {
        for unit in p.to_string_lossy().encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
    }
    out.extend_from_slice(&[0, 0]);
    out
}

impl Clipboard for WinClipboard {
    fn get(&self) -> Result<String, String> {
        arboard::Clipboard::new()
            .and_then(|mut c| c.get_text())
            .map_err(|e| format!("буфер обмена недоступен: {e}"))
    }

    fn set(&self, text: &str) -> Result<(), String> {
        arboard::Clipboard::new()
            .and_then(|mut c| c.set_text(text.to_string()))
            .map_err(|e| format!("буфер обмена недоступен: {e}"))
    }

    fn set_files(&self, paths: &[PathBuf]) -> Result<(), String> {
        let _open = Opened::new()?;
        empty()
            .and_then(|_| put(CF_HDROP, &drop_files(paths)))
            .map_err(|_| "Windows не принял файл в буфер обмена".to_string())
    }

    /// Text, a picture, files, rich text and the marks of password managers,
    /// in the order the clipboard lists them.
    fn save(&self) -> Result<SavedClip, String> {
        let kept: Vec<u32> = KEPT.into_iter().chain(kept_named()).collect();
        let marks = history_marks();
        let cloud = register(w!("CanUploadToCloudClipboard"));
        let _open = Opened::new()?;
        let mut formats = Vec::new();
        // SAFETY: the clipboard is open; listing its formats reads no memory.
        let mut format = unsafe { EnumClipboardFormats(0) };
        while format != 0 {
            if marks.contains(&format) || format == cloud {
                // A mark counts by being there; it may come without data.
                let bytes = read(format).unwrap_or_else(|| 0u32.to_le_bytes().to_vec());
                formats.push((format, bytes));
            } else if kept.contains(&format) {
                if let Some(bytes) = read(format) {
                    formats.push((format, bytes));
                }
            }
            // SAFETY: as above.
            format = unsafe { EnumClipboardFormats(format) };
        }
        Ok(SavedClip { formats })
    }

    fn restore(&self, saved: &SavedClip) -> Result<(), String> {
        let _open = Opened::new()?;
        empty()?;
        let lost = saved
            .formats
            .iter()
            .filter(|(format, bytes)| put(*format, bytes).is_err())
            .count();
        if lost > 0 {
            return Err(format!(
                "в буфер обмена не вернулись {lost} форматов из {}",
                saved.formats.len()
            ));
        }
        Ok(())
    }
}

/// Changes on every copy. Reading it opens nothing.
pub fn sequence() -> u32 {
    // SAFETY: no arguments, no state.
    unsafe { GetClipboardSequenceNumber() }
}

/// The clipboard holds a copy marked by a password manager.
pub fn excluded_from_history() -> bool {
    history_marks().into_iter().any(|id| {
        // SAFETY: only asks whether a format is present.
        id != 0 && unsafe { IsClipboardFormatAvailable(id) }.is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Puts this source file into the real clipboard; `Get-Clipboard -Format
    /// FileDropList` should show it afterwards.
    #[test]
    #[ignore = "changes the real clipboard"]
    fn live_file_into_the_clipboard() {
        let me = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src\\win\\clipboard.rs");
        WinClipboard.set_files(&[me]).unwrap();
    }

    /// Copy a picture or a password from a password manager first: after the
    /// test the clipboard holds it again, with its marks.
    #[test]
    #[ignore = "changes the real clipboard"]
    fn live_the_clipboard_comes_back_whole() {
        let saved = WinClipboard.save().unwrap();
        let marked = excluded_from_history();
        WinClipboard.set("тест Мики").unwrap();
        WinClipboard.restore(&saved).unwrap();
        let ids = |s: &SavedClip| s.formats.iter().map(|(f, _)| *f).collect::<Vec<_>>();
        assert_eq!(ids(&WinClipboard.save().unwrap()), ids(&saved));
        assert_eq!(excluded_from_history(), marked);
    }

    #[test]
    fn the_file_list_block_has_its_head_and_double_zero() {
        let b = drop_files(&[PathBuf::from("C:\\a.jpg")]);
        assert_eq!(&b[..4], &20u32.to_le_bytes());
        assert_eq!(&b[16..20], &1u32.to_le_bytes());
        let names: Vec<u16> = b[20..]
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(String::from_utf16_lossy(&names[..8]), "C:\\a.jpg");
        assert_eq!(&names[8..], &[0, 0]);
    }
}
