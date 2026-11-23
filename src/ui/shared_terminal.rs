use gtk4::{pango, prelude::*};
#[cfg(target_os = "linux")]
use jterm_gtk_view::RuntimeShell;
use jterm_gtk_view::{LinkActions, Pty, TerminalPane, Theme};
use std::rc::Rc;

use crate::{config::Config, keybindings::Action, pty::OwnedPty};

const PAGE_DATA: &str = "forge-shared-terminal";

impl Pty for OwnedPty {
    fn write(&self, bytes: &[u8]) -> Result<(), String> {
        self.write_bytes(bytes).map_err(|error| error.to_string())
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        OwnedPty::resize(self, cols, rows).map_err(|error| error.to_string())
    }

    fn stop(&self) {
        self.kill();
    }

    fn start_reader(
        &self,
        output: impl FnMut(Vec<u8>) + 'static,
        exited: impl FnOnce(i32) + 'static,
    ) -> Result<(), String> {
        OwnedPty::start_reader(self, output, exited);
        Ok(())
    }
}

pub(crate) struct SharedTerminal {
    pane: Rc<TerminalPane<OwnedPty>>,
    #[cfg(target_os = "linux")]
    runtime_guard: Option<std::sync::Arc<crate::gtk_process::Guard>>,
}

impl SharedTerminal {
    pub(crate) fn links() -> LinkActions {
        LinkActions {
            admit: jterm_core::link::is_openable_osc8,
            find_plain: Some(jterm_core::link::plain_url_at),
            attach_opener: |_| Box::new(crate::terminal::open_uri),
        }
    }

    pub(crate) fn open(
        config: &Config,
        argv: &[String],
        cwd: Option<&str>,
        close: impl Fn(&gtk4::Widget) + 'static,
    ) -> Result<gtk4::Widget, String> {
        let theme = Theme {
            font: pango::FontDescription::from_string(&config.font_desc),
            foreground: config.foreground,
            background: config.background,
            palette: config.palette,
        };
        let links = Self::links();
        #[cfg(target_os = "linux")]
        let mut runtime_guard = None;
        let status = jterm_core::review_input::safe_inline_display;
        let legacy = |theme, links, close| {
            let argv: Vec<_> = argv.iter().map(String::as_str).collect();
            let pty = OwnedPty::spawn(&argv, cwd, &jterm_gtk_view::CHILD_ENV)
                .map_err(|error| error.to_string())?;
            TerminalPane::new(pty, theme, status, links, close)
        };
        let (widget, pane) = {
            #[cfg(target_os = "linux")]
            {
                let runtime = RuntimeShell::spawn_candidate(
                    argv,
                    cwd,
                    crate::host::is_flatpak(),
                    crate::host::working_directory_available,
                    || {
                        Self::runtime_environment(
                            argv,
                            config.vim9.auto_gui,
                            &jterm_gtk_view::CHILD_ENV,
                        )
                    },
                )?;
                if let Some(runtime) = runtime {
                    let pid = runtime
                        .child_pid()
                        .ok_or("shared runtime child has no process id")?;
                    runtime_guard = Some(crate::gtk_process::Guard::capture(
                        pid,
                        RuntimeShell::candidate_program(argv),
                    ));
                    TerminalPane::with_runtime(runtime, theme, status, links, close)?
                } else {
                    legacy(theme, links, close)?
                }
            }
            #[cfg(not(target_os = "linux"))]
            legacy(theme, links, close)?
        };
        pane.set_history_row_limit(config.terminal_scrollback_lines as usize);
        let session = Self {
            pane,
            #[cfg(target_os = "linux")]
            runtime_guard,
        };
        unsafe { widget.set_data(PAGE_DATA, Rc::new(session)) };
        Ok(widget)
    }

