//! Real Bash/readline preservation under TermView search and native pane layout.
//! This exercises native reparent/resize, not the top-level split command dispatcher.
use super::*;
use gtk4 as gtk;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const RC: &str = r#"
set +o history
set -o emacs
bind 'set enable-bracketed-paste off'
bind 'set bell-style none'
_readline_snapshot() {
    printf '%s\n%s\n' "$READLINE_POINT" "$READLINE_LINE" > "$READLINE_REPORT/$$.tmp"
    mv "$READLINE_REPORT/$$.tmp" "$READLINE_REPORT/$$"
}
bind -x '"\C-x\C-d":_readline_snapshot'
PS1=$'\\[\e]133;A\a\\]$ \\[\e]133;B\a\\]'
PS2='> '
PS0='$(printf executed >> "$READLINE_REPORT/executed")'
PROMPT_COMMAND=
printf '\033]133;A\007$ \033]133;B\007printf needle\r\n\033]133;C\007'
for ((i=0; i<80; i++)); do printf 'needle history %03d: retained output for real pane resizing\r\n' "$i"; done
printf '\033]133;D;0\007'
"#;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Shell(Rc<TermView>);
impl Drop for Shell {
    fn drop(&mut self) {
        let pid = self.0.pty.pid_i32();
        self.0.pty.kill();
        // The production lifecycle owns reaping; keep the process alive to let
        // it finish, including when the test unwinds after an assertion.
        let until = Instant::now() + Duration::from_secs(3);
        while Path::new(&format!("/proc/{pid}")).exists() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn pump() {
    for _ in 0..128 {
        if !glib::MainContext::default().iteration(false) {
            break;
        }
    }
    std::thread::sleep(Duration::from_millis(2));
}
fn settle() {
    let until = Instant::now() + Duration::from_millis(250);
    while Instant::now() < until {
        pump();
    }
}
fn wait(mut ready: impl FnMut() -> bool, why: &str) {
    let until = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < until, "{why}");
        pump();
    }
    settle();
}

fn shell(root: &Path, mode: crate::config::TerminalMode, identity: &str) -> Shell {
    let mut config = Config::safe_defaults();
    config.terminal_mode = mode.clone();
    config.block_history_path = None;
    let argv = ["/bin/bash".to_owned()];
    let spawn = |_: &[&str], _: Option<&str>, _: &[(&str, &str)], _: bool| {
        // env -i also excludes inherited shell startup hooks and credentials.
        let args = [
            "/usr/bin/env".to_owned(),
            "-i".into(),
            "PATH=/usr/bin:/bin".into(),
            "TERM=xterm-256color".into(),
            "LC_ALL=C".into(),
            format!("HOME={}", root.display()),
            format!("XDG_CONFIG_HOME={}/config", root.display()),
            format!("XDG_CACHE_HOME={}/cache", root.display()),
            format!("XDG_DATA_HOME={}/data", root.display()),
            format!("XDG_STATE_HOME={}/state", root.display()),
            "INPUTRC=/dev/null".into(),
            "HISTFILE=/dev/null".into(),
            format!("READLINE_REPORT={}", root.display()),
            "/bin/bash".into(),
            "--noprofile".into(),
            "--rcfile".into(),
            root.join("bashrc").to_str().unwrap().to_owned(),
            "-i".into(),
        ];
        crate::pty::OwnedPty::spawn(
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
            Some(root.to_str().unwrap()),
            &[],
        )
    };
    let view = TermView::new_with_spawner(&config, &mode, &argv, None, Some(identity), &[], spawn);
    Shell(Rc::new(view.expect("isolated real Bash PTY")))
}

fn pty_columns(view: &TermView) -> u16 {
    let mut size = nix::libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: the live fixture owns this PTY and size is a writable winsize.
    assert_eq!(
        unsafe { nix::libc::ioctl(view.pty.master_fd_raw(), nix::libc::TIOCGWINSZ, &mut size) },
        0
    );
    size.ws_col
}

