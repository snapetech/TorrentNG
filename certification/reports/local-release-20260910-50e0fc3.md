# TorrentNG Local Release Gate

- Date UTC: 2026-09-10T19:36:47Z
- Host: kspld0
- Kernel: Linux 7.1.7-arch1-1 x86_64 GNU/Linux
- Rust: rustc 1.98.1 (48a229cea 2026-09-01)
- Cargo: cargo 1.98.1 (797e8a9bc 2026-08-05)
- Commit: 50e0fc3
- Branch: main
- Worktree: clean

## Gates

| Gate | Result | Duration |
|---|---|---|
| format | PASS | 1s |
| workspace tests | PASS | 5s |
| Storage NG feature matrix | PASS | 14s |
| WebUI certification | PASS | 11s |
| API facade certification | PASS | 1s |
| release artifact build | PASS | 0s |
| authenticated release-binary smoke | PASS | 0s |
| backup and restore drill | PASS | 2s |
| migration exported corpus coverage | PASS | 1s |
| native config security review | PASS | 0s |
| sidecar config security review | PASS | 0s |
| storage release certification | SKIP | 0s |

## Git Status

```text
```

## format

- Command: `cargo fmt --check`

```text
```

## workspace tests

- Command: `cargo test --workspace`

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.08s
     Running unittests src/lib.rs (target/debug/deps/rt_api_deluge-072a98054050b9f9)

running 26 tests
test tests::deluge_api_snapshot_estimates_scale_with_torrent_count ... ok
test tests::deluge_options_project_to_engine_limits ... ok
test tests::deluge_mutator_parsers_accept_client_shapes ... ok
test tests::deluge_torrent_data_decoder_accepts_data_urls_and_unpadded_base64 ... ok
test tests::deluge_peer_projection_uses_native_snapshots ... ok
test tests::deluge_projection_arguments_reject_malformed_filters_and_fields ... ok
test tests::deluge_state_projects_active_recheck_as_checking ... ok
test tests::deluge_shutdown_notifies_daemon ... ok
test tests::deluge_tracker_projection_uses_persisted_engine_state ... ok
test tests::deluge_file_probe_returns_array_shape ... ok
test tests::deluge_torrent_options_require_native_engine ... ok
test tests::deluge_label_mutation_requires_native_engine ... ok
test tests::deluge_router_enforces_configured_token_and_preserves_login_body ... ok
test tests::deluge_idempotency_key_replays_mutation_and_rejects_reuse ... ok
test tests::deluge_torrent_status_field_matrix_is_present ... ok
test tests::deluge_plugin_cache_and_notification_shapes_are_structured ... ok
test tests::deluge_torrents_status_honors_filter_dictionary ... ok
test tests::deluge_advertised_method_list_matches_probe_matrix ... ok
test tests::deluge_torrent_status_honors_requested_fields ... ok
test tests::deluge_url_download_returns_stateful_safe_token ... ok
test tests::deluge_update_ui_honors_requested_fields ... ok
test tests::deluge_update_ui_projects_registry ... ok
test tests::deluge_web_add_torrents_reports_unavailable_engine_per_item ... ok
test tests::deluge_url_download_tokens_are_one_shot_and_report_engine_failure ... ok
test tests::deluge_unsupported_plugin_writes_fail_closed ... ok
test tests::deluge_auth_and_config_are_supported ... ok

test result: ok. 26 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/rt_api_model-b14b44efa623d542)

running 14 tests
test auth::tests::csrf_accepts_same_host_origin_and_non_browser_requests ... ok
test auth::tests::session_cookie_detection_is_name_and_value_aware ... ok
test auth::tests::csrf_rejects_cross_site_and_mismatched_origins ... ok
test error::tests::error_serializes ... ok
test idempotency::tests::keys_are_bounded_to_printable_ascii ... ok
test metrics::tests::api_metrics_track_snapshot_and_sse_lifecycle ... ok
test idempotency::tests::dropped_execution_guard_releases_claim_for_retry ... ok
test idempotency::tests::same_key_replays_and_different_body_conflicts ... ok
test idempotency::tests::abandoned_key_can_be_retried ... ok
test snapshot::tests::range_is_bounded_and_exact ... ok
test snapshot::tests::bitmap_membership_updates_share_unmodified_chunks ... ok
test snapshot::tests::replacing_one_item_shares_unmodified_chunks ... ok
test torrent::tests::add_request_optional_fields ... ok
test torrent::tests::torrent_summary_serializes ... ok

test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/rt_api_native-f4295edb475eb3cc)

running 60 tests
test handlers::tests::job_view_projects_progress_and_checkpoint_fields ... ok
test handlers::tests::api_snapshot_estimates_scale_with_torrent_count ... ok
test handlers::tests::level_from_kind_is_conservative ... ok
test handlers::tests::json_store_validation_accepts_documented_rule_shapes ... ok
test handlers::tests::json_store_validation_rejects_malformed_executable_fields ... ok
test handlers::tests::metric_with_label_escapes_label_values_once ... ok
test handlers::tests::native_engine_capabilities_cover_rewrite_surface ... ok
test handlers::tests::native_webui_capabilities_match_mounted_routes ... ok
test handlers::tests::session_event_response_rejects_corrupt_durable_payload ... ok
test handlers::tests::session_event_response_projects_level_and_payload ... ok
test handlers::tests::get_torrent_found ... ok
test handlers::tests::get_torrent_not_found ... ok
test handlers::tests::jobs_without_engine_returns_unavailable ... ok
test handlers::tests::delete_torrent_found ... ok
test handlers::tests::diagnostics_without_engine_returns_unavailable ... ok
test handlers::tests::delete_torrent_not_found ... ok
test handlers::tests::metrics_reports_unavailable_without_engine ... ok
test handlers::tests::idempotency_key_replays_native_mutation_and_rejects_reuse ... ok
test handlers::tests::add_torrent_without_engine_returns_unavailable ... ok
test handlers::tests::storage_plan_preview_projects_move_steps ... ok
test handlers::tests::storage_plan_completed_steps_are_bounded_by_plan ... ok
test handlers::tests::list_torrents_with_entry ... ok
test handlers::tests::storage_plan_preview_projects_staged_import_copy ... ok
test handlers::tests::health_reports_unavailable_without_engine ... ok
test handlers::tests::list_torrents_empty ... ok
test handlers::tests::storage_plan_root_validation_rejects_escape ... ok
test handlers::tests::list_torrents_reports_total_independent_of_page_size ... ok
test handlers::tests::utp_capability_helpers_match_runtime_policy_values ... ok
test handlers::tests::update_torrent_limits_request_distinguishes_null_from_absent ... ok
test state::tests::signed_summary_projection_saturates_unsigned_counters ... ok
test handlers::tests::list_torrents_snapshot_pins_pages_across_mutations ... ok
test handlers::tests::torrent_delta_reports_initial_changes_and_removals ... ok
test state::tests::journal_refresh_applies_final_entry_state_without_registry_scan ... ok
test handlers::tests::torrent_delta_chunks_initial_snapshot_at_one_revision ... ok
test state::tests::media_type_facets_are_indexed_and_updated_incrementally ... ok
test state::tests::structural_snapshot_refresh_does_not_use_stale_positions ... ok
test state::tests::text_filter_index_handles_short_filters_and_case_folding ... ok
test handlers::tests::storage_without_engine_returns_unavailable ... ok
test handlers::tests::transfer_limits_without_engine_returns_unavailable ... ok
test handlers::tests::torrent_limits_without_engine_returns_unavailable ... ok
test handlers::tests::update_torrent_without_engine_updates_registry ... ok
test handlers::tests::tag_post_delete_and_bulk_set_are_native ... ok
test handlers::tests::storage_plan_completed_steps_accept_sorted_unique_subset ... ok
test handlers::tests::native_hash_resolution_preserves_unknown_targets_for_error_reporting ... ok
test handlers::tests::render_metrics_exposes_dependency_health_and_pressure ... ok
test handlers::tests::reannounce_torrent_without_engine_is_unavailable ... ok
test handlers::tests::native_login_issues_session_cookie_and_validates_tokens ... ok
test handlers::tests::mutating_endpoint_requires_configured_token ... ok
test handlers::tests::pause_torrent_found ... ok
test handlers::tests::mutating_endpoint_accepts_bearer_token ... ok
test handlers::tests::resume_torrent_found ... ok
test handlers::tests::recheck_torrent_found ... ok
test handlers::tests::patch_files_rejects_empty_body ... ok
test handlers::tests::patch_trackers_without_engine_reports_unavailable ... ok
test handlers::tests::set_category_without_engine_updates_registry ... ok
test handlers::tests::patch_tags_without_engine_updates_registry ... ok
test handlers::tests::native_rtorrent_compatibility_routes_fail_closed ... ok
test handlers::tests::native_alias_and_projection_routes_are_exposed ... ok
test handlers::tests::mutating_endpoint_accepts_session_cookie_token ... ok
test handlers::tests::render_metrics_includes_engine_stats ... ok

test result: ok. 60 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s

     Running unittests src/lib.rs (target/debug/deps/rt_api_qbit-54aa95d378559fdb)

running 76 tests
test handlers::tests::log_main_query_filters_qbit_types ... ok
test handlers::tests::parse_form_body_decodes_qbit_forms ... ok
test handlers::tests::parse_qbit_bool_accepts_common_wire_values ... ok
test handlers::tests::pieces_have_is_bounded ... ok
test handlers::tests::parse_peer_addrs_accepts_pipe_separated_socket_addresses ... ok
test handlers::tests::qbit_api_snapshot_estimates_scale_with_torrent_count ... ok
test handlers::tests::qbit_filters_do_not_silently_fall_back_to_all_torrents ... ok
test handlers::tests::qbit_files_project_partial_per_file_progress ... ok
test handlers::tests::qbit_log_entry_projects_session_events ... ok
test handlers::tests::qbit_progress_and_piece_count_ignore_stale_completion_timestamp ... ok
test handlers::tests::qbit_log_entry_rejects_corrupt_or_unidentified_session_events ... ok
test handlers::tests::qbit_peer_log_entry_projects_engine_peer_snapshot ... ok
test handlers::tests::qbit_log_type_uses_level_payload_and_kind_fallbacks ... ok
test handlers::tests::qbit_session_rates_sum_torrent_info_rates ... ok
test handlers::tests::qbit_state_projects_active_recheck_as_checking ... ok
test handlers::tests::qbit_swarm_projection_counts_live_peers_and_rates ... ok
test handlers::tests::qbit_tracker_projection_prefers_live_snapshot_order ... ok
test handlers::tests::qbit_tracker_projection_uses_persisted_engine_state ... ok
test handlers::tests::search_plugin_validation_rejects_silent_projection_loss ... ok
test handlers::tests::split_tracker_values_accepts_qbit_separators_and_dedupes ... ok
test handlers::tests::redact_log_url_removes_sensitive_parts ... ok
test handlers::tests::strict_mutation_parsers_reject_dropped_values ... ok
test handlers::tests::sync_rid_covers_tracker_snapshot_digest ... ok
test handlers::tests::sync_rid_is_order_independent_and_covers_metadata_projection ... ok
test handlers::tests::ssrf_guard_rejects_private_and_local_ips ... ok
test handlers::tests::qbit_torrent_peers_projection_and_rid_are_stable ... ok
test handlers::tests::login_returns_ok ... ok
test handlers::tests::login_requires_configured_token_and_cookie_auth_round_trips ... ok
test handlers::tests::category_and_global_tag_endpoints_update_registry ... ok
test handlers::tests::idempotency_key_replays_qbit_mutation_and_rejects_reuse ... ok
test handlers::tests::app_cookies_and_api_key_roundtrip ... ok
test handlers::tests::add_and_remove_tags_resolve_all_hashes ... ok
test handlers::tests::qbit_detail_endpoints_fail_closed_without_engine_metadata ... ok
test handlers::tests::create_tags_persists_empty_global_tags ... ok
test handlers::tests::app_shutdown_notifies_daemon_and_email_is_explicitly_unsupported ... ok
test handlers::tests::app_set_preferences_persists_form_and_json_updates ... ok
test handlers::tests::app_preferences_and_default_save_path_ok ... ok
test handlers::tests::torrents_properties_returns_registry_projection_without_engine ... ok
test handlers::tests::torrents_info_rejects_unavailable_speed_sorting ... ok
test handlers::tests::torrents_info_rejects_mixed_all_hash_filter ... ok
test model::tests::state_mapping_covers_all_internal_states ... ok
test handlers::tests::torrents_properties_missing_hash_is_bad_request ... ok
test model::tests::unknown_state_maps_to_unknown ... ok
test model::tests::torrent_info_serializes ... ok
test model::tests::torrent_properties_serializes_qbit_fields ... ok
test handlers::tests::torrents_info_filters_by_tag_and_sorts ... ok
test state::tests::journal_refresh_applies_final_entry_state_without_registry_scan ... ok
test state::tests::structural_snapshot_refresh_does_not_use_stale_positions ... ok
test handlers::tests::torrents_info_pages_are_pinned_by_snapshot_header ... ok
test handlers::tests::torrents_info_with_entry ... ok
test handlers::tests::torrents_info_intersects_hash_filter_with_indexed_filters ... ok
test handlers::tests::qbit_alias_and_broad_compat_routes_are_registered ... ok
test state::tests::arbitrary_sort_values_do_not_grow_snapshot_order_cache ... ok
test handlers::tests::transfer_ban_peers_fails_closed_without_engine ... ok
test handlers::tests::transfer_info_fails_closed_without_engine ... ok
test handlers::tests::transfer_limit_endpoints_roundtrip_without_engine ... ok
test handlers::tests::app_version_ok ... ok
test handlers::tests::qbit_file_priority_rejects_values_outside_engine_contract ... ok
test handlers::tests::reannounce_reports_unavailable_without_engine ... ok
test handlers::tests::torrents_categories_and_tags_project_registry_labels ... ok
test handlers::tests::torrents_add_without_engine_returns_unavailable ... ok
test handlers::tests::rename_and_set_location_update_registry_without_engine ... ok
test handlers::tests::set_category_resolves_all_hashes ... ok
test handlers::tests::sync_maindata_returns_full_update ... ok
test handlers::tests::qbit_rss_items_and_rules_round_trip ... ok
test handlers::tests::qbit_torrent_export_requires_hash_and_engine_blob ... ok
test handlers::tests::qbit_search_plugins_and_jobs_are_stateful ... ok
test handlers::tests::set_category_applies_stored_category_save_path ... ok
test handlers::tests::qbit_response_field_matrix_is_present ... ok
test handlers::tests::sync_maindata_uses_stable_rid_for_unchanged_registry ... ok
test handlers::tests::sync_maindata_returns_registry_deltas_and_removals ... ok
test handlers::tests::torrents_info_empty ... ok
test handlers::tests::set_category_decodes_url_encoded_form_values ... ok
test handlers::tests::qbit_torrent_export_streams_persisted_torrent_blob ... ok
test handlers::tests::torrents_info_default_page_is_bounded ... ok
test handlers::tests::engine_backed_qbit_app_state_survives_engine_restart ... ok

