use crate::AgentLaunchCommand;
use crate::history::{AgentHistoryProvider, HistoricalThread};

pub struct CodexHistoryProvider;

impl AgentHistoryProvider for CodexHistoryProvider {
    fn resume_command(
        &self,
        base: &AgentLaunchCommand,
        thread: &HistoricalThread,
        extra_args: &[String],
    ) -> AgentLaunchCommand {
        let mut args = vec!["resume".to_string(), thread.session_id.to_string()];
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
    use super::*;
    use collections::HashMap;
    use gpui::SharedString;
    use std::path::PathBuf;
    use std::time::SystemTime;

    #[test]
    fn resume_command_uses_resume_subcommand_and_session_id() {
        let base = AgentLaunchCommand {
            command: Some("codex".to_string()),
            args: vec!["--ignored-fresh-session-arg".to_string()],
            env: HashMap::default(),
            cwd: None,
            initialization_command: Some("source ~/.profile".to_string()),
            hidden: false,
            default_launch_option: None,
        };
        let thread = HistoricalThread {
            session_id: SharedString::from("session-a"),
            title: SharedString::from("title"),
            project_root: PathBuf::from("/root"),
            last_activity_at: SystemTime::UNIX_EPOCH,
        };

        let resumed = CodexHistoryProvider.resume_command(&base, &thread, &[]);

        assert_eq!(resumed.command, Some("codex".to_string()));
        assert_eq!(resumed.args, vec!["resume", "session-a"]);
        assert_eq!(resumed.cwd, Some(PathBuf::from("/root")));
        assert_eq!(
            resumed.initialization_command.as_deref(),
            Some("source ~/.profile")
        );
    }

    #[test]
    fn resume_command_appends_extra_args_for_resume_options() {
        let base = AgentLaunchCommand::default();
        let thread = HistoricalThread {
            session_id: SharedString::from("session-a"),
            title: SharedString::from("title"),
            project_root: PathBuf::from("/root"),
            last_activity_at: SystemTime::UNIX_EPOCH,
        };

        let resumed = CodexHistoryProvider.resume_command(
            &base,
            &thread,
            &["--dangerously-bypass-approvals-and-sandbox".to_string()],
        );

        assert_eq!(
            resumed.args,
            vec![
                "resume",
                "session-a",
                "--dangerously-bypass-approvals-and-sandbox"
            ]
        );
    }
}
