//! Bounded background persistence for UI-owned snapshots.
//!
//! GTK state must be copied while the widgets are on the main thread, but file
//! creation, encoding, compression and `fsync` do not belong there.  This
//! worker owns exactly one background thread.  At most one pending job is kept
//! per target; a newer snapshot replaces an older snapshot that has not begun
//! yet, while different pane/session targets retain FIFO ordering. Weighted
//! snapshots also share one retained-byte budget that includes the running job.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAX_PENDING_TARGETS: usize = 128;
/// Bound snapshot memory retained by submitted work, including the one task
/// currently executing. Keeping the running task charged matters because a
/// slow `fsync` can otherwise make room for another full-size snapshot before
/// the first closure releases its owned bytes.
pub(crate) const MAX_PENDING_ESTIMATED_BYTES: usize = 512 * 1024 * 1024;
const MAX_REPORTED_FAILURES: usize = 32;

type PersistenceTask = Box<dyn FnOnce() -> io::Result<()> + Send + 'static>;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PersistenceKey {
    kind: String,
    path: PathBuf,
    nonce: Option<u64>,
}

impl PersistenceKey {
    pub(crate) fn for_path(kind: &str, path: &Path) -> Self {
        Self {
            kind: kind.to_string(),
            path: path.to_path_buf(),
            nonce: None,
        }
    }

    /// Reads are not coalescible: two panes may intentionally restore from the
    /// same legacy file and each owns a different completion route.
    pub(crate) fn unique_for_path(kind: &str, path: &Path) -> Self {
        static NEXT_UNIQUE_KEY: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_UNIQUE_KEY.fetch_add(1, Ordering::Relaxed);
        Self {
            kind: kind.to_string(),
            path: path.to_path_buf(),
            nonce: Some(sequence),
        }
    }

    #[cfg(test)]
    fn named(name: &str) -> Self {
        Self::for_path("test", Path::new(name))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PersistenceFailure {
    pub(crate) operation: String,
    pub(crate) error: String,
}

impl fmt::Display for PersistenceFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.error)
    }
}

struct PendingJob {
    key: PersistenceKey,
    generation: u64,
    operation: String,
    estimated_bytes: usize,
    task: PersistenceTask,
}

struct WorkerState {
    accepting: bool,
    running: bool,
    exited: bool,
    next_generation: u64,
    order: VecDeque<PersistenceKey>,
    pending: HashMap<PersistenceKey, PendingJob>,
    retained_estimated_bytes: usize,
    failures: VecDeque<(PersistenceKey, u64, PersistenceFailure)>,
    failed_targets: HashMap<PersistenceKey, u64>,
}

impl WorkerState {
    fn new() -> Self {
        Self {
            accepting: true,
            running: false,
            exited: false,
            next_generation: 0,
            order: VecDeque::new(),
            pending: HashMap::new(),
            retained_estimated_bytes: 0,
            failures: VecDeque::new(),
            failed_targets: HashMap::new(),
        }
    }

    fn issue_generation(&mut self) -> u64 {
        let generation = self.next_generation;
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .expect("persistence generation exhausted");
        generation
    }

    fn record_failure(
        &mut self,
        key: PersistenceKey,
        generation: u64,
        failure: PersistenceFailure,
    ) {
        // A failing mount can reject every autosave in a burst. Report one
        // event per target until that target saves successfully again.
        if !self.failed_targets.contains_key(&key)
            && self.failed_targets.len() == MAX_REPORTED_FAILURES
        {
            if let Some(stale_key) = self.failed_targets.keys().next().cloned() {
                self.failed_targets.remove(&stale_key);
                self.failures
                    .retain(|(failed_key, _, _)| failed_key != &stale_key);
            }
        }
        if let Some(latest_generation) = self.failed_targets.get_mut(&key) {
            // A queue-admission failure can be newer than completion of the
            // task that was already running for this key. Do not let that older
            // completion replace or later clear the newer diagnostic.
            if generation < *latest_generation {
                return;
            }
            *latest_generation = generation;
            if let Some(existing) = self
                .failures
                .iter_mut()
                .find(|(existing_key, _, _)| existing_key == &key)
            {
                existing.1 = generation;
                existing.2 = failure;
            }
            return;
        }
        if self.failures.len() == MAX_REPORTED_FAILURES {
            if let Some((stale_key, _, _)) = self.failures.pop_front() {
                self.failed_targets.remove(&stale_key);
            }
        }
        self.failed_targets.insert(key.clone(), generation);
        self.failures.push_back((key, generation, failure));
    }

