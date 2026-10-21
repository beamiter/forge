//! Recovery is explicitly enabled only by ordinary local pane constructors.
//! Task and remote launch ownership remains with their existing controllers.
use super::launch_state::LaunchState;
use super::*;

pub(super) struct LaunchRecovery {
    pub(super) panel: gtk4::Box,
    pub(super) state: Rc<RefCell<LaunchState>>,
    pub(super) owner: Rc<launch_owner::LaunchOwner>,
    terminal: glib::WeakRef<Terminal>,
    root: glib::WeakRef<gtk4::Box>,
    cancellable: Cancellable,
    organism: std::rc::Weak<crate::ui::VteOrganismSurface>,
    enabled: Cell<bool>,
    shell: Entry,
    cwd: Entry,
    diagnostic: Label,
    retry: gtk4::Button,
    argv: RefCell<Vec<String>>,
    directory: RefCell<Option<String>>,
    session_id: Option<String>,
    env_extra: Vec<(String, String)>,
    auto_gui: bool,
    on_settled: RefCell<Option<Box<dyn FnOnce()>>>,
    failed_callbacks: StrCallbacks,
}

impl LaunchRecovery {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        root: &gtk4::Box,
        terminal: &Terminal,
        organism: &Rc<crate::ui::VteOrganismSurface>,
        argv: &[String],
        cwd: Option<&str>,
        session_id: Option<&str>,
        commands: &[String],
        env_extra: Vec<(String, String)>,
        auto_gui: bool,
        on_settled: Option<Box<dyn FnOnce()>>,
        failed_callbacks: StrCallbacks,
    ) -> Rc<Self> {
        let panel = gtk4::Box::new(Orientation::Vertical, 6);
        panel.set_visible(false);
        panel.set_margin_start(12);
        panel.set_margin_end(12);
        panel.set_margin_top(8);
        panel.set_margin_bottom(8);
        let diagnostic = Label::new(None);
        diagnostic.set_wrap(true);
        diagnostic.set_selectable(true);
        diagnostic.set_xalign(0.0);
        panel.append(&diagnostic);
        let shell = Entry::new();
        shell.set_text(argv.first().map(String::as_str).unwrap_or_default());
        shell.set_placeholder_text(Some("Shell executable (one path or program name)"));
        shell.update_property(&[gtk4::accessible::Property::Label("Shell executable")]);
        panel.append(&Label::new(Some("Shell executable")));
        panel.append(&shell);
        let arguments = argv
            .iter()
            .skip(1)
            .map(|arg| glib::shell_quote(arg).to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        let arguments = jterm_core::review_input::safe_inline_display(&arguments, 1024);
        let note = Label::new(Some(&format!(
            "Existing arguments are unchanged: {arguments}"
        )));
        note.set_wrap(true);
        note.set_xalign(0.0);
        panel.append(&note);
        let cwd_entry = Entry::new();
        cwd_entry.set_text(cwd.unwrap_or_default());
        cwd_entry.set_placeholder_text(Some("Working directory (blank uses home)"));
        cwd_entry.update_property(&[gtk4::accessible::Property::Label("Working directory")]);
        panel.append(&Label::new(Some("Working directory")));
        panel.append(&cwd_entry);
        let notice = Label::new(Some(if commands.is_empty() {
            "If the working directory is unavailable, the shell may start in your home directory. No startup commands are pending."
        } else {
            "If the working directory is unavailable, the shell may start in your home directory. Pending startup commands will run once after launch succeeds, including in the fallback directory."
        }));
        notice.set_wrap(true);
        notice.set_xalign(0.0);
        panel.append(&notice);
        let retry = gtk4::Button::with_label("Retry launch");
        panel.append(&retry);
        let state = Rc::new(RefCell::new(LaunchState::new(commands)));
        let recovery = Rc::new(Self {
            panel,
            owner: launch_owner::LaunchOwner::new(state.clone()),
            state,
            terminal: terminal.downgrade(),
            root: root.downgrade(),
            cancellable: Cancellable::new(),
            organism: Rc::downgrade(organism),
            enabled: Cell::new(false),
            shell,
            cwd: cwd_entry,
            diagnostic,
            retry,
            argv: RefCell::new(argv.to_vec()),
            directory: RefCell::new(cwd.map(str::to_owned)),
            session_id: session_id.map(str::to_owned),
            env_extra,
            auto_gui,
            on_settled: RefCell::new(on_settled),
            failed_callbacks,
        });
        let weak = Rc::downgrade(&recovery);
        recovery.retry.connect_clicked(move |_| {
            if let Some(recovery) = weak.upgrade() {
                recovery.retry();
            }
        });
        unsafe {
            terminal.set_data("forge-launch-recovery", Rc::downgrade(&recovery));
        }
        recovery
    }

    pub(super) fn enable(&self) {
        self.enabled.set(true);
        self.panel.set_visible(self.state.borrow().failed());
    }

    fn retry(self: &Rc<Self>) {
        if !self.enabled.get() || !self.state.borrow().failed() {
            return;
        }
        let Some(terminal) = self.terminal.upgrade() else {
            return;
        };
        let Some(root) = self.root.upgrade() else {
            return;
        };
        if crate::ui::PaneLeaf::from_widget(root.upcast_ref()).is_some_and(|leaf| {
            leaf.is_remote() || leaf.task_role().is_some() || leaf.task_session_id().is_some()
        }) {
            return;
        }
        // A queued click cannot resurrect a closed or detached pane.
        if terminal.root().is_none() {
            return;
        }
        let shell = self.shell.text().to_string();
        let cwd = self.cwd.text().to_string();
        if shell.trim().is_empty()
            || shell.chars().any(char::is_control)
            || cwd.chars().any(char::is_control)
        {
            self.diagnostic.set_text(
                "Enter a shell executable and a working directory without control characters.",
            );
            return;
        }
        let mut argv = self.argv.borrow_mut();
        if argv.is_empty() {
            argv.push(shell);
        } else {
            argv[0] = shell;
        }
        drop(argv);
        *self.directory.borrow_mut() = if cwd.is_empty() { None } else { Some(cwd) };
        self.start();
    }

    pub(super) fn start(self: &Rc<Self>) {
        let Some(terminal) = self.terminal.upgrade() else {
            return;
        };
        self.retry.set_label("Launching…");
        self.shell.set_editable(false);
        self.cwd.set_editable(false);
        let weak = Rc::downgrade(self);
        spawn_shell(
            &terminal,
            &self.argv.borrow(),
            self.directory.borrow().as_deref(),
            self.session_id.as_deref(),
            self.owner.clone(),
            self.env_extra.clone(),
            self.auto_gui,
            self.on_settled.borrow_mut().take(),
            &self.cancellable,
            Box::new(move |result| {
                let Some(recovery) = weak.upgrade() else {
                    return;
                };
                recovery.resolved(result);
            }),
        );
    }

    pub(super) fn close(&self) {
        self.state.borrow_mut().close();
        self.cancellable.cancel();
        self.panel.set_visible(false);
    }

    fn resolved(&self, result: Result<(), String>) {
        match result {
            Err(message) => {
                self.diagnostic.set_text(&message);
                self.retry.set_label("Retry launch");
                self.retry.set_sensitive(self.state.borrow().failed());
                self.shell.set_editable(true);
                self.cwd.set_editable(true);
                self.panel.set_visible(self.enabled.get());
                if let Some(organism) = self.organism.upgrade() {
                    if self.enabled.get() {
                        organism.set_local(false);
                    } else {
                        organism.retire();
                    }
                }
                for callback in self.failed_callbacks.borrow().iter() {
                    callback(&message);
                }
            }
            Ok(()) => {
                // Focus only when this recovery panel still owns focus in the
                // selected, visible pane. No deferred focus or background grab.
                if let Some(terminal) = self.terminal.upgrade() {
                    let owns_focus = terminal
                        .root()
                        .and_then(|root| root.focus())
                        .is_some_and(|focus| focus == self.panel || focus.is_ancestor(&self.panel));
                    if owns_focus
                        && terminal.is_mapped()
                        && belongs_to_selected_notebook_page(&terminal)
                    {
                        terminal.grab_focus();
                    }
                }
                self.panel.set_visible(false);
                if self.enabled.get() {
                    if let Some(organism) = self.organism.upgrade() {
                        organism.set_local(true);
                    }
                }
            }
        }
    }
}

