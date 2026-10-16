//! Small real-shell stage; no direct draft assignments or synthetic OSC feed.
use super::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use vte4::TerminalExt;

fn pump() {
    for _ in 0..128 {
        if !glib::MainContext::default().iteration(false) {
            break;
        }
    }
    std::thread::sleep(Duration::from_millis(2));
}
fn settle() {
    let deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < deadline {
        pump();
    }
}
fn wait(mut ready: impl FnMut() -> bool, why: &str) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !ready() {
        assert!(Instant::now() < deadline, "{why}");
        pump();
    }
    settle();
}
fn keys(args: &[&str]) {
    let mut child = Command::new("wtype").args(args).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "native Wayland keyboard injection");
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("native keyboard injection deadline");
        }
        pump();
    }
    settle();
}

struct Shell(Rc<TermView>);
impl Drop for Shell {
    fn drop(&mut self) {
        let pid = self.0.pty.pid_i32();
        self.0.pty.kill();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Path::new(&format!("/proc/{pid}")).exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
fn winsize(view: &TermView) -> (u16, u16) {
    let mut size = nix::libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: this fixture owns the PTY and provides a valid writable winsize.
    assert_eq!(
        unsafe { nix::libc::ioctl(view.pty.master_fd_raw(), nix::libc::TIOCGWINSZ, &mut size) },
        0
    );
    (size.ws_col, size.ws_row)
}
// Public text APIs expose no cell attributes or soft-wrap flags on VTE >=0.72.
// Record their exact strings without guessing which spaces are presentation.
fn extraction_snapshot(native: &vte4::Terminal) -> serde_json::Value {
    let (column, row) = native.cursor_position();
    let start = (row - 12).max(0);
    let columns = native.column_count();
    let read = |first, last| {
        native
            .text_range_format(vte4::Format::Text, first, 0, last, columns)
            .0
            .map(|text| text.to_string())
    };
    serde_json::json!({
        "cursor": [column, row], "columns": columns, "first_row": start,
        "range_text": read(start, row),
        "physical_rows": (start..=row).map(|r| serde_json::json!({"row":r,"text":read(r,r)})).collect::<Vec<_>>(),
        "cell_attributes_available": false, "soft_wrap_flags_available": false,
    })
}

fn bare_extraction_contrasts(out: &Path) {
    let cases = [
        ("fullwidth_at_last_cell", "aaaaaaaaＣ"),
        ("genuine_space_before_fullwidth", "aaaaaaaa Ｃ"),
        ("cjk_at_last_cell", "aaaaaaaa中"),
        ("combining_at_edge", "aaaaaaaae\u{301}Z"),
        ("zwj_at_edge", "aaaaaaaa👩\u{200d}💻"),
        ("ascii_soft_wrap", "aaaaaaaaXYZ"),
        ("explicit_hard_break", "aaaaaaaa\r\nＣ"),
    ];
    let mut records = Vec::new();
    for (label, bytes) in cases {
        // Separate native parser instance: never feed synthetic bytes to Bash's
        // VTE. This is extraction/rewrap evidence, not mapped render geometry.
        let native = vte4::Terminal::new();
        native.set_font(Some(&gtk4::pango::FontDescription::from_string(
            "DejaVu Sans Mono 14",
        )));
        native.set_size(9, 20);
        native.feed(bytes.as_bytes());
        let mut snapshots = Vec::new();
        for columns in [9, 6, 9] {
            native.set_size(columns, 20);
            // An ordered title fence proves the native parser consumed the
            // preceding feed; no guessed delay is the readiness oracle.
            let marker = glib::uuid_string_random().to_string();
            native.feed(format!("\x1b]2;{marker}\x07").as_bytes());
            wait(
                || {
                    native.window_title().as_deref() == Some(marker.as_str())
                        && native.column_count() == columns
                },
                "bare native parser fence and resized columns",
            );
            assert_eq!(native.column_count(), columns);
            snapshots.push(extraction_snapshot(&native));
        }
        records
            .push(serde_json::json!({"case":label,"controlled_feed":bytes,"controlled_feed_bytes":bytes.as_bytes(),"controlled_feed_sha256":glib::compute_checksum_for_data(glib::ChecksumType::Sha256, bytes.as_bytes()).unwrap().to_string(),"snapshots":snapshots}));
    }
    std::fs::write(
        out.join("bare-extraction-contrasts.json"),
        serde_json::to_vec_pretty(&records).unwrap(),
    )
    .unwrap();
}

fn capture(view: &TermView, window: &gtk4::Window, out: &Path, stage: usize, label: &str) {
    settle();
    view.render_backend.sync_geometry_to_pty();
    settle();
    let native = &view.active_vte;
    assert!(native.is_mapped());
    let grid = pty_grid_size(native, &view.block_scroll);
    assert_eq!(winsize(view), grid);
    assert_eq!(native.column_count(), i64::from(grid.0));
    assert_eq!(native.row_count(), i64::from(grid.1));
    let active = view.active.borrow();
    let clip = active.live_clip();
    let bar = active.live_scrollbar.compute_bounds(&clip).unwrap();
    let bounds = native.compute_bounds(&clip).unwrap();
    let origin = native
        .compute_point(&clip, &gtk4::graphene::Point::new(0.0, 0.0))
        .unwrap();
    let grid_right = f64::from(origin.x()) + (native.column_count() * native.char_width()) as f64;
    let paints = active.live_scrollbar.is_visible()
        && active.live_scrollbar.is_mapped()
        && active.live_scrollbar.opacity() > 0.0;
    let right = if paints {
        f64::from(bar.x()).min(f64::from(clip.width()))
    } else {
        f64::from(clip.width())
    };
    let report = serde_json::json!({
        "stage": stage, "case": label, "font": "DejaVu Sans Mono", "window": [window.width(),window.height()],
        "gtk_buffer_scale": window.scale_factor(), "pty_grid": [grid.0,grid.1], "vte_grid": [native.column_count(),native.row_count()],
        "cell": [native.char_width(),native.char_height()], "observed_application_draft": view.typed_cmd.borrow().clone(),
        "horizontal": {"clip_width": clip.width(), "native_content_width": native.width(),
            "native_bounds": [bounds.x(),bounds.y(),bounds.width(),bounds.height()],
            "native_content_origin": [origin.x(),origin.y()], "grid_right":grid_right, "unobscured_right":right,
            "scrollbar_bounds": [bar.x(),bar.y(),bar.width(),bar.height()], "scrollbar_paints":paints,
            "clearance":right-grid_right, "overlap":(grid_right-right).max(0.0)},
        "public_extraction": extraction_snapshot(native),
        "typed_shadow_fidelity": format!("{:?}", view.typed_cmd_fidelity.get()),
        "scope":"real Bash final accepted bytes; observed application draft is not a readline snapshot"
    });
    let tmp = out.join("stage.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    std::fs::rename(tmp, out.join(format!("stage-{stage}.json"))).unwrap();
    wait(
        || out.join(format!("stage-{stage}.ack")).exists(),
        "native screenshot acknowledgement",
    );
}

#[test]
#[ignore = "requires isolated Wayland, wtype and Bash; scripts/test-chat-shell-smoke.py"]
#[allow(deprecated)]
fn chat_real_bash_keyboard_and_clipboard() {
    jterm_core::child_env::capture_inherited_environment()
        .expect("capture child environment before GTK");
    gtk4::init().unwrap();
    let out = PathBuf::from(std::env::var("FORGE_F13_OUTPUT").unwrap());
    assert_eq!(std::env::var("FORGE_F13_FONT").unwrap(), "DejaVu Sans Mono");
    bare_extraction_contrasts(&out);
    let home = out.join("owned-shell");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(
        home.join("integration.bash"),
        include_str!("../../scripts/shell-integration/forge.bash"),
    )
    .unwrap();
    std::fs::write(home.join("rc"),"set +o history\nset -o emacs\nbind 'set enable-bracketed-paste on'\nPS1='$ '\nsource \"$HOME/integration.bash\"\n").unwrap();
    let mut config = crate::config::Config::safe_defaults();
    config.font_desc = "DejaVu Sans Mono 14".into();
    config.default_font_scale = 1.0;
    config.block_history_path = None;
    let args = vec![
        "/usr/bin/env".to_string(),
        "-i".into(),
        "PATH=/usr/bin:/bin".into(),
        "TERM=xterm-256color".into(),
        "LC_ALL=C.UTF-8".into(),
        format!("HOME={}", home.display()),
        "INPUTRC=/dev/null".into(),
        "HISTFILE=/dev/null".into(),
        "/bin/bash".into(),
        "--noprofile".into(),
        "--rcfile".into(),
        home.join("rc").to_str().unwrap().into(),
        "-i".into(),
    ];
    let view = Rc::new(
        TermView::new_with_spawner(
            &config,
            &crate::config::TerminalMode::Chat,
            &["/bin/bash".into()],
            None,
            None,
            &[],
            |_, _, _, _| {
                crate::pty::OwnedPty::spawn(
                    &args.iter().map(String::as_str).collect::<Vec<_>>(),
                    home.to_str(),
                    &[],
                )
            },
        )
        .unwrap(),
    );
    let _shell = Shell(view.clone());
    let window = gtk4::Window::builder()
        .title("Forge F13 native render")
        .default_width(800)
        .default_height(640)
        .child(&view.root)
        .build();
    window.present();
    settle();
    assert!(gtk4::prelude::WidgetExt::display(&window)
        .type_()
        .name()
        .contains("Wayland"));
    let loaded = view
        .active_vte
        .pango_context()
        .load_font(&view.active_vte.font().unwrap())
        .unwrap();
    assert_eq!(
        loaded.describe().family().as_deref(),
        Some("DejaVu Sans Mono")
    );
    wait(
        || view.ftcs_seen.get() && view.bstate.get() == BlockState::AwaitingCommand,
        "production shell integration prompt",
    );
    view.grab_focus();
    settle();
    // Snapshot before the strict focus oracle; do not pump GTK or inject input.
    let root_focus = gtk4::prelude::GtkWindowExt::focus(&window);
    let focus_diagnostic = serde_json::json!({
        "pid": std::process::id(),
        "native_mapped": view.active_vte.is_mapped(),
        "native_visible": view.active_vte.is_visible(),
        "native_focusable": view.active_vte.is_focusable(),
        "native_can_focus": view.active_vte.can_focus(),
        "native_sensitive": view.active_vte.is_sensitive(),
        "native_has_focus": view.active_vte.has_focus(),
        "window_active": window.is_active(),
        "window_mapped": window.is_mapped(),
        "root_focus_type": root_focus.as_ref().map(|w| w.type_().name().to_string()),
        "root_focus_is_native": root_focus.as_ref() == Some(view.active_vte.upcast_ref()),
    });
    std::fs::write(
        out.join("focus-native-before-assert.json"),
        serde_json::to_vec_pretty(&focus_diagnostic).unwrap(),
    )
    .unwrap();
    for kind in ["inputs", "seats", "tree"] {
        // Read-only public compositor diagnostics, bounded even on IPC failure.
        // Preserve the native oracle below regardless of diagnostic status.
        let result = Command::new("timeout")
            .args([
                "--kill-after=1s",
                "5s",
                "swaymsg",
                "-r",
                "-t",
                &format!("get_{kind}"),
            ])
            .output();
        match result {
            Ok(output) => {
                std::fs::write(out.join(format!("focus-{kind}.json")), output.stdout).unwrap();
                std::fs::write(
                    out.join(format!("focus-{kind}.status")),
                    format!(
                        "status={}\nstderr={}\n",
                        output.status,
                        String::from_utf8_lossy(&output.stderr)
                    ),
                )
                .unwrap();
            }
            Err(error) => eprintln!("focus diagnostic {kind}: {error}"),
        }
    }
    wait(
        || window.is_active() && view.active_vte.has_focus(),
        "native keyboard focus readiness",
    );
    assert!(view.active_vte.has_focus());
    assert!(window.is_active());
    let input_ready_tmp = out.join("input-ready.tmp");
    std::fs::write(
        &input_ready_tmp,
        serde_json::to_vec_pretty(&serde_json::json!({
            "window": [window.width(), window.height()],
            "has_focus": view.active_vte.has_focus(),
            "window_active": window.is_active(),
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::rename(input_ready_tmp, out.join("input-ready.json")).unwrap();
    wait(
        || out.join("input-ready.ack").exists(),
        "owned compositor focus before input",
    );
    assert!(view.active_vte.has_focus() && window.is_active());
    let shell_pid = view.pty.pid_i32();
    let mut records = Vec::new();
    let mut metrics = None;
    let mut tty = None;
    let mut stage = 0;
    for path in ["key", "pst"] {
        let token = glib::uuid_string_random().to_string();
        let literal = "中文 e\u{301} ＡＢＣ ".repeat(8);
        let command = format!(
            "printf '%s\\0' '{token}' '{literal}' \"$BASHPID\" \"$(tty)\" > \"$HOME/{path}\""
        );
        let target = home.join(path);
        assert_eq!(view.pty.pid_i32(), shell_pid);
        if path == "key" {
            keys(&[&command]);
            keys(&["界X"]);
            keys(&[
                "-P",
                "BackSpace",
                "-p",
                "BackSpace",
                "-P",
                "BackSpace",
                "-p",
                "BackSpace",
            ]);
        } else {
            assert!(
                view.bracketed_paste.get(),
                "real Bash must enable bracketed paste"
            );
            view.active_vte.clipboard().set_text(&command);
            // Real GDK asynchronous clipboard and production admission path.
            // This does not claim the application's global CtrlShiftV shortcut.
            let generation = view.accepted_input_generation.get();
            view.paste_from_clipboard();
            wait(
                || view.accepted_input_generation.get() > generation,
                "real clipboard admission",
            );
        }
        let observed = view.typed_cmd.borrow().clone();
        for width in [800, 480, 800] {
            window.set_default_size(width, 640);
            settle();
            assert_eq!(window.width(), width);
            assert!(!target.exists(), "input/paste must not submit before Enter");
            capture(&view, &window, &out, stage, path);
            stage += 1;
        }
        view.grab_focus();
        settle();
        assert!(view.active_vte.has_focus());
        keys(&["-P", "Return", "-p", "Return"]);
        wait(
            || target.exists(),
            "fresh native Enter must execute owned command",
        );
        let bytes = std::fs::read(&target).unwrap();
        let parts = bytes.split(|b| *b == 0).collect::<Vec<_>>();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0], token.as_bytes());
        assert_eq!(parts[1], literal.as_bytes());
        assert_eq!(parts[2], shell_pid.to_string().as_bytes());
        let actual_tty = String::from_utf8(parts[3].to_vec()).unwrap();
        assert!(actual_tty.starts_with("/dev/pts/"));
        assert_eq!(tty.get_or_insert(actual_tty.clone()), &actual_tty);
        wait(
            || {
                view.finished_blocks
                    .borrow()
                    .iter()
                    .any(|b| !b.is_background && b.cmd_text.contains(&token))
            },
            "real completed command capsule",
        );
        let capsule = {
            let blocks = view.finished_blocks.borrow();
            let block = blocks
                .iter()
                .find(|b| !b.is_background && b.cmd_text.contains(&token))
                .unwrap();
            // Persist independent truth before the unchanged identity oracle
            // can fail, so screen padding ambiguity is inspectable.
            std::fs::write(
                out.join(format!("{path}-capsule-identity.json")),
                serde_json::to_vec_pretty(&serde_json::json!({
                    "expected_command":command,"capsule_command":block.cmd_text,
                    "accepted_parts":parts.iter().map(|part| String::from_utf8_lossy(part).to_string()).collect::<Vec<_>>(),
                    "shell_pid":shell_pid,"tty":actual_tty,
                })).unwrap(),
            ).unwrap();
            assert_eq!(block.cmd_text, command);
            assert!(block.widget.has_css_class("chat-pair"));
            let terminal = &block.command_vte;
            let style = terminal.style_context();
            let pad = style.padding();
            let border = style.border();
            serde_json::json!({"font":terminal.font().unwrap().to_string(),"cell":[terminal.char_width(),terminal.char_height()],"rows":terminal.row_count(),"padding":[pad.left(),pad.right(),pad.top(),pad.bottom()],"border":[border.left(),border.right(),border.top(),border.bottom()],"height":terminal.height()})
        };
        if let Some(ref first) = metrics {
            assert_eq!(
                first, &capsule,
                "same-length keyboard/paste capsule metrics"
            );
        } else {
            metrics = Some(capsule.clone());
        }
        records.push(serde_json::json!({"path":path,"token":token,"pid":shell_pid,"tty":actual_tty,"accepted_hex":bytes.iter().map(|b|format!("{b:02x}")).collect::<String>(),"observed_draft_before_resize":observed,"capsule":capsule}));
        capture(&view, &window, &out, stage, "finished");
        stage += 1;
    }
    assert_eq!(stage, 8);
    assert_ne!(records[0]["token"], records[1]["token"]);
    std::fs::write(
        out.join("shell-acceptance.json"),
        serde_json::to_vec_pretty(&records).unwrap(),
    )
    .unwrap();
    window.close();
}
