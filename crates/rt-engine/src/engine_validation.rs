use super::*;
pub(crate) fn validation_validate_peer_command_len(
    count: usize,
    maximum: usize,
    kind: &str,
) -> CmdResult<()> {
    if count > maximum {
        return Err(format!(
            "{kind} contains {count} addresses; maximum is {maximum}"
        ));
    }
    Ok(())
}

pub(crate) fn validation_validate_command_item_len(
    count: usize,
    maximum: usize,
    kind: &str,
) -> CmdResult<()> {
    if count > maximum {
        return Err(format!(
            "{kind} contains {count} items; maximum is {maximum}"
        ));
    }
    Ok(())
}

pub(crate) fn validation_validate_active_peer_torrent_count(count: usize) -> CmdResult<()> {
    if count > MAX_ENGINE_ACTIVE_PEER_TORRENTS {
        return Err(format!(
            "active peer query spans {count} torrents; maximum is {MAX_ENGINE_ACTIVE_PEER_TORRENTS}"
        ));
    }
    Ok(())
}

pub(crate) fn validation_validate_peer_snapshot_total(count: usize) -> CmdResult<()> {
    if count > MAX_PEER_SNAPSHOT_ITEMS {
        return Err(format!(
            "active peer snapshot contains {count} peers; maximum is {MAX_PEER_SNAPSHOT_ITEMS}"
        ));
    }
    Ok(())
}

pub(crate) fn validation_validate_label_bytes(values: &[String], kind: &str) -> CmdResult<()> {
    let bytes = values
        .iter()
        .fold(0usize, |total, value| total.saturating_add(value.len()));
    if bytes > MAX_ENGINE_LABEL_BYTES {
        return Err(format!(
            "{kind} contains {bytes} bytes; maximum is {MAX_ENGINE_LABEL_BYTES}"
        ));
    }
    Ok(())
}

pub(crate) fn validation_validate_global_tag_set(tags: &[String]) -> CmdResult<()> {
    validation_validate_command_item_len(tags.len(), MAX_ENGINE_MUTATION_ITEMS, "global tag set")?;
    validation_validate_label_bytes(tags, "global tag set")
}

pub(crate) fn validation_validate_text_bytes(
    value: &str,
    maximum: usize,
    kind: &str,
) -> CmdResult<()> {
    if value.len() > maximum {
        return Err(format!(
            "{kind} contains {} bytes; maximum is {maximum}",
            value.len()
        ));
    }
    Ok(())
}

pub(crate) fn validation_validate_text_list_bytes(
    values: &[String],
    maximum: usize,
    aggregate_maximum: usize,
    kind: &str,
) -> CmdResult<()> {
    let mut bytes = 0usize;
    for (index, value) in values.iter().enumerate() {
        if value.len() > maximum {
            return Err(format!(
                "{kind} item at index {index} contains {} bytes; maximum is {maximum}",
                value.len()
            ));
        }
        bytes = bytes.saturating_add(value.len());
        if bytes > aggregate_maximum {
            return Err(format!(
                "{kind} contains {bytes} bytes; maximum is {aggregate_maximum}"
            ));
        }
    }
    Ok(())
}

pub(crate) fn validation_validate_storage_plan_path(
    path: &Path,
    total_bytes: &mut usize,
    kind: &str,
) -> CmdResult<()> {
    let path = path
        .to_str()
        .ok_or_else(|| format!("{kind} is not valid UTF-8"))?;
    validation_validate_text_bytes(path, MAX_ENGINE_STORAGE_PLAN_PATH_BYTES, kind)?;
    *total_bytes = total_bytes.saturating_add(path.len());
    if *total_bytes > MAX_ENGINE_STORAGE_PLAN_TOTAL_PATH_BYTES {
        return Err(format!(
            "storage plan paths contain {} bytes; maximum is {MAX_ENGINE_STORAGE_PLAN_TOTAL_PATH_BYTES}",
            *total_bytes
        ));
    }
    Ok(())
}

