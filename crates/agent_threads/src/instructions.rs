//! Nudges the user to add Flint's feature branch and worktree instructions to a
//! CLI's own global instructions file (e.g. `~/.codex/AGENTS.md`,
//! `~/.claude/CLAUDE.md`). Without that section, an agent has no way to
//! discover the `flint-agent-control` command at all, so agent-initiated
//! worktree creation (`crate::control`) silently never gets used even when
//! `agent_threads.agent_control` is on.
//!
//! The exact discovery block is selected for the host platform and only
//! offered for agent kinds with a verified shell/instructions capability.

use std::path::{Path, PathBuf};

use gpui::{Context, SharedString};
use settings::Settings as _;
use util::ResultExt as _;
use workspace::notifications::NotificationId;
use workspace::{Toast, Workspace};

use crate::AgentKindDefinition;
#[cfg(windows)]
use crate::WindowsInstructionShell;

/// The exact block Flint offers to append. `already_has_instructions` also
/// just checks for `DETECTION_MARKER` (a substring of this), so a file a
/// user (or an earlier Flint version) already has this in is recognized
/// as-is -- no separate sentinel comment needed.
#[cfg(any(unix, test))]
const WORKTREE_INSTRUCTIONS_BLOCK: &str = "\
## Feature branches and worktrees

For every repository change, use a feature branch. Do not commit directly to
`main`.

Use the current worktree when it is clean, available, and not in use by another
task. Create or switch to a feature branch in that worktree.

Create a new git worktree only when a separate directory is useful or required.
Examples include concurrent tasks, unrelated changes in the current worktree,
agent handoff, or an explicit user request. A new worktree must also use a
feature branch.

After creating a git worktree, run:

    find ~/Library/Application\\ Support/Flint ~/.local/share/flint -maxdepth 1 -name \"agent-control-*-executable.json\" -exec cat {} \\; 2>/dev/null

If it prints JSON with an \"executable\" field, run:

    \"<executable>\" retie-thread --worktree <path>

Use that value and the full path of the new worktree. This moves the thread to
the new worktree in Flint's Agent Threads panel. No output means Flint is not
installed or running. A \"not recognized\" report means this terminal is not a
Flint-tracked terminal. In both cases, continue without retieing the thread.

The main decision rule is: use a feature branch for change ownership; use a
worktree for directory isolation.
";

#[cfg(any(windows, test))]
const POWERSHELL_WORKTREE_INSTRUCTIONS_BLOCK: &str = "\
## Feature branches and worktrees

For every repository change, use a feature branch. Do not commit directly to
`main`.

Use the current worktree when it is clean, available, and not in use by another
task. Create or switch to a feature branch in that worktree.

Create a new git worktree only when a separate directory is useful or required.
Examples include concurrent tasks, unrelated changes in the current worktree,
agent handoff, or an explicit user request. A new worktree must also use a
feature branch.

After creating a git worktree, run this in PowerShell:

    $sessionId = (Get-Process -Id $PID).SessionId
    $marker = Get-ChildItem -Path (Join-Path $env:LOCALAPPDATA 'Flint') -Filter \"agent-control-*-$sessionId-executable.json\" -File | Select-Object -First 1
    $control = if ($marker) { (Get-Content -Raw $marker.FullName | ConvertFrom-Json).executable }

If `$control` contains an executable path, run:

    & $control retie-thread --worktree \"<path>\"

Use the full path of the new worktree. This moves the thread to the new worktree
in Flint's Agent Threads panel. No marker means Flint is not installed or
running in this Windows sign-in session. A \"not recognized\" report means this
terminal is not a Flint-tracked terminal. In both cases, continue without
retieing the thread.

The main decision rule is: use a feature branch for change ownership; use a
worktree for directory isolation.
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

fn worktree_instructions_block(kind: &AgentKindDefinition) -> Option<&'static str> {
    #[cfg(unix)]
    {
        let _ = kind;
        Some(WORKTREE_INSTRUCTIONS_BLOCK)
    }
    #[cfg(windows)]
    {
        if !kind.supports_windows_agent_control() {
            return None;
        }
        match kind.windows_agent_control.instruction_shell {
            Some(WindowsInstructionShell::PowerShell) => {
                Some(POWERSHELL_WORKTREE_INSTRUCTIONS_BLOCK)
            }
            None => None,
        }
    }
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
    let Some(instructions_block) = worktree_instructions_block(kind) else {
        #[cfg(windows)]
        if let Some(reason) = kind.windows_agent_control_unsupported_reason() {
            log::debug!(
                "agent_threads: Windows worktree instructions are unavailable for {}: {reason}",
                kind.id
            );
        }
        return;
    };
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
            append_instructions(&path, instructions_block);
        }),
        cx,
    );
}

