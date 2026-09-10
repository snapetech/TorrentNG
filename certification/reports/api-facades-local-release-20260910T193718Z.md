# TorrentNG API Facade Certification Report

- Date UTC: 2026-09-10T19:37:18Z
- Host: kspld0
- Rust: rustc 1.98.1 (48a229cea 2026-09-01)
- Cargo: cargo 1.98.1 (797e8a9bc 2026-08-05)
- Commit: 50e0fc3

## Gates

| Gate | Result |
|---|---|
| qBittorrent Web API facade matrix | PASS |
| Transmission RPC facade matrix | PASS |
| Deluge JSON-RPC facade matrix | PASS |
| rTorrent XMLRPC facade matrix | PASS |

## qBittorrent Web API facade matrix

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.07s
     Running unittests src/lib.rs (target/debug/deps/rt_api_qbit-47259de7c7b0c70a)

running 76 tests
test handlers::tests::log_main_query_filters_qbit_types ... ok
test handlers::tests::pieces_have_is_bounded ... ok
test handlers::tests::parse_qbit_bool_accepts_common_wire_values ... ok
test handlers::tests::parse_peer_addrs_accepts_pipe_separated_socket_addresses ... ok
test handlers::tests::parse_form_body_decodes_qbit_forms ... ok
test handlers::tests::qbit_api_snapshot_estimates_scale_with_torrent_count ... ok
test handlers::tests::qbit_files_project_partial_per_file_progress ... ok
test handlers::tests::qbit_log_type_uses_level_payload_and_kind_fallbacks ... ok
test handlers::tests::qbit_peer_log_entry_projects_engine_peer_snapshot ... ok
test handlers::tests::qbit_log_entry_projects_session_events ... ok
test handlers::tests::qbit_progress_and_piece_count_ignore_stale_completion_timestamp ... ok
test handlers::tests::qbit_session_rates_sum_torrent_info_rates ... ok
test handlers::tests::qbit_state_projects_active_recheck_as_checking ... ok
test handlers::tests::qbit_log_entry_rejects_corrupt_or_unidentified_session_events ... ok
test handlers::tests::qbit_filters_do_not_silently_fall_back_to_all_torrents ... ok
test handlers::tests::qbit_swarm_projection_counts_live_peers_and_rates ... ok
test handlers::tests::qbit_tracker_projection_prefers_live_snapshot_order ... ok
test handlers::tests::search_plugin_validation_rejects_silent_projection_loss ... ok
test handlers::tests::qbit_torrent_peers_projection_and_rid_are_stable ... ok
test handlers::tests::qbit_tracker_projection_uses_persisted_engine_state ... ok
test handlers::tests::split_tracker_values_accepts_qbit_separators_and_dedupes ... ok
test handlers::tests::redact_log_url_removes_sensitive_parts ... ok
test handlers::tests::strict_mutation_parsers_reject_dropped_values ... ok
test handlers::tests::sync_rid_covers_tracker_snapshot_digest ... ok
test handlers::tests::sync_rid_is_order_independent_and_covers_metadata_projection ... ok
test handlers::tests::ssrf_guard_rejects_private_and_local_ips ... ok
test handlers::tests::create_tags_persists_empty_global_tags ... ok
test handlers::tests::add_and_remove_tags_resolve_all_hashes ... ok
test handlers::tests::app_preferences_and_default_save_path_ok ... ok
test handlers::tests::app_version_ok ... ok
test handlers::tests::login_requires_configured_token_and_cookie_auth_round_trips ... ok
test handlers::tests::idempotency_key_replays_qbit_mutation_and_rejects_reuse ... ok
test handlers::tests::app_cookies_and_api_key_roundtrip ... ok
test handlers::tests::app_set_preferences_persists_form_and_json_updates ... ok
test handlers::tests::qbit_detail_endpoints_fail_closed_without_engine_metadata ... ok
test handlers::tests::category_and_global_tag_endpoints_update_registry ... ok
test handlers::tests::app_shutdown_notifies_daemon_and_email_is_explicitly_unsupported ... ok
test handlers::tests::qbit_alias_and_broad_compat_routes_are_registered ... ok
test handlers::tests::torrents_info_filters_by_tag_and_sorts ... ok
test model::tests::state_mapping_covers_all_internal_states ... ok
test handlers::tests::torrents_info_intersects_hash_filter_with_indexed_filters ... ok
test model::tests::torrent_info_serializes ... ok
test model::tests::torrent_properties_serializes_qbit_fields ... ok
test handlers::tests::torrents_info_rejects_mixed_all_hash_filter ... ok
test model::tests::unknown_state_maps_to_unknown ... ok
test handlers::tests::torrents_info_rejects_unavailable_speed_sorting ... ok
test handlers::tests::torrents_info_pages_are_pinned_by_snapshot_header ... ok
test state::tests::journal_refresh_applies_final_entry_state_without_registry_scan ... ok
test state::tests::structural_snapshot_refresh_does_not_use_stale_positions ... ok
test state::tests::arbitrary_sort_values_do_not_grow_snapshot_order_cache ... ok
test handlers::tests::torrents_properties_missing_hash_is_bad_request ... ok
test handlers::tests::torrents_info_with_entry ... ok
test handlers::tests::torrents_properties_returns_registry_projection_without_engine ... ok
test handlers::tests::transfer_info_fails_closed_without_engine ... ok
test handlers::tests::transfer_ban_peers_fails_closed_without_engine ... ok
test handlers::tests::transfer_limit_endpoints_roundtrip_without_engine ... ok
test handlers::tests::login_returns_ok ... ok
test handlers::tests::torrents_info_empty ... ok
test handlers::tests::torrents_add_without_engine_returns_unavailable ... ok
test handlers::tests::sync_maindata_returns_full_update ... ok
test handlers::tests::rename_and_set_location_update_registry_without_engine ... ok
test handlers::tests::set_category_decodes_url_encoded_form_values ... ok
test handlers::tests::qbit_torrent_export_requires_hash_and_engine_blob ... ok
test handlers::tests::qbit_search_plugins_and_jobs_are_stateful ... ok
test handlers::tests::reannounce_reports_unavailable_without_engine ... ok
test handlers::tests::qbit_file_priority_rejects_values_outside_engine_contract ... ok
test handlers::tests::qbit_rss_items_and_rules_round_trip ... ok
test handlers::tests::set_category_resolves_all_hashes ... ok
test handlers::tests::set_category_applies_stored_category_save_path ... ok
test handlers::tests::sync_maindata_uses_stable_rid_for_unchanged_registry ... ok
test handlers::tests::qbit_response_field_matrix_is_present ... ok
test handlers::tests::torrents_categories_and_tags_project_registry_labels ... ok
test handlers::tests::sync_maindata_returns_registry_deltas_and_removals ... ok
test handlers::tests::qbit_torrent_export_streams_persisted_torrent_blob ... ok
test handlers::tests::torrents_info_default_page_is_bounded ... ok
test handlers::tests::engine_backed_qbit_app_state_survives_engine_restart ... ok

