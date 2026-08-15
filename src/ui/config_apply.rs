//! config_apply — UiState methods extracted from ui (mechanical split, no logic changes)
use adw::prelude::*;
use gtk4::gdk::RGBA;
use gtk4::glib;
use gtk4::pango::FontDescription;
use libadwaita as adw;
use vte4::Terminal;
use vte4::{TerminalExt, TerminalExtManual};

use super::*;
use crate::config::{
    choose_shell_argv, config_file_path, load_config, validate_config_contents, Theme,
};
use crate::terminal::collect_terminals;

fn live_config_revision(
    config: &crate::config::Config,
) -> Option<crate::config_store::ConfigRevision> {
    config
        .persistence_revision
        .lock()
        .map(|revision| revision.clone())
        .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
}

fn reload_matches_live_revision(
    live_revision: Option<&crate::config_store::ConfigRevision>,
    disk_revision: &crate::config_store::ConfigRevision,
) -> bool {
    live_revision == Some(disk_revision)
}

impl UiState {
    pub(crate) fn show_config_error(&self, title: &str, message: &str) {
        if self.config_save_error_visible.replace(true) {
            return;
        }
        let dialog = adw::AlertDialog::new(Some(title), Some(message));
        dialog.add_response("ok", "OK");
        dialog.set_default_response(Some("ok"));
        dialog.set_close_response("ok");
        let visible = self.config_save_error_visible.clone();
        dialog.connect_response(None, move |_, _| visible.set(false));
        dialog.present(Some(&self.window));
    }

    /// Persist a UI-originated configuration change and make conflicts,
    /// validation refusal, lock timeouts and I/O failures visible to the user.
    pub(crate) fn persist_config(&self) {
        self.schedule_config_persist(true);
    }

    pub(crate) fn flush_pending_config(&self) {
        let generation = self.config_persist_generation.get().wrapping_add(1);
        self.config_persist_generation.set(generation);
        self.persist_config_now(false, true);
    }

    fn schedule_config_persist(&self, show_safe_mode_notice: bool) {
        if std::env::var_os("FORGE_SAFE_MODE").is_some() {
            if show_safe_mode_notice {
                self.show_config_error(
                    "Temporary safe-mode setting",
                    "This change applies only to the current window and will not be saved.",
                );
            }
            return;
        }
        let generation = self.config_persist_generation.get().wrapping_add(1);
        self.config_persist_generation.set(generation);
        let ui = self.clone();
        glib::timeout_add_local_once(CONFIG_PERSIST_DEBOUNCE, move || {
            if ui.config_persist_generation.get() == generation {
                ui.persist_config_now(false, false);
            }
        });
    }

    fn persist_config_now(&self, show_safe_mode_notice: bool, allow_sync_fallback: bool) {
        if std::env::var_os("FORGE_SAFE_MODE").is_some() {
            if show_safe_mode_notice {
                self.show_config_error(
                    "Temporary safe-mode setting",
                    "This change applies only to the current window and will not be saved.",
                );
            }
            return;
        }
        let snapshot = self.config.borrow().clone();
        let path = config_file_path();
        let key = crate::persistence::PersistenceKey::for_path("config", &path);
        if let Err(error) = crate::persistence::enqueue(key, CONFIG_PERSIST_OPERATION, move || {
            crate::config_store::save_config(&snapshot)
                .map(|_| ())
                .map_err(|error| std::io::Error::other(error.to_string()))
        }) {
            if allow_sync_fallback {
                if let Err(sync_error) = crate::config::save_config(&self.config.borrow()) {
                    self.show_config_error(
                        "Settings were not saved",
                        &format!(
                            "{sync_error}\n\nThe in-memory setting is still active. Reload the configuration (Ctrl+Shift+R) before trying again if the file changed elsewhere."
                        ),
                    );
                }
                return;
            }
            self.show_config_error(
                "Settings were not saved",
                &format!(
                    "{error}\n\nThe in-memory setting is still active. Reload the configuration (Ctrl+Shift+R) before trying again if the file changed elsewhere."
                ),
            );
        }
    }

