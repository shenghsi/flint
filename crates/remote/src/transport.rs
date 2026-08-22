use std::io::Write;

use crate::{
    RemoteArch, RemoteLibc, RemoteOs, RemotePlatform,
    json_log::LogRecord,
    protocol::{MESSAGE_LEN_SIZE, message_len_from_buffer, read_message_with_len, write_message},
};
use anyhow::{Context as _, Result};
use futures::{
    AsyncReadExt as _, FutureExt as _, StreamExt as _,
    channel::mpsc::{Sender, UnboundedReceiver, UnboundedSender},
};
use gpui::{AppContext as _, AsyncApp, Task};
use rpc::proto::Envelope;
use util::command::Child;
use util::{paths::PathStyle, rel_path::RelPath};

pub mod docker;
#[cfg(any(test, feature = "test-support"))]
pub mod mock;
pub mod ssh;
pub mod wsl;

fn remote_server_executable_path(
    home_directory: &str,
    relative_path: &RelPath,
    path_style: PathStyle,
) -> String {
    format!(
        "{}{}{}",
        home_directory.trim_end_matches(path_style.separators_ch()),
        path_style.primary_separator(),
        relative_path.display(path_style)
    )
}

fn parse_remote_home_directory(output: &str) -> Result<String> {
    output
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
        .context("remote home directory command returned no path")
}

// SSH cannot safely preserve multiline arguments through every supported login shell.
const POSIX_TARGET_PROBE: &str = concat!(
    "os=$(uname -s) || exit 1; ",
    "arch=$(uname -m) || exit 1; ",
    "libc=none; ",
    "if [ \"$os\" = Linux ]; then ",
    "libc=unknown; ",
    "if getconf GNU_LIBC_VERSION >/dev/null 2>&1; then ",
    "libc=glibc; ",
    "else ",
    "ldd_output=$(ldd --version 2>&1); ",
    "case \"$ldd_output\" in *musl*) libc=musl ;; esac; ",
    "fi; ",
    "fi; ",
    "printf '__FLINT_REMOTE_TARGET__\\t%s\\t%s\\t%s\\n' \"$os\" \"$arch\" \"$libc\"",
);

fn posix_target_probe_command() -> (&'static str, [&'static str; 2]) {
    ("sh", ["-c", POSIX_TARGET_PROBE])
}

fn parse_platform(output: &str) -> Result<RemotePlatform> {
    const TARGET_PREFIX: &str = "__FLINT_REMOTE_TARGET__\t";

    let target = output
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(TARGET_PREFIX))
        .context("remote target probe did not produce a tagged result")?;
    let mut fields = target.split('\t');
    let os = fields.next().context("remote target is missing its OS")?;
    let arch = fields
        .next()
        .context("remote target is missing its architecture")?;
    let libc = fields
        .next()
        .context("remote target is missing its libc result")?;

    let os = match os {
        "Darwin" => RemoteOs::MacOs,
        "Linux" => RemoteOs::Linux,
        "Windows" => RemoteOs::Windows,
        _ => anyhow::bail!(
            "Prebuilt remote servers are not yet available for {os:?}. See https://github.com/shenghsi/flint/blob/main/docs/src/remote-development.md"
        ),
    };

    // exclude armv5,6,7 as they are 32-bit.
    let arch = if arch.starts_with("armv8")
        || arch.starts_with("armv9")
        || arch.starts_with("arm64")
        || arch.starts_with("aarch64")
    {
        RemoteArch::Aarch64
    } else if arch.starts_with("x86") || arch == "AMD64" {
        RemoteArch::X86_64
    } else {
        anyhow::bail!(
            "Prebuilt remote servers are not yet available for {arch:?}. See https://github.com/shenghsi/flint/blob/main/docs/src/remote-development.md"
        )
    };

    let libc = match os {
        RemoteOs::Linux => Some(match libc {
            "glibc" => RemoteLibc::Glibc,
            "musl" => RemoteLibc::Musl,
            _ => RemoteLibc::Unknown,
        }),
        RemoteOs::MacOs | RemoteOs::Windows => None,
    };

    Ok(RemotePlatform { os, arch, libc })
}

