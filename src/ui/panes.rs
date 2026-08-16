//! panes — UiState methods extracted from ui (mechanical split, no logic changes)
use gtk4::prelude::*;
use gtk4::{Orientation, Paned};
use libadwaita as adw;
use std::io;
use std::rc::Rc;

use super::pane_dnd::{
    split_placement, tab_split_drop_allowed, unique_session_index, SplitAxis, SplitDropZone,
};
use super::*;
use crate::block_view::TermView;
use crate::keybindings::Direction;
use crate::state::generate_session_id;
use crate::terminal::{setup_terminal_click_handler, terminal_working_directory, VteTerminalView};
use vte4::TerminalExt as _;

#[derive(Clone)]
struct PaneLocation {
    page: gtk4::Widget,
    leaf: PaneLeaf,
}

/// Number of equal-size pane slots a subtree occupies along one axis.
///
/// A split on the other axis stacks its children instead of consuming more
/// space on this axis, so only the widest/tallest child determines its span.
/// This lets a mixed 2x2 tree balance like a grid while repeated same-axis
/// splits receive one equal slot per leaf instead of 1/2, 1/4, 1/8… widths.
fn pane_axis_span(widget: &gtk4::Widget, axis: Orientation) -> u32 {
    let Ok(paned) = widget.clone().downcast::<Paned>() else {
        return 1;
    };
    let Some(start) = paned.start_child() else {
        return 1;
    };
    let Some(end) = paned.end_child() else {
        return 1;
    };
    let start_span = pane_axis_span(&start, axis);
    let end_span = pane_axis_span(&end, axis);
    if paned.orientation() == axis {
        start_span.saturating_add(end_span)
    } else {
        start_span.max(end_span)
    }
}

fn balanced_split_position(extent: i32, start_span: u32, end_span: u32) -> Option<i32> {
    if extent <= 1 || start_span == 0 || end_span == 0 {
        return None;
    }
    let total_span = u64::from(start_span) + u64::from(end_span);
    let position = i64::from(extent) * i64::from(start_span) / total_span as i64;
    Some(position.clamp(1, i64::from(extent - 1)) as i32)
}

fn nearest_directional_index(
    centers: &[(f32, f32)],
    focused: usize,
    direction: Direction,
) -> Option<usize> {
    let (focused_x, focused_y) = *centers.get(focused)?;
    centers
        .iter()
        .enumerate()
        .filter_map(|(index, &(x, y))| {
            if index == focused {
                return None;
            }
            let dx = x - focused_x;
            let dy = y - focused_y;
            let in_direction = match direction {
                Direction::Left => dx < -1.0,
                Direction::Right => dx > 1.0,
                Direction::Up => dy < -1.0,
                Direction::Down => dy > 1.0,
            };
            if !in_direction {
                return None;
            }
            let distance = match direction {
                Direction::Left | Direction::Right => dx.abs() + dy.abs() * 0.1,
                Direction::Up | Direction::Down => dy.abs() + dx.abs() * 0.1,
            };
            Some((index, distance))
        })
        .min_by(|(_, left), (_, right)| left.total_cmp(right))
        .map(|(index, _)| index)
}

/// Run all fallible pane preparation before allowing any structural mutation.
/// Keeping this boundary explicit makes split's no-partial-commit guarantee
/// independently testable with an injected constructor failure.
fn prepare_then_commit<T, E>(
    prepare: impl FnOnce() -> Result<T, E>,
    commit: impl FnOnce(T),
) -> Result<(), E> {
    let prepared = prepare()?;
    commit(prepared);
    Ok(())
}

/// Rebalance every split according to the number of pane slots below it.
///
/// GTK Paned defaults each newly nested split to 50/50. Repeatedly splitting
/// the newest pane therefore leaves the first pane at half the window and
/// squeezes every later sibling into the remaining half. Recomputing the
/// proportions from subtree spans gives three same-axis panes 1/3 each, four
/// panes 1/4 each, and sensible dimensions for mixed-axis grids.
fn rebalance_pane_tree(widget: &gtk4::Widget) {
    let Ok(paned) = widget.clone().downcast::<Paned>() else {
        return;
    };
    let Some(start) = paned.start_child() else {
        return;
    };
    let Some(end) = paned.end_child() else {
        return;
    };
    let axis = paned.orientation();
    let extent = if axis == Orientation::Horizontal {
        paned.width()
    } else {
        paned.height()
    };
    let start_span = pane_axis_span(&start, axis);
    let end_span = pane_axis_span(&end, axis);
    if let Some(position) = balanced_split_position(extent, start_span, end_span) {
        paned.set_position(position);
    }
    rebalance_pane_tree(&start);
    rebalance_pane_tree(&end);
}

