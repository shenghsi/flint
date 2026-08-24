use std::sync::atomic::{AtomicBool, Ordering};

use agent_control_skill::{SkillEnvironment, SkillState};
use db::kvp::KeyValueStore;
use gpui::{
    App, AppContext, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, Render,
    SharedString, TaskExt, Window,
};
use ui::{AlertModal, Button, ButtonStyle, Checkbox, ToggleState, prelude::*};
use workspace::notifications::NotificationId;
use workspace::{ModalView, Toast, Workspace};

const INSTALL_REMINDER_SHOWN_KEY: &str = "agent-threads-control-skill-install-reminder-shown";
static INSTALL_REMINDER_STARTED: AtomicBool = AtomicBool::new(false);

pub(crate) fn synchronize_control_skills() -> Vec<String> {
    synchronize_control_skills_in(&SkillEnvironment::current())
}

fn synchronize_control_skills_in(environment: &SkillEnvironment) -> Vec<String> {
    match agent_control_skill::synchronize(environment) {
        Ok(outcomes) => outcomes
            .into_iter()
            .filter_map(|outcome| match outcome.state {
                SkillState::Modified => Some(format!(
                    "The Flint control skill for {} was modified and was not updated",
                    outcome.agent.label()
                )),
                SkillState::Missing => Some(format!(
                    "The Flint control skill for {} is missing; reinstall it or remove its ownership record",
                    outcome.agent.label()
                )),
                SkillState::NotInstalled
                | SkillState::Unowned
                | SkillState::InstalledCurrent
                | SkillState::InstalledOutdated => None,
            })
            .collect(),
        Err(error) => vec![format!("Could not update Flint control skills: {error:#}")],
    }
}

pub(crate) fn show_sync_errors(
    errors: &[String],
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) {
    for error in errors {
        workspace.show_toast(
            Toast::new(
                NotificationId::composite::<SkillSyncFailure>(SharedString::from(error.clone())),
                error.clone(),
            ),
            cx,
        );
    }
}

pub(crate) fn show_install_reminder(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let environment = SkillEnvironment::current();
    if !should_offer_install_reminder(&environment)
        || !matches!(
            KeyValueStore::global(cx).read_kvp(INSTALL_REMINDER_SHOWN_KEY),
            Ok(None)
        )
        || INSTALL_REMINDER_STARTED.swap(true, Ordering::AcqRel)
    {
        return;
    }

    let key_value_store = KeyValueStore::global(cx);
    cx.background_spawn(async move {
        key_value_store
            .write_kvp(INSTALL_REMINDER_SHOWN_KEY.to_string(), "shown".to_string())
            .await
    })
    .detach_and_log_err(cx);

    let selected_agents = install_reminder_agents(&environment);
    let workspace_handle = cx.weak_entity();
    workspace.toggle_modal(window, cx, move |_, cx| {
        ControlSkillInstallModal::new(selected_agents, workspace_handle, cx)
    });
}

fn install_reminder_agents(environment: &SkillEnvironment) -> Vec<agent_control_skill::AgentKind> {
    agent_control_skill::AgentKind::ALL
        .into_iter()
        .filter(|agent| {
            matches!(
                agent_control_skill::status(*agent, environment),
                Ok(SkillState::NotInstalled)
            )
        })
        .collect()
}

struct ControlSkillInstallModal {
    selected_agents: Vec<agent_control_skill::AgentKind>,
    workspace: gpui::WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    error: Option<SharedString>,
}

impl ControlSkillInstallModal {
    fn new(
        selected_agents: Vec<agent_control_skill::AgentKind>,
        workspace: gpui::WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            selected_agents,
            workspace,
            focus_handle: cx.focus_handle(),
            error: None,
        }
    }

    fn install(&mut self, cx: &mut Context<Self>) {
        let environment = SkillEnvironment::current();
        for agent in self.selected_agents.iter().copied() {
            if let Err(error) = agent_control_skill::install(agent, &environment, false) {
                self.error = Some(
                    format!(
                        "Could not install the skill for {}: {error:#}",
                        agent.label()
                    )
                    .into(),
                );
                cx.notify();
                return;
            }
        }

        let count = self.selected_agents.len();
        if let Err(error) = self.workspace.update(cx, |workspace, cx| {
            workspace.show_toast(
                Toast::new(
                    NotificationId::unique::<ControlSkillInstallReminder>(),
                    format!("Installed the Flint control skill for {count} agent(s)"),
                )
                .autohide(),
                cx,
            );
        }) {
            log::error!("show control skill installation result: {error:#}");
        }
        cx.emit(DismissEvent);
    }
}

impl Focusable for ControlSkillInstallModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for ControlSkillInstallModal {}
impl ModalView for ControlSkillInstallModal {}

