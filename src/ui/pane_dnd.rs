//! Typed drag payloads and pure pane-drop planning.
//!
//! GTK indices and tab positions are presentation state: both can change while
//! the pointer is held down. Drag payloads therefore carry the pane's persisted
//! session identity and every drop resolves that identity against the live pane
//! tree immediately before committing a move.

use gtk4::glib;

/// A split pane dragged by its visible header.
///
/// Keeping this a private boxed GType prevents VTE's text drop target from
/// accepting a session id as terminal input.
#[derive(Clone, Debug, glib::Boxed)]
#[boxed_type(name = "ForgePaneDragPayload")]
pub(crate) struct PaneDragPayload(pub(crate) String);

/// One tab-strip drag.
///
/// `tab_name` preserves ordinary tab reordering. `pane_session_id` is present
/// only while the source is still a single-pane tab, so a split tab can be
/// reordered but can never accidentally be nested as one opaque pane.
#[derive(Clone, Debug, PartialEq, Eq, glib::Boxed)]
#[boxed_type(name = "ForgeTabDragPayload")]
pub(crate) struct TabDragPayload {
    pub(crate) tab_name: String,
    pub(crate) pane_session_id: Option<String>,
}

pub(crate) fn tab_payload_can_split(payload: &TabDragPayload) -> bool {
    payload
        .pane_session_id
        .as_deref()
        .is_some_and(|session_id| !session_id.is_empty())
}

