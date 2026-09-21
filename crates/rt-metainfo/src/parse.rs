use std::collections::{HashMap, HashSet};

use sha1::{Digest as Sha1Digest, Sha1};
use sha2::{Digest as Sha2Digest, Sha256};

use rt_bencode::BValue;
use rt_hash::merkle_root;
use rt_path::SafeRelPath;

use crate::{
    error::MetainfoError,
    types::{
        TorrentFileV1, TorrentFileV2, TorrentMeta, TorrentMetaV1, TorrentMetaV2,
        V2PieceLayerRequirement, V2PieceLayerRequirements,
    },
};

pub const MAX_TORRENT_BYTES: usize = 64 * 1024 * 1024;
/// Per-call allocation ceiling for convenience parsers that do not receive an
/// external memory-governor reservation callback.
pub const MAX_CONVENIENCE_METAINFO_ALLOCATION_BYTES: usize = 512 * 1024 * 1024;
const MAX_FILES: usize = 100_000;
// Keep accepted torrent paths within rt-storage's recursive tree-walk bound
// so a torrent cannot pass parsing and later fail only during move/delete.
const MAX_PATH_COMPONENTS: usize = 64;
// BEP 52 stores shared directory prefixes once but the runtime owns one path
// per file. Bound that expansion independently of the encoded metainfo size.
const MAX_TOTAL_FILE_PATH_COMPONENTS: usize = 500_000;
const MAX_TOTAL_FILE_PATH_BYTES: usize = 16 * 1024 * 1024;
const MAX_TRACKER_URLS: usize = 4096;
const MAX_TRACKER_TIERS: usize = 256;
const MAX_TRACKER_URL_BYTES: usize = 8192;
const MAX_TRACKER_URL_TOTAL_BYTES: usize = 4 * 1024 * 1024;
const MAX_WEBSEED_URLS: usize = 4096;
const MAX_WEBSEED_URL_BYTES: usize = 8192;
const MAX_PIECES: usize = 16_000_000;
const MAX_NAME_BYTES: usize = 4096;
const MAX_PATH_COMPONENT_BYTES: usize = 4096;
// Optional top-level text is copied into owned metadata and can be retained
// by every active API projection. Keep it useful for ordinary comments while
// preventing the decoder's much larger generic string limit from multiplying
// across concurrent engine preparations.
const MAX_METADATA_TEXT_BYTES: usize = 256 * 1024;

#[derive(Default)]
struct FilePathBudget {
    total_components: usize,
    total_bytes: usize,
}

impl FilePathBudget {
    fn reserve(&mut self, components: &[&str]) -> Result<(), MetainfoError> {
        let total_components = self.total_components.saturating_add(components.len());
        if total_components > MAX_TOTAL_FILE_PATH_COMPONENTS {
            return Err(MetainfoError::LimitExceeded {
                field: "total file path components",
                limit: MAX_TOTAL_FILE_PATH_COMPONENTS,
            });
        }

        let path_bytes = components
            .iter()
            .map(|component| component.len())
            .fold(components.len().saturating_sub(1), usize::saturating_add);
        let total_bytes = self.total_bytes.saturating_add(path_bytes);
        if total_bytes > MAX_TOTAL_FILE_PATH_BYTES {
            return Err(MetainfoError::LimitExceeded {
                field: "aggregate file path bytes",
                limit: MAX_TOTAL_FILE_PATH_BYTES,
            });
        }

        self.total_components = total_components;
        self.total_bytes = total_bytes;
        Ok(())
    }
}

#[derive(Default, Clone, Copy)]
struct FileProjectionEstimate {
    files: usize,
    paths: usize,
    path_components: usize,
    path_component_bytes: usize,
    layered_files: usize,
}

impl FileProjectionEstimate {
    fn add_path(&mut self, components: usize, component_bytes: usize, piece_layered: bool) {
        self.paths = self.paths.saturating_add(1).min(MAX_FILES);
        self.path_components = self
            .path_components
            .saturating_add(components)
            .min(MAX_TOTAL_FILE_PATH_COMPONENTS);
        self.path_component_bytes = self
            .path_component_bytes
            .saturating_add(component_bytes)
            .min(MAX_TOTAL_FILE_PATH_BYTES);
        if piece_layered {
            self.layered_files = self.layered_files.saturating_add(1).min(MAX_FILES);
        }
    }
}

fn estimate_v1_file_projection(info: &BValue<'_>, name: &[u8]) -> FileProjectionEstimate {
    let mut estimate = FileProjectionEstimate::default();
    match info.get(b"files") {
        Some(BValue::List(file_list)) => {
            if file_list.len() > MAX_FILES {
                return estimate;
            }
            // The v1 parser allocates its exact file vector before validating
            // each entry, so even a later malformed path retains this capacity
            // until parsing returns.
            estimate.files = file_list.len();
            for entry in file_list {
                let Some(BValue::List(path)) = entry.get(b"path") else {
                    continue;
                };
                if path.len().saturating_add(1) > MAX_PATH_COMPONENTS {
                    continue;
                }
                let mut components = 1usize; // The torrent name is the v1 root.
                let mut bytes = name.len();
                let mut valid = true;
                for part in path {
                    let Some(value) = part.as_bytes() else {
                        valid = false;
                        break;
                    };
                    if value.len() > MAX_PATH_COMPONENT_BYTES || std::str::from_utf8(value).is_err()
                    {
                        valid = false;
                        break;
                    }
                    if !value.is_empty() {
                        components = components.saturating_add(1);
                        bytes = bytes.saturating_add(value.len());
                    }
                }
                if valid {
                    estimate.add_path(components, bytes, false);
                }
            }
        }
        Some(_) => {}
        None => {
            if info
                .get(b"length")
                .and_then(BValue::as_int)
                .is_some_and(|length| length >= 0)
            {
                estimate.files = 1;
                estimate.add_path(1, name.len(), false);
            }
        }
    }
    estimate
}

fn estimate_v2_file_projection(info: &BValue<'_>, piece_length: u64) -> FileProjectionEstimate {
    let mut estimate = FileProjectionEstimate::default();
    if let Some(file_tree) = info.get(b"file tree") {
        estimate_v2_file_tree_node(file_tree, 0, 0, piece_length, &mut estimate);
    }
    estimate
}

fn estimate_v2_file_tree_node(
    node: &BValue<'_>,
    depth: usize,
    path_component_bytes: usize,
    piece_length: u64,
    estimate: &mut FileProjectionEstimate,
) {
    if estimate.files >= MAX_FILES || depth > MAX_PATH_COMPONENTS {
        return;
    }
    let BValue::Dict(entries) = node else {
        return;
    };

    if let Some(leaf) = node.get(b"") {
        if depth == 0 || entries.len() != 1 {
            return;
        }
        let Some(length) = leaf
            .get(b"length")
            .and_then(BValue::as_int)
            .and_then(|value| u64::try_from(value).ok())
        else {
            return;
        };
        let has_valid_root = match leaf.get(b"pieces root") {
            Some(value) => value.as_bytes().is_some_and(|bytes| bytes.len() == 32),
            None => length == 0,
        };
        if !has_valid_root {
            return;
        }
        let layered = length > piece_length;
        estimate.files = estimate.files.saturating_add(1).min(MAX_FILES);
        estimate.add_path(depth, path_component_bytes, layered);
        return;
    }

    for (key, child) in entries {
        if key.is_empty()
            || key.len() > MAX_PATH_COMPONENT_BYTES
            || std::str::from_utf8(key).is_err()
            || depth >= MAX_PATH_COMPONENTS
        {
            continue;
        }
        estimate_v2_file_tree_node(
            child,
            depth + 1,
            path_component_bytes.saturating_add(key.len()),
            piece_length,
            estimate,
        );
    }
}

fn estimate_text_copy(value: Option<&BValue<'_>>, limit: usize) -> (usize, usize) {
    let Some(bytes) = value.and_then(BValue::as_bytes) else {
        return (0, 0);
    };
    if bytes.len() > limit
        || std::str::from_utf8(bytes)
            .ok()
            .is_none_or(|text| text.trim().is_empty())
    {
        return (0, 0);
    }
    (bytes.len(), 1)
}

fn estimate_announce_list(root: &BValue<'_>, announce_count: usize) -> (usize, usize, usize) {
    let Some(BValue::List(tiers)) = root.get(b"announce-list") else {
        return (0, 0, 0);
    };
    if tiers.len() > MAX_TRACKER_TIERS {
        return (0, 0, 0);
    }

    let mut tier_count = 0usize;
    let mut url_count = announce_count;
    let mut url_bytes = 0usize;
    for tier in tiers {
        let BValue::List(urls) = tier else {
            continue;
        };
        let before = url_count;
        for url in urls {
            let Some(text) = url
                .as_bytes()
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .map(str::trim)
                .filter(|text| !text.is_empty())
            else {
                continue;
            };
            if text.len() > MAX_TRACKER_URL_BYTES || url_count >= MAX_TRACKER_URLS {
                break;
            }
            let Some(total) = url_bytes.checked_add(text.len()) else {
                break;
            };
            if total.saturating_add(url_bytes_for_announce(root)) > MAX_TRACKER_URL_TOTAL_BYTES {
                break;
            }
            url_count += 1;
            url_bytes = total;
        }
        if url_count > before {
            tier_count = tier_count.saturating_add(1).min(MAX_TRACKER_TIERS);
        }
    }
    (
        tier_count,
        url_count.saturating_sub(announce_count),
        url_bytes,
    )
}

fn url_bytes_for_announce(root: &BValue<'_>) -> usize {
    estimate_text_copy(root.get(b"announce"), MAX_TRACKER_URL_BYTES).0
}

fn estimate_webseeds(root: &BValue<'_>) -> (usize, usize) {
    let mut count = 0usize;
    let mut bytes_total = 0usize;
    let mut add = |value: &[u8]| {
        let Ok(text) = std::str::from_utf8(value) else {
            return;
        };
        let text = text.trim();
        if text.is_empty() || text.len() > MAX_WEBSEED_URL_BYTES || count >= MAX_WEBSEED_URLS {
            return;
        }
        count += 1;
        bytes_total = bytes_total.saturating_add(text.len());
    };
    match root.get(b"url-list") {
        Some(BValue::Bytes(bytes)) => add(bytes),
        Some(BValue::List(values)) => {
            for value in values {
                if let Some(bytes) = value.as_bytes() {
                    add(bytes);
                }
            }
        }
        _ => {}
    }
    (count, bytes_total)
}

fn estimate_piece_layer_bytes(root: &BValue<'_>) -> usize {
    let Some(BValue::Dict(entries)) = root.get(b"piece layers") else {
        return 0;
    };
    entries.iter().fold(0usize, |total, (_, value)| {
        total.saturating_add(value.as_bytes().map_or(0, <[u8]>::len))
    })
}

fn add_file_projection_bytes<T>(
    total: &mut usize,
    estimate: FileProjectionEstimate,
    growing: bool,
) {
    // A growing Vec can transiently hold both its old and replacement buffers.
    // Capacity doubles, so reserving three slots per accepted output item
    // covers the worst boundary where the final insertion triggers growth.
    let capacity_factor = if growing { 3 } else { 1 };
    *total = total.saturating_add(
        estimate
            .files
            .saturating_mul(std::mem::size_of::<T>())
            .saturating_mul(capacity_factor),
    );
    *total = total.saturating_add(
        estimate
            .path_components
            .saturating_mul(std::mem::size_of::<String>()),
    );
    *total = total.saturating_add(estimate.path_component_bytes.saturating_mul(2));

    // File-path collision validation builds a temporary sortable path vector.
    // Windows also materializes UTF-16 components for ordinal case folding.
    *total = total.saturating_add(
        estimate
            .paths
            .saturating_mul(4 * std::mem::size_of::<usize>()),
    );
    if cfg!(windows) {
        *total = total
            .saturating_add(
                estimate
                    .path_components
                    .saturating_mul(std::mem::size_of::<Vec<u16>>()),
            )
            .saturating_add(estimate.path_component_bytes.saturating_mul(2));
    }
    if estimate.paths > 1 {
        *total = total.saturating_add(
            MAX_PATH_COMPONENTS
                .saturating_mul(MAX_PATH_COMPONENT_BYTES)
                .saturating_add(MAX_PATH_COMPONENTS),
        );
    }
}

