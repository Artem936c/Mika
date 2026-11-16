//! Animations the user brings: `.vrma` files of the VRM Animation format
//! (decision 0040). One folder per animation, named by the start of the
//! SHA-256 of its file, as models are.
//!
//! ```text
//! motions/<id>/motion.vrma
//! motions/<id>/info.json
//! ```
//!
//! The model asks for an animation by its name, so names are unique without
//! regard to case. Like a model, an animation file may use only what is in it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::glb::read_json;
use super::store::is_model_id;
use super::vrm::refers_outside;
use super::ModelError;

/// The biggest animation file taken.
pub const MAX_MOTION_BYTES: u64 = 20 * 1024 * 1024;
/// The longest name of an animation, characters.
const MAX_NAME: usize = 60;

const MOTION_FILE: &str = "motion.vrma";
const INFO_FILE: &str = "info.json";
/// Extensions an animation may require: only what the player knows.
const KNOWN_REQUIRED: [&str; 2] = ["VRMC_vrm_animation", "KHR_texture_transform"];

/// One animation in the folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MotionEntry {
    pub id: String,
    pub name: String,
    /// How long it plays, milliseconds.
    pub ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Info {
    name: String,
    ms: u64,
}

/// Checks the description of an animation file; gives how long it plays, ms.
pub fn inspect_motion(json: &Value) -> Result<u64, ModelError> {
    if json
        .get("extensions")
        .and_then(|e| e.get("VRMC_vrm_animation"))
        .is_none()
    {
        return Err(ModelError::NotVrm);
    }
    if refers_outside(json) {
        return Err(ModelError::External);
    }
    if let Some(name) = json
        .get("extensionsRequired")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find(|name| !KNOWN_REQUIRED.contains(name))
    {
        return Err(ModelError::Unsupported(
            name.chars().take(MAX_NAME).collect(),
        ));
    }
    // The length is the latest moment any channel of any animation reaches.
    let accessors = json.get("accessors").and_then(Value::as_array);
    let mut end = 0.0_f64;
    let mut any = false;
    for animation in json
        .get("animations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for sampler in animation
            .get("samplers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(input) = sampler.get("input").and_then(Value::as_u64) else {
                continue;
            };
            let max = accessors
                .and_then(|a| a.get(input as usize))
                .and_then(|a| a.get("max"))
                .and_then(Value::as_array)
                .and_then(|m| m.first())
                .and_then(Value::as_f64);
            if let Some(max) = max.filter(|m| m.is_finite() && *m >= 0.0) {
                end = end.max(max);
                any = true;
            }
        }
    }
    if !any {
        return Err(ModelError::Broken);
    }
    Ok((end * 1000.0).round() as u64)
}

fn inspect_motion_file(path: &Path) -> Result<u64, ModelError> {
    let file = std::fs::File::open(path)?;
    inspect_motion(&read_json(&mut std::io::BufReader::new(file))?)
}

/// A name as it is kept: trimmed and not too long.
fn clean_name(name: &str) -> String {
    name.trim()
        .chars()
        .take(MAX_NAME)
        .collect::<String>()
        .trim()
        .to_string()
}

pub struct MotionStore {
    dir: PathBuf,
    max_bytes: u64,
}

impl MotionStore {
    pub fn new(dir: PathBuf) -> MotionStore {
        MotionStore::with_limit(dir, MAX_MOTION_BYTES)
    }

    pub(crate) fn with_limit(dir: PathBuf, max_bytes: u64) -> MotionStore {
        MotionStore { dir, max_bytes }
    }

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

