# Tunneled Route Indicator Implementation Plan

**Date:** 2026-07-25
**Status:** Draft
**Design:** `2026-07-25-tunneled-route-indicator-design.md`

The change is small enough for a single pull request. It is sequenced below as
ordered commits within that PR rather than separate mergeable phases, because
the predicate has no value without a call site and no existing consumer to
regress.

## Verified preconditions

Confirmed against the working tree before planning:

- **`RemoteSettings::agent_route_for` has no production callers.** The only
  references are its definition (`remote_connections.rs:80`) and its own tests
  (`190`, `204`, `211`). Manage Servers reads `effective_agent_route` directly
  and `agent_threads` carries a parallel `route_for`. This feature is its first
  production consumer, so the allocation fix in step 1 cannot regress anyone,
  and the per-row cost is introduced by this change rather than pre-existing.
- **`recent_projects.rs` already imports what the predicate needs** — `App`
  (gpui import block, `:26-29`) and the `Settings` trait (`:37`). Only
  `RemoteAgentRoute` must be added to scope.
- **`title_bar.rs` already imports `RemoteConnectionOptions`** (`:29`) and
  already references `recent_projects::` for popovers (`:494`, `:649`), so the
  helper import adds no dependency.
- **Both picker rows share one structure** — `h_flex` containing optional icon,
  `highlighted.render(...)`, then a container `.tooltip(...)`: This Window at
  `1451-1472`, Recent Projects at `1602-1630`. Recent Projects additionally has
  `.flex_grow_1()` at `1613`.

## Step 1 — Stop cloning the connection vector

Crate: `crates/recent_projects`.

`agent_route_for` calls `self.ssh_connections()` (`remote_connections.rs:94`),
which clones the whole stored vector (`41-43`). The predicate runs once per
visible picker row, so change it to borrow and scan `self.ssh_connections.0.iter()`.

Behavior-preserving; the existing tests at `169-217` are the regression net. No
new tests.

Do this first so the feature never introduces the hot path in its cloning form.

## Step 2 — Add the shared predicate

Crate: `crates/recent_projects`, in `recent_projects.rs` beside
`icon_for_remote_connection` (`:1942`).

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

`IconName::ArrowRightLeft` already exists (`crates/icons/src/icons.rs:37`,
backed by `assets/icons/arrow_right_left.svg`) and has no other use in the
codebase, so no new asset and no conflicting established meaning. It is the
`⇄` form in Flint's own stroke style.

Do **not** edit the SVG's hardcoded `stroke="black"`. GPUI reduces icon SVGs to
an alpha mask (`crates/gpui/src/svg_renderer.rs:212-216`) and repaints them
with the call site's color, so that attribute never reaches the screen;
`Color::Muted` resolves per theme via `text_muted`
(`crates/ui/src/styles/color.rs:93`). Changing the asset would be a no-op that
implies the opposite.

Add `RemoteAgentRoute` to the `recent_projects.rs` imports (re-exported from
`remote_connections`, which does `pub use settings::{RemoteAgentRoute, SshConnection}`
at `:20`).

Unit tests in the existing test module beside
`this_window_project_icons_use_each_project_group_host` (`:2616`), using the
`SettingsStore::update_global` / `store.update_user_settings` pattern at
`:3048-3052`:

| Input | Expected |
| --- | --- |
| SSH host configured `agent_route: "tunneled"` | `true` |
| SSH host configured `agent_route: "direct"` | `false` |
| SSH host absent from `ssh_connections` | `false` |
| WSL connection | `false` |
| Docker connection | `false` |
| `None` (local project) | `false` |

The last four are the design's intentional omissions as tested capabilities.
The absent-host case is the one that could regress into a false marker, since
an unconfigured host resolves to `Some(Direct)` rather than `None`.

## Step 3 — Render in both picker rows

Crate: `crates/recent_projects`, in `recent_projects.rs`.

Identical insertion in both rows — a sibling child after `highlighted.render(...)`
and before the container `.tooltip(...)`:

```rust
.when(is_tunneled, |this| {
    this.child(
        div()
            .id("tunneled-route-marker")
            .child(tunneled_route_marker())
            .tooltip(Tooltip::text(TUNNELED_ROUTE_TOOLTIP)),
    )
})
```

- **This Window** (`:1462-1471`): source the options from `key.host()`, the same
  value `icon_for_project_group` already consumes at `:1371`.
