use gtk4::glib;
use nix::libc;
use nix::pty::{openpty, OpenptyResult};
use nix::unistd::{self, ForkResult, Pid};
use std::ffi::CString;
use std::fmt;
use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use crate::process::{ChildLifecycle, ReapOwner};
use crate::terminal::TERMINAL_ESCALATION;
use jterm_core::pty_input::{
    AdmittedInput, InputGuard, PasteModes, PastePolicy, UnbracketedMultiline,
};

enum PtyMsg {
    Data(Vec<u8>),
    Exit(i32),
}

/// Which process group currently owns the PTY foreground.
///
/// OSC command markers travel through the same byte stream as untrusted command
/// output. A marker is authoritative only after the interactive shell has
/// regained the terminal; an external foreground job can print identical bytes
/// but necessarily owns a different process group while it is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PtyForeground {
    Shell,
    Other,
    Unknown,
}

fn classify_foreground(shell_group: libc::pid_t, foreground_group: libc::pid_t) -> PtyForeground {
    if shell_group <= 0 || foreground_group <= 0 {
        PtyForeground::Unknown
    } else if shell_group == foreground_group {
        PtyForeground::Shell
    } else {
        PtyForeground::Other
    }
}

/// How often the reader thread asks the lifecycle for the child's status once
/// the PTY master has reached end of file.
const CHILD_REAP_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
/// How long a child may outlive its own PTY's EOF before the reader thread
/// stops waiting politely and terminates it.
const CHILD_REAP_ESCALATION_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

pub struct OwnedPty {
    master: std::sync::Arc<std::sync::Mutex<Option<OwnedFd>>>,
    /// Terminal input is written by a dedicated worker. A full PTY kernel buffer
    /// therefore backpressures that worker rather than GTK's main thread.
    input_tx: std::sync::Mutex<Option<mpsc::SyncSender<Vec<u8>>>>,
    /// The forked shell. This process reaps it (`ReapOwner::Ours`), so the
    /// lifecycle — not a raw pid — is the only handle any teardown path holds:
    /// it serializes every signal against its own `waitpid`, refuses to start a
    /// second escalation, and never leaves a zombie behind.
    lifecycle: Arc<ChildLifecycle>,
    /// Set by `kill`/`Drop` to release a reader parked on an idle PTY. Without
    /// it, a shell that left a descendant holding the slave open (`nohup ... &`,
    /// a detached ssh, any daemonized grandchild) kept the reader blocked in
    /// `read` for the rest of the process lifetime, pinning a duplicated master
    /// descriptor and its buffer.
    reader_cancelled: Arc<AtomicBool>,
    /// Linux wakeup for that reader. `None` means eventfd creation failed (or
    /// this is not Linux), and the reader falls back to bounded polling.
    reader_cancel_eventfd: Option<Arc<OwnedFd>>,
    /// The shared outgoing-byte filter. It removes paste-bracket markers from
    /// any payload body — a clipboard carrying `ESC[201~` would otherwise close
    /// the frame early and have its remainder run as a command — and tracks
    /// frames whose start, body and end arrive through separate `write_bytes`
    /// calls. Behind a `Mutex` only because `write_bytes` takes `&self`.
    input_guard: std::sync::Mutex<InputGuard>,
    /// Coalesce repeated backpressure logs until one later input is accepted.
    /// Error values contain only byte counts, never terminal content.
    input_error_reported: AtomicBool,
    /// Mirrors the shell's DECSET/DECRST 2004 state so multiline insertion can
    /// be protected at the central input boundary. Fed by the block parser's
    /// `ParserEvent::DecsetMode` (see [`OwnedPty::set_shell_bracketed_paste`]),
    /// which is the single owner of this mode for a forge pane.
    shell_bracketed_paste: AtomicBool,
    /// One-shot secret delivered to the interactive shell through an inherited
    /// pipe fd (never argv/environment). Updated bundled integrations close the
    /// fd before launching user commands and bind C/D ids to this value.
    shell_integration_token: Option<String>,
    /// Test-only slave end of a bare [`OwnedPty::for_tests`] pair, held open so
    /// master-side writes are not hung up. Never present in a spawned PTY,
    /// where the child owns the slave.
    #[cfg(test)]
    test_slave: Option<OwnedFd>,
    /// Test-only recorded answer for [`OwnedPty::foreground_owner`]. A bare
    /// PTY pair has no session, so the real `tcgetpgrp` probe could only ever
    /// return [`PtyForeground::Unknown`]; tests that exercise a foreground
    /// decision record the answer they mean instead.
    ///
    /// Mutable, because ownership genuinely moves during a command: an
    /// interactive `ssh` owns the terminal while it runs and hands it back
    /// before the local shell prints its own prompt marks. A test that cannot
    /// model that hand-back can only assert one half of a foreground rule.
    #[cfg(test)]
    test_foreground: std::sync::Mutex<Option<PtyForeground>>,
}

#[cfg(target_os = "linux")]
fn shell_token_channel() -> io::Result<Option<(String, OwnedFd, OwnedFd)>> {
    let mut random = [0u8; 16];
    // SAFETY: `random` is writable for exactly the supplied length.
    let read = unsafe {
        libc::getrandom(
            random.as_mut_ptr().cast(),
            random.len(),
            libc::GRND_NONBLOCK,
        )
    };
    if read != random.len() as isize {
        return Ok(None);
    }
    let mut token = String::with_capacity(random.len() * 2);
    for byte in random {
        use std::fmt::Write as _;
        let _ = write!(token, "{byte:02x}");
    }
    let mut fds = [-1; 2];
    // SAFETY: `fds` points to two writable integers. CLOEXEC is cleared only
    // for the read end in the post-fork child immediately before exec.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe2 returned two fresh owned descriptors.
    let read_fd = move_fd_above_stdio(unsafe { OwnedFd::from_raw_fd(fds[0]) })?;
    let write_fd = move_fd_above_stdio(unsafe { OwnedFd::from_raw_fd(fds[1]) })?;
    Ok(Some((token, read_fd, write_fd)))
}

#[cfg(target_os = "linux")]
fn move_fd_above_stdio(fd: OwnedFd) -> io::Result<OwnedFd> {
    if fd.as_raw_fd() > libc::STDERR_FILENO {
        return Ok(fd);
    }
    // SAFETY: F_DUPFD_CLOEXEC duplicates this valid owned descriptor and
    // chooses the first free number >= 3. The original closes on return.
    let duplicated = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if duplicated < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(duplicated) })
}

#[cfg(not(target_os = "linux"))]
fn shell_token_channel() -> io::Result<Option<(String, OwnedFd, OwnedFd)>> {
    Ok(None)
}

// Raw GLib FFI for g_unix_fd_add_full (not exposed by glib-rs 0.22)
extern "C" {
    fn g_unix_fd_add_full(
        priority: i32,
        fd: i32,
        condition: u32,
        function: extern "C" fn(fd: i32, condition: u32, user_data: *mut std::ffi::c_void) -> i32,
        user_data: *mut std::ffi::c_void,
        notify: extern "C" fn(data: *mut std::ffi::c_void),
    ) -> u32;
}

const G_IO_IN: u32 = 1;
// A block command may continuously repaint a spinner or progress bar. Keep PTY
// delivery at idle priority so GTK can dispatch pointer/button events first.
const G_PRIORITY_DEFAULT_IDLE: i32 = 200;
const READER_CANCEL_FALLBACK_POLL_MS: i32 = 50;

#[derive(Debug)]
enum ReaderPoll {
    PtyReady,
    Cancelled,
    TimedOut,
    /// The cancel descriptor could not be watched. The caller must discard it
    /// and continue with the bounded polling fallback instead of stopping the
    /// otherwise healthy PTY reader.
    CancelUnavailable(io::Error),
}

#[cfg(target_os = "linux")]
fn create_reader_cancel_eventfd() -> Option<Arc<OwnedFd>> {
    // SAFETY: eventfd returns a fresh descriptor on success; ownership moves
    // immediately into OwnedFd. Nonblocking keeps kill/Drop unable to stall.
    let raw = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
    if raw < 0 {
        log::warn!(
            "PTY reader cancel eventfd unavailable; using {} ms polling: {}",
            READER_CANCEL_FALLBACK_POLL_MS,
            io::Error::last_os_error()
        );
        return None;
    }
    // SAFETY: `raw` is a fresh successful eventfd result owned by this process.
    Some(Arc::new(unsafe { OwnedFd::from_raw_fd(raw) }))
}

#[cfg(not(target_os = "linux"))]
fn create_reader_cancel_eventfd() -> Option<Arc<OwnedFd>> {
    None
}

/// Wait until the PTY can be read or teardown requests reader cancellation.
///
/// With a cancel fd this blocks indefinitely, so an idle reader has no timer
/// wakeups. Without one it preserves the historical 50 ms atomic check. PTY
/// HUP/ERR are reported as readable so the following `read` observes EOF/EIO;
/// an invalid PTY fd remains a real reader failure.
fn poll_pty_or_reader_cancel(pty_fd: RawFd, cancel_fd: Option<RawFd>) -> io::Result<ReaderPoll> {
    loop {
        let mut ready = [
            libc::pollfd {
                fd: pty_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: cancel_fd.unwrap_or(-1),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let has_cancel_fd = cancel_fd.is_some();
        let descriptor_count: libc::nfds_t = if has_cancel_fd { 2 } else { 1 };
        let timeout = if has_cancel_fd {
            -1
        } else {
            READER_CANCEL_FALLBACK_POLL_MS
        };
        // SAFETY: `ready` contains `descriptor_count` initialized pollfd values
        // and remains writable for the duration of this blocking call.
        let polled = unsafe { libc::poll(ready.as_mut_ptr(), descriptor_count, timeout) };
        if polled < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return if has_cancel_fd {
                Ok(ReaderPoll::CancelUnavailable(error))
            } else {
                Err(error)
            };
        }
        if polled == 0 {
            return Ok(ReaderPoll::TimedOut);
        }

        if has_cancel_fd {
            let cancel_events = ready[1].revents;
            if cancel_events & libc::POLLIN != 0 {
                return Ok(ReaderPoll::Cancelled);
            }
            if cancel_events & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                return Ok(ReaderPoll::CancelUnavailable(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    format!("cancel eventfd poll failure: revents={cancel_events:#x}"),
                )));
            }
        }

        let pty_events = ready[0].revents;
        if pty_events & libc::POLLNVAL != 0 {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "PTY reader descriptor became invalid",
            ));
        }
        if pty_events & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0 {
            return Ok(ReaderPoll::PtyReady);
        }
        // A signal or platform-specific event may produce no relevant bits.
        // Rebuild pollfd values so stale revents can never leak into a retry.
    }
}

