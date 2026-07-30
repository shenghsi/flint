use gpui::{BackgroundExecutor, Task};
use notify::{Event, EventKind};
use parking_lot::Mutex;
use std::{
    collections::HashMap,
    ops::DerefMut,
    path::Path,
    sync::{Arc, LazyLock, OnceLock},
    time::{Duration, Instant},
};
use util::{ResultExt, paths::SanitizedPath};

use crate::{PathEvent, PathEventKind, Watcher};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum WatcherMode {
    #[default]
    Native,
    Poll,
}

pub struct FsWatcher {
    executor: BackgroundExecutor,
    tx: async_channel::Sender<()>,
    pending_path_events: Arc<Mutex<Vec<PathEvent>>>,
    registrations: Arc<Mutex<HashMap<WatchKey, FsWatcherRegistration>>>,
    pending_registrations: Arc<Mutex<HashMap<Arc<std::path::Path>, Task<()>>>>,
}

struct FsWatcherRegistration {
    id: WatcherRegistrationId,
    mode: WatcherMode,
    _parent_rename_watcher: Option<Box<dyn WatchBackend>>,
}

fn remove_fs_watcher_registration(registration: FsWatcherRegistration) {
    global_watcher().remove(registration.id);
}

impl FsWatcher {
    pub fn new(
        executor: BackgroundExecutor,
        tx: async_channel::Sender<()>,
        pending_path_events: Arc<Mutex<Vec<PathEvent>>>,
    ) -> Self {
        Self {
            executor,
            tx,
            pending_path_events,
            registrations: Default::default(),
            pending_registrations: Default::default(),
        }
    }

    fn add_existing_path(&self, path: Arc<Path>) -> anyhow::Result<()> {
        let case_insensitive = case_insensitive_path(&path);
        let key = WatchKey::for_registration(SanitizedPath::new(&path), case_insensitive);
        if self.registrations.lock().contains_key(&key) {
            log::trace!("path to watch is already watched: {path:?}");
            return Ok(());
        }

        match register_existing_path(
            path.clone(),
            case_insensitive,
            self.tx.clone(),
            self.pending_path_events.clone(),
        )? {
            Some(registration) => {
                self.registrations.lock().insert(key, registration);
            }
            None => {
                log::warn!("watch registration for {path:?} was skipped; retrying");
                self.add_pending_path(path);
            }
        }
        Ok(())
    }

    fn add_pending_path(&self, path: Arc<Path>) {
        let mut pending_registrations = self.pending_registrations.lock();
        if pending_registrations.contains_key(path.as_ref()) {
            return;
        }

        let task = self.executor.spawn(poll_path_until_created(
            self.executor.clone(),
            path.clone(),
            self.tx.clone(),
            self.pending_path_events.clone(),
            self.registrations.clone(),
            self.pending_registrations.clone(),
        ));
        pending_registrations.insert(path, task);
    }
}

impl Drop for FsWatcher {
    fn drop(&mut self) {
        self.pending_registrations.lock().clear();

        let mut registrations = HashMap::new();
        {
            let old = &mut self.registrations.lock();
            std::mem::swap(old.deref_mut(), &mut registrations);
        }

        for (_, registration) in registrations {
            remove_fs_watcher_registration(registration);
        }
    }
}

impl Watcher for FsWatcher {
    fn add(&self, path: &std::path::Path) -> anyhow::Result<()> {
        log::trace!("watcher add: {path:?}");

        let path: Arc<Path> = path.into();
        if path_covered_by_recursive_registration(
            &self.registrations.lock(),
            SanitizedPath::new(&path),
        ) {
            log::trace!("path to watch is covered by an existing registration: {path:?}");
            return Ok(());
        }

        if self
            .pending_registrations
            .lock()
            .contains_key(path.as_ref())
        {
            log::trace!("path to watch is already pending: {path:?}");
            return Ok(());
        }

        if std::fs::symlink_metadata(path.as_ref()).is_err() {
            self.add_pending_path(path);
            return Ok(());
        }

        self.add_existing_path(path)
    }

    fn remove(&self, path: &std::path::Path) -> anyhow::Result<()> {
        log::trace!("remove watched path: {path:?}");
        self.pending_registrations.lock().remove(path);

        let path = SanitizedPath::new(path);
        let registration = {
            let mut registrations = self.registrations.lock();
            registrations
                .remove(&WatchKey::exact(path))
                .or_else(|| registrations.remove(&WatchKey::folded(path)))
        };
        if let Some(registration) = registration {
            remove_fs_watcher_registration(registration);
        }
        Ok(())
    }
}

fn path_covered_by_recursive_registration(
    registrations: &HashMap<WatchKey, FsWatcherRegistration>,
    path: &SanitizedPath,
) -> bool {
    path.as_path().ancestors().skip(1).any(|ancestor| {
        let ancestor = SanitizedPath::unchecked_new(ancestor);
        [WatchKey::exact(ancestor), WatchKey::folded(ancestor)]
            .iter()
            .any(|key| {
                registrations.get(key).is_some_and(|registration| {
                    registration.mode == WatcherMode::Poll
                        || cfg!(any(target_os = "windows", target_os = "macos"))
                })
            })
    })
}

/// Detect whether a path requires polling instead of native file watching.
///
/// Returns `true` for filesystem types where inotify/FSEvents/ReadDirectoryChanges
/// silently fail to deliver events: 9P (WSL drvfs), NFS, CIFS/SMB, FUSE (sshfs), etc.
///
/// Can be overridden with the `ZED_FILE_WATCHER_MODE` environment variable:
/// - `native` — always use native OS watcher
/// - `poll` — always use polling
/// - `auto` (default) — auto-detect based on filesystem type
pub fn requires_poll_watcher(path: &Path) -> bool {
    match std::env::var("ZED_FILE_WATCHER_MODE")
        .as_deref()
        .unwrap_or("auto")
    {
        "native" => return false,
        "poll" => return true,
        _ => {}
    }

    #[cfg(target_os = "linux")]
    {
        return detect_requires_poll_watcher_linux(path);
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        false
    }
}

