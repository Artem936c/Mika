//! Windows tools. Each one goes through the risk policy via `mika_core::tools::Tool`.

pub mod apps;
pub mod files;
pub mod notes;
pub mod rdp;
pub mod shell;
pub mod sys;
pub mod system;
pub mod web;
pub mod win;

mod util;

#[cfg(test)]
mod testing;

use std::sync::Arc;

use mika_core::tools::{TimeTool, ToolRegistry};

pub use sys::ToolEnv;

/// Registers every tool. Risk levels come from each tool's `assess`.
pub fn register_all(reg: &mut ToolRegistry, env: &ToolEnv) {
    let profile = env.user_profile.clone();
    reg.register(Arc::new(TimeTool));

    reg.register(Arc::new(system::VolumeTool(env.volume.clone())));
    reg.register(Arc::new(system::PowerTool(env.power.clone())));
    reg.register(Arc::new(system::ClipboardTool(env.clipboard.clone())));
    reg.register(Arc::new(system::SystemInfoTool(env.probe.clone())));
    reg.register(Arc::new(system::ProcessesTool(env.probe.clone())));
    reg.register(Arc::new(system::KillProcessTool(env.runner.clone())));

    reg.register(Arc::new(apps::OpenAppTool::with_start_menu(
        env.launcher.clone(),
    )));
    reg.register(Arc::new(apps::ListWindowsTool(env.windows.clone())));
    reg.register(Arc::new(apps::FocusWindowTool(env.windows.clone())));
    reg.register(Arc::new(apps::CloseAppTool(env.windows.clone())));

    reg.register(Arc::new(files::FindFilesTool {
        user_profile: profile.clone(),
    }));
    reg.register(Arc::new(files::ReadFileTool {
        user_profile: profile.clone(),
    }));
    reg.register(Arc::new(files::WriteFileTool {
        user_profile: profile.clone(),
    }));
    reg.register(Arc::new(files::MovePathTool {
        user_profile: profile.clone(),
    }));
    reg.register(Arc::new(files::CreateFolderTool {
        user_profile: profile.clone(),
    }));
    reg.register(Arc::new(files::DeleteToTrashTool {
        trash: env.trash.clone(),
        user_profile: profile.clone(),
    }));
    reg.register(Arc::new(files::OpenPathTool {
        launcher: env.launcher.clone(),
        user_profile: profile,
    }));

    reg.register(Arc::new(shell::RunShellTool(env.runner.clone())));

    reg.register(Arc::new(web::OpenUrlTool(env.launcher.clone())));
    reg.register(Arc::new(web::WebSearchTool {
        http: env.http.clone(),
        enabled: env.web_enabled.clone(),
    }));
    reg.register(Arc::new(web::FetchPageTool {
        http: env.http.clone(),
        enabled: env.web_enabled.clone(),
    }));
    reg.register(Arc::new(web::WeatherTool {
        http: env.http.clone(),
        enabled: env.web_enabled.clone(),
    }));

    reg.register(Arc::new(notes::NoteCreateTool(env.db.clone())));
    reg.register(Arc::new(notes::NoteSearchTool(env.db.clone())));
    reg.register(Arc::new(notes::NoteReadTool(env.db.clone())));

    reg.register(Arc::new(rdp::RdpListTool(env.rdp.clone())));
    reg.register(Arc::new(rdp::RdpOpenTool {
        source: env.rdp.clone(),
        launcher: env.launcher.clone(),
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_unique_name_and_object_schema() {
        let (env, _) = testing::env();
        let mut reg = ToolRegistry::new();
        register_all(&mut reg, &env);
        let specs = reg.specs();
        assert_eq!(specs.len(), 28);
        for s in specs {
            assert_eq!(s.parameters["type"], "object", "{}", s.name);
            assert!(!s.description.is_empty());
        }
    }
}
