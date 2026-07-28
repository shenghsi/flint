use flint_actions::dev;
use gpui::{App, Menu, MenuItem, OsAction};
use release_channel::ReleaseChannel;
use terminal_view::terminal_panel;

pub fn app_menus(cx: &mut App) -> Vec<Menu> {
    use flint_actions::Quit;

    let mut view_items = vec![
        MenuItem::action(
            "Zoom In",
            flint_actions::IncreaseBufferFontSize { persist: false },
        ),
        MenuItem::action(
            "Zoom Out",
            flint_actions::DecreaseBufferFontSize { persist: false },
        ),
        MenuItem::action(
            "Reset Zoom",
            flint_actions::ResetBufferFontSize { persist: false },
        ),
        MenuItem::action(
            "Reset All Zoom",
            flint_actions::ResetAllZoom { persist: false },
        ),
        MenuItem::separator(),
        MenuItem::action("Toggle Left Dock", workspace::ToggleLeftDock),
        MenuItem::action("Toggle Right Dock", workspace::ToggleRightDock),
        MenuItem::action("Toggle Bottom Dock", workspace::ToggleBottomDock),
        MenuItem::action("Toggle All Docks", workspace::ToggleAllDocks),
        MenuItem::submenu(Menu {
            name: "Editor Layout".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Split Up", workspace::SplitUp::default()),
                MenuItem::action("Split Down", workspace::SplitDown::default()),
                MenuItem::action("Split Left", workspace::SplitLeft::default()),
                MenuItem::action("Split Right", workspace::SplitRight::default()),
            ],
        }),
        MenuItem::separator(),
        MenuItem::action("Project Panel", flint_actions::project_panel::ToggleFocus),
        MenuItem::action("Outline Panel", outline_panel::ToggleFocus),
        MenuItem::action("Terminal", terminal_panel::ToggleFocus),
        MenuItem::separator(),
        MenuItem::action("Extensions", flint_actions::Extensions::default()),
        MenuItem::action("Agent Threads", flint_actions::agent_threads::ToggleFocus),
        MenuItem::action("New Codex Thread", agent_threads::NewCodexThread),
        MenuItem::action("New Claude Thread", agent_threads::NewClaudeThread),
        MenuItem::separator(),
        MenuItem::action("Diagnostics", diagnostics::Deploy),
        MenuItem::separator(),
    ];

    if ReleaseChannel::try_global(cx) == Some(ReleaseChannel::Dev) {
        view_items.push(MenuItem::action(
            "Toggle GPUI Inspector",
            dev::ToggleInspector,
        ));
        view_items.push(MenuItem::separator());
    }

    vec![
        Menu {
            name: "Flint".into(),
            disabled: false,
            items: vec![
                MenuItem::action("About Flint", flint_actions::About),
                MenuItem::action("Check for Updates", auto_update::Check),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Settings").items([
                    MenuItem::action("Open Settings File", super::OpenSettingsFile),
                    MenuItem::action("Open Project Settings File", super::OpenProjectSettingsFile),
                    MenuItem::action("Open Default Settings", super::OpenDefaultSettings),
                    MenuItem::separator(),
                    MenuItem::action("Open Keymap", flint_actions::OpenKeymap),
                    MenuItem::action("Open Keymap File", flint_actions::OpenKeymapFile),
                    MenuItem::action(
                        "Open Default Key Bindings",
                        flint_actions::OpenDefaultKeymap,
                    ),
                    MenuItem::separator(),
                    MenuItem::action(
                        "Select Theme...",
                        flint_actions::theme_selector::Toggle::default(),
                    ),
                    MenuItem::action(
                        "Select Icon Theme...",
                        flint_actions::icon_theme_selector::Toggle::default(),
                    ),
                ])),
                MenuItem::separator(),
                #[cfg(target_os = "macos")]
                MenuItem::os_submenu("Services", gpui::SystemMenuType::Services),
                MenuItem::separator(),
                #[cfg(not(target_os = "windows"))]
                MenuItem::action("Install CLI", install_cli::InstallCliBinary),
                MenuItem::separator(),
                #[cfg(target_os = "macos")]
                MenuItem::action("Hide Flint", super::Hide),
                #[cfg(target_os = "macos")]
                MenuItem::action("Hide Others", super::HideOthers),
                #[cfg(target_os = "macos")]
                MenuItem::action("Show All", super::ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit Flint", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            disabled: false,
            items: vec![
                MenuItem::action("New", workspace::NewFile),
                MenuItem::action("New Window", workspace::NewWindow),
                MenuItem::separator(),
                #[cfg(not(target_os = "macos"))]
                MenuItem::action("Open File...", workspace::OpenFiles),
                MenuItem::action(
                    if cfg!(not(target_os = "macos")) {
                        "Open Folder..."
                    } else {
                        "Open…"
                    },
                    workspace::Open::default(),
                ),
                MenuItem::action(
                    "Open Recent...",
                    flint_actions::OpenRecent {
                        create_new_window: false,
                    },
                ),
                MenuItem::action(
                    "Open Remote...",
                    flint_actions::OpenRemote {
                        create_new_window: false,
                        from_existing_connection: false,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action("Add Folder to Project…", workspace::AddFolderToProject),
                MenuItem::separator(),
                MenuItem::action("Save", workspace::Save { save_intent: None }),
                MenuItem::action("Save As…", workspace::SaveAs),
                MenuItem::action("Save All", workspace::SaveAll { save_intent: None }),
                MenuItem::separator(),
                MenuItem::action(
                    "Close Editor",
                    workspace::CloseActiveItem {
                        save_intent: None,
                        close_pinned: true,
                    },
                ),
                MenuItem::action("Close Project", workspace::CloseProject),
                MenuItem::action("Close Window", workspace::CloseWindow),
            ],
        },
        Menu {
            name: "Edit".into(),
            disabled: false,
            items: vec![
                MenuItem::os_action("Undo", editor::actions::Undo, OsAction::Undo),
                MenuItem::os_action("Redo", editor::actions::Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", editor::actions::Cut, OsAction::Cut),
                MenuItem::os_action("Copy", editor::actions::Copy, OsAction::Copy),
                MenuItem::action("Copy and Trim", editor::actions::CopyAndTrim),
                MenuItem::os_action("Paste", editor::actions::Paste, OsAction::Paste),
                MenuItem::separator(),
                MenuItem::action("Find", search::buffer_search::Deploy::find()),
                MenuItem::action("Find in Project", workspace::DeploySearch::default()),
                MenuItem::action("Text Finder", flint_actions::text_finder::Toggle),
                MenuItem::separator(),
                MenuItem::action(
                    "Toggle Line Comment",
                    editor::actions::ToggleComments::default(),
                ),
            ],
        },
        Menu {
            name: "Selection".into(),
            disabled: false,
            items: vec![
                MenuItem::os_action(
                    "Select All",
                    editor::actions::SelectAll,
                    OsAction::SelectAll,
                ),
                MenuItem::action("Expand Selection", editor::actions::SelectLargerSyntaxNode),
                MenuItem::action("Shrink Selection", editor::actions::SelectSmallerSyntaxNode),
                MenuItem::action("Select Next Sibling", editor::actions::SelectNextSyntaxNode),
                MenuItem::action(
                    "Select Previous Sibling",
                    editor::actions::SelectPreviousSyntaxNode,
                ),
                MenuItem::separator(),
                MenuItem::action(
                    "Add Cursor Above",
                    editor::actions::AddSelectionAbove {
                        skip_soft_wrap: true,
                    },
                ),
                MenuItem::action(
                    "Add Cursor Below",
                    editor::actions::AddSelectionBelow {
                        skip_soft_wrap: true,
                    },
                ),
                MenuItem::action(
                    "Select Next Occurrence",
                    editor::actions::SelectNext {
                        replace_newest: false,
                    },
                ),
                MenuItem::action(
                    "Select Previous Occurrence",
                    editor::actions::SelectPrevious {
                        replace_newest: false,
                    },
                ),
                MenuItem::action("Select All Occurrences", editor::actions::SelectAllMatches),
                MenuItem::separator(),
                MenuItem::action("Move Line Up", editor::actions::MoveLineUp),
                MenuItem::action("Move Line Down", editor::actions::MoveLineDown),
                MenuItem::action("Duplicate Selection", editor::actions::DuplicateLineDown),
            ],
        },
        Menu {
            name: "View".into(),
            disabled: false,
            items: view_items,
        },
        Menu {
            name: "Go".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Back", workspace::GoBack),
                MenuItem::action("Forward", workspace::GoForward),
                MenuItem::separator(),
                MenuItem::action("Command Palette...", flint_actions::command_palette::Toggle),
                MenuItem::separator(),
                MenuItem::action("Go to File...", workspace::ToggleFileFinder::default()),
                // MenuItem::action("Go to Symbol in Project", project_symbols::Toggle),
                MenuItem::action(
                    "Go to Symbol in Editor...",
                    flint_actions::outline::ToggleOutline,
                ),
                MenuItem::action("Go to Line/Column...", editor::actions::ToggleGoToLine),
                MenuItem::separator(),
                MenuItem::action(
                    "Go to Definition",
                    editor::actions::GoToDefinition::default(),
                ),
                MenuItem::action("Go to Declaration", editor::actions::GoToDeclaration),
                MenuItem::action("Go to Type Definition", editor::actions::GoToTypeDefinition),
                MenuItem::action(
                    "Find All References",
                    editor::actions::FindAllReferences::default(),
                ),
                MenuItem::separator(),
                MenuItem::action("Next Problem", editor::actions::GoToDiagnostic::default()),
                MenuItem::action(
                    "Previous Problem",
                    editor::actions::GoToPreviousDiagnostic::default(),
                ),
            ],
        },
        Menu {
            name: "Run".into(),
            disabled: false,
            items: vec![
                MenuItem::action(
                    "Spawn Task",
                    flint_actions::Spawn::ViaModal {
                        reveal_target: None,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action("Edit tasks.json...", crate::flint::OpenProjectTasks),
            ],
        },
        Menu {
            name: "Window".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Minimize", super::Minimize),
                MenuItem::action("Zoom", super::Zoom),
                MenuItem::separator(),
            ],
        },
        Menu {
            name: "Help".into(),
            disabled: false,
            items: vec![
                MenuItem::action(
                    "Documentation",
                    super::OpenBrowser {
                        url: super::DOCS_URL.into(),
                    },
                ),
                MenuItem::action("Flint Repository", feedback::OpenFlintRepo),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_menu_labels(menus: &[Menu], labels: &mut Vec<String>) {
        for menu in menus {
            labels.push(menu.name.to_string());
            collect_menu_item_labels(&menu.items, labels);
        }
    }

    fn collect_menu_item_labels(items: &[MenuItem], labels: &mut Vec<String>) {
        for item in items {
            match item {
                MenuItem::Action { name, .. } => labels.push(name.to_string()),
                MenuItem::Submenu(menu) => {
                    labels.push(menu.name.to_string());
                    collect_menu_item_labels(&menu.items, labels);
                }
                MenuItem::SystemMenu(menu) => labels.push(menu.name.to_string()),
                MenuItem::Separator => {}
            }
        }
    }

    #[gpui::test]
    fn test_terminal_first_menus_omit_retired_surfaces(cx: &mut gpui::TestAppContext) {
        let menus = cx.update(app_menus);
        let mut labels = Vec::new();
        collect_menu_labels(&menus, &mut labels);

        for removed_label in [
            "Collab Panel",
            "Debugger Panel",
            "Terminal Panel",
            "Start Debugger",
            "Edit debug.json...",
            "Continue",
            "Step Over",
            "Step Into",
            "Step Out",
            "Clear All Breakpoints",
            "View Telemetry",
            "Show Welcome",
            "File Bug Report...",
            "Request Feature...",
            "Email Us...",
            "Flint Twitter",
            "Join the Team",
        ] {
            assert!(
                !labels.iter().any(|label| label == removed_label),
                "retired menu label {removed_label:?} should not be present; labels: {labels:?}"
            );
        }

        assert!(labels.iter().any(|label| label == "Terminal"));
        assert!(labels.iter().any(|label| label == "Extensions"));
        assert!(labels.iter().any(|label| label == "Project Panel"));
        assert!(labels.iter().any(|label| label == "Open Settings File"));
        assert!(labels.iter().any(|label| label == "Find in Project"));
    }
}
