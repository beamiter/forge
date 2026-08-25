use adw::prelude::*;
use gtk4::gdk::Key;
use gtk4::gdk::ModifierType;
use gtk4::gio::{self, Cancellable};
use gtk4::{
    glib, CssProvider, EventControllerKey, EventControllerScroll, EventControllerScrollFlags,
    Notebook, Orientation, ScrolledWindow, SearchBar, SearchEntry,
};
use libadwaita as adw;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::config::{choose_shell_argv, config_file_path, load_config, load_safe_config};
use crate::keybindings::Action;
use crate::logging::init_logging;
use crate::state::{
    detach_all_pane_leaves, finalize_tabs_state, kill_all_terminal_children, load_tabs_state,
    save_all_block_histories, save_tabs_state,
};
use crate::terminal::terminal_working_directory;
use crate::ui::{self, UiState};
use jterm_core::keybindings::{Chord, KeySym, Mods, NamedKey};

/// GApplication receives only a program name. All real launch arguments are
/// consumed by `cli::handle_early_args` before GTK is initialized.
const GTK_APPLICATION_ARGV: [&str; 1] = ["forge"];
const TAB_SWITCH_FOCUS_STABLE_FRAMES: u8 = 2;
const TAB_SWITCH_FOCUS_MAX_FRAMES: u8 = 16;

fn sync_maximize_button(window: &adw::ApplicationWindow, button: &gtk4::Button) {
    let (icon_name, label) = if window.is_maximized() {
        ("window-restore-symbolic", "Restore window")
    } else {
        ("window-maximize-symbolic", "Maximize window")
    };
    button.set_icon_name(icon_name);
    button.set_tooltip_text(Some(label));
    button.update_property(&[gtk4::accessible::Property::Label(label)]);
}

fn shortcut_modifiers(state: ModifierType) -> ModifierType {
    state
        & (ModifierType::CONTROL_MASK
            | ModifierType::SHIFT_MASK
            | ModifierType::ALT_MASK
            | ModifierType::SUPER_MASK
            | ModifierType::META_MASK)
}

/// GTK edge: translate a gdk key event into the family's toolkit-neutral
/// [`Chord`], or `None` for keys no chord can bind.
///
/// The GTK-only facts live here, not in the shared grammar:
///
/// - `ISO_Left_Tab` (what GTK reports for Shift+Tab) folds to `Tab` so one
///   chord entry covers both.
/// - Ctrl/Shift/Alt and GDK's Super/Meta masks participate. The latter two map
///   to the grammar's single cross-platform `Super` modifier.
/// - Keypad digits fold onto main-row digits, matching the shared chord
///   family. `KP_Enter` and keypad operators remain distinct and unbindable.
/// - Letters and symbols go through `Key::to_unicode()` and are lowercased,
///   matching the chord core's storage invariant.
/// - `F1`..`F24` are recognized via the keysym name.
fn chord_from_gdk(keyval: Key, state: ModifierType) -> Option<Chord> {
    let masked = shortcut_modifiers(state);
    let mods = Mods {
        ctrl: masked.contains(ModifierType::CONTROL_MASK),
        shift: masked.contains(ModifierType::SHIFT_MASK),
        alt: masked.contains(ModifierType::ALT_MASK),
        sup: masked.intersects(ModifierType::SUPER_MASK | ModifierType::META_MASK),
    };
    let key = match keyval {
        Key::Tab | Key::ISO_Left_Tab => KeySym::Named(NamedKey::Tab),
        Key::Escape => KeySym::Named(NamedKey::Escape),
        Key::Return => KeySym::Named(NamedKey::Return),
        Key::space => KeySym::Named(NamedKey::Space),
        Key::BackSpace => KeySym::Named(NamedKey::Backspace),
        Key::Delete => KeySym::Named(NamedKey::Delete),
        Key::Home => KeySym::Named(NamedKey::Home),
        Key::End => KeySym::Named(NamedKey::End),
        Key::Insert => KeySym::Named(NamedKey::Insert),
        Key::Page_Up => KeySym::Named(NamedKey::PageUp),
        Key::Page_Down => KeySym::Named(NamedKey::PageDown),
        Key::Up => KeySym::Named(NamedKey::Up),
        Key::Down => KeySym::Named(NamedKey::Down),
        Key::Left => KeySym::Named(NamedKey::Left),
        Key::Right => KeySym::Named(NamedKey::Right),
        Key::KP_0 => KeySym::Char('0'),
        Key::KP_1 => KeySym::Char('1'),
        Key::KP_2 => KeySym::Char('2'),
        Key::KP_3 => KeySym::Char('3'),
        Key::KP_4 => KeySym::Char('4'),
        Key::KP_5 => KeySym::Char('5'),
        Key::KP_6 => KeySym::Char('6'),
        Key::KP_7 => KeySym::Char('7'),
        Key::KP_8 => KeySym::Char('8'),
        Key::KP_9 => KeySym::Char('9'),
        other => {
            let name = other.name();
            if let Some(n) = name.as_deref().and_then(function_key_number) {
                KeySym::Function(n)
            } else if name.as_deref().is_some_and(|n| n.starts_with("KP_")) {
                return None;
            } else {
                let c = other.to_unicode()?;
                if c.is_control() {
                    return None;
                }
                // Store Unicode-lowercased, mirroring the parser; reject
                // multi-char lowerings the same way it does.
                let mut low = c.to_lowercase();
                match (low.next(), low.next()) {
                    (Some(lc), None) => KeySym::Char(lc),
                    _ => return None,
                }
            }
        }
    };
    Some(Chord { mods, key })
}

/// `F1`..`F24` from a gdk keysym name; anything else (including `F25`+ and
/// names that merely start with `F`) is not a bindable function key.
fn function_key_number(name: &str) -> Option<u8> {
    let digits = name.strip_prefix('F')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match digits.parse::<u8>() {
        Ok(n @ 1..=24) => Some(n),
        _ => None,
    }
}

/// A shortcut which must let its trigger key finish dispatching before
/// mutating focus.
///
/// Creating and focusing a VTE from Ctrl+Shift+T's key-pressed callback sends
/// the press to the old IM context but subsequent releases to the new one.
/// fcitx/ibus can then leave the new context inactive until a later tab
/// round-trip. Waiting for T's release gives the old IM context the matching
/// press/release pair. Do not wait for modifier-state notifications here:
/// capture-phase shortcut handling does not guarantee a later `modifiers`
/// signal, which could leave tab creation pending indefinitely.
#[derive(Debug)]
struct DeferredFocusShortcut {
    action: Action,
    trigger_keycode: u32,
    trigger_released: bool,
}

impl DeferredFocusShortcut {
    fn new(action: Action, trigger_keycode: u32) -> Self {
        Self {
            action,
            trigger_keycode,
            trigger_released: false,
        }
    }

    fn key_released(&mut self, keycode: u32) {
        if keycode == self.trigger_keycode {
            self.trigger_released = true;
        }
    }

    fn ready(&self) -> bool {
        self.trigger_released
    }
}

fn take_ready_shortcut(pending: &RefCell<Option<DeferredFocusShortcut>>) -> Option<Action> {
    let mut pending = pending.borrow_mut();
    pending
        .as_ref()
        .is_some_and(DeferredFocusShortcut::ready)
        .then(|| pending.take().expect("ready shortcut must exist").action)
}