test result: ok. 76 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s

     Running unittests src/lib.rs (target/debug/deps/rt_api_rtorrent-b68c889ce41950ba)

running 26 tests
test tests::file_tracker_and_peer_projectors_use_native_snapshot_fields ... ok
test tests::method_matrix_advertises_representative_rtorrent_families ... ok
test tests::global_throttle_setters_roundtrip_without_engine ... ok
test tests::download_reads_project_registry_state ... ok
test tests::rtorrent_api_snapshot_estimate_scales_with_torrents_and_commands ... ok
test tests::file_multicall_projects_registry_fallback_fields ... ok
test tests::lifecycle_fallback_mutates_registry_and_rejects_missing_torrents ... ok
test tests::multicall_honors_view_and_rejects_malformed_commands ... ok
test tests::custom_fields_roundtrip ... ok
test tests::configured_embedded_xmlrpc_state_rejects_missing_or_wrong_token ... ok
test tests::path_load_rejects_unsupported_filesystem_boundary ... ok
test tests::multicall_returns_rtorrent_row_shape ... ok
test tests::magnet_load_and_erase_update_registry ... ok
test tests::tracker_announce_fails_closed_without_engine ... ok
test tests::value_to_json_preserves_rtorrent_types ... ok
test tests::tracker_announce_requires_info_hash ... ok
test tests::rtorrent_mutators_reject_missing_or_malformed_values ... ok
test tests::xml_projection_saturates_unrepresentable_unsigned_values ... ok
test tests::raw_torrent_load_accepts_xmlrpc_base64_payload ... ok
test tests::xml_value_parser_accepts_nested_arrays_structs_base64_and_nil ... ok
test tests::view_size_projects_registry_backed_compat_views ... ok
test tests::xmlrpc_parser_accepts_array_struct_base64_and_nil_shapes ... ok
test tests::xmlrpc_fixture_roundtrips ... ok
test tests::torrent_local_state_is_case_insensitive_and_erased_with_torrent ... ok
test tests::torrent_throttle_setters_roundtrip_without_engine ... ok
test tests::xmlrpc_method_list_and_detail_multicalls_have_stable_shapes ... ok

test result: ok. 26 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/library_entry_point.rs (target/debug/deps/library_entry_point-74f568cb3cfd416b)

running 2 tests
test public_library_entry_point_enforces_embedded_credentials ... ok
test public_library_entry_point_executes_xmlrpc_without_http_server ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/rt_api_transmission-8d2ee114d83ffdcc)

running 41 tests
test tests::renamed_file_path_preserves_parent_directory ... ok
test tests::transmission_api_snapshot_estimates_scale_with_torrent_and_field_count ... ok
test tests::transmission_file_priority_classes_map_to_engine_priorities ... ok
test tests::transmission_file_completion_is_projected_per_file ... ok
test tests::transmission_magnet_link_formats_v1_and_v2 ... ok
test tests::transmission_lifecycle_seconds_project_registry_timestamps ... ok
test tests::transmission_numeric_projection_and_arguments_fail_closed ... ok
test tests::transmission_peer_rates_sum_native_snapshots ... ok
test tests::transmission_recheck_progress_projects_active_engine_job ... ok
test tests::transmission_seed_modes_reject_unknown_values ... ok
test tests::transmission_seed_modes_support_standard_camel_case_and_clear_overrides ... ok
test tests::transmission_session_limit_args_use_kib_wire_units ... ok
test tests::session_close_notifies_daemon_supervisor ... ok
test tests::transmission_group_methods_roundtrip_compat_state ... ok
test tests::transmission_json_rpc_20_batch_requests_are_supported ... ok
test tests::transmission_router_enforces_configured_token ... ok
test tests::transmission_idempotency_key_replays_mutation_and_rejects_reuse ... ok
test tests::transmission_session_id_handshake ... ok
test tests::transmission_torrent_add_uses_session_defaults_without_overriding_explicit_args ... ok
test tests::transmission_notification_subscriptions_roundtrip_state ... ok
test tests::transmission_json_rpc_20_uses_params_and_direct_result ... ok
test tests::transmission_torrent_get_rejects_malformed_projection_arguments ... ok
test tests::transmission_queue_stalled_settings_roundtrip_without_engine ... ok
test tests::transmission_session_access_control_projects_session_security_state ... ok
test tests::transmission_torrent_limits_accept_standard_camel_case_aliases ... ok
test tests::transmission_tracker_list_arg_accepts_common_shapes ... ok
test tests::transmission_sequential_from_piece_roundtrips_in_torrent_get ... ok
test tests::transmission_stats_and_location_are_supported ... ok
test tests::transmission_tracker_stats_project_persisted_engine_state ... ok
test tests::transmission_torrent_get_projects_v2_magnet_links ... ok
test tests::transmission_webseed_activity_projects_engine_snapshots ... ok
test tests::transmission_torrent_get_projects_registry ... ok
test tests::transmission_response_field_matrix_is_present ... ok
test tests::transmission_torrent_get_supports_table_format_and_recently_active ... ok
test tests::transmission_torrent_set_updates_labels_and_download_dir ... ok
test tests::transmission_torrent_group_assignment_roundtrips_in_torrent_get ... ok
test tests::transmission_torrent_set_limits_roundtrip_without_engine ... ok
test tests::transmission_session_set_persists_broad_compat_settings_without_engine ... ok
test tests::transmission_batches_are_bounded_before_response_allocation ... ok
test tests::transmission_common_mutators_are_accepted ... ok
test tests::transmission_snake_case_rpc_roundtrips_v41_shape ... ok

test result: ok. 41 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/rt_bencode-484708a8c024ba88)

running 13 tests
test decode::tests::decode_integer ... ok
test decode::tests::decode_dict ... ok
test decode::tests::decode_list ... ok
test decode::tests::decode_string ... ok
test decode::tests::nested_structures ... ok
test decode::tests::reject_excessive_node_count ... ok
test decode::tests::reject_leading_zero ... ok
test decode::tests::reject_negative_zero ... ok
test decode::tests::reject_trailing_data ... ok
test decode::tests::reject_unsorted_dict_keys ... ok
test encode::tests::roundtrip_bytes ... ok
test encode::tests::roundtrip_integer ... ok
test encode::tests::roundtrip_list ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/rt_config-934ff33309247b8c)

running 9 tests
test tests::db_path_fallback ... ok
test tests::default_config_is_valid ... ok
test tests::dht_port_fallback ... ok
test tests::invalid_config_is_rejected ... ok
test tests::metrics_torrent_identifier_opt_in_round_trips ... ok
test tests::parse_logging_toml ... ok
test tests::load_appends_newline_delimited_tokens_from_a_relative_secret_file ... ok
test tests::parse_toml_partial ... ok
test tests::config_file_size_is_bounded ... ok

test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/rt_db-77945faba9f9187c)

running 35 tests
test schema::tests::failed_migration_rolls_back_ddl_and_user_version ... ok
test settings_row::tests::setting_can_be_updated_inside_a_transaction ... ok
test settings_row::tests::setting_round_trip ... ok
test schema::tests::migrate_adds_session_event_level_index ... ok
test event_row::tests::append_session_event_rejects_invalid_json_payload ... ok
test schema::tests::wal_mode_enabled ... ok
test peer_ban_row::tests::peer_bans_round_trip ... ok
test schema::tests::migrate_creates_tables ... ok
test schema::tests::migrate_creates_durable_engine_backbone_tables ... ok
test schema::tests::migrate_idempotent ... ok
test storage_row::tests::mount_round_trip ... ok
test torrent_row::tests::get_not_found_errors ... ok
test torrent_row::tests::delete_nonexistent_returns_false ... ok
test schema::tests::migrate_adds_opaque_tracker_id_column ... ok
test event_row::tests::append_and_list_session_events ... ok
test storage_row::tests::storage_root_round_trip ... ok
test detail_row::tests::upsert_and_get_limits ... ok
test job_row::tests::upsert_and_get_job ... ok
test detail_row::tests::replace_and_list_files ... ok
test projection_row::tests::active_issue_is_idempotent_and_resolvable ... ok
test event_row::tests::append_and_list_job_events ... ok
test torrent_row::tests::corrupt_serialized_labels_fail_closed ... ok
test torrent_row::tests::upsert_and_get ... ok
test event_row::tests::list_session_events_filters_before_limit ... ok
test job_row::tests::upsert_updates_checkpoint ... ok
test detail_row::tests::replace_and_list_trackers ... ok
test job_row::tests::corrupt_job_checkpoint_json_fails_closed ... ok
test torrent_row::tests::category_definitions_are_durable_and_rename_cascades_labels ... ok
test job_row::tests::list_active_jobs_excludes_terminal ... ok
test detail_row::tests::tracker_health_groups_urls_and_deduplicates_per_torrent ... ok
test torrent_row::tests::delete_removes_record ... ok
test torrent_row::tests::list_all_returns_all ... ok
test torrent_row::tests::upsert_persists_normalized_labels ... ok
test torrent_row::tests::upsert_updates_state ... ok
test torrent_row::tests::upsert_replaces_normalized_tags ... ok

test result: ok. 35 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.15s

     Running unittests src/lib.rs (target/debug/deps/rt_dht-96c00e4b4ba11ad3)

running 19 tests
test krpc::tests::announce_peer_query_roundtrip ... ok
test krpc::tests::error_roundtrip ... ok
test krpc::tests::compact_peer_values_roundtrip_ipv4_and_ipv6 ... ok
test krpc::tests::ping_query_roundtrip ... ok
test krpc::tests::find_node_query_roundtrip ... ok
test krpc::tests::get_peers_query_roundtrip ... ok
test krpc::tests::rejects_bad_compact_lengths ... ok
test krpc::tests::response_rejects_multi_peer_value ... ok
test krpc::tests::response_roundtrip_with_nodes_values_and_token ... ok
test node_id::tests::distance_to_self_is_zero ... ok
test node_id::tests::display_is_40_hex_chars ... ok
test node_id::tests::known_distance ... ok
test node_id::tests::distance_xor_symmetric ... ok
test node_id::tests::leading_zeros_all_zero ... ok
test routing::tests::bucket_capacity ... ok
test routing::tests::insert_and_find ... ok
test routing::tests::closest_returns_k_nearest ... ok
test routing::tests::remove_node ... ok
test routing::tests::self_not_inserted ... ok

test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/rt_engine-2fcf545c3d10099f)

