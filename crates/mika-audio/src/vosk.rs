//! Vosk speech recognizer, loaded from its DLL while the app runs.
//!
//! The DLL and the model are downloaded on the user's request, so the app
//! builds and starts without them. Only the few functions the wake phrase
//! needs are bound; the declarations follow `vosk_api.h` of vosk-api 0.3.45.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::Path;
use std::sync::Arc;

use windows::core::{HSTRING, PCSTR};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH,
};

/// Name of the library inside its folder.
pub const LIBRARY: &str = "libvosk.dll";

type ModelNew = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type ModelFree = unsafe extern "C" fn(*mut c_void);
type FindWord = unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int;
type RecognizerNew = unsafe extern "C" fn(*mut c_void, f32) -> *mut c_void;
type SetWords = unsafe extern "C" fn(*mut c_void, c_int);
type RecognizerFree = unsafe extern "C" fn(*mut c_void);
type AcceptShorts = unsafe extern "C" fn(*mut c_void, *const i16, c_int) -> c_int;
type ResultText = unsafe extern "C" fn(*mut c_void) -> *const c_char;
type Reset = unsafe extern "C" fn(*mut c_void);
type SetLogLevel = unsafe extern "C" fn(c_int);

/// The loaded library and the functions taken from it.
pub struct VoskLib {
    module: HMODULE,
    model_new: ModelNew,
    model_free: ModelFree,
    find_word: FindWord,
    recognizer_new: RecognizerNew,
    set_words: SetWords,
    recognizer_free: RecognizerFree,
    accept: AcceptShorts,
    result: ResultText,
    partial: ResultText,
    reset: Reset,
}

// SAFETY: the module handle and function pointers do not change after loading,
// and the Vosk functions themselves may be called from any thread.
unsafe impl Send for VoskLib {}
unsafe impl Sync for VoskLib {}

impl Drop for VoskLib {
    fn drop(&mut self) {
        // SAFETY: models and recognizers hold an `Arc` of the library, so
        // nothing made by it is alive when it is unloaded.
        unsafe {
            let _ = FreeLibrary(self.module);
        }
    }
}

/// Takes one function from the library or names the one that is missing.
///
/// # Safety
/// `T` must be the function pointer type that matches the exported symbol.
unsafe fn symbol<T: Copy>(module: HMODULE, name: &CStr) -> Result<T, String> {
    let f = GetProcAddress(module, PCSTR(name.as_ptr().cast()))
        .ok_or_else(|| format!("в {LIBRARY} нет функции {}", name.to_string_lossy()))?;
    debug_assert_eq!(size_of::<T>(), size_of_val(&f));
    Ok(std::mem::transmute_copy(&f))
}

impl VoskLib {
    /// Loads `libvosk.dll` from `dir`. Its own DLLs (libstdc++ and others) are
    /// taken from the same folder, not from the search path.
    pub fn load(dir: &Path) -> Result<Arc<Self>, String> {
        let path = dir.join(LIBRARY);
        if !path.is_file() {
            return Err(format!("нет файла {}", path.display()));
        }
        // SAFETY: a full path to a file Mika downloaded and checked; the
        // symbols below are declared as in vosk_api.h.
        unsafe {
            let module = LoadLibraryExW(
                &HSTRING::from(path.as_os_str()),
                None,
                LOAD_WITH_ALTERED_SEARCH_PATH,
            )
            .map_err(|e| format!("не удалось загрузить {LIBRARY}: {}", e.message()))?;
            let lib = (|| {
                let quiet: SetLogLevel = symbol(module, c"vosk_set_log_level")?;
                // Kaldi writes every step to stderr otherwise.
                quiet(-1);
                Ok::<_, String>(Self {
                    module,
                    model_new: symbol(module, c"vosk_model_new")?,
                    model_free: symbol(module, c"vosk_model_free")?,
                    find_word: symbol(module, c"vosk_model_find_word")?,
                    recognizer_new: symbol(module, c"vosk_recognizer_new")?,
                    set_words: symbol(module, c"vosk_recognizer_set_words")?,
                    recognizer_free: symbol(module, c"vosk_recognizer_free")?,
                    accept: symbol(module, c"vosk_recognizer_accept_waveform_s")?,
                    result: symbol(module, c"vosk_recognizer_result")?,
                    partial: symbol(module, c"vosk_recognizer_partial_result")?,
                    reset: symbol(module, c"vosk_recognizer_reset")?,
                })
            })();
            if lib.is_err() {
                let _ = FreeLibrary(module);
            }
            lib.map(Arc::new)
        }
    }
}

/// Path as the narrow string Kaldi opens files with: the short 8.3 form when
/// the path has letters outside ASCII, as in a Cyrillic user name.
fn narrow_path(path: &Path) -> Result<CString, String> {
    let text = path.to_string_lossy().into_owned();
    let text = if text.is_ascii() {
        text
    } else {
        use windows::Win32::Storage::FileSystem::GetShortPathNameW;
        let wide = HSTRING::from(path.as_os_str());
        let mut buf = vec![0u16; 1024];
        // SAFETY: the buffer is large enough for any path Windows accepts here.
        let n = unsafe { GetShortPathNameW(&wide, Some(&mut buf)) } as usize;
        let short = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
        if n == 0 || !short.is_ascii() {
            return Err(format!(
                "путь к модели должен быть латиницей: {}",
                path.display()
            ));
        }
        short
    };
    CString::new(text).map_err(|e| e.to_string())
}

