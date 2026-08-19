use crate::AgentLaunchCommand;
use crate::history::{AgentHistoryProvider, HistoricalThread};

pub struct OpenCodeHistoryProvider;

impl AgentHistoryProvider for OpenCodeHistoryProvider {
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