pub(crate) fn validation_validate_storage_plan(plan: &StoragePlan) -> CmdResult<()> {
    if plan.can_apply && !plan.issues.is_empty() {
        return Err("storage plan cannot_apply state conflicts with its issues".to_owned());
    }
    validation_validate_command_item_len(
        plan.issues.len(),
        MAX_ENGINE_STORAGE_PLAN_ITEMS,
        "storage plan issue list",
    )?;
    validation_validate_command_item_len(
        plan.steps.len(),
        MAX_ENGINE_STORAGE_PLAN_ITEMS,
        "storage plan step list",
    )?;
    validation_validate_command_item_len(
        plan.rollback_steps.len(),
        MAX_ENGINE_STORAGE_PLAN_ITEMS,
        "storage plan rollback list",
    )?;

    let mut path_bytes = 0usize;
    for issue in &plan.issues {
        match issue {
            rt_storage::PlanIssue::SourceMissing(path)
            | rt_storage::PlanIssue::DestinationExists(path) => {
                validation_validate_storage_plan_path(
                    path,
                    &mut path_bytes,
                    "storage plan issue path",
                )?;
            }
            rt_storage::PlanIssue::InsufficientCapacity { .. }
            | rt_storage::PlanIssue::AtomicNoReplaceRenameUnavailable
            | rt_storage::PlanIssue::DeleteRequiresDryRunApproval => {}
        }
    }
    for step in plan.steps.iter().chain(plan.rollback_steps.iter()) {
        if let Some(path) = step.source.as_deref() {
            validation_validate_storage_plan_path(
                path,
                &mut path_bytes,
                "storage plan source path",
            )?;
        }
        if let Some(path) = step.destination.as_deref() {
            validation_validate_storage_plan_path(
                path,
                &mut path_bytes,
                "storage plan destination path",
            )?;
        }
    }

    let serialized = serde_json::to_vec(plan)
        .map_err(|error| format!("storage plan cannot be serialized: {error}"))?;
    if serialized.len() > MAX_ENGINE_STORAGE_PLAN_SERIALIZED_BYTES {
        return Err(format!(
            "storage plan serialization contains {} bytes; maximum is {MAX_ENGINE_STORAGE_PLAN_SERIALIZED_BYTES}",
            serialized.len()
        ));
    }
    Ok(())
}

pub(crate) fn validation_canonical_info_hash_checked(info_hash: String) -> CmdResult<String> {
    validation_validate_text_bytes(&info_hash, MAX_ENGINE_INFO_HASH_BYTES, "info hash")?;
    Ok(canonical_info_hash(info_hash))
}

pub(crate) fn validation_canonical_info_hash_list(
    info_hashes: Vec<String>,
) -> CmdResult<Vec<String>> {
    validation_validate_command_item_len(
        info_hashes.len(),
        MAX_ENGINE_MUTATION_ITEMS,
        "torrent hash list",
    )?;
    let mut bytes = 0usize;
    let mut canonical = Vec::with_capacity(info_hashes.len());
    for info_hash in info_hashes {
        let info_hash = validation_canonical_info_hash_checked(info_hash)?;
        bytes = bytes.saturating_add(info_hash.len());
        if bytes > MAX_ENGINE_INFO_HASH_LIST_BYTES {
            return Err(format!(
                "torrent hash list contains {bytes} bytes; maximum is {MAX_ENGINE_INFO_HASH_LIST_BYTES}"
            ));
        }
        canonical.push(info_hash);
    }
    Ok(canonical)
}

pub(crate) fn validation_validate_category_value(category: Option<&str>) -> CmdResult<()> {
    if let Some(category) = category {
        validation_validate_text_bytes(category, MAX_ENGINE_CATEGORY_BYTES, "category")?;
    }
    Ok(())
}

pub(crate) fn validation_validate_torrent_limits(limits: &EngineTorrentLimits) -> CmdResult<()> {
    if limits
        .seed_ratio_limit
        .is_some_and(|value| !value.is_finite())
    {
        return Err("torrent seed ratio limit must be finite".to_owned());
    }
    Ok(())
}

pub(crate) fn validation_validate_save_path_value(path: Option<&Path>) -> CmdResult<()> {
    if let Some(path) = path {
        let path = path.to_string_lossy();
        validation_validate_text_bytes(&path, MAX_ENGINE_SAVE_PATH_BYTES, "save path")?;
    }
    Ok(())
}

