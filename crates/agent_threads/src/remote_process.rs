use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use collections::HashMap;
use uuid::Uuid;

use crate::AgentLaunchCommand;

const LIFECYCLE_ENVIRONMENT_KEY: &str = "FLINT_AGENT_THREAD_ID";
const MAX_CLEANUP_STDERR_BYTES: usize = 64 * 1024;

const LAUNCH_SCRIPT: &str = r#"set -eu
lifecycle_id=$0
state_directory=$HOME/.local/state/flint/agent-threads
record=$state_directory/$lifecycle_id
temporary=$record.$$
umask 077
mkdir -p "$state_directory"
process_group=$(ps -o pgid= -p $$ | tr -d ' ')
if test -r /proc/$$/stat; then
    start_identity=linux:$(awk '{print $22}' /proc/$$/stat)
else
    start_identity=posix:$(ps -o lstart= -p $$)
fi
printf '%s\n%s\n%s\n%s\n' "$lifecycle_id" "$$" "$process_group" "$start_identity" >"$temporary"
mv -f "$temporary" "$record"
cleanup_record() {
    rm -f "$record"
}
trap cleanup_record EXIT
set +e
"$@"
exit_status=$?
set -e
exit "$exit_status"
"#;

const CLEANUP_SCRIPT: &str = r#"set -eu
lifecycle_id=$0
state_directory=$HOME/.local/state/flint/agent-threads
record=$state_directory/$lifecycle_id
test -f "$record" || exit 65
{
    IFS= read -r recorded_id
    IFS= read -r process_id
    IFS= read -r process_group
    IFS= read -r recorded_start
} <"$record"
test "$recorded_id" = "$lifecycle_id" || exit 66
case $process_id:$process_group in
    *[!0-9:]*|:*|*:) exit 66 ;;
esac
if ! kill -0 "$process_id" 2>/dev/null; then
    rm -f "$record"
    exit 0
fi
live_group=$(ps -o pgid= -p "$process_id" | tr -d ' ')
test "$live_group" = "$process_group" || exit 67
case $recorded_start in
    linux:*)
        test -r "/proc/$process_id/stat" || exit 67
        live_start=linux:$(awk '{print $22}' "/proc/$process_id/stat")
        test "$live_start" = "$recorded_start" || exit 67
        tr '\000' '\n' <"/proc/$process_id/environ" |
            grep -Fqx "FLINT_AGENT_THREAD_ID=$lifecycle_id" || exit 67
        ;;
    posix:*)
        live_start=posix:$(ps -o lstart= -p "$process_id")
        test "$live_start" = "$recorded_start" || exit 67
        ps eww -p "$process_id" -o command= |
            grep -Fq "FLINT_AGENT_THREAD_ID=$lifecycle_id" || exit 67
        ;;
    *) exit 66 ;;
esac
/bin/kill -TERM -- "-$process_group" 2>/dev/null ||
    /bin/kill -TERM "$process_id" 2>/dev/null || exit 68
attempt=0
while test "$attempt" -lt 5 && kill -0 "$process_id" 2>/dev/null; do
    sleep 0.1
    attempt=$((attempt + 1))
done
if kill -0 "$process_id" 2>/dev/null; then
    /bin/kill -KILL -- "-$process_group" 2>/dev/null ||
        /bin/kill -KILL "$process_id" 2>/dev/null || exit 68
fi
attempt=0
while test "$attempt" -lt 5 && kill -0 "$process_id" 2>/dev/null; do
    sleep 0.1
    attempt=$((attempt + 1))
done
kill -0 "$process_id" 2>/dev/null && exit 69
rm -f "$record"
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
struct CleanupRequest {
    program: String,
    args: Vec<String>,
    interactive: remote::Interactive,
    connection_sharing: remote::ConnectionSharing,
}

fn canonical_lifecycle_id(value: &str) -> Result<String> {
    let id = Uuid::parse_str(value).map_err(|_| anyhow!("invalid Agent Thread lifecycle ID"))?;
    let canonical = id.hyphenated().to_string();
    if canonical != value {
        return Err(anyhow!("invalid Agent Thread lifecycle ID"));
    }
    Ok(canonical)
}

fn wrap_remote_launch(command: &mut AgentLaunchCommand, lifecycle_id: &str) -> Result<()> {
    let lifecycle_id = canonical_lifecycle_id(lifecycle_id)?;
    let program = command
        .command
        .take()
        .ok_or_else(|| anyhow!("remote Agent Thread command is missing"))?;
    let original_arguments = std::mem::take(&mut command.args);
    command.command = Some("/bin/sh".into());
    command.args = vec![
        "-c".into(),
        LAUNCH_SCRIPT.into(),
        lifecycle_id.clone(),
        program,
    ];
    command.args.extend(original_arguments);
    command
        .env
        .insert(LIFECYCLE_ENVIRONMENT_KEY.into(), lifecycle_id);
    Ok(())
}

fn cleanup_request(lifecycle_id: &str) -> Result<CleanupRequest> {
    let lifecycle_id = canonical_lifecycle_id(lifecycle_id)?;
    Ok(CleanupRequest {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), CLEANUP_SCRIPT.into(), lifecycle_id],
        interactive: remote::Interactive::No,
        connection_sharing: remote::ConnectionSharing::Shared,
    })
}

#[derive(Clone)]
pub(crate) struct RemoteAgentProcess {
    lifecycle_id: String,
    connection: Arc<dyn remote::RemoteConnection>,
}