    fn record_success(&mut self, key: &PersistenceKey, generation: u64) {
        if self
            .failed_targets
            .get(key)
            .is_some_and(|failed_generation| *failed_generation <= generation)
        {
            self.failed_targets.remove(key);
            self.failures.retain(|(failed_key, failed_generation, _)| {
                failed_key != key || *failed_generation > generation
            });
        }
    }

    fn release_estimated_bytes(&mut self, estimated_bytes: usize) {
        self.retained_estimated_bytes = self
            .retained_estimated_bytes
            .checked_sub(estimated_bytes)
            .expect("persistence estimated-byte accounting underflow");
    }
}

struct WorkerShared {
    state: Mutex<WorkerState>,
    changed: Condvar,
    capacity: usize,
    estimated_byte_capacity: usize,
}

/// A byte-budget charge whose lifetime can extend beyond the worker closure
/// which created a retained result. Dropping the last result owner releases
/// the charge; acquisition is non-blocking so the single worker can always
/// shrink or discard a result instead of waiting behind its own pending jobs.
pub(crate) struct EstimatedBytesReservation {
    shared: Arc<WorkerShared>,
    estimated_bytes: usize,
}

impl EstimatedBytesReservation {
    pub(crate) const fn estimated_bytes(&self) -> usize {
        self.estimated_bytes
    }

    pub(crate) fn shrink_to(&mut self, estimated_bytes: usize) {
        assert!(
            estimated_bytes <= self.estimated_bytes,
            "retained-result reservations can only shrink"
        );
        let released = self.estimated_bytes - estimated_bytes;
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.release_estimated_bytes(released);
        self.estimated_bytes = estimated_bytes;
        self.shared.changed.notify_all();
    }
}

impl Drop for EstimatedBytesReservation {
    fn drop(&mut self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.release_estimated_bytes(self.estimated_bytes);
        self.shared.changed.notify_all();
    }
}

