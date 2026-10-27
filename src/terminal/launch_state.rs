//! One pane owns one launch attempt and one pending startup-command batch.
//! Kept independent of GTK so stale completions can be exercised without a child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Generation(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    New,
    Starting,
    Failed,
    Running,
    Exited,
    Closed,
}

pub(super) struct LaunchState {
    generation: Generation,
    phase: Phase,
    pending_commands: Vec<String>,
    child_exited: bool,
}

impl LaunchState {
    pub(super) fn new(commands: &[String]) -> Self {
        Self {
            generation: Generation(0),
            phase: Phase::New,
            pending_commands: commands.to_vec(),
            child_exited: false,
        }
    }

    /// Only failed execs can retry. A real child's exit uses the existing pane
    /// exit policy, and can never turn into a recovery launch.
    pub(super) fn begin(&mut self) -> Option<Generation> {
        if !matches!(self.phase, Phase::New | Phase::Failed) {
            return None;
        }
        self.generation = Generation(self.generation.0.checked_add(1)?);
        self.phase = Phase::Starting;
        Some(self.generation)
    }

    pub(super) fn resolve(&mut self, generation: Generation, success: bool) -> bool {
        if self.generation != generation || self.phase != Phase::Starting {
            return false;
        }
        self.phase = if success {
            Phase::Running
        } else {
            Phase::Failed
        };
        true
    }

    pub(super) fn failed_generation(&self, generation: Generation) -> bool {
        self.generation == generation && self.phase == Phase::Failed
    }

    pub(super) fn running_generation(&self, generation: Generation) -> bool {
        self.generation == generation && self.phase == Phase::Running
    }

    pub(super) fn closed(&self) -> bool {
        self.phase == Phase::Closed
    }

    pub(super) fn failed(&self) -> bool {
        self.phase == Phase::Failed
    }

    pub(super) fn exited(&mut self) -> bool {
        self.child_exited = true;
        if !matches!(self.phase, Phase::Starting | Phase::Running) {
            return false;
        }
        self.phase = Phase::Exited;
        self.pending_commands.clear();
        true
    }

    pub(super) fn may_acquire_child(&self, generation: Generation) -> bool {
        self.generation == generation
            && !self.child_exited
            && matches!(self.phase, Phase::Starting | Phase::Closed)
    }

    pub(super) fn child_exited(&self) -> bool {
        self.child_exited
    }

    pub(super) fn close(&mut self) {
        self.phase = Phase::Closed;
        self.pending_commands.clear();
    }

    pub(super) fn pending_count(&self) -> usize {
        self.pending_commands.len()
    }

    /// Called only after checking the native pane/child is still live. Taking
    /// before feeding means reentrant signals cannot replay any of the batch.
    pub(super) fn take_commands(&mut self, generation: Generation) -> Vec<String> {
        if !self.running_generation(generation) {
            return Vec::new();
        }
        std::mem::take(&mut self.pending_commands)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_corrected_success_is_single_flight_and_delivers_once() {
        let mut s = LaunchState::new(&["synthetic marker".into()]);
        let a = s.begin().unwrap();
        assert!(s.begin().is_none());
        assert!(s.take_commands(a).is_empty());
        assert!(s.resolve(a, false));
        assert!(s.failed_generation(a));
        assert!(s.take_commands(a).is_empty());
        let b = s.begin().unwrap();
        assert_ne!(a, b);
        assert!(
            !s.failed_generation(a),
            "reentrant retry invalidates the old diagnostic"
        );
        assert!(!s.resolve(a, true));
        assert!(!s.resolve(a, false));
        assert!(s.take_commands(a).is_empty());
        assert!(s.resolve(b, true));
        assert!(!s.resolve(b, false));
        assert_eq!(s.take_commands(b), ["synthetic marker"]);
        assert!(s.take_commands(b).is_empty());
        assert!(s.begin().is_none());
        assert!(s.exited());
        assert!(s.begin().is_none());
    }
    #[test]
    fn exit_and_close_invalidate_delayed_commands_and_completions() {
        for close in [false, true] {
            let mut s = LaunchState::new(&["never delivered".into()]);
            let a = s.begin().unwrap();
            assert!(s.resolve(a, true));
            if close {
                s.close();
            } else {
                assert!(s.exited());
            }
            assert!(s.take_commands(a).is_empty());
            assert_eq!(s.pending_count(), 0);
            assert!(s.begin().is_none());
            assert!(!s.resolve(a, true));
        }
        let mut s = LaunchState::new(&[]);
        let a = s.begin().unwrap();
        s.close();
        assert!(!s.resolve(a, true));
        assert!(!s.resolve(a, false));
    }
    #[test]
    fn repeated_failures_retain_only_the_original_pending_batch() {
        let mut s = LaunchState::new(&["one".into(), "two".into()]);
        for _ in 0..1000 {
            let a = s.begin().unwrap();
            assert!(s.resolve(a, false));
            assert!(s.failed());
            assert_eq!(s.pending_count(), 2);
        }
        let a = s.begin().unwrap();
        assert!(s.resolve(a, true));
        assert_eq!(s.take_commands(a), ["one", "two"]);
        assert_eq!(s.pending_count(), 0);
    }
    #[test]
    fn child_exit_before_resolution_blocks_state_admission_and_handle_acquisition() {
        let mut s = LaunchState::new(&["never delivered".into()]);
        let a = s.begin().unwrap();
        assert!(s.exited());
        assert!(s.child_exited());
        assert!(!s.may_acquire_child(a));
        assert!(!s.resolve(a, true));
        assert!(s.take_commands(a).is_empty());
        s.close();
        assert!(s.child_exited());
    }
    #[test]
    fn generation_overflow_fails_closed() {
        let mut s = LaunchState::new(&[]);
        s.generation = Generation(u64::MAX);
        assert!(s.begin().is_none());
    }
}
