//! Maintains Flint's agent-thread instructions in each supported local agent's
//! global instructions file.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context as _, Result, anyhow};
use gpui::{Context, SharedString};
use workspace::notifications::NotificationId;
use workspace::{Toast, Workspace};

use crate::AgentKindDefinition;
#[cfg(windows)]
use crate::WindowsInstructionShell;

#[cfg(any(unix, test))]
const LEGACY_WORKTREE_INSTRUCTIONS_BLOCK: &str = "\
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

#[cfg(any(unix, test))]
const SHORT_LEGACY_WORKTREE_INSTRUCTIONS_BLOCK: &str = "\
## Worktree creation

Run this after creating a git worktree:

    find ~/Library/Application\\ Support/Flint ~/.local/share/flint -maxdepth 1 -name \"agent-control-*-executable.json\" -exec cat {} \\; 2>/dev/null

If it prints JSON with an \"executable\" field, run:

    \"<executable>\" retie-thread --worktree <path>

using that value and the new worktree's full path. This moves the thread to
the new worktree in Flint's Agent Threads panel. No output means Flint is
not installed or running. A \"not recognized\" report means this terminal
just is not a Flint-tracked one, even if Flint is running elsewhere. Skip
this section either way.
";

#[cfg(any(unix, test))]
const DISCOVERY_LEGACY_WORKTREE_INSTRUCTIONS_BLOCK: &str = "\
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

#[cfg(any(windows, test))]
const LEGACY_POWERSHELL_WORKTREE_INSTRUCTIONS_BLOCK: &str = "\
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

const MANAGED_BLOCK_VERSION: u32 = 1;
const MANAGED_BLOCK_BEGIN_PREFIX: &str = "<!-- Flint managed agent-thread instructions: begin v";
const MANAGED_BLOCK_END: &str = "<!-- Flint managed agent-thread instructions: end -->";

#[cfg(any(unix, test))]
static WORKTREE_INSTRUCTIONS_BLOCK: LazyLock<String> = LazyLock::new(|| {
    managed_block(LEGACY_WORKTREE_INSTRUCTIONS_BLOCK.replace(
        "\"<executable>\" retie-thread --worktree <path>",
        "\"<executable>\" thread retie --worktree <path>",
    ))
});

#[cfg(any(windows, test))]
static POWERSHELL_WORKTREE_INSTRUCTIONS_BLOCK: LazyLock<String> = LazyLock::new(|| {
    managed_block(LEGACY_POWERSHELL_WORKTREE_INSTRUCTIONS_BLOCK.replace(
        "& $control retie-thread --worktree \"<path>\"",
        "& $control thread retie --worktree \"<path>\"",
    ))
});

fn managed_block(body: String) -> String {
    format!("{MANAGED_BLOCK_BEGIN_PREFIX}{MANAGED_BLOCK_VERSION} -->\n{body}{MANAGED_BLOCK_END}\n")
}

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

fn worktree_instructions_block(kind: &AgentKindDefinition) -> Option<&'static str> {
    #[cfg(unix)]
    {
        let _ = kind;
        Some(WORKTREE_INSTRUCTIONS_BLOCK.as_str())
    }
    #[cfg(windows)]
    {
        if !kind.supports_windows_agent_control() {
            return None;
        }
        match kind.windows_agent_control.instruction_shell {
            Some(WindowsInstructionShell::PowerShell) => {
                Some(POWERSHELL_WORKTREE_INSTRUCTIONS_BLOCK.as_str())
            }
            None => None,
        }
    }
}

#[derive(Debug)]
struct SynchronizedContent {
    content: String,
    changed: bool,
}