fn tab_focus_request_is_current(
    current_generation: u64,
    request_generation: u64,
    selected_page: Option<u32>,
    request_page: u32,
) -> bool {
    current_generation == request_generation && selected_page == Some(request_page)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TabFocusFrame {
    stable_frames: u8,
    should_grab: bool,
    complete: bool,
}

/// Advance one frame of a page-switch focus request.
///
/// An unmapped VTE must not receive `grab_focus()`: GTK can record it as the
/// logical focus widget before VTE's IM context is ready, suppressing the real
/// mapped `focus-in` that fcitx/ibus need. Once mapped, retry until focus has
/// remained on the exact live VTE for two consecutive frames.
fn next_tab_focus_frame(mapped: bool, has_focus: bool, stable_frames: u8) -> TabFocusFrame {
    if !mapped {
        return TabFocusFrame {
            stable_frames: 0,
            should_grab: false,
            complete: false,
        };
    }

    if !has_focus {
        return TabFocusFrame {
            stable_frames: 0,
            should_grab: true,
            complete: false,
        };
    }

    let stable_frames = stable_frames.saturating_add(1);
    TabFocusFrame {
        stable_frames,
        should_grab: false,
        complete: stable_frames >= TAB_SWITCH_FOCUS_STABLE_FRAMES,
    }
}

impl UiState {
    /// Cancel every frame-clock focus request created before a structural pane
    /// mutation that may keep the same Notebook page selected.
    pub(crate) fn invalidate_tab_focus_requests(&self) -> u64 {
        let generation = self.tab_focus_generation.get().wrapping_add(1);
        self.tab_focus_generation.set(generation);
        generation
    }

    /// Focus one exact live VTE once its (possibly reparented) page is mapped.
    /// The shared generation lets DnD replace a hover target's request with a
    /// request for the pane that actually moved, even though the page number did
    /// not change.
    pub(crate) fn request_tab_terminal_focus(
        &self,
        target_terminal: vte4::Terminal,
        page_num: u32,
    ) {
        let generation = self.invalidate_tab_focus_requests();
        let notebook_for_focus = self.notebook.clone();
        let ui_for_focus = self.clone();
        let focus_generation = self.tab_focus_generation.clone();
        let frame_count = Cell::new(0u8);
        let stable_frames = Cell::new(0u8);
        self.notebook.add_tick_callback(move |_, _| {
            if !tab_focus_request_is_current(
                focus_generation.get(),
                generation,
                notebook_for_focus.current_page(),
                page_num,
            ) {
                return glib::ControlFlow::Break;
            }

            if ui_for_focus.search_bar.is_search_mode() {
                ui_for_focus.search_apply();
                ui_for_focus.search_entry.grab_focus();
                return glib::ControlFlow::Break;
            }

            frame_count.set(frame_count.get() + 1);
            let focus_frame = next_tab_focus_frame(
                target_terminal.is_mapped(),
                target_terminal.has_focus(),
                stable_frames.get(),
            );
            stable_frames.set(focus_frame.stable_frames);
            if focus_frame.should_grab {
                target_terminal.grab_focus();
            }
            if focus_frame.complete {
                return glib::ControlFlow::Break;
            }

            if frame_count.get() >= TAB_SWITCH_FOCUS_MAX_FRAMES {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }
}

fn env_is_unset(k: &str) -> bool {
    std::env::var_os(k).is_none_or(|v| v.is_empty())
}

fn gtk_path_has_fcitx_module(path: &Path) -> bool {
    path.join("4.0.0/immodules/im-fcitx5.so").exists()
        || path.join("4.0.0/immodules/im-fcitx.so").exists()
        || path.join("4.0.0/immodules/libim-fcitx5.so").exists()
        || path.join("4.0.0/immodules/libim-fcitx.so").exists()
}

fn gtk_path_has_ibus_module(path: &Path) -> bool {
    path.join("4.0.0/immodules/im-ibus.so").exists()
        || path.join("4.0.0/immodules/libim-ibus.so").exists()
}

fn gtk_path_has_xim_module(path: &Path) -> bool {
    path.join("4.0.0/immodules/im-xim.so").exists()
        || path.join("4.0.0/immodules/libim-xim.so").exists()
}

fn candidate_fcitx_gtk_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(path) = option_env!("FCITX5_GTK_PATH").filter(|p| !p.is_empty()) {
        paths.push(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("FCITX5_GTK_PATH").filter(|p| !p.is_empty()) {
        paths.push(PathBuf::from(path));
    }
    if let Some(home) = std::env::var_os("HOME").filter(|p| !p.is_empty()) {
        paths.push(PathBuf::from(home).join(".nix-profile/lib/gtk-4.0"));
    }

    paths.extend(
        [
            "/run/current-system/sw/lib/gtk-4.0",
            "/usr/lib/gtk-4.0",
            "/usr/lib64/gtk-4.0",
            "/usr/lib/x86_64-linux-gnu/gtk-4.0",
            "/usr/local/lib/gtk-4.0",
        ]
        .into_iter()
        .map(PathBuf::from),
    );

    paths
}

fn prepend_gtk_path_if_missing(path: &Path) {
    let existing = std::env::var_os("GTK_PATH").unwrap_or_default();
    let already_present = std::env::split_paths(&existing).any(|p| p == path);
    if already_present {
        return;
    }

    let mut paths = vec![path.to_path_buf()];
    paths.extend(std::env::split_paths(&existing));
    match std::env::join_paths(paths) {
        Ok(combined) => unsafe { std::env::set_var("GTK_PATH", combined) },
        Err(err) => log::warn!("Failed to build GTK_PATH for input method: {err}"),
    }
}

fn should_use_xim_for_fcitx4(fcitx_gtk_path_found: bool, xim_gtk_path_found: bool) -> bool {
    !fcitx_gtk_path_found
        && xim_gtk_path_found
        && std::env::var("XMODIFIERS")
            .map(|s| s.contains("fcitx"))
            .unwrap_or(false)
        && !std::env::var_os("DISPLAY").is_none_or(|v| v.is_empty())
}

/// Make the GTK4 input-method module discoverable before GTK initializes, so
/// CJK preedit/commit works even when the binary is launched outside the nix
/// dev shell.
fn init_input_method_env() {
    let candidates = candidate_fcitx_gtk_paths();
    let fcitx_gtk_path = candidates.iter().find(|p| gtk_path_has_fcitx_module(p));
    let ibus_gtk_path = candidates.iter().find(|p| gtk_path_has_ibus_module(p));
    let xim_gtk_path = candidates.iter().find(|p| gtk_path_has_xim_module(p));

    if let Some(path) = fcitx_gtk_path {
        prepend_gtk_path_if_missing(path);
        log::debug!("Using fcitx GTK4 input module path {}", path.display());
    } else if let Some(path) = ibus_gtk_path {
        prepend_gtk_path_if_missing(path);
        log::debug!("Using ibus GTK4 input module path {}", path.display());
    } else if let Some(path) = xim_gtk_path {
        prepend_gtk_path_if_missing(path);
        log::debug!("Using xim GTK4 input module path {}", path.display());
    }

    let use_xim_for_fcitx4 =
        should_use_xim_for_fcitx4(fcitx_gtk_path.is_some(), xim_gtk_path.is_some());
    let gtk_im_module = std::env::var("GTK_IM_MODULE").unwrap_or_default();
    if gtk_im_module == "fcitx" && use_xim_for_fcitx4 {
        unsafe { std::env::set_var("GTK_IM_MODULE", "xim") };
        log::warn!(
            "GTK_IM_MODULE=fcitx but no GTK4 fcitx module was found; using xim via XMODIFIERS for fcitx4"
        );
    } else if gtk_im_module == "fcitx" && fcitx_gtk_path.is_none() {
        log::warn!(
            "GTK_IM_MODULE=fcitx but no GTK4 fcitx module was found. fcitx4 needs a GTK4 fcitx/xim module; install fcitx5-gtk or use ibus for GTK4 apps."
        );
    } else if env_is_unset("GTK_IM_MODULE") {
        let xmods = std::env::var("XMODIFIERS").unwrap_or_default();
        let module = if use_xim_for_fcitx4 {
            "xim"
        } else if xmods.contains("ibus")
            || (!std::env::var_os("IBUS_ADDRESS").is_none_or(|v| v.is_empty())
                && fcitx_gtk_path.is_none())
            || (fcitx_gtk_path.is_none() && ibus_gtk_path.is_some())
        {
            "ibus"
        } else {
            "fcitx"
        };
        unsafe { std::env::set_var("GTK_IM_MODULE", module) };
    }

    if env_is_unset("XMODIFIERS") {
        let module = std::env::var("GTK_IM_MODULE").unwrap_or_else(|_| "fcitx".to_string());
        if matches!(module.as_str(), "fcitx" | "ibus") {
            unsafe { std::env::set_var("XMODIFIERS", format!("@im={module}")) };
        }
    }
}

pub fn run() -> glib::ExitCode {
    // Freeze the launch-time environment before CLI parsing writes FORGE_*,
    // before input-method setup rewrites GTK_PATH/GTK_IM_MODULE/XMODIFIERS,
    // and before GTK or any worker thread starts: terminal and PTY children
    // spawned later must inherit the environment as it was at launch, not
    // these process-only mutations. (Other spawn paths — notebook cell
    // workers, the `flatpak-spawn` host bridge — still start from the live
    // environment.) A second capture would mean this ordering broke, so
    // treat it as a startup error.
    if let Err(err) = jterm_core::child_env::capture_inherited_environment() {
        eprintln!("forge: failed to capture the inherited environment: {err}");
        return glib::ExitCode::FAILURE;
    }
    jterm_core::identity::init(jterm_core::identity::AppIdentity {
        app_name: "forge",
        app_id: crate::host::APP_ID,
        // This crate's version, not core's: it is what child shells read as
        // TERM_PROGRAM_VERSION, so a tool feature-gating on the
        // TERM_PROGRAM/TERM_PROGRAM_VERSION pair must not be told the shared
        // library's version alongside our name.
        app_version: env!("CARGO_PKG_VERSION"),
    });
    if let Some(code) = crate::cli::handle_early_args() {
        return code;
    }
    let launch_options = crate::cli::launch_options().clone();
    init_logging();
    init_input_method_env();

    // NON_UNIQUE: each launch is its own process with its own window, instead of
    // the second invocation activating the first instance and then exiting.
    let app = adw::Application::builder()
        .application_id(crate::host::APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();

    app.connect_activate(move |app| {
        let launch = launch_options.clone();
        let (config, themes, keybinding_map) = if launch.safe_mode {
            load_safe_config()
        } else {
            load_config()
        };

        // Cache shell selection once to avoid extra process probes per new tab.
        let shell_argv = Rc::new(RefCell::new(if launch.safe_mode {
            vec!["sh".to_string()]
        } else {
            choose_shell_argv(config.shell.as_deref())
        }));

        let window_opacity = Rc::new(Cell::new(config.window_opacity));
        let config = Rc::new(RefCell::new(config));
        let available_themes = Rc::new(themes);
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .default_width(800)
            .default_height(600)
            .title("forge")
            .name("win_name")
            .modal(false)
            .resizable(true)
            .opacity(window_opacity.get())
            .build();

        // Create notebook for tabs (tabs hidden — custom tab bar is used instead)
        let notebook = Notebook::builder()
            .hexpand(true)
            .vexpand(true)
            .scrollable(true)
            .show_border(false)
            .show_tabs(false)
            .build();
        notebook.add_css_class("hidden-tabs");

        // Create search bar
        let search_entry = SearchEntry::new();
        search_entry.set_hexpand(true);
        search_entry.set_placeholder_text(Some("Find in blocks…"));
        search_entry.update_property(&[gtk4::accessible::Property::Label(
            "Find in terminal history",
        )]);

        let search_status = gtk4::Label::new(None);
        search_status.add_css_class("dim-label");
        search_status.set_accessible_role(gtk4::AccessibleRole::Status);
        search_status.set_width_chars(16);
        search_status.set_xalign(1.0);

        let search_prev_btn = gtk4::Button::from_icon_name("go-up-symbolic");
        search_prev_btn.set_tooltip_text(Some("Previous match (Shift+Enter)"));
        search_prev_btn.set_focus_on_click(false);
        search_prev_btn.update_property(&[gtk4::accessible::Property::Label(
            "Previous search match",
        )]);
        let search_next_btn = gtk4::Button::from_icon_name("go-down-symbolic");
        search_next_btn.set_tooltip_text(Some("Next match (Enter)"));
        search_next_btn.set_focus_on_click(false);
        search_next_btn
            .update_property(&[gtk4::accessible::Property::Label("Next search match")]);
        let search_close_btn = gtk4::Button::from_icon_name("window-close-symbolic");
        search_close_btn.set_tooltip_text(Some("Close search (Escape)"));
        search_close_btn.set_focus_on_click(false);
        search_close_btn.update_property(&[gtk4::accessible::Property::Label("Close search")]);

        let search_box = gtk4::Box::new(Orientation::Horizontal, 4);
        search_box.append(&search_entry);
        search_box.append(&search_status);
        search_box.append(&search_prev_btn);
        search_box.append(&search_next_btn);
        search_box.append(&search_close_btn);
        search_box.set_margin_start(4);
        search_box.set_margin_end(4);
        search_box.set_margin_top(2);
        search_box.set_margin_bottom(2);

        let search_bar = SearchBar::new();
        search_bar.set_child(Some(&search_box));
        search_bar.set_show_close_button(false);
        search_bar.connect_entry(&search_entry);

        // Custom tab bar CSS
        let css_provider = CssProvider::new();
        let bottom_bar_height = jterm_core::bottom_bar::BAR_HEIGHT;
        css_provider.load_from_string(&format!(
            "{}{} .bottom-bar {{ min-height: {bottom_bar_height}px; }}",
            ui::PANE_HEADER_CSS,
            ".tab-strip-btn { padding: 4px 8px; border-radius: 4px; border-bottom: 1px solid alpha(currentColor, 0.1); margin-bottom: 2px; }
             .tab-strip-btn:checked { font-weight: bold; border-radius: 4px; background-color: alpha(currentColor, 0.14); outline: 2px solid alpha(currentColor, 0.8); outline-offset: -2px; }
             .tab-strip-close { min-width: 16px; min-height: 16px; padding: 0; margin: 0; }
             .file-tree-drop-hover { background-color: alpha(currentColor, 0.16); border-radius: 4px; outline: 1px dashed alpha(currentColor, 0.45); outline-offset: -1px; }
             .tab-resize-handle { min-width: 8px; margin-left: 2px; border-left: 1px solid alpha(currentColor, 0.24); }
             .tab-resize-handle:hover { border-left-color: currentColor; }
             .sidebar-box { min-width: 140px; padding: 2px 4px; }
             .top-bar { padding: 2px 4px; }
             .hidden-tabs > header { min-height: 0; border: none; background: none; padding: 0; margin: 0; }
             .hidden-tabs > header > * { min-height: 0; min-width: 0; padding: 0; margin: 0; }
             .terminal-box scrollbar slider { min-width: 6px; border-radius: 3px; }
             .terminal-box scrollbar { padding: 0; }
             .tab-activity { font-style: italic; }
             @keyframes bell-flash { 0% { opacity: 1.0; } 50% { opacity: 0.5; } 100% { opacity: 1.0; } }
             .tab-bell-flash { animation: bell-flash 0.3s ease-in-out 2; }
             .tab-pinned { font-weight: bold; }
             .tab-dragging { opacity: 0.5; }
             .tab-drop-target { background-color: alpha(currentColor, 0.15); }
             .top-tabs .tab-drop-before { box-shadow: inset 3px 0 currentColor; }
             .top-tabs .tab-drop-after { box-shadow: inset -3px 0 currentColor; }
             .tab-strip:not(.top-tabs) .tab-drop-before { box-shadow: inset 0 3px currentColor; }
             .tab-strip:not(.top-tabs) .tab-drop-after { box-shadow: inset 0 -3px currentColor; }
             .tab-process-indicator { font-size: 0.8em; opacity: 0.6; margin-left: 4px; }
             .tab-pin-icon { font-size: 0.9em; opacity: 0.8; margin-right: 2px; }
             .tab-selected { background-color: alpha(currentColor, 0.14); outline: 2px solid alpha(currentColor, 0.8); outline-offset: -2px; }
             .tab-conn-dot { font-size: 0.7em; margin-right: 2px; }
             .window-controls { margin-left: 2px; }
             .window-control { min-width: 24px; min-height: 24px; padding: 0; border-radius: 999px; }
             @keyframes conn-pulse { 0% { opacity: 1.0; } 50% { opacity: 0.35; } 100% { opacity: 1.0; } }
             .tab-conn-dot.tab-connecting { animation: conn-pulse 1.2s ease-in-out infinite; }
             .tab-strip-search { padding: 4px 8px; margin: 2px 4px; }
             .top-tabs .tab-strip-btn { border-bottom: none; margin-bottom: 0; margin-right: 2px; }
             .bottom-bar { padding: 0 8px; border-top: 1px solid alpha(currentColor, 0.20); font-size: 0.85em; }
             .file-tree-box { border-top: 1px solid alpha(currentColor, 0.15); }
             .file-tree-header { padding: 2px 4px; }
             .file-tree-root { font-size: 0.85em; opacity: 0.7; }
             .file-tree { padding: 2px; }",
        ));
        gtk4::style_context_add_provider_for_display(
            &gtk4::gdk::Display::default().expect("display"),
            &css_provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        // Keep the visible command/navigation entry points in the same order as
        // Anvil: command center, sidebar, then tab placement.
        let command_palette_btn = gtk4::Button::from_icon_name("system-search-symbolic");
        command_palette_btn.set_focus_on_click(false);
        command_palette_btn.set_focusable(true);
        command_palette_btn.set_tooltip_text(Some("Open command center (Ctrl+Shift+P)"));
        command_palette_btn.update_property(&[
            gtk4::accessible::Property::Label("Open command center"),
            gtk4::accessible::Property::KeyShortcuts("Control+Shift+P"),
        ]);
        command_palette_btn.add_css_class("flat");

        let toggle_sidebar_btn = gtk4::Button::from_icon_name("sidebar-show-symbolic");
        toggle_sidebar_btn.set_focus_on_click(false);
        toggle_sidebar_btn.set_focusable(true);
        toggle_sidebar_btn.set_tooltip_text(Some("Toggle sidebar (Ctrl+\\)"));
        toggle_sidebar_btn.update_property(&[
            gtk4::accessible::Property::Label("Show or hide sidebar"),
            gtk4::accessible::Property::KeyShortcuts("Control+\\"),
        ]);
        toggle_sidebar_btn.add_css_class("flat");

        let add_tab_button = gtk4::Button::from_icon_name("list-add-symbolic");
        add_tab_button.set_focus_on_click(false);
        add_tab_button.set_focusable(true);
        add_tab_button.set_tooltip_text(Some("New tab (Ctrl+Shift+T)"));
        add_tab_button.update_property(&[
            gtk4::accessible::Property::Label("New terminal tab"),
            gtk4::accessible::Property::KeyShortcuts("Control+Shift+T"),
        ]);
        add_tab_button.add_css_class("flat");

        // Toggles the tab bar between the left sidebar and the top bar.
        let toggle_placement_btn = gtk4::Button::from_icon_name("view-list-symbolic");
        toggle_placement_btn.set_focus_on_click(false);
        toggle_placement_btn.set_focusable(true);
        toggle_placement_btn.set_tooltip_text(Some("Toggle tabs: sidebar / top bar"));
        toggle_placement_btn.update_property(&[
            gtk4::accessible::Property::Label("Move tabs between sidebar and top bar"),
            gtk4::accessible::Property::KeyShortcuts("Control+Alt+B"),
        ]);
        toggle_placement_btn.add_css_class("flat");

        // Holder for the tab strip when it lives in the top bar (horizontal).
        let top_tab_scroll = ScrolledWindow::new();
        top_tab_scroll.set_hexpand(true);
        top_tab_scroll.set_vexpand(false);
        top_tab_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Never);
        top_tab_scroll.set_overflow(gtk4::Overflow::Hidden);
        top_tab_scroll.set_width_request(0);
        top_tab_scroll.set_min_content_width(0);
        top_tab_scroll.set_max_content_width(1);
        top_tab_scroll.set_propagate_natural_width(false);
        top_tab_scroll.add_css_class("top-tab-scroll");
        top_tab_scroll.set_visible(false);
        top_tab_scroll.set_margin_start(128);
        top_tab_scroll.set_margin_end(104);

        // Overlay the leading controls, tab strip, and trailing actions so tab
        // geometry is independent of which controls happen to be visible.
        let top_bar = gtk4::Overlay::new();
        top_bar.add_css_class("top-bar");
        top_bar.set_height_request(40);
        top_bar.set_hexpand(true);
        let top_bar_background = gtk4::Box::new(Orientation::Horizontal, 0);
        top_bar.set_child(Some(&top_bar_background));

        let leading_actions = gtk4::Box::new(Orientation::Horizontal, 4);
        leading_actions.set_halign(gtk4::Align::Start);
        leading_actions.set_valign(gtk4::Align::Center);
        leading_actions.append(&command_palette_btn);
        leading_actions.append(&toggle_sidebar_btn);
        leading_actions.append(&toggle_placement_btn);
        top_bar.add_overlay(&leading_actions);
        top_bar.add_overlay(&top_tab_scroll);

        // A compact, stateful counterpart to Ctrl+Alt+G. Its checked state
        // follows the lifetime of the approval-gated Shell Agent session;
        // the full name remains available through its tooltip/accessibility.
        let agent_toggle = gtk4::ToggleButton::new();
        agent_toggle.set_icon_name("system-run-symbolic");
        agent_toggle.set_focus_on_click(false);
        agent_toggle.set_focusable(true);
        agent_toggle.set_tooltip_text(Some("Activate Shell Agent (Ctrl+Alt+G)"));
        agent_toggle.update_property(&[
            gtk4::accessible::Property::Label("Shell Agent"),
            gtk4::accessible::Property::KeyShortcuts("Control+Alt+G"),
        ]);
        agent_toggle.add_css_class("flat");

        // AdwApplicationWindow does not add a HeaderBar on its own. Keep the
        // compact custom bar, but give it the two pieces a real titlebar needs:
        //
        // - Explicit buttons avoid GtkWindowControls disabling minimize and
        //   maximize when its inherited window actions are unavailable in the
        //   current desktop/window state.
        // - WindowHandle gives non-interactive parts of the bar native
        //   move/double-click/right-click titlebar behavior on Wayland and X11.
        //
        // Interactive children (buttons, tab drag sources, search) continue to
        // receive their own gestures before the handle considers a window move.
        let trailing_actions = gtk4::Box::new(Orientation::Horizontal, 4);
        trailing_actions.set_halign(gtk4::Align::End);
        trailing_actions.set_valign(gtk4::Align::Center);
        trailing_actions.add_css_class("top-bar-actions");
        trailing_actions.add_css_class("window-controls");

        let minimize_window_button =
            gtk4::Button::from_icon_name("window-minimize-symbolic");
        minimize_window_button.add_css_class("flat");
        minimize_window_button.add_css_class("window-control");
        minimize_window_button.set_focus_on_click(false);
        minimize_window_button.set_tooltip_text(Some("Minimize window"));
        minimize_window_button
            .update_property(&[gtk4::accessible::Property::Label("Minimize window")]);

        let maximize_window_button =
            gtk4::Button::from_icon_name("window-maximize-symbolic");
        maximize_window_button.add_css_class("flat");
        maximize_window_button.add_css_class("window-control");
        maximize_window_button.set_focus_on_click(false);
        sync_maximize_button(&window, &maximize_window_button);

        let close_window_button = gtk4::Button::from_icon_name("window-close-symbolic");
        close_window_button.add_css_class("flat");
        close_window_button.add_css_class("window-control");
        close_window_button.set_focus_on_click(false);
        close_window_button.set_tooltip_text(Some("Close window"));
        close_window_button
            .update_property(&[gtk4::accessible::Property::Label("Close window")]);

        trailing_actions.append(&agent_toggle);
        trailing_actions.append(&add_tab_button);
        trailing_actions.append(&minimize_window_button);
        trailing_actions.append(&maximize_window_button);
        trailing_actions.append(&close_window_button);
        top_bar.add_overlay(&trailing_actions);

        let top_bar_handle = gtk4::WindowHandle::new();
        top_bar_handle.set_child(Some(&top_bar));

        {
            let window = window.clone();
            minimize_window_button.connect_clicked(move |_| window.minimize());
        }
        {
            let window = window.clone();
            maximize_window_button.connect_clicked(move |_| {
                if window.is_maximized() {
                    window.unmaximize();
                } else {
                    window.maximize();
                }
            });
        }
        {
            let button = maximize_window_button.clone();
            window.connect_maximized_notify(move |window| sync_maximize_button(window, &button));
        }
        {
            let window = window.clone();
            close_window_button.connect_clicked(move |_| window.close());
        }

        // Vertical sidebar with tab buttons (collapsible)
        let tab_strip = gtk4::Box::new(Orientation::Vertical, 2);
        tab_strip.add_css_class("tab-strip");
        tab_strip.set_hexpand(false);
        tab_strip.set_vexpand(true);
        tab_strip.set_valign(gtk4::Align::Start);

        let tab_strip_scroll = ScrolledWindow::new();
        tab_strip_scroll.set_hexpand(false);
        tab_strip_scroll.set_vexpand(true);
        tab_strip_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        tab_strip_scroll.set_child(Some(&tab_strip));

        let sidebar = gtk4::Box::new(Orientation::Vertical, 0);
        sidebar.add_css_class("sidebar-box");

        // Tab search entry for filtering. It always lives in the sidebar's Tabs
        // view, in both tab placements: the top bar is reserved for the tabs
        // themselves and the window controls, and a filter field there would eat
        // horizontal space the tab strip needs.
        // Keep the filter in normal keyboard focus order. Pointer and shortcut
        // access are useful, but they cannot be the only ways to reach a visible
        // text field.
        let tab_search_entry = SearchEntry::new();
        tab_search_entry.set_placeholder_text(Some("Filter tabs..."));
        tab_search_entry.update_property(&[gtk4::accessible::Property::Label("Filter tabs")]);
        tab_search_entry.add_css_class("tab-strip-search");
        let tab_search_wrapper = gtk4::Box::new(Orientation::Horizontal, 0);
        tab_search_wrapper.append(&tab_search_entry);

        // Mirror list, shown in place of the strip's holder when the strip is
        // docked to the top bar. Exactly one of the two is ever visible.
        let sidebar_tab_mirror = gtk4::Box::new(Orientation::Vertical, 2);
        sidebar_tab_mirror.add_css_class("tab-strip");
        sidebar_tab_mirror.set_hexpand(false);
        sidebar_tab_mirror.set_vexpand(true);
        sidebar_tab_mirror.set_valign(gtk4::Align::Start);

        let sidebar_tab_mirror_scroll = ScrolledWindow::new();
        sidebar_tab_mirror_scroll.set_hexpand(false);
        sidebar_tab_mirror_scroll.set_vexpand(true);
        sidebar_tab_mirror_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        sidebar_tab_mirror_scroll.set_child(Some(&sidebar_tab_mirror));
        sidebar_tab_mirror_scroll.set_visible(false);

        // Tabs view: filter entry + tab strip (or its mirror).
        let sidebar_tabs_page = gtk4::Box::new(Orientation::Vertical, 0);
        sidebar_tabs_page.set_vexpand(true);
        sidebar_tabs_page.append(&tab_search_wrapper);
        sidebar_tabs_page.append(&tab_strip_scroll);
        sidebar_tabs_page.append(&sidebar_tab_mirror_scroll);

        // File tree section (header + tree), shown in the sidebar.
        let file_tree_location = Rc::new(RefCell::new(ui::FsLocation::Local));
        let (file_tree_model, file_tree) =
            ui::build_file_tree_widgets(file_tree_location.clone(), config.clone());

        let file_tree_scroll = ScrolledWindow::new();
        file_tree_scroll.set_hexpand(false);
        file_tree_scroll.set_vexpand(true);
        file_tree_scroll.set_policy(gtk4::PolicyType::Automatic, gtk4::PolicyType::Automatic);
        file_tree_scroll.set_child(Some(&file_tree));

        let file_tree_root_label = gtk4::Label::new(Some("~"));
        file_tree_root_label.set_hexpand(true);
        file_tree_root_label.set_xalign(0.0);
        file_tree_root_label.set_ellipsize(gtk4::pango::EllipsizeMode::Start);
        file_tree_root_label.add_css_class("file-tree-root");

        let file_tree_cwd_btn = gtk4::Button::from_icon_name("go-home-symbolic");
        file_tree_cwd_btn.set_focus_on_click(false);
        file_tree_cwd_btn.set_focusable(true);
        file_tree_cwd_btn.set_tooltip_text(Some("Jump to current tab directory"));
        file_tree_cwd_btn.update_property(&[gtk4::accessible::Property::Label(
            "Show current terminal directory",
        )]);
        file_tree_cwd_btn.add_css_class("flat");

        let file_tree_up_btn = gtk4::Button::from_icon_name("go-up-symbolic");
        file_tree_up_btn.set_focus_on_click(false);
        file_tree_up_btn.set_focusable(true);
        file_tree_up_btn.set_tooltip_text(Some("Go to parent directory"));
        file_tree_up_btn
            .update_property(&[gtk4::accessible::Property::Label("Go to parent directory")]);
        file_tree_up_btn.add_css_class("flat");

        let file_tree_header = gtk4::Box::new(Orientation::Horizontal, 2);
        file_tree_header.add_css_class("file-tree-header");
        file_tree_header.append(&file_tree_root_label);
        file_tree_header.append(&file_tree_up_btn);
        file_tree_header.append(&file_tree_cwd_btn);

        // Type-to-filter toggle; the inline entry row it opens lives between
        // the header and the tree.
        let file_tree_filter_toggle = gtk4::ToggleButton::new();
        file_tree_filter_toggle.set_icon_name("system-search-symbolic");
        file_tree_filter_toggle.set_focus_on_click(false);
        file_tree_filter_toggle.set_focusable(true);
        file_tree_filter_toggle.set_tooltip_text(Some("Filter the loaded tree by name"));
        file_tree_filter_toggle
            .update_property(&[gtk4::accessible::Property::Label("Filter files")]);
        file_tree_filter_toggle.add_css_class("flat");
        file_tree_header.append(&file_tree_filter_toggle);

        // Location selector (Local + configured ssh/docker hosts), filled by
        // UiState::refresh_file_tree_location_selector once UiState exists.
        let file_tree_location_selector = ui::build_file_tree_location_selector();
        file_tree_header.append(&file_tree_location_selector);

        // Inline type-to-filter row, hidden until toggled; Esc closes it.
        let file_tree_filter_entry = gtk4::Entry::new();
        file_tree_filter_entry.set_placeholder_text(Some("Filter loaded names…"));
        file_tree_filter_entry.set_hexpand(true);
        file_tree_filter_entry.update_property(&[gtk4::accessible::Property::Label(
            "Filter loaded files by name",
        )]);
        let file_tree_filter_bar = gtk4::Box::new(Orientation::Horizontal, 4);
        file_tree_filter_bar.add_css_class("file-tree-filter");
        file_tree_filter_bar.set_margin_start(6);
        file_tree_filter_bar.set_margin_end(6);
        file_tree_filter_bar.set_margin_bottom(2);
        file_tree_filter_bar.append(&file_tree_filter_entry);
        file_tree_filter_bar.set_visible(false);

        let file_tree_box = gtk4::Box::new(Orientation::Vertical, 0);
        file_tree_box.add_css_class("file-tree-box");
        file_tree_box.set_vexpand(true);
        file_tree_box.append(&file_tree_header);
        file_tree_box.append(&file_tree_filter_bar);
        file_tree_box.append(&file_tree_scroll);

        // Segmented switcher at the top of the sidebar: Tabs | Files.
        let sidebar_tabs_btn = gtk4::ToggleButton::with_label("Tabs");
        sidebar_tabs_btn.set_focus_on_click(false);
        sidebar_tabs_btn.set_focusable(true);
        sidebar_tabs_btn.set_hexpand(true);
        sidebar_tabs_btn.set_active(true);
        sidebar_tabs_btn.set_tooltip_text(Some("Show terminal tabs"));
        sidebar_tabs_btn
            .update_property(&[gtk4::accessible::Property::Label("Show terminal tabs")]);
        let sidebar_files_btn = gtk4::ToggleButton::with_label("Files");
        sidebar_files_btn.set_focus_on_click(false);
        sidebar_files_btn.set_focusable(true);
        sidebar_files_btn.set_hexpand(true);
        sidebar_files_btn.set_tooltip_text(Some("Browse files from the current directory"));
        sidebar_files_btn
            .update_property(&[gtk4::accessible::Property::Label("Show file browser")]);
        let sidebar_switcher = gtk4::Box::new(Orientation::Horizontal, 0);
        sidebar_switcher.add_css_class("linked");
        sidebar_switcher.add_css_class("sidebar-switcher");
        sidebar_switcher.append(&sidebar_tabs_btn);
        sidebar_switcher.append(&sidebar_files_btn);

        // Stack shows exactly one sidebar view at a time.
        let sidebar_stack = gtk4::Stack::new();
        sidebar_stack.set_vexpand(true);
        sidebar_stack.add_named(&sidebar_tabs_page, Some("tabs"));
        sidebar_stack.add_named(&file_tree_box, Some("files"));

        sidebar.append(&sidebar_switcher);
        sidebar.append(&sidebar_stack);
        // Older configs have no explicit visibility key. `load_config` derives
        // their initial state from tab placement, matching anvil: top-bar tabs
        // start with the optional file sidebar closed.
        sidebar.set_visible(config.borrow().sidebar_visible);

        // Content area: resizable sidebar | notebook (draggable divider).
        let right_col = gtk4::Box::new(Orientation::Vertical, 0);
        right_col.set_hexpand(true);
        right_col.set_vexpand(true);
        right_col.append(&notebook);
        right_col.append(&search_bar);

        // AI sidebar: wraps `right_col` in another horizontal Paned so the
        // chat panel can dock on the right edge without disturbing the
        // existing sidebar / notebook layout. Built always; visibility is
        // controlled by adding/removing it as `ai_paned`'s end_child.
        let ai_panel_widget = ui::AiPanel::build(config.clone());
        let ai_paned = gtk4::Paned::new(Orientation::Horizontal);
        ai_paned.set_vexpand(true);
        ai_paned.set_wide_handle(true);
        ai_paned.set_start_child(Some(&right_col));
        ai_paned.set_resize_start_child(true);
        ai_paned.set_resize_end_child(false);
        ai_paned.set_shrink_start_child(true);
        ai_paned.set_shrink_end_child(false);
        let ai_initially_visible = {
            let config = config.borrow();
            config.ai_enabled && config.ai_panel_visible
        };
        if ai_initially_visible {
            ai_paned.set_end_child(Some(&ai_panel_widget.root));
        }

        let content_box = gtk4::Paned::new(Orientation::Horizontal);
        content_box.set_vexpand(true);
        content_box.set_wide_handle(true);
        content_box.set_start_child(Some(&sidebar));
        content_box.set_end_child(Some(&ai_paned));
        content_box.set_resize_start_child(false);
        content_box.set_resize_end_child(true);
        content_box.set_shrink_start_child(false);
        content_box.set_shrink_end_child(true);
        content_box.set_position(config.borrow().sidebar_width as i32);

        // Main layout: draggable top bar + content box (vertical), with the
        // status bar spanning the full window width underneath both.
        let (bottom_bar, bottom_bar_left, bottom_bar_right) = ui::build_bottom_bar();
        let main_box = gtk4::Box::new(Orientation::Vertical, 0);
        main_box.append(&top_bar_handle);
        main_box.append(&content_box);
        main_box.append(&bottom_bar);

        // Shared state
        let font_scale = Rc::new(Cell::new(config.borrow().default_font_scale));
        let tab_counter = Rc::new(Cell::new(0));
        // Load once per window even when the body is currently disabled. A
        // config reload followed by a new Block pane can then opt in without
        // silently falling back to volatile state. This is a bounded read and
        // never creates the file.
        let organism_memory = match crate::organism_memory::OrganismMemory::load_default() {
            Ok(memory) => Some(memory),
            Err(error) => {
                // Fail closed: a corrupt/future/unsafe memory file must not be
                // replaced by a fresh default at the next command.
                log::error!("ASCII organism memory disabled: {error}");
                None
            }
        };
        let organism_life = Rc::new(Cell::new(
            organism_memory
                .as_ref()
                .map(crate::organism_memory::OrganismMemory::life_state)
                .unwrap_or_default(),
        ));
        let organism_circadian = organism_memory
            .as_ref()
            .and_then(|memory| memory.circadian_profile_at(crate::organism_memory::unix_ms()));
        let organism_growth = organism_memory
            .as_ref()
            .map(crate::organism_memory::OrganismMemory::growth_progress)
            .unwrap_or_default();

        // Window-level toast host: transient feedback (e.g. the opacity
        // percentage while Ctrl+Alt+=/- is held) floats over the main layout.
        let toast_overlay = adw::ToastOverlay::new();
        toast_overlay.set_child(Some(&main_box));

        let ui = Rc::new(UiState {
            window: window.clone(),
            toast_overlay: toast_overlay.clone(),
            opacity_toast: Rc::new(RefCell::new(None)),
            notebook: notebook.clone(),
            tab_counter: tab_counter.clone(),
            font_scale: font_scale.clone(),
            config_persist_generation: Rc::new(Cell::new(0)),
            font_persist_generation: Rc::new(Cell::new(0)),
            pending_font_scale: Rc::new(Cell::new(None)),
            window_opacity: window_opacity.clone(),
            shell_argv: shell_argv.clone(),
            config: config.clone(),
            available_themes: available_themes.clone(),
            search_bar: search_bar.clone(),
            search_entry: search_entry.clone(),
            search_status: search_status.clone(),
            search_debounce_source: Rc::new(RefCell::new(None)),
            search_generation: Rc::new(Cell::new(0)),
            tab_strip: tab_strip.clone(),
            sidebar: sidebar.clone(),
            tab_strip_scroll: tab_strip_scroll.clone(),
            sidebar_tab_mirror: sidebar_tab_mirror.clone(),
            sidebar_tab_mirror_scroll: sidebar_tab_mirror_scroll.clone(),
            top_tab_scroll: top_tab_scroll.clone(),
            bottom_bar: bottom_bar.clone(),
            bottom_bar_left: bottom_bar_left.clone(),
            bottom_bar_right: bottom_bar_right.clone(),
            bottom_bar_content: Rc::new(RefCell::new(Default::default())),
            tab_placement: Rc::new(Cell::new(config.borrow().tab_placement)),
            sidebar_stack: sidebar_stack.clone(),
            sidebar_tabs_btn: sidebar_tabs_btn.clone(),
            sidebar_files_btn: sidebar_files_btn.clone(),
            sidebar_view: Rc::new(Cell::new(config.borrow().sidebar_view)),
            file_tree_model: file_tree_model.clone(),
            file_tree_root: Rc::new(RefCell::new(std::path::PathBuf::new())),
            file_tree_root_label: file_tree_root_label.clone(),
            file_tree_location: file_tree_location.clone(),
            file_tree_location_selector: file_tree_location_selector.clone(),
            file_tree_clipboard: Rc::new(RefCell::new(None)),
            file_tree_filter_bar: file_tree_filter_bar.clone(),
            file_tree_filter_entry: file_tree_filter_entry.clone(),
            file_tree_filter_toggle: file_tree_filter_toggle.clone(),
            tab_search_entry: tab_search_entry.clone(),
            selected_tabs: Rc::new(RefCell::new(Vec::new())),
            tab_drag_state: Rc::new(RefCell::new(Default::default())),
            tab_focus_generation: Rc::new(Cell::new(0)),
            command_palette_dialog: Rc::new(RefCell::new(None)),
            remote_picker_dialog: Rc::new(RefCell::new(None)),
            history_palette_dialog: Rc::new(RefCell::new(None)),
            cross_block_search_dialog: Rc::new(RefCell::new(None)),
            workflows_palette_dialog: Rc::new(RefCell::new(None)),
            settings_dialog: Rc::new(RefCell::new(None)),
            debug_dashboard_dialog: Rc::new(RefCell::new(None)),
            agent_session: Rc::new(RefCell::new(None)),
            agent_ui_lifetime: Rc::new(Default::default()),
            command_suggestion: Rc::new(RefCell::new(None)),
            organism_memory: Rc::new(RefCell::new(organism_memory)),
            organism_correction: ui::OrganismCorrectionSignal::new(organism_life.clone()),
            organism_activity: ui::OrganismActivity::new(organism_circadian, organism_growth),
            organism_presence: ui::OrganismPresence::new(),
            organism_agent: ui::OrganismAgentSignal::new(organism_life.clone()),
            organism_life,
            agent_toggle: agent_toggle.clone(),
            config_save_error_visible: Rc::new(Cell::new(false)),
            safe_mode_config_notice_visible: Rc::new(Cell::new(false)),
            keybinding_map: Rc::new(RefCell::new(keybinding_map)),
            zoom_state: Rc::new(RefCell::new(None)),
            scrollbar_css: CssProvider::new(),
            session_ids: Rc::new(RefCell::new(HashMap::new())),
            tab_connections: Rc::new(RefCell::new(HashMap::new())),
            ai_panel: ai_panel_widget.clone(),
            ai_paned: ai_paned.clone(),
            ai_panel_visible: Rc::new(Cell::new(ai_initially_visible)),
            ai_panel_width_restoring: Rc::new(Cell::new(false)),
        });

        // A configuration that could not be read leaves every setting at its
        // default, which looks exactly like a config file that does nothing.
        // Say so once, with the reason, rather than only in a log nobody has
        // enabled.
        if let Some(reason) = crate::config::load_error() {
            let toast = adw::Toast::new(&format!("Configuration not loaded: {reason}"));
            toast.set_timeout(0); // dismissed by hand: it is not a status blip
            toast_overlay.add_toast(toast);
        }

        // Background persistence failures arrive from a Send-only worker.
        // Polling a bounded queue keeps GTK objects on the main thread and
        // turns otherwise invisible fsync/permission failures into one concise
        // toast per affected operation.
        let persistence_toasts = toast_overlay.downgrade();
        let ui_for_persistence = Rc::downgrade(&ui);
        let ai_panel_for_persistence = ui.ai_panel.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            let Some(overlay) = persistence_toasts.upgrade() else {
                return glib::ControlFlow::Break;
            };
            for failure in crate::persistence::drain_failures() {
                if failure.operation == ui::CONFIG_PERSIST_OPERATION {
                    if let Some(ui) = ui_for_persistence.upgrade() {
                        ui.show_config_error(
                            "Settings were not saved",
                            &format!(
                                "{}\n\nThe in-memory setting is still active. Reload the configuration (Ctrl+Shift+R) before trying again if the file changed elsewhere.",
                                failure.error
                            ),
                        );
                    }
                    continue;
                }
                let toast = adw::Toast::new(&format!("{} failed: {}", failure.operation, failure.error));
                toast.set_timeout(8);
                overlay.add_toast(toast);
            }
            // A successful asynchronous window save may have compacted the AI
            // payload. Reflect its durable truncation marker once the worker
            // publishes the generation-safe in-memory snapshot.
            ai_panel_for_persistence.sync_persisted_truncation();
            glib::ControlFlow::Continue
        });

        // Register the dynamic scrollbar CSS provider and apply initial colors
        gtk4::style_context_add_provider_for_display(
            &gtk4::gdk::Display::default().expect("display"),
            &ui.scrollbar_css,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        ui.apply_dynamic_css();
        ui.sync_agent_toggle();
        ui.sync_bottom_bar_visibility();

        // jterm prefers jsh as its shell, so it is worth noticing when the
        // machine has no jsh or an old one. The bar builds hidden and reveals
        // itself only if the background check finds something to offer.
        main_box.insert_child_after(&ui.build_jsh_notice(), Some(&top_bar_handle));

        let ui_for_ai_close = Rc::downgrade(&ui);
        ui.ai_panel.connect_close_requested(move || {
            if let Some(ui) = ui_for_ai_close.upgrade() {
                ui.toggle_ai_panel();
            }
        });

        // Persist only the settled AI divider width. Paned emits position
        // notifications continuously while dragging; the generation guard
        // coalesces them into one config write after the drag pauses.
        let ai_width_epoch = Rc::new(Cell::new(0_u64));
        let ui_for_ai_width = Rc::downgrade(&ui);
        let epoch_for_ai_width = ai_width_epoch.clone();
        ai_paned.connect_notify_local(Some("position"), move |_, _| {
            let Some(ui) = ui_for_ai_width.upgrade() else {
                return;
            };
            if ui.ai_panel_width_restoring.get() {
                return;
            }
            if !ui.capture_ai_panel_width() {
                return;
            }
            let epoch = epoch_for_ai_width.get().wrapping_add(1);
            epoch_for_ai_width.set(epoch);
            let ui_for_save = Rc::downgrade(&ui);
            let epoch_for_save = epoch_for_ai_width.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
                if epoch_for_save.get() != epoch {
                    return;
                }
                if let Some(ui) = ui_for_save.upgrade() {
                    ui.persist_config();
                }
            });
        });

        // Wire the visible command-center entry point.
        let ui_for_palette = ui.clone();
        command_palette_btn.connect_clicked(move |_| {
            ui_for_palette.execute_action(crate::keybindings::Action::ToggleCommandPalette);
        });

        // Wire toggle sidebar button
        let ui_for_toggle = ui.clone();
        toggle_sidebar_btn.connect_clicked(move |_| {
            ui_for_toggle.toggle_sidebar();
        });

        // Wire tab-placement toggle (sidebar <-> top bar)
        let ui_for_placement = ui.clone();
        toggle_placement_btn.connect_clicked(move |_| {
            ui_for_placement.toggle_tab_placement();
        });

        // Wire the visible Shell Agent state toggle.
        let ui_for_agent = ui.clone();
        agent_toggle.connect_clicked(move |_| {
            ui_for_agent.toggle_agent_panel();
        });

        // Wire file-tree header buttons
        let ui_for_ft_cwd = ui.clone();
        file_tree_cwd_btn.connect_clicked(move |_| {
            ui_for_ft_cwd.file_tree_goto_current_cwd();
        });
        let ui_for_ft_up = ui.clone();
        file_tree_up_btn.connect_clicked(move |_| {
            ui_for_ft_up.file_tree_go_up();
        });

        // Wire file-tree expansion and file activation.
        ui.connect_file_tree_handlers(&file_tree);

        // Wire the file-tree location selector and fill it for the first time.
        ui.connect_file_tree_location_selector();
        ui.refresh_file_tree_location_selector();

        // Wire the type-to-filter row and its header toggle.
        ui.connect_file_tree_filter_bar();
        let ui_for_filter_toggle = ui.clone();
        file_tree_filter_toggle.connect_clicked(move |_| {
            ui_for_filter_toggle.toggle_file_tree_filter();
        });

        // Wire sidebar Tabs/Files segmented switcher
        let ui_for_tabs_view = ui.clone();
        sidebar_tabs_btn.connect_clicked(move |_| {
            ui_for_tabs_view.apply_sidebar_view(crate::config::SidebarView::Tabs, true);
        });
        let ui_for_files_view = ui.clone();
        sidebar_files_btn.connect_clicked(move |_| {
            ui_for_files_view.apply_sidebar_view(crate::config::SidebarView::Files, true);
        });

        // Initialize the file tree and apply the persisted tab placement
        // (which also restores the persisted sidebar view).
        ui.init_file_tree();
        ui.apply_tab_placement();
        ui.install_tab_bar_pane_drop();

        // Wire "+" button — inherit working directory from current session
        let ui_for_add = ui.clone();
        add_tab_button.connect_clicked(move |_| {
            let working_directory = ui_for_add
                .current_terminal()
                .as_ref()
                .and_then(terminal_working_directory);
            let startup = ui_for_add.config.borrow().startup_commands.clone();
            ui_for_add.add_new_tab(
                working_directory,
                None,
                None,
                crate::terminal::InitialCommands::from_config(startup.as_deref()),
            );
        });

        let requested_cwd = launch
            .working_directory
            .as_ref()
            .map(|path| {
                path.to_str()
                    .expect("CLI validation rejects non-UTF-8 working directories")
                    .to_owned()
            });
        let restore_session = !launch.safe_mode
            && !launch.no_restore
            && launch.working_directory.is_none()
            && launch.execute.is_none();
        // One-shot `--execute` windows and safe mode must not overwrite the
        // user's interactive workspace. `--no-restore` still starts a new
        // persistable workspace, matching anvil.
        let session_persistence = launch.execute.is_none() && !launch.safe_mode;

        // Atomically claim one ready window snapshot only when this launch did
        // not explicitly request a fresh cwd/command/workspace.
        let (saved_current, saved_tabs) = if restore_session {
            load_tabs_state()
        } else {
            crate::state::set_ai_conversation_snapshot(None);
            (None, Vec::new())
        };
        if restore_session {
            ui.ai_panel.restore_persisted_conversation();
        }
        if saved_tabs.is_empty() {
            if let Some(argv) = launch.execute.clone() {
                ui.add_new_tab_with_argv(requested_cwd, argv);
            } else {
                let startup = ui.config.borrow().startup_commands.clone();
                ui.add_new_tab(
                    requested_cwd,
                    None,
                    None,
                    crate::terminal::InitialCommands::from_config(startup.as_deref()),
                );
            }
        } else {
            for (name, layout) in saved_tabs {
                ui.restore_pane_layout(layout, name);
            }

            if let Some(page) = saved_current {
                let n_pages = notebook.n_pages();
                if n_pages > 0 {
                    notebook.set_current_page(Some(page.min(n_pages.saturating_sub(1))));
                }
            }
        }

        if session_persistence {
            // A snapshot may have been written while redaction was disabled
            // and restored after the user enabled it in config. Upgrade the
            // in-memory copy before the unconditional initial state save.
            ui.ai_panel.refresh_persisted_privacy();

            let notebook_for_ai = notebook.clone();
            let session_ids_for_ai = ui.session_ids.clone();
            ui.ai_panel.set_persistence_callback(move || {
                save_tabs_state(&notebook_for_ai, &session_ids_for_ai.borrow());
            });

            // Auto-save tabs state when tabs are added or removed. Defer until
            // the page's typed pane controller is fully attached.
            let session_ids_for_page_added = ui.session_ids.clone();
            let notebook_clone_for_added = notebook.clone();
            let ai_panel_for_page_added = ui.ai_panel.clone();
            notebook.connect_page_added(move |_notebook, _child, _page_num| {
                let nb = notebook_clone_for_added.clone();
                let sids = session_ids_for_page_added.clone();
                let ai_panel = ai_panel_for_page_added.clone();
                glib::idle_add_local_once(move || {
                    save_tabs_state(&nb, &sids.borrow());
                    ai_panel.sync_persisted_truncation();
                });
            });

            let session_ids_for_page_removed = ui.session_ids.clone();
            let notebook_clone_for_removed = notebook.clone();
            let ai_panel_for_page_removed = ui.ai_panel.clone();
            notebook.connect_page_removed(move |_notebook, _child, _page_num| {
                let nb = notebook_clone_for_removed.clone();
                let sids = session_ids_for_page_removed.clone();
                let ai_panel = ai_panel_for_page_removed.clone();
                glib::idle_add_local_once(move || {
                    save_tabs_state(&nb, &sids.borrow());
                    ai_panel.sync_persisted_truncation();
                });
            });

            // Drag/drop and keyboard tab moves are state changes too. Persist
            // their final Notebook order instead of waiting for window close.
            let session_ids_for_page_reordered = ui.session_ids.clone();
            let notebook_clone_for_reordered = notebook.clone();
            let ai_panel_for_page_reordered = ui.ai_panel.clone();
            notebook.connect_page_reordered(move |_notebook, _child, _page_num| {
                let nb = notebook_clone_for_reordered.clone();
                let sids = session_ids_for_page_reordered.clone();
                let ai_panel = ai_panel_for_page_reordered.clone();
                glib::idle_add_local_once(move || {
                    save_tabs_state(&nb, &sids.borrow());
                    ai_panel.sync_persisted_truncation();
                });
            });

            // Save initial state after tabs are restored.
            save_tabs_state(&notebook, &ui.session_ids.borrow());
            ui.ai_panel.sync_persisted_truncation();
        }

        // Setup key controller on window level with Capture phase
        // This allows us to intercept shortcuts before the terminal processes them
        let key_controller = EventControllerKey::new();
        key_controller.set_propagation_phase(gtk4::PropagationPhase::Capture);

        let ui_clone = ui.clone();
        let pending_focus_shortcut: Rc<RefCell<Option<DeferredFocusShortcut>>> =
            Rc::new(RefCell::new(None));
        let pending_for_press = pending_focus_shortcut.clone();

        key_controller.connect_key_pressed(move |_controller, keyval, keycode, state| {
            // Composer Enter semantics must win over optional user-defined
            // global bindings. The focused TextView controller also gives IME
            // candidate confirmation first refusal.
            if ui_clone.ai_panel.handles_enter_key()
                && matches!(keyval, Key::Return | Key::KP_Enter)
            {
                return false.into();
            }
            // The gdk edge folds the event into the shared chord form
            // (masked modifiers, ISO_Left_Tab → Tab, lowercased unicode).
            let Some(chord) = chord_from_gdk(keyval, state) else {
                return false.into();
            };

            let action = {
                let bindings = ui_clone.keybinding_map.borrow();
                bindings.lookup(&chord).or_else(|| {
                    // Alt modifies Copy into "copy block output". The binding
                    // map is intentionally exact, so retry only this one
                    // documented variant without Alt instead of making every
                    // shortcut accidentally accept extra modifiers.
                    if chord.mods.ctrl && chord.mods.shift && chord.mods.alt {
                        let copy_chord = Chord {
                            mods: Mods {
                                alt: false,
                                ..chord.mods
                            },
                            key: chord.key,
                        };
                        (bindings.lookup(&copy_chord) == Some(Action::Copy))
                            .then_some(Action::Copy)
                    } else {
                        None
                    }
                })
            };

            if let Some(action) = action {
                log::debug!("window shortcut matched: {action:?} ({chord:?})");
                match action {
                    Action::NewTab => {
                        // Keep the old VTE/IME context focused until it has seen
                        // T's matching release. Modifier release signals are
                        // intentionally not part of the completion condition.
                        let mut pending = pending_for_press.borrow_mut();
                        if pending.is_none() {
                            *pending = Some(DeferredFocusShortcut::new(action, keycode));
                        }
                        return true.into();
                    }
                    Action::Copy => {
                        // Handle at the window level so the shortcut works no
                        // matter which child has focus — in particular, after
                        // mouse-selecting text inside a finished block,
                        // focus lives on that TextView and the per-VTE
                        // block-mode handler never fires.
                        // A focused AI composer/transcript selection takes
                        // priority over terminal/block selections.
                        if ui_clone.ai_panel.copy_focused_selection() {
                            return true.into();
                        }
                        // copy_to_clipboard handles Warp block-selection,
                        // VTE selection, TextBuffer selection, and PRIMARY
                        // fallback in priority order. Pass Alt for the
                        // CopyBlockOutput variant.
                        if let Some(term_view) = ui_clone.current_term_view() {
                            let alt_held = state.contains(ModifierType::ALT_MASK);
                            term_view.copy_to_clipboard_with_modifier(alt_held);
                            return true.into();
                        }
                        ui_clone.execute_action(action);
                        return true.into();
                    }
                    Action::Paste => {
                        if ui_clone.ai_panel.paste_into_composer_if_focused() {
                            return true.into();
                        }
                        ui_clone.execute_action(action);
                        return true.into();
                    }
                    _ => {
                        ui_clone.execute_action(action);
                        return true.into();
                    }
                }
            }

            false.into()
        });

        let pending_for_release = pending_focus_shortcut.clone();
        let ui_for_release = ui.clone();
        key_controller.connect_key_released(move |_, _, keycode, _| {
            if let Some(pending) = pending_for_release.borrow_mut().as_mut() {
                pending.key_released(keycode);
            }
            if let Some(action) = take_ready_shortcut(&pending_for_release) {
                let ui = ui_for_release.clone();
                // Let the old widget and its IM context finish dispatching the
                // release before the action maps/focuses a new VTE.
                glib::idle_add_local_once(move || ui.execute_action(action));
            }
        });

        let pending_for_deactivate = pending_focus_shortcut.clone();
        let ui_for_window_presence = ui.clone();
        window.connect_is_active_notify(move |window| {
            if !window.is_active() {
                pending_for_deactivate.borrow_mut().take();
            }
            ui_for_window_presence.sync_organism_presence();
        });
        let ui_for_focus_presence = ui.clone();
        window.connect_focus_widget_notify(move |_| {
            ui_for_focus_presence.sync_organism_presence();
        });

        // Enter/Shift+Enter are handled by the capture-phase key controller
        // below (next/prev); incremental highlighting runs on search_changed.
        let ui_for_search_changed = ui.clone();
        search_entry.connect_search_changed(move |_| {
            ui_for_search_changed.schedule_search_apply();
        });

        let ui_for_search_next = ui.clone();
        search_next_btn.connect_clicked(move |_| {
            ui_for_search_next.search_next();
        });

        let ui_for_search_prev = ui.clone();
        search_prev_btn.connect_clicked(move |_| {
            ui_for_search_prev.search_prev();
        });

        let ui_for_search_close = ui.clone();
        search_close_btn.connect_clicked(move |_| {
            ui_for_search_close.toggle_search();
        });

        // Search entry key handler for Enter (next), Shift+Enter (prev), Escape.
        // Capture phase so Enter is consumed before it can reach the live VTE
        // (otherwise it submits a stray empty command to the shell).
        let search_key_controller = EventControllerKey::new();
        search_key_controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let ui_for_search_key = ui.clone();
        search_key_controller.connect_key_pressed(move |_, keyval, _, state| {
            match keyval {
                Key::Return | Key::KP_Enter => {
                    if state.contains(ModifierType::SHIFT_MASK) {
                        ui_for_search_key.search_prev();
                    } else {
                        ui_for_search_key.search_next();
                    }
                    return true.into();
                }
                Key::Escape => {
                    ui_for_search_key.toggle_search();
                    return true.into();
                }
                _ => {}
            }
            false.into()
        });
        search_entry.add_controller(search_key_controller);

        // Wire tab search entry: filter tabs by name
        let ui_for_tab_search = ui.clone();
        tab_search_entry.connect_search_changed(move |entry| {
            ui_for_tab_search.apply_tab_filter(entry.text().as_str());
        });

        // Focus terminal when switching tabs (split-aware) and sync tab strip
        let ui_for_switch = ui.clone();
        notebook.connect_switch_page(move |_, widget, page_num| {
            // `current_page` still names the old tab inside this signal.
            // Revoke its spatial body synchronously; the idle below resolves
            // and claims the newly selected page after GTK commits the switch.
            ui_for_switch.revoke_organism_presence();
            // Headers are only maintained for the visible tab, so the newly
            // selected one has to catch up before it is drawn.
            ui_for_switch.refresh_pane_headers_for(widget);
            if ui_for_switch.search_bar.is_search_mode() {
                ui_for_switch.invalidate_tab_focus_requests();
                ui_for_switch.search_apply();
                ui_for_switch.search_entry.grab_focus();
            } else if let Some(target_terminal) = ui_for_switch.terminal_in_page(widget) {
                if let Some(term_view) = ui_for_switch.term_view_in_page(widget) {
                    term_view.reveal_live_input();
                }

                // `switch-page` can run before the selected child has completed
                // its map/focus reconciliation. The shared request generation
                // also lets topology DnD replace this target if it moves another
                // live pane into the still-selected page before the next frame.
                ui_for_switch.request_tab_terminal_focus(target_terminal, page_num);
            } else {
                ui_for_switch.invalidate_tab_focus_requests();
            }
            // Clear activity/bell indicators for the tab being switched to
            let tab_name = widget.widget_name();
            ui_for_switch.clear_tab_indicators(tab_name.as_str());
            ui_for_switch.sync_tab_strip_active(Some(page_num));
            // File tree root and bottom bar follow the active tab, which the
            // notebook has not switched to yet while this signal runs.
            let ui_ft = ui_for_switch.clone();
            glib::idle_add_local_once(move || {
                ui_ft.file_tree_goto_current_cwd();
                ui_ft.refresh_bottom_bar();
                ui_ft.sync_organism_presence();
            });
        });

        window.add_controller(key_controller);

        // Ctrl+scroll to zoom font
        let scroll_controller = EventControllerScroll::new(EventControllerScrollFlags::VERTICAL);
        scroll_controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let ui_for_scroll = ui.clone();
        scroll_controller.connect_scroll(move |controller, _dx, dy| {
            let state = controller.current_event_state();
            // Touchpads emit fractional deltas; a zero step would still claim
            // the event, so let those through untouched.
            if state.contains(ModifierType::CONTROL_MASK) && dy != 0.0 {
                let font_step = 0.025;
                let current = ui_for_scroll.font_scale.get();
                let new_scale = if dy < 0.0 {
                    (current + font_step).min(10.0)
                } else {
                    (current - font_step).max(0.1)
                };
                ui_for_scroll.apply_font_scale(new_scale);
                return true.into();
            }
            false.into()
        });
        window.add_controller(scroll_controller);

        // Save state *before* GTK starts destroying widgets.
        let notebook_for_close_request = notebook.clone();
        let session_ids_for_close = ui.session_ids.clone();
        let ui_for_close_request = ui.clone();
        let app_for_close = app.clone();
        let config_for_close = ui.config.clone();
        let sidebar_for_close = sidebar.clone();
        let paned_for_close = content_box.clone();
        let ai_paned_for_close = ai_paned.clone();
        let ai_panel_visible_for_close = ui.ai_panel_visible.clone();
        let ai_panel_width_restoring_for_close = ui.ai_panel_width_restoring.clone();
        let ai_panel_for_close = ui.ai_panel.clone();
        let agent_session_for_close = ui.agent_session.clone();
        let command_suggestion_for_close = ui.command_suggestion.clone();
        let zoom_for_close = ui.zoom_state.clone();
        let close_allowed = Rc::new(Cell::new(false));
        let close_confirmation_open = Rc::new(Cell::new(false));
        window.connect_close_request(move |window| {
            if !close_allowed.get() {
                // A zoomed split keeps the sibling tree outside the Notebook.
                // Restore it before scanning/saving so no hidden pane escapes
                // confirmation or teardown. Cancellation simply leaves the tab
                // safely unzoomed.
                if let Some(state) = zoom_for_close.borrow_mut().take() {
                    let _ = ui::restore_zoomed_leaf(&notebook_for_close_request, &state.swap);
                }
                if close_confirmation_open.get() {
                    return true.into();
                }
                if let Some(processes) =
                    UiState::running_process_summary_for_notebook(&notebook_for_close_request)
                {
                    close_confirmation_open.set(true);
                    let window_for_confirmation = window.clone();
                    let allowed_for_confirmation = close_allowed.clone();
                    let open_for_confirmation = close_confirmation_open.clone();
                    glib::MainContext::default().spawn_local(async move {
                        let confirmed = UiState::confirm_close_with_processes(
                            &window_for_confirmation,
                            "Close window with running processes?",
                            "Close Window",
                            &processes,
                        )
                        .await;
                        open_for_confirmation.set(false);
                        if confirmed {
                            allowed_for_confirmation.set(true);
                            window_for_confirmation.close();
                        }
                    });
                    return true.into();
                }
            }

            // Persist the current sidebar geometry/state before teardown.
            let width = paned_for_close.position().max(120) as u32;
            {
                let mut config = config_for_close.borrow_mut();
                config.sidebar_width = width;
                config.sidebar_visible = sidebar_for_close.is_visible();
                if ai_panel_visible_for_close.get() && !ai_panel_width_restoring_for_close.get() {
                    let total_width = ai_paned_for_close.width();
                    let position = ai_paned_for_close.position();
                    if total_width > 0 && position >= 0 && position < total_width {
                        config.ai_panel_width = (total_width - position).clamp(240, 1200) as u32;
                    }
                }
            }
            ui_for_close_request.flush_pending_config();

            // Stop and reap any in-flight provider processes before widgets
            // and their request-routing state are destroyed.
            if let Some(suggestion) = command_suggestion_for_close.borrow_mut().take() {
                suggestion.shutdown();
            }
            if let Some(agent) = agent_session_for_close.borrow_mut().take() {
                // Snapshot before shutdown cancels the session; a cancelled
                // session refuses to snapshot by design.
                if session_persistence {
                    agent.persist();
                }
                agent.shutdown();
            }
            ai_panel_for_close.cancel_all_requests();
            if session_persistence {
                // Do not let the composer's draft debounce outlive the final
                // window snapshot. This also persists an in-flight question as
                // a recoverable draft rather than a fake completed turn.
                ai_panel_for_close.flush_persisted_conversation();
                save_tabs_state(&notebook_for_close_request, &session_ids_for_close.borrow());
            }
            save_all_block_histories(&notebook_for_close_request);
            kill_all_terminal_children(&notebook_for_close_request);

            // Permanently removing a pane must steal its typed controller from
            // root qdata; otherwise the controller/root ownership cycle keeps
            // TermView alive and its Drop save never runs.
            detach_all_pane_leaves(&notebook_for_close_request);

            // Explicitly clear all pages to break reference cycles and allow TermView cleanup.
            // This ensures OwnedPty drops, closing PTY master FD and signaling reader threads.
            while notebook_for_close_request.n_pages() > 0 {
                notebook_for_close_request.remove_page(Some(0));
            }
            if let Err(error) =
                jterm_core::command_history::flush_pending(std::time::Duration::from_secs(2))
            {
                log::warn!("command-history worker did not flush before shutdown: {error}");
            }
            // Captured outputs queued for jsh's execution journal ride a
            // writer thread of their own; without this bounded wait the last
            // command's output is lost whenever quit wins the race to disk.
            if !jterm_core::execution_journal::flush(std::time::Duration::from_secs(2)) {
                log::warn!("execution-journal writer did not flush before shutdown");
            }
            // Publish after every window save and Block-history snapshot, then
            // stop accepting work and wait for the single disk worker to drain.
            // The publication itself is queued, so rename/fsync also stays off
            // the GTK main thread.
            if session_persistence {
                finalize_tabs_state();
            }
            if let Err(error) =
                crate::organism_memory::flush_pending(std::time::Duration::from_millis(500))
            {
                log::warn!("ASCII organism memory could not be queued for shutdown: {error}");
            }
            if let Err(error) =
                crate::persistence::shutdown(std::time::Duration::from_secs(3))
            {
                // Leave `.active` recoverable rather than racing a late write
                // with an on-thread rename. The next launch reclaims it once
                // this process is definitely gone.
                log::warn!("persistence worker did not flush before shutdown: {error}");
            }
            for failure in crate::persistence::drain_failures() {
                log::error!("persistence failure during shutdown: {failure}");
            }

            // Directly quit the application
            app_for_close.quit();

            false.into()
        });

        let app_clone = app.clone();
        window.connect_destroy(move |_| {
            app_clone.quit();
        });

        window.set_content(Some(&toast_overlay));
        window.present();

        // Paned positions are meaningful only after the first allocation.
        ui.restore_ai_panel_width();

        // Directories and foreground commands are polled, not pushed, so the
        // split panes' headers need a slow tick to stay honest. It touches
        // only the visible tab, and only while that tab is actually split.
        {
            let ui = Rc::clone(&ui);
            let mut refresh_processes = false;
            glib::timeout_add_seconds_local(1, move || {
                ui.refresh_pane_headers();
                // Process badges historically refreshed every two seconds.
                // Keep that cadence while sharing one window timer, so moving
                // the work out of per-tab sources does not double /proc reads.
                refresh_processes = !refresh_processes;
                if !refresh_processes {
                    ui.refresh_tab_process_indicators();
                }
                // Same poll keeps the bottom bar's running/cwd/grid segments
                // honest for the focused pane.
                ui.refresh_bottom_bar();
                ui.sync_organism_presence();
                glib::ControlFlow::Continue
            });
        }

        // Focus the active terminal after window is shown
        ui.focus_current_terminal();
        ui.sync_organism_presence();

        // Safe mode deliberately ignores later config-file changes.
        if !launch.safe_mode {
            let config_path = config_file_path();
            if let Some(parent_dir) = config_path.parent() {
                let _ = fs::create_dir_all(parent_dir);
            }
            let config_file = gio::File::for_path(&config_path);
            match config_file.monitor_file(gio::FileMonitorFlags::NONE, None::<&Cancellable>) {
            Ok(monitor) => {
                let ui_for_reload = ui.clone();
                // Debounce: editors may write multiple events in rapid succession.
                let reload_pending: Rc<Cell<bool>> = Rc::new(Cell::new(false));
                monitor.connect_changed(move |_, _, _, event| {
                    if matches!(event, gio::FileMonitorEvent::Changed | gio::FileMonitorEvent::Created)
                        && !reload_pending.get() {
                            reload_pending.set(true);
                            let ui_reload = ui_for_reload.clone();
                            let pending = reload_pending.clone();
                            glib::timeout_add_local_once(
                                std::time::Duration::from_millis(200),
                                move || {
                                    pending.set(false);
                                    ui_reload.reload_config();
                                },
                            );
                        }
                });
                // Keep monitor alive by storing it on the window
                unsafe { window.set_data("config-monitor", monitor); }
            }
                Err(err) => {
                    log::warn!("Failed to watch config file: {err}");
                }
            }
        }
    });

    // All user-facing options were parsed and consumed by `handle_early_args`
    // before GTK initialisation.  Passing the process argv to GApplication a
    // second time makes it reject forge-specific launch options such as
    // `--no-restore`, `--safe-mode`, `--mode`, `-d`, and `-e`.  Give GTK only a
    // stable program name; the validated values are already captured above in
    // `launch_options`.
    app.run_with_args(&GTK_APPLICATION_ARGV)
}

#[cfg(test)]
mod tests {
    use crate::keybindings::Action;

    mod gdk_chord_edge {
        //! The gdk → [`Chord`] translation is pure data (no GTK runtime),
        //! so its GTK-only facts are pinned here: ISO_Left_Tab folding,
        //! keypad folding, Super/Meta mapping, unicode lowercasing, and F-key naming.
        use super::super::chord_from_gdk;
        use gtk4::gdk::{Key, ModifierType};
        use jterm_core::keybindings::parse;

        const CTRL: ModifierType = ModifierType::CONTROL_MASK;
        const CTRL_SHIFT: ModifierType = ModifierType::CONTROL_MASK.union(ModifierType::SHIFT_MASK);

        #[test]
        fn events_translate_to_the_same_chords_config_strings_parse_to() {
            let cases: &[(Key, ModifierType, &str)] = &[
                // Shifted letters arrive uppercase from GTK.
                (Key::T, CTRL_SHIFT, "Ctrl+Shift+T"),
                (Key::t, CTRL, "Ctrl+T"),
                // Shift+Tab arrives as ISO_Left_Tab and folds to Tab.
                (Key::ISO_Left_Tab, CTRL_SHIFT, "Ctrl+Shift+Tab"),
                (Key::Tab, CTRL, "Ctrl+Tab"),
                // Shifted digit rows deliver the symbol keysym.
                (Key::exclam, CTRL_SHIFT, "Ctrl+Shift+!"),
                (Key::equal, CTRL, "Ctrl+="),
                (Key::backslash, CTRL, "Ctrl+backslash"),
                // Main-row digits.
                (Key::_1, CTRL, "Ctrl+1"),
                (Key::_0, CTRL, "Ctrl+0"),
                // Numeric keypad digits share the family's main-row chords.
                (Key::KP_1, CTRL, "Ctrl+1"),
                (Key::KP_0, CTRL, "Ctrl+0"),
                // GDK exposes platform Super/Meta masks separately; both map
                // to the grammar's portable Super modifier.
                (Key::t, ModifierType::SUPER_MASK, "Super+T"),
                (Key::t, ModifierType::META_MASK, "Super+T"),
                // Named keys and function keys.
                (Key::Page_Up, CTRL, "Ctrl+PageUp"),
                (Key::Return, CTRL, "Ctrl+Enter"),
                (Key::F12, ModifierType::empty(), "F12"),
            ];
            for (keyval, state, chord_str) in cases {
                let want = parse(chord_str).expect("test chord parses");
                assert_eq!(
                    chord_from_gdk(*keyval, *state),
                    Some(want),
                    "{keyval:?} + {state:?} must translate to {chord_str}"
                );
            }
        }

        #[test]
        fn irrelevant_modifier_bits_are_masked_out() {
            let noisy = CTRL_SHIFT | ModifierType::LOCK_MASK | ModifierType::BUTTON1_MASK;
            assert_eq!(
                chord_from_gdk(Key::T, noisy),
                Some(parse("Ctrl+Shift+T").unwrap())
            );
        }

        #[test]
        fn keypad_digits_fold_but_keypad_control_and_operator_keys_remain_distinct() {
            assert_eq!(
                chord_from_gdk(Key::KP_1, CTRL),
                Some(parse("Ctrl+1").unwrap())
            );
            assert_eq!(chord_from_gdk(Key::KP_Enter, CTRL), None);
            assert_eq!(chord_from_gdk(Key::KP_Add, CTRL), None);
        }

        #[test]
        fn modifier_only_and_unmappable_keysyms_produce_no_chord() {
            assert_eq!(chord_from_gdk(Key::Control_L, CTRL), None);
            assert_eq!(chord_from_gdk(Key::Shift_L, CTRL_SHIFT), None);
            // F25 exists as a keysym but is outside the family's F1..F24.
            assert_eq!(chord_from_gdk(Key::F25, ModifierType::empty()), None);
        }
    }

    #[test]
    fn gtk_receives_only_the_sanitized_program_name() {
        // Keep this assertion beside the GApplication boundary.  Launch
        // options belong exclusively to cli::handle_early_args and must never
        // be forwarded for a second parse.
        assert_eq!(super::GTK_APPLICATION_ARGV, ["forge"]);
        assert_eq!(super::GTK_APPLICATION_ARGV.len(), 1);
    }

    #[test]
    fn new_tab_runs_after_trigger_release_even_with_modifiers_still_held() {
        let pending =
            std::cell::RefCell::new(Some(super::DeferredFocusShortcut::new(Action::NewTab, 28)));

        pending.borrow_mut().as_mut().unwrap().key_released(28);
        assert_eq!(super::take_ready_shortcut(&pending), Some(Action::NewTab));
        assert!(pending.borrow().is_none());
        assert_eq!(super::take_ready_shortcut(&pending), None);
    }

    #[test]
    fn new_tab_ignores_unrelated_key_releases() {
        let pending =
            std::cell::RefCell::new(Some(super::DeferredFocusShortcut::new(Action::NewTab, 28)));

        pending.borrow_mut().as_mut().unwrap().key_released(37);
        assert_eq!(super::take_ready_shortcut(&pending), None);

        pending.borrow_mut().as_mut().unwrap().key_released(28);
        assert_eq!(super::take_ready_shortcut(&pending), Some(Action::NewTab));
    }

    #[test]
    fn tab_focus_waits_for_mapping_before_grabbing() {
        let frame = super::next_tab_focus_frame(false, false, 1);
        assert_eq!(
            frame,
            super::TabFocusFrame {
                stable_frames: 0,
                should_grab: false,
                complete: false,
            }
        );
    }

    #[test]
    fn unmapped_logical_focus_does_not_complete_ime_focus_request() {
        let frame = super::next_tab_focus_frame(false, true, 1);
        assert_eq!(
            frame,
            super::TabFocusFrame {
                stable_frames: 0,
                should_grab: false,
                complete: false,
            }
        );
    }

    #[test]
    fn mapped_tab_retries_focus_then_requires_two_stable_frames() {
        let retry = super::next_tab_focus_frame(true, false, 0);
        assert!(retry.should_grab);
        assert!(!retry.complete);

        let first = super::next_tab_focus_frame(true, true, retry.stable_frames);
        assert_eq!(first.stable_frames, 1);
        assert!(!first.complete);

        let second = super::next_tab_focus_frame(true, true, first.stable_frames);
        assert_eq!(second.stable_frames, 2);
        assert!(second.complete);
    }

    #[test]
    fn losing_focus_resets_stability_and_requests_another_grab() {
        let frame = super::next_tab_focus_frame(true, false, 1);
        assert_eq!(
            frame,
            super::TabFocusFrame {
                stable_frames: 0,
                should_grab: true,
                complete: false,
            }
        );
    }

    #[test]
    fn stale_tab_focus_request_cannot_steal_focus_back() {
        assert!(super::tab_focus_request_is_current(9, 9, Some(2), 2));
        assert!(!super::tab_focus_request_is_current(10, 9, Some(2), 2));
        assert!(!super::tab_focus_request_is_current(9, 9, Some(1), 2));
    }

    #[test]
    fn topology_focus_request_supersedes_hover_target_on_the_same_page() {
        let hover_generation = 41;
        let moved_pane_generation = hover_generation + 1;
        let selected_page = Some(3);

        assert!(!super::tab_focus_request_is_current(
            moved_pane_generation,
            hover_generation,
            selected_page,
            3,
        ));
        assert!(super::tab_focus_request_is_current(
            moved_pane_generation,
            moved_pane_generation,
            selected_page,
            3,
        ));
    }
}
