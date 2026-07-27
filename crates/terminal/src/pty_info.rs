use gpui::{Context, Task};
use parking_lot::{MappedRwLockReadGuard, Mutex, RwLock, RwLockReadGuard};
use std::{path::PathBuf, sync::Arc};

#[cfg(target_os = "windows")]
use windows::Win32::{Foundation::HANDLE, System::Threading::GetProcessId};

#[cfg(any(windows, test))]
use std::collections::HashMap;

use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::{Event, Terminal};

#[cfg(any(windows, test))]
fn descendant_process_ids(
    root_process_id: u32,
    parent_process_ids: &HashMap<u32, u32>,
) -> Vec<u32> {
    parent_process_ids
        .keys()
        .copied()
        .filter(|process_id| {
            let mut current_process_id = *process_id;
            for _ in 0..parent_process_ids.len() {
                let Some(parent_process_id) = parent_process_ids.get(&current_process_id) else {
                    return false;
                };
                if *parent_process_id == root_process_id {
                    return true;
                }
                current_process_id = *parent_process_id;
            }
            false
        })
        .collect()
}

#[derive(Clone, Copy)]
pub struct ProcessIdGetter {
    handle: i32,
    fallback_pid: u32,
}

impl ProcessIdGetter {
    pub(crate) fn new(handle: i32, fallback_pid: u32) -> ProcessIdGetter {
        ProcessIdGetter {
            handle,
            fallback_pid,
        }
    }

    pub fn fallback_pid(&self) -> Pid {
        Pid::from_u32(self.fallback_pid)
    }
}

#[cfg(unix)]
impl ProcessIdGetter {
    fn pid(&self) -> Option<Pid> {
        // Negative pid means error.
        // Zero pid means no foreground process group is set on the PTY yet.
        // Avoid killing the current process by returning a zero pid.
        let pid = unsafe { libc::tcgetpgrp(self.handle) };
        if pid > 0 {
            return Some(Pid::from_u32(pid as u32));
        }

        if self.fallback_pid > 0 {
            return Some(Pid::from_u32(self.fallback_pid));
        }

        None
    }
}

