# TorrentNG Storage Move/Import Certification

- Generated: 2026-10-02T21:16:27Z
- Host: kspld0
- Commit: db4e333c
- Hardware root: /mnt/disks/gamespool1
- Hardware files: 64
- Hardware MiB/file: 1

| Gate | Result |
| --- | --- |
| storage planner/executor unit tests | PASS |

## storage planner/executor unit tests

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.04s
     Running unittests src/lib.rs (target/debug/deps/rt_storage-5b71d17a8abf4e27)

running 67 tests
test plan::tests::checkpointed_delete_rejects_a_recreated_target ... ok
test plan::tests::checkpointed_rename_without_live_proof_is_rejected ... ok
test plan::tests::cancellation_before_next_step_rolls_back_prior_staging ... ok
test plan::tests::delete_requires_prior_dry_run_approval ... ok
test plan::tests::descriptor_anchored_execution_rejects_an_ancestor_symlink ... ok
test plan::tests::cancellation_after_checkpoint_rolls_back_committed_staging ... ok
test plan::tests::descriptor_anchored_rename_never_replaces_a_new_destination ... ok
test plan::tests::checkpoint_failure_leaves_committed_step_resumable ... ok
test plan::tests::copy_verification_failure_removes_partial_file_destination ... ok
test plan::tests::cleanup_plan_is_idempotent_and_prunes_only_empty_directories ... ok
test plan::tests::descriptor_anchored_rename_checks_size_before_mutating ... ok
test plan::tests::controlled_copy_aborts_mid_step_and_rolls_back_staging ... ok
test plan::tests::controlled_delete_after_partial_removal_requires_manual_recovery ... ok
test plan::tests::execute_copy_verify_plan_rejects_symlink_source_file ... ok
test plan::tests::execute_copy_verify_plan_rolls_back_staged_file_on_short_copy ... ok
test plan::tests::durable_rename_checkpoint_reconciles_after_source_is_removed ... ok
test plan::tests::execute_delete_plan_removes_symlink_without_following_target ... ok
test plan::tests::execute_copy_verify_plan_rejects_nested_symlink_entry ... ok
test plan::tests::execute_delete_plan_removes_directory_tree_after_approval ... ok
test plan::tests::execute_import_copy_failure_does_not_leave_final_destination ... ok
test plan::tests::execute_import_plan_rejects_stale_expected_size_without_destination ... ok
test plan::tests::execute_import_plan_rejects_symlink_source_before_hardlink ... ok
test plan::tests::copy_directory_verification_failure_removes_partial_destination ... ok
test plan::tests::execute_move_plan_rejects_symlink_source_before_rename ... ok
test plan::tests::execute_import_plan_links_or_copies_without_removing_source ... ok
test plan::tests::execute_move_plan_renames_without_overwrite ... ok
test plan::tests::execute_copy_verify_plan_copies_directory_tree_and_verifies_bytes ... ok
test plan::tests::execute_plan_reports_rollback_step_failure_in_error_message ... ok
test plan::tests::execute_plan_under_roots_refuses_to_delete_the_storage_root ... ok
test plan::tests::execute_plan_under_roots_rejects_delete_escape ... ok
test plan::tests::execute_plan_under_roots_rejects_destination_escape_before_copy ... ok
test plan::tests::failure_after_destructive_step_requires_manual_recovery ... ok
test plan::tests::execute_plan_under_roots_rejects_parent_dir_components ... ok
test plan::tests::execute_rejects_an_out_of_range_checkpoint_before_mutating ... ok
test plan::tests::execute_plan_under_roots_allows_confined_move ... ok
test plan::tests::import_copy_plan_stages_before_final_rename ... ok
test plan::tests::import_plan_detects_existing_destination ... ok
test plan::tests::move_plan_fails_closed_when_atomic_rename_is_unavailable ... ok
test plan::tests::move_plan_reports_conflict_and_capacity ... ok
test plan::tests::failed_checkpoint_can_resume_without_repeating_filesystem_mutation ... ok
test plan::tests::move_plan_uses_rename_on_same_filesystem ... ok
test plan::tests::plan_delete_treats_broken_symlink_as_existing_source ... ok
test plan::tests::fully_checkpointed_cross_filesystem_move_reconciles_after_source_delete ... ok
test plan::tests::portable_execute_step_import_hardlinks_or_copies_without_removing_source ... ok
test plan::tests::portable_execute_step_prune_empty_dirs_removes_up_to_root ... ok
test plan::tests::move_plan_copy_verify_path_deletes_source_after_verified_rename ... ok
test plan::tests::plan_import_treats_broken_destination_symlink_as_existing ... ok
test plan::tests::portable_execute_step_rejects_an_ancestor_symlink_swapped_after_validation ... ok
test plan::tests::portable_execute_step_rejects_symlink_source ... ok
test plan::tests::portable_execute_step_copy_verify_rename_copies_and_verifies_content ... ok
test plan::tests::portable_execute_step_safe_delete_if_present_is_idempotent_when_missing ... ok
test plan::tests::portable_execute_step_renames_file ... ok
test plan::tests::portable_execute_step_safe_delete_removes_file ... ok
test plan::tests::portable_rollback_plan_runs_configured_rollback_steps ... ok
test plan::tests::reconcile_rejects_conflicting_rename_as_manual_recovery ... ok
test plan::tests::rename_rejects_a_stale_expected_size_before_moving ... ok
test plan::tests::reconcile_rejects_corrupt_staging_copy ... ok
test plan::tests::staged_import_copy_fails_closed_when_atomic_rename_is_unavailable ... ok
test plan::tests::reconcile_infers_copy_when_following_rename_is_already_committed ... ok
test plan::tests::move_plan_copy_verify_path_removes_source_directory_after_verified_rename ... ok
test plan::tests::verify_content_matches_detects_bit_flip_despite_matching_length ... ok
test plan::tests::reconcile_rejects_unverifiable_rename_committed_before_checkpoint ... ok
test plan::tests::repeating_a_completed_copy_plan_is_idempotent ... ok
test plan::tests::verify_content_matches_rejects_source_symlink_swap_during_final_scan ... ok
test plan::tests::verify_content_matches_rejects_extra_destination_entries ... ok
test plan::tests::retrying_a_completed_copy_plan_validates_checkpoint_state ... ok
test plan::tests::portable_tree_walks_reject_overdeep_directories_and_clean_partial_copy ... ok