fn estimate_torrent_projection_allocation_bytes(
    root: &BValue<'_>,
    info: &BValue<'_>,
    is_v2: bool,
    is_hybrid: bool,
) -> usize {
    let mut total = 0usize;
    let (announce_bytes, announce_count) =
        estimate_text_copy(root.get(b"announce"), MAX_TRACKER_URL_BYTES);
    let (tier_count, tracker_count, tracker_bytes) = estimate_announce_list(root, announce_count);
    let (webseed_count, webseed_bytes) = estimate_webseeds(root);
    let (comment_bytes, _) = estimate_text_copy(root.get(b"comment"), MAX_METADATA_TEXT_BYTES);
    let (creator_bytes, _) = estimate_text_copy(root.get(b"created by"), MAX_METADATA_TEXT_BYTES);
    let name_bytes = info
        .get(b"name")
        .and_then(BValue::as_bytes)
        .filter(|bytes| bytes.len() <= MAX_NAME_BYTES && std::str::from_utf8(bytes).is_ok())
        .map_or(0, <[u8]>::len);
    let hybrid_factor = if is_hybrid { 2 } else { 1 };

    let owned_text_bytes = announce_bytes
        .saturating_add(tracker_bytes)
        .saturating_add(webseed_bytes)
        .saturating_add(comment_bytes)
        .saturating_add(creator_bytes)
        .saturating_add(name_bytes)
        .saturating_mul(hybrid_factor)
        .saturating_mul(2);
    total = total.saturating_add(owned_text_bytes);
    total = total.saturating_add(
        tier_count
            .saturating_mul(2)
            .saturating_mul(std::mem::size_of::<Vec<String>>())
            .saturating_mul(hybrid_factor),
    );
    total = total.saturating_add(
        tracker_count
            .saturating_mul(2)
            .saturating_mul(std::mem::size_of::<String>())
            .saturating_mul(hybrid_factor),
    );
    total = total.saturating_add(
        webseed_count
            .saturating_mul(2)
            .saturating_mul(std::mem::size_of::<String>())
            .saturating_mul(hybrid_factor),
    );
    total = total.saturating_add(estimate_hash_table_bytes::<&str, ()>(webseed_count));

    let piece_bytes = info
        .get(b"pieces")
        .and_then(BValue::as_bytes)
        .filter(|bytes| bytes.len().is_multiple_of(20) && bytes.len() / 20 <= MAX_PIECES)
        .map_or(0, <[u8]>::len);
    if !is_v2 || is_hybrid {
        total = total.saturating_add(piece_bytes);
        let v1 = estimate_v1_file_projection(
            info,
            info.get(b"name")
                .and_then(BValue::as_bytes)
                .unwrap_or_default(),
        );
        add_file_projection_bytes::<TorrentFileV1>(&mut total, v1, false);
        // v1 constructs a temporary borrowed-component vector for one file at
        // a time; only the largest single path can be live at once.
        if v1.paths > 0 {
            total = total.saturating_add(MAX_PATH_COMPONENTS * std::mem::size_of::<&str>());
        }
    }

    if is_v2 {
        let piece_length = info
            .get(b"piece length")
            .and_then(BValue::as_int)
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(0);
        let files = estimate_v2_file_projection(info, piece_length);
        add_file_projection_bytes::<TorrentFileV2>(&mut total, files, true);
        // walk_file_tree holds one borrowed path vector per recursion frame.
        if files.paths > 0 {
            total = total.saturating_add(
                (MAX_PATH_COMPONENTS * (MAX_PATH_COMPONENTS + 1) / 2)
                    .saturating_mul(std::mem::size_of::<&str>()),
            );
        }
        let piece_layer_bytes = estimate_piece_layer_bytes(root);
        total = total.saturating_add(piece_layer_bytes.saturating_mul(4));
        total = total.saturating_add(estimate_hash_table_bytes::<[u8; 32], (u64, usize)>(
            files.layered_files,
        ));
        total = total.saturating_add(estimate_hash_table_bytes::<[u8; 32], Vec<[u8; 32]>>(
            files.layered_files,
        ));
    }

    // Inline fields and the hybrid Box are small, but include their storage in
    // the same admission instead of relying on the fixed parser baseline.
    total.saturating_add(
        hybrid_factor
            .saturating_mul(std::mem::size_of::<TorrentMetaV1>())
            .saturating_add(std::mem::size_of::<TorrentMetaV2>()),
    )
}

fn estimate_hash_table_bytes<K, V>(entries: usize) -> usize {
    // Account for bucket slack and old+new tables during a growth/rehash.
    entries
        .saturating_mul(std::mem::size_of::<(K, V)>().saturating_add(1))
        .saturating_mul(5)
}

fn estimate_v2_requirements_allocation_bytes(info: &BValue<'_>, piece_length: u64) -> usize {
    let files = estimate_v2_file_projection(info, piece_length);
    let mut total = 0usize;
    add_file_projection_bytes::<TorrentFileV2>(&mut total, files, true);
    if files.paths > 0 {
        total = total.saturating_add(
            (MAX_PATH_COMPONENTS * (MAX_PATH_COMPONENTS + 1) / 2)
                .saturating_mul(std::mem::size_of::<&str>()),
        );
    }
    total = total.saturating_add(estimate_hash_table_bytes::<[u8; 32], (u64, usize)>(
        files.layered_files,
    ));
    total.saturating_add(
        files
            .layered_files
            .saturating_mul(std::mem::size_of::<V2PieceLayerRequirement>())
            .saturating_mul(3),
    )
}

fn reserve_projection_allocation_bytes(
    bytes: usize,
    reserve: &mut impl FnMut(usize) -> bool,
) -> Result<(), MetainfoError> {
    if bytes > 0 && !reserve(bytes) {
        return Err(MetainfoError::Bencode(
            rt_bencode::BencodeError::AllocationBudgetExceeded { bytes },
        ));
    }
    Ok(())
}

/// Parse a `.torrent` file from raw bytes. Handles v1, v2 (BEP 52), and hybrid.
///
/// This convenience API caps parser allocations at
/// [`MAX_CONVENIENCE_METAINFO_ALLOCATION_BYTES`]. Callers that need shared
/// process-wide admission should use [`parse_torrent_with_allocation_reservation`].
pub fn parse_torrent(raw: &[u8]) -> Result<TorrentMeta, MetainfoError> {
    parse_torrent_with_allocation_limit(raw, MAX_CONVENIENCE_METAINFO_ALLOCATION_BYTES)
}

fn bounded_allocation_reservation(limit: usize) -> impl FnMut(usize) -> bool {
    let mut reserved = 0usize;
    move |additional| {
        let Some(next) = reserved.checked_add(additional) else {
            return false;
        };
        if next > limit {
            return false;
        }
        reserved = next;
        true
    }
}

fn parse_torrent_with_allocation_limit(
    raw: &[u8],
    limit: usize,
) -> Result<TorrentMeta, MetainfoError> {
    let mut reserve = bounded_allocation_reservation(limit);
    parse_torrent_with_allocation_reservation(raw, &mut reserve)
}

/// Parse a torrent while reserving decoded collection capacity and retained
/// raw-metainfo copies before those allocations are made. This entry point has
/// no built-in aggregate cap; the caller must enforce its own admission policy.
pub fn parse_torrent_with_allocation_reservation(
    raw: &[u8],
    reserve: &mut impl FnMut(usize) -> bool,
) -> Result<TorrentMeta, MetainfoError> {
    if raw.len() > MAX_TORRENT_BYTES {
        return Err(MetainfoError::LimitExceeded {
            field: "torrent bytes",
            limit: MAX_TORRENT_BYTES,
        });
    }

    let (val, info_span) =
        rt_bencode::decode_torrent_info_span_with_allocation_reservation(raw, reserve)?;

    let root = match &val {
        BValue::Dict(_) => &val,
        _ => return Err(MetainfoError::MissingField("root dict")),
    };

    let info = root
        .get(b"info")
        .ok_or(MetainfoError::MissingField("info"))?;

    let info_bytes = if let Some(span) = info_span {
        &raw[span]
    } else {
        return Err(MetainfoError::MissingField("info span"));
    };

    let meta_version = info.get(b"meta version").and_then(|v| v.as_int());
    let has_file_tree = info.get(b"file tree").is_some();
    let has_pieces = info.get(b"pieces").is_some();

    // BEP 52 requires implementations to reject a newer metadata version
    // before interpreting any of its fields. Otherwise a future torrent that
    // happens to contain legacy-looking fields can be silently misread as a
    // v1 torrent.
    if let Some(version) = meta_version {
        if version < 0 {
            return Err(MetainfoError::InvalidIntegerValue {
                field: "meta version",
                value: version,
            });
        }
        if version > 2 {
            return Err(MetainfoError::UnsupportedMetaVersion(version));
        }
        if version == 2 && !has_file_tree {
            return Err(MetainfoError::MissingField("file tree"));
        }
    }
    if has_file_tree && meta_version != Some(2) {
        return Err(match meta_version {
            Some(version) => MetainfoError::UnsupportedMetaVersion(version),
            None => MetainfoError::MissingField("meta version"),
        });
    }

    let is_v2 = meta_version == Some(2) && has_file_tree;
    let is_hybrid = is_v2 && has_pieces;
    reserve_projection_allocation_bytes(
        estimate_torrent_projection_allocation_bytes(root, info, is_v2, is_hybrid),
        reserve,
    )?;

    let announce = parse_announce(root)?;
    let announce_list = parse_announce_list(
        root,
        usize::from(announce.is_some()),
        announce.as_deref().map_or(0, str::len),
    )?;
    let webseeds = parse_webseeds(root)?;
    let comment = parse_optional_string(root, b"comment")?;
    let created_by = parse_optional_string(root, b"created by")?;
    let creation_date = root.get(b"creation date").and_then(|v| v.as_int());

    let name = get_string(info, b"name", "name", MAX_NAME_BYTES)?;
    if name.is_empty() {
        return Err(MetainfoError::ZeroLengthName);
    }

    let piece_length = get_positive_u64(info, b"piece length", "piece length")?;
    if piece_length > u32::MAX as u64 {
        return Err(MetainfoError::InvalidPieceLength(piece_length));
    }
    if is_v2 && (piece_length < 16 * 1024 || !piece_length.is_power_of_two()) {
        return Err(MetainfoError::InvalidPieceLength(piece_length));
    }

    let private = match info.get(b"private") {
        None => false,
        Some(value) => match value.as_int() {
            Some(0) => false,
            Some(1) => true,
            Some(value) => {
                return Err(MetainfoError::InvalidIntegerValue {
                    field: "private",
                    value,
                });
            }
            None => return Err(MetainfoError::InvalidFieldType("private")),
        },
    };

    if is_v2 && has_pieces {
        // Hybrid: compute both infohashes
        let info_hash_v1: [u8; 20] = {
            let mut h = Sha1::new();
            h.update(info_bytes);
            h.finalize().into()
        };
        let info_hash_v2: [u8; 32] = {
            let mut h = Sha256::new();
            h.update(info_bytes);
            h.finalize().into()
        };

        let pieces = parse_piece_hashes(info)?;
        let files_v1 = parse_files_v1(info, &name)?;
        validate_file_path_collisions(
            files_v1
                .iter()
                .filter(|file| !file.pad)
                .map(|file| &file.path),
        )?;
        let files_v2 = parse_file_tree(info, piece_length)?;
        validate_file_path_collisions(
            files_v2
                .iter()
                .filter(|file| !file.pad)
                .map(|file| &file.path),
        )?;
        let piece_layers = parse_piece_layers(root, &files_v2, piece_length)?;
        validate_piece_count(&pieces, &files_v1, piece_length)?;
        let raw_v1 = copy_raw_with_allocation_reservation(raw, reserve)?;
        let raw_v2 = copy_raw_with_allocation_reservation(raw, reserve)?;

        let meta_v1 = TorrentMetaV1 {
            info_hash: info_hash_v1,
            announce: announce.clone(),
            announce_list: announce_list.clone(),
            webseeds: webseeds.clone(),
            comment: comment.clone(),
            created_by: created_by.clone(),
            creation_date,
            name: name.clone(),
            piece_length,
            pieces,
            files: files_v1,
            private,
            raw: raw_v1,
        };
        let meta_v2 = TorrentMetaV2 {
            info_hash_v2,
            announce,
            announce_list,
            webseeds,
            comment,
            created_by,
            creation_date,
            name,
            piece_length,
            files: files_v2,
            piece_layers,
            private,
            raw: raw_v2,
        };
        validate_hybrid_file_layout(&meta_v1, &meta_v2)?;

        return Ok(TorrentMeta::Hybrid(Box::new(meta_v1), meta_v2));
    }

    if is_v2 {
        // Pure v2
        let info_hash_v2: [u8; 32] = {
            let mut h = Sha256::new();
            h.update(info_bytes);
            h.finalize().into()
        };
        let files_v2 = parse_file_tree(info, piece_length)?;
        validate_file_path_collisions(
            files_v2
                .iter()
                .filter(|file| !file.pad)
                .map(|file| &file.path),
        )?;
        let piece_layers = parse_piece_layers(root, &files_v2, piece_length)?;
        let raw_v2 = copy_raw_with_allocation_reservation(raw, reserve)?;
        return Ok(TorrentMeta::V2(TorrentMetaV2 {
            info_hash_v2,
            announce,
            announce_list,
            webseeds,
            comment,
            created_by,
            creation_date,
            name,
            piece_length,
            files: files_v2,
            piece_layers,
            private,
            raw: raw_v2,
        }));
    }

    // v1
    let info_hash: [u8; 20] = {
        let mut h = Sha1::new();
        h.update(info_bytes);
        h.finalize().into()
    };
    let pieces = parse_piece_hashes(info)?;
    let files = parse_files_v1(info, &name)?;
    validate_file_path_collisions(files.iter().filter(|file| !file.pad).map(|file| &file.path))?;
    validate_piece_count(&pieces, &files, piece_length)?;
    let raw_v1 = copy_raw_with_allocation_reservation(raw, reserve)?;

    Ok(TorrentMeta::V1(TorrentMetaV1 {
        info_hash,
        announce,
        announce_list,
        webseeds,
        comment,
        created_by,
        creation_date,
        name,
        piece_length,
        pieces,
        files,
        private,
        raw: raw_v1,
    }))
}

fn copy_raw_with_allocation_reservation(
    raw: &[u8],
    reserve: &mut impl FnMut(usize) -> bool,
) -> Result<Vec<u8>, MetainfoError> {
    if !reserve(raw.len()) {
        return Err(MetainfoError::Bencode(
            rt_bencode::BencodeError::AllocationBudgetExceeded { bytes: raw.len() },
        ));
    }
    let mut copy = Vec::new();
    copy.try_reserve_exact(raw.len())
        .map_err(|_| MetainfoError::Bencode(rt_bencode::BencodeError::AllocationFailed))?;
    copy.extend_from_slice(raw);
    Ok(copy)
}

