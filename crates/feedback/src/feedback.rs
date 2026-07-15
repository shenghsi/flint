use extension_host::ExtensionStore;
use flint_actions::feedback::{EmailFlint, FileBugReport, RequestFeature};
use gpui::{App, ClipboardItem, PromptLevel, actions};
use system_specs::{CopySystemSpecsIntoClipboard, SystemSpecs, os_name, os_version};
use util::ResultExt;
use workspace::Workspace;

actions!(
    flint,
    [
        /// Opens the Flint repository on GitHub.
        OpenFlintRepo,
        /// Copies installed extensions to the clipboard for bug reports.
        CopyInstalledExtensionsIntoClipboard
    ]
);

const ZED_REPO_URL: &str = "https://github.com/shenghsi/flint";

const REQUEST_FEATURE_URL: &str = "https://github.com/shenghsi/flint/discussions/new/choose";

fn file_bug_report_url(specs: &SystemSpecs) -> String {
    format!(
        concat!(
            "https://github.com/shenghsi/flint/issues/new",
            "?",
            "template=10_bug_report.yml",
            "&",
            "environment={}"
        ),
        urlencoding::encode(&specs.to_string())
    )
}

fn email_flint_url(specs: &SystemSpecs) -> String {
    format!(
        concat!(
            "https://github.com/shenghsi/flint/issues/new",
            "?",
            "body={}"
        ),
        email_body(specs)
    )
}

fn email_body(specs: &SystemSpecs) -> String {
    let body = format!("\n\nSystem Information:\n\n{}", specs);
    urlencoding::encode(&body).to_string()
}

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace
            .register_action(|_, _: &CopySystemSpecsIntoClipboard, window, cx| {
                let specs = SystemSpecs::new(window, cx, os_name(), os_version());

                cx.spawn_in(window, async move |_, cx| {
                    let specs = specs.await.to_string();

                    cx.update(|_, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(specs.clone()))
                    })
                    .log_err();

                    cx.prompt(
                        PromptLevel::Info,
                        "Copied into clipboard",
                        Some(&specs),
                        &["OK"],
                    )
                    .await
                })
                .detach();
            })
            .register_action(|_, _: &CopyInstalledExtensionsIntoClipboard, window, cx| {
                let clipboard_text = format_installed_extensions_for_clipboard(cx);
                cx.write_to_clipboard(ClipboardItem::new_string(clipboard_text.clone()));
                drop(window.prompt(
                    PromptLevel::Info,
                    "Copied into clipboard",
                    Some(&clipboard_text),
                    &["OK"],
                    cx,
                ));
            })
            .register_action(|_, _: &RequestFeature, _, cx| {
                cx.open_url(REQUEST_FEATURE_URL);
            })
            .register_action(move |_, _: &FileBugReport, window, cx| {
                let specs = SystemSpecs::new(window, cx, os_name(), os_version());
                cx.spawn_in(window, async move |_, cx| {
                    let specs = specs.await;
                    cx.update(|_, cx| {
                        cx.open_url(&file_bug_report_url(&specs));
                    })
                    .log_err();
                })
                .detach();
            })
            .register_action(move |_, _: &EmailFlint, window, cx| {
                let specs = SystemSpecs::new(window, cx, os_name(), os_version());
                cx.spawn_in(window, async move |_, cx| {
                    let specs = specs.await;
                    cx.update(|_, cx| {
                        cx.open_url(&email_flint_url(&specs));
                    })
                    .log_err();
                })
                .detach();
            })
            .register_action(move |_, _: &OpenFlintRepo, _, cx| {
                cx.open_url(ZED_REPO_URL);
            });
    })
    .detach();
}

fn format_installed_extensions_for_clipboard(cx: &mut App) -> String {
    let store = ExtensionStore::global(cx);
    let store = store.read(cx);
    let mut lines = Vec::with_capacity(store.extension_index.extensions.len());

    for (extension_id, entry) in store.extension_index.extensions.iter() {
        let line = format!(
            "- {} ({}) v{}{}",
            entry.manifest.name,
            extension_id,
            entry.manifest.version,
            if entry.dev { " (dev)" } else { "" }
        );
        lines.push(line);
    }

    lines.sort();

    if lines.is_empty() {
        return "No extensions installed.".to_string();
    }

    format!(
        "Installed extensions ({}):\n{}",
        lines.len(),
        lines.join("\n")
    )
}
