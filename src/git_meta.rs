//! Coalesced background Git metadata for the active-pane strip.
//!
//! The worker, the probe hardening, and the porcelain parser live in
//! [`jterm_core::git_meta`], shared with the other terminals; this module is
//! only forge's surface for them, plus the non-blocking UI variant below.
//!
//! The GTK strip goes through [`read_cached_and_refresh`]: it reads the last
//! completed value immediately while one app worker calls the shared bounded
//! `read_fresh` endpoint. The frame-budgeted `read` endpoint may return stale
//! data after 12ms, so its answer cannot safely earn this adapter's fresh TTL.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

pub use jterm_core::git_meta::*;

const MAX_GIT_CWD_BYTES: usize = 16 * 1024;
const MAX_QUEUED_PROBES: usize = 64;
const MAX_CACHE_ENTRIES: usize = 256;

/// How long a completed probe is served without asking Git again.
///
/// The bar this feeds repaints once a second, and a read used to queue a probe
/// every single time: an idle window forked one `git status` per second for as
/// long as it stayed open, on a repository nothing was touching. Git state only
/// moves when something in this window runs a command or changes directory, and
/// those call [`invalidate`]; this ceiling exists only so a change made by
/// another window or another terminal is eventually noticed too.
const CACHE_TTL: Duration = Duration::from_secs(30);
/// Failed/ambiguous probes remain stale, but must not run on every UI tick.
const FAILURE_RETRY_BACKOFF: Duration = Duration::from_secs(10);

type ProbeResult = Option<RepoMeta>;

#[derive(Clone)]
struct CacheEntry {
    result: ProbeResult,
    /// When a definitive probe last earned freshness, or `None` if no probe
    /// has answered yet or the last attempt failed. A failed refresh retains
    /// the previous visible result without granting it a new freshness TTL.
    ///
    /// [`UiGitMetaService::invalidate`] creates such an entry so a report about
    /// a directory Git has never been asked about still has somewhere to live.
    /// It is deliberately not `Instant::now()`: that would claim an answer this
    /// entry does not have, and the whole point of the TTL is that a recent
    /// answer may be served without asking again.
    refreshed_at: Option<Instant>,
    /// Failure cooldown, independent from freshness and invalidation debt.
    next_retry_at: Option<Instant>,
    /// How many times this window has reported a change in this directory.
    /// Saturates on exhaustion; new requests at the limit are refused.
    /// [`Self::invalidated`] compares it with the stored answer's generation.
    invalidations: u64,
    /// `invalidations` as it stood when the probe that produced `result` was
    /// queued.
    ///
    /// A probe cannot describe a change reported after Git had already been
    /// asked, so the two only agree once an answer has come back for every
    /// change this window knows about. Writing the flag straight to "not
    /// invalidated" on every completion — which is what this replaced — threw
    /// away exactly the mid-probe reports: on a large or FUSE-backed checkout
    /// a `git status` easily outlives the next command, and the second
    /// command's effect then stayed invisible in the bar for the whole
    /// `CACHE_TTL`.
    probed_at_invalidations: u64,
}

impl CacheEntry {
    /// Something in this window changed the repository since this answer was
    /// asked for, so the next read re-probes regardless of the freshness TTL,
    /// subject to the separate failure cooldown, while showing the old value.
    fn invalidated(&self) -> bool {
        self.invalidations != self.probed_at_invalidations
    }
}

/// One queued probe, carrying the invalidation generation of its path.
///
/// The generation travels with the request rather than being read when the
/// worker writes its answer back, because by then a change reported mid-probe
/// is indistinguishable from one reported before the probe started.
struct ProbeRequest {
    path: PathBuf,
    queued_at_invalidations: u64,
}

/// Whether a read has to queue a fresh probe, or may serve what it has.
///
/// `cached_age` is `None` when nothing has ever been probed for this path.
fn probe_is_due(cached_age: Option<Duration>, invalidated: bool) -> bool {
    invalidated || cached_age.is_none_or(|age| age >= CACHE_TTL)
}

fn cached_probe_is_due(entry: Option<&CacheEntry>, now: Instant) -> bool {
    let Some(entry) = entry else {
        return true;
    };
    entry.next_retry_at.is_none_or(|retry_at| now >= retry_at)
        && probe_is_due(
            entry
                .refreshed_at
                .map(|at| now.saturating_duration_since(at)),
            entry.invalidated(),
        )
}

