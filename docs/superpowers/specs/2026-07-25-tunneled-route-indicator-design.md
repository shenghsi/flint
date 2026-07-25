# Tunneled Remote Agent Route Indicator

## Problem

Flint distinguishes local from remote projects visually, but never surfaces
which remote agent route a remote project uses.

`RemoteAgentRoute` has two values (`crates/settings_content/src/settings_content.rs:1010`):

- `Direct` (the default) runs the configured ambient executable on the remote.
- `Tunneled` runs the pinned Flint-managed executable on the remote and routes
  its traffic through local Flint.

These behave differently enough that users need to know which one a project is
on, but the route is only discoverable by opening settings and matching the
project's host against `remote.ssh_connections` by hand.

Both of the surfaces that already distinguish local from remote fall short:

- The title bar (`crates/title_bar/src/title_bar.rs:441`,
  `render_remote_project_connection`) renders an icon, nickname, and
  connection-state dot for the foreground project, with no route information.
- The project picker (`crates/recent_projects/src/recent_projects.rs`) renders
  a per-row icon via `icon_for_remote_connection` that encodes only the
  transport (SSH / WSL / Docker), so a tunneled and a direct project on the
  same host are indistinguishable.

## Desired Behavior

A `⇄` marker appears next to a remote project wherever that project is
identified, and only when its agent route is `Tunneled`.

Nothing is rendered for `Direct`, for local projects, or for WSL and Docker
connections. `Direct` is the default, so the common case stays visually
unchanged and the marker's presence is the entire signal.

The marker carries hover text so the bare symbol is explainable.

## Scope

| Surface | Marker | Rationale |
| --- | --- | --- |
| Title bar remote connection | Yes | Foreground project; visible with no interaction |
| Project picker, **This Window** section | Yes | Every project in the window; the existing check mark shows which is foreground |
| Project picker, **Recent Projects** section | Yes | Projects the user may open |
| Project picker, **Current Folders** section | No | See below |
| `sidebar_recent_projects.rs` | No | See below |

**Current Folders is excluded** because its rows can never disagree with the
title bar. `get_open_folders` reads `connection_options` once from
`workspace.project()` (`recent_projects.rs:195-196`) and applies it to every
row, and `TitleBar::new` clones that same `Entity<Project>`. Both
`Project::remote_connection_options` and `Project::is_via_remote_server` are
derived from the same `remote_client` field (`crates/project/src/project.rs:1777`,
`2212`), so no state exists in which the section and the title bar differ. A
marker there would restate what the title bar already shows a few pixels away.

**`SidebarRecentProjects` is excluded** because it is unreachable. The module is
declared `pub mod sidebar_recent_projects` at `recent_projects.rs:5`, but no
code in the repository constructs `SidebarRecentProjects` or calls its
`popover` constructor. A marker added there would never render.

## Considered Approaches

### Sibling predicate function (chosen)

Add a `remote_connection_is_tunneled(options, cx) -> bool` helper beside the
existing `icon_for_remote_connection`, and have each render site guard its
marker with `.when(...)`.

Chosen because the "is it tunneled" rule and the marker's text live in exactly
one place while each render site keeps control of placement. That matters: the
title bar puts its icon inside an `IconWithIndicator` next to a nickname on a
single line, whereas picker rows place a bare icon before a `v_flex` of
name-over-path. The sites need different insertion points.

### Widen `icon_for_remote_connection`'s return type

Return a `{ icon, tunneled }` struct instead of an `IconName`. Rejected: it
changes a signature used by four call sites and by
`this_window_project_icons_use_each_project_group_host`, and each site must
still render the marker itself, so nothing is saved.

### Shared `RenderOnce` badge component

One component rendering icon and marker together. Rejected: the sites wrap
their icons differently enough (`IconWithIndicator` in the title bar, bare
`Icon` in picker rows) that the component would need enough configuration
knobs to be harder to follow than the call sites it replaced.

## Design

### Marker glyph

`⇄` (U+21C4), rendered as a `Label` in the UI font.

A text symbol rather than an SVG icon is a deliberate trade. `.FlintSans`
resolves to the bundled IBM Plex Sans (`crates/gpui/src/text_system.rs:1185`),
which contains U+21C4. However `ui_font_family` is user-configurable
(`assets/settings/default.json:57`), so a user on a font lacking the glyph gets
GPUI's fallback, which may render at a different weight or optical size than
the surrounding text. This is accepted; the SVG alternative
(`assets/icons/link.svg`) remains available if the fallback proves a problem in
practice.