impl Render for ControlSkillInstallModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        AlertModal::new("control-skill-install-reminder")
            .title(localization::text(
                cx,
                "agent-threads-control-skill-reminder",
            ))
            .width(rems(32.))
            .key_context("ControlSkillInstallReminder")
            .track_focus(&self.focus_handle)
            .child(localization::text(
                cx,
                "agent-threads-control-skill-reminder-detail",
            ))
            .child(
                v_flex()
                    .gap_2()
                    .children(
                        agent_control_skill::AgentKind::ALL
                            .into_iter()
                            .map(|agent| {
                                Checkbox::new(
                                    format!("control-skill-agent-{}", agent.id()),
                                    ToggleState::from(self.selected_agents.contains(&agent)),
                                )
                                .label(agent.label())
                                .on_click(cx.listener(
                                    move |this, state: &ToggleState, _, cx| {
                                        if state.selected() {
                                            if !this.selected_agents.contains(&agent) {
                                                this.selected_agents.push(agent);
                                            }
                                        } else {
                                            this.selected_agents
                                                .retain(|selected| *selected != agent);
                                        }
                                        cx.notify();
                                    },
                                ))
                            }),
                    ),
            )
            .when_some(self.error.clone(), |modal, error| {
                modal.child(Label::new(error).color(ui::Color::Error))
            })
            .footer(
                h_flex()
                    .p_3()
                    .gap_1()
                    .justify_end()
                    .child(
                        Button::new(
                            "control-skill-not-now",
                            localization::text(cx, "agent-threads-control-skill-not-now"),
                        )
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        Button::new(
                            "control-skill-install-selected",
                            localization::text(cx, "agent-threads-control-skill-install-selected"),
                        )
                        .style(ButtonStyle::Filled)
                        .disabled(self.selected_agents.is_empty())
                        .on_click(cx.listener(|this, _, _, cx| this.install(cx))),
                    ),
            )
    }
}

fn should_offer_install_reminder(environment: &SkillEnvironment) -> bool {
    [
        agent_control_skill::AgentKind::Codex,
        agent_control_skill::AgentKind::Claude,
    ]
    .into_iter()
    .any(|agent| {
        matches!(
            agent_control_skill::status(agent, environment),
            Ok(SkillState::NotInstalled)
        )
    })
}

enum SkillSyncFailure {}
enum ControlSkillInstallReminder {}

#[cfg(test)]
mod tests {
    use std::fs;

    use agent_control_skill::SkillEnvironment;
    use tempfile::TempDir;

    use super::{
        install_reminder_agents, should_offer_install_reminder, synchronize_control_skills_in,
    };

    #[test]
    fn startup_does_not_change_general_instruction_files_without_an_owned_skill() {
        let temporary_directory = TempDir::new().expect("create temporary directory");
        let home_directory = temporary_directory.path().join("home");
        let data_directory = temporary_directory.path().join("data");
        let codex_instructions = home_directory.join(".codex/AGENTS.md");
        fs::create_dir_all(codex_instructions.parent().expect("instructions parent"))
            .expect("create instructions parent");
        fs::write(&codex_instructions, "user instructions\n").expect("write instructions");
        let environment = SkillEnvironment::new(home_directory, data_directory);

        assert!(synchronize_control_skills_in(&environment).is_empty());
        assert_eq!(
            fs::read_to_string(codex_instructions).expect("read instructions"),
            "user instructions\n"
        );
    }

    #[test]
    fn install_reminder_is_offered_when_a_supported_skill_is_not_installed() {
        let temporary_directory = TempDir::new().expect("create temporary directory");
        let environment = SkillEnvironment::new(
            temporary_directory.path().join("home"),
            temporary_directory.path().join("data"),
        );

        assert!(should_offer_install_reminder(&environment));

        agent_control_skill::install(agent_control_skill::AgentKind::Codex, &environment, false)
            .expect("install Codex skill");
        assert!(should_offer_install_reminder(&environment));

        agent_control_skill::install(agent_control_skill::AgentKind::Claude, &environment, false)
            .expect("install Claude skill");
        assert!(!should_offer_install_reminder(&environment));
    }

    #[test]
    fn install_reminder_selects_only_agents_without_the_skill() {
        let temporary_directory = TempDir::new().expect("create temporary directory");
        let environment = SkillEnvironment::new(
            temporary_directory.path().join("home"),
            temporary_directory.path().join("data"),
        );
        agent_control_skill::install(agent_control_skill::AgentKind::Codex, &environment, false)
            .expect("install Codex skill");

        assert_eq!(
            install_reminder_agents(&environment),
            vec![agent_control_skill::AgentKind::Claude]
        );
    }
}
