//! Bounded, repo-scoped long-term memory for the native ASCII organism.
//!
//! Only structured counters, a short bounded transition-ordering window, and
//! a quantized life-state snapshot are stored. Transition ids are opaque and
//! carry no PID; command text and output never enter this file. Every durable
//! update is a cross-process transaction performed by Forge's persistence
//! worker: lock, bounded reread, apply one delta, and private atomic replacement.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::organism::{CommandKind, LifeState};

const SCHEMA_VERSION: u32 = 1;
const MAX_MEMORY_BYTES: u64 = 512 * 1024;
const MAX_DAILY_RECORDS: usize = 64;
const MAX_OBSERVATIONS: usize = 256;
const MAX_OBSERVATIONS_PER_RECORD: usize = 64;
/// Individual build durations are capped before aggregation so a clock jump
/// or pathological command can never distort the per-repo baseline.
const MAX_TRACKED_BUILD_MS: u64 = 21_600_000;
const MAX_RECENT_EVENT_IDS: usize = 512;
const MAX_EVENT_ID_BYTES: usize = 96;
const MAX_PENDING_MEMORY_EVENTS: usize = 256;
const MAX_ACKNOWLEDGED_MEMORY_EVENTS: usize = MAX_PENDING_MEMORY_EVENTS * 4;
const MAX_REPO_BYTES: usize = 2 * 1024;
const MAX_GIT_POINTER_BYTES: u64 = 16 * 1024;
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const LOCK_POLL: Duration = Duration::from_millis(20);
const LIFE_SCALE: f32 = 1_000.0;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LifeSnapshot {
    energy: u16,
    mood: u16,
    curiosity: u16,
    boredom: u16,
    stress: u16,
    social_need: u16,
    attachment: u16,
    confidence: u16,
}

impl Default for LifeSnapshot {
    fn default() -> Self {
        Self::from_state(LifeState::default())
    }
}

impl LifeSnapshot {
    fn from_state(state: LifeState) -> Self {
        Self {
            energy: quantize(state.energy),
            mood: quantize(state.mood),
            curiosity: quantize(state.curiosity),
            boredom: quantize(state.boredom),
            stress: quantize(state.stress),
            social_need: quantize(state.social_need),
            attachment: quantize(state.attachment),
            confidence: quantize(state.confidence),
        }
    }

    fn state(self) -> LifeState {
        LifeState {
            energy: dequantize(self.energy),
            mood: dequantize(self.mood),
            curiosity: dequantize(self.curiosity),
            boredom: dequantize(self.boredom),
            stress: dequantize(self.stress),
            social_need: dequantize(self.social_need),
            attachment: dequantize(self.attachment),
            confidence: dequantize(self.confidence),
        }
    }

    fn valid(self) -> bool {
        [
            self.energy,
            self.mood,
            self.curiosity,
            self.boredom,
            self.stress,
            self.social_need,
            self.attachment,
            self.confidence,
        ]
        .into_iter()
        .all(|value| value <= LIFE_SCALE as u16)
    }
}

fn quantize(value: f32) -> u16 {
    let value = if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.5
    };
    (value * LIFE_SCALE).round() as u16
}

