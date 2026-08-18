//! remote_fs — blocking local and remote filesystem access for the sidebar
//! file tree.
//!
//! Local entries come straight from `std::fs`. Remote entries come from the
//! hosts in `config.remote_hosts`: a small POSIX sh probe script is pushed to
//! the far side through the system `ssh` / `docker` binaries (no sshfs, no new
//! dependencies) and its byte output is parsed back here. This mirrors the
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

/// Hard cap on entries per directory listing, shared with the local scanner.
pub(crate) const MAX_DIRECTORY_ENTRIES: usize = 4_096;
/// Hard cap on one transfer payload. Exceeding it aborts the child, unlinks
/// the partial local file, and errors — never a silently truncated result.
pub(crate) const MAX_TRANSFER_BYTES: u64 = 512 * 1024 * 1024;
/// NAME_MAX on every Linux filesystem the sidebar can realistically browse.
const MAX_ENTRY_NAME_BYTES: usize = 255;
/// `list` output is bounded: 4096 entries of at most 255 name bytes each,
/// plus separators, plus headroom for a hostile far side.
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
/// cannot silently redirect it at another host.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum FsLocation {
    Local,
    Remote(usize),
}

impl FsLocation {
    /// Short label for the location selector: `Local`, or the host name
    /// prefixed by its transport (`ssh: ` / `docker: `).
    pub(crate) fn label(&self, hosts: &[RemoteHost]) -> String {
        match self {
            FsLocation::Local => "Local".to_string(),
            FsLocation::Remote(index) => match hosts.get(*index) {
                Some(host) => {
                    let transport = if host.docker { "docker" } else { "ssh" };
                    let name = jterm_core::review_input::safe_inline_display(&host.name, 256);
                    format!("{transport}: {name}")
                }
                None => "Remote (removed)".to_string(),
            },
        }
    }
}

/// A sidebar cut/copy payload. Same-location paste is a rename/copy; a
/// location mismatch turns the paste into a streaming transfer (download,
/// upload, or a temp-relayed remote-to-remote hop).
#[derive(Clone, Debug)]
pub(crate) struct FsClipboard {
    pub(crate) loc: FsLocation,
    pub(crate) path: PathBuf,
    pub(crate) is_dir: bool,
    pub(crate) cut: bool,
}

