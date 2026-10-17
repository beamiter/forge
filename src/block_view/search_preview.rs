//! Exact navigation in an immutable, bounded copy of retained record text.
//! This module never reads VTE or makes claims about terminal selection.
use std::ops::Range;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::{Duration, Instant};

pub(crate) const TEXT_LIMIT: usize = 1024 * 1024;
pub(crate) const QUERY_LIMIT: usize = 8 * 1024;
const DOCUMENT_LIMIT: usize = 4096;
const HIT_LIMIT: usize = 10_000;
const REGEX_LIMIT: usize = 256 * 1024;
const LINE_LIMIT: usize = 64 * 1024;
const SCAN_TIME: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Field {
    Prompt,
    Command,
    Output,
}
impl Field {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Prompt => "Prompt",
            Self::Command => "Command",
            Self::Output => "Output",
        }
    }
}
#[derive(Debug)]
pub(crate) struct Document {
    pub(crate) record_id: u64,
    pub(crate) field: Field,
    pub(crate) text: String,
    pub(crate) retained_only: bool,
}
// Keep ids non-repeating even when a callback outlives its producer. A CAS
// loop works on older stable toolchains as well as current Rust.
fn next_generation(counter: &AtomicU64) -> Option<u64> {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let next = current.checked_add(1)?;
        match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Some(next),
            Err(actual) => current = actual,
        }
    }
}
static SNAPSHOT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Default)]
pub(crate) struct Snapshot {
    pub(crate) id: u64,
    pub(crate) documents: Vec<Document>,
    pub(crate) missing_prompts: usize,
    pub(crate) missing_outputs: usize,
    raw_bytes: usize,
    display_bytes: usize,
    failed: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreviewError {
    QueryLimit,
    TextLimit,
    MatchLimit,
    LineLimit,
    TimeLimit,
    InvalidRegex,
    ZeroWidth,
    Cancelled,
    WorkerUnavailable,
}
impl PreviewError {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::QueryLimit => "Query exceeds the 8 KiB preview limit.",
            Self::TextLimit => "Preview unavailable: retained text exceeds the snapshot budget (1 MiB text / 4096 fields).",
            Self::LineLimit => "Preview incomplete: a logical line exceeds 64 KiB. Visual wrapping does not split logical lines.",
            Self::MatchLimit => "Preview incomplete: more than 10000 matches. Narrow the query.",
            Self::TimeLimit => "Preview incomplete: scan time limit reached. Narrow the query.",
            Self::InvalidRegex => "Unsupported or invalid Rust regex (including the regex size limit).",
            Self::ZeroWidth => "Preview does not support patterns that can match empty text.",
            Self::Cancelled => "Search changed. Waiting for the current query…",
            Self::WorkerUnavailable => "Preview search worker is unavailable. Close and reopen the preview.",
        }
    }
}

