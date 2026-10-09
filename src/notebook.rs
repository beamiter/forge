//! Executable `.jtnb.md` notebooks for the native GTK4 application.
//!
//! A notebook is ordinary Markdown with fenced shell cells. Shell source is
//! executed in an isolated child process and never injected into a live terminal.
//! Explicit fences (`bash`, `sh`, `zsh`, `fish`, `pwsh`) select that interpreter;
//! `shell` and unlabeled fences use the caller-provided shell argv verbatim.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use adw::prelude::*;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

/// Maximum combined stdout/stderr retained for one cell run.
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_NOTEBOOK_FILE_BYTES: u64 = 1024 * 1024;
const MAX_NOTEBOOK_SEGMENTS: usize = 512;
const MAX_NOTEBOOK_CELLS: usize = 128;
const MAX_NOTEBOOK_TEXT_BYTES: usize = 256 * 1024;
const MAX_NOTEBOOK_CELL_SOURCE_BYTES: usize = 256 * 1024;
const OUTPUT_POLL_INTERVAL: Duration = Duration::from_millis(40);
const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// A cell emitting output faster than GTK can paint yields the poll tick
/// after this many events instead of draining the whole queue in one go, so
/// the UI thread stays responsive under an output flood.
const MAX_OUTPUT_EVENTS_PER_TICK: usize = 32;
/// Process-wide ceiling on simultaneously running cell workers, so a "run
/// all" over many cells cannot spawn an unbounded set of interpreters.
const MAX_CONCURRENT_CELL_WORKERS: usize = 8;
static ACTIVE_CELL_WORKERS: AtomicI32 = AtomicI32::new(0);

/// RAII slot in the cell-worker concurrency ceiling: held by the worker
/// thread for its whole lifetime, released automatically when it ends.
struct CellWorkerPermit;

impl CellWorkerPermit {
    fn acquire() -> Option<Self> {
        let mut active = ACTIVE_CELL_WORKERS.load(Ordering::Acquire);
        loop {
            if active >= MAX_CONCURRENT_CELL_WORKERS as i32 {
                return None;
            }
            match ACTIVE_CELL_WORKERS.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Self),
                Err(observed) => active = observed,
            }
        }
    }
}

impl Drop for CellWorkerPermit {
    fn drop(&mut self) {
        ACTIVE_CELL_WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

pub use jterm_core::notebook_text::{parse_segments, render_text_to_pango, Segment};

pub fn is_notebook_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".jtnb.md"))
}

fn read_notebook_bounded(path: &Path) -> std::io::Result<String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "notebook source is not a regular file",
        ));
    }
    if metadata.len() > MAX_NOTEBOOK_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            format!("notebook exceeds the {MAX_NOTEBOOK_FILE_BYTES}-byte limit"),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_NOTEBOOK_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_NOTEBOOK_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            format!("notebook exceeds the {MAX_NOTEBOOK_FILE_BYTES}-byte limit"),
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "notebook source is not valid UTF-8",
        )
    })
}

fn bounded_utf8_prefix(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn safe_notebook_display(value: &str, max_bytes: usize) -> String {
    let value = bounded_utf8_prefix(value, max_bytes);
    let mut safe = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\n' | '\t') {
            safe.push(ch);
        } else if ch.is_control() || jterm_core::review_input::is_visual_spoofing_character(ch) {
            safe.push('\u{fffd}');
        } else {
            safe.push(ch);
        }
    }
    safe
}

fn safe_notebook_inline(value: &str, max_bytes: usize) -> String {
    safe_notebook_display(value, max_bytes)
        .chars()
        .map(|ch| {
            if matches!(ch, '\n' | '\t') {
                '\u{fffd}'
            } else {
                ch
            }
        })
        .collect()
}

fn notebook_cell_source_is_safe(source: &str) -> bool {
    source.len() <= MAX_NOTEBOOK_CELL_SOURCE_BYTES
        && !source
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t'))
        && !jterm_core::review_input::contains_noncontrol_visual_spoofing(source)
}