- **Recent Projects** (`:1614-1623`): source from the `SerializedWorkspaceLocation`
  match already present at `:1596`. Add `flex_shrink_0` here — this row carries
  `.flex_grow_1()` at `:1613` and long paths would otherwise squeeze the marker
  out. This Window has no `flex_grow_1` and does not need it.

Element ids must be unique per row; use the row index the way surrounding code
does.

Do **not** add the marker to the Current Folders row (`:1289-1360`), per the
design — every row there shares one connection, so the marker would be uniform
and disambiguate nothing.

## Step 4 — Render in the title bar

Crate: `crates/title_bar`, in `render_remote_project_connection`
(`title_bar.rs:441`).

1. Evaluate the predicate **before** the `match options` at `:447`, which
   consumes the owned `RemoteConnectionOptions` returned by
   `remote_connection_options` at `:444`.
2. Restructure the trigger's `h_flex` at `:506-519`: move `max_w_32` onto an
   inner `h_flex` wrapping only the `IconWithIndicator` and the nickname
   `Label`, leaving the marker as a sibling outside the cap so the nickname
   keeps its current width budget.
3. Append the route to the existing tooltip meta rather than nesting a tooltip.
   The marker sits inside a `trigger_with_tooltip` trigger
   (`crates/ui/src/components/popover_menu.rs:198-218`), which owns the tooltip
   and suppresses it while the menu is open. Meta becomes
   `Connected to: build-host · Tunneled agent route`, appended only when
   tunneled, across all five `ConnectionState` arms at `:463-477`.

The marker stays visible while reconnecting or disconnected — the route is
configuration, not live state, and the connection dot already carries health.

## Step 5 — Integration tests

Crate: `crates/recent_projects`.

Predicate tests alone would not have caught the placement error this design
went through review to fix, so two behavioral tests are required:

1. **Per-row independence.** A picker holding one tunneled and one direct
   project group marks exactly the tunneled row. This is the regression test
   for targeting the wrong picker section, and is feasible because each
   `ProjectGroupKey` carries its own host (`crates/project/src/project.rs:4916-4927`).
   Extend the fixture style of `this_window_project_icons_use_each_project_group_host`
   (`:2616-2653`), which already builds two groups with differing hosts.
2. **Live update.** With a picker already open, changing `agent_route` in
   settings updates the rendered rows, exercising the `refresh_windows` path
   (`crates/settings/src/settings_store.rs:392-401`) rather than trusting it.

Title bar width behavior and tooltip rendering are left to manual verification;
asserting on laid-out geometry would be brittle relative to its value.

## Manual verification

Against a fresh `/tmp/Flint-Local.app` (`./script/bundle-tmp-app`, checking the
exit code per the repo's build note):

1. A tunneled SSH project shows the marker in the title bar; a direct one shows
   nothing. Check in both a light and a dark theme — the icon is painted from
   `text_muted`, so this confirms the theme path end to end rather than only in
   whichever theme happens to be active.
2. Hovering the title bar trigger shows the route in the tooltip meta.
3. The picker marks the tunneled row in both This Window and Recent Projects,
   and hovering the glyph itself shows "Tunneled agent route".
4. Long paths in Recent Projects do not squeeze the marker out.
5. Changing the route via the Manage Servers dropdown updates both surfaces
   without restarting.
6. A long nickname still truncates rather than pushing the marker off.

## Cross-cutting

- Run `cargo fmt --all -- --check`, `./script/clippy` for `recent_projects` and
  `title_bar`, and both crates' test suites.
- Branch off `main`; never commit to `main` directly.
- PR title: `recent_projects: Mark tunneled remote projects` — imperative, no
  conventional-commit prefix, no trailing punctuation.
- Release notes:

```
Release Notes:

- Added a `⇄` indicator marking remote projects that use the tunneled agent
  route, in the title bar and the project picker.
```

## Open items to resolve during implementation

- `Color::Muted` at `IconSize::Small` may read too faintly. Check both light
  and dark themes during manual verification and adjust to `Color::Accent` if
  the marker does not register at a glance — it is the entire signal, so being
  missable defeats it. The change is confined to `tunneled_route_marker()`.
- Confirm `ArrowRightLeft` is legible at `IconSize::Small`; it carries more
  internal detail than most icons at that size. `IconSize::XSmall` is likely
  too small, but compare against the neighbouring server icon in the title bar.
