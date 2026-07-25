# Tunneled Remote Agent Route Indicator

## Problem

`RemoteAgentRoute` has two values (`crates/settings_content/src/settings_content.rs:1010`):

- `Direct` (the default) runs the configured ambient executable on the remote.
- `Tunneled` runs the pinned Flint-managed executable on the remote and routes
  its traffic through local Flint.

The route is already visible in two places:

- The Manage Servers list renders an "Agent: Direct/Tunneled" dropdown per
  server (`crates/recent_projects/src/remote_servers.rs:1784-1828`, invoked at
  `1633-1637`).
- The Agent Threads panel renders the active route as a muted section-header
  label for the current workspace (`crates/agent_threads/src/panel.rs:1088-1098`
  and `1138-1143`).

Both are *management* surfaces: you see the route while configuring a server,
or while already inside the agent panel of the project you are in. Neither
helps you answer "which route is *this* project on?" while you are working, or
"which of these projects is tunneled?" while choosing between them.

The two surfaces that do identify projects both stop at transport:

- The title bar (`crates/title_bar/src/title_bar.rs:441`,
  `render_remote_project_connection`) shows an icon, nickname, and
  connection-state dot for the foreground project, with no route information.
- The project picker (`crates/recent_projects/src/recent_projects.rs`) renders
  a per-row icon via `icon_for_remote_connection` encoding only SSH / WSL /
  Docker, so a tunneled and a direct project on the same host look identical.

## Desired Behavior

A `⇄` marker appears next to a remote project wherever that project is
identified, and only when its agent route is `Tunneled`.

Nothing renders for `Direct`, for local projects, or for WSL and Docker
connections. `Direct` is the default, so the common case is visually unchanged
and the marker's presence is the entire signal.

Every instance carries hover text naming the route, so the bare glyph is
explainable and cannot be mistaken for a property of the SSH connection itself.

## Scope

| Surface | Marker | Rationale |
| --- | --- | --- |
| Title bar remote connection | Yes | Foreground project; visible with no interaction |
| Project picker, **This Window** section | Yes | Every project in the window; the existing check mark shows which is foreground |
| Project picker, **Recent Projects** section | Yes | Projects the user may open |
| Project picker, **Current Folders** section | No | Uniform across rows; see below |
| Welcome page recent projects | No | Dependency direction; see below |
| `sidebar_recent_projects.rs` | No | Unreachable; see below |
| Manage Servers list | No | Already states the route in words |
| Agent Threads panel header | No | Already states the route in words |

### Current Folders is excluded

Every row in that section carries the same connection. `get_open_folders` reads
`connection_options` once from `workspace.project()`
(`recent_projects.rs:194-196`) and copies it into each entry (`234-251`). A
marker there would therefore be present on every row or absent from every row,
disambiguating nothing *within* the section — which is the only job it could do
there, since the section lists worktrees of one project rather than distinct
projects.

The section is also conditional: `get_open_folders` returns empty for a single
visible worktree (`recent_projects.rs:199-201`), so it is absent entirely for
typical one-folder projects.

The row is technically capable of hosting the marker — `folder.connection_options`
is already in hand at `recent_projects.rs:1289` and the name row at `1313-1332`
has room. This is a product-scope decision, not a structural limitation.

**Known gap:** the title bar's remote indicator is gated on
`title_bar_settings.show_project_items` (`title_bar.rs:220-238`, defaulted in
`assets/settings/default.json`). A user who disables project items loses the
title bar marker while the picker stays reachable, so for that user no picker
section states the current project's route. The Agent Threads panel and Manage
Servers still do. Accepted rather than conditioning Current Folders rendering
on another crate's title bar setting, which would couple the two surfaces for a
minority configuration.

### Welcome page is excluded

`crates/workspace/src/welcome.rs:339-343` does distinguish local from remote
(`IconName::Folder` vs `IconName::Server`), so it is a legitimate candidate.
It is excluded for two reasons:

1. **Dependency direction.** `recent_projects` depends on `workspace`
   (`crates/recent_projects/Cargo.toml:52`) and `workspace` has no reverse
   dependency. A predicate housed in `recent_projects` cannot be called from
   `workspace/src/welcome.rs`. Serving it would mean relocating the predicate
   to a crate both can reach, widening this change substantially.
2. **Rigid component.** Rows render through `SectionButton`
   (`welcome.rs:65-89`), which accepts a label and an icon and has no slot for
   trailing metadata.

Recorded as follow-up work rather than silently dropped.

### SidebarRecentProjects is excluded

The module is declared `pub mod sidebar_recent_projects` at
`recent_projects.rs:5`, and the type and its `popover` constructor exist at
`sidebar_recent_projects.rs:26-38` and `104-110`, but no code in the repository
constructs it. A marker added there would never render.

## Considered Approaches

### Sibling predicate function (chosen)

Add a `remote_connection_is_tunneled(options, cx) -> bool` helper beside the
existing `icon_for_remote_connection`, and guard each marker with `.when(...)`.

