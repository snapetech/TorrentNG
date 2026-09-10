# TorrentNG Storage Move/Import Certification

- Generated: 2026-09-10T17:53:46Z
- Host: kspls0
- Commit: b393eb0
- Hardware root: /mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/move-import
- Hardware files: 64
- Hardware MiB/file: 1

| Gate | Result |
| --- | --- |
| storage planner/executor unit tests | PASS |

## storage planner/executor unit tests

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.08s
     Running unittests src/lib.rs (target/debug/deps/rt_storage-70d2bb6d0492ef50)

running 36 tests
test plan::tests::plan_import_treats_broken_destination_symlink_as_existing ... ok
test plan::tests::import_plan_detects_existing_destination ... ok
test plan::tests::move_plan_reports_conflict_and_capacity ... ok
test plan::tests::delete_requires_prior_dry_run_approval ... ok
test plan::tests::execute_plan_under_roots_rejects_parent_dir_components ... ok
test plan::tests::execute_plan_under_roots_rejects_destination_escape_before_copy ... ok
test plan::tests::execute_delete_plan_removes_directory_tree_after_approval ... ok
test plan::tests::plan_delete_treats_broken_symlink_as_existing_source ... ok
test plan::tests::execute_delete_plan_removes_symlink_without_following_target ... ok
test plan::tests::execute_plan_under_roots_rejects_delete_escape ... ok
test plan::tests::execute_import_plan_links_or_copies_without_removing_source ... ok
test plan::tests::move_plan_uses_rename_on_same_filesystem ... ok
test plan::tests::execute_import_plan_rejects_symlink_source_before_hardlink ... ok
test plan::tests::descriptor_anchored_execution_rejects_an_ancestor_symlink ... ok
test plan::tests::execute_copy_verify_plan_rejects_symlink_source_file ... ok
test plan::tests::import_copy_plan_stages_before_final_rename ... ok
test plan::tests::execute_copy_verify_plan_rolls_back_staged_file_on_short_copy ... ok
test plan::tests::execute_import_copy_failure_does_not_leave_final_destination ... ok
test plan::tests::execute_move_plan_rejects_symlink_source_before_rename ... ok
test plan::tests::execute_plan_under_roots_allows_confined_move ... ok
test plan::tests::controlled_copy_aborts_mid_step_and_rolls_back_staging ... ok
test plan::tests::execute_copy_verify_plan_rejects_nested_symlink_entry ... ok
test plan::tests::execute_move_plan_renames_without_overwrite ... ok
test plan::tests::retrying_a_completed_plan_is_idempotent ... ok
test plan::tests::checkpoint_failure_leaves_committed_step_resumable ... ok
test plan::tests::verify_content_matches_detects_bit_flip_despite_matching_length ... ok
test plan::tests::reconcile_rejects_corrupt_staging_copy ... ok
test plan::tests::reconcile_detects_rename_committed_before_checkpoint ... ok
test plan::tests::move_plan_copy_verify_path_deletes_source_after_verified_rename ... ok
test plan::tests::failed_checkpoint_can_resume_without_repeating_filesystem_mutation ... ok
test plan::tests::cleanup_plan_is_idempotent_and_prunes_only_empty_directories ... ok
test plan::tests::repeating_a_completed_copy_plan_is_idempotent ... ok
test plan::tests::execute_copy_verify_plan_copies_directory_tree_and_verifies_bytes ... ok
test plan::tests::execute_plan_reports_rollback_step_failure_in_error_message ... ok
test plan::tests::move_plan_copy_verify_path_removes_source_directory_after_verified_rename ... ok
test plan::tests::reconcile_infers_copy_when_following_rename_is_already_committed ... ok

test result: ok. 36 passed; 0 failed; 0 ignored; 0 measured; 94 filtered out; finished in 0.00s

     Running tests/storage_move_import_hardware.rs (target/debug/deps/storage_move_import_hardware-d69c015c3fc016d9)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s

     Running tests/storage_real_device.rs (target/debug/deps/storage_real_device-4e54eaa6ccf67876)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.00s