    /// Push the current behavioral configuration into every live Block pane,
    /// including panes nested under splits.
    pub(crate) fn sync_block_configs(&self) {
        // Keep the UiState cell unborrowed while pane callbacks apply the
        // snapshot. The cells are separate today, but holding this `Ref`
        // across `reload_config` would become re-entrant if their ownership is
        // ever unified.
        let config = self.config.borrow().clone();
        for page in 0..self.notebook.n_pages() {
            let Some(widget) = self.notebook.nth_page(Some(page)) else {
                continue;
            };
            let Some(node) = PaneNode::from_widget(&widget) else {
                continue;
            };
            for leaf in node.leaves() {
                if let Some(view) = leaf.block_view() {
                    view.reload_config(&config);
                }
            }
        }
    }

    /// Apply a font scale to the live panes, the in-memory config, and — once
    /// the steps stop arriving — the config file. Used by the hotkeys, the
    /// Ctrl+wheel path, and the settings dialog; all three emit bursts (a held
    /// key, a wheel notch train, a dragged SpinRow) that must not rewrite the
    /// config file once per step.
    pub(crate) fn apply_font_scale(&self, new_scale: f64) {
        self.set_font_scale_all(new_scale);
        self.config.borrow_mut().default_font_scale = new_scale;
        let generation = self.font_persist_generation.get().wrapping_add(1);
        self.font_persist_generation.set(generation);
        let ui = self.clone();
        glib::timeout_add_local_once(FONT_PERSIST_DEBOUNCE, move || {
            // A newer step superseded this one; it owns the write instead.
            if ui.font_persist_generation.get() == generation {
                ui.persist_config();
            }
        });
    }

    pub(crate) fn set_font_scale_all(&self, new_scale: f64) {
        self.font_scale.set(new_scale);
        for i in 0..self.notebook.n_pages() {
            if let Some(widget) = self.notebook.nth_page(Some(i)) {
                let Some(node) = PaneNode::from_widget(&widget) else {
                    continue;
                };
                for leaf in node.leaves() {
                    if let Some(view) = leaf.block_view() {
                        // Updates the live surface, every finished renderer,
                        // and the TermView config used by future blocks.
                        view.set_font_scale(new_scale);
                    } else {
                        leaf.terminal().set_font_scale(new_scale);
                    }
                }
            }
        }
    }

    pub(crate) fn for_each_terminal(&self, f: impl Fn(&Terminal)) {
        for i in 0..self.notebook.n_pages() {
            if let Some(widget) = self.notebook.nth_page(Some(i)) {
                let mut terms = Vec::new();
                collect_terminals(&widget, &mut terms);
                for term in terms {
                    f(&term);
                }
            }
        }
    }

    pub(crate) fn apply_colors_all(&self) {
        let config = self.config.borrow();
        let palette_refs: Vec<&RGBA> = config.palette.iter().collect();
        self.for_each_terminal(|term| {
            term.set_colors(
                Some(&config.foreground),
                Some(&config.background),
                &palette_refs,
            );
            term.set_color_bold(None);
            term.set_color_cursor(Some(&config.cursor));
            term.set_color_cursor_foreground(Some(&config.cursor_foreground));
        });
        drop(config);
        self.apply_dynamic_css();
    }

