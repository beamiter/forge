#![allow(dead_code, non_camel_case_types)]
use std::cell::{Cell, RefCell};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
extern crate self as log;
static LOGS: AtomicUsize = AtomicUsize::new(0);
#[macro_export]
macro_rules! warn { ($($t:tt)*) => {{ let _ = format_args!($($t)*); LOGS.fetch_add(1, Ordering::Relaxed); }} }
#[macro_export]
macro_rules! debug { ($($t:tt)*) => {{ let _ = format_args!($($t)*); LOGS.fetch_add(1, Ordering::Relaxed); }} }
mod libc {
    use super::*;
    pub const EBADF: i32 = 9;
    pub const TIOCSWINSZ: u64 = /* SET */ 0;
    pub const TIOCGWINSZ: u64 = /* GET */ 0;
    #[repr(C)]
    #[derive(Default)]
    pub struct winsize {
        pub ws_row: u16,
        pub ws_col: u16,
        pub ws_xpixel: u16,
        pub ws_ypixel: u16,
    }
    thread_local! {
        pub static FAILURE: Cell<Option<i32>> = const { Cell::new(None) };
        pub static CALLS: Cell<usize> = const { Cell::new(0) };
    }
    #[link(name = "util")]
    unsafe extern "C" {
        pub fn openpty(
            master: *mut i32,
            slave: *mut i32,
            name: *mut u8,
            term: *const u8,
            size: *const winsize,
        ) -> i32;
        #[link_name = "ioctl"]
        pub fn native_ioctl(fd: i32, request: u64, size: *mut winsize) -> i32;
        fn __errno_location() -> *mut i32;
    }
    pub unsafe fn ioctl(fd: i32, request: u64, size: &winsize) -> i32 {
        CALLS.set(CALLS.get() + 1);
        if let Some(errno) = FAILURE.get() {
            *__errno_location() = errno;
            return -1;
        }
        native_ioctl(fd, request, (size as *const winsize).cast_mut())
    }
}
mod pty {
    pub const INITIAL_WINSIZE: (u16, u16) = (80, 24);
}
struct OwnedPty {
    master: Arc<Mutex<Option<OwnedFd>>>,
    resize_error_reported: AtomicI32,
}
impl OwnedPty {
    /* METHODS */
}
/* WINSIZE */
fn pair() -> (OwnedPty, OwnedFd, PtyWinsize) {
    libc::FAILURE.set(None);
    libc::CALLS.set(0);
    LOGS.store(0, Ordering::Relaxed);
    let (mut master, mut slave) = (-1, -1);
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        },
        0
    );
    (
        OwnedPty {
            master: Arc::new(Mutex::new(Some(unsafe { OwnedFd::from_raw_fd(master) }))),
            resize_error_reported: AtomicI32::new(0),
        },
        unsafe { OwnedFd::from_raw_fd(slave) },
        PtyWinsize::new(Rc::new(Cell::new(0))),
    )
}
fn size(slave: &OwnedFd) -> (u16, u16) {
    let mut ws = libc::winsize::default();
    assert_eq!(
        unsafe { libc::native_ioctl(slave.as_raw_fd(), libc::TIOCGWINSZ, &mut ws) },
        0
    );
    (ws.ws_col, ws.ws_row)
}
#[test]
fn native_success_matches_child_and_generation() {
    let (pty, slave, state) = pair();
    assert!(state.publish(&pty, (120, 40)));
    assert_eq!(size(&slave), (120, 40));
    assert_eq!(state.child_winsize(), (120, 40));
    assert_eq!(state.generation.get(), 1);
    assert!(!state.publish(&pty, (120, 40)));
    assert_eq!(libc::CALLS.get(), 1);
}
#[test]
fn ebadf_and_eio_never_commit_failed_size_or_retry() {
    for errno in [9, 5] {
        let (pty, _slave, state) = pair();
        state.publish(&pty, (80, 24));
        libc::FAILURE.set(Some(errno));
        assert!(!state.publish(&pty, (100, 30)));
        assert_eq!(state.child_winsize(), (80, 24));
        assert_eq!(state.generation.get(), 1);
        for _ in 0..20 {
            state.publish(&pty, (100, 30));
            state.reassert(&pty, (100, 30));
        }
        assert_eq!(libc::CALLS.get(), 2);
        assert_eq!(LOGS.load(Ordering::Relaxed), 1);
    }
}
#[test]
fn interrupted_retry_is_bounded_and_latest_size_wins() {
    let (pty, slave, state) = pair();
    libc::FAILURE.set(Some(4)); // EINTR
    for _ in 0..20 {
        state.publish(&pty, (100, 30));
    }
    assert_eq!(libc::CALLS.get(), 3);
    assert_eq!(state.generation.get(), 0);
    libc::FAILURE.set(None);
    assert!(state.publish(&pty, (140, 50)));
    assert_eq!(size(&slave), (140, 50));
    assert_eq!(state.child_winsize(), (140, 50));
    assert_eq!(state.generation.get(), 1);
}
#[test]
fn reassert_failure_retries_once_per_dispatch_not_twice() {
    let (pty, _slave, state) = pair();
    libc::FAILURE.set(Some(11)); // EAGAIN
    state.reassert(&pty, (100, 30));
    assert_eq!(libc::CALLS.get(), 1);
    state.publish(&pty, (100, 30));
    assert_eq!(libc::CALLS.get(), 2);
}
#[test]
fn close_before_resize_does_not_publish_or_use_recycled_fd() {
    let (pty, _slave, state) = pair();
    pty.master.lock().unwrap().take();
    assert!(!state.publish(&pty, (100, 30)));
    assert_eq!(state.generation.get(), 0);
    assert_eq!(libc::CALLS.get(), 0);
    for _ in 0..20 {
        state.publish(&pty, (100, 30));
    }
    assert_eq!(LOGS.load(Ordering::Relaxed), 1);
}
#[test]
fn successful_reassert_preserves_generation() {
    let (pty, slave, state) = pair();
    state.publish(&pty, (100, 30));
    let altered = libc::winsize {
        ws_col: 50,
        ws_row: 10,
        ..Default::default()
    };
    unsafe {
        libc::native_ioctl(
            slave.as_raw_fd(),
            libc::TIOCSWINSZ,
            (&altered as *const libc::winsize).cast_mut(),
        )
    };
    assert!(!state.reassert(&pty, (100, 30)));
    assert_eq!(size(&slave), (100, 30));
    assert_eq!(state.generation.get(), 1);
}
#[test]
fn rapid_resize_keeps_latest_intent_after_transient_errors() {
    let (pty, slave, state) = pair();
    for cols in 90..140 {
        libc::FAILURE.set(Some(4));
        state.publish(&pty, (cols, 30));
    }
    libc::FAILURE.set(None);
    state.publish(&pty, (139, 30));
    assert_eq!(size(&slave), (139, 30));
    assert_eq!(state.child_winsize(), (139, 30));
    assert_eq!(state.generation.get(), 1);
}
#[cfg(f03)]
#[test]
fn lock_contention_is_nonblocking_and_retries_latest() {
    let (pty, slave, state) = pair();
    let guard = pty.master.lock().unwrap();
    assert!(!state.publish(&pty, (100, 30)));
    assert_eq!(libc::CALLS.get(), 0);
    drop(guard);
    assert!(state.publish(&pty, (110, 35)));
    assert_eq!(size(&slave), (110, 35));
}
#[cfg(f03)]
#[test]
fn error_category_change_is_observable_and_success_resets_it() {
    let (pty, _slave, state) = pair();
    for (cols, errno) in [(100, 11), (101, 11), (102, 5), (103, 9)] {
        libc::FAILURE.set(Some(errno));
        state.publish(&pty, (cols, 30));
    }
    assert_eq!(LOGS.load(Ordering::Relaxed), 3);
    libc::FAILURE.set(None);
    state.publish(&pty, (104, 30));
    libc::FAILURE.set(Some(9));
    state.publish(&pty, (105, 30));
    assert_eq!(LOGS.load(Ordering::Relaxed), 4);
}
#[cfg(f03)]
#[test]
fn resize_propagates_errno_without_committing_lifecycle_changes() {
    let (pty, _slave, _) = pair();
    for errno in [9, 5, 4, 11] {
        assert_eq!(
            pty.resize_with(100, 30, |_, _| Err(io::Error::from_raw_os_error(errno)))
                .unwrap_err()
                .raw_os_error(),
            Some(errno)
        );
        assert!(pty.master.lock().unwrap().is_some());
    }
}
#[cfg(f03)]
#[test]
fn close_racing_ioctl_cannot_recycle_borrowed_descriptor() {
    let (pty, _slave, _) = pair();
    let closer = pty.master.clone();
    let (start_tx, start_rx) = std::sync::mpsc::channel();
    let (closed_tx, closed_rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        start_rx.recv().unwrap();
        closer.lock().unwrap().take();
        closed_tx.send(()).unwrap();
    });
    pty.resize_with(100, 30, |fd, ws| {
        start_tx.send(()).unwrap();
        assert!(closed_rx
            .recv_timeout(std::time::Duration::from_millis(30))
            .is_err());
        assert_eq!(unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, ws) }, 0);
        Ok(())
    })
    .unwrap();
    thread.join().unwrap();
    assert_eq!(
        pty.resize(101, 31).unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
}
