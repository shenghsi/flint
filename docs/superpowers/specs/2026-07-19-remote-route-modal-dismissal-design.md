# Keep the Remote Project Dialog Open After Route Selection

## Status

Approved in conversation on 2026-07-19.

Design owner: Codex.

## Problem

The Open Remote Project dialog contains an agent-route dropdown for SSH hosts.
Choosing `Through Flint` persists the setting synchronously from the dropdown's
click handler. The settings update rebuilds the remote-server rows while the
same click is still being dispatched. The route menu closes, but the containing
Open Remote Project dialog also disappears, preventing the user from choosing a
project or using Open Folder.

Changing an agent route must not initiate a remote connection or dismiss the
project dialog.

## Design

Keep the existing dropdown and its two route choices. When a route with no
active agent terminals is selected, schedule the settings mutation to run after
the current UI event has completed. The context menu can then finish its normal
dismissal before the settings update rebuilds the remote-server row.

Only persistence is deferred. The selected value still comes from the persisted
SSH connection setting on the next render. Opening a project remains an
explicit, separate action.

The existing active-agent flow remains unchanged: it asks for confirmation,
closes only matching agent terminals, and persists the route asynchronously
after cleanup. Cancelling that confirmation changes nothing.

## Error Handling

Use the existing settings writer and error-reporting behavior. If the modal or
row no longer exists when deferred work runs, do not start a connection or
dismiss another modal. No route-change guard is needed when there are no active
agent terminals because no agent process can race the transition.

## Tests

Add a GPUI regression test at the remote-project modal seam. It will choose
`Through Flint` for an SSH entry with no active agent terminals and verify that:

- the route setting is persisted;
- the route popover closes;
- the Open Remote Project modal remains active;
- no remote connection is started by route selection; and
- the project/open-folder controls remain available.

Run the focused `recent_projects` tests, formatting, and workspace clippy.