test result: ok. 76 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s

   Doc-tests rt_api_qbit

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

```

## Transmission RPC facade matrix

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.07s
     Running unittests src/lib.rs (target/debug/deps/rt_api_transmission-d840c87f821cdb18)

running 41 tests
test tests::transmission_api_snapshot_estimates_scale_with_torrent_and_field_count ... ok
test tests::renamed_file_path_preserves_parent_directory ... ok
test tests::transmission_file_priority_classes_map_to_engine_priorities ... ok
test tests::transmission_file_completion_is_projected_per_file ... ok
test tests::transmission_lifecycle_seconds_project_registry_timestamps ... ok
test tests::transmission_numeric_projection_and_arguments_fail_closed ... ok
test tests::transmission_magnet_link_formats_v1_and_v2 ... ok
test tests::transmission_peer_rates_sum_native_snapshots ... ok
test tests::session_close_notifies_daemon_supervisor ... ok
test tests::transmission_recheck_progress_projects_active_engine_job ... ok
test tests::transmission_idempotency_key_replays_mutation_and_rejects_reuse ... ok
test tests::transmission_json_rpc_20_batch_requests_are_supported ... ok
test tests::transmission_seed_modes_reject_unknown_values ... ok
test tests::transmission_seed_modes_support_standard_camel_case_and_clear_overrides ... ok
test tests::transmission_json_rpc_20_uses_params_and_direct_result ... ok
test tests::transmission_notification_subscriptions_roundtrip_state ... ok
test tests::transmission_session_limit_args_use_kib_wire_units ... ok
test tests::transmission_queue_stalled_settings_roundtrip_without_engine ... ok
test tests::transmission_group_methods_roundtrip_compat_state ... ok
test tests::transmission_router_enforces_configured_token ... ok
test tests::transmission_session_id_handshake ... ok
test tests::transmission_sequential_from_piece_roundtrips_in_torrent_get ... ok
test tests::transmission_torrent_limits_accept_standard_camel_case_aliases ... ok
test tests::transmission_tracker_stats_project_persisted_engine_state ... ok
test tests::transmission_session_access_control_projects_session_security_state ... ok
test tests::transmission_webseed_activity_projects_engine_snapshots ... ok
test tests::transmission_snake_case_rpc_roundtrips_v41_shape ... ok
test tests::transmission_torrent_get_projects_v2_magnet_links ... ok
test tests::transmission_torrent_get_supports_table_format_and_recently_active ... ok
test tests::transmission_torrent_add_uses_session_defaults_without_overriding_explicit_args ... ok
test tests::transmission_stats_and_location_are_supported ... ok
test tests::transmission_tracker_list_arg_accepts_common_shapes ... ok
test tests::transmission_torrent_get_projects_registry ... ok
test tests::transmission_session_set_persists_broad_compat_settings_without_engine ... ok
test tests::transmission_response_field_matrix_is_present ... ok
test tests::transmission_torrent_set_updates_labels_and_download_dir ... ok
test tests::transmission_torrent_get_rejects_malformed_projection_arguments ... ok
test tests::transmission_common_mutators_are_accepted ... ok
test tests::transmission_torrent_set_limits_roundtrip_without_engine ... ok
test tests::transmission_torrent_group_assignment_roundtrips_in_torrent_get ... ok
test tests::transmission_batches_are_bounded_before_response_allocation ... ok

test result: ok. 41 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_api_transmission

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

```