/// One directory entry, pre-display-sanitization. `name` is lossy-decoded for
/// display; `path` keeps the exact bytes so operations round-trip.
#[derive(Clone, Debug)]
pub(crate) struct FsEntry {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) is_dir: bool,
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
/// Wire protocol v2: `list` prints NUL-separated `<type>,<name>` pairs (types
/// `d`/`f`/`l`); `cat`/`tar` stream to stdout, `put`/`untar` consume stdin.
/// Exit codes are 0 ok, 2 usage/bad path, 3 cannot enter dir / not the
/// expected kind, 4 operation failed, 17 target exists. The v1 ops
/// (home/list/mkdir/mkfile/rm/mv/cp) are byte-identical to protocol v1.
const PROBE_SCRIPT: &str = r#"# remote-fs probe v2 — runs under `sh -c` as $0=probe, <op> [args...] as $1+.
# `list` stdout: NUL-separated pairs "<t>\0<name>\0", t in {d,f,l}, names relative.
# v2 adds streaming ops: cat (file -> stdout), put (stdin -> new file),
# tar (dir -> tar on stdout), untar (stdin tar -> existing dir).
# Exit codes: 0 ok, 2 usage/bad path, 3 cannot enter dir, 4 op failed, 17 target exists.
set -u
op=${1:-}
case "$op" in
  home)
    cd 2>/dev/null || cd / || exit 3
    pwd
    ;;
  list)
    d=${2:-}
    case "$d" in /*) ;; *) exit 2 ;; esac
    cd "$d" 2>/dev/null || exit 3
    for f in * .[!.]* ..?*; do
      if [ -d "$f" ]; then t=d
      elif [ -L "$f" ]; then t=l
      elif [ -e "$f" ]; then t=f
      else continue
      fi
      printf '%s\0%s\0' "$t" "$f"
    done
    ;;
  mkdir)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    [ -e "$p" ] && exit 17
    mkdir "$p" || exit 4
    ;;
  mkfile)
    p=${2:-}
    case "$p" in /*) ;; *) exit 2 ;; esac
    [ -e "$p" ] && exit 17
    : > "$p" || exit 4
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
    [ -e "$n" ] && exit 17
    mv "$s" "$n" || exit 4
    ;;
  cp)
    s=${2:-}; n=${3:-}
    case "$s" in /*) ;; *) exit 2 ;; esac
    case "$n" in /*) ;; *) exit 2 ;; esac
    [ -e "$n" ] && exit 17
    cp -a "$s" "$n" || exit 4
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
    [ -e "$p" ] && exit 17
    t="$p.fspart.$$"
    if ! cat > "$t"; then rm -f "$t"; exit 4; fi
    [ -e "$p" ] && { rm -f "$t"; exit 17; }
    mv "$t" "$p" || { rm -f "$t"; exit 4; }
    ;;
  tar)
    p=${2%/}
    case "$p" in /*?*) ;; *) exit 2 ;; esac
    [ -d "$p" ] || exit 3
    command -v tar >/dev/null 2>&1 || { echo "remote-fs: tar is not available" >&2; exit 4; }
    parent=${p%/*}
    tar cf - -C "${parent:-/}" "${p##*/}" || exit 4
    ;;
  untar)
    d=${2:-}
    case "$d" in /*) ;; *) exit 2 ;; esac
    [ -d "$d" ] || exit 3
    command -v tar >/dev/null 2>&1 || { echo "remote-fs: tar is not available" >&2; exit 4; }
    tar xf - -C "$d" || exit 4
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

/// Spawn a child from an argv vector with the given stdio arrangement.
fn spawn_argv(
    argv: &[String],
    stdin: Stdio,
    stdout: Stdio,
    stderr: Stdio,
) -> io::Result<std::process::Child> {
    let Some((program, args)) = argv.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty probe argv",
        ));
    };
    Command::new(program)
        .args(args)
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
}

/// Poll `child` to exit, killing and reaping it past the deadline. The
/// calling worker thread doubles as the watchdog, so a hung ssh or stopped
/// container cannot pin the thread forever.
fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> io::Result<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait(); // reap the killed child
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
/// bounded.
fn run_capture(
    argv: &[String],
    stdin_bytes: &[u8],
    timeout: Duration,
    max_out: u64,
) -> io::Result<Capture> {
    let mut child = spawn_argv(argv, Stdio::piped(), Stdio::piped(), Stdio::piped())?;

    // Feed stdin from a helper thread: a child that exits early fails the
    // write, and the detached thread then ends on its own.
    if let Some(mut stdin) = child.stdin.take() {
        let stdin_bytes = stdin_bytes.to_vec();
        let _ = std::thread::Builder::new()
            .name("forge-remote-fs-stdin".to_string())
            .spawn(move || {
                let _ = stdin.write_all(&stdin_bytes);
            });
    }

    let stdout_reader = spawn_bounded_reader(child.stdout.take(), max_out);
    let stderr_reader = spawn_bounded_reader(child.stderr.take(), max_out);

    let status = wait_with_timeout(&mut child, timeout);
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    let status = status?;
    Ok(Capture {
        status: status.code().unwrap_or(-1),
        stdout,
        stderr,
    })
}

/// Read a child pipe on its own thread, capped at `max_out` bytes. Once the
/// limit is reached the reader stops consuming; a child that keeps writing
/// then blocks on a full pipe until the watchdog kills it.
fn spawn_bounded_reader<R>(pipe: Option<R>, max_out: u64) -> std::thread::JoinHandle<Vec<u8>>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(pipe) = pipe {
            let _ = pipe.take(max_out).read_to_end(&mut buffer);
        }
        buffer
    })
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
    let argv = probe_argv(host, op, args);
    // The script rides in argv (`sh -c`), so stdin carries no payload here.
    let capture = run_capture(&argv, &[], timeout, max_out)?;
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
fn watchdog_streaming_child(
    child: &mut std::process::Child,
    rx: &mpsc::Receiver<io::Result<()>>,
    timeout: Duration,
) -> (i32, io::Result<()>) {
    let deadline = std::time::Instant::now() + timeout;
    let mut pump_outcome = None;
    loop {
        if pump_outcome.is_none() {
            if let Ok(outcome) = rx.try_recv() {
                if outcome.is_err() {
                    // The pump gave up: the child is stuck on a full pipe or
                    // a dead peer — kill it rather than waiting it out.
                    let _ = child.kill();
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
                        .recv()
                        .unwrap_or_else(|_| Err(io::Error::other("transfer pump died"))),
                };
                return (status.code().unwrap_or(-1), outcome);
            }
            Ok(None) => {}
            Err(error) => return (-1, Err(error)),
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = rx.recv();
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
) -> io::Result<()> {
    fail_if_exists(dst)?;
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
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let temp_guard = TempFileGuard(temp.clone());

    let mut child = spawn_argv(argv, Stdio::null(), Stdio::piped(), Stdio::piped())?;
    let stderr_reader = spawn_bounded_reader(child.stderr.take(), PROBE_OP_MAX_OUTPUT);
    let Some(mut stdout) = child.stdout.take() else {
        return Err(io::Error::other("could not open probe stdout"));
    };
    let (tx, rx) = mpsc::channel::<io::Result<()>>();
    std::thread::Builder::new()
        .name("forge-remote-fs-dl".to_string())
        .spawn(move || {
            let mut total = 0_u64;
            let mut buffer = vec![0_u8; STREAM_CHUNK];
            let result = loop {
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
                    }
                    Err(error) => break Err(error),
                }
            };
            let result = result.and_then(|total| file.sync_all().map(|_| total));
            let _ = tx.send(result.map(|_| ()));
        })
        .map_err(|error| io::Error::other(format!("could not start download streamer: {error}")))?;

    let (status, outcome) = watchdog_streaming_child(&mut child, &rx, timeout);
    probe_status_result(status, stderr_reader.join().unwrap_or_default())?;
    outcome?;
    finalize_download(&temp, dst)?;
    temp_guard.disarm();
    Ok(())
}

/// `rename` would silently clobber a `dst` created after the pre-stream
/// check, so re-check immediately before it. Called with the temp file still
/// guarded, so a failure here still unlinks the partial download.
fn finalize_download(temp: &Path, dst: &Path) -> io::Result<()> {
    fail_if_exists(dst)?;
    std::fs::rename(temp, dst)
}

/// Stream a local regular file into a probe's stdin (`put` payload). The
/// remote side's exit code takes precedence over local write errors: an
/// early `exit 17` surfaces as AlreadyExists, not as a broken pipe.
fn stream_upload_to_probe(
    argv: &[String],
    src: &Path,
    cap: u64,
    timeout: Duration,
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
    let mut file = std::fs::File::open(src)?;

    let mut child = spawn_argv(argv, Stdio::piped(), Stdio::null(), Stdio::piped())?;
    let stderr_reader = spawn_bounded_reader(child.stderr.take(), PROBE_OP_MAX_OUTPUT);
    let Some(mut stdin) = child.stdin.take() else {
        return Err(io::Error::other("could not open probe stdin"));
    };
    let (tx, rx) = mpsc::channel::<io::Result<()>>();
    std::thread::Builder::new()
        .name("forge-remote-fs-ul".to_string())
        .spawn(move || {
            let mut total = 0_u64;
            let mut buffer = vec![0_u8; STREAM_CHUNK];
            let result = loop {
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
                    }
                    Err(error) => break Err(error),
                }
            };
            let _ = tx.send(result.map(|_| ()));
        })
        .map_err(|error| io::Error::other(format!("could not start upload streamer: {error}")))?;

    let (status, outcome) = watchdog_streaming_child(&mut child, &rx, timeout);
    probe_status_result(status, stderr_reader.join().unwrap_or_default())?;
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
        name.to_string_lossy().into_owned(),
    ])
}

/// Pump `reader` → `writer` in chunks with a byte cap. On overflow the
/// `source_child` (the producing tar) is killed so it cannot linger blocked
/// on a full pipe; IO errors kill it too, then it is reaped.
fn pump_capped(
    mut reader: impl Read,
    mut writer: impl Write,
    mut source_child: std::process::Child,
    cap: u64,
    timeout: Duration,
) -> io::Result<()> {
    let mut total = 0_u64;
    let mut buffer = vec![0_u8; STREAM_CHUNK];
    let result = loop {
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
            }
            Err(error) => break Err(error),
        }
    };
    drop(writer); // EOF for the consumer
    if result.is_err() {
        let _ = source_child.kill();
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
pub(crate) fn download_file(host: &RemoteHost, src: &Path, dst: &Path) -> io::Result<()> {
    let argv = probe_argv(host, "cat", &[remote_path_arg(src)?]);
    stream_download_to_file(&argv, dst, MAX_TRANSFER_BYTES, TRANSFER_TIMEOUT)
}

/// Upload one regular local file to `dst` on a remote host, streaming. The
/// probe writes a temp file and renames it into place, re-checking existence
/// right before the rename.
pub(crate) fn upload_file(host: &RemoteHost, src: &Path, dst: &Path) -> io::Result<()> {
    let argv = probe_argv(host, "put", &[remote_path_arg(dst)?]);
    stream_upload_to_probe(&argv, src, MAX_TRANSFER_BYTES, TRANSFER_TIMEOUT)
}

/// Download a remote directory tree to `dst` (which must not exist): the
/// probe streams a tar of the directory and the local system tar extracts it
/// into `dst`'s parent. A partial extraction is removed on failure.
pub(crate) fn download_dir(host: &RemoteHost, src: &Path, dst: &Path) -> io::Result<()> {
    let argv = probe_argv(host, "tar", &[remote_path_arg(src)?]);
    fail_if_exists(dst)?;
    local_tar_available()?;
    let Some(parent) = dst.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "download target has no parent directory",
        ));
    };
    let local_argv = vec![
        "tar".to_string(),
        "xf".to_string(),
        "-".to_string(),
        "-C".to_string(),
        parent.to_string_lossy().into_owned(),
    ];

    let result = stream_download_dir(&argv, &local_argv, MAX_TRANSFER_BYTES, TRANSFER_TIMEOUT);
    if result.is_err() {
        // Anything at `dst` now is our partial extraction (it did not exist
        // before); remove it rather than leaving a half-tree behind.
        let _ = std::fs::remove_dir_all(dst);
    }
    result
}

fn stream_download_dir(
    argv: &[String],
    local_argv: &[String],
    cap: u64,
    timeout: Duration,
) -> io::Result<()> {
    let mut remote = spawn_argv(argv, Stdio::null(), Stdio::piped(), Stdio::piped())?;
    let remote_stderr = spawn_bounded_reader(remote.stderr.take(), PROBE_OP_MAX_OUTPUT);
    let mut local = spawn_argv(local_argv, Stdio::piped(), Stdio::null(), Stdio::piped())?;
    let local_stderr = spawn_bounded_reader(local.stderr.take(), PROBE_OP_MAX_OUTPUT);
    let Some(remote_stdout) = remote.stdout.take() else {
        return Err(io::Error::other("could not open probe stdout"));
    };
    let Some(local_stdin) = local.stdin.take() else {
        return Err(io::Error::other("could not open local tar stdin"));
    };

    let (tx, rx) = mpsc::channel::<io::Result<()>>();
    std::thread::Builder::new()
        .name("forge-remote-fs-dldir".to_string())
        .spawn(move || {
            // The "source child" here is the local extractor: a pump failure
            // must kill it so it cannot linger waiting for stdin.
            let outcome = pump_capped(remote_stdout, local_stdin, local, cap, timeout);
            let _ = tx.send(outcome);
        })
        .map_err(|error| io::Error::other(format!("could not start download pump: {error}")))?;

    let (status, outcome) = watchdog_streaming_child(&mut remote, &rx, timeout);
    probe_status_result(status, remote_stderr.join().unwrap_or_default())?;
    outcome?;
    let local_err = String::from_utf8_lossy(&local_stderr.join().unwrap_or_default())
        .trim()
        .to_string();
    if !local_err.is_empty() {
        log::warn!("local tar reported during directory download: {local_err}");
    }
    Ok(())
}

/// Upload a local directory tree to `dst` on a remote host (which must not
/// exist): the probe creates `dst` (exit 17 if taken), then the local system
/// tar streams the tree into `untar`. A failed upload removes the remote
/// `dst` it just created, best-effort.
pub(crate) fn upload_dir(host: &RemoteHost, src: &Path, dst: &Path) -> io::Result<()> {
    local_tar_available()?;
    if !src.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "upload source is not a directory",
        ));
    }
    let dst_arg = remote_path_arg(dst)?;
    run_probe(
        host,
        "mkdir",
        &[dst_arg],
        PROBE_OP_TIMEOUT,
        PROBE_OP_MAX_OUTPUT,
    )?;
    let local_argv = local_tar_create_argv(src)?;
    let remote_argv = probe_argv(host, "untar", &[dst_arg]);
    let result = stream_upload_dir(
        &local_argv,
        &remote_argv,
        MAX_TRANSFER_BYTES,
        TRANSFER_TIMEOUT,
    );
    if result.is_err() {
        let _ = run_probe(
            host,
            "rm",
            &[dst_arg],
            PROBE_OP_TIMEOUT,
            PROBE_OP_MAX_OUTPUT,
        );
    }
    result
}

fn stream_upload_dir(
    local_argv: &[String],
    remote_argv: &[String],
    cap: u64,
    timeout: Duration,
) -> io::Result<()> {
    let mut local = spawn_argv(local_argv, Stdio::null(), Stdio::piped(), Stdio::piped())?;
    let local_stderr = spawn_bounded_reader(local.stderr.take(), PROBE_OP_MAX_OUTPUT);
    let mut remote = spawn_argv(remote_argv, Stdio::piped(), Stdio::null(), Stdio::piped())?;
    let remote_stderr = spawn_bounded_reader(remote.stderr.take(), PROBE_OP_MAX_OUTPUT);
    let Some(local_stdout) = local.stdout.take() else {
        return Err(io::Error::other("could not open local tar stdout"));
    };
    let Some(remote_stdin) = remote.stdin.take() else {
        return Err(io::Error::other("could not open probe stdin"));
    };

    let (tx, rx) = mpsc::channel::<io::Result<()>>();
    std::thread::Builder::new()
        .name("forge-remote-fs-uldir".to_string())
        .spawn(move || {
            let outcome = pump_capped(local_stdout, remote_stdin, local, cap, timeout);
            let _ = tx.send(outcome);
        })
        .map_err(|error| io::Error::other(format!("could not start upload pump: {error}")))?;

    let (status, outcome) = watchdog_streaming_child(&mut remote, &rx, timeout);
    probe_status_result(status, remote_stderr.join().unwrap_or_default())?;
    outcome?;
    let local_err = String::from_utf8_lossy(&local_stderr.join().unwrap_or_default())
        .trim()
        .to_string();
    if !local_err.is_empty() {
        log::warn!("local tar reported during directory upload: {local_err}");
    }
    Ok(())
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

pub(crate) fn transfer_plan(from: &FsLocation, to: &FsLocation) -> Option<TransferPlan> {
    if from == to {
        return None;
    }
    Some(match (from, to) {
        (FsLocation::Remote(_), FsLocation::Local) => TransferPlan::Download,
        (FsLocation::Local, FsLocation::Remote(_)) => TransferPlan::Upload,
        (FsLocation::Remote(_), FsLocation::Remote(_)) => TransferPlan::Relay,
        // `from == to` was rejected above, so both-Local cannot reach here.
        (FsLocation::Local, FsLocation::Local) => unreachable!(),
    })
}

/// One cross-location transfer unit: download, upload, or a temp-relayed
/// remote-to-remote hop. `dst` must not exist anywhere along the way; every
/// leg pre-checks existence before a payload byte moves.
pub(crate) fn transfer(
    from: &FsLocation,
    hosts: &[RemoteHost],
    src: &Path,
    to: &FsLocation,
    dst: &Path,
    is_dir: bool,
) -> io::Result<()> {
    match (remote_host(from, hosts)?, remote_host(to, hosts)?) {
        (Some(src_host), None) => {
            if is_dir {
                download_dir(src_host, src, dst)
            } else {
                download_file(src_host, src, dst)
            }
        }
        (None, Some(dst_host)) => {
            if is_dir {
                upload_dir(dst_host, src, dst)
            } else {
                upload_file(dst_host, src, dst)
            }
        }
        (Some(src_host), Some(dst_host)) => transfer_relay(src_host, src, dst_host, dst, is_dir),
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
        let result =
            download_dir(src_host, src, &staged).and_then(|_| upload_dir(dst_host, &staged, dst));
        let _ = std::fs::remove_dir_all(&relay);
        result
    } else {
        let result =
            download_file(src_host, src, &relay).and_then(|_| upload_file(dst_host, &relay, dst));
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

/// Resolve a location against the snapshot of configured hosts taken when the
/// operation was queued.
fn remote_host<'a>(
    loc: &FsLocation,
    hosts: &'a [RemoteHost],
) -> io::Result<Option<&'a RemoteHost>> {
    match loc {
        FsLocation::Local => Ok(None),
        FsLocation::Remote(index) => hosts
            .get(*index)
            .map(Some)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "remote host was removed")),
    }
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

/// The directory the tree opens on for a location: the local behavior
/// ($HOME, else `/`) unchanged, or the remote account's home via the probe.
pub(crate) fn start_dir(loc: &FsLocation, hosts: &[RemoteHost]) -> io::Result<PathBuf> {
    match remote_host(loc, hosts)? {
        None => Ok(std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"))),
        Some(host) => {
            let capture = run_probe(host, "home", &[], PROBE_LIST_TIMEOUT, PROBE_HOME_MAX_OUTPUT)?;
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
    let path = PathBuf::from(String::from_utf8_lossy(line).into_owned());
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
) -> io::Result<Vec<FsEntry>> {
    match remote_host(loc, hosts)? {
        None => list_dir_local(dir),
        Some(host) => {
            let capture = run_probe(
                host,
                "list",
                &[remote_path_arg(dir)?],
                PROBE_LIST_TIMEOUT,
                PROBE_LIST_MAX_OUTPUT,
            )?;
            Ok(parse_list(&capture.stdout, dir))
        }
    }
}

fn list_dir_local(dir: &Path) -> io::Result<Vec<FsEntry>> {
    let mut entries = Vec::with_capacity(256);
    for entry in std::fs::read_dir(dir)?
        .take(MAX_DIRECTORY_ENTRIES)
        .flatten()
    {
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
    Ok(entries)
}

/// Parse `list` wire bytes: NUL-separated `<type>\0<name>\0` pairs. Malformed
/// tails, unknown types and empty names are skipped. Wire type `l` (a symlink
/// to something that is not a directory) maps to file: the tree never expands
/// it. A symlink to a directory arrives as `d` because the probe's `[ -d ]`
/// follows links, and expanding it re-lists through the link — that works.
fn parse_list(bytes: &[u8], dir: &Path) -> Vec<FsEntry> {
    use std::os::unix::ffi::OsStrExt;
    let mut tokens: Vec<&[u8]> = bytes.split(|byte| *byte == 0).collect();
    // The probe terminates every pair with NUL; a final unterminated token
    // means the capture was truncated, so the partial pair is dropped.
    if !bytes.is_empty() && bytes.last() != Some(&0) {
        tokens.pop();
    }
    let mut entries = Vec::new();
    // chunks_exact ignores a dangling half-pair on its own.
    for pair in tokens.chunks_exact(2) {
        if entries.len() >= MAX_DIRECTORY_ENTRIES {
            break;
        }
        let (kind, name) = (pair[0], pair[1]);
        let is_dir = match kind {
            b"d" => true,
            b"f" | b"l" => false,
            _ => continue,
        };
        if name.is_empty() {
            continue;
        }
        entries.push(FsEntry {
            name: String::from_utf8_lossy(name).into_owned(),
            path: dir.join(std::ffi::OsStr::from_bytes(name)),
            is_dir,
        });
    }
    sort_entries(&mut entries);
    entries
}

pub(crate) fn create_dir(loc: &FsLocation, hosts: &[RemoteHost], path: &Path) -> io::Result<()> {
    match remote_host(loc, hosts)? {
        // `create_dir` already fails with AlreadyExists when `path` exists.
        None => std::fs::create_dir(path),
        Some(host) => run_probe(
            host,
            "mkdir",
            &[remote_path_arg(path)?],
            PROBE_OP_TIMEOUT,
            PROBE_OP_MAX_OUTPUT,
        )
        .map(|_| ()),
    }
}

pub(crate) fn create_file(loc: &FsLocation, hosts: &[RemoteHost], path: &Path) -> io::Result<()> {
    match remote_host(loc, hosts)? {
        None => std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map(|_| ()),
        Some(host) => run_probe(
            host,
            "mkfile",
            &[remote_path_arg(path)?],
            PROBE_OP_TIMEOUT,
            PROBE_OP_MAX_OUTPUT,
        )
        .map(|_| ()),
    }
}

pub(crate) fn delete(loc: &FsLocation, hosts: &[RemoteHost], path: &Path) -> io::Result<()> {
    if path == Path::new("/") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to delete the filesystem root",
        ));
    }
    match remote_host(loc, hosts)? {
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
            host,
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
    match remote_host(loc, hosts)? {
        None => {
            fail_if_exists(dst)?;
            std::fs::rename(src, dst)
        }
        Some(host) => run_probe(
            host,
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
    match remote_host(loc, hosts)? {
        None => {
            fail_if_exists(dst)?;
            copy_recursive(src, dst, 0)
        }
        Some(host) => run_probe(
            host,
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
fn fail_if_exists(path: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", path.display()),
        ));
    }
    Ok(())
}

/// Small recursive local copier: directories are recreated entry by entry,
/// symlinks are re-pointed rather than followed, and regular files go through
/// `std::fs::copy` (which carries permissions). There is deliberately no
/// entry cap here — a user-invoked copy must not silently truncate — but the
/// recursion depth is bounded so a pathological tree fails instead of
/// exhausting the worker stack.
fn copy_recursive(src: &Path, dst: &Path, depth: usize) -> io::Result<()> {
    if depth >= MAX_COPY_DEPTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory nesting is too deep to copy",
        ));
    }
    let metadata = std::fs::symlink_metadata(src)?;
    if metadata.is_dir() {
        std::fs::create_dir(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dst.join(entry.file_name()), depth + 1)?;
        }
        Ok(())
    } else if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(src)?;
        std::os::unix::fs::symlink(target, dst)
    } else {
        std::fs::copy(src, dst).map(|_| ())
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
    use super::*;

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
        let argv = probe_argv(&host_fixture(), "list", &["/var/log"]);
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
        assert!(command.starts_with("sh -c '# remote-fs probe v2"));
        assert!(command.ends_with(" probe list '/var/log'"));
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
    fn list_parser_handles_spaces_newlines_and_non_utf8_names() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"d\0sub dir\0");
        bytes.extend_from_slice(b"f\0hello.txt\0");
        bytes.extend_from_slice(b"f\0line\nbreak\0");
        bytes.extend_from_slice(b"l\0dangling\0");
        bytes.extend_from_slice(b"f\0bad\xffname\0");
        let entries = parse_list(&bytes, Path::new("/data"));
        let names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "sub dir",
                "bad\u{fffd}name",
                "dangling",
                "hello.txt",
                "line\nbreak"
            ]
            .into_iter()
            .collect::<Vec<_>>()
        );
        assert!(entries[0].is_dir);
        assert!(!entries[1].is_dir);
        // A symlink reports as a file: the tree must not try to expand it.
        assert_eq!(entries[2].name, "dangling");
        assert!(!entries[2].is_dir);
        // Non-UTF8 names keep their exact bytes in the path for round-tripping.
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(entries[1].path.as_os_str().as_bytes(), b"/data/bad\xffname");
    }

    #[test]
    fn list_parser_skips_garbage_and_stops_at_the_entry_cap() {
        // Unknown type, empty name and a truncated trailing pair are dropped.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"x\0what\0");
        bytes.extend_from_slice(b"f\0\0");
        bytes.extend_from_slice(b"f\0ok\0");
        bytes.extend_from_slice(b"d\0truncated");
        let entries = parse_list(&bytes, Path::new("/d"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "ok");

        let mut big = Vec::new();
        for index in 0..MAX_DIRECTORY_ENTRIES + 16 {
            big.extend_from_slice(format!("f\0entry-{index}\0").as_bytes());
        }
        assert_eq!(
            parse_list(&big, Path::new("/d")).len(),
            MAX_DIRECTORY_ENTRIES
        );
    }

    #[test]
    fn list_parser_sorts_directories_first_then_case_insensitively() {
        let bytes = b"f\0Zulu\0d\0beta\0f\0Alpha\0d\0Able\0".as_slice();
        let entries = parse_list(bytes, Path::new("/d"));
        let names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
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

        let capture =
            run_capture(&argv("yes truncated"), b"", Duration::from_secs(5), 128).unwrap();
        assert!(capture.stdout.len() <= 128);

        let error =
            run_capture(&argv("sleep 30"), b"", Duration::from_millis(150), 64).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
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

        let listing = probe_locally("list", &[root_arg]);
        assert_eq!(listing.status, 0);
        let names: Vec<_> = parse_list(&listing.stdout, &root)
            .into_iter()
            .map(|entry| (entry.name, entry.is_dir))
            .collect();
        // The symlink points at a directory, and the probe's `[ -d ]` follows
        // it, so it lists as a directory rather than as wire type `l`.
        assert_eq!(
            names,
            vec![
                ("dir".to_string(), true),
                ("dir-link".to_string(), true),
                ("file.txt".to_string(), false),
            ]
        );

        // Relative paths are rejected before anything runs.
        assert_eq!(probe_locally("list", &["relative/path"]).status, 2);
        assert_eq!(
            probe_locally("list", &[&root.join("missing").to_string_lossy()]).status,
            3
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
    fn probe_v2_tar_untar_round_trip() {
        if local_tar_available().is_err() {
            return;
        }
        let root = unique_temp_dir("probe-v2-dir");
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
        let untarred = probe_locally_with_stdin("untar", &[&unpack_arg], &tarred.stdout);
        assert_eq!(untarred.status, 0);
        assert_eq!(std::fs::read(unpack.join("tree/top.txt")).unwrap(), b"top");
        assert_eq!(
            std::fs::read(unpack.join("tree/sub/nested.bin")).unwrap(),
            [0u8, 255, 1, 2]
        );

        // untar requires an existing directory.
        assert_eq!(
            probe_locally_with_stdin("untar", &[&missing], &tarred.stdout).status,
            3
        );

        let _ = std::fs::remove_dir_all(root);
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
        stream_download_to_file(&argv, &dst, 64 * 1024, Duration::from_secs(10)).unwrap();
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
            stream_download_to_file(&argv, &dst, 64 * 1024, Duration::from_secs(10))
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );

        // Overflow: the partial temp file is unlinked and no dst appears.
        let dst2 = root.join("out/too-big.bin");
        let argv = local_probe_argv("cat", &[&source_arg]);
        let error =
            stream_download_to_file(&argv, &dst2, 1_024, Duration::from_secs(10)).unwrap_err();
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
        stream_upload_to_probe(&argv, &source, 64 * 1024, Duration::from_secs(10)).unwrap();
        assert_eq!(
            std::fs::read(root.join("remote/landed.txt")).unwrap(),
            payload
        );

        // The probe's exit 17 maps to AlreadyExists even though the local
        // writer then sees a broken pipe.
        let argv = local_probe_argv("put", &[&remote]);
        assert_eq!(
            stream_upload_to_probe(&argv, &source, 64 * 1024, Duration::from_secs(10))
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );

        // A source over the cap is refused before the probe is even spawned.
        let argv = local_probe_argv("put", &[&root.join("remote/other").to_string_lossy()]);
        let error =
            stream_upload_to_probe(&argv, &source, 1_024, Duration::from_secs(10)).unwrap_err();
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

        // "Upload": local tar -> probe untar into an existing remote dir.
        let remote_dir = root.join("remote/tree");
        std::fs::create_dir_all(&remote_dir).unwrap();
        let local_argv = local_tar_create_argv(&root.join("tree")).unwrap();
        let remote_argv = local_probe_argv("untar", &[&remote_dir.to_string_lossy()]);
        stream_upload_dir(
            &local_argv,
            &remote_argv,
            64 * 1024 * 1024,
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(remote_dir.join("tree/a.txt")).unwrap(),
            b"aaa"
        );
        assert_eq!(
            std::fs::read(remote_dir.join("tree/sub/b.bin")).unwrap(),
            [9u8, 8, 7, 0]
        );

        // "Download": probe tar -> local tar into the dst parent.
        let dst = root.join("local-back/tree");
        std::fs::create_dir(root.join("local-back")).unwrap();
        let probe_argv = local_probe_argv("tar", &[&remote_dir.join("tree").to_string_lossy()]);
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
}