fn request_reader_cancel(cancelled: &AtomicBool, cancel_eventfd: Option<&OwnedFd>) {
    cancelled.store(true, Ordering::Release);
    #[cfg(target_os = "linux")]
    if let Some(cancel_eventfd) = cancel_eventfd {
        if let Err(error) = signal_eventfd(cancel_eventfd.as_raw_fd()) {
            log::warn!("could not wake PTY reader cancellation poll: {error}");
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = cancel_eventfd;
}

/// Bound queued output. Once this queue fills, the reader blocks and the kernel
/// PTY buffer provides natural backpressure to a runaway producer.
const PTY_QUEUE_CAPACITY: usize = 8;
/// Bound queued terminal input without ever blocking GTK. Each write is one
/// semantic unit (a keystroke sequence, paste frame, or command submission),
/// so saturation rejects the whole unit instead of enqueueing an executable
/// prefix. The queue therefore retains at most 16 MiB plus channel overhead.
const PTY_INPUT_QUEUE_CAPACITY: usize = 64;
pub const MAX_PTY_INPUT_MESSAGE_BYTES: usize = 256 * 1024;
/// Smaller chunks cap the amount of VTE feeding performed in one UI callback.
const PTY_READ_CHUNK_BYTES: usize = 32 * 1024;
/// Pace the fallback timer transport, which has no readiness signal of its own
/// and would otherwise spin. The eventfd transport re-arms itself at idle
/// priority instead — see `rearm_dispatch`.
const PTY_DISPATCH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(8);

/// A terminal-input write was not admitted to the dedicated writer queue.
///
/// The error deliberately carries byte counts only: callers can report
/// backpressure without copying shell input into logs, dialogs, or telemetry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PtyWriteError {
    TooLarge { bytes: usize, limit: usize },
    QueueFull { bytes: usize },
    Closed { bytes: usize },
}

impl fmt::Display for PtyWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { bytes, limit } => {
                write!(
                    formatter,
                    "PTY input is {bytes} bytes; the limit is {limit} bytes"
                )
            }
            Self::QueueFull { bytes } => write!(
                formatter,
                "PTY input queue is full; {bytes}-byte input was not sent"
            ),
            Self::Closed { bytes } => write!(
                formatter,
                "PTY input queue is closed; {bytes}-byte input was not sent"
            ),
        }
    }
}

impl std::error::Error for PtyWriteError {}

/// Policy for the PTY write boundary.
///
/// `FirstLineOnly` preserves forge's behaviour: without DECSET 2004 a
/// multiline payload is cut to its first logical line rather than submitting
/// every line. Control stripping stays off — this boundary carries keystrokes,
/// cursor keys and mouse reports, which are control bytes by construction —
/// while marker removal happens regardless of the policy.
fn boundary_paste_policy() -> PastePolicy {
    PastePolicy::prompt_insert(UnbracketedMultiline::FirstLineOnly)
}

struct FdWatchData<F: FnMut() -> bool> {
    callback: F,
}

extern "C" fn fd_watch_callback<F: FnMut() -> bool>(
    _fd: i32,
    _condition: u32,
    user_data: *mut std::ffi::c_void,
) -> i32 {
    let data = unsafe { &mut *(user_data as *mut FdWatchData<F>) };
    if (data.callback)() {
        1
    } else {
        0
    }
}

extern "C" fn fd_watch_destroy<F: FnMut() -> bool>(user_data: *mut std::ffi::c_void) {
    unsafe {
        drop(Box::from_raw(user_data as *mut FdWatchData<F>));
    }
}

fn unix_fd_add_local<F: FnMut() -> bool + 'static>(fd: RawFd, func: F) {
    let data = Box::new(FdWatchData { callback: func });
    let ptr = Box::into_raw(data) as *mut std::ffi::c_void;
    unsafe {
        g_unix_fd_add_full(
            G_PRIORITY_DEFAULT_IDLE,
            fd,
            G_IO_IN,
            fd_watch_callback::<F>,
            ptr,
            fd_watch_destroy::<F>,
        );
    }
}

/// Write a complete byte slice, retrying interrupted and partial writes.
///
/// This function is intentionally used only by the background writer thread:
/// blocking on a full PTY is correct backpressure there, but would freeze GTK if
/// performed by a key, paste, or block-recall callback.
fn write_all_fd(fd: RawFd, mut data: &[u8]) -> io::Result<()> {
    while !data.is_empty() {
        let written = unsafe { libc::write(fd, data.as_ptr().cast::<libc::c_void>(), data.len()) };
        if written > 0 {
            data = &data[written as usize..];
            continue;
        }
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "PTY write returned zero",
            ));
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error);
    }
    Ok(())
}

fn spawn_fd_writer(fd: OwnedFd) -> io::Result<mpsc::SyncSender<Vec<u8>>> {
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(PTY_INPUT_QUEUE_CAPACITY);
    std::thread::Builder::new()
        .name("forge-pty-writer".to_string())
        .spawn(move || {
            for data in rx {
                if let Err(error) = write_all_fd(fd.as_raw_fd(), &data) {
                    log::warn!("PTY input writer stopped: {error}");
                    break;
                }
            }
        })?;
    Ok(tx)
}

fn try_enqueue_input(
    sender: &mpsc::SyncSender<Vec<u8>>,
    data: Vec<u8>,
) -> Result<(), PtyWriteError> {
    let bytes = data.len();
    if bytes > MAX_PTY_INPUT_MESSAGE_BYTES {
        return Err(PtyWriteError::TooLarge {
            bytes,
            limit: MAX_PTY_INPUT_MESSAGE_BYTES,
        });
    }
    match sender.try_send(data) {
        Ok(()) => Ok(()),
        Err(mpsc::TrySendError::Full(_)) => Err(PtyWriteError::QueueFull { bytes }),
        Err(mpsc::TrySendError::Disconnected(_)) => Err(PtyWriteError::Closed { bytes }),
    }
}

fn filter_and_enqueue_input(
    guard: &mut InputGuard,
    sender: &mpsc::SyncSender<Vec<u8>>,
    data: &[u8],
    modes: PasteModes,
) -> Result<(), PtyWriteError> {
    filter_and_enqueue_input_with(guard, sender, data, modes, |_, _| ())
}

fn filter_and_enqueue_input_with<T>(
    guard: &mut InputGuard,
    sender: &mpsc::SyncSender<Vec<u8>>,
    data: &[u8],
    modes: PasteModes,
    observe: impl FnOnce(&[u8], bool) -> T,
) -> Result<T, PtyWriteError> {
    // Core's `InputGuard` exposes its frame state but no setter, so a rollback
    // reconstructs an equivalent guard from the one-bit state.
    let before_in_frame = guard.in_frame();
    let safe_data = guard
        .filter(data, modes, boundary_paste_policy())
        .into_owned();
    if safe_data.is_empty() {
        // No bytes crossed the boundary, so no frame transition did either.
        *guard = input_guard_with_frame(before_in_frame);
        return Ok(observe(&[], before_in_frame));
    }
    if safe_data.len() > MAX_PTY_INPUT_MESSAGE_BYTES {
        *guard = input_guard_with_frame(before_in_frame);
        return Err(PtyWriteError::TooLarge {
            bytes: safe_data.len(),
            limit: MAX_PTY_INPUT_MESSAGE_BYTES,
        });
    }
    let observed = observe(&safe_data, before_in_frame);
    let result = try_enqueue_input(sender, safe_data);
    if result.is_err() {
        // Queue admission is the commit point. A rejected opener/closer must
        // not alter how the next independent write is sanitized.
        *guard = input_guard_with_frame(before_in_frame);
    }
    result.map(|()| observed)
}

/// Rebuild an `InputGuard` with the given frame state. Filtering an opener
/// through a fresh guard is the one public transition into `in_frame`.
fn input_guard_with_frame(in_frame: bool) -> InputGuard {
    let mut guard = InputGuard::new();
    if in_frame {
        let _ = guard.filter(
            jterm_core::pty_input::PASTE_START,
            PasteModes { bracketed: true },
            boundary_paste_policy(),
        );
        debug_assert!(guard.in_frame());
    }
    guard
}

fn invalid_nul(context: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{context} contains an embedded NUL byte"),
    )
}

/// The PTY child's environment, built entirely before `fork` from the
/// launch-time snapshot frozen by `app::run` (see
/// [`jterm_core::child_env::capture_inherited_environment`]), so CLI- and
/// toolkit-written variables such as `FORGE_CONFIG` or `GTK_IM_MODULE` never
/// leak into the shell. Other spawn paths are not covered: notebook cell
/// workers and the `flatpak-spawn` host bridge still start from the live
/// environment.
///
/// `LESS` is a flag rather than a hardcoded value because the family disagrees:
/// forge keeps `R` so a short `git log` still opens an interactive pager on the
/// alternate screen, while ember/frost use `FR`. Everything else — `TERM`,
/// `COLORTERM`, `TERM_PROGRAM`, `TERM_PROGRAM_VERSION`, `VTE_VERSION` — is the
/// family's shared identity policy. Block mode never lets libvte spawn the
/// child, so before this these variables (bar `TERM`) never reached the shell
/// and `bat`/`delta`/`lazygit` fell back to 256 colours.
fn child_environment(env_extra: &[(&str, &str)]) -> io::Result<Vec<CString>> {
    jterm_core::child_env::envp_from_captured(&block_mode_child_env(), env_extra)
}

fn block_mode_child_env() -> jterm_core::child_env::ChildEnv<'static> {
    jterm_core::child_env::ChildEnv {
        less_default: Some("R"),
        ..jterm_core::child_env::ChildEnv::from_identity()
    }
}

/// Requested working directory the child may actually start in.
///
/// A restored session names the directory the tab last ran in, and that
/// directory can be gone by the time the tab comes back — a removed git
/// worktree, an unmounted drive, a cleaned `/tmp` path. Dropping the request
/// here (as anvil does) starts the shell in the application directory
/// instead; propagating the failure would cost the user the whole pane.
fn usable_working_directory(cwd: Option<&str>) -> Option<&str> {
    cwd.filter(|value| !value.is_empty()).filter(|directory| {
        let usable = crate::host::working_directory_available(directory);
        if !usable {
            log::warn!("PTY working directory is unavailable; using the application directory");
        }
        usable
    })
}

