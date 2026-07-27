use anyhow::{Context as _, Result};
use std::process::Stdio;

/// A wrapper around `smol::process::Child` that ensures all subprocesses
/// are killed when the process is terminated: on Unix by using process
/// groups, and on Windows by using job objects.
pub struct Child {
    process: smol::process::Child,
    #[cfg(windows)]
    job: Option<WindowsProcessJob>,
}

impl std::fmt::Debug for Child {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.process.fmt(formatter)
    }
}

impl std::ops::Deref for Child {
    type Target = smol::process::Child;

    fn deref(&self) -> &Self::Target {
        &self.process
    }
}

impl std::ops::DerefMut for Child {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.process
    }
}

impl Child {
    #[cfg(not(windows))]
    pub fn spawn(
        mut command: std::process::Command,
        stdin: Stdio,
        stdout: Stdio,
        stderr: Stdio,
    ) -> Result<Self> {
        crate::set_pre_exec_to_start_new_session(&mut command);
        let mut command = smol::process::Command::from(command);
        let process = command
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .with_context(|| {
                format!(
                    "failed to spawn command {}",
                    crate::redact::redact_command(&format!("{command:?}"))
                )
            })?;
        Ok(Self { process })
    }

    #[cfg(windows)]
    pub fn spawn(
        command: std::process::Command,
        stdin: Stdio,
        stdout: Stdio,
        stderr: Stdio,
    ) -> Result<Self> {
        let mut command = smol::process::Command::from(command);
        let process = command
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .with_context(|| {
                format!(
                    "failed to spawn command {}",
                    crate::redact::redact_command(&format!("{command:?}"))
                )
            })?;

        Ok(Self::from_process(process, true))
    }

    #[cfg(windows)]
    pub(crate) fn from_process(process: smol::process::Child, own_process_tree: bool) -> Self {
        let job = if own_process_tree {
            match WindowsProcessJob::assign(process.id()) {
                Ok(job) => Some(job),
                Err(error) => {
                    log::error!("failed to assign spawned process to a job object: {error:#}");
                    None
                }
            }
        } else {
            None
        };
        Self { process, job }
    }

    /// Consumes the child, drains its output, and waits for it to exit.
    pub async fn output(self) -> std::io::Result<std::process::Output> {
        // Keep the Windows job handle alive until the child has exited and its
        // output has been collected.
        self.process.output().await
    }

    #[cfg(not(windows))]
    pub fn kill(&mut self) -> Result<()> {
        let pid = self.process.id();
        unsafe {
            libc::killpg(pid as i32, libc::SIGKILL);
        }
        Ok(())
    }

    #[cfg(windows)]
    pub fn kill(&mut self) -> Result<()> {
        if let Some(job) = &self.job {
            job.terminate()
        } else {
            self.process.kill()?;
            Ok(())
        }
    }
}

#[cfg(windows)]
pub struct WindowsProcessJob(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
// SAFETY: Windows job object handles may be used from any thread.
unsafe impl Send for WindowsProcessJob {}

#[cfg(windows)]
// SAFETY: Windows job object handles may be used from any thread.
unsafe impl Sync for WindowsProcessJob {}

#[cfg(windows)]
impl WindowsProcessJob {
    pub fn assign(process_id: u32) -> Result<Self> {
        let job = Self::new()?;
        job.assign_process(process_id)?;
        Ok(job)
    }

    fn new() -> Result<Self> {
        use windows::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        unsafe {
            let job = Self(CreateJobObjectW(None, None).context("failed to create job object")?);
            let mut information = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &information as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .context("failed to set job object limits")?;
            Ok(job)
        }
    }

    fn assign_process(&self, process_id: u32) -> Result<()> {
        use crate::ResultExt as _;
        use windows::Win32::{
            Foundation::CloseHandle,
            System::{
                JobObjects::AssignProcessToJobObject,
                Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE},
            },
        };

        unsafe {
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, process_id)
                .context("failed to open process")?;
            let result = AssignProcessToJobObject(self.0, process)
                .context("failed to assign process to job object");
            CloseHandle(process).log_err();
            result
        }
    }

    pub fn terminate(&self) -> Result<()> {
        use windows::Win32::System::JobObjects::TerminateJobObject;

        unsafe { TerminateJobObject(self.0, 1).context("failed to terminate job object") }
    }
}

#[cfg(windows)]
impl Drop for WindowsProcessJob {
    fn drop(&mut self) {
        use crate::ResultExt as _;
        use windows::Win32::Foundation::CloseHandle;

        unsafe {
            CloseHandle(self.0).log_err();
        }
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::time::{Duration, Instant};

    struct ProcessCleanup(u32);

    impl Drop for ProcessCleanup {
        fn drop(&mut self) {
            if !process_is_alive(self.0) {
                return;
            }

            use windows::Win32::{
                Foundation::CloseHandle,
                System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess},
            };

            unsafe {
                match OpenProcess(PROCESS_TERMINATE, false, self.0) {
                    Ok(handle) => {
                        if let Err(error) = TerminateProcess(handle, 1) {
                            eprintln!("failed to clean up test process {}: {error}", self.0);
                        }
                        if let Err(error) = CloseHandle(handle) {
                            eprintln!("failed to close test process handle: {error}");
                        }
                    }
                    Err(error) => {
                        eprintln!(
                            "failed to open test process {} for cleanup: {error}",
                            self.0
                        );
                    }
                }
            }
        }
    }

