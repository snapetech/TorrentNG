# TorrentNG Universal Compatibility Certification Report

- Date UTC: 2026-09-10T19:02:21Z
- Host: kspld0
- Rust: rustc 1.100.0-nightly (a36d05efa 2026-09-09)
- Cargo: cargo 1.100.0-nightly (3c0b53475 2026-09-04)
- Commit: b393eb0
- Report directory: /home/keith/Documents/code/TorrentNG/certification/reports

## Gates

| Gate | Result |
|---|---|
| API facade endpoint and field matrices | PASS |
| migration dry-run, DB import, and fastresume matrices | PASS |
| migration exported corpus coverage | PASS |
| Track 1 sidecar qBittorrent compatibility flows | PASS |
| native API compatibility manifest | PASS |
| native engine state, tracker, and storage hooks | PASS |
| scale and metrics compatibility evidence | PASS |
| storage topology and peer-read matrix | PASS |
| Docker client interop local matrix | PASS |
| mobile qBittorrent compatibility matrix | PASS |
| public torrent interop matrix | PASS |
| real-device storage matrix | SKIP |

## API facade endpoint and field matrices

```text
/home/keith/Documents/code/TorrentNG/certification/reports/api-facades-universal-20260910T190221Z.md
```

## migration dry-run, DB import, and fastresume matrices

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.04s
     Running unittests src/lib.rs (target/debug/build/rt-migrate/347c1bc606eeb074/out/rt_migrate-347c1bc606eeb074)

running 51 tests
test export::tests::format_parsing_accepts_aliases ... ok
test tests::auxiliary_classification_ignores_hash_coincidences_in_filenames ... ok
test tests::aggregate_json_resume_matches_base32_info_hash_entries ... ok
test tests::bencoded_lifecycle_state_imports_to_native_torrent_row ... ok
test tests::bencoded_file_selection_imports_to_native_file_rows ... ok
test tests::bencoded_tracker_activity_imports_to_native_tracker_rows ... ok
test tests::bencoded_file_progress_imports_to_native_file_rows ... ok
test tests::biglybt_downloads_config_matches_hex_entries ... ok
test tests::file_hints_do_not_trust_final_symlinks ... ok
test tests::broad_sources_are_scannable_metadata_first ... ok
test tests::fastresume_apply_persists_imported_state_and_summary ... ok
test tests::json_file_selection_imports_to_native_file_rows ... ok
test tests::json_file_progress_imports_to_native_file_rows ... ok
test tests::json_lifecycle_state_imports_to_native_torrent_row ... ok
test tests::dry_run_preserves_auxiliary_client_artifacts_separately ... ok
test tests::oversized_resume_sidecar_is_skipped_with_warning ... ok
test tests::json_tracker_activity_imports_to_native_tracker_rows ... ok
test tests::path_remap_uses_longest_matching_prefix ... ok
test tests::padding_files_are_never_marked_wanted ... ok
test tests::recursive_scan_does_not_follow_directory_symlinks ... ok
test tests::path_remap_updates_db_rows_and_file_hint_trust ... ok
test tests::qbit_dry_run_preserves_resume_metadata ... ok
test tests::qbit_libtorrent2_resume_unpacks_bit_packed_pieces_field ... ok
test tests::require_verification_downgrades_imported_valid_pieces ... ok
test tests::rtorrent_complete_resume_synthesizes_seed_piece_state ... ok
test tests::qbit_libtorrent_resume_imports_piece_state ... ok
test tests::rtorrent_multi_file_directory_already_includes_torrent_name ... ok
test tests::utorrent_resume_dat_matches_raw_info_hash_entries ... ok
test tests::utorrent_bitfield_resume_imports_piece_state_under_trust_hints ... ok
test tests::rtorrent_dry_run_reports_missing_resume ... ok
test tests::rtorrent_pairs_hash_torrent_rtorrent_sidecar ... ok
test tests::short_piece_state_is_padded_and_reported ... ok
test tests::tixati_proprietary_state_stays_verification_first ... ok
test tests::rtorrent_multi_file_directory_renamed_by_external_tool_stays_safe_not_silently_broken ... ok
test tests::transmission_dry_run_reads_bencoded_resume ... ok
test tests::rtorrent_single_file_directory_is_left_unchanged ... ok
test tests::import_source_matrix_preserves_common_json_resume_fields ... ok
test tests::partial_piece_blocks_are_sorted_deduped_and_bounded ... ok
test tests::native_import_applies_db_and_fastresume_together ... ok
test export::tests::transmission_export_round_trips ... ok
test export::tests::generic_export_copies_torrent_and_manifest ... ok
test tests::import_plan_applies_native_db_rows ... ok
test export::tests::rtorrent_export_complete_is_recheck_free ... ok
test export::tests::missing_blob_is_skipped_not_fatal ... ok
test export::tests::oversized_blob_is_skipped_before_reading_contents ... ok
test export::tests::libtorrent_export_round_trips_through_qbittorrent_importer ... ok
test export::tests::rtorrent_export_partial_is_metadata_only ... ok
test export::tests::malformed_database_hash_is_skipped_before_path_join ... ok
test export::tests::utorrent_and_biglybt_aggregates_round_trip ... ok
test tests::bencoded_import_source_matrix_preserves_client_specific_aliases ... ok
test tests::native_apply_matrix_persists_common_resume_fields_for_all_sources ... ok

test result: ok. 51 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.08s

     Running tests/round_trip_matrix.rs (target/debug/build/rt-migrate/b8c870eba73f926d/out/round_trip_matrix-b8c870eba73f926d)

running 7 tests
test import_matrix_complete_and_partial_isos ... ok
test production_shape_bep47_padding_file_not_wanted_real_files_trusted ... ok
test production_shape_directory_equals_content_folder_bytes_preserved_and_trusted ... ok
test production_shape_directory_renamed_by_external_tool_stays_safe_metadata_only ... ok
test generic_export_is_universal_exit ... ok
test export_matrix_fidelity_and_layout ... ok
test round_trip_matrix_preserves_state ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s

     Running tests/scale.rs (target/debug/build/rt-migrate/9b39252fb7159ee8/out/scale-9b39252fb7159ee8)

running 1 test
test qbit_15k_dry_run_import_is_certified ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.94s

   Doc-tests rt_migrate

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

```

## migration exported corpus coverage

```text
/home/keith/Documents/code/TorrentNG/certification/reports/migration-corpus-universal-20260910T190223Z.md
```

## Track 1 sidecar qBittorrent compatibility flows

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.08s
     Running unittests src/lib.rs (target/debug/build/torrentng/2930fb9b242c16b7/out/torrentng-2930fb9b242c16b7)

running 2 tests
test qbcompat::handlers::tests::qb_server_state_includes_current_transfer_rates ... ok
test qbcompat::handlers::tests::qb_torrent_maps_started_idle_rows_as_stalled ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 123 filtered out; finished in 0.00s

     Running unittests src/main.rs (target/debug/build/torrentng/69335d2c21f65564/out/torrentng-69335d2c21f65564)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/benchmarks.rs (target/debug/build/torrentng/91a03a689ae2069a/out/benchmarks-91a03a689ae2069a)

running 2 tests
test bench_qb_sync_maindata_delta_under_50ms ... ignored, synthetic performance benchmark; run explicitly
test bench_qb_torrents_info_50k_under_500ms ... ignored, synthetic performance benchmark; run explicitly

test result: ok. 0 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/qbcompat.rs (target/debug/build/torrentng/59a58eddbb8c3444/out/qbcompat-59a58eddbb8c3444)

running 42 tests
test qb_extended_torrent_forms_parse ... ok
test qb_app_read_endpoints_accept_post_for_cross_seed ... ok
test qb_login_accepts_any_credentials ... ok
test qb_log_main_returns_retained_app_events ... ok
test qb_login_sets_session_cookie_for_api_token ... ok
test qb_limit_mutations_require_explicit_valid_values ... ok
test qb_canonical_api_v2_login_is_public ... ok
test qb_api_version ... ok
test qb_integration_flow_read_only_clients ... ok
test qb_set_location_updates_cache_for_known_torrents ... ok
test qb_set_preferences_validates_json ... ok
test qb_rss_rule_rejects_malformed_fields_instead_of_defaulting_them ... ok
test qb_mutation_booleans_fail_closed_and_capabilities_do_not_overclaim ... ok
test qb_create_delete_tags ... ok
test qb_create_remove_category ... ok
test qb_sid_cookie_authorizes_requests ... ok
test qb_rss_rules_use_native_rule_store ... ok
test qb_inert_surfaces_are_compatible ... ok
test qb_set_tags_replaces_cache_tags ... ok
test qb_integration_flow_cross_seed_tracker_and_reannounce ... ok
test qb_search_plugins_jobs_and_rss_items_are_stateful ... ok
test qb_hash_mutations_are_case_insensitive ... ok
test qb_hashes_all_expands_from_cache ... ok
test qb_sync_maindata_incremental ... ok
test qb_sync_maindata_empty ... ok
test qb_sync_maindata_rejects_negative_revision ... ok
test qb_canonical_api_v2_alias ... ok
test qb_sync_maindata_incremental_includes_removed_torrents ... ok
test qb_sync_maindata_includes_category_and_tag_metadata ... ok
test qb_integration_flow_arr_category_tag_and_sync ... ok
test qb_app_extra_info ... ok
test qb_maindata_delta_includes_metadata_changes ... ok
test qb_torrent_export_streams_rtorrent_session_blob ... ok
test qb_torrents_info_empty ... ok
test qb_torrents_info_rejects_malformed_pagination_booleans ... ok
test qb_transfer_info ... ok
test qb_torrents_info_paused_filter_only_returns_inactive ... ok
test qb_version ... ok
test qb_version_probes_are_public_for_arr_clients ... ok
test qb_torrent_properties_from_cache ... ok
test websocket_events_emit_for_qb_metadata_mutations ... ok
test qb_torrents_info_status_filters_match_cache_state ... ok

test result: ok. 42 passed; 0 failed; 0 ignored; 0 measured; 45 filtered out; finished in 0.16s

```

## native API compatibility manifest

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.07s
     Running unittests src/lib.rs (target/debug/build/rt-api-native/2c073f009b95839d/out/rt_api_native-2c073f009b95839d)

running 60 tests
test handlers::tests::api_snapshot_estimates_scale_with_torrent_count ... ok
test handlers::tests::job_view_projects_progress_and_checkpoint_fields ... ok
test handlers::tests::json_store_validation_accepts_documented_rule_shapes ... ok
test handlers::tests::json_store_validation_rejects_malformed_executable_fields ... ok
test handlers::tests::level_from_kind_is_conservative ... ok
test handlers::tests::metric_with_label_escapes_label_values_once ... ok
test handlers::tests::native_engine_capabilities_cover_rewrite_surface ... ok
test handlers::tests::native_webui_capabilities_match_mounted_routes ... ok
test handlers::tests::delete_torrent_found ... ok
test handlers::tests::diagnostics_without_engine_returns_unavailable ... ok
test handlers::tests::get_torrent_found ... ok
test handlers::tests::session_event_response_rejects_corrupt_durable_payload ... ok
test handlers::tests::session_event_response_projects_level_and_payload ... ok
test handlers::tests::delete_torrent_not_found ... ok
test handlers::tests::add_torrent_without_engine_returns_unavailable ... ok
test handlers::tests::list_torrents_empty ... ok
test handlers::tests::get_torrent_not_found ... ok
test handlers::tests::jobs_without_engine_returns_unavailable ... ok
test handlers::tests::idempotency_key_replays_native_mutation_and_rejects_reuse ... ok
test handlers::tests::health_reports_unavailable_without_engine ... ok
test handlers::tests::mutating_endpoint_accepts_session_cookie_token ... ok
test handlers::tests::storage_plan_preview_projects_staged_import_copy ... ok
test handlers::tests::storage_plan_preview_projects_move_steps ... ok
test handlers::tests::storage_plan_completed_steps_accept_sorted_unique_subset ... ok
test handlers::tests::storage_plan_completed_steps_are_bounded_by_plan ... ok
test handlers::tests::storage_plan_root_validation_rejects_escape ... ok
test handlers::tests::update_torrent_limits_request_distinguishes_null_from_absent ... ok
test handlers::tests::utp_capability_helpers_match_runtime_policy_values ... ok
test handlers::tests::list_torrents_reports_total_independent_of_page_size ... ok
test handlers::tests::render_metrics_exposes_dependency_health_and_pressure ... ok
test state::tests::signed_summary_projection_saturates_unsigned_counters ... ok
test handlers::tests::torrent_delta_chunks_initial_snapshot_at_one_revision ... ok
test handlers::tests::torrent_delta_reports_initial_changes_and_removals ... ok
test handlers::tests::list_torrents_with_entry ... ok
test handlers::tests::list_torrents_snapshot_pins_pages_across_mutations ... ok
test state::tests::journal_refresh_applies_final_entry_state_without_registry_scan ... ok
test state::tests::text_filter_index_handles_short_filters_and_case_folding ... ok
test state::tests::media_type_facets_are_indexed_and_updated_incrementally ... ok
test state::tests::structural_snapshot_refresh_does_not_use_stale_positions ... ok
test handlers::tests::set_category_without_engine_updates_registry ... ok
test handlers::tests::transfer_limits_without_engine_returns_unavailable ... ok
test handlers::tests::torrent_limits_without_engine_returns_unavailable ... ok
test handlers::tests::storage_without_engine_returns_unavailable ... ok
test handlers::tests::update_torrent_without_engine_updates_registry ... ok
test handlers::tests::tag_post_delete_and_bulk_set_are_native ... ok
test handlers::tests::render_metrics_includes_engine_stats ... ok
test handlers::tests::native_hash_resolution_preserves_unknown_targets_for_error_reporting ... ok
test handlers::tests::pause_torrent_found ... ok
test handlers::tests::resume_torrent_found ... ok
test handlers::tests::patch_files_rejects_empty_body ... ok
test handlers::tests::patch_tags_without_engine_updates_registry ... ok
test handlers::tests::mutating_endpoint_accepts_bearer_token ... ok
test handlers::tests::mutating_endpoint_requires_configured_token ... ok
test handlers::tests::native_rtorrent_compatibility_routes_fail_closed ... ok
test handlers::tests::recheck_torrent_found ... ok
test handlers::tests::metrics_reports_unavailable_without_engine ... ok
test handlers::tests::patch_trackers_without_engine_reports_unavailable ... ok
test handlers::tests::native_alias_and_projection_routes_are_exposed ... ok
test handlers::tests::reannounce_torrent_without_engine_is_unavailable ... ok
test handlers::tests::native_login_issues_session_cookie_and_validates_tokens ... ok

test result: ok. 60 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s

   Doc-tests rt_api_native

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

