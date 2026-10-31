//! The launcher transfers an existing owned handle, never a PID to reopen.
use super::launch_state::{Generation, LaunchState};
use super::*;

pub(super) struct LaunchOwner {
    pub(super) state: Rc<RefCell<LaunchState>>,
    child: RefCell<Option<(Generation, Arc<ChildLifecycle>)>>,
    admitted: Cell<bool>,
}

impl LaunchOwner {
    pub(super) fn new(state: Rc<RefCell<LaunchState>>) -> Rc<Self> {
        Rc::new(Self {
            state,
            child: RefCell::new(None),
            admitted: Cell::new(false),
        })
    }

    pub(super) fn adopt_owned(&self, generation: Generation, child: Arc<ChildLifecycle>) {
        if self.state.borrow().may_acquire_child(generation) && self.child.borrow().is_none() {
            *self.child.borrow_mut() = Some((generation, child));
        }
    }

    pub(super) fn child(&self, generation: Generation) -> Option<Arc<ChildLifecycle>> {
        self.child
            .borrow()
            .as_ref()
            .filter(|(current, _)| *current == generation)
            .map(|(_, child)| Arc::clone(child))
    }

    pub(super) fn admit(&self) {
        self.admitted.set(true);
    }

    pub(super) fn note_exit(&self, code: i32) -> bool {
        let admitted = self.admitted.get() && !self.state.borrow().closed();
        self.state.borrow_mut().exited();
        if let Some((_, child)) = self.child.borrow_mut().take() {
            debug_assert_eq!(child.exit_code(), Some(code));
        }
        admitted
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn until(main: &glib::MainContext, name: &str, done: impl Fn() -> bool) {
        let started = Instant::now();
        while !done() {
            while main.iteration(false) {}
            assert!(started.elapsed() < Duration::from_secs(8), "{name}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    #[ignore = "requires private GTK/DBus and real owned children; run alone with one test thread"]
    fn late_owned_outcomes_obey_controller_close_exit_and_reentry() {
        gtk4::init().expect("private GTK display");
        let main = glib::MainContext::default();
        let _main = main.acquire().unwrap();
        for scenario in ["closed", "exited", "reentrant-close"] {
            let terminal = Terminal::new();
            let state = Rc::new(RefCell::new(launch_state::LaunchState::new(&[
                "never delivered".into(),
            ])));
            let generation = state.borrow_mut().begin().unwrap();
            let owner = LaunchOwner::new(state.clone());
            let exited = owner.clone();
            terminal.connect_child_exited(move |_, status| {
                exited.note_exit((status >> 8) & 255);
            });
            let result = Rc::new(RefCell::new(None));
            let delivered = result.clone();
            let launch = crate::gtk_spawn::spawn(
                &terminal,
                &[
                    "/bin/sh",
                    "-c",
                    if scenario == "exited" {
                        "sleep .1"
                    } else {
                        "sleep 30"
                    },
                ],
                &["PATH=/usr/bin:/bin", "TERM=xterm-256color"],
                false,
                Some("/tmp"),
                move |outcome| *delivered.borrow_mut() = Some(outcome),
            );
            until(&main, "real owned outcome", || result.borrow().is_some());
            let child = result.borrow_mut().take().unwrap().unwrap();
            if scenario == "exited" {
                until(&main, "actual exit before controller delivery", || {
                    state.borrow().child_exited()
                });
                assert!(child.exit_code().is_some());
            } else if scenario == "closed" {
                state.borrow_mut().close();
            }
            let settled = Rc::new(Cell::new(0));
            let settles = settled.clone();
            let resolved = Rc::new(Cell::new(0));
            let resolutions = resolved.clone();
            let settled_state = state.clone();
            let settled_terminal = terminal.clone();
            resolve_spawn(
                &terminal,
                owner.clone(),
                generation,
                Ok(Arc::clone(&child)),
                Some(Box::new(move || {
                    settles.set(settles.get() + 1);
                    if scenario == "reentrant-close" {
                        settled_state.borrow_mut().close();
                        crate::gtk_process::close_vte(&settled_terminal);
                    }
                })),
                Box::new(move |_| resolutions.set(resolutions.get() + 1)),
            );
            assert_eq!(settled.get(), 1, "{scenario}");
            assert_eq!(resolved.get(), 0, "{scenario}");
            assert_eq!(state.borrow().pending_count(), 0, "{scenario}");
            if let Some(adopted) = owner.child(generation) {
                assert!(Arc::ptr_eq(&adopted, &child), "{scenario}");
            }
            crate::gtk_process::close_vte(&terminal);
            until(&main, "same owned child actually drained", || {
                launch.ready()
            });
            let mut status = 0;
            assert_eq!(
                unsafe { libc::waitpid(child.pid(), &mut status, libc::WNOHANG) },
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
        }
        println!("Forge controller: late owned success after close/actual exit and reentrant settled-close suppressed UI admission and drained the original handle");
    }
}