fn register_existing_path(
    path: Arc<Path>,
    case_insensitive: bool,
    tx: async_channel::Sender<()>,
    pending_path_events: Arc<Mutex<Vec<PathEvent>>>,
) -> anyhow::Result<Option<FsWatcherRegistration>> {
    let mode = if requires_poll_watcher(path.as_ref()) {
        log::info!(
            "Using poll watcher ({}ms interval) for {}",
            poll_interval().as_millis(),
            path.display()
        );
        WatcherMode::Poll
    } else {
        WatcherMode::Native
    };
    #[cfg(target_os = "windows")]
    let parent_rename_watch = register_parent_rename_watch(
        &path,
        case_insensitive,
        tx.clone(),
        pending_path_events.clone(),
    );
    #[cfg(not(target_os = "windows"))]
    let parent_rename_watch = None;
    let root_path = SanitizedPath::new_arc(path.as_ref());
    let path_for_callback = path.clone();
    let registration = global_watcher().add(path, mode, case_insensitive, {
        move |event: &notify::Event| {
            log::trace!("watcher received event: {event:?}");
            push_notify_event(
                &tx,
                &pending_path_events,
                &root_path,
                case_insensitive,
                path_for_callback.as_ref(),
                event,
            );
        }
    });
    let registration_id = match registration {
        Ok(Some(registration_id)) => registration_id,
        Ok(None) => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(Some(FsWatcherRegistration {
        id: registration_id,
        mode,
        _parent_rename_watcher: parent_rename_watch,
    }))
}

#[cfg(target_os = "windows")]
fn register_parent_rename_watch(
    path: &Arc<Path>,
    case_insensitive: bool,
    tx: async_channel::Sender<()>,
    pending_path_events: Arc<Mutex<Vec<PathEvent>>>,
) -> Option<Box<dyn WatchBackend>> {
    let parent = path.parent()?;
    let root_path = SanitizedPath::new_arc(path);
    let config = notify::Config::default().with_event_kinds(notify::EventKindMask::CORE);
    let mut watcher = <notify::RecommendedWatcher as notify::Watcher>::new(
        move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else {
                return;
            };
            if !matches!(
                event.kind,
                EventKind::Remove(_)
                    | EventKind::Create(_)
                    | EventKind::Modify(notify::event::ModifyKind::Name(_))
            ) {
                return;
            }

            let root_touched = event.paths.iter().any(|event_path| {
                if case_insensitive {
                    WatchKey::folded_path(SanitizedPath::new(event_path))
                        == WatchKey::folded_path(&root_path)
                } else {
                    event_path == root_path.as_path()
                }
            });
            if root_touched {
                enqueue_path_events(
                    &tx,
                    &pending_path_events,
                    vec![PathEvent {
                        path: root_path.as_path().to_path_buf(),
                        kind: Some(PathEventKind::Rescan),
                    }],
                );
            }
        },
        config,
    )
    .log_err()?;
    notify::Watcher::watch(&mut watcher, parent, notify::RecursiveMode::NonRecursive).log_err()?;
    Some(Box::new(watcher))
}

#[cfg(target_os = "linux")]
fn detect_requires_poll_watcher_linux(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = match CString::new(path.as_os_str().as_bytes()) {
        Ok(p) => p,
        Err(_) => return false,
    };

    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut stat) } != 0 {
        return false;
    }

    const V9FS_MAGIC: u64 = 0x0102_1997;
    const NFS_SUPER_MAGIC: u64 = 0x0000_6969;
    const CIFS_MAGIC: u64 = 0xFF53_4D42;
    const SMB_SUPER_MAGIC: u64 = 0x0000_517B;
    const SMB2_MAGIC: u64 = 0xFE53_4D42;
    const FUSE_SUPER_MAGIC: u64 = 0x6573_5546;

    let fs_type = (stat.f_type as u64) & 0xFFFF_FFFF;
    if fs_type == FUSE_SUPER_MAGIC && is_virtiofs(path) {
        return false;
    }

    if fs_type == V9FS_MAGIC
        || fs_type == NFS_SUPER_MAGIC
        || fs_type == CIFS_MAGIC
        || fs_type == SMB_SUPER_MAGIC
        || fs_type == SMB2_MAGIC
        || fs_type == FUSE_SUPER_MAGIC
    {
        log::info!(
            "Detected network/virtual filesystem (type 0x{:x}) at {}, using poll watcher",
            fs_type,
            path.display()
        );
        return true;
    }

    if is_wsl_drvfs_path(path) {
        log::info!(
            "Detected WSL drvfs mount at {}, using poll watcher",
            path.display()
        );
        return true;
    }

    false
}

#[cfg(target_os = "linux")]
fn is_virtiofs(path: &Path) -> bool {
    let Ok(mountinfo) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };

    let mut best_mount = None;
    for line in mountinfo.lines() {
        let fields = line.split(' ').collect::<Vec<_>>();
        let Some(separator) = fields.iter().position(|field| *field == "-") else {
            continue;
        };
        let (Some(mount_point), Some(fs_type)) = (fields.get(4), fields.get(separator + 1)) else {
            continue;
        };

        let mount_point = mount_point
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\");
        if path.starts_with(&mount_point)
            && best_mount.is_none_or(|(length, _)| mount_point.len() > length)
        {
            best_mount = Some((mount_point.len(), *fs_type));
        }
    }

    best_mount.is_some_and(|(_, fs_type)| fs_type == "virtiofs" || fs_type == "fuse.virtiofs")
}

#[cfg(target_os = "linux")]
fn is_wsl_drvfs_path(path: &Path) -> bool {
    if std::env::var_os("WSL_DISTRO_NAME").is_none() {
        if let Ok(version) = std::fs::read_to_string("/proc/version") {
            let version = version.to_lowercase();
            if !version.contains("microsoft") && !version.contains("wsl") {
                return false;
            }
        } else {
            return false;
        }
    }

    let Some(path) = path.to_str() else {
        return false;
    };
    if !path.starts_with("/mnt/") || path.len() < 6 {
        return false;
    }
    let after_mnt = &path[5..];
    after_mnt.starts_with(|c: char| c.is_ascii_alphabetic())
        && (after_mnt.len() == 1 || after_mnt.as_bytes()[1] == b'/')
}

#[cfg(target_os = "macos")]
fn case_insensitive_path(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;

    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return true;
    };
    // SAFETY: `path` is a valid, null-terminated string for the duration of the call.
    unsafe { libc::pathconf(path.as_ptr(), libc::_PC_CASE_SENSITIVE) == 0 }
}

