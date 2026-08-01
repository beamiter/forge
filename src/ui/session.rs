//! session — UiState methods extracted from ui (mechanical split, no logic changes)
use gtk4::prelude::*;
use gtk4::ToggleButton;
use std::io;

use super::*;

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
                cmds,
                pinned,
            } => {
                // Create a simple tab with the leaf layout. The persisted argv
                // becomes exactly one safely quoted command for the configured
                // shell; unsafe argvs skip replay instead of being reparsed.
                let initial_commands = self.restored_initial_commands(cmds.as_deref());
                let _terminal = self.add_new_tab(Some(dir), tab_name, Some(sid), initial_commands);
                // Return the page widget (last added page)
                let page_num = self.notebook.n_pages().saturating_sub(1);
                let page = self
                    .notebook
                    .nth_page(Some(page_num))
                    .expect("Just added a page");
                self.apply_restored_pin(&page, pinned == Some(true));
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
        let (dir, sid, cmds, pinned) = Self::first_saved_leaf(&layout);
        let initial_commands = self.restored_initial_commands(cmds.as_deref());
        self.add_new_tab(
            Some(dir.to_string()),
            tab_name,
            Some(sid.to_string()),
            initial_commands,
        );

        let page_num = self
            .notebook
            .current_page()
            .expect("normal tab creation selects the restored page");
        let first_page = self
            .notebook
            .nth_page(Some(page_num))
            .expect("normal tab creation inserted a page");
        let tab_label = self.notebook.tab_label(&first_page);
        let tab_widget_name = first_page.widget_name().to_string();

        // Build every additional leaf around an inert placeholder while the
        // real first page remains attached to the Notebook. Only a completely
        // prepared tree is allowed to replace it.
        let placeholder = gtk4::Box::new(gtk4::Orientation::Vertical, 0).upcast::<gtk4::Widget>();
        let mut first_leaf = Some(placeholder.clone());
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
                self.apply_restored_pin(&first_page, pinned == Some(true));
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
            self.apply_restored_pin(&first_page, pinned == Some(true));
            return first_page;
        };
        let replace_start = parent.start_child().as_ref() == Some(&placeholder);
        let replace_end = parent.end_child().as_ref() == Some(&placeholder);
        if !replace_start && !replace_end {
            log::error!("restored split tree first-pane slot has an invalid parent");
            Self::discard_prepared_leaves(prepared_leaves);
            self.apply_restored_pin(&first_page, pinned == Some(true));
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

        let inserted = self
            .notebook
            .insert_page(&restored, tab_label.as_ref(), Some(page_num));
        self.notebook.set_tab_reorderable(&restored, true);
        self.notebook.set_current_page(Some(inserted));
        self.notebook.set_show_tabs(false);
        self.apply_restored_pin(&restored, pinned == Some(true));
        self.sync_tab_strip_active(Some(inserted));
        self.sync_tab_bar_visibility();
        restored
    }

    fn first_saved_leaf(
        layout: &crate::state::PaneLayout,
    ) -> (&str, &str, &Option<Vec<String>>, Option<bool>) {
        match layout {
            crate::state::PaneLayout::Leaf {
                dir,
                sid,
                cmds,
                pinned,
            } => (dir, sid, cmds, *pinned),
            crate::state::PaneLayout::Split { start, .. } => Self::first_saved_leaf(start),
        }
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
        first_leaf: &mut Option<gtk4::Widget>,
        tab_widget_name: Option<String>,
        prepared_leaves: &mut Vec<PaneLeaf>,
    ) -> io::Result<gtk4::Widget> {
        use crate::state::PaneLayout;

        match layout {
            PaneLayout::Leaf {
                dir,
                sid,
                cmds,
                pinned,
            } => {
                let root = if let Some(existing) = first_leaf.take() {
                    existing
                } else {
                    // Restored split siblings follow the configured terminal
                    // mode, matching what `split_current` would have created.
                    let mode = self.config.borrow().terminal_mode.clone();
                    let initial_commands = self.restored_initial_commands(cmds.as_deref());
                    let leaf = self.create_pane_leaf(
                        &mode,
                        Some(&dir),
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
