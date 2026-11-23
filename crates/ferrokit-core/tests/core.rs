// SPDX-License-Identifier: LGPL-3.0-only
//! Single integration-test binary (one link step instead of one per file).

#[path = "layered_tests.rs"]
mod layered_tests;
#[path = "test_advanced_event_system.rs"]
mod test_advanced_event_system;
#[path = "test_application_exit_from_worker_thread.rs"]
mod test_application_exit_from_worker_thread;
#[path = "test_application_process_events_in_exec.rs"]
mod test_application_process_events_in_exec;
#[path = "test_blocking_queued_bound.rs"]
mod test_blocking_queued_bound;
#[path = "test_core_data_structures.rs"]
mod test_core_data_structures;
#[path = "test_core_runtime_extensions.rs"]
mod test_core_runtime_extensions;
#[path = "test_core_small_semantics.rs"]
mod test_core_small_semantics;
#[path = "test_data_parse_hardening.rs"]
mod test_data_parse_hardening;
#[path = "test_delete_later_destroys_subtree.rs"]
mod test_delete_later_destroys_subtree;
#[path = "test_destroyed_signal.rs"]
mod test_destroyed_signal;
#[path = "test_epoll_real_fds.rs"]
mod test_epoll_real_fds;
#[path = "test_event_queue_merge_index.rs"]
mod test_event_queue_merge_index;
#[path = "test_exec_cleanup_deferred_delete.rs"]
mod test_exec_cleanup_deferred_delete;
#[path = "test_filter_consumed_returns_true.rs"]
mod test_filter_consumed_returns_true;
#[path = "test_future_watcher_thread.rs"]
mod test_future_watcher_thread;
#[path = "test_install_event_filter_self_and_mutual.rs"]
mod test_install_event_filter_self_and_mutual;
#[path = "test_io_system.rs"]
mod test_io_system;
#[path = "test_kill_timer_ownership.rs"]
mod test_kill_timer_ownership;
#[path = "test_loop_teardown_events.rs"]
mod test_loop_teardown_events;
#[path = "test_loop_timer_fixes.rs"]
mod test_loop_timer_fixes;
#[path = "test_loop_wm_quit.rs"]
mod test_loop_wm_quit;
#[path = "test_meta_invoke_connection_type.rs"]
mod test_meta_invoke_connection_type;
#[path = "test_meta_invoke_overload_signal.rs"]
mod test_meta_invoke_overload_signal;
#[path = "test_meta_invoke_queued_resolution.rs"]
mod test_meta_invoke_queued_resolution;
#[path = "test_meta_object_system.rs"]
mod test_meta_object_system;
#[path = "test_meta_slots.rs"]
mod test_meta_slots;
#[path = "test_miri_aliasing_controls.rs"]
mod test_miri_aliasing_controls;
#[path = "test_move_to_thread_handler_adds_child.rs"]
mod test_move_to_thread_handler_adds_child;
#[path = "test_move_to_thread_state.rs"]
mod test_move_to_thread_state;
#[path = "test_move_to_thread_three_levels.rs"]
mod test_move_to_thread_three_levels;
#[path = "test_move_to_thread_with_borrowed_data.rs"]
mod test_move_to_thread_with_borrowed_data;
#[path = "test_object_data_encapsulation.rs"]
mod test_object_data_encapsulation;
#[path = "test_object_timer_native_release.rs"]
mod test_object_timer_native_release;
#[path = "test_owned_child_destroyed_in_callback.rs"]
mod test_owned_child_destroyed_in_callback;
#[path = "test_owned_child_not_read_during_its_callback.rs"]
mod test_owned_child_not_read_during_its_callback;
#[path = "test_owned_child_ownership.rs"]
mod test_owned_child_ownership;
#[path = "test_owned_closure_access.rs"]
mod test_owned_closure_access;
#[path = "test_owned_drop_after_thread_move.rs"]
mod test_owned_drop_after_thread_move;
#[path = "test_owned_drop_forwarded.rs"]
mod test_owned_drop_forwarded;
#[path = "test_owned_object.rs"]
mod test_owned_object;
#[path = "test_panic_containment.rs"]
mod test_panic_containment;
#[path = "test_parent_children_during_callbacks.rs"]
mod test_parent_children_during_callbacks;
#[path = "test_parent_tree_rules.rs"]
mod test_parent_tree_rules;
#[path = "test_parked_ancestor_frees_running_descendant.rs"]
mod test_parked_ancestor_frees_running_descendant;
#[path = "test_pointer_registration_between_callbacks.rs"]
mod test_pointer_registration_between_callbacks;
#[path = "test_post_before_loop.rs"]
mod test_post_before_loop;
#[path = "test_posted_payload_dropped_outside_the_locks.rs"]
mod test_posted_payload_dropped_outside_the_locks;
#[path = "test_precise_timer_contract.rs"]
mod test_precise_timer_contract;
#[path = "test_precise_timer_lifecycle.rs"]
mod test_precise_timer_lifecycle;
#[path = "test_property_clone_and_race.rs"]
mod test_property_clone_and_race;
#[path = "test_qobject_lifecycle_and_hierarchy.rs"]
mod test_qobject_lifecycle_and_hierarchy;
#[path = "test_qobject_safety_and_qt6_features.rs"]
mod test_qobject_safety_and_qt6_features;
#[path = "test_queued_delivery_regressions.rs"]
mod test_queued_delivery_regressions;
#[path = "test_quit_event_veto.rs"]
mod test_quit_event_veto;
#[path = "test_recursive_mutex.rs"]
mod test_recursive_mutex;
#[path = "test_registry_child_index.rs"]
mod test_registry_child_index;
#[path = "test_serialization_and_json.rs"]
mod test_serialization_and_json;
#[path = "test_serialization_bounds.rs"]
mod test_serialization_bounds;
#[path = "test_settimer_cross_thread.rs"]
mod test_settimer_cross_thread;
#[path = "test_signal_connection_identity.rs"]
mod test_signal_connection_identity;
#[path = "test_signal_direct_and_queued.rs"]
mod test_signal_direct_and_queued;
#[path = "test_signal_emit_while_mutating.rs"]
mod test_signal_emit_while_mutating;
#[path = "test_signal_local_context.rs"]
mod test_signal_local_context;
#[path = "test_signal_local_slots.rs"]
mod test_signal_local_slots;
#[path = "test_signal_owner_binding.rs"]
mod test_signal_owner_binding;
#[path = "test_signal_owner_liveness.rs"]
mod test_signal_owner_liveness;
#[path = "test_signal_sender_destroyed_during_emit.rs"]
mod test_signal_sender_destroyed_during_emit;
#[path = "test_signal_subscriber_drop_reentrancy.rs"]
mod test_signal_subscriber_drop_reentrancy;
#[path = "test_signal_unique_single_shot.rs"]
mod test_signal_unique_single_shot;
#[path = "test_thread_lifecycle.rs"]
mod test_thread_lifecycle;
#[path = "test_thread_sync_regressions.rs"]
mod test_thread_sync_regressions;
#[path = "test_thread_system.rs"]
mod test_thread_system;
#[path = "test_timer_context_single_shot.rs"]
mod test_timer_context_single_shot;
#[path = "test_timer_huge_interval.rs"]
mod test_timer_huge_interval;
#[path = "test_timer_local_single_shot.rs"]
mod test_timer_local_single_shot;
#[path = "test_timer_no_busy_wait.rs"]
mod test_timer_no_busy_wait;
#[path = "test_timer_pinned_start.rs"]
mod test_timer_pinned_start;
#[path = "test_timer_restart.rs"]
mod test_timer_restart;
#[path = "test_timer_single_shot_before_loop.rs"]
mod test_timer_single_shot_before_loop;
#[path = "test_timer_thread_affinity.rs"]
mod test_timer_thread_affinity;
#[path = "test_unique_connection_slot_identity.rs"]
mod test_unique_connection_slot_identity;
#[path = "test_variant_extensibility.rs"]
mod test_variant_extensibility;
