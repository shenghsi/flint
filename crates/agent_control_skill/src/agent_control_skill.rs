use std::cell::RefCell;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

pub const BUNDLED_SKILL: &str = include_str!("../skills/flintctl/SKILL.md");
pub const BUNDLED_SKILL_VERSION: u32 = 3;

static RELEASE_CHANNEL_NAME: LazyLock<String> = LazyLock::new(|| {
    if cfg!(debug_assertions) {
        std::env::var("ZED_RELEASE_CHANNEL").unwrap_or_else(|_| {
            include_str!("../../flint/RELEASE_CHANNEL")
                .trim()
                .to_string()
        })
    } else {
        include_str!("../../flint/RELEASE_CHANNEL")
            .trim()
            .to_string()
    }
});

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Codex,
    Claude,
}

impl AgentKind {
    pub const ALL: [Self; 1] = [Self::Codex];

    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex, Pi, OpenCode, and Claude Code",
            Self::Claude => "Claude Code (legacy installation)",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "codex" | "claude" | "pi" | "opencode" => Ok(Self::Codex),
            _ => bail!("unsupported agent {value:?}; expected codex, claude, pi, or opencode"),
        }
    }
}

#[derive(Debug)]
pub struct SkillEnvironment {
    home_directory: PathBuf,
    data_directory: PathBuf,
    bundled_skill_override: RefCell<Option<String>>,
}

impl SkillEnvironment {
    pub fn current() -> Self {
        Self::new(paths::home_dir().clone(), paths::data_dir().clone())
    }

    pub fn new(home_directory: PathBuf, data_directory: PathBuf) -> Self {
        Self {
            home_directory,
            data_directory,
            bundled_skill_override: RefCell::new(None),
        }
    }

    pub fn skill_path(&self, agent: AgentKind) -> PathBuf {
        match agent {
            AgentKind::Codex => self.home_directory.join(".agents/skills/flintctl/SKILL.md"),
            AgentKind::Claude => self.home_directory.join(".claude/skills/flintctl/SKILL.md"),
        }
    }

    pub fn codex_skill_path(&self) -> PathBuf {
        self.skill_path(AgentKind::Codex)
    }

    pub fn claude_skill_path(&self) -> PathBuf {
        self.skill_path(AgentKind::Claude)
    }

    fn record_path(&self, agent: AgentKind) -> PathBuf {
        self.data_directory
            .join("flintctl-skills")
            .join(format!("{}.json", agent.id()))
    }

    fn bundled_skill(&self) -> String {
        self.bundled_skill_override
            .borrow()
            .clone()
            .unwrap_or_else(|| BUNDLED_SKILL.to_string())
    }