```

## native engine state, tracker, and storage hooks

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.05s
     Running unittests src/lib.rs (target/debug/build/rt-engine/b1388d4ccd56ccf4/out/rt_engine-b1388d4ccd56ccf4)

running 201 tests
test command::tests::engine_stats_accumulates_torrent_runtime_storage_counters ... ok
test command::tests::engine_stats_tracks_top_hot_torrent_memory_estimates ... ok
test dht_task::tests::announced_peer_cache_is_bounded_and_keeps_duplicates ... ok
test dht_task::tests::announce_peer_query_uses_configured_listen_port ... ok
test dht_task::tests::announce_peer_requires_matching_token ... ok
test dht_task::tests::announce_peer_stores_peer_and_get_peers_returns_it ... ok
test dht_task::tests::announce_peer_stores_ipv6_peer_and_get_peers_returns_it ... ok
test dht_task::tests::closest_response_includes_known_nodes_and_token ... ok
test dht_task::tests::dht_ingress_budget_bounds_each_ip_and_expires_windows ... ok
test dht_task::tests::runtime_stats_count_dht_owned_caches ... ok
test dht_task::tests::prune_stale_outstanding_removes_expired_entries_only ... ok
test dht_task::tests::get_peers_response_forwards_discovered_peers_to_torrent ... ok
test dht_task::tests::lookup_continues_to_unqueried_closer_nodes ... ok
test egress_policy::tests::default_policy_denies_sensitive_address_ranges ... ok
test dht_task::tests::transaction_ids_are_nonzero_and_advance ... ok
test egress_policy::tests::default_policy_allows_only_http_webseeds ... ok
test dht_task::tests::lookup_restart_clears_previously_queried_nodes ... ok
test egress_policy::tests::default_policy_allows_public_tracker_schemes_only ... ok
test db_worker::tests::shutdown_drains_prior_work ... ok
test dht_task::tests::response_from_wrong_source_address_is_ignored ... ok
test dht_task::tests::response_with_unknown_transaction_id_is_ignored ... ok
test egress_policy::tests::policy_can_explicitly_allow_private_lan ... ok
test db_worker::tests::cancelled_queued_operation_is_not_applied ... ok
test db_worker::tests::operations_are_serial_and_worker_survives_errors_and_panics ... ok
test engine::tests::decode_info_hash_bytes_accepts_v1_and_v2_lengths ... ok
test engine::tests::engine_handle_liveness_tracks_command_receiver ... ok
test engine::tests::engine_handle_reports_peer_listener_health_separately ... ok
test egress_policy::tests::http_clients_are_reused_after_each_address_is_revalidated ... ok
test engine::tests::engine_handle_liveness_drops_on_actor_panic ... ok
test engine::tests::incoming_utp_listener_flag_is_boolean_only ... ok
test engine::tests::label_normalization_trims_dedupes_and_drops_empty_values ... ok
test engine::tests::metadata_placeholder_projection_preserves_trackers ... ok
test engine::tests::metadata_projection_preserves_files_trackers_and_privacy ... ok
test engine::tests::parse_info_hash_hex_rejects_invalid_input ... ok
test engine::tests::pure_v2_metadata_projects_to_engine_and_db_shapes ... ok
test command::tests::hot_seeding_1k_memory_attribution_stays_under_cap ... ok
test engine::tests::engine_handle_shutdown_aborts_an_actor_stuck_after_accepting_command ... ok
test engine::tests::prune_empty_dirs_stops_at_root_and_keeps_nonempty_dirs ... ok
test dht_task::tests::dht_ingress_budget_has_bounded_source_state ... ok
test db_worker::tests::sqlite_failure_crosses_boundary_and_worker_continues ... ok
test db_worker::tests::worker_owns_a_real_sqlite_connection_and_persists_ordered_work ... ok
test engine::tests::append_session_event_persists_payload ... ok
test engine::tests::add_peers_forwards_external_peers_to_torrent_task ... ok
test dht_task::tests::announced_peer_global_cap_applies_to_existing_info_hashes ... ok
test engine::tests::add_torrent_rolls_back_registry_and_blob_when_db_persist_fails ... ok
test engine::tests::add_torrent_rolls_back_registry_row_when_blob_write_fails ... ok
test engine::tests::row_conversion_preserves_session_fields ... ok
test engine::tests::row_conversion_saturates_unsigned_values_at_sqlite_integer_limit ... ok
test engine::tests::add_v2_only_magnet_persists_metadata_placeholder ... ok
test engine::tests::complete_v2_only_magnet_persists_metadata_without_task ... ok
test engine::tests::storage_io_config_maps_native_storage_toml ... ok
test dht_task::tests::announced_peer_map_is_bounded_across_info_hashes ... ok
test engine::tests::recheck_job_control_sends_torrent_commands_and_updates_state ... ok
test engine::tests::queue_order_moves_are_persisted ... ok
test engine::tests::failed_payload_cleanup_keeps_torrent_retryable ... ok
test engine::tests::malformed_persisted_control_settings_fail_closed ... ok
test engine::tests::storage_plan_resume_steps_are_sorted_unique_and_bounded ... ok
test engine::tests::metadata_projection_rejects_unrepresentable_file_policy_index ... ok
test engine::tests::load_persisted_torrents_restores_seeding_rows_as_dormant ... ok
test engine::tests::finished_torrent_task_is_removed_and_marked_error ... ok
test engine::tests::register_configured_storage_persists_root_and_mount ... ok
test engine::tests::global_limits_persist_to_settings_table ... ok
test engine::tests::load_persisted_torrents_restores_paused_registry_as_dormant ... ok
test engine::tests::network_features_persist_and_notify_running_torrents ... ok
test engine::tests::recover_interrupted_jobs_pauses_running_work ... ok
test engine::tests::recheck_job_helpers_persist_state_and_events ... ok
test engine::tests::engine_stats_include_registry_jobs_and_trackers ... ok
test metadata_task::tests::dht_only_peer_candidates_can_complete_magnet_metadata ... ok
test metadata_task::tests::metadata_attempt_cache_cap_scales_with_peer_limit ... ok
test metadata_task::tests::metadata_fetch_candidates_are_bounded_and_prune_retry_history ... ok
test metadata_task::tests::metadata_fetch_reservation_uses_metadata_governor_class ... ok
test metadata_task::tests::metadata_peer_retry_has_cooldown ... ok
test metadata_task::tests::parses_metadata_transport_policy ... ok
test metadata_task::tests::validates_metadata_info_hash ... ok
test metadata_task::tests::validates_metadata_piece_lengths ... ok
test network_budget::tests::limited_budget_accepts_request_larger_than_bucket_capacity ... ok
test network_budget::tests::limited_budget_refills_after_wait ... ok
test network_budget::tests::peer_slots_are_shared_across_clones ... ok
test network_budget::tests::unlimited_budget_does_not_wait ... ok
test peer_id::tests::default_identity_is_upstream_rtorrent_0_16_11_pair ... ok
test peer_id::tests::independently_generated_peer_ids_do_not_collide ... ok
test peer_id::tests::init_persists_and_is_idempotent_for_this_process ... ok
test peer_id::tests::persisted_suffix_is_stable_across_reloads ... ok
test peer_id::tests::runtime_user_agent_rejects_empty_or_non_ascii_values ... ok
test peer_ingress::tests::cancelled_admission_does_not_consume_per_ip_slot ... ok
test peer_ingress::tests::global_budget_limits_unrouted_handshakes ... ok
test peer_ingress::tests::global_rejection_does_not_consume_per_ip_slot ... ok
test peer_ingress::tests::per_ip_budget_limits_connection_storms ... ok
test storage_authority::tests::configured_roots_are_canonicalized_and_deduped ... ok
test storage_authority::tests::empty_roots_are_rejected ... ok
test storage_authority::tests::existing_and_missing_descendants_are_authorized ... ok
test storage_authority::tests::missing_roots_are_rejected ... ok
test storage_authority::tests::relative_parent_and_outside_paths_are_rejected ... ok
test storage_authority::tests::symlinked_existing_path_is_checked_by_canonical_target ... ok
test engine::tests::load_persisted_v2_rows_restore_taskless_registry_and_trackers ... ok
test engine::tests::engine_start_owns_background_storage_supervisor_until_shutdown ... ok
test storage_jobs::tests::dispatcher_rejects_duplicate_active_job_ids ... ok
test engine::tests::reserve_memory_command_holds_and_releases_lease ... ok
test engine::tests::pause_torrent_pauses_taskless_pure_v2_recheck_job ... ok
test engine::tests::private_magnet_completion_removes_provisional_dht_and_preserves_pause ... ok
test engine::tests::shutdown_torrent_tasks_sends_shutdown_and_waits_for_task_exit ... ok
test engine::tests::recovered_storage_plan_reconciles_filesystem_ahead_of_checkpoint ... ok
test engine::tests::recovered_delete_job_finalizes_metadata_after_payload_cleanup ... ok
test engine::tests::rename_file_and_folder_paths_update_metadata_projection ... ok
test engine::tests::remove_torrent_queues_payload_cleanup_outside_engine_actor ... ok
test storage_jobs::tests::worker_registration_guard_releases_on_unwind ... ok
test tier::tests::active_engine_work_stays_hot ... ok
test tier::tests::compact_piece_bitmap_roundtrips_and_rejects_tail_bits ... ok
test tier::tests::compact_piece_bitmap_supports_mutation_and_projection ... ok
test engine::tests::pure_v2_recheck_verifies_file_roots_without_torrent_task ... ok
test tier::tests::dormant_snapshot_is_accounted_and_cleared_on_promotion ... ok
test tier::tests::dormant_snapshot_keeps_only_small_tracker_deadline_inputs ... ok
test tier::tests::paused_stopped_and_error_default_to_dormant ... ok
test tier::tests::peer_activity_promotes_any_startable_state_to_hot ... ok
test tier::tests::rescheduling_and_cancelling_removes_stale_idle_deadlines ... ok
test tier::tests::scale_snapshot_enforces_100k_two_percent_proxy_budget ... ok
test tier::tests::tier_controller_promotes_and_demotes_with_shared_idle_checks ... ok
test tier::tests::tier_is_orthogonal_to_lifecycle_state_for_idle_seeds ... ok
test tier::tests::timer_wheel_pops_due_torrents_without_one_timer_per_entry ... ok
test tier::tests::timer_wheel_reschedule_and_cancel_ignore_stale_slots ... ok
test tier::tests::tracker_deadline_wheel_wakes_only_due_torrents ... ok
test tier::tests::tracker_due_idle_seed_stays_warm_without_task_activity ... ok
test torrent_task::tests::auto_outgoing_utp_policy_is_source_and_privacy_aware ... ok
test torrent_task::tests::choke_state_does_not_commit_when_peer_mailbox_is_full ... ok
test torrent_task::tests::configured_piece_assembly_cap_is_per_torrent_soft_ceiling ... ok
test torrent_task::tests::decodes_peer_wire_bitfield_msb_first ... ok
test torrent_task::tests::encodes_piece_flags_to_peer_wire_bitfield_msb_first ... ok
test torrent_task::tests::file_hints_capture_size_mtime_and_inode ... ok
test torrent_task::tests::file_hints_omit_missing_files_without_failing_the_whole_torrent ... ok
test torrent_task::tests::have_delivery_keeps_a_bounded_retry_until_mailbox_accepts_it ... ok
test torrent_task::tests::metadata_response_rejects_unrepresentable_or_out_of_range_piece ... ok
test torrent_task::tests::outstanding_piece_match_requires_exact_length ... ok
test torrent_task::tests::parses_outgoing_utp_policy ... ok
test torrent_task::tests::parses_ut_pex_added6_ipv6_peers ... ok
test torrent_task::tests::parses_ut_pex_added_and_added6_together ... ok
test engine::tests::startup_reconciles_missing_rows_and_quarantines_orphan_projections ... ok
test torrent_task::tests::parses_ut_pex_added_ipv4_peers ... ok
test torrent_task::tests::parses_ut_pex_dropped_ipv4_and_ipv6_peers ... ok
test torrent_task::tests::peer_availability_reconcile_counts_only_transitions ... ok
test torrent_task::tests::peer_event_channel_capacity_is_bounded_by_global_peer_budget ... ok
test torrent_task::tests::piece_assembly_budget_evicts_oldest_incomplete_piece ... ok
test torrent_task::tests::piece_assembly_budget_preserves_current_piece_when_possible ... ok
test torrent_task::tests::piece_assembly_budget_stops_at_current_piece_only ... ok
test torrent_task::tests::piece_assembly_rejects_conflicting_duplicate_block ... ok
test torrent_task::tests::piece_assembly_rejects_out_of_range_block ... ok
test torrent_task::tests::piece_assembly_tracks_complete_piece_bytes ... ok
test torrent_task::tests::private_peer_allowlist_only_accepts_tracker_peers ... ok
test torrent_task::tests::private_torrent_extension_handshake_does_not_advertise_pex ... ok
test torrent_task::tests::rejects_invalid_peer_wire_bitfield_shape ... ok
test torrent_task::tests::request_pipeline_reduces_near_piece_assembly_cap ... ok
test engine::tests::categories_survive_engine_restart_and_keep_torrent_labels_consistent ... ok
test torrent_task::tests::stopped_announce_is_consumed_once_per_session ... ok
test torrent_task::tests::stricter_limit_uses_lowest_enabled_limit ... ok
test torrent_task::tests::super_seed_visible_pieces_handles_empty_have_set ... ok
test torrent_task::tests::super_seed_visible_pieces_reveals_one_available_piece ... ok
test torrent_task::tests::timed_out_requests_are_returned_for_requeue ... ok
test torrent_task::tests::tracker_lifecycle_events_clear_after_one_success ... ok
test torrent_task::tests::tracker_peer_cache_cap_scales_with_peer_limit ... ok
test torrent_task::tests::tracker_peer_cache_drops_new_peers_after_cap ... ok
test torrent_task::tests::tracker_status_persistence_fields_are_stable ... ok
test torrent_task::tests::tracker_tiers_preserve_bep12_order_and_dedupe ... ok
test engine::tests::storage_move_db_failure_keeps_destination_live_and_commit_pending ... ok
test torrent_task::tests::upload_block_reads_across_many_file_regions ... ok
test torrent_task::tests::upload_block_reservation_uses_peer_buffer_governor_class ... ok
test torrent_task::tests::upload_context_piece_map_is_shared_not_deep_cloned_per_peer ... ok
test torrent_task::tests::webseed_block_url_accepts_direct_file_and_base_url ... ok
test torrent_task::tests::webseed_block_url_expands_single_file_directory_prefix ... ok
test torrent_task::tests::webseed_body_reservation_uses_webseed_governor_class ... ok
test torrent_task::tests::webseed_retry_delay_is_exponential_and_bounded ... ok
test tracker_runtime::tests::tracker_worker_budget_is_explicit_and_bounded ... ok
test engine::tests::storage_operation_admission_rejects_active_same_torrent_job ... ok
test engine::tests::storage_plan_execution_rejects_missing_server_roots ... ok
test engine::tests::subsystem_health_reports_dead_dependency_seams ... ok
test engine::tests::storage_root_projection_reports_capacity_and_root_errors ... ok
test engine::tests::torrent_blob_export_preserves_raw_metainfo_bytes ... ok
test engine::tests::torrent_diagnostic_explains_paused_private_tracker_gap ... ok
test engine::tests::update_torrent_limits_persists_and_reads_back ... ok
test engine::tests::storage_plan_execution_uses_persisted_roots_and_fails_closed ... ok
test engine::tests::update_torrent_limits_notifies_running_torrent_task ... ok
test engine::tests::user_agent_update_persists_and_changes_runtime_clients ... ok
test engine::tests::update_torrent_trackers_persists_summary_and_detail_rows ... ok
test storage_jobs::tests::closed_worker_releases_end_to_end_inflight_registration ... ok
test engine::tests::storage_plan_jobs_checkpoint_completed_steps ... ok
test storage_jobs::tests::queue_is_bounded_and_control_is_shared ... ok
test storage_jobs::tests::injected_worker_panic_is_contained_and_next_job_runs ... ok
test storage_jobs::tests::move_persistence_failure_remains_commit_pending ... ok
test storage_jobs::tests::terminal_persistence_records_partial_progress ... ok
test engine::tests::update_save_path_moves_existing_payload_through_storage_plan ... ok
test storage_jobs::tests::production_dispatcher_uses_dedicated_database_connection ... ok
test torrent_task::tests::transfer_stats_are_batched_until_progress_flush ... ok
test torrent_task::tests::seed_ratio_limit_pauses_a_completed_torrent ... ok
test storage_jobs::tests::dispatcher_rejects_when_end_to_end_inflight_bound_is_reached ... ok
test storage_jobs::tests::pause_at_step_boundary_releases_worker_for_another_job ... ok
test tier::tests::controller_tracks_100k_registry_entries_with_only_two_percent_hot ... ok
test storage_jobs::tests::paused_job_waits_without_consuming_worker_slot_until_resumed ... ok
test storage_jobs::tests::shutdown_requeues_active_and_queued_jobs_for_restart ... ok
test engine::tests::engine_command_send_is_bounded_when_mailbox_is_full ... ok
test engine::tests::engine_health_reply_is_bounded_when_actor_stops_replying ... ok
test torrent_task::tests::peer_event_delivery_is_bounded_when_torrent_actor_stalls ... ok
test engine::tests::dormant_promotion_is_detached_and_coalesced ... ok
test engine::tests::update_save_path_reroutes_running_task_and_recheck_finds_new_root ... ok

test result: ok. 201 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.26s

   Doc-tests rt_engine

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

```

