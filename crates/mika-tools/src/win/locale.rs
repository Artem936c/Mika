//! The display language of Windows.

/// Language id of the Windows display language of this user, such as 0x0419 for Russian.
pub fn ui_language() -> u16 {
    // SAFETY: no arguments and no pointers.
    unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() }
}

#[cfg(test)]
mod tests {
    #[test]
    fn windows_reports_a_display_language() {
        // Whatever it is, the primary language (the low ten bits) is known.
        assert_ne!(super::ui_language() & 0x3ff, 0);
    }
}
