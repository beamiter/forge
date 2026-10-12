//! Pure presentation decisions. Measured rectangles are native facts, never
//! guessed character-cell sizes or evidence about a VTE command's activity.

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn valid(self) -> bool {
        [
            self.x,
            self.y,
            self.width,
            self.height,
            self.x + self.width,
            self.y + self.height,
        ]
        .into_iter()
        .all(f64::is_finite)
            && self.width > 0.0
            && self.height > 0.0
    }

    pub fn union(self, other: Self) -> Option<Self> {
        if !self.valid() || !other.valid() {
            return None;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let width = (self.x + self.width).max(other.x + other.width) - x;
        let height = (self.y + self.height).max(other.y + other.height) - y;
        let result = Self {
            x,
            y,
            width,
            height,
        };
        result.valid().then_some(result)
    }

    pub fn contains(self, candidate: Self) -> bool {
        self.valid()
            && candidate.valid()
            && candidate.x >= self.x
            && candidate.y >= self.y
            && candidate.x + candidate.width <= self.x + self.width
            && candidate.y + candidate.height <= self.y + self.height
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Paint {
    Compact,
    ExpandedSprite,
    ExpandedCompactFallback,
    Hidden,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Envelope {
    pub width: i32,
    pub height: i32,
}

impl Envelope {
    pub fn valid(self) -> bool {
        self.width > 0 && self.height > 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Plan {
    pub paint: Paint,
    /// Never derived from fit or admission: retreat and clipping do not regrid.
    pub reservation: Option<Envelope>,
}

pub(super) fn plan(
    expanded: bool,
    admitted: bool,
    retreat: bool,
    envelope: Option<Envelope>,
    clip: Option<Rect>,
    full: Option<Rect>,
    fallback: Option<Rect>,
) -> Plan {
    let reservation = expanded.then_some(envelope).flatten().filter(|e| e.valid());
    let paint = if !admitted || retreat {
        Paint::Hidden
    } else if !expanded {
        Paint::Compact
    } else if reservation.is_none() {
        Paint::Hidden
    } else if clip
        .zip(full)
        .is_some_and(|(clip, body)| clip.contains(body))
    {
        Paint::ExpandedSprite
    } else if clip
        .zip(fallback)
        .is_some_and(|(clip, body)| clip.contains(body))
    {
        Paint::ExpandedCompactFallback
    } else {
        Paint::Hidden
    };
    Plan { paint, reservation }
}

pub(super) fn resolve_current<T>(
    target: &std::rc::Weak<T>,
    current: Option<&std::rc::Rc<T>>,
) -> Option<std::rc::Rc<T>> {
    let original = target.upgrade()?;
    current
        .filter(|current| std::rc::Rc::ptr_eq(current, &original))
        .map(|_| original)
}

/// Applying presentation never supplies authorization for live interaction.
pub(super) fn requested_change(current: bool, requested: bool, target_valid: bool) -> Option<bool> {
    (target_valid && current != requested).then_some(requested)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(width: f64) -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            width,
            height: 30.0,
        }
    }
    const SIZE: Envelope = Envelope {
        width: 70,
        height: 30,
    };
    fn choice(width: f64) -> Plan {
        plan(
            true,
            true,
            false,
            Some(SIZE),
            Some(rect(width)),
            Some(rect(70.0)),
            Some(rect(50.0)),
        )
    }
    #[test]
    fn compact_has_no_new_reservation() {
        assert_eq!(
            plan(false, true, false, None, None, None, None),
            Plan {
                paint: Paint::Compact,
                reservation: None
            }
        );
    }
    #[test]
    fn explicit_actions_are_idempotent_and_invalid_targets_do_nothing() {
        assert_eq!(requested_change(false, true, true), Some(true));
        assert_eq!(requested_change(true, true, true), None);
        assert_eq!(requested_change(true, false, true), Some(false));
        assert_eq!(requested_change(false, false, true), None);
        for current in [false, true] {
            for wanted in [false, true] {
                assert_eq!(requested_change(current, wanted, false), None);
            }
        }
    }
    #[test]
    fn exact_full_fallback_and_hidden_thresholds_preserve_envelope() {
        for (width, paint) in [
            (70.0, Paint::ExpandedSprite),
            (69.0, Paint::ExpandedCompactFallback),
            (50.0, Paint::ExpandedCompactFallback),
            (49.0, Paint::Hidden),
        ] {
            assert_eq!(
                choice(width),
                Plan {
                    paint,
                    reservation: Some(SIZE)
                }
            );
        }
    }
    #[test]
    fn focus_input_and_clipping_never_remove_expanded_reservation() {
        for admitted in [false, true] {
            for retreat in [false, true] {
                let p = plan(true, admitted, retreat, Some(SIZE), None, None, None);
                assert_eq!(p.reservation, Some(SIZE));
                assert_eq!(p.paint, Paint::Hidden);
            }
        }
    }
    #[test]
    fn missing_empty_negative_and_nonfinite_geometry_fail_closed() {
        for width in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(choice(width).paint, Paint::Hidden);
        }
        assert_eq!(
            plan(
                true,
                true,
                false,
                None,
                Some(rect(70.0)),
                Some(rect(70.0)),
                Some(rect(50.0))
            )
            .paint,
            Paint::Hidden
        );
        assert_eq!(
            plan(
                true,
                true,
                false,
                Some(Envelope {
                    width: 0,
                    height: 30
                }),
                Some(rect(70.0)),
                Some(rect(70.0)),
                None
            )
            .paint,
            Paint::Hidden
        );
    }
    #[test]
    fn positive_intersection_is_not_complete_fit() {
        let clip = Rect {
            x: 1.0,
            ..rect(69.0)
        };
        assert_eq!(
            plan(
                true,
                true,
                false,
                Some(SIZE),
                Some(clip),
                Some(rect(70.0)),
                Some(rect(50.0))
            )
            .paint,
            Paint::Hidden
        );
        let translated = Rect {
            x: -20.0,
            ..rect(70.0)
        };
        assert!(!rect(70.0).contains(translated));
    }
    #[test]
    fn captured_weak_target_never_retargets_replaced_or_closed_panes() {
        use std::rc::Rc;
        let first = Rc::new(1);
        let other = Rc::new(1);
        let target = Rc::downgrade(&first);
        assert!(resolve_current(&target, Some(&first)).is_some());
        assert!(resolve_current(&target, Some(&other)).is_none());
        assert!(resolve_current(&target, None).is_none());
        drop(first);
        assert!(resolve_current(&target, Some(&other)).is_none());
    }

    #[test]
    fn fit_uses_both_animation_poses_and_invalid_union_fails_closed() {
        let first = rect(70.0);
        let second = Rect { x: -2.0, ..first };
        let both = first.union(second).unwrap();
        assert!(!first.contains(both));
        assert_eq!(
            plan(
                true,
                true,
                false,
                Some(SIZE),
                Some(first),
                Some(both),
                Some(rect(50.0))
            )
            .paint,
            Paint::ExpandedCompactFallback
        );
        assert!(first.union(rect(f64::NAN)).is_none());
    }
}