## scale and metrics compatibility evidence

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.06s
     Running unittests src/lib.rs (target/debug/build/rt-metrics/c8f0dd102296f94f/out/rt_metrics-c8f0dd102296f94f)

running 6 tests
test counter::tests::counter_inc_and_get ... ok
test counter::tests::counter_reset ... ok
test counter::tests::metrics_snapshot ... ok
test resource::tests::class_and_global_caps_are_enforced ... ok
test resource::tests::leases_release_on_drop ... ok
test resource::tests::pressure_transitions_are_deterministic ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/scale.rs (target/debug/build/rt-metrics/2cbaaff9aaf8ca8d/out/scale-2cbaaff9aaf8ca8d)

running 19 tests
test recheck_does_not_starve_seeding_peer_reads ... ok
test crash_watermark_bounds_restart_recheck_to_dirty_pieces ... ok
test tracker_restart_storm_15k_is_spread_by_jitter ... ok
test storage_recheck_hashing_reports_scheduler_result_without_runtime_stall ... ok
test storage_peer_read_readahead_reduces_backend_reads_for_adjacent_blocks ... ok
test completed_piece_ram_hash_avoids_read_after_write_backend_reads ... ok
test storage_positioned_io_preserves_offsets_under_concurrency ... ok
test storage_file_pool_stays_bounded_under_active_file_churn ... ok
test storage_hash_pool_does_not_block_peer_read_path ... ok
test list_1k_torrents_under_100ms ... ok
test sparse_recheck_skips_holes_and_reports_extent_counters ... ok
test idle_memory_15k_under_2_5gb ... ok
test qbit_info_50k_under_500ms ... ok
test list_10k_torrents_under_200ms ... ok
test sync_maindata_15k_under_50ms ... ok
test list_15k_torrents_under_500ms ... ok
test native_filter_sort_15k_under_250ms ... ok
test idle_memory_100k_keeps_fixed_rss_task_fd_budget ... ok
test cold_db_load_15k_under_120s ... ok

test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.72s

   Doc-tests rt_metrics

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

```

## storage topology and peer-read matrix

```text

==> cargo test -p rt-storage
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.03s
     Running unittests src/lib.rs (target/debug/build/rt-storage/ec73286247d93d6f/out/rt_storage-ec73286247d93d6f)

running 130 tests
test backend::tests::fixed_buffer_strategy_names_are_stable_for_metrics ... ok
test backend::tests::uring_fixed_buffer_registration_budget_stays_below_common_memlock_limit ... ok
test device::tests::cow_fs_detection_covers_common_filesystems ... ok
test device::tests::mount_source_subpath_suffix_is_ignored ... ok
test device::tests::block_device_profile_uses_name_and_rotational_flag ... ok
test device::tests::mountinfo_read_is_bounded ... ok
test device::tests::sysfs_block_device_name_prefers_parent_block_device ... ok
test fd_limit::tests::capacity_respects_floor ... ok
test fd_limit::tests::capacity_scales_with_limit ... ok
test fd_limit::tests::raise_returns_nonzero ... ok
test elevator::tests::hdd_budget_holds_nonurgent_ops_until_elapsed ... ok
test device::tests::mountinfo_parser_decodes_paths_and_finds_longest_prefix ... ok
test elevator::tests::nvme_degenerates_to_zero_budget ... ok
test elevator::tests::zero_limited_drain_leaves_ready_ops_pending ... ok
test elevator::tests::writes_are_ordered_but_not_coalesced ... ok
test elevator::tests::choke_critical_and_foreground_bypass_budget ... ok
test device::tests::network_topology_uses_mount_source_as_device_id ... ok
test frame::tests::buffers_are_reused_within_class ... ok
test backend::tests::forcing_pread_selects_pread ... ok
test device::tests::topology_reports_fs_profile_and_cow ... ok
test frame::tests::into_bytes_releases_charge_without_copying_payload ... ok
test backend::tests::pread_backend_queue_fails_closed_when_full ... ok
test frame::tests::registered_slot_frame_keeps_charge_until_drop ... ok
test elevator::tests::limited_drain_uses_class_weights_and_keeps_remainder_pending ... ok
test backend::tests::backend_request_parses_user_values ... ok
test elevator::tests::ready_reads_are_offset_sorted_and_coalesced_per_file ... ok
test handle_cache::tests::ancestor_symlink_is_rejected_before_opening_the_file ... ok
test frame::tests::cap_enforced_with_backpressure ... ok
test frame::tests::acquire_release_roundtrips_capacity ... ok
test backend::tests::pread_past_eof_errors ... ok
test handle_cache::tests::final_component_symlink_is_rejected ... ok
test backend::tests::selected_backend_roundtrip ... ok
test handle_cache::tests::missing_file_read_errors_and_is_not_cached ... ok
test handle_cache::tests::read_and_write_handles_are_distinct ... ok
test frame::tests::oversize_is_exact_and_counted ... ok
test backend::tests::pwrite_then_pread_roundtrip ... ok
test handle_cache::tests::reuses_same_handle_for_repeated_opens ... ok
test open::tests::limited_read_rejects_oversized_runtime_file ... ok
test handle_cache::tests::lru_evicts_least_recently_used ... ok
test plan::tests::delete_requires_prior_dry_run_approval ... ok
test plan::tests::checkpoint_failure_leaves_committed_step_resumable ... ok
test plan::tests::descriptor_anchored_execution_rejects_an_ancestor_symlink ... ok
test plan::tests::cleanup_plan_is_idempotent_and_prunes_only_empty_directories ... ok
test plan::tests::controlled_copy_aborts_mid_step_and_rolls_back_staging ... ok
test plan::tests::execute_import_copy_failure_does_not_leave_final_destination ... ok
test plan::tests::execute_copy_verify_plan_rejects_symlink_source_file ... ok
test plan::tests::execute_plan_under_roots_rejects_delete_escape ... ok
test plan::tests::execute_delete_plan_removes_directory_tree_after_approval ... ok
test plan::tests::execute_copy_verify_plan_rolls_back_staged_file_on_short_copy ... ok
test plan::tests::execute_plan_under_roots_rejects_destination_escape_before_copy ... ok
test plan::tests::execute_delete_plan_removes_symlink_without_following_target ... ok
test plan::tests::move_plan_uses_rename_on_same_filesystem ... ok
test plan::tests::plan_import_treats_broken_destination_symlink_as_existing ... ok
test plan::tests::move_plan_copy_verify_path_deletes_source_after_verified_rename ... ok
test plan::tests::move_plan_copy_verify_path_removes_source_directory_after_verified_rename ... ok
test plan::tests::execute_import_plan_rejects_symlink_source_before_hardlink ... ok
test plan::tests::execute_import_plan_links_or_copies_without_removing_source ... ok
test plan::tests::execute_move_plan_rejects_symlink_source_before_rename ... ok
test plan::tests::execute_move_plan_renames_without_overwrite ... ok
test plan::tests::execute_copy_verify_plan_rejects_nested_symlink_entry ... ok
test plan::tests::execute_plan_reports_rollback_step_failure_in_error_message ... ok
test plan::tests::execute_copy_verify_plan_copies_directory_tree_and_verifies_bytes ... ok
test plan::tests::execute_plan_under_roots_allows_confined_move ... ok
test plan::tests::repeating_a_completed_copy_plan_is_idempotent ... ok
test plan::tests::retrying_a_completed_plan_is_idempotent ... ok
test plan::tests::verify_content_matches_detects_bit_flip_despite_matching_length ... ok
test backend::tests::uring_probe_is_diagnostic_not_panic ... ok
test plan::tests::reconcile_detects_rename_committed_before_checkpoint ... ok
test scheduler::tests::auto_preallocation_policy_uses_full_only_for_non_cow_hdd ... ok
test plan::tests::plan_delete_treats_broken_symlink_as_existing_source ... ok
test plan::tests::execute_plan_under_roots_rejects_parent_dir_components ... ok
test plan::tests::move_plan_reports_conflict_and_capacity ... ok
test scheduler::tests::blocking_pool_full_queue_fails_closed ... ok
test plan::tests::reconcile_rejects_corrupt_staging_copy ... ok
test runtime::tests::backend_short_io_maps_to_storage_error ... ok
test runtime::tests::backend_would_block_maps_to_queue_full ... ok
test plan::tests::failed_checkpoint_can_resume_without_repeating_filesystem_mutation ... ok
test plan::tests::import_plan_detects_existing_destination ... ok
test plan::tests::import_copy_plan_stages_before_final_rename ... ok
test scheduler::tests::peer_read_elevator_full_queue_fails_closed ... ok
test backend::tests::uring_request_has_clean_probe_fallback ... ok
test scheduler::tests::acquire_and_release_recheck ... ok
test backend::tests::uring_strategy_reports_frame_pool_slots_only_with_registered_buffers ... ok
test scheduler::tests::peer_read_not_starved_by_recheck ... ok
test scheduler::tests::scheduler_new_resolves_auto_to_sparse_without_path_topology ... ok
test scheduler::tests::ssd_has_higher_concurrency ... ok
test runtime::tests::missing_file_maps_to_not_found ... ok
test scheduler::tests::compatibility_read_still_returns_bytes ... ok
test scheduler::tests::full_mount_queue_fails_closed ... ok
test scheduler::tests::strict_write_sync_is_counted_and_not_left_dirty ... ok
test scheduler::tests::write_does_not_create_by_default ... ok
test scheduler::tests::stats_track_io_sync_and_hash_work ... ok
test scheduler::tests::peer_read_readahead_cache_is_config_bounded ... ok
test scheduler::tests::runtime_file_pool_rejects_an_ancestor_symlink ... ok
test scheduler::tests::file_pool_records_hits_and_evictions ... ok
test scheduler::tests::queued_disk_bytes_track_active_blocking_job_payload ... ok
test verify::tests::v2_file_verify_accepts_empty_file_without_root ... ok
test scheduler::tests::prepare_file_refuses_to_shrink_existing_data ... ok
test scheduler::tests::short_positioned_read_maps_to_storage_error ... ok
test scheduler::tests::queued_disk_governor_denies_before_enqueue ... ok
test scheduler::tests::peer_read_readahead_cache_returns_exact_requested_bytes ... ok
test backend::tests::forced_uring_roundtrip_when_kernel_supports_it ... ok
test scheduler::tests::prepare_file_does_not_create_through_an_ancestor_symlink ... ok
test scheduler::tests::read_and_write_roundtrip ... ok
test verify::tests::v2_file_verify_rejects_wrong_file_root ... ok
test verify::tests::v2_file_verify_accepts_matching_file_root ... ok
test verify::tests::verify_missing_file ... ok
test verify::tests::verify_range_returns_empty_for_reversed_or_out_of_range_bounds ... ok
test scheduler::tests::peer_read_readahead_cache_can_be_disabled ... ok
test verify::tests::verify_range_resumable ... ok
test scheduler::tests::peer_read_readahead_cache_is_invalidated_by_writes ... ok
test scheduler::tests::sync_all_open_files_syncs_dirty_paths_after_fd_eviction ... ok
test verify::tests::verify_all_reports_per_piece ... ok
test verify::tests::verify_invalid_piece ... ok
test verify::tests::verify_valid_piece ... ok
test scheduler::tests::hdd_peer_read_elevator_batches_shuffled_adjacent_reads ... ok
test verify::tests::verify_truncated_sparse_file_is_missing_not_zero_filled ... ok
test verify::tests::v2_file_verify_hashes_sparse_holes_as_zeroes ... ok
test verify::tests::verify_sparse_piece_hashes_holes_as_zeroes ... ok
test plan::tests::reconcile_infers_copy_when_following_rename_is_already_committed ... ok
test runtime::tests::global_read_write_roundtrip ... ok
test scheduler::tests::concurrent_positioned_writes_do_not_share_cursor ... ok
test scheduler::tests::sparse_prepare_creates_parent_once ... ok
test scheduler::tests::schedulers_on_same_device_share_global_queue ... ok
test scheduler::tests::read_nonexistent_file_does_not_create ... ok
test scheduler::tests::prepare_file_extends_without_disturbing_existing_bytes ... ok
test scheduler::tests::owned_read_returns_pooled_frame_for_exact_backend_read ... ok
test scheduler::tests::large_peer_and_recheck_reads_emit_page_cache_advice ... ok
test scheduler::tests::hdd_peer_read_elevator_dispatches_after_quiet_slice ... ok
test handle_cache::tests::idle_sweep_closes_stale_handles ... ok

test result: ok. 130 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s

     Running tests/storage_move_import_hardware.rs (target/debug/build/rt-storage/1dd684681865526e/out/storage_move_import_hardware-1dd684681865526e)

running 1 test
test move_import_delete_executor_runs_on_configured_storage_root ... ignored, real-root move/import certification; set TNG_STORAGE_MOVE_IMPORT_ROOT

