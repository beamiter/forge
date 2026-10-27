//! Retains foreign-child identity until VTE's exact child-exited notification.
//! No cancelled completion may reopen a PID after an observed foreign reap.
use super::launch_state::{Generation, LaunchState};
use super::*;

pub(super) struct LaunchOwner {
    pub(super) state: Rc<RefCell<LaunchState>>,
    child: RefCell<Option<(Generation, Arc<ChildLifecycle>)>>,
    admitted: Cell<bool>,
    // During retirement the widget must survive to deliver GLib's exit signal.
    // note_exit breaks this intentional, bounded keepalive after retiring the PID.
    retiring_terminal: RefCell<Option<Terminal>>,
}

impl LaunchOwner {
    pub(super) fn new(state: Rc<RefCell<LaunchState>>) -> Rc<Self> {
        Rc::new(Self {
            state,
            admitted: Cell::new(false),
            child: RefCell::new(None),
            retiring_terminal: RefCell::new(None),
        })
    }

    /// Runs before any completion hook can close/reenter this attempt. The
    /// main-context-owned exit record is checked before opening any handle.
    pub(super) fn observe_success(&self, generation: Generation, pid: i32) -> std::io::Result<()> {
        if !self.state.borrow().may_acquire_child(generation) || self.child.borrow().is_some() {
            return Ok(());
        }
        let child = ChildLifecycle::new(pid, ReapOwner::Foreign)?;
        *self.child.borrow_mut() = Some((generation, child));
        Ok(())
    }

    pub(super) fn child(&self, generation: Generation) -> Option<Arc<ChildLifecycle>> {
        self.child
            .borrow()
            .as_ref()
            .filter(|(current, _)| *current == generation)
            .map(|(_, child)| child.clone())
    }

    pub(super) fn admit(&self) {
        self.admitted.set(true);
    }

    pub(super) fn note_exit(&self, code: i32) -> bool {
        self.state.borrow_mut().exited();
        // Retire before releasing the exact widget or allowing any observer to
        // run. All asynchronous cleanup workers share this same lifecycle.
        if let Some((_, child)) = self.child.borrow_mut().take() {
            child.note_foreign_exit(code);
        }
        self.retiring_terminal.borrow_mut().take();
        self.admitted.get()
    }

    pub(super) fn retain_for_reap(&self, terminal: &Terminal) {
        *self.retiring_terminal.borrow_mut() = Some(terminal.clone());
    }
}

/// A successful Pty::spawn_async result with DO_NOT_REAP_CHILD and no watcher.
/// This non-Copy token is consumed when that exclusive ownership is handed to
/// VTE. Numeric signaling is confined to its exceptional pre-watch unwind.
pub(super) struct UnwatchedChild {
    pid: glib::Pid,
}

impl UnwatchedChild {
    pub(super) fn from_spawn_success(pid: glib::Pid) -> Self {
        Self { pid }
    }

    pub(super) fn acquire(
        &self,
        owner: &LaunchOwner,
        generation: Generation,
    ) -> std::io::Result<()> {
        owner.observe_success(generation, self.pid.into_glib())
    }

    pub(super) fn abort_before_watch(&self) -> std::io::Result<()> {
        let pid = self.pid.into_glib();
        // SAFETY: this successful spawn's sole reaping ownership is still held
        // by this token. Neither the PID nor its own group ID can be recycled.
        // No watcher is installed, no wait occurs, and the token is not copied.
        unsafe {
            if libc::getpgid(pid) == pid {
                libc::kill(-pid, libc::SIGKILL);
            }
            if libc::kill(pid, libc::SIGKILL) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        }
    }

