//! Nudges the user to add Flint's "Worktree creation" instructions to a
//! CLI's own global instructions file (e.g. `~/.codex/AGENTS.md`,
//! `~/.claude/CLAUDE.md`). Without that section, an agent has no way to
//! discover the `flint-agent-control` command at all, so agent-initiated
//! worktree creation (`crate::control`) silently never gets used even when
//! `agent_threads.agent_control` is on.
//!
//! Unix only, matching `crate::control`'s own gating: the feature this
//! nudges toward doesn't exist elsewhere.

use std::path::{Path, PathBuf};

use gpui::{Context, SharedString};
use settings::Settings as _;
use util::ResultExt as _;
use workspace::notifications::NotificationId;
use workspace::{Toast, Workspace};

use crate::AgentKindDefinition;

/// The exact block Flint offers to append. `already_has_instructions` also
/// just checks for `DETECTION_MARKER` (a substring of this), so a file a
/// user (or an earlier Flint version) already has this in is recognized
/// as-is -- no separate sentinel comment needed.
const WORKTREE_INSTRUCTIONS_BLOCK: &str = "\
# Worktree creation

Check for a Flint agent-control marker file first, with this command:

    find ~/Library/Application\\ Support/Flint ~/.local/share/flint -maxdepth 1 -name \"agent-control-*-executable.json\" -exec cat {} \\; 2>/dev/null

If this command shows no output, skip the rest of this section. Flint is not
tracking this terminal.

If this command shows output, it is JSON with a field named \"executable\".
If you create a new git worktree, run this command right after, using the
value of \"executable\" as the program to run:

    \"<executable>\" retie-thread --worktree <path>

Replace <executable> with the \"executable\" value from the file, and <path>
with the full path of the new worktree. This command tells Flint to move
this thread to the new worktree in its Agent Threads panel. The command
finds its own connection to Flint on its own; you do not need to read or
pass anything else from the file.
";

const DETECTION_MARKER: &str = "retie-thread --worktree";

/// Namespace for the "user has already been offered/shown this" key-value
/// store, keyed by agent kind id. Presence of a key (any value) means don't
/// show the nudge again -- shown once, not "acted on": the toast's own
/// close button has no dedicated dismiss hook to persist against, so we
/// record this at show time rather than only on an explicit click.
const DISMISSED_NAMESPACE: &str = "agent-thread-worktree-instructions-dismissed";

/// `None` for a kind whose global-instructions-file convention isn't
/// confirmed -- guessing a wrong path risks creating a file the CLI never
/// reads, which is worse than not offering at all. OpenCode's and Pi's
/// paths are confirmed against their own docs (opencode.ai/docs/rules,
/// github.com/earendil-works/pi's usage.md), not guessed from their
/// `home_dir_name`.
fn global_instructions_path(kind_id: &str) -> Option<PathBuf> {
    match kind_id {
        "codex" => Some(paths::home_dir().join(".codex").join("AGENTS.md")),
        "claude" => Some(paths::home_dir().join(".claude").join("CLAUDE.md")),
        "opencode" => Some(paths::home_dir().join(".config/opencode").join("AGENTS.md")),
        "pi" => Some(paths::home_dir().join(".pi/agent").join("AGENTS.md")),
        _ => None,
    }
}

/// Best-effort signal that this kind's CLI has actually been used on this
/// machine: its own config/home directory exists. Avoids creating an
/// instructions file (or nagging about one) for a tool the user has never
/// run.
fn kind_appears_installed(kind: &AgentKindDefinition) -> bool {
    paths::home_dir().join(kind.home_dir_name).exists()
}

fn already_has_instructions(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|content| content.contains(DETECTION_MARKER))
        .unwrap_or(false)
}

/// Checks whether `kind`'s global instructions file is missing the
/// worktree-creation section and, if so and the user hasn't already been
/// offered it, shows a dismissible toast in `workspace` offering to add it.
/// A no-op for remote projects, disabled `agent_control`, kinds without a
/// known instructions-file convention, kinds that don't appear installed,
/// files that already have the section, and kinds already offered before.
pub(crate) fn maybe_offer_worktree_instructions(
    kind: &AgentKindDefinition,
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) {
    if !crate::AgentThreadSettings::get_global(cx).agent_control {
        return;
    }
    let Some(path) = global_instructions_path(kind.id) else {
        return;
    };
    if !kind_appears_installed(kind) || already_has_instructions(&path) {
        return;
    }
    if db::kvp::KeyValueStore::global(cx)
        .scoped(DISMISSED_NAMESPACE)
        .read(kind.id)
        .log_err()
        .flatten()
        .is_some()
    {
        return;
    }

    let store = db::kvp::KeyValueStore::global(cx);
    db::write_and_log(cx, {
        let kind_id = kind.id.to_string();
        move || async move {
            store
                .scoped(DISMISSED_NAMESPACE)
                .write(kind_id, "1".into())
                .await
        }
    });

    let kind_label = kind.label.clone();
    let path_for_message = path.clone();
    let notification_id =
        NotificationId::composite::<WorktreeInstructionsNudge>(SharedString::from(kind.id));
    workspace.show_toast(
        Toast::new(
            notification_id,
            format!(
                "Add worktree auto-tracking instructions to {}? This lets {kind_label} tell \
                 Flint when it creates a new git worktree.",
                path_for_message.display()
            ),
        )
        .on_click("Add", move |_window, _cx| {
            append_instructions(&path);
        }),
        cx,
    );
}

