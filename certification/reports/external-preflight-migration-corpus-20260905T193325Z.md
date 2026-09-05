# TorrentNG Migration Corpus Certification

- Date UTC: 2026-09-05T19:33:25Z
- Host: kspld0
- Commit: ec17d79
- Corpus directory: /home/keith/Documents/code/TorrentNG/testdata/migration-corpus
- Strict corpus required: 1

## Synthetic Import/Apply Baseline

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.07s
     Running unittests src/lib.rs (target/debug/deps/rt_migrate-f951ef08dfeaf216)

running 51 tests
test export::tests::format_parsing_accepts_aliases ... ok
test tests::auxiliary_classification_ignores_hash_coincidences_in_filenames ... ok
test tests::aggregate_json_resume_matches_base32_info_hash_entries ... ok
test tests::bencoded_file_progress_imports_to_native_file_rows ... ok
test tests::bencoded_file_selection_imports_to_native_file_rows ... ok
test tests::bencoded_lifecycle_state_imports_to_native_torrent_row ... ok
test tests::bencoded_tracker_activity_imports_to_native_tracker_rows ... ok
test tests::file_hints_do_not_trust_final_symlinks ... ok
test tests::broad_sources_are_scannable_metadata_first ... ok
test tests::biglybt_downloads_config_matches_hex_entries ... ok
test tests::fastresume_apply_persists_imported_state_and_summary ... ok
test tests::json_lifecycle_state_imports_to_native_torrent_row ... ok
test tests::json_file_selection_imports_to_native_file_rows ... ok
test tests::json_tracker_activity_imports_to_native_tracker_rows ... ok
test tests::json_file_progress_imports_to_native_file_rows ... ok
test tests::oversized_resume_sidecar_is_skipped_with_warning ... ok
test tests::path_remap_uses_longest_matching_prefix ... ok
test tests::padding_files_are_never_marked_wanted ... ok
test tests::qbit_dry_run_preserves_resume_metadata ... ok
test tests::dry_run_preserves_auxiliary_client_artifacts_separately ... ok
test tests::qbit_libtorrent2_resume_unpacks_bit_packed_pieces_field ... ok
test tests::path_remap_updates_db_rows_and_file_hint_trust ... ok
test tests::recursive_scan_does_not_follow_directory_symlinks ... ok
test tests::qbit_libtorrent_resume_imports_piece_state ... ok
test tests::require_verification_downgrades_imported_valid_pieces ... ok
test tests::tixati_proprietary_state_stays_verification_first ... ok
test tests::rtorrent_pairs_hash_torrent_rtorrent_sidecar ... ok
test tests::rtorrent_multi_file_directory_already_includes_torrent_name ... ok
test tests::transmission_dry_run_reads_bencoded_resume ... ok
test tests::short_piece_state_is_padded_and_reported ... ok
test tests::rtorrent_multi_file_directory_renamed_by_external_tool_stays_safe_not_silently_broken ... ok
test tests::utorrent_bitfield_resume_imports_piece_state_under_trust_hints ... ok
test tests::rtorrent_dry_run_reports_missing_resume ... ok
test tests::utorrent_resume_dat_matches_raw_info_hash_entries ... ok
test tests::rtorrent_complete_resume_synthesizes_seed_piece_state ... ok
test tests::rtorrent_single_file_directory_is_left_unchanged ... ok
test tests::import_source_matrix_preserves_common_json_resume_fields ... ok
test tests::partial_piece_blocks_are_sorted_deduped_and_bounded ... ok
test tests::import_plan_applies_native_db_rows ... ok
test tests::native_import_applies_db_and_fastresume_together ... ok
test export::tests::oversized_blob_is_skipped_before_reading_contents ... ok
test export::tests::generic_export_copies_torrent_and_manifest ... ok
test export::tests::rtorrent_export_partial_is_metadata_only ... ok
test export::tests::missing_blob_is_skipped_not_fatal ... ok
test export::tests::rtorrent_export_complete_is_recheck_free ... ok
test export::tests::libtorrent_export_round_trips_through_qbittorrent_importer ... ok
test export::tests::transmission_export_round_trips ... ok
test export::tests::malformed_database_hash_is_skipped_before_path_join ... ok
test export::tests::utorrent_and_biglybt_aggregates_round_trip ... ok
test tests::bencoded_import_source_matrix_preserves_client_specific_aliases ... ok
test tests::native_apply_matrix_persists_common_resume_fields_for_all_sources ... ok

