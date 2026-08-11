//! Provides constructs for the Flint app version and release channel.

#![deny(missing_docs)]

use std::{env, str::FromStr, sync::LazyLock};

use gpui::{App, Global};
use semver::Version;

const FLINT_DOCS_URL: &str = "https://flint.dev/docs";
const CHINESE_DOCUMENTATION_PAGES: &[&str] = &["", "getting-started"];

/// stable | dev | nightly | preview
pub static RELEASE_CHANNEL_NAME: LazyLock<String> = LazyLock::new(|| {
    if cfg!(debug_assertions) {
        env::var("ZED_RELEASE_CHANNEL").unwrap_or_else(|_| {
            include_str!("../../flint/RELEASE_CHANNEL")
                .trim()
                .to_string()
        })
    } else {
        include_str!("../../flint/RELEASE_CHANNEL")
            .trim()
            .to_string()
    }
});

#[doc(hidden)]
pub static RELEASE_CHANNEL: LazyLock<ReleaseChannel> =
    LazyLock::new(|| match ReleaseChannel::from_str(&RELEASE_CHANNEL_NAME) {
        Ok(channel) => channel,
        _ => panic!("invalid release channel {}", *RELEASE_CHANNEL_NAME),
    });

/// The app identifier for the current release channel, Windows only.
#[cfg(target_os = "windows")]
pub fn app_identifier() -> &'static str {
    match *RELEASE_CHANNEL {
        ReleaseChannel::Dev => "Flint-Editor-Dev",
        ReleaseChannel::Nightly => "Flint-Editor-Nightly",
        ReleaseChannel::Preview => "Flint-Editor-Preview",
        ReleaseChannel::Stable => "Flint-Editor-Stable",
    }
}

/// The Git commit SHA that Flint was built at.
#[derive(Clone, Eq, Debug, PartialEq)]
pub struct AppCommitSha(String);

struct GlobalAppCommitSha(AppCommitSha);

impl Global for GlobalAppCommitSha {}

impl AppCommitSha {
    /// Creates a new [`AppCommitSha`].
    pub fn new(sha: String) -> Self {
        AppCommitSha(sha)
    }

    /// Returns the global [`AppCommitSha`], if one is set.
    pub fn try_global(cx: &App) -> Option<AppCommitSha> {
        cx.try_global::<GlobalAppCommitSha>()
            .map(|sha| sha.0.clone())
    }

    /// Sets the global [`AppCommitSha`].
    pub fn set_global(sha: AppCommitSha, cx: &mut App) {
        cx.set_global(GlobalAppCommitSha(sha))
    }

    /// Returns the full commit SHA.
    pub fn full(&self) -> String {
        self.0.to_string()
    }

    /// Returns the short (7 character) commit SHA.
    pub fn short(&self) -> String {
        self.0.chars().take(7).collect()
    }
}

struct GlobalAppVersion(Version);

impl Global for GlobalAppVersion {}

/// The version of Flint.
pub struct AppVersion;

impl AppVersion {
    /// Load the app version from env.
    pub fn load(
        pkg_version: &str,
        build_id: Option<&str>,
        commit_sha: Option<AppCommitSha>,
    ) -> Version {
        let mut version: Version = if let Ok(from_env) = env::var("ZED_APP_VERSION") {
            from_env.parse().expect("invalid ZED_APP_VERSION")
        } else {
            pkg_version.parse().expect("invalid version in Cargo.toml")
        };
        let mut pre = String::from(RELEASE_CHANNEL.dev_name());

        if let Some(build_id) = build_id {
            pre.push('.');
            pre.push_str(&build_id);
        }

        if let Some(sha) = commit_sha {
            pre.push('.');
            pre.push_str(&sha.0);
        }
        if let Ok(build) = semver::BuildMetadata::new(&pre) {
            version.build = build;
        }

        version
    }

    /// Returns the global version number.
    pub fn global(cx: &App) -> Version {
        if cx.has_global::<GlobalAppVersion>() {
            cx.global::<GlobalAppVersion>().0.clone()
        } else {
            Version::new(0, 0, 0)
        }
    }
}

/// A Flint release channel.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
pub enum ReleaseChannel {
    /// The development release channel.
    ///
    /// Used for local debug builds of Flint.
    #[default]
    Dev,

    /// The Nightly release channel.
    Nightly,

    /// The Preview release channel.
    Preview,

    /// The Stable release channel.
    Stable,
}

struct GlobalReleaseChannel(ReleaseChannel);

impl Global for GlobalReleaseChannel {}

/// Initializes the release channel.
pub fn init(app_version: Version, cx: &mut App) {
    cx.set_global(GlobalAppVersion(app_version));
    cx.set_global(GlobalReleaseChannel(*RELEASE_CHANNEL))
}

/// Initializes the release channel for tests that rely on fake release channel.
pub fn init_test(app_version: Version, release_channel: ReleaseChannel, cx: &mut App) {
    cx.set_global(GlobalAppVersion(app_version));
    cx.set_global(GlobalReleaseChannel(release_channel))
}

