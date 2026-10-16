#![allow(dead_code, non_camel_case_types)]
use std::fmt;
use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
extern crate self as jagent;
extern crate self as jterm_core;
/* JAGENT */
/* CORE */
use pty_input::{InputGuard, PasteModes, PastePolicy, UnbracketedMultiline};
mod libc {
    pub use std::ffi::c_void;
    pub const POLLIN: i16 = 1;
    pub const POLLOUT: i16 = 4;
    #[repr(C)]
    pub struct pollfd {
        pub fd: i32,
        pub events: i16,
        pub revents: i16,
    }
    unsafe extern "C" {
        pub fn write(fd: i32, data: *const c_void, len: usize) -> isize;
        pub fn read(fd: i32, data: *mut c_void, len: usize) -> isize;
        pub fn poll(fds: *mut pollfd, count: usize, timeout: i32) -> i32;
    }
}
/* PTY_INPUT_QUEUE_CAPACITY */
/* MAX_PTY_INPUT_MESSAGE_BYTES */
/* TYPES */
/* FUNCTIONS */
// Only unrelated lifecycle, reader and UI fields are omitted.
struct OwnedPty {
    input_tx: Mutex<Option<mpsc::SyncSender<Vec<u8>>>>,
    input_dispatch: Arc<Mutex<InputDispatch>>,
    input_guard: Mutex<InputGuard>,
    input_error_reported: AtomicBool,
    shell_bracketed_paste: AtomicBool,
}
impl OwnedPty {
    /* METHODS */
}
fn pair(bracketed: bool) -> (OwnedPty, mpsc::Receiver<Vec<u8>>) {
    let (tx, rx) = mpsc::sync_channel(PTY_INPUT_QUEUE_CAPACITY);
    (
        OwnedPty {
            input_tx: Mutex::new(Some(tx)),
            input_dispatch: Arc::new(Mutex::new(InputDispatch::default())),
            input_guard: Mutex::new(InputGuard::new()),
            input_error_reported: AtomicBool::new(false),
            shell_bracketed_paste: AtomicBool::new(bracketed),
        },
        rx,
    )
}
#[cfg(test)]
mod forge_input {
    use super::*;
    #[test]
    fn errors_explain_nothing_was_sent_without_payload() {
        let errors = [
            PtyWriteError::TooLarge {
                bytes: 262145,
                limit: 262144,
            },
            PtyWriteError::QueueFull { bytes: 17 },
            PtyWriteError::Closed { bytes: 17 },
            PtyWriteError::ImmediateUnavailable,
        ];
        for error in errors {
            assert!(
                error.to_string().to_lowercase().contains("not sent"),
                "{error}"
            );
        }
        assert!(errors[0].to_string().contains("262144"));
    }
    #[test]
    fn whole_unit_limit_and_utf8() {
        assert_eq!(MAX_PTY_INPUT_MESSAGE_BYTES, 256 * 1024);
        let (pty, rx) = pair(false);
        // Four-byte code points straddle any naive byte truncation boundary.
        let exact = "🦀".repeat(MAX_PTY_INPUT_MESSAGE_BYTES / 4).into_bytes();
        pty.write_bytes(&exact).unwrap();
        assert_eq!(rx.try_recv().unwrap(), exact);
        let oversized = [exact.as_slice(), "é".as_bytes()].concat();
        assert_eq!(
            pty.write_bytes(&oversized),
            Err(PtyWriteError::TooLarge {
                bytes: oversized.len(),
                limit: MAX_PTY_INPUT_MESSAGE_BYTES,
            })
        );
        assert!(rx.try_recv().is_err());
        pty.write_bytes("次".as_bytes()).unwrap();
        assert_eq!(std::str::from_utf8(&rx.try_recv().unwrap()).unwrap(), "次");
    }
    #[test]
    fn queue_saturates_at_64_and_recovers_without_rejected_prefix() {
        assert_eq!(PTY_INPUT_QUEUE_CAPACITY, 64);
        let (pty, rx) = pair(false);
        for i in 0..64 {
            pty.write_bytes(format!("{i}:é").as_bytes()).unwrap();
        }
        assert_eq!(
            pty.write_bytes(b"rejected-command\r"),
            Err(PtyWriteError::QueueFull { bytes: 17 })
        );
        for i in 0..64 {
            assert_eq!(rx.try_recv().unwrap(), format!("{i}:é").as_bytes());
        }
        assert!(rx.try_recv().is_err());
        pty.write_bytes(b"next").unwrap();
        assert_eq!(rx.try_recv().unwrap(), b"next");
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn all_closed_paths_reject_whole_unit() {
        let (pty, rx) = pair(false);
        pty.input_dispatch.lock().unwrap().closed = true;
        assert_eq!(
            pty.write_bytes(b"secret"),
            Err(PtyWriteError::Closed { bytes: 6 })
        );
        assert!(rx.try_recv().is_err());
        pty.input_dispatch.lock().unwrap().closed = false;
        drop(rx);
        assert_eq!(
            pty.write_bytes(b"secret"),
            Err(PtyWriteError::Closed { bytes: 6 })
        );
        *pty.input_tx.lock().unwrap() = None;
        assert_eq!(
            pty.write_bytes(b"secret"),
            Err(PtyWriteError::Closed { bytes: 6 })
        );
    }
    #[test]
    fn rejected_openers_and_closers_do_not_change_filter_state() {
        let (tx, rx) = mpsc::sync_channel(1);
        let mut guard = InputGuard::new();
        let modes = PasteModes { bracketed: true };
        tx.try_send(b"occupied".to_vec()).unwrap();
        assert_eq!(
            filter_and_enqueue_input(&mut guard, &tx, pty_input::PASTE_START, modes),
            Err(PtyWriteError::QueueFull { bytes: 6 })
        );
        assert!(!guard.in_frame());
        rx.recv().unwrap();
        filter_and_enqueue_input(&mut guard, &tx, pty_input::PASTE_START, modes).unwrap();
        assert!(guard.in_frame());
        assert_eq!(
            filter_and_enqueue_input(&mut guard, &tx, pty_input::PASTE_END, modes),
            Err(PtyWriteError::QueueFull { bytes: 6 })
        );
        assert!(guard.in_frame());
        rx.recv().unwrap();
        filter_and_enqueue_input(&mut guard, &tx, pty_input::PASTE_END, modes).unwrap();
        assert!(!guard.in_frame());
        rx.recv().unwrap();
        drop(rx);
        assert!(matches!(
            filter_and_enqueue_input(&mut guard, &tx, pty_input::PASTE_START, modes),
            Err(PtyWriteError::Closed { .. })
        ));
        assert!(!guard.in_frame());
    }
    #[test]
    fn framing_expansion_is_rejected_atomically() {
        let (pty, rx) = pair(true);
        let mut data = vec![b'x'; MAX_PTY_INPUT_MESSAGE_BYTES];
        data[10] = b'\n';
        assert_eq!(
            pty.write_bytes(&data),
            Err(PtyWriteError::TooLarge {
                bytes: data.len() + 12,
                limit: MAX_PTY_INPUT_MESSAGE_BYTES,
            })
        );
        assert!(!pty.input_guard.lock().unwrap().in_frame());
        assert_eq!(pty.input_dispatch.lock().unwrap().pending, 0);
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn native_pty_delivers_only_accepted_whole_units() {
        let master: RawFd = std::env::var("FORGE_TEST_MASTER").unwrap().parse().unwrap();
        let slave: RawFd = std::env::var("FORGE_TEST_SLAVE").unwrap().parse().unwrap();
        let (pty, rx) = pair(false);
        let exact = "🦀".repeat(MAX_PTY_INPUT_MESSAGE_BYTES / 4).into_bytes();
        pty.write_bytes(&exact).unwrap();
        let mut expected = exact.clone();
        for _ in 1..64 {
            pty.write_bytes("é".as_bytes()).unwrap();
            expected.extend_from_slice("é".as_bytes());
        }
        assert!(matches!(
            pty.write_bytes(b"REJECTED-PREFIX\r"),
            Err(PtyWriteError::QueueFull { .. })
        ));
        assert!(matches!(
            pty.write_bytes(&vec![b'Z'; MAX_PTY_INPUT_MESSAGE_BYTES + 1]),
            Err(PtyWriteError::TooLarge { .. })
        ));
        drop(pty);
        let writer = std::thread::spawn(move || {
            for unit in rx {
                write_all_fd(master, &unit, None).unwrap();
            }
        });
        let mut received = Vec::new();
        while received.len() < expected.len() {
            let mut ready = libc::pollfd {
                fd: slave,
                events: libc::POLLIN,
                revents: 0,
            };
            assert_eq!(
                unsafe { libc::poll(&mut ready, 1, 3000) },
                1,
                "PTY delivery timed out"
            );
            let mut buf = [0u8; 8192];
            let count = unsafe { libc::read(slave, buf.as_mut_ptr().cast(), buf.len()) };
            assert!(count > 0);
            received.extend_from_slice(&buf[..count as usize]);
        }
        writer.join().unwrap();
        assert_eq!(received, expected);
        assert!(std::str::from_utf8(&received).is_ok());
        let mut ready = libc::pollfd {
            fd: slave,
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(
            unsafe { libc::poll(&mut ready, 1, 100) },
            0,
            "rejected bytes reached PTY"
        );
    }
}
