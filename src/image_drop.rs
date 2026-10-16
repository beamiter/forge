//! Safe local-path drag payloads for terminal prompts.
//!
//! Desktop drag-and-drop supplies decoded local paths. We turn those paths into
//! ordinary review-first terminal input: no newline, no implicit submission,
//! and no control or bidi-spoofing characters. Any existing file or directory
//! is accepted — dropping a source file, a log or a folder is how people hand
//! a path to claude, codex or kimi — and the pane sends the payload as a paste
//! (bracketed when the program asked for it), as GNOME Terminal and kitty do.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

const VALIDATION_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_VALIDATORS: usize = 2;
static VALIDATORS: AtomicUsize = AtomicUsize::new(0);

/// Admission belongs to the worker, not the UI request. A timed-out stat can
/// remain blocked in the kernel; it must still occupy its slot until it exits.
struct Permit;
impl Drop for Permit {
    fn drop(&mut self) {
        VALIDATORS.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) struct PathValidation {
    receiver: mpsc::Receiver<Result<String, DropError>>,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
}

impl Drop for PathValidation {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl PathValidation {
    pub(crate) fn start(paths: Vec<PathBuf>) -> Result<Self, DropError> {
        Self::start_with(paths, VALIDATION_TIMEOUT, Path::exists)
    }

    #[cfg(test)]
    pub(crate) fn start_for_test(
        paths: Vec<PathBuf>,
        timeout: Duration,
        exists: impl Fn(&Path) -> bool + Send + 'static,
    ) -> Result<Self, DropError> {
        Self::start_with(paths, timeout, exists)
    }

    fn start_with(
        paths: Vec<PathBuf>,
        timeout: Duration,
        exists: impl Fn(&Path) -> bool + Send + 'static,
    ) -> Result<Self, DropError> {
        // Reject malformed/oversized drops before admission or filesystem I/O.
        let payload = quoted_paths_payload(&paths)?;
        let mut count = VALIDATORS.load(Ordering::Acquire);
        loop {
            if count >= MAX_VALIDATORS {
                return Err(DropError(
                    "file checks are busy; try again after the mount responds",
                ));
            }
            match VALIDATORS.compare_exchange_weak(
                count,
                count + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => count = current,
            }
        }
        let permit = Permit;
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let deadline = Instant::now() + timeout;
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("forge-path-drop".into())
            .spawn(move || {
                let _permit = permit;
                for path in paths {
                    if worker_cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
                        return;
                    }
                    if !exists(&path) {
                        let _ = sender.send(Err(DropError(
                            "only existing local files and folders can be dropped",
                        )));
                        return;
                    }
                }
                if !worker_cancelled.load(Ordering::Acquire) && Instant::now() < deadline {
                    let _ = sender.send(Ok(payload));
                }
            })
            .map_err(|_| DropError("could not start the file check"))?;
        Ok(Self {
            receiver,
            cancelled,
            deadline,
        })
    }

    /// Never waits, joins, or touches the filesystem. Expiry wins over a queued
    /// success so the UI cannot revive a request after its acceptance window.
    pub(crate) fn poll(&self) -> Option<Result<String, DropError>> {
        if Instant::now() >= self.deadline {
            self.cancelled.store(true, Ordering::Release);
            return Some(Err(DropError("file check timed out; nothing was pasted")));
        }
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(DropError("file check stopped; nothing was pasted")))
            }
        }
    }
}

