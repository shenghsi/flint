use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use agent_history::HistoryProvider as _;
use anyhow::{Context as _, Result};
use async_trait::async_trait;
use gpui::SharedString;

use crate::AgentLaunchCommand;
use crate::history::{
    AgentHistoryHost, AgentHistoryProvider, HistoricalThread, HistoryFileIdentity, HistoryFs,
    paths_equal_for_style,
};

pub struct OpenCodeHistoryProvider;

struct IndexedHistoryFs(Arc<dyn HistoryFs>);

#[async_trait]
impl agent_history::HistoryFs for IndexedHistoryFs {
    async fn read_dir(&self, path: &std::path::Path) -> Result<Vec<PathBuf>> {
        self.0.read_dir(path).await
    }

    async fn load(&self, path: &std::path::Path) -> Result<String> {
        self.0.load(path).await
    }

    async fn metadata(
        &self,
        path: &std::path::Path,
    ) -> Result<Option<agent_history::FileIdentity>> {
        self.0
            .metadata(path)
            .await?
            .map(indexed_file_identity)
            .transpose()
    }

    fn local_path(&self, path: &std::path::Path) -> Option<PathBuf> {
        self.0.local_path(path)
    }
}

fn indexed_file_identity(identity: HistoryFileIdentity) -> Result<agent_history::FileIdentity> {
    let (modified_at_secs, modified_at_nanos) = identity
        .modified_at
        .to_seconds_and_nanos_for_persistence()
        .context("OpenCode history timestamp is before the Unix epoch")?;
    Ok(agent_history::FileIdentity {
        modified_at_secs,
        modified_at_nanos,
        length: identity.length,
    })
}

#[async_trait]
impl AgentHistoryProvider for OpenCodeHistoryProvider {
    async fn scan(
        &self,
        host: &AgentHistoryHost,
        project_roots: &[PathBuf],
    ) -> Result<Vec<HistoricalThread>> {
        let indexed_host = agent_history::HistoryHost {
            fs: Arc::new(IndexedHistoryFs(host.fs.clone())),
            base_dir: host.base_dir.clone(),
            path_style: host.path_style,
        };
        let refresh = agent_history::OpenCodeHistoryProvider
            .refresh(&indexed_host, None)
            .await?;
        let mut threads = refresh
            .sessions
            .into_iter()
            .filter(|session| {
                let session_root = PathBuf::from(&session.working_dir);
                project_roots
                    .iter()
                    .any(|root| paths_equal_for_style(root, &session_root, host.path_style))
            })
            .map(|session| HistoricalThread {
                session_id: SharedString::from(session.session_id),
                title: SharedString::from(session.resolved_title),
                project_root: PathBuf::from(session.working_dir),
                last_activity_at: UNIX_EPOCH
                    + Duration::new(session.last_activity_secs, session.last_activity_nanos),
            })
            .collect::<Vec<_>>();
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.last_activity_at));
        Ok(threads)
    }

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