    pub(crate) fn apply_dynamic_css(&self) {
        self.install_command_correction_monitor();
        let config = self.config.borrow();
        let bg = &config.background;
        // Some terminal palettes intentionally choose a softer foreground
        // than WCAG permits for application chrome. Keep the terminal color
        // untouched, but make labels, buttons, and status text readable.
        let semantic = config.semantic_colors();
        let fg = &semantic.foreground;
        let br = (bg.red() * 255.0) as u8;
        let bg_g = (bg.green() * 255.0) as u8;
        let bb = (bg.blue() * 255.0) as u8;
        let fr = (fg.red() * 255.0) as u8;
        let fg_g = (fg.green() * 255.0) as u8;
        let fb = (fg.blue() * 255.0) as u8;
        let muted = &semantic.muted;
        let (mut_r, mut_g, mut_b) = (
            (muted.red() * 255.0) as u8,
            (muted.green() * 255.0) as u8,
            (muted.blue() * 255.0) as u8,
        );
        // Terminal ANSI colors are allowed to be low contrast by design, but
        // Forge chrome is ordinary UI text. Keep each theme's hue while
        // adjusting it toward the foreground when necessary.
        let ok = &semantic.success;
        let (ok_r, ok_g, ok_b) = (
            (ok.red() * 255.0) as u8,
            (ok.green() * 255.0) as u8,
            (ok.blue() * 255.0) as u8,
        );
        let err = &semantic.error;
        let (err_r, err_g, err_b) = (
            (err.red() * 255.0) as u8,
            (err.green() * 255.0) as u8,
            (err.blue() * 255.0) as u8,
        );
        let warning = &semantic.warning;
        let (warn_r, warn_g, warn_b) = (
            (warning.red() * 255.0) as u8,
            (warning.green() * 255.0) as u8,
            (warning.blue() * 255.0) as u8,
        );
        let accent = &semantic.accent;
        let (acc_r, acc_g, acc_b) = (
            (accent.red() * 255.0) as u8,
            (accent.green() * 255.0) as u8,
            (accent.blue() * 255.0) as u8,
        );
        let info = &semantic.info;
        let (info_r, info_g, info_b) = (
            (info.red() * 255.0) as u8,
            (info.green() * 255.0) as u8,
            (info.blue() * 255.0) as u8,
        );
        let css = format!(
            ".terminal-box scrollbar {{ background-color: rgb({br},{bg_g},{bb}); }}
             .terminal-box scrollbar trough {{ background-color: rgb({br},{bg_g},{bb}); }}
             .terminal-box scrollbar slider {{ background-color: rgba({fr},{fg_g},{fb},0.4); }}
             .terminal-box scrollbar slider:hover {{ background-color: rgba({fr},{fg_g},{fb},0.7); }}
             .top-bar {{ background-color: rgb({br},{bg_g},{bb}); color: rgb({fr},{fg_g},{fb}); }}
             .top-bar button {{ color: rgb({fr},{fg_g},{fb}); }}
             .sidebar-box {{ background-color: rgb({br},{bg_g},{bb}); }}
             .sidebar-switcher, .sidebar-switcher button, .sidebar-switcher label,
             .file-tree-header, .file-tree-header button, .file-tree-header label,
             .file-tree-root {{ color: rgb({fr},{fg_g},{fb}); }}
             .file-tree-root {{ opacity: 1.0; }}
             .tab-strip-btn {{ color: rgb({mut_r},{mut_g},{mut_b}); }}
             .tab-strip-btn:checked {{ color: rgb({fr},{fg_g},{fb}); }}
             .tab-strip-btn.tab-marked {{ background-color: rgba({fr},{fg_g},{fb},0.2); font-weight: bold; }}
             .tab-bell, .tab-pin-icon, .tab-conn-dot.tab-connecting {{ color: rgb({warn_r},{warn_g},{warn_b}); }}
             .tab-conn-dot.tab-connected {{ color: rgb({ok_r},{ok_g},{ok_b}); }}
             .tab-conn-dot.tab-disconnected {{ color: rgb({err_r},{err_g},{err_b}); }}
             .pane-header-command {{ color: rgb({info_r},{info_g},{info_b}); }}
             .tab-strip-search {{ color: rgb({fr},{fg_g},{fb}); }}
             .tab-strip-search text {{ color: rgb({fr},{fg_g},{fb}); caret-color: rgb({fr},{fg_g},{fb}); }}
             .ai-panel {{
                 min-width: 240px;
                 background-color: rgb({br},{bg_g},{bb});
                 color: rgb({fr},{fg_g},{fb});
                 border-left: 1px solid rgba({fr},{fg_g},{fb},0.16);
             }}
             .ai-panel-header {{
                 padding: 10px 10px 8px 10px;
                 border-bottom: 1px solid rgba({fr},{fg_g},{fb},0.12);
             }}
             .ai-panel-title {{ color: rgb({fr},{fg_g},{fb}); font-weight: 700; }}
             .ai-panel-subtitle {{ color: rgb({mut_r},{mut_g},{mut_b}); font-size: 0.86em; }}
             .ai-chat-header-button {{ min-width: 30px; min-height: 30px; padding: 4px; }}
             .ai-chat-library {{ background-color: rgb({br},{bg_g},{bb}); }}
             .ai-chat-library-toolbar {{
                 padding: 10px;
                 border-bottom: 1px solid rgba({fr},{fg_g},{fb},0.12);
             }}
             .ai-chat-search {{ color: rgb({fr},{fg_g},{fb}); }}
             .ai-chat-search text {{
                 color: rgb({fr},{fg_g},{fb});
                 caret-color: rgb({fr},{fg_g},{fb});
             }}
             .ai-chat-list {{
                 margin: 8px;
                 background-color: transparent;
             }}
             .ai-chat-row {{
                 color: rgb({fr},{fg_g},{fb});
                 border-radius: 8px;
                 margin: 2px 0;
             }}
             .ai-chat-row:hover {{ background-color: rgba({fr},{fg_g},{fb},0.08); }}
             .ai-chat-row.active {{ background-color: rgba({fr},{fg_g},{fb},0.14); }}
             .ai-chat-row.archived {{ color: rgb({mut_r},{mut_g},{mut_b}); }}
             .ai-chat-row.unread {{ font-weight: 700; }}
             .ai-chat-row.error {{ color: rgb({err_r},{err_g},{err_b}); }}
             .ai-chat-section {{
                 color: rgb({mut_r},{mut_g},{mut_b});
                 font-size: 0.82em;
                 font-weight: 700;
                 padding: 8px 8px 4px 8px;
             }}
             .ai-chat-empty {{ color: rgb({mut_r},{mut_g},{mut_b}); padding: 28px; }}
             .ai-transcript, .ai-transcript text {{
                 background-color: rgb({br},{bg_g},{bb});
                 color: rgb({fr},{fg_g},{fb});
             }}
             .ai-empty-state {{ color: rgb({mut_r},{mut_g},{mut_b}); padding: 24px; }}
             .ai-empty-title {{ color: rgb({fr},{fg_g},{fb}); font-weight: 700; font-size: 1.08em; }}
             .ai-empty-actions {{ margin: 4px 0; }}
             .ai-empty-action {{
                 min-height: 32px;
                 color: rgb({fr},{fg_g},{fb});
                 border: 1px solid rgba({fr},{fg_g},{fb},0.16);
                 border-radius: 8px;
             }}
             .ai-panel-status-row {{
                 min-height: 22px;
                 padding: 2px 10px 4px 10px;
                 color: rgb({mut_r},{mut_g},{mut_b});
             }}
             .ai-panel-status-row.error {{ color: rgb({err_r},{err_g},{err_b}); }}
             .ai-status-action {{ min-height: 28px; padding: 2px 8px; }}
             .ai-panel-composer {{
                 padding: 8px;
                 border-top: 1px solid rgba({fr},{fg_g},{fb},0.12);
             }}
             .ai-context-chip {{
                 padding: 5px 8px;
                 color: rgb({mut_r},{mut_g},{mut_b});
                 background-color: rgba({fr},{fg_g},{fb},0.07);
                 border: 1px solid rgba({fr},{fg_g},{fb},0.16);
                 border-radius: 9px;
             }}
             .ai-context-label {{ font-size: 0.88em; }}
             .ai-context-clear {{ min-height: 24px; padding: 1px 6px; }}
             .ai-panel-input {{
                 background-color: rgba({fr},{fg_g},{fb},0.06);
                 border: 1px solid rgba({fr},{fg_g},{fb},0.20);
                 border-radius: 10px;
             }}
             .ai-panel-input textview, .ai-panel-input text {{
                 background-color: transparent;
                 color: rgb({fr},{fg_g},{fb});
                 caret-color: rgb({fr},{fg_g},{fb});
             }}
             .ai-input-placeholder {{ color: rgb({mut_r},{mut_g},{mut_b}); padding: 8px; }}
             .ai-input-hint {{ color: rgb({mut_r},{mut_g},{mut_b}); font-size: 0.82em; }}
             .ai-send-button {{ min-width: 72px; min-height: 32px; }}
             .agent-surface {{
                 background-color: rgb({br},{bg_g},{bb});
                 color: rgb({fr},{fg_g},{fb});
             }}
             .agent-surface headerbar {{
                 background-color: rgb({br},{bg_g},{bb});
                 color: rgb({fr},{fg_g},{fb});
                 box-shadow: none;
             }}
             .agent-dashboard {{
                 background-color: rgb({br},{bg_g},{bb});
                 color: rgb({fr},{fg_g},{fb});
             }}
             .agent-overview, .agent-setting-card, .agent-status-card,
             .agent-composer, .agent-transcript-card {{
                 background-color: rgba({fr},{fg_g},{fb},0.055);
                 border: 1px solid rgba({fr},{fg_g},{fb},0.14);
                 border-radius: 12px;
             }}
             .agent-context-card {{
                 padding: 8px 10px;
                 background-color: rgba({fr},{fg_g},{fb},0.045);
                 border: 1px solid rgba({fr},{fg_g},{fb},0.12);
                 border-radius: 9px;
             }}
             .agent-overview {{ padding: 12px; }}
             .agent-icon {{
                 color: rgb({acc_r},{acc_g},{acc_b});
                 background-color: alpha(@accent_bg_color, 0.18);
                 border-radius: 10px;
                 padding: 8px;
             }}
             .agent-chip {{
                 color: rgb({mut_r},{mut_g},{mut_b});
                 background-color: rgba({fr},{fg_g},{fb},0.08);
                 border-radius: 999px;
                 padding: 4px 9px;
                 font-size: 0.82em;
             }}
             .agent-safety-chip {{
                 color: rgb({ok_r},{ok_g},{ok_b});
                 background-color: alpha(@success_bg_color, 0.14);
             }}
             .agent-setting-card {{ padding: 10px 12px; }}
             .agent-section-label {{
                 color: rgb({mut_r},{mut_g},{mut_b});
                 font-size: 0.78em;
                 font-weight: 700;
                 padding: 9px 11px 7px 11px;
                 border-bottom: 1px solid rgba({fr},{fg_g},{fb},0.10);
             }}
             .agent-transcript, .agent-transcript text {{
                 background-color: transparent;
                 color: rgb({fr},{fg_g},{fb});
             }}
             .agent-status-card {{ padding: 9px 11px; }}
             .agent-status {{ color: rgb({mut_r},{mut_g},{mut_b}); }}
             .agent-status-card progressbar trough {{
                 min-height: 4px;
                 background-color: rgba({fr},{fg_g},{fb},0.10);
                 border-radius: 999px;
             }}
             .agent-status-card progressbar progress {{
                 min-height: 4px;
                 background-color: @accent_bg_color;
                 border-radius: 999px;
             }}
             .agent-proposal-card {{
                 padding: 12px;
                 color: rgb({fr},{fg_g},{fb});
                 background-color: rgb({br},{bg_g},{bb});
                 border: 1px solid rgba({warn_r},{warn_g},{warn_b},0.48);
                 border-radius: 12px;
                 box-shadow: none;
             }}
             .agent-proposal-card .command-review {{
                 color: rgb({fr},{fg_g},{fb});
             }}
             .agent-danger-command {{
                 padding: 8px;
                 font-family: monospace;
                 background-color: alpha(@warning_bg_color, 0.16);
                 border-radius: 7px;
             }}
             .agent-composer {{ padding: 9px; }}
             .agent-input {{
                 min-height: 34px;
                 color: rgb({fr},{fg_g},{fb});
                 caret-color: rgb({fr},{fg_g},{fb});
                 background-color: rgba({br},{bg_g},{bb},0.62);
                 border-color: rgba({fr},{fg_g},{fb},0.20);
             }}
             .agent-input text {{
                 color: rgb({fr},{fg_g},{fb});
                 caret-color: rgb({fr},{fg_g},{fb});
             }}
             .agent-input placeholder, .agent-input text placeholder {{
                 color: rgb({mut_r},{mut_g},{mut_b});
             }}
             .agent-input:disabled, .agent-input:disabled text {{
                 color: rgb({mut_r},{mut_g},{mut_b});
             }}
             .agent-turn-label {{
                 color: rgb({mut_r},{mut_g},{mut_b});
             }}
             .agent-send {{ min-width: 72px; min-height: 34px; }}
             .agent-input-hint {{
                 color: rgb({mut_r},{mut_g},{mut_b});
                 font-size: 0.82em;
             }}
             .bottom-bar {{
                 background-color: rgb({br},{bg_g},{bb});
                 color: rgb({fr},{fg_g},{fb});
             }}
             .bottom-bar .bb-normal {{ color: rgb({fr},{fg_g},{fb}); }}
             .bottom-bar .bb-muted {{ color: rgb({mut_r},{mut_g},{mut_b}); }}
             .bottom-bar .bb-ok {{ color: rgb({ok_r},{ok_g},{ok_b}); }}
             .bottom-bar .bb-err {{ color: rgb({err_r},{err_g},{err_b}); }}"
        );
        self.scrollbar_css.load_from_string(&css);
        self.ai_panel.apply_theme_colors();
    }

