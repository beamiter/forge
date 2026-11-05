//! The desktop launcher reveals existing GUI-owned panes only.
use super::{PaneNode, UiState};
use gtk4::prelude::*;
use jterm_core::desktop_sessions::{Bridge, Session};
use std::{rc::Rc, time::Duration};

pub(crate) fn install(ui: &Rc<UiState>) {
    let owner = Rc::downgrade(ui);
    let mut bridge = None;
    gtk4::glib::timeout_add_local(Duration::from_millis(250), move || {
        let Some(ui) = owner.upgrade() else {
            return gtk4::glib::ControlFlow::Break;
        };
        if bridge.is_none() {
            let Some(surface) = ui.window.surface() else {
                return gtk4::glib::ControlFlow::Continue;
            };
            let raw = if surface.type_().name().contains("X11") {
                // Optional GDK backend symbol; Wayland remains unique-PID-only
                // and is unavailable when the process owns multiple windows.
                let function =
                    unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"gdk_x11_surface_get_xid".as_ptr()) };
                if function.is_null() {
                    None
                } else {
                    let get: unsafe extern "C" fn(*mut libc::c_void) -> libc::c_ulong =
                        unsafe { std::mem::transmute(function) };
                    Some(unsafe { get(surface.as_ptr().cast()) } as u64)
                }
            } else {
                None
            };
            bridge = Some(Bridge::new("forge", raw, || {}));
        }
        let bridge = bridge.as_ref().unwrap();
        let mut identities = Vec::new();
        for (key, terminal) in ui.agent_session_tabs.borrow().iter() {
            let Some(terminal) = terminal.upgrade() else {
                continue;
            };
            let pane = (0..ui.notebook.n_pages())
                .filter_map(|index| ui.notebook.nth_page(Some(index)))
                .filter_map(|page| PaneNode::from_widget(&page))
                .flat_map(|node| node.leaves())
                .find(|leaf| leaf.terminal() == &terminal)
                .and_then(|leaf| leaf.session_id());
            let Some(pane) = pane else {
                continue;
            };
            let Some(session) = ui.session_sidebar.desktop_session(key) else {
                continue;
            };
            if let Some(metadata) = Session::from_agent(&session, pane) {
                identities.push((metadata, key.clone(), session));
            }
        }
        bridge.publish(
            identities
                .iter()
                .map(|(metadata, _, _)| metadata.clone())
                .collect(),
        );
        for request in bridge.poll() {
            let success = if let Some((_, key, session)) =
                identities.iter().find(|(metadata, _, _)| {
                    metadata.session_id == request.session_id && metadata.pane_id == request.pane_id
                }) {
                let (completion, result) = std::sync::mpsc::channel();
                ui.add_agent_session_tab(super::agent_sessions::ResumeRequest {
                    runtime_key: key.clone(),
                    provider: session.provider,
                    id: session.id.clone(),
                    title: session.title.clone(),
                    launch: None,
                    completion,
                });
                matches!(result.try_recv(), Ok(Ok(())))
            } else {
                false
            };
            bridge.acknowledge(request, success);
        }
        gtk4::glib::ControlFlow::Continue
    });
}