#[cfg(target_os = "windows")]
fn case_insensitive_path(_path: &Path) -> bool {
    true
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn case_insensitive_path(_path: &Path) -> bool {
    false
}

fn path_is_under(path: &SanitizedPath, root: &SanitizedPath, case_insensitive: bool) -> bool {
    if !case_insensitive {
        return path.starts_with(root);
    }

    let path = WatchKey::folded_path(path);
    let root = WatchKey::folded_path(root);
    Path::new(path.as_ref()).starts_with(Path::new(root.as_ref()))
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum WatchKey {
    Exact(Arc<Path>),
    Folded(Arc<str>),
}

impl WatchKey {
    fn exact(path: &SanitizedPath) -> Self {
        Self::Exact(Arc::from(path.as_path()))
    }

    fn folded(path: &SanitizedPath) -> Self {
        Self::Folded(Self::folded_path(path))
    }

    fn folded_path(path: &SanitizedPath) -> Arc<str> {
        let path = path.as_path().to_string_lossy();
        #[cfg(target_os = "macos")]
        let path = {
            use unicode_normalization::UnicodeNormalization as _;
            path.chars().nfc().collect::<String>().to_lowercase()
        };
        #[cfg(not(target_os = "macos"))]
        let path = path.to_lowercase();
        path.into()
    }

    fn for_registration(path: &SanitizedPath, case_insensitive: bool) -> Self {
        if case_insensitive {
            Self::folded(path)
        } else {
            Self::exact(path)
        }
    }
}

async fn poll_path_until_created(
    executor: BackgroundExecutor,
    path: Arc<Path>,
    tx: async_channel::Sender<()>,
    pending_path_events: Arc<Mutex<Vec<PathEvent>>>,
    registrations: Arc<Mutex<HashMap<WatchKey, FsWatcherRegistration>>>,
    pending_registrations: Arc<Mutex<HashMap<Arc<Path>, Task<()>>>>,
) {
    loop {
        executor.timer(poll_interval()).await;

        if !pending_registrations.lock().contains_key(path.as_ref()) {
            return;
        }

        if smol::fs::symlink_metadata(path.as_ref()).await.is_err() {
            continue;
        }

        let case_insensitive = case_insensitive_path(path.as_ref());
        let key = WatchKey::for_registration(SanitizedPath::new(&path), case_insensitive);
        if registrations.lock().contains_key(&key) {
            pending_registrations.lock().remove(path.as_ref());
            return;
        }

        match register_existing_path(
            path.clone(),
            case_insensitive,
            tx.clone(),
            pending_path_events.clone(),
        ) {
            Ok(Some(registration)) => {
                {
                    let mut pending_registrations = pending_registrations.lock();
                    if pending_registrations.remove(path.as_ref()).is_none() {
                        global_watcher().remove(registration.id);
                        return;
                    }
                    registrations.lock().insert(key, registration);
                }
                enqueue_path_events(
                    &tx,
                    &pending_path_events,
                    vec![
                        PathEvent {
                            path: path.to_path_buf(),
                            kind: Some(PathEventKind::Created),
                        },
                        PathEvent {
                            path: path.to_path_buf(),
                            kind: Some(PathEventKind::Rescan),
                        },
                    ],
                );
                return;
            }
            Ok(None) => {}
            Err(error) => {
                log::warn!("failed to watch newly-created path {path:?}: {error}; retrying");
            }
        }
    }
}

fn enqueue_path_events(
    tx: &smol::channel::Sender<()>,
    pending_path_events: &Arc<Mutex<Vec<PathEvent>>>,
    mut path_events: Vec<PathEvent>,
) {
    if path_events.is_empty() {
        return;
    }

    path_events.sort();
    let mut pending_paths = pending_path_events.lock();
    if pending_paths.is_empty() {
        if let Err(error) = tx.try_send(()) {
            log::warn!("failed to notify filesystem event consumer: {error}");
        }
    }
    coalesce_pending_rescans(&mut pending_paths, &mut path_events);
    util::extend_sorted(&mut *pending_paths, path_events, usize::MAX, |a, b| {
        a.path.cmp(&b.path)
    });
}

fn push_notify_event(
    tx: &smol::channel::Sender<()>,
    pending_path_events: &Arc<Mutex<Vec<PathEvent>>>,
    root_path: &SanitizedPath,
    case_insensitive: bool,
    watched_root: &Path,
    event: &notify::Event,
) {
    let kind = match event.kind {
        EventKind::Create(_) => Some(PathEventKind::Created),
        EventKind::Modify(_) => Some(PathEventKind::Changed),
        EventKind::Remove(_) => Some(PathEventKind::Removed),
        _ => None,
    };
    let mut path_events = event
        .paths
        .iter()
        .filter_map(|event_path| {
            let event_path = SanitizedPath::new(event_path);
            path_is_under(event_path, root_path, case_insensitive).then(|| PathEvent {
                path: event_path.as_path().to_path_buf(),
                kind,
            })
        })
        .collect::<Vec<_>>();

    if event.need_rescan() {
        if !watcher_logging_rate_limited() {
            log::warn!("filesystem watcher lost sync for {watched_root:?}; scheduling rescan");
        }

        path_events.retain(|path_event| path_event.path != watched_root);
        path_events.push(PathEvent {
            path: watched_root.to_path_buf(),
            kind: Some(PathEventKind::Rescan),
        });
    }
    log::trace!("path_events: {:?}", path_events);
    enqueue_path_events(tx, pending_path_events, path_events);
}

fn watcher_logging_rate_limited() -> bool {
    static LAST_WARN: Mutex<Option<(Instant, usize)>> = Mutex::new(None);
    let Some((ref mut started, ref mut emitted)) = *LAST_WARN.lock() else {
        *LAST_WARN.lock() = Some((Instant::now(), 0));
        return false;
    };

    if started.elapsed().as_secs() < 1 {
        if *emitted < 20 {
            log::warn!("filesystem watcher lost sync for many files, not logging more");
            return true;
        } else {
            *emitted += 1;
        }
    } else {
        *emitted = 0;
        *started = Instant::now()
    }

    true
}

fn coalesce_pending_rescans(pending_paths: &mut Vec<PathEvent>, path_events: &mut Vec<PathEvent>) {
    if !path_events
        .iter()
        .any(|event| event.kind == Some(PathEventKind::Rescan))
    {
        return;
    }

    let mut new_rescan_paths: Vec<std::path::PathBuf> = path_events
        .iter()
        .filter(|e| e.kind == Some(PathEventKind::Rescan))
        .map(|e| e.path.clone())
        .collect();
    new_rescan_paths.sort_unstable();

    let mut deduped_rescans: Vec<std::path::PathBuf> = Vec::with_capacity(new_rescan_paths.len());
    for path in new_rescan_paths {
        if deduped_rescans
            .iter()
            .any(|ancestor| path != *ancestor && path.starts_with(ancestor))
        {
            continue;
        }
        deduped_rescans.push(path);
    }

    deduped_rescans.retain(|new_path| {
        !pending_paths
            .iter()
            .any(|pending| is_covered_rescan(pending.kind, new_path, &pending.path))
    });

    if !deduped_rescans.is_empty() {
        pending_paths.retain(|pending| {
            !deduped_rescans.iter().any(|rescan_path| {
                pending.path == *rescan_path
                    || is_covered_rescan(pending.kind, &pending.path, rescan_path)
            })
        });
    }

    path_events.retain(|event| {
        event.kind != Some(PathEventKind::Rescan) || deduped_rescans.contains(&event.path)
    });
}

fn is_covered_rescan(kind: Option<PathEventKind>, path: &Path, ancestor: &Path) -> bool {
    kind == Some(PathEventKind::Rescan) && path != ancestor && path.starts_with(ancestor)
}

#[derive(Default, Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct WatcherRegistrationId(u32);

struct WatcherRegistrationState {
    callback: Arc<dyn Fn(&notify::Event) + Send + Sync>,
    key: WatchKey,
    path: Arc<SanitizedPath>,
    mode: WatcherMode,
}

struct PathRegistrationState {
    watcher_ids: Vec<WatcherRegistrationId>,
    has_os_watcher: bool,
}

#[derive(Default)]
struct WatchPaths(HashMap<WatchKey, PathRegistrationState>);

impl WatchPaths {
    fn contains(&self, key: &WatchKey) -> bool {
        self.0.contains_key(key)
    }

    fn get_mut(&mut self, key: &WatchKey) -> Option<&mut PathRegistrationState> {
        self.0.get_mut(key)
    }

    fn entry(
        &mut self,
        key: WatchKey,
    ) -> std::collections::hash_map::Entry<'_, WatchKey, PathRegistrationState> {
        self.0.entry(key)
    }

    fn remove(&mut self, key: &WatchKey) {
        self.0.remove(key);
    }

    fn covered_by_recursive_ancestor(&self, path: &SanitizedPath, mode: WatcherMode) -> bool {
        if mode != WatcherMode::Poll && !cfg!(any(target_os = "windows", target_os = "macos")) {
            return false;
        }

        path.as_path().ancestors().skip(1).any(|ancestor| {
            let ancestor = SanitizedPath::unchecked_new(ancestor);
            self.0.contains_key(&WatchKey::exact(ancestor))
                || self.0.contains_key(&WatchKey::folded(ancestor))
        })
    }

    fn watcher_ids_covering(
        &self,
        path: &SanitizedPath,
        watcher_ids: &mut Vec<WatcherRegistrationId>,
    ) {
        for ancestor in path.as_path().ancestors() {
            let ancestor = SanitizedPath::unchecked_new(ancestor);
            if let Some(registration) = self.0.get(&WatchKey::exact(ancestor)) {
                watcher_ids.extend_from_slice(&registration.watcher_ids);
            }
            if let Some(registration) = self.0.get(&WatchKey::folded(ancestor)) {
                watcher_ids.extend_from_slice(&registration.watcher_ids);
            }
        }
    }
}

struct WatcherState {
    watchers: HashMap<WatcherRegistrationId, WatcherRegistrationState>,
    native_path_registrations: WatchPaths,
    poll_path_registrations: WatchPaths,
    cooldown_until: Option<Instant>,
    last_registration: WatcherRegistrationId,
}

impl WatcherState {
    fn is_native_watch_limit_cooldown_active(&self) -> bool {
        self.cooldown_until
            .is_some_and(|cooldown_until| cooldown_until > Instant::now())
    }

    fn path_registrations(&mut self, mode: WatcherMode) -> &mut WatchPaths {
        match mode {
            WatcherMode::Native => &mut self.native_path_registrations,
            WatcherMode::Poll => &mut self.poll_path_registrations,
        }
    }

    fn remove_registration(
        &mut self,
        id: WatcherRegistrationId,
    ) -> Option<(Arc<SanitizedPath>, WatcherMode)> {
        let registration_state = self.watchers.remove(&id)?;
        let path_registrations = self.path_registrations(registration_state.mode);
        let path_state = path_registrations.get_mut(&registration_state.key)?;
        path_state
            .watcher_ids
            .retain(|existing_id| *existing_id != id);
        if !path_state.watcher_ids.is_empty() {
            return None;
        }

        let was_actually_watched = path_state.has_os_watcher;
        path_registrations.remove(&registration_state.key);

        was_actually_watched.then_some((registration_state.path, registration_state.mode))
    }
}

trait WatchBackend: Send {
    fn watch(&mut self, path: &Path, mode: notify::RecursiveMode) -> notify::Result<()>;
    fn unwatch(&mut self, path: &Path) -> notify::Result<()>;
}

impl<T: notify::Watcher + Send> WatchBackend for T {
    fn watch(&mut self, path: &Path, mode: notify::RecursiveMode) -> notify::Result<()> {
        notify::Watcher::watch(self, path, mode)
    }

    fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        notify::Watcher::unwatch(self, path)
    }
}