running 201 tests
test command::tests::engine_stats_accumulates_torrent_runtime_storage_counters ... ok
test command::tests::engine_stats_tracks_top_hot_torrent_memory_estimates ... ok
test dht_task::tests::announced_peer_cache_is_bounded_and_keeps_duplicates ... ok
test dht_task::tests::announce_peer_query_uses_configured_listen_port ... ok
test dht_task::tests::announce_peer_stores_ipv6_peer_and_get_peers_returns_it ... ok
test dht_task::tests::announce_peer_stores_peer_and_get_peers_returns_it ... ok
test dht_task::tests::dht_ingress_budget_bounds_each_ip_and_expires_windows ... ok
test dht_task::tests::closest_response_includes_known_nodes_and_token ... ok
test dht_task::tests::response_from_wrong_source_address_is_ignored ... ok
test dht_task::tests::lookup_restart_clears_previously_queried_nodes ... ok
test dht_task::tests::response_with_unknown_transaction_id_is_ignored ... ok
test dht_task::tests::get_peers_response_forwards_discovered_peers_to_torrent ... ok
test dht_task::tests::announce_peer_requires_matching_token ... ok
test dht_task::tests::runtime_stats_count_dht_owned_caches ... ok
test egress_policy::tests::default_policy_allows_only_http_webseeds ... ok
test egress_policy::tests::default_policy_denies_sensitive_address_ranges ... ok
test dht_task::tests::transaction_ids_are_nonzero_and_advance ... ok
test egress_policy::tests::default_policy_allows_public_tracker_schemes_only ... ok
test egress_policy::tests::policy_can_explicitly_allow_private_lan ... ok
test db_worker::tests::shutdown_drains_prior_work ... ok
test db_worker::tests::cancelled_queued_operation_is_not_applied ... ok
test engine::tests::decode_info_hash_bytes_accepts_v1_and_v2_lengths ... ok
test engine::tests::engine_handle_liveness_tracks_command_receiver ... ok
test egress_policy::tests::http_clients_are_reused_after_each_address_is_revalidated ... ok
test engine::tests::engine_handle_reports_peer_listener_health_separately ... ok
test engine::tests::incoming_utp_listener_flag_is_boolean_only ... ok
test engine::tests::label_normalization_trims_dedupes_and_drops_empty_values ... ok
test db_worker::tests::operations_are_serial_and_worker_survives_errors_and_panics ... ok
test engine::tests::metadata_placeholder_projection_preserves_trackers ... ok
test engine::tests::metadata_projection_preserves_files_trackers_and_privacy ... ok
test engine::tests::parse_info_hash_hex_rejects_invalid_input ... ok
test command::tests::hot_seeding_1k_memory_attribution_stays_under_cap ... ok
test dht_task::tests::prune_stale_outstanding_removes_expired_entries_only ... ok
test engine::tests::engine_handle_liveness_drops_on_actor_panic ... ok
test dht_task::tests::lookup_continues_to_unqueried_closer_nodes ... ok
test engine::tests::prune_empty_dirs_stops_at_root_and_keeps_nonempty_dirs ... ok
test engine::tests::pure_v2_metadata_projects_to_engine_and_db_shapes ... ok
test dht_task::tests::dht_ingress_budget_has_bounded_source_state ... ok
test db_worker::tests::sqlite_failure_crosses_boundary_and_worker_continues ... ok
test db_worker::tests::worker_owns_a_real_sqlite_connection_and_persists_ordered_work ... ok
test engine::tests::engine_handle_shutdown_aborts_an_actor_stuck_after_accepting_command ... ok
test dht_task::tests::announced_peer_global_cap_applies_to_existing_info_hashes ... ok
test engine::tests::add_torrent_rolls_back_registry_row_when_blob_write_fails ... ok
test engine::tests::add_torrent_rolls_back_registry_and_blob_when_db_persist_fails ... ok
test engine::tests::append_session_event_persists_payload ... ok
test engine::tests::add_peers_forwards_external_peers_to_torrent_task ... ok
test engine::tests::row_conversion_preserves_session_fields ... ok
test engine::tests::row_conversion_saturates_unsigned_values_at_sqlite_integer_limit ... ok
test engine::tests::add_v2_only_magnet_persists_metadata_placeholder ... ok
test engine::tests::complete_v2_only_magnet_persists_metadata_without_task ... ok
test engine::tests::storage_io_config_maps_native_storage_toml ... ok
test dht_task::tests::announced_peer_map_is_bounded_across_info_hashes ... ok
test engine::tests::network_features_persist_and_notify_running_torrents ... ok
test engine::tests::metadata_projection_rejects_unrepresentable_file_policy_index ... ok
test engine::tests::queue_order_moves_are_persisted ... ok
test engine::tests::finished_torrent_task_is_removed_and_marked_error ... ok
test engine::tests::storage_plan_resume_steps_are_sorted_unique_and_bounded ... ok
test engine::tests::global_limits_persist_to_settings_table ... ok
test engine::tests::load_persisted_torrents_restores_seeding_rows_as_dormant ... ok
test engine::tests::malformed_persisted_control_settings_fail_closed ... ok
test engine::tests::load_persisted_v2_rows_restore_taskless_registry_and_trackers ... ok
test engine::tests::failed_payload_cleanup_keeps_torrent_retryable ... ok
test engine::tests::recheck_job_helpers_persist_state_and_events ... ok
test engine::tests::engine_stats_include_registry_jobs_and_trackers ... ok
test engine::tests::recover_interrupted_jobs_pauses_running_work ... ok
test engine::tests::load_persisted_torrents_restores_paused_registry_as_dormant ... ok
test engine::tests::recheck_job_control_sends_torrent_commands_and_updates_state ... ok
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
test engine::tests::pause_torrent_pauses_taskless_pure_v2_recheck_job ... ok
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
test storage_jobs::tests::dispatcher_rejects_duplicate_active_job_ids ... ok
test engine::tests::private_magnet_completion_removes_provisional_dht_and_preserves_pause ... ok
test engine::tests::register_configured_storage_persists_root_and_mount ... ok
test engine::tests::recovered_storage_plan_reconciles_filesystem_ahead_of_checkpoint ... ok
test engine::tests::engine_start_owns_background_storage_supervisor_until_shutdown ... ok
test engine::tests::recovered_delete_job_finalizes_metadata_after_payload_cleanup ... ok
test engine::tests::pure_v2_recheck_verifies_file_roots_without_torrent_task ... ok
test engine::tests::reserve_memory_command_holds_and_releases_lease ... ok
test engine::tests::rename_file_and_folder_paths_update_metadata_projection ... ok
test engine::tests::shutdown_torrent_tasks_sends_shutdown_and_waits_for_task_exit ... ok
test storage_jobs::tests::worker_registration_guard_releases_on_unwind ... ok
test tier::tests::active_engine_work_stays_hot ... ok
test tier::tests::compact_piece_bitmap_roundtrips_and_rejects_tail_bits ... ok
test tier::tests::compact_piece_bitmap_supports_mutation_and_projection ... ok
test engine::tests::categories_survive_engine_restart_and_keep_torrent_labels_consistent ... ok
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
test torrent_task::tests::parses_ut_pex_added_ipv4_peers ... ok
test torrent_task::tests::parses_ut_pex_dropped_ipv4_and_ipv6_peers ... ok
test torrent_task::tests::peer_availability_reconcile_counts_only_transitions ... ok
test torrent_task::tests::peer_event_channel_capacity_is_bounded_by_global_peer_budget ... ok
test engine::tests::remove_torrent_queues_payload_cleanup_outside_engine_actor ... ok
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
test engine::tests::startup_reconciles_missing_rows_and_quarantines_orphan_projections ... ok
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
test engine::tests::storage_operation_admission_rejects_active_same_torrent_job ... ok
test engine::tests::storage_move_db_failure_keeps_destination_live_and_commit_pending ... ok
test torrent_task::tests::upload_block_reservation_uses_peer_buffer_governor_class ... ok
test torrent_task::tests::upload_context_piece_map_is_shared_not_deep_cloned_per_peer ... ok
test torrent_task::tests::webseed_block_url_accepts_direct_file_and_base_url ... ok
test torrent_task::tests::webseed_block_url_expands_single_file_directory_prefix ... ok
test torrent_task::tests::webseed_body_reservation_uses_webseed_governor_class ... ok
test torrent_task::tests::webseed_retry_delay_is_exponential_and_bounded ... ok
test tracker_runtime::tests::tracker_worker_budget_is_explicit_and_bounded ... ok
test torrent_task::tests::upload_block_reads_across_many_file_regions ... ok
test engine::tests::storage_plan_execution_rejects_missing_server_roots ... ok
test engine::tests::subsystem_health_reports_dead_dependency_seams ... ok
test engine::tests::torrent_blob_export_preserves_raw_metainfo_bytes ... ok
test engine::tests::storage_plan_execution_uses_persisted_roots_and_fails_closed ... ok
test engine::tests::storage_root_projection_reports_capacity_and_root_errors ... ok
test engine::tests::torrent_diagnostic_explains_paused_private_tracker_gap ... ok
test engine::tests::user_agent_update_persists_and_changes_runtime_clients ... ok
test engine::tests::update_torrent_limits_persists_and_reads_back ... ok
test engine::tests::storage_plan_jobs_checkpoint_completed_steps ... ok
test engine::tests::update_torrent_limits_notifies_running_torrent_task ... ok
test engine::tests::update_torrent_trackers_persists_summary_and_detail_rows ... ok
test storage_jobs::tests::closed_worker_releases_end_to_end_inflight_registration ... ok
test engine::tests::update_save_path_moves_existing_payload_through_storage_plan ... ok
test storage_jobs::tests::move_persistence_failure_remains_commit_pending ... ok
test storage_jobs::tests::queue_is_bounded_and_control_is_shared ... ok
test storage_jobs::tests::injected_worker_panic_is_contained_and_next_job_runs ... ok
test storage_jobs::tests::terminal_persistence_records_partial_progress ... ok
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

test result: ok. 201 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.27s

     Running unittests src/lib.rs (target/debug/deps/rt_fastresume-9798d35a62cea664)

running 18 tests
test state::tests::durability_barrier_clears_dirty_piece_watermark ... ok
test state::tests::is_complete_only_when_all_valid ... ok
test state::tests::file_hint_change_invalidates_pieces ... ok
test state::tests::unclean_without_watermark_requires_full_recheck ... ok
test state::tests::unclean_watermark_downgrades_only_dirty_valid_pieces ... ok
test state::tests::new_state_all_unknown ... ok
test state::tests::validate_rejects_piece_count_mismatch ... ok
test state::tests::validate_succeeds_for_matching_state ... ok
test state::tests::validate_rejects_infohash_mismatch ... ok
test state::tests::validate_succeeds_for_v2_infohash ... ok
test store::tests::bounded_read_rejects_oversized_file ... ok
test store::tests::delete_nonexistent_ok ... ok
test store::tests::load_not_found ... ok
test store::tests::rejects_path_like_infohash ... ok
test store::tests::delete_existing ... ok
test store::tests::atomic_write_no_partial ... ok
test store::tests::save_and_load_roundtrip ... ok
test store::tests::validate_loaded_state ... ok

test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/rt_hash-06dff86c007458cf)

running 5 tests
test tests::block_hash_deterministic ... ok
test tests::infohash_v2_truncated_length ... ok
test tests::merkle_root_pads_to_power_of_two ... ok
test tests::merkle_root_single_leaf ... ok
test tests::merkle_root_two_leaves ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/rt_jobs-437d0e3281259f5f)

running 14 tests
test job::tests::cancellable_kinds ... ok
test job::tests::dry_run_kinds ... ok
test job::tests::job_cancel ... ok
test job::tests::job_failure ... ok
test job::tests::job_lifecycle ... ok
test job::tests::progress_fraction ... ok
test job::tests::zero_total_fraction ... ok
test queue::tests::active_jobs_excludes_terminal ... ok
test queue::tests::cancel_already_terminal ... ok
test queue::tests::cancel_active_job ... ok
test queue::tests::cancel_not_found ... ok
test queue::tests::destructive_requires_dry_run ... ok
test queue::tests::dry_run_job_enqueued_ok ... ok
test queue::tests::enqueue_and_retrieve ... ok

test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/rt_logging-b3d9afb0ff9b4b77)

running 3 tests
test tests::correlation_ids_are_bounded_and_header_safe ... ok
test tests::profile_filters_have_expected_escalation ... ok
test tests::precedence_prefers_env_then_filter_then_profile_then_legacy ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/rt_metainfo-c05cf356c9afdc98)

running 38 tests
test magnet::tests::parses_base32_btih ... ok
test magnet::tests::parses_lowercase_base32_btih ... ok
test magnet::tests::parses_v2_btmh_magnet ... ok
test magnet::tests::rejects_missing_exact_topic ... ok
test magnet::tests::rejects_invalid_base32_btih ... ok
test magnet::tests::parses_v1_magnet_with_name_and_trackers ... ok
test parse::tests::accepts_non_power_of_two_piece_length ... ok
test parse::tests::parse_multi_file ... ok
test parse::tests::infohash_stable_across_parses ... ok
test parse::tests::multi_file_drops_vestigial_empty_leading_path_component ... ok
test parse::tests::parse_private_flag ... ok
test parse::tests::parse_multi_file_marks_bep47_padding_files ... ok
test parse::tests::parse_single_file ... ok
test parse::tests::parse_v2_torrent ... ok
test parse::tests::parse_top_level_comment_creator_and_creation_date ... ok
test parse::tests::parse_webseeds_accepts_string_and_list_forms ... ok
test parse::tests::reject_file_tree_leaf_with_sibling_entries ... ok
test parse::tests::reject_files_field_with_wrong_type ... ok
test parse::tests::reject_i64_min_piece_length ... ok
test parse::tests::reject_future_metainfo_version_before_v1_fallback ... ok
test parse::tests::reject_invalid_pieces_length ... ok
test parse::tests::reject_negative_metainfo_version ... ok
test parse::tests::reject_negative_multi_file_length ... ok
test parse::tests::reject_negative_piece_length ... ok
test parse::tests::reject_negative_single_file_length ... ok
test parse::tests::reject_negative_v2_file_length ... ok
test parse::tests::reject_oversized_top_level_tracker_url ... ok
test parse::tests::reject_path_traversal ... ok
test parse::tests::reject_v2_piece_length_below_protocol_minimum ... ok
test parse::tests::reject_zero_piece_length ... ok
test parse::tests::v2_empty_file_may_omit_pieces_root ... ok
test parse::tests::torrent_meta_helpers ... ok
test parse::tests::v2_infohash_uses_sha256 ... ok
test parse::tests::v2_leaf_marks_bep47_padding_files ... ok
test parse::tests::v2_single_file_nested_in_subdirectory_preserves_tree_path ... ok
test parse::tests::v2_multi_file_preserves_tree_root_without_advisory_name ... ok
test parse::tests::v2_single_file_uses_rootless_file_tree_path ... ok
test parse::tests::zero_length_file_accepted ... ok