/// Open the child's working directory for the post-fork `fchdir`.
///
/// The directory already passed [`usable_working_directory`], so a failure here
/// is either a race (the path vanished in between) or a search-only `--x`
/// directory, which `chdir` accepts but `open` refuses. Neither is worth the
/// pane: fall back to the application directory with a warning.
fn open_working_directory(cwd: Option<&str>) -> Option<File> {
    let directory = cwd?;
    match File::open(directory) {
        Ok(file) => Some(file),
        Err(error) => {
            log::warn!(
                "Cannot open PTY working directory {}: {error}",
                jterm_core::review_input::safe_inline_display(directory, 2 * 1024)
            );
            None
        }
    }
}

impl OwnedPty {
    fn close_master_fd(&self) {
        if let Ok(mut guard) = self.master.lock() {
            guard.take();
        }
    }

    pub fn spawn(argv: &[&str], cwd: Option<&str>, env_extra: &[(&str, &str)]) -> io::Result<Self> {
        Self::spawn_inner(argv, cwd, env_extra, false)
    }

    /// Spawn an interactive shell and, when requested, give its startup
    /// integration a one-shot private token pipe. Ordinary PTY users never
    /// inherit that descriptor.
    pub(crate) fn spawn_with_shell_token(
        argv: &[&str],
        cwd: Option<&str>,
        env_extra: &[(&str, &str)],
        enable_shell_token: bool,
    ) -> io::Result<Self> {
        Self::spawn_inner(argv, cwd, env_extra, enable_shell_token)
    }

