//! remote_fs — blocking local and remote filesystem access for the sidebar
//! file tree.
//!
//! Local entries come straight from `std::fs`. Remote entries come from the
//! hosts in `config.remote_hosts`: a small POSIX sh probe script is pushed to
//! the far side through the system `ssh` / `docker` binaries (no sshfs, no new
//! dependencies) and its byte output is parsed back here. This mirrors the
//! script-over-ssh philosophy of `jterm_core::jsh_remote`: the far side only
//! ever sees a fixed script on stdin plus single-quote-escaped positional
//! parameters, never an interpolated path.
//!
//! Everything in this module blocks. Callers run it on worker threads and
//! return results to the GTK main loop through a channel, exactly like the
//! file-tree scanner in `super::file_tree`.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::config::RemoteHost;

/// Hard cap on entries per directory listing, shared with the local scanner.
pub(crate) const MAX_DIRECTORY_ENTRIES: usize = 4_096;
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
const WATCHDOG_POLL_INTERVAL: Duration = Duration::from_millis(10);
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

/// A sidebar cut/copy payload. Paste is only offered while the clipboard's
/// location matches the tree's current location; cross-host paste would need
/// a byte stream this module deliberately does not provide.
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

/// The far-side probe, fed to `sh -s -- <op> [args...]` on stdin. Wire
/// protocol v1: `list` prints NUL-separated `<type>,<name>` pairs (types
/// `d`/`f`/`l`); exit codes are 0 ok, 2 usage/bad path, 3 cannot enter dir,
/// 4 operation failed, 17 target exists.
const PROBE_SCRIPT: &str = r#"# remote-fs probe v1 — runs under `sh -s -- <op> [args...]`.
# `list` stdout: NUL-separated pairs "<t>\0<name>\0", t in {d,f,l}, names relative.
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

/// Build the local argv that runs the probe on a host. For ssh the whole
/// remote command is ONE argv element (`sh -s -- <op> '<arg>'...`) because
/// ssh joins and re-parses it remotely; the script itself travels on stdin.
/// For docker the probe argv is passed through raw, `-i` keeping stdin wired
/// and `-t` deliberately absent so output is never CRLF-mangled.
fn probe_argv(host: &RemoteHost, op: &str, args: &[&str]) -> Vec<String> {
    if host.docker {
        let mut argv = vec!["docker".to_string(), "exec".to_string(), "-i".to_string()];
        if let Some(user) = &host.user {
            argv.push("-u".to_string());
            argv.push(user.clone());
        }
        argv.push(host.host.clone());
        argv.push("sh".to_string());
        argv.push("-s".to_string());
        argv.push("--".to_string());
        argv.push(op.to_string());
        argv.extend(args.iter().map(|arg| (*arg).to_string()));
        return argv;
    }

    let mut command = format!("sh -s -- {op}");
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

/// Run a child to completion with piped stdio, bounded output and a hard
/// timeout. The calling worker thread doubles as the watchdog: it polls the
/// child and kills it past the deadline, so a hung ssh or stopped container
/// cannot pin the thread forever.
fn run_capture(
    argv: &[String],
    stdin_bytes: &[u8],
    timeout: Duration,
    max_out: u64,
) -> io::Result<Capture> {
    let Some((program, args)) = argv.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty probe argv",
        ));
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Feed the probe script from a helper thread: a child that exits early
    // fails the write, and the detached thread then ends on its own.
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

    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Ok(status);
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait(); // reap the killed child
            break Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "remote-fs probe timed out",
            ));
        }
        std::thread::sleep(WATCHDOG_POLL_INTERVAL);
    };
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
    let capture = run_capture(&argv, PROBE_SCRIPT.as_bytes(), timeout, max_out)?;
    probe_result(capture)
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
    fn ssh_argv_keeps_script_on_stdin_and_command_in_one_element() {
        let argv = probe_argv(&host_fixture(), "list", &["/var/log"]);
        assert_eq!(
            argv,
            vec![
                "ssh",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-p",
                "2222",
                "--",
                "alice@dev.example.com",
                "sh -s -- list '/var/log'",
            ]
        );
    }

    #[test]
    fn ssh_argv_quotes_every_operand_and_handles_missing_user() {
        let mut host = host_fixture();
        host.user = None;
        host.ssh_args = Vec::new();
        let argv = probe_argv(&host, "mv", &["/a b/c", "/d'e"]);
        assert_eq!(
            argv,
            vec![
                "ssh",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "--",
                "dev.example.com",
                "sh -s -- mv '/a b/c' '/d'\\''e'",
            ]
        );
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
                "docker", "exec", "-i", "-u", "alice", "builder", "sh", "-s", "--", "rm",
                "/tmp/x y",
            ]
        );
        assert!(!argv.iter().any(|arg| arg == "-t"));

        host.user = None;
        let argv = probe_argv(&host, "home", &[]);
        assert_eq!(
            argv,
            vec!["docker", "exec", "-i", "builder", "sh", "-s", "--", "home"]
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

    /// Run the embedded probe under the local `sh` — the same script bytes a
    /// remote side would receive on stdin.
    fn probe_locally(op: &str, args: &[&str]) -> Capture {
        let mut argv = vec!["sh".to_string(), "-s".to_string(), "--".to_string()];
        argv.push(op.to_string());
        argv.extend(args.iter().map(|arg| (*arg).to_string()));
        run_capture(
            &argv,
            PROBE_SCRIPT.as_bytes(),
            Duration::from_secs(10),
            PROBE_LIST_MAX_OUTPUT,
        )
        .unwrap()
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
}
