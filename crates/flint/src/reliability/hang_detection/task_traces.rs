use std::path::PathBuf;
use std::thread::ThreadId;

use anyhow::Context;
use gpui::{SerializedThreadTaskTimings, TasksIncluded};
use util::ResultExt;

use crate::STARTUP_TIME;

pub fn save_any(main_thread_id: ThreadId) -> Option<PathBuf> {
    cleanup_old_hang_traces();
    let Some(startup_time) = STARTUP_TIME.get() else {
        log::error!("Cannot save hang trace before startup time is initialized");
        return None;
    };
    let thread_timings = gpui::profiler::get_all_timings(TasksIncluded::CompletedAndRunning);

    let thread_timings = thread_timings
        .into_iter()
        .map(|mut timings| {
            if timings.thread_id == main_thread_id {
                timings.thread_name = Some("main".to_string());
            }

            SerializedThreadTaskTimings::convert(*startup_time, timings)
        })
        .collect::<Vec<_>>();

    let Some(timings) = serde_json::to_string(&thread_timings)
        .context("hang timings serialization")
        .log_err()
    else {
        return None;
    };

    // The trace is written whether or not tracing is enabled. With tracing on it
    // also carries each thread's recent task history; with tracing off it still
    // names the task that was running when the hang was sampled, which is the
    // part that identifies the hang.
    let trace_path = paths::hang_traces_dir().join(&format!(
        "hang-{}.miniprof.json",
        chrono::Local::now().format("%Y-%m-%d_%H-%M-%S")
    ));
    std::fs::write(&trace_path, timings)
        .with_context(|| format!("writing hang trace to {}", trace_path.display()))
        .map(|()| trace_path)
        .log_err()
}

pub fn cleanup_old_hang_traces() {
    if let Err(error) = cleanup_old_hang_traces_in(paths::hang_traces_dir()) {
        log::warn!("Failed to clean up old hang traces: {error}");
    }
}

fn cleanup_old_hang_traces_in(directory: &std::path::Path) -> anyhow::Result<()> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut files = entries
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|entry| {
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            file_name.starts_with("hang-") && file_name.ends_with(".miniprof.json")
        })
        .collect::<Vec<_>>();

    const MAX_HANG_TRACES: usize = 3;
    files.sort_by_key(|entry| entry.file_name());
    for entry in files
        .iter()
        .take(files.len().saturating_sub(MAX_HANG_TRACES))
    {
        std::fs::remove_file(entry.path())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_keeps_three_newest_hang_traces_and_ignores_other_files() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        for index in 1..=4 {
            std::fs::write(
                directory
                    .path()
                    .join(format!("hang-2026-01-01_00-00-0{index}.miniprof.json")),
                "[]",
            )?;
        }
        let unrelated = directory.path().join("profile.json");
        std::fs::write(&unrelated, "[]")?;

        cleanup_old_hang_traces_in(directory.path())?;

        let hang_trace_count = std::fs::read_dir(directory.path())?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("hang-"))
            .count();
        assert_eq!(hang_trace_count, 3);
        assert!(unrelated.exists());
        Ok(())
    }
}