test result: ok. 38 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/rt_metrics-29823a042442274b)

running 6 tests
test counter::tests::counter_inc_and_get ... ok
test counter::tests::counter_reset ... ok
test counter::tests::metrics_snapshot ... ok
test resource::tests::class_and_global_caps_are_enforced ... ok
test resource::tests::leases_release_on_drop ... ok
test resource::tests::pressure_transitions_are_deterministic ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/scale.rs (target/debug/deps/scale-1a5f2332c6dad04f)

running 19 tests
test recheck_does_not_starve_seeding_peer_reads ... ok
test crash_watermark_bounds_restart_recheck_to_dirty_pieces ... ok
test tracker_restart_storm_15k_is_spread_by_jitter ... ok
test storage_peer_read_readahead_reduces_backend_reads_for_adjacent_blocks ... ok
test storage_recheck_hashing_reports_scheduler_result_without_runtime_stall ... ok
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

test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.73s

     Running unittests src/lib.rs (target/debug/deps/rt_migrate-6fec678f6fedc57d)

running 51 tests
test export::tests::format_parsing_accepts_aliases ... ok
test tests::auxiliary_classification_ignores_hash_coincidences_in_filenames ... ok
test tests::aggregate_json_resume_matches_base32_info_hash_entries ... ok
test tests::bencoded_file_selection_imports_to_native_file_rows ... ok
test tests::bencoded_file_progress_imports_to_native_file_rows ... ok
test tests::bencoded_tracker_activity_imports_to_native_tracker_rows ... ok
test tests::file_hints_do_not_trust_final_symlinks ... ok
test tests::bencoded_lifecycle_state_imports_to_native_torrent_row ... ok
test tests::biglybt_downloads_config_matches_hex_entries ... ok
test tests::broad_sources_are_scannable_metadata_first ... ok
test tests::dry_run_preserves_auxiliary_client_artifacts_separately ... ok
test tests::fastresume_apply_persists_imported_state_and_summary ... ok
test tests::json_file_progress_imports_to_native_file_rows ... ok
test tests::json_tracker_activity_imports_to_native_tracker_rows ... ok
test tests::json_lifecycle_state_imports_to_native_torrent_row ... ok
test tests::json_file_selection_imports_to_native_file_rows ... ok
test tests::padding_files_are_never_marked_wanted ... ok
test tests::oversized_resume_sidecar_is_skipped_with_warning ... ok
test tests::qbit_libtorrent2_resume_unpacks_bit_packed_pieces_field ... ok
test tests::qbit_dry_run_preserves_resume_metadata ... ok
test tests::recursive_scan_does_not_follow_directory_symlinks ... ok
test tests::path_remap_updates_db_rows_and_file_hint_trust ... ok
test tests::rtorrent_dry_run_reports_missing_resume ... ok
test tests::qbit_libtorrent_resume_imports_piece_state ... ok
test tests::path_remap_uses_longest_matching_prefix ... ok
test tests::rtorrent_multi_file_directory_already_includes_torrent_name ... ok
test tests::rtorrent_pairs_hash_torrent_rtorrent_sidecar ... ok
test tests::rtorrent_multi_file_directory_renamed_by_external_tool_stays_safe_not_silently_broken ... ok
test tests::tixati_proprietary_state_stays_verification_first ... ok
test tests::utorrent_bitfield_resume_imports_piece_state_under_trust_hints ... ok
test tests::utorrent_resume_dat_matches_raw_info_hash_entries ... ok
test tests::short_piece_state_is_padded_and_reported ... ok
test tests::rtorrent_complete_resume_synthesizes_seed_piece_state ... ok
test tests::transmission_dry_run_reads_bencoded_resume ... ok
test tests::rtorrent_single_file_directory_is_left_unchanged ... ok
test tests::require_verification_downgrades_imported_valid_pieces ... ok
test tests::import_source_matrix_preserves_common_json_resume_fields ... ok
test tests::partial_piece_blocks_are_sorted_deduped_and_bounded ... ok
test tests::native_import_applies_db_and_fastresume_together ... ok
test tests::import_plan_applies_native_db_rows ... ok
test export::tests::rtorrent_export_complete_is_recheck_free ... ok
test export::tests::generic_export_copies_torrent_and_manifest ... ok
test export::tests::missing_blob_is_skipped_not_fatal ... ok
test export::tests::transmission_export_round_trips ... ok
test export::tests::oversized_blob_is_skipped_before_reading_contents ... ok
test export::tests::rtorrent_export_partial_is_metadata_only ... ok
test export::tests::libtorrent_export_round_trips_through_qbittorrent_importer ... ok
test export::tests::malformed_database_hash_is_skipped_before_path_join ... ok
test export::tests::utorrent_and_biglybt_aggregates_round_trip ... ok
test tests::bencoded_import_source_matrix_preserves_client_specific_aliases ... ok
test tests::native_apply_matrix_persists_common_resume_fields_for_all_sources ... ok

test result: ok. 51 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.09s

     Running tests/round_trip_matrix.rs (target/debug/deps/round_trip_matrix-066d067b020104bf)

running 7 tests
test import_matrix_complete_and_partial_isos ... ok
test production_shape_directory_renamed_by_external_tool_stays_safe_metadata_only ... ok
test production_shape_directory_equals_content_folder_bytes_preserved_and_trusted ... ok
test production_shape_bep47_padding_file_not_wanted_real_files_trusted ... ok
test generic_export_is_universal_exit ... ok
test round_trip_matrix_preserves_state ... ok
test export_matrix_fidelity_and_layout ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s

     Running tests/scale.rs (target/debug/deps/scale-fab0df4943d55749)

running 1 test
test qbit_15k_dry_run_import_is_certified ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.92s

     Running unittests src/lib.rs (target/debug/deps/rt_path-29f7173d9f47e207)

running 12 tests
test path::tests::accept_simple_path ... ok
test path::tests::reject_absolute_unix ... ok
test path::tests::reject_absolute_windows_drive ... ok
test path::tests::reject_nul_byte ... ok
test path::tests::reject_empty_component ... ok
test path::tests::reject_oversized_component ... ok
test path::tests::reject_embedded_separators_and_current_directory ... ok
test path::tests::reject_parent_traversal ... ok
test path::tests::reject_empty_path ... ok
test path::tests::reject_windows_reserved_nul ... ok
test path::tests::resolve_against_root ... ok
test path::tests::windows_reserved_allowed_when_disabled ... ok

test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/rt_peer_manager-c503ec425b37ad0e)

running 13 tests
test choker::tests::fewer_peers_than_slots_all_unchoked ... ok
test choker::tests::not_interested_always_choked ... ok
test choker::tests::optimistic_slot_assigned ... ok
test peer::tests::peer_id_unique ... ok
test peer::tests::new_peer_starts_choked ... ok
test choker::tests::top_n_peers_unchoked ... ok
test peer::tests::upload_rate_ema ... ok
test pool::tests::add_and_remove ... ok
test pool::tests::duplicate_address_rejected ... ok
test pool::tests::is_full_flag ... ok
test pool::tests::known_addresses_excludes_removed ... ok
test pool::tests::pool_full_rejected ... ok
test pool::tests::remove_unknown_errors ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/rt_peer_wire-658cefcd01b45b5f)

running 31 tests
test codec::tests::codec_partial_read_returns_none ... ok
test codec::tests::codec_multiple_messages_in_buffer ... ok
test codec::tests::codec_roundtrip_choke ... ok
test codec::tests::codec_roundtrip_have ... ok
test extension::tests::ut_metadata_data_roundtrip ... ok
test extension::tests::extension_handshake_roundtrip ... ok
test extension::tests::extension_handshake_rejects_deeply_nested_bencode ... ok
test extension::tests::ut_metadata_rejects_deeply_nested_bencode_without_deep_recursion ... ok
test extension::tests::ut_metadata_reject_roundtrip ... ok
test handshake::tests::extension_flag_roundtrip ... ok
test extension::tests::ut_metadata_request_roundtrip ... ok
test handshake::tests::roundtrip ... ok
test handshake::tests::wrong_protocol_rejected ... ok
test message::tests::bitfield_roundtrip ... ok
test message::tests::cancel_roundtrip ... ok
test message::tests::choke_roundtrip ... ok
test message::tests::extended_rejects_missing_extension_id ... ok
test message::tests::extended_roundtrip ... ok
test message::tests::have_roundtrip ... ok
test message::tests::interested_roundtrip ... ok
test message::tests::keepalive_roundtrip ... ok
test message::tests::message_length_prefix_correct ... ok
test message::tests::not_interested_roundtrip ... ok
test message::tests::piece_rejects_oversized_block ... ok
test message::tests::piece_roundtrip ... ok
test message::tests::request_rejects_oversized_block ... ok
test message::tests::request_rejects_zero_length ... ok
test message::tests::request_roundtrip ... ok
test message::tests::unchoke_roundtrip ... ok
test message::tests::unknown_id_rejected ... ok
test codec::tests::codec_rejects_oversized_message ... ok

test result: ok. 31 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/rt_piece_map-11782542ad852781)

running 16 tests
test map::tests::last_piece_shorter ... ok
test map::tests::piece_out_of_range ... ok
test map::tests::content_range_last_piece ... ok
test map::tests::piece_spanning_two_files ... ok
test map::tests::rejects_non_contiguous_file_spans ... ok
test map::tests::rejects_piece_count_overflow ... ok
test map::tests::request_out_of_bounds ... ok
test map::tests::single_file_piece_count ... ok
test map::tests::request_too_large ... ok
test map::tests::single_file_region ... ok
test map::tests::zero_length_file_skipped ... ok
test map::tests::valid_request_resolves_file_region ... ok
test map::tests::valid_request_can_span_many_files ... ok
test map::tests::zero_length_request_rejected ... ok
test map::tests::property::piece_lengths_sum_to_total ... ok
test map::tests::property::all_bytes_covered ... ok

test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

     Running unittests src/lib.rs (target/debug/deps/rt_piece_picker-e210876713628994)

running 32 tests
test availability::tests::add_bitfield_increments_counts ... ok
test availability::tests::add_have_increments_single ... ok
test availability::tests::bitfield_out_of_bounds_ignored ... ok
test availability::tests::rarest_first_orders_ascending ... ok
test availability::tests::rarest_first_respects_want_filter ... ok
test availability::tests::remove_does_not_underflow ... ok
test availability::tests::remove_bitfield_decrements ... ok
test availability::tests::remove_have_decrements_single_piece ... ok
test picker::tests::bytes_left_sums_wanted_piece_lengths ... ok
test picker::tests::bytes_left_subtracts_partially_received_blocks ... ok
test picker::tests::disabled_piece_is_not_requested_or_advertised_complete ... ok
test picker::tests::cancel_request_makes_block_available_again ... ok
test picker::tests::endgame_does_not_duplicate_same_block_to_same_peer ... ok
test picker::tests::endgame_duplicates_outstanding_blocks_after_fresh_work_is_exhausted ... ok
test picker::tests::endgame_pick_order_respects_sequential_from_piece ... ok
test picker::tests::have_pieces_is_inverse_of_wanted ... ok
test picker::tests::last_piece_may_be_shorter ... ok
test picker::tests::invalid_priority_piece_is_ignored ... ok
test picker::tests::mark_and_reject_update_recheck_accounting ... ok
test picker::tests::mark_have_removes_from_wanted ... ok
test picker::tests::no_pick_when_peer_has_nothing ... ok
test picker::tests::picks_block_from_single_piece ... ok
test picker::tests::picks_second_block_after_first ... ok
test picker::tests::partial_piece_snapshot_restores_received_blocks ... ok
test picker::tests::priority_pieces_selected_first ... ok
test picker::tests::reject_piece_makes_completed_piece_wanted_again ... ok
test picker::tests::piece_complete_after_all_blocks_received ... ok
test picker::tests::seed_picker_does_not_require_peer_availability ... ok
test picker::tests::seed_picker_respects_sequential_from_piece ... ok
test picker::tests::reset_outstanding_requests_makes_blocks_available_again ... ok
test picker::tests::sequential_from_piece_starts_at_configured_piece ... ok
test picker::tests::sequential_mode_picks_lowest_piece_before_rarest ... ok

test result: ok. 32 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/rt_session-84c821930ae080df)

running 28 tests
test registry::tests::by_state_filters ... ok
test registry::tests::aggregate_stats_follow_mutation_and_removal ... ok
test registry::tests::add_and_get ... ok
test registry::tests::changes_since_reports_mutations_and_removals ... ok
test registry::tests::dormant_entries_preserve_error_and_tracker_messages ... ok
test registry::tests::dormant_entries_are_compact_until_mutated ... ok
test registry::tests::duplicate_rejected ... ok
test registry::tests::len_tracks_count ... ok
test registry::tests::no_op_mutable_borrow_does_not_advance_revision_or_journal ... ok
test registry::tests::peer_bans_are_shared_without_affecting_torrent_counts ... ok
test registry::tests::remove_missing_errors ... ok
test registry::tests::revision_changes_on_add_mutate_and_remove ... ok
test registry::tests::valid_hex_infohashes_are_canonical_and_case_insensitive ... ok
test registry::tests::remove_returns_entry ... ok
test state::tests::can_pause_active_states ... ok
test state::tests::can_start_stopped ... ok
test state::tests::display_matches_serde ... ok
test state::tests::error_is_terminal ... ok
test torrent::tests::checking_to_seeding ... ok
test torrent::tests::invalid_transition_errors ... ok
test torrent::tests::ratio_calculation ... ok
test torrent::tests::ratio_zero_when_no_download ... ok
test torrent::tests::same_state_transition_is_idempotent ... ok
test torrent::tests::seeding_to_paused ... ok
test torrent::tests::seeding_torrent_can_be_rechecked ... ok
test torrent::tests::set_error_sets_state_and_message ... ok
test torrent::tests::starts_stopped ... ok
test torrent::tests::stopped_to_checking ... ok

