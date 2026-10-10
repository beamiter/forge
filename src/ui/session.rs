//! session — UiState methods extracted from ui (mechanical split, no logic changes)
use gtk4::prelude::*;
use gtk4::ToggleButton;
use std::io;

use super::*;

#[derive(Clone)]
struct RestoredLeafSeed {
    dir: String,
    sid: String,
    cwd_external: bool,
    remote_name: Option<String>,
    custom_title: Option<bool>,
    private_title: Option<bool>,
    cmds: Option<Vec<String>>,
    pinned: Option<bool>,
}

impl RestoredLeafSeed {
    fn from_layout(layout: &crate::state::PaneLayout) -> Option<Self> {
        match layout {
            crate::state::PaneLayout::Leaf {
                dir,
                sid,
                cwd_external,
                remote_name,
                custom_title,
                private_title,
                cmds,
                pinned,
            } => Some(Self {
                dir: dir.clone(),
                sid: sid.clone(),
                cwd_external: *cwd_external,
                remote_name: remote_name.clone(),
                custom_title: *custom_title,
                private_title: *private_title,
                cmds: cmds.clone(),
                pinned: *pinned,
            }),
            crate::state::PaneLayout::Split { start, .. } => Self::from_layout(start),
        }
    }

    fn first_managed(layout: &crate::state::PaneLayout) -> Option<Self> {
        match layout {
            crate::state::PaneLayout::Leaf { remote_name, .. } if remote_name.is_some() => {
                Self::from_layout(layout)
            }
            crate::state::PaneLayout::Leaf { .. } => None,
            crate::state::PaneLayout::Split { start, end, .. } => {
                Self::first_managed(start).or_else(|| Self::first_managed(end))
            }
        }
    }

    fn local_working_directory(&self) -> Option<String> {
        (!self.cwd_external && self.remote_name.is_none()).then(|| self.dir.clone())
    }
}

/// Resolve only the stable profile name from the snapshot. Every mutable
/// transport field comes from the currently validated configuration; the
/// saved jsh id is the sole reconnect datum allowed to override that profile.
fn restored_remote_host(
    hosts: &[crate::config::RemoteHost],
    remote_name: &str,
    session_id: &str,
) -> Option<crate::config::RemoteHost> {
    let mut host = hosts
        .iter()
        .take(crate::config::MAX_REMOTE_HOSTS)
        .find(|host| host.name == remote_name)
        .cloned()?;
    if jterm_core::execution_journal::is_valid_jsh_session_id(session_id) {
        host.session = Some(session_id.to_string());
    }
    Some(host)
}

/// A failed remote gate did not insert a page. Require a newly created local
/// result rather than letting the caller reuse whatever tab was selected.
fn restored_launch_or_local<T>(remote: Option<T>, local: impl FnOnce() -> T) -> (T, bool) {
    match remote {
        Some(terminal) => (terminal, false),
        None => (local(), true),
    }
}

