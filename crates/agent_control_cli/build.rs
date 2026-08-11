fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-env-changed=RELEASE_CHANNEL");
        println!("cargo:rerun-if-env-changed=GITHUB_RUN_NUMBER");
        windows_resources::compile(false)?;
    }
    Ok(())
}
