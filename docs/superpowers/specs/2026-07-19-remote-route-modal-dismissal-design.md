# Keep the Remote Project Dialog Open After Route Selection

## Status

Approved in conversation on 2026-07-19.

Design owner: Codex.

## Problem

The Open Remote Project dialog contains an agent-route dropdown for SSH hosts.
Choosing `Tunneled` persists the setting synchronously from the dropdown's
click handler. The cached row still contains the previous route, so the generic
settings synchronization treats the entire SSH connection as changed and
rebuilds the remote-server list unnecessarily.

The dismissal itself has a separate cause. `RemoteServerProjects` registers a
geometric `on_mouse_down_out` handler on its content. The dropdown is rendered
as a deferred popover outside the dialog's rectangle, so selecting a route is
classified as an outside click and emits the parent modal's `DismissEvent`.

Changing an agent route must not initiate a remote connection or dismiss the
project dialog.

## Design

Keep the existing dropdown and its two route choices. Remove the content-level
`on_mouse_down_out` dismissal handler. The workspace modal layer already owns
the full-screen backdrop and dismisses the modal when that backdrop is clicked.
A click on the deferred route menu remains inside the modal's element hierarchy,
so it closes only the dropdown. Escape, Cancel, and genuine backdrop clicks
continue to dismiss the Open Remote Project dialog.

When a route with no active agent terminals is selected for a saved SSH
connection:

1. update the matching cached row's route in place;
2. notify GPUI so the selector reflects the new value; and
3. persist the same value to settings.

When the settings notification arrives, the cached and persisted SSH
connections agree, so the generic synchronization does not rebuild the list.
Opening a project remains an explicit, separate action.

An SSH-config-only host has no saved row to update. Persisting its first explicit
route necessarily creates a saved SSH connection, which is a structural list
change. The modal-layer fix ensures that later synchronization preserves the
open modal.

The existing active-agent flow remains unchanged: it asks for confirmation,
closes only matching agent terminals, and persists the route asynchronously
after cleanup. Cancelling that confirmation changes nothing.

## Error Handling

Use the existing settings writer and error-reporting behavior. No route-change
guard is needed when there are no active agent terminals because no agent
process can race the transition.

## Tests

Add a GPUI regression test at the remote-project modal seam. It will choose
`Tunneled` for an SSH entry with no active agent terminals and verify that:

- the route setting is persisted;
- the route popover closes;
- the Open Remote Project modal remains active;
- the saved remote-server list is not reconstructed for the route-only change;
- a genuine backdrop click still dismisses the modal;
- no remote connection is started by route selection; and
- the project/open-folder controls remain available.

Run the focused `recent_projects` tests, formatting, and workspace clippy.
