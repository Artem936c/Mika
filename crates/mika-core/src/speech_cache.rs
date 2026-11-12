//! Sound of phrases a paid synthesizer has already made.
//!
//! The same phrase in the same voice is never ordered twice: greetings,
//! «Готово» and the answers to confirmations repeat often. Files live in a
//! folder of their own; when the folder grows past its limit, the phrases
//! heard longest ago go first.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

/// Room the cache may take on disk.
pub const SPEECH_CACHE_LIMIT: u64 = 100 * 1024 * 1024;

/// Longest phrase worth keeping: long sentences rarely repeat word for word.
const MAX_CACHED_CHARS: usize = 300;

const EXT: &str = "snd";

pub struct SpeechCache {
    dir: PathBuf,
    limit: u64,
}

/// What makes one sound different from another: the address, model and voice
/// that made it, and the phrase itself.
pub struct SoundKey<'a> {
    pub base_url: &'a str,
    pub model: &'a str,
    pub voice: &'a str,
    pub text: &'a str,
}

impl SoundKey<'_> {
    /// Phrase with spaces collapsed, so a stray space does not cost a new order.
    fn phrase(&self) -> String {
        self.text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// File name: a hash, so the folder never shows what was said.
    fn file_name(&self) -> Option<String> {
        let phrase = self.phrase();
        if phrase.is_empty() || phrase.chars().count() > MAX_CACHED_CHARS {
            return None;
        }
        let mut h = Sha256::new();
        for part in [
            self.base_url.trim().trim_end_matches('/'),
            self.model.trim(),
            self.voice.trim(),
            phrase.as_str(),
        ] {
            h.update(part.as_bytes());
            h.update([0]);
        }
        let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        Some(format!("{hex}.{EXT}"))
    }
}

impl SpeechCache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self::with_limit(dir, SPEECH_CACHE_LIMIT)
    }

    pub fn with_limit(dir: impl Into<PathBuf>, limit: u64) -> Self {
        Self {
            dir: dir.into(),
            limit,
        }
    }

    /// Sound made earlier for this phrase. A hit counts as a fresh use.
    pub fn get(&self, key: &SoundKey) -> Option<Vec<u8>> {
        let path = self.dir.join(key.file_name()?);
        let bytes = fs::read(&path).ok().filter(|b| !b.is_empty())?;
        if let Ok(f) = fs::File::options().write(true).open(&path) {
            let _ = f.set_modified(SystemTime::now());
        }
        Some(bytes)
    }

    /// Keeps the sound and makes room when the folder is over its limit.
    /// A failure only costs a future order, so it is logged and forgotten.
    pub fn put(&self, key: &SoundKey, sound: &[u8]) {
        let Some(name) = key.file_name() else {
            return;
        };
        if sound.is_empty() {
            return;
        }
        if let Err(e) = self.write(&name, sound) {
            log::warn!("speech cache: cannot keep a phrase: {e}");
            return;
        }
        self.trim();
    }

    fn write(&self, name: &str, sound: &[u8]) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        // Written aside and renamed, so a reader never sees half a file.
        let tmp = self.dir.join(format!(
            "{name}.{}.{:?}.part",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::write(&tmp, sound)?;
        fs::rename(&tmp, self.dir.join(name)).inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })
    }

    /// Sound files with their size and last use.
    fn entries(&self) -> Vec<(PathBuf, u64, SystemTime)> {
        let Ok(dir) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        dir.filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == EXT))
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                Some((e.path(), m.len(), m.modified().ok()?))
            })
            .collect()
    }

    /// Drops the phrases heard longest ago until the folder fits its limit.
    fn trim(&self) {
        let mut entries = self.entries();
        let mut total: u64 = entries.iter().map(|e| e.1).sum();
        if total <= self.limit {
            return;
        }
        entries.sort_by_key(|e| e.2);
        for (path, len, _) in entries {
            if total <= self.limit {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(len);
            }
        }
    }

    /// Bytes the cache takes on disk.
    pub fn size(&self) -> u64 {
        self.entries().iter().map(|e| e.1).sum()
    }

    /// Forgets every kept phrase.
    pub fn clear(&self) {
        for (path, _, _) in self.entries() {
            let _ = fs::remove_file(path);
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key<'a>(voice: &'a str, text: &'a str) -> SoundKey<'a> {
        SoundKey {
            base_url: "https://routerai.ru/api/v1/",
            model: "tts",
            voice,
            text,
        }
    }

    #[test]
    fn a_phrase_is_ordered_once_per_voice() {
        let dir = tempfile::tempdir().unwrap();
        let cache = SpeechCache::new(dir.path());
        assert_eq!(cache.get(&key("Kore", "Готово.")), None);
        cache.put(&key("Kore", "Готово."), b"RIFF1");
        assert_eq!(
            cache.get(&key("Kore", "  Готово. ")),
            Some(b"RIFF1".to_vec())
        );
        // Another voice or another phrase is a different sound.
        assert_eq!(cache.get(&key("Puck", "Готово.")), None);
        assert_eq!(cache.get(&key("Kore", "Готово!")), None);
        // The folder never shows the phrase.
        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1);
        assert!(!names[0].contains("Готово"));
    }

    #[test]
    fn long_and_empty_phrases_are_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let cache = SpeechCache::new(dir.path());
        let long = "слово ".repeat(100);
        cache.put(&key("Kore", &long), b"RIFF");
        cache.put(&key("Kore", "   "), b"RIFF");
        assert_eq!(cache.size(), 0);
    }

    #[test]
    fn the_phrase_heard_longest_ago_goes_first() {
        let dir = tempfile::tempdir().unwrap();
        let cache = SpeechCache::with_limit(dir.path(), 25);
        cache.put(&key("Kore", "раз"), &[1; 10]);
        cache.put(&key("Kore", "два"), &[2; 10]);
        // Make «раз» clearly older, then hear «два» again.
        let old = SystemTime::now() - std::time::Duration::from_secs(60);
        let raz = dir.path().join(key("Kore", "раз").file_name().unwrap());
        fs::File::options()
            .write(true)
            .open(&raz)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(cache.get(&key("Kore", "два")).is_some());
        cache.put(&key("Kore", "три"), &[3; 10]);
        assert_eq!(cache.get(&key("Kore", "раз")), None);
        assert!(cache.get(&key("Kore", "два")).is_some());
        assert!(cache.get(&key("Kore", "три")).is_some());
        assert_eq!(cache.size(), 20);
        cache.clear();
        assert_eq!(cache.size(), 0);
    }
}