/// Process-local authority for delayed tab-hover work.
///
/// Per-target timers are not enough: GTK may omit a target's `leave` when a
/// row is rebuilt, and a later drag can otherwise satisfy the old timer's
/// generic "some tab is dragging" check. The token binds a timeout to both the
/// exact typed source and one global drag generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TabDragToken {
    generation: u64,
    payload: TabDragPayload,
    target_tab_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActiveTabDrag {
    payload: TabDragPayload,
    origin_tab_name: Option<String>,
    pending_hover_target: Option<String>,
    preview_target: Option<String>,
    topology_committed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TabDragState {
    generation: Option<u64>,
    active: Option<ActiveTabDrag>,
}

impl Default for TabDragState {
    fn default() -> Self {
        Self {
            generation: Some(0),
            active: None,
        }
    }
}

impl TabDragState {
    fn advance(&mut self) -> Option<u64> {
        self.generation = self
            .generation
            .and_then(|generation| generation.checked_add(1));
        if self.generation.is_none() {
            // Exhaustion is practically unreachable, but wrapping would make
            // an ancient timer authoritative again. Disable hover fail-closed.
            self.active = None;
        }
        self.generation
    }

    pub(crate) fn begin(
        &mut self,
        payload: TabDragPayload,
        origin_tab_name: Option<String>,
    ) -> bool {
        if self.advance().is_none() {
            return false;
        }
        self.active = Some(ActiveTabDrag {
            payload,
            origin_tab_name,
            pending_hover_target: None,
            preview_target: None,
            topology_committed: false,
        });
        true
    }

    /// Invalidate pending hover work while retaining the active drag source.
    pub(crate) fn invalidate(&mut self) {
        let _next = self.advance();
        if let Some(active) = self.active.as_mut() {
            active.pending_hover_target = None;
        }
    }

    /// Finish a drag and return the stable tab identity that should be restored.
    /// A successful tab-to-split transaction deliberately keeps its new page.
    pub(crate) fn end(&mut self) -> Option<String> {
        let active = self.active.take();
        let _next = self.advance();
        active.and_then(|active| {
            (!active.topology_committed)
                .then_some(active.origin_tab_name)
                .flatten()
        })
    }

    /// Schedule at most one timeout for a target under the current exact source.
    /// Switching targets advances the global generation, invalidating the old
    /// target even if GTK never delivered its `leave` signal.
    pub(crate) fn schedule_hover(
        &mut self,
        payload: &TabDragPayload,
        target_tab_name: &str,
    ) -> Option<TabDragToken> {
        let eligible = !target_tab_name.is_empty()
            && self.active.as_ref().is_some_and(|active| {
                active.payload == *payload
                    && active.pending_hover_target.as_deref() != Some(target_tab_name)
                    && active.preview_target.as_deref() != Some(target_tab_name)
            });
        if !eligible {
            return None;
        }
        let generation = self.advance()?;
        self.active.as_mut()?.pending_hover_target = Some(target_tab_name.to_string());
        Some(TabDragToken {
            generation,
            payload: payload.clone(),
            target_tab_name: target_tab_name.to_string(),
        })
    }

    pub(crate) fn is_current(&self, token: &TabDragToken) -> bool {
        self.generation == Some(token.generation)
            && self.active.as_ref().is_some_and(|active| {
                active.payload == token.payload
                    && active.pending_hover_target.as_deref()
                        == Some(token.target_tab_name.as_str())
            })
    }

    pub(crate) fn activate_hover(&mut self, token: &TabDragToken) -> bool {
        if !self.is_current(token) {
            return false;
        }
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        active.pending_hover_target = None;
        active.preview_target = Some(token.target_tab_name.clone());
        true
    }

    pub(crate) fn commit_topology(&mut self, dragged_session: &str) -> bool {
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        if active.payload.pane_session_id.as_deref() != Some(dragged_session) {
            return false;
        }
        active.pending_hover_target = None;
        active.topology_committed = true;
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SplitDropZone {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SplitAxis {
    Horizontal,
    Vertical,
}

/// Structural split requested by one visual edge.
///
/// The boolean says whether the dragged pane precedes the target in the new
/// `Paned`. Keeping this mapping pure avoids four subtly different GTK paths.
pub(crate) fn split_placement(zone: SplitDropZone) -> (SplitAxis, bool) {
    match zone {
        SplitDropZone::Left => (SplitAxis::Horizontal, true),
        SplitDropZone::Right => (SplitAxis::Horizontal, false),
        SplitDropZone::Up => (SplitAxis::Vertical, true),
        SplitDropZone::Down => (SplitAxis::Vertical, false),
    }
}

/// Resolve a pointer to one of the four outer quarters of a pane.
///
/// The center half is deliberately not a target: it remains available for
/// terminal text drops and makes an imprecise/cancelled drag a no-op. At a
/// corner the physically nearest edge wins; exact ties prefer the horizontal
/// edge deterministically.
pub(crate) fn split_drop_zone(width: i32, height: i32, x: f64, y: f64) -> Option<SplitDropZone> {
    if width <= 0
        || height <= 0
        || !x.is_finite()
        || !y.is_finite()
        || x < 0.0
        || y < 0.0
        || x > f64::from(width)
        || y > f64::from(height)
    {
        return None;
    }

    let width = f64::from(width);
    let height = f64::from(height);
    let horizontal = if x <= width * 0.25 {
        Some((x, SplitDropZone::Left))
    } else if x >= width * 0.75 {
        Some((width - x, SplitDropZone::Right))
    } else {
        None
    };
    let vertical = if y <= height * 0.25 {
        Some((y, SplitDropZone::Up))
    } else if y >= height * 0.75 {
        Some((height - y, SplitDropZone::Down))
    } else {
        None
    };

    match (horizontal, vertical) {
        (Some((horizontal_distance, horizontal)), Some((vertical_distance, vertical))) => {
            if horizontal_distance <= vertical_distance {
                Some(horizontal)
            } else {
                Some(vertical)
            }
        }
        (Some((_, zone)), None) | (None, Some((_, zone))) => Some(zone),
        (None, None) => None,
    }
}

/// Find exactly one occurrence of a session id.
///
/// Persisted state is validated, but a defensive drop must still refuse a
/// duplicate identity rather than move whichever widget happened to be walked
/// first. Missing ids and empty requests are invalid for the same reason.
pub(crate) fn unique_session_index(
    sessions: impl IntoIterator<Item = Option<String>>,
    requested: &str,
) -> Option<usize> {
    if requested.is_empty() {
        return None;
    }
    let mut found = None;
    for (index, session) in sessions.into_iter().enumerate() {
        if session.as_deref() != Some(requested) {
            continue;
        }
        if found.replace(index).is_some() {
            return None;
        }
    }
    found
}

/// Final pure gate before a tab-to-split transaction may touch GTK parents.
pub(crate) fn tab_split_drop_allowed(
    source_is_plain: bool,
    same_session: bool,
    same_page: bool,
    zoomed: bool,
    connection_conflict: bool,
) -> bool {
    source_is_plain && !same_session && !same_page && !zoomed && !connection_conflict
}

#[cfg(test)]
mod tests {
    use super::{
        split_drop_zone, split_placement, tab_payload_can_split, tab_split_drop_allowed,
        unique_session_index, SplitAxis, SplitDropZone, TabDragPayload, TabDragState,
    };

    fn tab_payload(name: &str, session: &str) -> TabDragPayload {
        TabDragPayload {
            tab_name: name.to_string(),
            pane_session_id: Some(session.to_string()),
        }
    }

    #[test]
    fn outer_quarters_map_to_the_four_split_directions() {
        assert_eq!(
            split_drop_zone(400, 200, 10.0, 100.0),
            Some(SplitDropZone::Left)
        );
        assert_eq!(
            split_drop_zone(400, 200, 390.0, 100.0),
            Some(SplitDropZone::Right)
        );
        assert_eq!(
            split_drop_zone(400, 200, 200.0, 10.0),
            Some(SplitDropZone::Up)
        );
        assert_eq!(
            split_drop_zone(400, 200, 200.0, 190.0),
            Some(SplitDropZone::Down)
        );
    }

    #[test]
    fn center_outside_and_unallocated_drops_are_noops() {
        assert_eq!(split_drop_zone(400, 200, 200.0, 100.0), None);
        assert_eq!(split_drop_zone(400, 200, -1.0, 100.0), None);
        assert_eq!(split_drop_zone(400, 200, 401.0, 100.0), None);
        assert_eq!(split_drop_zone(0, 200, 0.0, 100.0), None);
        assert_eq!(split_drop_zone(400, 200, f64::NAN, 0.0), None);
    }

    #[test]
    fn nearest_corner_edge_wins_with_a_deterministic_tie() {
        assert_eq!(
            split_drop_zone(400, 200, 5.0, 20.0),
            Some(SplitDropZone::Left)
        );
        assert_eq!(
            split_drop_zone(400, 200, 20.0, 5.0),
            Some(SplitDropZone::Up)
        );
        assert_eq!(
            split_drop_zone(400, 200, 5.0, 5.0),
            Some(SplitDropZone::Left)
        );
    }

    #[test]
    fn placement_maps_edges_to_axis_and_child_order() {
        assert_eq!(
            split_placement(SplitDropZone::Left),
            (SplitAxis::Horizontal, true)
        );
        assert_eq!(
            split_placement(SplitDropZone::Right),
            (SplitAxis::Horizontal, false)
        );
        assert_eq!(
            split_placement(SplitDropZone::Up),
            (SplitAxis::Vertical, true)
        );
        assert_eq!(
            split_placement(SplitDropZone::Down),
            (SplitAxis::Vertical, false)
        );
    }

    #[test]
    fn stable_identity_resolution_rejects_unknown_empty_and_duplicates() {
        let sessions = || vec![Some("pane-a".to_string()), None, Some("pane-b".to_string())];
        assert_eq!(unique_session_index(sessions(), "pane-b"), Some(2));
        assert_eq!(unique_session_index(sessions(), "missing"), None);
        assert_eq!(unique_session_index(sessions(), ""), None);
        assert_eq!(
            unique_session_index([Some("same".to_string()), Some("same".to_string())], "same"),
            None
        );
    }

    #[test]
    fn transaction_preflight_rejects_every_lossy_or_self_move() {
        assert!(tab_split_drop_allowed(true, false, false, false, false));
        assert!(!tab_split_drop_allowed(false, false, false, false, false));
        assert!(!tab_split_drop_allowed(true, true, false, false, false));
        assert!(!tab_split_drop_allowed(true, false, true, false, false));
        assert!(!tab_split_drop_allowed(true, false, false, true, false));
        assert!(!tab_split_drop_allowed(true, false, false, false, true));
    }

    #[test]
    fn global_drag_generation_invalidates_an_older_hover_token() {
        let mut state = TabDragState::default();
        let payload = tab_payload("tab-1", "pane-a");
        assert!(state.begin(payload.clone(), Some("tab-origin".to_string())));
        let token = state.schedule_hover(&payload, "tab-2").unwrap();
        assert!(state.is_current(&token));

        state.invalidate();
        assert!(!state.is_current(&token));
    }

    #[test]
    fn a_later_source_cannot_consume_an_earlier_sources_timer() {
        let mut state = TabDragState::default();
        let first = tab_payload("tab-1", "pane-a");
        assert!(state.begin(first.clone(), Some("tab-1".to_string())));
        let first_token = state.schedule_hover(&first, "tab-target").unwrap();
        state.end();

        let second = tab_payload("tab-2", "pane-b");
        assert!(state.begin(second.clone(), Some("tab-2".to_string())));
        let second_token = state.schedule_hover(&second, "tab-target").unwrap();
        assert!(!state.is_current(&first_token));
        assert!(state.is_current(&second_token));
        assert!(state
            .schedule_hover(&tab_payload("tab-2", "different-pane"), "tab-other")
            .is_none());
    }

    #[test]
    fn a_new_hover_target_supersedes_an_old_target_without_leave() {
        let mut state = TabDragState::default();
        let payload = tab_payload("tab-1", "pane-a");
        assert!(state.begin(payload.clone(), Some("tab-1".to_string())));
        let first = state.schedule_hover(&payload, "tab-2").unwrap();
        assert!(state.schedule_hover(&payload, "tab-2").is_none());
        let second = state.schedule_hover(&payload, "tab-3").unwrap();
        assert!(!state.is_current(&first));
        assert!(state.is_current(&second));
    }

    #[test]
    fn cancelled_preview_restores_origin_but_committed_topology_does_not() {
        let mut state = TabDragState::default();
        let payload = tab_payload("tab-1", "pane-a");
        assert!(state.begin(payload.clone(), Some("tab-origin".to_string())));
        let token = state.schedule_hover(&payload, "tab-preview").unwrap();
        assert!(state.activate_hover(&token));
        assert_eq!(state.end().as_deref(), Some("tab-origin"));

        assert!(state.begin(payload.clone(), Some("tab-origin".to_string())));
        let token = state.schedule_hover(&payload, "tab-preview").unwrap();
        assert!(state.activate_hover(&token));
        state.invalidate();
        assert!(state.commit_topology("pane-a"));
        assert_eq!(state.end(), None);
    }

    #[test]
    fn topology_commit_must_match_the_exact_dragged_session() {
        let mut state = TabDragState::default();
        assert!(state.begin(
            tab_payload("tab-1", "pane-a"),
            Some("tab-origin".to_string())
        ));
        assert!(!state.commit_topology("pane-b"));
        assert_eq!(state.end().as_deref(), Some("tab-origin"));
    }

    #[test]
    fn drag_generation_exhaustion_fails_closed_instead_of_wrapping() {
        let mut state = TabDragState {
            generation: Some(u64::MAX),
            active: None,
        };
        assert!(!state.begin(tab_payload("tab-1", "pane-a"), Some("tab-1".to_string())));
        assert_eq!(state.generation, None);
        assert_eq!(state.active, None);
    }

    #[test]
    fn only_a_nonempty_stable_pane_identity_can_target_content_edges() {
        assert!(tab_payload_can_split(&tab_payload("tab-1", "pane-a")));
        assert!(!tab_payload_can_split(&TabDragPayload {
            tab_name: "tab-2".to_string(),
            pane_session_id: None,
        }));
        assert!(!tab_payload_can_split(&tab_payload("tab-3", "")));
    }
}
