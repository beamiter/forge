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
        "range_html": native.text_range_format(vte4::Format::Html, start, 0, row, columns).0.map(|text| text.to_string()),
        "physical_rows": (start..=row).map(|r| serde_json::json!({"row":r,"text":read(r,r)})).collect::<Vec<_>>(),
        "cell_attributes_available": false, "soft_wrap_flags_available": false,
    })
}

fn bare_extraction_contrasts(out: &Path) {
    let cases = [
        ("fullwidth_at_last_cell", 9, "aaaaaaaaＣ"),
        ("genuine_space_before_fullwidth", 9, "aaaaaaaa Ｃ"),
        ("cjk_at_last_cell", 9, "aaaaaaaa中"),
        ("combining_at_edge", 9, "aaaaaaaae\u{301}Z"),
        ("zwj_at_edge", 9, "aaaaaaaa👩\u{200d}💻"),
        ("ascii_soft_wrap", 9, "aaaaaaaaXYZ"),
        ("explicit_hard_break", 9, "aaaaaaaa\r\nＣ"),
        // Controlled raw Readline redisplays from owned history-disabled Bash
        // PTYs. Only the synthetic UUID is replaced (same byte/cell length).
        // These streams never enter the actual shell terminal. EL controls and
        // genuine-space rewrites are evidence, never whitespace repair rules.
        ("readline_padding_el", 9, "aaaaaaaa \u{001b}[KＣ"),
        ("genuine_boundary_space", 9, "aaaaaaaa Ｃ"),
        ("erased_space_then_wide", 9, "aaaaaaaa \r\u{001b}[8C\u{001b}[KＣ"),
        ("erased_space_then_retyped_space", 9, "aaaaaaaa \r\u{001b}[8C\u{001b}[K Ｃ"),
        ("overwrite_boundary_space", 9, "aaaaaaaa \r\u{001b}[8CＣ"),
        ("genuine_space_hard_break", 9, "aaaaaaaa \r\nＣ"),
        ("owned_readline_no_input_space", 69, "\u{001b}[H\u{001b}[2J$ \u{001b}]133;B\u{0007}printf '%s\\0' '00000000-0000-4000-8000-000000000001' '中文 é ＡＢＣ 中文 é ＡＢＣ 中文 é ＡＢＣ 中文 é ＡＢＣ 中文 é ＡＢＣ 中文 é ＡＢ \u{001b}[KＣ 中文 é ＡＢＣ 中文 é ＡＢＣ ' \"$BASHPID\" \"$(tty)\" > \"$HOME/key\""),
        ("owned_readline_genuine_input_space", 69, "\u{001b}[H\u{001b}[2J$ \u{001b}]133;B\u{0007}printf '%s\\0' '00000000-0000-4000-8000-000000000001' '中文 é ＡＢＣ 中文 é ＡＢＣ 中文 é ＡＢＣ 中文 é ＡＢＣ 中文 é ＡＢＣ 中文 é ＡＢ Ｃ 中文 é ＡＢＣ 中文 é ＡＢＣ ' \"$BASHPID\" \"$(tty)\" > \"$HOME/key\""),
    ];
    let mut records = Vec::new();
    for (label, initial_columns, bytes) in cases {
        // Separate native parser instance: never feed synthetic bytes to Bash's
        // VTE. This is extraction/rewrap evidence, not mapped render geometry.
        let native = vte4::Terminal::new();
        native.set_font(Some(&gtk4::pango::FontDescription::from_string(
            "DejaVu Sans Mono 14",
        )));
        native.set_size(initial_columns, 20);
        native.feed(bytes.as_bytes());
        let mut snapshots = Vec::new();
        for columns in [
            initial_columns,
            if initial_columns == 69 { 40 } else { 6 },
            initial_columns,
        ] {
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
            .push(serde_json::json!({"case":label,"initial_columns":initial_columns,"controlled_feed":bytes,"controlled_feed_bytes":bytes.as_bytes(),"controlled_feed_sha256":glib::compute_checksum_for_data(glib::ChecksumType::Sha256, bytes.as_bytes()).unwrap().to_string(),"snapshots":snapshots}));
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

// Read-only test observations at the actual accepted marker/finalize sites.
// Disabled except for this owned fixture; no production field or state changes.
thread_local! {
    static METADATA_EVENTS: RefCell<Option<Vec<serde_json::Value>>> = const { RefCell::new(None) };
}
pub(super) fn metadata_observe(event: impl FnOnce() -> serde_json::Value) {
    METADATA_EVENTS.with(|events| {
        if let Some(events) = events.borrow_mut().as_mut() {
            assert!(events.len() < 32, "bounded native metadata observations");
            events.push(event());
        }
    });
}
struct MetadataObservation;
impl Drop for MetadataObservation {
    fn drop(&mut self) {
        METADATA_EVENTS.with(|events| *events.borrow_mut() = None);
    }
}

#[test]
#[ignore = "requires isolated Wayland, wtype and Bash; scripts/test-chat-shell-smoke.py --metadata"]
fn chat_real_bash_history_metadata() {
    run_real_bash_keyboard_and_clipboard(true);
}

#[test]
#[ignore = "requires isolated Wayland, wtype and Bash; scripts/test-chat-shell-smoke.py"]
#[allow(deprecated)]
fn chat_real_bash_keyboard_and_clipboard() {
    run_real_bash_keyboard_and_clipboard(false);
}

#[allow(deprecated)]
fn run_real_bash_keyboard_and_clipboard(metadata: bool) {
    let _observations = MetadataObservation;
    if metadata {
        METADATA_EVENTS.with(|events| *events.borrow_mut() = Some(Vec::new()));
    }
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
    if metadata {
        // Only the owned history-enabled lane opts in. The legacy rc above
        // and its history-disabled strict identity oracle remain unchanged.
        std::fs::write(home.join("rc"), "set -o history\nshopt -s cmdhist\nHISTCONTROL=ignoreboth\nunset HISTIGNORE HISTTIMEFORMAT\nset -o emacs\nbind 'set enable-bracketed-paste on'\nPS1='$ '\nsource \"$HOME/integration.bash\"\n").unwrap();
    }
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
        let mut command = format!(
            "printf '%s\\0' '{token}' '{literal}' \"$BASHPID\" \"$(tty)\" > \"$HOME/{path}\""
        );
        if metadata {
            // BASH_COMMAND is Bash's currently executing simple command. It is
            // independent of the history field consumed by the OSC reporter.
            command = format!(
                "printf '%s\\0' '{token}' '{literal}' \"$BASHPID\" \"$(tty)\" \"$__forge_command_id\" \"$BASH_COMMAND\" > \"$HOME/{path}\""
            );
        }
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
        assert_eq!(parts.len(), if metadata { 7 } else { 5 });
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
        let mut identity = serde_json::Value::Null;
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
            if metadata {
                let execution_id = std::str::from_utf8(parts[4]).unwrap();
                assert!(execution_id.starts_with(&format!("forge-bash-{shell_pid}-")));
                assert_eq!(parts[5], command.as_bytes(), "Bash accepted simple command");
                assert!(parts[6].is_empty());
                let events =
                    METADATA_EVENTS.with(|events| events.borrow().as_ref().unwrap().clone());
                let matching = events
                    .iter()
                    .filter(|event| event["execution_id"] == execution_id)
                    .collect::<Vec<_>>();
                identity = serde_json::json!({"execution_id":execution_id,"block_id":block.id,
                    "expected_command":command,"capsule_command":block.cmd_text,"events":matching});
                std::fs::write(
                    out.join(format!("{path}-metadata-identity.json")),
                    serde_json::to_vec_pretty(&identity).unwrap(),
                )
                .unwrap();
                assert_eq!(matching.len(), 3, "one accepted C, D and completed record");
                assert_eq!(matching[0]["phase"], "start");
                assert_eq!(matching[0]["parsed_command"], command);
                assert_eq!(matching[0]["resolved_command"], command);
                assert_eq!(matching[0]["command_source"], "ShellReported");
                assert_eq!(matching[1]["phase"], "end");
                assert_eq!(matching[1]["correlation"], "MatchedUntrusted");
                assert_eq!(matching[1]["exit_code"], 0);
                assert_eq!(matching[2]["phase"], "record");
                assert_eq!(matching[2]["command"], command);
                assert_eq!(matching[2]["command_source"], "ShellReported");
                assert_eq!(matching[2]["completion_provenance"], "ShellReported");
                assert_eq!(matching[0]["block_id"], block.id);
                assert_eq!(matching[2]["block_id"], block.id);
                let stored = view.block_data.borrow();
                let record = stored.iter().find(|record| record.id == block.id).unwrap();
                assert_eq!(record.cmd, command);
                assert!(record.command_exact && !record.command_truncated);
                identity["stored_command_exact"] = serde_json::json!(record.command_exact);
                std::fs::write(
                    out.join(format!("{path}-metadata-identity.json")),
                    serde_json::to_vec_pretty(&identity).unwrap(),
                )
                .unwrap();
            }
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
        records.push(serde_json::json!({"path":path,"token":token,"pid":shell_pid,"tty":actual_tty,"metadata_identity":identity,"accepted_hex":bytes.iter().map(|b|format!("{b:02x}")).collect::<String>(),"observed_draft_before_resize":observed,"capsule":capsule}));
        capture(&view, &window, &out, stage, "finished");
        stage += 1;
    }
    if metadata {
        let events = METADATA_EVENTS.with(|events| events.borrow().as_ref().unwrap().clone());
        assert_eq!(
            events.len(),
            6,
            "exactly two complete real command lifecycles"
        );
        let expected = records
            .iter()
            .flat_map(|record| {
                record["metadata_identity"]["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            events, expected,
            "no extra or reordered accepted lifecycles"
        );
        std::fs::write(
            out.join("metadata-events.json"),
            serde_json::to_vec_pretty(&events).unwrap(),
        )
        .unwrap();
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
