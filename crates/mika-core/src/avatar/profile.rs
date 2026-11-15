//! The profile of a model: what it is and how it shows the ten emotions.
//! Lives beside the model file as `profile.json`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::slots::{auto_slot, manual_slot, Slot};
use super::vrm::{ModelInfo, Spec};
use super::ModelError;
use crate::emotion::Emotion;

/// The version of the profile file. Another one is not read.
pub const PROFILE_VERSION: u32 = 1;

/// The usual lip strength and its ends.
const LIP: f32 = 1.0;
const LIP_RANGE: (f32, f32) = (0.0, 1.5);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub version: u32,
    /// The kind of the model: `vrm`.
    pub format: String,
    pub spec: Spec,
    pub title: String,
    pub author: String,
    pub license: String,
    /// Every expression of the model, for the manual choice.
    pub expressions: Vec<String>,
    /// Keyed by `Emotion::as_str()`, all ten present.
    pub slots: BTreeMap<String, Slot>,
    /// Lip strength, 0.0..=1.5.
    pub lip: f32,
}

impl Profile {
    /// The profile of a model just brought in: every slot filled by the program.
    pub fn from_info(info: &ModelInfo) -> Profile {
        Profile {
            version: PROFILE_VERSION,
            format: "vrm".into(),
            spec: info.spec,
            title: info.title.clone(),
            author: info.author.clone(),
            license: info.license.clone(),
            expressions: info.expressions.clone(),
            slots: Emotion::ALL
                .iter()
                .map(|e| (e.as_str().to_string(), auto_slot(*e, &info.expressions)))
                .collect(),
            lip: LIP,
        }
    }

    /// `None` gives the slot back to the program; a name must be an
    /// expression of this model.
    pub fn set_slot(
        &mut self,
        emotion: Emotion,
        expression: Option<&str>,
    ) -> Result<(), ModelError> {
        let slot = match expression {
            None => auto_slot(emotion, &self.expressions),
            Some(name) if self.expressions.iter().any(|e| e == name) => manual_slot(name),
            Some(_) => return Err(ModelError::Unknown),
        };
        self.slots.insert(emotion.as_str().to_string(), slot);
        Ok(())
    }

    pub fn set_lip(&mut self, lip: f32) {
        self.lip = if lip.is_nan() {
            LIP
        } else {
            lip.clamp(LIP_RANGE.0, LIP_RANGE.1)
        };
    }

    /// A file that is not a profile of this version is broken.
    pub fn read(path: &Path) -> Result<Profile, ModelError> {
        let text = std::fs::read(path)?;
        let mut profile: Profile = serde_json::from_slice(&text).map_err(|_| ModelError::Broken)?;
        if profile.version != PROFILE_VERSION {
            return Err(ModelError::Broken);
        }
        // A file edited by hand: only the ten slots, every one of them, a sane strength.
        let mut slots = std::mem::take(&mut profile.slots);
        for emotion in Emotion::ALL {
            let slot = slots
                .remove(emotion.as_str())
                .unwrap_or_else(|| auto_slot(emotion, &profile.expressions));
            profile.slots.insert(emotion.as_str().to_string(), slot);
        }
        profile.set_lip(profile.lip);
        Ok(profile)
    }