test result: ok. 51 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.08s

     Running tests/round_trip_matrix.rs (target/debug/deps/round_trip_matrix-3c469a2986f7bbc6)

running 7 tests
test import_matrix_complete_and_partial_isos ... ok
test production_shape_bep47_padding_file_not_wanted_real_files_trusted ... ok
test production_shape_directory_renamed_by_external_tool_stays_safe_metadata_only ... ok
test production_shape_directory_equals_content_folder_bytes_preserved_and_trusted ... ok
test generic_export_is_universal_exit ... ok
test round_trip_matrix_preserves_state ... ok
test export_matrix_fidelity_and_layout ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s

     Running tests/scale.rs (target/debug/deps/scale-1808fdfa8abf6165)

running 1 test
test qbit_15k_dry_run_import_is_certified ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.93s

   Doc-tests rt_migrate

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

```

## Bidirectional Round-Trip Matrix

Every supported client x import/export/round-trip direction, with
ISO-shaped single-file and multi-file fixtures and complete + partial
piece state. Source: `crates/rt-migrate/tests/round_trip_matrix.rs`.

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.04s
     Running tests/round_trip_matrix.rs (target/debug/deps/round_trip_matrix-3c469a2986f7bbc6)

running 7 tests
test import_matrix_complete_and_partial_isos ... ok
test production_shape_directory_renamed_by_external_tool_stays_safe_metadata_only ... ok
test production_shape_bep47_padding_file_not_wanted_real_files_trusted ... ok
test production_shape_directory_equals_content_folder_bytes_preserved_and_trusted ... ok
test generic_export_is_universal_exit ... ok
test round_trip_matrix_preserves_state ... ok
test export_matrix_fidelity_and_layout ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s

```

## Exported Corpus Coverage

| Source family | Result | Evidence files | Evidence root |
|---|---|---:|---|
| qbittorrent | PASS | 1 | /home/keith/Documents/code/TorrentNG/testdata/migration-corpus/qbittorrent |
| transmission | PASS | 1 | /home/keith/Documents/code/TorrentNG/testdata/migration-corpus/transmission |
| deluge | PASS | 1 | /home/keith/Documents/code/TorrentNG/testdata/migration-corpus/deluge |
| utorrent | PASS | 1 | /home/keith/Documents/code/TorrentNG/testdata/migration-corpus/utorrent |
| biglybt | PASS | 1 | /home/keith/Documents/code/TorrentNG/testdata/migration-corpus/biglybt |
| tixati | PASS | 1 | /home/keith/Documents/code/TorrentNG/testdata/migration-corpus/tixati |
| rtorrent | PASS | 1 | /home/keith/Documents/code/TorrentNG/testdata/migration-corpus/rtorrent |
| generic | PASS | 1 | /home/keith/Documents/code/TorrentNG/testdata/migration-corpus/generic |

## Corpus Manifest

- Manifest: /home/keith/Documents/code/TorrentNG/testdata/migration-corpus/manifest.toml
- Status: PASS
- Detail: /home/keith/Documents/code/TorrentNG/testdata/migration-corpus/manifest.toml