impl Drop for LaunchRecovery {
    fn drop(&mut self) {
        self.close();
    }
}

/// Explicit teardown also runs when a widget/controller is still strongly held.
pub(crate) fn close_terminal_launch(terminal: &Terminal) {
    let recovery = unsafe {
        terminal
            .data::<std::rc::Weak<LaunchRecovery>>("forge-launch-recovery")
            .and_then(|weak| weak.as_ref().upgrade())
    };
    if let Some(recovery) = recovery {
        recovery.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    // Synthetic no-shell child: isolated Python only reads terminal bytes and
    // acknowledges them. No rc files, user commands, credentials or network.
    const SENSOR: &str = "import os,sys,tty\ntty.setraw(0)\nos.write(1,b'F10-CWD-MATCH\\r\\n' if os.path.samefile(os.getcwd(),sys.argv[1] if len(sys.argv)>1 else '/tmp') else b'F10-CWD-MISMATCH\\r\\n')\nos.write(1,b'F10-READY\\r\\n')\nb=b''\nwhile True:\n c=os.read(0,1)\n if not c: break\n if c==b'\\r':\n  os.write(1,b'F10-ACK:'+b+b'\\r\\n'); b=b''\n else: b+=c\n";

    fn pump_until(main: &glib::MainContext, stage: &str, ready: impl Fn() -> bool) {
        let start = Instant::now();
        let mut dispatches = 0;
        loop {
            while main.iteration(false) {
                dispatches += 1;
                assert!(
                    dispatches < 100_000 && start.elapsed() < Duration::from_secs(5),
                    "{stage}: dispatch budget"
                );
            }
            assert!(start.elapsed() < Duration::from_secs(5), "{stage}: timeout");
            if ready() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn wait(main: &glib::MainContext, duration: Duration) {
        let start = Instant::now();
        pump_until(main, "settle", || start.elapsed() >= duration);
    }

    fn text(terminal: &Terminal) -> String {
        terminal
            .text_format(vte4::Format::Text)
            .unwrap_or_default()
            .to_string()
    }

    // GtkEntry delegates focus to its internal GtkText. Assert actual root
    // ownership and keyboard focus, rather than has_focus on the outer Entry.
    fn focused_entry_widget(entry: &Entry) -> gtk4::Widget {
        let focus = entry
            .root()
            .and_then(|root| root.focus())
            .expect("entry root focus");
        assert!(
            focus == *entry.upcast_ref::<gtk4::Widget>() || focus.is_ancestor(entry),
            "the entry or its editable child must own root focus"
        );
        assert!(focus.has_focus(), "entry delegate must have keyboard focus");
        focus
    }

    #[test]
    #[ignore = "requires an isolated GTK display and synthetic Python PTY child"]
    fn manual_recovery_is_single_flight_scoped_and_delivers_startup_once() {
        gtk4::init().expect("GTK display");
        let main = glib::MainContext::default();
        let _guard = main.acquire().unwrap();
        let argv = vec![
            "/forge-test/missing-shell".into(),
            "-I".into(),
            "-S".into(),
            "-c".into(),
            SENSOR.into(),
        ];
        let view = Rc::new(VteTerminalView::new(
            Rc::new(RefCell::new(Config::safe_defaults())),
            &argv,
            Some("/tmp"),
            None,
            &["synthetic-startup".into()],
            Vec::new(),
            None,
        ));
        view.enable_local_launch_recovery();
        let leaf = crate::ui::PaneLeaf::Vte(view.clone());
        leaf.attach_to(&view.widget());
        leaf.set_remote(false);
        let window = gtk4::Window::new();
        window.set_default_size(900, 600);
        let row = gtk4::Box::new(Orientation::Horizontal, 0);
        row.append(&view.widget());
        let other = Entry::new();
        row.append(&other);
        window.set_child(Some(&row));
        window.present();
        let recovery = &view.launch_recovery;
        let failures = Rc::new(Cell::new(0));
        let recorded = failures.clone();
        view.connect_launch_failed(move |_| recorded.set(recorded.get() + 1));
        let exits = Rc::new(Cell::new(0));
        let recorded = exits.clone();
        view.connect_exited(move |_| recorded.set(recorded.get() + 1));
        pump_until(&main, "missing shell", || recovery.state.borrow().failed());
        assert!(recovery.panel.is_visible());
        assert_eq!(failures.get(), 1);
        assert_eq!(exits.get(), 0);
        assert_eq!(recovery.state.borrow().pending_count(), 1);
        // Remote/task metadata remains authoritative even if a stale local
        // button survives a controller reassignment.
        leaf.set_remote(true);
        recovery.retry.emit_clicked();
        assert!(recovery.state.borrow().failed());
        leaf.set_remote(false);
        // VTE falls back home for missing cwd. Only a genuine exec failure
        // belongs to the failed-launch/retry path.
        recovery.shell.set_text("/forge-test/another-missing-shell");
        recovery.cwd.set_text("/forge-test/missing-directory");
        recovery.retry.emit_clicked();
        recovery.retry.emit_clicked();
        pump_until(&main, "second missing shell", || {
            recovery.state.borrow().failed()
        });
        assert_eq!(failures.get(), 2);
        assert_eq!(exits.get(), 0);
        assert!(!text(view.vte()).contains("F10-ACK:"));
        recovery.shell.set_text("/usr/bin/python3");
        recovery.cwd.set_text("/tmp");
        recovery.retry.grab_focus();
        recovery.retry.emit_clicked();
        recovery.retry.emit_clicked();
        pump_until(&main, "synthetic child", || {
            text(view.vte()).contains("F10-ACK:synthetic-startup")
        });
        assert!(text(view.vte()).contains("F10-CWD-MATCH"));
        assert!(!text(view.vte()).contains("F10-CWD-MISMATCH"));
        let pid = view.pid_i32();
        assert!(pid > 0);
        assert!(!recovery.panel.is_visible());
        assert!(view.vte().has_focus());
        assert_eq!(recovery.state.borrow().pending_count(), 0);
        assert!(
            other.grab_focus(),
            "entry must accept focus before the action"
        );
        let other_focus = focused_entry_widget(&other);
        recovery.retry.emit_clicked();
        wait(&main, Duration::from_millis(650));
        assert_eq!(view.pid_i32(), pid);
        assert_eq!(
            text(view.vte())
                .matches("F10-ACK:synthetic-startup")
                .count(),
            1
        );
        assert_eq!(
            focused_entry_widget(&other),
            other_focus,
            "ignored retry must preserve the exact focus owner"
        );
        assert!(!view.vte().has_focus());
        assert_eq!(failures.get(), 2);
        view.kill();
        pump_until(&main, "child cleanup", || exits.get() == 1);
        crate::ui::PaneLeaf::detach_from(&view.widget());
        window.close();
    }
    #[test]
    #[ignore = "requires an isolated GTK display and synthetic Python PTY child"]
    fn unavailable_cwd_falls_back_home_and_pending_commands_run_once() {
        gtk4::init().expect("GTK display");
        let main = glib::MainContext::default();
        let _guard = main.acquire().unwrap();
        // VTE uses g_get_home_dir(), which need not match a later HOME edit.
        // The sensor compares directory identity; it writes no home files and
        // never prints either the requested path or the real home directory.
        let home = glib::home_dir()
            .to_str()
            .expect("UTF-8 test home")
            .to_owned();
        let argv = vec![
            "/forge-test/missing-shell".into(),
            "-I".into(),
            "-S".into(),
            "-c".into(),
            SENSOR.into(),
            home,
        ];
        let view = Rc::new(VteTerminalView::new(
            Rc::new(RefCell::new(Config::safe_defaults())),
            &argv,
            Some("/forge-test/missing-directory"),
            None,
            &["fallback-startup".into()],
            Vec::new(),
            None,
        ));
        view.enable_local_launch_recovery();
        let leaf = crate::ui::PaneLeaf::Vte(view.clone());
        leaf.attach_to(&view.widget());
        leaf.set_remote(false);
        let window = gtk4::Window::new();
        window.set_child(Some(&view.widget()));
        window.present();
        let failures = Rc::new(Cell::new(0));
        let recorded = failures.clone();
        view.connect_launch_failed(move |_| recorded.set(recorded.get() + 1));
        let exits = Rc::new(Cell::new(0));
        let recorded = exits.clone();
        view.connect_exited(move |_| recorded.set(recorded.get() + 1));
        let recovery = &view.launch_recovery;
        pump_until(&main, "missing executable before fallback", || {
            recovery.state.borrow().failed()
        });
        assert_eq!(failures.get(), 1);
        assert_eq!(recovery.state.borrow().pending_count(), 1);
        assert_eq!(exits.get(), 0);
        recovery.shell.set_text("/usr/bin/python3");
        recovery.retry.emit_clicked();
        recovery.retry.emit_clicked();
        pump_until(&main, "home fallback startup", || {
            text(view.vte()).contains("F10-ACK:fallback-startup")
        });
        assert!(text(view.vte()).contains("F10-CWD-MATCH"));
        assert!(!text(view.vte()).contains("F10-CWD-MISMATCH"));
        assert!(!recovery.panel.is_visible());
        assert_eq!(recovery.state.borrow().pending_count(), 0);
        let pid = view.pid_i32();
        assert!(pid > 0);
        recovery.retry.emit_clicked();
        wait(&main, Duration::from_millis(650));
        assert_eq!(view.pid_i32(), pid);
        assert_eq!(
            text(view.vte()).matches("F10-ACK:fallback-startup").count(),
            1
        );
        assert_eq!(failures.get(), 1, "cwd fallback is not a launch failure");
        view.kill();
        pump_until(&main, "fallback child cleanup", || exits.get() == 1);
        crate::ui::PaneLeaf::detach_from(&view.widget());
        window.close();
    }

    #[test]
    #[ignore = "requires an isolated GTK display and synthetic Python PTY child"]
    fn close_and_exit_retire_pending_commands_and_background_retry_keeps_focus() {
        gtk4::init().expect("GTK display");
        let main = glib::MainContext::default();
        let _guard = main.acquire().unwrap();
        let window = gtk4::Window::new();
        window.set_default_size(900, 600);
        let row = gtk4::Box::new(Orientation::Horizontal, 0);
        let other = Entry::new();
        row.append(&other);
        window.set_child(Some(&row));
        window.present();
        let make = |argv: &[String], settled: Option<Box<dyn FnOnce()>>| {
            Rc::new(VteTerminalView::new(
                Rc::new(RefCell::new(Config::safe_defaults())),
                argv,
                Some("/tmp"),
                None,
                &["never-replay".into()],
                Vec::new(),
                settled,
            ))
        };
        let resolved = Rc::new(Cell::new(0));
        let notified = resolved.clone();
        let closing = make(
            &[
                "/usr/bin/python3".into(),
                "-I".into(),
                "-S".into(),
                "-c".into(),
                SENSOR.into(),
            ],
            Some(Box::new(move || notified.set(notified.get() + 1))),
        );
        row.append(&closing.widget());
        // Raw terminal teardown and typed pane teardown share cancellation.
        crate::state::kill_terminal_child(closing.vte());
        closing.kill();
        pump_until(&main, "closed spawn settles", || resolved.get() == 1);
        wait(&main, Duration::from_millis(650));
        assert_eq!(closing.launch_recovery.state.borrow().pending_count(), 0);
        assert!(!closing.launch_recovery.panel.is_visible());
        assert_eq!(closing.pid_i32(), 0, "late child cannot install itself");
        assert!(!text(closing.vte()).contains("never-replay"));
        row.remove(&closing.widget());

        let exiting = make(
            &[
                "/usr/bin/python3".into(),
                "-I".into(),
                "-S".into(),
                "-c".into(),
                "pass".into(),
            ],
            None,
        );
        row.append(&exiting.widget());
        pump_until(&main, "exit before timer", || {
            exiting.launch_recovery.state.borrow().child_exited()
        });
        wait(&main, Duration::from_millis(650));
        assert_eq!(exiting.launch_recovery.state.borrow().pending_count(), 0);
        assert!(!text(exiting.vte()).contains("never-replay"));
        exiting.kill();
        row.remove(&exiting.widget());

        let background = make(
            &[
                "/forge-test/missing-shell".into(),
                "-I".into(),
                "-S".into(),
                "-c".into(),
                SENSOR.into(),
            ],
            None,
        );
        background.enable_local_launch_recovery();
        row.append(&background.widget());
        pump_until(&main, "background launch failure", || {
            background.launch_recovery.state.borrow().failed()
        });
        let recovery = &background.launch_recovery;
        recovery.shell.set_text("/usr/bin/python3");
        recovery.retry.grab_focus();
        recovery.retry.emit_clicked();
        assert!(
            other.grab_focus(),
            "entry must accept focus before the action"
        );
        let other_focus = focused_entry_widget(&other);
        pump_until(&main, "background retry ready", || {
            text(background.vte()).contains("F10-ACK:never-replay")
        });
        assert_eq!(
            focused_entry_widget(&other),
            other_focus,
            "completion must preserve the exact other-pane focus owner"
        );
        assert!(!background.vte().has_focus());
        background.kill();
        pump_until(&main, "background child cleanup", || {
            recovery.state.borrow().child_exited()
        });
        row.remove(&background.widget());

        let task = make(&["/forge-test/missing-shell".into()], None);
        task.enable_local_launch_recovery();
        let leaf = crate::ui::PaneLeaf::Vte(task.clone());
        leaf.attach_to(&task.widget());
        leaf.set_task_role(crate::agent_task::TaskTerminalRole::Agent);
        row.append(&task.widget());
        pump_until(&main, "task failure", || {
            task.launch_recovery.state.borrow().failed()
        });
        task.launch_recovery.shell.set_text("/usr/bin/python3");
        task.launch_recovery.retry.emit_clicked();
        assert!(
            task.launch_recovery.state.borrow().failed(),
            "task-owned attempt refuses generic retry"
        );
        task.kill();
        crate::ui::PaneLeaf::detach_from(&task.widget());
        window.close();
    }
}
