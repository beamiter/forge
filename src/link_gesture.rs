use gtk4::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// Keep native drag selection available. Activation requires a release over
/// the same target, on the same widget, without crossing the drag threshold.
pub(crate) fn install<T: PartialEq + 'static>(
    widget: &impl IsA<gtk4::Widget>,
    hit: impl Fn(f64, f64) -> Option<T> + 'static,
    activate: impl Fn(T) + 'static,
) {
    let gesture = gtk4::GestureClick::new();
    gesture.set_button(1);
    gesture.set_propagation_phase(gtk4::PropagationPhase::Capture);
    let pending = Rc::new(RefCell::new(None));
    let hit = Rc::new(hit);
    let press = pending.clone();
    let press_hit = hit.clone();
    gesture.connect_pressed(move |gesture, count, x, y| {
        use gtk4::gdk::ModifierType as M;
        let state = gesture.current_event_state();
        let ctrl = state.contains(M::CONTROL_MASK)
            && !state.intersects(
                M::SHIFT_MASK | M::ALT_MASK | M::SUPER_MASK | M::HYPER_MASK | M::META_MASK,
            );
        let target = (count == 1 && ctrl).then(|| press_hit(x, y)).flatten();
        gesture.set_state(if target.is_some() {
            gtk4::EventSequenceState::Claimed
        } else {
            gtk4::EventSequenceState::Denied
        });
        *press.borrow_mut() = target.map(|target| (x, y, target));
    });
    let stopped = pending.clone();
    gesture.connect_stopped(move |_| {
        stopped.borrow_mut().take();
    });
    let cancelled = pending.clone();
    gesture.connect_cancel(move |_, _| {
        cancelled.borrow_mut().take();
    });
    gesture.connect_released(move |gesture, count, x, y| {
        let Some((px, py, target)) = pending.borrow_mut().take() else {
            return;
        };
        if count == 1
            && (x - px).hypot(y - py) <= 4.0
            && hit(x, y).is_some_and(|current| current == target)
        {
            activate(target);
            gesture.set_state(gtk4::EventSequenceState::Claimed);
        }
    });
    widget.add_controller(gesture);
}