fn schedule_pane_rebalance(page: gtk4::Widget) {
    // The first idle runs after the new Paned enters the widget tree. A second
    // pass catches nested panes whose allocation changes because an ancestor
    // divider moved during the first pass.
    gtk4::glib::idle_add_local_once(move || {
        rebalance_pane_tree(&page);
        let page = page.clone();
        gtk4::glib::idle_add_local_once(move || {
            rebalance_pane_tree(&page);
        });
    });
}

impl UiState {
    /// Preserve the full spawn diagnostic in logs and give the user an immediate
    /// explanation of the safe recovery chosen by the calling transaction.
    pub(crate) fn report_block_spawn_error(
        &self,
        context: &str,
        error: &io::Error,
        recovery: &str,
    ) {
        log::error!("Block PTY spawn failed while {context}: {error:?}");
        let toast = adw::Toast::new(&format!(
            "Block terminal could not start: {error}. {recovery}"
        ));
        toast.set_timeout(8);
        self.toast_overlay.add_toast(toast);
    }

    /// Create a managed conventional-VTE pane leaf.
    ///
    /// Runtime splits and restored split layouts share this constructor so every
    /// leaf root stores a `PaneLeaf` controller. This keeps process callbacks and
    /// GTK object ownership attached to the same widget that enters the pane tree.
    pub(crate) fn create_vte_leaf(
        &self,
        working_directory: Option<&str>,
        session_id: Option<&str>,
        initial_commands: &[String],
        tab_widget_name: Option<String>,
    ) -> PaneLeaf {
        let sid = session_id
            .filter(|sid| jterm_core::execution_journal::is_valid_jsh_session_id(sid))
            .map(str::to_owned)
            .unwrap_or_else(generate_session_id);
        let shell_argv = self.shell_argv.borrow();
        let view = Rc::new(VteTerminalView::new(
            self.config.clone(),
            shell_argv.as_slice(),
            working_directory,
            Some(&sid),
            initial_commands,
        ));
        drop(shell_argv);

        let terminal = view.vte().clone();
        setup_terminal_click_handler(&terminal);
        self.setup_context_menu(&terminal);

        let ui_for_exit = UiState::clone(self);
        let root_for_exit = view.widget().downgrade();
        view.connect_exited(move |_| {
            if let Some(root) = root_for_exit.upgrade() {
                ui_for_exit.handle_terminal_exited(&root);
            }
        });

        let leaf = PaneLeaf::Vte(view);
        let root = leaf.root_widget();
        if let Some(name) = tab_widget_name.as_deref() {
            root.set_widget_name(name);
        }
        leaf.attach_to(&root);
        leaf.set_session_id(&sid);
        leaf.set_remote(false);
        self.install_pane_rearrange(&leaf);
        if tab_widget_name.is_some() {
            let ui_for_bell = self.clone();
            let root_for_bell = root.downgrade();
            if let PaneLeaf::Vte(view) = &leaf {
                view.connect_bell(move || {
                    log::debug!("Bell signal received (split)");
                    if let Some(root) = root_for_bell.upgrade() {
                        ui_for_bell.mark_tab_bell(&root.widget_name());
                    }
                });
            }

            let ui_for_activity = self.clone();
            let root_for_activity = root.downgrade();
            if let PaneLeaf::Vte(view) = &leaf {
                view.connect_activity(move || {
                    if let Some(root) = root_for_activity.upgrade() {
                        ui_for_activity.mark_tab_activity(&root.widget_name());
                    }
                });
            }
        }
        leaf
    }