/// Validate that the v1 and v2 views of a hybrid torrent describe the same
/// payload files in the same order and at the same piece boundaries.
///
/// V1 padding files are synthetic zero bytes used to align the next real file;
/// BEP 52 represents those gaps implicitly through `piece_offset` instead.
///
/// The v2 tree may either be rootless or include the v1 torrent name as its
/// root. Older creators also omitted the otherwise unnecessary final v1 pad
/// file, so both an unpadded end and a correctly piece-aligned padded end are
/// accepted.
pub fn validate_hybrid_file_layout(
    v1: &TorrentMetaV1,
    v2: &TorrentMetaV2,
) -> Result<(), MetainfoError> {
    if v1.piece_length == 0 || v1.piece_length != v2.piece_length {
        return Err(MetainfoError::InconsistentHybridLayout(
            "v1 and v2 piece lengths differ or are zero",
        ));
    }
    if v1.name != v2.name {
        return Err(MetainfoError::InconsistentHybridLayout(
            "v1 and v2 torrent names differ",
        ));
    }

    validate_file_path_collisions(
        v1.files
            .iter()
            .filter(|file| !file.pad)
            .map(|file| &file.path),
    )?;
    validate_file_path_collisions(
        v2.files
            .iter()
            .filter(|file| !file.pad)
            .map(|file| &file.path),
    )?;

    let piece_length = v1.piece_length;
    let mut v1_stream_offset = 0u64;
    let mut v1_payload_length = 0u64;
    let mut v2_stream_offset = 0u64;
    let mut v2_logical_end = 0u64;

    for (index, file) in v2.files.iter().enumerate() {
        if usize::try_from(file.index).ok() != Some(index) {
            return Err(MetainfoError::InconsistentHybridLayout(
                "v2 file indexes are not contiguous",
            ));
        }
        if file.pad {
            return Err(MetainfoError::InconsistentHybridLayout(
                "v2 file tree contains an explicit padding file",
            ));
        }
        if file.offset != v2_stream_offset {
            return Err(MetainfoError::InconsistentHybridLayout(
                "v2 file offsets are not contiguous",
            ));
        }
        let expected_piece_offset = align_piece_offset(v2_logical_end, piece_length)?;
        if file.piece_offset != expected_piece_offset {
            return Err(MetainfoError::InconsistentHybridLayout(
                "v2 file piece offsets are not correctly aligned",
            ));
        }
        v2_stream_offset = v2_stream_offset
            .checked_add(file.length)
            .ok_or(MetainfoError::IntegerOverflow("hybrid v2 payload length"))?;
        if file.length > 0 {
            v2_logical_end = file
                .piece_offset
                .checked_add(file.length)
                .ok_or(MetainfoError::IntegerOverflow("hybrid v2 logical length"))?;
        }
    }

    let is_multifile_v1 = v1.files.iter().any(|file| file.path.components().len() > 1);
    let mut path_layout_rooted = None;
    let mut v2_files = v2.files.iter();

    for (index, file) in v1.files.iter().enumerate() {
        if usize::try_from(file.index).ok() != Some(index) {
            return Err(MetainfoError::InconsistentHybridLayout(
                "v1 file indexes are not contiguous",
            ));
        }
        if file.offset != v1_stream_offset {
            return Err(MetainfoError::InconsistentHybridLayout(
                "v1 file offsets are not contiguous",
            ));
        }
        v1_stream_offset = v1_stream_offset
            .checked_add(file.length)
            .ok_or(MetainfoError::IntegerOverflow("hybrid v1 stream length"))?;

        if file.pad {
            continue;
        }

        let Some(v2_file) = v2_files.next() else {
            return Err(MetainfoError::InconsistentHybridLayout(
                "v1 contains more payload files than v2",
            ));
        };
        if file.length != v2_file.length {
            return Err(MetainfoError::InconsistentHybridLayout(
                "v1 and v2 payload file lengths differ",
            ));
        }

        let v1_path = file.path.components();
        let v2_path = v2_file.path.components();
        let rooted_match = v1_path == v2_path;
        let rootless_match = is_multifile_v1
            && v1_path
                .first()
                .is_some_and(|component| component == &v1.name)
            && v1_path.get(1..) == Some(v2_path);
        let this_layout_rooted = if rooted_match {
            true
        } else if rootless_match {
            false
        } else {
            return Err(MetainfoError::InconsistentHybridLayout(
                "v1 and v2 payload file paths differ",
            ));
        };
        if path_layout_rooted.is_some_and(|rooted| rooted != this_layout_rooted) {
            return Err(MetainfoError::InconsistentHybridLayout(
                "v2 file tree mixes rooted and rootless paths",
            ));
        }
        path_layout_rooted = Some(this_layout_rooted);

        if file.length > 0 && file.offset != v2_file.piece_offset {
            return Err(MetainfoError::InconsistentHybridLayout(
                "v1 padding does not align payload files to v2 piece boundaries",
            ));
        }
        v1_payload_length = v1_payload_length
            .checked_add(file.length)
            .ok_or(MetainfoError::IntegerOverflow("hybrid payload length"))?;
    }

    if v2_files.next().is_some() {
        return Err(MetainfoError::InconsistentHybridLayout(
            "v2 contains more payload files than v1",
        ));
    }
    if v1_payload_length == 0 {
        return Err(MetainfoError::InconsistentHybridLayout(
            "hybrid torrent contains no non-empty payload",
        ));
    }

    let aligned_v2_end = align_piece_offset(v2_logical_end, piece_length)?;
    if v1_stream_offset != v2_logical_end && v1_stream_offset != aligned_v2_end {
        return Err(MetainfoError::InconsistentHybridLayout(
            "v1 trailing padding does not match the v2 logical end",
        ));
    }

    Ok(())
}

/// Return the exact bencoded `info` dictionary bytes used for v1 infohashes
/// and BEP 9 metadata exchange.
pub fn torrent_info_bytes(raw: &[u8]) -> Result<Vec<u8>, MetainfoError> {
    let mut reserve = bounded_allocation_reservation(MAX_CONVENIENCE_METAINFO_ALLOCATION_BYTES);
    torrent_info_bytes_with_allocation_reservation(raw, &mut reserve)
}

pub fn torrent_info_bytes_with_allocation_reservation(
    raw: &[u8],
    reserve: &mut impl FnMut(usize) -> bool,
) -> Result<Vec<u8>, MetainfoError> {
    if raw.len() > MAX_TORRENT_BYTES {
        return Err(MetainfoError::LimitExceeded {
            field: "torrent bytes",
            limit: MAX_TORRENT_BYTES,
        });
    }
    let (_, info_span) =
        rt_bencode::decode_torrent_info_span_with_allocation_reservation(raw, reserve)?;
    let info_span = info_span.ok_or(MetainfoError::MissingField("info span"))?;
    copy_raw_with_allocation_reservation(&raw[info_span], reserve)
}

/// Extract the piece-layer work needed to complete a v2 or hybrid magnet.
///
/// BEP 9 transfers only the exact `info` dictionary. BEP 52 stores the
/// piece-layer hashes for files larger than one piece in the top-level
/// metainfo, so a magnet worker needs this bounded view before it can request
/// those hashes over the v2 peer protocol. The same file-tree validation used
/// by [`parse_torrent`] is reused here; the only intentionally missing input
/// is the top-level `piece layers` dictionary itself.
pub fn v2_piece_layer_requirements(
    info_bytes: &[u8],
) -> Result<Option<V2PieceLayerRequirements>, MetainfoError> {
    let mut reserve = bounded_allocation_reservation(MAX_CONVENIENCE_METAINFO_ALLOCATION_BYTES);
    v2_piece_layer_requirements_with_allocation_reservation(info_bytes, &mut reserve)
}

pub fn v2_piece_layer_requirements_with_allocation_reservation(
    info_bytes: &[u8],
    reserve: &mut impl FnMut(usize) -> bool,
) -> Result<Option<V2PieceLayerRequirements>, MetainfoError> {
    if info_bytes.len() > MAX_TORRENT_BYTES {
        return Err(MetainfoError::LimitExceeded {
            field: "info bytes",
            limit: MAX_TORRENT_BYTES,
        });
    }
    let info = rt_bencode::decode_with_allocation_reservation(info_bytes, reserve)?;
    let BValue::Dict(_) = &info else {
        return Err(MetainfoError::InvalidFieldType("info dict"));
    };
    let meta_version = info.get(b"meta version").and_then(|value| value.as_int());
    let has_file_tree = info.get(b"file tree").is_some();
    if let Some(version) = meta_version {
        if version < 0 {
            return Err(MetainfoError::InvalidIntegerValue {
                field: "meta version",
                value: version,
            });
        }
        if version > 2 {
            return Err(MetainfoError::UnsupportedMetaVersion(version));
        }
        if version == 2 && !has_file_tree {
            return Err(MetainfoError::MissingField("file tree"));
        }
    }
    if has_file_tree && meta_version != Some(2) {
        return Err(match meta_version {
            Some(version) => MetainfoError::UnsupportedMetaVersion(version),
            None => MetainfoError::MissingField("meta version"),
        });
    }
    if meta_version != Some(2) {
        return Ok(None);
    }

    let piece_length = get_positive_u64(&info, b"piece length", "piece length")?;
    if piece_length > u32::MAX as u64 {
        return Err(MetainfoError::InvalidPieceLength(piece_length));
    }
    if piece_length < 16 * 1024 || !piece_length.is_power_of_two() {
        return Err(MetainfoError::InvalidPieceLength(piece_length));
    }

    reserve_projection_allocation_bytes(
        estimate_v2_requirements_allocation_bytes(&info, piece_length),
        reserve,
    )?;
    let files = parse_file_tree(&info, piece_length)?;
    validate_file_path_collisions(files.iter().filter(|file| !file.pad).map(|file| &file.path))?;
    let mut requirements = Vec::new();
    let mut seen_roots = HashMap::<[u8; 32], (u64, usize)>::new();
    for file in files {
        if file.length <= piece_length {
            continue;
        }
        let pieces_root = file
            .pieces_root
            .ok_or(MetainfoError::MissingField("pieces root"))?;
        let hash_count = file
            .length
            .checked_add(piece_length - 1)
            .ok_or(MetainfoError::IntegerOverflow("piece layer count"))?
            / piece_length;
        let hash_count = usize::try_from(hash_count)
            .map_err(|_| MetainfoError::IntegerOverflow("piece layer count"))?;
        if hash_count > MAX_PIECES {
            return Err(MetainfoError::LimitExceeded {
                field: "piece layers",
                limit: MAX_PIECES,
            });
        }
        if let Some(&(previous_length, previous_hash_count)) = seen_roots.get(&pieces_root) {
            if previous_length != file.length || previous_hash_count != hash_count {
                return Err(MetainfoError::InvalidPieceLayer(
                    "same pieces root has conflicting file requirements",
                ));
            }
            // A piece layer is keyed by pieces root. Identical files share
            // that layer and should require only one network fetch.
            continue;
        }
        seen_roots.insert(pieces_root, (file.length, hash_count));
        requirements.push(V2PieceLayerRequirement {
            pieces_root,
            file_length: file.length,
            hash_count,
        });
    }

    Ok(Some(V2PieceLayerRequirements {
        piece_length,
        files: requirements,
    }))
}

/// Parse a v2 `file tree` dict into a flat list of files.
/// BEP 52 file tree: nested dicts where leaves have `{"": {"length": N, "pieces root": <bytes>}}`.
fn parse_file_tree(
    info: &BValue<'_>,
    piece_length: u64,
) -> Result<Vec<TorrentFileV2>, MetainfoError> {
    let file_tree = info
        .get(b"file tree")
        .ok_or(MetainfoError::MissingField("file tree"))?;

    let mut files = Vec::new();
    let mut offset = 0u64;
    let mut piece_offset = 0u64;
    let mut path_budget = FilePathBudget::default();
    // `name` is advisory in BEP 52. The tree itself is rootless and may
    // optionally contain a directory with the same name; adding `name`
    // unconditionally turns a standard single-file tree into a wrong path.
    walk_file_tree(
        file_tree,
        &[],
        &mut files,
        &mut offset,
        &mut piece_offset,
        piece_length,
        &mut path_budget,
    )?;

    if files.is_empty() {
        return Err(MetainfoError::MissingField("file tree (empty)"));
    }
    Ok(files)
}

fn walk_file_tree<'a>(
    node: &BValue<'a>,
    path_components: &[&str],
    out: &mut Vec<TorrentFileV2>,
    offset: &mut u64,
    piece_offset: &mut u64,
    piece_length: u64,
    path_budget: &mut FilePathBudget,
) -> Result<(), MetainfoError> {
    if out.len() >= MAX_FILES {
        return Err(MetainfoError::LimitExceeded {
            field: "files",
            limit: MAX_FILES,
        });
    }
    if path_components.len() > MAX_PATH_COMPONENTS {
        return Err(MetainfoError::LimitExceeded {
            field: "path components",
            limit: MAX_PATH_COMPONENTS,
        });
    }

    let dict = match node {
        BValue::Dict(pairs) => pairs,
        _ => return Err(MetainfoError::InvalidFieldType("file tree node")),
    };

    // Leaf: has empty-string key ""
    if let Some(leaf) = node.get(b"") {
        if path_components.is_empty() {
            return Err(MetainfoError::InvalidFieldType("file tree root"));
        }
        if dict.len() != 1 {
            return Err(MetainfoError::InvalidFieldType(
                "file tree leaf with sibling entries",
            ));
        }
        let length = get_nonnegative_u64(leaf, b"length", "file tree length")?;
        let pieces_root = match leaf.get(b"pieces root") {
            Some(value) => {
                let pieces_root_bytes = value
                    .as_bytes()
                    .ok_or(MetainfoError::InvalidFieldType("pieces root"))?;
                if pieces_root_bytes.len() != 32 {
                    return Err(MetainfoError::InvalidFieldType(
                        "pieces root (must be 32 bytes)",
                    ));
                }
                Some(pieces_root_bytes.try_into().expect("length checked"))
            }
            None if length == 0 => None,
            None => return Err(MetainfoError::MissingField("pieces root")),
        };

        path_budget.reserve(path_components)?;
        let path = SafeRelPath::from_components(path_components, cfg!(windows))?;

        let index = out.len() as u32;
        let file_piece_offset = align_piece_offset(*piece_offset, piece_length)?;
        out.push(TorrentFileV2 {
            index,
            length,
            path,
            offset: *offset,
            piece_offset: file_piece_offset,
            pieces_root,
            pad: is_pad_attr(leaf),
        });
        add_offset(offset, length, "file tree offset")?;
        if length > 0 {
            *piece_offset = file_piece_offset
                .checked_add(length)
                .ok_or(MetainfoError::IntegerOverflow("file tree piece offset"))?;
        }
        return Ok(());
    }

    // Interior node: recurse into each key (sorted dict)
    for (key, child) in dict {
        if key.is_empty() {
            continue;
        }
        if key.len() > MAX_PATH_COMPONENT_BYTES {
            return Err(MetainfoError::LimitExceeded {
                field: "path component bytes",
                limit: MAX_PATH_COMPONENT_BYTES,
            });
        }
        let component =
            std::str::from_utf8(key).map_err(|_| MetainfoError::InvalidUtf8("file tree key"))?;
        let mut new_path: Vec<&str> = path_components.to_vec();
        new_path.push(component);
        walk_file_tree(
            child,
            &new_path,
            out,
            offset,
            piece_offset,
            piece_length,
            path_budget,
        )?;
    }
    Ok(())
}

