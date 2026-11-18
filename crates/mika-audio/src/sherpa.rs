//! sherpa-onnx streaming recognizer, loaded from its DLL while the app runs
//! (decision 0048).
//!
//! The libraries and the Russian model are downloaded on the user's request,
//! so the app builds and starts without them. Only the functions the wake
//! phrase needs are bound; the structures follow the C API of sherpa-onnx
//! 2.0.1 field by field. The name and the stop words are hot words: the
//! decoder leans towards them, which makes them heard in noise.

use std::ffi::{c_char, c_float, CStr, CString};
use std::path::Path;
use std::ptr;
use std::sync::Arc;

use windows::core::{HSTRING, PCSTR};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH,
};

/// Name of the library inside its folder; `onnxruntime.dll` sits next to it.
pub const LIBRARY: &str = "sherpa-onnx-c-api.dll";

/// Files of the model the recognizer opens.
pub const MODEL_FILES: [&str; 5] = [
    "encoder.int8.onnx",
    "decoder.onnx",
    "joiner.int8.onnx",
    "tokens.txt",
    "bpe.model",
];

/// How strongly the decoder leans towards the hot words. 3 made her hear the
/// name in noise where there was none; 1.5 did not (decision 0048).
const HOTWORDS_SCORE: f32 = 1.5;