type DispatchEvent = (WatcherMode, Result<notify::Event, notify::Error>);

pub struct GlobalWatcher {
    state: Mutex<WatcherState>,

    // DANGER: never keep state lock while holding watcher lock
    // two mutexes because calling watcher.add triggers watcher.event, which needs watchers.
    native_watcher: Mutex<Option<Box<dyn WatchBackend>>>,
    poll_watcher: Mutex<Option<Box<dyn WatchBackend>>>,
    event_tx: Option<async_channel::Sender<DispatchEvent>>,
}

impl GlobalWatcher {
    #[must_use]
    fn add(
        &self,
        path: Arc<std::path::Path>,
        mode: WatcherMode,
        case_insensitive: bool,
        cb: impl Fn(&notify::Event) + Send + Sync + 'static,
    ) -> anyhow::Result<Option<WatcherRegistrationId>> {
        let path = SanitizedPath::from_arc(path);
        let key = WatchKey::for_registration(&path, case_insensitive);
        let mut state = self.state.lock();
        let (path_already_covered, path_already_registered) = {
            let registrations_for_mode = state.path_registrations(mode);
            (
                registrations_for_mode.covered_by_recursive_ancestor(&path, mode),
                registrations_for_mode.contains(&key),
            )
        };

        if !path_already_covered && !path_already_registered {
            if mode == WatcherMode::Native && state.is_native_watch_limit_cooldown_active() {
                return Ok(None);
            }

            drop(state);
            match self.watch(path.as_path(), mode) {
                Ok(()) => {}
                Err(error) if mode == WatcherMode::Native && is_max_files_watch_error(&error) => {
                    self.start_native_watch_limit_cooldown(path.as_path());
                    return Ok(None);
                }
                Err(error) => return Err(error),
            }
            state = self.state.lock();
        }

        let id = state.last_registration;
        state.last_registration = WatcherRegistrationId(id.0 + 1);

        let registration_state = WatcherRegistrationState {
            callback: Arc::new(cb),
            key: key.clone(),
            path,
            mode,
        };
        state.watchers.insert(id, registration_state);
        state
            .path_registrations(mode)
            .entry(key)
            .and_modify(|registration| registration.watcher_ids.push(id))
            .or_insert(PathRegistrationState {
                watcher_ids: vec![id],
                has_os_watcher: !path_already_covered,
            });

        Ok(Some(id))
    }