#[derive(Debug)]
pub enum NotebookError {
    InvalidExtension(PathBuf),
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for NotebookError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidExtension(path) => {
                write!(formatter, "not a .jtnb.md notebook: {}", path.display())
            }
            Self::Read { path, source } => {
                write!(
                    formatter,
                    "could not read notebook {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for NotebookError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::InvalidExtension(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
struct CommandSpec {
    argv: Vec<String>,
    source: String,
    cwd: PathBuf,
}

fn language_name(info: &str) -> &str {
    info.split_whitespace().next().unwrap_or("")
}

fn shell_argv_for_info(info: &str, configured_shell: &[String]) -> Option<Vec<String>> {
    let language = language_name(info).to_ascii_lowercase();
    match language.as_str() {
        "" | "shell" => (!configured_shell.is_empty()).then(|| configured_shell.to_vec()),
        "bash" | "sh" | "zsh" | "fish" => Some(vec![language]),
        "pwsh" => Some(vec![
            "pwsh".to_owned(),
            "-NoLogo".to_owned(),
            "-NonInteractive".to_owned(),
            "-Command".to_owned(),
            "-".to_owned(),
        ]),
        "powershell" => Some(vec![
            "powershell".to_owned(),
            "-NoLogo".to_owned(),
            "-NonInteractive".to_owned(),
            "-Command".to_owned(),
            "-".to_owned(),
        ]),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CellOutcome {
    Exited(i32),
    Cancelled,
    Failed(String),
}

impl CellOutcome {
    fn failed(&self) -> bool {
        !matches!(self, Self::Exited(0))
    }
}

enum WorkerEvent {
    /// The child has been spawned; carries its process-group id. Emitted once,
    /// before any output, so an observer can read the group over the ordered
    /// channel without racing the worker resetting `pgid` to 0 on completion.
    Started(i32),
    Output(OutputStream, Vec<u8>),
    Done(CellOutcome),
}

struct CellHandle {
    child: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<AtomicBool>,
    /// Diagnostic telemetry only. Signals require the owned child under its
    /// mutex; this cached number must never authorize a signal after reaping.
    pgid: Arc<AtomicI32>,
}

impl CellHandle {
    fn new() -> Self {
        Self {
            child: Arc::new(Mutex::new(None)),
            cancelled: Arc::new(AtomicBool::new(false)),
            pgid: Arc::new(AtomicI32::new(0)),
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Ok(mut guard) = self.child.lock() {
            if let Some(child) = guard.as_mut() {
                terminate_child_group(child);
            }
        }
    }
}

fn signal_process_group(pgid: i32) {
    #[cfg(unix)]
    if pgid > 0 {
        // SAFETY: a negative PID targets the process group created for this
        // cell. Failure is harmless when the group has already exited.
        unsafe {
            nix::libc::kill(-pgid, nix::libc::SIGKILL);
        }
    }
}

fn terminate_child_group(child: &mut Child) {
    #[cfg(unix)]
    {
        if let Ok(pid) = i32::try_from(child.id()) {
            // Each notebook cell is placed in its own process group. Killing the
            // group prevents descendants such as `sleep` from surviving Stop or
            // dialog close after the shell itself exits.
            unsafe {
                nix::libc::kill(-pid, nix::libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
}

fn wait_for_shared_child(child_slot: &Arc<Mutex<Option<Child>>>) -> std::io::Result<i32> {
    loop {
        let result = {
            let mut guard = child_slot
                .lock()
                .map_err(|_| std::io::Error::other("child handle mutex poisoned"))?;
            let child = guard
                .as_mut()
                .ok_or_else(|| std::io::Error::other("child handle missing before exit"))?;
            let pid = i32::try_from(child.id())
                .map_err(|_| std::io::Error::other("child pid does not fit i32"))?;
            let flags = nix::sys::wait::WaitPidFlag::WEXITED
                | nix::sys::wait::WaitPidFlag::WNOHANG
                | nix::sys::wait::WaitPidFlag::WNOWAIT;
            let observed = nix::sys::wait::waitid(
                nix::sys::wait::Id::Pid(nix::unistd::Pid::from_raw(pid)),
                flags,
            );
            match observed {
                Ok(
                    nix::sys::wait::WaitStatus::Exited(..)
                    | nix::sys::wait::WaitStatus::Signaled(..),
                ) => {
                    // Keep the leader unreaped until its group is terminated.
                    // Cancellation takes this same mutex and cannot use a PID
                    // between reaping it and clearing the ownership slot.
                    signal_process_group(pid);
                    let status = child.wait();
                    guard.take();
                    Some(status?.code().unwrap_or(-1))
                }
                Ok(_) | Err(nix::errno::Errno::EINTR) => None,
                Err(error) => {
                    if error == nix::errno::Errno::ECHILD {
                        // An external reaper already released this identity.
                        // Forget it without any last signal to its old number.
                        guard.take();
                    }
                    return Err(std::io::Error::from_raw_os_error(error as i32));
                }
            }
        };
        if let Some(code) = result {
            return Ok(code);
        }
        // Never retain the ownership lock while sleeping: Stop must stay live.
        std::thread::sleep(CHILD_POLL_INTERVAL);
    }
}

fn fail_cell_io_setup(
    child_slot: &Arc<Mutex<Option<Child>>>,
    pgid: &Arc<AtomicI32>,
    sender: &mpsc::SyncSender<WorkerEvent>,
    threads: Vec<std::thread::JoinHandle<()>>,
    error: io::Error,
    pipes: &CellPipeControl,
) {
    pipes.retire();
    if let Ok(mut guard) = child_slot.lock() {
        if let Some(child) = guard.as_mut() {
            terminate_child_group(child);
        }
    }
    let _ = wait_for_shared_child(child_slot);
    for thread in threads {
        let _ = thread.join();
    }
    pgid.store(0, Ordering::SeqCst);
    let _ = sender.send(WorkerEvent::Done(CellOutcome::Failed(format!(
        "I/O worker thread spawn failed: {error}"
    ))));
}

/// Escaped sessions can retain our pipe ends after the owned group exits.
/// Retire only our I/O, allowing a short final drain without signalling any
/// process outside that group. Running cells have no artificial time limit.
#[derive(Clone, Default)]
struct CellPipeControl {
    retired: Arc<Mutex<Option<std::time::Instant>>>,
    deferred_output: Arc<Mutex<[Vec<u8>; 2]>>,
}

impl CellPipeControl {
    fn retire(&self) {
        self.retired
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get_or_insert_with(std::time::Instant::now);
    }

    fn check(&self) -> io::Result<()> {
        if self
            .retired
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some_and(|when| when.elapsed() >= Duration::from_millis(100))
        {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "notebook pipe remained open after cell exit",
            ))
        } else {
            Ok(())
        }
    }

    fn wrap<T: std::os::fd::AsRawFd>(&self, inner: T) -> io::Result<CellPipe<T>> {
        let fd = inner.as_raw_fd();
        // SAFETY: `inner` continues to own fd; only its file status changes.
        let flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
        if flags < 0
            || unsafe { nix::libc::fcntl(fd, nix::libc::F_SETFL, flags | nix::libc::O_NONBLOCK) }
                < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(CellPipe {
            inner,
            control: self.clone(),
        })
    }

    fn send_output(
        &self,
        sender: &mpsc::SyncSender<WorkerEvent>,
        mut event: WorkerEvent,
    ) -> Result<(), WorkerEvent> {
        loop {
            if self.check().is_err() {
                return Err(event);
            }
            match sender.try_send(event) {
                Ok(()) => return Ok(()),
                Err(mpsc::TrySendError::Disconnected(pending)) => return Err(pending),
                Err(mpsc::TrySendError::Full(pending)) => event = pending,
            }
            // Backpressure is bounded by the same retirement deadline as I/O.
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn send_tail(&self, sender: &mpsc::SyncSender<WorkerEvent>) {
        // All I/O threads have joined and closed their descriptors first.
        // Delivering events may wait for the UI, just as the final Done does.
        let tails = std::mem::take(
            &mut *self
                .deferred_output
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        );
        for (stream, bytes) in [OutputStream::Stdout, OutputStream::Stderr]
            .into_iter()
            .zip(tails)
        {
            if !bytes.is_empty() && sender.send(WorkerEvent::Output(stream, bytes)).is_err() {
                break;
            }
        }
    }
}

struct CellPipe<T> {
    inner: T,
    control: CellPipeControl,
}

impl<T: std::os::fd::AsRawFd> CellPipe<T> {
    fn wait_ready(&self, events: nix::libc::c_short) -> io::Result<()> {
        self.control.check()?;
        let mut poll = nix::libc::pollfd {
            fd: self.inner.as_raw_fd(),
            events,
            revents: 0,
        };
        // SAFETY: poll points to one live descriptor still owned by `inner`.
        let result = unsafe { nix::libc::poll(&mut poll, 1, 10) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        self.control.check()
    }
}

impl<T: Read + std::os::fd::AsRawFd> Read for CellPipe<T> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            self.control.check()?;
            match self.inner.read(buffer) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.wait_ready(nix::libc::POLLIN)?
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                result => return result,
            }
        }
    }
}

impl<T: Write + std::os::fd::AsRawFd> Write for CellPipe<T> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            self.control.check()?;
            match self.inner.write(buffer) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.wait_ready(nix::libc::POLLOUT)?
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn stream_cell_output<T: Read + std::os::fd::AsRawFd>(
    mut output: CellPipe<T>,
    stream: OutputStream,
    sender: mpsc::SyncSender<WorkerEvent>,
) {
    let mut buffer = [0u8; 4096];
    let tail = loop {
        match output.read(&mut buffer) {
            Ok(0) => return,
            Err(error) if error.kind() == io::ErrorKind::TimedOut => break Vec::new(),
            Err(_) => return,
            Ok(count) => {
                match output.control.send_output(
                    &sender,
                    WorkerEvent::Output(stream, buffer[..count].to_vec()),
                ) {
                    Ok(()) => {}
                    Err(WorkerEvent::Output(_, bytes)) if output.control.check().is_err() => {
                        break bytes
                    }
                    Err(_) => return,
                }
            }
        }
    };
    // A busy UI must not lose the root's already-buffered tail just because
    // its channel was full during retirement. Drain the currently available
    // nonblocking bytes into a bounded final event, then close the descriptor.
    // One byte beyond the display cap preserves its truncation notification.
    let mut tail = tail;
    while tail.len() <= MAX_OUTPUT_BYTES {
        let limit = buffer.len().min(MAX_OUTPUT_BYTES + 1 - tail.len());
        match output.inner.read(&mut buffer[..limit]) {
            Ok(0) | Err(_) => break,
            Ok(count) => tail.extend_from_slice(&buffer[..count]),
        }
    }
    let index = match stream {
        OutputStream::Stdout => 0,
        OutputStream::Stderr => 1,
    };
    output
        .control
        .deferred_output
        .lock()
        .unwrap_or_else(|error| error.into_inner())[index] = tail;
}

const NON_UTF8_FLATPAK_CWD_ERROR: &str =
    "Notebook working directory contains non-UTF-8 bytes; Flatpak cannot pass it to the host safely.";

fn host_bridge_cwd(cwd: &Path, host_bridge: bool) -> Result<Option<String>, &'static str> {
    if !host_bridge {
        return Ok(None);
    }
    cwd.to_str()
        .map(|cwd| Some(cwd.to_owned()))
        .ok_or(NON_UTF8_FLATPAK_CWD_ERROR)
}

#[derive(Default)]
struct CellIoSpawner {
    // Per-worker fault injection exercises setup failure without exhausting
    // system thread resources or changing another concurrently running cell.
    #[cfg(test)]
    fail_at: Option<usize>,
    #[cfg(test)]
    attempts: usize,
}

impl CellIoSpawner {
    fn spawn<F>(&mut self, name: &str, task: F) -> io::Result<std::thread::JoinHandle<()>>
    where
        F: FnOnce() + Send + 'static,
    {
        #[cfg(test)]
        {
            self.attempts += 1;
            if self.fail_at == Some(self.attempts) {
                return Err(io::Error::other(
                    "injected notebook I/O thread spawn failure",
                ));
            }
        }
        std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(task)
    }
}

fn spawn_cell_worker(spec: CommandSpec, handle: &CellHandle) -> mpsc::Receiver<WorkerEvent> {
    spawn_cell_worker_with_io_spawner(spec, handle, CellIoSpawner::default())
}

fn spawn_cell_worker_with_io_spawner(
    spec: CommandSpec,
    handle: &CellHandle,
    mut io_spawner: CellIoSpawner,
) -> mpsc::Receiver<WorkerEvent> {
    // Bound queued output as well as the rendered buffers: a command that
    // writes faster than GTK can paint applies backpressure instead of growing
    // an unbounded cross-thread queue.
    let (sender, receiver) = mpsc::sync_channel(64);
    let child_slot = handle.child.clone();
    let cancelled = handle.cancelled.clone();
    let pgid = handle.pgid.clone();
    let host_bridge = crate::host::is_flatpak();
    let cwd_for_bridge = match host_bridge_cwd(&spec.cwd, host_bridge) {
        Ok(cwd) => cwd,
        Err(error) => {
            let _ = sender.try_send(WorkerEvent::Done(CellOutcome::Failed(error.to_owned())));
            return receiver;
        }
    };

    // Refuse to pile up workers past the concurrency ceiling; the cell fails
    // immediately with a clear reason instead of queueing silently.
    let Some(permit) = CellWorkerPermit::acquire() else {
        let _ = sender.try_send(WorkerEvent::Done(CellOutcome::Failed(format!(
            "at most {MAX_CONCURRENT_CELL_WORKERS} notebook cells may run concurrently"
        ))));
        return receiver;
    };

    let spawn_failure_sender = sender.clone();
    let spawn = std::thread::Builder::new()
        .name("forge-notebook-cell".to_owned())
        .spawn(move || {
            let _permit = permit;
            let executable_argv =
                crate::host::wrap_argv(&spec.argv, cwd_for_bridge.as_deref(), &[]);
            let Some((program, arguments)) = executable_argv.split_first() else {
                let _ = sender.send(WorkerEvent::Done(CellOutcome::Failed(
                    "no shell executable configured".to_owned(),
                )));
                return;
            };

            let mut command = Command::new(program);
            command
                .args(arguments)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if !host_bridge {
                command.current_dir(&spec.cwd);
            }
            #[cfg(unix)]
            command.process_group(0);

            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(error) => {
                    let _ = sender.send(WorkerEvent::Done(CellOutcome::Failed(format!(
                        "spawn failed: {error}"
                    ))));
                    return;
                }
            };
            if let Ok(id) = i32::try_from(child.id()) {
                pgid.store(id, Ordering::SeqCst);
                let _ = sender.send(WorkerEvent::Started(id));
            }

            let pipes = CellPipeControl::default();
            let io = (|| {
                let (Some(stdin), Some(stdout), Some(stderr)) =
                    (child.stdin.take(), child.stdout.take(), child.stderr.take())
                else {
                    return Err(io::Error::other(
                        "spawned shell did not expose all requested pipes",
                    ));
                };
                Ok((pipes.wrap(stdin)?, pipes.wrap(stdout)?, pipes.wrap(stderr)?))
            })();
            let (mut stdin, stdout, stderr) = match io {
                Ok(pipes) => pipes,
                Err(error) => {
                    terminate_child_group(&mut child);
                    let _ = child.wait();
                    pgid.store(0, Ordering::SeqCst);
                    let _ = sender.send(WorkerEvent::Done(CellOutcome::Failed(format!(
                        "pipe setup failed: {error}"
                    ))));
                    return;
                }
            };
            match child_slot.lock() {
                Ok(mut guard) => {
                    *guard = Some(child);
                    if cancelled.load(Ordering::SeqCst) {
                        if let Some(child) = guard.as_mut() {
                            terminate_child_group(child);
                        }
                    }
                }
                Err(_) => {
                    terminate_child_group(&mut child);
                    let _ = child.wait();
                    let _ = sender.send(WorkerEvent::Done(CellOutcome::Failed(
                        "child handle mutex poisoned".to_owned(),
                    )));
                    return;
                }
            }

            let mut io_threads = Vec::with_capacity(3);
            let source = spec.source;
            match io_spawner.spawn("forge-notebook-stdin", move || {
                let _ = stdin.write_all(source.as_bytes());
                if !source.ends_with('\n') {
                    let _ = stdin.write_all(b"\n");
                }
            }) {
                Ok(thread) => io_threads.push(thread),
                Err(error) => {
                    fail_cell_io_setup(&child_slot, &pgid, &sender, io_threads, error, &pipes);
                    return;
                }
            }
            let stdout_sender = sender.clone();
            match io_spawner.spawn("forge-notebook-stdout", move || {
                stream_cell_output(stdout, OutputStream::Stdout, stdout_sender);
            }) {
                Ok(thread) => io_threads.push(thread),
                Err(error) => {
                    fail_cell_io_setup(&child_slot, &pgid, &sender, io_threads, error, &pipes);
                    return;
                }
            }
            let stderr_sender = sender.clone();
            match io_spawner.spawn("forge-notebook-stderr", move || {
                stream_cell_output(stderr, OutputStream::Stderr, stderr_sender);
            }) {
                Ok(thread) => io_threads.push(thread),
                Err(error) => {
                    fail_cell_io_setup(&child_slot, &pgid, &sender, io_threads, error, &pipes);
                    return;
                }
            }

            let exit = wait_for_shared_child(&child_slot);
            // Group cleanup/reaping remains solely with the wait owner. Escaped
            // sessions may keep descriptors, so independently bound our I/O joins.
            pipes.retire();
            for thread in io_threads {
                let _ = thread.join();
            }
            pipes.send_tail(&sender);

            let outcome = match exit {
                Ok(_) if cancelled.load(Ordering::SeqCst) => CellOutcome::Cancelled,
                Ok(code) => CellOutcome::Exited(code),
                Err(error) => CellOutcome::Failed(format!("wait failed: {error}")),
            };
            pgid.store(0, Ordering::SeqCst);
            let _ = sender.send(WorkerEvent::Done(outcome));
        });
    if let Err(error) = spawn {
        let _ = spawn_failure_sender.try_send(WorkerEvent::Done(CellOutcome::Failed(format!(
            "worker thread spawn failed: {error}"
        ))));
    }

    receiver
}

fn current_cell_handle(active: &RefCell<Option<Rc<CellHandle>>>, handle: &Rc<CellHandle>) -> bool {
    active
        .borrow()
        .as_ref()
        .is_some_and(|current| Rc::ptr_eq(current, handle))
}

/// Decode each pipe independently: a read boundary is not a UTF-8 boundary.
/// Only an incomplete trailing code point survives between output events.
struct CellOutputCapture {
    remaining: usize,
    pending: [Vec<u8>; 2],
    truncated: bool,
}

impl CellOutputCapture {
    fn new(limit: usize) -> Self {
        Self {
            remaining: limit,
            pending: Default::default(),
            truncated: false,
        }
    }

    fn append(&mut self, stream: OutputStream, bytes: &[u8]) -> (String, bool) {
        let count = bytes.len().min(self.remaining);
        self.remaining -= count;
        let first_truncation = count < bytes.len() && !self.truncated;
        self.truncated |= count < bytes.len();
        let pending = &mut self.pending[match stream {
            OutputStream::Stdout => 0,
            OutputStream::Stderr => 1,
        }];
        pending.extend_from_slice(&bytes[..count]);
        let mut text = String::new();
        loop {
            match std::str::from_utf8(pending) {
                Ok(valid) => {
                    text.push_str(valid);
                    pending.clear();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    text.push_str(
                        std::str::from_utf8(&pending[..valid]).expect("validated prefix"),
                    );
                    if let Some(invalid) = error.error_len() {
                        text.push('\u{fffd}');
                        pending.drain(..valid + invalid);
                    } else {
                        pending.drain(..valid);
                        break;
                    }
                }
            }
        }
        (text, first_truncation)
    }

    fn finish(&mut self) -> [String; 2] {
        self.pending.each_mut().map(|pending| {
            let text = String::from_utf8_lossy(pending).into_owned();
            pending.clear();
            text
        })
    }
}

struct OutputPane {
    root: gtk4::Box,
    buffer: gtk4::TextBuffer,
    scroll: gtk4::ScrolledWindow,
}

impl OutputPane {
    fn new(title: &str, is_error: bool) -> Self {
        let root = gtk4::Box::new(gtk4::Orientation::Vertical, 3);
        root.set_visible(false);

        let label = gtk4::Label::new(Some(title));
        label.set_xalign(0.0);
        label.add_css_class("dim-label");
        if is_error {
            label.add_css_class("error");
        }
        root.append(&label);

        let buffer = gtk4::TextBuffer::new(None);
        let view = gtk4::TextView::with_buffer(&buffer);
        view.set_editable(false);
        view.set_cursor_visible(false);
        view.set_monospace(true);
        view.set_wrap_mode(gtk4::WrapMode::WordChar);
        view.add_css_class("notebook-output");
        let scroll = gtk4::ScrolledWindow::builder()
            .hexpand(true)
            .max_content_height(260)
            .child(&view)
            .build();
        scroll.set_propagate_natural_height(true);
        root.append(&scroll);

        Self {
            root,
            buffer,
            scroll,
        }
    }

    fn clear(&self) {
        self.buffer.set_text("");
        self.root.set_visible(false);
    }

    fn append(&self, text: &str) {
        self.root.set_visible(true);
        let mut end = self.buffer.end_iter();
        self.buffer.insert(&mut end, text);
        let adjustment = self.scroll.vadjustment();
        adjustment.set_value(adjustment.upper());
    }
}

type Completion = Box<dyn FnOnce(CellOutcome)>;

struct CellController {
    index: usize,
    frame: gtk4::Frame,
    command: Option<CommandSpec>,
    run_button: gtk4::Button,
    stop_button: gtk4::Button,
    stdout: OutputPane,
    stderr: OutputPane,
    status: gtk4::Label,
    active: RefCell<Option<Rc<CellHandle>>>,
    externally_locked: Cell<bool>,
}

impl CellController {
    fn new(
        index: usize,
        info: &str,
        source: &str,
        configured_shell: &[String],
        cwd: &Path,
    ) -> Rc<Self> {
        let argv = shell_argv_for_info(info, configured_shell);
        let source_is_safe = notebook_cell_source_is_safe(source);
        let command = source_is_safe
            .then_some(argv)
            .flatten()
            .map(|argv| CommandSpec {
                argv,
                source: source.to_owned(),
                cwd: cwd.to_path_buf(),
            });

        let frame = gtk4::Frame::new(None);
        frame.add_css_class("card");
        let body = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
        body.set_margin_top(8);
        body.set_margin_bottom(8);
        body.set_margin_start(8);
        body.set_margin_end(8);

        let toolbar = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        let language = language_name(info);
        let language = if language.is_empty() {
            "shell"
        } else {
            language
        };
        let language_display = safe_notebook_display(language, 128);
        let language_label = gtk4::Label::new(Some(&language_display));
        language_label.set_xalign(0.0);
        language_label.set_hexpand(true);
        language_label.add_css_class("dim-label");
        toolbar.append(&language_label);

        let copy_button = gtk4::Button::with_label("Copy");
        copy_button.add_css_class("flat");
        let source_for_copy = source.to_owned();
        copy_button.connect_clicked(move |_| {
            if let Some(display) = gtk4::gdk::Display::default() {
                display.clipboard().set_text(&source_for_copy);
            }
        });
        toolbar.append(&copy_button);

        let run_button = gtk4::Button::with_label("Run");
        let stop_button = gtk4::Button::with_label("Stop");
        stop_button.set_sensitive(false);
        if command.is_some() {
            run_button.add_css_class("suggested-action");
        } else if !source_is_safe {
            run_button.set_sensitive(false);
            run_button.set_tooltip_text(Some(
                "Cell execution is disabled because its source is oversized or contains hidden/control text",
            ));
        } else {
            run_button.set_sensitive(false);
            run_button.set_tooltip_text(Some(
                "Only shell fences are executable; use bash, sh, zsh, fish, pwsh, shell, or no label",
            ));
        }
        toolbar.append(&run_button);
        toolbar.append(&stop_button);
        body.append(&toolbar);

        let source_buffer = gtk4::TextBuffer::new(None);
        source_buffer.set_text(&safe_notebook_display(
            source,
            MAX_NOTEBOOK_CELL_SOURCE_BYTES,
        ));
        let source_view = gtk4::TextView::with_buffer(&source_buffer);
        source_view.set_editable(false);
        source_view.set_cursor_visible(false);
        source_view.set_monospace(true);
        source_view.set_wrap_mode(gtk4::WrapMode::None);
        source_view.add_css_class("notebook-source");
        let source_scroll = gtk4::ScrolledWindow::builder()
            .hexpand(true)
            .max_content_height(220)
            .child(&source_view)
            .build();
        source_scroll.set_propagate_natural_height(true);
        body.append(&source_scroll);

        let stdout = OutputPane::new("stdout", false);
        body.append(&stdout.root);
        let stderr = OutputPane::new("stderr", true);
        body.append(&stderr.root);

        let status = gtk4::Label::new(None);
        status.set_xalign(0.0);
        status.add_css_class("dim-label");
        status.set_visible(false);
        body.append(&status);
        frame.set_child(Some(&body));

        let cell = Rc::new(Self {
            index,
            frame,
            command,
            run_button,
            stop_button,
            stdout,
            stderr,
            status,
            active: RefCell::new(None),
            externally_locked: Cell::new(false),
        });

        let weak = Rc::downgrade(&cell);
        cell.run_button.connect_clicked(move |_| {
            if let Some(cell) = weak.upgrade() {
                let _ = cell.run(None);
            }
        });
        let weak = Rc::downgrade(&cell);
        cell.stop_button.connect_clicked(move |_| {
            if let Some(cell) = weak.upgrade() {
                cell.cancel();
            }
        });
        cell
    }

    fn runnable(&self) -> bool {
        self.command.is_some()
    }

    fn is_running(&self) -> bool {
        self.active.borrow().is_some()
    }

    fn set_external_lock(&self, locked: bool) {
        self.externally_locked.set(locked);
        self.sync_buttons();
    }

    fn sync_buttons(&self) {
        let running = self.is_running();
        self.run_button
            .set_sensitive(self.runnable() && !running && !self.externally_locked.get());
        self.stop_button.set_sensitive(running);
    }

    fn cancel(&self) {
        if let Some(handle) = self.active.borrow().as_ref() {
            handle.cancel();
            self.status.set_text("Cancelling…");
            self.stop_button.set_sensitive(false);
        }
    }

    fn append_output(&self, stream: OutputStream, text: &str) {
        match stream {
            OutputStream::Stdout => self.stdout.append(text),
            OutputStream::Stderr => self.stderr.append(text),
        }
    }

    fn finish(&self, outcome: &CellOutcome) {
        self.status.remove_css_class("error");
        self.status.remove_css_class("warning");
        match outcome {
            CellOutcome::Exited(code) => {
                self.status.set_text(&format!("exit {code}"));
                if *code != 0 {
                    self.status.add_css_class("error");
                }
            }
            CellOutcome::Cancelled => {
                self.status.set_text("cancelled");
                self.status.add_css_class("warning");
            }
            CellOutcome::Failed(error) => {
                self.status.set_text(&format!("failed: {error}"));
                self.status.add_css_class("error");
            }
        }
        self.sync_buttons();
    }

    fn run(self: &Rc<Self>, completion: Option<Completion>) -> bool {
        let Some(command) = self.command.clone() else {
            return false;
        };
        if self.is_running() {
            return false;
        }

        self.stdout.clear();
        self.stderr.clear();
        self.status.set_visible(true);
        self.status.set_text("Running…");
        self.status.remove_css_class("error");
        self.status.remove_css_class("warning");

        let handle = Rc::new(CellHandle::new());
        *self.active.borrow_mut() = Some(handle.clone());
        self.sync_buttons();
        let receiver = spawn_cell_worker(command, &handle);
        let weak_cell = Rc::downgrade(self);
        let mut completion = completion;
        let mut output = CellOutputCapture::new(MAX_OUTPUT_BYTES);

        glib::timeout_add_local(OUTPUT_POLL_INTERVAL, move || {
            let Some(cell) = weak_cell.upgrade() else {
                handle.cancel();
                return glib::ControlFlow::Break;
            };
            // An obsolete timer must not write into or finish a replacement run.
            if !current_cell_handle(&cell.active, &handle) {
                handle.cancel();
                return glib::ControlFlow::Break;
            }

            // Cap the events drained per tick: an output flood defers the
            // rest to the next tick instead of starving the main loop.
            let mut processed = 0usize;
            loop {
                match receiver.try_recv() {
                    Ok(WorkerEvent::Started(_)) => {}
                    Ok(WorkerEvent::Output(stream, bytes)) => {
                        let (text, first_truncation) = output.append(stream, &bytes);
                        if !text.is_empty() {
                            cell.append_output(stream, &text);
                        }
                        if first_truncation {
                            cell.stderr.append("\n[output truncated]\n");
                        }
                        processed += 1;
                        if processed >= MAX_OUTPUT_EVENTS_PER_TICK {
                            return glib::ControlFlow::Continue;
                        }
                    }
                    Ok(WorkerEvent::Done(outcome)) => {
                        let [stdout, stderr] = output.finish();
                        if !stdout.is_empty() {
                            cell.stdout.append(&stdout);
                        }
                        if !stderr.is_empty() {
                            cell.stderr.append(&stderr);
                        }
                        let is_current = cell
                            .active
                            .borrow()
                            .as_ref()
                            .is_some_and(|active| Rc::ptr_eq(active, &handle));
                        if is_current {
                            cell.active.borrow_mut().take();
                        }
                        cell.finish(&outcome);
                        if let Some(callback) = completion.take() {
                            callback(outcome);
                        }
                        return glib::ControlFlow::Break;
                    }
                    Err(mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        let [stdout, stderr] = output.finish();
                        if !stdout.is_empty() {
                            cell.stdout.append(&stdout);
                        }
                        if !stderr.is_empty() {
                            cell.stderr.append(&stderr);
                        }
                        let outcome = CellOutcome::Failed("worker disconnected".to_owned());
                        cell.active.borrow_mut().take();
                        cell.finish(&outcome);
                        if let Some(callback) = completion.take() {
                            callback(outcome);
                        }
                        return glib::ControlFlow::Break;
                    }
                }
            }
        });
        true
    }
}

#[derive(Default)]
struct RunAllStats {
    total: usize,
    finished: usize,
    failed: usize,
}

struct NotebookRuntime {
    cells: Vec<Rc<CellController>>,
    queue: RefCell<VecDeque<usize>>,
    run_all_active: Cell<bool>,
    closed: Cell<bool>,
    stats: RefCell<RunAllStats>,
    run_all_button: gtk4::Button,
    stop_all_button: gtk4::Button,
    status: gtk4::Label,
}

impl NotebookRuntime {
    fn start_run_all(self: &Rc<Self>) {
        if self.closed.get() || self.run_all_active.get() {
            return;
        }
        if self.cells.iter().any(|cell| cell.is_running()) {
            self.status
                .set_text("Wait for individually running cells, or stop them first.");
            self.status.add_css_class("warning");
            self.status.set_visible(true);
            return;
        }

        let queue: VecDeque<usize> = self
            .cells
            .iter()
            .enumerate()
            .filter_map(|(index, cell)| cell.runnable().then_some(index))
            .collect();
        if queue.is_empty() {
            self.status.set_text("No runnable shell cells.");
            self.status.set_visible(true);
            return;
        }

        *self.stats.borrow_mut() = RunAllStats {
            total: queue.len(),
            ..RunAllStats::default()
        };
        *self.queue.borrow_mut() = queue;
        self.run_all_active.set(true);
        self.run_all_button.set_sensitive(false);
        self.stop_all_button.set_sensitive(true);
        self.status.remove_css_class("error");
        self.status.remove_css_class("warning");
        self.status.set_visible(true);
        for cell in &self.cells {
            cell.set_external_lock(true);
        }
        self.run_next();
    }

    fn run_next(self: &Rc<Self>) {
        if !self.run_all_active.get() || self.closed.get() {
            return;
        }
        let Some(index) = self.queue.borrow_mut().pop_front() else {
            self.finish_run_all();
            return;
        };

        let stats = self.stats.borrow();
        self.status.set_text(&format!(
            "Running cell {} of {}…",
            stats.finished + 1,
            stats.total
        ));
        drop(stats);

        let cell = self.cells[index].clone();
        let weak_runtime: Weak<Self> = Rc::downgrade(self);
        if !cell.run(Some(Box::new(move |outcome| {
            if let Some(runtime) = weak_runtime.upgrade() {
                runtime.cell_finished(outcome);
            }
        }))) {
            self.cell_finished(CellOutcome::Failed(format!(
                "cell {} could not start",
                cell.index + 1
            )));
        }
    }

    fn cell_finished(self: &Rc<Self>, outcome: CellOutcome) {
        if !self.run_all_active.get() {
            return;
        }
        {
            let mut stats = self.stats.borrow_mut();
            stats.finished += 1;
            if outcome.failed() {
                stats.failed += 1;
            }
        }
        self.run_next();
    }

    fn finish_run_all(&self) {
        self.run_all_active.set(false);
        self.run_all_button.set_sensitive(true);
        self.stop_all_button.set_sensitive(false);
        for cell in &self.cells {
            cell.set_external_lock(false);
        }

        let stats = self.stats.borrow();
        self.status.remove_css_class("warning");
        if stats.failed == 0 {
            self.status
                .set_text(&format!("Run All finished: {} cell(s).", stats.finished));
            self.status.remove_css_class("error");
        } else {
            self.status.set_text(&format!(
                "Run All finished: {} cell(s), {} failed.",
                stats.finished, stats.failed
            ));
            self.status.add_css_class("error");
        }
    }

    fn stop_all(&self) {
        let was_run_all = self.run_all_active.replace(false);
        self.queue.borrow_mut().clear();
        for cell in &self.cells {
            cell.cancel();
            cell.set_external_lock(false);
        }
        self.run_all_button.set_sensitive(!self.closed.get());
        self.stop_all_button.set_sensitive(false);
        if was_run_all && !self.closed.get() {
            self.status.set_text("Run All cancelled.");
            self.status.add_css_class("warning");
        }
    }

    fn shutdown(&self) {
        self.closed.set(true);
        self.stop_all();
    }
}

/// Handle for a presented native GTK notebook dialog.
///
/// Dropping this Rust handle does not close the dialog; the presented GTK object
/// owns its UI lifetime. Closing the dialog always cancels and reaps active cells.
#[derive(Clone)]
pub struct NotebookDialog {
    dialog: adw::Dialog,
}

impl NotebookDialog {
    /// Read and present a `.jtnb.md` notebook.
    ///
    /// `shell_argv` is copied and used verbatim for `shell` or unlabeled cells.
    /// Explicit language fences select their named interpreter. `cwd`, when
    /// absent, defaults to the notebook file's directory.
    pub fn open(
        parent: &adw::ApplicationWindow,
        path: impl AsRef<Path>,
        shell_argv: &[String],
        cwd: Option<&Path>,
    ) -> Result<Self, NotebookError> {
        let path = path.as_ref().to_path_buf();
        if !is_notebook_path(&path) {
            return Err(NotebookError::InvalidExtension(path));
        }
        let contents = read_notebook_bounded(&path).map_err(|source| NotebookError::Read {
            path: path.clone(),
            source,
        })?;
        let segments = parse_segments(&contents);
        let mut omitted_segments = segments.len().saturating_sub(MAX_NOTEBOOK_SEGMENTS);
        let working_directory = cwd.map(Path::to_path_buf).unwrap_or_else(|| {
            path.parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        });

        let raw_title = path
            .file_name()
            .map(|name| format!("Notebook: {}", name.to_string_lossy()))
            .unwrap_or_else(|| format!("Notebook: {}", path.display()));
        let title = safe_notebook_inline(&raw_title, 512);
        let dialog = adw::Dialog::builder()
            .title(&title)
            .content_width(900)
            .content_height(700)
            .build();

        let header = adw::HeaderBar::new();
        let stop_all_button = gtk4::Button::with_label("Stop All");
        stop_all_button.set_sensitive(false);
        let run_all_button = gtk4::Button::with_label("Run All");
        run_all_button.add_css_class("suggested-action");
        header.pack_end(&stop_all_button);
        header.pack_end(&run_all_button);

        let content = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        content.set_margin_start(16);
        content.set_margin_end(16);

        let run_all_status = gtk4::Label::new(None);
        run_all_status.set_xalign(0.0);
        run_all_status.add_css_class("dim-label");
        run_all_status.set_visible(false);
        content.append(&run_all_status);

        let mut cells = Vec::new();
        for segment in segments.into_iter().take(MAX_NOTEBOOK_SEGMENTS) {
            match segment {
                Segment::Text(text) => {
                    let label = gtk4::Label::new(None);
                    label.set_use_markup(true);
                    label.set_markup(&render_text_to_pango(&safe_notebook_display(
                        &text,
                        MAX_NOTEBOOK_TEXT_BYTES,
                    )));
                    label.set_wrap(true);
                    label.set_xalign(0.0);
                    label.set_halign(gtk4::Align::Fill);
                    label.set_selectable(true);
                    content.append(&label);
                }
                Segment::Code { lang, src } => {
                    if cells.len() >= MAX_NOTEBOOK_CELLS {
                        omitted_segments = omitted_segments.saturating_add(1);
                        continue;
                    }
                    let cell = CellController::new(
                        cells.len(),
                        &lang,
                        &src,
                        shell_argv,
                        &working_directory,
                    );
                    content.append(&cell.frame);
                    cells.push(cell);
                }
            }
        }

        if omitted_segments > 0 {
            let warning = gtk4::Label::new(Some(&format!(
                "{omitted_segments} notebook segment(s) were omitted to keep the UI bounded."
            )));
            warning.set_xalign(0.0);
            warning.set_wrap(true);
            warning.add_css_class("warning");
            content.append(&warning);
        }

        let shell_display = if shell_argv.is_empty() {
            "(none)".to_owned()
        } else {
            safe_notebook_inline(&shell_argv.join(" "), 4 * 1024)
        };
        let working_directory_display =
            safe_notebook_inline(&working_directory.to_string_lossy(), 4 * 1024);
        let footer = gtk4::Label::new(Some(&format!(
            "Cells run in isolated process groups with cwd {}. `shell` and unlabeled cells use: {}. Source is provided on stdin; active terminals are never modified.",
            working_directory_display,
            shell_display
        )));
        footer.set_wrap(true);
        footer.set_xalign(0.0);
        footer.set_selectable(true);
        footer.add_css_class("dim-label");
        content.append(&footer);

        let runtime = Rc::new(NotebookRuntime {
            cells,
            queue: RefCell::new(VecDeque::new()),
            run_all_active: Cell::new(false),
            closed: Cell::new(false),
            stats: RefCell::new(RunAllStats::default()),
            run_all_button: run_all_button.clone(),
            stop_all_button: stop_all_button.clone(),
            status: run_all_status,
        });
        run_all_button.set_sensitive(runtime.cells.iter().any(|cell| cell.runnable()));

        let weak_runtime = Rc::downgrade(&runtime);
        run_all_button.connect_clicked(move |_| {
            if let Some(runtime) = weak_runtime.upgrade() {
                runtime.start_run_all();
            }
        });
        let weak_runtime = Rc::downgrade(&runtime);
        stop_all_button.connect_clicked(move |_| {
            if let Some(runtime) = weak_runtime.upgrade() {
                runtime.stop_all();
            }
        });
        dialog.connect_closed(move |_| runtime.shutdown());

        let scroll = gtk4::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&content)
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&scroll));
        dialog.set_child(Some(&toolbar));
        dialog.present(Some(parent));

        Ok(Self { dialog })
    }

    pub fn dialog(&self) -> &adw::Dialog {
        &self.dialog
    }

    pub fn close(&self) {
        self.dialog.force_close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_output_timer_requires_the_current_handle_identity() {
        let old = Rc::new(CellHandle::new());
        let current = Rc::new(CellHandle::new());
        let active = RefCell::new(Some(current.clone()));
        assert!(current_cell_handle(&active, &current));
        assert!(!current_cell_handle(&active, &old));
        *active.borrow_mut() = None;
        assert!(!current_cell_handle(&active, &current));
    }

    #[test]
    #[cfg(unix)]
    fn completed_handle_never_signals_stale_group_telemetry() {
        let mut sentinel = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let handle = CellHandle::new();
        handle.pgid.store(sentinel.id() as i32, Ordering::SeqCst);
        handle.cancel();
        std::thread::sleep(Duration::from_millis(50));
        let survived = sentinel.try_wait().unwrap().is_none();
        // This fixture owns and reaps its sentinel regardless of assertion.
        let _ = sentinel.kill();
        let _ = sentinel.wait();
        assert!(
            survived,
            "a completed cell signalled an unowned process group"
        );
    }

    #[test]
    #[cfg(unix)]
    fn externally_reaped_child_is_forgotten_without_late_signal_authority() {
        let mut child = Command::new("sh")
            .args(["-c", "exit 0"])
            .process_group(0)
            .spawn()
            .unwrap();
        child.wait().unwrap();
        let handle = CellHandle::new();
        *handle.child.lock().unwrap() = Some(child);
        let error = wait_for_shared_child(&handle.child).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(nix::libc::ECHILD));
        assert!(handle.child.lock().unwrap().is_none());
        handle.cancel();
    }

    #[test]
    #[cfg(unix)]
    fn flatpak_bridge_never_rewrites_a_non_utf8_working_directory() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let non_utf8 = PathBuf::from(OsString::from_vec(b"/tmp/notebook-\xff".to_vec()));
        assert_eq!(host_bridge_cwd(&non_utf8, false), Ok(None));
        assert_eq!(
            host_bridge_cwd(&non_utf8, true),
            Err(NON_UTF8_FLATPAK_CWD_ERROR)
        );
        assert_eq!(
            host_bridge_cwd(Path::new("/tmp/notebook"), true),
            Ok(Some("/tmp/notebook".to_owned()))
        );
    }

    #[test]
    fn parses_text_and_shell_fences() {
        let markdown = "Intro\n```bash\necho hi\n```\nMiddle\n```\nls\n```\nTail";
        let segments = parse_segments(markdown);
        assert_eq!(segments.len(), 5);
        assert!(matches!(segments[0], Segment::Text(_)));
        assert_eq!(
            segments[1],
            Segment::Code {
                lang: "bash".to_owned(),
                src: "echo hi".to_owned()
            }
        );
        assert_eq!(
            segments[3],
            Segment::Code {
                lang: "".to_owned(),
                src: "ls".to_owned()
            }
        );
    }

    #[test]
    fn supports_tildes_indent_and_long_fences() {
        let markdown = "  ~~~sh\necho tilde\n  ~~~\n````bash\necho ``` literal\n````\n";
        let segments = parse_segments(markdown);
        assert_eq!(segments.len(), 2);
        assert!(matches!(
            &segments[0],
            Segment::Code { lang, src } if lang == "sh" && src == "echo tilde"
        ));
        assert!(matches!(
            &segments[1],
            Segment::Code { lang, src } if lang == "bash" && src == "echo ``` literal"
        ));
    }

    #[test]
    fn unfinished_fence_remains_visible_as_text() {
        let segments = parse_segments("before\n```bash\necho incomplete\n");
        assert!(segments
            .iter()
            .all(|segment| matches!(segment, Segment::Text(_))));
        let joined = segments
            .into_iter()
            .map(|segment| match segment {
                Segment::Text(text) => text,
                Segment::Code { .. } => unreachable!(),
            })
            .collect::<String>();
        assert!(joined.contains("```bash"));
        assert!(joined.contains("echo incomplete"));
    }

    #[test]
    fn shell_fences_select_an_explicit_source() {
        let configured = vec!["/bin/zsh".to_owned(), "-l".to_owned()];
        assert_eq!(
            shell_argv_for_info("shell", &configured),
            Some(configured.clone())
        );
        assert_eq!(
            shell_argv_for_info("", &configured),
            Some(configured.clone())
        );
        assert_eq!(
            shell_argv_for_info("bash title=demo", &configured),
            Some(vec!["bash".to_owned()])
        );
        assert_eq!(shell_argv_for_info("python", &configured), None);
    }

    #[test]
    fn renders_safe_pango_markup() {
        let rendered = render_text_to_pango("# A & B\nUse **bold** and `x < y`");
        assert!(rendered.contains("A &amp; B</span>"));
        assert!(rendered.contains("<b>bold</b>"));
        assert!(rendered.contains("<tt>x &lt; y</tt>"));
    }

    #[test]
    fn notebook_extension_is_unambiguous() {
        assert!(is_notebook_path(Path::new("demo.jtnb.md")));
        assert!(!is_notebook_path(Path::new("demo.md")));
        assert!(!is_notebook_path(Path::new("demo.jtnb.md.bak")));
    }

    #[test]
    fn cell_worker_permit_caps_concurrency_and_releases_on_drop() {
        // Other tests may hold a couple of permits at the same time, so this
        // asserts the ceiling triggers within MAX_CONCURRENT_CELL_WORKERS
        // acquisitions rather than at an exact count.
        let mut held = Vec::new();
        let exhausted = loop {
            match CellWorkerPermit::acquire() {
                Some(permit) => held.push(permit),
                None => break true,
            }
            if held.len() > MAX_CONCURRENT_CELL_WORKERS {
                break false;
            }
        };
        assert!(
            exhausted,
            "acquisition must stop at the concurrency ceiling"
        );
        assert!(held.len() <= MAX_CONCURRENT_CELL_WORKERS);
        drop(held);
        assert!(
            CellWorkerPermit::acquire().is_some(),
            "dropped permits must release their slots"
        );
    }

    #[test]
    fn notebook_display_and_cell_execution_reject_hidden_text() {
        assert_eq!(
            safe_notebook_display("safe\u{202e}\x1btext\nnext", 1_024),
            "safe��text\nnext"
        );
        assert!(notebook_cell_source_is_safe("printf one\nprintf two\n"));
        assert!(!notebook_cell_source_is_safe("echo safe\u{202e}txt"));
        assert!(!notebook_cell_source_is_safe("echo \x1b[31mred"));
        assert!(!notebook_cell_source_is_safe(
            &"x".repeat(MAX_NOTEBOOK_CELL_SOURCE_BYTES + 1)
        ));
    }

    #[test]
    fn notebook_reader_is_bounded_and_rejects_links() {
        let root = std::env::temp_dir().join(format!(
            "forge-notebook-reader-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("safe.jtnb.md");
        std::fs::write(&source, "```sh\ntrue\n```\n").unwrap();
        assert!(read_notebook_bounded(&source).unwrap().contains("true"));

        let oversized = root.join("large.jtnb.md");
        std::fs::write(&oversized, vec![b'x'; MAX_NOTEBOOK_FILE_BYTES as usize + 1]).unwrap();
        assert!(read_notebook_bounded(&oversized).is_err());

        #[cfg(unix)]
        {
            let linked = root.join("linked.jtnb.md");
            std::os::unix::fs::symlink(&source, &linked).unwrap();
            assert!(read_notebook_bounded(&linked).is_err());
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn notebook_reader_rejects_fifo_without_blocking() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let fifo = std::env::temp_dir().join(format!(
            "forge-notebook-fifo-{}-{}.jtnb.md",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: path is a live NUL-terminated pathname and mode is valid.
        assert_eq!(unsafe { nix::libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        assert!(read_notebook_bounded(&fifo).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        let _ = std::fs::remove_file(fifo);
    }

    #[test]
    fn worker_keeps_stdout_and_stderr_separate() {
        let handle = CellHandle::new();
        let receiver = spawn_cell_worker(
            CommandSpec {
                argv: vec!["sh".to_owned()],
                source: "printf out; printf err >&2; exit 7".to_owned(),
                cwd: std::env::temp_dir(),
            },
            &handle,
        );
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let outcome = loop {
            match receiver
                .recv_timeout(Duration::from_secs(3))
                .expect("worker event")
            {
                WorkerEvent::Started(_) => {}
                WorkerEvent::Output(OutputStream::Stdout, bytes) => stdout.extend(bytes),
                WorkerEvent::Output(OutputStream::Stderr, bytes) => stderr.extend(bytes),
                WorkerEvent::Done(outcome) => break outcome,
            }
        };
        assert_eq!(stdout, b"out");
        assert_eq!(stderr, b"err");
        assert_eq!(outcome, CellOutcome::Exited(7));
    }

    #[test]
    #[cfg(unix)]
    fn cancellation_kills_and_reaps_the_entire_process_group() {
        let handle = CellHandle::new();
        let receiver = spawn_cell_worker(
            CommandSpec {
                argv: vec!["sh".to_owned()],
                source: "sleep 30 & echo ready; wait".to_owned(),
                cwd: std::env::temp_dir(),
            },
            &handle,
        );

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut stdout = Vec::new();
        while !stdout
            .windows(b"ready".len())
            .any(|chunk| chunk == b"ready")
        {
            assert!(std::time::Instant::now() < deadline, "cell did not start");
            match receiver
                .recv_timeout(Duration::from_millis(100))
                .expect("ready output")
            {
                WorkerEvent::Started(_) => {}
                WorkerEvent::Output(OutputStream::Stdout, bytes) => stdout.extend(bytes),
                WorkerEvent::Output(OutputStream::Stderr, _) => {}
                WorkerEvent::Done(outcome) => panic!("cell ended before cancellation: {outcome:?}"),
            }
        }
        let group = handle.pgid.load(Ordering::SeqCst);
        assert!(group > 0, "worker did not publish its process group");
        handle.cancel();

        let outcome = loop {
            match receiver
                .recv_timeout(Duration::from_secs(3))
                .expect("worker event")
            {
                WorkerEvent::Done(outcome) => break outcome,
                WorkerEvent::Started(_) | WorkerEvent::Output(_, _) => {}
            }
        };
        assert_eq!(outcome, CellOutcome::Cancelled);
        assert!(handle.child.lock().expect("child slot").is_none());

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            // Signal 0 checks group existence without modifying it.
            let exists = unsafe { nix::libc::kill(-group, 0) } == 0;
            if !exists {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "a descendant survived notebook cancellation"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[cfg(unix)]
    fn root_exit_terminates_background_descendants_before_joining_readers() {
        let handle = CellHandle::new();
        let receiver = spawn_cell_worker(
            CommandSpec {
                argv: vec!["sh".to_owned()],
                // `sleep` inherits the pipes, so joining the reader threads
                // would hang until it exits unless the worker ends the group.
                source: "sleep 30 &".to_owned(),
                cwd: std::env::temp_dir(),
            },
            &handle,
        );

        // Read the group from the event stream, not by polling `pgid`: for a
        // shell that backgrounds a child and exits, the worker can run its whole
        // lifecycle — publish the group, signal it, then reset `pgid` to 0 — in
        // the gap before a cold poll first samples it, so polling races to a
        // spurious "did not publish" timeout. The Started event is ordered ahead
        // of Done on the channel and cannot be missed.
        let group = loop {
            match receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("worker must announce its process group")
            {
                WorkerEvent::Started(group) => break group,
                WorkerEvent::Output(_, _) => {}
                WorkerEvent::Done(outcome) => {
                    panic!("cell finished before announcing its group: {outcome:?}")
                }
            }
        };
        assert!(group > 0, "worker published an invalid process group");

        let outcome = loop {
            match receiver
                .recv_timeout(Duration::from_secs(3))
                .expect("worker must not hang on inherited output pipes")
            {
                WorkerEvent::Done(outcome) => break outcome,
                WorkerEvent::Started(_) | WorkerEvent::Output(_, _) => {}
            }
        };
        assert_eq!(outcome, CellOutcome::Exited(0));
        assert_eq!(handle.pgid.load(Ordering::SeqCst), 0);
        assert!(handle.child.lock().expect("child slot").is_none());

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while unsafe { nix::libc::kill(-group, 0) } == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "a background descendant survived normal cell completion"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(all(test, unix))]
mod pipe_cleanup_tests {
    use super::*;
    use std::time::Instant;

    static PIPE_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct PipeFixture {
        root: PathBuf,
        handle: CellHandle,
    }

    impl PipeFixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "notebook-pipe-fixture-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&root).unwrap();
            Self {
                root,
                handle: CellHandle::new(),
            }
        }

        fn release_holder(&self) -> bool {
            // Only this fixture knows these private release paths. No process
            // enumeration or signals to an escaped numeric PID are needed.
            let _ = std::fs::write(self.root.join("release-holder"), b"");
            let deadline = Instant::now() + Duration::from_secs(3);
            while !self.root.join("holder-closed").exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            self.root.join("holder-closed").exists()
        }
    }

    impl Drop for PipeFixture {
        fn drop(&mut self) {
            self.handle.cancel();
            let _ = std::fs::write(self.root.join("release-root"), b"");
            if self.release_holder() {
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }
    }

    fn escaped_pipe_case(pipes: &str, cancel: bool) {
        let _serial = PIPE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = PipeFixture::new();
        let redirections = match pipes {
            "stdin" => "<&3 3<&- >/dev/null 2>/dev/null",
            "stdout" => "</dev/null 3<&- 2>/dev/null",
            "stderr" => "</dev/null 3<&- >/dev/null",
            "all" => "<&3 3<&-",
            _ => unreachable!(),
        };
        // The private holder announces readiness only after setsid. It holds
        // precisely the selected pipe ends, and exits on our release file.
        // A fixed safety ceiling also prevents leaks after a test-process crash.
        let script = format!(
            "exec 3<&0; setsid sh -c 'printf ready > holder-ready; n=0; \
             while [ ! -e release-holder ] && [ \"$n\" -lt 1500 ]; do \
             sleep 0.02; n=$((n+1)); done; \
             exec 0<&- 1>&- 2>&-; : > holder-closed' {redirections} & exec 3<&-; \
             while [ ! -e holder-ready ]; do sleep 0.01; done; \
             printf ready > root-ready; \
             while [ ! -e release-root ]; do sleep 0.01; done; \
             printf ordinary-out-tail; printf ordinary-err-tail >&2; exit 7"
        );
        let receiver = spawn_cell_worker(
            CommandSpec {
                argv: vec!["sh".to_owned(), "-c".to_owned(), script],
                // The root deliberately does not consume this stdin. A source
                // larger than pipe capacity exercises the writer as well.
                source: "x".repeat(256 * 1024),
                cwd: fixture.root.clone(),
            },
            &fixture.handle,
        );
        let ready_deadline = Instant::now() + Duration::from_secs(3);
        while !fixture.root.join("root-ready").exists() {
            assert!(
                Instant::now() < ready_deadline,
                "fixture never acknowledged startup"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(fixture.root.join("holder-ready").exists());
        let started = Instant::now();
        if cancel {
            fixture.handle.cancel();
        } else {
            std::fs::write(fixture.root.join("release-root"), b"").unwrap();
        }
        let deadline = started + Duration::from_secs(1);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut outcome = None;
        while Instant::now() < deadline {
            match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(WorkerEvent::Output(OutputStream::Stdout, bytes)) => stdout.extend(bytes),
                Ok(WorkerEvent::Output(OutputStream::Stderr, bytes)) => stderr.extend(bytes),
                Ok(WorkerEvent::Done(done)) => {
                    outcome = Some(done);
                    break;
                }
                Ok(WorkerEvent::Started(_)) => {}
                Err(_) => break,
            }
        }
        let elapsed = started.elapsed();
        let holder_was_still_live = !fixture.root.join("holder-closed").exists();
        let reaped = fixture.handle.child.lock().unwrap().is_none();
        // Clean up even against the old implementation, before asserting the
        // regression, so a failed test never strands its descriptor holder.
        assert!(
            fixture.release_holder(),
            "fixture holder failed to close its own pipes"
        );
        if outcome.is_none() {
            let cleanup_deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < cleanup_deadline {
                match receiver.recv_timeout(Duration::from_millis(50)) {
                    Ok(WorkerEvent::Done(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    _ => {}
                }
            }
        }
        eprintln!("{pipes}, cancel={cancel}: completion={outcome:?}, elapsed={elapsed:?}, root_reaped={reaped}, escaped_holder_survived={holder_was_still_live}");
        assert!(
            holder_was_still_live,
            "worker must not terminate the escaped session"
        );
        assert!(reaped, "the ordinary wait owner must reap the root");
        assert_eq!(
            outcome,
            Some(if cancel {
                CellOutcome::Cancelled
            } else {
                CellOutcome::Exited(7)
            }),
            "worker hung on escaped {pipes} descriptors"
        );
        if !cancel {
            assert_eq!(stdout, b"ordinary-out-tail");
            assert_eq!(stderr, b"ordinary-err-tail");
        }
    }

    #[test]
    fn escaped_stdout_does_not_hold_completion() {
        escaped_pipe_case("stdout", false);
    }

    #[test]
    fn escaped_stderr_does_not_hold_completion() {
        escaped_pipe_case("stderr", false);
    }

    #[test]
    fn escaped_stdin_does_not_hold_completion() {
        escaped_pipe_case("stdin", false);
    }

    #[test]
    fn cancellation_finishes_with_all_escaped_pipe_ends() {
        escaped_pipe_case("all", true);
    }

    #[test]
    fn source_and_output_cross_pipe_capacity_without_losing_the_tail() {
        let _serial = PIPE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let handle = CellHandle::new();
        let out = "o".repeat(72 * 1024);
        let err = "e".repeat(72 * 1024);
        let source = format!("printf '%s' '{out}'; printf '%s' '{err}' >&2; printf out-tail; printf err-tail >&2; exit 7");
        let receiver = spawn_cell_worker(
            CommandSpec {
                argv: vec!["sh".into()],
                source,
                cwd: std::env::temp_dir(),
            },
            &handle,
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let outcome = loop {
            match receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("ordinary source/output must finish")
            {
                WorkerEvent::Output(OutputStream::Stdout, bytes) => stdout.extend(bytes),
                WorkerEvent::Output(OutputStream::Stderr, bytes) => stderr.extend(bytes),
                WorkerEvent::Done(outcome) => break outcome,
                WorkerEvent::Started(_) => {}
            }
        };
        assert_eq!(outcome, CellOutcome::Exited(7));
        assert_eq!(stdout, format!("{out}out-tail").as_bytes());
        assert_eq!(stderr, format!("{err}err-tail").as_bytes());
        assert!(handle.child.lock().unwrap().is_none());
    }
    #[test]
    fn slow_consumer_keeps_the_roots_buffered_output_tail() {
        let _serial = PIPE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let handle = CellHandle::new();
        let words = (0..100).map(|_| "x").collect::<Vec<_>>().join(" ");
        let receiver = spawn_cell_worker(
            CommandSpec {
                argv: vec!["sh".into()],
                source: format!(
                    "for value in {words}; do printf x; sleep 0.02; done; printf final-tail"
                ),
                cwd: std::env::temp_dir(),
            },
            &handle,
        );
        let mut stdout = Vec::new();
        // A real output event acknowledges that the root and reader started.
        while stdout.is_empty() {
            match receiver.recv_timeout(Duration::from_secs(3)).unwrap() {
                WorkerEvent::Output(OutputStream::Stdout, bytes) => stdout.extend(bytes),
                WorkerEvent::Done(outcome) => panic!("root ended before handshake: {outcome:?}"),
                _ => {}
            }
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while handle.child.lock().unwrap().is_some() {
            assert!(Instant::now() < deadline, "root did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
        // Simulate a busy UI with a full output queue throughout retirement.
        std::thread::sleep(Duration::from_millis(200));
        let outcome = loop {
            match receiver.recv_timeout(Duration::from_secs(2)).unwrap() {
                WorkerEvent::Output(OutputStream::Stdout, bytes) => stdout.extend(bytes),
                WorkerEvent::Done(outcome) => break outcome,
                _ => {}
            }
        };
        assert_eq!(outcome, CellOutcome::Exited(0));
        assert_eq!(stdout, format!("{}final-tail", "x".repeat(100)).as_bytes());
    }

    #[test]
    fn deferred_output_is_bounded_even_when_bytes_are_always_ready() {
        // Deterministic adapter for a continuously readable escaped writer.
        // No poll is reached: the control is already retired before the read.
        struct AlwaysReady;
        impl Read for AlwaysReady {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                bytes.fill(b'x');
                Ok(bytes.len())
            }
        }
        impl std::os::fd::AsRawFd for AlwaysReady {
            fn as_raw_fd(&self) -> std::os::fd::RawFd {
                -1
            }
        }
        let control = CellPipeControl::default();
        *control.retired.lock().unwrap() = Some(Instant::now() - Duration::from_secs(1));
        let (sender, receiver) = mpsc::sync_channel(1);
        stream_cell_output(
            CellPipe {
                inner: AlwaysReady,
                control: control.clone(),
            },
            OutputStream::Stdout,
            sender.clone(),
        );
        assert_eq!(
            control.deferred_output.lock().unwrap()[0].len(),
            MAX_OUTPUT_BYTES + 1
        );
        assert!(control.deferred_output.lock().unwrap()[1].is_empty());
        control.send_tail(&sender);
        match receiver.recv().unwrap() {
            WorkerEvent::Output(OutputStream::Stdout, bytes) => {
                assert_eq!(bytes.len(), MAX_OUTPUT_BYTES + 1)
            }
            _ => panic!("missing bounded final output"),
        }
        assert!(control
            .deferred_output
            .lock()
            .unwrap()
            .iter()
            .all(Vec::is_empty));
    }

    #[test]
    fn output_backpressure_retires_without_waiting_for_a_consumer() {
        let control = CellPipeControl::default();
        let (sender, _receiver) = mpsc::sync_channel(1);
        sender
            .send(WorkerEvent::Output(OutputStream::Stdout, vec![1]))
            .unwrap();
        let reader_control = control.clone();
        let (ready_sender, ready_receiver) = mpsc::channel();
        let (done_sender, done_receiver) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            ready_sender.send(()).unwrap();
            let sent = reader_control
                .send_output(&sender, WorkerEvent::Output(OutputStream::Stdout, vec![2]));
            done_sender.send(sent.is_ok()).unwrap();
        });
        ready_receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(done_receiver
            .recv_timeout(Duration::from_millis(30))
            .is_err());
        let started = Instant::now();
        control.retire();
        assert!(!done_receiver.recv_timeout(Duration::from_secs(1)).unwrap());
        thread.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn retirement_drains_available_tail_then_bounds_an_open_reader() {
        let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let control = CellPipeControl::default();
        let mut reader = control.wrap(reader).unwrap();
        writer.write_all(b"tail").unwrap();
        control.retire();
        let first_retirement = *control.retired.lock().unwrap();
        let mut tail = [0u8; 4];
        reader.read_exact(&mut tail).unwrap();
        assert_eq!(&tail, b"tail");
        control.retire();
        assert_eq!(*control.retired.lock().unwrap(), first_retirement);
        let started = Instant::now();
        let error = reader.read(&mut tail).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
        // The peer is still owned and open; timeout did not depend on its EOF.
        writer.write_all(b"still-owned").unwrap();
    }

    #[test]
    fn pipe_setup_rejects_invalid_descriptors_without_io_workers() {
        struct InvalidDescriptor;
        impl std::os::fd::AsRawFd for InvalidDescriptor {
            fn as_raw_fd(&self) -> std::os::fd::RawFd {
                -1
            }
        }
        let result = CellPipeControl::default().wrap(InvalidDescriptor);
        assert!(matches!(result, Err(error) if error.raw_os_error() == Some(nix::libc::EBADF)));
    }
    #[test]
    fn each_io_thread_spawn_failure_closes_workers_and_reaps_the_root() {
        let _serial = PIPE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for fail_at in 1..=3 {
            let handle = CellHandle::new();
            let started = Instant::now();
            let receiver = spawn_cell_worker_with_io_spawner(
                CommandSpec {
                    argv: vec!["sh".into(), "-c".into(), "sleep 30".into()],
                    source: "x".repeat(256 * 1024),
                    cwd: std::env::temp_dir(),
                },
                &handle,
                CellIoSpawner {
                    fail_at: Some(fail_at),
                    attempts: 0,
                },
            );
            let deadline = started + Duration::from_secs(2);
            let outcome = loop {
                if let WorkerEvent::Done(outcome) = receiver
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .expect("injected spawn failure must finish")
                {
                    break outcome;
                }
            };
            assert!(
                matches!(outcome, CellOutcome::Failed(ref error) if error.contains("injected notebook I/O thread spawn failure")),
                "{outcome:?}"
            );
            assert!(handle.child.lock().unwrap().is_none());
            assert_eq!(handle.pgid.load(Ordering::SeqCst), 0);
            // Disconnection proves no previously started I/O worker retains a
            // channel sender after Done and the worker closure has returned.
            assert!(matches!(
                receiver.recv_timeout(Duration::from_secs(1)),
                Err(mpsc::RecvTimeoutError::Disconnected)
            ));
            eprintln!(
                "I/O spawn position {fail_at}: controlled failure and cleanup in {:?}",
                started.elapsed()
            );
        }
    }

    #[test]
    fn partial_io_setup_failure_retires_readers_and_reaps_the_owned_root() {
        let _serial = PIPE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let child = Command::new("sh")
            .args(["-c", "sleep 30"])
            .process_group(0)
            .spawn()
            .unwrap();
        let handle = CellHandle::new();
        handle.pgid.store(child.id() as i32, Ordering::SeqCst);
        *handle.child.lock().unwrap() = Some(child);
        let control = CellPipeControl::default();
        let (reader, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut reader = control.wrap(reader).unwrap();
        let (read_sender, read_receiver) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            read_sender
                .send(reader.read(&mut [0u8; 1]).map_err(|error| error.kind()))
                .unwrap();
        });
        let (sender, receiver) = mpsc::sync_channel(1);
        let started = Instant::now();
        fail_cell_io_setup(
            &handle.child,
            &handle.pgid,
            &sender,
            vec![thread],
            io::Error::other("fixture spawn failure"),
            &control,
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(read_receiver.recv().unwrap(), Err(io::ErrorKind::TimedOut));
        assert!(handle.child.lock().unwrap().is_none());
        assert_eq!(handle.pgid.load(Ordering::SeqCst), 0);
        assert!(
            matches!(receiver.recv().unwrap(), WorkerEvent::Done(CellOutcome::Failed(error)) if error.contains("fixture spawn failure"))
        );
        assert!(peer.write_all(b"closed-reader").is_err());
    }
}

#[cfg(test)]
mod output_capture_tests {
    use super::{CellOutputCapture, OutputStream};

    #[test]
    fn output_preserves_utf8_at_every_pipe_boundary() {
        let source = "aé中文🙂z";
        for split in 0..=source.len() {
            let mut capture = CellOutputCapture::new(1024);
            let mut text = capture
                .append(OutputStream::Stdout, &source.as_bytes()[..split])
                .0;
            text.push_str(
                &capture
                    .append(OutputStream::Stdout, &source.as_bytes()[split..])
                    .0,
            );
            text.push_str(&capture.finish()[0]);
            assert_eq!(text, source, "split at byte {split}");
        }
    }

    #[test]
    fn output_streams_keep_independent_utf8_tails() {
        let mut capture = CellOutputCapture::new(1024);
        assert_eq!(capture.append(OutputStream::Stdout, &[0xe4, 0xb8]).0, "");
        assert_eq!(
            capture.append(OutputStream::Stderr, b"warning").0,
            "warning"
        );
        assert_eq!(capture.append(OutputStream::Stdout, &[0xad]).0, "中");
        assert_eq!(capture.finish(), ["", ""]);
    }

    #[test]
    fn output_flushes_invalid_and_incomplete_bytes_on_completion() {
        let mut capture = CellOutputCapture::new(1024);
        assert_eq!(
            capture
                .append(OutputStream::Stdout, &[0xff, b'a', 0xf0, 0x9f])
                .0,
            "�a"
        );
        assert_eq!(capture.finish(), ["�", ""]);
        assert_eq!(capture.finish(), ["", ""]);
    }

    #[test]
    fn final_crossing_chunk_reports_truncation_once() {
        let mut capture = CellOutputCapture::new(5);
        assert_eq!(
            capture.append(OutputStream::Stdout, b"abc"),
            ("abc".into(), false)
        );
        assert_eq!(
            capture.append(OutputStream::Stderr, b"def"),
            ("de".into(), true)
        );
        assert_eq!(
            capture.append(OutputStream::Stdout, b"more"),
            ("".into(), false)
        );
        assert_eq!(capture.finish(), ["", ""]);
    }

    #[test]
    fn exact_limit_is_complete_until_another_byte_arrives() {
        let mut capture = CellOutputCapture::new(3);
        assert_eq!(
            capture.append(OutputStream::Stdout, "中".as_bytes()),
            ("中".into(), false)
        );
        assert_eq!(
            capture.append(OutputStream::Stdout, b""),
            ("".into(), false)
        );
        assert_eq!(
            capture.append(OutputStream::Stderr, b"!"),
            ("".into(), true)
        );
        let mut zero = CellOutputCapture::new(0);
        assert_eq!(zero.append(OutputStream::Stdout, b"x"), ("".into(), true));
    }

    #[test]
    fn truncating_a_code_point_flushes_only_its_retained_prefix() {
        let mut capture = CellOutputCapture::new(2);
        assert_eq!(
            capture.append(OutputStream::Stdout, "中".as_bytes()),
            ("".into(), true)
        );
        assert_eq!(capture.finish(), ["�", ""]);
    }
}
