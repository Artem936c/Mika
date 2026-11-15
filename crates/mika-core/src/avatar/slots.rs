//! Which expressions of a model show each of the ten emotions.

use serde::{Deserialize, Serialize};

use super::vrm::STANDARD;
use crate::emotion::Emotion;

/// Who filled the slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Auto,
    Manual,
}

/// How the model shows one emotion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Slot {
    pub source: Source,
    /// Expression name and weight 0..=1.
    pub mix: Vec<(String, f32)>,
    /// Yaw and pitch of the gaze, degrees; positive is to her left and up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gaze: Option<[f32; 2]>,
}

/// The slot as the program fills it: a standard expression, then the
/// model's own one found by its name, then a mix of what the model has.
pub fn auto_slot(emotion: Emotion, expressions: &[String]) -> Slot {
    let rule = rule(emotion);
    let has = |name: &str| expressions.iter().any(|e| e == name);
    let whole = |name: &str| Slot {
        source: Source::Auto,
        mix: vec![(name.to_string(), 1.0)],
        gaze: None,
    };
    if let Some(name) = rule.standard.filter(|name| has(name)) {
        return whole(name);
    }
    let own = expressions.iter().find(|name| {
        let lower = name.to_lowercase();
        !STANDARD.contains(&name.as_str()) && rule.words.iter().any(|word| lower.contains(word))
    });
    if let Some(name) = own {
        return whole(name);
    }
    Slot {
        source: Source::Auto,
        mix: rule
            .fallback
            .iter()
            .filter(|(name, _)| has(name))
            .map(|(name, weight)| (name.to_string(), *weight))
            .collect(),
        gaze: rule.gaze,
    }
}

/// The slot as the user chose it: one expression at full weight.
pub fn manual_slot(expression: &str) -> Slot {
    Slot {
        source: Source::Manual,
        mix: vec![(expression.to_string(), 1.0)],
        gaze: None,
    }
}

/// How one slot is filled.
struct Rule {
    /// The standard expression that is this emotion.
    standard: Option<&'static str>,
    /// Lower-case pieces of a name that mean this emotion.
    words: &'static [&'static str],
    /// What to mix from the standard expressions when nothing else fits.
    fallback: &'static [(&'static str, f32)],
    gaze: Option<[f32; 2]>,
}