    #[cfg(test)]
    fn replace_bundled_skill_for_test(&self, skill: String) {
        self.bundled_skill_override.replace(Some(skill));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkillState {
    NotInstalled,
    Unowned,
    InstalledCurrent,
    InstalledOutdated,
    Modified,
    Missing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SynchronizeOutcome {
    pub agent: AgentKind,
    pub state: SkillState,
}

#[derive(Deserialize, Serialize)]
struct OwnershipRecord {
    format_version: u32,
    #[serde(default)]
    bundled_skill_version: u32,
    #[serde(default)]
    release_channel: String,
    agent: AgentKind,
    skill_path: PathBuf,
    installed_digest: String,
}

pub fn install(agent: AgentKind, environment: &SkillEnvironment, replace: bool) -> Result<()> {
    let skill_path = environment.skill_path(agent);
    let record_path = environment.record_path(agent);
    if skill_path.exists() && !record_path.exists() {
        bail!(
            "an unowned skill already exists for {} at {}; Flint will not replace it",
            agent.label(),
            skill_path.display()
        );
    }
    if (skill_path.exists() || record_path.exists()) && !replace {
        bail!(
            "a skill or ownership record already exists for {}; use replace only after review",
            agent.label()
        );
    }
    let bundled_skill = environment.bundled_skill();
    atomic_write(&skill_path, bundled_skill.as_bytes())?;
    let record = OwnershipRecord {
        format_version: 1,
        bundled_skill_version: BUNDLED_SKILL_VERSION,
        release_channel: RELEASE_CHANNEL_NAME.clone(),
        agent,
        skill_path,
        installed_digest: digest(&bundled_skill),
    };
    let record_json = serde_json::to_vec_pretty(&record).context("serialize skill ownership")?;
    atomic_write(&record_path, &record_json)
}

pub fn status(agent: AgentKind, environment: &SkillEnvironment) -> Result<SkillState> {
    let Some(record) = read_record(agent, environment)? else {
        return if environment.skill_path(agent).exists() {
            Ok(SkillState::Unowned)
        } else {
            Ok(SkillState::NotInstalled)
        };
    };
    let installed = match fs::read_to_string(&record.skill_path) {
        Ok(installed) => installed,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SkillState::Missing);
        }
        Err(error) => return Err(error).context("read installed Flint control skill"),
    };
    if digest(&installed) != record.installed_digest {
        return Ok(SkillState::Modified);
    }
    if installed == environment.bundled_skill() {
        Ok(SkillState::InstalledCurrent)
    } else {
        Ok(SkillState::InstalledOutdated)
    }
}

pub fn synchronize(environment: &SkillEnvironment) -> Result<Vec<SynchronizeOutcome>> {
    let mut outcomes = Vec::new();
    for agent in AgentKind::ALL {
        let state = status(agent, environment)?;
        match state {
            SkillState::NotInstalled | SkillState::Unowned | SkillState::InstalledCurrent => {
                continue;
            }
            SkillState::InstalledOutdated => {
                install(agent, environment, true)?;
                outcomes.push(SynchronizeOutcome {
                    agent,
                    state: SkillState::InstalledCurrent,
                });
            }
            SkillState::Modified | SkillState::Missing => {
                outcomes.push(SynchronizeOutcome { agent, state });
            }
        }
    }
    Ok(outcomes)
}

pub fn uninstall(agent: AgentKind, environment: &SkillEnvironment, force: bool) -> Result<()> {
    let record = read_record(agent, environment)?
        .ok_or_else(|| anyhow!("Flint has no ownership record for {}", agent.label()))?;
    if status(agent, environment)? == SkillState::Modified && !force {
        bail!(
            "the installed {} skill was modified; review it before removal",
            agent.label()
        );
    }
    match fs::remove_file(&record.skill_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("remove installed Flint control skill"),
    }
    fs::remove_file(environment.record_path(agent)).context("remove skill ownership record")
}

fn read_record(
    agent: AgentKind,
    environment: &SkillEnvironment,
) -> Result<Option<OwnershipRecord>> {
    let contents = match fs::read(environment.record_path(agent)) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("read skill ownership record"),
    };
    let record: OwnershipRecord =
        serde_json::from_slice(&contents).context("parse skill ownership record")?;
    if record.format_version != 1 || record.agent != agent {
        bail!("invalid Flint skill ownership record for {}", agent.label());
    }
    if record.skill_path != environment.skill_path(agent) {
        bail!("Flint skill ownership record has an unexpected destination");
    }
    Ok(Some(record))
}

