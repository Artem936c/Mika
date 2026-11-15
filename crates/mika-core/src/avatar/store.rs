//! The folder the models live in: one folder per model, named by the start
//! of the SHA-256 of its file.
//!
//! ```text
//! models/<id>/model.vrm
//! models/<id>/profile.json
//! models/<id>/portraits/<name>.png
//! ```
//!
//! An id comes from the page, so every method checks its shape before the
//! disk is touched: sixteen lower-case hex digits and nothing else.

use std::path::{Path, PathBuf};

use super::glb::read_json;
use super::profile::Profile;
use super::vrm::{inspect, ModelInfo};
use super::ModelError;
use crate::emotion::Emotion;

/// The biggest picture of a model taken from the page.
pub const MAX_STILL_BYTES: usize = 1024 * 1024;

/// One model in the folder.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelEntry {
    pub id: String,
    pub profile: Profile,
    pub size_bytes: u64,
    /// Every picture of `still_names()` is on the disk.
    pub has_stills: bool,
}

/// Names of the pictures of a model: each emotion with the mouth shut and
/// open (`joy`, `joy-talk`), then the whole figure (`full`).
pub fn still_names() -> Vec<String> {
    let mut names = Vec::with_capacity(Emotion::ALL.len() * 2 + 1);
    for emotion in Emotion::ALL {
        names.push(emotion.as_str().to_string());
        names.push(format!("{}-talk", emotion.as_str()));
    }
    names.push("full".to_string());
    names
}

const MODEL_FILE: &str = "model.vrm";
const PROFILE_FILE: &str = "profile.json";
const PORTRAITS: &str = "portraits";
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// Hex digits of an id: the first eight bytes of the SHA-256 of the file.
const ID_LEN: usize = 16;