test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/storage_real_device.rs (target/debug/build/rt-storage/b3e5d8f1759acd32/out/storage_real_device-b3e5d8f1759acd32)

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
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.03s
     Running unittests src/lib.rs (target/debug/build/rt-storage/ec73286247d93d6f/out/rt_storage-ec73286247d93d6f)

running 8 tests
test device::tests::cow_fs_detection_covers_common_filesystems ... ok
test device::tests::mount_source_subpath_suffix_is_ignored ... ok
test device::tests::block_device_profile_uses_name_and_rotational_flag ... ok
test device::tests::mountinfo_read_is_bounded ... ok
test device::tests::mountinfo_parser_decodes_paths_and_finds_longest_prefix ... ok
test device::tests::sysfs_block_device_name_prefers_parent_block_device ... ok
test device::tests::network_topology_uses_mount_source_as_device_id ... ok
test device::tests::topology_reports_fs_profile_and_cow ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 122 filtered out; finished in 0.00s

     Running tests/storage_move_import_hardware.rs (target/debug/build/rt-storage/1dd684681865526e/out/storage_move_import_hardware-1dd684681865526e)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s

     Running tests/storage_real_device.rs (target/debug/build/rt-storage/b3e5d8f1759acd32/out/storage_real_device-b3e5d8f1759acd32)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.00s


==> cargo test -p rt-storage elevator::tests
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.03s
     Running unittests src/lib.rs (target/debug/build/rt-storage/ec73286247d93d6f/out/rt_storage-ec73286247d93d6f)

running 7 tests
test elevator::tests::limited_drain_uses_class_weights_and_keeps_remainder_pending ... ok
test elevator::tests::hdd_budget_holds_nonurgent_ops_until_elapsed ... ok
test elevator::tests::choke_critical_and_foreground_bypass_budget ... ok
test elevator::tests::ready_reads_are_offset_sorted_and_coalesced_per_file ... ok
test elevator::tests::zero_limited_drain_leaves_ready_ops_pending ... ok
test elevator::tests::nvme_degenerates_to_zero_budget ... ok
test elevator::tests::writes_are_ordered_but_not_coalesced ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 123 filtered out; finished in 0.00s

     Running tests/storage_move_import_hardware.rs (target/debug/build/rt-storage/1dd684681865526e/out/storage_move_import_hardware-1dd684681865526e)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s

     Running tests/storage_real_device.rs (target/debug/build/rt-storage/b3e5d8f1759acd32/out/storage_real_device-b3e5d8f1759acd32)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.00s


==> cargo test -p rt-storage auto_preallocation_policy
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.03s
     Running unittests src/lib.rs (target/debug/build/rt-storage/ec73286247d93d6f/out/rt_storage-ec73286247d93d6f)

running 1 test
test scheduler::tests::auto_preallocation_policy_uses_full_only_for_non_cow_hdd ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 129 filtered out; finished in 0.00s

     Running tests/storage_move_import_hardware.rs (target/debug/build/rt-storage/1dd684681865526e/out/storage_move_import_hardware-1dd684681865526e)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s

     Running tests/storage_real_device.rs (target/debug/build/rt-storage/b3e5d8f1759acd32/out/storage_real_device-b3e5d8f1759acd32)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.00s


==> cargo test -p rt-metrics storage_peer_read_readahead_reduces_backend_reads_for_adjacent_blocks -- --nocapture
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.06s
     Running unittests src/lib.rs (target/debug/build/rt-metrics/c8f0dd102296f94f/out/rt_metrics-c8f0dd102296f94f)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.00s

     Running tests/scale.rs (target/debug/build/rt-metrics/2cbaaff9aaf8ca8d/out/scale-2cbaaff9aaf8ca8d)

running 1 test
test storage_peer_read_readahead_reduces_backend_reads_for_adjacent_blocks ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 18 filtered out; finished in 0.00s


==> cargo test -p rt-metrics storage_hash_pool_does_not_block_peer_read_path -- --nocapture
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.06s
     Running unittests src/lib.rs (target/debug/build/rt-metrics/c8f0dd102296f94f/out/rt_metrics-c8f0dd102296f94f)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.00s

     Running tests/scale.rs (target/debug/build/rt-metrics/2cbaaff9aaf8ca8d/out/scale-2cbaaff9aaf8ca8d)

running 1 test
test storage_hash_pool_does_not_block_peer_read_path ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 18 filtered out; finished in 0.02s


==> cargo test -p rt-metrics storage_positioned_io_preserves_offsets_under_concurrency -- --nocapture
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.06s
     Running unittests src/lib.rs (target/debug/build/rt-metrics/c8f0dd102296f94f/out/rt_metrics-c8f0dd102296f94f)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.00s

     Running tests/scale.rs (target/debug/build/rt-metrics/2cbaaff9aaf8ca8d/out/scale-2cbaaff9aaf8ca8d)

running 1 test
test storage_positioned_io_preserves_offsets_under_concurrency ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 18 filtered out; finished in 0.00s


==> cargo test -p rt-metrics storage_file_pool_stays_bounded_under_active_file_churn -- --nocapture
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.06s
     Running unittests src/lib.rs (target/debug/build/rt-metrics/c8f0dd102296f94f/out/rt_metrics-c8f0dd102296f94f)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.00s

     Running tests/scale.rs (target/debug/build/rt-metrics/2cbaaff9aaf8ca8d/out/scale-2cbaaff9aaf8ca8d)

running 1 test
test storage_file_pool_stays_bounded_under_active_file_churn ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 18 filtered out; finished in 0.00s


==> cargo test -p rt-engine upload_block_reads_across_many_file_regions
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.05s
     Running unittests src/lib.rs (target/debug/build/rt-engine/b1388d4ccd56ccf4/out/rt_engine-b1388d4ccd56ccf4)

running 1 test
test torrent_task::tests::upload_block_reads_across_many_file_regions ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 200 filtered out; finished in 0.04s


==> cargo test -p rt-engine pure_v2_recheck_verifies_file_roots_without_torrent_task
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.06s
     Running unittests src/lib.rs (target/debug/build/rt-engine/b1388d4ccd56ccf4/out/rt_engine-b1388d4ccd56ccf4)

running 1 test
test engine::tests::pure_v2_recheck_verifies_file_roots_without_torrent_task ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 200 filtered out; finished in 0.01s

```

## Docker client interop local matrix

```text
[interop] starting interop compose stack
 Image torrentng/torrentngd:interop Building
#1 [internal] load local bake definitions
#1 reading from stdin 574B done
#1 DONE 0.0s

#2 [internal] load build definition from Dockerfile
#2 transferring dockerfile:
#2 transferring dockerfile: 1.26kB done
#2 DONE 0.1s

#3 [internal] load metadata for docker.io/library/node:22-bookworm-slim
#3 ...

#4 [auth] library/node:pull token for registry-1.docker.io
#4 DONE 0.0s

#5 [auth] library/debian:pull token for registry-1.docker.io
#5 DONE 0.0s

#6 [auth] library/rust:pull token for registry-1.docker.io
#6 DONE 0.0s

#7 [internal] load metadata for docker.io/library/rust:1.96-bookworm
#7 ...

#8 [internal] load metadata for docker.io/library/debian:bookworm-slim
#8 DONE 2.1s

#7 [internal] load metadata for docker.io/library/rust:1.96-bookworm
#7 DONE 2.3s

#3 [internal] load metadata for docker.io/library/node:22-bookworm-slim
#3 DONE 2.3s

#9 [internal] load .dockerignore
#9 transferring context: 221B done
#9 DONE 0.1s

#10 [internal] load build context
#10 ...

#11 [stage-2 1/5] FROM docker.io/library/debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
#11 resolve docker.io/library/debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171 0.2s done
#11 DONE 0.3s

#10 [internal] load build context
#10 transferring context: 5.00MB 0.0s done
#10 DONE 0.3s

#12 [build 1/5] FROM docker.io/library/rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663
#12 resolve docker.io/library/rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663 0.2s done
#12 ...

#13 [webui-build 1/6] FROM docker.io/library/node:22-bookworm-slim@sha256:83f487e0a63425e5b4d146fb5e5be574bcbe1b7b843d3ebafdd95eaf7767a7e5
#13 resolve docker.io/library/node:22-bookworm-slim@sha256:83f487e0a63425e5b4d146fb5e5be574bcbe1b7b843d3ebafdd95eaf7767a7e5 0.2s done
#13 DONE 0.7s

#12 [build 1/5] FROM docker.io/library/rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663
#12 sha256:e0085f6fd51ee58df30ebdbdafea5daa893b49a5593c90327224e7bbc597138d 0B / 215.25MB 0.2s
#12 sha256:9eb598f63f1e9867f81313f86224109c84b2af9646d8fca29113217b94315956 0B / 211.60MB 0.2s
#12 ...

#11 [stage-2 1/5] FROM docker.io/library/debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
#11 sha256:a8ac7f6c67abc236e4c745052c404112b8fab6fe8ac3a329d1ef3b867ad67c71 28.23MB / 28.23MB 0.8s done
#11 extracting sha256:a8ac7f6c67abc236e4c745052c404112b8fab6fe8ac3a329d1ef3b867ad67c71 0.3s done
#11 DONE 1.4s

#12 [build 1/5] FROM docker.io/library/rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663
#12 sha256:791c68bc2063683c3d15907b8ed1b777cf14ca153c6f8e5b12db0868dfa7e38a 0B / 64.40MB 0.2s
#12 sha256:791c68bc2063683c3d15907b8ed1b777cf14ca153c6f8e5b12db0868dfa7e38a 4.19MB / 64.40MB 0.3s
#12 sha256:791c68bc2063683c3d15907b8ed1b777cf14ca153c6f8e5b12db0868dfa7e38a 10.49MB / 64.40MB 0.5s
#12 sha256:791c68bc2063683c3d15907b8ed1b777cf14ca153c6f8e5b12db0868dfa7e38a 20.97MB / 64.40MB 0.6s
#12 sha256:f4fd7bf6f6036613e20f62549df75ed694b99118002358bea5a81baf3929d1ff 3.15MB / 24.04MB 0.2s
#12 sha256:791c68bc2063683c3d15907b8ed1b777cf14ca153c6f8e5b12db0868dfa7e38a 25.17MB / 64.40MB 0.8s
#12 sha256:f4fd7bf6f6036613e20f62549df75ed694b99118002358bea5a81baf3929d1ff 7.34MB / 24.04MB 0.3s
#12 ...

#13 [webui-build 1/6] FROM docker.io/library/node:22-bookworm-slim@sha256:83f487e0a63425e5b4d146fb5e5be574bcbe1b7b843d3ebafdd95eaf7767a7e5
#13 sha256:0282d618b52cd2eb8fcf041e780cd66b15e7ebae519a4054c5d293eae6b6d34f 447B / 447B 0.1s done
#13 sha256:3cddab114edadc7cf5a7b0b591e59f6312aade3dc4fb68e88aacca5eb1cc1c03 1.71MB / 1.71MB 0.6s done
#13 sha256:51c53f95c2cae3d00dfc0350297c92dea913fe129ec38b8deb8313e5f3e0297d 49.94MB / 49.94MB 1.0s done
#13 sha256:cea7eb9bbe80eabf8356efb28b3796ab904f36a32049aeb6b435920ca93f9285 3.31kB / 3.31kB 0.2s done
#13 extracting sha256:cea7eb9bbe80eabf8356efb28b3796ab904f36a32049aeb6b435920ca93f9285 0.1s done
#13 extracting sha256:51c53f95c2cae3d00dfc0350297c92dea913fe129ec38b8deb8313e5f3e0297d 0.5s done
#13 extracting sha256:3cddab114edadc7cf5a7b0b591e59f6312aade3dc4fb68e88aacca5eb1cc1c03 0.0s done
#13 DONE 2.3s

#12 [build 1/5] FROM docker.io/library/rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663
#12 ...

#13 [webui-build 1/6] FROM docker.io/library/node:22-bookworm-slim@sha256:83f487e0a63425e5b4d146fb5e5be574bcbe1b7b843d3ebafdd95eaf7767a7e5
#13 extracting sha256:0282d618b52cd2eb8fcf041e780cd66b15e7ebae519a4054c5d293eae6b6d34f 0.0s done
#13 DONE 2.4s

#12 [build 1/5] FROM docker.io/library/rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663
#12 sha256:791c68bc2063683c3d15907b8ed1b777cf14ca153c6f8e5b12db0868dfa7e38a 46.14MB / 64.40MB 1.1s
#12 sha256:f4fd7bf6f6036613e20f62549df75ed694b99118002358bea5a81baf3929d1ff 24.04MB / 24.04MB 0.6s done
#12 sha256:791c68bc2063683c3d15907b8ed1b777cf14ca153c6f8e5b12db0868dfa7e38a 58.72MB / 64.40MB 1.2s
#12 sha256:425befdf76e52426879d2abe42093a00dca59a893e7b4fa2a7679b0180b71d4b 1.05MB / 48.50MB 0.2s
#12 sha256:e0085f6fd51ee58df30ebdbdafea5daa893b49a5593c90327224e7bbc597138d 11.53MB / 215.25MB 1.8s
#12 sha256:791c68bc2063683c3d15907b8ed1b777cf14ca153c6f8e5b12db0868dfa7e38a 64.40MB / 64.40MB 1.3s done
#12 sha256:425befdf76e52426879d2abe42093a00dca59a893e7b4fa2a7679b0180b71d4b 10.49MB / 48.50MB 0.3s
#12 ...

#14 [webui-build 2/6] WORKDIR /webui
#14 DONE 0.5s

#12 [build 1/5] FROM docker.io/library/rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663
#12 sha256:425befdf76e52426879d2abe42093a00dca59a893e7b4fa2a7679b0180b71d4b 22.02MB / 48.50MB 0.5s
#12 ...

#15 [webui-build 3/6] COPY webui/package.json webui/package-lock.json ./
#15 DONE 0.1s

#12 [build 1/5] FROM docker.io/library/rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663
#12 sha256:425befdf76e52426879d2abe42093a00dca59a893e7b4fa2a7679b0180b71d4b 34.60MB / 48.50MB 0.6s
#12 sha256:425befdf76e52426879d2abe42093a00dca59a893e7b4fa2a7679b0180b71d4b 48.50MB / 48.50MB 0.8s done
#12 extracting sha256:425befdf76e52426879d2abe42093a00dca59a893e7b4fa2a7679b0180b71d4b
#12 sha256:e0085f6fd51ee58df30ebdbdafea5daa893b49a5593c90327224e7bbc597138d 23.07MB / 215.25MB 2.6s
#12 extracting sha256:425befdf76e52426879d2abe42093a00dca59a893e7b4fa2a7679b0180b71d4b 0.5s done
#12 extracting sha256:f4fd7bf6f6036613e20f62549df75ed694b99118002358bea5a81baf3929d1ff
#12 extracting sha256:f4fd7bf6f6036613e20f62549df75ed694b99118002358bea5a81baf3929d1ff 0.2s done
#12 sha256:e0085f6fd51ee58df30ebdbdafea5daa893b49a5593c90327224e7bbc597138d 40.89MB / 215.25MB 3.2s
#12 extracting sha256:791c68bc2063683c3d15907b8ed1b777cf14ca153c6f8e5b12db0868dfa7e38a
#12 sha256:e0085f6fd51ee58df30ebdbdafea5daa893b49a5593c90327224e7bbc597138d 52.43MB / 215.25MB 3.5s
#12 sha256:e0085f6fd51ee58df30ebdbdafea5daa893b49a5593c90327224e7bbc597138d 66.06MB / 215.25MB 3.8s
#12 extracting sha256:791c68bc2063683c3d15907b8ed1b777cf14ca153c6f8e5b12db0868dfa7e38a 0.8s done
#12 sha256:e0085f6fd51ee58df30ebdbdafea5daa893b49a5593c90327224e7bbc597138d 88.08MB / 215.25MB 4.1s
#12 sha256:9eb598f63f1e9867f81313f86224109c84b2af9646d8fca29113217b94315956 11.53MB / 211.60MB 3.9s
#12 ...