    /// Writes beside and renames: a cut write never leaves half a profile.
    pub fn write(&self, path: &Path) -> Result<(), ModelError> {
        let text = serde_json::to_vec_pretty(self).map_err(|_| ModelError::Broken)?;
        let beside = path.with_extension("json.tmp");
        std::fs::write(&beside, text)?;
        std::fs::rename(&beside, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&beside);
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::slots::Source;
    use super::*;

    fn info(expressions: &[&str]) -> ModelInfo {
        ModelInfo {
            spec: Spec::V1,
            title: "Test".into(),
            author: "Somebody".into(),
            license: "CC0".into(),
            expressions: expressions.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn a_new_profile_has_all_ten_slots_filled_by_the_program() {
        let profile = Profile::from_info(&info(&["happy", "sad"]));
        assert_eq!(profile.version, PROFILE_VERSION);
        assert_eq!(profile.format, "vrm");
        assert_eq!(profile.lip, 1.0);
        assert_eq!(profile.expressions, ["happy", "sad"]);
        assert_eq!(profile.slots.len(), 10);
        for emotion in Emotion::ALL {
            assert_eq!(profile.slots[emotion.as_str()].source, Source::Auto);
        }
        assert_eq!(profile.slots["joy"].mix, [("happy".to_string(), 1.0)]);
    }

    #[test]
    fn a_manual_choice_replaces_the_mix_and_the_gaze() {
        let mut profile = Profile::from_info(&info(&["happy", "relaxed"]));
        assert!(profile.slots["shy"].gaze.is_some());
        profile.set_slot(Emotion::Shy, Some("happy")).unwrap();
        let slot = &profile.slots["shy"];
        assert_eq!(slot.source, Source::Manual);
        assert_eq!(slot.mix, [("happy".to_string(), 1.0)]);
        assert_eq!(slot.gaze, None);

        profile.set_slot(Emotion::Shy, None).unwrap();
        assert_eq!(
            profile.slots["shy"],
            auto_slot(Emotion::Shy, &profile.expressions)
        );
    }

    #[test]
    fn a_manual_choice_must_be_an_expression_of_the_model() {
        let mut profile = Profile::from_info(&info(&["happy"]));
        let before = profile.clone();
        assert!(matches!(
            profile.set_slot(Emotion::Joy, Some("nope")),
            Err(ModelError::Unknown)
        ));
        assert_eq!(profile, before);
    }

    #[test]
    fn lip_strength_stays_in_range() {
        let mut profile = Profile::from_info(&info(&[]));
        for (given, kept) in [
            (9.0, 1.5),
            (-1.0, 0.0),
            (0.7, 0.7),
            (f32::NAN, 1.0),
            (f32::INFINITY, 1.5),
        ] {
            profile.set_lip(given);
            assert_eq!(profile.lip, kept, "{given}");
        }
    }

    #[test]
    fn a_profile_survives_the_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.json");
        let mut profile = Profile::from_info(&info(&["happy", "relaxed", "Blush"]));
        profile.set_slot(Emotion::Worry, Some("relaxed")).unwrap();
        profile.set_lip(0.5);
        profile.write(&path).unwrap();
        assert_eq!(Profile::read(&path).unwrap(), profile);
        // Nothing but the profile is left beside it.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_profile_of_another_version_is_broken() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.json");
        let mut json = serde_json::to_value(Profile::from_info(&info(&[]))).unwrap();
        json["version"] = serde_json::json!(2);
        std::fs::write(&path, json.to_string()).unwrap();
        assert!(matches!(Profile::read(&path), Err(ModelError::Broken)));

        std::fs::write(&path, "not json").unwrap();
        assert!(matches!(Profile::read(&path), Err(ModelError::Broken)));
        assert!(matches!(
            Profile::read(&dir.path().join("absent.json")),
            Err(ModelError::Io(_))
        ));
    }

    #[test]
    fn a_profile_edited_by_hand_is_made_whole_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.json");
        let mut json = serde_json::to_value(Profile::from_info(&info(&["happy"]))).unwrap();
        json["slots"].as_object_mut().unwrap().remove("joy");
        json["slots"]["made_up"] = serde_json::json!({ "source": "manual", "mix": [] });
        json["lip"] = serde_json::json!(40.0);
        std::fs::write(&path, json.to_string()).unwrap();

        let profile = Profile::read(&path).unwrap();
        assert_eq!(profile.slots.len(), 10);
        assert_eq!(profile.slots["joy"].mix, [("happy".to_string(), 1.0)]);
        assert_eq!(profile.lip, 1.5);
    }
}