fn align_piece_offset(offset: u64, piece_length: u64) -> Result<u64, MetainfoError> {
    let remainder = offset % piece_length;
    if remainder == 0 {
        return Ok(offset);
    }
    offset
        .checked_add(piece_length - remainder)
        .ok_or(MetainfoError::IntegerOverflow("file tree piece alignment"))
}

/// Parse and authenticate the top-level BEP 52 piece layers dictionary.
/// Piece-layer entries are required exactly for non-empty files larger than a
/// piece; their roots must reconstruct the corresponding file-tree `pieces
/// root`. This catches malformed metadata before it reaches storage or peer
/// transfer code.
fn parse_piece_layers(
    root: &BValue<'_>,
    files: &[TorrentFileV2],
    piece_length: u64,
) -> Result<HashMap<[u8; 32], Vec<[u8; 32]>>, MetainfoError> {
    // Layers are keyed by pieces root, so identical files share one entry.
    let mut required = HashMap::<[u8; 32], (u64, usize)>::new();
    for file in files {
        if file.length <= piece_length {
            continue;
        }
        let pieces_root = file
            .pieces_root
            .ok_or(MetainfoError::MissingField("pieces root"))?;
        let count = file
            .length
            .checked_add(piece_length - 1)
            .ok_or(MetainfoError::IntegerOverflow("piece layer count"))?
            / piece_length;
        let count = usize::try_from(count)
            .map_err(|_| MetainfoError::IntegerOverflow("piece layer count"))?;
        if count > MAX_PIECES {
            return Err(MetainfoError::LimitExceeded {
                field: "piece layers",
                limit: MAX_PIECES,
            });
        }
        if let Some((previous_length, previous_count)) =
            required.insert(pieces_root, (file.length, count))
        {
            if previous_length != file.length || previous_count != count {
                return Err(MetainfoError::InvalidPieceLayer(
                    "same pieces root has conflicting file requirements",
                ));
            }
        }
    }

    let Some(value) = root.get(b"piece layers") else {
        if required.is_empty() {
            return Ok(HashMap::new());
        }
        return Err(MetainfoError::MissingField("piece layers"));
    };
    let BValue::Dict(entries) = value else {
        return Err(MetainfoError::InvalidFieldType("piece layers"));
    };
    let mut layers = HashMap::with_capacity(entries.len().min(required.len()));
    for (key, value) in entries {
        if key.len() != 32 {
            return Err(MetainfoError::InvalidPieceLayer(
                "piece-layer key must be a 32-byte pieces root",
            ));
        }
        let pieces_root: [u8; 32] = (*key).try_into().expect("length checked");
        let Some(&(_, expected_count)) = required.get(&pieces_root) else {
            return Err(MetainfoError::InvalidPieceLayer(
                "piece-layer key has no matching layered file",
            ));
        };
        let bytes = value
            .as_bytes()
            .ok_or(MetainfoError::InvalidFieldType("piece layer"))?;
        if bytes.len() % 32 != 0 {
            return Err(MetainfoError::InvalidPieceLayer(
                "piece-layer bytes are not a multiple of 32",
            ));
        }
        let actual_count = bytes.len() / 32;
        if actual_count != expected_count {
            return Err(MetainfoError::InvalidPieceLayerCount {
                expected: expected_count,
                actual: actual_count,
            });
        }
        let (hash_chunks, remainder) = bytes.as_chunks::<32>();
        debug_assert!(remainder.is_empty());
        let hashes = hash_chunks.to_vec();
        if merkle_root(&hashes) != pieces_root {
            return Err(MetainfoError::PieceLayerRootMismatch);
        }
        if layers.insert(pieces_root, hashes).is_some() {
            return Err(MetainfoError::InvalidPieceLayer(
                "duplicate piece-layer key",
            ));
        }
    }
    if layers.len() != required.len() {
        return Err(MetainfoError::InvalidPieceLayer(
            "missing piece layer for a layered file",
        ));
    }
    Ok(layers)
}

fn parse_announce(root: &BValue<'_>) -> Result<Option<String>, MetainfoError> {
    let Some(value) = root.get(b"announce") else {
        return Ok(None);
    };
    let Some(bytes) = value.as_bytes() else {
        return Err(MetainfoError::InvalidFieldType("announce"));
    };
    if bytes.len() > MAX_TRACKER_URL_BYTES {
        return Err(MetainfoError::LimitExceeded {
            field: "tracker url bytes",
            limit: MAX_TRACKER_URL_BYTES,
        });
    }
    Ok(std::str::from_utf8(bytes)
        .ok()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned))
}

fn parse_optional_string(
    root: &BValue<'_>,
    key: &'static [u8],
) -> Result<Option<String>, MetainfoError> {
    let Some(bytes) = root.get(key).and_then(|v| v.as_bytes()) else {
        return Ok(None);
    };
    if bytes.len() > MAX_METADATA_TEXT_BYTES {
        return Err(MetainfoError::LimitExceeded {
            field: "metadata text bytes",
            limit: MAX_METADATA_TEXT_BYTES,
        });
    }
    Ok(std::str::from_utf8(bytes)
        .ok()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned))
}

fn parse_files_v1(info: &BValue<'_>, name: &str) -> Result<Vec<TorrentFileV1>, MetainfoError> {
    match info.get(b"files") {
        Some(BValue::List(file_list)) => {
            if file_list.len() > MAX_FILES {
                return Err(MetainfoError::LimitExceeded {
                    field: "files",
                    limit: MAX_FILES,
                });
            }
            // Multi-file torrent: name is the root directory
            let mut offset = 0u64;
            let mut path_budget = FilePathBudget::default();
            let mut files = Vec::with_capacity(file_list.len());
            for (idx, entry) in file_list.iter().enumerate() {
                let length = get_nonnegative_u64(entry, b"length", "file length")?;
                let path_list = match entry.get(b"path") {
                    Some(BValue::List(parts)) => parts,
                    _ => return Err(MetainfoError::MissingField("file path")),
                };
                if path_list.len().saturating_add(1) > MAX_PATH_COMPONENTS {
                    return Err(MetainfoError::LimitExceeded {
                        field: "path components",
                        limit: MAX_PATH_COMPONENTS,
                    });
                }
                let mut components: Vec<&str> = Vec::with_capacity(path_list.len() + 1);
                components.push(name);
                for part in path_list {
                    let s = match part {
                        BValue::Bytes(b) => {
                            if b.len() > MAX_PATH_COMPONENT_BYTES {
                                return Err(MetainfoError::LimitExceeded {
                                    field: "path component bytes",
                                    limit: MAX_PATH_COMPONENT_BYTES,
                                });
                            }
                            std::str::from_utf8(b)
                                .map_err(|_| MetainfoError::InvalidUtf8("path component"))?
                        }
                        _ => return Err(MetainfoError::InvalidFieldType("path component")),
                    };
                    // Some (old, real-world) torrent creation tools emit a
                    // vestigial empty leading path component - e.g.
                    // `path: ["", "movie.mkv"]`. Confirmed against a real,
                    // actively-seeding rTorrent production torrent: rTorrent
                    // itself silently drops it (files land at
                    // `<name>/movie.mkv`, no empty-named subdirectory), so
                    // rejecting the whole file here would import less than a
                    // real client does. `SafeRelPath` still rejects a path
                    // that ends up with zero components after this filtering.
                    if !s.is_empty() {
                        components.push(s);
                    }
                }
                path_budget.reserve(&components)?;
                let path = SafeRelPath::from_components(&components, cfg!(windows))?;
                let pad = is_pad_attr(entry);
                files.push(TorrentFileV1 {
                    index: idx as u32,
                    length,
                    path,
                    offset,
                    pad,
                });
                add_offset(&mut offset, length, "file offset")?;
            }
            Ok(files)
        }
        Some(_) => Err(MetainfoError::InvalidFieldType("files")),
        None => {
            // Single-file torrent
            let length = get_nonnegative_u64(info, b"length", "length")?;
            FilePathBudget::default().reserve(&[name])?;
            let path = SafeRelPath::from_name(name, cfg!(windows))?;
            Ok(vec![TorrentFileV1 {
                index: 0,
                length,
                path,
                offset: 0,
                pad: false,
            }])
        }
    }
}

fn validate_file_path_collisions<'a>(
    paths: impl IntoIterator<Item = &'a SafeRelPath>,
) -> Result<(), MetainfoError> {
    rt_path::validate_unique_file_paths(paths).map_err(|error| match error {
        rt_path::PathError::ConflictingPaths(path) => MetainfoError::ConflictingFilePaths(path),
        error => MetainfoError::InvalidPath(error),
    })
}

fn parse_announce_list(
    root: &BValue<'_>,
    initial_tracker_count: usize,
    initial_tracker_bytes: usize,
) -> Result<Vec<Vec<String>>, MetainfoError> {
    let Some(BValue::List(tiers)) = root.get(b"announce-list") else {
        return Ok(Vec::new());
    };
    if tiers.len() > MAX_TRACKER_TIERS {
        return Err(MetainfoError::LimitExceeded {
            field: "tracker tiers",
            limit: MAX_TRACKER_TIERS,
        });
    }
    let mut total = initial_tracker_count;
    let mut total_bytes = initial_tracker_bytes;
    let mut out = Vec::new();
    for tier in tiers {
        let BValue::List(urls) = tier else {
            continue;
        };
        let mut tier_urls = Vec::new();
        for u in urls {
            let Some(url) = u
                .as_bytes()
                .and_then(|b| std::str::from_utf8(b).ok())
                .map(str::trim)
                .filter(|url| !url.is_empty())
            else {
                continue;
            };
            if url.len() > MAX_TRACKER_URL_BYTES {
                return Err(MetainfoError::LimitExceeded {
                    field: "tracker url bytes",
                    limit: MAX_TRACKER_URL_BYTES,
                });
            }
            total = total.saturating_add(1);
            if total > MAX_TRACKER_URLS {
                return Err(MetainfoError::LimitExceeded {
                    field: "tracker urls",
                    limit: MAX_TRACKER_URLS,
                });
            }
            total_bytes = total_bytes.saturating_add(url.len());
            if total_bytes > MAX_TRACKER_URL_TOTAL_BYTES {
                return Err(MetainfoError::LimitExceeded {
                    field: "tracker url total bytes",
                    limit: MAX_TRACKER_URL_TOTAL_BYTES,
                });
            }
            tier_urls.push(url.to_owned());
        }
        if !tier_urls.is_empty() {
            out.push(tier_urls);
        }
    }
    Ok(out)
}

fn parse_webseeds(root: &BValue<'_>) -> Result<Vec<String>, MetainfoError> {
    let Some(value) = root.get(b"url-list") else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    match value {
        BValue::Bytes(bytes) => push_webseed_bytes(bytes, &mut out, &mut seen)?,
        BValue::List(values) => {
            for value in values {
                if let Some(bytes) = value.as_bytes() {
                    push_webseed_bytes(bytes, &mut out, &mut seen)?;
                }
            }
        }
        _ => {}
    }
    Ok(out)
}

fn push_webseed_bytes<'a>(
    bytes: &'a [u8],
    out: &mut Vec<String>,
    seen: &mut HashSet<&'a str>,
) -> Result<(), MetainfoError> {
    let Ok(value) = std::str::from_utf8(bytes) else {
        return Ok(());
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(());
    }
    if value.len() > MAX_WEBSEED_URL_BYTES {
        return Err(MetainfoError::LimitExceeded {
            field: "webseed url bytes",
            limit: MAX_WEBSEED_URL_BYTES,
        });
    }
    if seen.contains(value) {
        return Ok(());
    }
    if out.len() >= MAX_WEBSEED_URLS {
        return Err(MetainfoError::LimitExceeded {
            field: "webseed urls",
            limit: MAX_WEBSEED_URLS,
        });
    }
    seen.insert(value);
    out.push(value.to_owned());
    Ok(())
}

fn parse_piece_hashes(info: &BValue<'_>) -> Result<Vec<[u8; 20]>, MetainfoError> {
    let pieces_bytes = get_bytes(info, b"pieces", "pieces")?;
    if pieces_bytes.len() % 20 != 0 {
        return Err(MetainfoError::InvalidPiecesLength(pieces_bytes.len()));
    }
    let piece_count = pieces_bytes.len() / 20;
    if piece_count > MAX_PIECES {
        return Err(MetainfoError::LimitExceeded {
            field: "pieces",
            limit: MAX_PIECES,
        });
    }
    Ok(pieces_bytes.as_chunks::<20>().0.to_vec())
}

