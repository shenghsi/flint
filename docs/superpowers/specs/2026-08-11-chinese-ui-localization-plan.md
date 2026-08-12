# Chinese UI Localization Plan

## Goal

Add English (`en-US`) and Simplified Chinese (`zh-CN`) user interfaces to
Flint. English remains the default language. Users can select either language
in the Settings Editor.

The design must permit more languages, such as Traditional Chinese, in the
future. These languages are outside this work.

The language control must remain hidden until the Chinese catalog has full
coverage. This prevents a mixed English and Chinese interface.

## Localization boundary

Translate all text that Flint owns:

- application and native operating system menus;
- Command Palette action names and search terms;
- Settings Editor pages, controls, descriptions, and enum values;
- welcome and onboarding interfaces;
- editor menus, tooltips, status text, dialogs, and notifications;
- Project, Outline, Terminal, Diagnostics, and Git panels;
- search, file finder, tasks, debugger, REPL, and preview interfaces;
- Extensions interface controls;
- Agent Threads and provider configuration interfaces;
- Recent Projects, remote connections, and development containers;
- update, installation, trust, error, About, and license interfaces;
- accessibility labels and screen-reader text;
- CLI and helper-program messages; and
- dates, relative times, counts, file sizes, and plural forms.

Do not translate this data:

- file names, paths, source code, symbols, and branch names;
- terminal process output;
- Git, LSP, DAP, compiler, and tool output;
- extension-provided names and descriptions;
- model responses and external service messages;
- release-note and changelog bodies loaded from GitHub releases;
- JSON keys, action identifiers, command names, and protocol values; and
- logs that developers use for diagnosis.

For an external error, translate the explanation that Flint owns. Preserve the
original error detail.

Translate the controls around release notes in `crates/auto_update_ui`. Show
the GitHub release-note or changelog body without modification.

## Localization foundation

Create one `localization` crate as a new logical component. Set
`[lib] path = "localization.rs"` in its `Cargo.toml`.

Add these catalogs:

```text
assets/locales/en-US/common.ftl
assets/locales/en-US/editor.ftl
assets/locales/en-US/settings.ftl
assets/locales/zh-CN/common.ftl
assets/locales/zh-CN/editor.ftl
assets/locales/zh-CN/settings.ftl
```

Use one file for each product domain. Add files such as `git.ftl`,
`debugger.ftl`, and `agent_threads.ftl` when their migrations start. Load all
files for the selected locale into one bundle at start-up. This layout limits
merge conflicts between crate-based pull requests.

Use Fluent message catalogs. The interface needs variables, selection, and
plural rules.

Add `fluent-bundle` version `0.16.0` and `unic-langid` version `0.9.6` as
workspace dependencies. Both crates use the `Apache-2.0 OR MIT` license. Run
the repository license checks and review the transitive dependency set in the
localization foundation pull request before the migration starts.

Provide a small API:

```rust
tr!("settings.language.title")
tr!("files.selected-count", count = count)
```

The localization service must:

- store the selected language as global application state;
- use English as the fallback catalog;
- report a missing message identifier in development builds;
- support variables, counts, and formatted values;
- resolve text when the interface renders;
- notify all windows when the language changes;
- rebuild native menus and dock menus after a change; and
- rebuild the Command Palette and Settings Editor search indexes.

Parse each locale catalog only once. Cache translated messages that have no
arguments as `SharedString` values. Permit a view to cache translated text
with the current language-generation value. Clear the shared cache and
invalidate view caches when the language generation changes. Format messages
with arguments when their values change. Dense views must not parse Fluent or
allocate the same static label for each rendered row.

Add continuous integration checks for:

- missing Chinese messages;
- unused and duplicate message identifiers;
- different variables in the English and Chinese messages;
- invalid Fluent syntax; and
- user-facing string literals outside an explicit allowlist.

Add `assets/locales/migrated_crates.toml`. The string-literal check applies
only to production source in crates in this file. Each migration pull request
adds its crate after it removes or explicitly allows all user-facing literals.
The list must contain all product interface crates before the language control
becomes visible.