The label must not use `buffer_font`, which would compound the mismatch.

### Shared predicate

In `crates/recent_projects/src/recent_projects.rs`, beside
`icon_for_remote_connection`:

```rust
pub const TUNNELED_ROUTE_MARKER: &str = "⇄";
pub const TUNNELED_ROUTE_TOOLTIP: &str = "Tunneled agent route";

pub fn remote_connection_is_tunneled(
    options: Option<&RemoteConnectionOptions>,
    cx: &App,
) -> bool {
    let Some(options) = options else { return false };
    RemoteSettings::get_global(cx).agent_route_for(options)
        == Some(RemoteAgentRoute::Tunneled)
}
```

Returning `bool` rather than `Option<SharedString>` lets call sites read as
`.when(is_tunneled, ...)`, and keeps the glyph and hover text as consts so both
are defined once.

`RemoteSettings::agent_route_for` already exists and is tested
(`crates/recent_projects/src/remote_connections.rs:80`). It returns `None` for
non-SSH connections and `Some(Direct)` for SSH hosts absent from
`remote.ssh_connections`. Three inputs therefore collapse to `false`: local
projects, WSL and Docker connections, and anything Direct.

No new crate dependency is required. `title_bar` already depends on
`recent_projects`.

Settings changes call `cx.refresh_windows()`
(`crates/settings/src/settings_store.rs:400`), so reading the route during
render is reactive with no subscription.

### Rendering

All sites use:

```rust
Label::new(TUNNELED_ROUTE_MARKER)
    .size(LabelSize::Small)
    .color(Color::Muted)
```

`Color::Muted` matches how git branch names and other secondary metadata
already render in these rows.

**Title bar** (`title_bar.rs:502-520`). The `h_flex` currently carries
`max_w_32`, capping icon and nickname together. That cap moves inward to wrap
only those two, and the marker becomes a sibling outside it, so the nickname
keeps its existing width budget:

```
[⛁•] build-host ⇄
 └─ max_w_32 ─┘└new┘
```

`remote_connection_options` returns an owned value that the existing `match` at
line 447 consumes, so the predicate must be evaluated before that match.

**Picker rows**, each needing `flex_shrink_0` because two of them sit inside
flex-grow containers:

- **This Window** (`recent_projects.rs:1313`) — into the name row, after the
  branch name, from `key.host().as_ref()`.
- **Recent Projects** (`recent_projects.rs:1617`) — sibling after the
  highlighted match, from the `SerializedWorkspaceLocation` match already
  present at line 1596.

### Hover text

A bare symbol is not self-explanatory, so every instance is hoverable.

Picker rows wrap the marker in `div().id(...).tooltip(...)` reading
`TUNNELED_ROUTE_TOOLTIP`. Hovering the symbol directly is the discoverable
gesture, and the inner tooltip takes precedence over the row's own.

The title bar cannot do this: its marker sits inside a `trigger_with_tooltip`
trigger, where a nested tooltip does not behave. There the route folds into the
existing tooltip meta instead, appended only when tunneled:

```
Connected to: build-host · Tunneled agent route
```

## Testing

Tests for `remote_connection_is_tunneled` go in the existing
`recent_projects.rs` test module, beside
`this_window_project_icons_use_each_project_group_host` (line 2616). Settings
are injected with the `SettingsStore::update_global` /
`store.update_user_settings` pattern already used at line 3048.

| Input | Expected |
| --- | --- |
| SSH host configured `agent_route: "tunneled"` | `true` |
| SSH host configured `agent_route: "direct"` | `false` |
| SSH host absent from `ssh_connections` | `false` |
| WSL connection | `false` |
| Docker connection | `false` |
| `None` (local project) | `false` |

The last four rows pin down the intentional omissions that the repository's
Agent Threads guidance requires be represented as explicit, tested
capabilities. The absent-host case matters most: it is the one that could
regress into a false marker, since an unconfigured host still resolves to
`Some(Direct)` rather than `None`.

## Out of Scope

- Any way to change the route from these surfaces. The marker is read-only;
  the route remains editable only through settings and the Settings Editor
  control.
- Marking `Direct` explicitly. Absence of the marker is the signal.
- Wiring up `SidebarRecentProjects`.

## Release Note for the Implementation PR

The pull request implementing this design should carry:

```
Release Notes:

- Added a `⇄` indicator marking remote projects that use the tunneled agent
  route, in the title bar and the project picker.
```