test result: ok. 28 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/rt_storage-baaf688315c65377)

running 130 tests
test backend::tests::backend_request_parses_user_values ... ok
test backend::tests::fixed_buffer_strategy_names_are_stable_for_metrics ... ok
test backend::tests::uring_fixed_buffer_registration_budget_stays_below_common_memlock_limit ... ok
test backend::tests::forcing_pread_selects_pread ... ok
test backend::tests::pread_backend_queue_fails_closed_when_full ... ok
test device::tests::cow_fs_detection_covers_common_filesystems ... ok
test device::tests::block_device_profile_uses_name_and_rotational_flag ... ok
test device::tests::mountinfo_read_is_bounded ... ok
test device::tests::mountinfo_parser_decodes_paths_and_finds_longest_prefix ... ok
test device::tests::mount_source_subpath_suffix_is_ignored ... ok
test elevator::tests::hdd_budget_holds_nonurgent_ops_until_elapsed ... ok
test backend::tests::pwrite_then_pread_roundtrip ... ok
test elevator::tests::limited_drain_uses_class_weights_and_keeps_remainder_pending ... ok
test backend::tests::pread_past_eof_errors ... ok
test backend::tests::selected_backend_roundtrip ... ok
test device::tests::network_topology_uses_mount_source_as_device_id ... ok
test device::tests::sysfs_block_device_name_prefers_parent_block_device ... ok
test device::tests::topology_reports_fs_profile_and_cow ... ok
test elevator::tests::choke_critical_and_foreground_bypass_budget ... ok
test elevator::tests::nvme_degenerates_to_zero_budget ... ok
test elevator::tests::writes_are_ordered_but_not_coalesced ... ok
test elevator::tests::ready_reads_are_offset_sorted_and_coalesced_per_file ... ok
test elevator::tests::zero_limited_drain_leaves_ready_ops_pending ... ok
test backend::tests::uring_probe_is_diagnostic_not_panic ... ok
test fd_limit::tests::capacity_respects_floor ... ok
test fd_limit::tests::capacity_scales_with_limit ... ok
test backend::tests::uring_strategy_reports_frame_pool_slots_only_with_registered_buffers ... ok
test fd_limit::tests::raise_returns_nonzero ... ok
test frame::tests::acquire_release_roundtrips_capacity ... ok
test frame::tests::into_bytes_releases_charge_without_copying_payload ... ok
test frame::tests::registered_slot_frame_keeps_charge_until_drop ... ok
test frame::tests::oversize_is_exact_and_counted ... ok
test frame::tests::buffers_are_reused_within_class ... ok
test frame::tests::cap_enforced_with_backpressure ... ok
test handle_cache::tests::ancestor_symlink_is_rejected_before_opening_the_file ... ok
test handle_cache::tests::reuses_same_handle_for_repeated_opens ... ok
test open::tests::limited_read_rejects_oversized_runtime_file ... ok
test handle_cache::tests::final_component_symlink_is_rejected ... ok
test handle_cache::tests::missing_file_read_errors_and_is_not_cached ... ok
test plan::tests::delete_requires_prior_dry_run_approval ... ok
test backend::tests::uring_request_has_clean_probe_fallback ... ok
test plan::tests::checkpoint_failure_leaves_committed_step_resumable ... ok
test handle_cache::tests::lru_evicts_least_recently_used ... ok
test plan::tests::descriptor_anchored_execution_rejects_an_ancestor_symlink ... ok
test plan::tests::cleanup_plan_is_idempotent_and_prunes_only_empty_directories ... ok
test plan::tests::execute_delete_plan_removes_directory_tree_after_approval ... ok
test plan::tests::controlled_copy_aborts_mid_step_and_rolls_back_staging ... ok
test plan::tests::execute_import_plan_links_or_copies_without_removing_source ... ok
test plan::tests::execute_import_copy_failure_does_not_leave_final_destination ... ok
test plan::tests::execute_move_plan_rejects_symlink_source_before_rename ... ok
test plan::tests::execute_import_plan_rejects_symlink_source_before_hardlink ... ok
test plan::tests::execute_plan_reports_rollback_step_failure_in_error_message ... ok
test plan::tests::execute_plan_under_roots_rejects_delete_escape ... ok
test plan::tests::execute_plan_under_roots_rejects_destination_escape_before_copy ... ok
test plan::tests::execute_plan_under_roots_rejects_parent_dir_components ... ok
test handle_cache::tests::read_and_write_handles_are_distinct ... ok
test plan::tests::execute_move_plan_renames_without_overwrite ... ok
test plan::tests::execute_plan_under_roots_allows_confined_move ... ok
test plan::tests::execute_copy_verify_plan_rejects_symlink_source_file ... ok
test plan::tests::import_copy_plan_stages_before_final_rename ... ok
test plan::tests::import_plan_detects_existing_destination ... ok
test plan::tests::execute_copy_verify_plan_rejects_nested_symlink_entry ... ok
test plan::tests::failed_checkpoint_can_resume_without_repeating_filesystem_mutation ... ok
test plan::tests::move_plan_copy_verify_path_deletes_source_after_verified_rename ... ok
test plan::tests::plan_delete_treats_broken_symlink_as_existing_source ... ok
test plan::tests::plan_import_treats_broken_destination_symlink_as_existing ... ok
test plan::tests::execute_delete_plan_removes_symlink_without_following_target ... ok
test plan::tests::move_plan_copy_verify_path_removes_source_directory_after_verified_rename ... ok
test plan::tests::reconcile_infers_copy_when_following_rename_is_already_committed ... ok
test plan::tests::execute_copy_verify_plan_rolls_back_staged_file_on_short_copy ... ok
test plan::tests::reconcile_rejects_corrupt_staging_copy ... ok
test runtime::tests::backend_short_io_maps_to_storage_error ... ok
test plan::tests::move_plan_reports_conflict_and_capacity ... ok
test runtime::tests::backend_would_block_maps_to_queue_full ... ok
test plan::tests::retrying_a_completed_plan_is_idempotent ... ok
test scheduler::tests::auto_preallocation_policy_uses_full_only_for_non_cow_hdd ... ok
test plan::tests::repeating_a_completed_copy_plan_is_idempotent ... ok
test plan::tests::execute_copy_verify_plan_copies_directory_tree_and_verifies_bytes ... ok
test plan::tests::move_plan_uses_rename_on_same_filesystem ... ok
test plan::tests::verify_content_matches_detects_bit_flip_despite_matching_length ... ok
test scheduler::tests::blocking_pool_full_queue_fails_closed ... ok
test plan::tests::reconcile_detects_rename_committed_before_checkpoint ... ok
test scheduler::tests::peer_read_elevator_full_queue_fails_closed ... ok
test scheduler::tests::acquire_and_release_recheck ... ok
test scheduler::tests::queued_disk_bytes_track_active_blocking_job_payload ... ok
test scheduler::tests::ssd_has_higher_concurrency ... ok
test scheduler::tests::scheduler_new_resolves_auto_to_sparse_without_path_topology ... ok
test scheduler::tests::full_mount_queue_fails_closed ... ok
test runtime::tests::missing_file_maps_to_not_found ... ok
test runtime::tests::global_read_write_roundtrip ... ok
test scheduler::tests::owned_read_returns_pooled_frame_for_exact_backend_read ... ok
test scheduler::tests::file_pool_records_hits_and_evictions ... ok
test scheduler::tests::compatibility_read_still_returns_bytes ... ok
test scheduler::tests::short_positioned_read_maps_to_storage_error ... ok
test scheduler::tests::peer_read_readahead_cache_is_config_bounded ... ok
test scheduler::tests::queued_disk_governor_denies_before_enqueue ... ok
test scheduler::tests::peer_read_not_starved_by_recheck ... ok
test scheduler::tests::read_nonexistent_file_does_not_create ... ok
test scheduler::tests::prepare_file_does_not_create_through_an_ancestor_symlink ... ok
test scheduler::tests::hdd_peer_read_elevator_dispatches_after_quiet_slice ... ok
test scheduler::tests::read_and_write_roundtrip ... ok
test scheduler::tests::sparse_prepare_creates_parent_once ... ok
test scheduler::tests::concurrent_positioned_writes_do_not_share_cursor ... ok
test scheduler::tests::write_does_not_create_by_default ... ok
test scheduler::tests::prepare_file_refuses_to_shrink_existing_data ... ok
test scheduler::tests::strict_write_sync_is_counted_and_not_left_dirty ... ok
test scheduler::tests::peer_read_readahead_cache_returns_exact_requested_bytes ... ok
test scheduler::tests::stats_track_io_sync_and_hash_work ... ok
test scheduler::tests::schedulers_on_same_device_share_global_queue ... ok
test scheduler::tests::runtime_file_pool_rejects_an_ancestor_symlink ... ok
test verify::tests::v2_file_verify_rejects_wrong_file_root ... ok
test verify::tests::v2_file_verify_accepts_matching_file_root ... ok
test verify::tests::verify_invalid_piece ... ok
test scheduler::tests::peer_read_readahead_cache_is_invalidated_by_writes ... ok
test verify::tests::verify_range_resumable ... ok
test verify::tests::verify_all_reports_per_piece ... ok
test verify::tests::verify_missing_file ... ok
test verify::tests::verify_range_returns_empty_for_reversed_or_out_of_range_bounds ... ok
test verify::tests::verify_valid_piece ... ok
test verify::tests::verify_truncated_sparse_file_is_missing_not_zero_filled ... ok
test verify::tests::v2_file_verify_hashes_sparse_holes_as_zeroes ... ok
test scheduler::tests::hdd_peer_read_elevator_batches_shuffled_adjacent_reads ... ok
test backend::tests::forced_uring_roundtrip_when_kernel_supports_it ... ok
test scheduler::tests::prepare_file_extends_without_disturbing_existing_bytes ... ok
test verify::tests::v2_file_verify_accepts_empty_file_without_root ... ok
test scheduler::tests::peer_read_readahead_cache_can_be_disabled ... ok
test scheduler::tests::sync_all_open_files_syncs_dirty_paths_after_fd_eviction ... ok
test scheduler::tests::large_peer_and_recheck_reads_emit_page_cache_advice ... ok
test verify::tests::verify_sparse_piece_hashes_holes_as_zeroes ... ok
test handle_cache::tests::idle_sweep_closes_stale_handles ... ok

test result: ok. 130 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s

     Running tests/storage_move_import_hardware.rs (target/debug/deps/storage_move_import_hardware-17ba0afe31e86116)

running 1 test
test move_import_delete_executor_runs_on_configured_storage_root ... ignored, real-root move/import certification; set TNG_STORAGE_MOVE_IMPORT_ROOT

test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/storage_real_device.rs (target/debug/deps/storage_real_device-42f84e9186ce253d)

running 7 tests
test backend_selection_roundtrip_reports_capabilities ... ignored, real-device storage benchmark; run explicitly with --ignored --nocapture
test backend_stream_roundtrip_reports_throughput ... ignored, real-device storage benchmark; run explicitly with --ignored --nocapture
test hdd_peer_read_elevator_reduces_backend_reads_on_shuffled_adjacent_blocks ... ignored, real-device storage benchmark; run explicitly with --ignored --nocapture
test peer_read_readahead_reduces_backend_reads_on_adjacent_blocks ... ignored, real-device storage benchmark; run explicitly with --ignored --nocapture
test recheck_range_reports_runtime_progress ... ignored, real-device storage benchmark; run explicitly with --ignored --nocapture
test repeated_reads_reuse_one_open_file_handle ... ignored, real-device storage benchmark; run explicitly with --ignored --nocapture
test shuffled_peer_read_baseline_reports_current_scheduler_throughput ... ignored, real-device storage benchmark; run explicitly with --ignored --nocapture

test result: ok. 0 passed; 0 failed; 7 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/rt_testkit-6e0a546d8cb6e462)

running 5 tests
test tests::synthetic_rows_are_stable_and_scale_shaped ... ok
test tests::memory_db_is_migrated ... ok
test tests::seed_torrents_inserts_deterministic_rows ... ok
test tests::synthetic_dataset_writes_to_db ... ok
test tests::scale_matrix_includes_certification_sizes ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s

     Running unittests src/lib.rs (target/debug/deps/rt_tracker-dc0224b99b24da53)

