use chrono::{Datelike, Local};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

const SCHEMA_VERSION: u32 = 1;
const MAX_DAILY_RECORDS: usize = 32;
const MAX_MEMORY_BYTES: u64 = 1024 * 1024;

#[derive(Debug)]
pub struct MemoryLock {
    #[cfg(unix)]
    file: File,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyStats {
    pub day: u64,
    pub repo: String,
    pub build_failures: u32,
    pub build_successes: u32,
    pub git_pushes: u32,
    pub first_failure_at_ms: Option<u64>,
    pub first_success_at_ms: Option<u64>,
    pub debug_duration_ms: Option<u64>,
}

impl DailyStats {
    fn new(day: u64, repo: &str) -> Self {
        Self {
            day,
            repo: repo.to_owned(),
            build_failures: 0,
            build_successes: 0,
            git_pushes: 0,
            first_failure_at_ms: None,
            first_success_at_ms: None,
            debug_duration_ms: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    version: u32,
    pub sessions: u64,
    days: Vec<DailyStats>,
}

impl Default for Memory {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            sessions: 0,
            days: Vec::new(),
        }
    }
}

impl Memory {
    pub fn load(path: &Path) -> io::Result<Self> {
        let mut bytes = Vec::new();
        match File::open(path) {
            Ok(file) => {
                if !file.metadata()?.is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "organism memory must be a regular file",
                    ));
                }
                let mut limited = file.take(MAX_MEMORY_BYTES + 1);
                limited.read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_MEMORY_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "organism memory exceeds the 1 MiB safety limit",
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        }

