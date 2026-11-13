use std::path::PathBuf;

use windows::core::w;
use windows::Win32::Foundation::{GlobalFree, HANDLE};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, IsClipboardFormatAvailable,
    OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

use crate::sys::Clipboard;

/// Standard clipboard format of a file list.
const CF_HDROP: u32 = 15;

pub struct WinClipboard;

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
        let block = drop_files(paths);
        let busy = |_| "буфер обмена занят другой программой".to_string();
        // SAFETY: the memory is allocated, filled within its size and handed to
        // the clipboard, which owns it after a successful SetClipboardData;
        // the clipboard is closed on every path after it was opened.
        unsafe {
            let mem = GlobalAlloc(GMEM_MOVEABLE, block.len())
                .map_err(|_| "не хватило памяти".to_string())?;
            let ptr = GlobalLock(mem) as *mut u8;
            if ptr.is_null() {
                let _ = GlobalFree(Some(mem));
                return Err("не хватило памяти".into());
            }
            std::ptr::copy_nonoverlapping(block.as_ptr(), ptr, block.len());
            let _ = GlobalUnlock(mem);
            if let Err(e) = OpenClipboard(None) {
                let _ = GlobalFree(Some(mem));
                return Err(busy(e));
            }
            let set =
                EmptyClipboard().and_then(|_| SetClipboardData(CF_HDROP, Some(HANDLE(mem.0))));
            let _ = CloseClipboard();
            if set.is_err() {
                let _ = GlobalFree(Some(mem));
                return Err("Windows не принял файл в буфер обмена".into());
            }
        }
        Ok(())
    }
}

/// Changes on every copy. Reading it opens nothing.
pub fn sequence() -> u32 {
    // SAFETY: no arguments, no state.
    unsafe { GetClipboardSequenceNumber() }
}

/// Password managers mark their copies with these formats, so that clipboard
/// history and cloud clipboard leave them alone.
pub fn excluded_from_history() -> bool {
    let formats = [
        w!("ExcludeClipboardContentFromMonitorProcessing"),
        w!("Clipboard Viewer Ignore"),
        w!("CanIncludeInClipboardHistory"),
    ];
    formats.iter().any(|name| {
        // SAFETY: a constant wide string; registering an existing name returns its id.
        let id = unsafe { RegisterClipboardFormatW(*name) };
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