## Deluge JSON-RPC facade matrix

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.07s
     Running unittests src/lib.rs (target/debug/deps/rt_api_deluge-12cacd08bda27100)

running 26 tests
test tests::deluge_api_snapshot_estimates_scale_with_torrent_count ... ok
test tests::deluge_options_project_to_engine_limits ... ok
test tests::deluge_peer_projection_uses_native_snapshots ... ok
test tests::deluge_mutator_parsers_accept_client_shapes ... ok
test tests::deluge_state_projects_active_recheck_as_checking ... ok
test tests::deluge_projection_arguments_reject_malformed_filters_and_fields ... ok
test tests::deluge_torrent_data_decoder_accepts_data_urls_and_unpadded_base64 ... ok
test tests::deluge_shutdown_notifies_daemon ... ok
test tests::deluge_tracker_projection_uses_persisted_engine_state ... ok
test tests::deluge_file_probe_returns_array_shape ... ok
test tests::deluge_label_mutation_requires_native_engine ... ok
test tests::deluge_router_enforces_configured_token_and_preserves_login_body ... ok
test tests::deluge_torrent_options_require_native_engine ... ok
test tests::deluge_idempotency_key_replays_mutation_and_rejects_reuse ... ok
test tests::deluge_torrent_status_honors_requested_fields ... ok
test tests::deluge_update_ui_honors_requested_fields ... ok
test tests::deluge_advertised_method_list_matches_probe_matrix ... ok
test tests::deluge_torrent_status_field_matrix_is_present ... ok
test tests::deluge_url_download_returns_stateful_safe_token ... ok
test tests::deluge_torrents_status_honors_filter_dictionary ... ok
test tests::deluge_url_download_tokens_are_one_shot_and_report_engine_failure ... ok
test tests::deluge_plugin_cache_and_notification_shapes_are_structured ... ok
test tests::deluge_update_ui_projects_registry ... ok
test tests::deluge_unsupported_plugin_writes_fail_closed ... ok
test tests::deluge_auth_and_config_are_supported ... ok
test tests::deluge_web_add_torrents_reports_unavailable_engine_per_item ... ok

test result: ok. 26 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s

   Doc-tests rt_api_deluge

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

```

## rTorrent XMLRPC facade matrix

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.06s
     Running unittests src/lib.rs (target/debug/deps/rt_api_rtorrent-87fd6c13aa88b23b)

running 26 tests
test tests::file_tracker_and_peer_projectors_use_native_snapshot_fields ... ok
test tests::method_matrix_advertises_representative_rtorrent_families ... ok
test tests::file_multicall_projects_registry_fallback_fields ... ok
test tests::rtorrent_api_snapshot_estimate_scales_with_torrents_and_commands ... ok
test tests::custom_fields_roundtrip ... ok
test tests::download_reads_project_registry_state ... ok
test tests::configured_embedded_xmlrpc_state_rejects_missing_or_wrong_token ... ok
test tests::global_throttle_setters_roundtrip_without_engine ... ok
test tests::raw_torrent_load_accepts_xmlrpc_base64_payload ... ok
test tests::multicall_honors_view_and_rejects_malformed_commands ... ok
test tests::value_to_json_preserves_rtorrent_types ... ok
test tests::rtorrent_mutators_reject_missing_or_malformed_values ... ok
test tests::xml_projection_saturates_unrepresentable_unsigned_values ... ok
test tests::tracker_announce_requires_info_hash ... ok
test tests::tracker_announce_fails_closed_without_engine ... ok
test tests::xmlrpc_parser_accepts_array_struct_base64_and_nil_shapes ... ok
test tests::xml_value_parser_accepts_nested_arrays_structs_base64_and_nil ... ok
test tests::torrent_local_state_is_case_insensitive_and_erased_with_torrent ... ok
test tests::xmlrpc_fixture_roundtrips ... ok
test tests::view_size_projects_registry_backed_compat_views ... ok
test tests::multicall_returns_rtorrent_row_shape ... ok
test tests::lifecycle_fallback_mutates_registry_and_rejects_missing_torrents ... ok
test tests::xmlrpc_method_list_and_detail_multicalls_have_stable_shapes ... ok
test tests::torrent_throttle_setters_roundtrip_without_engine ... ok
test tests::path_load_rejects_unsupported_filesystem_boundary ... ok
test tests::magnet_load_and_erase_update_registry ... ok

test result: ok. 26 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/library_entry_point.rs (target/debug/deps/library_entry_point-bbf28af298d41e62)

running 2 tests
test public_library_entry_point_enforces_embedded_credentials ... ok
test public_library_entry_point_executes_xmlrpc_without_http_server ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests rt_api_rtorrent

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

```

Overall status: PASS