running 66 tests
test backoff::tests::backoff_increases_with_attempts ... ok
test backoff::tests::backoff_capped_at_max ... ok
test backoff::tests::backoff_reset_returns_to_base ... ok
test peer::tests::empty_compact_peers ... ok
test backoff::tests::jitter_stays_positive ... ok
test peer::tests::parse_single_compact_v4 ... ok
test peer::tests::reject_invalid_compact_length ... ok
test policy::tests::private_only_allows_tracker_peers ... ok
test backoff::tests::jitter_spreads_announces ... ok
test peer::tests::parse_compact_v6 ... ok
test peer::tests::parse_multiple_compact_v4 ... ok
test policy::tests::private_disables_dht_pex_lsd ... ok
test policy::tests::public_allows_all ... ok
test policy::tests::public_allows_any_peer_source ... ok
test request::tests::invalid_tracker_url ... ok
test request::tests::empty_event_not_in_query ... ok
test request::tests::info_hash_url_encoded ... ok
test request::tests::peer_id_is_not_double_encoded ... ok
test request::tests::preserves_existing_query ... ok
test request::tests::private_tracker_accounting_values_are_exact_in_query ... ok
test request::tests::scrape_url_rejects_non_announce_paths ... ok
test request::tests::scrape_url_accepts_v2_info_hash ... ok
test request::tests::scrape_url_preserves_existing_query_and_hash_encoding ... ok
test request::tests::scrape_url_rewrites_announce_path ... ok
test request::tests::stopped_event_in_query ... ok
test request::tests::tracker_id_is_echoed_only_when_provided ... ok
test request::tests::url_contains_required_fields ... ok
test request::tests::v2_info_hash_url_encoded_for_http_announce ... ok
test response::tests::parse_compact_ipv6_peers_field ... ok
test response::tests::parse_compact_response ... ok
test response::tests::parse_empty_peers ... ok
test response::tests::parse_failure_reason ... ok
test response::tests::parse_missing_interval ... ok
test response::tests::parse_noncompact_ipv6_peer ... ok
test response::tests::parse_preserves_opaque_tracker_id_bytes ... ok
test response::tests::parse_rejects_interval_overflowing_u32 ... ok
test response::tests::parse_rejects_negative_interval ... ok
test response::tests::parse_scrape_rejects_missing_info_hash ... ok
test response::tests::parse_scrape_rejects_negative_counts ... ok
test response::tests::parse_scrape_stats_for_info_hash ... ok
test response::tests::parse_treats_out_of_range_optional_stats_as_absent ... ok
test response::tests::parse_warning_message ... ok
test response::tests::reject_noncompact_peer_port_out_of_range ... ok
test state::tests::after_success_not_immediately_due ... ok
test state::tests::initial_state_is_due ... ok
test state::tests::failure_schedules_backoff ... ok
test state::tests::interval_set_from_response ... ok
test state::tests::multiple_failures_increase_backoff ... ok
test state::tests::response_min_interval_is_enforced ... ok
test state::tests::schedule_immediate_makes_it_due ... ok
test state::tests::success_resets_failure_count ... ok
test state::tests::success_retains_opaque_tracker_id ... ok
test state::tests::warning_sets_warning_status ... ok
test tier::tests::advance_wraps_within_tier ... ok
test tier::tests::all_tracker_urls_covers_all_tiers ... ok
test tier::tests::multi_tier_setup ... ok
test tier::tests::promote_active_moves_to_front ... ok
test tier::tests::single_tracker_tier ... ok
test udp::tests::announce_request_encodes_98_bytes ... ok
test udp::tests::announce_request_event_started_is_2 ... ok
test udp::tests::announce_response_parse ... ok
test udp::tests::announce_response_too_short ... ok
test udp::tests::connect_request_encodes_magic ... ok
test udp::tests::connect_response_parse ... ok
test udp::tests::connect_response_too_short ... ok
test udp::tests::v2_infohash_rejected_for_udp ... ok

test result: ok. 66 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/rt_utp-39b735a77ab53ca0)

running 28 tests
test congestion::tests::ack_below_target_increases_window ... ok
test congestion::tests::ack_above_target_reduces_window ... ok
test congestion::tests::timeout_halves_window_but_keeps_mtu_floor ... ok
test packet::tests::all_packet_types_roundtrip ... ok
test packet::tests::encode_decode_roundtrip ... ok
test packet::tests::oversized_extension_encode_errors ... ok
test packet::tests::packet_extension_chain_roundtrips ... ok
test packet::tests::packet_without_extensions_roundtrips_payload ... ok
test packet::tests::too_short_errors ... ok
test packet::tests::truncated_extension_header_errors ... ok
test packet::tests::truncated_extension_payload_errors ... ok
test packet::tests::unknown_type_errors ... ok
test packet::tests::wrong_version_errors ... ok
test selective_ack::tests::empty_selective_ack_has_no_offsets ... ok
test selective_ack::tests::selective_ack_offsets_roundtrip_to_bits ... ok
test state::tests::acceptor_uses_syn_to_seed_ack_and_connection_ids ... ok
test state::tests::ack_after_newest_sent_is_rejected ... ok
test state::tests::connection_ids_follow_bep29_syn_rule ... ok
test state::tests::data_send_advances_sequence_and_tracks_flight ... ok
test state::tests::inbound_data_updates_ack_and_delivers_payload ... ok
test state::tests::initiator_syn_to_state_establishes_connection ... ok
test state::tests::selective_ack_extension_can_be_attached_to_state_packet ... ok
test state::tests::sequence_before_handles_wraparound ... ok
test state::tests::wrong_connection_id_is_rejected ... ok
test state::tests::zero_ack_is_treated_as_no_ack_before_peer_has_seen_sequence ... ok
test transport::tests::utp_stream_connects_and_exchanges_payload ... ok
test transport::tests::utp_stream_read_exact_spans_payload_chunks ... ok
test transport::tests::utp_endpoint_accepts_multiple_streams_on_one_socket ... ok

test result: ok. 28 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/main.rs (target/debug/deps/torrentngd-442d7c927a3081c1)

running 16 tests
test export::tests::help_and_errors ... ok
test export::tests::parses_minimal_dry_run ... ok
test export::tests::parses_full_apply_invocation ... ok
test migrate::tests::help_short_circuits ... ok
test migrate::tests::confirm_accepts_yes_only ... ok
test migrate::tests::filter_plan_keeps_only_trusted_complete_when_requested ... ok
test migrate::tests::parses_minimal_dry_run ... ok
test migrate::tests::parses_full_apply_invocation ... ok
test migrate::tests::rejects_missing_required_and_unknowns ... ok
test migrate::tests::remap_parsing_validates_format ... ok
test migrate::tests::source_and_policy_aliases ... ok
test tests::daemon_auth_allows_webui_but_keeps_api_private ... ok
test tests::request_log_skips_health_metrics_ws_and_static_assets ... ok
test tests::static_dir_defaults_to_packaged_webui_path ... ok
test tests::request_id_accepts_bounded_safe_header_values ... ok
test migrate::tests::dry_run_and_apply_wire_to_native_db ... ok

test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_api_deluge

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_api_model

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_api_native

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_api_qbit

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_api_rtorrent

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_api_transmission

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_bencode

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_config

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_db

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_dht

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_engine

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_fastresume

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_hash

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_jobs

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_logging

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_metainfo

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_metrics

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_migrate

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_path

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_peer_manager

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_peer_wire

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_piece_map

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_piece_picker

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_session

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_storage

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_testkit

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_tracker

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_utp

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

```

## Storage NG feature matrix

- Command: `/home/keith/Documents/code/TorrentNG/scripts/storage_ng_feature_matrix.sh`

```text

== format ==

== owned-read adoption guard ==

== storage unit matrix ==
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.04s
     Running unittests src/lib.rs (target/debug/deps/rt_storage-70d2bb6d0492ef50)

running 130 tests
test backend::tests::backend_request_parses_user_values ... ok
test backend::tests::fixed_buffer_strategy_names_are_stable_for_metrics ... ok
test backend::tests::uring_fixed_buffer_registration_budget_stays_below_common_memlock_limit ... ok
test backend::tests::forcing_pread_selects_pread ... ok
test device::tests::cow_fs_detection_covers_common_filesystems ... ok
test backend::tests::pread_backend_queue_fails_closed_when_full ... ok
test device::tests::block_device_profile_uses_name_and_rotational_flag ... ok
test device::tests::mountinfo_parser_decodes_paths_and_finds_longest_prefix ... ok
test device::tests::mount_source_subpath_suffix_is_ignored ... ok
test device::tests::sysfs_block_device_name_prefers_parent_block_device ... ok
test device::tests::network_topology_uses_mount_source_as_device_id ... ok
test elevator::tests::limited_drain_uses_class_weights_and_keeps_remainder_pending ... ok
test elevator::tests::ready_reads_are_offset_sorted_and_coalesced_per_file ... ok
test elevator::tests::nvme_degenerates_to_zero_budget ... ok
test device::tests::topology_reports_fs_profile_and_cow ... ok
test backend::tests::pwrite_then_pread_roundtrip ... ok
test elevator::tests::zero_limited_drain_leaves_ready_ops_pending ... ok
test fd_limit::tests::capacity_respects_floor ... ok
test fd_limit::tests::capacity_scales_with_limit ... ok
test elevator::tests::hdd_budget_holds_nonurgent_ops_until_elapsed ... ok
test fd_limit::tests::raise_returns_nonzero ... ok
test backend::tests::selected_backend_roundtrip ... ok
test device::tests::mountinfo_read_is_bounded ... ok
test backend::tests::pread_past_eof_errors ... ok
test elevator::tests::choke_critical_and_foreground_bypass_budget ... ok
test frame::tests::acquire_release_roundtrips_capacity ... ok
test frame::tests::into_bytes_releases_charge_without_copying_payload ... ok
test frame::tests::oversize_is_exact_and_counted ... ok
test frame::tests::buffers_are_reused_within_class ... ok
test frame::tests::registered_slot_frame_keeps_charge_until_drop ... ok
test frame::tests::cap_enforced_with_backpressure ... ok
test handle_cache::tests::final_component_symlink_is_rejected ... ok
test handle_cache::tests::ancestor_symlink_is_rejected_before_opening_the_file ... ok
test elevator::tests::writes_are_ordered_but_not_coalesced ... ok
test handle_cache::tests::missing_file_read_errors_and_is_not_cached ... ok
test backend::tests::uring_probe_is_diagnostic_not_panic ... ok
test plan::tests::execute_plan_under_roots_rejects_delete_escape ... ok
test handle_cache::tests::reuses_same_handle_for_repeated_opens ... ok
test open::tests::limited_read_rejects_oversized_runtime_file ... ok
test plan::tests::execute_plan_under_roots_rejects_parent_dir_components ... ok
test plan::tests::delete_requires_prior_dry_run_approval ... ok
test plan::tests::execute_plan_under_roots_rejects_destination_escape_before_copy ... ok
test handle_cache::tests::lru_evicts_least_recently_used ... ok
test plan::tests::execute_plan_under_roots_allows_confined_move ... ok
test plan::tests::descriptor_anchored_execution_rejects_an_ancestor_symlink ... ok
test plan::tests::execute_copy_verify_plan_rejects_symlink_source_file ... ok
test plan::tests::checkpoint_failure_leaves_committed_step_resumable ... ok
test plan::tests::cleanup_plan_is_idempotent_and_prunes_only_empty_directories ... ok
test plan::tests::controlled_copy_aborts_mid_step_and_rolls_back_staging ... ok
test plan::tests::execute_copy_verify_plan_copies_directory_tree_and_verifies_bytes ... ok
test plan::tests::move_plan_reports_conflict_and_capacity ... ok
test plan::tests::move_plan_uses_rename_on_same_filesystem ... ok
test plan::tests::plan_delete_treats_broken_symlink_as_existing_source ... ok
test plan::tests::execute_copy_verify_plan_rejects_nested_symlink_entry ... ok
test handle_cache::tests::read_and_write_handles_are_distinct ... ok
test plan::tests::execute_import_plan_links_or_copies_without_removing_source ... ok
test plan::tests::plan_import_treats_broken_destination_symlink_as_existing ... ok
test plan::tests::execute_move_plan_rejects_symlink_source_before_rename ... ok
test plan::tests::move_plan_copy_verify_path_deletes_source_after_verified_rename ... ok
test plan::tests::execute_copy_verify_plan_rolls_back_staged_file_on_short_copy ... ok
test plan::tests::execute_plan_reports_rollback_step_failure_in_error_message ... ok
test plan::tests::move_plan_copy_verify_path_removes_source_directory_after_verified_rename ... ok
test plan::tests::import_plan_detects_existing_destination ... ok
test plan::tests::reconcile_detects_rename_committed_before_checkpoint ... ok
test plan::tests::failed_checkpoint_can_resume_without_repeating_filesystem_mutation ... ok
test runtime::tests::backend_short_io_maps_to_storage_error ... ok
test runtime::tests::backend_would_block_maps_to_queue_full ... ok
test scheduler::tests::auto_preallocation_policy_uses_full_only_for_non_cow_hdd ... ok
test plan::tests::repeating_a_completed_copy_plan_is_idempotent ... ok
test plan::tests::execute_delete_plan_removes_symlink_without_following_target ... ok
test plan::tests::import_copy_plan_stages_before_final_rename ... ok
test scheduler::tests::blocking_pool_full_queue_fails_closed ... ok
test plan::tests::execute_import_plan_rejects_symlink_source_before_hardlink ... ok
test plan::tests::execute_move_plan_renames_without_overwrite ... ok
test plan::tests::execute_delete_plan_removes_directory_tree_after_approval ... ok
test plan::tests::execute_import_copy_failure_does_not_leave_final_destination ... ok
test backend::tests::uring_strategy_reports_frame_pool_slots_only_with_registered_buffers ... ok
test plan::tests::reconcile_rejects_corrupt_staging_copy ... ok
test plan::tests::verify_content_matches_detects_bit_flip_despite_matching_length ... ok
test plan::tests::retrying_a_completed_plan_is_idempotent ... ok
test plan::tests::reconcile_infers_copy_when_following_rename_is_already_committed ... ok
test scheduler::tests::peer_read_elevator_full_queue_fails_closed ... ok
test scheduler::tests::acquire_and_release_recheck ... ok
test backend::tests::uring_request_has_clean_probe_fallback ... ok
test scheduler::tests::peer_read_not_starved_by_recheck ... ok
test scheduler::tests::scheduler_new_resolves_auto_to_sparse_without_path_topology ... ok
test scheduler::tests::ssd_has_higher_concurrency ... ok
test scheduler::tests::large_peer_and_recheck_reads_emit_page_cache_advice ... ok
test scheduler::tests::strict_write_sync_is_counted_and_not_left_dirty ... ok
test scheduler::tests::write_does_not_create_by_default ... ok
test scheduler::tests::prepare_file_extends_without_disturbing_existing_bytes ... ok
test verify::tests::v2_file_verify_accepts_empty_file_without_root ... ok
test scheduler::tests::runtime_file_pool_rejects_an_ancestor_symlink ... ok
test scheduler::tests::sparse_prepare_creates_parent_once ... ok
test scheduler::tests::short_positioned_read_maps_to_storage_error ... ok
test scheduler::tests::read_nonexistent_file_does_not_create ... ok
test verify::tests::v2_file_verify_rejects_wrong_file_root ... ok
test scheduler::tests::schedulers_on_same_device_share_global_queue ... ok
test backend::tests::forced_uring_roundtrip_when_kernel_supports_it ... ok
test scheduler::tests::hdd_peer_read_elevator_batches_shuffled_adjacent_reads ... ok
test scheduler::tests::sync_all_open_files_syncs_dirty_paths_after_fd_eviction ... ok
test verify::tests::verify_all_reports_per_piece ... ok
test verify::tests::verify_missing_file ... ok
test verify::tests::verify_range_returns_empty_for_reversed_or_out_of_range_bounds ... ok
test verify::tests::verify_invalid_piece ... ok
test verify::tests::verify_range_resumable ... ok
test verify::tests::v2_file_verify_accepts_matching_file_root ... ok
test verify::tests::v2_file_verify_hashes_sparse_holes_as_zeroes ... ok
test verify::tests::verify_truncated_sparse_file_is_missing_not_zero_filled ... ok
test verify::tests::verify_valid_piece ... ok
test scheduler::tests::full_mount_queue_fails_closed ... ok
test runtime::tests::missing_file_maps_to_not_found ... ok
test scheduler::tests::compatibility_read_still_returns_bytes ... ok
test scheduler::tests::concurrent_positioned_writes_do_not_share_cursor ... ok
test scheduler::tests::queued_disk_governor_denies_before_enqueue ... ok
test scheduler::tests::peer_read_readahead_cache_can_be_disabled ... ok
test scheduler::tests::queued_disk_bytes_track_active_blocking_job_payload ... ok
test scheduler::tests::hdd_peer_read_elevator_dispatches_after_quiet_slice ... ok
test scheduler::tests::owned_read_returns_pooled_frame_for_exact_backend_read ... ok
test scheduler::tests::file_pool_records_hits_and_evictions ... ok
test scheduler::tests::peer_read_readahead_cache_is_config_bounded ... ok
test scheduler::tests::peer_read_readahead_cache_is_invalidated_by_writes ... ok
test scheduler::tests::prepare_file_refuses_to_shrink_existing_data ... ok
test runtime::tests::global_read_write_roundtrip ... ok
test scheduler::tests::stats_track_io_sync_and_hash_work ... ok
test scheduler::tests::read_and_write_roundtrip ... ok
test scheduler::tests::peer_read_readahead_cache_returns_exact_requested_bytes ... ok
test scheduler::tests::prepare_file_does_not_create_through_an_ancestor_symlink ... ok
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


