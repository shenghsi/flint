use crate::{
    artifact_cache::AgentArtifactDownloadProgress, managed_agent::ManagedAgentInstallPhase,
};
use collections::HashMap;
use gpui::{
    App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, Render, SharedString,
};
use remote::{RemoteConnectionIdentity, RemotePlatform};
use std::{cell::Cell, rc::Rc};
use ui::{ProgressBar, SpinnerLabel, prelude::*};
use workspace::{
    NotificationFrame,
    notifications::{Notification, SuppressEvent},
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ManagedAgentProvisioningKey {
    pub remote_identity: RemoteConnectionIdentity,
    pub agent_id: &'static str,
    pub version: &'static str,
    pub platform: RemotePlatform,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManagedAgentProvisioningOwner {
    key: ManagedAgentProvisioningKey,
    token: uuid::Uuid,
}

pub(crate) struct ManagedAgentProvisioningCoordinator<T> {
    active: HashMap<ManagedAgentProvisioningKey, (uuid::Uuid, T)>,
}

impl<T> Default for ManagedAgentProvisioningCoordinator<T> {
    fn default() -> Self {
        Self {
            active: HashMap::default(),
        }
    }
}

impl<T: Clone> ManagedAgentProvisioningCoordinator<T> {
    pub fn begin(
        &mut self,
        key: ManagedAgentProvisioningKey,
        value: T,
    ) -> Result<ManagedAgentProvisioningOwner, T> {
        if let Some((_, active)) = self.active.get(&key) {
            return Err(active.clone());
        }
        let token = uuid::Uuid::new_v4();
        self.active.insert(key.clone(), (token, value));
        Ok(ManagedAgentProvisioningOwner { key, token })
    }

    #[cfg(test)]
    pub fn contains(&self, key: &ManagedAgentProvisioningKey) -> bool {
        self.active.contains_key(key)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.active.len()
    }

    pub fn finish(&mut self, owner: ManagedAgentProvisioningOwner) -> Option<T> {
        let (token, _) = self.active.get(&owner.key)?;
        if token != &owner.token {
            return None;
        }
        self.active.remove(&owner.key).map(|(_, value)| value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ManagedAgentProgressState {
    CheckingInstalled,
    Reusing,
    CheckingCache,
    AwaitingConfirmation,
    Downloading {
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
    },
    Verifying,
    VerifyingUploaded,
    Uploading,
    Installing,
    Launching,
    Resuming,
}

pub(crate) enum ManagedAgentProgressEvent {
    Download(AgentArtifactDownloadProgress),
    Install(ManagedAgentInstallPhase),
}

#[derive(Clone)]
pub(crate) struct ManagedAgentProgressReporter {
    sender: futures::channel::mpsc::UnboundedSender<ManagedAgentProgressEvent>,
    disconnected: Rc<Cell<bool>>,
}

impl ManagedAgentProgressReporter {
    pub fn channel() -> (
        Self,
        futures::channel::mpsc::UnboundedReceiver<ManagedAgentProgressEvent>,
    ) {
        let (sender, receiver) = futures::channel::mpsc::unbounded();
        (
            Self {
                sender,
                disconnected: Rc::new(Cell::new(false)),
            },
            receiver,
        )
    }

    pub fn report(&self, event: ManagedAgentProgressEvent) {
        if self.disconnected.get() {
            return;
        }
        if self.sender.unbounded_send(event).is_err() {
            self.disconnected.set(true);
            log::debug!("managed agent progress receiver closed");
        }
    }
}

impl ManagedAgentProgressState {
    pub fn update_download(&mut self, downloaded_bytes: u64, total_bytes: Option<u64>) {
        let Self::Downloading {
            downloaded_bytes: current_downloaded_bytes,
            total_bytes: current_total_bytes,
        } = self
        else {
            return;
        };
        *current_downloaded_bytes = (*current_downloaded_bytes).max(downloaded_bytes);
        if current_total_bytes.is_none() {
            *current_total_bytes = total_bytes;
        }
    }

    #[cfg(test)]
    pub fn downloaded_bytes(&self) -> Option<u64> {
        match self {
            Self::Downloading {
                downloaded_bytes, ..
            } => Some(*downloaded_bytes),
            _ => None,
        }
    }

    pub fn percentage(&self) -> Option<u64> {
        let Self::Downloading {
            downloaded_bytes,
            total_bytes: Some(total_bytes),
        } = self
        else {
            return None;
        };
        if *total_bytes == 0 {
            return Some(100);
        }
        Some((((*downloaded_bytes as u128 * 100) / *total_bytes as u128).min(100)) as u64)
    }

    pub fn detail(&self) -> String {
        match self {
            Self::Downloading {
                downloaded_bytes,
                total_bytes: Some(total_bytes),
            } => format!(
                "{}% · {} / {}",
                self.percentage().unwrap_or(0),
                format_bytes(*downloaded_bytes),
                format_bytes(*total_bytes)
            ),
            Self::Downloading {
                downloaded_bytes,
                total_bytes: None,
            } => format!("{} downloaded", format_bytes(*downloaded_bytes)),
            Self::CheckingInstalled => "Checking the installed remote CLI".to_string(),
            Self::Reusing => "Reusing the installed remote CLI".to_string(),
            Self::CheckingCache => "Checking the local artifact cache".to_string(),
            Self::AwaitingConfirmation => "Waiting for download confirmation".to_string(),
            Self::Verifying => "Verifying the official CLI".to_string(),
            Self::VerifyingUploaded => "Verifying the uploaded remote CLI".to_string(),
            Self::Uploading => "Uploading the CLI to the remote host".to_string(),
            Self::Installing => "Installing and verifying the remote CLI".to_string(),
            Self::Launching => "Launching the managed Agent Thread".to_string(),
            Self::Resuming => "Launching the managed CLI for this session".to_string(),
        }
    }
}

fn format_bytes(bytes: u64) -> String {
    const KILOBYTE: f64 = 1_000.0;
    const MEGABYTE: f64 = 1_000_000.0;
    const GIGABYTE: f64 = 1_000_000_000.0;

    let bytes = bytes as f64;
    if bytes >= GIGABYTE {
        format!("{:.1} GB", bytes / GIGABYTE)
    } else if bytes >= MEGABYTE {
        format!("{:.1} MB", bytes / MEGABYTE)
    } else if bytes >= KILOBYTE {
        format!("{:.1} KB", bytes / KILOBYTE)
    } else {
        format!("{bytes:.0} B")
    }
}

pub(crate) struct ManagedAgentProgressNotification {
    agent_label: SharedString,
    version: SharedString,
    state: ManagedAgentProgressState,
    focus_handle: FocusHandle,
}

impl ManagedAgentProgressNotification {
    pub fn new(
        agent_label: impl Into<SharedString>,
        version: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            agent_label: agent_label.into(),
            version: version.into(),
            state: ManagedAgentProgressState::CheckingInstalled,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn set_state(&mut self, state: ManagedAgentProgressState, cx: &mut Context<Self>) {
        self.state = state;
        cx.notify();
    }

    #[cfg(test)]
    pub fn state(&self) -> &ManagedAgentProgressState {
        &self.state
    }

    pub fn apply_event(&mut self, event: ManagedAgentProgressEvent, cx: &mut Context<Self>) {
        match event {
            ManagedAgentProgressEvent::Download(progress) if progress.complete => {
                self.state
                    .update_download(progress.downloaded_bytes, progress.total_bytes);
                self.state = ManagedAgentProgressState::Verifying;
            }
            ManagedAgentProgressEvent::Download(progress) => {
                if !matches!(self.state, ManagedAgentProgressState::Downloading { .. }) {
                    self.state = ManagedAgentProgressState::Downloading {
                        downloaded_bytes: 0,
                        total_bytes: progress.total_bytes,
                    };
                }
                self.state
                    .update_download(progress.downloaded_bytes, progress.total_bytes);
            }
            ManagedAgentProgressEvent::Install(ManagedAgentInstallPhase::CheckingRemote) => {
                self.state = ManagedAgentProgressState::CheckingInstalled;
            }
            ManagedAgentProgressEvent::Install(ManagedAgentInstallPhase::Reusing) => {
                self.state = ManagedAgentProgressState::Reusing;
            }
            ManagedAgentProgressEvent::Install(ManagedAgentInstallPhase::Uploading) => {
                self.state = ManagedAgentProgressState::Uploading;
            }
            ManagedAgentProgressEvent::Install(ManagedAgentInstallPhase::VerifyingRemote) => {
                self.state = ManagedAgentProgressState::VerifyingUploaded;
            }
            ManagedAgentProgressEvent::Install(ManagedAgentInstallPhase::Installing) => {
                self.state = ManagedAgentProgressState::Installing;
            }
        }
        cx.notify();
    }

    pub fn headline(&self) -> String {
        match self.state {
            ManagedAgentProgressState::CheckingInstalled => {
                format!("Checking installed {} CLI", self.agent_label)
            }
            ManagedAgentProgressState::Reusing => {
                format!("Reusing installed {} CLI", self.agent_label)
            }
            ManagedAgentProgressState::CheckingCache => {
                format!("Preparing Flint-managed {} CLI", self.agent_label)
            }
            ManagedAgentProgressState::AwaitingConfirmation => format!(
                "Waiting to download official {} CLI v{}",
                self.agent_label, self.version
            ),
            ManagedAgentProgressState::Downloading { .. } => format!(
                "Downloading official {} CLI v{}",
                self.agent_label, self.version
            ),
            ManagedAgentProgressState::Verifying => {
                format!("Verifying official {} CLI", self.agent_label)
            }
            ManagedAgentProgressState::VerifyingUploaded => {
                format!("Verifying uploaded {} CLI", self.agent_label)
            }
            ManagedAgentProgressState::Uploading => {
                format!("Uploading {} CLI to remote", self.agent_label)
            }
            ManagedAgentProgressState::Installing => {
                format!("Installing {} CLI on remote", self.agent_label)
            }
            ManagedAgentProgressState::Launching => {
                format!("Launching Flint-managed {}", self.agent_label)
            }
            ManagedAgentProgressState::Resuming => {
                format!("Resuming {} session", self.agent_label)
            }
        }
    }
}

impl Render for ManagedAgentProgressNotification {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let detail = self.state.detail();
        let content = v_flex()
            .w_96()
            .gap_2()
            .child(Label::new(self.headline()))
            .child(match self.state {
                ManagedAgentProgressState::Downloading {
                    downloaded_bytes,
                    total_bytes: Some(total_bytes),
                } => v_flex()
                    .gap_1()
                    .child(
                        ProgressBar::new(
                            "managed-agent-download-progress",
                            downloaded_bytes as f32,
                            total_bytes.max(1) as f32,
                            cx,
                        )
                        .fg_color(cx.theme().status().info),
                    )
                    .child(
                        Label::new(detail)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .into_any_element(),
                ManagedAgentProgressState::Downloading {
                    total_bytes: None, ..
                }
                | ManagedAgentProgressState::CheckingInstalled
                | ManagedAgentProgressState::Reusing
                | ManagedAgentProgressState::CheckingCache
                | ManagedAgentProgressState::AwaitingConfirmation
                | ManagedAgentProgressState::Verifying
                | ManagedAgentProgressState::VerifyingUploaded
                | ManagedAgentProgressState::Uploading
                | ManagedAgentProgressState::Installing
                | ManagedAgentProgressState::Launching
                | ManagedAgentProgressState::Resuming => h_flex()
                    .gap_2()
                    .child(SpinnerLabel::new().size(LabelSize::Small))
                    .child(
                        Label::new(detail)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .into_any_element(),
            });

        NotificationFrame::new()
            .with_title(Some(format!("Flint-managed {}", self.agent_label)))
            .show_close_button(false)
            .show_suppress_button(false)
            .with_content(content)
    }
}

impl Focusable for ManagedAgentProgressNotification {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for ManagedAgentProgressNotification {}
impl EventEmitter<SuppressEvent> for ManagedAgentProgressNotification {}
impl Notification for ManagedAgentProgressNotification {}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use remote::{RemoteArch, RemoteConnectionIdentity, RemoteLibc, RemoteOs, RemotePlatform};

    fn key(host: &str) -> ManagedAgentProvisioningKey {
        ManagedAgentProvisioningKey {
            remote_identity: RemoteConnectionIdentity::Ssh {
                host: host.to_string(),
                username: Some("developer".to_string()),
                port: Some(22),
            },
            agent_id: "codex",
            version: "0.144.6",
            platform: RemotePlatform {
                os: RemoteOs::Linux,
                arch: RemoteArch::X86_64,
                libc: Some(RemoteLibc::Glibc),
            },
        }
    }

    #[test]
    fn repeated_begin_returns_the_existing_operation() {
        let mut coordinator = ManagedAgentProvisioningCoordinator::default();
        let key = key("build.example.com");

        let owner = coordinator
            .begin(key.clone(), "first")
            .expect("first operation should own the reservation");
        let existing = coordinator
            .begin(key.clone(), "second")
            .expect_err("second operation should reuse the reservation");

        assert_eq!(existing, "first");
        assert_eq!(coordinator.finish(owner), Some("first"));
        assert!(!coordinator.contains(&key));
    }

    #[test]
    fn stale_owner_cannot_finish_a_retried_operation() {
        let mut coordinator = ManagedAgentProvisioningCoordinator::default();
        let key = key("build.example.com");
        let first = coordinator.begin(key.clone(), "first").unwrap();
        assert_eq!(coordinator.finish(first.clone()), Some("first"));
        let second = coordinator.begin(key.clone(), "second").unwrap();

        assert_eq!(coordinator.finish(first), None);
        assert!(coordinator.contains(&key));
        assert_eq!(coordinator.finish(second), Some("second"));
    }

    #[test]
    fn separate_remotes_can_provision_independently() {
        let mut coordinator = ManagedAgentProvisioningCoordinator::default();

        coordinator.begin(key("one.example.com"), "one").unwrap();
        coordinator.begin(key("two.example.com"), "two").unwrap();

        assert_eq!(coordinator.len(), 2);
    }

    #[test]
    fn download_progress_is_monotonic_and_calculates_percentage() {
        let mut state = ManagedAgentProgressState::Downloading {
            downloaded_bytes: 20,
            total_bytes: Some(100),
        };

        state.update_download(10, Some(100));
        assert_eq!(state.downloaded_bytes(), Some(20));
        state.update_download(63, Some(100));

        assert_eq!(state.downloaded_bytes(), Some(63));
        assert_eq!(state.percentage(), Some(63));
    }

    #[test]
    fn unknown_download_length_has_no_percentage() {
        let state = ManagedAgentProgressState::Downloading {
            downloaded_bytes: 12_400_000,
            total_bytes: None,
        };

        assert_eq!(state.percentage(), None);
        assert_eq!(state.detail(), "12.4 MB downloaded");
    }

    #[test]
    fn known_download_length_formats_progress_detail() {
        let state = ManagedAgentProgressState::Downloading {
            downloaded_bytes: 18_400_000,
            total_bytes: Some(49_700_000),
        };

        assert_eq!(state.detail(), "37% · 18.4 MB / 49.7 MB");
    }

    #[gpui::test]
    fn progress_notification_exposes_the_current_download(cx: &mut TestAppContext) {
        let notification =
            cx.new(|cx| ManagedAgentProgressNotification::new("Codex", "0.144.6", cx));

        notification.update(cx, |notification, cx| {
            notification.set_state(
                ManagedAgentProgressState::Downloading {
                    downloaded_bytes: 18_400_000,
                    total_bytes: Some(49_700_000),
                },
                cx,
            );
        });

        assert_eq!(
            notification.read_with(cx, |notification, _| notification.headline()),
            "Downloading official Codex CLI v0.144.6"
        );
        assert_eq!(
            notification.read_with(cx, |notification, _| notification.state().detail()),
            "37% · 18.4 MB / 49.7 MB"
        );
    }

    #[gpui::test]
    fn remote_verification_has_distinct_installed_cli_copy(cx: &mut TestAppContext) {
        let notification =
            cx.new(|cx| ManagedAgentProgressNotification::new("Codex", "0.144.6", cx));

        notification.update(cx, |notification, cx| {
            notification.apply_event(
                ManagedAgentProgressEvent::Install(ManagedAgentInstallPhase::VerifyingRemote),
                cx,
            );
        });

        assert_eq!(
            notification.read_with(cx, |notification, _| notification.headline()),
            "Verifying uploaded Codex CLI"
        );
        assert_eq!(
            notification.read_with(cx, |notification, _| notification.state().detail()),
            "Verifying the uploaded remote CLI"
        );
    }

    #[gpui::test]
    fn installed_cli_check_and_reuse_have_distinct_copy(cx: &mut TestAppContext) {
        let notification =
            cx.new(|cx| ManagedAgentProgressNotification::new("Codex", "0.144.6", cx));

        assert_eq!(
            notification.read_with(cx, |notification, _| notification.headline()),
            "Checking installed Codex CLI"
        );
        assert_eq!(
            notification.read_with(cx, |notification, _| notification.state().detail()),
            "Checking the installed remote CLI"
        );

        notification.update(cx, |notification, cx| {
            notification.apply_event(
                ManagedAgentProgressEvent::Install(ManagedAgentInstallPhase::Reusing),
                cx,
            );
        });

        assert_eq!(
            notification.read_with(cx, |notification, _| notification.headline()),
            "Reusing installed Codex CLI"
        );
        assert_eq!(
            notification.read_with(cx, |notification, _| notification.state().detail()),
            "Reusing the installed remote CLI"
        );
    }

    #[gpui::test]
    fn resume_has_distinct_session_copy(cx: &mut TestAppContext) {
        let notification =
            cx.new(|cx| ManagedAgentProgressNotification::new("Codex", "0.144.6", cx));

        notification.update(cx, |notification, cx| {
            notification.set_state(ManagedAgentProgressState::Resuming, cx);
        });

        assert_eq!(
            notification.read_with(cx, |notification, _| notification.headline()),
            "Resuming Codex session"
        );
        assert_eq!(
            notification.read_with(cx, |notification, _| notification.state().detail()),
            "Launching the managed CLI for this session"
        );
    }
}