/// Returns the Flint docs URL for the current release channel for the given
/// `slug`.
pub fn docs_url(slug: &str, cx: &App) -> String {
    let english_url = ReleaseChannel::try_global(cx)
        .unwrap_or(*RELEASE_CHANNEL)
        .docs_url(slug);
    if localization::try_language(cx) != Some(localization::UiLanguage::SimplifiedChinese) {
        return english_url;
    }

    let (page, anchor) = slug
        .split_once('#')
        .map_or((slug, None), |(page, anchor)| (page, Some(anchor)));
    if !CHINESE_DOCUMENTATION_PAGES.contains(&page) {
        let english_page_url = ReleaseChannel::try_global(cx)
            .unwrap_or(*RELEASE_CHANNEL)
            .docs_url(page);
        return match anchor {
            Some(anchor) => {
                format!("{english_page_url}?language-fallback=zh-CN#{anchor}")
            }
            None => format!("{english_page_url}?language-fallback=zh-CN"),
        };
    }

    let page = if page.is_empty() {
        String::new()
    } else {
        format!("{page}.html")
    };
    match anchor {
        Some(anchor) => format!("{FLINT_DOCS_URL}/zh-CN/{page}#{anchor}"),
        None => format!("{FLINT_DOCS_URL}/zh-CN/{page}"),
    }
}

impl ReleaseChannel {
    /// All release channels.
    pub const ALL: [ReleaseChannel; 4] = [
        ReleaseChannel::Dev,
        ReleaseChannel::Nightly,
        ReleaseChannel::Preview,
        ReleaseChannel::Stable,
    ];

    /// Returns the global [`ReleaseChannel`].
    pub fn global(cx: &App) -> Self {
        cx.global::<GlobalReleaseChannel>().0
    }

    /// Returns the global [`ReleaseChannel`], if one is set.
    pub fn try_global(cx: &App) -> Option<Self> {
        cx.try_global::<GlobalReleaseChannel>()
            .map(|channel| channel.0)
    }

    /// Returns whether we want to poll for updates for this [`ReleaseChannel`]
    pub fn poll_for_updates(&self) -> bool {
        !matches!(self, ReleaseChannel::Dev)
    }

    /// Returns the display name for this [`ReleaseChannel`].
    pub fn display_name(&self) -> &'static str {
        match self {
            ReleaseChannel::Dev => "Flint Dev",
            ReleaseChannel::Nightly => "Flint Nightly",
            ReleaseChannel::Preview => "Flint Preview",
            ReleaseChannel::Stable => "Flint",
        }
    }

    /// Returns the programmatic name for this [`ReleaseChannel`].
    pub fn dev_name(&self) -> &'static str {
        match self {
            ReleaseChannel::Dev => "dev",
            ReleaseChannel::Nightly => "nightly",
            ReleaseChannel::Preview => "preview",
            ReleaseChannel::Stable => "stable",
        }
    }

    /// Returns the application ID that's used by Wayland as application ID
    /// and WM_CLASS on X11.
    /// This also has to match the bundle identifier for Flint on macOS.
    pub fn app_id(&self) -> &'static str {
        match self {
            ReleaseChannel::Dev => "dev.flint.Flint-Dev",
            ReleaseChannel::Nightly => "dev.flint.Flint-Nightly",
            ReleaseChannel::Preview => "dev.flint.Flint-Preview",
            ReleaseChannel::Stable => "dev.flint.Flint",
        }
    }

    /// Returns the query parameter for this [`ReleaseChannel`].
    pub fn release_query_param(&self) -> Option<&'static str> {
        match self {
            Self::Dev => None,
            Self::Nightly => Some("nightly=1"),
            Self::Preview => Some("preview=1"),
            Self::Stable => None,
        }
    }

    /// Returns the Flint docs URL for the given `slug`.
    ///
    /// Documentation has one hosted copy for all release channels.
    pub fn docs_url(&self, slug: &str) -> String {
        if slug.is_empty() {
            return FLINT_DOCS_URL.to_string();
        }

        match slug.split_once('#') {
            Some((page, anchor)) => format!("{FLINT_DOCS_URL}/{page}.html#{anchor}"),
            None => format!("{FLINT_DOCS_URL}/{slug}.html"),
        }
    }
}

/// Error indicating that release channel string does not match any known release channel names.
#[derive(Copy, Clone, Debug, Hash, PartialEq)]
pub struct InvalidReleaseChannel;

impl FromStr for ReleaseChannel {
    type Err = InvalidReleaseChannel;

    fn from_str(channel: &str) -> Result<Self, Self::Err> {
        Ok(match channel {
            "dev" => ReleaseChannel::Dev,
            "nightly" => ReleaseChannel::Nightly,
            "preview" => ReleaseChannel::Preview,
            "stable" => ReleaseChannel::Stable,
            _ => return Err(InvalidReleaseChannel),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ReleaseChannel;
    use gpui::TestAppContext;

    #[test]
    fn test_docs_url_for_release_channel() {
        let expected = "https://flint.dev/docs/settings.html";
        assert_eq!(ReleaseChannel::Dev.docs_url("settings"), expected);
        assert_eq!(ReleaseChannel::Nightly.docs_url("settings"), expected);
        assert_eq!(ReleaseChannel::Preview.docs_url("settings"), expected);
        assert_eq!(ReleaseChannel::Stable.docs_url("settings"), expected);
        assert_eq!(
            ReleaseChannel::Stable.docs_url("tasks#custom-git-commands"),
            "https://flint.dev/docs/tasks.html#custom-git-commands"
        );
    }

    #[gpui::test]
    fn test_chinese_docs_url_and_english_fallback(cx: &mut TestAppContext) {
        cx.update(|cx| {
            super::init_test(semver::Version::new(1, 0, 0), ReleaseChannel::Stable, cx);
            localization::init(localization::UiLanguage::SimplifiedChinese, cx)
                .expect("test localization must load");

            assert_eq!(
                super::docs_url("getting-started", cx),
                "https://flint.dev/docs/zh-CN/getting-started.html"
            );
            assert_eq!(
                super::docs_url("tasks#custom-git-commands", cx),
                "https://flint.dev/docs/tasks.html?language-fallback=zh-CN#custom-git-commands"
            );
        });
    }
}
