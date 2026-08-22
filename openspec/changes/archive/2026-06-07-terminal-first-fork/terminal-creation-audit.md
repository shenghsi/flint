# Terminal Creation Path Audit

This audit traces the current terminal creation paths before making terminal
items the default workspace surface.

## Existing Center Terminal Path

- `terminal_view::init` registers `TerminalView::deploy` for
  `workspace::NewCenterTerminal`.
- `TerminalView::deploy` resolves the default working directory and calls
  `TerminalPanel::add_center_terminal`.
- `TerminalPanel::add_center_terminal` creates a `TerminalView` and adds it to
  the active center pane with `workspace.add_item_to_active_pane`.
- `TerminalView` implements `workspace::Item` and uses terminal title/task state
  for tab content.

This is the path to promote as the default terminal creation behavior.

## Existing Panel Terminal Path

- `terminal_panel::init` registers `TerminalPanel::new_terminal` for
  `workspace::NewTerminal` and `TerminalPanel::open_terminal` for
  `workspace::OpenTerminal`.
- `TerminalPanel::new_terminal` opens a center terminal only when the active
  center pane already has focused terminal content.
- Otherwise `TerminalPanel::new_terminal` resolves the terminal panel and calls
  `add_terminal_shell` or `add_local_terminal_shell`.
- `add_terminal_shell_internal` creates a `TerminalView` but inserts it into the
  terminal panel's internal pane group and focuses/opens the terminal panel
  according to `RevealStrategy`.

This is the bottom-dock-first behavior the fork should stop exposing as the
default user path.

## Task Spawning

- `TerminalPanel::spawn_task` reuses or replaces terminal items associated with
  a task label.
- `spawn_in_new_terminal` already respects `RevealTarget::Center` by delegating
  to `TerminalPanel::add_center_terminal`.
- `RevealTarget::Dock` still routes through panel-specific terminal task
  creation.

Task spawning can preserve existing behavior initially, but terminal-first task
launching should prefer `RevealTarget::Center`.

## Keymaps and Menus

- Default keymaps bind common terminal shortcuts to `workspace::NewTerminal`,
  not `workspace::NewCenterTerminal`.
- `TerminalView` and terminal/pane context menus expose both `NewTerminal` and
  `NewCenterTerminal` actions.
- App menus currently route the visible terminal entry through
  `terminal_panel::ToggleFocus`, even after its label was renamed to
  `Terminal`.

The first terminal behavior change should make `workspace::NewTerminal` open a
center item by default so existing keybindings become terminal-first without
requiring broad keymap churn.