fn dequantize(value: u16) -> f32 {
    f32::from(value.min(LIFE_SCALE as u16)) / LIFE_SCALE
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DailyStats {
    day: i64,
    repo: String,
    build_failures: u32,
    build_successes: u32,
    git_pushes: u32,
    open_failures: u32,
    open_failure_at_ms: Option<u64>,
    last_recovery_duration_ms: Option<u64>,
    recovered_pending_push: bool,
    /// Snapshot before the first retained observation. Version 1 was never
    /// shipped without these fields, but defaults let development fixtures and
    /// the standalone seed format migrate without weakening strict decoding.
    #[serde(default)]
    baseline: Option<StatsBaseline>,
    #[serde(default)]
    observations: Vec<Observation>,
    /// Saturating aggregates of successful build/test wall times — scalars
    /// only, never command text. Accumulated directly by `apply_event` and
    /// deliberately outside the observation replay, mirroring how late
    /// compacted events only bump monotonic counters.
    #[serde(default)]
    build_duration_sum_ms: u64,
    #[serde(default)]
    build_duration_count: u32,
}

impl DailyStats {
    fn new(day: i64, repo: String) -> Self {
        Self {
            day,
            repo,
            build_failures: 0,
            build_successes: 0,
            git_pushes: 0,
            open_failures: 0,
            open_failure_at_ms: None,
            last_recovery_duration_ms: None,
            recovered_pending_push: false,
            baseline: None,
            observations: Vec::new(),
            build_duration_sum_ms: 0,
            build_duration_count: 0,
        }
    }

    fn baseline(&self) -> StatsBaseline {
        StatsBaseline {
            build_failures: self.build_failures,
            build_successes: self.build_successes,
            git_pushes: self.git_pushes,
            open_failures: self.open_failures,
            open_failure_at_ms: self.open_failure_at_ms,
            last_recovery_duration_ms: self.last_recovery_duration_ms,
            recovered_pending_push: self.recovered_pending_push,
            compacted_through: self
                .baseline
                .as_ref()
                .and_then(|baseline| baseline.compacted_through.clone()),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StatsBaseline {
    build_failures: u32,
    build_successes: u32,
    git_pushes: u32,
    open_failures: u32,
    open_failure_at_ms: Option<u64>,
    last_recovery_duration_ms: Option<u64>,
    recovered_pending_push: bool,
    /// Events at or before this total-order cursor were folded into the
    /// aggregate prefix. A pathologically late writer is ignored instead of
    /// being replayed on the wrong side of a compacted success or push.
    #[serde(default)]
    compacted_through: Option<ObservationCursor>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct ObservationCursor {
    at_ms: u64,
    id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum ObservationKind {
    BuildFailure,
    BuildSuccess,
    GitPush,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    id: String,
    at_ms: u64,
    kind: ObservationKind,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DiskMemory {
    version: u32,
    life: LifeSnapshot,
    life_updated_at_ms: u64,
    #[serde(default)]
    life_updated_event_id: String,
    /// Bounded durable idempotence tokens. They contain no command data or PID
    /// and outlive observation compaction long enough to cover every
    /// in-process pending/retry window.
    #[serde(default)]
    recent_event_ids: Vec<String>,
    days: Vec<DailyStats>,
}

impl Default for DiskMemory {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            life: LifeSnapshot::default(),
            life_updated_at_ms: 0,
            life_updated_event_id: String::new(),
            recent_event_ids: Vec::new(),
            days: Vec::new(),
        }
    }
}

impl DiskMemory {
    fn validate(&self) -> io::Result<()> {
        if self.version != SCHEMA_VERSION {
            return Err(invalid(format!(
                "unsupported ASCII organism memory version {} (expected {SCHEMA_VERSION})",
                self.version
            )));
        }
        if !self.life.valid() {
            return Err(invalid("ASCII organism memory has an invalid life state"));
        }
        if !self.life_updated_event_id.is_empty() && !valid_event_id(&self.life_updated_event_id) {
            return Err(invalid(
                "ASCII organism memory has an invalid life-state cursor",
            ));
        }
        if self.days.len() > MAX_DAILY_RECORDS {
            return Err(invalid("ASCII organism memory has too many daily records"));
        }
        if self.recent_event_ids.len() > MAX_RECENT_EVENT_IDS {
            return Err(invalid(
                "ASCII organism memory has too many idempotence tokens",
            ));
        }
        let mut recent_ids = HashSet::with_capacity(self.recent_event_ids.len());
        for id in &self.recent_event_ids {
            if !valid_event_id(id) || !recent_ids.insert(id.as_str()) {
                return Err(invalid(
                    "ASCII organism memory has an invalid or duplicate idempotence token",
                ));
            }
        }

        let mut seen = HashSet::with_capacity(self.days.len());
        let mut observation_ids = HashSet::new();
        let mut observation_count = 0usize;
        for stats in &self.days {
            if !valid_repo_id(&stats.repo) {
                return Err(invalid(
                    "ASCII organism memory has an invalid repository identifier",
                ));
            }
            if !seen.insert((stats.day, stats.repo.as_str())) {
                return Err(invalid(
                    "ASCII organism memory has duplicate day/repository records",
                ));
            }
            if stats.open_failures > stats.build_failures
                || (stats.open_failures == 0) != stats.open_failure_at_ms.is_none()
            {
                return Err(invalid(
                    "ASCII organism memory has an inconsistent failure streak",
                ));
            }
            if stats.recovered_pending_push && stats.last_recovery_duration_ms.is_none() {
                return Err(invalid(
                    "ASCII organism memory has an inconsistent recovery marker",
                ));
            }
            if stats.build_duration_sum_ms
                > u64::from(stats.build_duration_count).saturating_mul(MAX_TRACKED_BUILD_MS)
            {
                return Err(invalid(
                    "ASCII organism memory has an inconsistent build-duration aggregate",
                ));
            }
            if stats.observations.len() > MAX_OBSERVATIONS_PER_RECORD
                || (!stats.observations.is_empty() && stats.baseline.is_none())
            {
                return Err(invalid(
                    "ASCII organism memory has an invalid observation window",
                ));
            }
            observation_count = observation_count
                .checked_add(stats.observations.len())
                .ok_or_else(|| invalid("ASCII organism observation count overflow"))?;
            let compacted_through = stats
                .baseline
                .as_ref()
                .and_then(|baseline| baseline.compacted_through.as_ref());
            if compacted_through.is_some_and(|cursor| !valid_event_id(&cursor.id)) {
                return Err(invalid(
                    "ASCII organism memory has an invalid compaction cursor",
                ));
            }
            for observation in &stats.observations {
                let cursor = ObservationCursor {
                    at_ms: observation.at_ms,
                    id: observation.id.clone(),
                };
                if !valid_event_id(&observation.id)
                    || compacted_through.is_some_and(|compacted| &cursor <= compacted)
                    || !observation_ids.insert(observation.id.as_str())
                {
                    return Err(invalid(
                        "ASCII organism memory has an invalid or duplicate observation id",
                    ));
                }
            }
            if !stats.observations.is_empty() && !summary_matches_observations(stats) {
                return Err(invalid(
                    "ASCII organism memory counters do not match their observation log",
                ));
            }
        }
        if observation_count > MAX_OBSERVATIONS {
            return Err(invalid(
                "ASCII organism memory has too many retained observations",
            ));
        }
        Ok(())
    }

    fn stats(&self, day: i64, repo: &str) -> Option<&DailyStats> {
        self.days
            .iter()
            .find(|stats| stats.day == day && stats.repo == repo)
    }

    fn has_event_id(&self, id: &str) -> bool {
        self.recent_event_ids.iter().any(|recent| recent == id)
            || self.days.iter().any(|stats| {
                stats
                    .observations
                    .iter()
                    .any(|observation| observation.id == id)
            })
    }

    fn remember_event_id(&mut self, id: &str) {
        if self.recent_event_ids.iter().any(|recent| recent == id) {
            return;
        }
        self.recent_event_ids.push(id.to_owned());
        if self.recent_event_ids.len() > MAX_RECENT_EVENT_IDS {
            let overflow = self.recent_event_ids.len() - MAX_RECENT_EVENT_IDS;
            self.recent_event_ids.drain(..overflow);
        }
    }

    fn stats_mut(&mut self, day: i64, repo: &str) -> &mut DailyStats {
        if let Some(index) = self
            .days
            .iter()
            .position(|stats| stats.day == day && stats.repo == repo)
        {
            return &mut self.days[index];
        }

        self.days.push(DailyStats::new(day, repo.to_owned()));
        self.prune_around(day, repo);
        let index = self
            .days
            .iter()
            .position(|stats| stats.day == day && stats.repo == repo)
            .expect("new organism memory record must survive bounded pruning");
        &mut self.days[index]
    }

    fn prune_around(&mut self, inserted_day: i64, inserted_repo: &str) {
        if self.days.len() <= MAX_DAILY_RECORDS {
            return;
        }
        self.days.sort_by(|left, right| {
            left.day
                .cmp(&right.day)
                .then_with(|| left.repo.cmp(&right.repo))
        });
        let inserted = self
            .days
            .iter()
            .position(|stats| stats.day == inserted_day && stats.repo == inserted_repo)
            .expect("inserted organism record exists before pruning");
        let remove = if inserted == 0 { 1 } else { 0 };
        self.days.remove(remove);
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn valid_repo_id(repo: &str) -> bool {
    !repo.is_empty()
        && repo.len() <= MAX_REPO_BYTES
        && Path::new(repo).is_absolute()
        && !repo.chars().any(char::is_control)
        && !crate::review_input::contains_visual_spoof(repo)
}

fn valid_event_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_EVENT_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[derive(Clone, Debug)]
pub(crate) struct MemoryEvent {
    id: String,
    at_ms: u64,
    day: i64,
    repo: Option<String>,
    kind: CommandKind,
    exit_code: Option<i32>,
    /// Wall time of the finished command; a content-free scalar feeding the
    /// per-repo build-duration baseline. Never persisted as an event — only
    /// the bounded aggregates reach disk.
    duration_ms: Option<u64>,
    life: LifeSnapshot,
}

impl MemoryEvent {
    pub(crate) fn now_for_repo(
        kind: CommandKind,
        exit_code: Option<i32>,
        repo: Option<String>,
        life: LifeState,
        duration_ms: Option<u64>,
    ) -> Self {
        let at_ms = unix_ms();
        Self {
            id: next_event_id(),
            at_ms,
            day: local_day_at_ms(at_ms),
            repo,
            kind,
            exit_code,
            duration_ms,
            life: LifeSnapshot::from_state(life),
        }
    }

    #[cfg(test)]
    fn fixed(
        at_ms: u64,
        day: i64,
        repo: Option<&str>,
        kind: CommandKind,
        exit_code: Option<i32>,
        life: LifeState,
    ) -> Self {
        Self {
            id: next_event_id(),
            at_ms,
            day,
            repo: repo.map(str::to_owned),
            kind,
            exit_code,
            duration_ms: None,
            life: LifeSnapshot::from_state(life),
        }
    }

    #[cfg(test)]
    fn with_duration(mut self, duration_ms: Option<u64>) -> Self {
        self.duration_ms = duration_ms;
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MemoryInsight {
    pub(crate) open_failures: u32,
    pub(crate) recovered_failures: u32,
    pub(crate) push_after_recovery: bool,
    pub(crate) faster_than_yesterday: bool,
    pub(crate) repo_remembered: bool,
    /// Mean successful build/test wall time across every remembered day of
    /// this repo, once at least three samples exist.
    pub(crate) typical_build_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RepoContext {
    pub(crate) day: i64,
    pub(crate) repo: String,
    pub(crate) open_failures: u32,
    pub(crate) recovered_pending_push: bool,
    /// Today's build/test successes in this repo, for habituated reactions.
    pub(crate) successes_today: u32,
    /// Today's build/test failures in this repo, for sensitized reactions.
    pub(crate) failures_today: u32,
    /// Distinct remembered day records for this repo. Derived on read from the
    /// bounded `days` window — never persisted separately. Zero means the
    /// organism has no memory of ever working in this checkout.
    pub(crate) familiarity_days: u32,
}

pub(crate) struct OrganismMemory {
    path: PathBuf,
    memory: DiskMemory,
    session_events: VecDeque<MemoryEvent>,
}

impl OrganismMemory {
    pub(crate) fn load_default() -> io::Result<Self> {
        Self::load(crate::config::default_ascii_organism_memory_path())
    }

    fn load(path: PathBuf) -> io::Result<Self> {
        Ok(Self {
            memory: read_memory(&path)?,
            path,
            session_events: VecDeque::new(),
        })
    }

    pub(crate) fn life_state(&self) -> LifeState {
        self.memory.life.state()
    }

    pub(crate) fn refresh(&mut self) -> io::Result<()> {
        acknowledge_session_events(&self.path, &mut self.session_events);
        let mut memory = read_memory(&self.path)?;
        for event in &self.session_events {
            apply_event(&mut memory, event);
        }
        self.memory = memory;
        Ok(())
    }

    pub(crate) fn context_now(&mut self, cwd: Option<&str>) -> Option<RepoContext> {
        let cwd = cwd?;
        // Repository identity is deliberately resolved afresh. A permanent
        // cwd cache goes stale across `git init`/worktree removal and lets
        // attacker-controlled OSC cwd strings grow process memory without a
        // bound. This path runs only for build/test/push lifecycle events.
        let repo = git_repo_root_for(cwd)?;
        let day = local_day_at_ms(unix_ms());
        let (open_failures, recovered_pending_push, successes_today, failures_today) = self
            .memory
            .stats(day, &repo)
            .map(|stats| {
                (
                    stats.open_failures,
                    stats.recovered_pending_push,
                    stats.build_successes,
                    stats.build_failures,
                )
            })
            .unwrap_or((0, false, 0, 0));
        let familiarity_days = self
            .memory
            .days
            .iter()
            .filter(|stats| stats.repo == repo)
            .count() as u32;
        Some(RepoContext {
            day,
            repo,
            open_failures,
            recovered_pending_push,
            successes_today,
            failures_today,
            familiarity_days,
        })
    }

    fn apply_local(&mut self, event: &MemoryEvent) -> MemoryInsight {
        if self.session_events.len() == MAX_PENDING_MEMORY_EVENTS {
            self.session_events.pop_front();
        }
        self.session_events.push_back(event.clone());
        apply_event(&mut self.memory, event)
    }

    /// Admit one update to the durable queue and local optimistic view as a
    /// single operation. A persistence-worker scheduling error is still safe
    /// when the event was retained for shutdown/later retry. If the bounded
    /// event queue itself rejects the update, return only a throw-away preview
    /// insight so refresh can never resurrect an event absent from disk.
    pub(crate) fn apply_and_enqueue(
        &mut self,
        event: MemoryEvent,
    ) -> (MemoryInsight, io::Result<()>) {
        let result = enqueue_event(self.path.clone(), event.clone());
        let retained = result.is_ok() || event_is_queued(&self.path, &event.id);
        if retained {
            (self.apply_local(&event), result)
        } else {
            let mut preview = self.memory.clone();
            (apply_event(&mut preview, &event), result)
        }
    }

    #[cfg(test)]
    fn memory(&self) -> &DiskMemory {
        &self.memory
    }
}

fn apply_event(memory: &mut DiskMemory, event: &MemoryEvent) -> MemoryInsight {
    if event.at_ms > memory.life_updated_at_ms
        || (event.at_ms == memory.life_updated_at_ms
            && event.id.as_str() > memory.life_updated_event_id.as_str())
    {
        memory.life = event.life;
        memory.life_updated_at_ms = event.at_ms;
        memory.life_updated_event_id = event.id.clone();
    }

    let Some(repo) = event.repo.as_deref() else {
        return MemoryInsight::default();
    };
    let mut insight = MemoryInsight {
        repo_remembered: memory.days.iter().any(|stats| stats.repo == repo),
        ..MemoryInsight::default()
    };

    let observation_kind = match (event.kind, event.exit_code) {
        (CommandKind::BuildOrTest, Some(code)) if code != 0 => Some(ObservationKind::BuildFailure),
        (CommandKind::BuildOrTest, Some(0)) => Some(ObservationKind::BuildSuccess),
        (CommandKind::GitPush, Some(0)) => Some(ObservationKind::GitPush),
        _ => None,
    };
    let Some(kind) = observation_kind else {
        return insight;
    };
    // This check precedes every capacity mutation. An ambiguous atomic-write
    // retry must be a byte-for-byte logical no-op even after its observation
    // id has been folded out of the ordering window.
    if memory.has_event_id(&event.id) {
        return insight;
    }
    // Duration aggregates live outside the replayed observation window: they
    // are bounded monotone scalars, accumulated exactly once per event id.
    if kind == ObservationKind::BuildSuccess {
        if let Some(duration) = event.duration_ms {
            let stats = memory.stats_mut(event.day, repo);
            // Once the count saturates the pair must stop moving together,
            // or the sum could outgrow the count*cap validation invariant
            // and wedge persistence fail-closed.
            if stats.build_duration_count < u32::MAX {
                stats.build_duration_sum_ms = stats
                    .build_duration_sum_ms
                    .saturating_add(duration.min(MAX_TRACKED_BUILD_MS));
                stats.build_duration_count = stats.build_duration_count.saturating_add(1);
            }
        }
    }

    let replay = {
        let stats = memory.stats_mut(event.day, repo);
        let cursor = ObservationCursor {
            at_ms: event.at_ms,
            id: event.id.clone(),
        };
        if stats
            .baseline
            .as_ref()
            .and_then(|baseline| baseline.compacted_through.as_ref())
            .is_some_and(|compacted| &cursor <= compacted)
        {
            // Exact ordering before the compacted cursor is unavailable. A
            // genuinely new late event still contributes its monotonic count;
            // durable recent_event_ids distinguishes it from a retry. Keep the
            // newer prefix's derived failure/recovery state unchanged.
            let baseline = stats
                .baseline
                .as_mut()
                .expect("a compaction cursor always belongs to a baseline");
            match kind {
                ObservationKind::BuildFailure => {
                    baseline.build_failures = baseline.build_failures.saturating_add(1);
                }
                ObservationKind::BuildSuccess => {
                    baseline.build_successes = baseline.build_successes.saturating_add(1);
                }
                ObservationKind::GitPush => {
                    baseline.git_pushes = baseline.git_pushes.saturating_add(1);
                }
            }
            replay_observations(stats, None);
            insight.open_failures = stats.open_failures;
            None
        } else {
            if stats.baseline.is_none() {
                stats.baseline = Some(stats.baseline());
            }
            stats.observations.push(Observation {
                id: event.id.clone(),
                at_ms: event.at_ms,
                kind,
            });
            let replay = replay_observations(stats, Some(&event.id));
            if stats.observations.len() > MAX_OBSERVATIONS_PER_RECORD {
                // Insert and total-order the incoming event before advancing
                // the cursor, including an older event at exact capacity.
                compact_oldest_observations(stats);
            }
            Some(replay)
        }
    };
    memory.remember_event_id(&event.id);
    compact_global_observations(memory);
    if let Some(replay) = replay {
        insight.recovered_failures = replay.recovered_failures;
        insight.open_failures = replay.open_failures;
        insight.push_after_recovery = replay.push_after_recovery;
    }
    insight.faster_than_yesterday =
        insight.push_after_recovery && faster_than_previous_day(memory, event.day, repo);
    let (duration_sum, duration_count) = memory
        .days
        .iter()
        .filter(|stats| stats.repo == repo)
        .fold((0u64, 0u32), |(sum, count), stats| {
            (
                sum.saturating_add(stats.build_duration_sum_ms),
                count.saturating_add(stats.build_duration_count),
            )
        });
    if duration_count >= 3 {
        insight.typical_build_ms = Some(duration_sum / u64::from(duration_count));
    }
    insight
}

fn compact_global_observations(memory: &mut DiskMemory) {
    while memory
        .days
        .iter()
        .map(|stats| stats.observations.len())
        .sum::<usize>()
        > MAX_OBSERVATIONS
    {
        let Some(index) = memory
            .days
            .iter()
            .enumerate()
            .filter(|(_, stats)| stats.observations.len() >= 2)
            .min_by(|(_, left), (_, right)| {
                let left = left
                    .observations
                    .first()
                    .map(|event| (event.at_ms, &event.id));
                let right = right
                    .observations
                    .first()
                    .map(|event| (event.at_ms, &event.id));
                left.cmp(&right)
            })
            .map(|(index, _)| index)
        else {
            // With at most MAX_DAILY_RECORDS records and more than
            // MAX_OBSERVATIONS observations, pigeonhole guarantees a foldable
            // record. Keep this guard fail-closed if constants ever change.
            break;
        };
        compact_oldest_observations(&mut memory.days[index]);
    }
}

fn compact_oldest_observations(stats: &mut DailyStats) {
    if stats.observations.len() < 2 {
        return;
    }
    stats.observations.sort_by(|left, right| {
        left.at_ms
            .cmp(&right.at_ms)
            .then_with(|| left.id.cmp(&right.id))
    });
    let keep = stats.observations.split_off(stats.observations.len() / 2);
    let folded_events = std::mem::replace(&mut stats.observations, keep);
    let compacted_through = folded_events.last().map(|event| ObservationCursor {
        at_ms: event.at_ms,
        id: event.id.clone(),
    });
    let retained = std::mem::take(&mut stats.observations);
    stats.observations = folded_events;
    replay_observations(stats, None);
    let mut baseline = stats.baseline();
    if compacted_through > baseline.compacted_through {
        baseline.compacted_through = compacted_through;
    }
    stats.baseline = Some(baseline);
    stats.observations = retained;
    replay_observations(stats, None);
}

#[derive(Clone, Copy, Debug, Default)]
struct ReplayInsight {
    open_failures: u32,
    recovered_failures: u32,
    push_after_recovery: bool,
}

fn replay_observations(stats: &mut DailyStats, target_id: Option<&str>) -> ReplayInsight {
    stats.observations.sort_by(|left, right| {
        left.at_ms
            .cmp(&right.at_ms)
            .then_with(|| left.id.cmp(&right.id))
    });
    let baseline = stats.baseline.clone().unwrap_or_default();
    stats.build_failures = baseline.build_failures;
    stats.build_successes = baseline.build_successes;
    stats.git_pushes = baseline.git_pushes;
    stats.open_failures = baseline.open_failures;
    stats.open_failure_at_ms = baseline.open_failure_at_ms;
    stats.last_recovery_duration_ms = baseline.last_recovery_duration_ms;
    stats.recovered_pending_push = baseline.recovered_pending_push;

    let mut insight = ReplayInsight::default();
    for observation in &stats.observations {
        match observation.kind {
            ObservationKind::BuildFailure => {
                stats.build_failures = stats.build_failures.saturating_add(1);
                stats.open_failures = stats.open_failures.saturating_add(1);
                stats.open_failure_at_ms.get_or_insert(observation.at_ms);
                stats.recovered_pending_push = false;
                if target_id == Some(observation.id.as_str()) {
                    insight.open_failures = stats.open_failures;
                }
            }
            ObservationKind::BuildSuccess => {
                stats.build_successes = stats.build_successes.saturating_add(1);
                let recovered_failures = stats.open_failures;
                if let Some(started) = stats.open_failure_at_ms.take() {
                    stats.last_recovery_duration_ms = observation.at_ms.checked_sub(started);
                    stats.recovered_pending_push = stats.last_recovery_duration_ms.is_some();
                }
                stats.open_failures = 0;
                if target_id == Some(observation.id.as_str()) {
                    insight.recovered_failures = recovered_failures;
                }
            }
            ObservationKind::GitPush => {
                stats.git_pushes = stats.git_pushes.saturating_add(1);
                let after_recovery = std::mem::take(&mut stats.recovered_pending_push);
                if target_id == Some(observation.id.as_str()) {
                    insight.push_after_recovery = after_recovery;
                }
            }
        }
    }
    insight
}

fn summary_matches_observations(stats: &DailyStats) -> bool {
    let expected = stats.baseline();
    let mut rebuilt = stats.clone();
    replay_observations(&mut rebuilt, None);
    rebuilt.baseline() == expected
}

fn faster_than_previous_day(memory: &DiskMemory, day: i64, repo: &str) -> bool {
    let Some(previous_day) = day.checked_sub(1) else {
        return false;
    };
    let (Some(today), Some(previous)) = (memory.stats(day, repo), memory.stats(previous_day, repo))
    else {
        return false;
    };
    if today.build_successes == 0 || previous.build_successes == 0 {
        return false;
    }
    match (
        today.last_recovery_duration_ms,
        previous.last_recovery_duration_ms,
    ) {
        (Some(today), Some(previous)) => today < previous,
        (None, None) => today.build_failures < previous.build_failures,
        _ => false,
    }
}

fn read_memory(path: &Path) -> io::Result<DiskMemory> {
    let json = match crate::snapshot_file::read_bounded_private(path, MAX_MEMORY_BYTES) {
        Ok(json) => json,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(DiskMemory::default()),
        Err(error) => return Err(error),
    };
    let memory: DiskMemory = serde_json::from_str(&json)
        .map_err(|error| invalid(format!("invalid ASCII organism memory: {error}")))?;
    memory.validate()?;
    Ok(memory)
}

fn enqueue_event(path: PathBuf, event: MemoryEvent) -> io::Result<()> {
    let queues = event_queues();
    {
        let mut queues = queues
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let queue = queues.entry(path.clone()).or_default();
        if queue.iter().any(|queued| queued.id == event.id) {
            return schedule_queued_events(path);
        }
        if queue.len() >= MAX_PENDING_MEMORY_EVENTS {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "ASCII organism memory queue is full",
            ));
        }
        queue.push_back(event);
    }

    schedule_queued_events(path)
}

fn schedule_queued_events(path: PathBuf) -> io::Result<()> {
    let key = crate::persistence::PersistenceKey::for_path("ascii-organism", &path);
    let queued_path = path.clone();
    crate::persistence::enqueue(key, "Save ASCII organism memory", move || {
        let events: Vec<_> = {
            let queues = event_queues()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            queues
                .get(&queued_path)
                .map(|events| events.iter().cloned().collect())
                .unwrap_or_default()
        };
        if events.is_empty() {
            return Ok(());
        }
        let mut events = events;
        events.sort_by(|left, right| {
            left.at_ms
                .cmp(&right.at_ms)
                .then_with(|| left.id.cmp(&right.id))
        });
        transact_batch(&queued_path, &events)?;
        record_acknowledged_events(&queued_path, &events);
        let completed: HashSet<_> = events.iter().map(|event| event.id.as_str()).collect();
        let mut queues = event_queues()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(queue) = queues.get_mut(&queued_path) {
            queue.retain(|event| !completed.contains(event.id.as_str()));
            if queue.is_empty() {
                queues.remove(&queued_path);
            }
        }
        Ok(())
    })
}

/// Re-publish every retained memory queue before the shared persistence worker
/// enters its bounded shutdown drain. Failed transactions deliberately keep
/// their events here, so a recovered mount/permission state gets one last
/// durable attempt even when no later command arrived to schedule it.
pub(crate) fn flush_pending(timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    let paths: Vec<_> = event_queues()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .filter(|(_, events)| !events.is_empty())
        .map(|(path, _)| path.clone())
        .collect();
    let mut first_error = None;
    for path in paths {
        loop {
            match schedule_queued_events(path.clone()) {
                Ok(()) => break,
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(LOCK_POLL);
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                    break;
                }
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn event_queues() -> &'static Mutex<HashMap<PathBuf, VecDeque<MemoryEvent>>> {
    static QUEUES: OnceLock<Mutex<HashMap<PathBuf, VecDeque<MemoryEvent>>>> = OnceLock::new();
    QUEUES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn event_is_queued(path: &Path, id: &str) -> bool {
    event_queues()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(path)
        .is_some_and(|events| events.iter().any(|event| event.id == id))
}

fn acknowledged_events() -> &'static Mutex<HashMap<PathBuf, VecDeque<String>>> {
    static ACKNOWLEDGED: OnceLock<Mutex<HashMap<PathBuf, VecDeque<String>>>> = OnceLock::new();
    ACKNOWLEDGED.get_or_init(|| Mutex::new(HashMap::new()))
}

fn record_acknowledged_events(path: &Path, events: &[MemoryEvent]) {
    let mut acknowledged = acknowledged_events()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let ids = acknowledged.entry(path.to_path_buf()).or_default();
    for event in events {
        if !ids.iter().any(|id| id == &event.id) {
            ids.push_back(event.id.clone());
        }
    }
    while ids.len() > MAX_ACKNOWLEDGED_MEMORY_EVENTS {
        ids.pop_front();
    }
}

fn acknowledge_session_events(path: &Path, session_events: &mut VecDeque<MemoryEvent>) {
    if session_events.is_empty() {
        return;
    }
    let local_ids: HashSet<_> = session_events
        .iter()
        .map(|event| event.id.as_str())
        .collect();
    let mut acknowledged = acknowledged_events()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut completed = HashSet::new();
    if let Some(ids) = acknowledged.get_mut(path) {
        ids.retain(|id| {
            if local_ids.contains(id.as_str()) {
                completed.insert(id.clone());
                false
            } else {
                true
            }
        });
        if ids.is_empty() {
            acknowledged.remove(path);
        }
    }
    session_events.retain(|event| !completed.contains(&event.id));
}

fn transact(path: &Path, event: &MemoryEvent) -> io::Result<()> {
    transact_batch(path, std::slice::from_ref(event))
}

fn transact_batch(path: &Path, events: &[MemoryEvent]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "ASCII organism memory path has no parent",
        )
    })?;
    crate::snapshot_file::ensure_private_directory(parent)?;
    let _lock = MemoryTransactionLock::acquire(path, LOCK_TIMEOUT)?;
    let mut memory = read_memory(path)?;
    for event in events {
        apply_event(&mut memory, event);
    }
    memory.validate()?;
    let mut bytes = serde_json::to_vec_pretty(&memory).map_err(io::Error::other)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_MEMORY_BYTES {
        return Err(invalid("ASCII organism memory exceeds its 512 KiB limit"));
    }
    crate::snapshot_file::write_atomic_private(path, &bytes)
}

struct MemoryTransactionLock {
    directory: File,
    sidecar: File,
}

impl MemoryTransactionLock {
    fn acquire(path: &Path, timeout: Duration) -> io::Result<Self> {
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "memory path has no parent")
        })?;
        let directory = open_private_directory(parent)?;
        lock_with_timeout(&directory, timeout, "ASCII organism memory directory")?;

        let lock_path = lock_path_for(path)?;
        let sidecar = match open_private_lock_file(&lock_path) {
            Ok(file) => file,
            Err(error) => {
                unlock(&directory);
                return Err(error);
            }
        };
        if let Err(error) = lock_with_timeout(&sidecar, timeout, "ASCII organism memory lock") {
            unlock(&directory);
            return Err(error);
        }
        Ok(Self { directory, sidecar })
    }
}

impl Drop for MemoryTransactionLock {
    fn drop(&mut self) {
        unlock(&self.sidecar);
        unlock(&self.directory);
    }
}

fn lock_path_for(path: &Path) -> io::Result<PathBuf> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "memory path has no file name")
    })?;
    let mut lock_name = name.to_os_string();
    lock_name.push(".lock");
    Ok(path.with_file_name(lock_name))
}