    fn enqueue(&self, mode: WatcherMode, event: Result<notify::Event, notify::Error>) {
        if matches!(
            event,
            Ok(Event {
                kind: EventKind::Access(_),
                ..
            })
        ) {
            return;
        }

        if let Some(event_tx) = &self.event_tx {
            if let Err(error) = event_tx.try_send((mode, event)) {
                log::error!("failed to queue filesystem watcher event: {error}");
            }
        } else {
            self.dispatch(mode, event);
        }
    }

    fn dispatch(&self, mode: WatcherMode, event: Result<notify::Event, notify::Error>) {
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                log::warn!("watcher error for {mode:?}: {error}");
                return;
            }
        };

        log::trace!("global handle event for {mode:?}: {event:?}");

        let callbacks = {
            let state = self.state.lock();
            if event.need_rescan() {
                let callbacks = state
                    .watchers
                    .values()
                    .filter(|registration| registration.mode == mode)
                    .map(|registration| registration.callback.clone())
                    .collect::<Vec<_>>();
                log::warn!(
                    "filesystem watcher lost sync for {mode:?}; scheduling rescans for {} registrations",
                    callbacks.len()
                );
                callbacks
            } else {
                let path_registrations = match mode {
                    WatcherMode::Native => &state.native_path_registrations,
                    WatcherMode::Poll => &state.poll_path_registrations,
                };
                let mut watcher_ids = Vec::new();
                for path in &event.paths {
                    let path = SanitizedPath::new(path);
                    path_registrations.watcher_ids_covering(path, &mut watcher_ids);
                }
                watcher_ids.sort_unstable_by_key(|id| id.0);
                watcher_ids.dedup();
                watcher_ids
                    .into_iter()
                    .filter_map(|id| state.watchers.get(&id))
                    .map(|registration| registration.callback.clone())
                    .collect()
            }
        };

        for callback in callbacks {
            callback(&event);
        }
    }

    fn dispatch_batch(
        &self,
        first_event: DispatchEvent,
        event_rx: &async_channel::Receiver<DispatchEvent>,
    ) {
        let mut native_rescan_dispatched = false;
        let mut poll_rescan_dispatched = false;

        for (mode, event) in
            std::iter::once(first_event).chain(std::iter::from_fn(|| event_rx.try_recv().ok()))
        {
            let rescan_dispatched = match mode {
                WatcherMode::Native => &mut native_rescan_dispatched,
                WatcherMode::Poll => &mut poll_rescan_dispatched,
            };
            if event.as_ref().is_ok_and(notify::Event::need_rescan) {
                if *rescan_dispatched {
                    continue;
                }
                *rescan_dispatched = true;
            }

            self.dispatch(mode, event);
        }
    }

    fn start_native_watch_limit_cooldown(&self, path: &Path) {
        let mut state = self.state.lock();
        let now = Instant::now();
        let should_log = !state.is_native_watch_limit_cooldown_active();
        state.cooldown_until = Some(now + *NATIVE_WATCH_LIMIT_COOLDOWN);
        if should_log {
            log::warn!(
                "OS file watch limit reached while watching {path:?}; skipping new native file watcher registrations for {} seconds",
                NATIVE_WATCH_LIMIT_COOLDOWN.as_secs()
            );
        }
    }

    pub fn remove(&self, id: WatcherRegistrationId) {
        let mut state = self.state.lock();
        let Some((path, mode)) = state.remove_registration(id) else {
            return;
        };
        drop(state);
        self.unwatch(path.as_path(), mode).log_err();
    }

    fn watch(&self, path: &Path, mode: WatcherMode) -> anyhow::Result<()> {
        match mode {
            WatcherMode::Native => {
                self.ensure_native_watcher()?;
                let mut native_watcher = self.native_watcher.lock();
                let native_watcher = native_watcher
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("native watcher failed to initialize"))?;
                native_watcher.watch(
                    path,
                    if cfg!(any(target_os = "windows", target_os = "macos")) {
                        notify::RecursiveMode::Recursive
                    } else {
                        notify::RecursiveMode::NonRecursive
                    },
                )?;
            }
            WatcherMode::Poll => {
                self.ensure_poll_watcher()?;
                let mut poll_watcher = self.poll_watcher.lock();
                let poll_watcher = poll_watcher
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("poll watcher failed to initialize"))?;
                poll_watcher.watch(path, notify::RecursiveMode::Recursive)?;
            }
        }

        Ok(())
    }

    fn unwatch(&self, path: &Path, mode: WatcherMode) -> anyhow::Result<()> {
        let result = match mode {
            WatcherMode::Native => self
                .native_watcher
                .lock()
                .as_mut()
                .map(|watcher| watcher.unwatch(path)),
            WatcherMode::Poll => self
                .poll_watcher
                .lock()
                .as_mut()
                .map(|watcher| watcher.unwatch(path)),
        };

        match result {
            Some(Err(error)) if !matches!(error.kind, notify::ErrorKind::WatchNotFound) => {
                Err(error.into())
            }
            _ => Ok(()),
        }
    }

    fn ensure_native_watcher(&self) -> anyhow::Result<()> {
        if self.native_watcher.lock().is_some() {
            return Ok(());
        }

        let config = notify::Config::default().with_event_kinds(notify::EventKindMask::CORE);
        let watcher =
            <notify::RecommendedWatcher as notify::Watcher>::new(handle_native_event, config)?;
        *self.native_watcher.lock() = Some(Box::new(watcher));
        Ok(())
    }

    fn ensure_poll_watcher(&self) -> anyhow::Result<()> {
        if self.poll_watcher.lock().is_some() {
            return Ok(());
        }

        let config = notify::Config::default().with_poll_interval(*POLL_INTERVAL);
        let watcher = notify::PollWatcher::new(handle_poll_event, config)?;
        *self.poll_watcher.lock() = Some(Box::new(watcher));
        Ok(())
    }
}

fn is_max_files_watch_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<notify::Error>()
        .is_some_and(|error| matches!(&error.kind, notify::ErrorKind::MaxFilesWatch))
}

