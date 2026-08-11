use flint_actions::dev;
use gpui::{App, Menu, MenuItem, OsAction, SharedString};
use release_channel::ReleaseChannel;
use terminal_view::terminal_panel;

fn menu_text(cx: &App, identifier: &'static str) -> SharedString {
    localization::text(cx, identifier)
}

pub fn app_menus(cx: &mut App) -> Vec<Menu> {
    use flint_actions::Quit;

    let mut view_items = vec![
        MenuItem::action(
            menu_text(cx, "menu-zoom-in"),
            flint_actions::IncreaseBufferFontSize { persist: false },
        ),
        MenuItem::action(
            menu_text(cx, "menu-zoom-out"),
            flint_actions::DecreaseBufferFontSize { persist: false },
        ),
        MenuItem::action(
            menu_text(cx, "menu-reset-zoom"),
            flint_actions::ResetBufferFontSize { persist: false },
        ),
        MenuItem::action(
            menu_text(cx, "menu-reset-all-zoom"),
            flint_actions::ResetAllZoom { persist: false },
        ),
        MenuItem::separator(),
        MenuItem::action(
            menu_text(cx, "menu-toggle-left-dock"),
            workspace::ToggleLeftDock,
        ),
        MenuItem::action(
            menu_text(cx, "menu-toggle-right-dock"),
            workspace::ToggleRightDock,
        ),
        MenuItem::action(
            menu_text(cx, "menu-toggle-bottom-dock"),
            workspace::ToggleBottomDock,
        ),
        MenuItem::action(
            menu_text(cx, "menu-toggle-all-docks"),
            workspace::ToggleAllDocks,
        ),
        MenuItem::submenu(Menu {
            name: menu_text(cx, "menu-editor-layout"),
            disabled: false,
            items: vec![
                MenuItem::action(
                    menu_text(cx, "menu-split-up"),
                    workspace::SplitUp::default(),
                ),
                MenuItem::action(
                    menu_text(cx, "menu-split-down"),
                    workspace::SplitDown::default(),
                ),
                MenuItem::action(
                    menu_text(cx, "menu-split-left"),
                    workspace::SplitLeft::default(),
                ),
                MenuItem::action(
                    menu_text(cx, "menu-split-right"),
                    workspace::SplitRight::default(),
                ),
            ],
        }),
        MenuItem::separator(),
        MenuItem::action(
            menu_text(cx, "menu-project-panel"),
            flint_actions::project_panel::ToggleFocus,
        ),
        MenuItem::action(
            menu_text(cx, "menu-outline-panel"),
            outline_panel::ToggleFocus,
        ),
        MenuItem::action(menu_text(cx, "menu-terminal"), terminal_panel::ToggleFocus),
        MenuItem::separator(),
        MenuItem::action(
            menu_text(cx, "menu-extensions"),
            flint_actions::Extensions::default(),
        ),
        MenuItem::action(
            menu_text(cx, "menu-agent-threads"),
            flint_actions::agent_threads::ToggleFocus,
        ),
        MenuItem::action(
            menu_text(cx, "menu-new-codex-thread"),
            agent_threads::NewCodexThread,
        ),
        MenuItem::action(
            menu_text(cx, "menu-new-claude-thread"),
            agent_threads::NewClaudeThread,
        ),
        MenuItem::action(
            menu_text(cx, "menu-new-pi-thread"),
            agent_threads::NewPiThread,
        ),
        MenuItem::action(
            menu_text(cx, "menu-new-opencode-thread"),
            agent_threads::NewOpenCodeThread,
        ),
        MenuItem::separator(),
        MenuItem::action(menu_text(cx, "menu-diagnostics"), diagnostics::Deploy),
        MenuItem::separator(),
    ];

    if ReleaseChannel::try_global(cx) == Some(ReleaseChannel::Dev) {
        view_items.push(MenuItem::action(
            menu_text(cx, "menu-toggle-gpui-inspector"),
            dev::ToggleInspector,
        ));
        view_items.push(MenuItem::separator());
    }

    vec![
        Menu {
            name: menu_text(cx, "menu-flint"),
            disabled: false,
            items: vec![
                MenuItem::action(menu_text(cx, "menu-about-flint"), flint_actions::About),
                MenuItem::action(menu_text(cx, "menu-check-for-updates"), auto_update::Check),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new(menu_text(cx, "menu-settings")).items([
                    MenuItem::action(
                        menu_text(cx, "menu-open-settings-file"),
                        super::OpenSettingsFile,
                    ),
                    MenuItem::action(
                        menu_text(cx, "menu-open-project-settings-file"),
                        super::OpenProjectSettingsFile,
                    ),
                    MenuItem::action(
                        menu_text(cx, "menu-open-default-settings"),
                        super::OpenDefaultSettings,
                    ),
                    MenuItem::separator(),
                    MenuItem::action(menu_text(cx, "menu-open-keymap"), flint_actions::OpenKeymap),
                    MenuItem::action(
                        menu_text(cx, "menu-open-keymap-file"),
                        flint_actions::OpenKeymapFile,
                    ),
                    MenuItem::action(
                        menu_text(cx, "menu-open-default-key-bindings"),
                        flint_actions::OpenDefaultKeymap,
                    ),
                    MenuItem::separator(),
                    MenuItem::action(
                        menu_text(cx, "menu-select-theme"),
                        flint_actions::theme_selector::Toggle::default(),
                    ),
                    MenuItem::action(
                        menu_text(cx, "menu-select-icon-theme"),
                        flint_actions::icon_theme_selector::Toggle::default(),
                    ),
                ])),
                MenuItem::separator(),
                #[cfg(target_os = "macos")]
                MenuItem::os_submenu(
                    menu_text(cx, "menu-services"),
                    gpui::SystemMenuType::Services,
                ),
                MenuItem::separator(),
                #[cfg(not(target_os = "windows"))]
                MenuItem::action(
                    menu_text(cx, "menu-install-cli"),
                    install_cli::InstallCliBinary,
                ),
                MenuItem::separator(),
                #[cfg(target_os = "macos")]
                MenuItem::action(menu_text(cx, "menu-hide-flint"), super::Hide),
                #[cfg(target_os = "macos")]
                MenuItem::action(menu_text(cx, "menu-hide-others"), super::HideOthers),
                #[cfg(target_os = "macos")]
                MenuItem::action(menu_text(cx, "menu-show-all"), super::ShowAll),
                MenuItem::separator(),
                MenuItem::action(menu_text(cx, "menu-quit-flint"), Quit),
            ],
        },
        Menu {
            name: menu_text(cx, "menu-file"),
            disabled: false,
            items: vec![
                MenuItem::action(menu_text(cx, "menu-new"), workspace::NewFile),
                MenuItem::action(menu_text(cx, "menu-new-window"), workspace::NewWindow),
                MenuItem::separator(),
                #[cfg(not(target_os = "macos"))]
                MenuItem::action(menu_text(cx, "menu-open-file"), workspace::OpenFiles),
                MenuItem::action(
                    if cfg!(not(target_os = "macos")) {
                        menu_text(cx, "menu-open-folder")
                    } else {
                        menu_text(cx, "menu-open")
                    },
                    workspace::Open::default(),
                ),
                MenuItem::action(
                    menu_text(cx, "menu-open-recent"),
                    flint_actions::OpenRecent {
                        create_new_window: false,
                    },
                ),
                MenuItem::action(
                    menu_text(cx, "menu-open-remote"),
                    flint_actions::OpenRemote {
                        create_new_window: false,
                        from_existing_connection: false,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action(
                    menu_text(cx, "menu-add-folder-to-project"),
                    workspace::AddFolderToProject,
                ),
                MenuItem::separator(),
                MenuItem::action(
                    menu_text(cx, "menu-save"),
                    workspace::Save { save_intent: None },
                ),
                MenuItem::action(menu_text(cx, "menu-save-as"), workspace::SaveAs),
                MenuItem::action(
                    menu_text(cx, "menu-save-all"),
                    workspace::SaveAll { save_intent: None },
                ),
                MenuItem::separator(),
                MenuItem::action(
                    menu_text(cx, "menu-close-editor"),
                    workspace::CloseActiveItem {
                        save_intent: None,
                        close_pinned: true,
                    },
                ),
                MenuItem::action(menu_text(cx, "menu-close-project"), workspace::CloseProject),
                MenuItem::action(menu_text(cx, "menu-close-window"), workspace::CloseWindow),
            ],
        },
        Menu {
            name: menu_text(cx, "menu-edit"),
            disabled: false,
            items: vec![
                MenuItem::os_action(
                    menu_text(cx, "menu-undo"),
                    editor::actions::Undo,
                    OsAction::Undo,
                ),
                MenuItem::os_action(
                    menu_text(cx, "menu-redo"),
                    editor::actions::Redo,
                    OsAction::Redo,
                ),
                MenuItem::separator(),
                MenuItem::os_action(
                    menu_text(cx, "menu-cut"),
                    editor::actions::Cut,
                    OsAction::Cut,
                ),
                MenuItem::os_action(
                    menu_text(cx, "menu-copy"),
                    editor::actions::Copy,
                    OsAction::Copy,
                ),
                MenuItem::action(
                    menu_text(cx, "menu-copy-and-trim"),
                    editor::actions::CopyAndTrim,
                ),
                MenuItem::os_action(
                    menu_text(cx, "menu-paste"),
                    editor::actions::Paste,
                    OsAction::Paste,
                ),
                MenuItem::separator(),
                MenuItem::action(
                    menu_text(cx, "menu-find"),
                    search::buffer_search::Deploy::find(),
                ),
                MenuItem::action(
                    menu_text(cx, "menu-find-in-project"),
                    workspace::DeploySearch::default(),
                ),
                MenuItem::action(
                    menu_text(cx, "menu-text-finder"),
                    flint_actions::text_finder::Toggle,
                ),
                MenuItem::separator(),
                MenuItem::action(
                    menu_text(cx, "menu-toggle-line-comment"),
                    editor::actions::ToggleComments::default(),
                ),
            ],
        },
        Menu {
            name: menu_text(cx, "menu-selection"),
            disabled: false,
            items: vec![
                MenuItem::os_action(
                    menu_text(cx, "menu-select-all"),
                    editor::actions::SelectAll,
                    OsAction::SelectAll,
                ),
                MenuItem::action(
                    menu_text(cx, "menu-expand-selection"),
                    editor::actions::SelectLargerSyntaxNode,
                ),
                MenuItem::action(
                    menu_text(cx, "menu-shrink-selection"),
                    editor::actions::SelectSmallerSyntaxNode,
                ),
                MenuItem::action(
                    menu_text(cx, "menu-select-next-sibling"),
                    editor::actions::SelectNextSyntaxNode,
                ),
                MenuItem::action(
                    menu_text(cx, "menu-select-previous-sibling"),
                    editor::actions::SelectPreviousSyntaxNode,
                ),
                MenuItem::separator(),
                MenuItem::action(
                    menu_text(cx, "menu-add-cursor-above"),
                    editor::actions::AddSelectionAbove {
                        skip_soft_wrap: true,
                    },
                ),
                MenuItem::action(
                    menu_text(cx, "menu-add-cursor-below"),
                    editor::actions::AddSelectionBelow {
                        skip_soft_wrap: true,
                    },
                ),
                MenuItem::action(
                    menu_text(cx, "menu-select-next-occurrence"),
                    editor::actions::SelectNext {
                        replace_newest: false,
                    },
                ),
                MenuItem::action(
                    menu_text(cx, "menu-select-previous-occurrence"),
                    editor::actions::SelectPrevious {
                        replace_newest: false,
                    },
                ),
                MenuItem::action(
                    menu_text(cx, "menu-select-all-occurrences"),
                    editor::actions::SelectAllMatches,
                ),
                MenuItem::separator(),
                MenuItem::action(
                    menu_text(cx, "menu-move-line-up"),
                    editor::actions::MoveLineUp,
                ),
                MenuItem::action(
                    menu_text(cx, "menu-move-line-down"),
                    editor::actions::MoveLineDown,
                ),
                MenuItem::action(
                    menu_text(cx, "menu-duplicate-selection"),
                    editor::actions::DuplicateLineDown,
                ),
            ],
        },
        Menu {
            name: menu_text(cx, "menu-view"),
            disabled: false,
            items: view_items,
        },
        Menu {
            name: menu_text(cx, "menu-go"),
            disabled: false,
            items: vec![
                MenuItem::action(menu_text(cx, "menu-back"), workspace::GoBack),
                MenuItem::action(menu_text(cx, "menu-forward"), workspace::GoForward),
                MenuItem::separator(),
                MenuItem::action(
                    menu_text(cx, "menu-command-palette"),
                    flint_actions::command_palette::Toggle,
                ),
                MenuItem::separator(),
                MenuItem::action(
                    menu_text(cx, "menu-go-to-file"),
                    workspace::ToggleFileFinder::default(),
                ),
                MenuItem::action(
                    menu_text(cx, "menu-go-to-symbol-in-editor"),
                    flint_actions::outline::ToggleOutline,
                ),
                MenuItem::action(
                    menu_text(cx, "menu-go-to-line-column"),
                    editor::actions::ToggleGoToLine,
                ),
                MenuItem::separator(),
                MenuItem::action(
                    menu_text(cx, "menu-go-to-definition"),
                    editor::actions::GoToDefinition::default(),
                ),
                MenuItem::action(
                    menu_text(cx, "menu-go-to-declaration"),
                    editor::actions::GoToDeclaration,
                ),
                MenuItem::action(
                    menu_text(cx, "menu-go-to-type-definition"),
                    editor::actions::GoToTypeDefinition,
                ),
                MenuItem::action(
                    menu_text(cx, "menu-find-all-references"),
                    editor::actions::FindAllReferences::default(),
                ),
                MenuItem::separator(),
                MenuItem::action(
                    menu_text(cx, "menu-next-problem"),
                    editor::actions::GoToDiagnostic::default(),
                ),
                MenuItem::action(
                    menu_text(cx, "menu-previous-problem"),
                    editor::actions::GoToPreviousDiagnostic::default(),
                ),
            ],
        },
        Menu {
            name: menu_text(cx, "menu-run"),
            disabled: false,
            items: vec![
                MenuItem::action(
                    menu_text(cx, "menu-spawn-task"),
                    flint_actions::Spawn::ViaModal {
                        reveal_target: None,
                    },
                ),
                MenuItem::separator(),
                MenuItem::action(
                    menu_text(cx, "menu-edit-tasks-json"),
                    crate::flint::OpenProjectTasks,
                ),
            ],
        },
        Menu {
            name: menu_text(cx, "menu-window"),
            disabled: false,
            items: vec![
                MenuItem::action(menu_text(cx, "menu-minimize"), super::Minimize),
                MenuItem::action(menu_text(cx, "menu-window-zoom"), super::Zoom),
                MenuItem::separator(),
            ],
        },
        Menu {
            name: menu_text(cx, "menu-help"),
            disabled: false,
            items: vec![
                MenuItem::action(
                    menu_text(cx, "menu-documentation"),
                    super::OpenBrowser {
                        url: super::DOCS_URL.into(),
                    },
                ),
                MenuItem::action(
                    menu_text(cx, "menu-flint-repository"),
                    feedback::OpenFlintRepo,
                ),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menus(cx: &mut gpui::TestAppContext) -> Vec<Menu> {
        cx.update(|cx| {
            if !cx.has_global::<localization::Localization>() {
                localization::init(localization::UiLanguage::English, cx)
                    .expect("test localization must load");
            }
            app_menus(cx)
        })
    }

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
        let menus = menus(cx);
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

    #[gpui::test]
    fn view_menu_exposes_every_agent_thread_action(cx: &mut gpui::TestAppContext) {
        let menus = menus(cx);
        let mut labels = Vec::new();
        collect_menu_labels(&menus, &mut labels);

        for label in [
            "New Codex Thread",
            "New Claude Thread",
            "New Pi Thread",
            "New OpenCode Thread",
        ] {
            assert!(
                labels.iter().any(|candidate| candidate == label),
                "missing first-class agent action {label:?}"
            );
        }
    }

    #[gpui::test]
    fn menus_use_the_selected_language(cx: &mut gpui::TestAppContext) {
        let _ = menus(cx);
        let menus = cx.update(|cx| {
            localization::set_language(localization::UiLanguage::SimplifiedChinese, cx);
            app_menus(cx)
        });
        let mut labels = Vec::new();
        collect_menu_labels(&menus, &mut labels);

        for label in ["文件", "编辑", "设置", "命令面板...", "新建 Codex 线程"] {
            assert!(labels.iter().any(|candidate| candidate == label));
        }
    }
}
