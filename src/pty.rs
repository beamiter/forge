use gtk4::glib;
use nix::libc;
use nix::pty::{openpty, OpenptyResult};
use nix::unistd::{self, ForkResult, Pid};
use std::ffi::CString;
use std::fmt;
use std::fs::File;
use std::io::{self, Read as _};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use crate::process::{ChildLifecycle, ReapOwner};
use crate::pty_input::{InputGuard, PasteModes, PastePolicy, UnbracketedMultiline};
use crate::terminal::TERMINAL_ESCALATION;

enum PtyMsg {
    Data(Vec<u8>),
    Exit(i32),
}

/// How often the reader thread asks the lifecycle for the child's status once
/// the PTY master has reached end of file.
const CHILD_REAP_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

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
/// Keep a continuously-ready PTY from monopolizing GTK's main loop. The first
/// chunk is dispatched immediately; queued follow-ups are paced at this rate.
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

fn invalid_nul(context: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{context} contains an embedded NUL byte"),
    )
}

/// The child's environment, built entirely before `fork`.
///
/// `LESS` is a flag rather than a hardcoded value because the family disagrees:
/// forge keeps `R` so a short `git log` still opens an interactive pager on the
/// alternate screen, while ember/frost use `FR`. Everything else — `TERM`,
/// `COLORTERM`, `TERM_PROGRAM`, `TERM_PROGRAM_VERSION`, `VTE_VERSION` — is the
/// family's shared identity policy. Block mode never lets libvte spawn the
/// child, so before this these variables (bar `TERM`) never reached the shell
/// and `bat`/`delta`/`lazygit` fell back to 256 colours.
fn child_environment(env_extra: &[(&str, &str)]) -> io::Result<Vec<CString>> {
    crate::child_env::envp(&block_mode_child_env(), env_extra)
}

