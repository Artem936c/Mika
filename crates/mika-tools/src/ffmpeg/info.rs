//! `av_info`: what is inside a video or sound file.

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::policy::paths::PathAccess;
use mika_core::tools::{spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::output::ext_of;
use super::time::show_time;
use super::{codec_name, quoted, size_text, AvEnv, MediaInfo};
use crate::files::path_assessment;
use crate::util::schema;

pub struct AvInfoTool(pub AvEnv);

fn decimal(x: f64) -> String {
    let s = format!("{x:.2}");
    s.trim_end_matches('0')
        .trim_end_matches('.')
        .replace('.', ",")
}

fn channels(n: u32) -> String {
    match n {
        1 => "моно".into(),
        2 => "стерео".into(),
        n => format!("{n} каналов"),
    }
}

/// Lines about the file for the model.
pub(crate) fn describe(name: &str, container: &str, info: &MediaInfo) -> String {
    let mut head = vec![format!("{name}: {container}")];
    if let Some(d) = info.duration_s {
        head.push(show_time(d));
    }
    head.push(size_text(info.size));
    if let Some(b) = info.bit_rate {
        head.push(format!("{} Мбит/с", decimal(b as f64 / 1_000_000.0)));
    }
    let mut lines = vec![format!("{}.", head.join(", "))];
    if let Some(v) = &info.video {
        let mut l = format!("Видео: {} {}×{}", codec_name(&v.codec), v.width, v.height);
        if let Some(fps) = v.fps {
            l.push_str(&format!(", {} кадр/с", decimal(fps)));
        }
        if v.rotation != 0 {
            l.push_str(&format!(", повёрнуто на {}°", v.rotation));
        }
        lines.push(format!("{l}."));
    } else {
        lines.push("Видео нет.".into());
    }
    if let Some(a) = &info.audio {
        let mut l = format!("Звук: {}", codec_name(&a.codec));
        if a.sample_rate > 0 {
            l.push_str(&format!(", {} кГц", decimal(a.sample_rate as f64 / 1000.0)));
        }
        if a.channels > 0 {
            l.push_str(&format!(", {}", channels(a.channels)));
        }
        if let Some(b) = a.bit_rate {
            l.push_str(&format!(", {} кбит/с", b / 1000));
        }
        lines.push(format!("{l}."));
    } else {
        lines.push("Звука нет.".into());
    }
    if info.subtitle_count > 0 {
        lines.push(format!("Дорожек субтитров: {}.", info.subtitle_count));
    }
    lines.join("\n")
}

#[async_trait]
impl Tool for AvInfoTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "av_info",
            "Сведения о видео- или аудиофайле: длительность, размер, кодеки, разрешение, частота кадров, звук.",
            schema(&[("path", "string", "Полный путь к файлу")], &["path"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let paths = self.0.ffmpeg()?;
        let input = self.0.input(args, "path")?;
        self.0.probe_now(&paths, &input)?;
        Ok(path_assessment(
            &input,
            &self.0.user_profile,
            PathAccess::Read,
            format!("Посмотреть сведения о {}", quoted(&input)),
            RiskLevel::Low,
        ))
    }

    async fn run(
        &self,
        args: Value,
        _approval: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let paths = self.0.ffmpeg()?;
        let input = self.0.input(&args, "path")?;
        let info = self.0.probe(&paths, &input, &cancel).await?;
        let ext = ext_of(&input).to_uppercase();
        let container = if ext.is_empty() {
            info.format_name.clone()
        } else {
            ext
        };
        Ok(ToolOutput::new(describe(
            &quoted(&input),
            &container,
            &info,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{av, probe_json};
    use super::*;
    use crate::testing::run;

    #[tokio::test]
    async fn info_reads_the_file_once_and_is_low_risk() {
        let t = av();
        let input = t.file("клип.mp4");
        t.answer(&probe_json(
            "mov,mp4",
            751.0,
            256_901_120,
            Some(("h264", 1280, 720)),
            Some("aac"),
        ));
        let tool = AvInfoTool(t.av());
        let args = serde_json::json!({ "path": input.display().to_string() });
        let a = tool.assess(&args).unwrap();
        assert_eq!(a.level, RiskLevel::Low);
        let out = run(&tool, args).await.unwrap();
        assert_eq!(
            out.text,
            "«клип.mp4»: MP4, 12:31, 245 МБ, 2 Мбит/с.\nВидео: H.264 1280×720, 30 кадр/с.\nЗвук: AAC, 48 кГц, стерео, 128 кбит/с."
        );
        assert!(out.data.is_none());
        assert_eq!(
            t.f.runs.lock().len(),
            1,
            "the second look comes from the cache"
        );
        let probe = &t.f.runs.lock()[0];
        assert!(probe.0.ends_with("ffprobe.exe"));
    }

    #[test]
    fn missing_ffmpeg_stops_the_risk_check_with_a_card() {
        let t = av();
        *t.env.shared.ffmpeg_path.write() = t.dir.path().join("nothing").display().to_string();
        if super::super::locate(&t.env.shared.ffmpeg_path.read()).is_some() {
            return; // This machine has FFmpeg in PATH.
        }
        let input = t.file("a.mp4");
        let r =
            AvInfoTool(t.av()).assess(&serde_json::json!({ "path": input.display().to_string() }));
        assert!(matches!(
            r,
            Err(ToolError::NotReady { data, .. }) if data["kind"] == "ffmpeg_missing"
        ));
    }

    #[test]
    fn addresses_and_missing_files_are_refused() {
        let t = av();
        let tool = AvInfoTool(t.av());
        for path in ["https://x/a.mp4", "C:\\нет\\a.mp4"] {
            assert!(matches!(
                tool.assess(&serde_json::json!({ "path": path })),
                Err(ToolError::BadArguments(_))
            ));
        }
    }
}