== storage certification script self-test ==
storage certification self-test: PASS

== resource governor and scale proxies ==
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.07s
     Running unittests src/lib.rs (target/debug/deps/rt_metrics-51cb908b55e16dc6)

running 6 tests
test counter::tests::counter_inc_and_get ... ok
test counter::tests::counter_reset ... ok
test counter::tests::metrics_snapshot ... ok
test resource::tests::class_and_global_caps_are_enforced ... ok
test resource::tests::leases_release_on_drop ... ok
test resource::tests::pressure_transitions_are_deterministic ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/scale.rs (target/debug/deps/scale-a94e67aeae0fe940)

running 19 tests
test recheck_does_not_starve_seeding_peer_reads ... ok
test crash_watermark_bounds_restart_recheck_to_dirty_pieces ... ok
test tracker_restart_storm_15k_is_spread_by_jitter ... ok
test completed_piece_ram_hash_avoids_read_after_write_backend_reads ... ok
test storage_recheck_hashing_reports_scheduler_result_without_runtime_stall ... ok
test storage_positioned_io_preserves_offsets_under_concurrency ... ok
test storage_hash_pool_does_not_block_peer_read_path ... ok
test storage_peer_read_readahead_reduces_backend_reads_for_adjacent_blocks ... ok
test storage_file_pool_stays_bounded_under_active_file_churn ... ok
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


== configuration defaults ==
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.03s
     Running unittests src/lib.rs (target/debug/deps/rt_config-932561101fc78255)

running 9 tests
test tests::db_path_fallback ... ok
test tests::dht_port_fallback ... ok
test tests::default_config_is_valid ... ok
test tests::invalid_config_is_rejected ... ok
test tests::metrics_torrent_identifier_opt_in_round_trips ... ok
test tests::parse_logging_toml ... ok
test tests::load_appends_newline_delimited_tokens_from_a_relative_secret_file ... ok
test tests::parse_toml_partial ... ok
test tests::config_file_size_is_bounded ... ok

test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_config

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s


== engine storage/resource consumers ==
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.06s
     Running unittests src/lib.rs (target/debug/deps/rt_engine-f64af50313e12476)

running 201 tests
test command::tests::engine_stats_accumulates_torrent_runtime_storage_counters ... ok
test command::tests::engine_stats_tracks_top_hot_torrent_memory_estimates ... ok
test dht_task::tests::announced_peer_cache_is_bounded_and_keeps_duplicates ... ok
test dht_task::tests::announce_peer_query_uses_configured_listen_port ... ok
test dht_task::tests::announce_peer_requires_matching_token ... ok
test dht_task::tests::announce_peer_stores_ipv6_peer_and_get_peers_returns_it ... ok
test dht_task::tests::announce_peer_stores_peer_and_get_peers_returns_it ... ok
test dht_task::tests::closest_response_includes_known_nodes_and_token ... ok
test dht_task::tests::dht_ingress_budget_bounds_each_ip_and_expires_windows ... ok
test dht_task::tests::prune_stale_outstanding_removes_expired_entries_only ... ok
test egress_policy::tests::default_policy_denies_sensitive_address_ranges ... ok
test egress_policy::tests::policy_can_explicitly_allow_private_lan ... ok
test dht_task::tests::lookup_restart_clears_previously_queried_nodes ... ok
test dht_task::tests::lookup_continues_to_unqueried_closer_nodes ... ok
test dht_task::tests::transaction_ids_are_nonzero_and_advance ... ok
test egress_policy::tests::default_policy_allows_only_http_webseeds ... ok
test egress_policy::tests::default_policy_allows_public_tracker_schemes_only ... ok
test dht_task::tests::response_from_wrong_source_address_is_ignored ... ok
test dht_task::tests::get_peers_response_forwards_discovered_peers_to_torrent ... ok
test engine::tests::decode_info_hash_bytes_accepts_v1_and_v2_lengths ... ok
test egress_policy::tests::http_clients_are_reused_after_each_address_is_revalidated ... ok
test engine::tests::engine_handle_liveness_tracks_command_receiver ... ok
test engine::tests::engine_handle_reports_peer_listener_health_separately ... ok
test engine::tests::incoming_utp_listener_flag_is_boolean_only ... ok
test engine::tests::label_normalization_trims_dedupes_and_drops_empty_values ... ok
test db_worker::tests::operations_are_serial_and_worker_survives_errors_and_panics ... ok
test db_worker::tests::shutdown_drains_prior_work ... ok
test db_worker::tests::cancelled_queued_operation_is_not_applied ... ok
test engine::tests::metadata_projection_preserves_files_trackers_and_privacy ... ok
test engine::tests::metadata_placeholder_projection_preserves_trackers ... ok
test engine::tests::parse_info_hash_hex_rejects_invalid_input ... ok
test command::tests::hot_seeding_1k_memory_attribution_stays_under_cap ... ok
test engine::tests::append_session_event_persists_payload ... ok
test engine::tests::add_peers_forwards_external_peers_to_torrent_task ... ok
test engine::tests::prune_empty_dirs_stops_at_root_and_keeps_nonempty_dirs ... ok
test engine::tests::pure_v2_metadata_projects_to_engine_and_db_shapes ... ok
test dht_task::tests::response_with_unknown_transaction_id_is_ignored ... ok
test engine::tests::engine_handle_liveness_drops_on_actor_panic ... ok
test dht_task::tests::runtime_stats_count_dht_owned_caches ... ok
test dht_task::tests::dht_ingress_budget_has_bounded_source_state ... ok
test db_worker::tests::sqlite_failure_crosses_boundary_and_worker_continues ... ok
test db_worker::tests::worker_owns_a_real_sqlite_connection_and_persists_ordered_work ... ok
test engine::tests::engine_handle_shutdown_aborts_an_actor_stuck_after_accepting_command ... ok
test engine::tests::add_torrent_rolls_back_registry_and_blob_when_db_persist_fails ... ok
test engine::tests::add_torrent_rolls_back_registry_row_when_blob_write_fails ... ok
test engine::tests::add_v2_only_magnet_persists_metadata_placeholder ... ok
test engine::tests::row_conversion_preserves_session_fields ... ok
test engine::tests::row_conversion_saturates_unsigned_values_at_sqlite_integer_limit ... ok
test dht_task::tests::announced_peer_global_cap_applies_to_existing_info_hashes ... ok
test dht_task::tests::announced_peer_map_is_bounded_across_info_hashes ... ok
test engine::tests::storage_io_config_maps_native_storage_toml ... ok
test engine::tests::network_features_persist_and_notify_running_torrents ... ok
test engine::tests::malformed_persisted_control_settings_fail_closed ... ok
test engine::tests::load_persisted_torrents_restores_paused_registry_as_dormant ... ok
test engine::tests::failed_payload_cleanup_keeps_torrent_retryable ... ok
test engine::tests::metadata_projection_rejects_unrepresentable_file_policy_index ... ok
test engine::tests::storage_plan_resume_steps_are_sorted_unique_and_bounded ... ok
test engine::tests::recheck_job_helpers_persist_state_and_events ... ok
test engine::tests::queue_order_moves_are_persisted ... ok
test engine::tests::load_persisted_torrents_restores_seeding_rows_as_dormant ... ok
test engine::tests::finished_torrent_task_is_removed_and_marked_error ... ok
test engine::tests::global_limits_persist_to_settings_table ... ok
test engine::tests::register_configured_storage_persists_root_and_mount ... ok
test engine::tests::load_persisted_v2_rows_restore_taskless_registry_and_trackers ... ok
test engine::tests::pause_torrent_pauses_taskless_pure_v2_recheck_job ... ok
test engine::tests::engine_stats_include_registry_jobs_and_trackers ... ok
test engine::tests::recover_interrupted_jobs_pauses_running_work ... ok
test metadata_task::tests::dht_only_peer_candidates_can_complete_magnet_metadata ... ok
test metadata_task::tests::metadata_attempt_cache_cap_scales_with_peer_limit ... ok
test engine::tests::reserve_memory_command_holds_and_releases_lease ... ok
test metadata_task::tests::metadata_fetch_candidates_are_bounded_and_prune_retry_history ... ok
test metadata_task::tests::metadata_fetch_reservation_uses_metadata_governor_class ... ok
test metadata_task::tests::metadata_peer_retry_has_cooldown ... ok
test metadata_task::tests::parses_metadata_transport_policy ... ok
test metadata_task::tests::validates_metadata_info_hash ... ok
test metadata_task::tests::validates_metadata_piece_lengths ... ok
test network_budget::tests::limited_budget_refills_after_wait ... ok
test network_budget::tests::limited_budget_accepts_request_larger_than_bucket_capacity ... ok
test network_budget::tests::peer_slots_are_shared_across_clones ... ok
test network_budget::tests::unlimited_budget_does_not_wait ... ok
test peer_id::tests::default_identity_is_upstream_rtorrent_0_16_11_pair ... ok
test peer_id::tests::independently_generated_peer_ids_do_not_collide ... ok
test peer_id::tests::persisted_suffix_is_stable_across_reloads ... ok
test peer_id::tests::init_persists_and_is_idempotent_for_this_process ... ok
test peer_id::tests::runtime_user_agent_rejects_empty_or_non_ascii_values ... ok
test peer_ingress::tests::cancelled_admission_does_not_consume_per_ip_slot ... ok
test peer_ingress::tests::global_budget_limits_unrouted_handshakes ... ok
test peer_ingress::tests::global_rejection_does_not_consume_per_ip_slot ... ok
test peer_ingress::tests::per_ip_budget_limits_connection_storms ... ok
test storage_authority::tests::empty_roots_are_rejected ... ok
test storage_authority::tests::configured_roots_are_canonicalized_and_deduped ... ok
test storage_authority::tests::existing_and_missing_descendants_are_authorized ... ok
test storage_authority::tests::missing_roots_are_rejected ... ok
test storage_authority::tests::relative_parent_and_outside_paths_are_rejected ... ok
test storage_authority::tests::symlinked_existing_path_is_checked_by_canonical_target ... ok
test engine::tests::recheck_job_control_sends_torrent_commands_and_updates_state ... ok
test engine::tests::private_magnet_completion_removes_provisional_dht_and_preserves_pause ... ok
test engine::tests::recovered_delete_job_finalizes_metadata_after_payload_cleanup ... ok
test engine::tests::rename_file_and_folder_paths_update_metadata_projection ... ok
test storage_jobs::tests::dispatcher_rejects_duplicate_active_job_ids ... ok
test engine::tests::shutdown_torrent_tasks_sends_shutdown_and_waits_for_task_exit ... ok
test engine::tests::recovered_storage_plan_reconciles_filesystem_ahead_of_checkpoint ... ok
test engine::tests::engine_start_owns_background_storage_supervisor_until_shutdown ... ok
test engine::tests::dormant_promotion_is_detached_and_coalesced ... ok
test engine::tests::remove_torrent_queues_payload_cleanup_outside_engine_actor ... ok
test storage_jobs::tests::worker_registration_guard_releases_on_unwind ... ok
test tier::tests::active_engine_work_stays_hot ... ok
test tier::tests::compact_piece_bitmap_roundtrips_and_rejects_tail_bits ... ok
test tier::tests::compact_piece_bitmap_supports_mutation_and_projection ... ok
test engine::tests::startup_reconciles_missing_rows_and_quarantines_orphan_projections ... ok
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
test torrent_task::tests::parses_ut_pex_added_ipv4_peers ... ok
test torrent_task::tests::parses_ut_pex_dropped_ipv4_and_ipv6_peers ... ok
test torrent_task::tests::peer_availability_reconcile_counts_only_transitions ... ok
test torrent_task::tests::peer_event_channel_capacity_is_bounded_by_global_peer_budget ... ok
test engine::tests::complete_v2_only_magnet_persists_metadata_without_task ... ok
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
test engine::tests::pure_v2_recheck_verifies_file_roots_without_torrent_task ... ok
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
test engine::tests::categories_survive_engine_restart_and_keep_torrent_labels_consistent ... ok
test torrent_task::tests::upload_block_reads_across_many_file_regions ... ok
test torrent_task::tests::upload_block_reservation_uses_peer_buffer_governor_class ... ok
test torrent_task::tests::upload_context_piece_map_is_shared_not_deep_cloned_per_peer ... ok
test torrent_task::tests::webseed_block_url_accepts_direct_file_and_base_url ... ok
test torrent_task::tests::webseed_block_url_expands_single_file_directory_prefix ... ok
test torrent_task::tests::webseed_body_reservation_uses_webseed_governor_class ... ok
test torrent_task::tests::webseed_retry_delay_is_exponential_and_bounded ... ok
test tracker_runtime::tests::tracker_worker_budget_is_explicit_and_bounded ... ok
test engine::tests::storage_move_db_failure_keeps_destination_live_and_commit_pending ... ok
test engine::tests::storage_plan_execution_rejects_missing_server_roots ... ok
test engine::tests::storage_root_projection_reports_capacity_and_root_errors ... ok
test engine::tests::torrent_blob_export_preserves_raw_metainfo_bytes ... ok
test engine::tests::storage_operation_admission_rejects_active_same_torrent_job ... ok
test engine::tests::subsystem_health_reports_dead_dependency_seams ... ok
test engine::tests::update_torrent_limits_notifies_running_torrent_task ... ok
test engine::tests::torrent_diagnostic_explains_paused_private_tracker_gap ... ok
test engine::tests::user_agent_update_persists_and_changes_runtime_clients ... ok
test engine::tests::storage_plan_execution_uses_persisted_roots_and_fails_closed ... ok
test storage_jobs::tests::closed_worker_releases_end_to_end_inflight_registration ... ok
test engine::tests::update_torrent_limits_persists_and_reads_back ... ok
test storage_jobs::tests::queue_is_bounded_and_control_is_shared ... ok
test engine::tests::storage_plan_jobs_checkpoint_completed_steps ... ok
test engine::tests::update_torrent_trackers_persists_summary_and_detail_rows ... ok
test storage_jobs::tests::move_persistence_failure_remains_commit_pending ... ok
test storage_jobs::tests::terminal_persistence_records_partial_progress ... ok
test storage_jobs::tests::injected_worker_panic_is_contained_and_next_job_runs ... ok
test engine::tests::update_save_path_moves_existing_payload_through_storage_plan ... ok
test storage_jobs::tests::production_dispatcher_uses_dedicated_database_connection ... ok
test torrent_task::tests::seed_ratio_limit_pauses_a_completed_torrent ... ok
test torrent_task::tests::transfer_stats_are_batched_until_progress_flush ... ok
test storage_jobs::tests::dispatcher_rejects_when_end_to_end_inflight_bound_is_reached ... ok
test storage_jobs::tests::pause_at_step_boundary_releases_worker_for_another_job ... ok
test tier::tests::controller_tracks_100k_registry_entries_with_only_two_percent_hot ... ok
test storage_jobs::tests::paused_job_waits_without_consuming_worker_slot_until_resumed ... ok
test storage_jobs::tests::shutdown_requeues_active_and_queued_jobs_for_restart ... ok
test engine::tests::engine_command_send_is_bounded_when_mailbox_is_full ... ok
test engine::tests::engine_health_reply_is_bounded_when_actor_stops_replying ... ok
test torrent_task::tests::peer_event_delivery_is_bounded_when_torrent_actor_stalls ... ok
test engine::tests::update_save_path_reroutes_running_task_and_recheck_finds_new_root ... ok