impl Snapshot {
    pub(crate) fn new() -> Result<Self, PreviewError> {
        let id = next_generation(&SNAPSHOT_ID).ok_or(PreviewError::WorkerUnavailable)?;
        Ok(Self {
            id,
            ..Self::default()
        })
    }
    /// Check the source before copying it. Display escaping has a separate
    /// aggregate ceiling, checked before each append. No terminal replay or
    /// unbounded intermediate conversion is involved.
    pub(crate) fn push(
        &mut self,
        record_id: u64,
        field: Field,
        source: &str,
        retained_only: bool,
    ) -> Result<(), PreviewError> {
        if self.failed {
            return Err(PreviewError::TextLimit);
        }
        let result = self.push_bounded(record_id, field, source, retained_only);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn push_bounded(
        &mut self,
        record_id: u64,
        field: Field,
        source: &str,
        retained_only: bool,
    ) -> Result<(), PreviewError> {
        let raw_bytes = self
            .raw_bytes
            .checked_add(source.len())
            .ok_or(PreviewError::TextLimit)?;
        if raw_bytes > TEXT_LIMIT || self.documents.len() >= DOCUMENT_LIMIT {
            return Err(PreviewError::TextLimit);
        }
        let mut text = String::new();
        for ch in source.chars() {
            // Make controls and bidi formatting visible. Search is explicitly
            // over this displayed representation, not a terminal emulation.
            if (ch.is_control() && ch != '\n' && ch != '\t')
                || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{061c}')
            {
                for escaped in ch.escape_unicode() {
                    self.append_display(&mut text, escaped)?;
                }
            } else {
                self.append_display(&mut text, ch)?;
            }
        }
        self.raw_bytes = raw_bytes;
        self.documents.push(Document {
            record_id,
            field,
            text,
            retained_only,
        });
        Ok(())
    }
    fn append_display(&mut self, text: &mut String, ch: char) -> Result<(), PreviewError> {
        let next = self
            .display_bytes
            .checked_add(ch.len_utf8())
            .ok_or(PreviewError::TextLimit)?;
        if next > TEXT_LIMIT {
            return Err(PreviewError::TextLimit);
        }
        text.push(ch);
        self.display_bytes = next;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    pub(crate) document: usize,
    pub(crate) bytes: Range<usize>,
    /// GTK TextIter offsets count Unicode scalar values, not UTF-8 or cells.
    pub(crate) chars: Range<i32>,
}
#[derive(Debug, Default)]
pub(crate) struct Matches {
    pub(crate) hits: Vec<Hit>,
}

/// Line-local, non-overlapping Rust regex matches over the exact displayed
/// snapshot. Hard LF boundaries cannot be crossed even by \s or [^x]. Soft
/// wrapping in the TextView does not alter this text. No full offset map is
/// allocated: monotonic match endpoints are converted in a linear pass and
/// the two offsets are charged to each bounded Hit's fixed-size metadata.
pub(crate) fn search(
    snapshot: &Snapshot,
    query: &str,
    use_regex: bool,
    mut cancelled: impl FnMut() -> bool,
) -> Result<Matches, PreviewError> {
    if query.len() > QUERY_LIMIT {
        return Err(PreviewError::QueryLimit);
    }
    if snapshot.failed {
        return Err(PreviewError::TextLimit);
    }
    if query.is_empty() {
        return Ok(Matches::default());
    }
    let started = Instant::now();
    if cancelled() {
        return Err(PreviewError::Cancelled);
    }
    let pattern = if use_regex {
        query.to_owned()
    } else {
        regex::escape(query)
    };
    let hir = regex_syntax::parse(&pattern).map_err(|_| PreviewError::InvalidRegex)?;
    if hir.properties().minimum_len() == Some(0) {
        return Err(PreviewError::ZeroWidth);
    }
    let re = regex::RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .multi_line(true)
        .size_limit(REGEX_LIMIT)
        .dfa_size_limit(REGEX_LIMIT)
        .build()
        .map_err(|_| PreviewError::InvalidRegex)?;
    let mut matches = Matches::default();
    let mut check = || {
        if cancelled() {
            Err(PreviewError::Cancelled)
        } else if started.elapsed() > SCAN_TIME {
            Err(PreviewError::TimeLimit)
        } else {
            Ok(())
        }
    };
    check()?;
    for (document, doc) in snapshot.documents.iter().enumerate() {
        let mut byte_base = 0;
        let mut char_base = 0;
        for line_with_lf in doc.text.split_inclusive('\n') {
            check()?;
            let line = line_with_lf.strip_suffix('\n').unwrap_or(line_with_lf);
            if line.len() > LINE_LIMIT {
                return Err(PreviewError::LineLimit);
            }
            let mut previous_byte = 0;
            let mut previous_char = char_base;
            for found in re.find_iter(line) {
                check()?;
                if matches.hits.len() == HIT_LIMIT {
                    return Err(PreviewError::MatchLimit);
                }
                let start_char =
                    previous_char + line[previous_byte..found.start()].chars().count() as i32;
                let end_char = start_char + line[found.start()..found.end()].chars().count() as i32;
                matches.hits.push(Hit {
                    document,
                    bytes: byte_base + found.start()..byte_base + found.end(),
                    chars: start_char..end_char,
                });
                previous_byte = found.end();
                previous_char = end_char;
            }
            char_base = previous_char + line_with_lf[previous_byte..].chars().count() as i32;
            byte_base += line_with_lf.len();
            check()?;
        }
    }
    check()?;
    Ok(matches)
}

pub(crate) fn step(current: usize, total: usize, backwards: bool) -> Option<usize> {
    if total == 0 || current >= total {
        return None;
    }
    Some(if backwards {
        if current == 0 {
            total - 1
        } else {
            current - 1
        }
    } else {
        (current + 1) % total
    })
}

struct Request {
    generation: u64,
    query: String,
    use_regex: bool,
}
pub(crate) struct Reply {
    pub(crate) snapshot_id: u64,
    pub(crate) generation: u64,
    pub(crate) result: Result<Matches, PreviewError>,
}
/// One worker and one latest-request slot per dialog. Rapid typing cannot
/// create an unbounded worker queue or accumulate obsolete result vectors.
pub(crate) struct Worker {
    generation: Arc<AtomicU64>,
    stopped: Arc<AtomicBool>,
    request: Arc<Mutex<Option<Request>>>,
    reply: Arc<Mutex<Option<Reply>>>,
    wake: mpsc::SyncSender<()>,
    #[cfg(test)]
    publication_gate: Arc<Mutex<Option<PublicationGate>>>,
    #[cfg(test)]
    exited: Arc<AtomicBool>,
}
#[cfg(test)]
struct PublicationGate {
    reached: mpsc::SyncSender<()>,
    release: mpsc::Receiver<()>,
    published: mpsc::SyncSender<()>,
    finish: mpsc::Receiver<()>,
}
#[cfg(test)]
pub(crate) type PublicationHandshake = (
    mpsc::Receiver<()>,
    mpsc::SyncSender<()>,
    mpsc::Receiver<()>,
    mpsc::SyncSender<()>,
);
impl Worker {
    pub(crate) fn new(snapshot: Arc<Snapshot>) -> Result<Self, PreviewError> {
        let generation = Arc::new(AtomicU64::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let request = Arc::new(Mutex::new(None::<Request>));
        let reply = Arc::new(Mutex::new(None));
        let (wake, receive) = mpsc::sync_channel(1);
        let (generation_thread, stopped_thread, request_thread, reply_thread) = (
            generation.clone(),
            stopped.clone(),
            request.clone(),
            reply.clone(),
        );
        #[cfg(test)]
        let publication_gate = Arc::new(Mutex::new(None::<PublicationGate>));
        #[cfg(test)]
        let publication_gate_thread = publication_gate.clone();
        #[cfg(test)]
        let exited = Arc::new(AtomicBool::new(false));
        #[cfg(test)]
        let exited_thread = exited.clone();
        std::thread::Builder::new()
            .name("search-preview".into())
            .spawn(move || {
                while receive.recv().is_ok() {
                    if stopped_thread.load(Ordering::Acquire) {
                        break;
                    }
                    let Some(request) = request_thread.lock().expect("preview request").take()
                    else {
                        continue;
                    };
                    let result = search(&snapshot, &request.query, request.use_regex, || {
                        stopped_thread.load(Ordering::Acquire)
                            || generation_thread.load(Ordering::Acquire) != request.generation
                    });
                    if !stopped_thread.load(Ordering::Acquire)
                        && generation_thread.load(Ordering::Acquire) == request.generation
                    {
                        // Test the real check-to-store race: newer UI state may
                        // arrive after validation but before this publication.
                        #[cfg(test)]
                        let gate = publication_gate_thread.lock().unwrap().take();
                        #[cfg(test)]
                        if let Some(gate) = &gate {
                            let _ = gate.reached.send(());
                            let _ = gate.release.recv_timeout(Duration::from_secs(5));
                        }
                        *reply_thread.lock().expect("preview reply") = Some(Reply {
                            snapshot_id: snapshot.id,
                            generation: request.generation,
                            result,
                        });
                        #[cfg(test)]
                        if let Some(gate) = gate {
                            let _ = gate.published.send(());
                            let _ = gate.finish.recv_timeout(Duration::from_secs(5));
                        }
                    }
                }
                #[cfg(test)]
                exited_thread.store(true, Ordering::Release);
            })
            .map_err(|_| PreviewError::WorkerUnavailable)?;
        Ok(Self {
            generation,
            stopped,
            request,
            reply,
            wake,
            #[cfg(test)]
            publication_gate,
            #[cfg(test)]
            exited,
        })
    }
    pub(crate) fn submit(&self, query: &str, use_regex: bool) -> Result<u64, PreviewError> {
        let generation =
            next_generation(&self.generation).ok_or(PreviewError::WorkerUnavailable)?;
        *self.reply.lock().expect("preview reply") = None;
        *self.request.lock().expect("preview request") = None;
        if self.stopped.load(Ordering::Acquire) {
            return Err(PreviewError::WorkerUnavailable);
        }
        if query.len() > QUERY_LIMIT {
            return Err(PreviewError::QueryLimit);
        }
        *self.request.lock().expect("preview request") = Some(Request {
            generation,
            query: query.to_owned(),
            use_regex,
        });
        match self.wake.try_send(()) {
            Ok(()) | Err(mpsc::TrySendError::Full(())) => Ok(generation),
            Err(mpsc::TrySendError::Disconnected(())) => Err(PreviewError::WorkerUnavailable),
        }
    }
    #[cfg(test)]
    pub(crate) fn has_exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }
    #[cfg(test)]
    pub(crate) fn pause_next_publication(&self) -> PublicationHandshake {
        let (reached, arrived) = mpsc::sync_channel(1);
        let (release, wait) = mpsc::sync_channel(1);
        let (published, stored) = mpsc::sync_channel(1);
        let (finish, resume) = mpsc::sync_channel(1);
        *self.publication_gate.lock().unwrap() = Some(PublicationGate {
            reached,
            release: wait,
            published,
            finish: resume,
        });
        (arrived, release, stored, finish)
    }
    pub(crate) fn take_reply(&self) -> Option<Reply> {
        self.reply.lock().expect("preview reply").take()
    }
    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        let _ = self.wake.try_send(());
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) fn reply_is_current(
    reply: &Reply,
    snapshot_id: u64,
    generation: u64,
    owner_present: bool,
) -> bool {
    owner_present && reply.snapshot_id == snapshot_id && reply.generation == generation
}

#[cfg(test)]
#[path = "search_preview_tests.rs"]
mod tests;