/// An acoustic model with its vocabulary. Shared by recognizers.
pub struct Model {
    lib: Arc<VoskLib>,
    ptr: *mut c_void,
}

// SAFETY: Vosk models are read-only after loading and safe to share.
unsafe impl Send for Model {}
unsafe impl Sync for Model {}

impl Drop for Model {
    fn drop(&mut self) {
        // SAFETY: made by `model_new`, freed once.
        unsafe { (self.lib.model_free)(self.ptr) }
    }
}

impl Model {
    /// Loads the model folder: seconds, and about 100 MB of memory.
    pub fn load(lib: Arc<VoskLib>, dir: &Path) -> Result<Arc<Self>, String> {
        let path = narrow_path(dir)?;
        // SAFETY: a valid C string; null means the folder is not a model.
        let ptr = unsafe { (lib.model_new)(path.as_ptr()) };
        if ptr.is_null() {
            return Err(format!("не удалось открыть модель {}", dir.display()));
        }
        Ok(Arc::new(Self { lib, ptr }))
    }

    /// True when the word is in the model's vocabulary: the recognizer can
    /// only hear words it knows.
    pub fn knows(&self, word: &str) -> bool {
        let Ok(w) = CString::new(word) else {
            return false;
        };
        // SAFETY: valid model and C string.
        unsafe { (self.lib.find_word)(self.ptr, w.as_ptr()) >= 0 }
    }
}

/// Recognizer of free speech. Used from one thread.
pub struct Recognizer {
    model: Arc<Model>,
    ptr: *mut c_void,
}

// SAFETY: a recognizer may move between threads; it is never shared.
unsafe impl Send for Recognizer {}

impl Drop for Recognizer {
    fn drop(&mut self) {
        // SAFETY: made by `recognizer_new_grm`, freed once.
        unsafe { (self.model.lib.recognizer_free)(self.ptr) }
    }
}

/// Text of a Vosk answer: `{"partial" : "мика"}` or `{"text" : "мика"}`.
fn answer_text(json: &str, field: &str) -> String {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v[field].as_str().map(str::to_string))
        .unwrap_or_default()
}

/// A word of a final answer and how sure the recognizer is of it, 0..=1.
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub word: String,
    pub conf: f32,
}

/// Words of a final answer: `{"result":[{"conf":0.97,"word":"мика",…}],"text":"мика"}`.
fn answer_words(json: &str) -> Vec<Word> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    v["result"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|w| {
                    Some(Word {
                        word: w["word"].as_str()?.to_string(),
                        conf: w["conf"].as_f64()? as f32,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What the recognizer heard in the sound given so far.
#[derive(Debug, Clone, PartialEq)]
pub enum Heard {
    /// The phrase is still going: the words so far.
    Partial(String),
    /// A pause ended the phrase: its text and words.
    Final { text: String, words: Vec<Word> },
}

impl Recognizer {
    /// Hears any words of the model's vocabulary. Limiting it to a list of
    /// phrases was tried and dropped: with a list it bends «мама», «микрофон»
    /// and «мир» into the name with full confidence.
    pub fn new(model: Arc<Model>, rate: f32) -> Result<Self, String> {
        // SAFETY: valid model.
        let ptr = unsafe { (model.lib.recognizer_new)(model.ptr, rate) };
        if ptr.is_null() {
            return Err("распознаватель Vosk не создан".into());
        }
        // SAFETY: valid recognizer.
        unsafe { (model.lib.set_words)(ptr, 1) };
        Ok(Self { model, ptr })
    }

    /// Gives 16-bit samples at the rate given to `new`.
    pub fn accept(&mut self, samples: &[i16]) -> Heard {
        let lib = &self.model.lib;
        // SAFETY: the pointer and length describe a live slice; the answer
        // strings belong to the recognizer and are copied before the next call.
        unsafe {
            let ended = (lib.accept)(self.ptr, samples.as_ptr(), samples.len() as c_int);
            if ended == 1 {
                let json = CStr::from_ptr((lib.result)(self.ptr)).to_string_lossy();
                Heard::Final {
                    text: answer_text(&json, "text"),
                    words: answer_words(&json),
                }
            } else {
                let text = CStr::from_ptr((lib.partial)(self.ptr)).to_string_lossy();
                Heard::Partial(answer_text(&text, "partial"))
            }
        }
    }

    /// Forgets the sound given so far.
    pub fn reset(&mut self) {
        // SAFETY: valid recognizer.
        unsafe { (self.model.lib.reset)(self.ptr) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vosk_answers_are_read() {
        assert_eq!(answer_text(r#"{"partial" : "мика"}"#, "partial"), "мика");
        assert_eq!(
            answer_text("{\n  \"text\" : \"эй мика\"\n}", "text"),
            "эй мика"
        );
        assert_eq!(answer_text("{}", "text"), "");
        assert_eq!(answer_text("not json", "text"), "");
        let json = r#"{"result":[{"conf":0.5,"end":1.2,"start":0.9,"word":"эй"},{"conf":1.0,"end":1.5,"start":1.2,"word":"мика"}],"text":"эй мика"}"#;
        assert_eq!(
            answer_words(json),
            vec![
                Word {
                    word: "эй".into(),
                    conf: 0.5
                },
                Word {
                    word: "мика".into(),
                    conf: 1.0
                }
            ]
        );
        assert!(answer_words("{}").is_empty());
    }

    #[test]
    fn a_missing_library_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let e = VoskLib::load(dir.path()).err().unwrap();
        assert!(e.contains(LIBRARY), "{e}");
    }
}
