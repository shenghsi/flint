use clap::Parser;
use remote_server::Commands;
use std::io::Write as _;
use std::path::PathBuf;

#[derive(Parser)]
#[command(disable_version_flag = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// Used for SSH/Git password authentication, to remove the need for netcat as a dependency,
    /// by having Flint act like netcat communicating over a Unix socket.
    #[arg(long, hide = true)]
    askpass: Option<String>,
    /// Used for recording minidumps on crashes by having the server run a separate
    /// process communicating over a socket.
    #[arg(long, hide = true)]
    crash_handler: Option<PathBuf>,
    /// Used for loading the environment from the project.
    #[arg(long, hide = true)]
    printenv: bool,
}

fn main() -> anyhow::Result<()> {
    let invoked_as_flintctl = std::env::args_os().next().is_some_and(invokes_flintctl);

    // Keep everything Flint stores on the remote host under `~/.flint/remote`
    // (managed agent binaries, db, extensions, languages, logs, server_state,
    // config) instead of the XDG default `~/.local/share/flint`. This must run
    // before any path is resolved, and before every dispatch branch below so
    // the crash-handler subprocess and proxy agree with the server on paths.
    let remote_data_dir = paths::home_dir().join(".flint").join("remote");
    paths::set_custom_data_dir(&remote_data_dir.to_string_lossy());

    if invoked_as_flintctl {
        agent_control_cli::main_with_transport(remote_server::run_remote_control_client);
        return Ok(());
    }

    let cli = Cli::parse();
    if let Err(error) = remote_server::install_remote_control_command() {
        log::warn!("failed to install remote flintctl command: {error:#}");
    }

    if let Some(socket_path) = &cli.askpass {
        askpass::main(socket_path);
        return Ok(());
    }

    if let Some(socket) = &cli.crash_handler {
        crashes::crash_server(socket.as_path(), paths::logs_dir().clone());
        return Ok(());
    }

    if cli.printenv {
        util::shell_env::print_env();
        return Ok(());
    }

    if let Some(command) = cli.command {
        use remote_server::ExecuteProxyError;

        let res = remote_server::run(command);
        if let Err(e) = &res
            && let Some(e) = e.downcast_ref::<ExecuteProxyError>()
        {
            std::io::stderr().write_fmt(format_args!("{e:#}\n")).ok();
            // It is important for us to report the proxy spawn exit code here
            // instead of the generic 1 that result returns
            // The client reads the exit code to determine if the server process has died when trying to reconnect
            // signaling that it needs to try spawning a new server
            std::process::exit(e.to_exit_code());
        }
        res
    } else {
        std::io::stderr()
            .write_all(b"usage: remote <run|proxy|version>\n")
            .ok();
        std::process::exit(1);
    }
}

fn invokes_flintctl(path: impl AsRef<std::ffi::OsStr>) -> bool {
    PathBuf::from(path.as_ref())
        .file_stem()
        .is_some_and(|stem| stem == "flintctl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_name_selects_remote_control_mode() {
        assert!(invokes_flintctl("/remote/version/flintctl"));
        assert!(invokes_flintctl("flintctl.exe"));
        assert!(!invokes_flintctl("/remote/version/remote_server"));
    }
}