fn validate_piece_count(
    pieces: &[[u8; 20]],
    files: &[TorrentFileV1],
    piece_length: u64,
) -> Result<(), MetainfoError> {
    let total_length = files.iter().try_fold(0u64, |total, file| {
        total
            .checked_add(file.length)
            .ok_or(MetainfoError::IntegerOverflow("total length"))
    })?;
    if total_length == 0 || !files.iter().any(|file| !file.pad && file.length > 0) {
        // The engine's v1 piece map has no meaningful piece-zero state. Keep
        // an empty file inside an otherwise non-empty torrent valid. BEP 47
        // padding alone is synthetic and does not make a payload torrent.
        return Err(MetainfoError::ZeroTotalLength);
    }
    let expected = if total_length == 0 {
        0
    } else {
        total_length
            .checked_add(piece_length - 1)
            .ok_or(MetainfoError::IntegerOverflow("piece count"))?
            / piece_length
    };
    let expected =
        usize::try_from(expected).map_err(|_| MetainfoError::IntegerOverflow("piece count"))?;
    if pieces.len() != expected {
        return Err(MetainfoError::InvalidPieceCount {
            expected,
            actual: pieces.len(),
        });
    }
    Ok(())
}

/// BEP 47: a file dict/leaf carries `"attr"` as a short string of one-letter
/// flags; `'p'` marks a padding file real clients never write to disk.
fn is_pad_attr(dict: &BValue<'_>) -> bool {
    match dict.get(b"attr") {
        Some(BValue::Bytes(b)) => b.contains(&b'p'),
        _ => false,
    }
}

fn get_bytes<'a>(
    dict: &'a BValue<'_>,
    key: &[u8],
    field: &'static str,
) -> Result<&'a [u8], MetainfoError> {
    match dict.get(key) {
        Some(BValue::Bytes(b)) => Ok(b),
        Some(_) => Err(MetainfoError::InvalidFieldType(field)),
        None => Err(MetainfoError::MissingField(field)),
    }
}

fn get_string(
    dict: &BValue<'_>,
    key: &[u8],
    field: &'static str,
    max_bytes: usize,
) -> Result<String, MetainfoError> {
    let b = get_bytes(dict, key, field)?;
    if b.len() > max_bytes {
        return Err(MetainfoError::LimitExceeded {
            field: "name bytes",
            limit: max_bytes,
        });
    }
    std::str::from_utf8(b)
        .map(|s| s.to_owned())
        .map_err(|_| MetainfoError::InvalidUtf8(field))
}

fn get_int(dict: &BValue<'_>, key: &[u8], field: &'static str) -> Result<i64, MetainfoError> {
    match dict.get(key) {
        Some(BValue::Int(n)) => Ok(*n),
        Some(_) => Err(MetainfoError::InvalidFieldType(field)),
        None => Err(MetainfoError::MissingField(field)),
    }
}

fn get_nonnegative_u64(
    dict: &BValue<'_>,
    key: &[u8],
    field: &'static str,
) -> Result<u64, MetainfoError> {
    let value = get_int(dict, key, field)?;
    u64::try_from(value).map_err(|_| MetainfoError::InvalidIntegerValue { field, value })
}

/// BEP 3 recommends (does not require) a power-of-two piece length, and
/// nothing downstream (rt-piece-map, rt-piece-picker, rt-storage) relies on
/// it - all piece-boundary math here uses plain division/modulo, never bit
/// shifts. Requiring it anyway rejects real, actively-seeded torrents from
/// well-known release groups: confirmed against a 7351-torrent production
/// rTorrent session, where 43 legitimate torrents (e.g. a UHD BluRay remux
/// with `piece length` 7995392, not a power of two) were otherwise skipped
/// outright. Piece-count DoS protection is handled separately by
/// `MAX_PIECES` in `parse_piece_hashes`, independent of piece-length shape.
fn get_positive_u64(
    dict: &BValue<'_>,
    key: &[u8],
    field: &'static str,
) -> Result<u64, MetainfoError> {
    let value = get_nonnegative_u64(dict, key, field)?;
    if value == 0 {
        return Err(MetainfoError::InvalidPieceLength(value));
    }
    Ok(value)
}

