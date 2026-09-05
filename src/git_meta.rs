//! Coalesced background Git metadata for the active-pane strip.
//!
//! The worker, the probe hardening, and the porcelain parser live in
//! [`jterm_core::git_meta`], shared with the other terminals; this module is
//! only forge's surface for them, plus the non-blocking UI variant below.
//!
//! `jterm_core::git_meta::read` already runs Git in a bounded worker, but waits
//! briefly for that worker so command-line/background callers can receive a
//! fresh answer. A 12ms wait is still most of a 60Hz frame, so the GTK strip
//! goes through [`read_cached_and_refresh`]: it reads the last completed value
//! immediately while one app worker performs the possibly-waiting shared call.

use std::collections::{HashMap, HashSet};
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

type ProbeResult = Option<RepoMeta>;

#[derive(Clone)]
struct CacheEntry {
    result: ProbeResult,
    /// When the probe that produced `result` finished, or `None` for an entry
    /// no probe has answered for yet.
    ///
    /// [`UiGitMetaService::invalidate`] creates such an entry so a report about
    /// a directory Git has never been asked about still has somewhere to live.
    /// It is deliberately not `Instant::now()`: that would claim an answer this
    /// entry does not have, and the whole point of the TTL is that a recent
    /// answer may be served without asking again.
    refreshed_at: Option<Instant>,
    /// How many times this window has reported a change in this directory.
    /// Only ever grows; [`Self::invalidated`] reads it against the generation
    /// the stored answer was asked for.
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
    /// asked for, so the next read re-probes regardless of the TTL — while
    /// still showing this value, so the bar does not blank out waiting for Git.
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

struct UiGitMetaService {
    request_tx: mpsc::SyncSender<ProbeRequest>,
    cache: Arc<Mutex<HashMap<PathBuf, CacheEntry>>>,
    pending: Arc<Mutex<HashSet<PathBuf>>>,
}

impl UiGitMetaService {
    fn new() -> Option<Self> {
        let (request_tx, request_rx) = mpsc::sync_channel(MAX_QUEUED_PROBES);
        let cache = Arc::new(Mutex::new(HashMap::new()));
        let pending = Arc::new(Mutex::new(HashSet::new()));
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
        if let Ok(mut cache) = self.cache.lock() {
            match cache.get_mut(path) {
                Some(entry) => entry.invalidations = entry.invalidations.saturating_add(1),
                None => insert_bounded(
                    &mut cache,
                    path.to_path_buf(),
                    CacheEntry {
                        result: None,
                        refreshed_at: None,
                        invalidations: 1,
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
        {
            let Ok(mut pending) = self.pending.lock() else {
                return false;
            };
            if !pending.insert(path.clone()) {
                return true;
            }
        }
        // Read the generation as late as possible, but still before the worker
        // can start Git. A report that lands in the gap costs one redundant
        // probe, which is the safe direction: the alternative is an answer
        // that silently claims to cover a change it was never asked about.
        let queued_at_invalidations = self.invalidations(&path);
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
    pending: &Mutex<HashSet<PathBuf>>,
) {
    for ProbeRequest {
        path,
        queued_at_invalidations,
    } in requests
    {
        let result = jterm_core::git_meta::read(&path);
        if let Ok(mut cache) = cache.lock() {
            // Carry the running count forward instead of resetting it: an
            // `invalidate` that arrived while Git was running has already
            // pushed it past the generation this probe was asked for, and that
            // gap is the entry's only memory of a change this answer predates.
            // The entry it reads may be one `invalidate` created for exactly
            // that purpose, which is why a report never needs an answer to
            // survive.
            let invalidations = cache
                .get(&path)
                .map_or(queued_at_invalidations, |entry| entry.invalidations);
            insert_bounded(
                &mut cache,
                path.clone(),
                CacheEntry {
                    result,
                    refreshed_at: Some(Instant::now()),
                    invalidations,
                    probed_at_invalidations: queued_at_invalidations,
                },
            );
        }
        if let Ok(mut pending) = pending.lock() {
            pending.remove(&path);
        }
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
    let due = probe_is_due(
        cached
            .as_ref()
            .and_then(|entry| entry.refreshed_at.map(|at| at.elapsed())),
        cached.as_ref().is_some_and(CacheEntry::invalidated),
    );
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
            pending: Arc::new(Mutex::new(HashSet::new())),
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
            pending: Arc::new(Mutex::new(HashSet::new())),
        };
        let path = Path::new("/work/repo");
        service.cache.lock().unwrap().insert(
            path.to_path_buf(),
            CacheEntry {
                result: None,
                refreshed_at: Some(Instant::now()),
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
        // Never a directory, so the shared reader answers `None` without
        // forking Git; this test is about the bookkeeping around the answer.
        let path = PathBuf::from("/proc/self/exe/not-a-directory");
        let cache = Mutex::new(HashMap::new());
        let pending = Mutex::new(HashSet::new());
        cache.lock().unwrap().insert(
            path.clone(),
            CacheEntry {
                result: None,
                refreshed_at: Some(Instant::now() - CACHE_TTL),
                // Two commands have finished; the probe in flight was queued
                // after the first one and knows nothing of the second.
                invalidations: 2,
                probed_at_invalidations: 0,
            },
        );
        pending.lock().unwrap().insert(path.clone());

        let (request_tx, request_rx) = mpsc::sync_channel(1);
        request_tx
            .send(ProbeRequest {
                path: path.clone(),
                queued_at_invalidations: 1,
            })
            .expect("the queue takes one request");
        drop(request_tx);
        worker_loop(request_rx, &cache, &pending);

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
        request_tx
            .send(ProbeRequest {
                path: path.clone(),
                queued_at_invalidations: 2,
            })
            .expect("the queue takes one request");
        drop(request_tx);
        worker_loop(request_rx, &cache, &pending);

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
        // Never a directory, so the shared reader answers `None` without
        // forking Git; this test is about the bookkeeping around the answer.
        let path = PathBuf::from("/proc/self/exe/not-a-directory");
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let service = UiGitMetaService {
            request_tx,
            cache: Arc::new(Mutex::new(HashMap::new())),
            pending: Arc::new(Mutex::new(HashSet::new())),
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
        worker_loop(worker_rx, &service.cache, &service.pending);

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
            pending: Arc::new(Mutex::new(HashSet::new())),
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
            pending: Arc::new(Mutex::new(HashSet::new())),
        };
        let ran_the_command = Path::new("/work/anvil");
        let merely_focused = Path::new("/work/forge");
        for path in [ran_the_command, merely_focused] {
            service.cache.lock().unwrap().insert(
                path.to_path_buf(),
                CacheEntry {
                    result: None,
                    refreshed_at: Some(Instant::now()),
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