#[cfg(windows)]
impl ProcessIdGetter {
    fn pid(&self) -> Option<Pid> {
        let pid = unsafe { GetProcessId(HANDLE(self.handle as _)) };
        // the GetProcessId may fail and returns zero, which will lead to a stack overflow issue
        if pid == 0 {
            // in the builder process, there is a small chance, almost negligible,
            // that this value could be zero, which means child_watcher returns None,
            // GetProcessId returns 0.
            if self.fallback_pid == 0 {
                return None;
            }
            return Some(Pid::from_u32(self.fallback_pid));
        }
        Some(Pid::from_u32(pid))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ProcessInfo {
    pub(crate) name: String,
    pub(crate) cwd: PathBuf,
    pub(crate) argv: Vec<String>,
}

/// Fetches Flint-relevant Pseudo-Terminal (PTY) process information
pub(crate) struct PtyProcessInfo {
    system: RwLock<System>,
    refresh_kind: ProcessRefreshKind,
    pid_getter: ProcessIdGetter,
    #[cfg(windows)]
    process_job: Option<util::process::WindowsProcessJob>,
    last_foreground_pid: Mutex<Option<Pid>>,
    pub(crate) current: RwLock<Option<ProcessInfo>>,
    task: Mutex<Option<Task<()>>>,
}

impl PtyProcessInfo {
    pub(crate) fn new(pid_getter: ProcessIdGetter) -> PtyProcessInfo {
        // sysinfo retains an open procfs handle for every process and task entry on Linux.
        let process_refresh_kind = ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always)
            .with_exe(UpdateKind::Always)
            .without_tasks();
        let mut system = System::new();
        // A full initial refresh would retain every process on the machine for this terminal.
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid_getter.fallback_pid()]),
            true,
            process_refresh_kind,
        );

        #[cfg(windows)]
        let process_job =
            util::process::WindowsProcessJob::assign(pid_getter.fallback_pid().as_u32())
                .map_err(|error| {
                    log::error!("failed to assign terminal process to a job object: {error:#}");
                })
                .ok();

        PtyProcessInfo {
            system: RwLock::new(system),
            refresh_kind: process_refresh_kind,
            pid_getter,
            #[cfg(windows)]
            process_job,
            last_foreground_pid: Mutex::new(None),
            current: RwLock::new(None),
            task: Mutex::new(None),
        }
    }

    pub(crate) fn pid_getter(&self) -> &ProcessIdGetter {
        &self.pid_getter
    }

    fn refresh(&self) -> Option<MappedRwLockReadGuard<'_, Process>> {
        let pid = self.pid_getter.pid()?;
        let fallback_pid = self.pid_getter.fallback_pid();
        let mut system = self.system.write();
        // Targeted refreshes do not evict earlier PIDs, so rebuild when the foreground job changes.
        if self.last_foreground_pid.lock().replace(pid) != Some(pid) {
            *system = System::new();
        }
        let pids = [pid, fallback_pid];
        let pids = if pid == fallback_pid {
            &pids[..1]
        } else {
            &pids[..]
        };
        system.refresh_processes_specifics(ProcessesToUpdate::Some(pids), true, self.refresh_kind);
        drop(system);
        RwLockReadGuard::try_map(self.system.read(), |system| system.process(pid)).ok()
    }

    fn get_child(&self) -> Option<MappedRwLockReadGuard<'_, Process>> {
        let pid = self.pid_getter.fallback_pid();
        RwLockReadGuard::try_map(self.system.read(), |system| system.process(pid)).ok()
    }

    #[cfg(unix)]
    pub(crate) fn kill_current_process(&self) -> bool {
        let Some(pid) = self.pid_getter.pid() else {
            return false;
        };
        unsafe { libc::killpg(pid.as_u32() as i32, libc::SIGKILL) == 0 }
    }

    #[cfg(windows)]
    pub(crate) fn kill_current_process(&self) -> bool {
        let root_process_id = self.pid_getter.fallback_pid();
        let mut process_snapshot = System::new();
        process_snapshot.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().without_tasks(),
        );
        let parent_process_ids = process_snapshot
            .processes()
            .iter()
            .filter_map(|(process_id, process)| {
                Some((process_id.as_u32(), process.parent()?.as_u32()))
            })
            .collect();
        let descendant_process_ids =
            descendant_process_ids(root_process_id.as_u32(), &parent_process_ids);
        #[cfg(test)]
        eprintln!(
            "[DEBUG-pr76-conpty] root={} current={:?} has_job={} descendants={descendant_process_ids:?}",
            root_process_id.as_u32(),
            self.pid_getter.pid().map(|process_id| process_id.as_u32()),
            self.process_job.is_some()
        );

        let killed_current_process = if let Some(process_job) = &self.process_job {
            match process_job.terminate() {
                Ok(()) => true,
                Err(error) => {
                    log::error!("failed to terminate terminal process job: {error:#}");
                    self.refresh().is_some_and(|process| process.kill())
                }
            }
        } else {
            self.refresh().is_some_and(|process| process.kill())
        };

        let descendant_pids = descendant_process_ids
            .iter()
            .copied()
            .map(Pid::from_u32)
            .collect::<Vec<_>>();
        process_snapshot.refresh_processes_specifics(
            ProcessesToUpdate::Some(&descendant_pids),
            true,
            ProcessRefreshKind::nothing().without_tasks(),
        );
        #[cfg(test)]
        eprintln!(
            "[DEBUG-pr76-conpty] surviving descendants={:?}",
            descendant_pids
                .iter()
                .filter(|process_id| process_snapshot.process(**process_id).is_some())
                .map(|process_id| process_id.as_u32())
                .collect::<Vec<_>>()
        );
        let mut killed_descendants = true;
        for process_id in descendant_pids {
            let Some(process) = process_snapshot.process(process_id) else {
                continue;
            };
            let killed = process.kill();
            #[cfg(test)]
            eprintln!(
                "[DEBUG-pr76-conpty] terminate descendant={} result={killed}",
                process_id.as_u32()
            );
            if !killed {
                log::error!(
                    "failed to terminate escaped terminal descendant process {}",
                    process_id.as_u32()
                );
                killed_descendants = false;
            }
        }

        killed_current_process && killed_descendants
    }

    #[cfg(all(not(unix), not(windows)))]
    pub(crate) fn kill_current_process(&self) -> bool {
        self.refresh().is_some_and(|process| process.kill())
    }

    pub(crate) fn kill_child_process(&self) -> bool {
        self.get_child().is_some_and(|process| process.kill())
    }

    #[cfg(unix)]
    pub(crate) fn terminate_child_process(&self) -> bool {
        let pid = self.pid_getter.fallback_pid();
        unsafe { libc::killpg(pid.as_u32() as i32, libc::SIGTERM) == 0 }
    }

    #[cfg(not(unix))]
    pub(crate) fn terminate_child_process(&self) -> bool {
        false
    }

    fn load(&self) -> Option<ProcessInfo> {
        let process = self.refresh()?;
        let cwd = process.cwd().map_or(PathBuf::new(), |p| p.to_owned());

        let info = ProcessInfo {
            name: process.name().to_str()?.to_owned(),
            cwd,
            argv: process
                .cmd()
                .iter()
                .filter_map(|s| s.to_str().map(ToOwned::to_owned))
                .collect(),
        };
        *self.current.write() = Some(info.clone());
        Some(info)
    }

    #[cfg(all(test, unix))]
    pub(crate) fn load_for_test(&self) -> Option<ProcessInfo> {
        self.load()
    }

    /// Updates the cached process info, emitting a [`Event::TitleChanged`] event if the Flint-relevant info has changed
    pub(crate) fn emit_title_changed_if_changed(self: &Arc<Self>, cx: &mut Context<'_, Terminal>) {
        if self.task.lock().is_some() {
            return;
        }
        let this = self.clone();
        let has_changed = cx.background_executor().spawn(async move {
            let previous = this.current.read().clone();
            let current = this.load();
            let has_changed = match (previous.as_ref(), current.as_ref()) {
                (None, None) => false,
                (Some(prev), Some(now)) => prev.cwd != now.cwd || prev.name != now.name,
                _ => true,
            };
            if has_changed {
                *this.current.write() = current;
            }
            has_changed
        });
        let this = Arc::downgrade(self);
        *self.task.lock() = Some(cx.spawn(async move |term, cx| {
            if has_changed.await {
                term.update(cx, |_, cx| cx.emit(Event::TitleChanged)).ok();
            }
            if let Some(this) = this.upgrade() {
                this.task.lock().take();
            }
        }));
    }

    pub(crate) fn pid(&self) -> Option<Pid> {
        self.pid_getter.pid()
    }
}

