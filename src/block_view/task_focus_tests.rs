//! Physical keyboard handoff from a task button into the existing PTY.
use super::*;
use std::cell::Cell;
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

fn settle() {
    let deadline = Instant::now() + Duration::from_millis(350);
    while Instant::now() < deadline {
        while gtk4::glib::MainContext::default().iteration(false) {}
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
#[ignore = "requires isolated DISPLAY and xdotool"]
fn task_button_held_enter_never_submits_to_focused_terminal() {
    gtk4::init().expect("GTK display");
    let mut leaked = Vec::new();
    for key in ["Return", "KP_Enter"] {
        crate::enter_ownership::reset_for_test();
        let config = Config::safe_defaults();
        let view = Rc::new(
            TermView::new_with_spawner(
                &config,
                &crate::config::TerminalMode::Block,
                &["sh".into()],
                None,
                None,
                &[],
                |_, _, _, _| crate::pty::OwnedPty::for_tests(crate::pty::PtyForeground::Shell),
            )
            .unwrap(),
        );
        let button = gtk4::Button::with_label("Agent terminal");
        let clicked = Rc::new(Cell::new(0));
        let target = view.clone();
        let clicks = clicked.clone();
        button.connect_clicked(move |_| {
            clicks.set(clicks.get() + 1);
            crate::enter_ownership::claim_held();
            target.grab_focus();
        });
        let root = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
        root.append(&button);
        root.append(&view.root);
        let title = format!("forge-task-focus-{}-{key}", std::process::id());
        let window = gtk4::Window::builder()
            .title(&title)
            .default_width(640)
            .default_height(400)
            .child(&root)
            .build();
        let controller = gtk4::EventControllerKey::new();
        controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
        crate::enter_ownership::install_controller(&controller);
        window.add_controller(controller);
        crate::enter_ownership::watch_window(&window);
        window.present();
        settle();
        let output = Command::new("xdotool")
            .args(["search", "--name", &title])
            .output()
            .unwrap();
        assert!(output.status.success());
        let id = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .last()
            .unwrap()
            .to_string();
        assert!(Command::new("xdotool")
            .args(["windowfocus", "--sync", &id])
            .status()
            .unwrap()
            .success());
        assert!(button.grab_focus());
        settle();
        view.pty.drain_test_slave(Duration::from_millis(20));
        assert!(Command::new("xdotool")
            .args(["keydown", key])
            .status()
            .unwrap()
            .success());
        settle();
        let before_release = clicked.get();
        assert!(Command::new("xdotool")
            .args(["keydown", key])
            .status()
            .unwrap()
            .success());
        settle();
        assert!(Command::new("xdotool")
            .args(["keyup", key])
            .status()
            .unwrap()
            .success());
        settle();
        let bytes = view.pty.drain_test_slave(Duration::from_millis(50));
        eprintln!(
            "task focus {key}: clicked before release={before_release}, clicks={}, bytes={bytes:?}",
            clicked.get()
        );
        assert!(
            clicked.get() > 0,
            "real keyboard input must activate the task button"
        );
        assert!(
            window.is_active(),
            "xdotool must activate the native test window"
        );
        assert!(
            view.active_vte.has_focus(),
            "the live VTE must actually receive focus"
        );
        if !bytes.is_empty() {
            leaked.push((key, bytes));
        }
        // Positive control: a fresh complete Enter must reach this same PTY.
        assert!(Command::new("xdotool")
            .args(["key", key])
            .status()
            .unwrap()
            .success());
        settle();
        let control = view.pty.drain_test_slave(Duration::from_millis(50));
        assert!(
            control.iter().any(|byte| matches!(byte, b'\r' | b'\n')),
            "fresh Enter must reach the PTY recorder: {key} {control:?}"
        );
        window.close();
        settle();
    }
    assert!(
        leaked.is_empty(),
        "task focus gestures submitted to the PTY: {leaked:?}"
    );
}