/// Sixteen lower-case hex digits: the only text that may become a folder name.
pub fn is_model_id(id: &str) -> bool {
    id.len() == ID_LEN && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Reads the description of a model file and checks it.
fn inspect_file(path: &Path) -> Result<ModelInfo, ModelError> {
    let file = std::fs::File::open(path)?;
    inspect(&read_json(&mut std::io::BufReader::new(file))?)
}

pub struct Store {
    dir: PathBuf,
    max_bytes: u64,
}

impl Store {
    pub fn new(dir: PathBuf) -> Store {
        Store::with_limit(dir, super::MAX_MODEL_BYTES)
    }

    pub(crate) fn with_limit(dir: PathBuf, max_bytes: u64) -> Store {
        Store { dir, max_bytes }
    }

    /// The folder of a model, when the id has the right shape and it is there.
    fn folder(&self, id: &str) -> Result<PathBuf, ModelError> {
        if !is_model_id(id) {
            return Err(ModelError::Unknown);
        }
        let folder = self.dir.join(id);
        if folder.is_dir() {
            Ok(folder)
        } else {
            Err(ModelError::Unknown)
        }
    }

    /// Checks the file and copies it in. The same file twice is one model.
    pub fn import(&self, file: &Path) -> Result<ModelEntry, ModelError> {
        if std::fs::metadata(file)?.len() > self.max_bytes {
            return Err(ModelError::TooBig);
        }
        // A file that is not a model is refused before anything is written.
        inspect_file(file)?;
        std::fs::create_dir_all(&self.dir)?;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let incoming = self.dir.join(format!(
            ".incoming-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let taken = self.take_in(file, &incoming);
        if taken.is_err() {
            let _ = std::fs::remove_file(&incoming);
        }
        taken
    }

    /// Copies the file to `incoming` while hashing it, then gives it its folder.
    fn take_in(&self, file: &Path, incoming: &Path) -> Result<ModelEntry, ModelError> {
        use sha2::{Digest, Sha256};
        use std::io::{Read, Write};

        let mut source = std::fs::File::open(file)?;
        let mut copy = std::fs::File::create(incoming)?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        let mut total: u64 = 0;
        loop {
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            total += read as u64;
            // The file may have grown since its size was asked.
            if total > self.max_bytes {
                return Err(ModelError::TooBig);
            }
            hash.update(&buffer[..read]);
            copy.write_all(&buffer[..read])?;
        }
        copy.sync_all()?;
        drop(copy);
        let id: String = hash.finalize()[..ID_LEN / 2]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();

        let folder = self.dir.join(&id);
        if folder.join(MODEL_FILE).is_file() {
            std::fs::remove_file(incoming)?;
            return self.entry(&id);
        }
        // What was copied is what is checked: the source may have changed meanwhile.
        let info = inspect_file(incoming)?;
        std::fs::create_dir_all(&folder)?;
        let placed = std::fs::rename(incoming, folder.join(MODEL_FILE))
            .map_err(ModelError::from)
            .and_then(|()| Profile::from_info(&info).write(&folder.join(PROFILE_FILE)));
        if let Err(e) = placed {
            let _ = std::fs::remove_dir_all(&folder);
            return Err(e);
        }
        self.entry(&id)
    }

    /// Sorted by title, then id. A folder that cannot be read is passed over.
    pub fn list(&self) -> Vec<ModelEntry> {
        let mut models: Vec<ModelEntry> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|item| item.file_name().into_string().ok())
            .filter_map(|name| self.entry(&name).ok())
            .collect();
        models.sort_by(|a, b| {
            (a.profile.title.to_lowercase(), &a.id).cmp(&(b.profile.title.to_lowercase(), &b.id))
        });
        models
    }

    pub fn entry(&self, id: &str) -> Result<ModelEntry, ModelError> {
        let folder = self.folder(id)?;
        let model = folder.join(MODEL_FILE);
        let size_bytes = match std::fs::metadata(&model) {
            Ok(meta) if meta.is_file() => meta.len(),
            _ => return Err(ModelError::Unknown),
        };
        let profile_path = folder.join(PROFILE_FILE);
        let profile = match Profile::read(&profile_path) {
            Ok(profile) => profile,
            // A lost or broken profile is made again; the manual choices are gone.
            Err(_) => {
                let profile = Profile::from_info(&inspect_file(&model)?);
                profile.write(&profile_path)?;
                profile
            }
        };
        let portraits = folder.join(PORTRAITS);
        let has_stills = still_names()
            .iter()
            .all(|name| portraits.join(format!("{name}.png")).is_file());
        Ok(ModelEntry {
            id: id.to_string(),
            profile,
            size_bytes,
            has_stills,
        })
    }

    /// The model file, checked again first: it may have been swapped.
    pub fn bytes(&self, id: &str) -> Result<Vec<u8>, ModelError> {
        let model = self.folder(id)?.join(MODEL_FILE);
        match std::fs::metadata(&model) {
            Ok(meta) if meta.is_file() && meta.len() <= self.max_bytes => {}
            Ok(meta) if meta.is_file() => return Err(ModelError::TooBig),
            _ => return Err(ModelError::Unknown),
        }
        inspect_file(&model)?;
        Ok(std::fs::read(&model)?)
    }

    pub fn set_slot(
        &self,
        id: &str,
        emotion: Emotion,
        expression: Option<&str>,
    ) -> Result<Profile, ModelError> {
        let mut profile = self.entry(id)?.profile;
        profile.set_slot(emotion, expression)?;
        profile.write(&self.folder(id)?.join(PROFILE_FILE))?;
        Ok(profile)
    }

    pub fn set_lip(&self, id: &str, lip: f32) -> Result<Profile, ModelError> {
        let mut profile = self.entry(id)?.profile;
        profile.set_lip(lip);
        profile.write(&self.folder(id)?.join(PROFILE_FILE))?;
        Ok(profile)
    }

    /// All or nothing: one bad picture and none is written.
    pub fn save_stills(&self, id: &str, stills: &[(String, Vec<u8>)]) -> Result<(), ModelError> {
        let portraits = self.folder(id)?.join(PORTRAITS);
        let names = still_names();
        let good = |(name, bytes): &(String, Vec<u8>)| {
            names.contains(name)
                && bytes.len() <= MAX_STILL_BYTES
                && bytes.starts_with(&PNG_SIGNATURE)
        };
        if !stills.iter().all(good) {
            return Err(ModelError::BadStill);
        }
        std::fs::create_dir_all(&portraits)?;
        for (name, bytes) in stills {
            std::fs::write(portraits.join(format!("{name}.png")), bytes)?;
        }
        Ok(())
    }

    /// The pictures on the disk, in the order of `still_names()`.
    pub fn stills(&self, id: &str) -> Vec<(String, Vec<u8>)> {
        let Ok(folder) = self.folder(id) else {
            return Vec::new();
        };
        let portraits = folder.join(PORTRAITS);
        still_names()
            .into_iter()
            .filter_map(|name| {
                let bytes = std::fs::read(portraits.join(format!("{name}.png"))).ok()?;
                (bytes.len() <= MAX_STILL_BYTES).then_some((name, bytes))
            })
            .collect()
    }

    pub fn delete(&self, id: &str) -> Result<(), ModelError> {
        std::fs::remove_dir_all(self.folder(id)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::{glb, vrm1};
    use super::*;
    use crate::avatar::Source;

    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    fn png(len: usize) -> Vec<u8> {
        let mut bytes = PNG.to_vec();
        bytes.resize(len.max(PNG.len()), 0);
        bytes
    }

    /// A folder for the models and a folder with a model file to bring in.
    fn setup() -> (tempfile::TempDir, Store, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("models"));
        let file = dir.path().join("from").join("her.vrm");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            glb(&vrm1(&["happy", "relaxed"], &["Blush"]).to_string()),
        )
        .unwrap();
        (dir, store, file)
    }

    fn all_stills() -> Vec<(String, Vec<u8>)> {
        still_names().into_iter().map(|n| (n, png(64))).collect()
    }

    #[test]
    fn there_are_twenty_one_pictures() {
        let names = still_names();
        assert_eq!(names.len(), 21);
        assert_eq!(&names[..4], ["neutral", "neutral-talk", "joy", "joy-talk"]);
        assert_eq!(names[20], "full");
    }

    #[test]
    fn an_imported_model_is_listed_with_its_profile() {
        let (_dir, store, file) = setup();
        assert!(store.list().is_empty());
        let entry = store.import(&file).unwrap();
        assert_eq!(entry.id.len(), 16);
        assert!(entry.id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')));
        assert_eq!(entry.profile.title, "Test");
        assert_eq!(entry.size_bytes, std::fs::metadata(&file).unwrap().len());
        assert!(!entry.has_stills);
        assert_eq!(store.list(), std::slice::from_ref(&entry));
        assert_eq!(store.entry(&entry.id).unwrap(), entry);
        assert_eq!(
            store.bytes(&entry.id).unwrap(),
            std::fs::read(&file).unwrap()
        );
        // The file the user chose is left where it was.
        assert!(file.exists());
    }

    #[test]
    fn the_same_file_twice_is_one_model() {
        let (_dir, store, file) = setup();
        let first = store.import(&file).unwrap();
        store
            .set_slot(&first.id, Emotion::Shy, Some("happy"))
            .unwrap();
        let second = store.import(&file).unwrap();
        assert_eq!(second.id, first.id);
        assert_eq!(second.profile.slots["shy"].source, Source::Manual);
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn a_bad_file_leaves_no_folder() {
        let (dir, store, _file) = setup();
        let plain = dir.path().join("from").join("plain.glb");
        std::fs::write(&plain, glb(r#"{"asset":{"version":"2.0"}}"#)).unwrap();
        assert!(matches!(store.import(&plain), Err(ModelError::NotVrm)));
        let text = dir.path().join("from").join("notes.vrm");
        std::fs::write(&text, "hello").unwrap();
        assert!(matches!(store.import(&text), Err(ModelError::NotGlb)));
        assert!(matches!(
            store.import(&dir.path().join("from").join("absent.vrm")),
            Err(ModelError::Io(_))
        ));
        let left = std::fs::read_dir(dir.path().join("models"))
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(left, 0);
    }

    #[test]
    fn a_file_over_the_limit_is_refused_by_its_size() {
        let (dir, _store, file) = setup();
        let small = Store::with_limit(dir.path().join("models"), 64);
        assert!(std::fs::metadata(&file).unwrap().len() > 64);
        assert!(matches!(small.import(&file), Err(ModelError::TooBig)));
        assert!(small.list().is_empty());
    }

    #[test]
    fn an_id_that_is_not_16_hex_chars_is_unknown() {
        let (dir, store, file) = setup();
        let real = store.import(&file).unwrap().id;
        // Something a bad id could reach if it were joined to the folder.
        let outside = dir.path().join("from");
        for id in [
            "../from",
            "..",
            "",
            "0123",
            "ABCDEF0123456789",
            "0123456789abcdef/..",
            "0123456789abcdeg",
            &format!("{real}\\..\\..\\from"),
            &format!("{real} "),
        ] {
            assert!(
                matches!(store.bytes(id), Err(ModelError::Unknown)),
                "{id:?}"
            );
            assert!(
                matches!(store.entry(id), Err(ModelError::Unknown)),
                "{id:?}"
            );
            assert!(
                matches!(store.delete(id), Err(ModelError::Unknown)),
                "{id:?}"
            );
            assert!(matches!(
                store.set_slot(id, Emotion::Joy, None),
                Err(ModelError::Unknown)
            ));
            assert!(matches!(store.set_lip(id, 1.0), Err(ModelError::Unknown)));
            assert!(matches!(
                store.save_stills(id, &all_stills()),
                Err(ModelError::Unknown)
            ));
            assert!(store.stills(id).is_empty());
        }
        assert!(outside.join("her.vrm").exists());
        // A well-formed id of a model that is not there.
        assert!(matches!(
            store.bytes("0123456789abcdef"),
            Err(ModelError::Unknown)
        ));
    }

    #[test]
    fn a_file_swapped_after_import_is_checked_again() {
        let (dir, store, file) = setup();
        let id = store.import(&file).unwrap().id;
        let mut bad = vrm1(&["happy"], &[]);
        bad["images"] = serde_json::json!([{ "uri": "https://x.example/t.png" }]);
        let inside = dir.path().join("models").join(&id).join("model.vrm");
        std::fs::write(&inside, glb(&bad.to_string())).unwrap();
        assert!(matches!(store.bytes(&id), Err(ModelError::External)));
    }

    #[test]
    fn stills_are_saved_only_when_they_are_png_with_known_names() {
        let (dir, store, file) = setup();
        let id = store.import(&file).unwrap().id;
        let portraits = dir.path().join("models").join(&id).join("portraits");

        let bad: [(&str, Vec<u8>); 5] = [
            ("evil", png(64)),
            ("../x", png(64)),
            ("joy", b"GIF89a not a png at all".to_vec()),
            ("joy", png(MAX_STILL_BYTES + 1)),
            ("joy", Vec::new()),
        ];
        for (name, bytes) in bad {
            let batch = [("neutral".to_string(), png(64)), (name.to_string(), bytes)];
            assert!(
                matches!(store.save_stills(&id, &batch), Err(ModelError::BadStill)),
                "{name}"
            );
            // The good one of the same batch is not written either.
            assert!(!portraits.join("neutral.png").exists(), "{name}");
        }
        assert!(store.stills(&id).is_empty());

        store
            .save_stills(
                &id,
                &[
                    ("neutral".into(), png(64)),
                    ("full".into(), png(MAX_STILL_BYTES)),
                ],
            )
            .unwrap();
        let saved = store.stills(&id);
        assert_eq!(saved.len(), 2);
        assert_eq!(
            (saved[0].0.as_str(), saved[1].0.as_str()),
            ("neutral", "full")
        );
        assert_eq!(saved[0].1, png(64));
    }

    #[test]
    fn has_stills_needs_all_twenty_one() {
        let (_dir, store, file) = setup();
        let id = store.import(&file).unwrap().id;
        let mut stills = all_stills();
        let last = stills.pop().unwrap();
        store.save_stills(&id, &stills).unwrap();
        assert!(!store.entry(&id).unwrap().has_stills);
        store.save_stills(&id, &[last]).unwrap();
        assert!(store.entry(&id).unwrap().has_stills);
    }

    #[test]
    fn slots_and_lips_are_kept_on_the_disk() {
        let (_dir, store, file) = setup();
        let id = store.import(&file).unwrap().id;
        let profile = store.set_slot(&id, Emotion::Worry, Some("Blush")).unwrap();
        assert_eq!(profile.slots["worry"].source, Source::Manual);
        assert_eq!(store.set_lip(&id, 0.4).unwrap().lip, 0.4);
        let read = store.entry(&id).unwrap().profile;
        assert_eq!(read.slots["worry"].mix, [("Blush".to_string(), 1.0)]);
        assert_eq!(read.lip, 0.4);
        assert!(matches!(
            store.set_slot(&id, Emotion::Joy, Some("nope")),
            Err(ModelError::Unknown)
        ));
    }

    #[test]
    fn a_lost_profile_is_made_again_from_the_model() {
        let (dir, store, file) = setup();
        let id = store.import(&file).unwrap().id;
        let profile = dir.path().join("models").join(&id).join("profile.json");
        std::fs::write(&profile, "{ broken").unwrap();
        assert_eq!(store.entry(&id).unwrap().profile.title, "Test");
        std::fs::remove_file(&profile).unwrap();
        assert_eq!(store.list().len(), 1);
        assert!(profile.exists());
    }

    #[test]
    fn models_are_listed_by_title() {
        let (dir, store, file) = setup();
        store.import(&file).unwrap();
        let mut other = vrm1(&["sad"], &[]);
        other["extensions"]["VRMC_vrm"]["meta"]["name"] = serde_json::json!("Anna");
        let second = dir.path().join("from").join("anna.vrm");
        std::fs::write(&second, glb(&other.to_string())).unwrap();
        store.import(&second).unwrap();
        // A stray folder and a stray file are not models.
        std::fs::create_dir_all(dir.path().join("models").join("notes")).unwrap();
        std::fs::write(dir.path().join("models").join("readme.txt"), "x").unwrap();
        let titles: Vec<String> = store.list().into_iter().map(|m| m.profile.title).collect();
        assert_eq!(titles, ["Anna", "Test"]);
    }

    #[test]
    fn delete_removes_the_folder_and_nothing_else() {
        let (dir, store, file) = setup();
        let id = store.import(&file).unwrap().id;
        let models = dir.path().join("models");
        std::fs::create_dir_all(models.join("fedcba9876543210")).unwrap();
        std::fs::write(models.join("readme.txt"), "x").unwrap();
        store.delete(&id).unwrap();
        assert!(!models.join(&id).exists());
        assert!(models.join("fedcba9876543210").exists());
        assert!(models.join("readme.txt").exists());
        assert!(matches!(store.delete(&id), Err(ModelError::Unknown)));
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// The figure of boxes the preview uses, when it has been made
    /// (`node frontend/src/dev/make-sample-vrm.mjs`): a file written by
    /// another program goes through the same door as a model of the user.
    #[test]
    #[ignore = "needs frontend/src/dev/sample.vrm"]
    fn the_sample_model_of_the_preview_is_taken_in() {
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../frontend/src/dev/sample.vrm");
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("models"));
        let entry = store.import(&file).unwrap();
        let profile = &entry.profile;
        // The script makes either version of the format; the older one has
        // no standard surprise and brings its own.
        let surprise = match profile.spec {
            crate::avatar::Spec::V1 => "surprised",
            crate::avatar::Spec::V0 => "Surprised",
        };
        assert_eq!(profile.slots["surprise"].mix, [(surprise.to_string(), 1.0)]);
        assert!(profile.expressions.iter().any(|e| e == "happy"));
        assert!(profile.expressions.iter().any(|e| e == "Blush"));
        assert_eq!(profile.slots["joy"].mix, [("happy".to_string(), 1.0)]);
        assert_eq!(profile.slots["shy"].mix, [("Blush".to_string(), 1.0)]);
        assert_eq!(
            store.bytes(&entry.id).unwrap().len() as u64,
            entry.size_bytes
        );
    }
}