struct UiGitMetaService {
    request_tx: mpsc::SyncSender<ProbeRequest>,
    cache: Arc<Mutex<HashMap<PathBuf, CacheEntry>>>,
    pending: Arc<Mutex<HashMap<PathBuf, u64>>>,
}

impl UiGitMetaService {
    fn new() -> Option<Self> {
        let (request_tx, request_rx) = mpsc::sync_channel(MAX_QUEUED_PROBES);
        let cache = Arc::new(Mutex::new(HashMap::new()));
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let worker_cache = cache.clone();
        let worker_pending = pending.clone();
        thread::Builder::new()
            .name("forge-ui-git-meta".to_string())
            .spawn(move || worker_loop(request_rx, &worker_cache, &worker_pending))
            .ok()?;
        Some(Self {
            request_tx,
            cache,
            pending,
        })
    }

    fn cached(&self, path: &Path) -> Option<CacheEntry> {
        self.cache.lock().ok()?.get(path).cloned()
    }

    /// Mark this path's cached probe as owing a refresh.
    ///
    /// A path with no entry gets one, holding the report and no answer. It is
    /// tempting to skip that — an unprobed path is already due — but "no entry"
    /// and "no probe in flight" are different things, and the first probe of a
    /// directory is exactly when they come apart: a pane that has just opened
    /// or just changed directory has asked Git and has nothing back yet, and
    /// the worker will create the entry when it answers. Dropping the report
    /// here would let that answer land as if it covered a change it was queued
    /// before ever hearing about, and a cold repository is the slowest probe
    /// there is, so this is the likeliest way to lose one — not the rarest.
    fn invalidate(&self, path: &Path) {
        // Pending identities outlive cache eviction. Lock pending before cache
        // everywhere so completion cannot clear a report in the gap between
        // these two stores. The queue plus its one worker bounds this registry.
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        if let Some(generation) = pending.get_mut(path) {
            *generation = generation.saturating_add(1);
        }
        if let Ok(mut cache) = self.cache.lock() {
            match cache.get_mut(path) {
                Some(entry) => {
                    entry.invalidations = pending
                        .get(path)
                        .copied()
                        .unwrap_or_else(|| entry.invalidations.saturating_add(1));
                }
                None => insert_bounded(
                    &mut cache,
                    path.to_path_buf(),
                    CacheEntry {
                        result: None,
                        refreshed_at: None,
                        next_retry_at: None,
                        invalidations: pending.get(path).copied().unwrap_or(1),
                        probed_at_invalidations: 0,
                    },
                ),
            }
        }
    }

    /// The generation a probe queued right now would be answering for. A path
    /// with no entry has had nothing reported about it, so it starts at zero.
    fn invalidations(&self, path: &Path) -> u64 {
        self.cache
            .lock()
            .ok()
            .and_then(|cache| cache.get(path).map(|entry| entry.invalidations))
            .unwrap_or(0)
    }

    fn request(&self, path: &Path) -> bool {
        let path = path.to_path_buf();
        let queued_at_invalidations;
        {
            let Ok(mut pending) = self.pending.lock() else {
                return false;
            };
            if pending.contains_key(&path) {
                return true;
            }
            queued_at_invalidations = self.invalidations(&path);
            // Never admit a probe at an exhausted generation: an additional
            // invalidation could otherwise compare equal to its completion.
            if queued_at_invalidations == u64::MAX {
                return false;
            }
            pending.insert(path.clone(), queued_at_invalidations);
        }
        if self
            .request_tx
            .try_send(ProbeRequest {
                path: path.clone(),
                queued_at_invalidations,
            })
            .is_ok()
        {
            return true;
        }
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&path);
        }
        false
    }
}

/// Store `entry` under `path`, dropping some other path first once the map is
/// at its ceiling.
///
/// Both writers share this. The cache is keyed by directory and both a probe
/// and a bare report can introduce a key, so a session that walks a large tree
/// would otherwise grow it for the life of the window.
fn insert_bounded(cache: &mut HashMap<PathBuf, CacheEntry>, path: PathBuf, entry: CacheEntry) {
    if !cache.contains_key(&path) && cache.len() >= MAX_CACHE_ENTRIES {
        if let Some(evicted) = cache.keys().next().cloned() {
            cache.remove(&evicted);
        }
    }
    cache.insert(path, entry);
}