Chosen because the rule and the marker's text live in one place while each
render site keeps control of placement. `title_bar` already depends on
`recent_projects` (`crates/title_bar/Cargo.toml:43`), so no new dependency is
introduced.

The limitation is documented above: this placement cannot serve
`workspace/src/welcome.rs`.

### Widen `icon_for_remote_connection`'s return type

Return a `{ icon, tunneled }` struct instead of an `IconName`. Rejected: it
changes a signature used by four call sites and by
`this_window_project_icons_use_each_project_group_host`, and each site must
still render the marker itself, so nothing is saved.

### Shared `RenderOnce` badge component

One component rendering icon and marker together. Rejected: the title bar wraps
its icon in `IconWithIndicator` for the connection-state dot while picker rows
use a bare `Icon`, so the component would need enough configuration knobs to be
harder to follow than the call sites it replaced.

## Design

### Marker glyph

`IconName::ArrowRightLeft` (`crates/icons/src/icons.rs:37`, backed by
`assets/icons/arrow_right_left.svg`), rendered at `IconSize::Small`.

The visual is the `⇄` form: a rightward arrow above a leftward arrow. An SVG is
used rather than the U+21C4 text character because `ui_font_family` is
user-configurable (`assets/settings/default.json:57`). The character is present
in the bundled IBM Plex Sans — `.FlintSans` maps to it at
`crates/gpui/src/text_system.rs:1185`, and cmap inspection of
`IBMPlexSans-Regular.ttf` and `-SemiBold.ttf` finds `0x21C4` — but a user on a
different UI font would get GPUI's fallback, which can differ in weight or
optical size from surrounding text. The SVG has no such failure mode and picks
up `Color` like every other glyph in these surfaces.

Converting the font glyph to an SVG asset was considered and rejected. Flint's
icons are stroke-based line art (`fill="none"`, `stroke-width="1.2"`, rounded
caps); a font glyph converts to filled outlines and would read heavier than its
neighbors. It would also create a derivative of an SIL OFL 1.1 font with a
Reserved Font Name inside `assets/icons`, which currently carries a single
Lucide ISC notice (`assets/icons/LICENSES`). The existing Lucide icon avoids
both problems.

`ArrowRightLeft` has no other use in the codebase, so it carries no conflicting
established meaning.

### Semantic risk

The route governs agent traffic only — `crates/agent_threads/src/store.rs`
applies it when launching agents, and the Manage Servers copy at
`remote_servers.rs:1850-1862` states that ordinary remote editing and terminals
are unaffected. A bare arrow beside a connection host could be misread as
describing the SSH connection.

