//! Install and update surfaces for the companion shell, jsh.
//!
//! Two entry points, both explicit: the command palette action, and a notice
//! bar that appears only after a background check found something actionable.
//! Nothing installs itself, and nothing blocks startup — the check runs on a
//! worker thread and the bar stays hidden until it has an answer.

use adw::prelude::*;
use gtk4::glib;
use gtk4::{Align, Box as GBox, Button, Label, Orientation};
use libadwaita as adw;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use super::UiState;
use crate::jsh_install::{self, Status};

/// How often the pending check result is polled from the worker thread.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

impl UiState {
    /// Run the installer in its own tab. The script narrates what it does, so
    /// the tab is the progress UI — no bespoke dialog, and the user can read
    /// the failure or interrupt it with Ctrl+C like any other command.
    pub(crate) fn install_or_update_jsh(&self) {
        match jsh_install::install_argv() {
            Ok(argv) => {
                self.add_named_tab_with_argv("Install jsh", argv);
            }
            Err(error) => {
                let error = error.to_string();
                log::warn!(
                    "cannot stage the jsh installer: {}",
                    crate::review_input::safe_inline_display(&error, 4 * 1024)
                );
                let error = crate::review_input::safe_multiline_display(&error, 16 * 1024);
                let dialog = adw::AlertDialog::new(
                    Some("Cannot install jsh"),
                    Some(&format!("Writing the installer script failed: {error}")),
                );
                dialog.add_response("ok", "OK");
                dialog.set_default_response(Some("ok"));
                dialog.present(Some(&self.window));
            }
        }
    }

    /// Build the (initially hidden) jsh notice bar and kick off the update
    /// check that may reveal it. The caller places the returned widget.
    pub(crate) fn build_jsh_notice(self: &Rc<Self>) -> GBox {
        let bar = GBox::new(Orientation::Horizontal, 8);
        bar.add_css_class("toolbar");
        bar.set_margin_start(6);
        bar.set_margin_end(6);
        bar.set_margin_top(2);
        bar.set_margin_bottom(2);
        bar.set_visible(false);

        let label = Label::new(None);
        label.set_halign(Align::Start);
        label.set_hexpand(true);
        // One line, shortened in the middle: a notice bar must not grow the
        // header when the window is narrow.
        label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
        label.set_xalign(0.0);
        bar.append(&label);

        let action = Button::with_label("Install");
        action.add_css_class("suggested-action");
        bar.append(&action);

        let dismiss = Button::from_icon_name("window-close-symbolic");
        dismiss.add_css_class("flat");
        dismiss.set_tooltip_text(Some("Dismiss until the next launch"));
        bar.append(&dismiss);

        {
            let ui = Rc::clone(self);
            let bar = bar.clone();
            action.connect_clicked(move |_| {
                bar.set_visible(false);
                ui.install_or_update_jsh();
            });
        }
        {
            let bar = bar.clone();
            dismiss.connect_clicked(move |_| bar.set_visible(false));
        }

        self.start_jsh_update_check(&bar, &label, &action);
        bar
    }

    /// Ask the installer what is published, off the main loop, and reveal the
    /// notice bar if the answer is actionable.
    fn start_jsh_update_check(self: &Rc<Self>, bar: &GBox, label: &Label, action: &Button) {
        // "startup" asks the network every launch; "daily" reuses the
        // installer's cache, which every jterm on this machine shares.
        let Some(max_age) = self.config.borrow().jsh_update_check.max_age() else {
            return;
        };

        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _ = sender.send(jsh_install::check_blocking(max_age));
        });

        let receiver = RefCell::new(receiver);
        let bar = bar.clone();
        let label = label.clone();
        let action = action.clone();
        glib::timeout_add_local(POLL_INTERVAL, move || match receiver.borrow().try_recv() {
            Ok(status) => {
                apply_jsh_status(&bar, &label, &action, &status);
                glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(TryRecvError::Disconnected) => {
                log::warn!("jsh update check ended without a result");
                glib::ControlFlow::Break
            }
        });
    }
}

/// Turn a check result into UI. A check that failed, or found nothing to do,
/// leaves the bar hidden: an offline machine must not grow a bar whose button
/// cannot work.
fn apply_jsh_status(bar: &GBox, label: &Label, action: &Button, status: &Status) {
    if let Some(error) = &status.error {
        log::info!(
            "jsh update check unavailable: {}",
            crate::review_input::safe_inline_display(error, 4 * 1024)
        );
    }
    if let Some(other) = &status.shadowed_by {
        // Some other binary named jsh, earlier on PATH. Installing does not fix
        // PATH order, so the installer explains it in the tab; here it is only
        // worth a log line.
        log::warn!(
            "PATH resolves jsh to {}, which jterm does not manage",
            crate::review_input::safe_inline_display(other, 4 * 1024)
        );
    }

    let Some(prompt) = jsh_install::prompt_for(status) else {
        bar.set_visible(false);
        return;
    };
    let title = crate::review_input::safe_inline_display(&prompt.banner_title(), 4 * 1024);
    let button = crate::review_input::safe_inline_display(prompt.button_label(), 256);
    log::info!("jsh notice: {title}");
    label.set_text(&title);
    action.set_label(&button);
    bar.set_visible(true);
}
