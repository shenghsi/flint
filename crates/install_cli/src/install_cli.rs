#[cfg(not(target_os = "windows"))]
mod install_cli_binary;
mod register_flint_scheme;

#[cfg(not(target_os = "windows"))]
pub use install_cli_binary::{InstallCliBinary, install_cli_binary};
pub use register_flint_scheme::{FLINT_URL_SCHEME, RegisterFlintScheme, register_flint_scheme};

#[cfg(test)]
mod tests {
    #[test]
    fn registers_the_flint_url_scheme() {
        assert_eq!(super::FLINT_URL_SCHEME, "flint");
    }
}