/// Parses the output of `echo $SHELL` to determine the remote shell.
/// Takes the last line to skip possible shell initialization output.
fn parse_shell(output: &str, fallback_shell: &str) -> String {
    let output = output.trim();
    let shell = output.rsplit_once('\n').map_or(output, |(_, last)| last);
    if shell.is_empty() {
        log::error!("$SHELL is not set, falling back to {fallback_shell}");
        fallback_shell.to_owned()
    } else {
        shell.to_owned()
    }
}

fn handle_rpc_messages_over_child_process_stdio(
    mut remote_proxy_process: Child,
    incoming_tx: UnboundedSender<Envelope>,
    mut outgoing_rx: UnboundedReceiver<Envelope>,
    mut connection_activity_tx: Sender<()>,
    cx: &AsyncApp,
) -> Task<Result<i32>> {
    let mut child_stderr = remote_proxy_process.stderr.take().unwrap();
    let mut child_stdout = remote_proxy_process.stdout.take().unwrap();
    let mut child_stdin = remote_proxy_process.stdin.take().unwrap();

    let mut stdin_buffer = Vec::new();
    let mut stdout_buffer = Vec::new();
    let mut stderr_buffer = Vec::new();
    let mut stderr_offset = 0;

    let stdin_task = cx.background_spawn(async move {
        while let Some(outgoing) = outgoing_rx.next().await {
            write_message(&mut child_stdin, &mut stdin_buffer, outgoing).await?;
        }
        anyhow::Ok(())
    });

    let stdout_task = cx.background_spawn({
        let mut connection_activity_tx = connection_activity_tx.clone();
        async move {
            loop {
                stdout_buffer.resize(MESSAGE_LEN_SIZE, 0);
                let len = child_stdout.read(&mut stdout_buffer).await?;

                if len == 0 {
                    return anyhow::Ok(());
                }

                if len < MESSAGE_LEN_SIZE {
                    child_stdout.read_exact(&mut stdout_buffer[len..]).await?;
                }

                let message_len = message_len_from_buffer(&stdout_buffer);
                let envelope =
                    read_message_with_len(&mut child_stdout, &mut stdout_buffer, message_len)
                        .await?;
                connection_activity_tx.try_send(()).ok();
                incoming_tx.unbounded_send(envelope).ok();
            }
        }
    });

    let stderr_task: Task<anyhow::Result<()>> = cx.background_spawn(async move {
        loop {
            stderr_buffer.resize(stderr_offset + 1024, 0);

            let len = child_stderr
                .read(&mut stderr_buffer[stderr_offset..])
                .await?;
            if len == 0 {
                return anyhow::Ok(());
            }

            stderr_offset += len;
            let mut start_ix = 0;
            while let Some(ix) = stderr_buffer[start_ix..stderr_offset]
                .iter()
                .position(|b| b == &b'\n')
            {
                let line_ix = start_ix + ix;
                let content = &stderr_buffer[start_ix..line_ix];
                start_ix = line_ix + 1;
                if let Ok(record) = serde_json::from_slice::<LogRecord>(content) {
                    record.log(log::logger())
                } else {
                    std::io::stderr()
                        .write_fmt(format_args!(
                            "(remote) {}\n",
                            String::from_utf8_lossy(content)
                        ))
                        .ok();
                }
            }
            stderr_buffer.drain(0..start_ix);
            stderr_offset -= start_ix;

            connection_activity_tx.try_send(()).ok();
        }
    });

    cx.background_spawn(async move {
        let result = futures::select! {
            result = stdin_task.fuse() => {
                result.context("stdin")
            }
            result = stdout_task.fuse() => {
                result.context("stdout")
            }
            result = stderr_task.fuse() => {
                result.context("stderr")
            }
        };
        let exit_status = remote_proxy_process.status().await?;
        let status = exit_status.code().unwrap_or_else(|| {
            #[cfg(unix)]
            let status = std::os::unix::process::ExitStatusExt::signal(&exit_status).unwrap_or(1);
            #[cfg(not(unix))]
            let status = 1;
            status
        });
        match result {
            Ok(_) => Ok(status),
            Err(error) => Err(error),
        }
    })
}