fn add_offset(offset: &mut u64, length: u64, field: &'static str) -> Result<(), MetainfoError> {
    *offset = offset
        .checked_add(length)
        .ok_or(MetainfoError::IntegerOverflow(field))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rt_bencode::encode;
    use rt_bencode::BValue;

    fn make_pieces(n: usize) -> Vec<u8> {
        vec![0u8; n * 20]
    }

    fn single_file_torrent(
        name: &str,
        length: i64,
        piece_length: i64,
        private: Option<i64>,
    ) -> Vec<u8> {
        single_file_torrent_with_piece_count(name, length, piece_length, private, 1)
    }

    fn single_file_torrent_with_piece_count(
        name: &str,
        length: i64,
        piece_length: i64,
        private: Option<i64>,
        piece_count: usize,
    ) -> Vec<u8> {
        single_file_torrent_with_private_value(
            name,
            length,
            piece_length,
            private.map(BValue::Int),
            piece_count,
        )
    }

    fn single_file_torrent_with_private_value(
        name: &str,
        length: i64,
        piece_length: i64,
        private: Option<BValue<'_>>,
        piece_count: usize,
    ) -> Vec<u8> {
        let pieces_data = make_pieces(piece_count);
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"length", BValue::Int(length)),
            (b"name", BValue::Bytes(name.as_bytes())),
            (b"piece length", BValue::Int(piece_length)),
            (b"pieces", BValue::Bytes(&pieces_data)),
        ];
        if let Some(value) = private {
            info_pairs.push((b"private", value));
        }
        // bencode dict keys must be sorted
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let info = BValue::Dict(info_pairs);

        let mut pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (
                b"announce",
                BValue::Bytes(b"http://tracker.example.com/announce"),
            ),
            (b"info", info),
        ];
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        encode(&BValue::Dict(pairs))
    }

    fn multi_file_torrent(name: &str, files: &[(&str, i64)]) -> Vec<u8> {
        let file_entries: Vec<BValue<'_>> = files
            .iter()
            .map(|(fname, len)| {
                let mut pairs: Vec<(&[u8], BValue<'_>)> = vec![
                    (b"length", BValue::Int(*len)),
                    (b"path", BValue::List(vec![BValue::Bytes(fname.as_bytes())])),
                ];
                pairs.sort_by(|a, b| a.0.cmp(b.0));
                BValue::Dict(pairs)
            })
            .collect();

        let pieces_data = make_pieces(1);
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"files", BValue::List(file_entries)),
            (b"name", BValue::Bytes(name.as_bytes())),
            (b"piece length", BValue::Int(512 * 1024)),
            (b"pieces", BValue::Bytes(&pieces_data)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let info = BValue::Dict(info_pairs);

        let mut pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (
                b"announce",
                BValue::Bytes(b"http://tracker.example.com/announce"),
            ),
            (b"info", info),
        ];
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        encode(&BValue::Dict(pairs))
    }

    fn v1_torrent_with_path_parts(path_parts: &[&str]) -> Vec<u8> {
        let file = BValue::Dict(vec![
            (b"length".as_ref(), BValue::Int(1024)),
            (
                b"path".as_ref(),
                BValue::List(
                    path_parts
                        .iter()
                        .map(|part| BValue::Bytes(part.as_bytes()))
                        .collect(),
                ),
            ),
        ]);
        let pieces = make_pieces(1);
        let mut info_fields: Vec<(&[u8], BValue<'_>)> = vec![
            (b"files", BValue::List(vec![file])),
            (b"name", BValue::Bytes(b"root")),
            (b"piece length", BValue::Int(512 * 1024)),
            (b"pieces", BValue::Bytes(&pieces)),
        ];
        info_fields.sort_by(|left, right| left.0.cmp(right.0));
        let mut root_fields: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://t.example/a")),
            (b"info", BValue::Dict(info_fields)),
        ];
        root_fields.sort_by(|left, right| left.0.cmp(right.0));
        encode(&BValue::Dict(root_fields))
    }

    fn hybrid_multi_file_torrent(
        v1_files: &[(&[&str], i64, bool)],
        v2_files: &[(&str, i64)],
        rooted_v2_tree: bool,
    ) -> Vec<u8> {
        let piece_length = 16 * 1024;
        let total_length = v1_files.iter().map(|(_, length, _)| *length).sum::<i64>();
        let piece_count = usize::try_from((total_length + piece_length - 1) / piece_length)
            .expect("test piece count");
        let pieces_data = make_pieces(piece_count);
        let pad_root = b"bundle";
        let pieces_root = [0xA5; 32];

        let file_entries = v1_files
            .iter()
            .map(|(path, length, pad)| {
                let mut fields: Vec<(&[u8], BValue<'_>)> = vec![
                    (b"length", BValue::Int(*length)),
                    (
                        b"path",
                        BValue::List(
                            path.iter()
                                .map(|component| BValue::Bytes(component.as_bytes()))
                                .collect(),
                        ),
                    ),
                ];
                if *pad {
                    fields.push((b"attr", BValue::Bytes(b"p")));
                }
                fields.sort_by(|a, b| a.0.cmp(b.0));
                BValue::Dict(fields)
            })
            .collect();

        let mut v2_entries: Vec<(&[u8], BValue<'_>)> = v2_files
            .iter()
            .map(|(name, length)| {
                let mut fields: Vec<(&[u8], BValue<'_>)> = vec![(b"length", BValue::Int(*length))];
                if *length > 0 {
                    fields.push((b"pieces root", BValue::Bytes(&pieces_root)));
                }
                fields.sort_by(|a, b| a.0.cmp(b.0));
                (
                    name.as_bytes(),
                    BValue::Dict(vec![(b"".as_ref(), BValue::Dict(fields))]),
                )
            })
            .collect();
        v2_entries.sort_by(|a, b| a.0.cmp(b.0));
        let file_tree_entries = if rooted_v2_tree {
            vec![(pad_root.as_ref(), BValue::Dict(v2_entries))]
        } else {
            v2_entries
        };

        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"file tree", BValue::Dict(file_tree_entries)),
            (b"files", BValue::List(file_entries)),
            (b"meta version", BValue::Int(2)),
            (b"name", BValue::Bytes(b"bundle")),
            (b"piece length", BValue::Int(piece_length)),
            (b"pieces", BValue::Bytes(&pieces_data)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        encode(&BValue::Dict(vec![(
            b"info".as_ref(),
            BValue::Dict(info_pairs),
        )]))
    }

    #[test]
    fn parse_single_file() {
        let raw = single_file_torrent("test.bin", 1024, 512 * 1024, None);
        let meta = parse_torrent(&raw).unwrap();
        let TorrentMeta::V1(m) = meta else {
            panic!("expected V1")
        };
        assert_eq!(m.name, "test.bin");
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.files[0].length, 1024);
        assert_eq!(m.total_length(), 1024);
        assert!(m.is_single_file());
        assert!(!m.private);
        assert_eq!(m.info_hash.len(), 20);
    }

    #[test]
    fn parse_allocation_reservation_covers_hybrid_tree_and_both_raw_copies() {
        let raw = hybrid_multi_file_torrent(
            &[(&["payload.bin"][..], 1024, false)],
            &[("payload.bin", 1024)],
            false,
        );
        let mut requested = Vec::new();
        let mut reserve = |bytes| {
            requested.push(bytes);
            true
        };
        let parsed = parse_torrent_with_allocation_reservation(&raw, &mut reserve).unwrap();
        assert!(matches!(parsed, TorrentMeta::Hybrid(_, _)));
        assert!(
            requested.len() > 3,
            "decoder growth, metainfo projections, and raw copies must be charged"
        );
        assert_eq!(&requested[requested.len() - 2..], &[raw.len(), raw.len()]);
        let before_raw_copies = requested.len() - 2;
        let projection_bytes = requested[before_raw_copies - 1];
        assert!(projection_bytes > 0);

        let mut calls = 0;
        let mut deny_projection = |_bytes| {
            calls += 1;
            calls < before_raw_copies
        };
        assert!(matches!(
            parse_torrent_with_allocation_reservation(&raw, &mut deny_projection),
            Err(MetainfoError::Bencode(
                rt_bencode::BencodeError::AllocationBudgetExceeded { bytes }
            )) if bytes == projection_bytes
        ));
        assert_eq!(calls, before_raw_copies);

        let decoder_growth_count = before_raw_copies;
        let mut denied_growth_count = 0;
        let mut deny_first_raw_copy = |_bytes| {
            denied_growth_count += 1;
            denied_growth_count <= decoder_growth_count
        };
        assert!(matches!(
            parse_torrent_with_allocation_reservation(&raw, &mut deny_first_raw_copy),
            Err(MetainfoError::Bencode(
                rt_bencode::BencodeError::AllocationBudgetExceeded { bytes }
            )) if bytes == raw.len()
        ));
        assert_eq!(denied_growth_count, decoder_growth_count + 1);
    }

    #[test]
    fn convenience_parse_budget_is_cumulative_and_denies_before_growth() {
        let mut reserve = bounded_allocation_reservation(8);
        assert!(reserve(6));
        assert!(!reserve(3));
        assert!(reserve(2));

        let raw = single_file_torrent("bounded.bin", 1, 16_384, None);
        assert!(matches!(
            parse_torrent_with_allocation_limit(&raw, 0),
            Err(MetainfoError::Bencode(
                rt_bencode::BencodeError::AllocationBudgetExceeded { bytes }
            )) if bytes > 0
        ));
    }

    #[test]
    fn webseed_deduplication_handles_many_repeated_entries() {
        let duplicate = BValue::Bytes(b"https://seed.example/payload");
        let root = BValue::Dict(vec![(
            b"url-list".as_ref(),
            BValue::List(vec![duplicate; 4096]),
        )]);

        assert_eq!(
            parse_webseeds(&root).unwrap(),
            ["https://seed.example/payload"]
        );
    }

    #[test]
    fn info_bytes_allocation_reservation_covers_decoder_and_output() {
        let raw = v2_torrent("root", "payload.bin", 65_536);
        let mut requested = Vec::new();
        let mut reserve = |bytes| {
            requested.push(bytes);
            true
        };
        let info = torrent_info_bytes_with_allocation_reservation(&raw, &mut reserve).unwrap();
        assert!(!requested.is_empty());
        assert_eq!(requested.last(), Some(&info.len()));
        assert_eq!(info, torrent_info_bytes(&raw).unwrap());
    }

    #[test]
    fn v2_magnet_projection_is_reserved_before_file_graph_allocation() {
        let raw = v2_torrent("root", "payload.bin", 65_536);
        let info = torrent_info_bytes(&raw).unwrap();
        let mut requested = Vec::new();
        let mut reserve = |bytes| {
            requested.push(bytes);
            true
        };
        let requirements =
            v2_piece_layer_requirements_with_allocation_reservation(&info, &mut reserve)
                .unwrap()
                .unwrap();
        assert_eq!(requirements.files.len(), 1);
        assert!(requested.len() >= 2);
        let projection_bytes = *requested.last().unwrap();
        assert!(projection_bytes > 0);

        let mut calls = 0;
        let mut deny_projection = |_bytes| {
            calls += 1;
            calls < requested.len()
        };
        assert!(matches!(
            v2_piece_layer_requirements_with_allocation_reservation(
                &info,
                &mut deny_projection
            ),
            Err(MetainfoError::Bencode(
                rt_bencode::BencodeError::AllocationBudgetExceeded { bytes }
            )) if bytes == projection_bytes
        ));
        assert_eq!(calls, requested.len());
    }

    #[test]
    fn reject_files_field_with_wrong_type() {
        let pieces_data = make_pieces(1);
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"files", BValue::Int(1)),
            (b"length", BValue::Int(1024)),
            (b"name", BValue::Bytes(b"test.bin")),
            (b"piece length", BValue::Int(512 * 1024)),
            (b"pieces", BValue::Bytes(&pieces_data)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let mut root = vec![(b"info".as_ref(), BValue::Dict(info_pairs))];
        root.sort_by(|a, b| a.0.cmp(b.0));

        assert!(matches!(
            parse_torrent(&encode(&BValue::Dict(root))),
            Err(MetainfoError::InvalidFieldType("files"))
        ));
    }

    #[test]
    fn parse_multi_file() {
        let raw = multi_file_torrent("mydir", &[("a.txt", 100), ("b.txt", 200)]);
        let meta = parse_torrent(&raw).unwrap();
        let TorrentMeta::V1(m) = meta else {
            panic!("expected V1")
        };
        assert_eq!(m.files.len(), 2);
        assert_eq!(m.files[0].offset, 0);
        assert_eq!(m.files[1].offset, 100);
        assert_eq!(m.total_length(), 300);
        assert!(!m.is_single_file());
    }

    #[test]
    fn parse_multi_file_marks_bep47_padding_files() {
        let real_pairs: Vec<(&[u8], BValue<'_>)> = {
            let mut p: Vec<(&[u8], BValue<'_>)> = vec![
                (b"length", BValue::Int(100)),
                (b"path", BValue::List(vec![BValue::Bytes(b"a.txt")])),
            ];
            p.sort_by(|a, b| a.0.cmp(b.0));
            p
        };
        let pad_pairs: Vec<(&[u8], BValue<'_>)> = {
            let mut p: Vec<(&[u8], BValue<'_>)> = vec![
                (b"attr", BValue::Bytes(b"p")),
                (b"length", BValue::Int(28)),
                (
                    b"path",
                    BValue::List(vec![BValue::Bytes(b".pad"), BValue::Bytes(b"128")]),
                ),
            ];
            p.sort_by(|a, b| a.0.cmp(b.0));
            p
        };
        let file_entries = vec![BValue::Dict(real_pairs), BValue::Dict(pad_pairs)];

        let pieces_data = make_pieces(1);
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"files", BValue::List(file_entries)),
            (b"name", BValue::Bytes(b"mydir")),
            (b"piece length", BValue::Int(512 * 1024)),
            (b"pieces", BValue::Bytes(&pieces_data)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let mut root: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://t.example/a")),
            (b"info", BValue::Dict(info_pairs)),
        ];
        root.sort_by(|a, b| a.0.cmp(b.0));
        let raw = encode(&BValue::Dict(root));

        let TorrentMeta::V1(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V1")
        };
        assert_eq!(m.files.len(), 2);
        assert!(!m.files[0].pad, "real file must not be marked pad");
        assert!(m.files[1].pad, "attr:p file must be marked pad");
    }

    #[test]
    fn hybrid_layout_accepts_rootless_or_rooted_v2_with_optional_tail_padding() {
        let with_tail_padding = hybrid_multi_file_torrent(
            &[
                (&["a.bin"], 1_000, false),
                (&[".pad", "15384"], 15_384, true),
                (&["b.bin"], 500, false),
                (&[".pad", "15884"], 15_884, true),
            ],
            &[("a.bin", 1_000), ("b.bin", 500)],
            false,
        );
        let TorrentMeta::Hybrid(v1, v2) = parse_torrent(&with_tail_padding).unwrap() else {
            panic!("expected hybrid torrent");
        };
        assert_eq!(v1.total_length(), 32_768);
        assert_eq!(v2.total_length(), 1_500);
        assert_eq!(v2.piece_count(), 2);
        assert_eq!(v2.files[0].path.as_display(), "a.bin");

        let without_tail_padding = hybrid_multi_file_torrent(
            &[
                (&["a.bin"], 1_000, false),
                (&[".pad", "15384"], 15_384, true),
                (&["b.bin"], 500, false),
            ],
            &[("a.bin", 1_000), ("b.bin", 500)],
            true,
        );
        let TorrentMeta::Hybrid(v1, v2) = parse_torrent(&without_tail_padding).unwrap() else {
            panic!("expected hybrid torrent");
        };
        assert_eq!(v1.total_length(), 16_884);
        assert_eq!(v2.files[0].path.as_display(), "bundle/a.bin");
    }

    #[test]
    fn hybrid_layout_allows_reused_paths_for_synthetic_padding_files() {
        let raw = hybrid_multi_file_torrent(
            &[
                (&["a.bin"], 1_000, false),
                (&[".pad", "15384"], 15_384, true),
                (&["b.bin"], 1_000, false),
                (&[".pad", "15384"], 15_384, true),
                (&["c.bin"], 1_000, false),
            ],
            &[("a.bin", 1_000), ("b.bin", 1_000), ("c.bin", 1_000)],
            false,
        );

        let TorrentMeta::Hybrid(v1, v2) = parse_torrent(&raw).unwrap() else {
            panic!("expected hybrid torrent");
        };
        assert_eq!(v1.files.iter().filter(|file| file.pad).count(), 2);
        assert_eq!(v2.files.len(), 3);
    }

    #[test]
    fn hybrid_layout_rejects_mismatched_paths_lengths_and_padding() {
        let mismatched_path = hybrid_multi_file_torrent(
            &[
                (&["a.bin"], 1_000, false),
                (&[".pad", "15384"], 15_384, true),
                (&["other.bin"], 500, false),
            ],
            &[("a.bin", 1_000), ("b.bin", 500)],
            false,
        );
        assert!(matches!(
            parse_torrent(&mismatched_path),
            Err(MetainfoError::InconsistentHybridLayout(
                "v1 and v2 payload file paths differ"
            ))
        ));

        let mismatched_length = hybrid_multi_file_torrent(
            &[
                (&["a.bin"], 1_000, false),
                (&[".pad", "15384"], 15_384, true),
                (&["b.bin"], 499, false),
            ],
            &[("a.bin", 1_000), ("b.bin", 500)],
            false,
        );
        assert!(matches!(
            parse_torrent(&mismatched_length),
            Err(MetainfoError::InconsistentHybridLayout(
                "v1 and v2 payload file lengths differ"
            ))
        ));

        let misaligned_padding = hybrid_multi_file_torrent(
            &[
                (&["a.bin"], 1_000, false),
                (&[".pad", "15383"], 15_383, true),
                (&["b.bin"], 500, false),
            ],
            &[("a.bin", 1_000), ("b.bin", 500)],
            false,
        );
        assert!(matches!(
            parse_torrent(&misaligned_padding),
            Err(MetainfoError::InconsistentHybridLayout(
                "v1 padding does not align payload files to v2 piece boundaries"
            ))
        ));

        let invalid_tail_padding = hybrid_multi_file_torrent(
            &[
                (&["a.bin"], 1_000, false),
                (&[".pad", "15384"], 15_384, true),
                (&["b.bin"], 500, false),
                (&[".pad", "15883"], 15_883, true),
            ],
            &[("a.bin", 1_000), ("b.bin", 500)],
            false,
        );
        assert!(matches!(
            parse_torrent(&invalid_tail_padding),
            Err(MetainfoError::InconsistentHybridLayout(
                "v1 trailing padding does not match the v2 logical end"
            ))
        ));
    }

    #[test]
    fn parse_private_flag() {
        let raw = single_file_torrent("priv.bin", 512, 512 * 1024, Some(1));
        let TorrentMeta::V1(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V1")
        };
        assert!(m.private);
    }

    #[test]
    fn parse_missing_and_zero_private_flags_as_public() {
        for private in [None, Some(0)] {
            let raw = single_file_torrent("public.bin", 512, 512 * 1024, private);
            let TorrentMeta::V1(meta) = parse_torrent(&raw).unwrap() else {
                panic!("expected V1")
            };
            assert!(!meta.private);
        }
    }

    #[test]
    fn parse_rejects_unknown_private_flag_integers() {
        for value in [-1, 2] {
            let raw = single_file_torrent("invalid-private.bin", 512, 512 * 1024, Some(value));
            assert!(matches!(
                parse_torrent(&raw),
                Err(MetainfoError::InvalidIntegerValue {
                    field: "private",
                    value: actual,
                }) if actual == value
            ));
        }
    }

    #[test]
    fn parse_rejects_non_integer_private_flag() {
        let raw = single_file_torrent_with_private_value(
            "invalid-private.bin",
            512,
            512 * 1024,
            Some(BValue::Bytes(b"1")),
            1,
        );
        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::InvalidFieldType("private"))
        ));
    }

    #[test]
    fn parse_top_level_comment_creator_and_creation_date() {
        let pieces_data = make_pieces(1);
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"length", BValue::Int(1024)),
            (b"name", BValue::Bytes(b"noted.bin")),
            (b"piece length", BValue::Int(512 * 1024)),
            (b"pieces", BValue::Bytes(&pieces_data)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let mut root: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://t.example/a")),
            (b"comment", BValue::Bytes(b" Release notes ")),
            (b"created by", BValue::Bytes(b"TorrentNG test fixture")),
            (b"creation date", BValue::Int(1_700_000_000)),
            (b"info", BValue::Dict(info_pairs)),
        ];
        root.sort_by(|a, b| a.0.cmp(b.0));
        let raw = encode(&BValue::Dict(root));

        let TorrentMeta::V1(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V1")
        };
        assert_eq!(m.comment.as_deref(), Some("Release notes"));
        assert_eq!(m.created_by.as_deref(), Some("TorrentNG test fixture"));
        assert_eq!(m.creation_date, Some(1_700_000_000));
    }

    #[test]
    fn parse_webseeds_accepts_string_and_list_forms() {
        let pieces_data = make_pieces(1);
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"length", BValue::Int(1024)),
            (b"name", BValue::Bytes(b"seeded.bin")),
            (b"piece length", BValue::Int(512 * 1024)),
            (b"pieces", BValue::Bytes(&pieces_data)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let mut root: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://t.example/a")),
            (b"info", BValue::Dict(info_pairs)),
            (
                b"url-list",
                BValue::List(vec![
                    BValue::Bytes(b" https://seed.example/file "),
                    BValue::Bytes(b"https://seed.example/file"),
                    BValue::Bytes(b"https://mirror.example/file"),
                ]),
            ),
        ];
        root.sort_by(|a, b| a.0.cmp(b.0));
        let raw = encode(&BValue::Dict(root));
        let TorrentMeta::V1(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V1")
        };
        assert_eq!(
            m.webseeds,
            vec![
                "https://seed.example/file".to_owned(),
                "https://mirror.example/file".to_owned()
            ]
        );
    }

    #[test]
    fn infohash_stable_across_parses() {
        let raw = single_file_torrent("stable.bin", 1024, 512 * 1024, None);
        let TorrentMeta::V1(m1) = parse_torrent(&raw).unwrap() else {
            panic!("expected V1")
        };
        let TorrentMeta::V1(m2) = parse_torrent(&raw).unwrap() else {
            panic!("expected V1")
        };
        assert_eq!(m1.info_hash, m2.info_hash);
    }

    #[test]
    fn reject_path_traversal() {
        // Build a multi-file torrent with a path component ".."
        let pieces_data = make_pieces(1);
        let file_entry = BValue::Dict({
            let mut p: Vec<(&[u8], BValue<'_>)> = vec![
                (b"length", BValue::Int(100)),
                (
                    b"path",
                    BValue::List(vec![BValue::Bytes(b".."), BValue::Bytes(b"evil.sh")]),
                ),
            ];
            p.sort_by(|a, b| a.0.cmp(b.0));
            p
        });
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"files", BValue::List(vec![file_entry])),
            (b"name", BValue::Bytes(b"safe")),
            (b"piece length", BValue::Int(512 * 1024)),
            (b"pieces", BValue::Bytes(&pieces_data)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let mut root: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://t.example/a")),
            (b"info", BValue::Dict(info_pairs)),
        ];
        root.sort_by(|a, b| a.0.cmp(b.0));
        let raw = encode(&BValue::Dict(root));
        assert!(parse_torrent(&raw).is_err());
    }

    #[test]
    fn reject_duplicate_file_paths() {
        let raw = multi_file_torrent("root", &[("same.bin", 1), ("same.bin", 1)]);
        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::ConflictingFilePaths(_))
        ));

        let parent = SafeRelPath::from_components(&["root", "node"], false).unwrap();
        let child = SafeRelPath::from_components(&["root", "node", "child.bin"], false).unwrap();
        assert!(matches!(
            validate_file_path_collisions([&parent, &child]),
            Err(MetainfoError::ConflictingFilePaths(_))
        ));
    }

    #[test]
    fn v1_paths_are_bounded_to_storage_walk_depth() {
        let accepted = vec!["dir"; MAX_PATH_COMPONENTS - 1];
        assert!(parse_torrent(&v1_torrent_with_path_parts(&accepted)).is_ok());

        let rejected = vec!["dir"; MAX_PATH_COMPONENTS];
        assert!(matches!(
            parse_torrent(&v1_torrent_with_path_parts(&rejected)),
            Err(MetainfoError::LimitExceeded {
                field: "path components",
                limit: MAX_PATH_COMPONENTS
            })
        ));
    }

    #[test]
    fn v2_file_path_budget_rejects_expansion_before_owning_the_path() {
        let leaf = BValue::Dict(vec![(b"length".as_ref(), BValue::Int(0))]);
        let node = BValue::Dict(vec![(b"".as_ref(), leaf)]);
        let mut budget = FilePathBudget {
            total_components: MAX_TOTAL_FILE_PATH_COMPONENTS,
            total_bytes: 0,
        };
        let mut files = Vec::new();
        let mut offset = 0;
        let mut piece_offset = 0;

        let error = walk_file_tree(
            &node,
            &["payload"],
            &mut files,
            &mut offset,
            &mut piece_offset,
            16 * 1024,
            &mut budget,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            MetainfoError::LimitExceeded {
                field: "total file path components",
                ..
            }
        ));
        assert!(files.is_empty());

        let mut byte_budget = FilePathBudget {
            total_components: 0,
            total_bytes: MAX_TOTAL_FILE_PATH_BYTES,
        };
        assert!(matches!(
            byte_budget.reserve(&["x"]),
            Err(MetainfoError::LimitExceeded {
                field: "aggregate file path bytes",
                ..
            })
        ));
    }

    #[cfg(windows)]
    #[test]
    fn reject_windows_alternate_stream_paths_in_v1_and_v2() {
        let v1 = single_file_torrent("payload:stream", 1024, 512 * 1024, None);
        assert!(matches!(
            parse_torrent(&v1),
            Err(MetainfoError::InvalidPath(_))
        ));

        let v2 = v2_torrent("root", "payload:stream", 65_536);
        assert!(matches!(
            parse_torrent(&v2),
            Err(MetainfoError::InvalidPath(_))
        ));

        let case_aliases = v2_multi_file_torrent("root", &[("Payload.bin", 1), ("payload.BIN", 1)]);
        assert!(matches!(
            parse_torrent(&case_aliases),
            Err(MetainfoError::ConflictingFilePaths(_))
        ));
    }

    #[test]
    fn reject_invalid_pieces_length() {
        // 19 bytes is not a multiple of 20
        let bad_pieces = vec![0u8; 19];
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"length", BValue::Int(1024)),
            (b"name", BValue::Bytes(b"bad.bin")),
            (b"piece length", BValue::Int(512 * 1024)),
            (b"pieces", BValue::Bytes(&bad_pieces)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let mut root: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://t.example/a")),
            (b"info", BValue::Dict(info_pairs)),
        ];
        root.sort_by(|a, b| a.0.cmp(b.0));
        let raw = encode(&BValue::Dict(root));
        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::InvalidPiecesLength(19))
        ));
    }

    #[test]
    fn reject_zero_piece_length() {
        let pieces_data = make_pieces(1);
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"length", BValue::Int(1024)),
            (b"name", BValue::Bytes(b"bad.bin")),
            (b"piece length", BValue::Int(0)),
            (b"pieces", BValue::Bytes(&pieces_data)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let mut root: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://t.example/a")),
            (b"info", BValue::Dict(info_pairs)),
        ];
        root.sort_by(|a, b| a.0.cmp(b.0));
        let raw = encode(&BValue::Dict(root));
        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::InvalidPieceLength(0))
        ));
    }

    #[test]
    fn accepts_non_power_of_two_piece_length() {
        // A real, currently-seeding torrent from a well-known release
        // group uses this exact non-power-of-two piece length; nothing
        // downstream requires a power of two (see get_positive_u64's doc
        // comment), and rejecting it means importing 100% of a real
        // library isn't actually achievable.
        let raw = single_file_torrent("Black.Phone.2.mkv", 1024, 7_995_392, None);
        let TorrentMeta::V1(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V1")
        };
        assert_eq!(m.piece_length, 7_995_392);
    }

    #[test]
    fn multi_file_drops_vestigial_empty_leading_path_component() {
        // A real, ~20-year-old scene release, still actively seeding on a
        // real rTorrent box, encodes each file's path as
        // ["", "movie.mkv"] - a vestigial empty leading component. Real
        // clients (confirmed: rTorrent) place the file at
        // `<name>/movie.mkv` directly, no empty-named subdirectory.
        // Rejecting the whole torrent over this imports less than a real
        // client does.
        let file_entry = BValue::Dict({
            let mut p: Vec<(&[u8], BValue<'_>)> = vec![
                (b"length", BValue::Int(1024)),
                (
                    b"path",
                    BValue::List(vec![BValue::Bytes(b""), BValue::Bytes(b"movie.mkv")]),
                ),
            ];
            p.sort_by(|a, b| a.0.cmp(b.0));
            p
        });
        let pieces_data = make_pieces(1);
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"files", BValue::List(vec![file_entry])),
            (b"name", BValue::Bytes(b"Old.Release-GROUP")),
            (b"piece length", BValue::Int(512 * 1024)),
            (b"pieces", BValue::Bytes(&pieces_data)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let mut root: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://t.example/a")),
            (b"info", BValue::Dict(info_pairs)),
        ];
        root.sort_by(|a, b| a.0.cmp(b.0));
        let raw = encode(&BValue::Dict(root));

        let TorrentMeta::V1(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V1")
        };
        assert_eq!(m.files.len(), 1);
        assert_eq!(
            m.files[0].path.as_display(),
            "Old.Release-GROUP/movie.mkv",
            "the empty component must be dropped, not preserved as a subdirectory"
        );
    }

    #[test]
    fn reject_negative_piece_length() {
        let raw = single_file_torrent("bad.bin", 1024, -1, None);
        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::InvalidIntegerValue {
                field: "piece length",
                value: -1
            })
        ));
    }

    #[test]
    fn reject_i64_min_piece_length() {
        let raw = single_file_torrent("bad.bin", 1024, i64::MIN, None);
        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::InvalidIntegerValue { field: "piece length", value }) if value == i64::MIN
        ));
    }

    #[test]
    fn reject_negative_single_file_length() {
        let raw = single_file_torrent("bad.bin", -1, 512 * 1024, None);
        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::InvalidIntegerValue {
                field: "length",
                value: -1
            })
        ));
    }

    #[test]
    fn reject_negative_multi_file_length() {
        let raw = multi_file_torrent("dir", &[("bad.bin", -1)]);
        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::InvalidIntegerValue {
                field: "file length",
                value: -1
            })
        ));
    }

    #[test]
    fn zero_length_file_accepted() {
        let raw = multi_file_torrent("dir", &[("empty.txt", 0), ("data.bin", 100)]);
        let TorrentMeta::V1(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V1")
        };
        assert_eq!(m.files[0].length, 0);
        assert_eq!(m.files[1].offset, 0); // empty file doesn't advance offset
    }

    #[test]
    fn reject_zero_total_length_torrent() {
        let raw = single_file_torrent_with_piece_count("empty.bin", 0, 512 * 1024, None, 0);
        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::ZeroTotalLength)
        ));
    }

    fn v2_torrent(name: &str, file_name: &str, length: i64) -> Vec<u8> {
        v2_torrent_with_settings(name, file_name, length, 16 * 1024, 2, true)
    }

    fn v2_torrent_with_settings(
        name: &str,
        file_name: &str,
        length: i64,
        piece_length: i64,
        meta_version: i64,
        include_pieces_root: bool,
    ) -> Vec<u8> {
        let layer_count = if length > piece_length && piece_length > 0 {
            usize::try_from((length as u64).div_ceil(piece_length as u64)).unwrap()
        } else {
            0
        };
        let layer_hashes = (0..layer_count)
            .map(|index| rt_hash::BlockHash::of(&index.to_be_bytes()).0)
            .collect::<Vec<_>>();
        let pieces_root = if layer_count > 0 {
            rt_hash::merkle_root(&layer_hashes).to_vec()
        } else {
            vec![0xABu8; 32]
        };
        let mut leaf_pairs: Vec<(&[u8], BValue<'_>)> = vec![(b"length", BValue::Int(length))];
        if include_pieces_root {
            leaf_pairs.push((b"pieces root", BValue::Bytes(&pieces_root)));
        }
        let leaf = BValue::Dict({
            let mut p = leaf_pairs;
            p.sort_by(|a, b| a.0.cmp(b.0));
            p
        });
        let file_node = BValue::Dict(vec![(b"".as_ref(), leaf)]);
        let file_tree = BValue::Dict(vec![(file_name.as_bytes(), file_node)]);

        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"file tree", file_tree),
            (b"meta version", BValue::Int(meta_version)),
            (b"name", BValue::Bytes(name.as_bytes())),
            (b"piece length", BValue::Int(piece_length)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));

        let mut layer_bytes = Vec::with_capacity(layer_hashes.len() * 32);
        for hash in &layer_hashes {
            layer_bytes.extend_from_slice(hash);
        }
        let mut root: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://t.example/a")),
            (b"info", BValue::Dict(info_pairs)),
        ];
        if layer_count > 0 && include_pieces_root {
            root.push((
                b"piece layers",
                BValue::Dict(vec![(pieces_root.as_slice(), BValue::Bytes(&layer_bytes))]),
            ));
        }
        root.sort_by(|a, b| a.0.cmp(b.0));
        encode(&BValue::Dict(root))
    }

    fn v2_torrent_with_shared_piece_layer(files: &[(&str, i64)]) -> Vec<u8> {
        const PIECE_LENGTH: i64 = 16 * 1024;
        assert!(!files.is_empty());
        let hash_count =
            usize::try_from((files[0].1 as u64).div_ceil(PIECE_LENGTH as u64)).unwrap();
        let layer_hashes = (0..hash_count)
            .map(|index| rt_hash::BlockHash::of(&index.to_be_bytes()).0)
            .collect::<Vec<_>>();
        let pieces_root = rt_hash::merkle_root(&layer_hashes);
        let mut layer_bytes = Vec::with_capacity(layer_hashes.len() * 32);
        for hash in &layer_hashes {
            layer_bytes.extend_from_slice(hash);
        }

        let mut file_entries: Vec<(&[u8], BValue<'_>)> = files
            .iter()
            .map(|(name, length)| {
                let mut leaf_fields: Vec<(&[u8], BValue<'_>)> = vec![
                    (b"length", BValue::Int(*length)),
                    (b"pieces root", BValue::Bytes(&pieces_root)),
                ];
                leaf_fields.sort_by(|left, right| left.0.cmp(right.0));
                let leaf = BValue::Dict(leaf_fields);
                let node = BValue::Dict(vec![(b"".as_ref(), leaf)]);
                (name.as_bytes(), node)
            })
            .collect();
        file_entries.sort_by(|left, right| left.0.cmp(right.0));

        let mut info_fields: Vec<(&[u8], BValue<'_>)> = vec![
            (b"file tree", BValue::Dict(file_entries)),
            (b"meta version", BValue::Int(2)),
            (b"name", BValue::Bytes(b"shared")),
            (b"piece length", BValue::Int(PIECE_LENGTH)),
        ];
        info_fields.sort_by(|left, right| left.0.cmp(right.0));

        let mut root_fields: Vec<(&[u8], BValue<'_>)> = vec![
            (b"info", BValue::Dict(info_fields)),
            (
                b"piece layers",
                BValue::Dict(vec![(pieces_root.as_slice(), BValue::Bytes(&layer_bytes))]),
            ),
        ];
        root_fields.sort_by(|left, right| left.0.cmp(right.0));
        encode(&BValue::Dict(root_fields))
    }

    #[test]
    fn parse_v2_torrent() {
        let raw = v2_torrent("mydir", "data.bin", 65536);
        let meta = parse_torrent(&raw).unwrap();
        let TorrentMeta::V2(m) = meta else {
            panic!("expected V2")
        };
        assert_eq!(m.name, "mydir");
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.files[0].length, 65536);
        assert_eq!(m.files[0].path.as_display(), "data.bin");
        assert_eq!(m.info_hash_v2.len(), 32);
    }

    #[test]
    fn identical_layered_files_share_one_piece_layer_and_magnet_requirement() {
        let raw = v2_torrent_with_shared_piece_layer(&[("a.bin", 65_536), ("b.bin", 65_536)]);
        let TorrentMeta::V2(meta) = parse_torrent(&raw).unwrap() else {
            panic!("expected V2")
        };
        assert_eq!(meta.files.len(), 2);
        assert_eq!(meta.files[0].pieces_root, meta.files[1].pieces_root);
        assert_eq!(meta.piece_layers.len(), 1);
        assert_eq!(meta.piece_layers.values().next().unwrap().len(), 4);

        let info = torrent_info_bytes(&raw).unwrap();
        let requirements = v2_piece_layer_requirements(&info).unwrap().unwrap();
        assert_eq!(requirements.files.len(), 1);
        assert_eq!(requirements.files[0].file_length, 65_536);
        assert_eq!(requirements.files[0].hash_count, 4);
    }

    #[test]
    fn identical_piece_root_with_conflicting_file_requirements_is_rejected() {
        let raw = v2_torrent_with_shared_piece_layer(&[("a.bin", 65_536), ("b.bin", 81_920)]);
        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::InvalidPieceLayer(_))
        ));

        let info = torrent_info_bytes(&raw).unwrap();
        assert!(matches!(
            v2_piece_layer_requirements(&info),
            Err(MetainfoError::InvalidPieceLayer(_))
        ));
    }

    #[test]
    fn extracts_v2_piece_layer_requirements_without_top_level_layers() {
        let raw = v2_torrent("mydir", "data.bin", 65_536);
        let info = torrent_info_bytes(&raw).unwrap();
        let requirements = v2_piece_layer_requirements(&info).unwrap().unwrap();

        assert_eq!(requirements.piece_length, 16 * 1024);
        assert_eq!(requirements.files.len(), 1);
        assert_eq!(requirements.files[0].file_length, 65_536);
        assert_eq!(requirements.files[0].hash_count, 4);
    }

    #[test]
    fn v2_piece_layer_requirements_are_empty_for_single_piece_files() {
        let raw = v2_torrent("mydir", "data.bin", 1024);
        let info = torrent_info_bytes(&raw).unwrap();
        let requirements = v2_piece_layer_requirements(&info).unwrap().unwrap();
        assert!(requirements.files.is_empty());
    }

    // Regression coverage for rootless BEP 52 file-tree paths.

    fn v2_multi_file_torrent(dir_name: &str, files: &[(&str, i64)]) -> Vec<u8> {
        let pieces_root = vec![0xCDu8; 32];
        let mut leaves: Vec<(&[u8], BValue<'_>)> = files
            .iter()
            .map(|(fname, length)| {
                let leaf = BValue::Dict({
                    let mut p: Vec<(&[u8], BValue<'_>)> = vec![
                        (b"length", BValue::Int(*length)),
                        (b"pieces root", BValue::Bytes(&pieces_root)),
                    ];
                    p.sort_by(|a, b| a.0.cmp(b.0));
                    p
                });
                let node = BValue::Dict(vec![(b"".as_ref(), leaf)]);
                (fname.as_bytes(), node)
            })
            .collect();
        leaves.sort_by(|a, b| a.0.cmp(b.0));
        let file_tree = BValue::Dict(leaves);

        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"file tree", file_tree),
            (b"meta version", BValue::Int(2)),
            (b"name", BValue::Bytes(dir_name.as_bytes())),
            (b"piece length", BValue::Int(16 * 1024)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));

        let mut root: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://t.example/a")),
            (b"info", BValue::Dict(info_pairs)),
        ];
        root.sort_by(|a, b| a.0.cmp(b.0));
        encode(&BValue::Dict(root))
    }

    fn v2_single_file_in_subdir_torrent(
        name: &str,
        dir: &str,
        file_name: &str,
        length: i64,
    ) -> Vec<u8> {
        let pieces_root = vec![0xEFu8; 32];
        let leaf = BValue::Dict({
            let mut p: Vec<(&[u8], BValue<'_>)> = vec![
                (b"length", BValue::Int(length)),
                (b"pieces root", BValue::Bytes(&pieces_root)),
            ];
            p.sort_by(|a, b| a.0.cmp(b.0));
            p
        });
        let file_node = BValue::Dict(vec![(b"".as_ref(), leaf)]);
        let subdir_node = BValue::Dict(vec![(file_name.as_bytes(), file_node)]);
        let file_tree = BValue::Dict(vec![(dir.as_bytes(), subdir_node)]);

        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"file tree", file_tree),
            (b"meta version", BValue::Int(2)),
            (b"name", BValue::Bytes(name.as_bytes())),
            (b"piece length", BValue::Int(16 * 1024)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));

        let mut root: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://t.example/a")),
            (b"info", BValue::Dict(info_pairs)),
        ];
        root.sort_by(|a, b| a.0.cmp(b.0));
        encode(&BValue::Dict(root))
    }

    #[test]
    fn v2_single_file_uses_rootless_file_tree_path() {
        let raw = v2_torrent("mydir", "data.bin", 65536);
        let TorrentMeta::V2(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V2")
        };
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.files[0].path.as_display(), "data.bin");
    }

    #[test]
    fn v2_leaf_marks_bep47_padding_files() {
        let real_root = vec![0x11u8; 32];
        let pad_root = vec![0x22u8; 32];
        let real_leaf = BValue::Dict({
            let mut p: Vec<(&[u8], BValue<'_>)> = vec![
                (b"length", BValue::Int(1000)),
                (b"pieces root", BValue::Bytes(&real_root)),
            ];
            p.sort_by(|a, b| a.0.cmp(b.0));
            p
        });
        let pad_leaf = BValue::Dict({
            let mut p: Vec<(&[u8], BValue<'_>)> = vec![
                (b"attr", BValue::Bytes(b"p")),
                (b"length", BValue::Int(24)),
                (b"pieces root", BValue::Bytes(&pad_root)),
            ];
            p.sort_by(|a, b| a.0.cmp(b.0));
            p
        });
        let file_tree = BValue::Dict({
            let mut p: Vec<(&[u8], BValue<'_>)> = vec![
                (
                    b"01.flac".as_ref(),
                    BValue::Dict(vec![(b"".as_ref(), real_leaf)]),
                ),
                (
                    b".pad".as_ref(),
                    BValue::Dict(vec![(
                        b"24".as_ref(),
                        BValue::Dict(vec![(b"".as_ref(), pad_leaf)]),
                    )]),
                ),
            ];
            p.sort_by(|a, b| a.0.cmp(b.0));
            p
        });

        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"file tree", file_tree),
            (b"meta version", BValue::Int(2)),
            (b"name", BValue::Bytes(b"album")),
            (b"piece length", BValue::Int(16 * 1024)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let mut root: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://t.example/a")),
            (b"info", BValue::Dict(info_pairs)),
        ];
        root.sort_by(|a, b| a.0.cmp(b.0));
        let raw = encode(&BValue::Dict(root));

        let TorrentMeta::V2(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V2")
        };
        assert_eq!(m.files.len(), 2);
        let real = m
            .files
            .iter()
            .find(|f| f.path.as_display() == "01.flac")
            .unwrap();
        let pad = m
            .files
            .iter()
            .find(|f| f.path.as_display() == ".pad/24")
            .unwrap();
        assert!(!real.pad);
        assert!(pad.pad);
        assert_eq!(m.total_length(), 1_000);
    }

    #[test]
    fn v2_multi_file_preserves_tree_root_without_advisory_name() {
        let raw = v2_multi_file_torrent("album", &[("01.flac", 1000), ("02.flac", 2000)]);
        let TorrentMeta::V2(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V2")
        };
        assert_eq!(m.files.len(), 2);
        let paths: Vec<String> = m.files.iter().map(|f| f.path.as_display()).collect();
        assert!(paths.contains(&"01.flac".to_owned()));
        assert!(paths.contains(&"02.flac".to_owned()));
    }

    #[test]
    fn v2_single_file_nested_in_subdirectory_preserves_tree_path() {
        let raw = v2_single_file_in_subdir_torrent("mydir", "docs", "readme.txt", 42);
        let TorrentMeta::V2(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V2")
        };
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.files[0].path.as_display(), "docs/readme.txt");
    }

    #[test]
    fn reject_negative_v2_file_length() {
        let raw = v2_torrent("mydir", "bad.bin", -1);
        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::InvalidIntegerValue {
                field: "file tree length",
                value: -1
            })
        ));
    }

    #[test]
    fn v2_empty_file_may_omit_pieces_root() {
        let raw = v2_torrent_with_settings("empty", "empty.bin", 0, 16 * 1024, 2, false);
        let TorrentMeta::V2(meta) = parse_torrent(&raw).unwrap() else {
            panic!("expected V2")
        };

        assert_eq!(meta.files.len(), 1);
        assert_eq!(meta.files[0].path.as_display(), "empty.bin");
        assert_eq!(meta.files[0].pieces_root, None);
    }

    #[test]
    fn reject_v2_piece_length_below_protocol_minimum() {
        let raw = v2_torrent_with_settings("dir", "data.bin", 1, 8192, 2, true);

        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::InvalidPieceLength(8192))
        ));
    }

    #[test]
    fn reject_future_metainfo_version_before_v1_fallback() {
        let raw = v2_torrent_with_settings("future", "data.bin", 1, 16 * 1024, 3, true);

        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::UnsupportedMetaVersion(3))
        ));
    }

    #[test]
    fn reject_negative_metainfo_version() {
        let raw = v2_torrent_with_settings("negative", "data.bin", 1, 16 * 1024, -1, false);

        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::InvalidIntegerValue {
                field: "meta version",
                value: -1
            })
        ));
    }

    #[test]
    fn reject_oversized_top_level_tracker_url() {
        let pieces = make_pieces(1);
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"length", BValue::Int(1)),
            (b"name", BValue::Bytes(b"data.bin")),
            (b"piece length", BValue::Int(512 * 1024)),
            (b"pieces", BValue::Bytes(&pieces)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let tracker = vec![b'x'; MAX_TRACKER_URL_BYTES + 1];
        let raw = encode(&BValue::Dict(vec![
            (b"announce".as_ref(), BValue::Bytes(&tracker)),
            (b"info".as_ref(), BValue::Dict(info_pairs)),
        ]));

        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::LimitExceeded {
                field: "tracker url bytes",
                limit: MAX_TRACKER_URL_BYTES
            })
        ));
    }

    #[test]
    fn reject_tracker_url_total_before_retaining_unbounded_tiers() {
        let tracker = vec![b'x'; MAX_TRACKER_URL_BYTES];
        let urls = (0..=MAX_TRACKER_URL_TOTAL_BYTES / MAX_TRACKER_URL_BYTES)
            .map(|_| BValue::Bytes(tracker.as_slice()))
            .collect();
        let root = BValue::Dict(vec![(
            b"announce-list".as_ref(),
            BValue::List(vec![BValue::List(urls)]),
        )]);

        assert!(matches!(
            parse_announce_list(&root, 0, 0),
            Err(MetainfoError::LimitExceeded {
                field: "tracker url total bytes",
                limit: MAX_TRACKER_URL_TOTAL_BYTES,
            })
        ));
    }

    #[test]
    fn reject_oversized_name_before_copying_it() {
        let pieces = make_pieces(1);
        let name = vec![b'n'; MAX_NAME_BYTES + 1];
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"length", BValue::Int(1)),
            (b"name", BValue::Bytes(&name)),
            (b"piece length", BValue::Int(512 * 1024)),
            (b"pieces", BValue::Bytes(&pieces)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let raw = encode(&BValue::Dict(vec![(
            b"info".as_ref(),
            BValue::Dict(info_pairs),
        )]));

        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::LimitExceeded {
                field: "name bytes",
                limit: MAX_NAME_BYTES
            })
        ));
    }

    #[test]
    fn reject_oversized_path_component_before_copying_it() {
        let path_component = "p".repeat(MAX_PATH_COMPONENT_BYTES + 1);
        let raw = multi_file_torrent("dir", &[(&path_component, 1)]);

        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::LimitExceeded {
                field: "path component bytes",
                limit: MAX_PATH_COMPONENT_BYTES
            })
        ));
    }

    #[test]
    fn reject_oversized_optional_metadata_text() {
        let pieces = make_pieces(1);
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"length", BValue::Int(1)),
            (b"name", BValue::Bytes(b"data.bin")),
            (b"piece length", BValue::Int(512 * 1024)),
            (b"pieces", BValue::Bytes(&pieces)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let comment = vec![b'c'; MAX_METADATA_TEXT_BYTES + 1];
        let raw = encode(&BValue::Dict(vec![
            (b"comment".as_ref(), BValue::Bytes(&comment)),
            (b"info".as_ref(), BValue::Dict(info_pairs)),
        ]));

        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::LimitExceeded {
                field: "metadata text bytes",
                limit: MAX_METADATA_TEXT_BYTES
            })
        ));
    }

    #[test]
    fn reject_file_tree_leaf_with_sibling_entries() {
        let pieces_root = [0xABu8; 32];
        let leaf = BValue::Dict(vec![
            (
                b"".as_ref(),
                BValue::Dict(vec![(b"length".as_ref(), BValue::Int(1))]),
            ),
            (b"sibling".as_ref(), BValue::Dict(Vec::new())),
        ]);
        let file_tree = BValue::Dict(vec![(b"data.bin".as_ref(), leaf)]);
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"file tree", file_tree),
            (b"meta version", BValue::Int(2)),
            (b"name", BValue::Bytes(b"tree")),
            (b"piece length", BValue::Int(16 * 1024)),
            (b"pieces root", BValue::Bytes(&pieces_root)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let raw = encode(&BValue::Dict(vec![(
            b"info".as_ref(),
            BValue::Dict(info_pairs),
        )]));

        assert!(matches!(
            parse_torrent(&raw),
            Err(MetainfoError::InvalidFieldType(
                "file tree leaf with sibling entries"
            ))
        ));
    }

    #[test]
    fn v2_infohash_uses_sha256() {
        let raw = v2_torrent("x", "f", 1024);
        let TorrentMeta::V2(m) = parse_torrent(&raw).unwrap() else {
            panic!("expected V2")
        };
        // SHA-256 produces 32-byte output; verify it's non-zero
        assert_ne!(m.info_hash_v2, [0u8; 32]);
    }

    #[test]
    fn torrent_meta_helpers() {
        let raw_v1 = single_file_torrent("t.bin", 512, 512 * 1024, None);
        let meta_v1 = parse_torrent(&raw_v1).unwrap();
        assert!(meta_v1.v1_info_hash().is_some());
        assert!(meta_v1.v2_info_hash().is_none());
        assert_eq!(meta_v1.name(), "t.bin");

        let raw_v2 = v2_torrent("mydir", "f", 1024);
        let meta_v2 = parse_torrent(&raw_v2).unwrap();
        assert!(meta_v2.v1_info_hash().is_none());
        assert!(meta_v2.v2_info_hash().is_some());
    }
}
