//! The Windows "Save as" and "Open" dialogs.
//!
//! A path chosen here comes from the user's own hand in a window the program
//! opened itself: nothing the webview says can name a file. Any part of the
//! app that needs a file picker should use this module.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Controls::Dialogs::{
    CommDlgExtendedError, GetOpenFileNameW, GetSaveFileNameW, OFN_ALLOWMULTISELECT,
    OFN_DONTADDTORECENT, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_NOCHANGEDIR,
    OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};

use crate::strings::tr;

/// Room for the chosen path, UTF-16 units: the longest path Windows gives.
const FILE_BUFFER: usize = 32768;

/// Text for a Windows call: no zero inside, one at the end.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16()
        .filter(|unit| *unit != 0)
        .chain(std::iter::once(0))
        .collect()
}

/// One entry of the file type list: `label (pattern)\0pattern\0\0`.
fn filter_wide(label: &str, pattern: &str) -> Vec<u16> {
    let mut out = wide(&format!("{label} ({pattern})"));
    out.extend(wide(pattern));
    out.push(0);
    out
}

/// Several entries of the file type list, closed by one more zero.
fn filters_wide(filters: &[(&str, &str)]) -> Vec<u16> {
    let mut out = Vec::new();
    for (label, pattern) in filters {
        out.extend(wide(&format!("{label} ({pattern})")));
        out.extend(wide(pattern));
    }
    out.push(0);
    out
}