enum WorktreeInstructionsNudge {}

fn append_instructions(path: &Path, instructions_block: &str) {
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
            updated.push_str(instructions_block);
            std::fs::write(path, updated).log_err();
        }
        Err(_) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).log_err();
            }
            std::fs::write(path, instructions_block).log_err();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_block() -> &'static str {
        #[cfg(unix)]
        return WORKTREE_INSTRUCTIONS_BLOCK;
        #[cfg(windows)]
        return POWERSHELL_WORKTREE_INSTRUCTIONS_BLOCK;
    }

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
        append_instructions(&path, test_block());
        let content = std::fs::read_to_string(&path).expect("failed to read the written file");
        assert!(content.contains(DETECTION_MARKER));
    }

    #[test]
    fn append_instructions_preserves_existing_content() {
        let temp_dir = tempfile::tempdir().expect("failed to create a temp dir");
        let path = temp_dir.path().join("CLAUDE.md");
        std::fs::write(&path, "Always be concise.\n").expect("failed to write the fixture file");

        append_instructions(&path, test_block());

        let content = std::fs::read_to_string(&path).expect("failed to read the written file");
        assert!(content.starts_with("Always be concise.\n"));
        assert!(content.contains(DETECTION_MARKER));
    }

    #[test]
    fn append_instructions_does_not_duplicate_an_existing_block() {
        let temp_dir = tempfile::tempdir().expect("failed to create a temp dir");
        let path = temp_dir.path().join("AGENTS.md");
        let block = test_block();
        std::fs::write(&path, block).expect("failed to write the fixture file");

        append_instructions(&path, block);

        let content = std::fs::read_to_string(&path).expect("failed to read the written file");
        assert_eq!(content.matches(DETECTION_MARKER).count(), 1);
    }

    #[test]
    fn all_platform_blocks_explain_when_to_use_a_feature_branch_and_worktree() {
        for block in [
            WORKTREE_INSTRUCTIONS_BLOCK,
            POWERSHELL_WORKTREE_INSTRUCTIONS_BLOCK,
        ] {
            assert!(block.contains("For every repository change, use a feature branch."));
            assert!(block.contains("Do not commit directly to\n`main`."));
            assert!(block.contains(
                "Use the current worktree when it is clean, available, and not in use by another\n\
                 task."
            ));
            assert!(block.contains(
                "Create a new git worktree only when a separate directory is useful or required."
            ));
            assert!(block.contains(
                "use a feature branch for change ownership; use a\n\
                 worktree for directory isolation."
            ));
        }
    }

    #[test]
    fn all_platform_blocks_include_the_platform_specific_retie_command() {
        assert!(
            WORKTREE_INSTRUCTIONS_BLOCK
                .contains("find ~/Library/Application\\ Support/Flint ~/.local/share/flint")
        );
        assert!(
            WORKTREE_INSTRUCTIONS_BLOCK.contains("\"<executable>\" retie-thread --worktree <path>")
        );
        assert!(
            POWERSHELL_WORKTREE_INSTRUCTIONS_BLOCK.contains("(Get-Process -Id $PID).SessionId")
        );
        assert!(
            POWERSHELL_WORKTREE_INSTRUCTIONS_BLOCK
                .contains("agent-control-*-$sessionId-executable.json")
        );
        assert!(
            POWERSHELL_WORKTREE_INSTRUCTIONS_BLOCK
                .contains("& $control retie-thread --worktree \"<path>\"")
        );
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

    #[cfg(windows)]
    #[test]
    fn windows_nudge_is_enabled_only_for_verified_agent_shells() {
        let mut registry = crate::agent_kind_registry().into_iter();
        let codex = registry.next().expect("Codex is registered");
        assert_eq!(codex.id, "codex");
        assert_eq!(
            worktree_instructions_block(&codex),
            Some(POWERSHELL_WORKTREE_INSTRUCTIONS_BLOCK)
        );
        assert_eq!(codex.windows_agent_control_unsupported_reason(), None);

        for kind in registry {
            assert!(
                worktree_instructions_block(&kind).is_none(),
                "{} must stay disabled until its Windows shell and cwd authorization are verified",
                kind.id
            );
            assert!(
                kind.windows_agent_control_unsupported_reason().is_some(),
                "{} must explain why Windows agent control is unavailable",
                kind.id
            );
        }
    }
}