fn send(view: &TermView, bytes: &[u8]) {
    view.pty
        .write_bytes(bytes)
        .expect("readline input accepted");
}
fn snapshot(view: &TermView, root: &Path) -> (usize, String) {
    let report = root.join(view.pty.pid_i32().to_string());
    match std::fs::remove_file(&report) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => panic!("remove old synthetic report: {error}"),
    }
    send(view, b"\x18\x04");
    wait(|| report.is_file(), "readline observer did not respond");
    let contents = std::fs::read_to_string(report).unwrap();
    let (point, line) = contents
        .strip_suffix('\n')
        .unwrap()
        .split_once('\n')
        .unwrap();
    (point.parse().unwrap(), line.to_owned())
}
fn owns_focus(window: &gtk::Window, widget: &impl IsA<gtk::Widget>) -> bool {
    gtk::prelude::GtkWindowExt::focus(window)
        .is_some_and(|focus| focus == *widget.as_ref() || focus.is_ancestor(widget))
}

fn geometry_diagnostic(view: &TermView) -> String {
    let vte = &view.active_vte;
    let mut chain = Vec::new();
    let mut widget = Some(view.active.borrow().live_clip().upcast::<gtk::Widget>());
    for _ in 0..8 {
        let Some(current) = widget else { break };
        chain.push(format!(
            "{}:own_visible={},effective_visible={},child_visible={},mapped={},allocated={}x{},requested={}x{},horizontal_measure={:?}",
            current.type_().name(),
            current.get_visible(),
            current.is_visible(), current.is_child_visible(), current.is_mapped(),
            current.width(),
            current.height(),
            current.width_request(),
            current.height_request(),
            current.measure(gtk::Orientation::Horizontal, -1)
        ));
        widget = current.parent();
    }
    format!("tick_armed={} diagnostics={:?} expected_surface={:?} live_vte_own_visible={} live_vte_effective_visible={} live_vte_child_visible={} live_vte_mapped={} sent_winsize={:?} live_vte_allocated={}x{} live_vte_requested={}x{} live_vte_horizontal_measure={:?} clip_ancestors={chain:?}",
        view.resize_tick_id.borrow().is_some(), view.resize_diagnostics,
        chat::surface(view.bstate.get(), view.ftcs_seen.get()),
        vte.get_visible(), vte.is_visible(), vte.is_child_visible(), vte.is_mapped(), view.pty_winsize.sent.get(),
        vte.width(), vte.height(), vte.width_request(), vte.height_request(),
        vte.measure(gtk::Orientation::Horizontal, -1))
}