test result: ok. 201 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.27s

   Doc-tests rt_engine

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s


== native API metrics projection ==
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.08s
     Running unittests src/lib.rs (target/debug/deps/rt_api_native-df89b98aa2ddf445)

running 60 tests
test handlers::tests::api_snapshot_estimates_scale_with_torrent_count ... ok
test handlers::tests::job_view_projects_progress_and_checkpoint_fields ... ok
test handlers::tests::level_from_kind_is_conservative ... ok
test handlers::tests::json_store_validation_rejects_malformed_executable_fields ... ok
test handlers::tests::json_store_validation_accepts_documented_rule_shapes ... ok
test handlers::tests::metric_with_label_escapes_label_values_once ... ok
test handlers::tests::native_engine_capabilities_cover_rewrite_surface ... ok
test handlers::tests::native_webui_capabilities_match_mounted_routes ... ok
test handlers::tests::session_event_response_projects_level_and_payload ... ok
test handlers::tests::session_event_response_rejects_corrupt_durable_payload ... ok
test handlers::tests::delete_torrent_found ... ok
test handlers::tests::diagnostics_without_engine_returns_unavailable ... ok
test handlers::tests::get_torrent_not_found ... ok
test handlers::tests::list_torrents_empty ... ok
test handlers::tests::get_torrent_found ... ok
test handlers::tests::delete_torrent_not_found ... ok
test handlers::tests::jobs_without_engine_returns_unavailable ... ok
test handlers::tests::add_torrent_without_engine_returns_unavailable ... ok
test handlers::tests::mutating_endpoint_accepts_bearer_token ... ok
test handlers::tests::storage_plan_preview_projects_staged_import_copy ... ok
test handlers::tests::list_torrents_with_entry ... ok
test handlers::tests::list_torrents_reports_total_independent_of_page_size ... ok
test handlers::tests::storage_plan_completed_steps_are_bounded_by_plan ... ok
test handlers::tests::health_reports_unavailable_without_engine ... ok
test handlers::tests::storage_plan_preview_projects_move_steps ... ok
test handlers::tests::update_torrent_limits_request_distinguishes_null_from_absent ... ok
test handlers::tests::idempotency_key_replays_native_mutation_and_rejects_reuse ... ok
test handlers::tests::storage_plan_root_validation_rejects_escape ... ok
test handlers::tests::utp_capability_helpers_match_runtime_policy_values ... ok
test state::tests::signed_summary_projection_saturates_unsigned_counters ... ok
test handlers::tests::torrent_delta_reports_initial_changes_and_removals ... ok
test handlers::tests::list_torrents_snapshot_pins_pages_across_mutations ... ok
test state::tests::journal_refresh_applies_final_entry_state_without_registry_scan ... ok
test state::tests::text_filter_index_handles_short_filters_and_case_folding ... ok
test state::tests::media_type_facets_are_indexed_and_updated_incrementally ... ok
test handlers::tests::torrent_delta_chunks_initial_snapshot_at_one_revision ... ok
test state::tests::structural_snapshot_refresh_does_not_use_stale_positions ... ok
test handlers::tests::transfer_limits_without_engine_returns_unavailable ... ok
test handlers::tests::storage_without_engine_returns_unavailable ... ok
test handlers::tests::torrent_limits_without_engine_returns_unavailable ... ok
test handlers::tests::tag_post_delete_and_bulk_set_are_native ... ok
test handlers::tests::update_torrent_without_engine_updates_registry ... ok
test handlers::tests::storage_plan_completed_steps_accept_sorted_unique_subset ... ok
test handlers::tests::native_hash_resolution_preserves_unknown_targets_for_error_reporting ... ok
test handlers::tests::render_metrics_exposes_dependency_health_and_pressure ... ok
test handlers::tests::recheck_torrent_found ... ok
test handlers::tests::set_category_without_engine_updates_registry ... ok
test handlers::tests::mutating_endpoint_requires_configured_token ... ok
test handlers::tests::native_login_issues_session_cookie_and_validates_tokens ... ok
test handlers::tests::metrics_reports_unavailable_without_engine ... ok
test handlers::tests::resume_torrent_found ... ok
test handlers::tests::mutating_endpoint_accepts_session_cookie_token ... ok
test handlers::tests::reannounce_torrent_without_engine_is_unavailable ... ok
test handlers::tests::patch_tags_without_engine_updates_registry ... ok
test handlers::tests::native_rtorrent_compatibility_routes_fail_closed ... ok
test handlers::tests::patch_trackers_without_engine_reports_unavailable ... ok
test handlers::tests::pause_torrent_found ... ok
test handlers::tests::patch_files_rejects_empty_body ... ok
test handlers::tests::native_alias_and_projection_routes_are_exposed ... ok
test handlers::tests::render_metrics_includes_engine_stats ... ok

test result: ok. 60 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s

   Doc-tests rt_api_native

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

```

## WebUI certification

- Command: `/home/keith/Documents/code/TorrentNG/scripts/webui_certification.sh`

```text
/home/keith/Documents/code/TorrentNG/certification/reports/webui-certification-20260910T193707Z.md
```

## API facade certification

- Command: `/home/keith/Documents/code/TorrentNG/scripts/api_facade_certification.sh /home/keith/Documents/code/TorrentNG/certification/reports/api-facades-local-release-20260910T193718Z.md`

```text
/home/keith/Documents/code/TorrentNG/certification/reports/api-facades-local-release-20260910T193718Z.md
```

## release artifact build

- Command: `cargo build --release --locked -p torrentngd`

```text
    Finished `release` profile [optimized] target(s) in 0.08s
```

## authenticated release-binary smoke

- Command: `/home/keith/Documents/code/TorrentNG/scripts/backend_burndown_native_release_smoke.sh /home/keith/Documents/code/TorrentNG/certification/reports/backend-burndown-native-release-smoke-local-release-20260910T193719Z.md`

```text
/home/keith/Documents/code/TorrentNG/certification/reports/backend-burndown-native-release-smoke-local-release-20260910T193719Z.md
```

## backup and restore drill

- Command: `/home/keith/Documents/code/TorrentNG/scripts/backup_restore_certification.sh /home/keith/Documents/code/TorrentNG/certification/reports/backup-restore-local-release-20260910T193719Z.md`

```text
/home/keith/Documents/code/TorrentNG/certification/reports/backup-restore-local-release-20260910T193719Z.md
```

## migration exported corpus coverage

- Command: `/home/keith/Documents/code/TorrentNG/scripts/migration_corpus_certification.sh /home/keith/Documents/code/TorrentNG/certification/reports/migration-corpus-local-release-20260910T193721Z.md`

```text
/home/keith/Documents/code/TorrentNG/certification/reports/migration-corpus-local-release-20260910T193721Z.md
```

## native config security review

- Command: `bash -c
  set -euo pipefail
  TNG_API_TOKENS="${TNG_API_TOKENS:-local-release-native-token}" \
    "$1/scripts/security_review.sh" "$1/deploy/native/config.toml" "$2/security-review-native-local-$(date -u +%Y%m%dT%H%M%SZ).md"
 _ /home/keith/Documents/code/TorrentNG /home/keith/Documents/code/TorrentNG/certification/reports`

```text
/home/keith/Documents/code/TorrentNG/certification/reports/security-review-native-local-20260910T193722Z.md
```

## sidecar config security review

- Command: `bash -c
  set -euo pipefail
  TNG_API_TOKENS="${TNG_API_TOKENS:-local-release-sidecar-token}" \
  TNG_SECRET_KEY="${TNG_SECRET_KEY:-local-release-sidecar-secret-00000000000000000000}" \
    "$1/scripts/security_review.sh" "$1/deploy/docker/sidecar.config.toml" "$2/security-review-sidecar-local-$(date -u +%Y%m%dT%H%M%SZ).md"
 _ /home/keith/Documents/code/TorrentNG /home/keith/Documents/code/TorrentNG/certification/reports`

```text
/home/keith/Documents/code/TorrentNG/certification/reports/security-review-sidecar-local-20260910T193722Z.md
```

## storage release certification

```text
SKIP: set TNG_STORAGE_MATRIX_TARGETS='/mnt/nvme /mnt/hdd' to run real-device probes
```

Overall status: PASS_WITH_WARNINGS
Warnings: 1
Total duration: 35s
