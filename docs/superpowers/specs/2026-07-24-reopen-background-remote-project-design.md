# Reopen Background Remote Projects from This Window

## Problem

When a window contains a foreground local project and a background remote
project, Flint restores only the foreground workspace after restarting. The
background project remains visible under **This Window** because its
`ProjectGroupKey` is persisted, including its remote connection options and
paths.

Selecting that remote project currently checks only for a previously active
loaded workspace. Restored project groups have no such workspace reference, so
the picker falls through to `find_or_create_local_workspace`. This fails to
reopen the remote project and can replace its remote identity with a local one,
causing the local-project icon to appear.

## Desired Behavior

Selecting a remote project under **This Window** must:

1. Activate an existing loaded workspace when one is available.
2. Otherwise reconnect using the remote connection options stored in the
   project-group key.
3. Open and activate the remote paths in the current window.
4. Perform no remote reconnection until the user selects the project.
5. Preserve the remote project-group identity if reconnection fails.

Local project groups must continue to use the existing local workspace loader.

## Considered Approaches

### Route by project-group host in the picker

After the existing loaded-workspace lookup, inspect the selected
`ProjectGroupKey`. Remote groups use `open_remote_project`; local groups retain
the current `find_or_create_local_workspace` fallback.

This is the chosen approach because it changes only the incorrect decision
point, reuses the established remote connection flow, and avoids reconnecting
background projects during application startup.

### Add a generic project-group opener to `MultiWorkspace`

Move both local and remote opening behind a new `MultiWorkspace` API. This
would centralize routing, but remote connection setup requires UI-aware
dependencies and callbacks that would broaden the change without another
caller needing the abstraction.

### Restore all background remote workspaces at startup

Reconnect every persisted background remote project while restoring the
window. This would make later activation immediate, but could trigger SSH
connections, credential prompts, and failures without user intent.

## Implementation

The **This Window** confirmation path currently checks only the group's last
active workspace (`MultiWorkspace::last_active_workspace_for_group`). It will
be extended to also check any currently loaded workspace belonging to the
group via the existing but not-yet-called
`MultiWorkspace::workspaces_for_project_group`, since the last-active
reference can be stale (dropped) while another workspace for the same group
is still open. If either lookup finds a workspace, Flint will activate it.
This broader check is new behavior, not a restatement of today's lookup.

If no workspace is loaded:

- A local project group will continue through
  `find_or_create_local_workspace`.
- A remote project group will pass its stored connection options and paths to
  `open_remote_project`, with the current `MultiWorkspace` as the requesting
  window. SSH options will be completed from current remote settings in the
  same way as recent remote projects.

The remote operation will use the existing user-facing error prompt. It will
not create a provisional local workspace, so a failed connection cannot
convert the group or its icon to local.

## Testing

A GPUI regression test will construct the restart-shaped state:

- A local workspace is active in a `MultiWorkspace`.
- A remote project-group key is restored without a loaded remote workspace.
- The remote entry is selected and confirmed from the recent-project picker.

The test will use a mock remote connection and assert that the resulting
active workspace is remote and belongs to the selected remote project group.
It will also verify that the group retains its remote host identity, which
drives the remote icon.

Existing local project-group activation tests and the focused
`recent_projects` test suite will be run to guard against regressions.

## Non-Goals

- Reconnecting remote projects during startup.
- Changing how project groups are persisted.
- Changing the behavior of the **Recent Projects** section.
- Retrying failed remote connections automatically.
