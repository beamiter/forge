#!/usr/bin/env bash
# Run only the explicitly named GTK/VTE regressions. Each test gets a fresh
# process, display, and session bus; sweeping `--ignored` would also execute
# manual diagnostics and micro-benchmarks that are not CI gates.

set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
cd "${PROJECT_ROOT}"

# A developer's font scale, density, history, or theme must not change the
# geometry asserted by these regressions. Give the suite an empty, private XDG
# tree just as CI gets from a fresh account; Cargo/Nix keep their own homes.
test_xdg_root="$(mktemp -d)"
cleanup() {
    rm -rf -- "${test_xdg_root}"
}
trap cleanup EXIT
mkdir -p \
    "${test_xdg_root}/config" \
    "${test_xdg_root}/data" \
    "${test_xdg_root}/state" \
    "${test_xdg_root}/cache" \
    "${test_xdg_root}/runtime"
chmod 700 "${test_xdg_root}/runtime"
export XDG_CONFIG_HOME="${test_xdg_root}/config"
export XDG_DATA_HOME="${test_xdg_root}/data"
export XDG_STATE_HOME="${test_xdg_root}/state"
export XDG_CACHE_HOME="${test_xdg_root}/cache"
export XDG_RUNTIME_DIR="${test_xdg_root}/runtime"

for command in dbus-run-session xvfb-run xdotool; do
    if ! command -v "${command}" >/dev/null 2>&1; then
        printf 'Error: %s is required for display-backed tests.\n' "${command}" >&2
        exit 1
    fi
done

# Nix's dbus-daemon keeps its session policy in the store instead of `/etc`.
# Pass the policy beside the selected daemon when available; distro installs
# continue to use their ordinary `/etc/dbus-1/session.conf` fallback.
dbus_run_args=()
dbus_daemon="$(command -v dbus-daemon)"
dbus_prefix="${dbus_daemon%/bin/dbus-daemon}"
dbus_store_config="${dbus_prefix}/share/dbus-1/session.conf"
if [[ -r "${dbus_store_config}" ]]; then
    dbus_run_args+=("--config-file=${dbus_store_config}")
elif [[ ! -r /etc/dbus-1/session.conf ]]; then
    printf 'Error: no D-Bus session configuration is readable.\n' >&2
    exit 1
fi

