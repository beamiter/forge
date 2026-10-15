//! Production Agent render paths with a mapped, nonexecuting terminal.
use super::*;
use std::time::{Duration, Instant};

fn pump() {
    let end = Instant::now() + Duration::from_millis(100);
    while Instant::now() < end {
        for _ in 0..100 {
            if !gtk4::glib::MainContext::default().iteration(false) {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn owns_focus(widget: &impl IsA<gtk4::Widget>) -> bool {
    let widget = widget.as_ref();
    widget
        .root()
        .and_then(|root| root.focus())
        .is_some_and(|focus| focus == *widget || focus.is_ancestor(widget))
}

fn fixture() -> (Rc<AgentRuntime>, gtk4::Window, Vec<gtk4::Widget>) {
    adw::init().expect("isolated GTK display");
    let target = crate::block_view::organism_settings_test_view();
    let config = Rc::new(RefCell::new(crate::config::Config::safe_defaults()));
    let card = GBox::new(Orientation::Vertical, 0);
    let input = Entry::new();
    let proposal_box = GBox::new(Orientation::Vertical, 0);
    card.append(&input);
    card.append(&proposal_box);
    let runtime = Rc::new(AgentRuntime {
        session: RefCell::new(AgentSession::new(20)),
        history_notice: Label::new(None),
        target: target.clone(),
        ui_lifetime: Rc::new(AgentUiLifetime::default()),
        config: config.clone(),
        shell: "sh".to_string(),
        block_context: RefCell::new(None),
        card: card.clone().upcast(),
        input,
        send: Button::new(),
        stop_request: Button::new(),
        retry_request: Button::new(),
        context_clear: Button::new(),
        context_attach: Button::new(),
        context_card: GBox::new(Orientation::Vertical, 0),
        context_label: Label::new(None),
        status: Label::new(None),
        status_spinner: Spinner::new(),
        prompt_status: Label::new(None),
        turn_progress: ProgressBar::new(),
        turn_label: Label::new(None),
        session_action: Button::new(),
        proposal_box,
        task_epoch: Cell::new(0),
        pending_command: RefCell::new(None),
        next_execution_generation: Cell::new(0),
        request_cancellation: RefCell::new(None),
        busy: Cell::new(false),
        alive: Cell::new(true),
        organism_agent: super::super::OrganismAgentSignal::new(
            Rc::new(Cell::new(Default::default())),
            config,
        ),
    });
    assert!(target.insert_inline_notice(&runtime.card));
    let column = GBox::new(Orientation::Vertical, 0);
    column.append(&target.widget());
    let peers: Vec<gtk4::Widget> = vec![
        Entry::new().upcast(),             // another pane's input
        gtk4::SearchEntry::new().upcast(), // Find
        Button::with_label("History action").upcast(),
    ];
    for peer in &peers {
        column.append(peer);
    }
    let window = gtk4::Window::builder()
        .default_width(800)
        .default_height(700)
        .child(&column)
        .build();
    window.present();
    pump();
    assert!(runtime.input.is_mapped());
    (runtime, window, peers)
}

#[test]
#[ignore = "requires DISPLAY; mapped Agent render and focus ownership"]
fn asynchronous_agent_renders_preserve_newer_focus_owners() {
    let (runtime, window, peers) = fixture();
    for peer in &peers {
        for reply in ["say", "failure", "stopped", "proposal"] {
            *runtime.session.borrow_mut() = AgentSession::new(20);
            runtime.clear_proposal();
            runtime.session.borrow_mut().submit_user("task").unwrap();
            runtime.busy.set(true);
            runtime.sync_controls();
            assert!(peer.grab_focus());
            assert!(owns_focus(peer));
            runtime.busy.set(false);
            match reply {
                "proposal" => {
                    let outcome = runtime
                        .session
                        .borrow_mut()
                        .accept_model_reply(r#"{"action":"run","command":"pwd"}"#)
                        .unwrap();
                    let ModelOutcome::Proposal { id, command, .. } = outcome else {
                        panic!("expected proposal");
                    };
                    AgentRuntime::render_proposal_card(&runtime, id, command);
                }
                "say" => {
                    let outcome = runtime
                        .session
                        .borrow_mut()
                        .accept_model_reply(
                            r#"{"action":"say","message":"Ready for your next instruction"}"#,
                        )
                        .unwrap();
                    let ModelOutcome::Said(message) = outcome else {
                        panic!("expected message");
                    };
                    runtime.append("Agent", &message);
                    runtime.render_session_state(None);
                }
                _ => {
                    runtime.session.borrow_mut().model_failed(reply).unwrap();
                    runtime.append(
                        if reply == "stopped" {
                            "Stopped"
                        } else {
                            "Error"
                        },
                        reply,
                    );
                    runtime.render_session_state(Some(reply));
                }
            }
            assert!(owns_focus(peer), "{reply} render stole focus synchronously");
            pump();
            runtime.render_session_state(None);
            pump();
            assert!(owns_focus(peer), "repeated {reply} render stole focus");
        }
    }
    // Positive control: an explicit New task still returns to its composer.
    runtime.session.borrow_mut().cancel();
    AgentRuntime::resume_or_start_new(runtime.clone());
    pump();
    assert!(owns_focus(&runtime.input));
    runtime.session.borrow_mut().submit_user("finish").unwrap();
    let outcome = runtime
        .session
        .borrow_mut()
        .accept_model_reply(r#"{"action":"done","message":"Finished"}"#)
        .unwrap();
    let ModelOutcome::Completed(message) = outcome else {
        panic!("expected completion");
    };
    peers[0].grab_focus();
    runtime.append("Agent", &message);
    runtime.render_session_state(None);
    assert!(owns_focus(&peers[0]));
    AgentRuntime::resume_or_start_new(runtime.clone());
    pump();
    assert!(owns_focus(&runtime.input), "explicit Follow up owns focus");
    // Review remains reachable without an asynchronous grab_focus: its entry
    // is in GTK's normal focus chain and keeps Enter-to-approve handling.
    runtime.session.borrow_mut().submit_user("review").unwrap();
    let outcome = runtime
        .session
        .borrow_mut()
        .accept_model_reply(r#"{"action":"run","command":"pwd"}"#)
        .unwrap();
    let ModelOutcome::Proposal { id, command, .. } = outcome else {
        panic!("expected proposal");
    };
    peers[0].grab_focus();
    AgentRuntime::render_proposal_card(&runtime, id, command);
    pump();
    assert!(runtime
        .proposal_box
        .child_focus(gtk4::DirectionType::TabForward));
    assert!(owns_focus(&runtime.proposal_box));
    window.close();
}