impl UiState {
    /// Recursively restore a pane layout from saved state
    pub(crate) fn restore_pane_layout(
        &self,
        layout: crate::state::PaneLayout,
        tab_name: Option<String>,
    ) -> gtk4::Widget {
        use crate::state::PaneLayout;

        match layout {
            PaneLayout::Leaf {
                dir,
                sid,
                cwd_external,
                remote_name,
                custom_title,
                private_title,
                cmds,
                pinned,
            } => {
                let seed = RestoredLeafSeed {
                    dir,
                    sid,
                    cwd_external,
                    remote_name,
                    custom_title,
                    private_title,
                    cmds,
                    pinned,
                };
                let (terminal, remote_rejected) = if let Some(name) = seed.remote_name.as_deref() {
                    if let Some(host) = self.resolve_restored_remote(name, &seed.sid) {
                        restored_launch_or_local(
                            self.add_restored_remote_tab(&host, seed.sid.clone(), tab_name),
                            || self.add_failed_remote_restore_tab(),
                        )
                    } else {
                        self.show_missing_remote_profile(name);
                        (
                            self.add_new_tab(
                                None,
                                tab_name,
                                Some(seed.sid.clone()),
                                crate::terminal::InitialCommands::default(),
                            ),
                            false,
                        )
                    }
                } else {
                    // Structured unmanaged argv is still replayed through the
                    // configured shell, but an external cwd is never reused on
                    // the local host.
                    let initial_commands = self.restored_initial_commands(seed.cmds.as_deref());
                    (
                        self.add_new_tab(
                            seed.local_working_directory(),
                            tab_name,
                            Some(seed.sid.clone()),
                            initial_commands,
                        ),
                        false,
                    )
                };
                let page = self.restored_page_for_terminal(&terminal);
                if remote_rejected {
                    set_tab_custom_title(&page, true);
                } else {
                    if let Some(custom_title) = seed.custom_title {
                        set_tab_custom_title(&page, custom_title);
                    }
                    if seed.private_title == Some(true) {
                        self.set_tab_title_privacy(&page, true);
                    }
                }
                self.apply_restored_pin(&page, seed.pinned == Some(true));
                page
            }
            PaneLayout::Split {
                orientation,
                position,
                start,
                end,
            } => {
                let layout = PaneLayout::Split {
                    orientation,
                    position,
                    start,
                    end,
                };
                self.restore_split_tab(layout, tab_name)
            }
        }
    }