#[repr(C)]
#[derive(Clone, Copy)]
struct TransducerConfig {
    encoder: *const c_char,
    decoder: *const c_char,
    joiner: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TwoPaths {
    encoder: *const c_char,
    decoder: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OnePath {
    model: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ModelConfig {
    transducer: TransducerConfig,
    paraformer: TwoPaths,
    zipformer2_ctc: OnePath,
    tokens: *const c_char,
    num_threads: i32,
    provider: *const c_char,
    debug: i32,
    model_type: *const c_char,
    modeling_unit: *const c_char,
    bpe_vocab: *const c_char,
    tokens_buf: *const u8,
    tokens_buf_size: i32,
    nemo_ctc: OnePath,
    t_one_ctc: OnePath,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FeatureConfig {
    sample_rate: i32,
    feature_dim: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CtcFstConfig {
    graph: *const c_char,
    max_active: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct HomophoneConfig {
    dict_dir: *const c_char,
    lexicon: *const c_char,
    rule_fsts: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RecognizerConfig {
    feat_config: FeatureConfig,
    model_config: ModelConfig,
    decoding_method: *const c_char,
    max_active_paths: i32,
    enable_endpoint: i32,
    rule1_min_trailing_silence: c_float,
    rule2_min_trailing_silence: c_float,
    rule3_min_utterance_length: c_float,
    hotwords_file: *const c_char,
    hotwords_score: c_float,
    ctc_fst_decoder_config: CtcFstConfig,
    rule_fsts: *const c_char,
    rule_fars: *const c_char,
    blank_penalty: c_float,
    hotwords_buf: *const u8,
    hotwords_buf_size: i32,
    hr: HomophoneConfig,
}

type Handle = *const std::ffi::c_void;
type CreateRecognizer = unsafe extern "C" fn(*const RecognizerConfig) -> Handle;
type Destroy = unsafe extern "C" fn(Handle);
type CreateStreamWithHotwords = unsafe extern "C" fn(Handle, *const c_char) -> Handle;
type AcceptWaveform = unsafe extern "C" fn(Handle, i32, *const f32, i32);
type RecognizerStream = unsafe extern "C" fn(Handle, Handle) -> i32;
type DecodeStream = unsafe extern "C" fn(Handle, Handle);
type ResultJson = unsafe extern "C" fn(Handle, Handle) -> *const c_char;
type FreeJson = unsafe extern "C" fn(*const c_char);

/// The loaded library and the functions taken from it.
pub struct SherpaLib {
    module: HMODULE,
    create_recognizer: CreateRecognizer,
    destroy_recognizer: Destroy,
    create_stream: CreateStreamWithHotwords,
    destroy_stream: Destroy,
    accept: AcceptWaveform,
    is_ready: RecognizerStream,
    decode: DecodeStream,
    result: ResultJson,
    free_result: FreeJson,
    reset: DecodeStream,
    is_endpoint: RecognizerStream,
}

// SAFETY: the module handle and function pointers do not change after
// loading; recognizers made by the library are used from one thread.
unsafe impl Send for SherpaLib {}
unsafe impl Sync for SherpaLib {}

impl Drop for SherpaLib {
    fn drop(&mut self) {
        // SAFETY: recognizers hold an `Arc` of the library, so nothing made
        // by it is alive when it is unloaded.
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

impl SherpaLib {
    /// Loads the library from `dir`; `onnxruntime.dll` is taken from the
    /// same folder, not from the search path.
    pub fn load(dir: &Path) -> Result<Arc<Self>, String> {
        let path = dir.join(LIBRARY);
        if !path.is_file() {
            return Err(format!("нет файла {}", path.display()));
        }
        // SAFETY: a full path to a file Mika downloaded and checked; the
        // symbols below are declared as in the C API of sherpa-onnx 2.0.1.
        unsafe {
            let module = LoadLibraryExW(
                &HSTRING::from(path.as_os_str()),
                None,
                LOAD_WITH_ALTERED_SEARCH_PATH,
            )
            .map_err(|e| format!("не удалось загрузить {LIBRARY}: {}", e.message()))?;
            let lib = (|| {
                Ok::<_, String>(Self {
                    module,
                    create_recognizer: symbol(module, c"SherpaOnnxCreateOnlineRecognizer")?,
                    destroy_recognizer: symbol(module, c"SherpaOnnxDestroyOnlineRecognizer")?,
                    create_stream: symbol(module, c"SherpaOnnxCreateOnlineStreamWithHotwords")?,
                    destroy_stream: symbol(module, c"SherpaOnnxDestroyOnlineStream")?,
                    accept: symbol(module, c"SherpaOnnxOnlineStreamAcceptWaveform")?,
                    is_ready: symbol(module, c"SherpaOnnxIsOnlineStreamReady")?,
                    decode: symbol(module, c"SherpaOnnxDecodeOnlineStream")?,
                    result: symbol(module, c"SherpaOnnxGetOnlineStreamResultAsJson")?,
                    free_result: symbol(module, c"SherpaOnnxDestroyOnlineStreamResultJson")?,
                    reset: symbol(module, c"SherpaOnnxOnlineStreamReset")?,
                    is_endpoint: symbol(module, c"SherpaOnnxOnlineStreamIsEndpoint")?,
                })
            })();
            if lib.is_err() {
                let _ = FreeLibrary(module);
            }
            lib.map(Arc::new)
        }
    }
}

/// True when every file the recognizer opens is in `dir`.
pub fn model_complete(dir: &Path) -> bool {
    MODEL_FILES.iter().all(|f| dir.join(f).is_file())
}

/// Streaming recognizer of free speech with hot words. Used from one thread.
pub struct Recognizer {
    lib: Arc<SherpaLib>,
    recognizer: Handle,
    stream: Handle,
}

// SAFETY: a recognizer may move between threads; it is never shared.
unsafe impl Send for Recognizer {}

impl Drop for Recognizer {
    fn drop(&mut self) {
        // SAFETY: both made by the library below, freed once, stream first.
        unsafe {
            (self.lib.destroy_stream)(self.stream);
            (self.lib.destroy_recognizer)(self.recognizer);
        }
    }
}

impl Recognizer {
    /// Opens the model in `dir` with `hotwords`, one phrase a line. Writes
    /// `bpe.vocab` next to `bpe.model` the first time: the decoder needs it
    /// to split the hot words into pieces.
    pub fn new(lib: Arc<SherpaLib>, dir: &Path, hotwords: &[String]) -> Result<Self, String> {
        if !model_complete(dir) {
            return Err(format!(
                "в папке модели не хватает файлов: {}",
                dir.display()
            ));
        }
        let vocab = dir.join("bpe.vocab");
        if !vocab.is_file() {
            let model = std::fs::read(dir.join("bpe.model")).map_err(|e| e.to_string())?;
            let text = bpe_vocab(&model).ok_or("не удалось прочитать bpe.model")?;
            std::fs::write(&vocab, text).map_err(|e| e.to_string())?;
        }
        let path = |f: &str| crate::vosk::narrow_path(&dir.join(f));
        let encoder = path("encoder.int8.onnx")?;
        let decoder = path("decoder.onnx")?;
        let joiner = path("joiner.int8.onnx")?;
        let tokens = path("tokens.txt")?;
        let bpe = path("bpe.vocab")?;
        let cpu = c"cpu";
        let bpe_unit = c"bpe";
        let method = c"modified_beam_search";
        let none = ptr::null();
        let config = RecognizerConfig {
            feat_config: FeatureConfig {
                sample_rate: 16_000,
                feature_dim: 80,
            },
            model_config: ModelConfig {
                transducer: TransducerConfig {
                    encoder: encoder.as_ptr(),
                    decoder: decoder.as_ptr(),
                    joiner: joiner.as_ptr(),
                },
                paraformer: TwoPaths {
                    encoder: none,
                    decoder: none,
                },
                zipformer2_ctc: OnePath { model: none },
                tokens: tokens.as_ptr(),
                num_threads: 1,
                provider: cpu.as_ptr(),
                debug: 0,
                model_type: none,
                modeling_unit: bpe_unit.as_ptr(),
                bpe_vocab: bpe.as_ptr(),
                tokens_buf: ptr::null(),
                tokens_buf_size: 0,
                nemo_ctc: OnePath { model: none },
                t_one_ctc: OnePath { model: none },
            },
            decoding_method: method.as_ptr(),
            max_active_paths: 4,
            enable_endpoint: 1,
            rule1_min_trailing_silence: 2.4,
            rule2_min_trailing_silence: 0.8,
            rule3_min_utterance_length: 20.0,
            hotwords_file: none,
            hotwords_score: HOTWORDS_SCORE,
            ctc_fst_decoder_config: CtcFstConfig {
                graph: none,
                max_active: 3000,
            },
            rule_fsts: none,
            rule_fars: none,
            blank_penalty: 0.0,
            hotwords_buf: ptr::null(),
            hotwords_buf_size: 0,
            hr: HomophoneConfig {
                dict_dir: none,
                lexicon: none,
                rule_fsts: none,
            },
        };
        let words = CString::new(hotwords.join("\n")).map_err(|e| e.to_string())?;
        // SAFETY: every pointer in `config` and `words` outlives the calls;
        // null means the model could not be opened.
        unsafe {
            let recognizer = (lib.create_recognizer)(&config);
            if recognizer.is_null() {
                return Err(format!("не удалось открыть модель {}", dir.display()));
            }
            let stream = (lib.create_stream)(recognizer, words.as_ptr());
            if stream.is_null() {
                (lib.destroy_recognizer)(recognizer);
                return Err("распознаватель sherpa-onnx не создан".into());
            }
            Ok(Self {
                lib,
                recognizer,
                stream,
            })
        }
    }

    /// Gives mono samples at `rate` and decodes what is ready. Returns the
    /// words of the phrase so far and whether a pause has just ended it.
    pub fn accept(&mut self, samples: &[f32], rate: u32) -> (String, bool) {
        let lib = &self.lib;
        // SAFETY: valid handles; the slice is live for the call; the answer
        // string is copied before it is freed.
        unsafe {
            (lib.accept)(
                self.stream,
                rate as i32,
                samples.as_ptr(),
                samples.len() as i32,
            );
            while (lib.is_ready)(self.recognizer, self.stream) != 0 {
                (lib.decode)(self.recognizer, self.stream);
            }
            let json = (lib.result)(self.recognizer, self.stream);
            let text = if json.is_null() {
                String::new()
            } else {
                let text = result_text(&CStr::from_ptr(json).to_string_lossy());
                (lib.free_result)(json);
                text
            };
            let ended = (lib.is_endpoint)(self.recognizer, self.stream) != 0;
            (text, ended)
        }
    }

    /// Forgets the phrase so far.
    pub fn reset(&mut self) {
        // SAFETY: valid handles.
        unsafe { (self.lib.reset)(self.recognizer, self.stream) }
    }
}

/// Text of an answer: `{"text": "мика", "tokens": [...], ...}`.
fn result_text(json: &str) -> String {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v["text"].as_str().map(|t| t.trim().to_string()))
        .unwrap_or_default()
}

/// `bpe.vocab` for a sentencepiece `bpe.model`: a line `piece<TAB>score`
/// for every piece. The model is a protobuf `ModelProto` whose field 1 is a
/// repeated `SentencePiece { 1: piece, 2: score }`. None when it is not one.
pub fn bpe_vocab(model: &[u8]) -> Option<String> {
    let mut out = String::new();
    for field in fields(model)? {
        if let (1, Value::Bytes(piece)) = field {
            let mut name = None;
            let mut score = 0.0f32;
            for inner in fields(piece)? {
                match inner {
                    (1, Value::Bytes(b)) => name = Some(std::str::from_utf8(b).ok()?),
                    (2, Value::Fixed32(v)) => score = f32::from_le_bytes(v),
                    _ => {}
                }
            }
            out.push_str(name?);
            out.push('\t');
            out.push_str(&score.to_string());
            out.push('\n');
        }
    }
    (!out.is_empty()).then_some(out)
}

enum Value<'a> {
    Varint,
    Fixed64,
    Bytes(&'a [u8]),
    Fixed32([u8; 4]),
}

fn varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*i)?;
        *i += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return Some(value);
        }
    }
    None
}

/// The top-level fields of a protobuf message, or None when it is broken.
fn fields(b: &[u8]) -> Option<Vec<(u64, Value<'_>)>> {
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let key = varint(b, &mut i)?;
        let value = match key & 7 {
            0 => {
                varint(b, &mut i)?;
                Value::Varint
            }
            1 => {
                b.get(i..i + 8)?;
                i += 8;
                Value::Fixed64
            }
            2 => {
                let size = varint(b, &mut i)? as usize;
                let bytes = b.get(i..i.checked_add(size)?)?;
                i += size;
                Value::Bytes(bytes)
            }
            5 => {
                let v: [u8; 4] = b.get(i..i + 4)?.try_into().ok()?;
                i += 4;
                Value::Fixed32(v)
            }
            _ => return None,
        };
        out.push((key >> 3, value));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A protobuf `SentencePiece` with a name and a score.
    fn piece(name: &str, score: f32) -> Vec<u8> {
        let mut p = vec![0x0a, name.len() as u8];
        p.extend_from_slice(name.as_bytes());
        p.push(0x15);
        p.extend_from_slice(&score.to_le_bytes());
        let mut field = vec![0x0a, p.len() as u8];
        field.extend(p);
        field
    }

    #[test]
    fn the_vocabulary_lists_every_piece_with_its_score() {
        let mut model = piece("<unk>", 0.0);
        model.extend(piece("▁ми", -5.5));
        // Field 2 of ModelProto (trainer spec) is skipped.
        model.extend([0x12, 0x02, 0x08, 0x01]);
        assert_eq!(bpe_vocab(&model).unwrap(), "<unk>\t0\n▁ми\t-5.5\n");
    }

    #[test]
    fn a_broken_model_gives_no_vocabulary() {
        assert!(bpe_vocab(&[0x0a, 0x10, 0x01]).is_none());
        assert!(bpe_vocab(&[]).is_none());
    }

    #[test]
    fn the_answer_text_is_trimmed() {
        assert_eq!(result_text(r#"{"text":" мика ","is_final":false}"#), "мика");
        assert_eq!(result_text("not json"), "");
    }

    /// The real files: `MIKA_SHERPA_DIR` with `lib` and `model` inside.
    #[test]
    #[ignore = "needs the downloaded sherpa-onnx files"]
    fn hears_speech_from_a_file() {
        let root = std::path::PathBuf::from(std::env::var("MIKA_SHERPA_DIR").unwrap());
        let lib = SherpaLib::load(&root.join("lib")).unwrap();
        let mut r = Recognizer::new(lib, &root.join("model"), &["мика".into()]).unwrap();
        let (text, _) = r.accept(&vec![0.0; 16_000], 16_000);
        assert_eq!(text, "");
    }
}
