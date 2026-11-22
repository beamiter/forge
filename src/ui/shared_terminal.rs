use gtk4::{pango, prelude::*};
#[cfg(target_os = "linux")]
use jterm_gtk_view::RuntimeShell;
use jterm_gtk_view::{LinkActions, Pty, TerminalPane, Theme};
use std::rc::Rc;

use crate::{config::Config, pty::OwnedPty};

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
        let links = LinkActions {
            admit: |_, uri| jterm_core::link::is_openable_url(uri),
            attach_opener: |_| Box::new(crate::terminal::open_uri),
        };
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
                    Self::runtime_environment,
                )?;
                if let Some(runtime) = runtime {
                    let pid = runtime
                        .child_pid()
                        .ok_or("shared runtime child has no process id")?;
                    runtime_guard = Some(crate::gtk_process::Guard::capture(
                        pid,
                        Some(std::path::Path::new(&argv[0])),
                    ));
                    TerminalPane::with_runtime(runtime, theme, status, links, close)?
                } else {
                    legacy(theme, links, close)?
                }
            }
            #[cfg(not(target_os = "linux"))]
            legacy(theme, links, close)?
        };
        let session = Self {
            pane,
            #[cfg(target_os = "linux")]
            runtime_guard,
        };
        unsafe { widget.set_data(PAGE_DATA, Rc::new(session)) };
        Ok(widget)
    }

    #[cfg(target_os = "linux")]
    fn runtime_environment() -> Result<Vec<std::ffi::CString>, String> {
        let policy = jterm_core::child_env::ChildEnv {
            less_default: Some("R"),
            ..jterm_core::child_env::ChildEnv::from_identity()
        };
        let env = jterm_core::child_env::envp_from_captured(&policy, &jterm_gtk_view::CHILD_ENV)
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

    pub(crate) fn focus(&self) {
        self.pane.focus();
    }

    pub(crate) fn close_blocker(&self) -> Option<String> {
        #[cfg(target_os = "linux")]
        if let (Some(runtime), Some(guard)) = (self.pane.runtime(), &self.runtime_guard) {
            return runtime
                .exit_code()
                .is_none()
                .then(|| guard.blocker(true, None))
                .flatten();
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
            return Box::new(move || runtime.exit_code().is_none() && probe());
        }
        self.pane
            .pty()
            .expect("legacy pane owns a PTY")
            .withdrawal_work_probe()
    }

    pub(crate) fn stop(&self) {
        self.pane.stop();
    }

    pub(crate) fn copy(&self) {
        self.pane.copy();
    }

    pub(crate) fn paste(&self) {
        self.pane.paste();
    }
}