    pub(crate) fn apply_font_all(&self) {
        let config = self.config.borrow();
        let font_desc = FontDescription::from_string(&config.font_desc);
        drop(config);
        for i in 0..self.notebook.n_pages() {
            if let Some(widget) = self.notebook.nth_page(Some(i)) {
                let Some(node) = PaneNode::from_widget(&widget) else {
                    continue;
                };
                for leaf in node.leaves() {
                    if let Some(view) = leaf.block_view() {
                        view.set_font(&font_desc);
                    } else {
                        leaf.terminal().set_font(Some(&font_desc));
                    }
                }
            }
        }
    }

    pub(crate) fn apply_scrollback_all(&self) {
        let lines = self.config.borrow().terminal_scrollback_lines;
        self.for_each_terminal(|term| {
            term.set_scrollback_lines(lines as i64);
        });
    }

    pub(crate) fn apply_theme(&self, theme: &Theme) {
        {
            let mut config = self.config.borrow_mut();
            config.theme_name = theme.name.clone();
            config.foreground = theme.foreground;
            config.background = theme.background;
            config.cursor = theme.cursor;
            config.cursor_foreground = theme.cursor_foreground;
            config.palette = theme.palette;
        }
        self.apply_colors_all();
    }

    /// Reload configuration from disk and apply changes.
    pub(crate) fn reload_config(&self) {
        if std::env::var_os("FORGE_SAFE_MODE").is_some() {
            let dialog = adw::AlertDialog::new(
                Some("Configuration reload disabled"),
                Some("Safe mode keeps the built-in VTE profile isolated from user configuration."),
            );
            dialog.add_response("ok", "OK");
            dialog.set_default_response(Some("ok"));
            dialog.present(Some(&self.window));
            return;
        }
        let path = config_file_path();
        let validation = match crate::config_store::read_config_text(&path) {
            Ok(Some(contents)) => {
                let disk_revision =
                    crate::config_store::ConfigRevision::from_bytes(contents.as_bytes());
                let live_revision = {
                    let config = self.config.borrow();
                    live_config_revision(&config)
                };
                if reload_matches_live_revision(live_revision.as_ref(), &disk_revision) {
                    log::debug!(
                        "Config reload skipped: file matches the current in-memory revision"
                    );
                    return;
                }
                validate_config_contents(&contents).map_err(|error| {
                    crate::config::config_syntax_diagnostic(&contents, &error).to_string()
                })
            }
            Ok(None) => {
                let disk_revision = crate::config_store::ConfigRevision::missing();
                let live_revision = {
                    let config = self.config.borrow();
                    live_config_revision(&config)
                };
                if reload_matches_live_revision(live_revision.as_ref(), &disk_revision) {
                    log::debug!(
                        "Config reload skipped: file matches the current in-memory revision"
                    );
                    return;
                }
                Ok(Vec::new())
            }
            Err(error) => Err(error.to_string()),
        };
        {
            match validation {
                Ok(issues) if issues.iter().any(|issue| issue.is_error()) => {
                    let details = issues
                        .iter()
                        .filter(|issue| issue.is_error())
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("\n");
                    for issue in issues {
                        log::error!("Config reload rejected: {issue}");
                    }
                    self.show_config_error(
                        "Configuration reload rejected",
                        &format!(
                            "The current settings remain active. Fix these errors first:\n\n{details}"
                        ),
                    );
                    return;
                }
                Err(err) => {
                    let path_display = jterm_core::review_input::safe_inline_display(
                        &path.to_string_lossy(),
                        2 * 1024,
                    );
                    log::error!("Config reload rejected for {path_display}: {err}");
                    self.show_config_error(
                        "Configuration reload rejected",
                        &format!("The current settings remain active. {path_display}: {err}"),
                    );
                    return;
                }
                _ => {}
            }
        }
        let (new_config, _themes, new_keybindings) = load_config();
        let opacity = new_config.window_opacity;
        let font_scale = new_config.default_font_scale;
        let tab_placement = new_config.tab_placement;
        let sidebar_view = new_config.sidebar_view;
        let sidebar_visible = new_config.sidebar_visible;
        let ai_visible = new_config.ai_enabled && new_config.ai_panel_visible;

        // New panes/tabs immediately use a changed shell; all other config is
        // replaced as one coherent snapshot instead of retaining stale fields.
        *self.shell_argv.borrow_mut() = choose_shell_argv(new_config.shell.as_deref());
        *self.config.borrow_mut() = new_config;

        // TermView owns a shared clone used by long-lived callbacks. Refresh it
        // as well so behavior changes do not require reopening block tabs.
        self.sync_block_configs();

        // Apply all visual changes
        self.window_opacity.set(opacity);
        self.window.set_opacity(opacity);
        self.set_font_scale_all(font_scale);
        self.apply_font_all();
        self.apply_colors_all();
        self.apply_scrollback_all();

        self.tab_placement.set(tab_placement);
        self.sidebar_view.set(sidebar_view);
        self.apply_tab_placement();
        self.set_sidebar_visible(sidebar_visible, false);
        self.sync_bottom_bar_visibility();

        self.ai_panel_visible.set(ai_visible);
        if ai_visible {
            self.ai_paned.set_end_child(Some(&self.ai_panel.root));
            self.restore_ai_panel_width();
        } else {
            self.ai_paned.set_end_child(None::<&gtk4::Widget>);
        }
        self.ai_panel.refresh_config_display();
        self.ai_panel.refresh_persisted_privacy();
        self.sync_agent_toggle();

        // Update keybindings
        *self.keybinding_map.borrow_mut() = new_keybindings;

        log::info!("Configuration reloaded from disk");
    }
}

#[cfg(test)]
mod tests {
    use super::reload_matches_live_revision;
    use crate::config_store::ConfigRevision;

    #[test]
    fn reload_skips_matching_present_revision() {
        let revision = ConfigRevision::from_bytes(b"theme = 'light'\n");
        assert!(reload_matches_live_revision(Some(&revision), &revision));
    }

    #[test]
    fn reload_skips_matching_missing_revision() {
        let revision = ConfigRevision::missing();
        assert!(reload_matches_live_revision(Some(&revision), &revision));
    }

    #[test]
    fn reload_keeps_external_revision_changes_visible() {
        let live = ConfigRevision::from_bytes(b"theme = 'light'\n");
        let disk = ConfigRevision::from_bytes(b"theme = 'dark'\n");
        assert!(!reload_matches_live_revision(Some(&live), &disk));
        assert!(!reload_matches_live_revision(None, &disk));
    }
}
