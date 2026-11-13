//! Picture generation through the configured image provider.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{arg_str, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::sys::ImageGen;
use crate::util::{failed, schema, truncate};

/// Folder inside the user profile where generated pictures go.
pub const PICTURES_SUBDIR: &str = "Pictures\\Mika";

pub struct GenerateImageTool {
    pub images: Arc<dyn ImageGen>,
    pub user_profile: String,
}

/// File extension by the first bytes of the picture.
pub fn image_extension(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        "webp"
    } else {
        "png"
    }
}

/// First words of the prompt as a file name part: letters, digits, spaces.
fn slug(prompt: &str) -> String {
    let clean: String = prompt
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let words: Vec<&str> = clean.split_whitespace().take(5).collect();
    truncate(&words.join(" "), 40)
        .trim_end_matches('…')
        .to_string()
}

#[async_trait]
impl Tool for GenerateImageTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "generate_image",
            "Нарисовать картинку по описанию. Файл сохраняется в «Изображения\\Mika» и показывается в чате.",
            schema(
                &[("prompt", "string", "Подробное описание картинки")],
                &["prompt"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let prompt = arg_str(args, "prompt")?;
        if prompt.trim().is_empty() {
            return Err(ToolError::BadArguments("пустое описание картинки".into()));
        }
        Ok(Assessment::new(
            RiskLevel::Low,
            "Нарисовать картинку",
            truncate(prompt, 200),
            "описание уйдёт провайдеру картинок; создаст один файл",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let prompt = arg_str(&args, "prompt")?.trim().to_string();
        let bytes = tokio::select! {
            _ = cancel.cancelled() => return Err(ToolError::Cancelled),
            b = self.images.generate(&prompt) => b.map_err(failed)?,
        };
        if bytes.is_empty() {
            return Err(failed("провайдер вернул пустую картинку"));
        }
        let folder = PathBuf::from(&self.user_profile).join(PICTURES_SUBDIR);
        tokio::fs::create_dir_all(&folder)
            .await
            .map_err(|e| failed(e.to_string()))?;
        let stamp = mika_core::storage::now().replace(':', "-");
        let name = format!(
            "{} {}.{}",
            &stamp[..19.min(stamp.len())],
            slug(&prompt),
            image_extension(&bytes)
        );
        let path = folder.join(name.replace("  ", " "));
        tokio::fs::write(&path, &bytes)
            .await
            .map_err(|e| failed(e.to_string()))?;
        Ok(
            ToolOutput::new(format!(
                "Картинка готова: {}. Чтобы отправить её в Telegram, вызови messenger_send с file — этим путём.",
                path.display()
            ))
                .with_data(json!({ "kind": "image", "path": path.display().to_string() })),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{env_with, run};
    use mika_core::storage::Db;

    #[tokio::test]
    async fn picture_is_saved_and_returned_as_a_card() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().to_string_lossy().into_owned();
        let (e, _) = env_with(Arc::new(Db::open_in_memory().unwrap()), &profile);
        let tool = GenerateImageTool {
            images: e.shared.images.clone(),
            user_profile: profile,
        };
        let out = run(&tool, json!({"prompt": "Рыжий кот, акварель!"}))
            .await
            .unwrap();
        let data = out.data.unwrap();
        assert_eq!(data["kind"], "image");
        let path = data["path"].as_str().unwrap();
        assert!(path.contains("Pictures\\Mika"), "{path}");
        assert!(path.ends_with("Рыжий кот акварель.jpg"), "{path}");
        assert_eq!(std::fs::read(path).unwrap(), [0xFF, 0xD8, 0xFF, 0xE0]);
        assert!(tool.assess(&json!({"prompt": " "})).is_err());
    }

    #[test]
    fn extension_follows_the_bytes() {
        assert_eq!(image_extension(b"\x89PNG\r\n"), "png");
        assert_eq!(image_extension(&[0xFF, 0xD8, 0xFF, 0xE0]), "jpg");
        assert_eq!(image_extension(b"RIFF\0\0\0\0WEBPVP8 "), "webp");
        assert_eq!(image_extension(b""), "png");
    }
}