#16 [webui-build 4/6] RUN npm ci
#16 1.674
#16 1.674 added 163 packages, and audited 164 packages in 2s
#16 1.674
#16 1.674 47 packages are looking for funding
#16 1.674   run `npm fund` for details
#16 1.674
#16 1.674 found 0 vulnerabilities
#16 1.675 npm notice
#16 1.675 npm notice New major version of npm available! 10.9.8 -> 12.0.2
#16 1.675 npm notice Changelog: https://github.com/npm/cli/releases/tag/v12.0.2
#16 1.675 npm notice To update run: npm install -g npm@12.0.2
#16 1.675 npm notice
#16 DONE 2.1s

#12 [build 1/5] FROM docker.io/library/rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663
#12 sha256:e0085f6fd51ee58df30ebdbdafea5daa893b49a5593c90327224e7bbc597138d 99.61MB / 215.25MB 4.2s
#12 sha256:e0085f6fd51ee58df30ebdbdafea5daa893b49a5593c90327224e7bbc597138d 113.25MB / 215.25MB 4.4s
#12 ...

#17 [webui-build 5/6] COPY webui/ ./
#17 DONE 0.3s

#12 [build 1/5] FROM docker.io/library/rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663
#12 sha256:e0085f6fd51ee58df30ebdbdafea5daa893b49a5593c90327224e7bbc597138d 124.78MB / 215.25MB 4.5s
#12 sha256:e0085f6fd51ee58df30ebdbdafea5daa893b49a5593c90327224e7bbc597138d 136.31MB / 215.25MB 4.7s
#12 sha256:e0085f6fd51ee58df30ebdbdafea5daa893b49a5593c90327224e7bbc597138d 160.43MB / 215.25MB 5.0s
#12 sha256:e0085f6fd51ee58df30ebdbdafea5daa893b49a5593c90327224e7bbc597138d 183.50MB / 215.25MB 5.3s
#12 sha256:9eb598f63f1e9867f81313f86224109c84b2af9646d8fca29113217b94315956 23.07MB / 211.60MB 5.1s
#12 sha256:e0085f6fd51ee58df30ebdbdafea5daa893b49a5593c90327224e7bbc597138d 195.04MB / 215.25MB 5.4s
#12 sha256:e0085f6fd51ee58df30ebdbdafea5daa893b49a5593c90327224e7bbc597138d 206.57MB / 215.25MB 5.6s
#12 sha256:e0085f6fd51ee58df30ebdbdafea5daa893b49a5593c90327224e7bbc597138d 215.25MB / 215.25MB 5.8s done
#12 sha256:9eb598f63f1e9867f81313f86224109c84b2af9646d8fca29113217b94315956 34.60MB / 211.60MB 6.0s
#12 sha256:9eb598f63f1e9867f81313f86224109c84b2af9646d8fca29113217b94315956 47.19MB / 211.60MB 6.6s
#12 sha256:9eb598f63f1e9867f81313f86224109c84b2af9646d8fca29113217b94315956 61.87MB / 211.60MB 7.1s
#12 sha256:9eb598f63f1e9867f81313f86224109c84b2af9646d8fca29113217b94315956 76.55MB / 211.60MB 7.4s
#12 sha256:9eb598f63f1e9867f81313f86224109c84b2af9646d8fca29113217b94315956 95.42MB / 211.60MB 7.7s
#12 ...

#18 [webui-build 6/6] RUN TNG_WEBUI_OUT_DIR=dist npm run build
#18 0.267
#18 0.267 > torrentng-webui@0.1.0 build
#18 0.267 > tsc && vite build
#18 0.267
#18 2.485 vite v8.2.2 building client environment for production...
#18 2.506 transforming...
#18 3.142 ✓ 94 modules transformed.
#18 3.294 rendering chunks...
#18 3.369 computing gzip size...
#18 3.374 dist/index.html                             0.62 kB │ gzip:   0.41 kB
#18 3.374 dist/assets/index-CtGr9d3k.css             24.28 kB │ gzip:   4.68 kB
#18 3.374 dist/assets/UserAgentPanel-C2XiwE6r.js      5.51 kB │ gzip:   1.91 kB
#18 3.374 dist/assets/LogsPanel-CMJqDDZn.js           6.40 kB │ gzip:   2.01 kB
#18 3.374 dist/assets/RatioGroupsPanel--iTLp3di.js    7.54 kB │ gzip:   2.31 kB
#18 3.374 dist/assets/WorkflowsPanel-CAmQQ1tE.js     10.11 kB │ gzip:   2.86 kB
#18 3.374 dist/assets/RssRulesPanel-C265o7F7.js      10.25 kB │ gzip:   2.86 kB
#18 3.374 dist/assets/StoragePanel-CpDNOEkV.js       16.51 kB │ gzip:   4.26 kB
#18 3.374 dist/assets/EnginePanel-mDXN15pC.js        44.67 kB │ gzip:  10.87 kB
#18 3.374 dist/assets/index-DoaHm8ZH.js             437.55 kB │ gzip: 121.70 kB
#18 3.374
#18 3.375 ✓ built in 889ms
#18 DONE 3.5s

#12 [build 1/5] FROM docker.io/library/rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663
#12 sha256:9eb598f63f1e9867f81313f86224109c84b2af9646d8fca29113217b94315956 125.83MB / 211.60MB 8.1s
#12 sha256:9eb598f63f1e9867f81313f86224109c84b2af9646d8fca29113217b94315956 137.36MB / 211.60MB 8.3s
#12 ...

#19 [stage-2 2/5] RUN apt-get update     && apt-get install -y --no-install-recommends ca-certificates curl tini     && rm -rf /var/lib/apt/lists/*     && useradd --system --home /var/lib/torrentngd --create-home --shell /usr/sbin/nologin torrentngd     && mkdir -p /data /config /var/lib/torrentngd /usr/share/torrentng/webui     && chown -R torrentngd:torrentngd /data /config /var/lib/torrentngd
#19 0.418 Get:1 http://deb.debian.org/debian bookworm InRelease [151 kB]
#19 0.636 Get:2 http://deb.debian.org/debian bookworm-updates InRelease [55.4 kB]
#19 0.929 Get:3 http://deb.debian.org/debian-security bookworm-security InRelease [34.8 kB]
#19 1.013 Get:4 http://deb.debian.org/debian bookworm/main amd64 Packages [8790 kB]
#19 3.819 Get:5 http://deb.debian.org/debian bookworm-updates/main amd64 Packages [6924 B]
#19 3.878 Get:6 http://deb.debian.org/debian-security bookworm-security/main amd64 Packages [338 kB]
#19 4.356 Fetched 9376 kB in 4s (2247 kB/s)
#19 4.356 Reading package lists...
#19 4.654 Reading package lists...
#19 4.941 Building dependency tree...
#19 5.055 Reading state information...
#19 5.148 The following additional packages will be installed:
#19 5.149   libbrotli1 libcurl4 libgssapi-krb5-2 libk5crypto3 libkeyutils1 libkrb5-3
#19 5.149   libkrb5support0 libldap-2.5-0 libnghttp2-14 libpsl5 librtmp1 libsasl2-2
#19 5.150   libsasl2-modules-db libssh2-1 libssl3 openssl
#19 5.150 Suggested packages:
#19 5.150   krb5-doc krb5-user
#19 5.150 Recommended packages:
#19 5.150   krb5-locales libldap-common publicsuffix libsasl2-modules
#19 5.226 The following NEW packages will be installed:
#19 5.226   ca-certificates curl libbrotli1 libcurl4 libgssapi-krb5-2 libk5crypto3
#19 5.226   libkeyutils1 libkrb5-3 libkrb5support0 libldap-2.5-0 libnghttp2-14 libpsl5
#19 5.227   librtmp1 libsasl2-2 libsasl2-modules-db libssh2-1 libssl3 openssl tini
#19 5.338 0 upgraded, 19 newly installed, 0 to remove and 1 not upgraded.
#19 5.338 Need to get 6107 kB of archives.
#19 5.338 After this operation, 15.5 MB of additional disk space will be used.
#19 5.338 Get:1 http://deb.debian.org/debian bookworm/main amd64 libssl3 amd64 3.0.20-1~deb12u2 [2036 kB]
#19 5.606 Get:2 http://deb.debian.org/debian bookworm/main amd64 openssl amd64 3.0.20-1~deb12u2 [1439 kB]
#19 5.628 Get:3 http://deb.debian.org/debian-security bookworm-security/main amd64 ca-certificates all 20250419~deb12u1 [162 kB]
#19 5.631 Get:4 http://deb.debian.org/debian bookworm/main amd64 libbrotli1 amd64 1.0.9-2+b6 [275 kB]
#19 5.635 Get:5 http://deb.debian.org/debian bookworm/main amd64 libkrb5support0 amd64 1.20.1-2+deb12u5 [33.2 kB]
#19 5.636 Get:6 http://deb.debian.org/debian bookworm/main amd64 libk5crypto3 amd64 1.20.1-2+deb12u5 [79.7 kB]
#19 5.637 Get:7 http://deb.debian.org/debian bookworm/main amd64 libkeyutils1 amd64 1.6.3-2 [8808 B]
#19 5.637 Get:8 http://deb.debian.org/debian bookworm/main amd64 libkrb5-3 amd64 1.20.1-2+deb12u5 [332 kB]
#19 5.656 Get:9 http://deb.debian.org/debian bookworm/main amd64 libgssapi-krb5-2 amd64 1.20.1-2+deb12u5 [135 kB]
#19 5.657 Get:10 http://deb.debian.org/debian bookworm/main amd64 libsasl2-modules-db amd64 2.1.28+dfsg-10 [20.3 kB]
#19 5.658 Get:11 http://deb.debian.org/debian bookworm/main amd64 libsasl2-2 amd64 2.1.28+dfsg-10 [59.7 kB]
#19 5.691 Get:12 http://deb.debian.org/debian bookworm/main amd64 libldap-2.5-0 amd64 2.5.13+dfsg-5 [183 kB]
#19 5.695 Get:13 http://deb.debian.org/debian bookworm/main amd64 libnghttp2-14 amd64 1.52.0-1+deb12u3 [72.4 kB]
#19 5.696 Get:14 http://deb.debian.org/debian bookworm/main amd64 libpsl5 amd64 0.21.2-1 [58.7 kB]
#19 5.697 Get:15 http://deb.debian.org/debian bookworm/main amd64 librtmp1 amd64 2.4+20151223.gitfa8646d.1-2+b2 [60.8 kB]
#19 5.698 Get:16 http://deb.debian.org/debian-security bookworm-security/main amd64 libssh2-1 amd64 1.10.0-3+deb12u1 [176 kB]
#19 5.700 Get:17 http://deb.debian.org/debian bookworm/main amd64 libcurl4 amd64 7.88.1-10+deb12u15 [392 kB]
#19 5.732 Get:18 http://deb.debian.org/debian bookworm/main amd64 curl amd64 7.88.1-10+deb12u15 [316 kB]
#19 5.737 Get:19 http://deb.debian.org/debian bookworm/main amd64 tini amd64 0.19.0-1+b3 [267 kB]
#19 5.800 debconf: delaying package configuration, since apt-utils is not installed
#19 5.817 Fetched 6107 kB in 1s (12.0 MB/s)
#19 5.841 Selecting previously unselected package libssl3:amd64.
#19 5.841 (Reading database ... (Reading database ... 5%(Reading database ... 10%(Reading database ... 15%(Reading database ... 20%(Reading database ... 25%(Reading database ... 30%(Reading database ... 35%(Reading database ... 40%(Reading database ... 45%(Reading database ... 50%(Reading database ... 55%(Reading database ... 60%(Reading database ... 65%(Reading database ... 70%(Reading database ... 75%(Reading database ... 80%(Reading database ... 85%(Reading database ... 90%(Reading database ... 95%(Reading database ... 100%(Reading database ... 6096 files and directories currently installed.)
#19 5.844 Preparing to unpack .../00-libssl3_3.0.20-1~deb12u2_amd64.deb ...
#19 5.861 Unpacking libssl3:amd64 (3.0.20-1~deb12u2) ...
#19 5.971 Selecting previously unselected package openssl.
#19 5.972 Preparing to unpack .../01-openssl_3.0.20-1~deb12u2_amd64.deb ...
#19 5.977 Unpacking openssl (3.0.20-1~deb12u2) ...
#19 6.074 Selecting previously unselected package ca-certificates.
#19 6.075 Preparing to unpack .../02-ca-certificates_20250419~deb12u1_all.deb ...
#19 6.080 Unpacking ca-certificates (20250419~deb12u1) ...
#19 6.137 Selecting previously unselected package libbrotli1:amd64.
#19 6.139 Preparing to unpack .../03-libbrotli1_1.0.9-2+b6_amd64.deb ...
#19 6.143 Unpacking libbrotli1:amd64 (1.0.9-2+b6) ...
#19 6.192 Selecting previously unselected package libkrb5support0:amd64.
#19 6.193 Preparing to unpack .../04-libkrb5support0_1.20.1-2+deb12u5_amd64.deb ...
#19 6.198 Unpacking libkrb5support0:amd64 (1.20.1-2+deb12u5) ...
#19 6.235 Selecting previously unselected package libk5crypto3:amd64.
#19 6.236 Preparing to unpack .../05-libk5crypto3_1.20.1-2+deb12u5_amd64.deb ...
#19 6.240 Unpacking libk5crypto3:amd64 (1.20.1-2+deb12u5) ...
#19 6.279 Selecting previously unselected package libkeyutils1:amd64.
#19 6.280 Preparing to unpack .../06-libkeyutils1_1.6.3-2_amd64.deb ...
#19 6.285 Unpacking libkeyutils1:amd64 (1.6.3-2) ...
#19 6.321 Selecting previously unselected package libkrb5-3:amd64.
#19 6.322 Preparing to unpack .../07-libkrb5-3_1.20.1-2+deb12u5_amd64.deb ...
#19 6.327 Unpacking libkrb5-3:amd64 (1.20.1-2+deb12u5) ...
#19 6.384 Selecting previously unselected package libgssapi-krb5-2:amd64.
#19 6.385 Preparing to unpack .../08-libgssapi-krb5-2_1.20.1-2+deb12u5_amd64.deb ...
#19 6.390 Unpacking libgssapi-krb5-2:amd64 (1.20.1-2+deb12u5) ...
#19 6.424 Selecting previously unselected package libsasl2-modules-db:amd64.
#19 6.425 Preparing to unpack .../09-libsasl2-modules-db_2.1.28+dfsg-10_amd64.deb ...
#19 6.430 Unpacking libsasl2-modules-db:amd64 (2.1.28+dfsg-10) ...
#19 6.464 Selecting previously unselected package libsasl2-2:amd64.
#19 6.466 Preparing to unpack .../10-libsasl2-2_2.1.28+dfsg-10_amd64.deb ...
#19 6.471 Unpacking libsasl2-2:amd64 (2.1.28+dfsg-10) ...
#19 6.509 Selecting previously unselected package libldap-2.5-0:amd64.
#19 6.511 Preparing to unpack .../11-libldap-2.5-0_2.5.13+dfsg-5_amd64.deb ...
#19 6.533 Unpacking libldap-2.5-0:amd64 (2.5.13+dfsg-5) ...
#19 6.577 Selecting previously unselected package libnghttp2-14:amd64.
#19 6.584 Preparing to unpack .../12-libnghttp2-14_1.52.0-1+deb12u3_amd64.deb ...
#19 6.599 Unpacking libnghttp2-14:amd64 (1.52.0-1+deb12u3) ...
#19 6.687 Selecting previously unselected package libpsl5:amd64.
#19 6.691 Preparing to unpack .../13-libpsl5_0.21.2-1_amd64.deb ...
#19 6.698 Unpacking libpsl5:amd64 (0.21.2-1) ...
#19 6.748 Selecting previously unselected package librtmp1:amd64.
#19 6.751 Preparing to unpack .../14-librtmp1_2.4+20151223.gitfa8646d.1-2+b2_amd64.deb ...
#19 6.757 Unpacking librtmp1:amd64 (2.4+20151223.gitfa8646d.1-2+b2) ...
#19 6.802 Selecting previously unselected package libssh2-1:amd64.
#19 6.805 Preparing to unpack .../15-libssh2-1_1.10.0-3+deb12u1_amd64.deb ...
#19 6.812 Unpacking libssh2-1:amd64 (1.10.0-3+deb12u1) ...
#19 6.861 Selecting previously unselected package libcurl4:amd64.
#19 6.863 Preparing to unpack .../16-libcurl4_7.88.1-10+deb12u15_amd64.deb ...
#19 6.868 Unpacking libcurl4:amd64 (7.88.1-10+deb12u15) ...
#19 6.916 Selecting previously unselected package curl.
#19 6.918 Preparing to unpack .../17-curl_7.88.1-10+deb12u15_amd64.deb ...
#19 6.924 Unpacking curl (7.88.1-10+deb12u15) ...
#19 6.990 Selecting previously unselected package tini.
#19 6.993 Preparing to unpack .../18-tini_0.19.0-1+b3_amd64.deb ...
#19 6.999 Unpacking tini (0.19.0-1+b3) ...
#19 7.074 Setting up libkeyutils1:amd64 (1.6.3-2) ...
#19 7.095 Setting up libpsl5:amd64 (0.21.2-1) ...
#19 7.110 Setting up libbrotli1:amd64 (1.0.9-2+b6) ...
#19 7.125 Setting up libssl3:amd64 (3.0.20-1~deb12u2) ...
#19 7.141 Setting up libnghttp2-14:amd64 (1.52.0-1+deb12u3) ...
#19 7.157 Setting up libkrb5support0:amd64 (1.20.1-2+deb12u5) ...
#19 7.174 Setting up libsasl2-modules-db:amd64 (2.1.28+dfsg-10) ...
#19 7.191 Setting up librtmp1:amd64 (2.4+20151223.gitfa8646d.1-2+b2) ...
#19 7.213 Setting up tini (0.19.0-1+b3) ...
#19 7.229 Setting up libk5crypto3:amd64 (1.20.1-2+deb12u5) ...
#19 7.248 Setting up libsasl2-2:amd64 (2.1.28+dfsg-10) ...
#19 7.265 Setting up libssh2-1:amd64 (1.10.0-3+deb12u1) ...
#19 7.283 Setting up libkrb5-3:amd64 (1.20.1-2+deb12u5) ...
#19 7.301 Setting up openssl (3.0.20-1~deb12u2) ...
#19 7.324 Setting up libldap-2.5-0:amd64 (2.5.13+dfsg-5) ...
#19 7.340 Setting up ca-certificates (20250419~deb12u1) ...
#19 7.388 debconf: unable to initialize frontend: Dialog
#19 7.388 debconf: (TERM is not set, so the dialog frontend is not usable.)
#19 7.388 debconf: falling back to frontend: Readline
#19 7.388 debconf: unable to initialize frontend: Readline
#19 7.388 debconf: (Can't locate Term/ReadLine.pm in @INC (you may need to install the Term::ReadLine module) (@INC contains: /etc/perl /usr/local/lib/x86_64-linux-gnu/perl/5.36.0 /usr/local/share/perl/5.36.0 /usr/lib/x86_64-linux-gnu/perl5/5.36 /usr/share/perl5 /usr/lib/x86_64-linux-gnu/perl-base /usr/lib/x86_64-linux-gnu/perl/5.36 /usr/share/perl/5.36 /usr/local/lib/site_perl) at /usr/share/perl5/Debconf/FrontEnd/Readline.pm line 7.)
#19 7.388 debconf: falling back to frontend: Teletype
#19 7.567 Updating certificates in /etc/ssl/certs...
#19 7.816 150 added, 0 removed; done.
#19 7.848 Setting up libgssapi-krb5-2:amd64 (1.20.1-2+deb12u5) ...
#19 7.863 Setting up libcurl4:amd64 (7.88.1-10+deb12u15) ...
#19 7.877 Setting up curl (7.88.1-10+deb12u15) ...
#19 7.892 Processing triggers for libc-bin (2.36-9+deb12u14) ...
#19 7.911 Processing triggers for ca-certificates (20250419~deb12u1) ...
#19 7.917 Updating certificates in /etc/ssl/certs...
#19 8.100 0 added, 0 removed; done.
#19 8.100 Running hooks in /etc/ca-certificates/update.d...
#19 8.101 done.
#19 DONE 8.2s