    pub(super) fn watch(self, terminal: &Terminal, pty: &vte4::Pty) {
        terminal.set_pty(Some(pty));
        terminal.watch_child(self.pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn until(main: &glib::MainContext, stage: &str, ready: impl Fn() -> bool) {
        let start = Instant::now();
        let mut work = 0;
        loop {
            while main.iteration(false) {
                work += 1;
                assert!(
                    work < 100_000 && start.elapsed() < Duration::from_secs(5),
                    "{stage}: dispatch budget"
                );
            }
            assert!(start.elapsed() < Duration::from_secs(5), "{stage}: timeout");
            if ready() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    #[ignore = "requires GTK display, Linux PTY and isolated synthetic Python children"]
    fn successful_spawn_losing_close_and_exit_before_completion_retire_exact_ownership() {
        gtk4::init().expect("GTK display");
        let main = glib::MainContext::default();
        let _guard = main.acquire().unwrap();
        for case in 0..3 {
            let close_before_completion = case == 0;
            let acquisition_failure = case == 2;
            let directory = std::env::temp_dir().join(format!(
                "forge-f10-retire-{}-{}-{case}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&directory).unwrap();
            let ready = directory.join("ready");
            let callback_ready = ready.clone();
            let terminal = Terminal::new();
            let state = Rc::new(RefCell::new(LaunchState::new(&["must-never-run".into()])));
            let owner = LaunchOwner::new(state.clone());
            let generation = state.borrow_mut().begin().unwrap();
            let exits = Rc::new(Cell::new(0));
            let exited = exits.clone();
            let exit_owner = owner.clone();
            terminal.connect_child_exited(move |_, status| {
                let code =
                    crate::process::exit_code_from_wait_status(status).expect("terminal exit");
                exit_owner.note_exit(code);
                exited.set(exited.get() + 1);
            });
            let pty = terminal
                .pty_new_sync(PtyFlags::DEFAULT, None::<&Cancellable>)
                .unwrap();
            let callback_pty = pty.clone();
            let callback_terminal = terminal.clone();
            let callback_owner = owner.clone();
            let completion = Rc::new(RefCell::new(None));
            let observed = completion.clone();
            // No shell or user startup files. The cancellation case stays alive
            // and ignores TERM; the exit case is an immediate successful exit.
            let program = if close_before_completion || acquisition_failure {
                "import signal,time,sys\nsignal.signal(signal.SIGTERM,signal.SIG_IGN)\nopen(sys.argv[1], 'x').close()\ntime.sleep(60)"
            } else {
                "pass"
            };
            pty.spawn_async(
                Some("/tmp"),
                &[
                    "/usr/bin/python3",
                    "-I",
                    "-S",
                    "-c",
                    program,
                    ready.to_str().unwrap(),
                ],
                &[],
                SpawnFlags::DEFAULT,
                || {},
                -1,
                None::<&Cancellable>,
                move |result| {
                    let pid = result.expect("this fixture must force a successful native spawn");
                    if close_before_completion || acquisition_failure {
                        // No GTK dispatch: no watcher has yet been installed.
                        // Positive readiness proves TERM is already ignored.
                        let deadline = Instant::now();
                        while !callback_ready.exists() {
                            assert!(
                                deadline.elapsed() < Duration::from_secs(3),
                                "synthetic child readiness"
                            );
                            std::thread::sleep(Duration::from_millis(2));
                        }
                    }
                    if acquisition_failure {
                        let start = Instant::now();
                        let message = unwind_unowned_spawn(
                            &callback_owner,
                            UnwatchedChild::from_spawn_success(pid),
                            &callback_terminal,
                            &callback_pty,
                            std::io::Error::from_raw_os_error(libc::EMFILE),
                        );
                        assert!(start.elapsed() < Duration::from_secs(2));
                        assert!(message.contains("Detached descendant"));
                        assert!(callback_owner.state.borrow().closed());
                        assert!(callback_owner.child.borrow().is_none());
                        *observed.borrow_mut() = Some((pid, None));
                        return;
                    }
                    callback_owner
                        .observe_success(generation, pid.into_glib())
                        .unwrap();
                    let child = callback_owner
                        .child(generation)
                        .expect("pre-watch exact child handle");
                    if close_before_completion {
                        callback_owner.state.borrow_mut().close();
                        // Real success is deliberately delivered to the CLOSED
                        // production boundary, never a cancellation-error branch.
                        let abort_started = Instant::now();
                        resolve_spawn(
                            &callback_terminal,
                            callback_owner.clone(),
                            generation,
                            Some((UnwatchedChild::from_spawn_success(pid), &callback_pty)),
                            Ok(pid),
                            None,
                            Box::new(|_| panic!("retired completion reached UI")),
                        );
                        assert!(
                            abort_started.elapsed() < Duration::from_secs(2),
                            "bounded direct abort must not block GTK"
                        );
                    } else {
                        callback_terminal.set_pty(Some(&callback_pty));
                        callback_terminal.watch_child(pid);
                    }
                    *observed.borrow_mut() = Some((pid, Some(child)));
                },
            );
            until(&main, "successful native callback", || {
                completion.borrow().is_some()
            });
            until(&main, "exact foreign reaper notification", || {
                exits.get() == 1
            });
            let (pid, child) = completion.borrow_mut().take().unwrap();
            // A child-exited signal follows VTE's sole waitpid. ECHILD proves
            // this fixture is already reaped without stealing an exit status.
            let mut status = 0;
            assert_eq!(
                unsafe { libc::waitpid(pid.into_glib(), &mut status, libc::WNOHANG) },
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
            if ready.exists() {
                std::fs::remove_file(&ready).unwrap();
            }
            std::fs::remove_dir(&directory).unwrap();
            if acquisition_failure {
                assert!(owner.retiring_terminal.borrow().is_none());
                assert!(state.borrow().closed());
                assert_eq!(state.borrow().pending_count(), 0);
                assert_eq!(exits.get(), 1);
                let weak_terminal = terminal.downgrade();
                let weak_owner = Rc::downgrade(&owner);
                drop(terminal);
                drop(owner);
                until(&main, "failed-acquisition keepalive release", || {
                    weak_terminal.upgrade().is_none() && weak_owner.upgrade().is_none()
                });
                continue;
            }
            let child = child.unwrap();
            assert!(child.exit_code().is_some());
            assert!(owner.child.borrow().is_none());
            assert!(owner.retiring_terminal.borrow().is_none());
            if !close_before_completion {
                // The actual child was reaped, then the production completion
                // path sees its old success. It must not reopen that numeric PID.
                resolve_spawn(
                    &terminal,
                    owner.clone(),
                    generation,
                    None,
                    Ok(pid),
                    None,
                    Box::new(|_| panic!("exited completion reached UI")),
                );
                owner.observe_success(generation, pid.into_glib()).unwrap();
                assert!(
                    owner.child.borrow().is_none(),
                    "observed exit forbids reacquisition"
                );
            }
            assert_eq!(state.borrow().pending_count(), 0);
            assert!(terminal_child_lifecycle(&terminal).is_none());
            assert!(
                child.signal(0).is_err(),
                "retired handle rejects even signal zero"
            );
            assert!(!child.terminate(TERMINAL_ESCALATION));
            child.force_kill_and_reap(); // cached retirement: no signal/scan/reap
            assert_eq!(exits.get(), 1, "exactly one foreign watcher");
            let weak_terminal = terminal.downgrade();
            let weak_owner = Rc::downgrade(&owner);
            drop(terminal);
            drop(owner);
            until(&main, "retirement keepalive release", || {
                weak_terminal.upgrade().is_none() && weak_owner.upgrade().is_none()
            });
        }
    }
}
