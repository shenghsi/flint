use gpui::{AsyncApp, actions};

pub const FLINT_URL_SCHEME: &str = "flint";

actions!(
    cli,
    [
        /// Registers the flint:// URL scheme handler.
        RegisterFlintScheme
    ]
);

pub async fn register_flint_scheme(cx: &AsyncApp) -> anyhow::Result<()> {
    cx.update(|cx| cx.register_url_scheme(FLINT_URL_SCHEME))
        .await
}