In a release build, a missing Chinese message falls back to English. If both
catalogs lack the message, show its stable message identifier and write one
error to the log. A missing message must not cause a panic or blank interface.

## Language setting

Add a global-only setting:

```json [settings]
{
  "ui_language": "zh-CN"
}
```

Support these values:

- `en-US`
- `zh-CN`

Add the control to **Settings > General > Language**. Show the choices as:

- English
- 简体中文

The setting must:

- exist only in user settings;
- not permit a project to change the application language;
- apply without an application restart;
- persist for the next launch;
- use English as the default for existing and new installations; and
- keep JSON values independent of translated labels.

Add `ui_language` directly to `UserSettingsContent`, outside its flattened
`SettingsContent` field. Do not add it to `SettingsContent` or
`ProjectSettingsContent`. Add a Settings Store accessor, change notification,
and a user-settings file update function for this field. The project settings
schema must reject `ui_language`. Settings profiles and release-channel or
platform overrides must not change it.

Add an exact Settings Editor control for `ui_language`. The control must write
through the new user-settings update function.

## Shared interface systems

Complete shared systems before feature crates. These systems affect most of the
application.

### Native menus

Replace the English labels in `crates/flint/src/flint/app_menus.rs` with
message identifiers.

Rebuild these items after a language change:

- main application menus;
- submenus;
- dock menus;
- Windows jump-list entries; and
- operating system prompt buttons that Flint supplies.

### Command Palette and actions

The Command Palette currently creates English text from Rust action names. Add
an action presentation registry with:

- a stable action identifier;
- a localized title;
- an optional localized description;
- localized search terms; and
- English search aliases when Chinese is active.

Do not change action identifiers or keymap JSON. A Chinese user must be able to
search with Chinese or English terms.

Pinyin search is outside this work. Users can search with Chinese characters,
English terms, stable action identifiers, and stable setting JSON paths. A
later change can add pinyin search after it defines transliteration rules,
ranking, and the effect on index size.

### Settings Editor

Replace static titles and descriptions with localization message identifiers.

Localize:

- page and section names;
- setting titles and descriptions;
- enum display values;
- buttons and tooltips;
- search results; and
- empty and error states.

Settings search must match Chinese text, English source text, and stable JSON
paths.

### Shared components

Localize default text in reusable components:

- buttons;
- pickers and dropdowns;
- context menus;
- alerts and prompts;
- notifications;
- empty states;
- input placeholders;
- pagination and count labels; and
- accessibility labels.

## Product surface migration

Use small crate-based pull requests. Each pull request must migrate its English
strings and its Chinese strings.

Use this order:

1. Application shell, welcome, onboarding, title bar, workspace, and panels.
2. Editor, search, file finder, diagnostics, tasks, and terminal controls.
3. Settings, keymap, theme, icon theme, language, encoding, and line-ending
   selectors.
4. Git, debugger, REPL, snippets, and document preview interfaces.
5. Extensions, remote projects, recent projects, and development containers.
6. Agent Threads, model provider settings, credentials, and plan usage.
7. Auto-update, installation, About, feedback, trust, and failure interfaces.
8. CLI, helper processes, platform-specific prompts, and accessibility text.

Keep proper names such as Flint, Codex, Claude, GitHub, Rust, and JSON unchanged
unless Chinese has a standard display form.

## Chinese layout and text support

GPUI has CJK line-breaking support, but its current fallback stack does not
guarantee Chinese glyph coverage. Add platform fallback names for `PingFang
SC` on macOS, `Microsoft YaHei UI` on Windows, and `Noto Sans CJK SC` on
Linux.

Bundle the required Noto Sans CJK SC regular font data for Linux so a fresh
installation works without a Chinese font package. Load it through GPUI before
the first Chinese interface renders. Include the upstream SIL Open Font
License 1.1 notice in Flint's license assets. Measure and record the app size
change in the pull request. If the size is too large, use an upstream
Simplified Chinese subset that retains all catalog glyphs. Do not create an
unreviewed font subset.