```
| full storage unit suite | PASS |

## full storage unit suite

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.07s
     Running unittests src/lib.rs (target/debug/deps/rt_storage-70d2bb6d0492ef50)

running 130 tests
test backend::tests::fixed_buffer_strategy_names_are_stable_for_metrics ... ok
test backend::tests::backend_request_parses_user_values ... ok
test backend::tests::uring_fixed_buffer_registration_budget_stays_below_common_memlock_limit ... ok
test device::tests::mount_source_subpath_suffix_is_ignored ... ok
test device::tests::cow_fs_detection_covers_common_filesystems ... ok
test device::tests::mountinfo_parser_decodes_paths_and_finds_longest_prefix ... ok
test device::tests::sysfs_block_device_name_prefers_parent_block_device ... ok
test backend::tests::forcing_pread_selects_pread ... ok
test device::tests::mountinfo_read_is_bounded ... ok
test device::tests::block_device_profile_uses_name_and_rotational_flag ... ok
test elevator::tests::hdd_budget_holds_nonurgent_ops_until_elapsed ... ok
test elevator::tests::choke_critical_and_foreground_bypass_budget ... ok
test device::tests::network_topology_uses_mount_source_as_device_id ... ok
test elevator::tests::nvme_degenerates_to_zero_budget ... ok
test elevator::tests::ready_reads_are_offset_sorted_and_coalesced_per_file ... ok
test elevator::tests::limited_drain_uses_class_weights_and_keeps_remainder_pending ... ok
test elevator::tests::writes_are_ordered_but_not_coalesced ... ok
test elevator::tests::zero_limited_drain_leaves_ready_ops_pending ... ok
test backend::tests::uring_probe_is_diagnostic_not_panic ... ok
test fd_limit::tests::capacity_scales_with_limit ... ok
test fd_limit::tests::raise_returns_nonzero ... ok
test fd_limit::tests::capacity_respects_floor ... ok
test frame::tests::into_bytes_releases_charge_without_copying_payload ... ok
test frame::tests::oversize_is_exact_and_counted ... ok
test frame::tests::acquire_release_roundtrips_capacity ... ok
test frame::tests::cap_enforced_with_backpressure ... ok
test frame::tests::registered_slot_frame_keeps_charge_until_drop ... ok
test backend::tests::pread_backend_queue_fails_closed_when_full ... ok
test frame::tests::buffers_are_reused_within_class ... ok
test backend::tests::pread_past_eof_errors ... ok
test device::tests::topology_reports_fs_profile_and_cow ... ok
test handle_cache::tests::missing_file_read_errors_and_is_not_cached ... ok
test backend::tests::selected_backend_roundtrip ... ok
test handle_cache::tests::final_component_symlink_is_rejected ... ok
test backend::tests::pwrite_then_pread_roundtrip ... ok
test handle_cache::tests::read_and_write_handles_are_distinct ... ok
test plan::tests::execute_copy_verify_plan_rejects_symlink_source_file ... ok
test handle_cache::tests::ancestor_symlink_is_rejected_before_opening_the_file ... ok
test plan::tests::execute_copy_verify_plan_rolls_back_staged_file_on_short_copy ... ok
test handle_cache::tests::reuses_same_handle_for_repeated_opens ... ok
test plan::tests::execute_delete_plan_removes_symlink_without_following_target ... ok
test open::tests::limited_read_rejects_oversized_runtime_file ... ok
test handle_cache::tests::lru_evicts_least_recently_used ... ok
test plan::tests::execute_import_copy_failure_does_not_leave_final_destination ... ok
test plan::tests::execute_move_plan_rejects_symlink_source_before_rename ... ok
test plan::tests::execute_import_plan_rejects_symlink_source_before_hardlink ... ok
test plan::tests::execute_move_plan_renames_without_overwrite ... ok
test plan::tests::execute_plan_reports_rollback_step_failure_in_error_message ... ok
test plan::tests::execute_plan_under_roots_allows_confined_move ... ok
test plan::tests::move_plan_reports_conflict_and_capacity ... ok
test plan::tests::move_plan_uses_rename_on_same_filesystem ... ok
test plan::tests::plan_delete_treats_broken_symlink_as_existing_source ... ok
test plan::tests::execute_delete_plan_removes_directory_tree_after_approval ... ok
test plan::tests::plan_import_treats_broken_destination_symlink_as_existing ... ok
test plan::tests::execute_import_plan_links_or_copies_without_removing_source ... ok
test plan::tests::descriptor_anchored_execution_rejects_an_ancestor_symlink ... ok
test plan::tests::execute_plan_under_roots_rejects_delete_escape ... ok
test plan::tests::reconcile_infers_copy_when_following_rename_is_already_committed ... ok
test plan::tests::checkpoint_failure_leaves_committed_step_resumable ... ok
test plan::tests::cleanup_plan_is_idempotent_and_prunes_only_empty_directories ... ok
test plan::tests::delete_requires_prior_dry_run_approval ... ok
test plan::tests::controlled_copy_aborts_mid_step_and_rolls_back_staging ... ok
test plan::tests::execute_plan_under_roots_rejects_destination_escape_before_copy ... ok
test plan::tests::execute_plan_under_roots_rejects_parent_dir_components ... ok
test scheduler::tests::auto_preallocation_policy_uses_full_only_for_non_cow_hdd ... ok
test plan::tests::import_copy_plan_stages_before_final_rename ... ok
test plan::tests::reconcile_rejects_corrupt_staging_copy ... ok
test plan::tests::import_plan_detects_existing_destination ... ok
test plan::tests::reconcile_detects_rename_committed_before_checkpoint ... ok
test scheduler::tests::blocking_pool_full_queue_fails_closed ... ok
test plan::tests::failed_checkpoint_can_resume_without_repeating_filesystem_mutation ... ok
test plan::tests::execute_copy_verify_plan_rejects_nested_symlink_entry ... ok
test plan::tests::move_plan_copy_verify_path_removes_source_directory_after_verified_rename ... ok
test backend::tests::uring_strategy_reports_frame_pool_slots_only_with_registered_buffers ... ok
test runtime::tests::backend_short_io_maps_to_storage_error ... ok
test plan::tests::retrying_a_completed_plan_is_idempotent ... ok
test plan::tests::execute_copy_verify_plan_copies_directory_tree_and_verifies_bytes ... ok
test plan::tests::repeating_a_completed_copy_plan_is_idempotent ... ok
test scheduler::tests::peer_read_elevator_full_queue_fails_closed ... ok
test scheduler::tests::acquire_and_release_recheck ... ok
test runtime::tests::backend_would_block_maps_to_queue_full ... ok
test scheduler::tests::peer_read_not_starved_by_recheck ... ok
test runtime::tests::missing_file_maps_to_not_found ... ok
test runtime::tests::global_read_write_roundtrip ... ok
test scheduler::tests::owned_read_returns_pooled_frame_for_exact_backend_read ... ok
test scheduler::tests::compatibility_read_still_returns_bytes ... ok
test scheduler::tests::full_mount_queue_fails_closed ... ok
test scheduler::tests::stats_track_io_sync_and_hash_work ... ok
test scheduler::tests::scheduler_new_resolves_auto_to_sparse_without_path_topology ... ok
test scheduler::tests::ssd_has_higher_concurrency ... ok
test scheduler::tests::write_does_not_create_by_default ... ok
test scheduler::tests::strict_write_sync_is_counted_and_not_left_dirty ... ok
test scheduler::tests::sync_all_open_files_syncs_dirty_paths_after_fd_eviction ... ok
test verify::tests::v2_file_verify_rejects_wrong_file_root ... ok
test verify::tests::v2_file_verify_accepts_empty_file_without_root ... ok
test backend::tests::forced_uring_roundtrip_when_kernel_supports_it ... ok
test verify::tests::verify_invalid_piece ... ok
test verify::tests::verify_range_resumable ... ok
test scheduler::tests::large_peer_and_recheck_reads_emit_page_cache_advice ... ok
test verify::tests::verify_valid_piece ... ok
test verify::tests::v2_file_verify_accepts_matching_file_root ... ok
test verify::tests::verify_missing_file ... ok
test plan::tests::move_plan_copy_verify_path_deletes_source_after_verified_rename ... ok
test plan::tests::verify_content_matches_detects_bit_flip_despite_matching_length ... ok
test scheduler::tests::sparse_prepare_creates_parent_once ... ok
test scheduler::tests::prepare_file_extends_without_disturbing_existing_bytes ... ok
test scheduler::tests::concurrent_positioned_writes_do_not_share_cursor ... ok
test verify::tests::v2_file_verify_hashes_sparse_holes_as_zeroes ... ok
test scheduler::tests::file_pool_records_hits_and_evictions ... ok
test verify::tests::verify_range_returns_empty_for_reversed_or_out_of_range_bounds ... ok
test scheduler::tests::prepare_file_does_not_create_through_an_ancestor_symlink ... ok
test scheduler::tests::peer_read_readahead_cache_is_config_bounded ... ok
test scheduler::tests::peer_read_readahead_cache_can_be_disabled ... ok
test verify::tests::verify_truncated_sparse_file_is_missing_not_zero_filled ... ok
test scheduler::tests::queued_disk_governor_denies_before_enqueue ... ok
test scheduler::tests::schedulers_on_same_device_share_global_queue ... ok
test scheduler::tests::prepare_file_refuses_to_shrink_existing_data ... ok
test backend::tests::uring_request_has_clean_probe_fallback ... ok
test scheduler::tests::read_and_write_roundtrip ... ok
test scheduler::tests::peer_read_readahead_cache_returns_exact_requested_bytes ... ok
test scheduler::tests::hdd_peer_read_elevator_dispatches_after_quiet_slice ... ok
test scheduler::tests::read_nonexistent_file_does_not_create ... ok
test scheduler::tests::short_positioned_read_maps_to_storage_error ... ok
test scheduler::tests::queued_disk_bytes_track_active_blocking_job_payload ... ok
test scheduler::tests::runtime_file_pool_rejects_an_ancestor_symlink ... ok
test scheduler::tests::peer_read_readahead_cache_is_invalidated_by_writes ... ok
test verify::tests::verify_all_reports_per_piece ... ok
test scheduler::tests::hdd_peer_read_elevator_batches_shuffled_adjacent_reads ... ok
test verify::tests::verify_sparse_piece_hashes_holes_as_zeroes ... ok
test handle_cache::tests::idle_sweep_closes_stale_handles ... ok

test result: ok. 130 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s

     Running tests/storage_move_import_hardware.rs (target/debug/deps/storage_move_import_hardware-d69c015c3fc016d9)

running 1 test
test move_import_delete_executor_runs_on_configured_storage_root ... ignored, real-root move/import certification; set TNG_STORAGE_MOVE_IMPORT_ROOT

test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/storage_real_device.rs (target/debug/deps/storage_real_device-4e54eaa6ccf67876)

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
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.08s
     Running tests/storage_move_import_hardware.rs (target/debug/deps/storage_move_import_hardware-d69c015c3fc016d9)

running 1 test
tng_storage_move_import root=/mnt/datapool_lvm_media/.torrentng-certification-20260910-b393eb0/move-import files=64 mib_per_file=1 bytes=67108864 moved=1 imported=1 deleted=1 root_confined=1
test move_import_delete_executor_runs_on_configured_storage_root ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 9.75s

```

## Notes

- Covers symlink-safe no-overwrite move execution, copy-based move source cleanup after verified rename, hardlink-or-copy import, recursive directory copy verification, rename/copy/import symlink rejection, symlink-safe delete, staged rollback cleanup, approved directory delete, and storage-root confinement.
- Set TNG_STORAGE_MOVE_IMPORT_ROOT to run the same executor on a real storage root. Increase TNG_STORAGE_MOVE_IMPORT_FILES and TNG_STORAGE_MOVE_IMPORT_MIB_PER_FILE for larger operator soaks.