struct PersistenceWorker {
    shared: Arc<WorkerShared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl PersistenceWorker {
    fn new(capacity: usize) -> io::Result<Self> {
        Self::new_with_limits(capacity, MAX_PENDING_ESTIMATED_BYTES)
    }

    fn new_with_limits(capacity: usize, estimated_byte_capacity: usize) -> io::Result<Self> {
        let shared = Arc::new(WorkerShared {
            state: Mutex::new(WorkerState::new()),
            changed: Condvar::new(),
            capacity,
            estimated_byte_capacity,
        });
        let worker_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("forge-persistence".to_string())
            .spawn(move || run_worker(worker_shared))?;
        Ok(Self {
            shared,
            thread: Mutex::new(Some(handle)),
        })
    }

    fn enqueue(
        &self,
        key: PersistenceKey,
        operation: String,
        task: PersistenceTask,
    ) -> io::Result<()> {
        self.enqueue_weighted(key, operation, 0, task)
    }

    fn enqueue_weighted(
        &self,
        key: PersistenceKey,
        operation: String,
        estimated_bytes: usize,
        task: PersistenceTask,
    ) -> io::Result<()> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let generation = state.issue_generation();
        if !state.accepting {
            let error = io::Error::new(
                io::ErrorKind::BrokenPipe,
                "persistence worker is shutting down",
            );
            state.record_failure(
                key,
                generation,
                PersistenceFailure {
                    operation,
                    error: error.to_string(),
                },
            );
            // A rejected task may own a retained-result permit whose Drop
            // re-enters this ledger. Release the mutex before dropping it.
            drop(state);
            return Err(error);
        }

        let previous_estimated_bytes = state
            .pending
            .get(&key)
            .map_or(0, |previous| previous.estimated_bytes);
        let replacing_pending = state.pending.contains_key(&key);
        if !replacing_pending && state.pending.len() >= self.shared.capacity {
            let error = io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "persistence queue is full ({} distinct targets)",
                    self.shared.capacity
                ),
            );
            state.record_failure(
                key,
                generation,
                PersistenceFailure {
                    operation,
                    error: error.to_string(),
                },
            );
            drop(state);
            return Err(error);
        }

        let retained_without_previous = state
            .retained_estimated_bytes
            .checked_sub(previous_estimated_bytes)
            .expect("pending persistence bytes exceed retained-byte accounting");
        let next_retained_estimated_bytes = retained_without_previous.checked_add(estimated_bytes);
        if next_retained_estimated_bytes
            .is_none_or(|bytes| bytes > self.shared.estimated_byte_capacity)
        {
            let error = io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "persistence queue estimated-byte budget exceeded ({} bytes retained, {} byte submission, {} byte limit)",
                    retained_without_previous,
                    estimated_bytes,
                    self.shared.estimated_byte_capacity
                ),
            );
            state.record_failure(
                key,
                generation,
                PersistenceFailure {
                    operation,
                    error: error.to_string(),
                },
            );
            drop(state);
            return Err(error);
        }
        let next_retained_estimated_bytes = next_retained_estimated_bytes
            .expect("checked above: persistence retained-byte addition fits");

        let job = PendingJob {
            key: key.clone(),
            generation,
            operation,
            estimated_bytes,
            task,
        };
        if replacing_pending {
            // Keep the target's original queue position, but replace all owned
            // snapshot bytes with the newest state. Admission was checked before
            // touching `previous`, so a rejected replacement leaves it intact.
            let previous = state
                .pending
                .insert(key, job)
                .expect("replacing_pending guarantees an existing job");
            state.retained_estimated_bytes = next_retained_estimated_bytes;
            // Pending closures may own external permits/leases whose Drop
            // locks this WorkerState. Never run user-owned destructors while
            // the ledger mutex is held.
            drop(state);
            drop(previous);
            return Ok(());
        }

        state.order.push_back(key.clone());
        state.pending.insert(key, job);
        state.retained_estimated_bytes = next_retained_estimated_bytes;
        self.shared.changed.notify_one();
        Ok(())
    }

    fn try_reserve_estimated_bytes(
        &self,
        estimated_bytes: usize,
    ) -> io::Result<EstimatedBytesReservation> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(next_retained_estimated_bytes) = state
            .retained_estimated_bytes
            .checked_add(estimated_bytes)
            .filter(|bytes| *bytes <= self.shared.estimated_byte_capacity)
        else {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "persistence retained-result byte budget exceeded ({} bytes retained, {} byte reservation, {} byte limit)",
                    state.retained_estimated_bytes,
                    estimated_bytes,
                    self.shared.estimated_byte_capacity,
                ),
            ));
        };
        state.retained_estimated_bytes = next_retained_estimated_bytes;
        Ok(EstimatedBytesReservation {
            shared: Arc::clone(&self.shared),
            estimated_bytes,
        })
    }

    fn reserve_estimated_bytes_up_to(&self, max_bytes: usize) -> EstimatedBytesReservation {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let available = self
            .shared
            .estimated_byte_capacity
            .saturating_sub(state.retained_estimated_bytes);
        let estimated_bytes = max_bytes.min(available);
        state.retained_estimated_bytes = state
            .retained_estimated_bytes
            .checked_add(estimated_bytes)
            .expect("reservation is capped by available persistence bytes");
        EstimatedBytesReservation {
            shared: Arc::clone(&self.shared),
            estimated_bytes,
        }
    }

    fn drain_failures(&self) -> Vec<PersistenceFailure> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .failures
            .drain(..)
            .map(|(_, _, failure)| failure)
            .collect()
    }

    fn shutdown(&self, timeout: Duration) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.accepting = false;
        self.shared.changed.notify_all();

        while !state.exited {
            let now = Instant::now();
            if now >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "persistence worker did not flush before shutdown",
                ));
            }
            let remaining = deadline.saturating_duration_since(now);
            let (next, wait) = self
                .shared
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
            if wait.timed_out() && !state.exited {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "persistence worker did not flush before shutdown",
                ));
            }
        }
        drop(state);

        if let Some(handle) = self
            .thread
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            handle
                .join()
                .map_err(|_| io::Error::other("persistence worker panicked"))?;
        }
        Ok(())
    }
}