fn block_mode_child_env() -> crate::child_env::ChildEnv<'static> {
    crate::child_env::ChildEnv {
        less_default: Some("R"),
        ..crate::child_env::ChildEnv::from_identity()
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
                crate::review_input::safe_inline_display(directory, 2 * 1024)
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
        let c_environment = child_environment(env_extra)?;
        let mut environment_ptrs: Vec<*const libc::c_char> =
            c_environment.iter().map(|entry| entry.as_ptr()).collect();
        environment_ptrs.push(std::ptr::null());
        let cwd_file = if host_bridge {
            None
        } else {
            open_working_directory(effective_cwd)
        };
        let cwd_fd = cwd_file.as_ref().map(AsRawFd::as_raw_fd).unwrap_or(-1);

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
                    input_guard: std::sync::Mutex::new(InputGuard::new()),
                    input_error_reported: AtomicBool::new(false),
                    shell_bracketed_paste: AtomicBool::new(false),
                })
            }
            Err(error) => Err(io::Error::other(error)),
        }
    }

    pub fn pid_i32(&self) -> i32 {
        self.lifecycle.pid()
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
        if data.len() > MAX_PTY_INPUT_MESSAGE_BYTES {
            return Err(PtyWriteError::TooLarge {
                bytes: data.len(),
                limit: MAX_PTY_INPUT_MESSAGE_BYTES,
            });
        }
        let modes = PasteModes {
            bracketed: self.shell_bracketed_paste(),
        };
        // A poisoned guard would mean another thread panicked mid-filter; keep
        // the choke point rather than letting writes bypass it.
        let safe_data = {
            let mut guard = self
                .input_guard
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard
                .filter(data, modes, boundary_paste_policy())
                .into_owned()
        };

        if safe_data.is_empty() {
            return Ok(());
        }
        if safe_data.len() > MAX_PTY_INPUT_MESSAGE_BYTES {
            return Err(PtyWriteError::TooLarge {
                bytes: safe_data.len(),
                limit: MAX_PTY_INPUT_MESSAGE_BYTES,
            });
        }
        let input_tx = self
            .input_tx
            .lock()
            .ok()
            .and_then(|sender| sender.as_ref().cloned());
        let Some(input_tx) = input_tx else {
            return Err(PtyWriteError::Closed {
                bytes: safe_data.len(),
            });
        };
        let result = try_enqueue_input(&input_tx, safe_data);
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
        spawn_reader_thread(reader_fd, lifecycle, tx, "forge-pty-reader", move || {
            notify_eventfd_once(&eventfd_for_thread, &wake_pending_for_thread);
        });

        let on_exit = std::cell::Cell::new(Some(on_exit));

        unix_fd_add_local(eventfd.as_raw_fd(), move || {
            drain_eventfd(eventfd.as_raw_fd());

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
                            drain_eventfd(eventfd.as_raw_fd());
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
                    let eventfd = Arc::clone(&eventfd);
                    glib::timeout_add_local_once(PTY_DISPATCH_INTERVAL, move || {
                        signal_eventfd(eventfd.as_raw_fd());
                    });
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
        spawn_reader_thread(reader_fd, lifecycle, tx, "forge-pty-reader-poll", || {});

        let on_exit = std::cell::Cell::new(Some(on_exit));
        let rx = std::cell::RefCell::new(rx);

        glib::timeout_add_local(PTY_DISPATCH_INTERVAL, move || {
            match rx.borrow().try_recv() {
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
            }
        });
    }
}

fn spawn_reader_thread(
    reader_fd: OwnedFd,
    lifecycle: Arc<ChildLifecycle>,
    tx: mpsc::SyncSender<PtyMsg>,
    thread_name: &'static str,
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
fn wait_for_child_exit(lifecycle: &ChildLifecycle) -> i32 {
    loop {
        if let Some(code) = lifecycle.poll_reap() {
            return code;
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
fn kill_and_reap_unreferenced(child: Pid) {
    let _ = nix::sys::signal::kill(child, nix::sys::signal::Signal::SIGKILL);
    let _ = nix::sys::wait::waitpid(child, None);
}

/// Merge bytes already waiting on the PTY into one bounded delivery. This
/// reduces GTK crossings for programs that emit a repaint in several writes.
fn coalesce_pending(fd: RawFd, file: &mut std::fs::File, buf: &mut [u8], combined: &mut Vec<u8>) {
    const MAX_FOLLOWUP_READS: u32 = 8;
    let mut follow_ups = 0u32;
    while combined.len() < PTY_READ_CHUNK_BYTES && follow_ups < MAX_FOLLOWUP_READS {
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
        follow_ups += 1;
    }
}

fn notify_eventfd_once(eventfd: &OwnedFd, wake_pending: &AtomicBool) {
    if !wake_pending.swap(true, Ordering::AcqRel) {
        signal_eventfd(eventfd.as_raw_fd());
    }
}

fn drain_eventfd(eventfd: RawFd) {
    let mut value = 0u64;
    unsafe {
        libc::read(
            eventfd,
            (&mut value as *mut u64).cast::<libc::c_void>(),
            std::mem::size_of::<u64>(),
        );
    }
}

fn signal_eventfd(efd: RawFd) {
    let val: u64 = 1;
    unsafe {
        libc::write(efd, &val as *const u64 as *const libc::c_void, 8);
    }
}

impl Drop for OwnedPty {
    fn drop(&mut self) {
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

    /// The PTY boundary is now `pty_input::InputGuard`; these tests pin the
    /// wiring (which policy this repo passes) rather than re-testing the shared
    /// filter, whose own suite lives in jterm_core.
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
                    crate::pty_input::PASTE_START,
                    body,
                    crate::pty_input::PASTE_END
                ],
                false
            ),
            vec![
                crate::pty_input::PASTE_START.to_vec(),
                body.to_vec(),
                crate::pty_input::PASTE_END.to_vec(),
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
                crate::pty_input::PASTE_START,
                b"docs\x1b[201~\rrm -rf ~\r",
                crate::pty_input::PASTE_END,
            ],
            false,
        );
        assert_eq!(filtered[1], b"docs\rrm -rf ~\r".to_vec());
        assert_eq!(filtered[2], crate::pty_input::PASTE_END.to_vec());
    }

    #[test]
    fn ordinary_single_line_input_is_unchanged() {
        assert_eq!(
            guarded(&[b"git status"], false),
            vec![b"git status".to_vec()]
        );
    }
}
