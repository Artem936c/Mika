//! «Помаши», «поклонись», «станцуй»: a gesture of the avatar (decision 0040).
//! One of her own gestures, or an animation the user brought in, by name.
//! It only moves the model on the screen: low risk.

use std::path::PathBuf;

use async_trait::async_trait;
use mika_core::avatar::MotionStore;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{arg_str_opt, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use mika_core::{Event, EventBus};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

/// Her own gestures, as the scene of the window knows them (`GESTURES` in body.ts).
pub const BUILTIN_GESTURES: [&str; 9] = [
    "wave",
    "bow",
    "nod",
    "shake_head",
    "shrug",
    "clap",
    "peace",
    "spin",
    "dance",
];

pub struct AvatarGestureTool {
    pub bus: EventBus,
    /// The folder of the user's animations.
    pub motions: PathBuf,
}

impl AvatarGestureTool {
    fn names(&self) -> Vec<String> {
        MotionStore::new(self.motions.clone())
            .list()
            .into_iter()
            .map(|m| m.name)
            .collect()
    }

    /// The gesture a call asks for: a built-in name, or an animation of the user.
    fn pick(&self, args: &Value) -> Result<(String, Option<String>), ToolError> {
        let name = arg_str_opt(args, "name").unwrap_or("").trim().to_string();
        let lower = name.to_lowercase();
        // The user's own animation wins over her gesture of the same name.
        if let Some(m) = MotionStore::new(self.motions.clone()).by_name(&name) {
            return Ok((m.name, Some(m.id)));
        }
        if let Some(own) = BUILTIN_GESTURES.iter().find(|g| **g == lower) {
            return Ok((own.to_string(), None));
        }
        let mut all: Vec<String> = BUILTIN_GESTURES.iter().map(|g| g.to_string()).collect();
        all.extend(self.names());
        Err(ToolError::BadArguments(format!(
            "нет такого жеста «{name}»; есть: {}",
            all.join(", ")
        )))
    }
}

#[async_trait]
impl Tool for AvatarGestureTool {
    fn spec(&self) -> ToolSpec {
        let own = self.names();
        let mut description = format!(
            "Жест аватара на экране: помахать, поклониться, кивнуть, покачать головой, пожать плечами, похлопать, показать «мир», покружиться, станцевать. Встроенные: {}.",
            BUILTIN_GESTURES.join(", ")
        );
        if !own.is_empty() {
            description.push_str(&format!(
                " Анимации пользователя по имени: {}.",
                own.join(", ")
            ));
        }
        description.push_str(" Вызывай, когда просят что-то показать телом или станцевать.");
        spec(
            "avatar_gesture",
            &description,
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "встроенный жест или имя анимации пользователя" },
                },
                "required": ["name"],
            }),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let (name, _) = self.pick(args)?;
        Ok(Assessment::new(
            RiskLevel::Low,
            "Жест аватара",
            name,
            "только движение модели на экране",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let (name, motion) = self.pick(&args)?;
        self.bus.publish(Event::AvatarGesture {
            name: name.clone(),
            motion,
        });
        Ok(ToolOutput::new(format!(
            "Жест «{name}» показан. Если выбран встроенный аватар-картинка, он просто подпрыгивает."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> (tempfile::TempDir, AvatarGestureTool) {
        let dir = tempfile::tempdir().unwrap();
        let t = AvatarGestureTool {
            bus: EventBus::new(16),
            motions: dir.path().join("motions"),
        };
        (dir, t)
    }

    /// An animation file of the user, put straight into the folder.
    fn own_motion(dir: &std::path::Path, name: &str) -> String {
        let id = "0123456789abcdef";
        let folder = dir.join("motions").join(id);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("info.json"),
            json!({ "name": name, "ms": 1000 }).to_string(),
        )
        .unwrap();
        id.to_string()
    }

    #[test]
    fn a_gesture_is_low_risk() {
        let (_d, t) = tool();
        let a = t.assess(&json!({ "name": "wave" })).unwrap();
        assert_eq!(a.level, RiskLevel::Low);
    }

    #[tokio::test]
    async fn a_built_in_gesture_is_sent_to_the_windows() {
        let (_d, t) = tool();
        let mut rx = t.bus.subscribe();
        t.run(
            json!({ "name": "Wave" }),
            Approval::for_tests(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            rx.recv().await.unwrap(),
            Event::AvatarGesture {
                name: "wave".into(),
                motion: None
            }
        );
    }

    #[tokio::test]
    async fn an_animation_of_the_user_is_found_by_name_in_any_case() {
        let (d, t) = tool();
        let id = own_motion(d.path(), "Victory Dance");
        let mut rx = t.bus.subscribe();
        t.run(
            json!({ "name": "victory dance" }),
            Approval::for_tests(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            rx.recv().await.unwrap(),
            Event::AvatarGesture {
                name: "Victory Dance".into(),
                motion: Some(id)
            }
        );
    }

    #[tokio::test]
    async fn an_animation_of_the_user_wins_over_a_gesture_of_the_same_name() {
        let (d, t) = tool();
        let id = own_motion(d.path(), "Dance");
        let mut rx = t.bus.subscribe();
        t.run(
            json!({ "name": "dance" }),
            Approval::for_tests(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            rx.recv().await.unwrap(),
            Event::AvatarGesture {
                name: "Dance".into(),
                motion: Some(id)
            }
        );
    }

    #[test]
    fn an_unknown_gesture_names_the_known_ones() {
        let (d, t) = tool();
        own_motion(d.path(), "Victory Dance");
        let Err(ToolError::BadArguments(text)) = t.assess(&json!({ "name": "moonwalk" })) else {
            panic!("an unknown name must be refused");
        };
        assert!(
            text.contains("wave") && text.contains("dance") && text.contains("Victory Dance"),
            "{text}"
        );
    }

    #[test]
    fn the_description_lists_the_gestures_and_the_animations() {
        let (d, t) = tool();
        assert!(!t.spec().description.contains("Анимации пользователя"));
        own_motion(d.path(), "Victory Dance");
        let text = t.spec().description;
        for g in BUILTIN_GESTURES {
            assert!(text.contains(g), "{g}");
        }
        assert!(text.contains("Victory Dance"));
    }
}