fn run_worker(shared: Arc<WorkerShared>) {
    loop {
        let job = {
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                if let Some(key) = state.order.pop_front() {
                    if let Some(job) = state.pending.remove(&key) {
                        state.running = true;
                        break Some(job);
                    }
                    continue;
                }
                if !state.accepting {
                    debug_assert!(state.pending.is_empty());
                    // Retained-result reservations may intentionally outlive
                    // the worker thread (for example, decoded history waiting
                    // for GTK to consume it). Their Drop still owns `shared`
                    // and releases the ledger independently.
                    state.exited = true;
                    shared.changed.notify_all();
                    break None;
                }
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };

        let Some(job) = job else {
            return;
        };
        let PendingJob {
            key,
            generation,
            operation,
            estimated_bytes,
            task,
        } = job;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task))
            .unwrap_or_else(|_| Err(io::Error::other("persistence task panicked")));
        if let Err(error) = result {
            log::error!("{operation}: {error}");
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.record_failure(
                key,
                generation,
                PersistenceFailure {
                    operation,
                    error: error.to_string(),
                },
            );
            state.release_estimated_bytes(estimated_bytes);
            state.running = false;
            shared.changed.notify_all();
            continue;
        }

        let mut state = shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.record_success(&key, generation);
        state.release_estimated_bytes(estimated_bytes);
        state.running = false;
        shared.changed.notify_all();
    }
}

static PERSISTENCE_WORKER: OnceLock<Result<PersistenceWorker, String>> = OnceLock::new();

fn global_worker() -> io::Result<&'static PersistenceWorker> {
    PERSISTENCE_WORKER
        .get_or_init(|| {
            PersistenceWorker::new(MAX_PENDING_TARGETS).map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|error| io::Error::other(error.clone()))
}

/// Submit a closure that captures only small metadata. Owned snapshots or
/// bounded working sets large enough to matter must use [`enqueue_weighted`]
/// so target-count admission also bounds retained memory.
pub(crate) fn enqueue(
    key: PersistenceKey,
    operation: impl Into<String>,
    task: impl FnOnce() -> io::Result<()> + Send + 'static,
) -> io::Result<()> {
    global_worker()?.enqueue(key, operation.into(), Box::new(task))
}

/// Submit work with a conservative estimate of the memory it retains. The
/// charge remains active while the task runs and is released only after its
/// success, failure, or caught panic has been recorded.
pub(crate) fn enqueue_weighted(
    key: PersistenceKey,
    operation: impl Into<String>,
    estimated_bytes: usize,
    task: impl FnOnce() -> io::Result<()> + Send + 'static,
) -> io::Result<()> {
    global_worker()?.enqueue_weighted(key, operation.into(), estimated_bytes, Box::new(task))
}

/// Try to charge a retained result which escapes its worker closure. This never
/// waits for capacity: callers on the persistence thread must shrink or drop
/// the result on `WouldBlock`, otherwise they could deadlock behind queued work
/// whose own charge is preventing admission.
pub(crate) fn try_reserve_estimated_bytes(
    estimated_bytes: usize,
) -> io::Result<EstimatedBytesReservation> {
    global_worker()?.try_reserve_estimated_bytes(estimated_bytes)
}

/// Reserve as much of `max_bytes` as is currently available without waiting.
/// The returned permit may be zero-sized; callers use its exact amount as the
/// retained-result budget and revoke deletion authority if pressure forced a
/// smaller result than the product-level cap.
pub(crate) fn reserve_estimated_bytes_up_to(
    max_bytes: usize,
) -> io::Result<EstimatedBytesReservation> {
    Ok(global_worker()?.reserve_estimated_bytes_up_to(max_bytes))
}

pub(crate) fn drain_failures() -> Vec<PersistenceFailure> {
    match PERSISTENCE_WORKER.get() {
        Some(Ok(worker)) => worker.drain_failures(),
        _ => Vec::new(),
    }
}