    /// Restore a split without a throwaway process or a second, partially-wired
    /// strip button. The first saved leaf is launched through normal tab creation;
    /// its real page, header and complete strip wiring are then retained while the
    /// remaining VTE leaves are built around it.
    fn restore_split_tab(
        &self,
        layout: crate::state::PaneLayout,
        tab_name: Option<String>,
    ) -> gtk4::Widget {
        let first = RestoredLeafSeed::from_layout(&layout).expect("split must contain a leaf");
        let managed = RestoredLeafSeed::first_managed(&layout);
        let resolved_remote = managed.as_ref().and_then(|leaf| {
            leaf.remote_name
                .as_deref()
                .and_then(|name| self.resolve_restored_remote(name, &leaf.sid))
                .map(|host| (leaf.clone(), host))
        });
        if let Some(managed) = managed.as_ref() {
            if resolved_remote.is_none() {
                if let Some(name) = managed.remote_name.as_deref() {
                    self.show_missing_remote_profile(name);
                }
            }
        }

        // The one fully wired page can represent a managed remote wherever it
        // sits in the saved split tree. The placeholder is consumed at that
        // exact leaf; local siblings are prepared around it transactionally.
        let (terminal, remote_rejected) = if let Some((leaf, host)) = resolved_remote.as_ref() {
            restored_launch_or_local(
                self.add_restored_remote_tab(host, leaf.sid.clone(), tab_name),
                || self.add_failed_remote_restore_tab(),
            )
        } else {
            let initial_commands = if first.remote_name.is_some() {
                crate::terminal::InitialCommands::default()
            } else {
                self.restored_initial_commands(first.cmds.as_deref())
            };
            (
                self.add_new_tab(
                    first.local_working_directory(),
                    tab_name,
                    Some(first.sid.clone()),
                    initial_commands,
                ),
                false,
            )
        };
        let placeholder_remote_target = resolved_remote
            .as_ref()
            .and_then(|(leaf, _)| leaf.remote_name.clone());

        let first_page = self.restored_page_for_terminal(&terminal);
        let page_num = self
            .notebook
            .page_num(&first_page)
            .expect("the returned restore terminal belongs to its new page");
        if remote_rejected {
            set_tab_custom_title(&first_page, true);
        } else {
            if let Some(custom_title) = first.custom_title {
                set_tab_custom_title(&first_page, custom_title);
            }
            if first.private_title == Some(true) {
                self.set_tab_title_privacy(&first_page, true);
            }
        }
        let custom_title_cell = tab_custom_title_cell(&first_page);
        let private_title_cell = tab_private_title_cell(&first_page);
        let tab_label = self.notebook.tab_label(&first_page);
        let tab_widget_name = first_page.widget_name().to_string();

        // Build every additional leaf around an inert placeholder while the
        // real first page remains attached to the Notebook. Only a completely
        // prepared tree is allowed to replace it.
        let placeholder = gtk4::Box::new(gtk4::Orientation::Vertical, 0).upcast::<gtk4::Widget>();
        // This is only a structural position selector, never transport metadata.
        // Even a rejected remote gets its fresh local fallback at that exact
        // saved leaf, preserving all independently restorable local siblings.
        let mut first_leaf = Some((placeholder_remote_target, placeholder.clone()));
        let mut prepared_leaves = Vec::new();
        let restored = match self.restore_pane_layout_internal(
            layout,
            &mut first_leaf,
            Some(tab_widget_name.clone()),
            &mut prepared_leaves,
        ) {
            Ok(restored) => restored,
            Err(error) => {
                Self::discard_prepared_leaves(prepared_leaves);
                self.report_block_spawn_error(
                    "restoring a split layout",
                    &error,
                    "Restored this tab as a single pane instead.",
                );
                self.apply_restored_pin(&first_page, first.pinned == Some(true));
                return first_page;
            }
        };
        debug_assert!(first_leaf.is_none());

        let Some(parent) = placeholder
            .parent()
            .and_then(|parent| parent.downcast::<gtk4::Paned>().ok())
        else {
            log::error!("restored split tree lost its prepared first-pane slot");
            Self::discard_prepared_leaves(prepared_leaves);
            self.apply_restored_pin(&first_page, first.pinned == Some(true));
            return first_page;
        };
        let replace_start = parent.start_child().as_ref() == Some(&placeholder);
        let replace_end = parent.end_child().as_ref() == Some(&placeholder);
        if !replace_start && !replace_end {
            log::error!("restored split tree first-pane slot has an invalid parent");
            Self::discard_prepared_leaves(prepared_leaves);
            self.apply_restored_pin(&first_page, first.pinned == Some(true));
            return first_page;
        }

        // Commit: detach the live first page only after all fallible Block PTY
        // creation succeeded, then replace the placeholder and insert the full
        // tree back into exactly the same Notebook slot.
        self.notebook.remove_page(Some(page_num));
        if replace_start {
            parent.set_start_child(Some(&first_page));
        } else {
            parent.set_end_child(Some(&first_page));
        }
        restored.set_widget_name(&tab_widget_name);
        if let Some(custom_title) = custom_title_cell {
            attach_tab_custom_title_cell(&restored, custom_title);
        }
        if let Some(private_title) = private_title_cell {
            attach_tab_private_title_cell(&restored, private_title);
        }

        let inserted = self
            .notebook
            .insert_page(&restored, tab_label.as_ref(), Some(page_num));
        self.notebook.set_tab_reorderable(&restored, true);
        self.notebook.set_current_page(Some(inserted));
        self.notebook.set_show_tabs(false);
        self.apply_restored_pin(&restored, first.pinned == Some(true));
        self.sync_tab_strip_active(Some(inserted));
        self.sync_tab_bar_visibility();
        restored
    }

    /// A rejected connection is visibly local and has no saved remote launch
    /// data: neither cwd/commands nor the remote session/history identity.
    fn add_failed_remote_restore_tab(&self) -> vte4::Terminal {
        self.add_new_tab(
            None,
            Some("Local (remote restore failed)".to_string()),
            None,
            crate::terminal::InitialCommands::default(),
        )
    }

