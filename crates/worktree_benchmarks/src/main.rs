use std::{
    path::Path,
    sync::{Arc, atomic::AtomicUsize},
};

use anyhow::{Context as _, Result};
use fs::RealFs;
use settings::WorktreeId;
use tempfile::TempDir;
use worktree::Worktree;

fn main() -> Result<()> {
    let (worktree_root_path, _generated_fixture) = benchmark_root()?;
    let app = gpui_platform::headless();

    app.run(|cx| {
        settings::init(cx);
        let fs = Arc::new(RealFs::new(None, cx.background_executor().clone()));

        cx.spawn(async move |cx| {
            let worktree = Worktree::local(
                Path::new(&worktree_root_path),
                true,
                fs,
                Arc::new(AtomicUsize::new(0)),
                true,
                WorktreeId::from_proto(0),
                cx,
            )
            .await
            .expect("Worktree initialization to succeed");
            let did_finish_scan = worktree.update(cx, |this, _| {
                this.as_local()
                    .expect("benchmark worktree should be local")
                    .scan_complete()
            });
            let start = std::time::Instant::now();
            did_finish_scan.await;
            let elapsed = start.elapsed();
            let (files, directories) =
                worktree.read_with(cx, |this, _| (this.file_count(), this.dir_count()));
            println!(
                "{:?} for {directories} directories and {files} files",
                elapsed
            );
            cx.update(|cx| {
                cx.quit();
            })
        })
        .detach();
    });

    Ok(())
}

fn benchmark_root() -> Result<(String, Option<TempDir>)> {
    let mut arguments = std::env::args().skip(1);
    let Some(first_argument) = arguments.next() else {
        anyhow::bail!(
            "Missing benchmark input\nUsage: worktree_benchmarks PATH_TO_WORKTREE_ROOT\n       worktree_benchmarks --deferred-directories COUNT"
        );
    };

    if first_argument != "--deferred-directories" {
        return Ok((first_argument, None));
    }

    let directory_count = arguments
        .next()
        .context("Missing deferred directory count")?
        .parse::<usize>()
        .context("Deferred directory count must be a nonnegative integer")?;
    let fixture = tempfile::tempdir().context("Create benchmark fixture")?;
    std::fs::write(fixture.path().join(".gitignore"), "ignored-*\n")
        .context("Write benchmark .gitignore")?;
    for index in 0..directory_count {
        std::fs::create_dir(fixture.path().join(format!("ignored-{index:08}")))
            .context("Create deferred benchmark directory")?;
    }

    Ok((fixture.path().to_string_lossy().into_owned(), Some(fixture)))
}