    fn spawn_inner(
        argv: &[&str],
        cwd: Option<&str>,
        env_extra: &[(&str, &str)],
        enable_shell_token: bool,
    ) -> io::Result<Self> {
        let argv_owned: Vec<String> = argv.iter().map(|value| (*value).to_string()).collect();
        let host_bridge = crate::host::is_flatpak();
        // Decide the directory once, before it is baked into the host-bridge
        // wrapper argv and used to resolve a relative executable: the bridge
        // encodes `--directory`, so a caller cannot correct this afterwards.
        let effective_cwd = usable_working_directory(cwd);
        let executable_argv = crate::host::wrap_argv(&argv_owned, effective_cwd, env_extra);
        if executable_argv.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty PTY argv",
            ));
        }

        // Prepare every allocation and environment lookup before fork. GTK is
        // multi-threaded, so the child may only use async-signal-safe libc
        // operations until exec replaces the process image.
        // Resolve before fork: a PATH walk between fork and exec would allocate
        // and read directories in a process where only async-signal-safe calls
        // are legal. The shared resolver also checks the execute bit, so a
        // missing or non-executable shell is reported here instead of becoming a
        // child that exits 127 for no visible reason.
        let executable = crate::host::resolve_executable(
            &executable_argv[0],
            std::env::var_os("PATH").as_deref(),
            if host_bridge { None } else { effective_cwd },
        )?;
        let executable_c = CString::new(executable.as_os_str().as_bytes())
            .map_err(|_| invalid_nul("PTY executable path"))?;
        let c_argv: Vec<CString> = executable_argv
            .iter()
            .map(|argument| {
                CString::new(argument.as_str()).map_err(|_| invalid_nul("PTY argument"))
            })
            .collect::<io::Result<_>>()?;
        let mut argv_ptrs: Vec<*const libc::c_char> =
            c_argv.iter().map(|argument| argument.as_ptr()).collect();
        argv_ptrs.push(std::ptr::null());
        // execvp historically falls back to /bin/sh for an executable text
        // file without a shebang. Preserve that behavior while keeping the
        // fallback argv allocation on the safe side of fork.
        let mut shell_fallback_ptrs: Vec<*const libc::c_char> =
            Vec::with_capacity(c_argv.len() + 2);
        shell_fallback_ptrs.push(c"sh".as_ptr());
        shell_fallback_ptrs.push(executable_c.as_ptr());
        shell_fallback_ptrs.extend(c_argv.iter().skip(1).map(|argument| argument.as_ptr()));
        shell_fallback_ptrs.push(std::ptr::null());
        let mut c_environment = child_environment(env_extra)?;
        let token_channel = if host_bridge || !enable_shell_token {
            None
        } else {
            shell_token_channel()?
        };
        c_environment.retain(|entry| {
            !entry.as_bytes().starts_with(b"FORGE_SHELL_INTEGRATION_FD=")
                && !entry
                    .as_bytes()
                    .starts_with(b"FORGE_SHELL_INTEGRATION_TOKEN=")
        });
        if let Some((_, read_fd, _)) = token_channel.as_ref() {
            c_environment.push(
                CString::new(format!(
                    "FORGE_SHELL_INTEGRATION_FD={}",
                    read_fd.as_raw_fd()
                ))
                .map_err(|_| invalid_nul("shell integration fd environment"))?,
            );
        }
        let mut environment_ptrs: Vec<*const libc::c_char> =
            c_environment.iter().map(|entry| entry.as_ptr()).collect();
        environment_ptrs.push(std::ptr::null());
        let cwd_file = if host_bridge {
            None
        } else {
            open_working_directory(effective_cwd)
        };
        let cwd_fd = cwd_file.as_ref().map(AsRawFd::as_raw_fd).unwrap_or(-1);
        let token_read_fd = token_channel
            .as_ref()
            .map(|(_, fd, _)| fd.as_raw_fd())
            .unwrap_or(-1);
        let token_write_fd = token_channel
            .as_ref()
            .map(|(_, _, fd)| fd.as_raw_fd())
            .unwrap_or(-1);

        let initial_size = nix::pty::Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let OpenptyResult { master, slave } =
            openpty(Some(&initial_size), None).map_err(io::Error::other)?;
        let master_fd = master.as_raw_fd();
        let slave_fd = slave.as_raw_fd();

        match unsafe { unistd::fork() } {
            Ok(ForkResult::Child) => unsafe {
                libc::close(master_fd);
                if token_write_fd >= 0 {
                    libc::close(token_write_fd);
                }
                if libc::setsid() < 0 || libc::ioctl(slave_fd, libc::TIOCSCTTY, 0) < 0 {
                    libc::_exit(126);
                }
                if cwd_fd >= 0 {
                    if libc::fchdir(cwd_fd) < 0 {
                        libc::_exit(126);
                    }
                    if cwd_fd > libc::STDERR_FILENO {
                        libc::close(cwd_fd);
                    }
                }
                if libc::dup2(slave_fd, libc::STDIN_FILENO) < 0
                    || libc::dup2(slave_fd, libc::STDOUT_FILENO) < 0
                    || libc::dup2(slave_fd, libc::STDERR_FILENO) < 0
                {
                    libc::_exit(126);
                }
                if slave_fd > libc::STDERR_FILENO {
                    libc::close(slave_fd);
                }
                if token_read_fd >= 0 && libc::fcntl(token_read_fd, libc::F_SETFD, 0) < 0 {
                    libc::_exit(126);
                }
                libc::execve(
                    executable_c.as_ptr(),
                    argv_ptrs.as_ptr(),
                    environment_ptrs.as_ptr(),
                );
                let exec_error = *libc::__errno_location();
                if exec_error == libc::ENOEXEC {
                    libc::execve(
                        c"/bin/sh".as_ptr(),
                        shell_fallback_ptrs.as_ptr(),
                        environment_ptrs.as_ptr(),
                    );
                    libc::_exit(126);
                }
                libc::_exit(if exec_error == libc::ENOENT { 127 } else { 126 });
            },
            Ok(ForkResult::Parent { child }) => {
                let shell_integration_token =
                    if let Some((token, read_fd, write_fd)) = token_channel {
                        drop(read_fd);
                        let mut writer = File::from(write_fd);
                        if writer
                            .write_all(format!("{token}\n").as_bytes())
                            .and_then(|()| writer.flush())
                            .is_err()
                        {
                            kill_and_reap_unreferenced(child);
                            return Err(io::Error::new(
                                io::ErrorKind::BrokenPipe,
                                "could not deliver the shell integration token",
                            ));
                        }
                        Some(token)
                    } else {
                        None
                    };
                drop(slave);
                let lifecycle = match ChildLifecycle::new(child.as_raw(), ReapOwner::Ours) {
                    Ok(lifecycle) => lifecycle,
                    Err(error) => {
                        kill_and_reap_unreferenced(child);
                        return Err(error);
                    }
                };
                let writer_fd = match master.try_clone() {
                    Ok(fd) => fd,
                    Err(error) => return Err(abort_spawn(&lifecycle, error)),
                };
                let input_tx = match spawn_fd_writer(writer_fd) {
                    Ok(tx) => tx,
                    Err(error) => return Err(abort_spawn(&lifecycle, error)),
                };
                Ok(OwnedPty {
                    master: std::sync::Arc::new(std::sync::Mutex::new(Some(master))),
                    input_tx: std::sync::Mutex::new(Some(input_tx)),
                    lifecycle,
                    reader_cancelled: Arc::new(AtomicBool::new(false)),
                    reader_cancel_eventfd: create_reader_cancel_eventfd(),
                    input_guard: std::sync::Mutex::new(InputGuard::new()),
                    input_error_reported: AtomicBool::new(false),
                    shell_bracketed_paste: AtomicBool::new(false),
                    shell_integration_token,
                    #[cfg(test)]
                    test_slave: None,
                    #[cfg(test)]
                    test_foreground: std::sync::Mutex::new(None),
                })
            }
            Err(error) => Err(io::Error::other(error)),
        }
    }

    pub fn pid_i32(&self) -> i32 {
        self.lifecycle.pid()
    }

    pub(crate) fn shell_integration_token(&self) -> Option<&str> {
        self.shell_integration_token.as_deref()
    }

    /// Share this PTY's child lifecycle with a widget-tree teardown path.
    ///
    /// Every holder terminates the same lifecycle, so an explicit pane close,
    /// a window close sweeping the notebook, and this `OwnedPty`'s own drop
    /// collapse into exactly one escalation.
    pub fn lifecycle(&self) -> Arc<ChildLifecycle> {
        Arc::clone(&self.lifecycle)
    }

    /// Raw master-side fd, or -1 if the PTY has already been closed.
    ///
    /// The descriptor remains owned by this `OwnedPty`; callers only borrow the
    /// integer long enough for non-mutating probes such as `tcgetpgrp(3)`.
    pub fn master_fd_raw(&self) -> i32 {
        self.master
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(AsRawFd::as_raw_fd))
            .unwrap_or(-1)
    }

    /// Probe whether the interactive shell, rather than one of its foreground
    /// jobs, currently owns this PTY. Syscall failures remain distinct from a
    /// positive shell match so approval/observation paths can fail closed.
    pub(crate) fn foreground_owner(&self) -> PtyForeground {
        // A test PTY (`for_tests`) has no session behind its slave end, so the
        // probe below could only answer `Unknown`. Compiled out of every
        // non-test build together with the field it reads.
        #[cfg(test)]
        if let Some(recorded) = *self
            .test_foreground
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            return recorded;
        }
        let fd = self.master_fd_raw();
        let shell_pid = self.pid_i32();
        if fd < 0 || shell_pid <= 0 {
            return PtyForeground::Unknown;
        }
        // SAFETY: both calls are read-only probes. `fd` may be closed by a
        // concurrent teardown; that race simply returns -1 and maps to Unknown.
        let foreground_group = unsafe { libc::tcgetpgrp(fd) };
        let shell_group = unsafe { libc::getpgid(shell_pid) };
        classify_foreground(shell_group, foreground_group)
    }

    /// Record the shell's DECSET/DECRST 2004 state.
    ///
    /// The block parser is the single owner of this mode: it is a real CSI state
    /// machine, so it also sees an enable split across two PTY read chunks,
    /// which the raw byte scan this replaced could only approximate with a
    /// retained tail.
    pub fn set_shell_bracketed_paste(&self, enabled: bool) {
        self.shell_bracketed_paste.store(enabled, Ordering::Relaxed);
    }

    /// Whether the child advertised DECSET 2004 and will strip paste framing.
    pub fn shell_bracketed_paste(&self) -> bool {
        self.shell_bracketed_paste.load(Ordering::Relaxed)
    }

    #[must_use = "terminal input may be rejected by bounded nonblocking backpressure"]
    pub fn write_bytes(&self, data: &[u8]) -> Result<(), PtyWriteError> {
        self.write_bytes_with(data, |_, _| ())
    }

    /// Queue bytes and report the editor semantics that actually crossed the
    /// central PTY filter. Native VTE commits use this so multiline truncation,
    /// automatic paste framing, and marker removal cannot leave their local
    /// command shadow ahead of the shell.
    #[must_use = "terminal input may be rejected by bounded nonblocking backpressure"]
    pub(crate) fn write_bytes_admitted(&self, data: &[u8]) -> Result<AdmittedInput, PtyWriteError> {
        self.write_bytes_with(data, jterm_core::pty_input::admitted_input)
    }

    fn write_bytes_with<T>(
        &self,
        data: &[u8],
        observe: impl FnOnce(&[u8], bool) -> T,
    ) -> Result<T, PtyWriteError> {
        if data.len() > MAX_PTY_INPUT_MESSAGE_BYTES {
            return Err(PtyWriteError::TooLarge {
                bytes: data.len(),
                limit: MAX_PTY_INPUT_MESSAGE_BYTES,
            });
        }
        let modes = PasteModes {
            bracketed: self.shell_bracketed_paste(),
        };
        let input_tx = self
            .input_tx
            .lock()
            .ok()
            .and_then(|sender| sender.as_ref().cloned());
        let Some(input_tx) = input_tx else {
            return Err(PtyWriteError::Closed { bytes: data.len() });
        };
        // A poisoned guard would mean another thread panicked mid-filter; keep
        // the choke point rather than letting writes bypass it. Hold it through
        // queue admission so the frame state commits atomically with the bytes.
        let mut guard = self
            .input_guard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let result = filter_and_enqueue_input_with(&mut guard, &input_tx, data, modes, observe);
        if result.is_ok() {
            self.input_error_reported.store(false, Ordering::Relaxed);
        }
        result
    }

    /// Report a rejected write without leaking its contents or flooding logs
    /// while the same backpressure condition persists.
    pub fn report_write_error(&self, context: &'static str, error: PtyWriteError) {
        if !self.input_error_reported.swap(true, Ordering::Relaxed) {
            log::warn!("{context}: {error}");
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        if let Ok(guard) = self.master.lock() {
            if let Some(fd) = guard.as_ref() {
                let ws = libc::winsize {
                    ws_row: rows,
                    ws_col: cols,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };
                unsafe {
                    libc::ioctl(fd.as_raw_fd(), libc::TIOCSWINSZ, &ws);
                }
            }
        }
    }

    /// Close this pane's PTY and tear the shell down.
    ///
    /// Safe to call more than once and safe to race with [`Drop`]: the second
    /// `terminate` sees a teardown already in flight and returns without
    /// starting another escalation ladder for the same child.
    pub fn kill(&self) {
        request_reader_cancel(
            &self.reader_cancelled,
            self.reader_cancel_eventfd.as_deref(),
        );
        self.close_master_fd();
        self.close_input_writer();
        self.lifecycle.terminate(TERMINAL_ESCALATION);
    }

    fn close_input_writer(&self) {
        if let Ok(mut sender) = self.input_tx.lock() {
            sender.take();
        }
    }

    /// Start an async reader. A bounded channel transfers 32 KiB chunks to the
    /// GLib main thread; when the UI falls behind, the child is naturally slowed
    /// through the channel and kernel PTY buffers instead of growing memory
    /// without limit.
    pub fn start_reader<F, E>(&self, mut callback: F, on_exit: E)
    where
        F: FnMut(Vec<u8>) + 'static,
        E: FnOnce(i32) + 'static,
    {
        let reader_fd = match self
            .master
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().and_then(|fd| fd.try_clone().ok()))
        {
            Some(fd) => fd,
            None => return,
        };

        let lifecycle = self.lifecycle();
        let (tx, rx) = mpsc::sync_channel::<PtyMsg>(PTY_QUEUE_CAPACITY);

        // Create an eventfd for signaling data availability to the main thread.
        let efd: RawFd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if efd < 0 {
            self.start_reader_polling(reader_fd, lifecycle, tx, rx, callback, on_exit);
            return;
        }
        let eventfd = Arc::new(unsafe { OwnedFd::from_raw_fd(efd) });
        let wake_pending = Arc::new(AtomicBool::new(false));
        let eventfd_for_thread = Arc::clone(&eventfd);
        let wake_pending_for_thread = Arc::clone(&wake_pending);
        spawn_reader_thread(
            reader_fd,
            lifecycle,
            tx,
            "forge-pty-reader",
            Arc::clone(&self.reader_cancelled),
            self.reader_cancel_eventfd.clone(),
            move || {
                notify_eventfd_once(&eventfd_for_thread, &wake_pending_for_thread);
            },
        );

        let on_exit = std::cell::Cell::new(Some(on_exit));

        unix_fd_add_local(eventfd.as_raw_fd(), move || {
            let _ = drain_eventfd(eventfd.as_raw_fd());

            // A producer may enqueue between the first empty read and clearing
            // `wake_pending`. Recheck after clearing so that transition cannot
            // lose its only eventfd notification.
            let message = match rx.try_recv() {
                Ok(message) => message,
                Err(mpsc::TryRecvError::Empty) => {
                    wake_pending.store(false, Ordering::Release);
                    match rx.try_recv() {
                        Ok(message) => {
                            wake_pending.store(true, Ordering::Release);
                            let _ = drain_eventfd(eventfd.as_raw_fd());
                            message
                        }
                        Err(mpsc::TryRecvError::Empty) => return true,
                        Err(mpsc::TryRecvError::Disconnected) => return false,
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => return false,
            };

            match message {
                PtyMsg::Data(data) => {
                    callback(data);
                    // Re-arm at idle priority rather than after a fixed sleep.
                    // This watch is itself a DEFAULT_IDLE source, so GTK's input
                    // handling (DEFAULT) and its frame clock (HIGH_IDLE + 20)
                    // both preempt the loop: the frame boundary comes from the
                    // priority that is already there instead of from a constant
                    // that has to guess one. The former fixed 8 ms re-arm handed
                    // the renderer at most one 32 KiB chunk per tick — a
                    // ~4 MiB/s ceiling that spent ~98% of each interval asleep,
                    // and it showed: measured over `seq 1 200000` in a
                    // 1600x1000 pane, the screen only reached a new frame every
                    // 6th capture frame (~200 ms of visibly frozen output at a
                    // time). Idle re-arm removes the gap and cuts the run from
                    // Enter to last repaint by about a quarter, with input
                    // latency under a 20 MiB stream unchanged.
                    rearm_dispatch(&eventfd, &wake_pending);
                    true
                }
                PtyMsg::Exit(code) => {
                    if let Some(f) = on_exit.take() {
                        f(code);
                    }
                    false
                }
            }
        });
    }

    fn start_reader_polling<F, E>(
        &self,
        reader_fd: OwnedFd,
        lifecycle: Arc<ChildLifecycle>,
        tx: mpsc::SyncSender<PtyMsg>,
        rx: mpsc::Receiver<PtyMsg>,
        mut callback: F,
        on_exit: E,
    ) where
        F: FnMut(Vec<u8>) + 'static,
        E: FnOnce(i32) + 'static,
    {
        spawn_reader_thread(
            reader_fd,
            lifecycle,
            tx,
            "forge-pty-reader-poll",
            Arc::clone(&self.reader_cancelled),
            self.reader_cancel_eventfd.clone(),
            || {},
        );

        let on_exit = std::cell::Cell::new(Some(on_exit));
        let rx = std::cell::RefCell::new(rx);

        // Idle priority, matching the eventfd watch above and the comment on
        // G_PRIORITY_DEFAULT_IDLE. A plain `timeout_add_local` runs at
        // G_PRIORITY_DEFAULT, so this path — reached exactly when `eventfd()`
        // fails, i.e. under descriptor pressure with many panes open — used to
        // preempt GTK's redraw and input dispatch on every tick.
        glib::timeout_add_local_full(
            PTY_DISPATCH_INTERVAL,
            glib::Priority::DEFAULT_IDLE,
            move || match rx.borrow().try_recv() {
                Ok(PtyMsg::Data(data)) => {
                    callback(data);
                    glib::ControlFlow::Continue
                }
                Ok(PtyMsg::Exit(code)) => {
                    if let Some(f) = on_exit.take() {
                        f(code);
                    }
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
            },
        );
    }
}

fn spawn_reader_thread(
    reader_fd: OwnedFd,
    lifecycle: Arc<ChildLifecycle>,
    tx: mpsc::SyncSender<PtyMsg>,
    thread_name: &'static str,
    reader_cancelled: Arc<AtomicBool>,
    mut reader_cancel_eventfd: Option<Arc<OwnedFd>>,
    notify: impl Fn() + Send + Clone + 'static,
) {
    let failure_tx = tx.clone();
    let failure_notify = notify.clone();
    let failure_lifecycle = Arc::clone(&lifecycle);
    if let Err(error) = std::thread::Builder::new()
        .name(thread_name.to_string())
        .spawn(move || {
            let mut file = std::fs::File::from(reader_fd);
            let fd = file.as_raw_fd();
            let mut buf = [0u8; PTY_READ_CHUNK_BYTES];
            loop {
                if reader_cancelled.load(Ordering::Acquire) {
                    break;
                }
                match poll_pty_or_reader_cancel(
                    fd,
                    reader_cancel_eventfd
                        .as_ref()
                        .map(|eventfd| eventfd.as_raw_fd()),
                ) {
                    Ok(ReaderPoll::PtyReady) => {}
                    Ok(ReaderPoll::Cancelled) => break,
                    Ok(ReaderPoll::TimedOut) => continue,
                    Ok(ReaderPoll::CancelUnavailable(error)) => {
                        log::warn!(
                            "PTY reader cancel eventfd unavailable; reverting to {} ms polling: {error}",
                            READER_CANCEL_FALLBACK_POLL_MS
                        );
                        reader_cancel_eventfd = None;
                        continue;
                    }
                    Err(error) => {
                        log::warn!("PTY reader poll stopped: {error}");
                        break;
                    }
                }
                match file.read(&mut buf) {
                    Ok(0) => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                    Ok(n) => {
                        let mut combined = Vec::with_capacity(PTY_READ_CHUNK_BYTES);
                        combined.extend_from_slice(&buf[..n]);
                        coalesce_pending(fd, &mut file, &mut buf, &mut combined);
                        if tx.send(PtyMsg::Data(combined)).is_err() {
                            return;
                        }
                        notify();
                    }
                }
            }

            let code = wait_for_child_exit(&lifecycle);
            if tx.send(PtyMsg::Exit(code)).is_ok() {
                notify();
            }
        })
    {
        log::error!("failed to spawn PTY reader thread '{thread_name}': {error}");
        // Nothing will ever read this PTY or observe the child's exit, so the
        // child must not merely be signaled: without the reap below it would
        // stay a zombie for the life of the process.
        failure_lifecycle.force_kill_and_reap();
        if failure_tx.try_send(PtyMsg::Exit(1)).is_ok() {
            failure_notify();
        }
    }
}

/// Wait for the child's terminal status after the PTY master reached end of
/// file.
///
/// The lifecycle is the process' single reaper for this child, so this polls
/// it instead of calling `waitpid` directly: a concurrent teardown may be
/// escalating on the same pid, and the reap must not race the signals. A
/// status always arrives — whoever runs the escalation ladder reaps at the end
/// of it, and an untouched child is reaped here on its own exit.
///
/// EOF normally precedes the shell's exit by milliseconds. A child that keeps
/// the pid alive after its own PTY reached EOF — stopped by SIGSTOP, or a
/// detached process that inherited the slave — used to park this thread for
/// the life of the process, and with it the `tx`, the queue, the GLib source
/// (which only removes itself on `Disconnected`) and every Rc the engine
/// callback captured: the pane's VTE, block list and history. After five
/// seconds, terminate the child through the same session-drain ladder a pane
/// close uses; the status then arrives like any other.
fn wait_for_child_exit(lifecycle: &Arc<ChildLifecycle>) -> i32 {
    let started = std::time::Instant::now();
    let mut termination_requested = false;
    loop {
        if let Some(code) = lifecycle.poll_reap() {
            return code;
        }
        if !termination_requested && started.elapsed() >= CHILD_REAP_ESCALATION_AFTER {
            log::warn!(
                "PTY reader reached EOF but child {} is still alive; terminating it",
                lifecycle.pid()
            );
            lifecycle.terminate(TERMINAL_ESCALATION);
            termination_requested = true;
        }
        std::thread::sleep(CHILD_REAP_POLL_INTERVAL);
    }
}

/// Abandon a PTY setup that failed after `fork` already succeeded.
///
/// Every early return between a successful fork and a working reader thread
/// goes through here. The child is running with no reader, no writer, and no
/// owner, so signaling it is not enough: only the reap keeps the failure from
/// leaking a zombie that lives as long as the application.
fn abort_spawn(lifecycle: &ChildLifecycle, error: io::Error) -> io::Error {
    lifecycle.force_kill_and_reap();
    error
}

/// Reap a freshly forked child that could not be given a [`ChildLifecycle`].
///
/// This is the one place a raw signal is still correct: the pid is an unreaped
/// child of this process, so the kernel cannot have handed the number to
/// anybody else, and no lifecycle exists yet to route through. Blocking is
/// fine — the caller is already returning an error out of `spawn`.
///
/// A child that already ran `setsid` leads its own group, and that group can
/// hold more than the shell — background jobs, a relay pipeline. The whole
/// group must go down with it, so nothing outlives a failed spawn; the
/// `getpgid` guard is what keeps the group signal from ever reaching this
/// process' own group while the child is still between fork and `setsid`.
fn kill_and_reap_unreferenced(child: Pid) {
    let pid = child.as_raw();
    unsafe {
        if libc::getpgid(pid) == pid {
            libc::kill(-pid, libc::SIGKILL);
        }
        libc::kill(pid, libc::SIGKILL);
    }
    let _ = nix::sys::wait::waitpid(child, None);
}

/// Wall-clock a reader thread may spend merging follow-up reads into one
/// delivered chunk. Bounds reader dwell for a producer that never stops being
/// readable; `PTY_READ_CHUNK_BYTES` is what ends the loop for everything else.
const PTY_COALESCE_BUDGET: std::time::Duration = std::time::Duration::from_millis(2);

/// Merge bytes already waiting on the PTY into one bounded delivery. This
/// reduces GTK crossings for programs that emit a repaint in several writes.
///
/// The bound that matters is `PTY_READ_CHUNK_BYTES` — one delivered chunk, one
/// main-thread callback — plus a small wall-clock budget so a firehose cannot
/// park the reader here. It used to be a count of follow-up reads instead, and
/// that count, not the byte cap, is what every chunk actually hit: a line-
/// buffered writer such as `seq` emits one small write per line, so nine reads
/// delivered ~700 bytes and the 32 KiB cap was never reached. Every per-chunk
/// fixed cost in the pipeline — the GLib idle round trip and the source churn
/// it causes, the chunk `Vec`, the parse pass, the activity fan-out, the
/// `vte_terminal_feed` call — was therefore paid about twelve times more often
/// than the design intended.
///
/// Every poll still waits a millisecond, exactly as before — that is what
/// merges the writes of a producer the reader can outrun, which is most of
/// them: a paced writer leaves the tty buffer momentarily empty between writes,
/// and a zero-timeout follow-up poll would stop coalescing at the first such
/// gap and deliver SMALLER chunks than the old loop did. The budget, not the
/// poll timeout, is what bounds the dwell, and at 2 ms it is a quarter of the
/// 8 ms the old nine-poll loop could spend.
fn coalesce_pending(fd: RawFd, file: &mut std::fs::File, buf: &mut [u8], combined: &mut Vec<u8>) {
    let started = std::time::Instant::now();
    while combined.len() < PTY_READ_CHUNK_BYTES && started.elapsed() < PTY_COALESCE_BUDGET {
        let mut poll_fd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut poll_fd, 1, 1) };
        if ready <= 0 || (poll_fd.revents & libc::POLLIN) == 0 {
            break;
        }

        let remaining = PTY_READ_CHUNK_BYTES - combined.len();
        let read_len = remaining.min(buf.len());
        match file.read(&mut buf[..read_len]) {
            Ok(0) => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
            Ok(read) => combined.extend_from_slice(&buf[..read]),
        }
    }
}