pub(crate) fn shutdown(timeout: Duration) -> io::Result<()> {
    match PERSISTENCE_WORKER.get() {
        Some(Ok(worker)) => worker.shutdown(timeout),
        Some(Err(error)) => Err(io::Error::other(error.clone())),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    fn retained_estimated_bytes(worker: &PersistenceWorker) -> usize {
        worker
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retained_estimated_bytes
    }

    #[test]
    fn non_utf8_paths_keep_their_original_identity() {
        let first = PathBuf::from(OsString::from_vec(vec![b'h', 0x80]));
        let second = PathBuf::from(OsString::from_vec(vec![b'h', 0x81]));
        assert_eq!(first.to_string_lossy(), second.to_string_lossy());
        assert_ne!(
            PersistenceKey::for_path("history", &first),
            PersistenceKey::for_path("history", &second)
        );
    }

    #[test]
    fn slow_io_keeps_submit_nonblocking_and_latest_pending_snapshot_wins() {
        let worker = PersistenceWorker::new(4).unwrap();
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let writes = Arc::new(Mutex::new(Vec::new()));

        let writes_first = Arc::clone(&writes);
        worker
            .enqueue(
                PersistenceKey::named("session"),
                "save session".into(),
                Box::new(move || {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    writes_first.lock().unwrap().push(1);
                    Ok(())
                }),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let writes_stale = Arc::clone(&writes);
        worker
            .enqueue(
                PersistenceKey::named("session"),
                "save session".into(),
                Box::new(move || {
                    writes_stale.lock().unwrap().push(2);
                    Ok(())
                }),
            )
            .unwrap();
        let writes_latest = Arc::clone(&writes);
        worker
            .enqueue(
                PersistenceKey::named("session"),
                "save session".into(),
                Box::new(move || {
                    writes_latest.lock().unwrap().push(3);
                    Ok(())
                }),
            )
            .unwrap();

        // Enqueue returned while the first task was deliberately blocked.
        assert!(writes.lock().unwrap().is_empty());
        release_tx.send(()).unwrap();
        worker.shutdown(Duration::from_secs(1)).unwrap();
        assert_eq!(*writes.lock().unwrap(), [1, 3]);
    }

    #[test]
    fn write_failure_is_reported_and_does_not_stop_later_jobs() {
        let worker = PersistenceWorker::new(4).unwrap();
        let completed = Arc::new(AtomicUsize::new(0));
        worker
            .enqueue(
                PersistenceKey::named("broken"),
                "save block history".into(),
                Box::new(|| Err(io::Error::new(io::ErrorKind::PermissionDenied, "read-only"))),
            )
            .unwrap();
        let completed_job = Arc::clone(&completed);
        worker
            .enqueue(
                PersistenceKey::named("healthy"),
                "save session".into(),
                Box::new(move || {
                    completed_job.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }),
            )
            .unwrap();

        worker.shutdown(Duration::from_secs(1)).unwrap();
        assert_eq!(completed.load(Ordering::SeqCst), 1);
        assert_eq!(
            worker.drain_failures(),
            [PersistenceFailure {
                operation: "save block history".into(),
                error: "read-only".into(),
            }]
        );
    }

    #[test]
    fn failures_with_the_same_operation_remain_distinct_per_target() {
        let worker = PersistenceWorker::new(4).unwrap();
        for target in ["left", "right"] {
            worker
                .enqueue(
                    PersistenceKey::named(target),
                    "Save Block history".into(),
                    Box::new(move || Err(io::Error::new(io::ErrorKind::PermissionDenied, target))),
                )
                .unwrap();
        }
        worker.shutdown(Duration::from_secs(1)).unwrap();
        let failures = worker.drain_failures();
        assert_eq!(failures.len(), 2);
        assert!(failures.iter().any(|failure| failure.error == "left"));
        assert!(failures.iter().any(|failure| failure.error == "right"));
    }

    #[test]
    fn later_success_removes_an_undrained_failure_for_the_same_target() {
        let worker = PersistenceWorker::new_with_limits(4, 10).unwrap();
        let key = PersistenceKey::named("recovering");
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);

        worker
            .enqueue_weighted(
                key.clone(),
                "save session".into(),
                4,
                Box::new(move || {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Err(io::Error::new(io::ErrorKind::PermissionDenied, "first"))
                }),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        worker
            .enqueue_weighted(key, "save session".into(), 6, Box::new(|| Ok(())))
            .unwrap();

        release_tx.send(()).unwrap();
        worker.shutdown(Duration::from_secs(1)).unwrap();
        assert_eq!(retained_estimated_bytes(&worker), 0);
        assert!(worker.drain_failures().is_empty());
    }

    #[test]
    fn estimated_byte_budget_accepts_exact_limit_and_rejects_limit_plus_one() {
        let worker = PersistenceWorker::new_with_limits(4, 10).unwrap();
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        worker
            .enqueue_weighted(
                PersistenceKey::named("exact"),
                "save exact".into(),
                10,
                Box::new(move || {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                }),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(retained_estimated_bytes(&worker), 10);

        let error = worker
            .enqueue_weighted(
                PersistenceKey::named("overflow"),
                "save overflow".into(),
                1,
                Box::new(|| Ok(())),
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(retained_estimated_bytes(&worker), 10);

        release_tx.send(()).unwrap();
        worker.shutdown(Duration::from_secs(1)).unwrap();
        assert_eq!(retained_estimated_bytes(&worker), 0);
    }

    #[test]
    fn weighted_replacement_updates_accounting_without_dropping_previous_on_rejection() {
        let worker = PersistenceWorker::new_with_limits(4, 10).unwrap();
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        worker
            .enqueue(
                PersistenceKey::named("blocker"),
                "block worker".into(),
                Box::new(move || {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                }),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let writes = Arc::new(Mutex::new(Vec::new()));
        for (estimated_bytes, value) in [(6, 6), (4, 4), (10, 10)] {
            let writes = Arc::clone(&writes);
            worker
                .enqueue_weighted(
                    PersistenceKey::named("snapshot"),
                    "save snapshot".into(),
                    estimated_bytes,
                    Box::new(move || {
                        writes.lock().unwrap().push(value);
                        Ok(())
                    }),
                )
                .unwrap();
            assert_eq!(retained_estimated_bytes(&worker), estimated_bytes);
        }

        let rejected_writes = Arc::clone(&writes);
        let error = worker
            .enqueue_weighted(
                PersistenceKey::named("snapshot"),
                "save snapshot".into(),
                11,
                Box::new(move || {
                    rejected_writes.lock().unwrap().push(11);
                    Ok(())
                }),
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(retained_estimated_bytes(&worker), 10);

        release_tx.send(()).unwrap();
        worker.shutdown(Duration::from_secs(1)).unwrap();
        assert_eq!(*writes.lock().unwrap(), [10]);
        assert_eq!(retained_estimated_bytes(&worker), 0);
        let failures = worker.drain_failures();
        assert_eq!(failures.len(), 1);
        assert!(failures[0].error.contains("estimated-byte budget exceeded"));
    }

    #[test]
    fn replacing_task_drops_reentrant_result_permit_outside_worker_lock() {
        let worker = PersistenceWorker::new_with_limits(4, 10).unwrap();
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        worker
            .enqueue(
                PersistenceKey::named("blocker"),
                "block worker".into(),
                Box::new(move || {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                }),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let reservation = worker.try_reserve_estimated_bytes(3).unwrap();
        worker
            .enqueue(
                PersistenceKey::named("replace-me"),
                "old load result".into(),
                Box::new(move || {
                    drop(reservation);
                    Ok(())
                }),
            )
            .unwrap();
        assert_eq!(retained_estimated_bytes(&worker), 3);

        // Replacing the pending closure drops its captured reservation. Its
        // destructor locks WorkerState, so this call deadlocked before the old
        // PendingJob was moved out of the mutex guard.
        worker
            .enqueue(
                PersistenceKey::named("replace-me"),
                "new load result".into(),
                Box::new(|| Ok(())),
            )
            .unwrap();
        assert_eq!(retained_estimated_bytes(&worker), 0);

        release_tx.send(()).unwrap();
        worker.shutdown(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn running_and_pending_jobs_share_the_estimated_byte_budget() {
        let worker = PersistenceWorker::new_with_limits(4, 10).unwrap();
        let (first_started_tx, first_started_rx) = mpsc::sync_channel(0);
        let (first_release_tx, first_release_rx) = mpsc::sync_channel(0);
        worker
            .enqueue_weighted(
                PersistenceKey::named("first"),
                "save first".into(),
                4,
                Box::new(move || {
                    first_started_tx.send(()).unwrap();
                    first_release_rx.recv().unwrap();
                    Ok(())
                }),
            )
            .unwrap();
        first_started_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        let (second_started_tx, second_started_rx) = mpsc::sync_channel(0);
        let (second_release_tx, second_release_rx) = mpsc::sync_channel(0);
        worker
            .enqueue_weighted(
                PersistenceKey::named("second"),
                "save second".into(),
                6,
                Box::new(move || {
                    second_started_tx.send(()).unwrap();
                    second_release_rx.recv().unwrap();
                    Ok(())
                }),
            )
            .unwrap();
        assert_eq!(retained_estimated_bytes(&worker), 10);

        let error = worker
            .enqueue_weighted(
                PersistenceKey::named("third"),
                "save third".into(),
                1,
                Box::new(|| Ok(())),
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(retained_estimated_bytes(&worker), 10);

        first_release_tx.send(()).unwrap();
        second_started_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(retained_estimated_bytes(&worker), 6);
        worker
            .enqueue_weighted(
                PersistenceKey::named("third"),
                "save third".into(),
                4,
                Box::new(|| Ok(())),
            )
            .unwrap();
        assert_eq!(retained_estimated_bytes(&worker), 10);

        second_release_tx.send(()).unwrap();
        worker.shutdown(Duration::from_secs(1)).unwrap();
        assert_eq!(retained_estimated_bytes(&worker), 0);
    }

    #[test]
    fn retained_result_reservation_stays_charged_until_drop() {
        let worker = PersistenceWorker::new_with_limits(4, 10).unwrap();
        let first = worker.try_reserve_estimated_bytes(6).unwrap();
        let second = worker.try_reserve_estimated_bytes(4).unwrap();
        assert_eq!(first.estimated_bytes(), 6);
        assert_eq!(retained_estimated_bytes(&worker), 10);

        let error = match worker.try_reserve_estimated_bytes(1) {
            Ok(_) => panic!("reservation above the exact limit unexpectedly succeeded"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(retained_estimated_bytes(&worker), 10);

        drop(first);
        assert_eq!(retained_estimated_bytes(&worker), 4);
        drop(second);
        assert_eq!(retained_estimated_bytes(&worker), 0);
        worker.shutdown(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn partial_result_reservation_takes_available_bytes_and_shrinks_to_actual() {
        let worker = PersistenceWorker::new_with_limits(4, 10).unwrap();
        let blocker = worker.try_reserve_estimated_bytes(7).unwrap();
        let mut partial = worker.reserve_estimated_bytes_up_to(10);
        assert_eq!(partial.estimated_bytes(), 3);
        assert_eq!(retained_estimated_bytes(&worker), 10);

        partial.shrink_to(1);
        assert_eq!(partial.estimated_bytes(), 1);
        assert_eq!(retained_estimated_bytes(&worker), 8);
        drop(partial);
        drop(blocker);
        assert_eq!(retained_estimated_bytes(&worker), 0);
        worker.shutdown(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn four_retained_history_results_leave_room_for_one_decoder() {
        let worker = PersistenceWorker::new_with_limits(16, MAX_PENDING_ESTIMATED_BYTES).unwrap();
        let default_result_bytes = 100 * 1024 * 1024;
        let decoder_bytes = 64 * 1024 * 1024;
        let results: Vec<_> = (0..4)
            .map(|_| {
                let transient = worker.try_reserve_estimated_bytes(decoder_bytes).unwrap();
                let mut result = worker.reserve_estimated_bytes_up_to(128 * 1024 * 1024);
                assert_eq!(result.estimated_bytes(), 128 * 1024 * 1024);
                result.shrink_to(default_result_bytes);
                drop(transient);
                result
            })
            .collect();
        let decoder = worker.try_reserve_estimated_bytes(decoder_bytes).unwrap();
        assert_eq!(retained_estimated_bytes(&worker), 464 * 1024 * 1024);

        // Pending load closures retain paths and completion handles only, so
        // multiple panes must remain target-count admitted without each being
        // precharged for a worst-case result.
        for pane in 0..8 {
            worker
                .enqueue(
                    PersistenceKey::named(&format!("pane-{pane}")),
                    "load history".into(),
                    Box::new(|| Ok(())),
                )
                .unwrap();
        }

        drop(decoder);
        drop(results);
        worker.shutdown(Duration::from_secs(1)).unwrap();
        assert_eq!(retained_estimated_bytes(&worker), 0);
    }

    #[test]
    fn shutdown_times_out_while_io_is_stuck_then_flushes_after_release() {
        let worker = PersistenceWorker::new(1).unwrap();
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        worker
            .enqueue(
                PersistenceKey::named("slow"),
                "save session".into(),
                Box::new(move || {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                }),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let error = worker.shutdown(Duration::from_millis(10)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        release_tx.send(()).unwrap();
        worker.shutdown(Duration::from_secs(1)).unwrap();
    }
}