fn rule(emotion: Emotion) -> Rule {
    let (standard, words, fallback, gaze): (_, &[&str], &[(&str, f32)], _) = match emotion {
        Emotion::Neutral => (None, &[], &[], None),
        Emotion::Joy => (
            Some("happy"),
            &["happy", "joy", "smile", "喜", "笑", "рад"],
            &[],
            None,
        ),
        Emotion::Surprise => (
            Some("surprised"),
            &["surprise", "驚", "удив"],
            &[("oh", 0.5)],
            None,
        ),
        Emotion::Sad => (
            Some("sad"),
            &["sad", "sorrow", "cry", "悲", "груст"],
            &[],
            None,
        ),
        Emotion::Angry => (Some("angry"), &["angry", "anger", "怒", "зл"], &[], None),
        Emotion::Wink => (
            None,
            &["wink", "ウインク", "подмиг"],
            &[("blinkLeft", 1.0), ("happy", 0.5)],
            None,
        ),
        Emotion::Sleepy => (
            None,
            &["sleep", "眠", "сон"],
            &[("relaxed", 0.5), ("blink", 0.6)],
            None,
        ),
        Emotion::Shy => (
            None,
            &["shy", "blush", "embarrass", "照", "смущ"],
            &[("relaxed", 0.6)],
            Some([15.0, -15.0]),
        ),
        Emotion::Think => (None, &["think", "考", "дума"], &[], Some([-15.0, 15.0])),
        Emotion::Worry => (
            None,
            &["worry", "anxious", "trouble", "困", "трев"],
            &[("sad", 0.5), ("surprised", 0.2)],
            None,
        ),
    };
    Rule {
        standard,
        words,
        fallback,
        gaze,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn mix(list: &[(&str, f32)]) -> Vec<(String, f32)> {
        list.iter().map(|(n, w)| (n.to_string(), *w)).collect()
    }

    #[test]
    fn standard_expressions_fill_their_slots() {
        let model = names(&["happy", "sad", "angry", "surprised"]);
        for (emotion, name) in [
            (Emotion::Joy, "happy"),
            (Emotion::Sad, "sad"),
            (Emotion::Angry, "angry"),
            (Emotion::Surprise, "surprised"),
        ] {
            let slot = auto_slot(emotion, &model);
            assert_eq!(slot.mix, mix(&[(name, 1.0)]), "{emotion:?}");
            assert_eq!((slot.source, slot.gaze), (Source::Auto, None));
        }
    }

    #[test]
    fn a_custom_expression_is_found_by_its_name() {
        let slot = auto_slot(Emotion::Shy, &names(&["happy", "Blush_A"]));
        assert_eq!(slot.mix, mix(&[("Blush_A", 1.0)]));
        assert_eq!(slot.gaze, None);
        assert_eq!(
            auto_slot(Emotion::Shy, &names(&["照れ"])).mix,
            mix(&[("照れ", 1.0)])
        );
        assert_eq!(
            auto_slot(Emotion::Worry, &names(&["Тревога"])).mix,
            mix(&[("Тревога", 1.0)])
        );
        // The standard expression wins over a name that only looks right.
        assert_eq!(
            auto_slot(Emotion::Joy, &names(&["Smile2", "happy"])).mix,
            mix(&[("happy", 1.0)])
        );
        assert_eq!(
            auto_slot(Emotion::Joy, &names(&["Smile2"])).mix,
            mix(&[("Smile2", 1.0)])
        );
    }

    #[test]
    fn a_standard_name_is_never_taken_as_custom() {
        // "relaxed" is a part of the sleepy mix, not an expression called sleepy.
        assert_eq!(
            auto_slot(Emotion::Sleepy, &names(&["relaxed"])).mix,
            mix(&[("relaxed", 0.5)])
        );
        // "sad" must not become the angry face because of a word inside it.
        assert!(auto_slot(Emotion::Angry, &names(&["sad"])).mix.is_empty());
    }

    #[test]
    fn fallbacks_drop_what_the_model_lacks() {
        assert_eq!(
            auto_slot(Emotion::Wink, &names(&["happy"])).mix,
            mix(&[("happy", 0.5)])
        );
        assert_eq!(
            auto_slot(Emotion::Wink, &names(&["blinkLeft", "happy"])).mix,
            mix(&[("blinkLeft", 1.0), ("happy", 0.5)])
        );
        assert_eq!(
            auto_slot(Emotion::Worry, &names(&["sad"])).mix,
            mix(&[("sad", 0.5)])
        );
        assert_eq!(
            auto_slot(Emotion::Sleepy, &names(&["relaxed", "blink"])).mix,
            mix(&[("relaxed", 0.5), ("blink", 0.6)])
        );
    }

    #[test]
    fn vrm_0_surprise_falls_back_to_the_mouth() {
        assert_eq!(
            auto_slot(Emotion::Surprise, &names(&["aa", "oh"])).mix,
            mix(&[("oh", 0.5)])
        );
    }

    #[test]
    fn a_model_without_expressions_gets_empty_mixes() {
        for emotion in Emotion::ALL {
            let slot = auto_slot(emotion, &[]);
            assert!(slot.mix.is_empty(), "{emotion:?}");
            assert_eq!(slot.source, Source::Auto);
        }
        // The eyes still say something.
        assert_eq!(auto_slot(Emotion::Shy, &[]).gaze, Some([15.0, -15.0]));
        assert_eq!(auto_slot(Emotion::Think, &[]).gaze, Some([-15.0, 15.0]));
        assert_eq!(auto_slot(Emotion::Neutral, &[]).gaze, None);
    }

    #[test]
    fn the_gaze_belongs_to_the_fallback_only() {
        assert_eq!(
            auto_slot(Emotion::Shy, &names(&["relaxed"])).gaze,
            Some([15.0, -15.0])
        );
        assert_eq!(auto_slot(Emotion::Shy, &names(&["Shy"])).gaze, None);
    }

    #[test]
    fn a_manual_slot_is_one_expression_at_full_weight() {
        let slot = manual_slot("happy");
        assert_eq!(slot.source, Source::Manual);
        assert_eq!(slot.mix, mix(&[("happy", 1.0)]));
        assert_eq!(slot.gaze, None);
    }

    #[test]
    fn a_slot_is_written_as_pairs() {
        let slot = auto_slot(Emotion::Shy, &names(&["relaxed"]));
        assert_eq!(
            serde_json::to_value(&slot).unwrap(),
            serde_json::json!({ "source": "auto", "mix": [["relaxed", 0.6f32]], "gaze": [15.0, -15.0] })
        );
        let plain = serde_json::to_value(manual_slot("happy")).unwrap();
        assert!(plain.get("gaze").is_none(), "{plain}");
    }
}