fn find_canvas(widget: &gtk::Widget) -> Option<gtk::Widget> {
    if widget.has_css_class("chat-canvas") {
        return Some(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(canvas) = find_canvas(&current) {
            return Some(canvas);
        }
        child = current.next_sibling();
    }
    None
}

#[test]
#[ignore = "requires an isolated GTK process, display, and Bash/readline"]
fn search_and_native_split_preserve_readline_draft_cursor_and_undo() {
    jterm_core::child_env::capture_inherited_environment()
        .expect("capture child environment before GTK");
    gtk::init().expect("GTK display");
    let scratch = Scratch(std::env::temp_dir().join(format!(
            "readline-native-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
    std::fs::create_dir_all(&scratch.0).unwrap();
    std::fs::write(scratch.0.join("bashrc"), RC).unwrap();
    for mode in [
        crate::config::TerminalMode::Block,
        crate::config::TerminalMode::Chat,
    ] {
        let original = shell(&scratch.0, mode.clone(), "readline-original");
        let control = shell(&scratch.0, mode.clone(), "readline-control");
        let sibling = shell(&scratch.0, mode.clone(), "readline-sibling");
        let query = gtk::SearchEntry::new();
        let bar = gtk::SearchBar::new();
        bar.set_child(Some(&query));
        bar.connect_entry(&query);
        let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        holder.set_vexpand(true);
        holder.append(&original.0.root);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&bar);
        root.append(&holder);
        let window = gtk::Window::builder()
            .default_width(1100)
            .default_height(500)
            .child(&root)
            .build();
        let control_window = gtk::Window::builder()
            .default_width(1100)
            .default_height(500)
            .child(&control.0.root)
            .build();
        control_window.present();
        window.present();
        wait(
            || {
                original.0.bstate.get() == BlockState::AwaitingCommand
                    && !original.0.finished_blocks.borrow().is_empty()
                    && control.0.bstate.get() == BlockState::AwaitingCommand
            },
            "Bash prompt and completed history must arrive",
        );
        for view in [&original.0, &control.0] {
            send(view, b"printf KEEPxyz");
            let _ = snapshot(view, &scratch.0);
            send(view, b"\x02\x02\x02Q");
        }
        let before = snapshot(&original.0, &scratch.0);
        assert_eq!(before, (12, "printf KEEPQxyz".into()));
        assert_eq!(snapshot(&control.0, &scratch.0), before);
        let session_identity = original.0.session_id.clone();
        assert_eq!(session_identity.as_deref(), Some("readline-original"));
        let identity = (
            original.0.pty.pid_i32(),
            original.0.pty.master_fd_raw(),
            original.0.active_vte.clone(),
        );
        let original_columns = pty_columns(&original.0);
        assert!(original_columns > 0);
        let width = original.0.root.width();
        assert!(width > 0);

        // Use the production native search backend with a genuine search entry.
        // This fixture intentionally does not instantiate the window controller.
        let weak = Rc::downgrade(&original.0);
        query.connect_search_changed(move |entry| {
            if let Some(view) = weak.upgrade() {
                view.find_in_blocks(entry.text().as_str(), false);
            }
        });
        for _ in 0..2 {
            bar.set_search_mode(true);
            assert!(query.grab_focus());
            query.set_text("needle");
            settle();
            let FindSearchResult::Matches(first) = original.0.find_in_blocks("needle", false)
            else {
                panic!("retained Bash history must contain searchable matches");
            };
            assert!(first.total > 1);
            let FindNavigationResult::Progress(next) = original.0.find_next() else {
                panic!("find-next must navigate, not invalidate or fall back");
            };
            assert_eq!(next.current, first.current + 1);
            assert_eq!(next.total, first.total);
            settle();
            assert!(
                owns_focus(&window, &query),
                "search navigation retains query focus"
            );
            assert_eq!(snapshot(&original.0, &scratch.0), before);
            assert!(owns_focus(&window, &query));
            query.set_text("");
            original.0.clear_find();
            bar.set_search_mode(false);
            original.0.grab_focus();
            settle();
            assert!(owns_focus(&window, &original.0.active_vte));
            assert_eq!(snapshot(&original.0, &scratch.0), before);
        }

        let before_split_geometry = format!(
            "diagnostics={:?} vte_mapped={} clip_mapped={} clip_width={}",
            original.0.resize_diagnostics,
            original.0.active_vte.is_mapped(),
            original.0.active.borrow().live_clip().is_mapped(),
            original.0.active.borrow().live_clip().width(),
        );
        let lifecycle_events = Rc::new(Cell::new((0u32, 0u32, 0u32, 0u32)));
        {
            let events = lifecycle_events.clone();
            original.0.active_vte.connect_map(move |_| {
                let (a, b, c, d) = events.get();
                events.set((a + 1, b, c, d));
            });
            let events = lifecycle_events.clone();
            original.0.active_vte.connect_unmap(move |_| {
                let (a, b, c, d) = events.get();
                events.set((a, b + 1, c, d));
            });
            let events = lifecycle_events.clone();
            original
                .0
                .active
                .borrow()
                .live_clip()
                .connect_map(move |_| {
                    let (a, b, c, d) = events.get();
                    events.set((a, b, c + 1, d));
                });
            let events = lifecycle_events.clone();
            original
                .0
                .active
                .borrow()
                .live_clip()
                .connect_unmap(move |_| {
                    let (a, b, c, d) = events.get();
                    events.set((a, b, c, d + 1));
                });
        }
        let canvas_resize_events = Rc::new(Cell::new(0u32));
        if mode == crate::config::TerminalMode::Chat {
            let canvas = find_canvas(original.0.root.upcast_ref()).expect("stable Chat canvas");
            let frame = canvas.parent().unwrap().downcast::<gtk::Overlay>().unwrap();
            let observer = frame
                .last_child()
                .unwrap()
                .downcast::<gtk::DrawingArea>()
                .unwrap();
            let events = canvas_resize_events.clone();
            observer.connect_resize(move |_, _, _| events.set(events.get() + 1));
        }

        // Native layout/reparent boundary used by split panes. This is not a
        // test of AppModel/UiState's top-level split command or shell selection.
        gtk::prelude::GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
        holder.remove(&original.0.root);
        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_hexpand(true);
        paned.set_vexpand(true);
        paned.set_resize_start_child(true);
        paned.set_resize_end_child(true);
        paned.set_shrink_start_child(true);
        paned.set_shrink_end_child(true);
        paned.set_start_child(Some(&original.0.root));
        paned.set_end_child(Some(&sibling.0.root));
        paned.set_position(width / 2);
        holder.append(&paned);
        sibling.0.grab_focus();
        let resize_deadline = Instant::now() + Duration::from_secs(5);
        while !(sibling.0.bstate.get() == BlockState::AwaitingCommand
            && original.0.root.width() < width
            && pty_columns(&original.0) < original_columns)
        {
            assert!(
                Instant::now() < resize_deadline,
                "split must resize the original live pane: mode={mode:?} original_state={:?} sibling_state={:?} old_width={width} current_width={} window_width={} split_position={} old_columns={original_columns} pty_columns={} vte_columns={} original_mapped={} sibling_mapped={} canvas_resize_events={} before_split_geometry={before_split_geometry} lifecycle_events_vte_map_unmap_clip_map_unmap={:?} geometry={}",
                original.0.bstate.get(), sibling.0.bstate.get(),
                original.0.root.width(), window.width(), paned.position(),
                pty_columns(&original.0), original.0.active_vte.column_count(),
                original.0.root.is_mapped(), sibling.0.root.is_mapped(),
                canvas_resize_events.get(), lifecycle_events.get(), geometry_diagnostic(&original.0),
            );
            pump();
        }
        settle();
        assert!(owns_focus(&window, &sibling.0.active_vte));
        assert_eq!(snapshot(&sibling.0, &scratch.0), (0, String::new()));
        send(&sibling.0, b"printf SIBLING");
        let sibling_before = snapshot(&sibling.0, &scratch.0);
        original.0.grab_focus();
        settle();
        assert!(owns_focus(&window, &original.0.active_vte));
        assert_eq!(
            (
                original.0.pty.pid_i32(),
                original.0.pty.master_fd_raw(),
                original.0.active_vte.clone()
            ),
            identity
        );
        assert_eq!(snapshot(&original.0, &scratch.0), before);
        assert_eq!(original.0.session_id, session_identity);
        send(&original.0, b"\x1f");
        send(&control.0, b"\x1f");
        let undone = snapshot(&original.0, &scratch.0);
        assert_eq!(undone, (11, "printf KEEPxyz".into()));
        assert_eq!(snapshot(&control.0, &scratch.0), undone);
        assert_eq!(snapshot(&sibling.0, &scratch.0), sibling_before);
        assert!(
            !scratch.0.join("executed").exists(),
            "no draft may be submitted"
        );
        window.close();
        control_window.close();
    }
}