        let mut memory: Self = serde_json::from_slice(&bytes).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid organism memory: {error}"),
            )
        })?;
        if memory.version != SCHEMA_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unsupported organism memory version {} (expected {SCHEMA_VERSION})",
                    memory.version
                ),
            ));
        }
        let mut seen = HashSet::new();
        for stats in &memory.days {
            if stats.repo.len() > 4096 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "organism memory contains an oversized repository identifier",
                ));
            }
            if !seen.insert((stats.day, stats.repo.as_str())) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "organism memory contains duplicate day/repository records",
                ));
            }
        }
        memory.prune();
        Ok(memory)
    }

    pub fn save_atomic(&self, path: &Path) -> io::Result<()> {
        let parent = ensure_parent(path)?;

        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("memory.json");
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let mut temporary_file = None;
        for attempt in 0..32 {
            let candidate = parent.join(format!(
                ".{file_name}.tmp-{}-{nonce}-{attempt}",
                std::process::id()
            ));
            let mut options = OpenOptions::new();
            // O_EXCL/create_new refuses pre-existing symlinks instead of
            // following a predictable temporary path and truncating its target.
            options.create_new(true).write(true);
            #[cfg(unix)]
            options.mode(0o600);
            match options.open(&candidate) {
                Ok(file) => {
                    temporary_file = Some((candidate, file));
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        let (temporary, mut file) = temporary_file.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not reserve a unique organism memory temporary file",
            )
        })?;

        let result = (|| {
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, path)?;
            // Persist the directory entry as well as the file contents.
            File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub fn acquire_lock(path: &Path) -> io::Result<MemoryLock> {
        let parent = ensure_parent(path)?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("memory.json");
        let lock_path = parent.join(format!(".{file_name}.lock"));
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(lock_path)?;

        #[cfg(unix)]
        {
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result != 0 {
                let error = io::Error::last_os_error();
                return Err(io::Error::new(
                    error.kind(),
                    format!("organism memory is already in use: {error}"),
                ));
            }
        }

        Ok(MemoryLock {
            #[cfg(unix)]
            file,
        })
    }

    pub fn begin_session(&mut self) {
        self.sessions = self.sessions.saturating_add(1);
    }

    pub fn record_failure(&mut self, day: u64, repo: &str, at_ms: u64) {
        let stats = self.stats_mut(day, repo);
        stats.build_failures = stats.build_failures.saturating_add(1);
        stats.first_failure_at_ms.get_or_insert(at_ms);
    }

    pub fn record_success(&mut self, day: u64, repo: &str, at_ms: u64) {
        let stats = self.stats_mut(day, repo);
        stats.build_successes = stats.build_successes.saturating_add(1);
        stats.first_success_at_ms.get_or_insert(at_ms);
        if stats.debug_duration_ms.is_none() {
            stats.debug_duration_ms = stats
                .first_failure_at_ms
                .and_then(|started| at_ms.checked_sub(started));
        }
    }

    pub fn record_push(&mut self, day: u64, repo: &str) {
        let stats = self.stats_mut(day, repo);
        stats.git_pushes = stats.git_pushes.saturating_add(1);
    }

    pub fn today(&self, day: u64, repo: &str) -> Option<&DailyStats> {
        self.days
            .iter()
            .find(|stats| stats.day == day && stats.repo == repo)
    }

    pub fn previous(&self, day: u64, repo: &str) -> Option<&DailyStats> {
        let previous_day = day.checked_sub(1)?;
        self.days
            .iter()
            .find(|stats| stats.day == previous_day && stats.repo == repo)
    }

    pub fn faster_than_previous(&self, day: u64, repo: &str) -> bool {
        let Some(today) = self.today(day, repo) else {
            return false;
        };
        let Some(previous) = self.previous(day, repo) else {
            return false;
        };

        if let (Some(today_duration), Some(previous_duration)) =
            (today.debug_duration_ms, previous.debug_duration_ms)
        {
            return today_duration < previous_duration;
        }

        today.build_successes > 0
            && previous.build_successes > 0
            && today.build_failures < previous.build_failures
    }

    pub fn demo(day: u64, repo: &str) -> Self {
        let previous_day = day.saturating_sub(1);
        Self {
            version: SCHEMA_VERSION,
            sessions: 7,
            days: vec![DailyStats {
                day: previous_day,
                repo: repo.to_owned(),
                build_failures: 5,
                build_successes: 1,
                git_pushes: 1,
                first_failure_at_ms: Some(1_000),
                first_success_at_ms: Some(301_000),
                debug_duration_ms: Some(300_000),
            }],
        }
    }

    pub fn default_path() -> Option<PathBuf> {
        if let Some(state_home) = std::env::var_os("XDG_STATE_HOME") {
            let state_home = PathBuf::from(state_home);
            if !state_home.as_os_str().is_empty() && state_home.is_absolute() {
                return Some(state_home.join("forge/ascii-organism.json"));
            }
        }
        std::env::var_os("HOME").and_then(|home| {
            let home = PathBuf::from(home);
            (!home.as_os_str().is_empty() && home.is_absolute())
                .then(|| home.join(".local/state/forge/ascii-organism.json"))
        })
    }

    pub fn local_day() -> u64 {
        Local::now()
            .date_naive()
            .num_days_from_ce()
            .try_into()
            .unwrap_or_default()
    }

    pub fn unix_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    fn stats_mut(&mut self, day: u64, repo: &str) -> &mut DailyStats {
        if let Some(index) = self
            .days
            .iter()
            .position(|stats| stats.day == day && stats.repo == repo)
        {
            return &mut self.days[index];
        }

        self.days.push(DailyStats::new(day, repo));
        self.days.sort_by_key(|stats| stats.day);
        if self.days.len() > MAX_DAILY_RECORDS {
            let new_index = self
                .days
                .iter()
                .position(|stats| stats.day == day && stats.repo == repo)
                .expect("new daily stats must exist before bounded eviction");
            let remove_index = if new_index == 0 { 1 } else { 0 };
            self.days.remove(remove_index);
        }
        let index = self
            .days
            .iter()
            .position(|stats| stats.day == day && stats.repo == repo)
            .expect("new daily stats must survive pruning");
        &mut self.days[index]
    }

    fn prune(&mut self) {
        self.days.sort_by_key(|stats| stats.day);
        if self.days.len() > MAX_DAILY_RECORDS {
            self.days.drain(..self.days.len() - MAX_DAILY_RECORDS);
        }
    }
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn ensure_parent(path: &Path) -> io::Result<&Path> {
    let parent = parent_dir(path);
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(parent)?;
    Ok(parent)
}

#[cfg(unix)]
impl Drop for MemoryLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_directory(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "forge-ascii-organism-{label}-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn compares_debug_duration_before_failure_count_fallback() {
        let mut memory = Memory::demo(20, "forge");
        memory.record_failure(20, "forge", 10_000);
        memory.record_failure(20, "forge", 20_000);
        memory.record_failure(20, "forge", 30_000);
        memory.record_success(20, "forge", 90_000);

        assert!(memory.faster_than_previous(20, "forge"));
    }

    #[test]
    fn requires_a_previous_day_for_comparison() {
        let mut memory = Memory::default();
        memory.record_success(20, "forge", 90_000);

        assert!(!memory.faster_than_previous(20, "forge"));
    }

    #[test]
    fn a_gap_is_not_described_as_yesterday() {
        let mut memory = Memory::default();
        memory.record_failure(17, "forge", 1_000);
        memory.record_success(17, "forge", 2_000);
        memory.record_success(20, "forge", 3_000);

        assert!(!memory.faster_than_previous(20, "forge"));
    }

    #[test]
    fn json_round_trip_preserves_daily_stats() {
        let mut memory = Memory::default();
        memory.begin_session();
        memory.record_failure(20, "forge", 10_000);
        memory.record_success(20, "forge", 40_000);
        let json = serde_json::to_vec(&memory).expect("serialize memory");
        let restored: Memory = serde_json::from_slice(&json).expect("restore memory");

        assert_eq!(restored.sessions, 1);
        assert_eq!(
            restored.today(20, "forge").unwrap().debug_duration_ms,
            Some(30_000)
        );
    }

    #[test]
    fn backdated_insert_survives_bounded_eviction() {
        let mut memory = Memory::default();
        for day in 100..132 {
            memory.record_failure(day, "forge", day * 1_000);
        }
        memory.record_failure(1, "forge", 1_000);

        assert!(memory.today(1, "forge").is_some());
        assert_eq!(memory.days.len(), MAX_DAILY_RECORDS);
    }

    #[test]
    fn atomic_file_round_trip_is_owner_only_and_leaves_no_temporary_file() {
        let directory = test_directory("round-trip");
        let path = directory.join("memory.json");
        let mut memory = Memory::default();
        memory.record_failure(20, "forge", 1_000);
        memory.save_atomic(&path).expect("save memory");
        let restored = Memory::load(&path).expect("load memory");

        assert_eq!(restored.today(20, "forge").unwrap().build_failures, 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn lock_refuses_a_second_persistent_writer() {
        let directory = test_directory("lock");
        let path = directory.join("memory.json");
        let first = Memory::acquire_lock(&path).expect("first lock");
        let second = Memory::acquire_lock(&path);
        assert!(second.is_err());
        drop(first);
        Memory::acquire_lock(&path).expect("lock after release");
        fs::remove_dir_all(directory).expect("remove test directory");
    }
}