fn open_private_directory(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ASCII organism memory parent is not a directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // SAFETY: geteuid has no preconditions and only reads process state.
        if metadata.uid() != unsafe { nix::libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "ASCII organism memory parent must be private and user-owned",
            ));
        }
    }
    Ok(file)
}

fn open_private_lock_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ASCII organism memory lock is not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // SAFETY: geteuid has no preconditions and only reads process state.
        if metadata.uid() != unsafe { nix::libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "ASCII organism memory lock is not a private user-owned file",
            ));
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

#[cfg(unix)]
fn try_lock(file: &File) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    // SAFETY: file owns a live descriptor and flock retains no Rust pointer.
    let result =
        unsafe { nix::libc::flock(file.as_raw_fd(), nix::libc::LOCK_EX | nix::libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error
        .raw_os_error()
        .is_some_and(|code| code == nix::libc::EAGAIN || code == nix::libc::EWOULDBLOCK)
    {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(not(unix))]
fn try_lock(_file: &File) -> io::Result<bool> {
    Ok(true)
}

fn lock_with_timeout(file: &File, timeout: Duration, label: &str) -> io::Result<()> {
    let started = Instant::now();
    loop {
        match try_lock(file)? {
            true => return Ok(()),
            false if started.elapsed() < timeout => std::thread::sleep(LOCK_POLL),
            false => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!("timed out waiting for {label}"),
                ))
            }
        }
    }
}