Mitigated by hover text that names the route explicitly ("Tunneled agent
route") at every instance, and by the vocabulary already established by the
existing Manage Servers dropdown and Agent Threads label. The marker is never
rendered without reachable hover text.

### Shared predicate

In `crates/recent_projects/src/recent_projects.rs`, beside
`icon_for_remote_connection`:

```rust
pub const TUNNELED_ROUTE_TOOLTIP: &str = "Tunneled agent route";

pub fn remote_connection_is_tunneled(
    options: Option<&RemoteConnectionOptions>,
    cx: &App,
) -> bool {
    let Some(options) = options else { return false };
    RemoteSettings::get_global(cx).agent_route_for(options)
        == Some(RemoteAgentRoute::Tunneled)
}

pub fn tunneled_route_marker() -> impl IntoElement {
    Icon::new(IconName::ArrowRightLeft)
        .size(IconSize::Small)
        .color(Color::Muted)
}
```

Returning `bool` rather than `Option<SharedString>` lets call sites read as
`.when(is_tunneled, ...)`.

The marker is built by a constructor rather than exposed as a raw glyph
constant so that its representation is changeable in one place. Both open
questions about its appearance — whether `Color::Muted` is legible enough for
the only signal the feature has, and whether the icon reads correctly at
`IconSize::Small` — resolve inside this function without touching a call site.

`RemoteSettings::agent_route_for` already exists and is tested
(`crates/recent_projects/src/remote_connections.rs:80-103`, tests at `169-217`).
It returns `None` for non-SSH connections and `Some(Direct)` for SSH hosts
absent from `remote.ssh_connections`. Local projects, WSL, Docker, unknown SSH
hosts, and explicit Direct therefore all collapse to `false`.

**Required fix to avoid per-row allocation.** `agent_route_for` currently calls
`self.ssh_connections()` (`remote_connections.rs:94`), which clones the entire
stored connection vector (`41-43`). The predicate runs once per visible picker
row, so this must be changed to borrow and scan `self.ssh_connections.0.iter()`
instead. This is a behavior-preserving change to an existing method covered by
existing tests.

**Duplicate identities.** `agent_route_for` uses `.find`, so two settings
entries sharing host, username, and port resolve to the first. The Manage
Servers selector writes by concrete list index
(`remote_servers.rs:1914-1923`), so editing the second duplicate would leave
the marker reflecting the first. Pre-existing behavior, unchanged here and not
worth guarding against for a malformed configuration, but recorded so the
inconsistency is not mistaken for a bug in this feature.

### Reactivity

Settings changes call `cx.refresh_windows()`
(`crates/settings/src/settings_store.rs:392-401`, wired at
`crates/flint/src/main.rs:490-493`), GPUI marks every window for refresh
(`crates/gpui/src/app.rs:942-946`), and refresh bypasses cached view rendering
(`crates/gpui/src/view.rs:155-178`). Reading the route during render is
therefore reactive with no explicit subscription.

### Rendering

All sites render the marker through `tunneled_route_marker()`.
`Color::Muted` matches the secondary metadata already in these rows.

**Title bar** (`title_bar.rs:502-520`). The `h_flex` currently carries
`max_w_32`, capping icon and nickname together. That cap moves inward to wrap
only those two, and the marker becomes a sibling outside it, so the nickname
keeps its existing width budget:

```
[⛁•] build-host ⇄
 └─ max_w_32 ─┘└new┘
```

`remote_connection_options` returns an owned value consumed by the `match` at
line 447, so the predicate must be evaluated before that match.

**Both picker rows share one placement rule.** This Window
(`recent_projects.rs:1451-1472`) and Recent Projects (`1602-1630`) have the
same structure: an `h_flex` containing an optional icon, the rendered
`HighlightedMatchWithPaths`, and a container tooltip. The marker is inserted as
a sibling child after `highlighted.render(...)` and before `.tooltip(...)` in
both.

Neither row exposes the name, branch, or active check individually — all three
are delegated to `HighlightedMatchWithPaths` — so there is no inner name row to
target, and none is needed.

Recent Projects additionally carries `.flex_grow_1()` on its container
(`1613`), so its marker needs `flex_shrink_0` to survive long paths. This
Window has no `flex_grow_1` and does not.

### Hover text

Picker rows wrap the marker in `div().id(...).tooltip(...)` reading
`TUNNELED_ROUTE_TOOLTIP`. This takes precedence over the row's own tooltip:
`prepaint_tooltip` iterates `tooltip_requests` in reverse
(`crates/gpui/src/window.rs:2809`), and children register after parents, so the
innermost request is served first.

The title bar cannot nest: its marker sits inside a `trigger_with_tooltip`
trigger (`crates/ui/src/components/popover_menu.rs:198-218`), which attaches
the tooltip to the outer trigger and suppresses it while the menu is open.
There the route folds into the existing tooltip meta, appended only when
tunneled:

```
Connected to: build-host · Tunneled agent route
```

### Behavior during connection failure

The marker remains visible while the connection is reconnecting or
disconnected. `remote_connection_options` stays available independently of
`remote_connection_state` (`crates/project/src/project.rs:1771-1782`), and the
route is a configuration property rather than a live connection state — it
describes how agents *would* reach their provider, which does not stop being
true while the link is down. The connection-state dot already communicates
health separately.

## Testing

### Predicate tests

In the existing `recent_projects.rs` test module, beside
`this_window_project_icons_use_each_project_group_host` (line 2616), using the
`SettingsStore::update_global` / `store.update_user_settings` injection pattern
at `3048-3052`:

| Input | Expected |
| --- | --- |
| SSH host configured `agent_route: "tunneled"` | `true` |
| SSH host configured `agent_route: "direct"` | `false` |
| SSH host absent from `ssh_connections` | `false` |
| WSL connection | `false` |
| Docker connection | `false` |
| `None` (local project) | `false` |

The last four pin down the intentional omissions that the repository's Agent
Threads guidance requires be represented as explicit, tested capabilities. The
absent-host case matters most: it is the one that could regress into a false
marker, since an unconfigured host resolves to `Some(Direct)` rather than
`None`.

### Integration tests

Predicate tests alone would not have caught the placement error this design
went through review to fix, so they are not sufficient on their own:

1. **Per-row independence.** A picker containing one tunneled and one direct
   project group marks exactly the tunneled row. This is the regression test
   for targeting the wrong picker section, and is feasible because each
   `ProjectGroupKey` carries its own host (`crates/project/src/project.rs:4916-4927`).
2. **Live update.** With a picker already open, changing `agent_route` in
   settings updates the rendered rows, exercising the `refresh_windows` path
   rather than trusting it.

Title bar width behavior and tooltip rendering are left to manual verification;
asserting on laid-out geometry would be brittle relative to its value.

## Out of Scope

- Changing the route from these surfaces. The marker is read-only; the route
  remains editable through the Manage Servers dropdown.
- Marking `Direct` explicitly. Absence of the marker is the signal.
- The Welcome page. Decided against: serving it requires relocating the
  predicate to a crate both `workspace` and `recent_projects` can reach, which
  is disproportionate to the value of marking a launcher list.
- Wiring up `SidebarRecentProjects`.
- De-duplicating `RemoteSettings::agent_route_for` against the parallel
  implementation in `crates/agent_threads/src/agent_threads.rs:388-413`. Two
  crates independently resolve the same setting; worth unifying, but not as a
  side effect of adding an indicator.

## Release Note for the Implementation PR

```
Release Notes:

- Added a `⇄` indicator marking remote projects that use the tunneled agent
  route, in the title bar and the project picker.
```