    /// Checks the file and copies it in, named after the file. The same file twice is one animation.
    pub fn import(&self, file: &Path) -> Result<MotionEntry, ModelError> {
        if std::fs::metadata(file)?.len() > self.max_bytes {
            return Err(ModelError::TooBig);
        }
        inspect_motion_file(file)?;
        let bytes = std::fs::read(file)?;
        if bytes.len() as u64 > self.max_bytes {
            return Err(ModelError::TooBig);
        }
        let id: String = {
            use sha2::{Digest, Sha256};
            Sha256::digest(&bytes)[..8]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        };
        if let Ok(entry) = self.entry(&id) {
            return Ok(entry);
        }
        // What is kept is what is checked: the bytes read, not the file again.
        let ms = inspect_motion(&read_json(&mut std::io::Cursor::new(&bytes))?)?;
        let stem = file
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let base = match clean_name(&stem) {
            name if name.is_empty() => "animation".to_string(),
            name => name,
        };
        let mut name = base.clone();
        let mut n = 2;
        while self.by_name(&name).is_some() {
            name = format!("{base} {n}");
            n += 1;
        }
        let folder = self.dir.join(&id);
        std::fs::create_dir_all(&folder)?;
        let placed = std::fs::write(folder.join(MOTION_FILE), &bytes)
            .map_err(ModelError::from)
            .and_then(|()| self.write_info(&folder, &Info { name, ms }));
        if let Err(e) = placed {
            let _ = std::fs::remove_dir_all(&folder);
            return Err(e);
        }
        self.entry(&id)
    }

    fn write_info(&self, folder: &Path, info: &Info) -> Result<(), ModelError> {
        let text = serde_json::to_string_pretty(info).map_err(|_| ModelError::Broken)?;
        let temp = folder.join(".info.tmp");
        std::fs::write(&temp, text)?;
        std::fs::rename(&temp, folder.join(INFO_FILE))?;
        Ok(())
    }

    fn entry(&self, id: &str) -> Result<MotionEntry, ModelError> {
        let folder = self.folder(id)?;
        let text = std::fs::read_to_string(folder.join(INFO_FILE))?;
        let info: Info = serde_json::from_str(&text).map_err(|_| ModelError::Broken)?;
        Ok(MotionEntry {
            id: id.to_string(),
            name: info.name,
            ms: info.ms,
        })
    }

