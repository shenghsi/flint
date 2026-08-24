use std::sync::atomic::{AtomicBool, Ordering};

use agent_control_skill::{SkillEnvironment, SkillState};
use db::kvp::KeyValueStore;
use gpui::{Action, AppContext, Context, PromptButton, PromptLevel, SharedString, TaskExt, Window};
use workspace::notifications::NotificationId;
use workspace::{Toast, Workspace};

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
    _workspace: &mut Workspace,
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

    let prompt = window.prompt(
        PromptLevel::Info,
        &localization::text(cx, "agent-threads-control-skill-reminder"),
        Some(&localization::text(
            cx,
            "agent-threads-control-skill-reminder-detail",
        )),
        &[
            PromptButton::new(localization::text(cx, "agent-threads-control-skill-manage")),
            PromptButton::cancel(localization::text(
                cx,
                "agent-threads-control-skill-not-now",
            )),
        ],
        cx,
    );
    cx.spawn_in(window, async move |_workspace, cx| {
        if matches!(prompt.await.ok(), Some(0)) {
            cx.update(|window, cx| {
                window.dispatch_action(flint_actions::ManageAgentControlSkill.boxed_clone(), cx);
            })?;
        }
        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
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

#[cfg(test)]
mod tests {
    use std::fs;

    use agent_control_skill::SkillEnvironment;
    use tempfile::TempDir;

    use super::{should_offer_install_reminder, synchronize_control_skills_in};

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
}
