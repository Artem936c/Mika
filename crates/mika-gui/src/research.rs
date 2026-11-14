//! Settings of the deep search: the model, the report folder, the limits.

use std::sync::Arc;

use mika_core::config::ResearchConfig;
use mika_core::policy::paths::{classify_path, path_risk, PathAccess};
use mika_core::tools::RiskLevel;
use mika_tools::research::{folder_problem, report_folder};

use crate::app::Ctx;

type CtxState<'a> = tauri::State<'a, Arc<Ctx>>;

/// Longest model name that is saved.
const MODEL_MAX: usize = 200;

/// The deep search block of the «Tools» page.
#[derive(Debug, PartialEq, serde::Serialize)]
pub struct ResearchView {
    /// The saved setting, may be empty.
    folder: String,
    /// Where reports go now.
    folder_shown: String,
    /// `%USERPROFILE%\Documents\Mika\Research`.
    folder_default: String,
    /// The saved setting, may be empty.
    model: String,
    /// The model the inner requests use now.
    model_shown: String,
    max_searches: u32,
    max_pages: u32,
    max_minutes: u32,
    max_tokens: u32,
    /// Most paid model requests of one run.
    llm_calls: u32,
    /// So many tokens that every run is asked about.
    high_tokens: bool,
    /// The folder is a place where every write is asked about.
    high_folder: bool,
}

/// The view of `settings` for a user with the profile `user_profile`.
/// `model_shown` is the model an empty setting means.
fn view(settings: &ResearchConfig, user_profile: &str, model_shown: String) -> ResearchView {
    let limits = settings.limits();
    let folder = report_folder(&settings.folder, user_profile);
    let class = classify_path(&folder.to_string_lossy(), user_profile, false);
    ResearchView {
        folder: settings.folder.clone(),
        folder_shown: folder.display().to_string(),
        folder_default: report_folder("", user_profile).display().to_string(),
        model: settings.model.clone(),
        model_shown,
        max_searches: limits.searches,
        max_pages: limits.pages,
        max_minutes: limits.minutes,
        max_tokens: limits.tokens,
        llm_calls: limits.llm_calls(),
        high_tokens: limits.high(),
        high_folder: path_risk(class, PathAccess::Write) == RiskLevel::High,
    }
}

/// The settings as they are saved: the texts trimmed, the limits inside their
/// bounds. `Err` when the folder cannot be used.
fn checked(
    folder: &str,
    model: &str,
    max_searches: u32,
    max_pages: u32,
    max_minutes: u32,
    max_tokens: u32,
) -> Result<ResearchConfig, String> {
    let folder = folder.trim().to_string();
    if folder_problem(&folder).is_some() {
        return Err(crate::strings::tr("error.research_folder"));
    }
    let model: String = model.trim().chars().take(MODEL_MAX).collect();
    let limits = ResearchConfig {
        max_searches,
        max_pages,
        max_minutes,
        max_tokens,
        ..Default::default()
    }
    .limits();
    Ok(ResearchConfig {
        folder,
        model: model.trim().to_string(),
        max_searches: limits.searches,
        max_pages: limits.pages,
        max_minutes: limits.minutes,
        max_tokens: limits.tokens,
    })
}

fn view_of(ctx: &Ctx) -> ResearchView {
    let settings = ctx.config.lock().research.clone();
    let env = ctx.tool_env();
    let model_shown = env.shared.text_model.model_id(&settings.model);
    view(&settings, &env.user_profile, model_shown)
}

#[tauri::command]
pub fn get_research_settings(ctx: CtxState<'_>) -> ResearchView {
    view_of(&ctx)
}

/// Saves the settings. The running tool reads them at its next call: the
/// registry is not rebuilt. The folder is made only when a report is written.
#[tauri::command]
pub fn set_research_settings(
    ctx: CtxState<'_>,
    folder: String,
    model: String,
    max_searches: u32,
    max_pages: u32,
    max_minutes: u32,
    max_tokens: u32,
) -> Result<ResearchView, String> {
    let new = checked(
        &folder,
        &model,
        max_searches,
        max_pages,
        max_minutes,
        max_tokens,
    )?;
    ctx.config.lock().research = new.clone();
    *ctx.tool_env().shared.research.write() = new;
    ctx.save_config()?;
    Ok(view_of(&ctx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strings::tr;

    const PROFILE: &str = "C:\\Users\\me";

    #[test]
    fn folder_error_has_a_text() {
        assert_ne!(tr("error.research_folder"), "error.research_folder");
        assert_eq!(
            checked("D:\\Reports\\*", "", 5, 8, 5, 50_000),
            Err(tr("error.research_folder"))
        );
        assert!(checked("https://example.com/x", "", 5, 8, 5, 50_000).is_err());
    }

    #[test]
    fn settings_are_trimmed_and_limits_stay_in_bounds() {
        let long = "m".repeat(300);
        let saved = checked("  D:\\Reports ", &format!(" {long} "), 0, 100, 1, 9_000_000).unwrap();
        assert_eq!(saved.folder, "D:\\Reports");
        assert_eq!(saved.model.chars().count(), MODEL_MAX);
        assert_eq!(
            (
                saved.max_searches,
                saved.max_pages,
                saved.max_minutes,
                saved.max_tokens
            ),
            (2, 16, 2, 200_000)
        );
        let plain = checked("", " mistralai/mistral-nemo ", 5, 8, 5, 50_000).unwrap();
        assert_eq!(
            plain,
            ResearchConfig {
                model: "mistralai/mistral-nemo".into(),
                ..Default::default()
            }
        );
    }

    #[test]
    fn view_shows_where_reports_go_and_what_a_run_costs() {
        let v = view(&ResearchConfig::default(), PROFILE, "gpt-4o-mini".into());
        assert_eq!(v.folder, "");
        assert_eq!(v.folder_shown, "C:\\Users\\me\\Documents\\Mika\\Research");
        assert_eq!(v.folder_default, v.folder_shown);
        assert_eq!(
            (v.model.as_str(), v.model_shown.as_str()),
            ("", "gpt-4o-mini")
        );
        assert_eq!(v.llm_calls, 10);
        assert!(!v.high_tokens && !v.high_folder);

        let risky = ResearchConfig {
            folder: "C:\\Program Files\\Reports".into(),
            max_tokens: 150_000,
            max_pages: 16,
            ..Default::default()
        };
        let v = view(&risky, PROFILE, "m".into());
        assert_eq!(v.folder_shown, "C:\\Program Files\\Reports");
        assert_eq!(v.folder_default, "C:\\Users\\me\\Documents\\Mika\\Research");
        assert_eq!(v.llm_calls, 18);
        assert!(v.high_tokens && v.high_folder);

        // A value outside the bounds in the file is shown as it will run.
        let odd = ResearchConfig {
            max_searches: 99,
            max_tokens: 1,
            ..Default::default()
        };
        let v = view(&odd, PROFILE, "m".into());
        assert_eq!((v.max_searches, v.max_tokens), (10, 20_000));
    }

    #[test]
    fn view_goes_to_the_window_in_snake_case() {
        let v = view(&ResearchConfig::default(), PROFILE, "m".into());
        let json = serde_json::to_value(&v).unwrap();
        for key in [
            "folder",
            "folder_shown",
            "folder_default",
            "model",
            "model_shown",
            "max_searches",
            "max_pages",
            "max_minutes",
            "max_tokens",
            "llm_calls",
            "high_tokens",
            "high_folder",
        ] {
            assert!(json.get(key).is_some(), "{key}");
        }
        assert_eq!(json["max_tokens"], 50_000);
    }
}