#12 [build 1/5] FROM docker.io/library/rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663
#12 sha256:9eb598f63f1e9867f81313f86224109c84b2af9646d8fca29113217b94315956 159.38MB / 211.60MB 8.6s
#12 sha256:9eb598f63f1e9867f81313f86224109c84b2af9646d8fca29113217b94315956 184.55MB / 211.60MB 8.9s
#12 sha256:9eb598f63f1e9867f81313f86224109c84b2af9646d8fca29113217b94315956 198.18MB / 211.60MB 9.0s
#12 sha256:9eb598f63f1e9867f81313f86224109c84b2af9646d8fca29113217b94315956 211.60MB / 211.60MB 9.2s
#12 sha256:9eb598f63f1e9867f81313f86224109c84b2af9646d8fca29113217b94315956 211.60MB / 211.60MB 9.3s done
#12 extracting sha256:9eb598f63f1e9867f81313f86224109c84b2af9646d8fca29113217b94315956
#12 extracting sha256:9eb598f63f1e9867f81313f86224109c84b2af9646d8fca29113217b94315956 1.6s done
#12 DONE 12.0s

#12 [build 1/5] FROM docker.io/library/rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663
#12 extracting sha256:e0085f6fd51ee58df30ebdbdafea5daa893b49a5593c90327224e7bbc597138d
#12 extracting sha256:e0085f6fd51ee58df30ebdbdafea5daa893b49a5593c90327224e7bbc597138d 1.4s done
#12 DONE 13.3s

#20 [build 2/5] WORKDIR /src
#20 DONE 0.6s

#21 [build 3/5] COPY Cargo.toml Cargo.lock ./
#21 DONE 0.2s

#22 [build 4/5] COPY crates ./crates
#22 DONE 0.1s