fn worker_loop(
    requests: mpsc::Receiver<ProbeRequest>,
    cache: &Mutex<HashMap<PathBuf, CacheEntry>>,
    pending: &Mutex<HashMap<PathBuf, u64>>,
) {
    worker_loop_with_probe(requests, cache, pending, jterm_core::git_meta::read_fresh)
}

fn worker_loop_with_probe(
    requests: mpsc::Receiver<ProbeRequest>,
    cache: &Mutex<HashMap<PathBuf, CacheEntry>>,
    pending: &Mutex<HashMap<PathBuf, u64>>,
    mut probe: impl FnMut(&Path) -> Result<ProbeResult, jterm_core::git_meta::FreshReadError>,
) {
    for ProbeRequest {
        path,
        queued_at_invalidations,
    } in requests
    {
        let result = probe(&path);
        let Ok(mut pending) = pending.lock() else {
            continue;
        };
        // A completion belongs only to its registered single-flight path.
        let Some(invalidations) = pending.get(&path).copied() else {
            continue;
        };
        if let Ok(mut cache) = cache.lock() {
            match result {
                Ok(result) => insert_bounded(
                    &mut cache,
                    path.clone(),
                    CacheEntry {
                        result,
                        refreshed_at: Some(Instant::now()),
                        next_retry_at: None,
                        invalidations,
                        probed_at_invalidations: queued_at_invalidations,
                    },
                ),
                Err(_) => {
                    // Busy admission, timeout and probe failure do not earn a
                    // 30-second TTL and do not erase the last visible answer.
                    let next_retry_at = Some(Instant::now() + FAILURE_RETRY_BACKOFF);
                    if let Some(entry) = cache.get_mut(&path) {
                        entry.refreshed_at = None;
                        entry.next_retry_at = next_retry_at;
                        entry.invalidations = invalidations;
                    } else {
                        insert_bounded(
                            &mut cache,
                            path.clone(),
                            CacheEntry {
                                result: None,
                                refreshed_at: None,
                                next_retry_at,
                                invalidations,
                                probed_at_invalidations: queued_at_invalidations,
                            },
                        );
                    }
                }
            }
        }
        pending.remove(&path);
    }
}

fn service() -> Option<&'static UiGitMetaService> {
    static SERVICE: OnceLock<Option<UiGitMetaService>> = OnceLock::new();
    SERVICE.get_or_init(UiGitMetaService::new).as_ref()
}

fn cwd_key_is_bounded(cwd: &Path) -> bool {
    let bytes = cwd.as_os_str().as_encoded_bytes();
    bytes.len() <= MAX_GIT_CWD_BYTES && !bytes.contains(&0)
}

/// Return the last completed probe immediately, and schedule a coalesced
/// refresh only when one is actually due.
///
/// This is the UI-strip variant of [`read`]. A cache hit must not spend the
/// caller's frame budget waiting for a newer Git process: the worker updates
/// the shared cache, and the next ordinary UI refresh observes it. It must not
/// fork one either: a completed probe is served for `CACHE_TTL`, and only a
/// change this window itself made ([`invalidate`]) cuts that short.
pub fn read_cached_and_refresh(cwd: &Path) -> Option<RepoMeta> {
    // Do not stat the path on the GTK thread: a FUSE/remote mount can make
    // even `is_dir` miss a frame. The worker-side blocking reader validates it.
    if !cwd_key_is_bounded(cwd) {
        return None;
    }
    let service = service()?;
    let cached = service.cached(cwd);
    let due = cached_probe_is_due(cached.as_ref(), Instant::now());
    if due {
        let _ = service.request(cwd);
    }
    cached.and_then(|entry| entry.result)
}