tests=(
    block_view::scroll::canvas_native_tests::retained_canvas_scrolls_through_frames_and_yields_to_reading
    ui::organism::inspector::tests::snapshot_dialog_is_scrollable_and_closes_without_an_animation_source
    ui::organism::tests::inspection_captures_real_state_without_attention_or_lifecycle_changes
    ui::organism::tests::one_companion_crosses_splits_without_cloning_its_body_or_mind
    ui::organism_vte::tests::one_companion_moves_between_vte_splits_without_resizing_grids
    ui::tasks_panel::tests::cached_selected_row_survives_no_focus_move_hide_and_reveal
    ui::tasks_panel::tests::task_filters_route_real_x11_keyboard_by_identity
    ui::tasks_panel::tests::task_filters_keep_identity_hidden_updates_and_keyboard_routing
    ui::tasks_panel::tests::thousand_task_filter_and_update_native_benchmark
    terminal::launch_owner::tests::successful_spawn_losing_close_and_exit_before_completion_retire_exact_ownership
    terminal::launch_recovery::tests::manual_recovery_is_single_flight_scoped_and_delivers_startup_once
    terminal::launch_recovery::tests::unavailable_cwd_falls_back_home_and_pending_commands_run_once
    terminal::launch_recovery::tests::close_and_exit_retire_pending_commands_and_background_retry_keeps_focus
    block_view::search_navigation_tests::native_search_identity_characterization
    ui::dialogs::native_palette_activation_tests::palette_activation_preserves_snapshot_unfold_failure_focus_and_stale_ownership
    ui::dialogs::tests::workflow_form_weak_callbacks_release_closed_widgets
    ui::tasks_panel::tests::approval_details_are_paged_read_only_and_stale_controls_cannot_decide
    block_view::incremental_find_tests::block_native_find_controls_keep_query_caret_and_tab_focus
    block_view::incremental_find_tests::chat_native_find_controls_keep_query_caret_and_tab_focus
    block_view::readline_preservation_tests::search_and_native_split_preserve_readline_draft_cursor_and_undo
    agent_task_ui::feedback_tests::task_feedback_keeps_input_draft_and_focus
    block_view::ime_gate::tests::preedit_lifecycle_tracks_each_live_context_without_text
    block_view::blocks::command_reuse_labels_tests::recall_header_describes_insert_only_action
    block_view::shortcut_hints::tests::menu_hints_follow_current_mapping_without_dispatch
    block_view::blocks::fold_preview_tests::folded_preview_tracks_filters_and_preserves_output
    ui::agent_panel::focus_tests::asynchronous_agent_renders_preserve_newer_focus_owners
    ui::tasks_panel::tests::dashboard_filters_preserve_selection_without_actions
    ui::tasks_panel::tests::dashboard_search_activate_selects_current_visible_result_only
    ui::tasks_panel::tests::dashboard_filter_counts_follow_search_not_selected_view
    ui::tasks_panel::tests::selected_project_focus_keeps_query_identity_and_rejects_hidden_selection
    vim9_ide::native_buffer::tests::retained_overlays_reuse_blink_frames_then_refresh_theme_contents_and_pango
    ui::tasks_panel::tests::task_terminal_buttons_only_emit_focus_for_existing_bindings
    ui::tasks_panel::tests::task_reason_updates_and_disappears_without_retargeting
    ui::tasks_panel::tests::hidden_tasks_offer_only_restore_and_hide_follows_manager_gates
    ui::agent_sessions::native_tests::session_manager_filters_preserves_selection_and_resumes_exact_argv
    ui::agent_sessions::native_tests::agent_management_v4_refresh_never_retargets_and_guards_live_resume
    ui::agent_sessions::session_bulk::native_tests::agent_management_v3_batch_controls_keep_exact_targets_and_drafts
    ui::tasks_panel::control_tests::task_control_center_preserves_search_action_identity_and_rejected_drafts
    ui::task_ops::focus_tests::task_terminal_focus_tracks_reordered_split_and_closed_leaf
    terminal::tests::explicit_focus_return_respects_reopened_search
    block_view::task_focus_tests::task_button_held_enter_never_submits_to_focused_terminal
    block_view::blocks::tests::folded_state_survives_virtualization_and_pane_filtering
    block_view::fold_geometry_tests::fold_actions_update_visible_and_parked_geometry
    block_view::search_navigation_tests::block_output_search_unfolds_only_target
    block_view::search_navigation_tests::chat_output_search_unfolds_only_target
    block_view::search_navigation_tests::block_live_find_excludes_retained_scrollback
    block_view::search_navigation_tests::chat_live_find_excludes_retained_scrollback
    block_view::search_navigation_tests::block_live_find_rebuilds_after_native_changes
    block_view::search_navigation_tests::chat_live_find_rebuilds_after_native_changes
    block_view::find::tests::live_skip_budget_rejects_without_navigation
    block_view::incremental_find_tests::block_incremental_find_reveals_exact_occurrence
    block_view::incremental_find_tests::chat_incremental_find_reveals_exact_occurrence
    block_view::incremental_find_tests::block_filtered_single_hit_reopens_without_taking_focus
    block_view::incremental_find_tests::chat_filtered_single_hit_reopens_without_taking_focus
    block_view::incremental_find_tests::block_resized_reveal_restores_last_filtered_occurrence
    block_view::incremental_find_tests::chat_resized_reveal_restores_last_filtered_occurrence
    block_view::incremental_find_tests::block_pending_replay_tracks_latest_navigation
    block_view::incremental_find_tests::chat_pending_replay_tracks_latest_navigation
    block_view::incremental_find_tests::block_pending_replay_retires_after_interruption
    block_view::incremental_find_tests::chat_pending_replay_retires_after_interruption
    block_view::incremental_find_tests::block_large_replay_preserves_narrowed_native_cursor
    block_view::incremental_find_tests::chat_large_replay_preserves_narrowed_native_cursor
    block_view::blocks::tests::live_clip_reserves_css_insets_without_an_extra_grid_row
    block_view::paste_feedback::tests::notice_preserves_focus
    block_view::input_feedback::tests::rejected_input_notice_is_bounded_and_recovers_exact_draft
    block_view::tests::truncated_clipboard_feedback_tracks_actual_delivery
    block_view::tests::ssh_remote_prompt_cursor_stays_inside_live_clip_without_clear
    block_view::tests::native_ime_commit_cancel_and_independent_ptys
    block_view::tests::native_ime_send_button_preserves_composition
    block_view::tests::delayed_clipboard_paste_preserves_newer_pane_search_and_empty_focus
    block_view::tests::delayed_clipboard_paste_restores_original_history_only_after_admission
    block_view::tests::delayed_clipboard_paste_closed_or_hidden_target_cannot_reclaim_focus
    block_view::tests::block_return_to_live_yields_to_newer_reading_and_keeps_search_focus
    block_view::tests::chat_return_to_live_yields_to_newer_reading_and_keeps_search_focus
    block_view::tests::existing_block_tab_switch_preserves_global_search_focus
    block_view::tests::existing_unified_tab_switch_preserves_global_search_focus
    block_view::tests::deferred_live_focus_restores_empty_owner_after_prompt_end_and_alt_exit
    block_view::tests::deferred_live_focus_never_steals_new_pane_find_or_history_ownership
    block_view::tests::deferred_live_focus_retires_hidden_detached_and_changed_root_targets
    ui::organism::tests::idle_refresh_skips_unchanged_tooltip_and_non_owner_spatial_render
    ui::organism::tests::hidden_sticky_heartbeat_catches_up_on_map_without_retaining_pane
    block_view::tests::chat_native_activate_preserves_send_and_stop_origins_through_delays
    block_view::tests::chat_stop_stays_bound_across_held_replay_and_pointer_or_key_press
    block_view::tests::chat_send_button_observes_production_queue_admission_and_held_boundaries
    block_view::tests::chat_long_prompt_shrinks_and_mapped_block_pane_keeps_focus
    block_view::cross_selection::tests::chat_selection_root_reaches_the_docked_native_composer
    block_view::blocks::tests::chat_pair_keeps_native_surfaces_and_gap_is_not_accumulated
    block_view::tests::chat_docking_keeps_pty_rows_and_native_hit_geometry_stable
    block_view::chat::tests::chat_moves_only_the_native_clip_and_keeps_history_anchor
    font::tests::context_menu_tracks_live_theme_and_font_without_parent_emphasis
    block_view::font_sync_tests::font_groups_stay_independent_across_native_panes_and_late_rows
    ui::dialogs::organism_preview::tests::preview_lifecycle_keeps_motion_local_and_stops_hidden_sources
    ui::organism::tests::deferred_agent_loss_retires_closed_panes_but_keeps_hidden_outcomes
    ui::organism::tests::live_settings_reuse_body_preserve_focus_and_stop_disabled_timers
    ui::organism::tests::agent_signal_uses_window_setting_when_one_native_pane_is_disabled
    ui::bottom_bar::tests::long_status_segments_keep_compact_geometry_and_full_text
    ui::file_tree::tests::path_dialog_deep_ancestors_keep_actions_and_focus_reachable
    ui::file_tree::tests::path_dialog_long_components_keep_open_visible_and_full_targets
    state::tests::failed_quarantine_blocks_the_real_window_autosave
    state::tests::rejected_window_layouts_survive_the_first_real_autosave
    font::tests::an_explicit_appended_fallback_resolves_on_a_real_vte
    block_view::history::tests::unified_pane_lifecycle_preserves_failed_and_incomplete_restore_bytes
    block_view::history::tests::unified_pane_fresh_drop_reopen_and_runtime_retention_remain_writable
    block_view::history::tests::explicit_clear_cancels_a_pending_history_retry_without_stale_failure
    block_view::history::tests::retry_save_again_reports_repeated_failure_from_real_pane
    block_view::history::tests::retry_reload_first_reports_repeated_failure_from_real_pane
    block_view::history::tests::failed_zone_history_notice_wraps_and_explains_recovery
    block_view::tests::block_navigation_releases_feed_without_a_history_borrow
    block_view::cross_selection::tests::a_single_native_text_selection_survives_whole_card_selection_precedence
    ui::tabs::tests::failed_remote_launch_diagnostics_never_mark_the_attempt_connected
    ui::sidebar_tabs::tests::many_tab_switches_update_mirror_before_notebook_commits
    ui::dialogs::tests::cross_block_search_compact_rows_are_inside_horizontal_scroller
    ui::dialogs::tests::cross_block_search_close_borrows_the_gtk_dialog_without_releasing_its_slot
    ui::panes::tests::equalize_without_allocation_leaves_the_tree_untouched
    block_view::blocks::tests::diag_short_ls_block_geometry
    block_view::blocks::tests::block_density_switches_on_widgets_that_already_exist
    block_view::find::tests::a_card_vte_must_drop_its_previous_anchor_before_a_fresh_query
    block_view::find::tests::unified_vte_fresh_query_reaches_scrollback_before_a_prior_match
    block_view::find::tests::unified_bounded_and_native_fallback_prefer_visible_match_with_huge_old_scrollback
    block_view::find::tests::unified_complete_windows_step_visible_then_wrapped_history_on_real_vte
    block_view::find::tests::live_find_counts_what_the_live_vte_search_steps_through
    block_view::blocks::tests::a_precomputed_card_does_not_rewalk_its_transcript
    block_view::blocks::tests::lifecycle_chip_and_quick_actions_expose_truthful_status
    block_view::scroll::tests::widget_pool_releases_heavy_children_and_stale_controllers
    block_view::tests::entering_alt_screen_ends_the_block_selection_it_hides
    block_view::tests::alt_screen_chrome_keeps_the_running_cards_geometry
    block_view::tests::a_real_vte_focus_report_reaches_the_program_but_is_not_typing
    block_view::tests::a_finished_codex_session_keeps_its_history_in_the_card_and_block_data
    block_view::tests::header_button_presses_do_not_select_and_a_hidden_readout_measures
    block_view::tests::a_real_cards_header_buttons_do_not_select_it
    block_view::tests::a_dropped_path_reaches_the_program_as_a_bracketed_paste
    block_view::tests::slow_file_drop_keeps_gtk_responsive_and_discards_stale_delivery
    block_view::tests::an_alt_screen_round_trip_keeps_a_real_running_cards_height
    block_view::tests::a_truncated_output_shows_its_notice_on_a_real_card
    terminal::tests::a_failed_vte_spawn_retires_task_sessions_without_an_exit_code
    terminal::tests::an_osc8_label_opens_its_target_on_a_real_vte
    block_view::css::tests::the_generated_stylesheet_parses_without_error
    font::tests::default_font_resolution_preserves_grid_and_configuration
    font::tests::a_real_vte_is_given_the_icon_family_behind_the_configured_one
    block_view::onboarding::tests::inline_notice_priority_handles_multiple_cards_reparenting_and_narrow_panes
    block_view::onboarding::tests::block_onboarding_overlay_is_non_measuring_and_non_targetable
    block_view::tests::a_stranded_focus_mount_declines_only_what_the_focused_widget_owns
    block_view::blocks::tests::repeated_action_feedback_restores_the_original_affordance
    block_view::review::tests::closing_review_preserves_scrolled_history_and_invoking_control
    block_view::tests::block_review_production_actions_are_read_only_responsive_and_revalidated
    block_view::tests::persistent_block_actions_reopen_without_duplicates_or_pty_input
    block_view::blocks::tests::outputless_cards_cannot_create_a_phantom_collapsed_summary
    block_view::blocks::tests::long_context_uses_one_flexible_accessible_summary
    block_view::blocks::tests::block_actions_remain_discoverable_without_hover
    block_view::blocks::tests::pointer_leave_never_disables_keyboard_focused_actions
    block_view::blocks::tests::revealing_a_cards_actions_does_not_move_its_metadata
    block_view::blocks::tests::the_selection_hint_sits_on_the_spacers_left
    block_view::tests::a_refusal_flash_is_visible_exposed_and_restores_only_the_latest_status
    block_view::tests::late_inline_notice_adopts_the_panes_current_density
    block_view::tests::dock_mount_refuses_a_widget_another_region_owns
    block_view::tests::unified_search_capture_real_vte_uses_half_open_column_boundary
    block_view::tests::real_vte_prompt_area_covers_a_menu_after_the_pane_has_scrolled
    block_view::unified_chrome::tests::real_vte_osc8_row_probe_and_rewrap_smoke
    block_view::unified_chrome::tests::real_vte_non_bottom_anchor_calibrates_and_wide_badge_probe_fails_closed
    block_view::unified_navigation_tests::unified_navigation_preserves_reading_identity_selection_and_tui
    block_view::unified_images::tests::real_vte_keeps_nonzero_marker_column_through_narrow_wide_rewrap
)

