//! Pure logical-cursor properties. These do not model or certify VTE matching.
use super::*;

fn surface(index: usize, count: usize) -> FindSurface {
    FindSurface {
        block_id: 10 + (index / 2) as u64,
        block_index: 99 - index,
        is_output: index & 1 == 1,
        is_live: false,
        count,
        vte_cursor: None,
        pending_replay: None,
        complete: true,
        initial_wrap: false,
        wrap_before: Some(0),
        render_stamp: (80, 24, false, 7),
    }
}

// Expand stable identities independently of compressed cursor arithmetic.
fn flat(surfaces: &[FindSurface]) -> Vec<(u64, bool, usize)> {
    surfaces
        .iter()
        .flat_map(|s| (0..s.count).map(move |ordinal| (s.block_id, s.is_output, ordinal)))
        .collect()
}

fn cursor(surfaces: &[FindSurface], identity: (u64, bool, usize)) -> FindCursor {
    let surface = surfaces
        .iter()
        .position(|s| (s.block_id, s.is_output) == (identity.0, identity.1))
        .unwrap();
    FindCursor {
        surface,
        occurrence: identity.2,
        global: flat(surfaces)
            .iter()
            .position(|&value| value == identity)
            .unwrap(),
    }
}

fn domains(mut check: impl FnMut(&[FindSurface])) {
    // Exactly 340 nonempty domains. Every surface has 1..=4 occurrences,
    // preserving the production admission invariant (no zero-count surface).
    for length in 1..=4 {
        for encoded in 0..4usize.pow(length) {
            let mut value = encoded;
            let mut surfaces = Vec::new();
            for index in 0..length as usize {
                surfaces.push(surface(index, 1 + value % 4));
                value /= 4;
            }
            check(&surfaces);
        }
    }
}

#[test]
fn all_small_compressed_domains_match_flat_identity_navigation() {
    let mut checks = 0;
    domains(|surfaces| {
        let identities = flat(surfaces);
        for (position, &identity) in identities.iter().enumerate() {
            let current = cursor(surfaces, identity);
            for capped in [false, true] {
                for direction in [FindDirection::Next, FindDirection::Previous] {
                    let forward = matches!(direction, FindDirection::Next);
                    let boundary = if forward {
                        position + 1 == identities.len()
                    } else {
                        position == 0
                    };
                    let target_position = if capped && boundary {
                        position
                    } else if forward {
                        identities
                            .iter()
                            .enumerate()
                            .nth(position + 1)
                            .map_or(0, |(i, _)| i)
                    } else {
                        identities[..position]
                            .iter()
                            .enumerate()
                            .next_back()
                            .map_or(identities.len() - 1, |(i, _)| i)
                    };
                    let expected = cursor(surfaces, identities[target_position]);
                    let current_surface = &surfaces[current.surface];
                    let crosses_surface_edge = if forward {
                        current.occurrence + 1 == current_surface.count
                    } else {
                        current.occurrence == 0
                    };
                    let expected_reset = crosses_surface_edge && !(capped && boundary);
                    assert_eq!(step_compressed_cursor(surfaces, current, identities.len(), capped, direction),
                               Some((expected, expected_reset)),
                               "surfaces={surfaces:?} current={current:?} capped={capped} direction={direction:?}");
                    checks += 1;
                }
            }
        }
    });
    assert_eq!(checks, 12_520);
}

#[test]
fn resume_uses_record_surface_and_occurrence_after_every_rotation() {
    domains(|original| {
        for rotation in 0..original.len() {
            let mut surfaces = original.to_vec();
            surfaces.rotate_left(rotation);
            for identity in flat(&surfaces) {
                let resume = FindResume {
                    block_id: identity.0,
                    is_output: identity.1,
                    occurrence: identity.2,
                };
                let expected = cursor(&surfaces, identity);
                assert_eq!(resumed_find_cursor(&surfaces, resume), Some(expected));
                let mut unavailable = surfaces.clone();
                unavailable[expected.surface].is_live = true;
                assert_eq!(
                    resumed_find_cursor(&unavailable, resume),
                    None,
                    "live surface must not substitute a completed-record identity"
                );
                unavailable.remove(expected.surface);
                assert_eq!(
                    resumed_find_cursor(&unavailable, resume),
                    None,
                    "removed identity must not bind to the same vector index"
                );
                let past_end = FindResume {
                    occurrence: surfaces[expected.surface].count,
                    ..resume
                };
                assert_eq!(resumed_find_cursor(&surfaces, past_end), None);
            }
        }
    });
}

#[test]
fn every_native_cursor_action_rejects_stale_finished_render_identity() {
    let stamp = (80, 24, false, 7);
    // Each component is independently changed, plus unavailable provenance.
    let stale = [
        None,
        Some((81, 24, false, 7)),
        Some((80, 25, false, 7)),
        Some((80, 24, true, 7)),
        Some((80, 24, false, 8)),
    ];
    let mut checks = 0;
    let mut discriminating_checks = 0;
    for surface_index in [0, 1] {
        for count in 1..=8 {
            for complete in [false, true] {
                for current in std::iter::once(None).chain((0..count).map(Some)) {
                    for occurrence in 0..=count {
                        for direction in [FindDirection::Next, FindDirection::Previous] {
                            let mut finished = surface(surface_index, count);
                            finished.complete = complete;
                            finished.vte_cursor = current;
                            let current_action =
                                native_cursor_action(&finished, occurrence, direction);
                            assert_eq!(
                                validated_native_cursor_action(
                                    &finished,
                                    occurrence,
                                    direction,
                                    Some(stamp)
                                ),
                                current_action
                            );
                            for stale_stamp in stale {
                                assert_eq!(validated_native_cursor_action(&finished, occurrence, direction, stale_stamp), None,
                                           "count={count} complete={complete} current={current:?} occurrence={occurrence} direction={direction:?}");
                                checks += 1;
                                if current_action.is_some() {
                                    discriminating_checks += 1;
                                }
                            }
                            // Only the reserved active-live identity bypasses the
                            // finished-card stamp. Real live invalidation is checked
                            // by a separate production caller, outside this test.
                            finished.block_id = 0;
                            assert_eq!(
                                validated_native_cursor_action(
                                    &finished, occurrence, direction, None
                                ),
                                None
                            );
                            finished.block_id = 10;
                            finished.is_live = true;
                            assert_eq!(
                                validated_native_cursor_action(
                                    &finished, occurrence, direction, None
                                ),
                                None
                            );
                            finished.block_id = 0;
                            assert_eq!(
                                validated_native_cursor_action(
                                    &finished, occurrence, direction, None
                                ),
                                current_action
                            );
                        }
                    }
                }
            }
        }
    }
    assert_eq!(checks, 11_360);
    assert_eq!(discriminating_checks, 3_020);
}
