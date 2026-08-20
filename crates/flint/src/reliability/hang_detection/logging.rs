use std::fmt::Display;
use std::panic::Location;
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use collections::HashMap;
use itertools::Itertools;
use log::info;

#[derive(Debug, Hash, Eq, PartialEq)]
enum PerfIssue {
    Foreground(&'static Location<'static>),
    Background(&'static Location<'static>),
    Action(&'static str),
}

pub struct Reporter {
    forget_after: Duration,
    history: HashMap<PerfIssue, Instant>,
    report_longer_then: Duration,
    foreground_thread: ThreadId,
}

type ReportMade = bool;
impl Reporter {
    pub fn new(
        forget_after: Duration,
        report_longer_then: Duration,
        foreground_thread: ThreadId,
    ) -> Self {
        Self {
            forget_after,
            history: HashMap::default(),
            report_longer_then,
            foreground_thread,
        }
    }
    pub fn check_and_report(
        &mut self,
        task_stats: &[gpui::ThreadTaskStatistics],
        action_stats: &[gpui::ThreadActionStatistics],
    ) -> ReportMade {
        // History is updated once, after every reporter has run, so that the
        // same location hanging on several threads within one sample is still
        // reported for each of them.
        let mut reported = Vec::new();
        let mut reported_task_hangs = false;
        reported_task_hangs |= self.report_hanging_foreground(task_stats, &mut reported);
        reported_task_hangs |= self.report_hanging_background(task_stats, &mut reported);

        self.report_hanging_actions(action_stats, &mut reported);
        self.update_reported(reported);
        reported_task_hangs
    }

    fn hold_report(&self, issue: PerfIssue) -> bool {
        self.history
            .get(&issue)
            .map(Instant::elapsed)
            .is_some_and(|since_report| since_report < self.forget_after)
    }
    fn update_reported(&mut self, new: Vec<PerfIssue>) {
        let now = Instant::now();
        for issue in new {
            self.history.insert(issue, now);
        }
    }
}

impl Reporter {
    fn report_hanging_foreground(
        &mut self,
        task_stats: &[gpui::ThreadTaskStatistics],
        reported: &mut Vec<PerfIssue>,
    ) -> ReportMade {
        let foreground = self.foreground_thread;
        let Some(foreground) = task_stats.iter().find(|t| t.thread_id == foreground) else {
            return false; // during startup the foreground may not yet have statistics
        };

        let hangs: Vec<_> = foreground
            .stats
            .longest_poll_times
            .into_iter()
            .filter(|task| task.poll_duration() > self.report_longer_then)
            .filter(|task| !self.hold_report(PerfIssue::Foreground(task.location)))
            .collect();
        reported.extend(
            hangs
                .iter()
                .map(|task| PerfIssue::Foreground(task.location)),
        );
        if !hangs.is_empty() {
            info!("New foreground hang detected:\n{}", DisplayTasks(&hangs));
        }

        !hangs.is_empty()
    }

    fn report_hanging_background(
        &mut self,
        task_stats: &[gpui::ThreadTaskStatistics],
        reported: &mut Vec<PerfIssue>,
    ) -> ReportMade {
        let foreground = self.foreground_thread;
        let background = task_stats.iter().filter(move |t| t.thread_id != foreground);

        let mut report_made = false;
        for worker in background {
            let hangs: Vec<_> = worker
                .stats
                .longest_poll_times
                .into_iter()
                .filter(|stat| stat.poll_duration() > self.report_longer_then)
                .filter(|task| !self.hold_report(PerfIssue::Background(task.location)))
                .collect();

            if hangs.is_empty() {
                continue;
            }

            reported.extend(
                hangs
                    .iter()
                    .map(|task| PerfIssue::Background(task.location)),
            );

            info!(
                "Background hang detected on {}:\n{}",
                worker.thread_name.as_deref().unwrap_or_else(|| "Unknown"),
                DisplayTasks(&hangs)
            );
            report_made = true;
        }
        report_made
    }