Check:

- Chinese glyph fallback on macOS, Linux, and Windows;
- Chinese input methods in all text inputs;
- cursor movement and text selection;
- line wrapping without spaces;
- menu, button, tab, and dialog width;
- text at all supported UI scale values;
- truncation and tooltip behavior; and
- mixed Chinese and Latin text, paths, and keyboard shortcuts.

Add an internal pseudo-language for development builds. It can expand English
text to find fixed-width layouts. Do not expose it to users.

## Localized formatting

Use the selected UI language for display text such as:

- singular and plural counts;
- relative time;
- dates;
- durations;
- file sizes;
- list separators;
- progress; and
- percentages.

Keep the operating system time-cycle preference, such as 12-hour or 24-hour
time, unless the user has an explicit Flint setting. Update the current locale
handling in `crates/time_format/src/time_format.rs` so language and time-cycle
rules do not conflict.

## Documentation and help routes

Add a Chinese documentation tree and a documentation language selector.

When Chinese is active:

- help links open the Chinese page when it exists;
- a missing Chinese page opens the English page;
- the page identifies English as the fallback; and
- action identifiers, settings JSON, code, and commands remain unchanged.

Do not use the application Fluent catalogs for documentation. Store translated
documentation as Markdown in `docs/src.zh-CN`. Add
`docs/book.zh-CN.toml` with `language = "zh-CN"`, the Chinese source path, and
the `/docs/zh-CN/` site URL. Update the documentation build to build both
books.

Add the language selector to `docs/theme/index.hbs`. It links the English and
Chinese versions of the current page when both versions exist. If a Chinese
page does not exist, it links to the English page and identifies the fallback.
The documentation build must validate the locale page map and broken links.

Use one glossary for the application and documentation.

## Translation process

Create a reviewed glossary before the full translation starts. It must define
terms such as:

- Agent Thread;
- worktree;
- project;
- buffer;
- Command Palette;
- Settings Editor;
- Restricted Mode;
- extension;
- language server;
- debugger; and
- terminal.

Require review by a native Simplified Chinese speaker. Translate complete
messages, not individual words. This preserves correct Chinese sentence order.

Freeze English interface text before the final Chinese review. Any later
English change must update the Chinese catalog in the same pull request.

## Test strategy

Add these test groups:

- catalog parsing and message fallback tests;
- exact English and Chinese output tests;
- variable and plural-rule tests;
- language setting read, write, and persistence tests;
- tests that reject project-level language overrides;
- live language-change tests;
- native menu rebuild tests;
- Command Palette Chinese and English search tests;
- Settings Editor Chinese and English search tests;
- GPUI component tests in both languages;
- visual tests for representative narrow and wide layouts;
- accessibility label tests;
- CJK input, wrapping, and font fallback tests;
- CLI language tests; and
- hard-coded user-text inventory tests.

Before each Rust push, run:

```sh
cargo fmt --all -- --check
./script/clippy
```

Run affected crate tests. Build `/tmp/Flint-Local.app` for final macOS
verification.

## Release sequence

Use feature branches and pull requests. Do not commit to `main`.

Use these release gates:

1. Add the localization framework with only English active.
2. Migrate shared systems and all product crates.
3. Complete the Chinese catalog and native-speaker review.
4. Pass automated catalog coverage with no missing Chinese messages.
5. Pass macOS, Linux, and Windows interface checks.
6. Expose the language control.
7. Publish the feature only after a green Nightly build.

## Completion criteria

The work is complete when:

- users can select English or 简体中文 in the Settings Editor;
- the selection persists and applies without a restart;
- all visible and accessibility text that Flint owns uses the selected
  language;
- Chinese mode contains no unintended English text;
- external content and stable identifiers remain unchanged;
- Command Palette and Settings search work with Chinese and English terms;
- Chinese text renders correctly on all supported platforms; and
- continuous integration prevents new untranslated user-facing strings.