/// Report that this window did something that can move Git's answer, so the
/// next read re-probes instead of waiting out the TTL.
pub fn invalidate(cwd: &Path) {
    if !cwd_key_is_bounded(cwd) {
        return;
    }
    if let Some(service) = service() {
        service.invalidate(cwd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_requests_coalesce_without_waiting_for_a_reply() {
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let service = UiGitMetaService {
            request_tx,
            cache: Arc::new(Mutex::new(HashMap::new())),
            pending: Arc::new(Mutex::new(HashMap::new())),
        };
        let path = Path::new("/work/repo");

        assert!(service.request(path));
        assert!(service.request(path));
        assert_eq!(request_rx.try_iter().count(), 1);
        assert_eq!(service.pending.lock().unwrap().len(), 1);
    }

    /// The bar behind this cache repaints once a second. A fresh entry must
    /// answer from memory; only a real change in this window, or a TTL long
    /// enough that idle polling is negligible, may fork Git again.
    #[test]
    fn a_fresh_entry_is_served_without_forking_git_again() {
        assert!(probe_is_due(None, false), "nothing probed yet");
        assert!(!probe_is_due(Some(Duration::ZERO), false));
        assert!(!probe_is_due(
            Some(CACHE_TTL - Duration::from_millis(1)),
            false
        ));
        assert!(probe_is_due(Some(CACHE_TTL), false));

        // A command finished, or the pane moved: the answer is owed now, not
        // in thirty seconds.
        assert!(probe_is_due(Some(Duration::ZERO), true));

        // One second of idle polling against a thirty-second ceiling: the bar
        // asks CACHE_TTL/1s times and Git runs once.
        let polls = CACHE_TTL.as_secs();
        let forked = (0..polls)
            .filter(|second| probe_is_due(Some(Duration::from_secs(*second)), false))
            .count();
        assert_eq!(
            forked, 0,
            "an idle window must not fork git while its answer is fresh"
        );
    }

    #[test]
    fn an_invalidated_entry_is_reprobed_but_still_answers() {
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let service = UiGitMetaService {
            request_tx,
            cache: Arc::new(Mutex::new(HashMap::new())),
            pending: Arc::new(Mutex::new(HashMap::new())),
        };
        let path = Path::new("/work/repo");
        service.cache.lock().unwrap().insert(
            path.to_path_buf(),
            CacheEntry {
                result: None,
                refreshed_at: Some(Instant::now()),
                next_retry_at: None,
                invalidations: 0,
                probed_at_invalidations: 0,
            },
        );

        service.invalidate(path);
        let entry = service.cached(path).expect("the previous answer survives");
        assert!(entry.invalidated());
        assert!(probe_is_due(
            entry.refreshed_at.map(|at| at.elapsed()),
            entry.invalidated()
        ));

        // The request that answers it is queued for the generation the
        // invalidation created, not the one before it.
        assert!(service.request(path));
        let queued = request_rx.try_recv().expect("a probe was queued");
        assert_eq!(queued.queued_at_invalidations, 1);

        // Invalidating a path nobody has probed records the report against an
        // entry that holds no answer, so a probe already in flight for it
        // cannot land as though it covered the change. Reporting still never
        // asks Git itself — only a read does.
        let never_probed = Path::new("/work/never-probed");
        service.invalidate(never_probed);
        let entry = service.cached(never_probed).expect("the report is kept");
        assert!(entry.result.is_none(), "no answer is invented for it");
        assert!(entry.refreshed_at.is_none(), "and none is claimed");
        assert!(entry.invalidated());
        assert!(probe_is_due(
            entry.refreshed_at.map(|at| at.elapsed()),
            entry.invalidated()
        ));
        assert_eq!(request_rx.try_iter().count(), 0);
    }

    /// A finished probe answers for the generation it was queued at, and for
    /// no later one. A command that changes Git state while `git status` is
    /// still running is the ordinary way two generations end up in flight at
    /// once — a slow or FUSE-backed checkout plus two commands in a row — and
    /// the report it filed must survive the answer that predates it.
    #[test]
    fn a_probe_that_raced_an_invalidation_leaves_the_entry_still_owing_one() {
        // A synthetic completion keeps this bookkeeping test filesystem- and
        // process-free; production uses the same completion path.
        let path = PathBuf::from("/fixture/repo");
        let cache = Mutex::new(HashMap::new());
        let pending = Mutex::new(HashMap::new());
        cache.lock().unwrap().insert(
            path.clone(),
            CacheEntry {
                result: None,
                refreshed_at: Some(Instant::now() - CACHE_TTL),
                next_retry_at: None,
                // Two commands have finished; the probe in flight was queued
                // after the first one and knows nothing of the second.
                invalidations: 2,
                probed_at_invalidations: 0,
            },
        );
        pending.lock().unwrap().insert(path.clone(), 2);

        let (request_tx, request_rx) = mpsc::sync_channel(1);
        request_tx
            .send(ProbeRequest {
                path: path.clone(),
                queued_at_invalidations: 1,
            })
            .expect("the queue takes one request");
        drop(request_tx);
        worker_loop_with_probe(request_rx, &cache, &pending, |_| Ok(None));

        let entry = cache.lock().unwrap().get(&path).cloned().expect("answered");
        assert!(
            entry.invalidated(),
            "the second command's change was reported after this probe started"
        );
        assert!(probe_is_due(
            entry.refreshed_at.map(|at| at.elapsed()),
            entry.invalidated()
        ));
        assert!(
            pending.lock().unwrap().is_empty(),
            "the completed probe stops coalescing further requests"
        );

        // The re-probe that follows is queued for the newer generation, and
        // clears the debt when nothing moves under it.
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        pending.lock().unwrap().insert(path.clone(), 2);
        request_tx
            .send(ProbeRequest {
                path: path.clone(),
                queued_at_invalidations: 2,
            })
            .expect("the queue takes one request");
        drop(request_tx);
        worker_loop_with_probe(request_rx, &cache, &pending, |_| Ok(None));

        let entry = cache.lock().unwrap().get(&path).cloned().expect("answered");
        assert!(
            !entry.invalidated(),
            "an answer covering every reported change is served for the whole TTL"
        );
        assert!(!probe_is_due(
            entry.refreshed_at.map(|at| at.elapsed()),
            entry.invalidated()
        ));
    }

    /// The same race on the first probe of a directory, which is the one a
    /// pane that has just opened or just changed directory is always in. There
    /// is no entry to mark then — the worker creates it when Git answers — so
    /// a report that only marks existing entries is dropped precisely when the
    /// answer about to land is the one that has to hear it. A cold repository
    /// is also the slowest probe there is, which makes this the likeliest
    /// instance of the race rather than an exotic one.
    #[test]
    fn a_first_probe_that_raced_an_invalidation_leaves_the_entry_still_owing_one() {
        // A synthetic completion keeps this bookkeeping test filesystem- and
        // process-free; production uses the same completion path.
        let path = PathBuf::from("/fixture/repo");
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let service = UiGitMetaService {
            request_tx,
            cache: Arc::new(Mutex::new(HashMap::new())),
            pending: Arc::new(Mutex::new(HashMap::new())),
        };

        // The bar's first read of a pane that has just opened: nothing cached,
        // so a probe goes out for generation zero.
        assert!(service.request(&path));
        assert!(
            service.cached(&path).is_none(),
            "the entry does not exist until Git answers"
        );

        // Git is still running when the user's command finishes.
        service.invalidate(&path);

        let queued = request_rx.try_recv().expect("a probe was queued");
        assert_eq!(queued.queued_at_invalidations, 0);
        let (worker_tx, worker_rx) = mpsc::sync_channel(1);
        worker_tx.send(queued).expect("the queue takes one request");
        drop(worker_tx);
        worker_loop_with_probe(worker_rx, &service.cache, &service.pending, |_| Ok(None));

        let entry = service.cached(&path).expect("answered");
        assert!(
            entry.invalidated(),
            "the report arrived after this probe was queued and must outlive it"
        );
        assert!(probe_is_due(
            entry.refreshed_at.map(|at| at.elapsed()),
            entry.invalidated()
        ));
    }

    /// A report may now introduce a cache key, and a window that walks a large
    /// tree reports one per command in one directory after another. The
    /// ceiling that bounds the probe path has to bound this one too, or the
    /// map grows for the life of the window.
    #[test]
    fn reports_about_directories_nobody_probed_stay_within_the_cache_ceiling() {
        let (request_tx, _request_rx) = mpsc::sync_channel(1);
        let service = UiGitMetaService {
            request_tx,
            cache: Arc::new(Mutex::new(HashMap::new())),
            pending: Arc::new(Mutex::new(HashMap::new())),
        };

        for index in 0..MAX_CACHE_ENTRIES * 2 {
            service.invalidate(Path::new(&format!("/work/repo-{index}")));
        }

        assert_eq!(service.cache.lock().unwrap().len(), MAX_CACHE_ENTRIES);
        // Re-reporting a directory already in the map replaces nothing, so the
        // ceiling is not a reason to forget a change that is still pending.
        let survivor = service
            .cache
            .lock()
            .unwrap()
            .keys()
            .next()
            .cloned()
            .expect("the ceiling is not zero");
        service.invalidate(&survivor);
        assert_eq!(service.cache.lock().unwrap().len(), MAX_CACHE_ENTRIES);
        assert_eq!(service.cached(&survivor).unwrap().invalidations, 2);
    }

    /// The cache is keyed by directory, which is what makes invalidating the
    /// wrong pane's directory a thirty-second-long lie rather than a wasted
    /// probe: nothing about marking one path reaches another.
    #[test]
    fn invalidating_one_directory_leaves_every_other_one_untouched() {
        let (request_tx, _request_rx) = mpsc::sync_channel(4);
        let service = UiGitMetaService {
            request_tx,
            cache: Arc::new(Mutex::new(HashMap::new())),
            pending: Arc::new(Mutex::new(HashMap::new())),
        };
        let ran_the_command = Path::new("/work/anvil");
        let merely_focused = Path::new("/work/forge");
        for path in [ran_the_command, merely_focused] {
            service.cache.lock().unwrap().insert(
                path.to_path_buf(),
                CacheEntry {
                    result: None,
                    refreshed_at: Some(Instant::now()),
                    next_retry_at: None,
                    invalidations: 0,
                    probed_at_invalidations: 0,
                },
            );
        }

        service.invalidate(merely_focused);

        assert!(!service.cached(ran_the_command).unwrap().invalidated());
        assert!(!probe_is_due(
            Some(Duration::ZERO),
            service.cached(ran_the_command).unwrap().invalidated()
        ));
        assert!(service.cached(merely_focused).unwrap().invalidated());
    }

    #[test]
    fn cache_keys_are_bounded_before_queueing() {
        assert!(cwd_key_is_bounded(Path::new("/work/repo")));
        assert!(!cwd_key_is_bounded(Path::new(
            &"x".repeat(MAX_GIT_CWD_BYTES + 1)
        )));

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            assert!(!cwd_key_is_bounded(Path::new(std::ffi::OsStr::from_bytes(
                b"bad\0path",
            ))));
        }
    }
}

#[cfg(test)]
mod adapter_cache_regressions {
    use super::*;
    fn service() -> (UiGitMetaService, mpsc::Receiver<ProbeRequest>) {
        let (request_tx, requests) = mpsc::sync_channel(4);
        (
            UiGitMetaService {
                request_tx,
                cache: Arc::new(Mutex::new(HashMap::new())),
                pending: Arc::new(Mutex::new(HashMap::new())),
            },
            requests,
        )
    }
    fn run_queued(
        service: &UiGitMetaService,
        request: ProbeRequest,
        result: Result<ProbeResult, FreshReadError>,
    ) {
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send(request).unwrap();
        drop(tx);
        worker_loop_with_probe(rx, &service.cache, &service.pending, |_| result.clone());
    }
    #[test]
    fn in_flight_invalidation_survives_actual_bounded_eviction() {
        let (service, requests) = service();
        for index in 0..MAX_CACHE_ENTRIES {
            service.invalidate(Path::new(&format!("/fixture/{index}")));
        }
        let victim = service.cache.lock().unwrap().keys().next().unwrap().clone();
        assert!(service.request(&victim));
        let request = requests.try_recv().unwrap();
        service.invalidate(&victim);
        service.invalidate(Path::new("/fixture/forces-eviction"));
        assert!(!service.cache.lock().unwrap().contains_key(&victim));
        run_queued(&service, request, Ok(None));
        assert!(service.cached(&victim).unwrap().invalidated());
        assert!(service.pending.lock().unwrap().is_empty());
        assert_eq!(service.cache.lock().unwrap().len(), MAX_CACHE_ENTRIES);
    }
    #[test]
    fn unavailable_or_timed_out_probe_never_earns_fresh_ttl_or_erases_previous_value() {
        for error in [FreshReadError::Unavailable, FreshReadError::Timeout] {
            let (service, requests) = service();
            let path = Path::new("/fixture/slow");
            assert!(service.request(path));
            run_queued(&service, requests.try_recv().unwrap(), Err(error));
            let cold = service
                .cached(path)
                .expect("a bounded failure cooldown is retained");
            assert!(cold.result.is_none());
            assert!(cold.refreshed_at.is_none());
            assert!(!cached_probe_is_due(Some(&cold), Instant::now()));
            let previous = Some(RepoMeta {
                branch: "previous".into(),
                dirty: true,
                ahead: None,
                behind: None,
            });
            insert_bounded(
                &mut service.cache.lock().unwrap(),
                path.into(),
                CacheEntry {
                    result: previous.clone(),
                    refreshed_at: Some(Instant::now()),
                    next_retry_at: None,
                    invalidations: 0,
                    probed_at_invalidations: 0,
                },
            );
            service.invalidate(path);
            assert!(service.request(path));
            run_queued(&service, requests.try_recv().unwrap(), Err(error));
            let entry = service.cached(path).unwrap();
            assert_eq!(entry.result, previous);
            assert!(entry.refreshed_at.is_none());
            assert!(entry.invalidated());
            assert!(service.pending.lock().unwrap().is_empty());
        }
    }
    #[test]
    fn definitive_completed_absence_earns_ttl_but_unregistered_completion_cannot_publish() {
        let (service, requests) = service();
        let path = Path::new("/fixture/non-directory");
        assert!(service.request(path));
        run_queued(&service, requests.try_recv().unwrap(), Ok(None));
        assert!(service.cached(path).unwrap().refreshed_at.is_some());
        let absent = PathBuf::from("/fixture/unregistered");
        run_queued(
            &service,
            ProbeRequest {
                path: absent.clone(),
                queued_at_invalidations: 0,
            },
            Ok(None),
        );
        assert!(service.cached(&absent).is_none());
    }
    #[test]
    fn generation_exhaustion_retains_debt_and_refuses_new_requests() {
        let (service, requests) = service();
        let path = Path::new("/fixture/exhausted");
        insert_bounded(
            &mut service.cache.lock().unwrap(),
            path.into(),
            CacheEntry {
                result: None,
                refreshed_at: None,
                next_retry_at: None,
                invalidations: u64::MAX - 1,
                probed_at_invalidations: u64::MAX - 1,
            },
        );
        assert!(service.request(path));
        service.invalidate(path);
        service.invalidate(path);
        run_queued(&service, requests.try_recv().unwrap(), Ok(None));
        let entry = service.cached(path).unwrap();
        assert_eq!(entry.invalidations, u64::MAX);
        assert!(entry.invalidated());
        assert!(!service.request(path));
        assert!(service.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn failed_or_full_request_admission_leaves_no_unbounded_identity() {
        let (tx, _rx) = mpsc::sync_channel(1);
        let service = UiGitMetaService {
            request_tx: tx,
            cache: Arc::new(Mutex::new(HashMap::new())),
            pending: Arc::new(Mutex::new(HashMap::new())),
        };
        assert!(service.request(Path::new("/fixture/one")));
        for index in 0..100 {
            assert!(!service.request(Path::new(&format!("/fixture/full/{index}"))));
        }
        assert_eq!(service.pending.lock().unwrap().len(), 1);
    }
    #[test]
    fn failures_obey_a_separate_bounded_retry_cooldown_even_after_invalidation() {
        let (service, requests) = service();
        let path = Path::new("/fixture/backoff");
        assert!(service.request(path));
        run_queued(
            &service,
            requests.try_recv().unwrap(),
            Err(FreshReadError::Unavailable),
        );
        let failed = service.cached(path).unwrap();
        let retry_at = failed.next_retry_at.unwrap();
        assert_eq!(FAILURE_RETRY_BACKOFF, Duration::from_secs(10));
        assert!(failed.refreshed_at.is_none());
        assert!(!cached_probe_is_due(
            Some(&failed),
            retry_at - Duration::from_nanos(1)
        ));
        assert!(cached_probe_is_due(Some(&failed), retry_at));
        service.invalidate(path);
        let changed = service.cached(path).unwrap();
        assert!(changed.invalidated());
        assert_eq!(changed.next_retry_at, Some(retry_at));
        assert!(!cached_probe_is_due(
            Some(&changed),
            retry_at - Duration::from_nanos(1)
        ));
        assert!(service.request(path));
        run_queued(&service, requests.try_recv().unwrap(), Ok(None));
        let completed = service.cached(path).unwrap();
        assert!(completed.refreshed_at.is_some());
        assert!(completed.next_retry_at.is_none());
        assert!(!completed.invalidated());
    }
}