export RUST_TEST_THREADS=1
export GDK_BACKEND=x11
export GSK_RENDERER=cairo
export LIBGL_ALWAYS_SOFTWARE=1
export NO_AT_BRIDGE=1
export GTK_A11Y=none

listed_tests="$(cargo test --lib --all-features --locked -- --ignored --list)"
for test_name in "${tests[@]}"; do
    if ! grep -Fqx -- "${test_name}: test" <<<"${listed_tests}"; then
        printf 'Error: GTK display regression is not registered: %s\n' "${test_name}" >&2
        exit 1
    fi
done

# One diagnostic replay only, for this exact failed fixture. This function's
# result never replaces the original cargo-test failure status below.
capture_vte_crash() {
    local original_status="$1" test_binary diagnostic_status=0
    local diagnostic_dir="${GTK_PAINT_DIAGNOSTICS_DIR:-$PROJECT_ROOT/gtk-paint-diagnostics}/f08-native-crash"
    mkdir -p "$diagnostic_dir" || return
    printf 'F08 original cargo-test exit status: %s\n' "$original_status" | tee "$diagnostic_dir/status.txt"
    command -v gdb >/dev/null || return
    command -v timeout >/dev/null || return
    # Resolve the exact current lib-test executable from Cargo's structured
    # artifact output; never guess a hash or select a stale directory entry.
    cargo test --lib --all-features --locked --no-run --message-format=json \
        > "$diagnostic_dir/cargo-artifacts.jsonl" || return
    test_binary="$(python3 - "$diagnostic_dir/cargo-artifacts.jsonl" <<'PY_BINARY'
import json, sys
paths = set()
for line in open(sys.argv[1]):
    item = json.loads(line)
    if item.get("reason") == "compiler-artifact" and item.get("executable") and item.get("profile", {}).get("test") and "lib" in item.get("target", {}).get("kind", []):
        paths.add(item["executable"])
if len(paths) != 1:
    raise SystemExit(f"Expected exactly one current lib-test binary, got {paths!r}")
print(paths.pop())
PY_BINARY
    )" || return
    {
        printf 'Exact fixture: %s\n' "$test_name"
        git rev-parse HEAD
        sha256sum "$test_binary" src/ui/organism_vte.rs src/ui/organism_vte/lifecycle_tests.rs scripts/test-gtk-display.sh .github/workflows/ci.yml
        gdb --version
    } > "$diagnostic_dir/identity.txt" || return
    # Fresh bus/display, bounded replay. --return-child-result retains the
    # inferior failure; the caller ALSO retains the original execution status.
    GTK_IM_MODULE="$fixture_im_module" GTK_A11Y="$a11y_backend" \
        timeout --signal=TERM --kill-after=10s 240s \
        dbus-run-session "${dbus_run_args[@]}" -- \
        xvfb-run --auto-servernum --server-args='-screen 0 1280x800x24 -nolisten tcp' \
        gdb --batch --return-child-result \
            -ex 'set pagination off' -ex 'set confirm off' \
            -ex 'set debuginfod enabled off' -ex run -ex 'thread apply all bt full' \
            --args "$test_binary" "$test_name" --ignored --exact --nocapture \
        > "$diagnostic_dir/backtrace.txt" 2>&1 || diagnostic_status=$?
    cat "$diagnostic_dir/identity.txt" "$diagnostic_dir/backtrace.txt"
    printf 'F08 diagnostic replay exit status: %s; original status remains: %s\n' \
        "$diagnostic_status" "$original_status" | tee -a "$diagnostic_dir/status.txt"
}