    /// Create a managed `TermView` pane leaf in `mode` (Block or Unified).
    ///
    /// Mirrors `create_vte_leaf` so Block tabs split into Block panes instead of
    /// silently downgrading the new pane to the conventional VTE backend. The
    /// mode is passed down to `TermView::new` rather than re-read from the
    /// shared config, so a caller that pinned Block keeps Block.
    pub(crate) fn create_block_leaf(
        &self,
        mode: &crate::config::TerminalMode,
        working_directory: Option<&str>,
        session_id: Option<&str>,
        initial_commands: &[String],
        tab_widget_name: Option<String>,
    ) -> io::Result<PaneLeaf> {
        let sid = session_id
            .filter(|sid| jterm_core::execution_journal::is_valid_jsh_session_id(sid))
            .map(str::to_owned)
            .unwrap_or_else(generate_session_id);
        let shell_argv = self.shell_argv.borrow();
        let view = Rc::new(TermView::new(
            &self.config.borrow(),
            mode,
            shell_argv.as_slice(),
            working_directory,
            Some(&sid),
            initial_commands,
        )?);
        drop(shell_argv);
        view.start_history_load();

        let terminal = view.vte().clone();
        setup_terminal_click_handler(&terminal);
        self.setup_context_menu(&terminal);

        let ui_for_exit = UiState::clone(self);
        let view_for_exit = Rc::downgrade(&view);
        let root_for_exit = view.widget().downgrade();
        view.connect_exited(move |_| {
            let Some(view) = view_for_exit.upgrade() else {
                return;
            };
            let Some(root) = root_for_exit.upgrade() else {
                return;
            };
            let _ = view.save_history();
            ui_for_exit.handle_terminal_exited(&root);
        });

        self.connect_block_command_history(&view);
        self.connect_block_ai_action(&view);
        self.connect_bottom_bar_block_status(&view);
        self.attach_ascii_organism_to_view(&view, false);
        // A nested split does not add a Notebook page, so attach the per-pane
        // correction request epoch here instead of relying solely on page-added.
        self.attach_command_correction_to_view(view.clone(), false);

        let leaf = PaneLeaf::Block(view);
        let root = leaf.root_widget();
        if let Some(name) = tab_widget_name.as_deref() {
            root.set_widget_name(name);
        }
        leaf.attach_to(&root);
        leaf.set_session_id(&sid);
        leaf.set_remote(false);
        self.install_pane_rearrange(&leaf);
        if tab_widget_name.is_some() {
            if let PaneLeaf::Block(view) = &leaf {
                self.connect_block_tab_attention(view, &root);
                let ui_for_bell = self.clone();
                let root_for_bell = root.downgrade();
                view.connect_bell(move || {
                    log::debug!("Bell signal received (split)");
                    if let Some(root) = root_for_bell.upgrade() {
                        ui_for_bell.mark_tab_bell(&root.widget_name());
                    }
                });

                let ui_for_activity = self.clone();
                let root_for_activity = root.downgrade();
                view.connect_activity(move || {
                    if let Some(root) = root_for_activity.upgrade() {
                        ui_for_activity.mark_tab_activity(&root.widget_name());
                    }
                });
            }
        }
        Ok(leaf)
    }