    fn report_hanging_actions(
        &mut self,
        action_stats: &[gpui::ThreadActionStatistics],
        reported: &mut Vec<PerfIssue>,
    ) {
        let foreground = self.foreground_thread;
        let Some(foreground) = action_stats.iter().find(|t| t.thread_id == foreground) else {
            return; // during startup the foreground may not yet have statistics
        };

        let hangs: Vec<_> = foreground
            .statistics
            .longest_runtimes(true)
            .filter(|action| action.runtime() > self.report_longer_then)
            .filter(|action| !self.hold_report(PerfIssue::Action(action.name)))
            .collect();

        reported.extend(hangs.iter().map(|action| PerfIssue::Action(action.name)));
        if !hangs.is_empty() {
            info!("Action hang detected:\n{}", DisplayActions(hangs));
        }
    }
}

struct DisplayActions(Vec<gpui::profiler::ActionTiming>);
struct DisplayTasks<'a>(&'a [gpui::TaskTiming]);

impl Display for DisplayActions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Actions(s) that ran too long\n")?;
        for action in self.0.iter().sorted_by_key(|action| action.runtime()).rev() {
            f.write_fmt(format_args!(
                "{:<20} - {}",
                format!("{:?}", action.runtime()), // impl debug does not support alignment
                action.name
            ))?;
            writeln!(f)?;
        }
        Ok(())
    }
}

impl<'a> Display for DisplayTasks<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tasks(s) that ran too long\n")?;
        for task in self
            .0
            .iter()
            .sorted_by_key(|task| task.poll_duration())
            .rev()
        {
            f.write_fmt(format_args!(
                "{:<20} - {}",
                format!("{:?}", task.poll_duration()), // impl debug does not support alignment
                task.location
            ))?;
            writeln!(f)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TaskStatistics, TaskTiming, ThreadTaskStatistics, YieldTime};
    use std::thread;

    const HANG: Duration = Duration::from_millis(500);
    const REPORT_LONGER_THEN: Duration = Duration::from_millis(100);

    #[track_caller]
    fn hanging_task(poll_duration: Duration) -> TaskTiming {
        let mut task = TaskTiming::placeholder();
        task.location = std::panic::Location::caller();
        task.end = YieldTime(task.start + poll_duration);
        task
    }

    fn thread_stats(thread_id: ThreadId, name: &str, task: TaskTiming) -> ThreadTaskStatistics {
        let mut stats = TaskStatistics::default();
        stats.longest_poll_times[0] = task;
        ThreadTaskStatistics {
            thread_name: Some(name.to_string()),
            thread_id,
            stats,
        }
    }

    fn reporter(forget_after: Duration, foreground_thread: ThreadId) -> Reporter {
        Reporter::new(forget_after, REPORT_LONGER_THEN, foreground_thread)
    }

    fn other_thread_id() -> ThreadId {
        let handle = thread::spawn(|| {});
        let thread_id = handle.thread().id();
        handle.join().ok();
        thread_id
    }

    #[test]
    fn foreground_hang_is_reported_again_once_the_history_expires() {
        let foreground = thread::current().id();
        let forget_after = Duration::from_millis(20);
        let mut reporter = reporter(forget_after, foreground);
        let stats = [thread_stats(foreground, "main", hanging_task(HANG))];

        assert!(reporter.check_and_report(&stats, &[]));
        // The same location within the forget window stays quiet, so a busy
        // location does not flood the log.
        assert!(!reporter.check_and_report(&stats, &[]));

        thread::sleep(forget_after * 2);
        assert!(
            reporter.check_and_report(&stats, &[]),
            "a location that keeps hanging must be reported again"
        );
    }

    #[test]
    fn one_location_hanging_on_several_workers_is_reported_for_each() {
        let foreground = thread::current().id();
        let mut reporter = reporter(Duration::from_mins(5), foreground);
        let task = hanging_task(HANG);
        let stats = [
            thread_stats(other_thread_id(), "Worker-1", task),
            thread_stats(other_thread_id(), "Worker-2", task),
        ];

        let mut reported = Vec::new();
        assert!(reporter.report_hanging_background(&stats, &mut reported));
        assert_eq!(
            reported.len(),
            2,
            "history is only updated after every worker has been checked"
        );
    }

    #[test]
    fn tasks_below_the_threshold_are_not_reported() {
        let foreground = thread::current().id();
        let mut reporter = reporter(Duration::from_mins(5), foreground);
        let stats = [thread_stats(
            foreground,
            "main",
            hanging_task(REPORT_LONGER_THEN / 2),
        )];

        assert!(!reporter.check_and_report(&stats, &[]));
        assert!(reporter.history.is_empty());
    }
}
