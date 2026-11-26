use gtk4::{pango, prelude::*};
#[cfg(target_os = "linux")]
use jterm_gtk_view::RuntimeShell;
use jterm_gtk_view::{CursorColors, LinkActions, TerminalPane, Theme};
#[cfg(target_os = "linux")]
use std::os::fd::BorrowedFd;
use std::rc::Rc;

use crate::{config::Config, keybindings::Action, pty::OwnedPty};

const PAGE_DATA: &str = "forge-shared-terminal";

pub(crate) struct SharedTerminal {
    pane: Rc<TerminalPane<OwnedPty>>,
    find: Rc<jterm_gtk_view::TerminalFind<OwnedPty>>,
    #[cfg(target_os = "linux")]
    runtime_guard: Option<std::sync::Arc<crate::gtk_process::Guard>>,
}

impl SharedTerminal {
    #[cfg(forge_shared_terminal)]
    pub(crate) fn document_links() -> LinkActions {
        LinkActions {
            find_plain: Some(jterm_core::link::document_url_at),
            ..Self::links()
        }
    }

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
            cursor: Some(CursorColors {
                foreground: config.cursor_foreground,
                background: config.cursor,
            }),
        };
        let links = Self::links();
        #[cfg(all(target_os = "linux", any(not(forge_shared_terminal), test)))]
        let mut runtime_guard = None;
        #[cfg(all(target_os = "linux", forge_shared_terminal, not(test)))]
        let runtime_guard;
        let status = jterm_core::review_input::safe_inline_display;
        #[cfg(any(not(all(forge_shared_terminal, target_os = "linux")), test))]
        let legacy = |theme, links, close| {
            let argv: Vec<_> = argv.iter().map(String::as_str).collect();
            let pty = OwnedPty::spawn(&argv, cwd, &jterm_gtk_view::CHILD_ENV)
                .map_err(|error| error.to_string())?;
            TerminalPane::new(pty, theme, status, links, close)
        };
        let (widget, pane) = {
            #[cfg(target_os = "linux")]
            {
                #[cfg(all(forge_shared_terminal, not(test)))]
                let runtime = if crate::host::is_flatpak() {
                    let launch = crate::host::wrap_argv(argv, cwd, &jterm_gtk_view::CHILD_ENV);
                    Self::runtime_environment(
                        argv,
                        config.vim9.auto_gui,
                        &jterm_gtk_view::CHILD_ENV,
                    )
                    .and_then(|environment| {
                        RuntimeShell::spawn_pane_command(&launch, Some("/"), None, environment)
                    })
                } else {
                    RuntimeShell::spawn_configured_shell(
                        argv,
                        cwd,
                        None,
                        crate::host::working_directory_available,
                        || {
                            Self::runtime_environment(
                                argv,
                                config.vim9.auto_gui,
                                &jterm_gtk_view::CHILD_ENV,
                            )
                        },
                    )
                }
                .map(Some)?;
                #[cfg(any(not(forge_shared_terminal), test))]
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
                    #[cfg(any(not(forge_shared_terminal), test))]
                    {
                        legacy(theme, links, close)?
                    }
                    #[cfg(all(forge_shared_terminal, not(test)))]
                    {
                        return Err("shared terminal launch supplied no session".into());
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            legacy(theme, links, close)?
        };
        pane.set_history_row_limit(config.terminal_scrollback_lines as usize);
        let session = Self {
            find: pane.find_controller(),
            pane,
            #[cfg(target_os = "linux")]
            runtime_guard,
        };
        crate::eva_companion::tag_pane(&widget);
        unsafe { widget.set_data(PAGE_DATA, Rc::new(session)) };
        Ok(widget)
    }

    #[cfg(target_os = "linux")]
    fn runtime_environment(
        argv: &[String],
        auto_gui: bool,
        terminal_env: &[(&str, &str)],
    ) -> Result<Vec<std::ffi::CString>, String> {
        let vim = crate::vim9_ide::shell_environment(argv, auto_gui);
        let extra = terminal_env
            .iter()
            .copied()
            .chain(
                vim.iter()
                    .map(|(name, value)| (name.as_str(), value.as_str())),
            )
            .collect::<Vec<_>>();
        Self::captured_runtime_environment(&extra)
    }

    #[cfg(target_os = "linux")]
    fn captured_runtime_environment(
        extra: &[(&str, &str)],
    ) -> Result<Vec<std::ffi::CString>, String> {
        let policy = jterm_core::child_env::ChildEnv {
            less_default: Some("R"),
            ..jterm_core::child_env::ChildEnv::from_identity()
        };
        let env = jterm_core::child_env::envp_from_captured(&policy, extra)
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
            Action::ScrollUp => self.pane.scroll(-3),
            Action::ScrollDown => self.pane.scroll(3),
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
    hosted_directory: Option<String>,
    detached_focus: Option<gtk4::Button>,
    detached_style: Option<gtk4::CssProvider>,
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
        let cwd = cwd.or(home.as_deref());
        if crate::host::is_flatpak() {
            let Some(launch) = RuntimeShell::configured_shell_argv(argv, session_id) else {
                return Some(Self::failed(
                    config,
                    argv,
                    "configured shell session identity differs from the restored pane",
                ));
            };
            return Self::open_host_command(config, &launch, cwd);
        }
        #[cfg(all(forge_shared_terminal, not(test)))]
        let runtime = RuntimeShell::spawn_configured_shell(
            argv,
            cwd,
            Some(session_id),
            crate::host::working_directory_available,
            || {
                SharedTerminal::runtime_environment(
                    argv,
                    config.vim9.auto_gui,
                    &jterm_gtk_view::INTERACTIVE_CHILD_ENV,
                )
            },
        )
        .map(Some);
        #[cfg(any(not(forge_shared_terminal), test))]
        let runtime = RuntimeShell::spawn_session_candidate(
            argv,
            cwd,
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
        Self::from_runtime(config, argv, runtime, None, false)
    }

    pub(crate) fn open_hosted_attachment(
        config: &Config,
        argv: &[String],
        cwd: &str,
    ) -> Option<Rc<Self>> {
        let (launch, local_directory) =
            crate::host::hosted_attachment_pty(argv, cwd, &jterm_gtk_view::INTERACTIVE_CHILD_ENV);
        #[cfg(all(forge_shared_terminal, not(test)))]
        let runtime = SharedTerminal::runtime_environment(
            argv,
            config.vim9.auto_gui,
            &jterm_gtk_view::INTERACTIVE_CHILD_ENV,
        )
        .and_then(|environment| RuntimeShell::spawn_program(&launch, local_directory, environment))
        .map(Some);
        #[cfg(any(not(forge_shared_terminal), test))]
        let runtime = RuntimeShell::spawn_hosted_attachment(&launch, local_directory, || {
            SharedTerminal::runtime_environment(
                argv,
                config.vim9.auto_gui,
                &jterm_gtk_view::INTERACTIVE_CHILD_ENV,
            )
        });
        Self::from_runtime(config, &launch, runtime, Some(cwd), false)
    }

    pub(crate) fn open_explicit(
        config: &Config,
        argv: &[String],
        cwd: Option<&str>,
    ) -> Option<Rc<Self>> {
        let home = std::env::var("HOME").ok();
        let launch = crate::agent_cli::inline_argv(argv);
        let cwd = cwd.or(home.as_deref());
        if crate::host::is_flatpak() {
            return Self::open_host_command(config, &launch, cwd);
        }
        #[cfg(all(forge_shared_terminal, not(test)))]
        let runtime = SharedTerminal::runtime_environment(
            argv,
            config.vim9.auto_gui,
            &jterm_gtk_view::INTERACTIVE_CHILD_ENV,
        )
        .and_then(|environment| RuntimeShell::spawn_pane_command(&launch, cwd, None, environment))
        .map(Some);
        #[cfg(any(not(forge_shared_terminal), test))]
        let runtime =
            RuntimeShell::spawn_explicit_command(&launch, cwd, crate::host::is_flatpak(), || {
                SharedTerminal::runtime_environment(
                    argv,
                    config.vim9.auto_gui,
                    &jterm_gtk_view::INTERACTIVE_CHILD_ENV,
                )
            });
        Self::from_runtime(config, &launch, runtime, None, false)
    }

    pub(crate) fn open_task(
        config: &Config,
        argv: &[String],
        cwd: Option<&str>,
        pinned_directory: Option<BorrowedFd<'_>>,
        env_extra: &[(String, String)],
        inline_agent: bool,
    ) -> Option<Rc<Self>> {
        let host_bridge = crate::host::is_flatpak();
        let host_pin = pinned_directory.filter(|_| host_bridge);
        let home = std::env::var("HOME").ok();
        let cwd = cwd.or(home.as_deref());
        let vim = if env_extra.is_empty() {
            crate::vim9_ide::shell_environment(argv, config.vim9.auto_gui)
        } else {
            Vec::new()
        };
        let extra: Vec<_> = vim
            .iter()
            .chain(env_extra)
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let argv = crate::agent_cli::inline_argv(argv);
        let launch = if host_pin.is_some() {
            crate::host::wrap_pinned_validation_argv(&argv, &extra)
        } else {
            crate::host::wrap_argv(&argv, cwd, &extra)
        };
        let local_cwd = if host_bridge { Some("/") } else { cwd };
        let runtime =
            SharedTerminal::captured_runtime_environment(&extra).and_then(|environment| {
                match host_pin {
                    Some(directory) => RuntimeShell::spawn_forwarded_pane_command(
                        &launch,
                        directory,
                        crate::host::FLATPAK_VALIDATION_DIRECTORY_FD,
                        environment,
                    ),
                    None => RuntimeShell::spawn_pane_command(
                        &launch,
                        local_cwd,
                        pinned_directory,
                        environment,
                    ),
                }
            });
        Self::from_runtime(config, &launch, runtime.map(Some), None, inline_agent)
    }

    fn open_host_command(config: &Config, argv: &[String], cwd: Option<&str>) -> Option<Rc<Self>> {
        let environment = &jterm_gtk_view::INTERACTIVE_CHILD_ENV;
        let launch = crate::host::wrap_argv(argv, cwd, environment);
        #[cfg(all(forge_shared_terminal, not(test)))]
        let runtime = SharedTerminal::runtime_environment(argv, config.vim9.auto_gui, environment)
            .and_then(|environment| {
                RuntimeShell::spawn_pane_command(&launch, Some("/"), None, environment)
            })
            .map(Some);
        #[cfg(any(not(forge_shared_terminal), test))]
        let runtime = RuntimeShell::spawn_explicit_command(&launch, Some("/"), false, || {
            SharedTerminal::runtime_environment(argv, config.vim9.auto_gui, environment)
        });
        Self::from_runtime(config, &launch, runtime, None, false)
    }

    pub(crate) fn failed(config: &Config, argv: &[String], error: &str) -> Rc<Self> {
        Self::from_runtime(config, argv, Err(error.to_owned()), None, false)
            .expect("a failed launch always supplies a diagnostic pane")
    }

    fn from_runtime(
        config: &Config,
        argv: &[String],
        runtime: Result<Option<Rc<RuntimeShell>>, String>,
        hosted_directory: Option<&str>,
        inline_task: bool,
    ) -> Option<Rc<Self>> {
        let open = |runtime: Rc<RuntimeShell>| {
            let pid = runtime
                .child_pid()
                .ok_or("shared shell has no live process id")?;
            let guard =
                crate::gtk_process::Guard::capture(pid, RuntimeShell::candidate_program(argv));
            let mount = if hosted_directory.is_some() {
                TerminalPane::with_hosted_attachment
            } else if inline_task {
                TerminalPane::with_inline_task
            } else {
                TerminalPane::with_runtime_embedded
            };
            let (widget, pane) = mount(
                runtime,
                Self::theme(config, config.default_font_scale),
                jterm_core::review_input::safe_inline_display,
                SharedTerminal::links(),
            )?;
            pane.set_history_row_limit(config.terminal_scrollback_lines as usize);
            Ok::<_, String>((
                widget,
                SharedTerminal {
                    find: pane.find_controller(),
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
        let mut detached_style = None;
        let detached_focus = if let Some(live) = &live {
            let stack = gtk4::Stack::new();
            stack.set_hexpand(true);
            stack.set_vexpand(true);
            stack.add_named(&view, Some("attached"));
            let placeholder = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
            placeholder.set_hexpand(true);
            placeholder.set_vexpand(true);
            let content = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
            content.set_halign(gtk4::Align::Center);
            content.set_valign(gtk4::Align::Center);
            content.set_vexpand(true);
            content.append(&gtk4::Label::new(Some("Terminal view detached")));
            let resume = gtk4::Button::with_label("Reattach terminal view");
            content.append(&resume);
            placeholder.append(&content);
            stack.add_named(&placeholder, Some("detached"));
            stack.set_visible_child_name("attached");
            let detach = gtk4::Button::with_label("Detach terminal view");
            detach.set_halign(gtk4::Align::End);
            let style = gtk4::CssProvider::new();
            Self::style_detached_view(&style, config);
            for widget in [
                placeholder.clone().upcast::<gtk4::Widget>(),
                resume.clone().upcast(),
                detach.clone().upcast(),
            ] {
                #[allow(deprecated)]
                widget
                    .style_context()
                    .add_provider(&style, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);
            }
            detached_style = Some(style);
            detach.set_tooltip_text(Some("Release this view while its terminal keeps running. Closing the pane still stops its process."));
            if let Ok(ticket) = live.pane.view_attachment() {
                detach.update_property(&[gtk4::accessible::Property::Description(&format!(
                    "session={} epoch={} generation={}",
                    ticket.session, ticket.epoch, ticket.generation
                ))]);
            } else {
                detach.set_sensitive(false);
            }
            let pane = Rc::downgrade(&live.pane);
            let weak_stack = stack.downgrade();
            let resume_weak = resume.downgrade();
            detach.connect_clicked(move |button| {
                let (Some(pane), Some(stack), Some(resume)) =
                    (pane.upgrade(), weak_stack.upgrade(), resume_weak.upgrade())
                else {
                    return;
                };
                let result = pane
                    .view_attachment()
                    .and_then(|ticket| pane.switch_view_attachment(&ticket, false));
                match result {
                    Ok(ticket) => {
                        resume.update_property(&[gtk4::accessible::Property::Description(
                            &format!(
                                "session={} epoch={} generation={}",
                                ticket.session, ticket.epoch, ticket.generation
                            ),
                        )]);
                        button.set_sensitive(false);
                        stack.set_visible_child_name("detached");
                        resume.grab_focus();
                    }
                    Err(error) => button.set_tooltip_text(Some(&error)),
                }
            });
            let pane = Rc::downgrade(&live.pane);
            let weak_stack = stack.downgrade();
            let detach_weak = detach.downgrade();
            resume.connect_clicked(move |button| {
                let (Some(pane), Some(stack), Some(detach)) =
                    (pane.upgrade(), weak_stack.upgrade(), detach_weak.upgrade())
                else {
                    return;
                };
                let result = pane
                    .view_attachment()
                    .and_then(|ticket| pane.switch_view_attachment(&ticket, true));
                match result {
                    Ok(ticket) => {
                        detach.update_property(&[gtk4::accessible::Property::Description(
                            &format!(
                                "session={} epoch={} generation={}",
                                ticket.session, ticket.epoch, ticket.generation
                            ),
                        )]);
                        stack.set_visible_child_name("attached");
                        detach.set_sensitive(true);
                        pane.focus();
                    }
                    Err(error) => button.set_tooltip_text(Some(&error)),
                }
            });
            root.append(&detach);
            root.append(&stack);
            Some(resume)
        } else {
            root.append(&view);
            None
        };
        crate::eva_companion::tag_pane(&root);
        Some(Rc::new(Self {
            root: root.upcast(),
            header,
            live,
            failure,
            hosted_directory: hosted_directory.map(str::to_owned),
            detached_focus,
            detached_style,
        }))
    }

    pub(crate) fn scaled_font(config: &Config, scale: f64) -> pango::FontDescription {
        let mut font = crate::font::terminal_font_description(&config.font_desc, config);
        if font.size() > 0 && scale.is_finite() && scale > 0.0 {
            let size = (f64::from(font.size()) * scale).round();
            if font.is_size_absolute() {
                font.set_absolute_size(size);
            } else {
                font.set_size(size as i32);
            }
        }
        font
    }

    pub(crate) fn theme(config: &Config, scale: f64) -> Theme {
        Theme {
            font: Self::scaled_font(config, scale),
            foreground: config.foreground,
            background: config.background,
            palette: config.palette,
            cursor: Some(CursorColors {
                foreground: config.cursor_foreground,
                background: config.cursor,
            }),
        }
    }

    fn style_detached_view(style: &gtk4::CssProvider, config: &Config) {
        style.load_from_string(&format!(
            "* {{ background-color: {}; background-image: none; color: {}; }}",
            config.background, config.foreground
        ));
    }

    pub(crate) fn update_theme(&self, config: &Config, scale: f64) {
        if let Some(style) = &self.detached_style {
            Self::style_detached_view(style, config);
        }
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
        if self
            .live
            .as_ref()
            .is_some_and(|live| !live.pane.view_is_attached())
        {
            if let Some(button) = &self.detached_focus {
                return button.clone().upcast();
            }
        }
        self.live.as_ref().map_or_else(
            || self.root.last_child().unwrap_or_else(|| self.root.clone()),
            |live| live.pane.screen().clone().upcast(),
        )
    }

    pub(crate) fn terminal_pane(&self) -> Option<&Rc<TerminalPane<OwnedPty>>> {
        self.live.as_ref().map(|live| &live.pane)
    }

    pub(crate) fn working_directory(&self) -> Option<String> {
        let runtime = self.terminal_pane()?.runtime()?;
        self.hosted_directory
            .clone()
            .or_else(|| runtime.inspect_child(jterm_core::process::process_cwd))
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

    pub(crate) fn find_controller(&self) -> Option<&Rc<jterm_gtk_view::TerminalFind<OwnedPty>>> {
        self.live.as_ref().map(|live| &live.find)
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