pub(crate) fn validation_validate_tracker_urls(trackers: &[String]) -> CmdResult<()> {
    validation_validate_command_item_len(
        trackers.len(),
        MAX_ENGINE_MUTATION_ITEMS,
        "tracker list",
    )?;
    let mut bytes = 0usize;
    for (index, tracker) in trackers.iter().enumerate() {
        if tracker.len() > MAX_TRACKER_URL_BYTES {
            return Err(format!(
                "tracker URL at index {index} contains {} bytes; maximum is {MAX_TRACKER_URL_BYTES}",
                tracker.len()
            ));
        }
        bytes = bytes.saturating_add(tracker.len());
        if bytes > MAX_ENGINE_TRACKER_BYTES {
            return Err(format!(
                "tracker list contains {bytes} bytes; maximum is {MAX_ENGINE_TRACKER_BYTES}"
            ));
        }
        let tracker = tracker.trim();
        debug_assert!(tracker.len() <= MAX_TRACKER_URL_BYTES);
    }
    Ok(())
}

pub(crate) fn validation_tracker_urls_bytes(trackers: &[String]) -> usize {
    trackers
        .iter()
        .map(|tracker| tracker.trim().len())
        .fold(0usize, usize::saturating_add)
}

pub(crate) fn validation_validate_meta_tracker_fields(
    announce: Option<&String>,
    announce_list: &[Vec<String>],
) -> CmdResult<()> {
    validation_validate_command_item_len(
        announce_list.len(),
        MAX_ENGINE_META_TRACKER_TIERS,
        "metainfo tracker tier list",
    )?;
    let mut seen = HashSet::<String>::new();
    let mut count = 0usize;
    let mut bytes = 0usize;
    let mut validate = |tracker: &str| -> CmdResult<()> {
        if tracker.len() > MAX_TRACKER_URL_BYTES {
            return Err(format!(
                "metainfo tracker URL at index {count} contains {} bytes; maximum is {MAX_TRACKER_URL_BYTES}",
                tracker.len()
            ));
        }
        bytes = bytes.saturating_add(tracker.len());
        if bytes > MAX_ENGINE_TRACKER_BYTES {
            return Err(format!(
                "metainfo tracker list contains {bytes} bytes; maximum is {MAX_ENGINE_TRACKER_BYTES}"
            ));
        }
        let tracker = tracker.trim();
        if tracker.is_empty() || seen.contains(tracker) {
            return Ok(());
        }
        if count >= MAX_ENGINE_MUTATION_ITEMS {
            return Err(format!(
                "metainfo tracker list contains more than {MAX_ENGINE_MUTATION_ITEMS} items"
            ));
        }
        seen.insert(tracker.to_owned());
        count += 1;
        Ok(())
    };

    if let Some(announce) = announce {
        validate(announce)?;
    }
    for tier in announce_list {
        for tracker in tier {
            validate(tracker)?;
        }
    }
    Ok(())
}

pub(crate) fn validation_validate_meta_tracker_urls(meta: &TorrentMeta) -> CmdResult<()> {
    match meta {
        TorrentMeta::V1(meta) => {
            validation_validate_meta_tracker_fields(meta.announce.as_ref(), &meta.announce_list)
        }
        // The v1 metainfo is the authoritative tracker projection for a
        // hybrid torrent, matching `meta_all_trackers` and the v1 runtime
        // task that the engine can actually start.
        TorrentMeta::Hybrid(meta, _) => {
            validation_validate_meta_tracker_fields(meta.announce.as_ref(), &meta.announce_list)
        }
        TorrentMeta::V2(meta) => {
            validation_validate_meta_tracker_fields(meta.announce.as_ref(), &meta.announce_list)
        }
    }
}

pub(crate) fn validation_validate_meta_webseed_urls(webseeds: &[String]) -> CmdResult<()> {
    validation_validate_command_item_len(
        webseeds.len(),
        MAX_ENGINE_META_WEBSEEDS,
        "metainfo webseed list",
    )?;
    validation_validate_text_list_bytes(
        webseeds,
        MAX_TRACKER_URL_BYTES,
        MAX_ENGINE_META_WEBSEED_BYTES,
        "metainfo webseed list",
    )
}

pub(crate) fn validation_safe_relative_path_bytes(path: &SafeRelPath) -> usize {
    path.components()
        .iter()
        .map(String::len)
        .fold(0usize, usize::saturating_add)
        .saturating_add(path.components().len().saturating_sub(1))
}

