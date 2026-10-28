//! Pure font-group policy; deliberately independent of GTK so it can be
//! exercised without a display, D-Bus, PTYs or application initialization.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FontGroup {
    Chrome,
    Content,
}

pub(crate) fn clamp_scale(scale: f64) -> f64 {
    if scale.is_finite() {
        scale.clamp(0.1, 10.0)
    } else {
        1.0
    }
}

/// Missing (legacy) or invalid chrome values inherit the resolved content
/// value exactly once at load time, not on subsequent content zooms.
pub(crate) fn initial_chrome_scale(chrome: Option<f64>, content: f64) -> f64 {
    clamp_scale(chrome.filter(|value| value.is_finite()).unwrap_or(content))
}

/// `steps` is normalized from GTK's surface pixels by the event adapter.
/// Preserve fractional touchpad deltas rather than rounding each event to a
/// full notch. Some devices batch notches; their magnitude must be preserved.
/// A consumed event at the limit still returns Some, preventing scroll leaks.
pub(crate) fn wheel_scale(current: f64, control: bool, steps: f64) -> Option<f64> {
    if !control || !steps.is_finite() || steps == 0.0 {
        return None;
    }
    Some((clamp_scale(current) - steps * 0.025).clamp(0.1, 10.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-12, "{actual} != {expected}");
    }

    #[test]
    fn old_config_inherits_once_and_new_config_reloads_independently() {
        near(initial_chrome_scale(None, 1.75), 1.75);
        near(initial_chrome_scale(Some(0.8), 1.75), 0.8);
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            near(initial_chrome_scale(Some(invalid), 1.75), 1.75);
        }
        near(initial_chrome_scale(Some(-1.0), 2.0), 0.1);
        near(initial_chrome_scale(Some(12.0), 2.0), 10.0);
    }

    #[test]
    fn legacy_main_only_then_restart_keeps_chrome_fixed() {
        let legacy_content = 1.75;
        let chrome = initial_chrome_scale(None, legacy_content);
        let content = wheel_scale(legacy_content, true, -4.0).unwrap();
        // Persistence writes both effective values, even on the first edit.
        near(initial_chrome_scale(Some(chrome), content), 1.75);
        near(content, 1.85);
    }

    #[test]
    fn legacy_chrome_only_then_restart_keeps_content_fixed() {
        let content = 1.75;
        let chrome = wheel_scale(initial_chrome_scale(None, content), true, 4.0).unwrap();
        near(initial_chrome_scale(Some(chrome), content), 1.65);
        near(content, 1.75);
    }

    #[test]
    fn ordinary_horizontal_zero_and_invalid_scroll_pass_through() {
        for delta in [-10.0, -0.001, 0.0, 0.1, 20.0, f64::NAN] {
            assert_eq!(wheel_scale(1.0, false, delta), None);
        }
        for delta in [0.0, -0.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(wheel_scale(1.0, true, delta), None);
        }
    }

    #[test]
    fn fractional_and_batched_steps_have_the_same_total() {
        let mut scale = 1.0;
        for _ in 0..100 {
            scale = wheel_scale(scale, true, -0.01).unwrap();
        }
        near(scale, wheel_scale(1.0, true, -1.0).unwrap());
        near(wheel_scale(1.0, true, -4.0).unwrap(), 1.1);
        near(wheel_scale(scale, true, 1.0).unwrap(), 1.0);
    }

    #[test]
    fn clamp_limits_consume_events_and_reverse_immediately() {
        near(wheel_scale(10.0, true, -1.0).unwrap(), 10.0);
        near(wheel_scale(0.1, true, 1.0).unwrap(), 0.1);
        near(wheel_scale(10.0, true, 1.0).unwrap(), 9.975);
        near(wheel_scale(0.1, true, -1.0).unwrap(), 0.125);
        near(wheel_scale(1.0, true, f64::MAX).unwrap(), 0.1);
        near(wheel_scale(1.0, true, -f64::MAX).unwrap(), 10.0);
    }

    #[test]
    fn alternating_groups_never_change_the_other_scale() {
        let mut scales = [1.75, 0.8];
        for (group, delta) in [(FontGroup::Chrome, -1.0), (FontGroup::Content, 2.0)] {
            let index = match group {
                FontGroup::Chrome => 0,
                FontGroup::Content => 1,
            };
            let other = scales[1 - index];
            scales[index] = wheel_scale(scales[index], true, delta).unwrap();
            assert_eq!(scales[1 - index], other);
        }
        near(scales[0], 1.775);
        near(scales[1], 0.75);
    }

    // Source-level wiring guards complement arithmetic tests. These do not
    // claim to simulate GTK picking, native popovers, or rendered pixels.
    #[test]
    fn routing_uses_disjoint_pointer_subtrees_not_focus_or_window_capture() {
        let source = include_str!("main.rs");
        let route = source
            .split("// Each capture controller owns")
            .nth(1)
            .unwrap()
            .split("// Save state *before*")
            .next()
            .unwrap();
        for root in [
            "top_bar_handle",
            "sidebar",
            "canvas_header",
            "notebook",
            "search_bar",
            "side_stack",
            "bottom_bar",
        ] {
            assert!(route.contains(&format!("{root}.clone().upcast::<gtk4::Widget>()")));
        }
        let compact: String = route.chars().filter(|c| !c.is_whitespace()).collect();
        assert_eq!(compact.matches("FontGroup::Chrome),").count(), 5);
        assert_eq!(compact.matches("FontGroup::Content),").count(), 2);
        assert!(compact.contains("font_roots.push((composer,FontGroup::Content));"));
        assert!(
            !compact.contains("(right_col.clone().upcast::<gtk4::Widget>(),FontGroup::Content)")
        );
        assert!(
            compact.contains("(canvas_header.clone().upcast::<gtk4::Widget>(),FontGroup::Chrome)")
        );
        assert!(route.contains("PropagationPhase::Capture"));
        assert!(route.contains("SURFACE_UNITS_PER_STEP"));
        assert!(route.contains("return true.into()"));
        assert!(!route.contains("window.add_controller"));
        assert!(!route.contains("active_terminal"));
        assert!(!route.contains(".focus()"));
    }

    #[test]
    fn settings_size_and_scale_callbacks_target_only_their_own_group() {
        let source = include_str!("ui/dialogs.rs");
        let main_size = source
            .split("font_size_row.connect_notify_local")
            .nth(1)
            .unwrap()
            .split("// --- Signal: Font Scale ---")
            .next()
            .unwrap();
        assert!(main_size.contains("font_desc = new_desc"));
        assert!(!main_size.contains("chrome_font_size ="));
        let chrome_size = source
            .split("chrome_size_row.connect_notify_local")
            .nth(1)
            .unwrap()
            .split("chrome_scale_row.connect_notify_local")
            .next()
            .unwrap();
        assert!(chrome_size.contains("chrome_font_size ="));
        assert!(!chrome_size.contains("font_desc ="));
        assert!(!chrome_size.contains("apply_font_all"));
        let chrome_scale = source
            .split("chrome_scale_row.connect_notify_local")
            .nth(1)
            .unwrap()
            .split("// --- Signal: Opacity ---")
            .next()
            .unwrap();
        assert!(chrome_scale.contains("FontGroup::Chrome"));
        assert!(!chrome_scale.contains("apply_font_scale("));
    }

    #[test]
    fn both_scales_have_separate_idle_slots_and_one_persistence_debounce() {
        let source = include_str!("ui/config_apply.rs");
        let apply = source
            .split("pub(crate) fn apply_font_group_scale")
            .nth(1)
            .unwrap()
            .split("pub(crate) fn set_font_scale_all")
            .next()
            .unwrap();
        assert!(apply.contains("default_font_scale = new_scale"));
        assert!(apply.contains("chrome_font_scale = new_scale"));
        assert!(apply.contains("&self.pending_font_scale"));
        assert!(apply.contains("&self.pending_chrome_font_scale"));
        assert_eq!(apply.matches("ui.persist_config()").count(), 1);
        assert!(source.contains("self.pending_chrome_font_scale.set(None)"));
    }
}