#[cfg(any(debug_assertions, feature = "build-remote-server-binary"))]
async fn build_remote_server_from_source(
    platform: &crate::RemotePlatform,
    delegate: &dyn crate::RemoteClientDelegate,
    binary_exists_on_server: bool,
    cx: &mut AsyncApp,
) -> Result<Option<std::path::PathBuf>> {
    use std::env::VarError;
    use std::path::Path;
    use util::command::{Command, Stdio, new_command};

    if let Ok(path) = std::env::var("ZED_COPY_REMOTE_SERVER") {
        let path = std::path::PathBuf::from(path);
        if path.exists() {
            return Ok(Some(path));
        } else {
            log::warn!(
                "ZED_COPY_REMOTE_SERVER path does not exist, falling back to ZED_BUILD_REMOTE_SERVER: {}",
                path.display()
            );
        }
    }

    // By default, we make building remote server from source opt-out and we do not force artifact compression
    // for quicker builds.
    let build_remote_server =
        std::env::var("ZED_BUILD_REMOTE_SERVER").unwrap_or("nocompress".into());

    if let "never" = &*build_remote_server {
        return Ok(None);
    } else if let "false" | "no" | "off" | "0" = &*build_remote_server {
        if binary_exists_on_server {
            return Ok(None);
        }
        log::warn!("ZED_BUILD_REMOTE_SERVER is disabled, but no server binary exists on the server")
    }

    async fn run_cmd(command: &mut Command) -> Result<()> {
        let output = command
            .kill_on_drop(true)
            .stdout(Stdio::inherit())
            .output()
            .await?;
        anyhow::ensure!(
            output.status.success(),
            "Failed to run command: {command:?}: output: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    let use_musl = !build_remote_server.contains("nomusl");
    let triple = format!(
        "{}-{}",
        platform.arch,
        match platform.os {
            RemoteOs::Linux =>
                if use_musl {
                    "unknown-linux-musl"
                } else {
                    "unknown-linux-gnu"
                },
            RemoteOs::MacOs => "apple-darwin",
            RemoteOs::Windows if cfg!(windows) => "pc-windows-msvc",
            RemoteOs::Windows => "pc-windows-gnu",
        }
    );
    let mut rust_flags = match std::env::var("RUSTFLAGS") {
        Ok(val) => val,
        Err(VarError::NotPresent) => String::new(),
        Err(e) => {
            log::error!("Failed to get env var `RUSTFLAGS` value: {e}");
            String::new()
        }
    };
    if platform.os == RemoteOs::Linux && use_musl {
        rust_flags.push_str(" -C target-feature=+crt-static");

        if let Ok(path) = std::env::var("ZED_ZSTD_MUSL_LIB") {
            rust_flags.push_str(&format!(" -C link-arg=-L{path}"));
        }
    }
    if platform.arch.as_str() == std::env::consts::ARCH
        && platform.os.as_str() == std::env::consts::OS
    {
        delegate.set_status(Some("Building remote server binary from source"), cx);
        log::info!("building remote server binary from source");
        run_cmd(
            new_command("cargo")
                .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
                .args([
                    "build",
                    "--package",
                    "remote_server",
                    "--features",
                    "debug-embed",
                    "--target-dir",
                    "target/remote_server",
                    "--target",
                    &triple,
                ])
                .env("RUSTFLAGS", &rust_flags),
        )
        .await?;
    } else {
        if which("zig", cx).await?.is_none() {
            anyhow::bail!(if cfg!(not(windows)) {
                "zig not found on $PATH, install zig (see https://ziglang.org/learn/getting-started or use zigup)"
            } else {
                "zig not found on $PATH, install zig (use `winget install -e --id zig.zig` or see https://ziglang.org/learn/getting-started or use zigup)"
            });
        }

        let rustup = which("rustup", cx)
            .await?
            .context("rustup not found on $PATH, install rustup (see https://rustup.rs/)")?;
        delegate.set_status(Some("Adding rustup target for cross-compilation"), cx);
        log::info!("adding rustup target");
        run_cmd(new_command(rustup).args(["target", "add"]).arg(&triple)).await?;

        if which("cargo-zigbuild", cx).await?.is_none() {
            delegate.set_status(Some("Installing cargo-zigbuild for cross-compilation"), cx);
            log::info!("installing cargo-zigbuild");
            run_cmd(new_command("cargo").args(["install", "--locked", "cargo-zigbuild"])).await?;
        }

        delegate.set_status(
            Some(&format!(
                "Building remote binary from source for {triple} with Zig"
            )),
            cx,
        );
        log::info!("building remote binary from source for {triple} with Zig");
        run_cmd(
            new_command("cargo")
                .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
                .args([
                    "zigbuild",
                    "--package",
                    "remote_server",
                    "--features",
                    "debug-embed",
                    "--target-dir",
                    "target/remote_server",
                    "--target",
                    &triple,
                ])
                .env("RUSTFLAGS", &rust_flags),
        )
        .await?;
    };
    let bin_path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("target")
        .join("remote_server")
        .join(&triple)
        .join("debug")
        .join("remote_server")
        .with_extension(if platform.os.is_windows() { "exe" } else { "" });

    let path = if !build_remote_server.contains("nocompress") {
        delegate.set_status(Some("Compressing binary"), cx);

        #[cfg(not(target_os = "windows"))]
        let archive_path = {
            run_cmd(new_command("gzip").arg("-f").arg(&bin_path)).await?;
            bin_path.with_extension("gz")
        };

        #[cfg(target_os = "windows")]
        let archive_path = {
            let zip_path = bin_path.with_extension("zip");
            if smol::fs::metadata(&zip_path).await.is_ok() {
                smol::fs::remove_file(&zip_path).await?;
            }
            let compress_command = format!(
                "Compress-Archive -Path '{}' -DestinationPath '{}' -Force",
                bin_path.display(),
                zip_path.display(),
            );
            run_cmd(new_command("powershell.exe").args([
                "-NoProfile",
                "-Command",
                &compress_command,
            ]))
            .await?;
            zip_path
        };

        std::env::current_dir()?.join(archive_path)
    } else {
        bin_path
    };

    Ok(Some(path))
}

#[cfg(any(debug_assertions, feature = "build-remote-server-binary"))]
async fn which(
    binary_name: impl AsRef<str>,
    cx: &mut AsyncApp,
) -> Result<Option<std::path::PathBuf>> {
    let binary_name = binary_name.as_ref().to_string();
    let binary_name_cloned = binary_name.clone();
    let res = cx
        .background_spawn(async move { which::which(binary_name_cloned) })
        .await;
    match res {
        Ok(path) => Ok(Some(path)),
        Err(which::Error::CannotFindBinaryPath) => Ok(None),
        Err(err) => Err(anyhow::anyhow!(
            "Failed to run 'which' to find the binary '{binary_name}': {err}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_probe_command_arguments_are_single_line() {
        let (_, arguments) = posix_target_probe_command();

        assert!(
            arguments
                .iter()
                .all(|argument| !argument.contains(['\n', '\r']))
        );
    }

    #[cfg(unix)]
    #[test]
    fn posix_probe_reports_a_tagged_local_target() {
        let output = smol::block_on(async {
            let (program, arguments) = posix_target_probe_command();
            smol::process::Command::new(program)
                .args(arguments)
                .output()
                .await
                .expect("POSIX shell should run the target probe")
        });
        let stdout = String::from_utf8(output.stdout).expect("probe output should be UTF-8");

        let result = parse_platform(&stdout).expect("probe output should parse");

        assert!(output.status.success());
        let expected_os = if cfg!(target_os = "macos") {
            RemoteOs::MacOs
        } else {
            RemoteOs::Linux
        };
        assert_eq!(result.os, expected_os);
    }

    #[test]
    fn parses_tagged_glibc_linux_target() {
        let result = parse_platform("__FLINT_REMOTE_TARGET__\tLinux\tx86_64\tglibc\n")
            .expect("tagged target should parse");

        assert_eq!(
            result,
            RemotePlatform {
                os: RemoteOs::Linux,
                arch: RemoteArch::X86_64,
                libc: Some(RemoteLibc::Glibc),
            }
        );
    }

    #[test]
    fn parses_tagged_windows_target_without_libc() {
        let result = parse_platform("__FLINT_REMOTE_TARGET__\tWindows\tAMD64\tnone\n")
            .expect("tagged Windows target should parse");

        assert_eq!(
            result,
            RemotePlatform {
                os: RemoteOs::Windows,
                arch: RemoteArch::X86_64,
                libc: None,
            }
        );
    }

    #[test]
    fn parses_target_after_shell_startup_noise() {
        let result = parse_platform(
            "welcome from shell startup\n__FLINT_REMOTE_TARGET__\tLinux\taarch64\tmusl\n",
        )
        .expect("tagged target should be found after startup noise");

        assert_eq!(result.arch, RemoteArch::Aarch64);
        assert_eq!(result.libc, Some(RemoteLibc::Musl));
    }

    #[test]
    fn preserves_linux_target_when_libc_is_unknown() {
        let result = parse_platform("__FLINT_REMOTE_TARGET__\tLinux\tx86_64\tunknown\n")
            .expect("unknown libc should not hide the base target");

        assert_eq!(result.os, RemoteOs::Linux);
        assert_eq!(result.arch, RemoteArch::X86_64);
        assert_eq!(result.libc, Some(RemoteLibc::Unknown));
    }

    #[test]
    fn parses_tagged_macos_target_without_libc() {
        let result = parse_platform("__FLINT_REMOTE_TARGET__\tDarwin\tarm64\tnone\n")
            .expect("tagged macOS target should parse");

        assert_eq!(result.os, RemoteOs::MacOs);
        assert_eq!(result.arch, RemoteArch::Aarch64);
        assert_eq!(result.libc, None);
    }

    #[test]
    fn test_parse_shell() {
        assert_eq!(parse_shell("/bin/bash\n", "sh"), "/bin/bash");
        assert_eq!(parse_shell("/bin/zsh\n", "sh"), "/bin/zsh");

        assert_eq!(parse_shell("/bin/bash", "sh"), "/bin/bash");
        assert_eq!(
            parse_shell("some shell init output\n/bin/bash\n", "sh"),
            "/bin/bash"
        );
        assert_eq!(
            parse_shell("some shell init output\n/bin/bash", "sh"),
            "/bin/bash"
        );
        assert_eq!(parse_shell("", "sh"), "sh");
        assert_eq!(parse_shell("\n", "sh"), "sh");
    }

    #[test]
    fn remote_server_command_path_is_absolute_for_each_path_style() {
        let relative =
            RelPath::unix(".flint_server/flint-remote-server").expect("relative server path");

        assert_eq!(
            remote_server_executable_path("/home/flint", relative, PathStyle::Posix),
            "/home/flint/.flint_server/flint-remote-server"
        );
        assert_eq!(
            remote_server_executable_path(r"C:\Users\flint", relative, PathStyle::Windows),
            r"C:\Users\flint\.flint_server\flint-remote-server"
        );
    }

    #[test]
    fn remote_home_directory_uses_the_last_nonempty_output_line() {
        assert_eq!(
            parse_remote_home_directory("shell notice\n/home/flint\n").expect("parse remote home"),
            "/home/flint"
        );
    }
}