pub(crate) fn validation_validate_meta_files<'a, I>(files: I) -> CmdResult<u64>
where
    I: IntoIterator<Item = (usize, &'a SafeRelPath, u64, u64)>,
{
    let mut count = 0usize;
    let mut total_bytes = 0usize;
    let mut total_length = 0u64;
    for (index, path, length, offset) in files {
        count = count.saturating_add(1);
        if count > MAX_ENGINE_META_FILES {
            return Err(format!(
                "metainfo file list contains more than {MAX_ENGINE_META_FILES} items"
            ));
        }
        let expected_index = count - 1;
        if index != expected_index {
            return Err(format!(
                "metainfo file index {index} is not the expected index {expected_index}"
            ));
        }
        if offset != total_length {
            return Err(format!(
                "metainfo file offset {offset} at index {index} does not match expected {total_length}"
            ));
        }
        total_length = total_length.checked_add(length).ok_or_else(|| {
            "metainfo file lengths overflow the engine's unsigned total".to_owned()
        })?;
        let path_bytes = validation_safe_relative_path_bytes(path);
        if path_bytes > MAX_ENGINE_META_PATH_BYTES {
            return Err(format!(
                "metainfo file path at index {index} contains {path_bytes} bytes; maximum is {MAX_ENGINE_META_PATH_BYTES}"
            ));
        }
        total_bytes = total_bytes.saturating_add(path_bytes);
        if total_bytes > MAX_ENGINE_META_PATH_TOTAL_BYTES {
            return Err(format!(
                "metainfo file paths contain {total_bytes} bytes; maximum is {MAX_ENGINE_META_PATH_TOTAL_BYTES}"
            ));
        }
    }
    Ok(total_length)
}

pub(crate) fn validation_validate_meta_common(
    name: &str,
    comment: Option<&String>,
    created_by: Option<&String>,
    webseeds: &[String],
    announce: Option<&String>,
    announce_list: &[Vec<String>],
) -> CmdResult<()> {
    validation_validate_text_bytes(name, MAX_ENGINE_NAME_BYTES, "torrent name")?;
    if let Some(comment) = comment {
        validation_validate_text_bytes(comment, MAX_ENGINE_METADATA_TEXT_BYTES, "torrent comment")?;
    }
    if let Some(created_by) = created_by {
        validation_validate_text_bytes(
            created_by,
            MAX_ENGINE_METADATA_TEXT_BYTES,
            "torrent created-by",
        )?;
    }
    validation_validate_meta_tracker_fields(announce, announce_list)?;
    validation_validate_meta_webseed_urls(webseeds)
}

pub(crate) fn validation_validate_torrent_meta(meta: &TorrentMeta) -> CmdResult<()> {
    let raw_bytes = meta_raw(meta).len();
    if raw_bytes > MAX_TORRENT_BYTES {
        return Err(format!(
            "torrent metainfo contains {raw_bytes} bytes; maximum is {MAX_TORRENT_BYTES}"
        ));
    }
    match meta {
        TorrentMeta::V1(meta) => {
            validation_validate_meta_common(
                &meta.name,
                meta.comment.as_ref(),
                meta.created_by.as_ref(),
                &meta.webseeds,
                meta.announce.as_ref(),
                &meta.announce_list,
            )?;
            let total = validation_validate_meta_files(
                meta.files
                    .iter()
                    .enumerate()
                    .map(|(index, file)| (index, &file.path, file.length, file.offset)),
            )?;
            if total == 0 || !meta.files.iter().any(|file| !file.pad && file.length > 0) {
                return Err("torrent total length must be greater than zero".to_owned());
            }
            if meta.piece_length == 0 || meta.piece_length > u64::from(u32::MAX) {
                return Err(format!(
                    "torrent piece length {} is outside the engine range",
                    meta.piece_length
                ));
            }
            let expected_pieces = total
                .checked_add(meta.piece_length - 1)
                .ok_or_else(|| "torrent piece count overflow".to_owned())?
                / meta.piece_length;
            let expected_pieces = usize::try_from(expected_pieces)
                .map_err(|_| "torrent piece count does not fit in memory".to_owned())?;
            if expected_pieces > MAX_ENGINE_META_PIECES {
                return Err(format!(
                    "torrent contains {expected_pieces} pieces; maximum is {MAX_ENGINE_META_PIECES}"
                ));
            }
            if meta.pieces.len() != expected_pieces {
                return Err(format!(
                    "torrent piece count {} does not match expected {expected_pieces}",
                    meta.pieces.len()
                ));
            }
        }
        TorrentMeta::V2(meta) => {
            validation_validate_meta_common(
                &meta.name,
                meta.comment.as_ref(),
                meta.created_by.as_ref(),
                &meta.webseeds,
                meta.announce.as_ref(),
                &meta.announce_list,
            )?;
            let total = validation_validate_meta_files(
                meta.files
                    .iter()
                    .enumerate()
                    .map(|(index, file)| (index, &file.path, file.length, file.offset)),
            )?;
            if total == 0 || !meta.files.iter().any(|file| !file.pad && file.length > 0) {
                return Err("torrent total length must be greater than zero".to_owned());
            }
            if meta.piece_length == 0 || meta.piece_length > u64::from(u32::MAX) {
                return Err(format!(
                    "torrent piece length {} is outside the engine range",
                    meta.piece_length
                ));
            }
            let piece_count = usize::try_from(meta.piece_count())
                .map_err(|_| "torrent piece count does not fit in memory".to_owned())?;
            if piece_count > MAX_ENGINE_META_PIECES {
                return Err(format!(
                    "torrent contains {piece_count} pieces; maximum is {MAX_ENGINE_META_PIECES}"
                ));
            }
        }
        TorrentMeta::Hybrid(v1, v2) => {
            validation_validate_meta_common(
                &v1.name,
                v1.comment.as_ref(),
                v1.created_by.as_ref(),
                &v1.webseeds,
                v1.announce.as_ref(),
                &v1.announce_list,
            )?;
            validation_validate_meta_common(
                &v2.name,
                v2.comment.as_ref(),
                v2.created_by.as_ref(),
                &v2.webseeds,
                v2.announce.as_ref(),
                &v2.announce_list,
            )?;
            let total = validation_validate_meta_files(
                v1.files
                    .iter()
                    .enumerate()
                    .map(|(index, file)| (index, &file.path, file.length, file.offset)),
            )?;
            validation_validate_meta_files(
                v2.files
                    .iter()
                    .enumerate()
                    .map(|(index, file)| (index, &file.path, file.length, file.offset)),
            )?;
            validate_hybrid_file_layout(v1, v2).map_err(|error| error.to_string())?;
            if total == 0 {
                return Err("torrent total length must be greater than zero".to_owned());
            }
            if v1.piece_length == 0 || v1.piece_length > u64::from(u32::MAX) {
                return Err(format!(
                    "torrent piece length {} is outside the engine range",
                    v1.piece_length
                ));
            }
            let expected_pieces = total
                .checked_add(v1.piece_length - 1)
                .ok_or_else(|| "torrent piece count overflow".to_owned())?
                / v1.piece_length;
            let expected_pieces = usize::try_from(expected_pieces)
                .map_err(|_| "torrent piece count does not fit in memory".to_owned())?;
            if expected_pieces > MAX_ENGINE_META_PIECES {
                return Err(format!(
                    "torrent contains {expected_pieces} pieces; maximum is {MAX_ENGINE_META_PIECES}"
                ));
            }
            if v1.pieces.len() != expected_pieces {
                return Err(format!(
                    "torrent piece count {} does not match expected {expected_pieces}",
                    v1.pieces.len()
                ));
            }
            if v2.piece_length == 0 || v2.piece_length > u64::from(u32::MAX) {
                return Err(format!(
                    "torrent v2 piece length {} is outside the engine range",
                    v2.piece_length
                ));
            }
            let v2_piece_count = usize::try_from(v2.piece_count())
                .map_err(|_| "torrent v2 piece count does not fit in memory".to_owned())?;
            if v2_piece_count > MAX_ENGINE_META_PIECES {
                return Err(format!(
                    "torrent contains {v2_piece_count} v2 pieces; maximum is {MAX_ENGINE_META_PIECES}"
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn validation_validate_magnet_input(magnet: &MagnetLink) -> CmdResult<()> {
    if let Some(display_name) = magnet.display_name.as_ref() {
        validation_validate_text_bytes(display_name, MAX_ENGINE_NAME_BYTES, "magnet display name")?;
    }
    validation_validate_tracker_urls(&magnet.trackers)?;
    if magnet.peer_addresses.len() > MAX_ENGINE_MAGNET_PEERS {
        return Err(format!(
            "magnet contains {} direct peers; maximum is {MAX_ENGINE_MAGNET_PEERS}",
            magnet.peer_addresses.len()
        ));
    }
    if let Some((index, _)) = magnet
        .peer_addresses
        .iter()
        .enumerate()
        .find(|(_, peer)| peer.port() == 0)
    {
        return Err(format!(
            "magnet direct peer at index {index} has a zero port"
        ));
    }
    Ok(())
}
