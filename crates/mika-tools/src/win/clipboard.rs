use crate::sys::Clipboard;

pub struct WinClipboard;

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
}
