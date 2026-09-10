# TorrentNG Universal Live Certification

- Date UTC: 2026-09-10T17:54:25Z
- Commit: b393eb0
- Run local Docker interop: 0
- Run public torrent interop: 0
- Run real-device storage matrix: 1

## Gates

| Gate | Result | Detail |
|---|---|---|
| Docker client interop local matrix | SKIP | set UNIVERSAL_LIVE_LOCAL=1 |
| public torrent interop matrix | SKIP | set UNIVERSAL_LIVE_PUBLIC=1 after approving public legal torrent downloads |
| real-device storage matrix | PASS | completed |

## Docker client interop local matrix

```text
SKIP: set UNIVERSAL_LIVE_LOCAL=1
```

## public torrent interop matrix

```text
SKIP: set UNIVERSAL_LIVE_PUBLIC=1 after approving public legal torrent downloads
```

## real-device storage matrix

- Command: `bash -c STORAGE_PHASE_B_REAL_DEVICE=1 "$1" _ /tmp/torrentng-674b283-20260910-v4/scripts/storage_phase_b_matrix.sh`

```text

==> cargo test -p rt-storage
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.08s
     Running unittests src/lib.rs (target/debug/deps/rt_storage-70d2bb6d0492ef50)

running 130 tests
test backend::tests::backend_request_parses_user_values ... ok
test backend::tests::fixed_buffer_strategy_names_are_stable_for_metrics ... ok
test backend::tests::forcing_pread_selects_pread ... ok
test backend::tests::pread_backend_queue_fails_closed_when_full ... ok
test backend::tests::uring_probe_is_diagnostic_not_panic ... ok
test backend::tests::uring_fixed_buffer_registration_budget_stays_below_common_memlock_limit ... ok
test device::tests::cow_fs_detection_covers_common_filesystems ... ok
test backend::tests::pread_past_eof_errors ... ok
test device::tests::mountinfo_parser_decodes_paths_and_finds_longest_prefix ... ok
test device::tests::block_device_profile_uses_name_and_rotational_flag ... ok
test device::tests::mount_source_subpath_suffix_is_ignored ... ok
test device::tests::mountinfo_read_is_bounded ... ok
test backend::tests::pwrite_then_pread_roundtrip ... ok
test backend::tests::selected_backend_roundtrip ... ok
test device::tests::sysfs_block_device_name_prefers_parent_block_device ... ok
test elevator::tests::hdd_budget_holds_nonurgent_ops_until_elapsed ... ok
test elevator::tests::choke_critical_and_foreground_bypass_budget ... ok
test device::tests::network_topology_uses_mount_source_as_device_id ... ok
test backend::tests::uring_strategy_reports_frame_pool_slots_only_with_registered_buffers ... ok
test device::tests::topology_reports_fs_profile_and_cow ... ok
test elevator::tests::limited_drain_uses_class_weights_and_keeps_remainder_pending ... ok
test elevator::tests::writes_are_ordered_but_not_coalesced ... ok
test elevator::tests::ready_reads_are_offset_sorted_and_coalesced_per_file ... ok
test elevator::tests::zero_limited_drain_leaves_ready_ops_pending ... ok
test fd_limit::tests::capacity_respects_floor ... ok
test elevator::tests::nvme_degenerates_to_zero_budget ... ok
test fd_limit::tests::capacity_scales_with_limit ... ok
test fd_limit::tests::raise_returns_nonzero ... ok
test frame::tests::acquire_release_roundtrips_capacity ... ok
test frame::tests::cap_enforced_with_backpressure ... ok
test frame::tests::buffers_are_reused_within_class ... ok
test plan::tests::execute_delete_plan_removes_directory_tree_after_approval ... ok
test frame::tests::oversize_is_exact_and_counted ... ok
test frame::tests::registered_slot_frame_keeps_charge_until_drop ... ok
test plan::tests::execute_import_plan_links_or_copies_without_removing_source ... ok
test plan::tests::execute_move_plan_rejects_symlink_source_before_rename ... ok
test plan::tests::execute_import_plan_rejects_symlink_source_before_hardlink ... ok
test handle_cache::tests::final_component_symlink_is_rejected ... ok
test handle_cache::tests::ancestor_symlink_is_rejected_before_opening_the_file ... ok
test handle_cache::tests::missing_file_read_errors_and_is_not_cached ... ok
test plan::tests::execute_plan_reports_rollback_step_failure_in_error_message ... ok
test plan::tests::execute_import_copy_failure_does_not_leave_final_destination ... ok
test handle_cache::tests::read_and_write_handles_are_distinct ... ok
test handle_cache::tests::reuses_same_handle_for_repeated_opens ... ok
test handle_cache::tests::lru_evicts_least_recently_used ... ok
test plan::tests::execute_plan_under_roots_rejects_delete_escape ... ok
test plan::tests::execute_plan_under_roots_rejects_destination_escape_before_copy ... ok
test plan::tests::execute_plan_under_roots_rejects_parent_dir_components ... ok
test plan::tests::execute_plan_under_roots_allows_confined_move ... ok
test plan::tests::import_copy_plan_stages_before_final_rename ... ok
test plan::tests::import_plan_detects_existing_destination ... ok
test plan::tests::descriptor_anchored_execution_rejects_an_ancestor_symlink ... ok
test plan::tests::checkpoint_failure_leaves_committed_step_resumable ... ok
test plan::tests::execute_copy_verify_plan_rejects_symlink_source_file ... ok
test frame::tests::into_bytes_releases_charge_without_copying_payload ... ok
test plan::tests::failed_checkpoint_can_resume_without_repeating_filesystem_mutation ... ok
test plan::tests::move_plan_copy_verify_path_deletes_source_after_verified_rename ... ok
test plan::tests::move_plan_reports_conflict_and_capacity ... ok
test plan::tests::move_plan_uses_rename_on_same_filesystem ... ok
test plan::tests::plan_delete_treats_broken_symlink_as_existing_source ... ok
test plan::tests::execute_copy_verify_plan_rolls_back_staged_file_on_short_copy ... ok
test plan::tests::execute_delete_plan_removes_symlink_without_following_target ... ok
test plan::tests::plan_import_treats_broken_destination_symlink_as_existing ... ok
test plan::tests::delete_requires_prior_dry_run_approval ... ok
test plan::tests::execute_copy_verify_plan_rejects_nested_symlink_entry ... ok
test runtime::tests::backend_short_io_maps_to_storage_error ... ok
test plan::tests::reconcile_infers_copy_when_following_rename_is_already_committed ... ok
test runtime::tests::backend_would_block_maps_to_queue_full ... ok
test plan::tests::reconcile_rejects_corrupt_staging_copy ... ok
test plan::tests::retrying_a_completed_plan_is_idempotent ... ok
test plan::tests::execute_copy_verify_plan_copies_directory_tree_and_verifies_bytes ... ok
test plan::tests::move_plan_copy_verify_path_removes_source_directory_after_verified_rename ... ok
test plan::tests::execute_move_plan_renames_without_overwrite ... ok
test plan::tests::reconcile_detects_rename_committed_before_checkpoint ... ok
test plan::tests::verify_content_matches_detects_bit_flip_despite_matching_length ... ok
test open::tests::limited_read_rejects_oversized_runtime_file ... ok
test plan::tests::repeating_a_completed_copy_plan_is_idempotent ... ok
test scheduler::tests::auto_preallocation_policy_uses_full_only_for_non_cow_hdd ... ok
test plan::tests::controlled_copy_aborts_mid_step_and_rolls_back_staging ... ok
test scheduler::tests::blocking_pool_full_queue_fails_closed ... ok
test backend::tests::uring_request_has_clean_probe_fallback ... ok
test scheduler::tests::peer_read_elevator_full_queue_fails_closed ... ok
test scheduler::tests::peer_read_not_starved_by_recheck ... ok
test scheduler::tests::acquire_and_release_recheck ... ok
test runtime::tests::missing_file_maps_to_not_found ... ok
test scheduler::tests::scheduler_new_resolves_auto_to_sparse_without_path_topology ... ok
test scheduler::tests::peer_read_readahead_cache_can_be_disabled ... ok
test scheduler::tests::full_mount_queue_fails_closed ... ok
test scheduler::tests::ssd_has_higher_concurrency ... ok
test scheduler::tests::short_positioned_read_maps_to_storage_error ... ok
test scheduler::tests::compatibility_read_still_returns_bytes ... ok
test scheduler::tests::file_pool_records_hits_and_evictions ... ok
test plan::tests::cleanup_plan_is_idempotent_and_prunes_only_empty_directories ... ok
test scheduler::tests::strict_write_sync_is_counted_and_not_left_dirty ... ok
test scheduler::tests::stats_track_io_sync_and_hash_work ... ok
test scheduler::tests::concurrent_positioned_writes_do_not_share_cursor ... ok
test scheduler::tests::peer_read_readahead_cache_is_config_bounded ... ok
test scheduler::tests::queued_disk_bytes_track_active_blocking_job_payload ... ok
test scheduler::tests::hdd_peer_read_elevator_dispatches_after_quiet_slice ... ok
test runtime::tests::global_read_write_roundtrip ... ok
test scheduler::tests::sparse_prepare_creates_parent_once ... ok
test verify::tests::v2_file_verify_rejects_wrong_file_root ... ok
test backend::tests::forced_uring_roundtrip_when_kernel_supports_it ... ok
test scheduler::tests::owned_read_returns_pooled_frame_for_exact_backend_read ... ok
test scheduler::tests::write_does_not_create_by_default ... ok
test scheduler::tests::read_nonexistent_file_does_not_create ... ok
test scheduler::tests::peer_read_readahead_cache_is_invalidated_by_writes ... ok
test scheduler::tests::peer_read_readahead_cache_returns_exact_requested_bytes ... ok
test verify::tests::v2_file_verify_accepts_empty_file_without_root ... ok
test scheduler::tests::large_peer_and_recheck_reads_emit_page_cache_advice ... ok
test scheduler::tests::read_and_write_roundtrip ... ok
test scheduler::tests::queued_disk_governor_denies_before_enqueue ... ok
test scheduler::tests::prepare_file_extends_without_disturbing_existing_bytes ... ok
test scheduler::tests::prepare_file_does_not_create_through_an_ancestor_symlink ... ok
test verify::tests::verify_range_returns_empty_for_reversed_or_out_of_range_bounds ... ok
test verify::tests::verify_missing_file ... ok
test verify::tests::verify_range_resumable ... ok
test verify::tests::verify_invalid_piece ... ok
test scheduler::tests::runtime_file_pool_rejects_an_ancestor_symlink ... ok
test scheduler::tests::schedulers_on_same_device_share_global_queue ... ok
test verify::tests::v2_file_verify_hashes_sparse_holes_as_zeroes ... ok
test verify::tests::v2_file_verify_accepts_matching_file_root ... ok
test verify::tests::verify_truncated_sparse_file_is_missing_not_zero_filled ... ok
test scheduler::tests::sync_all_open_files_syncs_dirty_paths_after_fd_eviction ... ok
test scheduler::tests::hdd_peer_read_elevator_batches_shuffled_adjacent_reads ... ok
test scheduler::tests::prepare_file_refuses_to_shrink_existing_data ... ok
test verify::tests::verify_all_reports_per_piece ... ok
test verify::tests::verify_valid_piece ... ok
test handle_cache::tests::idle_sweep_closes_stale_handles ... ok
test verify::tests::verify_sparse_piece_hashes_holes_as_zeroes ... ok

test result: ok. 130 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s

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


==> cargo test -p rt-storage device::tests
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.07s
     Running unittests src/lib.rs (target/debug/deps/rt_storage-70d2bb6d0492ef50)

running 8 tests
test device::tests::cow_fs_detection_covers_common_filesystems ... ok
test device::tests::mount_source_subpath_suffix_is_ignored ... ok
test device::tests::sysfs_block_device_name_prefers_parent_block_device ... ok
test device::tests::mountinfo_read_is_bounded ... ok
test device::tests::mountinfo_parser_decodes_paths_and_finds_longest_prefix ... ok
test device::tests::block_device_profile_uses_name_and_rotational_flag ... ok
test device::tests::network_topology_uses_mount_source_as_device_id ... ok
test device::tests::topology_reports_fs_profile_and_cow ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 122 filtered out; finished in 0.00s

     Running tests/storage_move_import_hardware.rs (target/debug/deps/storage_move_import_hardware-d69c015c3fc016d9)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s

     Running tests/storage_real_device.rs (target/debug/deps/storage_real_device-4e54eaa6ccf67876)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.00s


==> cargo test -p rt-storage elevator::tests
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.07s
     Running unittests src/lib.rs (target/debug/deps/rt_storage-70d2bb6d0492ef50)

running 7 tests
test elevator::tests::limited_drain_uses_class_weights_and_keeps_remainder_pending ... ok
test elevator::tests::ready_reads_are_offset_sorted_and_coalesced_per_file ... ok
test elevator::tests::choke_critical_and_foreground_bypass_budget ... ok
test elevator::tests::zero_limited_drain_leaves_ready_ops_pending ... ok
test elevator::tests::nvme_degenerates_to_zero_budget ... ok
test elevator::tests::writes_are_ordered_but_not_coalesced ... ok
test elevator::tests::hdd_budget_holds_nonurgent_ops_until_elapsed ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 123 filtered out; finished in 0.00s

     Running tests/storage_move_import_hardware.rs (target/debug/deps/storage_move_import_hardware-d69c015c3fc016d9)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s

     Running tests/storage_real_device.rs (target/debug/deps/storage_real_device-4e54eaa6ccf67876)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.00s


==> cargo test -p rt-storage auto_preallocation_policy
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.08s
     Running unittests src/lib.rs (target/debug/deps/rt_storage-70d2bb6d0492ef50)

running 1 test
test scheduler::tests::auto_preallocation_policy_uses_full_only_for_non_cow_hdd ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 129 filtered out; finished in 0.00s

     Running tests/storage_move_import_hardware.rs (target/debug/deps/storage_move_import_hardware-d69c015c3fc016d9)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s

     Running tests/storage_real_device.rs (target/debug/deps/storage_real_device-4e54eaa6ccf67876)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.00s


==> cargo test -p rt-metrics storage_peer_read_readahead_reduces_backend_reads_for_adjacent_blocks -- --nocapture
   Compiling rt-engine v0.1.0 (/tmp/torrentng-674b283-20260910-v4/crates/rt-engine)
   Compiling rt-api-native v0.1.0 (/tmp/torrentng-674b283-20260910-v4/crates/rt-api-native)
   Compiling rt-api-qbit v0.1.0 (/tmp/torrentng-674b283-20260910-v4/crates/rt-api-qbit)
   Compiling rt-metrics v0.1.0 (/tmp/torrentng-674b283-20260910-v4/crates/rt-metrics)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 7.28s
     Running unittests src/lib.rs (target/debug/deps/rt_metrics-51cb908b55e16dc6)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.00s

     Running tests/scale.rs (target/debug/deps/scale-a94e67aeae0fe940)

running 1 test
test storage_peer_read_readahead_reduces_backend_reads_for_adjacent_blocks ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 18 filtered out; finished in 0.01s


==> cargo test -p rt-metrics storage_hash_pool_does_not_block_peer_read_path -- --nocapture
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.12s
     Running unittests src/lib.rs (target/debug/deps/rt_metrics-51cb908b55e16dc6)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.00s

     Running tests/scale.rs (target/debug/deps/scale-a94e67aeae0fe940)

running 1 test
test storage_hash_pool_does_not_block_peer_read_path ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 18 filtered out; finished in 0.05s


==> cargo test -p rt-metrics storage_positioned_io_preserves_offsets_under_concurrency -- --nocapture
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.14s
     Running unittests src/lib.rs (target/debug/deps/rt_metrics-51cb908b55e16dc6)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.00s

     Running tests/scale.rs (target/debug/deps/scale-a94e67aeae0fe940)

running 1 test
test storage_positioned_io_preserves_offsets_under_concurrency ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 18 filtered out; finished in 0.01s


==> cargo test -p rt-metrics storage_file_pool_stays_bounded_under_active_file_churn -- --nocapture
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.13s
     Running unittests src/lib.rs (target/debug/deps/rt_metrics-51cb908b55e16dc6)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.00s

     Running tests/scale.rs (target/debug/deps/scale-a94e67aeae0fe940)

running 1 test
test storage_file_pool_stays_bounded_under_active_file_churn ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 18 filtered out; finished in 0.01s


==> cargo test -p rt-engine upload_block_reads_across_many_file_regions
   Compiling rt-engine v0.1.0 (/tmp/torrentng-674b283-20260910-v4/crates/rt-engine)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 16.09s
     Running unittests src/lib.rs (target/debug/deps/rt_engine-f64af50313e12476)

running 1 test
test torrent_task::tests::upload_block_reads_across_many_file_regions ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 200 filtered out; finished in 0.03s


==> cargo test -p rt-engine pure_v2_recheck_verifies_file_roots_without_torrent_task
   Compiling rt-engine v0.1.0 (/tmp/torrentng-674b283-20260910-v4/crates/rt-engine)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 12.57s
     Running unittests src/lib.rs (target/debug/deps/rt_engine-3aa293987a4e796d)

running 1 test
test engine::tests::pure_v2_recheck_verifies_file_roots_without_torrent_task ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 200 filtered out; finished in 0.02s


==> cargo test -p rt-storage --test storage_real_device -- --ignored --nocapture
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.08s
     Running tests/storage_real_device.rs (target/debug/deps/storage_real_device-809adf36dba744a8)

running 7 tests
tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-universal-live-b393eb0/tng-storage-bench-R4al9o profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-universal-live-b393eb0/tng-storage-bench-2041Jw profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_backend requested=auto selected=pread reason="auto uses pread baseline; request io_uring explicitly after correctness benchmarks" fixed_buffers=false fixed_buffer_strategy=disabled registered_files=false max_batch_len=1 fixed_buffer_len=0
tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-universal-live-b393eb0/tng-storage-bench-SzsWcc profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-universal-live-b393eb0/tng-storage-bench-PIGdeU profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-universal-live-b393eb0/tng-storage-bench-nKgXVG profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
test backend_selection_roundtrip_reports_capabilities ... ok
tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-universal-live-b393eb0/tng-storage-bench-wOalTx profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_bench_path=/mnt/datapool_lvm_media/.torrentng-universal-live-b393eb0/tng-storage-bench-BxJD80 profile=Hdd fs=Some("ext4") cow=false device=Some(DeviceId("dm-0"))
tng_storage_file_pool reads=10000 hits=9999 misses=1 open_files=1 capacity=64
test repeated_reads_reuse_one_open_file_handle ... ok
tng_storage_elevator blocks=4096 block_len=16384 total_mib=64.00 elapsed_ms=1636 mib_s=39.11 submitted=4096 backend_reads=1 reduction=4096.00x batches=1 coalesced=4095
test hdd_peer_read_elevator_reduces_backend_reads_on_shuffled_adjacent_blocks ... ok
tng_storage_backend_stream requested=pread selected=pread reason="forced by storage backend configuration" blocks=1024 block_len=262144 total_mib=256.00 write_elapsed_ms=2325 read_elapsed_ms=1845 write_mib_s=110.06 read_mib_s=138.70 fixed_buffers=false fixed_buffer_strategy=disabled registered_files=false max_batch_len=1 fixed_buffer_len=0
test backend_stream_roundtrip_reports_throughput ... ok
tng_storage_shuffled_baseline blocks=4096 block_len=16384 total_mib=64.00 elapsed_ms=2504 mib_s=25.56 read_ops=4096 backend_reads=4096
test shuffled_peer_read_baseline_reports_current_scheduler_throughput ... ok
tng_storage_readahead submitted=4096 backend_reads=128 reduction=32.00x elapsed_ms=5284
test peer_read_readahead_reduces_backend_reads_on_adjacent_blocks ... ok
tng_storage_recheck pieces=4096 piece_len=16384 total_mib=64.00 valid=4096 invalid=0 missing=0 first_missing=None elapsed_ms=5000 mib_s=12.80 read_ops=4096 backend_reads=4096 hash_ops=4096 hash_latency_ms=1617
test recheck_range_reports_runtime_progress ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.97s

```

Overall status: PASS_WITH_SKIPS
Skipped gates: 2
