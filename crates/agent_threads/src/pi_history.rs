use crate::AgentLaunchCommand;
use crate::history::{AgentHistoryProvider, HistoricalThread};

pub struct PiHistoryProvider;

impl AgentHistoryProvider for PiHistoryProvider {
    fn resume_command(
        &self,
        base: &AgentLaunchCommand,
        thread: &HistoricalThread,
        extra_args: &[String],
    ) -> AgentLaunchCommand {
        let mut args = vec!["--session".to_string(), thread.session_id.to_string()];
        args.extend(extra_args.iter().cloned());
        AgentLaunchCommand {
            command: base.command.clone(),
            args,
            env: base.env.clone(),
            cwd: Some(thread.project_root.clone()),
            initialization_command: base.initialization_command.clone(),
            hidden: base.hidden,
            default_launch_option: base.default_launch_option.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::UNIX_EPOCH;

    use gpui::SharedString;

    use super::*;

    #[test]
    fn resume_uses_session_id_and_original_project_root() {
        let base = AgentLaunchCommand {
            command: Some("custom-pi".to_string()),
            args: vec!["--new-only".to_string()],
            env: [("PI_CODING_AGENT_DIR".to_string(), "/pi-home".to_string())]
                .into_iter()
                .collect(),
            initialization_command: Some("source ~/.profile".to_string()),
            hidden: true,
            ..Default::default()
        };
        let thread = HistoricalThread {
            session_id: SharedString::from("session-a"),
            title: SharedString::from("Pi session"),
            project_root: PathBuf::from("/root"),
            last_activity_at: UNIX_EPOCH,
        };

        let command = PiHistoryProvider.resume_command(
            &base,
            &thread,
            &["--extension".to_string(), "review.ts".to_string()],
        );

        assert_eq!(command.command.as_deref(), Some("custom-pi"));
        assert_eq!(
            command.args,
            ["--session", "session-a", "--extension", "review.ts"]
        );
        assert_eq!(command.cwd.as_deref(), Some(Path::new("/root")));
        assert_eq!(
            command.env.get("PI_CODING_AGENT_DIR").map(String::as_str),
            Some("/pi-home")
        );
        assert_eq!(
            command.initialization_command.as_deref(),
            Some("source ~/.profile")
        );
        assert!(command.hidden);
    }
}
