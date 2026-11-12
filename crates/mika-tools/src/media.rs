//! Media playback in any player: pause, next, previous, what is playing.

use std::sync::Arc;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{arg_str, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::sys::{Media, MediaAction};
use crate::util::{failed, schema};

fn action(s: &str) -> Result<(MediaAction, &'static str), ToolError> {
    Ok(match s {
        "play_pause" | "pause" | "play" => (MediaAction::PlayPause, "Пауза или продолжить"),
        "next" => (MediaAction::Next, "Следующий трек"),
        "previous" | "prev" => (MediaAction::Previous, "Предыдущий трек"),
        "stop" => (MediaAction::Stop, "Остановить воспроизведение"),
        other => {
            return Err(ToolError::BadArguments(format!(
                "неизвестное действие {other}"
            )))
        }
    })
}

pub struct MediaControlTool(pub Arc<dyn Media>);

#[async_trait]
impl Tool for MediaControlTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "media_control",
            "Управлять музыкой и видео в любом плеере: пауза и продолжение, следующий, предыдущий, стоп.",
            schema(
                &[("action", "string", "one of: play_pause, next, previous, stop")],
                &["action"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let (_, summary) = action(arg_str(args, "action")?)?;
        Ok(Assessment::new(
            RiskLevel::Low,
            summary,
            "плеер",
            "как клавиша на клавиатуре",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let (act, summary) = action(arg_str(&args, "action")?)?;
        self.0.control(act).map_err(failed)?;
        Ok(ToolOutput::new(format!(
            "{summary}: команда отправлена плееру."
        )))
    }
}

pub struct NowPlayingTool(pub Arc<dyn Media>);

#[async_trait]
impl Tool for NowPlayingTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "now_playing",
            "Что сейчас играет: название, исполнитель, плеер.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Узнать, что играет",
            "плеер",
            "только чтение",
        ))
    }

    async fn run(
        &self,
        _args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let Some(t) = self.0.now_playing().map_err(failed)? else {
            return Ok(ToolOutput::new("Сейчас ничего не играет."));
        };
        let state = if t.playing {
            "играет"
        } else {
            "на паузе"
        };
        let who = if t.artist.is_empty() {
            String::new()
        } else {
            format!(" — {}", t.artist)
        };
        let app = if t.app.is_empty() {
            String::new()
        } else {
            format!(" ({})", t.app)
        };
        Ok(
            ToolOutput::new(format!("Сейчас {state}: {}{who}{app}.", t.title)).with_data(json!({
                "kind": "track", "title": t.title, "artist": t.artist, "playing": t.playing
            })),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::Track;
    use crate::testing::{env, run};

    #[tokio::test]
    async fn control_sends_the_action() {
        let (e, f) = env();
        let tool = MediaControlTool(e.media.clone());
        run(&tool, json!({"action": "next"})).await.unwrap();
        run(&tool, json!({"action": "pause"})).await.unwrap();
        assert_eq!(
            *f.media.lock(),
            vec![MediaAction::Next, MediaAction::PlayPause]
        );
        assert!(tool.assess(&json!({"action": "louder"})).is_err());
    }

    #[tokio::test]
    async fn now_playing_names_track_or_says_silence() {
        let (e, f) = env();
        let tool = NowPlayingTool(e.media.clone());
        assert_eq!(
            run(&tool, json!({})).await.unwrap().text,
            "Сейчас ничего не играет."
        );
        *f.track.lock() = Some(Track {
            title: "Кукушка".into(),
            artist: "Кино".into(),
            app: "Яндекс Музыка".into(),
            playing: true,
        });
        let out = run(&tool, json!({})).await.unwrap();
        assert_eq!(out.text, "Сейчас играет: Кукушка — Кино (Яндекс Музыка).");
        assert_eq!(out.data.unwrap()["kind"], "track");
    }
}