static POLL_INTERVAL: LazyLock<Duration> = LazyLock::new(|| {
    let poll_ms: u64 = std::env::var("ZED_FILE_WATCHER_POLL_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2000)
        .clamp(500, 30000);
    Duration::from_millis(poll_ms)
});

static NATIVE_WATCH_LIMIT_COOLDOWN: LazyLock<Duration> = LazyLock::new(|| {
    let cooldown_seconds: u64 = std::env::var("ZED_NATIVE_WATCH_LIMIT_COOLDOWN_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(5)
        .clamp(0, 300);
    Duration::from_secs(cooldown_seconds)
});

pub fn poll_interval() -> Duration {
    *POLL_INTERVAL
}

static FS_WATCHER_INSTANCE: OnceLock<GlobalWatcher> = OnceLock::new();

fn global_watcher() -> &'static GlobalWatcher {
    FS_WATCHER_INSTANCE.get_or_init(|| {
        let (event_tx, event_rx) = async_channel::unbounded::<DispatchEvent>();
        let event_tx = match std::thread::Builder::new()
            .name("fs-watcher-dispatch".to_owned())
            .spawn(move || {
                while let Ok(first_event) = event_rx.recv_blocking() {
                    global_watcher().dispatch_batch(first_event, &event_rx);
                }
            })
        {
            Ok(_) => Some(event_tx),
            Err(error) => {
                log::error!(
                    "failed to spawn filesystem watcher dispatch thread; dispatching events on the reader thread: {error}"
                );
                None
            }
        };

        GlobalWatcher {
            state: Mutex::new(WatcherState {
                watchers: Default::default(),
                native_path_registrations: Default::default(),
                poll_path_registrations: Default::default(),
                cooldown_until: None,
                last_registration: Default::default(),
            }),
            native_watcher: Mutex::new(None),
            poll_watcher: Mutex::new(None),
            event_tx,
        }
    })
}

fn handle_native_event(event: Result<notify::Event, notify::Error>) {
    handle_event(WatcherMode::Native, event);
}

fn handle_poll_event(event: Result<notify::Event, notify::Error>) {
    handle_event(WatcherMode::Poll, event);
}

fn handle_event(mode: WatcherMode, event: Result<notify::Event, notify::Error>) {
    global_watcher().enqueue(mode, event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::HashSet,
        path::PathBuf,
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        time::Duration,
    };

    static GLOBAL_WATCHER_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn rescan(path: &str) -> PathEvent {
        PathEvent {
            path: PathBuf::from(path),
            kind: Some(PathEventKind::Rescan),
        }
    }

    fn changed(path: &str) -> PathEvent {
        PathEvent {
            path: PathBuf::from(path),
            kind: Some(PathEventKind::Changed),
        }
    }

    #[derive(Default)]
    struct FakeWatchBackend {
        watched_paths: HashSet<PathBuf>,
        watch_calls: Vec<PathBuf>,
        unwatch_calls: Vec<PathBuf>,
        fail_with_watch_limit: bool,
        unwatch_returns_watch_not_found: bool,
    }

    struct SharedFakeWatchBackend(Arc<Mutex<FakeWatchBackend>>);

    impl WatchBackend for SharedFakeWatchBackend {
        fn watch(&mut self, path: &Path, _mode: notify::RecursiveMode) -> notify::Result<()> {
            let path = path.to_path_buf();
            let mut backend = self.0.lock();
            backend.watch_calls.push(path.clone());
            if backend.fail_with_watch_limit {
                return Err(notify::Error::new(notify::ErrorKind::MaxFilesWatch));
            }
            backend.watched_paths.insert(path);
            Ok(())
        }

        fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
            let path = path.to_path_buf();
            let mut backend = self.0.lock();
            backend.unwatch_calls.push(path.clone());
            if backend.unwatch_returns_watch_not_found {
                return Err(notify::Error::new(notify::ErrorKind::WatchNotFound));
            }
            if backend.watched_paths.remove(&path) {
                Ok(())
            } else {
                Err(notify::Error::generic("path was not watched"))
            }
        }
    }

    fn test_watcher(poll_watcher: Arc<Mutex<FakeWatchBackend>>) -> GlobalWatcher {
        test_watcher_with_backends(None, Some(poll_watcher))
    }

    fn test_watcher_with_backends(
        native_watcher: Option<Arc<Mutex<FakeWatchBackend>>>,
        poll_watcher: Option<Arc<Mutex<FakeWatchBackend>>>,
    ) -> GlobalWatcher {
        GlobalWatcher {
            state: Mutex::new(WatcherState {
                watchers: Default::default(),
                native_path_registrations: Default::default(),
                poll_path_registrations: Default::default(),
                cooldown_until: None,
                last_registration: Default::default(),
            }),
            native_watcher: Mutex::new(
                native_watcher.map(|watcher| {
                    Box::new(SharedFakeWatchBackend(watcher)) as Box<dyn WatchBackend>
                }),
            ),
            poll_watcher: Mutex::new(
                poll_watcher.map(|watcher| {
                    Box::new(SharedFakeWatchBackend(watcher)) as Box<dyn WatchBackend>
                }),
            ),
            event_tx: None,
        }
    }

    struct TestCase {
        name: &'static str,
        pending_paths: Vec<PathEvent>,
        path_events: Vec<PathEvent>,
        expected_pending_paths: Vec<PathEvent>,
        expected_path_events: Vec<PathEvent>,
    }

    #[test]
    fn covered_child_registration_is_not_unwatched_after_parent_is_removed() {
        let backend = Arc::new(Mutex::new(FakeWatchBackend::default()));
        let watcher = test_watcher(backend.clone());
        let parent = Arc::<Path>::from(Path::new("/repo"));
        let child = Arc::<Path>::from(Path::new("/repo/foo.csproj"));

        let parent_registration = watcher
            .add(parent.as_ref().into(), WatcherMode::Poll, false, |_| {})
            .expect("add parent watch")
            .expect("parent watch registered");
        let child_registration = watcher
            .add(child.as_ref().into(), WatcherMode::Poll, false, |_| {})
            .expect("add covered child watch")
            .expect("child watch registered");

        watcher.remove(parent_registration);
        watcher.remove(child_registration);

        let backend = backend.lock();
        assert_eq!(backend.watch_calls, &[parent.to_path_buf()]);
        assert_eq!(backend.unwatch_calls, &[parent.to_path_buf()]);
    }

    #[test]
    fn native_watch_limit_cools_down_subsequent_native_registrations() {
        let native_backend = Arc::new(Mutex::new(FakeWatchBackend {
            fail_with_watch_limit: true,
            ..Default::default()
        }));
        let poll_backend = Arc::new(Mutex::new(FakeWatchBackend::default()));
        let watcher = test_watcher_with_backends(Some(native_backend.clone()), Some(poll_backend));
        let first_path = Arc::<Path>::from(Path::new("/repo/first"));
        let second_path = Arc::<Path>::from(Path::new("/repo/second"));

        let first_registration = watcher
            .add(first_path.clone(), WatcherMode::Native, false, |_| {})
            .expect("native watch limit is handled");
        let second_registration = watcher
            .add(second_path, WatcherMode::Native, false, |_| {})
            .expect("native watch limit backoff is handled");

        assert!(first_registration.is_none());
        assert!(second_registration.is_none());

        let native_backend = native_backend.lock();
        assert_eq!(native_backend.watch_calls, &[first_path.to_path_buf()]);
    }

    struct NativeCooldownGuard(Option<Instant>);

    impl NativeCooldownGuard {
        fn activate() -> Self {
            let mut state = global_watcher().state.lock();
            let previous = state.cooldown_until;
            state.cooldown_until = Some(Instant::now() + Duration::from_secs(60));
            Self(previous)
        }
    }

    impl Drop for NativeCooldownGuard {
        fn drop(&mut self) {
            global_watcher().state.lock().cooldown_until = self.0;
        }
    }

    #[gpui::test]
    async fn skipped_existing_path_registration_is_retried(executor: BackgroundExecutor) {
        let _test_guard = GLOBAL_WATCHER_TEST_LOCK.lock();
        let _cooldown_guard = NativeCooldownGuard::activate();
        let temporary_directory = tempfile::tempdir().expect("create temporary directory");
        let watched_path: Arc<Path> = temporary_directory.path().into();
        let (event_tx, _event_rx) = async_channel::unbounded();
        let pending_path_events = Arc::new(Mutex::new(Vec::new()));
        let watcher = FsWatcher::new(executor, event_tx, pending_path_events);

        watcher
            .add_existing_path(watched_path.clone())
            .expect("defer watch registration during cooldown");

        assert!(
            watcher
                .pending_registrations
                .lock()
                .contains_key(watched_path.as_ref()),
            "a skipped registration should be retried"
        );
    }

    #[test]
    fn unwatch_accepts_path_already_removed_by_backend() {
        let native_backend = Arc::new(Mutex::new(FakeWatchBackend {
            unwatch_returns_watch_not_found: true,
            ..Default::default()
        }));
        let watcher = test_watcher_with_backends(Some(native_backend), None);

        watcher
            .unwatch(Path::new("/removed"), WatcherMode::Native)
            .expect("an already-removed watch should count as removed");
    }

    #[test]
    fn native_event_reader_does_not_run_subscriber_callbacks() {
        let _test_guard = GLOBAL_WATCHER_TEST_LOCK.lock();
        let temporary_directory = tempfile::tempdir().expect("create temporary directory");
        let watched_path: Arc<Path> = temporary_directory.path().into();
        let (callback_started_tx, callback_started_rx) = mpsc::channel();
        let (release_callback_tx, release_callback_rx) = mpsc::channel();
        let release_callback_rx = Arc::new(Mutex::new(release_callback_rx));
        let registration = global_watcher()
            .add(
                watched_path.clone(),
                WatcherMode::Native,
                case_insensitive_path(&watched_path),
                move |_| {
                    callback_started_tx.send(()).expect("report callback start");
                    release_callback_rx
                        .lock()
                        .recv()
                        .expect("wait for callback release");
                },
            )
            .expect("add native watch")
            .expect("register native watch");

        let event_path = watched_path.join("changed.txt");
        let (reader_returned_tx, reader_returned_rx) = mpsc::channel();
        let reader_thread = std::thread::spawn(move || {
            handle_native_event(Ok(notify::Event::new(EventKind::Modify(
                notify::event::ModifyKind::Any,
            ))
            .add_path(event_path)));
            reader_returned_tx
                .send(())
                .expect("report reader completion");
        });

        callback_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("subscriber callback should start");
        let reader_returned = reader_returned_rx
            .recv_timeout(Duration::from_millis(100))
            .is_ok();
        release_callback_tx
            .send(())
            .expect("release subscriber callback");
        reader_thread.join().expect("join reader thread");
        global_watcher().remove(registration);

        assert!(
            reader_returned,
            "native event reader should return before subscriber callbacks finish"
        );
    }

    #[test]
    fn native_event_dispatches_only_to_covering_registration() {
        let _test_guard = GLOBAL_WATCHER_TEST_LOCK.lock();
        let temporary_directory = tempfile::tempdir().expect("create temporary directory");
        let first_path: Arc<Path> = temporary_directory.path().join("first").into();
        let second_path: Arc<Path> = temporary_directory.path().join("second").into();
        std::fs::create_dir_all(first_path.as_ref()).expect("create first watched directory");
        std::fs::create_dir_all(second_path.as_ref()).expect("create second watched directory");

        let (first_callback_tx, first_callback_rx) = mpsc::channel();
        let first_registration = global_watcher()
            .add(first_path.clone(), WatcherMode::Native, false, move |_| {
                first_callback_tx
                    .send(())
                    .expect("report first callback invocation");
            })
            .expect("add first native watch")
            .expect("register first native watch");
        let (second_callback_tx, second_callback_rx) = mpsc::channel();
        let second_registration = global_watcher()
            .add(second_path, WatcherMode::Native, false, move |_| {
                second_callback_tx
                    .send(())
                    .expect("report second callback invocation");
            })
            .expect("add second native watch")
            .expect("register second native watch");

        handle_native_event(Ok(notify::Event::new(EventKind::Modify(
            notify::event::ModifyKind::Any,
        ))
        .add_path(first_path.join("changed.txt"))));

        first_callback_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("covering callback should run");
        let unrelated_callback_ran = second_callback_rx
            .recv_timeout(Duration::from_millis(100))
            .is_ok();
        global_watcher().remove(first_registration);
        global_watcher().remove(second_registration);

        assert!(
            !unrelated_callback_ran,
            "an event should not wake an unrelated watch registration"
        );
    }

    #[test]
    fn native_event_dispatches_when_reported_casing_differs() {
        let _test_guard = GLOBAL_WATCHER_TEST_LOCK.lock();
        let temporary_directory = tempfile::tempdir().expect("create temporary directory");
        let registered_path = temporary_directory.path().join("CaseRoot");
        std::fs::create_dir_all(&registered_path).expect("create watched directory");
        let differently_cased_path = temporary_directory.path().join("caseroot");
        if !differently_cased_path.exists() {
            return;
        }

        let (callback_tx, callback_rx) = mpsc::channel();
        let registration = global_watcher()
            .add(
                registered_path.into(),
                WatcherMode::Native,
                true,
                move |_| callback_tx.send(()).expect("report callback invocation"),
            )
            .expect("add native watch")
            .expect("register native watch");

        handle_native_event(Ok(notify::Event::new(EventKind::Modify(
            notify::event::ModifyKind::Any,
        ))
        .add_path(differently_cased_path.join("changed.txt"))));

        let callback_ran = callback_rx.recv_timeout(Duration::from_secs(1)).is_ok();
        global_watcher().remove(registration);

        assert!(
            callback_ran,
            "a case-insensitive registration should match differently-cased event paths"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_event_dispatches_when_unicode_normalization_differs() {
        let _test_guard = GLOBAL_WATCHER_TEST_LOCK.lock();
        let temporary_directory = tempfile::tempdir().expect("create temporary directory");
        let registered_path = temporary_directory.path().join("Caf\u{e9}");
        std::fs::create_dir_all(&registered_path).expect("create watched directory");
        let differently_normalized_path = temporary_directory.path().join("Cafe\u{301}");
        if !differently_normalized_path.exists() {
            return;
        }

        let (callback_tx, callback_rx) = mpsc::channel();
        let registration = global_watcher()
            .add(
                registered_path.into(),
                WatcherMode::Native,
                true,
                move |_| callback_tx.send(()).expect("report callback invocation"),
            )
            .expect("add native watch")
            .expect("register native watch");

        handle_native_event(Ok(notify::Event::new(EventKind::Modify(
            notify::event::ModifyKind::Any,
        ))
        .add_path(differently_normalized_path.join("changed.txt"))));

        let callback_ran = callback_rx.recv_timeout(Duration::from_secs(1)).is_ok();
        global_watcher().remove(registration);

        assert!(
            callback_ran,
            "a macOS registration should match normalization-equivalent event paths"
        );
    }

    #[test]
    fn queued_rescans_are_coalesced_without_dropping_normal_events() {
        let _test_guard = GLOBAL_WATCHER_TEST_LOCK.lock();
        let temporary_directory = tempfile::tempdir().expect("create temporary directory");
        let watched_path: Arc<Path> = temporary_directory.path().into();
        let (callback_tx, callback_rx) = mpsc::channel();
        let (first_callback_started_tx, first_callback_started_rx) = mpsc::channel();
        let (release_first_callback_tx, release_first_callback_rx) = mpsc::channel();
        let release_first_callback_rx = Arc::new(Mutex::new(release_first_callback_rx));
        let first_callback = Arc::new(AtomicBool::new(true));
        let registration = global_watcher()
            .add(
                watched_path.clone(),
                WatcherMode::Native,
                case_insensitive_path(&watched_path),
                {
                    move |event| {
                        callback_tx
                            .send(event.need_rescan())
                            .expect("record dispatched event");
                        if first_callback.swap(false, Ordering::SeqCst) {
                            first_callback_started_tx
                                .send(())
                                .expect("report first callback start");
                            release_first_callback_rx
                                .lock()
                                .recv()
                                .expect("wait for first callback release");
                        }
                    }
                },
            )
            .expect("add native watch")
            .expect("register native watch");

        handle_native_event(Ok(notify::Event::new(EventKind::Modify(
            notify::event::ModifyKind::Any,
        ))
        .add_path(watched_path.join("first.txt"))));
        first_callback_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first callback should start");

        let rescan = || notify::Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan);
        handle_native_event(Ok(rescan()));
        handle_native_event(Ok(rescan()));
        handle_native_event(Ok(notify::Event::new(EventKind::Modify(
            notify::event::ModifyKind::Any,
        ))
        .add_path(watched_path.join("last.txt"))));
        release_first_callback_tx
            .send(())
            .expect("release first callback");

        let mut dispatched_rescans = Vec::new();
        while let Ok(need_rescan) = callback_rx.recv_timeout(Duration::from_millis(200)) {
            dispatched_rescans.push(need_rescan);
        }
        global_watcher().remove(registration);

        assert_eq!(dispatched_rescans, vec![false, true, false]);
    }

    #[test]
    fn test_coalesce_pending_rescans() {
        let test_cases = [
            TestCase {
                name: "coalesces descendant rescans under pending ancestor",
                pending_paths: vec![rescan("/root")],
                path_events: vec![rescan("/root/child"), rescan("/root/child/grandchild")],
                expected_pending_paths: vec![rescan("/root")],
                expected_path_events: vec![],
            },
            TestCase {
                name: "new ancestor rescan replaces pending descendant rescans",
                pending_paths: vec![
                    changed("/other"),
                    rescan("/root/child"),
                    rescan("/root/child/grandchild"),
                ],
                path_events: vec![rescan("/root")],
                expected_pending_paths: vec![changed("/other")],
                expected_path_events: vec![rescan("/root")],
            },
            TestCase {
                name: "same path rescan replaces pending non-rescan event",
                pending_paths: vec![changed("/root")],
                path_events: vec![rescan("/root")],
                expected_pending_paths: vec![],
                expected_path_events: vec![rescan("/root")],
            },
            TestCase {
                name: "unrelated rescans are preserved",
                pending_paths: vec![rescan("/root-a")],
                path_events: vec![rescan("/root-b")],
                expected_pending_paths: vec![rescan("/root-a")],
                expected_path_events: vec![rescan("/root-b")],
            },
            TestCase {
                name: "batch ancestor rescan replaces descendant rescan",
                pending_paths: vec![],
                path_events: vec![rescan("/root/child"), rescan("/root")],
                expected_pending_paths: vec![],
                expected_path_events: vec![rescan("/root")],
            },
        ];

        for test_case in test_cases {
            let mut pending_paths = test_case.pending_paths;
            let mut path_events = test_case.path_events;

            coalesce_pending_rescans(&mut pending_paths, &mut path_events);

            assert_eq!(
                pending_paths, test_case.expected_pending_paths,
                "pending_paths mismatch for case: {}",
                test_case.name
            );
            assert_eq!(
                path_events, test_case.expected_path_events,
                "path_events mismatch for case: {}",
                test_case.name
            );
        }
    }
}

pub fn global<T>(f: impl FnOnce(&GlobalWatcher) -> T) -> anyhow::Result<T> {
    let global_watcher = global_watcher();
    global_watcher.ensure_native_watcher()?;
    Ok(f(global_watcher))
}
