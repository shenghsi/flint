use std::collections::{HashMap, HashSet};

use anyhow::{Context as _, Result, bail};
pub use fluent_bundle::FluentArgs;
use fluent_bundle::{FluentResource, concurrent::FluentBundle};
use gpui::{App, BorrowAppContext as _, Global, SharedString};
use parking_lot::RwLock;
use rust_embed::RustEmbed;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use unic_langid::LanguageIdentifier;

const ENGLISH_LOCALE: &str = "en-US";
const SIMPLIFIED_CHINESE_LOCALE: &str = "zh-CN";
const SIMPLIFIED_CHINESE_UI_READY: bool = true;

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Deserialize,
    Eq,
    Hash,
    JsonSchema,
    PartialEq,
    Serialize,
    strum::VariantArray,
    strum::VariantNames,
)]
pub enum UiLanguage {
    #[default]
    #[serde(rename = "en-US")]
    English,
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
    #[cfg(debug_assertions)]
    #[serde(skip)]
    Pseudo,
}

impl UiLanguage {
    pub const ALL: [Self; 2] = [Self::English, Self::SimplifiedChinese];

    pub const fn locale(self) -> &'static str {
        match self {
            Self::English => ENGLISH_LOCALE,
            Self::SimplifiedChinese => SIMPLIFIED_CHINESE_LOCALE,
            #[cfg(debug_assertions)]
            Self::Pseudo => "en-XA",
        }
    }

    pub const fn native_name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::SimplifiedChinese => "简体中文",
            #[cfg(debug_assertions)]
            Self::Pseudo => "Pseudo",
        }
    }
}

#[derive(RustEmbed)]
#[folder = "../../assets/locales"]
#[include = "en-US/*.ftl"]
#[include = "zh-CN/*.ftl"]
struct CatalogAssets;

pub struct Localization {
    bundles: HashMap<UiLanguage, FluentBundle<FluentResource>>,
    language: UiLanguage,
    generation: u64,
    static_messages: RwLock<HashMap<(UiLanguage, &'static str), SharedString>>,
    reported_errors: RwLock<HashSet<String>>,
}

impl Global for Localization {}

impl Localization {
    pub fn load(language: UiLanguage) -> Result<Self> {
        let mut sources = HashMap::<UiLanguage, Vec<(String, String)>>::new();

        for path in CatalogAssets::iter() {
            let path = path.as_ref();
            let language = if path.starts_with(ENGLISH_LOCALE) {
                UiLanguage::English
            } else if path.starts_with(SIMPLIFIED_CHINESE_LOCALE) {
                UiLanguage::SimplifiedChinese
            } else {
                continue;
            };
            let source = CatalogAssets::get(path)
                .with_context(|| format!("missing embedded localization catalog {path}"))?;
            let source = std::str::from_utf8(source.data.as_ref())
                .with_context(|| format!("localization catalog {path} is not UTF-8"))?;
            sources
                .entry(language)
                .or_default()
                .push((path.to_owned(), source.to_owned()));
        }

        Self::from_sources(language, sources)
    }

    fn from_sources(
        language: UiLanguage,
        mut sources: HashMap<UiLanguage, Vec<(String, String)>>,
    ) -> Result<Self> {
        let mut bundles = HashMap::new();

        for locale in UiLanguage::ALL {
            let language_identifier = locale
                .locale()
                .parse::<LanguageIdentifier>()
                .with_context(|| format!("invalid built-in locale {}", locale.locale()))?;
            let mut bundle = FluentBundle::new_concurrent(vec![language_identifier]);
            bundle.set_use_isolating(false);
            let Some(mut locale_sources) = sources.remove(&locale) else {
                bail!("no localization catalogs found for {}", locale.locale());
            };
            locale_sources.sort_by(|left, right| left.0.cmp(&right.0));

            for (path, source) in locale_sources {
                let resource = FluentResource::try_new(source).map_err(|(_, errors)| {
                    anyhow::anyhow!("failed to parse localization catalog {path}: {errors:?}")
                })?;
                bundle.add_resource(resource).map_err(|errors| {
                    anyhow::anyhow!("failed to add localization catalog {path}: {errors:?}")
                })?;
            }

            bundles.insert(locale, bundle);
        }

        Ok(Self {
            bundles,
            language,
            generation: 0,
            static_messages: RwLock::new(HashMap::new()),
            reported_errors: RwLock::new(HashSet::new()),
        })
    }

