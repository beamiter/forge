//! Native-only lifetime soak, invoked by the existing display/a11y fixture.
use super::*;
use libadwaita::prelude::*;
use std::io::Read;
use std::os::fd::AsRawFd;

// BEGIN PURE LIFECYCLE BUDGET
mod budget {
    use std::time::Duration;

    pub(super) struct Budget {
        started: Duration,
        operations: usize,
    }

    impl Budget {
        pub(super) fn new(started: Duration) -> Self {
            Self {
                started,
                operations: 0,
            }
        }

        pub(super) fn check(&self, elapsed: Duration) -> Result<(), &'static str> {
            if elapsed >= Duration::from_secs(180) {
                return Err("180-second total budget exhausted");
            }
            if elapsed.saturating_sub(self.started) >= Duration::from_secs(3) {
                return Err("3-second stage budget exhausted");
            }
            Ok(())
        }

        fn step(&mut self, elapsed: Duration) -> Result<(), &'static str> {
            self.check(elapsed)?;
            if self.operations >= 10_000 {
                return Err("10000-dispatch stage work budget exhausted");
            }
            self.operations += 1;
            Ok(())
        }
    }

    /// A perpetually-ready dispatcher must fail, never return partial success.
    /// Checking after dispatch also catches the final callback exceeding time.
    pub(super) fn drain(
        budget: &mut Budget,
        mut elapsed: impl FnMut() -> Duration,
        mut dispatch: impl FnMut() -> bool,
    ) -> Result<(), &'static str> {
        loop {
            budget.step(elapsed())?;
            let more = dispatch();
            budget.check(elapsed())?;
            if !more {
                return Ok(());
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::cell::Cell;

        #[test]
        fn always_ready_dispatch_fails_at_work_limit_without_clock_progress() {
            let calls = Cell::new(0);
            let result = drain(
                &mut Budget::new(Duration::ZERO),
                || Duration::ZERO,
                || {
                    calls.set(calls.get() + 1);
                    true
                },
            );
            assert_eq!(result, Err("10000-dispatch stage work budget exhausted"));
            assert_eq!(calls.get(), 10_000);
        }

        #[test]
        fn deadlines_fail_before_dispatch_including_already_quiet_context() {
            for (start, elapsed, expected) in [
                (
                    Duration::ZERO,
                    Duration::from_secs(3),
                    "3-second stage budget exhausted",
                ),
                (
                    Duration::from_secs(179),
                    Duration::from_secs(180),
                    "180-second total budget exhausted",
                ),
            ] {
                let calls = Cell::new(0);
                assert_eq!(
                    drain(
                        &mut Budget::new(start),
                        || elapsed,
                        || {
                            calls.set(calls.get() + 1);
                            false
                        }
                    ),
                    Err(expected)
                );
                assert_eq!(calls.get(), 0);
            }
        }

        #[test]
        fn final_dispatch_overrun_is_failure_not_success() {
            for (start, after, expected) in [
                (
                    Duration::ZERO,
                    Duration::from_secs(3),
                    "3-second stage budget exhausted",
                ),
                (
                    Duration::from_secs(179),
                    Duration::from_secs(180),
                    "180-second total budget exhausted",
                ),
            ] {
                let now = Cell::new(start);
                assert_eq!(
                    drain(
                        &mut Budget::new(start),
                        || now.get(),
                        || {
                            now.set(after);
                            false
                        }
                    ),
                    Err(expected)
                );
            }
        }

        #[test]
        fn finite_dispatch_drain_and_explicit_final_success_check() {
            let calls = Cell::new(0);
            let mut budget = Budget::new(Duration::from_secs(1));
            assert_eq!(
                drain(
                    &mut budget,
                    || Duration::from_secs(1),
                    || {
                        calls.set(calls.get() + 1);
                        calls.get() < 4
                    }
                ),
                Ok(())
            );
            assert_eq!(calls.get(), 4);
            assert!(budget.check(Duration::from_millis(3999)).is_ok());
            assert_eq!(
                budget.check(Duration::from_secs(4)),
                Err("3-second stage budget exhausted")
            );
            assert_eq!(
                Budget::new(Duration::from_secs(180)).check(Duration::from_secs(180)),
                Err("180-second total budget exhausted")
            );
        }
    }
}
// END PURE LIFECYCLE BUDGET

fn checked(result: Result<(), &'static str>, iteration: usize, stage: &str) {
    assert!(
        result.is_ok(),
        "F08 iteration={iteration} stage={stage}: {result:?}"
    );
}

fn pump(
    main: &glib::MainContext,
    started: Instant,
    iteration: usize,
    stage: &str,
    budget: &mut budget::Budget,
) {
    checked(
        budget::drain(budget, || started.elapsed(), || main.iteration(false)),
        iteration,
        stage,
    );
}