/// Ask the dispatch watch to look at the queue again once GTK has run
/// everything more urgent.
///
/// Signalling the eventfd from inside its own callback is enough: the watch is
/// level-triggered on readability, so GLib sees it ready on the very next poll,
/// and because the watch sits at `DEFAULT_IDLE` the main loop still dispatches
/// every ready input event (`DEFAULT`) and frame-clock tick (`HIGH_IDLE + 20`)
/// ahead of it — GLib only dispatches the highest-priority band that is ready.
/// This used to hop through a throwaway `idle_add_local_once` at the same
/// priority, which bought nothing the priority does not already guarantee and
/// cost one GSource create + dispatch + destroy and one extra main-loop
/// iteration per delivered chunk. With ~2000 chunks in a 1.3 MB stream, that
/// churn was the largest userspace cost left after the paint fixes
/// (`g_source_ref` / `g_source_iter_next` / `g_source_unref_internal`).
///
/// `wake_pending` stays set, so the reader thread does not also signal and
/// the eventfd counter cannot run away. If the signal fails the flag is
/// cleared, which hands the next wakeup back to the reader thread rather than
/// leaving the queue armed with nobody scheduled to drain it.
fn rearm_dispatch(eventfd: &Arc<OwnedFd>, wake_pending: &Arc<AtomicBool>) {
    if signal_eventfd(eventfd.as_raw_fd()).is_err() {
        wake_pending.store(false, Ordering::Release);
    }
}

fn notify_eventfd_once(eventfd: &OwnedFd, wake_pending: &AtomicBool) {
    if !wake_pending.swap(true, Ordering::AcqRel) && signal_eventfd(eventfd.as_raw_fd()).is_err() {
        // Do not leave the queue permanently armed without a kernel wakeup.
        // EINTR is retried below; this covers any other write failure.
        wake_pending.store(false, Ordering::Release);
    }
}