    /// Bind the page to the actual constructor result. Unified's compatibility
    /// VTE need not be a visible descendant, so use its attached PaneLeaf's
    /// terminal identity rather than ancestry or current-page selection.
    fn restored_page_for_terminal(&self, terminal: &vte4::Terminal) -> gtk4::Widget {
        (0..self.notebook.n_pages())
            .filter_map(|index| self.notebook.nth_page(Some(index)))
            .find(|page| {
                PaneLeaf::from_widget(page).is_some_and(|leaf| leaf.terminal() == terminal)
            })
            .expect("normal tab creation attached the returned terminal to a new page")
    }

    fn resolve_restored_remote(
        &self,
        remote_name: &str,
        session_id: &str,
    ) -> Option<crate::config::RemoteHost> {
        restored_remote_host(&self.config.borrow().remote_hosts, remote_name, session_id)
    }

    fn show_missing_remote_profile(&self, remote_name: &str) {
        let remote_name = jterm_core::review_input::safe_inline_display(remote_name, 512);
        log::warn!(
            "Managed remote '{remote_name}' is no longer configured; restoring a local shell without replaying stale connection data"
        );
        let toast = adw::Toast::new(&format!(
            "Remote profile “{remote_name}” was removed or renamed; its saved connection was not restored."
        ));
        toast.set_timeout(8);
        self.toast_overlay.add_toast(toast);
    }

    /// Quote a persisted restorable argv for the configured interactive shell,
    /// skipping replay (with a warning) when it cannot be quoted safely.
    fn restored_initial_commands(
        &self,
        argv: Option<&[String]>,
    ) -> crate::terminal::InitialCommands {
        crate::terminal::InitialCommands::from_restored_argv(argv, &self.shell_argv.borrow())
    }

    fn restore_pane_layout_internal(
        &self,
        layout: crate::state::PaneLayout,
        first_leaf: &mut Option<(Option<String>, gtk4::Widget)>,
        tab_widget_name: Option<String>,
        prepared_leaves: &mut Vec<PaneLeaf>,
    ) -> io::Result<gtk4::Widget> {
        use crate::state::PaneLayout;

        match layout {
            PaneLayout::Leaf {
                dir,
                sid,
                cwd_external,
                remote_name,
                custom_title: _,
                private_title: _,
                cmds,
                pinned,
            } => {
                let use_existing = first_leaf.as_ref().is_some_and(|(target_remote, _)| {
                    target_remote
                        .as_deref()
                        .is_none_or(|target| remote_name.as_deref() == Some(target))
                });
                let root = if use_existing {
                    first_leaf.take().expect("existing leaf checked above").1
                } else {
                    // Restored split siblings follow the configured terminal
                    // mode, matching what `split_current` would have created.
                    let mode = self.config.borrow().terminal_mode.clone();
                    // A managed profile that disappeared is restored as an
                    // inert local shell. Its saved cwd belongs to the remote
                    // namespace and its old argv is intentionally absent.
                    let initial_commands = if remote_name.is_some() {
                        crate::terminal::InitialCommands::default()
                    } else {
                        self.restored_initial_commands(cmds.as_deref())
                    };
                    let working_directory =
                        (!cwd_external && remote_name.is_none()).then_some(dir.as_str());
                    let leaf = self.create_pane_leaf(
                        &mode,
                        working_directory,
                        Some(&sid),
                        initial_commands.as_slice(),
                        tab_widget_name,
                    )?;
                    let root = leaf.root_widget();
                    prepared_leaves.push(leaf);
                    root
                };
                if pinned == Some(true) {
                    unsafe {
                        root.set_data::<bool>("pinned", true);
                    }
                }
                Ok(root)
            }
            PaneLayout::Split {
                orientation,
                position,
                start,
                end,
            } => {
                let start_widget = self.restore_pane_layout_internal(
                    *start,
                    first_leaf,
                    tab_widget_name.clone(),
                    prepared_leaves,
                )?;
                let end_widget = self.restore_pane_layout_internal(
                    *end,
                    first_leaf,
                    tab_widget_name,
                    prepared_leaves,
                )?;

                let paned = gtk4::Paned::new(match orientation {
                    'h' => gtk4::Orientation::Horizontal,
                    'v' => gtk4::Orientation::Vertical,
                    _ => gtk4::Orientation::Horizontal,
                });

                paned.set_hexpand(true);
                paned.set_vexpand(true);
                paned.set_start_child(Some(&start_widget));
                paned.set_end_child(Some(&end_widget));
                paned.set_position(position);

                Ok(paned.upcast::<gtk4::Widget>())
            }
        }
    }