fn digest(contents: &str) -> String {
    format!("{:x}", Sha256::digest(contents.as_bytes()))
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("destination has no parent directory"))?;
    fs::create_dir_all(parent).context("create skill destination directory")?;
    let mut temporary = tempfile_path(parent, path);
    for suffix in 0..100_u8 {
        temporary.set_extension(format!("tmp-{}-{suffix}", std::process::id()));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(mut file) => {
                let result = (|| {
                    file.write_all(contents)
                        .context("write temporary skill file")?;
                    file.sync_all().context("sync temporary skill file")?;
                    drop(file);
                    replace_file(path, &temporary).context("replace installed skill")
                })();
                match result {
                    Ok(()) => return Ok(()),
                    Err(error) => {
                        let _ = fs::remove_file(&temporary);
                        return Err(error);
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("create temporary skill file"),
        }
    }
    bail!("could not allocate a temporary skill file")
}

#[cfg(not(windows))]
fn replace_file(destination: &Path, source: &Path) -> Result<()> {
    fs::rename(source, destination).context("rename replacement file")
}

#[cfg(windows)]
fn replace_file(destination: &Path, source: &Path) -> Result<()> {
    use std::time::Duration;

    use windows::Win32::{
        Foundation::ERROR_SHARING_VIOLATION,
        Storage::FileSystem::{REPLACE_FILE_FLAGS, ReplaceFileW},
    };
    use windows::core::HSTRING;

    let created_destination = if !destination.exists() {
        match fs::File::create_new(destination) {
            Ok(file) => {
                drop(file);
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(error) => return Err(error).context("create replacement destination"),
        }
    } else {
        false
    };
    for attempt in 0..10 {
        let result = unsafe {
            ReplaceFileW(
                &HSTRING::from(destination.to_string_lossy().into_owned()),
                &HSTRING::from(source.to_string_lossy().into_owned()),
                None,
                REPLACE_FILE_FLAGS::default(),
                None,
                None,
            )
        };
        match result {
            Ok(()) => return Ok(()),
            Err(error)
                if error.code().0
                    == windows::core::HRESULT::from_win32(ERROR_SHARING_VIOLATION.0).0
                    && attempt < 9 =>
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                if created_destination {
                    let _ = fs::remove_file(destination);
                }
                return Err(error).context("replace destination file");
            }
        }
    }
    unreachable!("the retry loop either returns or retries")
}

fn tempfile_path(parent: &Path, path: &Path) -> PathBuf {
    parent.join(
        path.file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("SKILL.md")),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::{AgentKind, SkillEnvironment, SkillState, install, status, synchronize, uninstall};

    fn environment(temporary_directory: &TempDir) -> SkillEnvironment {
        SkillEnvironment::new(
            temporary_directory.path().join("home"),
            temporary_directory.path().join("data"),
        )
    }

    #[test]
    fn shared_agent_skill_target_names_all_supported_agents() {
        assert_eq!(
            AgentKind::Codex.label(),
            "Codex, Pi, OpenCode, and Claude Code"
        );
    }

    #[test]
    fn all_agents_use_one_shared_installation_target() {
        assert_eq!(AgentKind::ALL, [AgentKind::Codex]);
        for agent_name in ["codex", "claude", "pi", "opencode"] {
            assert_eq!(
                AgentKind::parse(agent_name).expect("parse supported agent"),
                AgentKind::Codex
            );
        }
    }

    #[test]
    fn synchronization_does_not_install_without_consent() {
        let temporary_directory = TempDir::new().expect("create temporary directory");
        let environment = environment(&temporary_directory);

        assert_eq!(
            synchronize(&environment).expect("synchronize skills"),
            Vec::new()
        );
        assert_eq!(
            status(AgentKind::Codex, &environment).expect("read status"),
            SkillState::NotInstalled
        );
        assert!(!environment.codex_skill_path().exists());
    }

    #[test]
    fn synchronization_preserves_an_unowned_skill() {
        let temporary_directory = TempDir::new().expect("create temporary directory");
        let environment = environment(&temporary_directory);
        let installed_path = environment.codex_skill_path();
        fs::create_dir_all(installed_path.parent().expect("skill parent"))
            .expect("create skill parent");
        fs::write(&installed_path, "another owner\n").expect("write unowned skill");

        assert_eq!(
            status(AgentKind::Codex, &environment).expect("read status"),
            SkillState::Unowned
        );
        assert!(
            synchronize(&environment)
                .expect("synchronize skills")
                .is_empty()
        );
        assert_eq!(
            fs::read_to_string(installed_path).expect("read unowned skill"),
            "another owner\n"
        );
    }

    #[test]
    fn synchronization_updates_an_unchanged_owned_skill() {
        let temporary_directory = TempDir::new().expect("create temporary directory");
        let environment = environment(&temporary_directory);
        install(AgentKind::Codex, &environment, false).expect("install skill");
        let installed_path = environment.codex_skill_path();
        let old_skill = fs::read_to_string(&installed_path).expect("read installed skill");
        environment.replace_bundled_skill_for_test(old_skill.replace("version: 3", "version: 4"));

        assert_eq!(
            synchronize(&environment).expect("synchronize skills").len(),
            1
        );
        assert!(
            fs::read_to_string(installed_path)
                .expect("read updated skill")
                .contains("version: 4")
        );
        assert_eq!(
            status(AgentKind::Codex, &environment).expect("read status"),
            SkillState::InstalledCurrent
        );
    }

    #[test]
    fn synchronization_preserves_a_modified_owned_skill() {
        let temporary_directory = TempDir::new().expect("create temporary directory");
        let environment = environment(&temporary_directory);
        install(AgentKind::Codex, &environment, false).expect("install skill");
        let installed_path = environment.codex_skill_path();
        fs::write(&installed_path, "user changes\n").expect("modify skill");

        let outcomes = synchronize(&environment).expect("synchronize skills");

        assert_eq!(outcomes.len(), 1);
        assert_eq!(
            status(AgentKind::Codex, &environment).expect("read status"),
            SkillState::Modified
        );
        assert_eq!(
            fs::read_to_string(installed_path).expect("read modified skill"),
            "user changes\n"
        );
    }

    #[test]
    fn uninstall_refuses_to_remove_a_modified_skill() {
        let temporary_directory = TempDir::new().expect("create temporary directory");
        let environment = environment(&temporary_directory);
        install(AgentKind::Codex, &environment, false).expect("install skill");
        let installed_path = environment.codex_skill_path();
        fs::write(&installed_path, "user changes\n").expect("modify skill");

        assert!(uninstall(AgentKind::Codex, &environment, false).is_err());
        assert!(installed_path.exists());
    }

    #[test]
    fn replace_refuses_to_overwrite_an_unowned_skill() {
        let temporary_directory = TempDir::new().expect("create temporary directory");
        let environment = environment(&temporary_directory);
        let installed_path = environment.codex_skill_path();
        fs::create_dir_all(installed_path.parent().expect("skill parent"))
            .expect("create skill parent");
        fs::write(&installed_path, "another owner\n").expect("write unowned skill");

        assert!(install(AgentKind::Codex, &environment, true).is_err());
        assert_eq!(
            fs::read_to_string(installed_path).expect("read unowned skill"),
            "another owner\n"
        );
    }

    #[test]
    fn ownership_record_tracks_the_bundled_version_and_release_channel() {
        let temporary_directory = TempDir::new().expect("create temporary directory");
        let environment = environment(&temporary_directory);
        install(AgentKind::Codex, &environment, false).expect("install skill");

        let record: serde_json::Value = serde_json::from_slice(
            &fs::read(environment.record_path(AgentKind::Codex)).expect("read ownership record"),
        )
        .expect("parse ownership record");

        assert_eq!(record["bundled_skill_version"], 3);
        assert!(record["release_channel"].as_str().is_some());
    }

    #[test]
    fn installed_skill_probes_the_endpoint_before_flintctl() {
        let temporary_directory = TempDir::new().expect("create temporary directory");
        let environment = environment(&temporary_directory);
        install(AgentKind::Codex, &environment, false).expect("install skill");
        let installed = fs::read_to_string(environment.codex_skill_path())
            .expect("read installed control skill");

        let unix_endpoint = installed
            .find("matching marker or socket")
            .expect("Unix endpoint gate");
        let windows_endpoint = installed
            .find("marker or named pipe is absent")
            .expect("Windows endpoint gate");
        let probe = installed
            .find("terminal current --json")
            .expect("authoritative caller probe");

        assert!(unix_endpoint < probe);
        assert!(windows_endpoint < probe);
        assert!(!installed.contains("FLINT_AGENT_THREAD="));
    }

    #[test]
    fn installed_skill_separates_terminal_and_thread_probe_results() {
        let temporary_directory = TempDir::new().expect("create temporary directory");
        let environment = environment(&temporary_directory);
        install(AgentKind::Codex, &environment, false).expect("install skill");
        let installed = fs::read_to_string(environment.codex_skill_path())
            .expect("read installed control skill");

        for required_decision in [
            "is_agent_thread: true",
            "is_agent_thread: false",
            "connection fails",
            "protocol is incompatible",
            "caller is not recognized",
            "TERM_PROGRAM",
            "ZED_TERM",
        ] {
            assert!(
                installed.contains(required_decision),
                "installed skill must cover {required_decision:?}"
            );
        }
    }
}