fn synchronize_content(
    existing: &str,
    current_block: &str,
    legacy_blocks: &[&str],
) -> Result<SynchronizedContent> {
    if let Some(start) = existing.find(MANAGED_BLOCK_BEGIN_PREFIX) {
        let Some(relative_end) = existing[start..].find(MANAGED_BLOCK_END) else {
            return Err(anyhow!("Flint managed instruction block has no end marker"));
        };
        let mut end = start + relative_end + MANAGED_BLOCK_END.len();
        if existing.as_bytes().get(end) == Some(&b'\n') {
            end += 1;
        }
        let mut content = existing.to_string();
        let mut changed = false;
        if &existing[start..end] != current_block {
            content.replace_range(start..end, current_block);
            changed = true;
        }
        for legacy_block in legacy_blocks {
            if let Some(legacy_start) = content.find(legacy_block) {
                content.replace_range(legacy_start..legacy_start + legacy_block.len(), "");
                changed = true;
            }
        }
        return Ok(SynchronizedContent { content, changed });
    }

    for legacy_block in legacy_blocks {
        if let Some(start) = existing.find(legacy_block) {
            let mut content = existing.to_string();
            content.replace_range(start..start + legacy_block.len(), current_block);
            return Ok(SynchronizedContent {
                content,
                changed: true,
            });
        }
    }

    let mut content = existing.to_string();
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    if !content.is_empty() && !content.ends_with("\n\n") {
        content.push('\n');
    }
    content.push_str(current_block);
    Ok(SynchronizedContent {
        content,
        changed: true,
    })
}

fn synchronize_instructions_file(
    path: &Path,
    current_block: &str,
    legacy_blocks: &[&str],
) -> Result<bool> {
    let existing = match std::fs::read_to_string(path) {
        Ok(existing) => existing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("failed to read {path:?}")),
    };
    let synchronized = synchronize_content(&existing, current_block, legacy_blocks)?;
    if !synchronized.changed {
        return Ok(false);
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("instruction path has no parent: {path:?}"))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create instruction directory {parent:?}"))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("instructions");
    let temporary_path = parent.join(format!(".{file_name}.flint-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut temporary = std::fs::File::create(&temporary_path)
            .with_context(|| format!("failed to create {temporary_path:?}"))?;
        temporary
            .write_all(synchronized.content.as_bytes())
            .with_context(|| format!("failed to write {temporary_path:?}"))?;
        temporary
            .sync_all()
            .with_context(|| format!("failed to sync {temporary_path:?}"))?;
        crate::control::replace_marker(&temporary_path, path)
            .with_context(|| format!("failed to replace {path:?}"))?;
        Ok(())
    })();
    if result.is_err()
        && let Err(error) = std::fs::remove_file(&temporary_path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("failed to remove temporary instruction file: {error}");
    }
    result.map(|()| true)
}

pub(crate) fn synchronize_worktree_instructions() -> Vec<String> {
    let mut errors = Vec::new();
    for kind in crate::agent_kind_registry() {
        let Some(path) = global_instructions_path(kind.id) else {
            continue;
        };
        if !kind_appears_installed(&kind) {
            continue;
        }
        let Some(current_block) = worktree_instructions_block(&kind) else {
            continue;
        };
        let legacy_blocks = legacy_worktree_instructions_blocks(&kind);
        if let Err(error) = synchronize_instructions_file(&path, current_block, legacy_blocks) {
            let message = format!(
                "Could not update Flint instructions for {} at {}: {error:#}",
                kind.label,
                path.display()
            );
            log::error!("agent_threads: {message}");
            errors.push(message);
        }
    }
    errors
}

fn legacy_worktree_instructions_blocks(kind: &AgentKindDefinition) -> &'static [&'static str] {
    #[cfg(unix)]
    {
        let _ = kind;
        &[
            LEGACY_WORKTREE_INSTRUCTIONS_BLOCK,
            SHORT_LEGACY_WORKTREE_INSTRUCTIONS_BLOCK,
            DISCOVERY_LEGACY_WORKTREE_INSTRUCTIONS_BLOCK,
        ]
    }
    #[cfg(windows)]
    {
        let _ = kind;
        &[LEGACY_POWERSHELL_WORKTREE_INSTRUCTIONS_BLOCK]
    }
}