/// The files chosen in a dialog that allows several: one full path, or the
/// folder followed by the names, each closed by a zero, the list by two.
fn chosen_files(buffer: &[u16]) -> Vec<PathBuf> {
    let parts: Vec<PathBuf> = buffer
        .split(|unit| *unit == 0)
        .take_while(|part| !part.is_empty())
        .map(|part| PathBuf::from(OsString::from_wide(part)))
        .collect();
    match parts.split_first() {
        None => Vec::new(),
        Some((one, [])) => vec![one.clone()],
        Some((folder, names)) => names.iter().map(|n| folder.join(n)).collect(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Save,
    Open,
    OpenMany,
}

/// What a dialog is opened with, ready to cross to its thread.
struct Request {
    kind: Kind,
    owner: isize,
    title: Vec<u16>,
    filter: Vec<u16>,
    default_name: Vec<u16>,
    default_ext: Vec<u16>,
}

/// Shows the dialog and waits for it. Runs on a thread of its own.
fn show(request: Request) -> Result<Vec<PathBuf>, String> {
    let mut file = vec![0u16; FILE_BUFFER];
    // The name offered in "Save as"; the zero at its end is already there.
    let name = &request.default_name[..request.default_name.len().saturating_sub(1)];
    if name.len() < FILE_BUFFER {
        file[..name.len()].copy_from_slice(name);
    }
    let common =
        OFN_EXPLORER | OFN_NOCHANGEDIR | OFN_HIDEREADONLY | OFN_PATHMUSTEXIST | OFN_DONTADDTORECENT;
    // SAFETY: an all-zero OPENFILENAMEW is its documented empty state: null
    // strings, no hook, no template.
    let mut ofn: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    ofn.hwndOwner = request.owner as HWND;
    ofn.lpstrFilter = request.filter.as_ptr();
    ofn.nFilterIndex = 1;
    ofn.lpstrFile = file.as_mut_ptr();
    ofn.nMaxFile = FILE_BUFFER as u32;
    ofn.lpstrTitle = request.title.as_ptr();
    // No initial folder: Windows remembers the last one.
    if request.default_ext.len() > 1 {
        ofn.lpstrDefExt = request.default_ext.as_ptr();
    }
    ofn.Flags = match request.kind {
        Kind::Save => common | OFN_OVERWRITEPROMPT,
        Kind::Open => common | OFN_FILEMUSTEXIST,
        Kind::OpenMany => common | OFN_FILEMUSTEXIST | OFN_ALLOWMULTISELECT,
    };
    // SAFETY: every pointer in `ofn` points into a buffer that lives until
    // the call returns, and `nMaxFile` is the real length of the file buffer.
    // The dialog runs its own message loop on this thread.
    let chosen = unsafe {
        match request.kind {
            Kind::Save => GetSaveFileNameW(&mut ofn),
            Kind::Open | Kind::OpenMany => GetOpenFileNameW(&mut ofn),
        }
    };
    if chosen == 0 {
        // SAFETY: plain query of the last dialog error of this thread.
        return match unsafe { CommDlgExtendedError() } {
            // The user closed the dialog.
            0 => Ok(Vec::new()),
            code => {
                log::warn!("file dialog failed: {code:#x}");
                Err(tr("error.backup_dialog"))
            }
        };
    }
    if request.kind == Kind::OpenMany {
        return Ok(chosen_files(&file));
    }
    let len = file.iter().position(|unit| *unit == 0).unwrap_or(0);
    if len == 0 {
        return Ok(Vec::new());
    }
    Ok(vec![PathBuf::from(OsString::from_wide(&file[..len]))])
}

/// A fresh thread has no COM state of ours, and the dialog pumps its own
/// messages there.
fn run(request: Request) -> Result<Vec<PathBuf>, String> {
    std::thread::spawn(move || show(request))
        .join()
        .unwrap_or_else(|_| Err(tr("error.backup_dialog")))
}

/// Blocking. Runs the Windows dialog on a thread of its own and waits.
/// `owner` is the window the dialog belongs to (0 for none). `filter` is
/// (label, pattern), such as ("Файл копии", "*.mikabackup"). `Ok(None)` when
/// the user cancelled. Call it inside `spawn_blocking`.
pub fn pick_save(
    owner: isize,
    title: &str,
    filter: (&str, &str),
    default_name: &str,
    default_ext: &str,
) -> Result<Option<PathBuf>, String> {
    run(Request {
        kind: Kind::Save,
        owner,
        title: wide(title),
        filter: filter_wide(filter.0, filter.1),
        default_name: wide(default_name),
        default_ext: wide(default_ext),
    })
    .map(|paths| paths.into_iter().next())
}

/// Blocking, like `pick_save`. The file must exist.
pub fn pick_open(
    owner: isize,
    title: &str,
    filter: (&str, &str),
) -> Result<Option<PathBuf>, String> {
    run(Request {
        kind: Kind::Open,
        owner,
        title: wide(title),
        filter: filter_wide(filter.0, filter.1),
        default_name: wide(""),
        default_ext: wide(""),
    })
    .map(|paths| paths.into_iter().next())
}

/// Blocking, like `pick_open`, and several files may be chosen. `filters`
/// are the entries of the type list, the first one shown. Empty when the
/// user cancelled.
pub fn pick_open_many(
    owner: isize,
    title: &str,
    filters: &[(&str, &str)],
) -> Result<Vec<PathBuf>, String> {
    run(Request {
        kind: Kind::OpenMany,
        owner,
        title: wide(title),
        filter: filters_wide(filters),
        default_name: wide(""),
        default_ext: wide(""),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn filter_is_a_label_and_a_pattern_closed_by_two_zeros() {
        assert_eq!(
            filter_wide("Файл копии", "*.mikabackup"),
            units("Файл копии (*.mikabackup)\0*.mikabackup\0\0")
        );
        // A zero inside a text would cut the list short.
        assert_eq!(filter_wide("a\0b", "*.x"), units("ab (*.x)\0*.x\0\0"));
    }

    #[test]
    fn several_filters_are_closed_by_one_more_zero() {
        assert_eq!(
            filters_wide(&[("A", "*.a"), ("B", "*.*")]),
            units("A (*.a)\0*.a\0B (*.*)\0*.*\0\0")
        );
    }

    #[test]
    fn one_or_several_chosen_files() {
        assert_eq!(
            chosen_files(&units("C:\\d\\a.png\0\0garbage")),
            vec![PathBuf::from("C:\\d\\a.png")]
        );
        assert_eq!(
            chosen_files(&units("C:\\d\0a.png\0b c.txt\0\0")),
            vec![
                PathBuf::from("C:\\d").join("a.png"),
                PathBuf::from("C:\\d").join("b c.txt")
            ]
        );
        assert!(chosen_files(&units("\0\0")).is_empty());
    }

    #[test]
    fn texts_end_with_one_zero() {
        assert_eq!(wide("ab"), vec![97, 98, 0]);
        assert_eq!(wide(""), vec![0]);
        assert_eq!(wide("a\0b"), vec![97, 98, 0]);
    }
}