    /// Append finished Block commands to the cross-session command history.
    /// Shared by tab-level Block views and Block split leaves.
    pub(crate) fn connect_block_command_history(&self, view: &Rc<TermView>) {
        let config_for_history = self.config.clone();
        let view_for_history = Rc::downgrade(view);
        view.connect_block_finished(move |command, exit_code, _agent_generation, _duration_ms| {
            let config = config_for_history.borrow();
            if !config.command_history_enabled {
                return;
            }
            let Some(path) = config.command_history_path.as_deref() else {
                return;
            };
            let cwd = view_for_history
                .upgrade()
                .map(|view| view.cwd())
                .filter(|cwd| !cwd.is_empty());
            let end_time_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|duration| u64::try_from(duration.as_millis()).ok());
            // The family's history JSONL is shared with jsh and the other
            // terminals, and its schema has a plain exit_code — so a status the
            // shell never reported is recorded as the sentinel rather than as a
            // successful 0.
            let (exit_code, _) = crate::block_view::exit_code_for_shared_surface(exit_code);
            if let Err(err) = jterm_core::command_history::enqueue(
                std::path::Path::new(path),
                config.command_history_max_entries as usize,
                &command,
                cwd.as_deref(),
                exit_code,
                end_time_ms,
            ) {
                log::warn!("failed to append command history: {err}");
            }
        });
    }

    /// Create a split/restore pane leaf matching the requested terminal mode.
    pub(crate) fn create_pane_leaf(
        &self,
        mode: &crate::config::TerminalMode,
        working_directory: Option<&str>,
        session_id: Option<&str>,
        initial_commands: &[String],
        tab_widget_name: Option<String>,
    ) -> io::Result<PaneLeaf> {
        match mode {
            // Unified is a render backend inside `TermView`, not a separate
            // leaf: the pane tree, headers and drag/drop are identical, so the
            // requested mode travels on into `TermView::new` and picks the
            // backend there.
            crate::config::TerminalMode::Block | crate::config::TerminalMode::Unified => self
                .create_block_leaf(
                    mode,
                    working_directory,
                    session_id,
                    initial_commands,
                    tab_widget_name,
                ),
            crate::config::TerminalMode::Vte => Ok(self.create_vte_leaf(
                working_directory,
                session_id,
                initial_commands,
                tab_widget_name,
            )),
        }
    }

    /// Make one leaf draggable by its header and droppable as a swap target.
    ///
    /// Both sides read the pane's live session id at gesture time rather than
    /// capturing it: restore can reassign a leaf's id after construction.
    pub(crate) fn install_pane_rearrange(&self, leaf: &PaneLeaf) {
        let ui = self.clone();
        let target_root = leaf.root_widget().downgrade();
        leaf.install_pane_drag(move |dragged| {
            let Some(target) = target_root
                .upgrade()
                .and_then(|root| PaneLeaf::from_widget(&root))
                .and_then(|leaf| leaf.session_id())
            else {
                return false;
            };
            ui.swap_panes_by_session(dragged, &target)
        });

        let ui = self.clone();
        let target_root = leaf.root_widget().downgrade();
        leaf.install_tab_split_drop(move |dragged, zone| {
            let Some(target) = target_root
                .upgrade()
                .and_then(|root| PaneLeaf::from_widget(&root))
                .and_then(|leaf| leaf.session_id())
            else {
                return false;
            };
            ui.move_plain_tab_to_split(dragged, &target, zone)
        });
    }

    fn pane_locations(&self) -> Vec<PaneLocation> {
        let mut locations = Vec::new();
        for index in 0..self.notebook.n_pages() {
            let Some(page) = self.notebook.nth_page(Some(index)) else {
                continue;
            };
            let Some(node) = PaneNode::from_widget(&page) else {
                continue;
            };
            locations.extend(node.leaves().into_iter().map(|leaf| PaneLocation {
                page: page.clone(),
                leaf,
            }));
        }
        locations
    }

    fn pane_location_by_session(&self, session_id: &str) -> Option<PaneLocation> {
        let locations = self.pane_locations();
        let index = unique_session_index(
            locations.iter().map(|location| location.leaf.session_id()),
            session_id,
        )?;
        locations.get(index).cloned()
    }

    /// Move one existing ordinary tab beside a live target pane.
    ///
    /// Every identity, parent slot, page index, and connection-map conflict is
    /// resolved before the source page is detached. The commit then reparents
    /// the existing `PaneLeaf` root without touching its controller, PTY, shell,
    /// scrollback, or session id.
    pub(crate) fn move_plain_tab_to_split(
        &self,
        dragged_session: &str,
        target_session: &str,
        zone: SplitDropZone,
    ) -> bool {
        // A content drop is a new phase of the gesture even if GTK omitted the
        // preceding tab-button leave. No delayed hover callback may outlive it.
        self.invalidate_tab_drag_hover();
        let Some(source) = self.pane_location_by_session(dragged_session) else {
            return false;
        };
        let Some(target) = self.pane_location_by_session(target_session) else {
            return false;
        };
        let Some(source_node) = PaneNode::from_widget(&source.page) else {
            return false;
        };

        let source_name = source.page.widget_name().to_string();
        let target_name = target.page.widget_name().to_string();
        let Some(source_tab_num) = source_name
            .strip_prefix("tab-")
            .and_then(|number| number.parse::<u32>().ok())
        else {
            return false;
        };
        let Some(target_tab_num) = target_name
            .strip_prefix("tab-")
            .and_then(|number| number.parse::<u32>().ok())
        else {
            return false;
        };
        let Some(source_page_index) = self.notebook.page_num(&source.page) else {
            return false;
        };
        let Some(plan) =
            plan_existing_leaf_split(&self.notebook, &target.page, &target.leaf.root_widget())
        else {
            return false;
        };
        let plan = plan.after_removing_page(source_page_index);

        // A tab can currently represent one reconnecting remote pane. Refuse
        // the only lossy case: merging two tabs that both own such a record.
        let source_connection = self.tab_connections.borrow().get(&source_tab_num).cloned();
        let connection_conflict = source_connection.is_some()
            && self.tab_connections.borrow().contains_key(&target_tab_num);
        if !tab_split_drop_allowed(
            !source_node.is_split() && source.leaf.root_widget() == source.page,
            dragged_session == target_session,
            source.page == target.page,
            self.zoom_state.borrow().is_some(),
            connection_conflict,
        ) {
            return false;
        }

        let target_pinned = self.tab_page_is_pinned(&target.page);
        let target_custom_title = tab_custom_title_cell(&target.page);
        let target_private_title = tab_private_title_cell(&target.page);
        let (axis, incoming_first) = split_placement(zone);
        let orientation = match axis {
            SplitAxis::Horizontal => Orientation::Horizontal,
            SplitAxis::Vertical => Orientation::Vertical,
        };
        let source_root = source.leaf.root_widget();

        // Commit. Notebook persistence callbacks defer to idle, so observers
        // see only the final tree after these synchronous GTK operations.
        if let Some(root) = source.page.root() {
            root.set_focus(None::<&gtk4::Widget>);
        }
        self.remove_strip_button_for(&source.page);
        self.selected_tabs
            .borrow_mut()
            .retain(|name| name != &source_name);
        self.session_ids.borrow_mut().remove(&source_tab_num);
        let moved_connection = self.tab_connections.borrow_mut().remove(&source_tab_num);
        self.notebook.remove_page(Some(source_page_index));

        source_root.set_widget_name(&target_name);
        let page = plan.commit(&self.notebook, &source_root, orientation, incoming_first);
        // A direct target page is now a new `Paned`; normalize both that page
        // root and every retained/moved leaf before sorting or persistence can
        // observe the replacement.
        Self::set_tab_page_pinned(&page, target_pinned);
        if let Some(custom_title) = target_custom_title {
            attach_tab_custom_title_cell(&page, custom_title);
        }
        if let Some(private_title) = target_private_title {
            attach_tab_private_title_cell(&page, private_title);
        }
        if let Some(connection) = moved_connection {
            let status = connection.status;
            self.tab_connections
                .borrow_mut()
                .insert(target_tab_num, connection);
            self.set_tab_conn_status(target_tab_num, status);
        }

        schedule_pane_rebalance(page.clone());
        self.notebook
            .set_current_page(self.notebook.page_num(&page));
        self.sync_tab_strip_active(self.notebook.page_num(&page));
        self.sync_tab_bar_visibility();
        self.refresh_pane_headers_for(&page);
        source.leaf.grab_focus();
        if let Some(page_num) = self.notebook.page_num(&page) {
            self.request_tab_terminal_focus(source.leaf.terminal().clone(), page_num);
        }
        // Drag-end normally restores the page that was active before a hover
        // preview. This transaction is the one exception: its new split page
        // and moved terminal are now the intentional destination.
        self.commit_tab_split_drag(dragged_session);
        true
    }

    /// Detach exactly one split leaf into a normal tab by stable session id.
    pub(crate) fn move_pane_to_new_tab_by_session(&self, session_id: &str) -> bool {
        if self.zoom_state.borrow().is_some() {
            return false;
        }
        let Some(location) = self.pane_location_by_session(session_id) else {
            return false;
        };
        let Some(node) = PaneNode::from_widget(&location.page) else {
            return false;
        };
        if !node.is_split() || location.leaf.root_widget().parent().is_none() {
            return false;
        }

        let source_pinned = self.tab_page_is_pinned(&location.page);
        let source_private_title =
            tab_private_title_cell(&location.page).unwrap_or_else(|| Rc::new(Cell::new(false)));
        let working_directory = self.pane_working_directory(&location.leaf);
        let source_page_name = location.page.widget_name().to_string();
        let Some(sibling) = detach_leaf_and_promote(&self.notebook, &location.leaf.root_widget())
        else {
            return false;
        };
        if let Some(source_page) = (0..self.notebook.n_pages()).find_map(|index| {
            self.notebook
                .nth_page(Some(index))
                .filter(|page| page.widget_name().as_str() == source_page_name)
        }) {
            Self::set_tab_page_pinned(&source_page, source_pinned);
            attach_tab_private_title_cell(&source_page, source_private_title.clone());
            self.refresh_pane_headers_for(&source_page);
        } else {
            // Defensive fallback for an embedding tree without a conventional
            // tab name; the promoted subtree is still internally coherent.
            Self::set_tab_page_pinned(&sibling, source_pinned);
            attach_tab_private_title_cell(&sibling, source_private_title.clone());
            self.refresh_pane_headers_for(&sibling);
        }
        Self::set_tab_page_pinned(&location.leaf.root_widget(), source_pinned);
        attach_tab_private_title_cell(
            &location.leaf.root_widget(),
            Rc::new(Cell::new(source_private_title.get())),
        );
        self.add_pane_leaf_as_new_tab(location.leaf, working_directory);
        true
    }

    /// Bring the current tab's pane headers up to date: visibility, numbering,
    /// focus highlight, and the title / directory / running-command line.
    ///
    /// A tab with a single pane hides its header entirely — the tab strip and
    /// window title already name it, and the strip would only cost a row.
    /// Background tabs are not rendered, so their PTYs are left alone.
    pub(crate) fn refresh_pane_headers(&self) {
        let Some(page_widget) = self
            .notebook
            .current_page()
            .and_then(|page| self.notebook.nth_page(Some(page)))
        else {
            return;
        };
        self.refresh_pane_headers_for(&page_widget);
    }

    /// Refresh one specific page's headers. `switch-page` fires before the
    /// notebook's current page has moved, so the new tab must be named
    /// explicitly rather than looked up.
    pub(crate) fn refresh_pane_headers_for(&self, page_widget: &gtk4::Widget) {
        let Some(node) = PaneNode::from_widget(page_widget) else {
            return;
        };
        // `leaves()` walks the tree start-child first, so its order is the
        // order the user sees. A swap changes it; a creation-order list would
        // not, and the numbers would stop matching the layout.
        let leaves = node.leaves();
        let split = leaves.len() > 1;
        let focused_root = node.active_leaf().map(|leaf| leaf.root_widget());
        for (position, leaf) in leaves.iter().enumerate() {
            let header = leaf.pane_header();
            header.set_header_visible(split);
            header.set_focused(focused_root.as_ref() == Some(&leaf.root_widget()));
            if !split {
                continue;
            }
            let cwd = self.pane_working_directory(leaf);
            let title = super::pane_header::pane_header_title(
                self.pane_title(leaf).as_deref(),
                cwd.as_deref(),
                position,
            );
            header.set_status(
                position,
                &title,
                cwd.as_deref()
                    .map(super::pane_header::abbreviate_home)
                    .as_deref(),
                leaf.foreground_process_name().as_deref(),
            );
        }
    }

    /// A pane's working directory. Block views track it themselves because
    /// their PTY is not owned by the live VTE, so their own record outranks
    /// VTE's OSC 7 / child-pid inspection.
    pub(crate) fn pane_working_directory(&self, leaf: &PaneLeaf) -> Option<String> {
        leaf.block_view()
            .map(|view| view.cwd())
            .filter(|cwd| !cwd.is_empty())
            .or_else(|| terminal_working_directory(leaf.terminal()))
    }

    /// The OSC title this pane's terminal last reported, if any.
    fn pane_title(&self, leaf: &PaneLeaf) -> Option<String> {
        leaf.terminal()
            .window_title()
            .map(|title| title.to_string())
            .filter(|title| !title.trim().is_empty())
    }

    /// Exchange two panes' positions in the current tab's split tree after a
    /// header drag. Only the panes move: the tree shape and every divider
    /// position the user arranged stay exactly as they were.
    pub(crate) fn swap_panes_by_session(&self, dragged: &str, target: &str) -> bool {
        if dragged == target {
            return false;
        }
        let Some(page_widget) = self
            .notebook
            .current_page()
            .and_then(|page| self.notebook.nth_page(Some(page)))
        else {
            return false;
        };
        let Some(node) = PaneNode::from_widget(&page_widget) else {
            return false;
        };
        let leaves = node.leaves();
        let find = |session: &str| {
            let index = unique_session_index(leaves.iter().map(PaneLeaf::session_id), session)?;
            leaves.get(index).cloned()
        };
        // A drop from another tab would have to move a pane between two page
        // trees and two tab identities; refuse rather than half-apply it.
        let (Some(dragged_leaf), Some(target_leaf)) = (find(dragged), find(target)) else {
            return false;
        };
        if !super::pane_header::swap_pane_widgets(
            &dragged_leaf.root_widget(),
            &target_leaf.root_widget(),
        ) {
            return false;
        }
        // Focus follows the dragged pane into its new slot.
        dragged_leaf.grab_focus();
        self.refresh_pane_headers();
        true
    }

    pub(crate) fn split_current(&self, orientation: Orientation) {
        let Some(page_num) = self.notebook.current_page() else {
            return;
        };
        let Some(page_widget) = self.notebook.nth_page(Some(page_num)) else {
            return;
        };
        let Some(page_node) = PaneNode::from_widget(&page_widget) else {
            return;
        };

        let Some(current_leaf) = page_node.active_leaf() else {
            return;
        };
        let current_term = current_leaf.terminal().clone();
        // Block views track cwd themselves (their PTY is not owned by the live
        // VTE), so prefer that over VTE's OSC 7 / child-pid inspection.
        let working_directory = current_leaf
            .block_view()
            .map(|view| view.cwd())
            .filter(|cwd| !cwd.is_empty())
            .or_else(|| terminal_working_directory(&current_term));
        let tab_widget_name = Some(page_widget.widget_name().to_string());
        let pinned = self.tab_page_is_pinned(&page_widget);
        let custom_title = tab_custom_title_cell(&page_widget);
        let private_title = tab_private_title_cell(&page_widget);
        let current_widget = current_leaf.root_widget();
        let parent = current_widget.parent();

        // The new pane inherits the backend of the pane being split so a Block
        // tab splits into Block panes rather than a conventional VTE sibling.
        // Block and Unified share the leaf type; which of the two the new
        // `TermView` renders with follows the shared config, exactly as for a
        // new tab, and is honoured by `TermView::new` because it is passed
        // down rather than re-derived.
        let split_mode = if current_leaf.is_block() {
            let configured = self.config.borrow().terminal_mode.clone();
            if configured.uses_term_view() {
                configured
            } else {
                crate::config::TerminalMode::Block
            }
        } else {
            crate::config::TerminalMode::Vte
        };
        let split = prepare_then_commit(
            || {
                self.create_pane_leaf(
                    &split_mode,
                    working_directory.as_deref(),
                    None,
                    &[],
                    tab_widget_name,
                )
            },
            |new_leaf| {
                let new_widget = new_leaf.root_widget();

                let paned = Paned::new(orientation);
                paned.set_hexpand(true);
                paned.set_vexpand(true);
                paned.set_resize_start_child(true);
                paned.set_resize_end_child(true);
                paned.set_shrink_start_child(true);
                paned.set_shrink_end_child(true);

                let current_extent = if orientation == Orientation::Horizontal {
                    current_widget.width()
                } else {
                    current_widget.height()
                };
                if let Some(position) = balanced_split_position(current_extent, 1, 1) {
                    paned.set_position(position);
                }

                if let Some(ref parent) = parent {
                    if let Ok(parent_paned) = parent.clone().downcast::<Paned>() {
                        let is_start = parent_paned.start_child().as_ref() == Some(&current_widget);
                        if is_start {
                            parent_paned.set_start_child(Some(&paned));
                        } else {
                            parent_paned.set_end_child(Some(&paned));
                        }
                        paned.set_start_child(Some(&current_widget));
                        paned.set_end_child(Some(&new_widget));
                    } else {
                        for index in 0..self.notebook.n_pages() {
                            if let Some(candidate) = self.notebook.nth_page(Some(index)) {
                                if candidate == current_widget {
                                    paned.set_widget_name(&candidate.widget_name());
                                    if let Some(custom_title) = custom_title.clone() {
                                        attach_tab_custom_title_cell(
                                            &paned.clone().upcast(),
                                            custom_title,
                                        );
                                    }
                                    if let Some(private_title) = private_title.clone() {
                                        attach_tab_private_title_cell(
                                            &paned.clone().upcast(),
                                            private_title,
                                        );
                                    }
                                    let tab_label = self.notebook.tab_label(&candidate);
                                    self.notebook.remove_page(Some(index));
                                    paned.set_start_child(Some(&current_widget));
                                    paned.set_end_child(Some(&new_widget));
                                    let inserted = self.notebook.insert_page(
                                        &paned,
                                        tab_label.as_ref(),
                                        Some(index),
                                    );
                                    self.notebook.set_tab_reorderable(&paned, true);
                                    self.notebook.set_current_page(Some(inserted));
                                    break;
                                }
                            }
                        }
                    }
                }

                if let Some(page) = self
                    .notebook
                    .current_page()
                    .and_then(|page| self.notebook.nth_page(Some(page)))
                {
                    Self::set_tab_page_pinned(&page, pinned);
                    schedule_pane_rebalance(page);
                    // The tab just became split, so every pane's header appears now.
                    self.refresh_pane_headers();
                }
                new_leaf.grab_focus();
                if let Some(page_num) = self.notebook.current_page() {
                    self.request_tab_terminal_focus(new_leaf.terminal().clone(), page_num);
                }
            },
        );
        if let Err(error) = split {
            self.report_block_spawn_error(
                "splitting a pane",
                &error,
                "The existing pane layout was left unchanged.",
            );
        }
    }

    pub(crate) fn cycle_pane_focus(&self, direction: i32) {
        let Some(page_num) = self.notebook.current_page() else {
            return;
        };
        let Some(widget) = self.notebook.nth_page(Some(page_num)) else {
            return;
        };
        let Some(node) = PaneNode::from_widget(&widget) else {
            return;
        };
        let leaves = node.leaves();
        if leaves.len() <= 1 {
            return;
        }

        let focused = leaves
            .iter()
            .position(|leaf| leaf.terminal().has_focus())
            .unwrap_or(0);
        let next = if direction > 0 {
            (focused + 1) % leaves.len()
        } else if focused == 0 {
            leaves.len() - 1
        } else {
            focused - 1
        };
        leaves[next].grab_focus();
        self.refresh_pane_headers();
    }

    pub(crate) fn resize_pane(&self, target_orientation: Orientation, delta: i32) {
        let Some(page_num) = self.notebook.current_page() else {
            return;
        };
        let Some(page_widget) = self.notebook.nth_page(Some(page_num)) else {
            return;
        };
        let Some(leaf) = PaneNode::from_widget(&page_widget).and_then(|node| node.active_leaf())
        else {
            return;
        };

        let mut widget = leaf.root_widget().parent();
        while let Some(current) = widget {
            if let Ok(paned) = current.clone().downcast::<Paned>() {
                if paned.orientation() == target_orientation {
                    paned.set_position((paned.position() + delta).max(0));
                    return;
                }
            }
            widget = current.parent();
        }
    }

    pub(crate) fn focus_pane_directional(&self, direction: Direction) {
        log::debug!("focus_pane_directional: {direction:?}");
        let Some(page_num) = self.notebook.current_page() else {
            log::debug!("focus_pane_directional: no current page");
            return;
        };
        let Some(page_widget) = self.notebook.nth_page(Some(page_num)) else {
            log::debug!("focus_pane_directional: no page widget");
            return;
        };
        let Some(node) = PaneNode::from_widget(&page_widget) else {
            log::debug!("focus_pane_directional: page widget has no PaneNode");
            return;
        };
        let leaves = node.leaves();
        if leaves.len() <= 1 {
            log::debug!("focus_pane_directional: single leaf, nothing to focus");
            return;
        }
        // Focus can temporarily live on a finished Block VTE, a scrollbar, or
        // another descendant rather than the leaf's live input VTE. active_leaf
        // resolves the full focus subtree and falls back to the last active pane
        // instead of silently dropping the shortcut.
        let Some(focused) = node.active_leaf() else {
            log::debug!("focus_pane_directional: no active leaf");
            return;
        };
        let focused_root = focused.root_widget();
        if log::log_enabled!(log::Level::Debug) {
            let focus_widget = page_widget.root().and_then(|root| root.focus());
            log::debug!(
                "focus_pane_directional: window focus widget = {:?}",
                focus_widget.as_ref().map(|widget| widget.type_().name())
            );
        }

        let mut positioned = Vec::with_capacity(leaves.len());
        for leaf in leaves {
            let root = leaf.root_widget();
            let Some(bounds) = root.compute_bounds(&page_widget) else {
                continue;
            };
            let cx = bounds.x() + bounds.width() / 2.0;
            let cy = bounds.y() + bounds.height() / 2.0;
            positioned.push((leaf, (cx, cy)));
        }

        let Some(focused_index) = positioned
            .iter()
            .position(|(leaf, _)| leaf.root_widget() == focused_root)
        else {
            log::debug!("focus_pane_directional: focused leaf not in positioned set");
            return;
        };
        let centers = positioned
            .iter()
            .map(|(_, center)| *center)
            .collect::<Vec<_>>();
        let Some(target) = nearest_directional_index(&centers, focused_index, direction) else {
            log::debug!(
                "focus_pane_directional: no pane {direction:?} of index {focused_index} \
                 (centers: {centers:?})"
            );
            return;
        };
        log::debug!("focus_pane_directional: focusing pane {target}");
        positioned[target].0.grab_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::{balanced_split_position, nearest_directional_index, prepare_then_commit};
    use crate::keybindings::Direction;
    use std::cell::Cell;

    #[test]
    fn failed_pane_preparation_never_commits_a_split() {
        let committed = Cell::new(false);
        let result = prepare_then_commit(
            || -> std::io::Result<()> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "injected PTY spawn failure",
                ))
            },
            |()| committed.set(true),
        );

        assert_eq!(
            result
                .expect_err("injected pane creation should fail")
                .kind(),
            std::io::ErrorKind::NotFound
        );
        assert!(!committed.get(), "failed split mutated the pane tree");
    }

    #[test]
    fn balanced_position_allocates_equal_same_axis_slots() {
        assert_eq!(balanced_split_position(1_200, 1, 1), Some(600));
        assert_eq!(balanced_split_position(1_200, 1, 2), Some(400));
        assert_eq!(balanced_split_position(1_200, 2, 1), Some(800));
        assert_eq!(balanced_split_position(1_200, 3, 1), Some(900));
    }

    #[test]
    fn balanced_position_rejects_unallocated_or_empty_splits() {
        assert_eq!(balanced_split_position(0, 1, 1), None);
        assert_eq!(balanced_split_position(100, 0, 1), None);
        assert_eq!(balanced_split_position(100, 1, 0), None);
    }

    #[test]
    fn directional_focus_selects_the_nearest_pane_on_each_axis() {
        // 2x2 pane grid in visual order.
        let centers = [(25.0, 25.0), (75.0, 25.0), (25.0, 75.0), (75.0, 75.0)];

        assert_eq!(
            nearest_directional_index(&centers, 3, Direction::Left),
            Some(2)
        );
        assert_eq!(
            nearest_directional_index(&centers, 2, Direction::Right),
            Some(3)
        );
        assert_eq!(
            nearest_directional_index(&centers, 3, Direction::Up),
            Some(1)
        );
        assert_eq!(
            nearest_directional_index(&centers, 1, Direction::Down),
            Some(3)
        );
    }

    #[test]
    fn directional_focus_does_not_wrap_at_an_outer_edge() {
        let centers = [(25.0, 25.0), (75.0, 25.0)];

        assert_eq!(
            nearest_directional_index(&centers, 0, Direction::Left),
            None
        );
        assert_eq!(
            nearest_directional_index(&centers, 1, Direction::Right),
            None
        );
        assert_eq!(nearest_directional_index(&centers, 0, Direction::Up), None);
        assert_eq!(
            nearest_directional_index(&centers, 0, Direction::Down),
            None
        );
    }
}