impl RemoteAgentProcess {
    pub(crate) fn prepare(
        command: &mut AgentLaunchCommand,
        connection: Arc<dyn remote::RemoteConnection>,
        lifecycle_id: Uuid,
    ) -> Result<Self> {
        let lifecycle_id = lifecycle_id.hyphenated().to_string();
        wrap_remote_launch(command, &lifecycle_id)?;
        Ok(Self {
            lifecycle_id,
            connection,
        })
    }

    pub(crate) async fn force_terminate(&self) -> Result<()> {
        let request = cleanup_request(&self.lifecycle_id)?;
        let command = self.connection.build_command(
            Some(request.program),
            &request.args,
            &HashMap::default(),
            None,
            None,
            request.interactive,
            request.connection_sharing,
        )?;
        let output = util::command::new_command(&command.program)
            .args(&command.args)
            .envs(&command.env)
            .output()
            .await
            .context("failed to run remote Agent Thread cleanup")?;
        if !output.status.success() {
            let stderr_length = output.stderr.len().min(MAX_CLEANUP_STDERR_BYTES);
            let stderr = String::from_utf8_lossy(&output.stderr[..stderr_length]);
            return Err(anyhow!(
                "remote Agent Thread cleanup exited with {}: {}",
                output.status,
                stderr.trim()
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShutdownOutcome {
    Graceful,
    Forced,
}

pub(crate) async fn wait_for_graceful_exit_or_force<Completion, Timeout, Force>(
    completion: Completion,
    timeout: Timeout,
    force: Force,
) -> Result<ShutdownOutcome>
where
    Completion: std::future::Future<Output = ()>,
    Timeout: std::future::Future<Output = ()>,
    Force: std::future::Future<Output = Result<()>>,
{
    use futures::FutureExt as _;

    let completion = completion.fuse();
    let timeout = timeout.fuse();
    futures::pin_mut!(completion, timeout);
    futures::select_biased! {
        () = completion => Ok(ShutdownOutcome::Graceful),
        () = timeout => {
            force.await?;
            Ok(ShutdownOutcome::Forced)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentLaunchCommand;
    use collections::HashMap;

    const LIFECYCLE_ID: &str = "7c3630a3-d084-4c8a-bb13-0adf73936942";

    #[test]
    fn remote_launch_uses_fixed_wrapper_and_preserves_agent_argv() {
        let mut command = AgentLaunchCommand {
            command: Some("/opt/codex path/codex".into()),
            args: vec!["resume".into(), "session with spaces".into()],
            env: HashMap::default(),
            ..AgentLaunchCommand::default()
        };

        wrap_remote_launch(&mut command, LIFECYCLE_ID).expect("valid command");

        assert_eq!(command.command.as_deref(), Some("/bin/sh"));
        assert_eq!(command.args[0], "-c");
        assert_eq!(command.args[2], LIFECYCLE_ID);
        assert_eq!(command.args[3], "/opt/codex path/codex");
        assert_eq!(command.args[4], "resume");
        assert_eq!(command.args[5], "session with spaces");
        assert_eq!(
            command
                .env
                .get("FLINT_AGENT_THREAD_ID")
                .map(String::as_str),
            Some(LIFECYCLE_ID)
        );
        assert!(!command.args[1].contains("/opt/codex path/codex"));
    }

    #[test]
    fn lifecycle_id_must_be_a_canonical_uuid() {
        let mut command = AgentLaunchCommand {
            command: Some("codex".into()),
            ..AgentLaunchCommand::default()
        };

        let error = wrap_remote_launch(&mut command, "../../other-process")
            .expect_err("unsafe lifecycle id must fail");

        assert_eq!(error.to_string(), "invalid Agent Thread lifecycle ID");
    }

    #[test]
    fn cleanup_command_is_non_interactive_and_shared() {
        let request = cleanup_request(LIFECYCLE_ID).expect("valid lifecycle id");

        assert_eq!(request.program, "/bin/sh");
        assert_eq!(request.args[0], "-c");
        assert_eq!(request.args[2], LIFECYCLE_ID);
        assert_eq!(request.interactive, remote::Interactive::No);
        assert_eq!(
            request.connection_sharing,
            remote::ConnectionSharing::Shared
        );
    }

    #[gpui::test]
    async fn graceful_completion_skips_force_cleanup(cx: &mut gpui::TestAppContext) {
        let forced = std::rc::Rc::new(std::cell::Cell::new(false));
        let outcome = wait_for_graceful_exit_or_force(
            futures::future::ready(()),
            cx.background_executor
                .timer(std::time::Duration::from_secs(60)),
            {
                let forced = forced.clone();
                async move {
                    forced.set(true);
                    anyhow::Ok(())
                }
            },
        )
        .await
        .expect("graceful shutdown");

        assert_eq!(outcome, ShutdownOutcome::Graceful);
        assert!(!forced.get());
    }

    #[gpui::test]
    async fn timeout_runs_force_cleanup_once(cx: &mut gpui::TestAppContext) {
        let force_count = std::rc::Rc::new(std::cell::Cell::new(0));
        let outcome = wait_for_graceful_exit_or_force(
            futures::future::pending::<()>(),
            cx.background_executor
                .timer(std::time::Duration::from_millis(1)),
            {
                let force_count = force_count.clone();
                async move {
                    force_count.set(force_count.get() + 1);
                    anyhow::Ok(())
                }
            },
        )
        .await
        .expect("forced shutdown");

        assert_eq!(outcome, ShutdownOutcome::Forced);
        assert_eq!(force_count.get(), 1);
    }
}