    /// Sorted by name. A folder that cannot be read is passed over.
    pub fn list(&self) -> Vec<MotionEntry> {
        let Ok(dirs) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out: Vec<MotionEntry> = dirs
            .filter_map(Result::ok)
            .filter_map(|d| self.entry(&d.file_name().to_string_lossy()).ok())
            .collect();
        out.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.id.cmp(&b.id))
        });
        out
    }

    /// The animation of a name, without regard to case.
    pub fn by_name(&self, name: &str) -> Option<MotionEntry> {
        let wanted = clean_name(name).to_lowercase();
        self.list()
            .into_iter()
            .find(|m| m.name.to_lowercase() == wanted)
    }

    /// Gives an animation another name; an empty name or one another has is refused.
    pub fn rename(&self, id: &str, name: &str) -> Result<MotionEntry, ModelError> {
        let folder = self.folder(id)?;
        let mut entry = self.entry(id)?;
        let name = clean_name(name);
        if name.is_empty() || self.by_name(&name).is_some_and(|m| m.id != id) {
            return Err(ModelError::BadName);
        }
        self.write_info(
            &folder,
            &Info {
                name: name.clone(),
                ms: entry.ms,
            },
        )?;
        entry.name = name;
        Ok(entry)
    }

    /// The file, checked again before it is given.
    pub fn bytes(&self, id: &str) -> Result<Vec<u8>, ModelError> {
        let path = self.folder(id)?.join(MOTION_FILE);
        inspect_motion_file(&path)?;
        Ok(std::fs::read(path)?)
    }

    pub fn delete(&self, id: &str) -> Result<(), ModelError> {
        std::fs::remove_dir_all(self.folder(id)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::glb;
    use super::*;
    use serde_json::json;

    fn vrma() -> serde_json::Value {
        json!({
            "asset": { "version": "2.0" },
            "extensionsUsed": ["VRMC_vrm_animation"],
            "extensions": { "VRMC_vrm_animation": { "specVersion": "1.0", "humanoid": { "humanBones": {} } } },
            "accessors": [{ "max": [2.5], "min": [0.0], "count": 2, "type": "SCALAR", "componentType": 5126 }],
            "animations": [{ "samplers": [{ "input": 0, "output": 0 }], "channels": [] }]
        })
    }

    fn setup(name: &str, json: &serde_json::Value) -> (tempfile::TempDir, MotionStore, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let store = MotionStore::new(dir.path().join("motions"));
        let file = dir.path().join("from").join(name);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, glb(&json.to_string())).unwrap();
        (dir, store, file)
    }

    #[test]
    fn an_animation_is_taken_in_with_its_name_and_length() {
        let (_d, store, file) = setup("Wave hello.vrma", &vrma());
        let entry = store.import(&file).unwrap();
        assert_eq!(entry.name, "Wave hello");
        assert_eq!(entry.ms, 2500);
        assert_eq!(store.list(), vec![entry.clone()]);
        assert_eq!(
            store.bytes(&entry.id).unwrap(),
            std::fs::read(&file).unwrap()
        );
        // The same file twice is one animation.
        assert_eq!(store.import(&file).unwrap(), entry);
    }

    #[test]
    fn a_file_without_the_animation_extension_is_refused() {
        let mut json = vrma();
        json["extensions"] = json!({});
        let (_d, store, file) = setup("x.vrma", &json);
        assert!(matches!(store.import(&file), Err(ModelError::NotVrm)));
    }

    #[test]
    fn an_animation_that_refers_outside_is_refused() {
        let mut json = vrma();
        json["buffers"] = json!([{ "uri": "https://x.example/y.bin", "byteLength": 4 }]);
        let (_d, store, file) = setup("x.vrma", &json);
        assert!(matches!(store.import(&file), Err(ModelError::External)));
    }

    #[test]
    fn a_big_file_is_refused() {
        let (_d, _store, file) = setup("x.vrma", &vrma());
        let store = MotionStore::with_limit(file.parent().unwrap().join("motions"), 10);
        assert!(matches!(store.import(&file), Err(ModelError::TooBig)));
        assert_eq!(MAX_MOTION_BYTES, 20 * 1024 * 1024);
    }

    #[test]
    fn a_second_animation_of_the_same_name_gets_a_number() {
        let (dir, store, file) = setup("Dance.vrma", &vrma());
        store.import(&file).unwrap();
        let mut other = vrma();
        other["accessors"][0]["max"] = json!([1.0]);
        let second = dir.path().join("from").join("dance.vrma");
        std::fs::write(&second, glb(&other.to_string())).unwrap();
        let entry = store.import(&second).unwrap();
        assert_eq!(entry.name, "dance 2");
    }

    #[test]
    fn a_name_is_changed_unless_another_has_it() {
        let (dir, store, file) = setup("One.vrma", &vrma());
        let one = store.import(&file).unwrap();
        let mut other = vrma();
        other["accessors"][0]["max"] = json!([1.0]);
        let second = dir.path().join("from").join("Two.vrma");
        std::fs::write(&second, glb(&other.to_string())).unwrap();
        let two = store.import(&second).unwrap();
        assert!(matches!(
            store.rename(&two.id, "one"),
            Err(ModelError::BadName)
        ));
        assert!(matches!(
            store.rename(&two.id, "   "),
            Err(ModelError::BadName)
        ));
        let renamed = store.rename(&two.id, "  Bow  ").unwrap();
        assert_eq!(renamed.name, "Bow");
        assert_eq!(store.by_name("BOW").unwrap().id, two.id);
        assert_eq!(store.by_name("one").unwrap().id, one.id);
        assert!(store.by_name("nothing").is_none());
    }

    #[test]
    fn an_animation_is_deleted_with_its_folder() {
        let (_d, store, file) = setup("x.vrma", &vrma());
        let entry = store.import(&file).unwrap();
        store.delete(&entry.id).unwrap();
        assert!(store.list().is_empty());
        assert!(matches!(store.bytes(&entry.id), Err(ModelError::Unknown)));
    }

    #[test]
    fn a_wrong_id_is_refused_before_the_disk() {
        let (_d, store, _file) = setup("x.vrma", &vrma());
        for id in ["..", "../models", "ABCDEF0123456789", ""] {
            assert!(matches!(store.bytes(id), Err(ModelError::Unknown)), "{id}");
            assert!(matches!(store.delete(id), Err(ModelError::Unknown)), "{id}");
            assert!(
                matches!(store.rename(id, "x"), Err(ModelError::Unknown)),
                "{id}"
            );
        }
    }
}