#[cfg(test)]
mod descendant_tests {
    use super::*;

    #[test]
    fn finds_nested_descendants_without_following_cycles() {
        let parent_process_ids = HashMap::from([(2, 1), (3, 2), (4, 3), (5, 9), (6, 7), (7, 6)]);

        let mut descendants = descendant_process_ids(1, &parent_process_ids);
        descendants.sort_unstable();

        assert_eq!(descendants, vec![2, 3, 4]);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "the test needs real short-lived child processes and may block"
    )]
    fn process_map_stays_bounded_during_foreground_process_churn() {
        let mut info = PtyProcessInfo::new(ProcessIdGetter::new(-1, std::process::id()));
        assert!(
            info.get_child().is_some(),
            "the spawned child must be inspectable before the first foreground refresh"
        );
        assert!(info.load_for_test().is_some());
        let initial_process_count = info.system.read().processes().len();
        assert!(
            initial_process_count <= 2,
            "creating one terminal retained {initial_process_count} process entries"
        );

        for _ in 0..3 {
            let mut child = std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .expect("spawn foreground process");
            info.pid_getter = ProcessIdGetter::new(-1, child.id());
            let loaded = info.load_for_test().is_some();
            child.kill().expect("kill foreground process");
            child.wait().expect("wait for foreground process");
            assert!(loaded, "foreground process should be inspectable");
        }

        let churned_process_count = info.system.read().processes().len();
        assert!(
            churned_process_count <= 2,
            "foreground churn retained {churned_process_count} process entries"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "the test needs real short-lived child processes and may block"
    )]
    fn foreground_process_churn_does_not_retain_procfs_descriptors() {
        fn open_descriptor_count() -> usize {
            std::fs::read_dir("/proc/self/fd")
                .expect("read process descriptors")
                .count()
        }

        let mut info = PtyProcessInfo::new(ProcessIdGetter::new(-1, std::process::id()));
        let baseline_descriptor_count = open_descriptor_count();

        for _ in 0..16 {
            let mut child = std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .expect("spawn foreground process");
            info.pid_getter = ProcessIdGetter::new(-1, child.id());
            let loaded = info.load_for_test().is_some();
            child.kill().expect("kill foreground process");
            child.wait().expect("wait for foreground process");
            assert!(loaded, "foreground process should be inspectable");
        }

        let churned_descriptor_count = open_descriptor_count();
        assert!(
            churned_descriptor_count <= baseline_descriptor_count + 2,
            "foreground churn retained {} procfs descriptors",
            churned_descriptor_count.saturating_sub(baseline_descriptor_count)
        );
        assert!(
            std::process::Command::new("true")
                .status()
                .expect("spawn process after descriptor stress")
                .success(),
            "new process should run after descriptor stress"
        );
    }
}