test result: ok. 67 passed; 0 failed; 0 ignored; 0 measured; 187 filtered out; finished in 0.02s

     Running tests/storage_move_import_hardware.rs (target/debug/deps/storage_move_import_hardware-e03958a2f5331ea8)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s

     Running tests/storage_real_device.rs (target/debug/deps/storage_real_device-9d55b69a9f6c1e1e)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.00s

```
| full storage unit suite | PASS |

## full storage unit suite

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.04s
     Running unittests src/lib.rs (target/debug/deps/rt_storage-5b71d17a8abf4e27)

running 254 tests
test alloc_audit::tests::allocated_range_query_clamps_to_the_file_and_ignores_garbage ... ok
test alloc_audit::tests::allocated_range_paging_continues_from_the_last_range_until_done ... ok
test alloc_audit::tests::allocated_range_query_reports_unsupported_as_none ... ok
test alloc_audit::tests::allocated_range_query_propagates_real_errors ... ok
test alloc_audit::tests::allocated_range_query_over_an_empty_file_asks_nothing ... ok
test alloc_audit::tests::allocated_range_query_terminates_when_a_page_makes_no_progress ... ok
test alloc_audit::tests::allocated_ranges_become_holes_between_and_after_them ... ok
test alloc_audit::tests::extents_past_end_of_file_are_clamped ... ok
test alloc_audit::tests::extents_with_no_gaps_yield_no_ranges ... ok
test alloc_audit::tests::empty_extent_list_is_one_whole_file_hole ... ok
test alloc_audit::tests::leading_middle_and_trailing_holes_are_reported ... ok
test alloc_audit::tests::merged_coalesces_adjacent_and_overlapping_ranges ... ok
test alloc_audit::tests::missing_file_is_an_io_error_not_unsupported ... ok
test alloc_audit::tests::unwritten_extents_are_separate_from_holes ... ok
test backend::tests::backend_request_parses_user_values ... ok
test backend::tests::fixed_buffer_strategy_names_are_stable_for_metrics ... ok
test backend::tests::checked_io_offset_rejects_u64_overflow ... ok
test backend::tests::fixed_file_identity_keeps_read_and_write_descriptors_separate ... ok
test backend::tests::pending_uring_job_retains_descriptor_lease_until_completion ... ok
test backend::tests::forcing_pread_selects_pread ... ok
test backend::tests::queued_job_retains_descriptor_lease_after_caller_cancellation ... ok
test backend::tests::pread_backend_queue_fails_closed_when_full ... ok
test backend::tests::poisoned_backend_receiver_preserves_queued_work ... ok
test boot::tests::boot_time_identity_round_trips_and_validates ... ok
test boot::tests::boot_times_within_the_tolerance_are_the_same_boot ... ok
test device::tests::cow_fs_detection_covers_common_filesystems ... ok
test boot::tests::mixed_or_opaque_identities_compare_exactly ... ok
test boot::tests::identity_file_read_is_bounded ... ok
test device::tests::mount_source_subpath_suffix_is_ignored ... ok
test device::tests::mount_details_include_mount_and_superblock_options ... ok
test durability_probe::tests::local_block_filesystems_are_strong ... ok
test durability_probe::tests::network_stacked_and_fuse_mounts_are_unknown_not_weak ... ok
test backend::tests::unavailable_pread_backend_fails_closed_without_workers ... ok
test durability_probe::tests::other_platform_filesystem_names_are_classified ... ok
test durability_probe::tests::overrides_do_not_match_on_partial_component_names ... ok
test durability_probe::tests::overrides_use_the_longest_matching_prefix ... ok
test backend::tests::pread_past_eof_errors ... ok
test backend::tests::pwrite_then_pread_roundtrip ... ok
test boot::tests::boot_change_is_tri_state ... ok
test device::tests::sysfs_block_device_name_prefers_parent_block_device ... ok
test backend::tests::uring_fixed_buffer_registration_budget_stays_below_common_memlock_limit ... ok
test boot::tests::identity_parser_trims_and_rejects_garbage ... ok
test boot::tests::linux_reports_a_stable_identity_within_one_boot ... ok
test durability_probe::tests::disabled_barriers_downgrade_an_otherwise_strong_filesystem ... ok
test boot::tests::boot_time_is_wall_clock_minus_uptime ... ok
test device::tests::mountinfo_parser_decodes_paths_and_finds_longest_prefix ... ok
test device::tests::mountinfo_read_is_bounded ... ok
test device::tests::block_device_profile_uses_name_and_rotational_flag ... ok
test device::tests::topology_reports_fs_profile_and_cow ... ok
test device::tests::network_topology_uses_mount_source_as_device_id ... ok
test durability_probe::tests::volatile_filesystems_are_weak ... ok
test fd_limit::tests::raise_returns_nonzero ... ok
test fd_limit::tests::soft_limit_conversion_handles_platform_rlim_t_width ... ok
test fd_limit::tests::unlimited_limit_still_has_a_finite_cache_ceiling ... ok
test backend::tests::uring_probe_is_diagnostic_not_panic ... ok
test durability_probe::tests::trust_ordering_puts_weak_last_for_escalation_logic ... ok
test elevator::tests::hdd_budget_holds_nonurgent_ops_until_elapsed ... ok
test elevator::tests::choke_critical_and_foreground_bypass_budget ... ok
test frame::tests::acquire_release_roundtrips_capacity ... ok
test elevator::tests::limited_drain_uses_class_weights_and_keeps_remainder_pending ... ok
test frame::tests::cap_accounts_for_pooled_backing_capacity ... ok
test frame::tests::into_bytes_releases_charge_without_copying_payload ... ok
test elevator::tests::nvme_degenerates_to_zero_budget ... ok
test frame::tests::buffers_are_reused_within_class ... ok
test elevator::tests::ready_reads_are_offset_sorted_and_coalesced_per_file ... ok
test elevator::tests::zero_limited_drain_leaves_ready_ops_pending ... ok
test error::tests::unsupported_operation_errnos_are_distinguished_from_media_errors ... ok
test elevator::tests::writes_are_ordered_but_not_coalesced ... ok
test fd_limit::tests::capacity_respects_low_fd_limits_and_reserved_fraction ... ok
test fd_limit::tests::capacity_scales_with_limit ... ok
test frame::tests::cap_enforced_with_backpressure ... ok
test durability_probe::tests::unrecognized_filesystems_are_unknown ... ok
test file_handle::tests::independent_cache_limiters_share_budget_and_wake_each_others_waiters ... ok
test handle_cache::tests::missing_file_read_errors_and_is_not_cached ... ok
test file_handle::tests::shared_budget_release_wakes_async_waiter_in_another_cache ... ok
test handle_cache::tests::lru_evicts_least_recently_used ... ok
test handle_cache::tests::reuses_same_handle_for_repeated_opens ... ok
test handle_cache::tests::read_and_write_handles_are_distinct ... ok
test handle_cache::tests::reopens_path_after_unlink_and_recreate ... ok
test handle_cache::tests::zero_capacity_returns_open_files_without_retaining_them ... ok
test handle_cache::tests::poisoned_handle_cache_discards_lru_and_reopens_safely ... ok
test frame::tests::registered_slot_frame_keeps_charge_until_drop ... ok
test backend::tests::uring_strategy_reports_frame_pool_slots_only_with_registered_buffers ... ok
test handle_cache::tests::ancestor_symlink_is_rejected_before_opening_the_file ... ok
test frame::tests::oversize_is_exact_and_counted ... ok
test backend::tests::uring_fatal_submission_preserves_pending_io_lifetimes ... ok
test frame::tests::poisoned_registered_slot_list_drops_invalid_and_duplicate_indices ... ok
test open::tests::limited_read_rejects_fifo_without_blocking ... ok
test open::tests::limited_read_rejects_oversized_runtime_file ... ok
test handle_cache::tests::async_descriptor_wait_does_not_block_executor_progress ... ok
test handle_cache::tests::final_component_symlink_is_rejected ... ok
test open::tests::sync_directory_uses_the_runtime_directory_boundary ... ok
test plan::tests::cancellation_after_checkpoint_rolls_back_committed_staging ... ok
test plan::tests::checkpoint_failure_leaves_committed_step_resumable ... ok
test plan::tests::cancellation_before_next_step_rolls_back_prior_staging ... ok
test open::tests::create_new_no_follow_refuses_to_replace_an_existing_file ... ok
test frame::tests::poisoned_frame_cache_drops_idle_buffers_and_recovers ... ok
test plan::tests::checkpointed_rename_without_live_proof_is_rejected ... ok
test plan::tests::checkpointed_delete_rejects_a_recreated_target ... ok
test plan::tests::descriptor_anchored_rename_checks_size_before_mutating ... ok
test backend::tests::uring_request_has_clean_probe_fallback ... ok
test backend::tests::uring_completed_operations_release_shared_fixed_file_slot ... ok
test backend::tests::selected_backend_roundtrip ... ok
test plan::tests::execute_delete_plan_removes_symlink_without_following_target ... ok
test plan::tests::execute_delete_plan_removes_directory_tree_after_approval ... ok
test plan::tests::controlled_copy_aborts_mid_step_and_rolls_back_staging ... ok
test plan::tests::cleanup_plan_is_idempotent_and_prunes_only_empty_directories ... ok
test plan::tests::controlled_delete_after_partial_removal_requires_manual_recovery ... ok
test plan::tests::copy_verification_failure_removes_partial_file_destination ... ok
test plan::tests::descriptor_anchored_rename_never_replaces_a_new_destination ... ok
test plan::tests::copy_directory_verification_failure_removes_partial_destination ... ok
test backend::tests::uring_rejected_submission_releases_new_fixed_file_slot ... ok
test plan::tests::execute_copy_verify_plan_rejects_symlink_source_file ... ok
test plan::tests::durable_rename_checkpoint_reconciles_after_source_is_removed ... ok
test plan::tests::execute_import_plan_rejects_symlink_source_before_hardlink ... ok
test plan::tests::execute_import_copy_failure_does_not_leave_final_destination ... ok
test plan::tests::execute_import_plan_rejects_stale_expected_size_without_destination ... ok
test plan::tests::execute_move_plan_rejects_symlink_source_before_rename ... ok
test plan::tests::execute_plan_reports_rollback_step_failure_in_error_message ... ok
test plan::tests::execute_move_plan_renames_without_overwrite ... ok
test plan::tests::execute_import_plan_links_or_copies_without_removing_source ... ok
test durability_probe::tests::probing_a_real_path_never_panics_and_reports_a_fs_type ... ok
test plan::tests::delete_requires_prior_dry_run_approval ... ok
test plan::tests::descriptor_anchored_execution_rejects_an_ancestor_symlink ... ok
test plan::tests::execute_copy_verify_plan_rejects_nested_symlink_entry ... ok
test plan::tests::execute_plan_under_roots_rejects_delete_escape ... ok
test plan::tests::execute_plan_under_roots_rejects_parent_dir_components ... ok
test plan::tests::execute_copy_verify_plan_copies_directory_tree_and_verifies_bytes ... ok
test plan::tests::execute_copy_verify_plan_rolls_back_staged_file_on_short_copy ... ok
test plan::tests::execute_plan_under_roots_rejects_destination_escape_before_copy ... ok
test plan::tests::execute_rejects_an_out_of_range_checkpoint_before_mutating ... ok
test plan::tests::import_copy_plan_stages_before_final_rename ... ok
test plan::tests::import_plan_detects_existing_destination ... ok
test backend::tests::forced_uring_roundtrip_when_kernel_supports_it ... ok
test plan::tests::move_plan_fails_closed_when_atomic_rename_is_unavailable ... ok
test plan::tests::move_plan_uses_rename_on_same_filesystem ... ok
test plan::tests::fully_checkpointed_cross_filesystem_move_reconciles_after_source_delete ... ok
test plan::tests::plan_delete_treats_broken_symlink_as_existing_source ... ok
test plan::tests::execute_plan_under_roots_refuses_to_delete_the_storage_root ... ok
test plan::tests::plan_import_treats_broken_destination_symlink_as_existing ... ok
test plan::tests::portable_execute_step_import_hardlinks_or_copies_without_removing_source ... ok
test plan::tests::execute_plan_under_roots_allows_confined_move ... ok
test plan::tests::portable_execute_step_prune_empty_dirs_removes_up_to_root ... ok
test plan::tests::failure_after_destructive_step_requires_manual_recovery ... ok
test plan::tests::portable_execute_step_rejects_symlink_source ... ok
test plan::tests::portable_execute_step_safe_delete_if_present_is_idempotent_when_missing ... ok
test plan::tests::move_plan_reports_conflict_and_capacity ... ok
test plan::tests::portable_execute_step_renames_file ... ok
test plan::tests::portable_execute_step_copy_verify_rename_copies_and_verifies_content ... ok
test plan::tests::portable_execute_step_safe_delete_removes_file ... ok
test plan::tests::move_plan_copy_verify_path_deletes_source_after_verified_rename ... ok
test plan::tests::portable_rollback_plan_runs_configured_rollback_steps ... ok
test plan::tests::failed_checkpoint_can_resume_without_repeating_filesystem_mutation ... ok
test plan::tests::move_plan_copy_verify_path_removes_source_directory_after_verified_rename ... ok
test plan::tests::reconcile_rejects_conflicting_rename_as_manual_recovery ... ok
test plan::tests::rename_rejects_a_stale_expected_size_before_moving ... ok
test plan::tests::portable_execute_step_rejects_an_ancestor_symlink_swapped_after_validation ... ok
test plan::tests::staged_import_copy_fails_closed_when_atomic_rename_is_unavailable ... ok
test plan::tests::reconcile_rejects_corrupt_staging_copy ... ok
test plan::tests::reconcile_infers_copy_when_following_rename_is_already_committed ... ok
test plan::tests::reconcile_rejects_unverifiable_rename_committed_before_checkpoint ... ok
test plan::tests::verify_content_matches_detects_bit_flip_despite_matching_length ... ok
test runtime::tests::backend_short_io_maps_to_storage_error ... ok
test plan::tests::verify_content_matches_rejects_extra_destination_entries ... ok
test scheduler::tests::auto_preallocation_policy_uses_full_only_for_non_cow_hdd ... ok
test runtime::tests::backend_would_block_maps_to_queue_full ... ok
test plan::tests::repeating_a_completed_copy_plan_is_idempotent ... ok
test backend::tests::forced_uring_read_only_roundtrip ... ok
test scheduler::tests::blocking_pool_closed_queue_fails_closed ... ok
test plan::tests::retrying_a_completed_copy_plan_validates_checkpoint_state ... ok
test scheduler::tests::blocking_pool_full_queue_fails_closed ... ok
test plan::tests::verify_content_matches_rejects_source_symlink_swap_during_final_scan ... ok
test scheduler::tests::configured_file_pool_is_clamped_to_the_process_fd_budget ... ok
test scheduler::tests::dirty_sync_does_not_clear_a_newer_write_generation ... ok
test scheduler::tests::dropped_write_waiter_invalidates_cache_after_backend_completion ... ok
test scheduler::tests::peer_read_elevator_full_queue_fails_closed ... ok
test scheduler::tests::cancelled_queued_blocking_job_is_skipped ... ok
test scheduler::tests::acquire_and_release_recheck ... ok
test scheduler::tests::canceled_queued_peer_read_is_not_dispatched ... ok
test alloc_audit::tests::fully_written_file_reports_no_gaps ... ok
test alloc_audit::tests::sparse_file_holes_are_detected_when_the_filesystem_reports_them ... ok
test alloc_audit::tests::preallocated_but_unwritten_file_is_flagged_on_extent_filesystems ... ok
test scheduler::tests::poisoned_dirty_path_state_is_preserved_and_unpoisoned ... ok
test scheduler::tests::peer_read_readahead_is_admitted_for_actual_backend_buffer ... ok
test scheduler::tests::peer_read_readahead_cache_reopens_a_replaced_path ... ok
test scheduler::tests::poisoned_peer_read_cache_is_discarded_as_a_miss ... ok
test scheduler::tests::peer_read_readahead_cache_is_invalidated_by_writes ... ok
test scheduler::tests::poisoned_file_pool_discards_cached_handles_and_reopens ... ok
test scheduler::tests::queued_disk_bytes_track_active_blocking_job_payload ... ok
test file_handle::tests::process_budget_caps_independent_caches_under_low_rlimit ... ok
test scheduler::tests::concurrent_positioned_writes_do_not_share_cursor ... ok
test scheduler::tests::file_pool_writes_to_the_recreated_path_not_the_unlinked_inode ... ok
test scheduler::tests::path_schedulers_share_bounded_worker_resources ... ok
test scheduler::tests::file_pool_records_hits_and_evictions ... ok
test scheduler::tests::malformed_peer_read_batch_returns_short_io_instead_of_panicking ... ok
test scheduler::tests::queued_disk_governor_denies_before_enqueue ... ok
test scheduler::tests::full_preallocation_leaves_unwritten_extents_not_zero_blocks ... ok
test scheduler::tests::read_nonexistent_file_does_not_create ... ok
test scheduler::tests::schedulers_on_same_device_share_global_queue ... ok
test scheduler::tests::compatibility_read_still_returns_bytes ... ok
test scheduler::tests::peer_read_readahead_cache_can_be_disabled ... ok
test scheduler::tests::runtime_file_pool_rejects_an_ancestor_symlink ... ok
test scheduler::tests::file_range_overflow_is_rejected_before_storage_access ... ok
test scheduler::tests::file_pool_reopens_a_path_after_unlink_and_recreate ... ok
test scheduler::tests::large_peer_and_recheck_reads_emit_page_cache_advice ... ok
test scheduler::tests::short_positioned_read_maps_to_storage_error ... ok
test scheduler::tests::full_mount_queue_fails_closed ... ok
test scheduler::tests::owned_read_returns_pooled_frame_for_exact_backend_read ... ok
test scheduler::tests::sparse_prepare_creates_parent_once ... ok
test scheduler::tests::prepare_file_extends_without_disturbing_existing_bytes ... ok
test scheduler::tests::peer_read_readahead_cache_is_config_bounded ... ok
test scheduler::tests::strict_write_sync_is_counted_and_not_left_dirty ... ok
test runtime::tests::missing_file_maps_to_not_found ... ok
test runtime::tests::global_read_write_roundtrip ... ok
test scheduler::tests::peer_read_not_starved_by_recheck ... ok
test scheduler::tests::stats_track_io_sync_and_hash_work ... ok
test scheduler::tests::read_and_write_roundtrip ... ok
test scheduler::tests::peer_read_readahead_cache_returns_exact_requested_bytes ... ok
test scheduler::tests::write_handle_snapshot_drops_replaced_paths ... ok
test scheduler::tests::zero_capacity_file_pool_opens_without_retaining_the_file ... ok
test secure_fs::tests::directory_name_comparison_is_order_independent_and_detects_extras ... ok
test scheduler::tests::prepare_file_does_not_create_through_an_ancestor_symlink ... ok
test scheduler::tests::scheduler_new_resolves_auto_to_sparse_without_path_topology ... ok
test secure_fs::tests::directory_names_returns_all_entries ... ok
test plan::tests::portable_tree_walks_reject_overdeep_directories_and_clean_partial_copy ... ok
test scheduler::tests::write_does_not_create_by_default ... ok
test scheduler::tests::prepare_file_refuses_to_shrink_existing_data ... ok
test scheduler::tests::sync_all_open_files_syncs_dirty_paths_after_fd_eviction ... ok
test verify::tests::verify_invalid_piece ... ok
test verify::tests::verify_missing_file ... ok
test verify::tests::verify_range_returns_empty_for_reversed_or_out_of_range_bounds ... ok
test verify::tests::verify_piece_treats_missing_bep47_padding_as_zeroes ... ok
test verify::tests::v2_file_verify_rejects_wrong_file_root ... ok
test verify::tests::verify_range_resumable ... ok
test verify::tests::v2_file_verify_rejects_missing_empty_file_without_root ... ok
test verify::tests::verify_valid_piece ... ok
test secure_fs::tests::overdeep_directory_walks_fail_closed_without_stack_recursion ... ok
test scheduler::tests::hdd_peer_read_elevator_batches_shuffled_adjacent_reads ... ok
test scheduler::tests::hdd_peer_read_elevator_dispatches_after_quiet_slice ... ok
test scheduler::tests::ssd_has_higher_concurrency ... ok
test verify::tests::verify_truncated_sparse_file_is_missing_not_zero_filled ... ok
test verify::tests::v2_file_verify_accepts_empty_file_without_root ... ok
test verify::tests::verify_all_reports_per_piece ... ok
test verify::tests::v2_file_verify_hashes_sparse_holes_as_zeroes ... ok
test verify::tests::v2_file_verify_accepts_matching_file_root ... ok
test scheduler::tests::sync_all_open_files_stays_within_low_descriptor_limit ... ok
test verify::tests::verify_sparse_piece_hashes_holes_as_zeroes ... ok
test handle_cache::tests::idle_sweep_closes_stale_handles ... ok
test scheduler::tests::verifier_retries_transient_hash_queue_full ... ok
test handle_cache::tests::waits_for_an_evicted_in_flight_descriptor_lease ... ok
test verify::tests::verify_piece_streams_large_piece ... ok
test scheduler::tests::file_pool_waits_for_an_evicted_in_flight_descriptor_lease ... ok
test handle_cache::tests::concurrent_cache_opens_stay_within_low_descriptor_limit ... ok
test scheduler::tests::concurrent_file_pool_opens_stay_within_low_descriptor_limit ... ok

test result: ok. 254 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.29s

     Running tests/storage_move_import_hardware.rs (target/debug/deps/storage_move_import_hardware-e03958a2f5331ea8)

running 1 test
test move_import_delete_executor_runs_on_configured_storage_root ... ignored, real-root move/import certification; set TNG_STORAGE_MOVE_IMPORT_ROOT

test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/storage_real_device.rs (target/debug/deps/storage_real_device-9d55b69a9f6c1e1e)

running 7 tests
test backend_selection_roundtrip_reports_capabilities ... ignored, real-device storage benchmark; run explicitly with --ignored --nocapture
test backend_stream_roundtrip_reports_throughput ... ignored, real-device storage benchmark; run explicitly with --ignored --nocapture
test hdd_peer_read_elevator_reduces_backend_reads_on_shuffled_adjacent_blocks ... ignored, real-device storage benchmark; run explicitly with --ignored --nocapture
test peer_read_readahead_reduces_backend_reads_on_adjacent_blocks ... ignored, real-device storage benchmark; run explicitly with --ignored --nocapture
test recheck_range_reports_runtime_progress ... ignored, real-device storage benchmark; run explicitly with --ignored --nocapture
test repeated_reads_reuse_one_open_file_handle ... ignored, real-device storage benchmark; run explicitly with --ignored --nocapture
test shuffled_peer_read_baseline_reports_current_scheduler_throughput ... ignored, real-device storage benchmark; run explicitly with --ignored --nocapture

test result: ok. 0 passed; 0 failed; 7 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_storage

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

```
| real-root move/import/delete executor | PASS |

## real-root move/import/delete executor

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.04s
     Running tests/storage_move_import_hardware.rs (target/debug/deps/storage_move_import_hardware-e03958a2f5331ea8)

running 1 test
tng_storage_move_import root=/mnt/disks/gamespool1 files=64 mib_per_file=1 bytes=67108864 moved=1 imported=1 deleted=1 root_confined=1
test move_import_delete_executor_runs_on_configured_storage_root ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.83s

```

## Notes

- Covers symlink-safe no-overwrite move execution, copy-based move source cleanup after verified rename, hardlink-or-copy import, recursive directory copy verification, rename/copy/import symlink rejection, symlink-safe delete, staged rollback cleanup, approved directory delete, and storage-root confinement.
- Set TNG_STORAGE_MOVE_IMPORT_ROOT to run the same executor on a real storage root. Increase TNG_STORAGE_MOVE_IMPORT_FILES and TNG_STORAGE_MOVE_IMPORT_MIB_PER_FILE for larger operator soaks.