fn connected(object: &impl IsA<glib::Object>, handler: libc::c_ulong) -> bool {
    // The object stays strongly held by this probe. The handler ID is captured
    // while connected, and is queried only on that same live object.
    unsafe {
        glib::gobject_ffi::g_signal_handler_is_connected(object.as_ref().as_ptr(), handler) != 0
    }
}

fn source(main: &glib::MainContext, slot: &RefCell<Option<NativeSource>>) -> glib::Source {
    main.find_source_by_id(&slot.borrow().as_ref().expect("source installed").id)
        .expect("source registered in native main context")
}

fn drain(mut slave: &std::fs::File, started: Instant, iteration: usize, stage: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 256];
    let mut budget = budget::Budget::new(started.elapsed());
    checked(
        budget::drain(
            &mut budget,
            || started.elapsed(),
            || match slave.read(&mut buffer) {
                Ok(0) => panic!("PTY sensor unexpectedly closed"),
                Ok(n) => {
                    bytes.extend_from_slice(&buffer[..n]);
                    true
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => true,
                Err(error) => panic!("PTY sensor failed: {error}"),
            },
        ),
        iteration,
        stage,
    );
    bytes
}

pub(super) fn exercise(window: &gtk4::Window, main: &glib::MainContext) {
    let host = window.downcast_ref::<libadwaita::Window>().unwrap();
    gtk4::prelude::RootExt::set_focus(window, gtk4::Widget::NONE);
    host.set_content(gtk4::Widget::NONE);
    let started = Instant::now();
    // No shell, fork, command, or input is involved in the lifecycle loop.
    let pair = nix::pty::openpty(None, None).unwrap();
    let mut attrs = nix::sys::termios::tcgetattr(&pair.slave).unwrap();
    nix::sys::termios::cfmakeraw(&mut attrs);
    nix::sys::termios::tcsetattr(&pair.slave, nix::sys::termios::SetArg::TCSANOW, &attrs).unwrap();
    let fd = pair.slave.as_raw_fd();
    // SAFETY: fcntl operates on our owned slave descriptor, with checked results.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        assert!(flags >= 0);
        assert_eq!(libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK), 0);
    }
    let slave = std::fs::File::from(pair.slave);
    let pty = vte4::Pty::foreign_sync(pair.master, None::<&gtk4::gio::Cancellable>).unwrap();
    let mut config = Config::safe_defaults();
    config.ascii_organism_enabled = true;
    config.ascii_organism_motion = Some(OrganismMotion::Full);
    let neutral = jterm_core::organism_daily::PreviewPose::Calm.context();
    assert_eq!(
        neutral,
        RenderContext::new(Behavior::Idle, BodyLanguage::default(), false)
    );

    struct CycleDiagnostic<'a>(usize, &'a Cell<&'static str>);
    impl Drop for CycleDiagnostic<'_> {
        fn drop(&mut self) {
            if std::thread::panicking() {
                eprintln!("F08 failure: iteration={} stage={}", self.0, self.1.get());
            }
        }
    }
    for iteration in 0..1000 {
        let stage = Cell::new("construct");
        let _diagnostic = CycleDiagnostic(iteration, &stage);
        let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        let terminal = vte4::Terminal::new();
        terminal.set_hexpand(true);
        terminal.set_vexpand(true);
        terminal.set_pty(Some(&pty));
        root.append(&terminal);
        let runtime = VteOrganismSurface::new(&root, &terminal, &config);
        let wait = |name: &'static str, ready: &dyn Fn() -> bool| {
            stage.set(name);
            let mut budget = budget::Budget::new(started.elapsed());
            loop {
                pump(main, started, iteration, name, &mut budget);
                let complete = ready();
                checked(budget.check(started.elapsed()), iteration, name);
                if complete {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        let quiet = || {
            assert!(
                runtime.source.borrow().is_none(),
                "iteration={iteration} stage={}",
                stage.get()
            );
            assert!(runtime.geometry_idle.borrow().is_none());
            assert!(runtime.host.borrow().is_none());
            assert!(runtime.paint.borrow().is_none());
            assert_eq!(runtime.glyph.opacity(), 0.0);
            if let Some(widgets) = runtime.expanded_widgets() {
                assert!(widgets.labels.iter().all(|label| label.opacity() == 0.0));
            }
        };
        runtime.set_local(true);
        host.set_content(Some(&root));
        terminal.grab_focus();
        wait("mapped Full", &|| {
            runtime.source.borrow().is_some() && runtime.paint.borrow().is_some()
        });
        if iteration == 0 {
            // Positive control: verify the exact VTE-to-slave path this test
            // watches. Only this test-owned control is allowed to write.
            assert!(drain(&slave, started, iteration, stage.get()).is_empty());
            terminal.feed_child(b"F08-pty-sensor");
            let observed = RefCell::new(Vec::new());
            wait("PTY sensor control", &|| {
                observed
                    .borrow_mut()
                    .extend(drain(&slave, started, iteration, stage.get()));
                observed.borrow().len() >= b"F08-pty-sensor".len()
            });
            assert_eq!(&*observed.borrow(), b"F08-pty-sensor");
        }
        runtime.last_input.set(None);
        runtime.refresh();
        let frame = source(main, &runtime.source);
        let stale_frame = runtime.source.borrow().as_ref().unwrap().ticket.clone();
        let stale_refresh = runtime.refresh_ticket.borrow().clone();
        let host_handler = unsafe { runtime.host.borrow().as_ref().unwrap().1.as_raw() };
        let (clock, paint_handler) = {
            let paint = runtime.paint.borrow();
            let (clock, handler) = paint.as_ref().unwrap();
            (clock.upgrade().unwrap(), unsafe { handler.as_raw() })
        };
        assert!(connected(window, host_handler));
        assert!(connected(&clock, paint_handler));
        runtime.defer_geometry_refresh();
        let geometry = source(main, &runtime.geometry_idle);
        runtime.apply_settings(true, OrganismMotion::Static);
        assert!(frame.is_destroyed());
        assert!(geometry.is_destroyed());
        assert!(!runtime.current_refresh(&stale_refresh));
        assert!(!consume_source(&runtime.source, &stale_frame));
        assert!(
            runtime.source.borrow().is_none(),
            "Static starts no animation source"
        );
        assert_eq!(
            runtime.glyph.text().as_str(),
            sticky_glyph_with_context(neutral, 0).as_ref()
        );
        assert!(runtime.set_expanded(true));
        wait("Static expanded", &|| {
            runtime
                .expanded_widgets()
                .is_some_and(|widgets| widgets.slot.is_mapped())
                && runtime.geometry_idle.borrow().is_none()
                && runtime.paint_plan(true, false).paint == Paint::ExpandedSprite
        });
        let widgets = runtime.expanded_widgets().unwrap();
        for label in &widgets.labels {
            assert_eq!(label.accessible_role(), gtk4::AccessibleRole::Presentation);
        }
        assert_eq!(
            runtime.glyph.accessible_role(),
            gtk4::AccessibleRole::Presentation
        );
        assert_ne!(
            runtime.capability.accessible_role(),
            gtk4::AccessibleRole::Presentation
        );
        assert!(gtk4::test_accessible_has_property(
            &runtime.capability,
            gtk4::AccessibleProperty::Description
        ));
        // Shared-core neutral fixtures are the only four decorative candidates.
        let expected = [
            sprite_frame_with_context(neutral, 0),
            sprite_frame_with_context(neutral, 35),
            sticky_glyph_with_context(neutral, 0),
            sticky_glyph_with_context(neutral, 55),
        ];
        for (label, expected) in widgets.labels.iter().zip(expected) {
            assert_eq!(label.text().as_str(), expected.as_ref());
        }
        assert!(runtime.source.borrow().is_none());
        // Static permits one input-retreat deadline, never an animation timer.
        // Set the observed age directly: don't inject terminal input or spend
        // HUMAN_INPUT_RETREAT wall time on each iteration of the soak.
        runtime.last_input.set(Some(runtime.epoch.elapsed()));
        runtime.refresh();
        let retreat = source(main, &runtime.source);
        runtime.last_input.set(None);
        runtime.refresh();
        assert!(retreat.is_destroyed());
        assert!(runtime.source.borrow().is_none());
        runtime.apply_settings(false, OrganismMotion::Full);
        stage.set("disabled");
        quiet();
        assert!(!connected(window, host_handler));
        assert!(!connected(&clock, paint_handler));
        let ticket = runtime.refresh_ticket.borrow().clone();
        window.notify("is-active");
        assert!(
            runtime.current_refresh(&ticket),
            "retired host callback ran"
        );
        runtime.apply_settings(true, OrganismMotion::Full);
        wait("reenabled", &|| runtime.source.borrow().is_some());
        let frame = source(main, &runtime.source);
        let hidden_host_handler = unsafe { runtime.host.borrow().as_ref().unwrap().1.as_raw() };
        let (hidden_clock, hidden_paint_handler) = {
            let paint = runtime.paint.borrow();
            let (clock, handler) = paint.as_ref().unwrap();
            (clock.upgrade().unwrap(), unsafe { handler.as_raw() })
        };
        root.set_visible(false);
        stage.set("hidden switch");
        quiet();
        assert!(!connected(window, hidden_host_handler));
        assert!(!connected(&hidden_clock, hidden_paint_handler));
        assert!(frame.is_destroyed());
        let ticks = runtime.frame_ticks.get();
        pump(
            main,
            started,
            iteration,
            stage.get(),
            &mut budget::Budget::new(started.elapsed()),
        );
        quiet();
        assert_eq!(runtime.frame_ticks.get(), ticks, "hidden pane animated");
        gtk4::prelude::RootExt::set_focus(window, gtk4::Widget::NONE);
        host.set_content(gtk4::Widget::NONE);
        quiet();
        root.set_visible(true);
        host.set_content(Some(&root));
        terminal.grab_focus();
        wait("remounted", &|| runtime.source.borrow().is_some());
        assert!(Rc::ptr_eq(&widgets, &runtime.expanded_widgets().unwrap()));
        let frame = source(main, &runtime.source);
        runtime.defer_geometry_refresh();
        let geometry = source(main, &runtime.geometry_idle);
        let host_handler = unsafe { runtime.host.borrow().as_ref().unwrap().1.as_raw() };
        let (clock, paint_handler) = {
            let paint = runtime.paint.borrow();
            let (clock, handler) = paint.as_ref().unwrap();
            (clock.upgrade().unwrap(), unsafe { handler.as_raw() })
        };
        let stale_refresh = runtime.refresh_ticket.borrow().clone();
        if iteration.is_multiple_of(2) {
            runtime.retire();
            stage.set("retired");
            quiet();
            assert!(!runtime.current_refresh(&stale_refresh));
            let ticket = runtime.refresh_ticket.borrow().clone();
            window.notify("is-active");
            assert!(runtime.current_refresh(&ticket));
            assert!(!runtime.set_expanded(false));
        }
        let weak_runtime = Rc::downgrade(&runtime);
        let weak_nodes: Vec<_> = std::iter::once(runtime.row.clone().upcast::<gtk4::Widget>())
            .chain(std::iter::once(runtime.glyph.clone().upcast()))
            .chain(std::iter::once(runtime.capability.clone().upcast()))
            .chain(std::iter::once(widgets.slot.clone().upcast()))
            .chain(widgets.labels.iter().map(|label| label.clone().upcast()))
            .map(|widget| widget.downgrade())
            .collect();
        assert_eq!(
            weak_nodes.len(),
            8,
            "bounded native/accessibility widget set"
        );
        stage.set("close/drop");
        drop(runtime);
        assert!(
            weak_runtime.upgrade().is_none(),
            "iteration={iteration}: adapter retained"
        );
        assert!(frame.is_destroyed());
        assert!(geometry.is_destroyed());
        assert!(!connected(window, host_handler));
        assert!(!connected(&clock, paint_handler));
        drop(widgets);
        gtk4::prelude::RootExt::set_focus(window, gtk4::Widget::NONE);
        host.set_content(gtk4::Widget::NONE);
        stage.set("before PTY detach");
        pump(
            main,
            started,
            iteration,
            stage.get(),
            &mut budget::Budget::new(started.elapsed()),
        );
        assert!(
            drain(&slave, started, iteration, stage.get()).is_empty(),
            "iteration={iteration}: unexpected PTY write before detach"
        );
        stage.set("PTY detach");
        terminal.set_pty(None);
        drop(terminal);
        drop(root);
        pump(
            main,
            started,
            iteration,
            stage.get(),
            &mut budget::Budget::new(started.elapsed()),
        );
        stage.set("widget release");
        let mut release_budget = budget::Budget::new(started.elapsed());
        loop {
            pump(main, started, iteration, stage.get(), &mut release_budget);
            let released = weak_nodes.iter().all(|node| node.upgrade().is_none());
            checked(
                release_budget.check(started.elapsed()),
                iteration,
                stage.get(),
            );
            if released {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            drain(&slave, started, iteration, stage.get()).is_empty(),
            "iteration={iteration}: unexpected PTY write"
        );
        checked(
            release_budget.check(started.elapsed()),
            iteration,
            "cycle success",
        );
        if iteration % 100 == 99 {
            eprintln!(
                "F08 lifecycle: {}/1000 cycles, elapsed={:?}",
                iteration + 1,
                started.elapsed()
            );
        }
    }
    checked(
        budget::Budget::new(started.elapsed()).check(started.elapsed()),
        1000,
        "final success",
    );
    eprintln!("F08 lifecycle: 1000/1000; sources destroyed, host/paint handlers disconnected, 8000 widget releases, zero adapter PTY bytes; elapsed={:?}", started.elapsed());
}
