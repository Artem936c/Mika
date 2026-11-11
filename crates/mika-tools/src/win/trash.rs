use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::UI::Shell::{
    SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE,
    SHFILEOPSTRUCTW,
};

use crate::sys::Trash;

pub struct RecycleBin;

impl Trash for RecycleBin {
    fn trash(&self, path: &Path) -> Result<(), String> {
        if !path.exists() {
            return Err(format!("нет такого пути: {}", path.display()));
        }
        // The API wants a list ending with two zero characters.
        let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();
        from.extend([0, 0]);
        let mut op = SHFILEOPSTRUCTW {
            wFunc: FO_DELETE,
            pFrom: PCWSTR(from.as_ptr()),
            fFlags: (FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_SILENT.0 | FOF_NOERRORUI.0)
                as u16,
            ..Default::default()
        };
        // SAFETY: `from` outlives the call; the struct is fully initialised.
        let code = unsafe { SHFileOperationW(&mut op) };
        if code == 0 && !op.fAnyOperationsAborted.as_bool() {
            Ok(())
        } else {
            Err(format!("не удалось переместить в корзину (код {code})"))
        }
    }
}