fn drain_eventfd(eventfd: RawFd) -> io::Result<()> {
    let mut value = 0u64;
    loop {
        // SAFETY: eventfd reads exactly one native u64. The descriptor is
        // nonblocking, so a raced or redundant drain is harmless.
        let read = unsafe {
            libc::read(
                eventfd,
                (&mut value as *mut u64).cast::<libc::c_void>(),
                std::mem::size_of::<u64>(),
            )
        };
        if read == std::mem::size_of::<u64>() as isize {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if error.kind() == io::ErrorKind::WouldBlock {
            return Ok(());
        }
        return Err(error);
    }
}

fn signal_eventfd(eventfd: RawFd) -> io::Result<()> {
    let value = 1u64;
    loop {
        // SAFETY: eventfd writes exactly one native u64. EAGAIN is harmless:
        // it means a kernel wakeup is already pending in the counter.
        let written = unsafe {
            libc::write(
                eventfd,
                (&value as *const u64).cast::<libc::c_void>(),
                std::mem::size_of::<u64>(),
            )
        };
        if written == std::mem::size_of::<u64>() as isize {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if error.kind() == io::ErrorKind::WouldBlock {
            return Ok(());
        }
        return Err(error);
    }
}

/// Test-only PTY plumbing, so the block reader's dispatch can be driven
/// without a shell (or a display) behind it.
#[cfg(test)]
impl OwnedPty {
    /// A PTY over a bare `openpty` pair with no shell behind it.
    ///
    /// Nothing runs on the slave end; it is held open only so master-side
    /// writes (the reader's protocol replies) reach a kernel buffer instead of
    /// failing with `EIO`, and put into raw non-blocking mode so
    /// [`Self::drain_test_slave`] can read them back. A placeholder child is
    /// forked and exits immediately
    /// because [`ChildLifecycle`] must own a real pid — it is what `Drop`
    /// signals and reaps, and a production `OwnedPty` never exists without one.
    /// The child sets its own process group first, so the ladder's group
    /// signal cannot reach the test runner.
    ///
    /// `foreground` is what [`OwnedPty::foreground_owner`] answers: a bare
    /// pair has no session, so the real `tcgetpgrp` probe could only ever
    /// return [`PtyForeground::Unknown`], which would silence every
    /// foreground-gated decision under test.
    pub(crate) fn for_tests(foreground: PtyForeground) -> io::Result<Self> {
        let OpenptyResult { master, slave } = openpty(None, None).map_err(io::Error::other)?;
        prepare_test_slave(&slave);
        // SAFETY: the child performs only async-signal-safe syscalls before
        // `_exit`, which is the rule for forking from this multi-threaded
        // process (the same rule `spawn_inner`'s child body follows).
        let child = match unsafe { unistd::fork() }.map_err(io::Error::other)? {
            ForkResult::Child => unsafe {
                libc::setpgid(0, 0);
                libc::_exit(0);
            },
            ForkResult::Parent { child } => child,
        };
        let lifecycle = match ChildLifecycle::new(child.as_raw(), ReapOwner::Ours) {
            Ok(lifecycle) => lifecycle,
            Err(error) => {
                kill_and_reap_unreferenced(child);
                return Err(error);
            }
        };
        let writer_fd = match master.try_clone() {
            Ok(fd) => fd,
            Err(error) => return Err(abort_spawn(&lifecycle, error)),
        };
        let input_tx = match spawn_fd_writer(writer_fd) {
            Ok(tx) => tx,
            Err(error) => return Err(abort_spawn(&lifecycle, error)),
        };
        Ok(OwnedPty {
            master: std::sync::Arc::new(std::sync::Mutex::new(Some(master))),
            input_tx: std::sync::Mutex::new(Some(input_tx)),
            lifecycle,
            reader_cancelled: Arc::new(AtomicBool::new(false)),
            reader_cancel_eventfd: create_reader_cancel_eventfd(),
            input_guard: std::sync::Mutex::new(InputGuard::new()),
            input_error_reported: AtomicBool::new(false),
            shell_bracketed_paste: AtomicBool::new(false),
            shell_integration_token: None,
            test_slave: Some(slave),
            test_foreground: std::sync::Mutex::new(Some(foreground)),
        })
    }

    /// Move terminal ownership, the way a foreground job returning control
    /// does. Test-only counterpart of [`OwnedPty::foreground_owner`].
    #[cfg(test)]
    pub(crate) fn set_test_foreground(&self, foreground: PtyForeground) {
        *self
            .test_foreground
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(foreground);
    }

    /// Read whatever the master side has already been written, waiting up to
    /// `wait` for the first byte to appear.
    ///
    /// Two reasons a test PTY needs this. A protocol reply is written by the
    /// background writer thread, so a test that wants to observe one has to
    /// wait for it rather than assume it landed; and nothing else drains the
    /// slave's input queue, so a test that provoked replies past the kernel
    /// buffer would wedge that writer thread instead of failing.
    pub(crate) fn drain_test_slave(&self, wait: std::time::Duration) -> Vec<u8> {
        let Some(slave) = self.test_slave.as_ref() else {
            return Vec::new();
        };
        let fd = slave.as_raw_fd();
        let deadline = std::time::Instant::now() + wait;
        let mut drained = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            // SAFETY: `buffer` is a live, correctly sized local and `fd` is
            // owned by `self` for the duration of the call.
            let read =
                unsafe { libc::read(fd, buffer.as_mut_ptr().cast::<libc::c_void>(), buffer.len()) };
            if read > 0 {
                drained.extend_from_slice(&buffer[..read as usize]);
                continue;
            }
            if read == 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            // Nothing queued yet: keep waiting only while nothing at all has
            // arrived. Once a reply has started, one drained read is the whole
            // of it — `write_all_fd` hands the kernel a reply in one call.
            if error.kind() == io::ErrorKind::WouldBlock
                && drained.is_empty()
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
            break;
        }
        drained
    }
}

/// Put a bare test slave into raw, non-blocking mode.
///
/// A slave left in its default canonical mode buffers master-side writes until
/// a line delimiter, and terminal protocol replies carry none — a reader would
/// see nothing at all. Raw mode also drops `ECHO`, so replies are not mirrored
/// back into the master's own read queue. Best-effort: a failure here only
/// costs a test its ability to observe replies, never correctness of the code
/// under test.
#[cfg(test)]
fn prepare_test_slave(slave: &OwnedFd) {
    let fd = slave.as_raw_fd();
    // SAFETY: `fd` is owned by the caller for the duration of the call and
    // `attrs` is a live local of the exact type these calls expect.
    unsafe {
        let mut attrs: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut attrs) == 0 {
            libc::cfmakeraw(&mut attrs);
            let _ = libc::tcsetattr(fd, libc::TCSANOW, &attrs);
        }
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 {
            let _ = libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
}

impl Drop for OwnedPty {
    fn drop(&mut self) {
        request_reader_cancel(
            &self.reader_cancelled,
            self.reader_cancel_eventfd.as_deref(),
        );
        self.close_master_fd();
        self.close_input_writer();
        // A pane closed explicitly already started this; `terminate` reports
        // that and does nothing. Whatever runs the ladder also reaps the
        // child, and the lifecycle's own drop is the backstop if nothing did.
        self.lifecycle.terminate(TERMINAL_ESCALATION);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::time::{Duration, Instant};

    #[cfg(target_os = "linux")]
    #[test]
    fn reader_cancel_eventfd_keeps_idle_poll_asleep_and_wakes_it() {
        let (reader, _writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let cancel_eventfd = create_reader_cancel_eventfd().expect("create cancel eventfd");
        let cancel_for_reader = Arc::clone(&cancel_eventfd);
        let (started_tx, started_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            result_tx
                .send(poll_pty_or_reader_cancel(
                    reader.as_raw_fd(),
                    Some(cancel_for_reader.as_raw_fd()),
                ))
                .unwrap();
        });

        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        match result_rx.recv_timeout(Duration::from_millis(120)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(result) => {
                reader.join().unwrap();
                panic!("idle reader poll returned without input or cancellation: {result:?}");
            }
            Err(error) => {
                reader.join().unwrap();
                panic!("reader poll result channel failed: {error}");
            }
        }

        let cancelled = AtomicBool::new(false);
        request_reader_cancel(&cancelled, Some(&cancel_eventfd));
        let result = result_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("cancel eventfd must wake the idle reader poll");
        reader.join().unwrap();
        assert!(cancelled.load(Ordering::Acquire));
        assert!(matches!(result, Ok(ReaderPoll::Cancelled)));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn invalid_cancel_fd_downgrades_to_bounded_polling() {
        let (reader, _writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let result = poll_pty_or_reader_cancel(reader.as_raw_fd(), Some(i32::MAX)).unwrap();
        assert!(matches!(result, ReaderPoll::CancelUnavailable(_)));

        let started = Instant::now();
        let result = poll_pty_or_reader_cancel(reader.as_raw_fd(), None).unwrap();
        assert!(matches!(result, ReaderPoll::TimedOut));
        assert!(started.elapsed() >= Duration::from_millis(40));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reader_poll_reports_pty_hangup_as_readable() {
        let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let cancel_eventfd = create_reader_cancel_eventfd().expect("create cancel eventfd");
        drop(writer);

        let result =
            poll_pty_or_reader_cancel(reader.as_raw_fd(), Some(cancel_eventfd.as_raw_fd()))
                .unwrap();
        assert!(matches!(result, ReaderPoll::PtyReady));
    }

    /// The strict child-environment API requires the process-global one-shot
    /// capture `app::run` performs first. Tests share one process, so a
    /// capture already done by an earlier test (`AlreadyExists`) is fine.
    fn ensure_environment_captured() {
        match jterm_core::child_env::capture_inherited_environment() {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => panic!("capture inherited environment: {err}"),
        }
    }

    /// Forge wiring around core's freeze: a variable written into the process
    /// environment *after* the capture (what CLI's `FORGE_*` and the
    /// input-method `GTK_*` rewrites are in the app) must not reach the child.
    #[test]
    fn child_environment_excludes_variables_set_after_the_capture() {
        ensure_environment_captured();
        assert!(jterm_core::child_env::inherited_environment_is_captured());
        unsafe { std::env::set_var("FORGE_POST_CAPTURE_TEST", "1") };
        let block = child_environment(&[]).expect("captured environment builds");
        unsafe { std::env::remove_var("FORGE_POST_CAPTURE_TEST") };
        assert!(
            block
                .iter()
                .all(|entry| !entry.as_bytes().starts_with(b"FORGE_POST_CAPTURE_TEST=")),
            "a post-capture variable leaked into the child environment"
        );
    }

    #[test]
    fn foreground_owner_never_conflates_probe_failure_with_the_shell() {
        assert_eq!(classify_foreground(42, 42), PtyForeground::Shell);
        assert_eq!(classify_foreground(42, 43), PtyForeground::Other);
        assert_eq!(classify_foreground(-1, 42), PtyForeground::Unknown);
        assert_eq!(classify_foreground(42, -1), PtyForeground::Unknown);
        assert_eq!(classify_foreground(0, 0), PtyForeground::Unknown);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn token_pipe_is_private_bounded_and_above_standard_io() {
        let (token, read_fd, write_fd) = shell_token_channel()
            .expect("create token pipe")
            .expect("Linux provides getrandom and pipe2");
        assert_eq!(token.len(), 32);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(read_fd.as_raw_fd() > libc::STDERR_FILENO);
        assert!(write_fd.as_raw_fd() > libc::STDERR_FILENO);

        let mut writer = File::from(write_fd);
        writer.write_all(format!("{token}\n").as_bytes()).unwrap();
        drop(writer);
        let mut reader = File::from(read_fd);
        let mut delivered = String::new();
        reader.read_to_string(&mut delivered).unwrap();
        assert_eq!(delivered, format!("{token}\n"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn bundled_bash_consumes_fd_and_announces_the_matching_token() {
        ensure_environment_captured();
        let integration = format!(
            "{}/scripts/shell-integration/forge.bash",
            env!("CARGO_MANIFEST_DIR")
        );
        let pty = OwnedPty::spawn_with_shell_token(
            &[
                "/bin/bash",
                "--noprofile",
                "--rcfile",
                integration.as_str(),
                "-i",
            ],
            None,
            &[("PS1", "forge-fd-test$ ")],
            true,
        )
        .expect("spawn token-aware bash");
        let token = pty
            .shell_integration_token()
            .expect("token was issued")
            .to_string();
        let ready = format!("\x1b]7771;{token}\x07");
        let output = read_until(
            pty.master_fd_raw(),
            ready.as_bytes(),
            Duration::from_secs(5),
        );
        assert!(
            output
                .windows(ready.len())
                .any(|window| window == ready.as_bytes()),
            "integration did not announce the issued token: {:?}",
            String::from_utf8_lossy(&output)
        );

        pty.write_bytes(
            b"env | grep -Eq '^FORGE_SHELL_INTEGRATION_(FD|TOKEN)=' && echo FORGE_TOKEN_LEAK || echo FORGE_TOKEN_CLEAN\r",
        )
        .expect("queue environment check");
        let output = read_until(
            pty.master_fd_raw(),
            b"FORGE_TOKEN_CLEAN",
            Duration::from_secs(5),
        );
        assert!(
            output
                .windows(b"FORGE_TOKEN_CLEAN".len())
                .any(|window| window == b"FORGE_TOKEN_CLEAN"),
            "integration token metadata leaked into commands: {:?}",
            String::from_utf8_lossy(&output)
        );
        pty.kill();
    }

    /// True once `pid` is no longer a reapable child of this process, i.e. it
    /// left no zombie behind.
    fn is_fully_reaped(pid: i32) -> bool {
        let mut status: libc::c_int = 0;
        // SAFETY: `status` is a live c_int for the duration of the call.
        let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        waited < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD)
    }

    #[test]
    fn input_queue_rejects_whole_messages_when_full_or_oversized() {
        let (sender, receiver) = mpsc::sync_channel(1);
        assert_eq!(try_enqueue_input(&sender, b"first".to_vec()), Ok(()));
        assert_eq!(
            try_enqueue_input(&sender, b"second-secret".to_vec()),
            Err(PtyWriteError::QueueFull { bytes: 13 })
        );
        assert_eq!(receiver.try_recv().unwrap(), b"first");
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        let oversized = vec![b'x'; MAX_PTY_INPUT_MESSAGE_BYTES + 1];
        assert_eq!(
            try_enqueue_input(&sender, oversized),
            Err(PtyWriteError::TooLarge {
                bytes: MAX_PTY_INPUT_MESSAGE_BYTES + 1,
                limit: MAX_PTY_INPUT_MESSAGE_BYTES,
            })
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn rejected_paste_frame_transition_rolls_back_before_the_next_write() {
        let modes = PasteModes { bracketed: false };
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.try_send(b"occupied".to_vec()).unwrap();
        let mut guard = InputGuard::new();

        assert!(matches!(
            filter_and_enqueue_input(&mut guard, &sender, b"\x1b[200~", modes),
            Err(PtyWriteError::QueueFull { .. })
        ));
        assert!(!guard.in_frame());
        assert_eq!(receiver.try_recv().unwrap(), b"occupied");

        filter_and_enqueue_input(&mut guard, &sender, b"one\ntwo", modes).unwrap();
        assert_eq!(receiver.try_recv().unwrap(), b"one");
        assert!(!guard.in_frame());

        drop(receiver);
        assert!(matches!(
            filter_and_enqueue_input(&mut guard, &sender, b"\x1b[200~", modes),
            Err(PtyWriteError::Closed { .. })
        ));
        assert!(!guard.in_frame());
    }

    #[test]
    fn observed_admission_matches_the_bytes_that_crossed_the_filter() {
        let (sender, receiver) = mpsc::sync_channel(4);
        let mut guard = InputGuard::new();

        let unframed = filter_and_enqueue_input_with(
            &mut guard,
            &sender,
            b"one\rtwo",
            PasteModes { bracketed: false },
            jterm_core::pty_input::admitted_input,
        )
        .unwrap();
        assert_eq!(receiver.try_recv().unwrap(), b"one");
        assert_eq!(unframed.editor_bytes, b"one");
        assert!(!unframed.submits_line);

        let bracketed = filter_and_enqueue_input_with(
            &mut guard,
            &sender,
            b"one\ntwo",
            PasteModes { bracketed: true },
            jterm_core::pty_input::admitted_input,
        )
        .unwrap();
        assert_eq!(receiver.try_recv().unwrap(), b"\x1b[200~one\ntwo\x1b[201~");
        assert_eq!(bracketed.editor_bytes, b"one\ntwo");
        assert!(bracketed.had_framing);
        assert!(!bracketed.submits_line);

        let opener = filter_and_enqueue_input_with(
            &mut guard,
            &sender,
            b"\x1b[200~",
            PasteModes { bracketed: true },
            jterm_core::pty_input::admitted_input,
        )
        .unwrap();
        assert_eq!(receiver.try_recv().unwrap(), b"\x1b[200~");
        assert!(opener.editor_bytes.is_empty());
        let framed_body = filter_and_enqueue_input_with(
            &mut guard,
            &sender,
            b"three\nfour",
            PasteModes { bracketed: true },
            jterm_core::pty_input::admitted_input,
        )
        .unwrap();
        assert_eq!(receiver.try_recv().unwrap(), b"three\nfour");
        assert_eq!(framed_body.editor_bytes, b"three\nfour");
        assert!(framed_body.had_framing);
        assert!(!framed_body.submits_line);
        filter_and_enqueue_input(
            &mut guard,
            &sender,
            b"\x1b[201~",
            PasteModes { bracketed: true },
        )
        .unwrap();
        assert_eq!(receiver.try_recv().unwrap(), b"\x1b[201~");

        let filtered_empty = filter_and_enqueue_input_with(
            &mut guard,
            &sender,
            b"\x1b[201~",
            PasteModes { bracketed: false },
            jterm_core::pty_input::admitted_input,
        )
        .unwrap();
        assert_eq!(filtered_empty, AdmittedInput::default());
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn input_queue_reports_disconnect_without_exposing_payload() {
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(receiver);
        let error = try_enqueue_input(&sender, b"private command".to_vec()).unwrap_err();
        assert_eq!(error, PtyWriteError::Closed { bytes: 15 });
        let display = error.to_string();
        assert!(display.contains("15-byte"));
        assert!(!display.contains("private"));
    }

    fn wait_until(timeout: Duration, mut ready: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if ready() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Collect PTY output until `needle` shows up or the deadline passes. The
    /// caller asserts on the result, so a timeout returns what did arrive
    /// instead of failing here — that makes the assertion message useful.
    fn read_until(fd: RawFd, needle: &[u8], timeout: Duration) -> Vec<u8> {
        let deadline = Instant::now() + timeout;
        let mut output = Vec::new();
        while Instant::now() < deadline {
            let mut poll_fd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut poll_fd, 1, 100) } <= 0 {
                continue;
            }
            let mut buffer = [0u8; 256];
            let read =
                unsafe { libc::read(fd, buffer.as_mut_ptr().cast::<libc::c_void>(), buffer.len()) };
            if read < 0
                && matches!(
                    io::Error::last_os_error().kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                )
            {
                continue;
            }
            if read <= 0 {
                // EOF, or EIO once the child is gone: nothing more will arrive.
                break;
            }
            output.extend_from_slice(&buffer[..read as usize]);
            if output.windows(needle.len()).any(|window| window == needle) {
                break;
            }
        }
        output
    }

    /// A PTY setup that fails after the fork must not leak the child it
    /// already created: signaling it is not enough, somebody has to reap it.
    #[test]
    fn aborting_a_failed_spawn_reaps_the_forked_child() {
        let child = std::process::Command::new("sh")
            .args(["-c", "exec sleep 30"])
            .spawn()
            .expect("spawn test child");
        let pid = child.id() as i32;
        std::mem::forget(child); // The lifecycle owns waitpid from here on.
        let lifecycle = ChildLifecycle::new(pid, ReapOwner::Ours).expect("reference the child");

        let returned = abort_spawn(&lifecycle, io::Error::other("master.try_clone failed"));

        assert_eq!(returned.to_string(), "master.try_clone failed");
        assert_eq!(lifecycle.exit_code(), Some(128 + libc::SIGKILL));
        assert!(
            is_fully_reaped(pid),
            "the aborted spawn left a zombie behind for pid {pid}"
        );
    }

    /// An explicit pane close followed by the pane's own drop must run exactly
    /// one escalation ladder, and that one ladder must still reap the child.
    #[test]
    fn an_explicit_close_and_a_later_drop_share_one_teardown() {
        ensure_environment_captured();
        let pty = OwnedPty::spawn(&["/bin/sh", "-c", "exec sleep 30"], None, &[])
            .expect("spawn PTY child");
        let lifecycle = pty.lifecycle();
        let pid = lifecycle.pid();

        pty.kill();
        assert!(
            !lifecycle.terminate(TERMINAL_ESCALATION),
            "a second teardown must not start another escalation ladder"
        );
        drop(pty);
        assert!(
            !lifecycle.terminate(TERMINAL_ESCALATION),
            "the pane's own drop must not start another escalation ladder either"
        );

        assert!(
            wait_until(Duration::from_secs(5), || lifecycle.exit_code().is_some()),
            "the single escalation ladder never reaped the child"
        );
        assert!(
            is_fully_reaped(pid),
            "the closed pane left a zombie behind for pid {pid}"
        );
    }

    #[test]
    fn spawned_child_receives_prepared_environment_and_cwd() {
        ensure_environment_captured();
        let pty = OwnedPty::spawn(
            &["/bin/sh", "-c", "printf '%s|' \"$TERM_PROGRAM\"; pwd"],
            Some("/tmp"),
            &[("TERM_PROGRAM", "forge-test")],
        )
        .expect("spawn PTY child");
        let fd = pty.master_fd_raw();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut output = Vec::new();

        while std::time::Instant::now() < deadline {
            let mut poll_fd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll_fd, 1, 100) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                panic!("poll PTY output: {error}");
            }
            if ready == 0 {
                continue;
            }

            let mut buffer = [0u8; 256];
            let read =
                unsafe { libc::read(fd, buffer.as_mut_ptr().cast::<libc::c_void>(), buffer.len()) };
            if read > 0 {
                output.extend_from_slice(&buffer[..read as usize]);
                if output
                    .windows(b"forge-test|/tmp".len())
                    .any(|window| window == b"forge-test|/tmp")
                {
                    break;
                }
            } else if read < 0 {
                let error = io::Error::last_os_error();
                if !matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) && error.raw_os_error() != Some(libc::EIO)
                {
                    panic!("read PTY output: {error}");
                }
            }
        }

        assert!(
            output
                .windows(b"forge-test|/tmp".len())
                .any(|window| window == b"forge-test|/tmp"),
            "unexpected PTY output: {:?}",
            String::from_utf8_lossy(&output)
        );
        pty.kill();
        assert!(
            pty.input_tx.lock().expect("input sender").is_none(),
            "kill must disconnect the dedicated input writer"
        );
    }

    /// Restoring a session whose recorded directory was deleted or unmounted
    /// must still open the pane. Before the guard the `File::open` error walked
    /// out of `spawn` into the block-mode caller's `expect` and took the whole
    /// application down with it.
    #[test]
    fn a_vanished_working_directory_falls_back_to_the_application_directory() {
        ensure_environment_captured();
        let missing = format!("/tmp/forge-removed-worktree-{}", std::process::id());
        assert!(!Path::new(&missing).exists(), "test path must not exist");
        let application_directory = std::env::current_dir().expect("process working directory");
        let expected = application_directory.as_os_str().as_bytes();

        let pty = OwnedPty::spawn(&["/bin/sh", "-c", "pwd"], Some(missing.as_str()), &[])
            .expect("a deleted working directory must not fail the spawn");
        let output = read_until(pty.master_fd_raw(), expected, Duration::from_secs(5));
        pty.kill();

        assert!(
            output
                .windows(expected.len())
                .any(|window| window == expected),
            "child did not start in the application directory: {:?}",
            String::from_utf8_lossy(&output)
        );
    }

    #[test]
    fn an_unopenable_working_directory_yields_no_directory_handle() {
        assert!(open_working_directory(None).is_none());
        assert!(
            open_working_directory(Some("/tmp")).is_some(),
            "an ordinary directory must still be handed to fchdir"
        );
        assert!(
            open_working_directory(Some("/tmp/forge-unmounted-drive")).is_none(),
            "a path that disappeared after the probe must not fail the spawn"
        );
    }

    #[test]
    fn an_unavailable_working_directory_is_not_requested_from_the_child() {
        assert_eq!(usable_working_directory(None), None);
        assert_eq!(usable_working_directory(Some("")), None);
        assert_eq!(usable_working_directory(Some("/tmp")), Some("/tmp"));
        assert_eq!(
            usable_working_directory(Some("/tmp/forge-removed-worktree")),
            None
        );
    }

    /// Updated in round 8: the shared resolver checks the execute bit before
    /// fork, so a shell that exists but cannot run fails the spawn with a
    /// readable error instead of becoming a child that exits 126 silently.
    #[test]
    fn a_non_executable_program_fails_the_spawn_before_fork() {
        assert!(Path::new("/etc/passwd").exists());
        let error = match OwnedPty::spawn(&["/etc/passwd"], None, &[]) {
            Ok(pty) => {
                pty.kill();
                panic!("a non-executable file must not be spawned");
            }
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    /// `kill_and_reap_unreferenced` runs while the child may already lead its
    /// own group; the fixture is the same `sh -c "sleep 30 & wait"` shape the
    /// remote_fs timeout test uses to prove a background job cannot outlive
    /// its leader's kill.
    #[test]
    fn kill_and_reap_unreferenced_takes_the_whole_group_down() {
        use std::os::unix::process::CommandExt as _;

        let pid_file = std::env::temp_dir().join(format!(
            "forge-pty-group-kill-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let command = format!("sleep 30 & echo $! > {}; wait", pid_file.display());
        let child = unsafe {
            std::process::Command::new("sh")
                .arg("-c")
                .arg(&command)
                .pre_exec(|| {
                    // Match the spawn path: the child leads its own session
                    // and process group before the exec.
                    if libc::setsid() < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                })
                .spawn()
                .expect("spawn the group fixture")
        };
        let leader = Pid::from_raw(child.id() as i32);

        // Wait until the background job exists and its pid is on disk.
        let deadline = Instant::now() + Duration::from_secs(5);
        let background = loop {
            if let Ok(contents) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = contents.trim().parse::<i32>() {
                    break pid;
                }
            }
            assert!(
                Instant::now() < deadline,
                "the fixture never reported its background job"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            unsafe { libc::getpgid(background) },
            leader.as_raw(),
            "the fixture's background job must share the leader's process group"
        );

        kill_and_reap_unreferenced(leader);
        let _ = std::fs::remove_file(&pid_file);

        assert!(
            matches!(
                nix::sys::wait::waitpid(leader, None),
                Err(nix::errno::Errno::ECHILD)
            ),
            "the leader must already be reaped"
        );
        // SIGKILL reaches the whole group at once; only init's reap of the
        // orphaned zombie may lag, so give the disappearance a bounded wait.
        let deadline = Instant::now() + Duration::from_secs(5);
        while unsafe { libc::kill(background, 0) } == 0 {
            assert!(
                Instant::now() < deadline,
                "the background job survived the leader's group kill"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn complete_writer_delivers_the_entire_payload() {
        let payload_len = 128 * 1024;
        let (mut reader, writer) = UnixStream::pair().expect("create socket pair");

        let handle = std::thread::spawn(move || {
            let payload = vec![0x5a; payload_len];
            write_all_fd(writer.as_raw_fd(), &payload).expect("write payload");
        });

        let mut received = vec![0; payload_len];
        reader.read_exact(&mut received).expect("read payload");
        handle.join().expect("writer thread");
        assert!(received.iter().all(|byte| *byte == 0x5a));
    }

    #[test]
    fn eventfd_wakeup_is_coalesced_until_consumer_rearms() {
        let raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
        assert!(raw >= 0);
        let eventfd = unsafe { OwnedFd::from_raw_fd(raw) };
        let wake_pending = AtomicBool::new(false);

        notify_eventfd_once(&eventfd, &wake_pending);
        notify_eventfd_once(&eventfd, &wake_pending);

        let mut value = 0u64;
        let read = unsafe {
            libc::read(
                eventfd.as_raw_fd(),
                (&mut value as *mut u64).cast::<libc::c_void>(),
                std::mem::size_of::<u64>(),
            )
        };
        assert_eq!(read as usize, std::mem::size_of::<u64>());
        assert_eq!(value, 1);

        wake_pending.store(false, Ordering::Release);
        notify_eventfd_once(&eventfd, &wake_pending);
        value = 0;
        let read = unsafe {
            libc::read(
                eventfd.as_raw_fd(),
                (&mut value as *mut u64).cast::<libc::c_void>(),
                std::mem::size_of::<u64>(),
            )
        };
        assert_eq!(read as usize, std::mem::size_of::<u64>());
        assert_eq!(value, 1);
    }

    /// The PTY boundary is now `jterm_core::pty_input::InputGuard`; these tests
    /// pin the wiring (which policy this repo passes) rather than re-testing
    /// the shared filter, whose own suite lives in jterm_core.
    fn guarded(chunks: &[&[u8]], bracketed: bool) -> Vec<Vec<u8>> {
        let mut guard = InputGuard::new();
        let modes = PasteModes { bracketed };
        chunks
            .iter()
            .map(|chunk| guard.filter(chunk, modes, boundary_paste_policy()).to_vec())
            .collect()
    }

    #[test]
    fn unframed_multiline_insert_falls_back_without_shell_support() {
        assert_eq!(
            guarded(&[b"echo first\necho second"], false),
            vec![b"echo first".to_vec()]
        );
        assert_eq!(
            guarded(&[b"echo first\r\necho second"], false),
            vec![b"echo first".to_vec()]
        );
    }

    #[test]
    fn shell_supported_multiline_insert_is_automatically_bracketed() {
        assert_eq!(
            guarded(&[b"echo first\necho second"], true),
            vec![b"\x1b[200~echo first\necho second\x1b[201~".to_vec()]
        );
    }

    #[test]
    fn embedded_marker_cannot_bypass_unframed_multiline_protection() {
        let hostile = b"echo first\x1b[201~\necho second";
        assert_eq!(guarded(&[hostile], false), vec![b"echo first".to_vec()]);
        assert_eq!(
            guarded(&[hostile], true),
            vec![b"\x1b[200~echo first\necho second\x1b[201~".to_vec()]
        );
    }

    /// Updated in round 8: a trailing CR used to exempt the whole payload from
    /// the multiline check, so every earlier line of an unbracketed payload was
    /// submitted. The shared guard only exempts a payload whose *single* line was
    /// explicitly submitted.
    #[test]
    fn a_trailing_return_no_longer_exempts_earlier_lines() {
        assert_eq!(
            guarded(&[b"if true; then\necho ok\nfi\r"], false),
            vec![b"if true; then".to_vec()]
        );
        assert_eq!(
            guarded(&[b"git status\r"], false),
            vec![b"git status\r".to_vec()],
            "one submitted line is not multiline input"
        );
    }

    /// Updated in round 8: a lone LF used to be wrapped as insertion-only
    /// content. It is now the explicit submission it looks like, which is what
    /// the guard's single-line fast path means.
    #[test]
    fn enter_and_line_feed_reach_the_child_unchanged() {
        assert_eq!(guarded(&[b"\r"], true), vec![b"\r".to_vec()]);
        assert_eq!(guarded(&[b"\n"], true), vec![b"\n".to_vec()]);
    }

    #[test]
    fn bracketed_paste_preserves_multiline_body_across_writes() {
        let body = b"echo first\necho second";
        assert_eq!(
            guarded(
                &[
                    jterm_core::pty_input::PASTE_START,
                    body,
                    jterm_core::pty_input::PASTE_END
                ],
                false
            ),
            vec![
                jterm_core::pty_input::PASTE_START.to_vec(),
                body.to_vec(),
                jterm_core::pty_input::PASTE_END.to_vec(),
            ]
        );
    }

    /// The reason this boundary exists: a hostile clipboard that closes the
    /// frame early must not leave `rm -rf ~` behind as a typed command line,
    /// even when the frame's body arrives as its own write.
    #[test]
    fn an_embedded_terminator_is_removed_from_a_frame_body() {
        let filtered = guarded(
            &[
                jterm_core::pty_input::PASTE_START,
                b"docs\x1b[201~\rrm -rf ~\r",
                jterm_core::pty_input::PASTE_END,
            ],
            false,
        );
        assert_eq!(filtered[1], b"docs\rrm -rf ~\r".to_vec());
        assert_eq!(filtered[2], jterm_core::pty_input::PASTE_END.to_vec());
    }

    #[test]
    fn ordinary_single_line_input_is_unchanged() {
        assert_eq!(
            guarded(&[b"git status"], false),
            vec![b"git status".to_vec()]
        );
    }
}