for test_name in "${tests[@]}"; do
    printf 'GTK display regression: %s\n' "${test_name}"
    # These fixtures check accessible values; GTK caches the backend at init.
    a11y_backend=none
    if [[ "$test_name" == ui::organism::tests::live_settings_reuse_body_preserve_focus_and_stop_disabled_timers || "$test_name" == ui::tasks_panel::tests::approval_details_are_paged_read_only_and_stale_controls_cannot_decide ]]; then
        a11y_backend="test"
    fi
    fixture_im_module="${GTK_IM_MODULE-}"
    if [[ "$test_name" == block_view::tests::native_ime_commit_cancel_and_independent_ptys || "$test_name" == block_view::tests::native_ime_send_button_preserves_composition ]]; then
        fixture_im_module=simple
    fi
    if GTK_IM_MODULE="$fixture_im_module" GTK_A11Y="$a11y_backend" dbus-run-session "${dbus_run_args[@]}" -- \
        xvfb-run --auto-servernum --server-args='-screen 0 1280x800x24 -nolisten tcp' \
        cargo test --lib --all-features --locked "${test_name}" -- \
        --ignored --exact --nocapture; then
        :
    else
        original_status=$?
        if [[ "${FORGE_NATIVE_CRASH_DIAGNOSTICS:-0}" == 1 && "$test_name" == ui::organism::tests::live_settings_reuse_body_preserve_focus_and_stop_disabled_timers ]]; then
            capture_vte_crash "$original_status" || printf 'F08 diagnostic collection failed; preserving original status %s\n' "$original_status" >&2
        fi
        exit "$original_status"
    fi
done

printf 'GTK display regressions passed: %d/%d.\n' "${#tests[@]}" "${#tests[@]}"