    /// Tear down off-notebook leaves built before a restore transaction failed.
    /// `PaneLeaf::attach_to` stores a strong controller on the widget, so the
    /// qdata must be detached explicitly before the temporary tree can release
    /// its PTYs and callbacks.
    fn discard_prepared_leaves(leaves: Vec<PaneLeaf>) {
        for leaf in leaves {
            if let PaneLeaf::Block(view) = &leaf {
                view.suppress_history_persistence();
            }
            let root = leaf.root_widget();
            let _ = PaneLeaf::detach_from(&root);
            leaf.kill();
        }
    }

    fn apply_restored_pin(&self, page: &gtk4::Widget, pinned: bool) {
        Self::set_tab_page_pinned(page, pinned);
        let name = page.widget_name();
        let mut child = self.tab_strip.first_child();
        while let Some(widget) = child {
            if widget.widget_name() == name {
                if let Ok(button) = widget.clone().downcast::<ToggleButton>() {
                    if pinned {
                        button.add_css_class("tab-pinned");
                    } else {
                        button.remove_css_class("tab-pinned");
                    }
                    unsafe {
                        button.set_data::<bool>("pinned", pinned);
                    }
                    Self::set_pin_icon_visible(&button.clone().upcast(), pinned);
                }
                break;
            }
            child = widget.next_sibling();
        }
    }