    fn configure_process_tree(command: &mut impl ProcessTreeCommand, temp_dir: &std::path::Path) {
        let pid_file = temp_dir.join("grandchild_pid");
        let escaped_pid_file = pid_file.display().to_string().replace('\'', "''");
        command.configure(
            ["-NoProfile", "-Command"],
            format!(
                "$p = Start-Process -FilePath ping.exe -ArgumentList @('-n','60','127.0.0.1') -PassThru -WindowStyle Hidden; \
                 Set-Content -LiteralPath '{escaped_pid_file}' -Value $p.Id; \
                 Wait-Process -Id $p.Id"
            ),
        );
    }

    trait ProcessTreeCommand {
        fn configure(&mut self, arguments: [&str; 2], script: String);
    }

    impl ProcessTreeCommand for std::process::Command {
        fn configure(&mut self, arguments: [&str; 2], script: String) {
            self.args(arguments).arg(script);
        }
    }

    impl ProcessTreeCommand for crate::command::Command {
        fn configure(&mut self, arguments: [&str; 2], script: String) {
            self.args(arguments).arg(script);
        }
    }

    fn wait_for_grandchild(temp_dir: &std::path::Path) -> (u32, ProcessCleanup) {
        let pid_file = temp_dir.join("grandchild_pid");
        let deadline = Instant::now() + Duration::from_secs(5);
        let grandchild_pid = loop {
            if let Ok(contents) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = contents.trim().parse::<u32>()
            {
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for grandchild pid file"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(
            process_is_alive(grandchild_pid),
            "grandchild should be alive after spawning"
        );
        (grandchild_pid, ProcessCleanup(grandchild_pid))
    }

    fn spawn_process_tree(temp_dir: &std::path::Path) -> (Child, u32, ProcessCleanup) {
        let mut command = std::process::Command::new("powershell.exe");
        configure_process_tree(&mut command, temp_dir);
        let child = Child::spawn(command, Stdio::null(), Stdio::null(), Stdio::null())
            .expect("failed to spawn powershell");
        let (grandchild_pid, cleanup) = wait_for_grandchild(temp_dir);
        (child, grandchild_pid, cleanup)
    }

    fn process_is_alive(pid: u32) -> bool {
        use windows::Win32::{
            Foundation::{CloseHandle, STILL_ACTIVE},
            System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        };

        unsafe {
            let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return false;
            };
            let mut exit_code = 0u32;
            let alive = GetExitCodeProcess(handle, &mut exit_code).is_ok()
                && exit_code == STILL_ACTIVE.0 as u32;
            if let Err(error) = CloseHandle(handle) {
                eprintln!("failed to close process handle: {error}");
            }
            alive
        }
    }

    fn assert_process_exits(pid: u32, message: &str) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while process_is_alive(pid) {
            assert!(Instant::now() < deadline, "{message} (pid {pid})");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    fn kill_terminates_grandchildren() {
        let temp_dir = tempfile::tempdir().expect("failed to create temporary directory");
        let (mut child, grandchild_pid, _cleanup) = spawn_process_tree(temp_dir.path());

        child.kill().expect("failed to kill child");

        assert_process_exits(
            grandchild_pid,
            "grandchild should be terminated after killing the child",
        );
    }

    #[test]
    fn drop_terminates_grandchildren() {
        let temp_dir = tempfile::tempdir().expect("failed to create temporary directory");
        let (child, grandchild_pid, _cleanup) = spawn_process_tree(temp_dir.path());

        drop(child);

        assert_process_exits(
            grandchild_pid,
            "grandchild should be terminated after dropping the child",
        );
    }

    #[test]
    fn command_kill_on_drop_terminates_grandchildren() {
        let temp_dir = tempfile::tempdir().expect("failed to create temporary directory");
        let mut command = crate::command::new_command("powershell.exe");
        configure_process_tree(&mut command, temp_dir.path());
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let child = command.spawn().expect("failed to spawn powershell");
        let (grandchild_pid, _cleanup) = wait_for_grandchild(temp_dir.path());

        drop(child);

        assert_process_exits(
            grandchild_pid,
            "grandchild should be terminated when a kill-on-drop command is dropped",
        );
    }

    #[test]
    fn command_without_kill_on_drop_remains_detached() {
        let temp_dir = tempfile::tempdir().expect("failed to create temporary directory");
        let mut command = crate::command::new_command("powershell.exe");
        configure_process_tree(&mut command, temp_dir.path());
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command.spawn().expect("failed to spawn powershell");
        let (grandchild_pid, _cleanup) = wait_for_grandchild(temp_dir.path());

        drop(child);

        assert!(
            process_is_alive(grandchild_pid),
            "a detached command should outlive its dropped process handle"
        );
    }
}
