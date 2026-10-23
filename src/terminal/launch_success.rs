//! One-shot successful-spawn observers, distinct from settlement cleanup hooks.
use std::cell::{Cell, RefCell};

#[derive(Default)]
pub(super) struct LaunchSuccess {
    succeeded: Cell<bool>,
    observers: RefCell<Vec<Box<dyn FnOnce()>>>,
}
impl LaunchSuccess {
    pub(super) fn connect(&self, observer: impl FnOnce() + 'static) {
        if self.succeeded.get() {
            observer();
        } else {
            self.observers.borrow_mut().push(Box::new(observer));
        }
    }
    pub(super) fn resolved(&self, succeeded: bool) {
        if !succeeded || self.succeeded.replace(true) {
            return;
        }
        let observers = std::mem::take(&mut *self.observers.borrow_mut());
        for observer in observers {
            observer();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;
    #[test]
    fn failed_settlement_never_announces_success_and_success_is_exactly_once() {
        let success = LaunchSuccess::default();
        let calls = Rc::new(Cell::new(0));
        let counted = calls.clone();
        success.connect(move || counted.set(counted.get() + 1));
        success.resolved(false);
        success.resolved(false);
        assert_eq!(calls.get(), 0);
        success.resolved(true);
        success.resolved(true);
        success.resolved(false);
        assert_eq!(calls.get(), 1);
    }
    #[test]
    fn late_subscribers_observe_success_without_replaying_previous_observers() {
        let success = LaunchSuccess::default();
        success.resolved(true);
        let calls = Rc::new(Cell::new(0));
        let counted = calls.clone();
        success.connect(move || counted.set(counted.get() + 1));
        success.resolved(true);
        assert_eq!(calls.get(), 1);
    }
}
