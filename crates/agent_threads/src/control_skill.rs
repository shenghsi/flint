use agent_control_skill::{SkillEnvironment, SkillState};
use gpui::{Context, SharedString};
use workspace::notifications::NotificationId;
use workspace::{Toast, Workspace};

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

enum SkillSyncFailure {}

#[cfg(test)]
mod tests {
    use std::fs;

    use agent_control_skill::SkillEnvironment;
    use tempfile::TempDir;

    use super::synchronize_control_skills_in;

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
}