#[cfg(unix)]
fn unlock(file: &File) {
    use std::os::fd::AsRawFd;
    // SAFETY: file owns a live descriptor for this call.
    let result = unsafe { nix::libc::flock(file.as_raw_fd(), nix::libc::LOCK_UN) };
    if result != 0 {
        log::warn!(
            "failed to release ASCII organism memory lock: {}",
            io::Error::last_os_error()
        );
    }
}

#[cfg(not(unix))]
fn unlock(_file: &File) {}

pub(crate) fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn next_event_id() -> String {
    static NEXT_EVENT: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT_EVENT.fetch_add(1, Ordering::Relaxed);
    #[cfg(target_os = "linux")]
    {
        let mut random = [0_u8; 16];
        // SAFETY: random is a live writable buffer and getrandom retains no
        // pointer. The id is opaque on disk: it encodes neither PID nor time.
        let read = unsafe {
            nix::libc::getrandom(
                random.as_mut_ptr().cast(),
                random.len(),
                nix::libc::GRND_NONBLOCK,
            )
        };
        if read == random.len() as isize {
            return random.iter().map(|byte| format!("{byte:02x}")).collect();
        }
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    format!(
        "{:016x}",
        nonce.rotate_left(17) ^ sequence.wrapping_mul(0x9e37_79b9_7f4a_7c15)
    )
}

pub(crate) fn local_day_at_ms(at_ms: u64) -> i64 {
    let unix_seconds = i64::try_from(at_ms / 1_000).unwrap_or(i64::MAX);
    #[cfg(unix)]
    {
        let seconds: nix::libc::time_t = unix_seconds;
        let mut local: nix::libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: both pointers refer to live, correctly aligned values.
        if unsafe { nix::libc::localtime_r(&seconds, &mut local) }.is_null() {
            return unix_seconds.div_euclid(86_400);
        }
        days_from_civil(
            i64::from(local.tm_year) + 1900,
            u32::try_from(local.tm_mon + 1).unwrap_or(1),
            u32::try_from(local.tm_mday).unwrap_or(1),
        )
    }
    #[cfg(not(unix))]
    unix_seconds.div_euclid(86_400)
}

/// Gregorian civil date to days since 1970-01-01 (Howard Hinnant).
fn days_from_civil(mut year: i64, month: u32, day: u32) -> i64 {
    year -= i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let shifted_month = i64::from(month) + if month > 2 { -3 } else { 9 };
    let doy = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub(crate) fn git_repo_root_for(cwd: &str) -> Option<String> {
    if cwd.is_empty()
        || cwd.len() > 16 * 1024
        || cwd.chars().any(char::is_control)
        || crate::review_input::contains_visual_spoof(cwd)
    {
        return None;
    }
    let cwd = Path::new(cwd);
    if !cwd.is_absolute() {
        return None;
    }
    // A canonical root keeps one checkout under one key even when the shell's
    // logical cwd contains symlinks or `..`. Remote panes never call this
    // helper; see the explicit provenance gate in ui/organism.rs.
    let canonical = fs::canonicalize(cwd).ok()?;
    if !canonical.is_dir() {
        return None;
    }
    let mut directory = Some(canonical.as_path());
    while let Some(candidate) = directory {
        let marker = candidate.join(".git");
        match fs::symlink_metadata(&marker) {
            Ok(metadata) => {
                let kind = metadata.file_type();
                if !kind.is_dir() && !kind.is_file() {
                    // A symlinked marker is neither a trustworthy local repo
                    // identity nor a reason to keep walking into a parent
                    // checkout and misattribute the command there.
                    return None;
                }
                let head = if kind.is_dir() {
                    marker.join("HEAD")
                } else {
                    let pointer = read_small_git_file(&marker)?;
                    let target = pointer.trim().strip_prefix("gitdir:")?.trim();
                    let target = Path::new(target);
                    let git_dir = if target.is_absolute() {
                        target.to_path_buf()
                    } else {
                        candidate.join(target)
                    };
                    git_dir.join("HEAD")
                };
                if !valid_git_head(&head) {
                    return None;
                }
                let repo = candidate.to_str()?;
                return valid_repo_id(repo).then(|| repo.to_owned());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
        directory = candidate.parent();
    }
    None
}

fn valid_git_head(path: &Path) -> bool {
    let Some(head) = read_small_git_file(path) else {
        return false;
    };
    let head = head.trim();
    head.strip_prefix("ref:")
        .is_some_and(|reference| reference.trim().starts_with("refs/"))
        || (matches!(head.len(), 40 | 64) && head.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn read_small_git_file(path: &Path) -> Option<String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_GIT_POINTER_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_GIT_POINTER_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_GIT_POINTER_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "forge-organism-memory-{label}-{}-{id}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir(&path).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            Self(path)
        }

        fn memory_path(&self) -> PathBuf {
            self.0.join("state/ascii-organism.json")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn event(
        at_ms: u64,
        day: i64,
        repo: &Path,
        kind: CommandKind,
        exit_code: Option<i32>,
    ) -> MemoryEvent {
        MemoryEvent::fixed(
            at_ms,
            day,
            repo.to_str(),
            kind,
            exit_code,
            LifeState::default(),
        )
    }

    #[test]
    fn civil_day_index_has_exact_gregorian_boundaries() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(
            days_from_civil(2000, 2, 28) + 1,
            days_from_civil(2000, 2, 29)
        );
        assert_eq!(
            days_from_civil(2000, 2, 29) + 1,
            days_from_civil(2000, 3, 1)
        );
        assert_eq!(
            days_from_civil(2100, 2, 28) + 1,
            days_from_civil(2100, 3, 1)
        );
    }

    #[test]
    fn git_identity_is_the_full_local_root_and_rejects_symlink_markers() {
        let root = TestDir::new("repo-root");
        let repo = root.0.join("same-name");
        let nested = repo.join("a/b");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(git_repo_root_for(nested.to_str().unwrap()), None);
        fs::create_dir(repo.join(".git")).unwrap();
        fs::write(repo.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
        assert_eq!(
            git_repo_root_for(nested.to_str().unwrap()),
            repo.to_str().map(str::to_owned)
        );
        assert_eq!(git_repo_root_for("relative/repo"), None);

        fs::remove_dir_all(repo.join(".git")).unwrap();
        assert_eq!(git_repo_root_for(nested.to_str().unwrap()), None);
        fs::create_dir(repo.join(".git")).unwrap();
        fs::write(repo.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let linked = root.0.join("linked");
            fs::create_dir(&linked).unwrap();
            symlink(repo.join(".git"), linked.join(".git")).unwrap();
            assert_eq!(git_repo_root_for(linked.to_str().unwrap()), None);
        }
    }

    #[test]
    fn restart_round_trip_is_repo_and_day_scoped_and_private() {
        let root = TestDir::new("restart");
        let path = root.memory_path();
        let repo_a = root.0.join("repo-a");
        let repo_b = root.0.join("repo-b");
        let day = 20_000;

        transact(
            &path,
            &event(10_000, day, &repo_a, CommandKind::BuildOrTest, Some(1)),
        )
        .unwrap();
        transact(
            &path,
            &event(16_000, day, &repo_a, CommandKind::BuildOrTest, Some(0)),
        )
        .unwrap();
        transact(
            &path,
            &event(17_000, day, &repo_a, CommandKind::GitPush, Some(0)),
        )
        .unwrap();
        transact(
            &path,
            &event(18_000, day, &repo_b, CommandKind::BuildOrTest, Some(2)),
        )
        .unwrap();

        let loaded = OrganismMemory::load(path.clone()).unwrap();
        let a = loaded
            .memory()
            .stats(day, repo_a.to_str().unwrap())
            .unwrap();
        assert_eq!(a.build_failures, 1);
        assert_eq!(a.build_successes, 1);
        assert_eq!(a.git_pushes, 1);
        assert_eq!(a.last_recovery_duration_ms, Some(6_000));
        assert_eq!(a.open_failures, 0);
        let b = loaded
            .memory()
            .stats(day, repo_b.to_str().unwrap())
            .unwrap();
        assert_eq!(b.build_failures, 1);
        assert_eq!(b.open_failures, 1);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        assert!(fs::read_dir(path.parent().unwrap()).unwrap().all(|entry| {
            let name = entry.unwrap().file_name();
            !name.to_string_lossy().contains(".tmp.")
        }));
    }

    #[test]
    fn build_durations_aggregate_once_per_event_and_yield_a_baseline() {
        let repo = "/work/forge";
        let day = 60_000;
        let mut memory = DiskMemory::default();
        let success = |at_ms: u64, duration: Option<u64>| {
            MemoryEvent::fixed(
                at_ms,
                day,
                Some(repo),
                CommandKind::BuildOrTest,
                Some(0),
                LifeState::default(),
            )
            .with_duration(duration)
        };

        let first = success(1_000, Some(30_000));
        let one = apply_event(&mut memory, &first);
        assert_eq!(one.typical_build_ms, None, "needs three samples");
        // An ambiguous retry of the same event id is a byte-for-byte no-op.
        apply_event(&mut memory, &first);
        apply_event(&mut memory, &success(2_000, Some(60_000)));
        let insight = apply_event(&mut memory, &success(3_000, Some(90_000)));
        assert_eq!(insight.typical_build_ms, Some(60_000));

        let stats = memory.stats(day, repo).unwrap();
        assert_eq!(stats.build_duration_sum_ms, 180_000);
        assert_eq!(stats.build_duration_count, 3);
        memory.validate().unwrap();

        // Failures and duration-less successes never touch the aggregate,
        // and a pathological duration is capped before it lands.
        apply_event(
            &mut memory,
            &MemoryEvent::fixed(
                4_000,
                day,
                Some(repo),
                CommandKind::BuildOrTest,
                Some(1),
                LifeState::default(),
            )
            .with_duration(Some(500_000)),
        );
        apply_event(&mut memory, &success(5_000, None));
        apply_event(&mut memory, &success(6_000, Some(u64::MAX)));
        let stats = memory.stats(day, repo).unwrap();
        assert_eq!(stats.build_duration_count, 4);
        assert_eq!(stats.build_duration_sum_ms, 180_000 + MAX_TRACKED_BUILD_MS);
        memory.validate().unwrap();

        // The aggregate survives observation compaction untouched.
        let sum_before = memory.stats(day, repo).unwrap().build_duration_sum_ms;
        compact_oldest_observations(memory.stats_mut(day, repo));
        assert_eq!(
            memory.stats(day, repo).unwrap().build_duration_sum_ms,
            sum_before
        );

        // An inconsistent aggregate is rejected on load.
        let mut poisoned = memory.clone();
        poisoned.days[0].build_duration_sum_ms =
            u64::from(poisoned.days[0].build_duration_count) * MAX_TRACKED_BUILD_MS + 1;
        assert!(poisoned.validate().is_err());
    }

    #[test]
    fn yesterday_comparison_is_exact_and_repo_local() {
        let repo = "/work/forge";
        let other = "/work/other";
        let day = 50_000;
        let mut memory = DiskMemory::default();

        apply_event(
            &mut memory,
            &MemoryEvent::fixed(
                1_000,
                day - 1,
                Some(repo),
                CommandKind::BuildOrTest,
                Some(1),
                LifeState::default(),
            ),
        );
        apply_event(
            &mut memory,
            &MemoryEvent::fixed(
                11_000,
                day - 1,
                Some(repo),
                CommandKind::BuildOrTest,
                Some(0),
                LifeState::default(),
            ),
        );
        apply_event(
            &mut memory,
            &MemoryEvent::fixed(
                20_000,
                day,
                Some(repo),
                CommandKind::BuildOrTest,
                Some(1),
                LifeState::default(),
            ),
        );
        apply_event(
            &mut memory,
            &MemoryEvent::fixed(
                25_000,
                day,
                Some(repo),
                CommandKind::BuildOrTest,
                Some(0),
                LifeState::default(),
            ),
        );
        let insight = apply_event(
            &mut memory,
            &MemoryEvent::fixed(
                26_000,
                day,
                Some(repo),
                CommandKind::GitPush,
                Some(0),
                LifeState::default(),
            ),
        );
        assert!(insight.push_after_recovery);
        assert!(insight.faster_than_yesterday);

        let mut gap = memory.clone();
        gap.days.retain(|stats| stats.day != day - 1);
        assert!(!faster_than_previous_day(&gap, day, repo));
        assert!(!faster_than_previous_day(&memory, day, other));

        let today = memory.stats_mut(day, repo);
        today.last_recovery_duration_ms = Some(10_000);
        assert!(!faster_than_previous_day(&memory, day, repo));
    }

    #[test]
    fn corrupt_or_future_memory_is_never_replaced_by_a_transaction() {
        let root = TestDir::new("fail-closed");
        let path = root.memory_path();
        crate::snapshot_file::ensure_private_directory(path.parent().unwrap()).unwrap();
        let corrupt = b"{\"version\":99,\"life\":{},\"life_updated_at_ms\":0,\"days\":[]}";
        crate::snapshot_file::write_atomic_private(&path, corrupt).unwrap();
        let repo = root.0.join("repo");
        let result = transact(
            &path,
            &event(1, 1, &repo, CommandKind::BuildOrTest, Some(1)),
        );
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), corrupt);
    }

    #[cfg(unix)]
    #[test]
    fn repo_paths_are_never_loaded_from_a_group_or_world_readable_file() {
        use std::os::unix::fs::PermissionsExt;

        let root = TestDir::new("private-read");
        let path = root.memory_path();
        let repo = root.0.join("repo");
        transact(
            &path,
            &event(1, 1, &repo, CommandKind::BuildOrTest, Some(1)),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(OrganismMemory::load(path).is_err());
    }

    #[test]
    fn mixed_writer_order_replays_failure_recovery_and_push_by_event_time() {
        let repo = "/work/ordered";
        let day = 42;
        let mut memory = DiskMemory::default();
        let failure = MemoryEvent::fixed(
            1_000,
            day,
            Some(repo),
            CommandKind::BuildOrTest,
            Some(1),
            LifeState::default(),
        );
        let success = MemoryEvent::fixed(
            5_000,
            day,
            Some(repo),
            CommandKind::BuildOrTest,
            Some(0),
            LifeState::default(),
        );
        let push = MemoryEvent::fixed(
            6_000,
            day,
            Some(repo),
            CommandKind::GitPush,
            Some(0),
            LifeState::default(),
        );

        // Simulate three processes acquiring the filesystem lock backwards.
        apply_event(&mut memory, &push);
        apply_event(&mut memory, &success);
        apply_event(&mut memory, &failure);

        let stats = memory.stats(day, repo).unwrap();
        assert_eq!(stats.build_failures, 1);
        assert_eq!(stats.build_successes, 1);
        assert_eq!(stats.git_pushes, 1);
        assert_eq!(stats.open_failures, 0);
        assert_eq!(stats.last_recovery_duration_ms, Some(4_000));
        assert!(!stats.recovered_pending_push);
        memory.validate().unwrap();
    }

    #[test]
    fn observation_window_compacts_without_losing_aggregate_state() {
        let repo = "/work/hot-repo";
        let mut memory = DiskMemory::default();
        let failures = u64::try_from(MAX_OBSERVATIONS_PER_RECORD).unwrap() + 73;
        for index in 0..failures {
            apply_event(
                &mut memory,
                &MemoryEvent::fixed(
                    10_000 + index,
                    99,
                    Some(repo),
                    CommandKind::BuildOrTest,
                    Some(1),
                    LifeState::default(),
                ),
            );
        }
        apply_event(
            &mut memory,
            &MemoryEvent::fixed(
                20_000,
                99,
                Some(repo),
                CommandKind::BuildOrTest,
                Some(0),
                LifeState::default(),
            ),
        );
        apply_event(
            &mut memory,
            &MemoryEvent::fixed(
                21_000,
                99,
                Some(repo),
                CommandKind::GitPush,
                Some(0),
                LifeState::default(),
            ),
        );
        let stats = memory.stats(99, repo).unwrap();
        assert_eq!(stats.build_failures, failures as u32);
        assert_eq!(stats.build_successes, 1);
        assert_eq!(stats.git_pushes, 1);
        assert_eq!(stats.open_failures, 0);
        assert!(!stats.recovered_pending_push);
        assert!(stats.baseline.is_some());
        assert!(stats.observations.len() <= MAX_OBSERVATIONS_PER_RECORD);

        let baseline_failures_before = stats.baseline.as_ref().unwrap().build_failures;
        let retained_before = stats.observations.len();
        let watermark = stats
            .baseline
            .as_ref()
            .and_then(|baseline| baseline.compacted_through.as_ref())
            .unwrap()
            .at_ms;
        apply_event(
            &mut memory,
            &MemoryEvent::fixed(
                watermark.saturating_sub(1),
                99,
                Some(repo),
                CommandKind::BuildOrTest,
                Some(1),
                LifeState::default(),
            ),
        );
        let stats = memory.stats(99, repo).unwrap();
        assert_eq!(stats.build_failures, failures as u32 + 1);
        assert_eq!(stats.build_successes, 1);
        assert_eq!(stats.git_pushes, 1);
        assert_eq!(stats.open_failures, 0);
        assert_eq!(stats.observations.len(), retained_before);
        assert_eq!(
            stats.baseline.as_ref().unwrap().build_failures,
            baseline_failures_before + 1
        );
        memory.validate().unwrap();
        let mut serialized = serde_json::to_vec_pretty(&memory).unwrap();
        serialized.push(b'\n');
        assert!(serialized.len() as u64 <= MAX_MEMORY_BYTES);
    }

    #[test]
    fn an_older_event_at_the_compaction_boundary_is_ordered_before_folding() {
        let repo = "/work/compaction-boundary";
        let mut memory = DiskMemory::default();
        for index in 0..MAX_OBSERVATIONS_PER_RECORD {
            apply_event(
                &mut memory,
                &MemoryEvent::fixed(
                    100 + index as u64,
                    100,
                    Some(repo),
                    CommandKind::BuildOrTest,
                    Some(1),
                    LifeState::default(),
                ),
            );
        }
        apply_event(
            &mut memory,
            &MemoryEvent::fixed(
                50,
                100,
                Some(repo),
                CommandKind::BuildOrTest,
                Some(1),
                LifeState::default(),
            ),
        );

        let stats = memory.stats(100, repo).unwrap();
        assert_eq!(stats.build_failures, 65);
        assert_eq!(stats.open_failures, 65);
        assert!(stats.observations.len() <= MAX_OBSERVATIONS_PER_RECORD);
        memory.validate().unwrap();
    }

    #[test]
    fn a_compacted_event_id_remains_idempotent_across_ambiguous_retry() {
        let repo = "/work/idempotent";
        let mut memory = DiskMemory::default();
        let first = MemoryEvent::fixed(
            1_000,
            101,
            Some(repo),
            CommandKind::BuildOrTest,
            Some(1),
            LifeState::default(),
        );
        apply_event(&mut memory, &first);
        for index in 1..=MAX_OBSERVATIONS_PER_RECORD {
            apply_event(
                &mut memory,
                &MemoryEvent::fixed(
                    1_000 + index as u64,
                    101,
                    Some(repo),
                    CommandKind::BuildOrTest,
                    Some(1),
                    LifeState::default(),
                ),
            );
        }
        let before = memory.stats(101, repo).unwrap().clone();
        let recent_before = memory.recent_event_ids.len();
        apply_event(&mut memory, &first);
        let after = memory.stats(101, repo).unwrap();
        assert_eq!(after.build_failures, before.build_failures);
        assert_eq!(after.open_failures, before.open_failures);
        assert_eq!(after.observations.len(), before.observations.len());
        assert_eq!(memory.recent_event_ids.len(), recent_before);
        memory.validate().unwrap();
    }

    #[test]
    fn a_retried_compacted_transaction_is_idempotent_on_disk() {
        let root = TestDir::new("ambiguous-retry");
        let path = root.memory_path();
        let repo = root.0.join("repo");
        let events: Vec<_> = (0..=MAX_OBSERVATIONS_PER_RECORD)
            .map(|index| {
                event(
                    2_000 + index as u64,
                    102,
                    &repo,
                    CommandKind::BuildOrTest,
                    Some(1),
                )
            })
            .collect();
        transact_batch(&path, &events).unwrap();
        // Models rename succeeding while the first caller observes an error
        // from the final durability step and submits the exact batch again.
        transact_batch(&path, &events).unwrap();

        let memory = read_memory(&path).unwrap();
        let stats = memory.stats(102, repo.to_str().unwrap()).unwrap();
        assert_eq!(stats.build_failures, 65);
        assert_eq!(stats.open_failures, 65);
        memory.validate().unwrap();
    }

    #[test]
    fn global_observation_compaction_preserves_every_daily_aggregate() {
        let mut memory = DiskMemory::default();
        let repos: Vec<_> = (0..5).map(|index| format!("/work/repo-{index}")).collect();
        let mut duplicate = None;
        for (repo_index, repo) in repos.iter().enumerate() {
            let count = if repo_index == 0 { 52 } else { 51 };
            for event_index in 0..count {
                let event = MemoryEvent::fixed(
                    10_000 + (repo_index * 100 + event_index) as u64,
                    200 + repo_index as i64,
                    Some(repo),
                    CommandKind::BuildOrTest,
                    Some(1),
                    LifeState::default(),
                );
                if duplicate.is_none() {
                    duplicate = Some(event.clone());
                }
                apply_event(&mut memory, &event);
            }
        }
        assert_eq!(
            memory
                .days
                .iter()
                .map(|stats| stats.observations.len())
                .sum::<usize>(),
            MAX_OBSERVATIONS
        );
        let before: Vec<_> = memory
            .days
            .iter()
            .map(|stats| (stats.day, stats.repo.clone(), stats.build_failures))
            .collect();
        apply_event(&mut memory, duplicate.as_ref().unwrap());
        assert_eq!(
            memory
                .days
                .iter()
                .map(|stats| (stats.day, stats.repo.clone(), stats.build_failures))
                .collect::<Vec<_>>(),
            before
        );

        apply_event(
            &mut memory,
            &MemoryEvent::fixed(
                99_000,
                204,
                Some(&repos[4]),
                CommandKind::BuildOrTest,
                Some(1),
                LifeState::default(),
            ),
        );
        assert_eq!(memory.days.len(), 5);
        assert_eq!(
            memory
                .days
                .iter()
                .map(|stats| stats.build_failures)
                .sum::<u32>(),
            257
        );
        assert!(
            memory
                .days
                .iter()
                .map(|stats| stats.observations.len())
                .sum::<usize>()
                <= MAX_OBSERVATIONS
        );
        memory.validate().unwrap();
    }

    #[test]
    fn every_maximal_valid_schema_fits_the_pretty_json_budget() {
        let ids: Vec<_> = (0..MAX_RECENT_EVENT_IDS)
            .map(|index| format!("id{index:094}"))
            .collect();
        let mut memory = DiskMemory {
            recent_event_ids: ids.clone(),
            ..DiskMemory::default()
        };
        for record in 0..MAX_DAILY_RECORDS {
            let suffix = format!("-{record}");
            let mut repo = String::from("/");
            while repo.len() + suffix.len() + 1 < MAX_REPO_BYTES {
                repo.push('"');
            }
            repo.push_str(&suffix);
            let observations: Vec<_> = (0..4)
                .map(|offset| Observation {
                    id: ids[record * 4 + offset].clone(),
                    at_ms: 1_000 + offset as u64,
                    kind: ObservationKind::BuildFailure,
                })
                .collect();
            memory.days.push(DailyStats {
                day: record as i64,
                repo,
                build_failures: 4,
                build_successes: 0,
                git_pushes: 0,
                open_failures: 4,
                open_failure_at_ms: Some(1_000),
                last_recovery_duration_ms: None,
                recovered_pending_push: false,
                baseline: Some(StatsBaseline::default()),
                observations,
                // Widest valid encodings the duration aggregate can reach.
                build_duration_sum_ms: u64::from(u32::MAX) * MAX_TRACKED_BUILD_MS,
                build_duration_count: u32::MAX,
            });
        }
        memory.validate().unwrap();
        let mut bytes = serde_json::to_vec_pretty(&memory).unwrap();
        bytes.push(b'\n');
        assert!(
            bytes.len() as u64 <= MAX_MEMORY_BYTES,
            "maximal valid schema encoded to {} bytes",
            bytes.len()
        );
    }

    #[test]
    fn persisted_session_events_are_not_replayed_after_compaction() {
        let root = TestDir::new("session-ack");
        let path = root.memory_path();
        let repo = root.0.join("repo");
        let mut memory = OrganismMemory::load(path.clone()).unwrap();
        let count = MAX_OBSERVATIONS_PER_RECORD + 17;
        let events: Vec<_> = (0..count)
            .map(|index| {
                event(
                    30_000 + index as u64,
                    123,
                    &repo,
                    CommandKind::BuildOrTest,
                    Some(1),
                )
            })
            .collect();
        for event in &events {
            memory.apply_local(event);
        }
        transact_batch(&path, &events).unwrap();
        record_acknowledged_events(&path, &events);

        memory.refresh().unwrap();
        let stats = memory.memory().stats(123, repo.to_str().unwrap()).unwrap();
        assert_eq!(stats.build_failures, count as u32);
        assert_eq!(stats.open_failures, count as u32);
        assert!(memory.session_events.is_empty());

        // A second refresh is also idempotent after the folded observation ids
        // have disappeared from the retained tail.
        memory.refresh().unwrap();
        let stats = memory.memory().stats(123, repo.to_str().unwrap()).unwrap();
        assert_eq!(stats.build_failures, count as u32);
        assert_eq!(stats.open_failures, count as u32);
    }

    #[test]
    fn a_full_durable_queue_never_diverges_the_local_cache() {
        let root = TestDir::new("queue-full");
        let path = root.memory_path();
        let repo = root.0.join("repo");
        let mut queued = VecDeque::new();
        for index in 0..MAX_PENDING_MEMORY_EVENTS {
            queued.push_back(event(
                40_000 + index as u64,
                321,
                &repo,
                CommandKind::BuildOrTest,
                Some(1),
            ));
        }
        event_queues().lock().unwrap().insert(path.clone(), queued);

        let mut memory = OrganismMemory::load(path.clone()).unwrap();
        let rejected = event(50_000, 321, &repo, CommandKind::BuildOrTest, Some(1));
        let (preview, result) = memory.apply_and_enqueue(rejected);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
        assert_eq!(preview.open_failures, 1);
        assert!(memory.memory().stats(321, repo.to_str().unwrap()).is_none());
        assert!(memory.session_events.is_empty());

        event_queues().lock().unwrap().remove(&path);
    }

    #[test]
    fn life_snapshot_is_bounded_and_restored_without_floats_on_disk() {
        let state = LifeState {
            energy: -2.0,
            mood: 2.0,
            curiosity: f32::NAN,
            boredom: 0.1234,
            stress: 0.4,
            social_need: 0.5,
            attachment: 0.75,
            confidence: 1.0,
        };
        let snapshot = LifeSnapshot::from_state(state);
        assert!(snapshot.valid());
        let restored = snapshot.state();
        assert_eq!(restored.energy, 0.0);
        assert_eq!(restored.mood, 1.0);
        assert_eq!(restored.curiosity, 0.5);
        assert!((restored.boredom - 0.123).abs() < f32::EPSILON);
    }

    #[test]
    fn same_millisecond_life_updates_have_a_deterministic_event_cursor() {
        let mut lower = MemoryEvent::fixed(
            77_000,
            1,
            None,
            CommandKind::Other,
            Some(0),
            LifeState {
                energy: 0.1,
                ..LifeState::default()
            },
        );
        lower.id = "cursor-a".to_owned();
        let mut higher = MemoryEvent::fixed(
            77_000,
            1,
            None,
            CommandKind::Other,
            Some(0),
            LifeState {
                energy: 0.9,
                ..LifeState::default()
            },
        );
        higher.id = "cursor-b".to_owned();

        let mut forward = DiskMemory::default();
        apply_event(&mut forward, &lower);
        apply_event(&mut forward, &higher);
        let mut reverse = DiskMemory::default();
        apply_event(&mut reverse, &higher);
        apply_event(&mut reverse, &lower);

        assert_eq!(forward.life.energy, reverse.life.energy);
        assert_eq!(forward.life_updated_event_id, "cursor-b");
        assert_eq!(reverse.life_updated_event_id, "cursor-b");
        assert_eq!(forward.life.state().energy, 0.9);
    }

    #[test]
    fn concurrent_writer_helper() {
        let Some(path) = std::env::var_os("FORGE_ORGANISM_TEST_PATH") else {
            return;
        };
        let repo = PathBuf::from(std::env::var_os("FORGE_ORGANISM_TEST_REPO").unwrap());
        let count: u32 = std::env::var("FORGE_ORGANISM_TEST_COUNT")
            .unwrap()
            .parse()
            .unwrap();
        for index in 0..count {
            transact(
                Path::new(&path),
                &event(
                    1_000 + u64::from(index),
                    70_000,
                    &repo,
                    CommandKind::BuildOrTest,
                    Some(1),
                ),
            )
            .unwrap();
        }
    }

    #[test]
    fn concurrent_transactions_preserve_every_delta() {
        if std::env::var_os("FORGE_ORGANISM_TEST_PATH").is_some() {
            return;
        }
        let root = TestDir::new("concurrent");
        let path = root.memory_path();
        let repo = root.0.join("repo");
        let executable = std::env::current_exe().unwrap();
        let spawn = || {
            Command::new(&executable)
                .arg("--exact")
                .arg("organism_memory::tests::concurrent_writer_helper")
                .arg("--nocapture")
                .env("FORGE_ORGANISM_TEST_PATH", &path)
                .env("FORGE_ORGANISM_TEST_REPO", &repo)
                .env("FORGE_ORGANISM_TEST_COUNT", "20")
                .spawn()
                .unwrap()
        };
        let mut first = spawn();
        let mut second = spawn();
        assert!(first.wait().unwrap().success());
        assert!(second.wait().unwrap().success());

        let memory = read_memory(&path).unwrap();
        let stats = memory.stats(70_000, repo.to_str().unwrap()).unwrap();
        assert_eq!(stats.build_failures, 40);
        assert_eq!(stats.open_failures, 40);
        assert_eq!(memory.days.len(), 1);
    }
}