pub(crate) fn show_sync_errors(
    errors: &[String],
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) {
    for (index, error) in errors.iter().enumerate() {
        workspace.show_toast(
            Toast::new(
                NotificationId::composite::<InstructionSyncFailure>(SharedString::from(
                    index.to_string(),
                )),
                error.clone(),
            ),
            cx,
        );
    }
}

enum InstructionSyncFailure {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synchronization_replaces_only_the_managed_block() {
        let existing = format!(
            "user before\n\n{MANAGED_BLOCK_BEGIN_PREFIX}0 -->\nold Flint text\n{MANAGED_BLOCK_END}\n\nuser after\n"
        );

        let updated = synchronize_content(&existing, test_block(), test_legacy_blocks())
            .expect("synchronize content");

        assert!(updated.changed);
        assert!(updated.content.starts_with("user before\n\n"));
        assert!(updated.content.ends_with("\nuser after\n"));
        assert_eq!(
            updated.content.matches(MANAGED_BLOCK_BEGIN_PREFIX).count(),
            1
        );
        assert!(
            updated
                .content
                .contains("\"<executable>\" thread retie --worktree <path>")
        );
        assert!(!updated.content.contains("old Flint text"));
    }

    #[test]
    fn synchronization_migrates_the_exact_unmarked_legacy_block() {
        let existing = format!("user before\n\n{}\nuser after\n", test_legacy_blocks()[0]);

        let updated = synchronize_content(&existing, test_block(), test_legacy_blocks())
            .expect("synchronize content");

        assert!(updated.changed);
        assert!(!updated.content.contains("\"<executable>\" retie-thread"));
        assert!(
            updated
                .content
                .contains("\"<executable>\" thread retie --worktree <path>")
        );
        assert!(updated.content.starts_with("user before\n\n"));
        assert!(updated.content.ends_with("user after\n"));
    }

    #[cfg(unix)]
    #[test]
    fn synchronization_migrates_the_short_unmarked_legacy_block() {
        let existing =
            format!("user before\n\n{SHORT_LEGACY_WORKTREE_INSTRUCTIONS_BLOCK}\nuser after\n");

        let updated = synchronize_content(&existing, test_block(), test_legacy_blocks())
            .expect("synchronize content");

        assert!(!updated.content.contains("retie-thread"));
        assert_eq!(
            updated.content.matches(MANAGED_BLOCK_BEGIN_PREFIX).count(),
            1
        );
        assert!(updated.content.starts_with("user before\n\n"));
        assert!(updated.content.ends_with("user after\n"));
    }