enum WorktreeInstructionsNudge {}

fn append_instructions(path: &Path) {
    match std::fs::read_to_string(path) {
        Ok(existing) => {
            if existing.contains(DETECTION_MARKER) {
                return;
            }
            let mut updated = existing;
            if !updated.is_empty() && !updated.ends_with('\n') {
                updated.push('\n');
            }
            if !updated.is_empty() {
                updated.push('\n');
            }
            updated.push_str(WORKTREE_INSTRUCTIONS_BLOCK);
            std::fs::write(path, updated).log_err();
        }
        Err(_) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).log_err();
            }
            std::fs::write(path, WORKTREE_INSTRUCTIONS_BLOCK).log_err();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn already_has_instructions_detects_an_existing_manually_added_block() {
        let temp_dir = tempfile::tempdir().expect("failed to create a temp dir");
        let path = temp_dir.path().join("AGENTS.md");
        std::fs::write(
            &path,
            "some preamble\n\n# Worktree creation\n\nretie-thread --worktree\n",
        )
        .expect("failed to write the fixture file");
        assert!(already_has_instructions(&path));
    }

    #[test]
    fn already_has_instructions_is_false_for_a_missing_or_unrelated_file() {
        let temp_dir = tempfile::tempdir().expect("failed to create a temp dir");
        assert!(!already_has_instructions(
            &temp_dir.path().join("missing.md")
        ));

        let unrelated = temp_dir.path().join("AGENTS.md");
        std::fs::write(&unrelated, "just some other instructions\n")
            .expect("failed to write the fixture file");
        assert!(!already_has_instructions(&unrelated));
    }

    #[test]
    fn append_instructions_creates_a_missing_file() {
        let temp_dir = tempfile::tempdir().expect("failed to create a temp dir");
        let path = temp_dir.path().join("nested").join("AGENTS.md");
        append_instructions(&path);
        let content = std::fs::read_to_string(&path).expect("failed to read the written file");
        assert!(content.contains(DETECTION_MARKER));
    }

    #[test]
    fn append_instructions_preserves_existing_content() {
        let temp_dir = tempfile::tempdir().expect("failed to create a temp dir");
        let path = temp_dir.path().join("CLAUDE.md");
        std::fs::write(&path, "Always be concise.\n").expect("failed to write the fixture file");

        append_instructions(&path);

        let content = std::fs::read_to_string(&path).expect("failed to read the written file");
        assert!(content.starts_with("Always be concise.\n"));
        assert!(content.contains(DETECTION_MARKER));
    }

    #[test]
    fn append_instructions_does_not_duplicate_an_existing_block() {
        let temp_dir = tempfile::tempdir().expect("failed to create a temp dir");
        let path = temp_dir.path().join("AGENTS.md");
        std::fs::write(&path, WORKTREE_INSTRUCTIONS_BLOCK)
            .expect("failed to write the fixture file");

        append_instructions(&path);

        let content = std::fs::read_to_string(&path).expect("failed to read the written file");
        assert_eq!(content.matches(DETECTION_MARKER).count(), 1);
    }

    #[test]
    fn global_instructions_path_is_none_for_an_unknown_convention() {
        assert!(global_instructions_path("not-a-real-kind").is_none());
    }

    #[test]
    fn global_instructions_path_covers_every_registered_kind() {
        assert_eq!(
            global_instructions_path("codex"),
            Some(paths::home_dir().join(".codex/AGENTS.md"))
        );
        assert_eq!(
            global_instructions_path("claude"),
            Some(paths::home_dir().join(".claude/CLAUDE.md"))
        );
        assert_eq!(
            global_instructions_path("opencode"),
            Some(paths::home_dir().join(".config/opencode/AGENTS.md"))
        );
        assert_eq!(
            global_instructions_path("pi"),
            Some(paths::home_dir().join(".pi/agent/AGENTS.md"))
        );
    }
}