#23 [build 5/5] RUN cargo build --release --locked -p torrentngd
#23 0.139     Updating crates.io index
#23 3.336  Downloading crates ...
#23 3.592   Downloaded anyhow v1.0.104
#23 3.611   Downloaded ahash v0.8.12
#23 3.641   Downloaded atomic-waker v1.1.2
#23 3.659   Downloaded rand_pcg v0.10.2
#23 3.685   Downloaded futures-core v0.3.32
#23 3.687   Downloaded form_urlencoded v1.2.2
#23 3.701   Downloaded async-trait v0.1.89
#23 3.743   Downloaded mime v0.3.17
#23 3.754   Downloaded lazy_static v1.5.0
#23 3.781   Downloaded untrusted v0.9.0
#23 3.785   Downloaded zerofrom v0.1.8
#23 3.787   Downloaded stable_deref_trait v1.2.1
#23 3.791   Downloaded scopeguard v1.2.0
#23 3.794   Downloaded sha1 v0.10.6
#23 3.799   Downloaded version_check v0.9.5
#23 3.814   Downloaded yoke-derive v0.8.2
#23 3.818   Downloaded zerofrom-derive v0.1.7
#23 3.820   Downloaded rustc-hash v2.1.2
#23 3.821   Downloaded utf-8 v0.7.6
#23 3.823   Downloaded futures-task v0.3.32
#23 3.825   Downloaded tracing-serde v0.2.0
#23 3.826   Downloaded tower-service v0.3.3
#23 3.832   Downloaded utf8_iter v1.0.4
#23 3.835   Downloaded toml_write v0.1.2
#23 3.837   Downloaded tokio-macros v2.7.0
#23 3.839   Downloaded try-lock v0.2.5
#23 3.841   Downloaded tower-layer v0.3.3
#23 3.843   Downloaded slab v0.4.12
#23 3.845   Downloaded rand_core v0.6.4
#23 3.846   Downloaded serde_urlencoded v0.7.1
#23 3.849   Downloaded futures-io v0.3.32
#23 3.850   Downloaded crypto-common v0.1.7
#23 3.857   Downloaded futures-macro v0.3.32
#23 3.867   Downloaded serde_spanned v0.6.9
#23 3.873   Downloaded http-body v1.0.1
#23 3.875   Downloaded want v0.3.1
#23 3.881   Downloaded toml_datetime v0.6.11
#23 3.883   Downloaded tinyvec_macros v0.1.1
#23 3.885   Downloaded subtle v2.6.1
#23 3.890   Downloaded sha2 v0.10.9
#23 3.892   Downloaded tracing-log v0.2.0
#23 3.894   Downloaded signal-hook-registry v1.4.8
#23 3.895   Downloaded zeroize v1.8.2
#23 3.896   Downloaded tokio-tungstenite v0.24.0
#23 3.903   Downloaded synstructure v0.13.2
#23 3.905   Downloaded matchers v0.2.0
#23 3.908   Downloaded hex v0.4.3
#23 3.911   Downloaded writeable v0.6.3
#23 3.914   Downloaded thread_local v1.1.9
#23 3.921   Downloaded thiserror-impl v2.0.18
#23 3.924   Downloaded serde_path_to_error v0.1.20
#23 3.927   Downloaded tinystr v0.8.3
#23 3.930   Downloaded cpufeatures v0.2.17
#23 3.933   Downloaded thiserror v2.0.18
#23 3.947   Downloaded toml v0.8.23
#23 3.950   Downloaded zmij v1.0.21
#23 3.954   Downloaded thiserror-impl v1.0.69
#23 3.964   Downloaded tokio-rustls v0.26.4
#23 3.969   Downloaded shlex v1.3.0
#23 3.973   Downloaded smallvec v1.15.1
#23 3.975   Downloaded rustls-pki-types v1.14.1
#23 3.981   Downloaded zerovec-derive v0.11.3
#23 3.984   Downloaded tracing-core v0.1.36
#23 3.988   Downloaded tracing-attributes v0.1.31
#23 3.992   Downloaded serde_core v1.0.228
#23 3.998   Downloaded unicode-ident v1.0.24
#23 4.002   Downloaded serde_derive v1.0.228
#23 4.020   Downloaded yoke v0.8.2
#23 4.028   Downloaded ryu v1.0.23
#23 4.045   Downloaded tungstenite v0.24.0
#23 4.064   Downloaded toml_edit v0.22.27
#23 4.083   Downloaded zerotrie v0.2.4
#23 4.088   Downloaded tower v0.5.3
#23 4.098   Downloaded rustls-webpki v0.103.13
#23 4.100   Downloaded uuid v1.23.1
#23 4.104   Downloaded url v2.5.8
#23 4.106   Downloaded idna_adapter v1.2.2
#23 4.107   Downloaded cfg-if v1.0.4
#23 4.108   Downloaded typenum v1.20.0
#23 4.110   Downloaded serde v1.0.228
#23 4.113   Downloaded equivalent v1.0.2
#23 4.114   Downloaded potential_utf v0.1.5
#23 4.115   Downloaded itoa v1.0.18
#23 4.116   Downloaded tinyvec v1.11.0
#23 4.118   Downloaded socket2 v0.6.3
#23 4.120   Downloaded sharded-slab v0.1.7
#23 4.123   Downloaded cpufeatures v0.3.1
#23 4.125   Downloaded cfg_aliases v0.2.1
#23 4.126   Downloaded futures-sink v0.3.32
#23 4.126   Downloaded block-buffer v0.10.4
#23 4.127   Downloaded http-range-header v0.4.2
#23 4.128   Downloaded spin v0.9.8
#23 4.131   Downloaded fallible-streaming-iterator v0.1.9
#23 4.132   Downloaded rand_chacha v0.3.1
#23 4.134   Downloaded errno v0.3.14
#23 4.135   Downloaded rustversion v1.0.22
#23 4.137   Downloaded unicase v2.9.0
#23 4.138   Downloaded thiserror v1.0.69
#23 4.141   Downloaded sync_wrapper v1.0.2
#23 4.142   Downloaded tower-http v0.5.2
#23 4.147   Downloaded zerovec v0.11.6
#23 4.152   Downloaded tokio-util v0.7.18
#23 4.160   Downloaded percent-encoding v2.3.2
#23 4.161   Downloaded http-body-util v0.1.3
#23 4.163   Downloaded fallible-iterator v0.3.0
#23 4.164   Downloaded digest v0.10.7
#23 4.166   Downloaded reqwest v0.12.28
#23 4.173   Downloaded serde_json v1.0.149
#23 4.186   Downloaded tower-http v0.6.10
#23 4.284   Downloaded lru-slab v0.1.2
#23 4.293   Downloaded winnow v0.7.15
#23 4.360   Downloaded webpki-roots v1.0.7
#23 4.369   Downloaded vcpkg v0.2.15
#23 6.440   Downloaded quote v1.0.45
#23 6.584   Downloaded quinn-udp v0.5.14
#23 6.666   Downloaded zerocopy v0.8.48
#23 8.081   Downloaded syn v2.0.117
#23 8.881   Downloaded rand v0.8.6
#23 9.066   Downloaded rand v0.10.2
#23 9.273   Downloaded mio v1.2.0
#23 9.676   Downloaded rustls v0.23.40
#23 10.29   Downloaded idna v1.1.0
#23 10.41   Downloaded hyper v1.9.0
#23 10.78   Downloaded icu_properties_data v2.2.0
#23 11.28   Downloaded futures-util v0.3.32
#23 11.72   Downloaded tracing v0.1.44
#23 11.99   Downloaded quinn-proto v0.11.16
#23 12.22   Downloaded axum v0.7.9
#23 12.38   Downloaded aho-corasick v1.1.4
#23 12.67   Downloaded hashbrown v0.17.1
#23 12.97   Downloaded hashbrown v0.14.5
#23 13.09   Downloaded http v1.4.0
#23 13.18   Downloaded io-uring v0.7.12
#23 13.23   Downloaded regex-automata v0.4.14
#23 13.55   Downloaded indexmap v2.14.0
#23 13.60   Downloaded hyper-util v0.1.20
#23 13.66   Downloaded futures v0.3.32
#23 13.71   Downloaded cc v1.2.62
#23 13.74   Downloaded base64 v0.23.1
#23 13.77   Downloaded quinn v0.11.9
#23 13.79   Downloaded memchr v2.8.0
#23 13.84   Downloaded icu_normalizer v2.2.0
#23 13.88   Downloaded icu_locale_core v2.2.0
#23 13.96   Downloaded icu_collections v2.2.0
#23 14.10   Downloaded getrandom v0.4.2
#23 14.16   Downloaded bytes v1.11.1
#23 14.21   Downloaded base64 v0.22.1
#23 14.25   Downloaded icu_properties v2.2.0
#23 14.27   Downloaded tokio v1.52.3
#23 14.81   Downloaded icu_normalizer_data v2.2.0
#23 14.83   Downloaded bitflags v2.11.1
#23 14.88   Downloaded rand_core v0.10.1
#23 14.92   Downloaded proc-macro2 v1.0.106
#23 14.96   Downloaded parking_lot_core v0.9.12
#23 14.97   Downloaded log v0.4.29
#23 15.00   Downloaded icu_provider v2.2.0
#23 15.03   Downloaded httparse v1.10.1
#23 15.06   Downloaded getrandom v0.2.17
#23 15.07   Downloaded libc v0.2.186
#23 15.25   Downloaded matchit v0.7.3
#23 15.26   Downloaded litemap v0.8.2
#23 15.27   Downloaded futures-channel v0.3.32
#23 15.28   Downloaded data-encoding v2.11.0
#23 15.29   Downloaded chacha20 v0.10.2
#23 15.32   Downloaded futures-executor v0.3.32
#23 15.33   Downloaded axum-macros v0.4.2
#23 15.42   Downloaded axum-core v0.4.5
#23 15.43   Downloaded ppv-lite86 v0.2.21
#23 15.44   Downloaded pkg-config v0.3.33
#23 15.44   Downloaded pin-project-lite v0.2.17
#23 15.50   Downloaded parking_lot v0.12.5
#23 15.51   Downloaded once_cell v1.21.4
#23 15.53   Downloaded nu-ansi-term v0.50.3
#23 15.54   Downloaded mime_guess v2.0.5
#23 15.55   Downloaded hyper-rustls v0.27.9
#23 15.55   Downloaded httpdate v1.0.3
#23 15.58   Downloaded hashlink v0.9.1
#23 15.63   Downloaded regex-syntax v0.8.10
#23 15.68   Downloaded tracing-subscriber v0.3.23
#23 15.71   Downloaded multer v3.1.0
#23 15.73   Downloaded lock_api v0.4.14
#23 15.73   Downloaded generic-array v0.14.7
#23 15.74   Downloaded find-msvc-tools v0.1.9
#23 15.75   Downloaded ring v0.17.14
#23 16.28   Downloaded displaydoc v0.2.5
#23 16.33   Downloaded byteorder v1.5.0
#23 16.35   Downloaded ipnet v2.12.0
#23 16.38   Downloaded encoding_rs v0.8.35
#23 16.68   Downloaded rusqlite v0.31.0
#23 17.32   Downloaded libsqlite3-sys v0.28.0
#23 18.19    Compiling proc-macro2 v1.0.106
#23 18.19    Compiling quote v1.0.45
#23 18.19    Compiling unicode-ident v1.0.24
#23 18.19    Compiling cfg-if v1.0.4
#23 18.19    Compiling libc v0.2.186
#23 18.19    Compiling smallvec v1.15.1
#23 18.19    Compiling pin-project-lite v0.2.17
#23 18.19    Compiling serde_core v1.0.228
#23 18.19    Compiling version_check v0.9.5
#23 18.19    Compiling once_cell v1.21.4
#23 18.19    Compiling itoa v1.0.18
#23 18.19    Compiling bytes v1.11.1
#23 18.19    Compiling serde v1.0.228
#23 18.19    Compiling thiserror v1.0.69
#23 18.20    Compiling parking_lot_core v0.9.12
#23 18.21    Compiling memchr v2.8.0
#23 18.21    Compiling futures-core v0.3.32
#23 18.21    Compiling scopeguard v1.2.0
#23 18.21    Compiling stable_deref_trait v1.2.1
#23 18.21    Compiling log v0.4.29
#23 18.21    Compiling futures-sink v0.3.32
#23 18.21    Compiling shlex v1.3.0
#23 18.21    Compiling find-msvc-tools v0.1.9
#23 18.21    Compiling getrandom v0.4.2
#23 18.21    Compiling rand_core v0.10.1
#23 18.21    Compiling typenum v1.20.0
#23 18.21    Compiling zerocopy v0.8.48
#23 18.21    Compiling slab v0.4.12
#23 18.21    Compiling futures-io v0.3.32
#23 18.21    Compiling futures-task v0.3.32
#23 18.21    Compiling writeable v0.6.3
#23 18.21    Compiling percent-encoding v2.3.2
#23 18.26    Compiling litemap v0.8.2
#23 18.40    Compiling lock_api v0.4.14
#23 18.41    Compiling tracing-core v0.1.36
#23 18.42    Compiling icu_properties_data v2.2.0
#23 18.43    Compiling icu_normalizer_data v2.2.0
#23 18.44    Compiling futures-channel v0.3.32
#23 18.44    Compiling utf8_iter v1.0.4
#23 18.46    Compiling zmij v1.0.21
#23 18.47    Compiling cc v1.2.62
#23 18.54    Compiling httparse v1.10.1
#23 18.54    Compiling cpufeatures v0.2.17
#23 18.56    Compiling serde_json v1.0.149
#23 18.56    Compiling form_urlencoded v1.2.2
#23 18.56    Compiling bitflags v2.11.1
#23 18.57    Compiling generic-array v0.14.7
#23 18.58    Compiling tower-service v0.3.3
#23 18.59    Compiling ahash v0.8.12
#23 18.61    Compiling zeroize v1.8.2
#23 18.61    Compiling httpdate v1.0.3
#23 18.62    Compiling try-lock v0.2.5
#23 18.64    Compiling vcpkg v0.2.15
#23 18.65    Compiling untrusted v0.9.0
#23 18.66    Compiling tower-layer v0.3.3
#23 18.66    Compiling pkg-config v0.3.33
#23 18.67    Compiling sync_wrapper v1.0.2
#23 18.68    Compiling regex-syntax v0.8.10
#23 18.70    Compiling http v1.4.0
#23 18.74    Compiling atomic-waker v1.1.2
#23 18.80    Compiling rustls v0.23.40
#23 18.83    Compiling cpufeatures v0.3.1
#23 18.84    Compiling want v0.3.1
#23 18.86    Compiling ipnet v2.12.0
#23 18.93    Compiling rustls-pki-types v1.14.1
#23 18.95    Compiling base64 v0.22.1
#23 18.98    Compiling hashbrown v0.17.1
#23 18.98    Compiling lazy_static v1.5.0
#23 18.98    Compiling mime v0.3.17
#23 19.03    Compiling subtle v2.6.1
#23 19.04    Compiling chacha20 v0.10.2
#23 19.05    Compiling equivalent v1.0.2
#23 19.06    Compiling tracing-log v0.2.0
#23 19.06    Compiling thread_local v1.1.9
#23 19.08    Compiling fallible-streaming-iterator v0.1.9
#23 19.14    Compiling ryu v1.0.23
#23 19.19    Compiling io-uring v0.7.12
#23 19.22    Compiling toml_write v0.1.2
#23 19.23    Compiling sharded-slab v0.1.7
#23 19.26    Compiling errno v0.3.14
#23 19.32    Compiling socket2 v0.6.3
#23 19.35    Compiling mio v1.2.0
#23 19.43    Compiling syn v2.0.117
#23 19.44    Compiling getrandom v0.2.17
#23 19.48    Compiling signal-hook-registry v1.4.8
#23 19.48    Compiling block-buffer v0.10.4
#23 19.49    Compiling crypto-common v0.1.7
#23 19.49    Compiling http-body v1.0.1
#23 19.53    Compiling indexmap v2.14.0
#23 19.55    Compiling parking_lot v0.12.5
#23 19.55    Compiling hex v0.4.3
#23 19.55    Compiling fallible-iterator v0.3.0
#23 19.56    Compiling winnow v0.7.15
#23 19.58    Compiling rand v0.10.2
#23 19.59    Compiling rustversion v1.0.22
#23 19.59    Compiling nu-ansi-term v0.50.3
#23 19.63    Compiling webpki-roots v1.0.7
#23 19.63    Compiling multer v3.1.0
#23 19.64    Compiling byteorder v1.5.0
#23 19.64    Compiling anyhow v1.0.104
#23 19.65    Compiling regex-automata v0.4.14
#23 19.67    Compiling rand_core v0.6.4
#23 19.67    Compiling utf-8 v0.7.6
#23 19.68    Compiling data-encoding v2.11.0
#23 19.76    Compiling digest v0.10.7
#23 19.82    Compiling http-body-util v0.1.3
#23 19.88    Compiling encoding_rs v0.8.35
#23 19.90    Compiling spin v0.9.8
#23 19.90    Compiling rt-metrics v0.1.0 (/src/crates/rt-metrics)
#23 19.94    Compiling rt-piece-picker v0.1.0 (/src/crates/rt-piece-picker)
#23 19.96    Compiling matchit v0.7.3
#23 19.96    Compiling unicase v2.9.0
#23 19.98    Compiling base64 v0.23.1
#23 19.99    Compiling http-range-header v0.4.2
#23 20.02    Compiling sha1 v0.10.6
#23 20.03    Compiling sha2 v0.10.9
#23 20.12    Compiling ring v0.17.14
#23 20.14    Compiling libsqlite3-sys v0.28.0
#23 20.20    Compiling uuid v1.23.1
#23 20.20    Compiling serde_path_to_error v0.1.20
#23 20.31    Compiling mime_guess v2.0.5
#23 20.45    Compiling rt-hash v0.1.0 (/src/crates/rt-hash)
#23 21.32    Compiling matchers v0.2.0
#23 21.38    Compiling ppv-lite86 v0.2.21
#23 21.46    Compiling hashbrown v0.14.5
#23 21.62    Compiling rand_chacha v0.3.1
#23 21.75    Compiling rand v0.8.6
#23 21.98    Compiling hashlink v0.9.1
#23 22.03    Compiling synstructure v0.13.2
#23 22.27    Compiling serde_derive v1.0.228
#23 22.27    Compiling thiserror-impl v1.0.69
#23 22.27    Compiling zerofrom-derive v0.1.7
#23 22.27    Compiling yoke-derive v0.8.2
#23 22.27    Compiling tokio-macros v2.7.0
#23 22.27    Compiling zerovec-derive v0.11.3
#23 22.27    Compiling displaydoc v0.2.5
#23 22.27    Compiling tracing-attributes v0.1.31
#23 22.27    Compiling futures-macro v0.3.32
#23 22.27    Compiling async-trait v0.1.89
#23 22.27    Compiling axum-macros v0.4.2
#23 22.77    Compiling tokio v1.52.3
#23 22.78    Compiling futures-util v0.3.32
#23 22.82    Compiling tracing v0.1.44
#23 22.82    Compiling zerofrom v0.1.8
#23 22.89    Compiling yoke v0.8.2
#23 23.08    Compiling zerotrie v0.2.4
#23 23.18    Compiling zerovec v0.11.6
#23 23.26    Compiling rt-bencode v0.1.0 (/src/crates/rt-bencode)
#23 23.26    Compiling tungstenite v0.24.0
#23 23.26    Compiling rt-peer-manager v0.1.0 (/src/crates/rt-peer-manager)
#23 23.35    Compiling rt-dht v0.1.0 (/src/crates/rt-dht)
#23 23.60    Compiling tinystr v0.8.3
#23 23.60    Compiling potential_utf v0.1.5
#23 23.65    Compiling icu_collections v2.2.0
#23 23.69    Compiling icu_locale_core v2.2.0
#23 24.01    Compiling rt-path v0.1.0 (/src/crates/rt-path)
#23 24.01    Compiling toml_datetime v0.6.11
#23 24.01    Compiling tracing-serde v0.2.0
#23 24.01    Compiling serde_spanned v0.6.9
#23 24.01    Compiling serde_urlencoded v0.7.1
#23 24.07    Compiling tracing-subscriber v0.3.23
#23 24.12    Compiling toml_edit v0.22.27
#23 24.12    Compiling rt-piece-map v0.1.0 (/src/crates/rt-piece-map)
#23 24.19    Compiling rt-fastresume v0.1.0 (/src/crates/rt-fastresume)
#23 24.24    Compiling icu_provider v2.2.0
#23 24.43    Compiling icu_normalizer v2.2.0
#23 24.43    Compiling icu_properties v2.2.0
#23 24.71    Compiling futures-executor v0.3.32
#23 24.71    Compiling axum-core v0.4.5
#23 24.81    Compiling futures v0.3.32
#23 25.12    Compiling rt-logging v0.1.0 (/src/crates/rt-logging)
#23 25.13    Compiling idna_adapter v1.2.2
#23 25.17    Compiling idna v1.1.0
#23 25.39    Compiling url v2.5.8
#23 25.53    Compiling toml v0.8.23
#23 25.82    Compiling rt-metainfo v0.1.0 (/src/crates/rt-metainfo)
#23 25.82    Compiling rt-tracker v0.1.0 (/src/crates/rt-tracker)
#23 25.87    Compiling rt-config v0.1.0 (/src/crates/rt-config)
#23 26.56    Compiling rustls-webpki v0.103.13
#23 26.60    Compiling hyper v1.9.0
#23 26.60    Compiling tower v0.5.3
#23 26.60    Compiling tokio-util v0.7.18
#23 26.60    Compiling rt-session v0.1.0 (/src/crates/rt-session)
#23 26.60    Compiling tokio-tungstenite v0.24.0
#23 26.60    Compiling rt-utp v0.1.0 (/src/crates/rt-utp)
#23 26.60    Compiling rt-storage v0.1.0 (/src/crates/rt-storage)
#23 26.60    Compiling rt-api-model v0.1.0 (/src/crates/rt-api-model)
#23 26.95    Compiling rt-peer-wire v0.1.0 (/src/crates/rt-peer-wire)
#23 26.95    Compiling tower-http v0.5.2
#23 27.02    Compiling tower-http v0.6.10
#23 27.34    Compiling hyper-util v0.1.20
#23 28.41    Compiling axum v0.7.9
#23 29.33    Compiling tokio-rustls v0.26.4
#23 29.52    Compiling hyper-rustls v0.27.9
#23 29.63    Compiling reqwest v0.12.28
#23 52.60    Compiling rusqlite v0.31.0
#23 53.09    Compiling rt-db v0.1.0 (/src/crates/rt-db)
#23 53.61    Compiling rt-engine v0.1.0 (/src/crates/rt-engine)
#23 53.61    Compiling rt-migrate v0.1.0 (/src/crates/rt-migrate)
#23 59.15    Compiling rt-api-qbit v0.1.0 (/src/crates/rt-api-qbit)
#23 59.15    Compiling rt-api-native v0.1.0 (/src/crates/rt-api-native)
#23 59.15    Compiling rt-api-deluge v0.1.0 (/src/crates/rt-api-deluge)
#23 59.15    Compiling rt-api-transmission v0.1.0 (/src/crates/rt-api-transmission)
#23 78.65    Compiling torrentngd v0.1.0 (/src/crates/torrentngd)
#23 116.5     Finished `release` profile [optimized] target(s) in 1m 56s
#23 DONE 116.7s