| Source family | Artifact | Source | Permission | SHA-256 |
|---|---|---|---|---|
| qbittorrent | qbittorrent/generated.fastresume | TorrentNG generated qBittorrent/libtorrent-style resume fixture | generated by TorrentNG test corpus; safe to commit | 2a29527633b4d09dfc03858133a23763f37025dfd539088fd9da9540eabf8744 |
| transmission | transmission/generated.resume | TorrentNG generated Transmission-style resume fixture | generated by TorrentNG test corpus; safe to commit | 648fbae32c095778c5580f737cbef519b8be4fd9d1c13e8cc52a64d48bb1ea44 |
| deluge | deluge/generated.state | TorrentNG generated Deluge-style state fixture | generated by TorrentNG test corpus; safe to commit | d760829e14f69c630809793487b8a1bc35cd5a9f674775dcda6308aae9cbfcb0 |
| utorrent | utorrent/resume.dat | TorrentNG generated uTorrent/BitTorrent Classic-style resume fixture | generated by TorrentNG test corpus; safe to commit | 3ec4c23f10503f47e0241f05011cfe9b0e5af152b1a46b557a061763d5f02360 |
| biglybt | biglybt/downloads.config | TorrentNG generated BiglyBT/Vuze-style config fixture | generated by TorrentNG test corpus; safe to commit | ef8b062b3c42b8a4d666ab7f7554b186e935f7cb3a1774226ba6f562ae50f491 |
| tixati | tixati/generated.config | TorrentNG generated Tixati-style config fixture | generated by TorrentNG test corpus; safe to commit | b6de385622b7061e58decadba3760add60201a7fe976cf11f71c8d4725a6cc30 |
| rtorrent | rtorrent/generated.torrent | TorrentNG generated rTorrent/session-style torrent fixture | generated by TorrentNG test corpus; safe to commit | 7c54f6880385036724a550e43ebd05c71a65efa02e3a09317fd3e64b47655b6b |
| generic | generic/generated.resume.json | TorrentNG generated generic JSON resume fixture | generated by TorrentNG test corpus; safe to commit | 450229e110a1aeed102d87f1c792ca5f192f9bc73388d2c34cc3c50dd0f8beae |

## Evidence Inventory

| Source family | File | SHA-256 |
|---|---|---|
| qbittorrent | testdata/migration-corpus/qbittorrent/generated.fastresume | 2a29527633b4d09dfc03858133a23763f37025dfd539088fd9da9540eabf8744 |
| transmission | testdata/migration-corpus/transmission/generated.resume | 648fbae32c095778c5580f737cbef519b8be4fd9d1c13e8cc52a64d48bb1ea44 |
| deluge | testdata/migration-corpus/deluge/generated.state | d760829e14f69c630809793487b8a1bc35cd5a9f674775dcda6308aae9cbfcb0 |
| utorrent | testdata/migration-corpus/utorrent/resume.dat | 3ec4c23f10503f47e0241f05011cfe9b0e5af152b1a46b557a061763d5f02360 |
| biglybt | testdata/migration-corpus/biglybt/downloads.config | ef8b062b3c42b8a4d666ab7f7554b186e935f7cb3a1774226ba6f562ae50f491 |
| tixati | testdata/migration-corpus/tixati/generated.config | b6de385622b7061e58decadba3760add60201a7fe976cf11f71c8d4725a6cc30 |
| rtorrent | testdata/migration-corpus/rtorrent/generated.torrent | 7c54f6880385036724a550e43ebd05c71a65efa02e3a09317fd3e64b47655b6b |
| generic | testdata/migration-corpus/generic/generated.resume.json | 450229e110a1aeed102d87f1c792ca5f192f9bc73388d2c34cc3c50dd0f8beae |

## Required Layout

```text
testdata/migration-corpus/qbittorrent/
testdata/migration-corpus/transmission/
testdata/migration-corpus/deluge/
testdata/migration-corpus/utorrent/
testdata/migration-corpus/biglybt/
testdata/migration-corpus/tixati/
testdata/migration-corpus/rtorrent/
testdata/migration-corpus/generic/
```

Place real exported client resume/config/torrent artifacts under each source family.
When artifacts are present, copy manifest.example.toml to manifest.toml and record source/version/permission metadata.
Strict release mode requires a manifest, family-confined declared artifacts, and declarations for every discovered evidence file.
Set TNG_REQUIRE_MIGRATION_CORPUS=1 to make missing source-family corpora fail this gate.

- Missing source families: 0

Overall status: PASS