const MAX_DROPPED_PATHS: usize = 16;
const MAX_DROP_PAYLOAD_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DropError(&'static str);

impl fmt::Display for DropError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

fn quoted_paths_payload(paths: &[PathBuf]) -> Result<String, DropError> {
    if paths.is_empty() {
        return Err(DropError("the drop contained no local files"));
    }
    if paths.len() > MAX_DROPPED_PATHS {
        return Err(DropError("too many files were dropped at once"));
    }

    let mut quoted = Vec::with_capacity(paths.len());
    let mut bytes = 0usize;
    for path in paths {
        let text = path
            .to_str()
            .ok_or(DropError("the dropped path is not valid UTF-8"))?;
        if text.len() > MAX_DROP_PAYLOAD_BYTES {
            return Err(DropError("the dropped paths are too long"));
        }
        if text.chars().any(char::is_control)
            || jterm_core::review_input::contains_visual_spoofing(text)
        {
            return Err(DropError(
                "the dropped path contains hidden or control text",
            ));
        }

        let encoded = if text
            .chars()
            .all(|character| character.is_alphanumeric() || "._-/~".contains(character))
        {
            text.to_string()
        } else {
            jterm_core::process::shell_single_quote(text)
        };
        bytes = bytes
            .checked_add(encoded.len() + 1)
            .ok_or(DropError("the dropped paths are too long"))?;
        if bytes > MAX_DROP_PAYLOAD_BYTES {
            return Err(DropError("the dropped paths are too long"));
        }
        quoted.push(encoded);
    }

    let mut payload = quoted.join(" ");
    // Match desktop terminal drag behavior: leave the caret ready for more
    // prompt text without ever adding Enter.
    payload.push(' ');
    Ok(payload)
}

// Synchronous compatibility helper only for existing test fixtures. Production
// callbacks must use PathValidation; existence is not an authorization check.
#[cfg(test)]
pub(crate) fn dropped_paths_payload(paths: &[PathBuf]) -> Result<String, DropError> {
    let payload = quoted_paths_payload(paths)?;
    if paths.iter().any(|path| !path.exists()) {
        return Err(DropError(
            "only existing local files and folders can be dropped",
        ));
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temporary_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "jterm-path-drop-{}-{nonce}-{name}",
            std::process::id()
        ))
    }

    fn temporary_file(name: &str) -> PathBuf {
        let path = temporary_path(name);
        File::create(&path).expect("temporary file");
        path
    }

    #[test]
    fn image_paths_are_shell_quoted_and_never_submitted() {
        let image = temporary_file("screen shot's.PNG");
        let payload = dropped_paths_payload(std::slice::from_ref(&image)).expect("valid image");
        assert_eq!(
            payload,
            format!(
                "{} ",
                jterm_core::process::shell_single_quote(image.to_str().unwrap())
            )
        );
        assert!(!payload.contains('\n'));
        assert!(!payload.contains('\r'));
        std::fs::remove_file(image).expect("cleanup");
    }

    #[test]
    fn any_existing_file_or_folder_can_be_dropped() {
        let notes = temporary_file("notes.txt");
        let folder = temporary_path("a folder");
        std::fs::create_dir(&folder).expect("temporary folder");
        let payload = dropped_paths_payload(&[notes.clone(), folder.clone()]).expect("valid drop");
        assert_eq!(
            payload,
            format!(
                "{} {} ",
                notes.to_str().unwrap(),
                jterm_core::process::shell_single_quote(folder.to_str().unwrap())
            )
        );
        std::fs::remove_file(notes).expect("cleanup");
        std::fs::remove_dir(folder).expect("cleanup");
    }

    #[test]
    fn missing_control_and_spoofed_paths_are_rejected() {
        assert_eq!(
            dropped_paths_payload(&[temporary_path("gone.txt")]).unwrap_err(),
            DropError("only existing local files and folders can be dropped")
        );
        for name in ["line\nbreak.txt", "bidi\u{202e}txt.exe"] {
            let path = temporary_file(name);
            assert_eq!(
                dropped_paths_payload(std::slice::from_ref(&path)).unwrap_err(),
                DropError("the dropped path contains hidden or control text"),
                "{name:?}"
            );
            std::fs::remove_file(path).expect("cleanup");
        }
        assert_eq!(
            dropped_paths_payload(&[]).unwrap_err(),
            DropError("the drop contained no local files")
        );
    }
    #[test]
    fn lexical_limits_and_async_deadline_admission_cancel_are_bounded() {
        use std::sync::atomic::AtomicUsize;
        assert!(quoted_paths_payload(&vec![PathBuf::from("/a"); 17]).is_err());
        assert!(
            quoted_paths_payload(&[PathBuf::from("a".repeat(MAX_DROP_PAYLOAD_BYTES))]).is_err()
        );
        assert!(
            quoted_paths_payload(&[PathBuf::from("a".repeat(MAX_DROP_PAYLOAD_BYTES - 1))]).is_ok()
        );
        assert!(
            quoted_paths_payload(&[PathBuf::from("'".repeat(MAX_DROP_PAYLOAD_BYTES / 2))]).is_err()
        );
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            assert!(
                quoted_paths_payload(&[PathBuf::from(std::ffi::OsString::from_vec(vec![255]))])
                    .is_err()
            );
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let (release_a, gate_a) = mpsc::channel();
        let (release_b, gate_b) = mpsc::channel();
        let (entered, enter_rx) = mpsc::channel();
        let (entered_b, enter_b_rx) = mpsc::channel();
        let a_calls = calls.clone();
        let a = PathValidation::start_with(
            vec!["/a".into(), "/b".into()],
            Duration::from_millis(500),
            move |_| {
                a_calls.fetch_add(1, Ordering::SeqCst);
                entered.send(()).unwrap();
                gate_a.recv().unwrap();
                true
            },
        )
        .unwrap();
        let b = PathValidation::start_with(vec!["/a".into()], Duration::from_secs(5), move |_| {
            entered_b.send(()).unwrap();
            gate_b.recv().unwrap();
            true
        })
        .unwrap();
        enter_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        enter_b_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let started = Instant::now();
        assert!(a.poll().is_none());
        assert!(started.elapsed() < Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(530));
        assert_eq!(
            a.poll().unwrap().unwrap_err(),
            DropError("file check timed out; nothing was pasted")
        );
        drop(a);
        drop(b);
        assert!(
            PathValidation::start_with(vec!["/a".into()], Duration::from_secs(1), |_| true)
                .is_err(),
            "cancelled blocked calls retain admission"
        );
        release_a.send(()).unwrap();
        release_b.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while VALIDATORS.load(Ordering::Acquire) != 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(VALIDATORS.load(Ordering::Acquire), 0);
        assert_eq!(
            calls.load(Ordering::Acquire),
            1,
            "cancel prevents next path check"
        );
        let valid =
            PathValidation::start_with(vec!["/a b's".into()], Duration::from_secs(2), |_| true)
                .unwrap();
        let result = loop {
            if let Some(result) = valid.poll() {
                break result;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(
            result.unwrap(),
            format!("{} ", jterm_core::process::shell_single_quote("/a b's"))
        );
        // Receipt may precede the worker's final destructor by a few
        // instructions. Avoid making this sequential test race admission.
        let wait_idle = || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while VALIDATORS.load(Ordering::Acquire) != 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(VALIDATORS.load(Ordering::Acquire), 0);
        };
        wait_idle();
        let real_path = temporary_file("async check.txt");
        let real = PathValidation::start(vec![real_path.clone()]).unwrap();
        loop {
            if let Some(result) = real.poll() {
                assert!(result.is_ok());
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        std::fs::remove_file(real_path).unwrap();
        wait_idle();
        let missing =
            PathValidation::start_with(vec!["/gone".into()], Duration::from_secs(2), |_| false)
                .unwrap();
        loop {
            if let Some(result) = missing.poll() {
                assert!(result.is_err());
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