#24 [stage-2 3/5] COPY --from=build /src/target/release/torrentngd /usr/local/bin/torrentngd
#24 DONE 0.2s

#25 [stage-2 4/5] COPY --from=webui-build /webui/dist /usr/share/torrentng/webui
#25 DONE 0.1s

#26 [stage-2 5/5] COPY deploy/native/config.toml /etc/torrentngd/config.toml
#26 DONE 0.1s

#27 exporting to oci image format
#27 exporting layers
#27 exporting layers 1.2s done
#27 exporting manifest sha256:6a90b13a6151d87201fc0d6ed1d0121b6468010ce76684ede23110762ec1d49b 0.1s done
#27 exporting config sha256:74b9382c4f49299aec8555890c9987d1b89f20d7ea0a70deea7d43f599a7dee7 0.0s done
#27 sending tarball
#27 sending tarball 1.1s done
#27 DONE 2.4s

#28 importing to docker
#28 DONE 0.0s

#29 resolving provenance for metadata file
#29 DONE 0.0s
 Image torrentng/torrentngd:interop Built
 Network torrentng-interop_interop Creating
 Volume torrentng-interop_torrentngd-state Creating
 Network torrentng-interop_interop Creating
 Volume torrentng-interop_torrentngd-state Creating
 Volume torrentng-interop_torrentngd-state Created
 Volume torrentng-interop_torrentngd-state Created
 Network torrentng-interop_interop Created
 Network torrentng-interop_interop Created
 Container torrentng-interop-transmission-1 Creating
 Container torrentng-interop-torrentngd-1 Creating
 Container torrentng-interop-rtorrent-1 Creating
 Container torrentng-interop-fixture-http-1 Creating
 Container torrentng-interop-opentracker-1 Creating
 Container torrentng-interop-deluge-1 Creating
 Container torrentng-interop-qbittorrent-1 Creating
 Container torrentng-interop-opentracker-1 Created
 Container torrentng-interop-torrentngd-1 Created
 Container torrentng-interop-deluge-1 Created
 Container torrentng-interop-transmission-1 Created
 Container torrentng-interop-qbittorrent-1 Created
 Container torrentng-interop-fixture-http-1 Created
 Container torrentng-interop-rtorrent-1 Created
 Container torrentng-interop-opentracker-1 Starting
 Container torrentng-interop-torrentngd-1 Starting
 Container torrentng-interop-rtorrent-1 Starting
 Container torrentng-interop-transmission-1 Starting
 Container torrentng-interop-deluge-1 Starting
 Container torrentng-interop-fixture-http-1 Starting
 Container torrentng-interop-qbittorrent-1 Starting
 Container torrentng-interop-opentracker-1 Started
 Container torrentng-interop-torrentngd-1 Started
 Container torrentng-interop-rtorrent-1 Started
 Container torrentng-interop-transmission-1 Started
 Container torrentng-interop-deluge-1 Started
 Container torrentng-interop-fixture-http-1 Started
 Container torrentng-interop-qbittorrent-1 Started
[interop] waiting for client APIs and ports
[interop] creating local legal fixtures
[interop] running local case rust-pulls-from-qbit
[interop] running local case rust-pulls-from-transmission
[interop] running local case rust-pulls-from-deluge
[interop] running local case rust-pulls-from-rtorrent
[interop] running local case qbit-pulls-from-rust
[interop] running local case transmission-pulls-from-rust
[interop] running local case deluge-pulls-from-rust
[interop] running local case rtorrent-pulls-from-rust
[interop] running local case mesh-swarm
[interop] running local case churn
[interop] running extended local case rust-webseed-only
[interop] running extended local case rust-explicit-peer-private
[interop] running extended local case rust-restart-recovery
 Container torrentng-interop-torrentngd-1 Restarting
 Container torrentng-interop-torrentngd-1 Started
[interop] running extended local case rust-api-facades
[interop] running protocol local case rust-magnet-with-tracker
[interop] ensuring interop services are running: opentracker fixture-http
 Container torrentng-interop-opentracker-1 Running
 Container torrentng-interop-fixture-http-1 Running
[interop] running protocol local case rust-trackerless-magnet
[interop] running protocol local case rust-udp-tracker
[interop] ensuring interop services are running: opentracker
 Container torrentng-interop-opentracker-1 Running
[interop] running protocol local case rust-multi-tracker-fallback
[interop] ensuring interop services are running: opentracker
 Container torrentng-interop-opentracker-1 Running
[interop] running protocol local case tracker-outage-after-peer-discovery
[interop] ensuring interop services are running: opentracker
 Container torrentng-interop-opentracker-1 Running
 Container torrentng-interop-opentracker-1 Stopping
 Container torrentng-interop-opentracker-1 Stopped
[interop] running protocol local case webseed-outage-fallback
[interop] ensuring interop services are running: opentracker fixture-http
 Container torrentng-interop-fixture-http-1 Running
 Container torrentng-interop-opentracker-1 Starting
 Container torrentng-interop-opentracker-1 Started
 Container torrentng-interop-fixture-http-1 Stopping
 Container torrentng-interop-fixture-http-1 Stopped
[interop] running protocol local case private-torrent-no-dht-pex
[interop] running protocol local case resume-after-partial-download
[interop] ensuring interop services are running: fixture-http
 Container torrentng-interop-fixture-http-1 Starting
 Container torrentng-interop-fixture-http-1 Started
 Container torrentng-interop-torrentngd-1 Restarting
 Container torrentng-interop-torrentngd-1 Started
[interop] running protocol local case force-recheck-corruption-repair
[interop] ensuring interop services are running: fixture-http
 Container torrentng-interop-fixture-http-1 Running
[interop] running protocol local case missing-file-recovery
[interop] ensuring interop services are running: fixture-http
 Container torrentng-interop-fixture-http-1 Running
[interop] running protocol local case rust-seeds-to-all-reference-clients
[interop] ensuring interop services are running: opentracker fixture-http
 Container torrentng-interop-opentracker-1 Running
 Container torrentng-interop-fixture-http-1 Running
[interop] running protocol local case endgame-multi-peer
[interop] ensuring interop services are running: opentracker
 Container torrentng-interop-opentracker-1 Running
[interop] running protocol local case rust-partial-file-selection
[interop] running protocol local case rust-qbit-mutation-facade
[interop] ensuring interop services are running: opentracker
 Container torrentng-interop-opentracker-1 Running
[interop] wrote report /home/keith/Documents/code/TorrentNG/certification/reports/interop-matrix-20260910T190228Z.md
[interop] keeping interop stack because INTEROP_KEEP_STACK=1
```

## mobile qBittorrent compatibility matrix

```text
/home/keith/Documents/code/TorrentNG/certification/reports/mobile-compat-universal-20260910T192200Z.md
```

## public torrent interop matrix

```text
[interop] starting interop compose stack
 Image torrentng/torrentngd:interop Building
#1 [internal] load local bake definitions
#1 reading from stdin 574B done
#1 DONE 0.0s

#2 [internal] load build definition from Dockerfile
#2 transferring dockerfile: 1.26kB done
#2 DONE 0.0s

#3 [internal] load metadata for docker.io/library/rust:1.96-bookworm
#3 ...

#4 [internal] load metadata for docker.io/library/debian:bookworm-slim
#4 DONE 2.8s

#5 [internal] load metadata for docker.io/library/node:22-bookworm-slim
#5 DONE 2.9s

#3 [internal] load metadata for docker.io/library/rust:1.96-bookworm
#3 DONE 3.4s

#6 [internal] load .dockerignore
#6 transferring context: 221B done
#6 DONE 0.0s

#7 [internal] load build context
#7 transferring context: 15.99kB done
#7 DONE 0.0s

#8 [stage-2 1/5] FROM docker.io/library/debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
#8 resolve docker.io/library/debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171 0.1s done
#8 DONE 0.1s

#9 [webui-build 1/6] FROM docker.io/library/node:22-bookworm-slim@sha256:83f487e0a63425e5b4d146fb5e5be574bcbe1b7b843d3ebafdd95eaf7767a7e5
#9 resolve docker.io/library/node:22-bookworm-slim@sha256:83f487e0a63425e5b4d146fb5e5be574bcbe1b7b843d3ebafdd95eaf7767a7e5 0.1s done
#9 DONE 0.1s

#10 [build 1/5] FROM docker.io/library/rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663
#10 resolve docker.io/library/rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663 0.1s done
#10 DONE 0.1s

#11 [webui-build 3/6] COPY webui/package.json webui/package-lock.json ./
#11 CACHED

#12 [webui-build 4/6] RUN npm ci
#12 CACHED

#13 [build 3/5] COPY Cargo.toml Cargo.lock ./
#13 CACHED

#14 [webui-build 5/6] COPY webui/ ./
#14 CACHED

#15 [build 2/5] WORKDIR /src
#15 CACHED

#16 [build 5/5] RUN cargo build --release --locked -p torrentngd
#16 CACHED

#17 [stage-2 4/5] COPY --from=webui-build /webui/dist /usr/share/torrentng/webui
#17 CACHED

#18 [stage-2 2/5] RUN apt-get update     && apt-get install -y --no-install-recommends ca-certificates curl tini     && rm -rf /var/lib/apt/lists/*     && useradd --system --home /var/lib/torrentngd --create-home --shell /usr/sbin/nologin torrentngd     && mkdir -p /data /config /var/lib/torrentngd /usr/share/torrentng/webui     && chown -R torrentngd:torrentngd /data /config /var/lib/torrentngd
#18 CACHED

#19 [webui-build 6/6] RUN TNG_WEBUI_OUT_DIR=dist npm run build
#19 CACHED

#20 [build 4/5] COPY crates ./crates
#20 CACHED

#21 [webui-build 2/6] WORKDIR /webui
#21 CACHED

#22 [stage-2 3/5] COPY --from=build /src/target/release/torrentngd /usr/local/bin/torrentngd
#22 CACHED

#23 [stage-2 5/5] COPY deploy/native/config.toml /etc/torrentngd/config.toml
#23 CACHED

#24 exporting to image
#24 exporting layers done
#24 exporting manifest sha256:2050c70a30ca3023ae75d174636be135e8a10d1af3107372fecd36f4aa1e9d93 done
#24 exporting config sha256:f03bea227aebf62f42186f61eef00107ad5eb7f22d6f79bc9fb000eb4dfcf88d done
#24 exporting attestation manifest sha256:696f5800cc90ec5f2bd213479491de7efd3637488c98c67d254aaa55aa6d573e 0.0s done
#24 exporting manifest list sha256:dab693eaa3a3a7efb32617e37e71188ae0c16904755131abb6d06b2479c14972 0.0s done
#24 naming to docker.io/torrentng/torrentngd:interop
#24 naming to docker.io/torrentng/torrentngd:interop done
#24 unpacking to docker.io/torrentng/torrentngd:interop 0.0s done
#24 DONE 0.1s

#25 resolving provenance for metadata file
#25 DONE 0.0s
 Image torrentng/torrentngd:interop Built
 Network torrentng-interop_interop Creating
 Network torrentng-interop_interop Creating
 Volume torrentng-interop_torrentngd-state Creating
 Volume torrentng-interop_torrentngd-state Creating
 Volume torrentng-interop_torrentngd-state Created
 Volume torrentng-interop_torrentngd-state Created
 Network torrentng-interop_interop Created
 Network torrentng-interop_interop Created
 Container torrentng-interop-fixture-http-1 Creating
 Container torrentng-interop-rtorrent-1 Creating
 Container torrentng-interop-opentracker-1 Creating
 Container torrentng-interop-qbittorrent-1 Creating
 Container torrentng-interop-deluge-1 Creating
 Container torrentng-interop-transmission-1 Creating
 Container torrentng-interop-torrentngd-1 Creating
 Container torrentng-interop-opentracker-1 Created
 Container torrentng-interop-qbittorrent-1 Created
 Container torrentng-interop-rtorrent-1 Created
 Container torrentng-interop-deluge-1 Created
 Container torrentng-interop-transmission-1 Created
 Container torrentng-interop-torrentngd-1 Created
 Container torrentng-interop-fixture-http-1 Created
 Container torrentng-interop-opentracker-1 Starting
 Container torrentng-interop-qbittorrent-1 Starting
 Container torrentng-interop-torrentngd-1 Starting
 Container torrentng-interop-deluge-1 Starting
 Container torrentng-interop-rtorrent-1 Starting
 Container torrentng-interop-transmission-1 Starting
 Container torrentng-interop-fixture-http-1 Starting
 Container torrentng-interop-opentracker-1 Started
 Container torrentng-interop-qbittorrent-1 Started
 Container torrentng-interop-torrentngd-1 Started
 Container torrentng-interop-deluge-1 Started
 Container torrentng-interop-rtorrent-1 Started
 Container torrentng-interop-transmission-1 Started
 Container torrentng-interop-fixture-http-1 Started
[interop] waiting for client APIs and ports
[interop] resolving public torrent debian from official source
[interop] wrote report /home/keith/Documents/code/TorrentNG/certification/reports/interop-matrix-20260910T192200Z.md
[interop] keeping interop stack because INTEROP_KEEP_STACK=1
```

## real-device storage matrix

```text
SKIP: set UNIVERSAL_COMPAT_REAL_DEVICE=1 and configure storage test paths to run ignored device tests
```

Overall status: PASS_WITH_SKIPS
Skipped gates: 1