    #[cfg(target_os = "linux")]
    fn runtime_environment(
        argv: &[String],
        auto_gui: bool,
        terminal_env: &[(&str, &str)],
    ) -> Result<Vec<std::ffi::CString>, String> {
        let policy = jterm_core::child_env::ChildEnv {
            less_default: Some("R"),
            ..jterm_core::child_env::ChildEnv::from_identity()
        };
        let vim = crate::vim9_ide::shell_environment(argv, auto_gui);
        let extra = terminal_env
            .iter()
            .copied()
            .chain(
                vim.iter()
                    .map(|(name, value)| (name.as_str(), value.as_str())),
            )
            .collect::<Vec<_>>();
        let env = jterm_core::child_env::envp_from_captured(&policy, &extra)
            .map_err(|error| error.to_string())?;
        let mut env = crate::agent_cli::environment(env);
        env.retain(|item| {
            let bytes = item.as_bytes();
            !bytes.starts_with(b"FORGE_SHELL_INTEGRATION_FD=")
                && !bytes.starts_with(b"FORGE_SHELL_INTEGRATION_TOKEN=")
        });
        Ok(env)
    }

    pub(crate) fn from_page(page: &gtk4::Widget) -> Option<Rc<Self>> {
        unsafe {
            page.data::<Rc<Self>>(PAGE_DATA)
                .map(|data| data.as_ref().clone())
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn matches_vim_source(
        &self,
        invocation: &jterm_core::vim9::Vim9ShellInvocation,
    ) -> bool {
        self.pane
            .runtime()
            .and_then(|runtime| {
                runtime.inspect_child(|pid| invocation.matches_pid(pid).then_some(()))
            })
            .is_some()
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn focus_widget(&self) -> gtk4::Widget {
        self.pane.screen().clone().upcast()
    }

    pub(crate) fn focus(&self) {
        self.pane.focus();
    }

    pub(crate) fn close_blocker(&self) -> Option<String> {
        #[cfg(target_os = "linux")]
        if let (Some(runtime), Some(guard)) = (self.pane.runtime(), &self.runtime_guard) {
            // Shutdown stops the runtime before its reader can publish an exit code.
            runtime.child_pid()?;
            return guard.blocker(true, None);
        }
        self.pane.pty().and_then(|pty| pty.close_blocker(true))
    }

    pub(crate) fn child_cleanup(&self) -> crate::gtk_process::ChildDrain {
        #[cfg(target_os = "linux")]
        if let Some(runtime) = self.pane.runtime() {
            return crate::gtk_process::ChildDrain::SharedRuntime(Rc::clone(runtime));
        }
        self.pane
            .pty()
            .expect("legacy pane owns a PTY")
            .lifecycle()
            .into()
    }

    pub(crate) fn withdrawal_work_probe(&self) -> Box<dyn Fn() -> bool> {
        #[cfg(target_os = "linux")]
        if let (Some(runtime), Some(guard)) = (self.pane.runtime(), &self.runtime_guard) {
            let runtime = Rc::clone(runtime);
            let probe = guard.withdrawal_work_probe();
            return Box::new(move || runtime.child_pid().is_some() && probe());
        }
        self.pane
            .pty()
            .expect("legacy pane owns a PTY")
            .withdrawal_work_probe()
    }

    pub(crate) fn stop(&self) {
        self.pane.stop();
    }

    pub(crate) fn terminal_action(&self, action: Action) {
        match action {
            Action::Copy => self.pane.copy(),
            Action::Paste => self.pane.paste(),
            Action::ToggleSearch => self.pane.toggle_history_search(),
            _ => {}
        }
    }
}

/// A conventional terminal mounted as a regular notebook/split leaf.
#[cfg(target_os = "linux")]
pub(crate) struct SharedPane {
    root: gtk4::Widget,
    header: super::PaneHeader,
    live: Option<SharedTerminal>,
    failure: Option<String>,
}

#[cfg(target_os = "linux")]
impl SharedPane {
    pub(crate) fn open(
        config: &Config,
        argv: &[String],
        cwd: Option<&str>,
        session_id: &str,
    ) -> Option<Rc<Self>> {
        let home = std::env::var("HOME").ok();
        let runtime = RuntimeShell::spawn_session_candidate(
            argv,
            cwd.or(home.as_deref()),
            session_id,
            crate::host::is_flatpak(),
            crate::host::working_directory_available,
            || {
                SharedTerminal::runtime_environment(
                    argv,
                    config.vim9.auto_gui,
                    &jterm_gtk_view::INTERACTIVE_CHILD_ENV,
                )
            },
        );
        let open = |runtime: Rc<RuntimeShell>| {
            let pid = runtime
                .child_pid()
                .ok_or("shared shell has no live process id")?;
            let guard =
                crate::gtk_process::Guard::capture(pid, RuntimeShell::candidate_program(argv));
            let (widget, pane) = TerminalPane::with_runtime_embedded(
                runtime,
                Self::theme(config, config.default_font_scale),
                jterm_core::review_input::safe_inline_display,
                SharedTerminal::links(),
            )?;
            pane.set_history_row_limit(config.terminal_scrollback_lines as usize);
            Ok::<_, String>((
                widget,
                SharedTerminal {
                    pane,
                    runtime_guard: Some(guard),
                },
            ))
        };
        let (view, live, failure) = match runtime.and_then(|runtime| runtime.map(open).transpose())
        {
            Ok(None) => return None,
            Ok(Some((view, session))) => (view, Some(session), None),
            Err(error) => {
                let safe = jterm_core::review_input::safe_inline_display(&error, 768);
                let message = format!("Cannot start terminal: {safe}");
                let label = gtk4::Label::new(Some(&message));
                label.set_wrap(true);
                label.set_focusable(true);
                (label.upcast(), None, Some(message))
            }
        };
        let header = super::PaneHeader::new();
        let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.append(header.widget());
        root.append(&view);
        Some(Rc::new(Self {
            root: root.upcast(),
            header,
            live,
            failure,
        }))
    }

    fn theme(config: &Config, scale: f64) -> Theme {
        let mut font = crate::font::terminal_font_description(&config.font_desc, config);
        if font.size() > 0 && scale.is_finite() && scale > 0.0 {
            let size = (f64::from(font.size()) * scale).round();
            if font.is_size_absolute() {
                font.set_absolute_size(size);
            } else {
                font.set_size(size as i32);
            }
        }
        Theme {
            font,
            foreground: config.foreground,
            background: config.background,
            palette: config.palette,
        }
    }

    pub(crate) fn update_theme(&self, config: &Config, scale: f64) {
        if let Some(live) = &self.live {
            live.pane.set_theme(Self::theme(config, scale));
        }
    }

    pub(crate) fn root(&self) -> &gtk4::Widget {
        &self.root
    }

    pub(crate) fn header(&self) -> &super::PaneHeader {
        &self.header
    }

    pub(crate) fn focus_widget(&self) -> gtk4::Widget {
        self.live.as_ref().map_or_else(
            || self.root.last_child().unwrap_or_else(|| self.root.clone()),
            |live| live.pane.screen().clone().upcast(),
        )
    }

    pub(crate) fn terminal_pane(&self) -> Option<&Rc<TerminalPane<OwnedPty>>> {
        self.live.as_ref().map(|live| &live.pane)
    }

    pub(crate) fn terminal_action(&self, action: Action) {
        if let Some(terminal) = &self.live {
            terminal.terminal_action(action);
        }
    }

    pub(crate) fn matches_vim_source(
        &self,
        invocation: &jterm_core::vim9::Vim9ShellInvocation,
    ) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.matches_vim_source(invocation))
    }

    pub(crate) fn launch_error(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    pub(crate) fn close_blocker(&self) -> Option<String> {
        self.live.as_ref()?.close_blocker()
    }

    pub(crate) fn child_cleanup(&self) -> Option<crate::gtk_process::ChildDrain> {
        self.live.as_ref().map(SharedTerminal::child_cleanup)
    }

    pub(crate) fn withdrawal_work_probe(&self) -> Box<dyn Fn() -> bool> {
        match &self.live {
            Some(live) => live.withdrawal_work_probe(),
            None => Box::new(|| false),
        }
    }

    pub(crate) fn stop(&self) {
        if let Some(live) = &self.live {
            live.stop();
        }
    }
}