    pub fn language(&self) -> UiLanguage {
        self.language
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn set_language(&mut self, language: UiLanguage) -> bool {
        if self.language == language {
            return false;
        }

        self.language = language;
        self.generation = self.generation.wrapping_add(1);
        self.static_messages.write().clear();
        true
    }

    pub fn text(&self, identifier: &'static str) -> SharedString {
        let cache_key = (self.language, identifier);
        if let Some(message) = self.static_messages.read().get(&cache_key) {
            return message.clone();
        }

        let message = self.format(identifier, None);
        self.static_messages
            .write()
            .insert(cache_key, message.clone());
        message
    }

    pub fn text_with_args(&self, identifier: &'static str, args: &FluentArgs<'_>) -> SharedString {
        self.format(identifier, Some(args))
    }

    fn format(&self, identifier: &'static str, args: Option<&FluentArgs<'_>>) -> SharedString {
        if let Some(message) = self.format_for_language(self.language, identifier, args) {
            return message;
        }

        if self.language != UiLanguage::English
            && let Some(message) = self.format_for_language(UiLanguage::English, identifier, args)
        {
            self.report_once(format!(
                "missing localization message {identifier:?} for {}",
                self.language.locale()
            ));
            return message;
        }

        self.report_once(format!(
            "missing localization message {identifier:?} in all catalogs"
        ));
        SharedString::from(identifier)
    }

    fn format_for_language(
        &self,
        language: UiLanguage,
        identifier: &'static str,
        args: Option<&FluentArgs<'_>>,
    ) -> Option<SharedString> {
        #[cfg(debug_assertions)]
        let bundle_language = if language == UiLanguage::Pseudo {
            UiLanguage::English
        } else {
            language
        };
        #[cfg(not(debug_assertions))]
        let bundle_language = language;
        let bundle = self.bundles.get(&bundle_language)?;
        let message = bundle.get_message(identifier)?;
        let pattern = message.value()?;
        let mut errors = Vec::new();
        let formatted = bundle.format_pattern(pattern, args, &mut errors);

        if !errors.is_empty() {
            self.report_once(format!(
                "failed to format localization message {identifier:?} for {}: {errors:?}",
                language.locale()
            ));
        }

        let formatted = formatted.into_owned();
        #[cfg(debug_assertions)]
        if language == UiLanguage::Pseudo {
            return Some(SharedString::from(pseudo_localize(&formatted)));
        }
        Some(SharedString::from(formatted))
    }

    fn report_once(&self, error: String) {
        if self.reported_errors.write().insert(error.clone()) {
            log::error!("{error}");
        }
    }
}

#[cfg(debug_assertions)]
fn pseudo_localize(text: &str) -> String {
    let mut expanded = String::with_capacity(text.len() * 2);
    expanded.push_str("［!! ");
    for character in text.chars() {
        expanded.push(character);
        if matches!(
            character,
            'a' | 'e' | 'i' | 'o' | 'u' | 'A' | 'E' | 'I' | 'O' | 'U'
        ) {
            expanded.push(character);
        }
    }
    expanded.push_str(" !!］");
    expanded
}

pub fn init(language: UiLanguage, cx: &mut App) -> Result<()> {
    cx.set_global(Localization::load(language)?);
    Ok(())
}

/// Formats a message without GPUI application state for CLI and helper programs.
pub fn text_for_language(language: UiLanguage, identifier: &'static str) -> Result<String> {
    Ok(Localization::load(language)?.text(identifier).to_string())
}

pub fn language(cx: &App) -> UiLanguage {
    cx.global::<Localization>().language()
}

pub fn try_language(cx: &App) -> Option<UiLanguage> {
    cx.try_global::<Localization>().map(Localization::language)
}

pub const fn simplified_chinese_ui_ready() -> bool {
    SIMPLIFIED_CHINESE_UI_READY
}

pub fn effective_development_language(language: UiLanguage) -> UiLanguage {
    #[cfg(debug_assertions)]
    if std::env::var_os("FLINT_PSEUDO_LANGUAGE").is_some() {
        return UiLanguage::Pseudo;
    }
    language
}

pub fn generation(cx: &App) -> u64 {
    cx.global::<Localization>().generation()
}

pub fn set_language(language: UiLanguage, cx: &mut App) -> bool {
    let changed =
        cx.update_global::<Localization, _>(|localization, _| localization.set_language(language));
    if changed {
        cx.refresh_windows();
    }
    changed
}

pub fn text(cx: &App, identifier: &'static str) -> SharedString {
    cx.global::<Localization>().text(identifier)
}

pub fn text_with_args(cx: &App, identifier: &'static str, args: &FluentArgs<'_>) -> SharedString {
    cx.global::<Localization>().text_with_args(identifier, args)
}

#[macro_export]
macro_rules! tr {
    ($cx:expr, $identifier:literal) => {
        $crate::text($cx, $identifier)
    };
    ($cx:expr, $identifier:literal, $($name:ident = $value:expr),+ $(,)?) => {{
        let mut args = $crate::FluentArgs::new();
        $(args.set(stringify!($name), $value);)+
        $crate::text_with_args($cx, $identifier, &args)
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn catalog_contract(locale: UiLanguage) -> HashMap<String, HashSet<String>> {
        let mut messages = HashMap::new();
        for path in CatalogAssets::iter() {
            if !path.starts_with(locale.locale()) {
                continue;
            }
            let source = CatalogAssets::get(path.as_ref()).expect("embedded catalog must exist");
            let source = std::str::from_utf8(source.data.as_ref())
                .expect("embedded catalog must contain UTF-8");
            let mut current_identifier = None;
            for line in source.lines() {
                let trimmed = line.trim_start();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                if !line.chars().next().is_some_and(char::is_whitespace)
                    && let Some((identifier, _)) = line.split_once('=')
                {
                    let identifier = identifier.trim().to_owned();
                    assert!(
                        messages
                            .insert(identifier.clone(), HashSet::new())
                            .is_none(),
                        "duplicate message {identifier:?} in {}",
                        locale.locale()
                    );
                    current_identifier = Some(identifier);
                }
                let Some(identifier) = current_identifier.as_ref() else {
                    continue;
                };
                let mut remaining = line;
                while let Some(variable_start) = remaining.find('$') {
                    remaining = &remaining[variable_start + 1..];
                    let variable_end = remaining
                        .find(|character: char| {
                            !(character.is_ascii_alphanumeric() || character == '_')
                        })
                        .unwrap_or(remaining.len());
                    if variable_end > 0 {
                        messages
                            .get_mut(identifier)
                            .expect("current message must exist")
                            .insert(remaining[..variable_end].to_owned());
                    }
                    remaining = &remaining[variable_end..];
                }
            }
        }
        messages
    }

    fn test_localization(language: UiLanguage) -> Localization {
        Localization::load(language).expect("test catalogs must load")
    }

    #[test]
    fn loads_each_supported_language() {
        for language in UiLanguage::ALL {
            let localization = test_localization(language);
            assert_eq!(localization.language(), language);
            assert_eq!(
                localization.text("settings-language-native-name"),
                language.native_name()
            );
        }
    }

    #[test]
    fn catalogs_have_the_same_messages_and_variables() {
        assert_eq!(
            catalog_contract(UiLanguage::English),
            catalog_contract(UiLanguage::SimplifiedChinese)
        );
    }

    #[test]
    fn changes_language_and_invalidates_static_messages() {
        let mut localization = test_localization(UiLanguage::English);
        assert_eq!(localization.text("common-cancel"), "Cancel");

        assert!(localization.set_language(UiLanguage::SimplifiedChinese));
        assert_eq!(localization.generation(), 1);
        assert_eq!(localization.text("common-cancel"), "取消");
        assert!(!localization.set_language(UiLanguage::SimplifiedChinese));
        assert_eq!(localization.generation(), 1);
    }

    #[test]
    fn formats_arguments() {
        let localization = test_localization(UiLanguage::SimplifiedChinese);
        let mut args = FluentArgs::new();
        args.set("count", 5_i64);
        assert_eq!(
            localization.text_with_args("common-selected-file-count", &args),
            "已选择 5 个文件"
        );
    }

    #[test]
    fn formats_messages_without_application_state_for_cli_programs() {
        assert_eq!(
            text_for_language(UiLanguage::SimplifiedChinese, "cli-error-prefix")
                .expect("CLI message must format"),
            "错误"
        );
    }

    #[test]
    fn falls_back_to_english_and_then_to_the_identifier() {
        let sources = HashMap::from([
            (
                UiLanguage::English,
                vec![("en-US/test.ftl".into(), "english-only = English".into())],
            ),
            (
                UiLanguage::SimplifiedChinese,
                vec![("zh-CN/test.ftl".into(), "chinese-only = 中文".into())],
            ),
        ]);
        let localization = Localization::from_sources(UiLanguage::SimplifiedChinese, sources)
            .expect("test catalogs must load");

        assert_eq!(localization.text("english-only"), "English");
        assert_eq!(
            localization.text("missing-everywhere"),
            "missing-everywhere"
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn pseudo_language_expands_english_without_being_user_selectable() {
        let localization = test_localization(UiLanguage::Pseudo);
        assert_eq!(localization.text("common-cancel"), "［!! Caanceel !!］");
        assert!(!UiLanguage::ALL.contains(&UiLanguage::Pseudo));
    }
}