    fn set_pin_icon_visible(widget: &gtk4::Widget, visible: bool) {
        if let Ok(image) = widget.clone().downcast::<gtk4::Image>() {
            if image.icon_name().as_deref() == Some("bookmark-symbolic") {
                image.set_visible(visible);
            }
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            Self::set_pin_icon_visible(&current, visible);
            child = current.next_sibling();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::restored_remote_host;
    use crate::config::RemoteHost;

    fn profile() -> RemoteHost {
        RemoteHost {
            name: "production".into(),
            host: "new.example.test".into(),
            user: Some("current-user".into()),
            docker: false,
            deploy_artifact: None,
            remote_shell: "/opt/current/jsh".into(),
            session: Some("profile-session".into()),
            ssh_args: vec!["-p".into(), "2222".into()],
            login_shell: true,
            multiplex: true,
            deploy: jterm_core::jsh_remote::Deploy::Off,
        }
    }

    #[test]
    fn rejected_remote_launch_creates_a_fresh_local_result() {
        let calls = std::cell::Cell::new(0);
        let old_selected_page = 7;
        let (created, rejected) = super::restored_launch_or_local(None, || {
            calls.set(calls.get() + 1);
            8
        });
        assert!(rejected);
        assert_ne!(created, old_selected_page);
        assert_eq!(calls.get(), 1);
        let (created, rejected) = super::restored_launch_or_local(Some(9), || {
            panic!("successful remote launch must not create a local shell")
        });
        assert_eq!(created, 9);
        assert!(!rejected);
    }

    #[test]
    fn saved_session_override_can_reject_an_otherwise_valid_remote_profile() {
        let mut current = profile();
        current.session = Some("s".into());
        current.ssh_args = vec![format!("-o{}", "x".repeat(4094)); 64];
        let fixed = current.name.len()
            + current.host.len()
            + current.user.as_deref().map_or(0, str::len)
            + current.remote_shell.len()
            + 1;
        let last = current.ssh_args.last_mut().unwrap();
        last.truncate(last.len() - fixed);
        assert!(crate::config::validate_remote_host(&current).is_ok());
        let restored = restored_remote_host(&[current], "production", "saved-session-7").unwrap();
        assert_eq!(
            crate::config::validate_remote_host(&restored),
            Err("Remote profile exceeds the execution byte budget.")
        );
    }

    #[test]
    fn repeated_profile_names_choose_the_first_managed_leaf_position() {
        use crate::state::PaneLayout;
        fn leaf(remote: Option<&str>, sid: &str) -> PaneLayout {
            PaneLayout::Leaf {
                dir: "/synthetic".into(),
                sid: sid.into(),
                cwd_external: remote.is_some(),
                remote_name: remote.map(str::to_owned),
                custom_title: None,
                private_title: None,
                cmds: None,
                pinned: None,
            }
        }
        let layout = PaneLayout::Split {
            orientation: 'h',
            position: 30,
            start: Box::new(leaf(None, "local")),
            end: Box::new(PaneLayout::Split {
                orientation: 'v',
                position: 40,
                start: Box::new(leaf(Some("production"), "first-remote")),
                end: Box::new(leaf(Some("production"), "second-remote")),
            }),
        };
        let selected = super::RestoredLeafSeed::first_managed(&layout).unwrap();
        assert_eq!(selected.sid, "first-remote");
        assert_eq!(selected.remote_name.as_deref(), Some("production"));
    }

    #[test]
    fn restore_binds_created_terminal_and_keeps_rejected_placeholder_position() {
        let source = include_str!("session.rs");
        let production = source.split_once("\n#[cfg(test)]").unwrap().0;
        assert_eq!(production.matches("restored_launch_or_local(").count(), 2);
        assert!(!production.contains(".current_page()"));
        assert!(production.contains("leaf.terminal() == terminal"));
        let fallback = production
            .split_once("fn add_failed_remote_restore_tab(")
            .unwrap()
            .1
            .split_once("fn restored_page_for_terminal(")
            .unwrap()
            .0;
        assert!(fallback.contains("Local (remote restore failed)"));
        assert!(fallback.contains("InitialCommands::default()"));
        assert_eq!(
            fallback.matches("None,").count(),
            2,
            "no remote cwd or saved session"
        );
        let split = production
            .split_once("fn restore_split_tab(")
            .unwrap()
            .1
            .split_once("fn add_failed_remote_restore_tab(")
            .unwrap()
            .0;
        assert!(split.contains("Some((placeholder_remote_target, placeholder.clone()))"));
        assert!(!split.contains("filter(|_| !remote_rejected)"));
        assert!(split.contains("if remote_rejected {\n            set_tab_custom_title(&first_page, true);\n        } else {"));
        let build = production
            .split_once("fn restore_pane_layout_internal(")
            .unwrap()
            .1;
        assert!(build.contains("first_leaf.take()"));
        assert!(!build.contains("set_managed_remote_name"));
    }

    #[test]
    fn managed_restore_re_resolves_current_profile_and_only_overrides_session() {
        let current = profile();
        let restored = restored_remote_host(
            std::slice::from_ref(&current),
            "production",
            "saved-session-7",
        )
        .unwrap();

        assert_eq!(restored.host, "new.example.test");
        assert_eq!(restored.user.as_deref(), Some("current-user"));
        assert_eq!(restored.remote_shell, "/opt/current/jsh");
        assert_eq!(restored.ssh_args, ["-p", "2222"]);
        assert_eq!(restored.session.as_deref(), Some("saved-session-7"));
        assert!(restored_remote_host(&[current], "removed", "saved-session-7").is_none());

        let mut active = vec![profile(); crate::config::MAX_REMOTE_HOSTS];
        for (index, host) in active.iter_mut().enumerate() {
            host.name = format!("active-{index}");
        }
        active.push(profile());
        assert!(
            restored_remote_host(&active, "production", "saved-session-7").is_none(),
            "workspace restore must not reactivate profile 129"
        );
    }
}