    #[cfg(unix)]
    #[test]
    fn synchronization_removes_short_legacy_block_beside_current_block() {
        let existing = format!(
            "{SHORT_LEGACY_WORKTREE_INSTRUCTIONS_BLOCK}\n{}",
            test_block()
        );

        let updated = synchronize_content(&existing, test_block(), test_legacy_blocks())
            .expect("synchronize content");

        assert!(updated.changed);
        assert!(!updated.content.contains("retie-thread"));
        assert_eq!(
            updated.content.matches(MANAGED_BLOCK_BEGIN_PREFIX).count(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn synchronization_removes_discovery_legacy_block_beside_current_block() {
        let existing = format!(
            "{DISCOVERY_LEGACY_WORKTREE_INSTRUCTIONS_BLOCK}\n{}",
            test_block()
        );

        let updated = synchronize_content(&existing, test_block(), test_legacy_blocks())
            .expect("synchronize content");

        assert!(updated.changed);
        assert!(!updated.content.contains("retie-thread"));
        assert_eq!(
            updated.content.matches(MANAGED_BLOCK_BEGIN_PREFIX).count(),
            1
        );
    }

    #[test]
    fn synchronization_preserves_a_modified_legacy_lookalike() {
        let lookalike = test_legacy_blocks()[0].replace(
            "The main decision rule is:",
            "My manually edited decision rule is:",
        );

        let updated = synchronize_content(&lookalike, test_block(), test_legacy_blocks())
            .expect("synchronize content");

        assert!(
            updated
                .content
                .contains("My manually edited decision rule is:")
        );
        assert_eq!(
            updated.content.matches(MANAGED_BLOCK_BEGIN_PREFIX).count(),
            1
        );
    }

    #[test]
    fn synchronization_rejects_a_managed_block_without_an_end_marker() {
        let existing = format!("user text\n{MANAGED_BLOCK_BEGIN_PREFIX}0 -->\nbroken\n");
        let error = synchronize_content(&existing, test_block(), test_legacy_blocks())
            .expect_err("missing end marker must fail");
        assert!(error.to_string().contains("no end marker"));
    }

    #[test]
    fn synchronization_preserves_newline_boundaries() {
        let updated = synchronize_content("user text", test_block(), test_legacy_blocks())
            .expect("synchronize content");
        assert!(updated.content.starts_with("user text\n\n"));
        assert!(updated.content.ends_with("\n"));
    }

    fn test_block() -> &'static str {
        #[cfg(unix)]
        return WORKTREE_INSTRUCTIONS_BLOCK.as_str();
        #[cfg(windows)]
        return POWERSHELL_WORKTREE_INSTRUCTIONS_BLOCK.as_str();
    }

    fn test_legacy_blocks() -> &'static [&'static str] {
        #[cfg(unix)]
        return &[
            LEGACY_WORKTREE_INSTRUCTIONS_BLOCK,
            SHORT_LEGACY_WORKTREE_INSTRUCTIONS_BLOCK,
            DISCOVERY_LEGACY_WORKTREE_INSTRUCTIONS_BLOCK,
        ];
        #[cfg(windows)]
        return &[LEGACY_POWERSHELL_WORKTREE_INSTRUCTIONS_BLOCK];
    }

    #[test]
    fn synchronization_creates_a_missing_file() {
        let temp_dir = tempfile::tempdir().expect("failed to create a temp dir");
        let path = temp_dir.path().join("nested").join("AGENTS.md");
        assert!(
            synchronize_instructions_file(&path, test_block(), test_legacy_blocks())
                .expect("synchronize instructions")
        );
        let content = std::fs::read_to_string(&path).expect("failed to read the written file");
        assert!(content.contains(MANAGED_BLOCK_BEGIN_PREFIX));
    }

    #[test]
    fn synchronization_preserves_existing_content() {
        let temp_dir = tempfile::tempdir().expect("failed to create a temp dir");
        let path = temp_dir.path().join("CLAUDE.md");
        std::fs::write(&path, "Always be concise.\n").expect("failed to write the fixture file");

        synchronize_instructions_file(&path, test_block(), test_legacy_blocks())
            .expect("synchronize instructions");

        let content = std::fs::read_to_string(&path).expect("failed to read the written file");
        assert!(content.starts_with("Always be concise.\n"));
        assert!(content.contains(MANAGED_BLOCK_BEGIN_PREFIX));
    }

    #[test]
    fn synchronization_does_not_duplicate_an_existing_block() {
        let temp_dir = tempfile::tempdir().expect("failed to create a temp dir");
        let path = temp_dir.path().join("AGENTS.md");
        let block = test_block();
        std::fs::write(&path, block).expect("failed to write the fixture file");

        assert!(
            !synchronize_instructions_file(&path, block, test_legacy_blocks())
                .expect("synchronize instructions")
        );

        let content = std::fs::read_to_string(&path).expect("failed to read the written file");
        assert_eq!(content.matches(MANAGED_BLOCK_BEGIN_PREFIX).count(), 1);
    }

    #[test]
    fn all_platform_blocks_explain_when_to_use_a_feature_branch_and_worktree() {
        for block in [
            WORKTREE_INSTRUCTIONS_BLOCK.as_str(),
            POWERSHELL_WORKTREE_INSTRUCTIONS_BLOCK.as_str(),
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
            WORKTREE_INSTRUCTIONS_BLOCK.contains("\"<executable>\" thread retie --worktree <path>")
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
                .contains("& $control thread retie --worktree \"<path>\"")
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
            Some(POWERSHELL_WORKTREE_INSTRUCTIONS_BLOCK.as_str())
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
