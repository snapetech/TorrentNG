//! Reverse migration: project native TorrentNG state back into other clients.
//!
//! This is the exit path / anti-lock-in feature. It reads the native model
//! read-only (`rt-db` rows + persisted `.torrent` blobs + `rt-fastresume`
//! state) and writes per-client resume files so a user leaving TorrentNG can
//! resume seeding elsewhere without a full recheck wherever the target format
//! can carry piece state.
//!
//! Fidelity mirrors the import asymmetry:
//!
//! - libtorrent (`qBittorrent`/`Deluge`) and Transmission carry a full piece
//!   map → recheck-free, partials included.
//! - rTorrent only trusts complete-state sidecars → recheck-free for complete
//!   torrents, metadata-only for partials.
//! - uTorrent/BiglyBT aggregate formats carry a completed-piece bitfield →
//!   recheck-free at whole-piece granularity.
//! - Generic always works: it copies the `.torrent` files plus a manifest;
//!   the destination rechecks.

use std::collections::BTreeMap;
use std::path::Path;

use rt_db::{DbError, TorrentFileRow, TorrentRow, TorrentTrackerRow};
use rt_fastresume::{FastresumeStore, PieceState};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Db(#[from] DbError),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Target client format for a reverse export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExportFormat {
    /// `.torrent` files + a JSON manifest. Always correct; destination rechecks.
    Generic,
    /// libtorrent `.fastresume` + `.torrent` (qBittorrent `BT_backup` / Deluge).
    Libtorrent,
    /// Transmission `torrents/` + `resume/` bencoded resume.
    Transmission,
    /// rTorrent session `.torrent` + `.rtorrent` sidecar.
    Rtorrent,
    /// uTorrent/BitTorrent classic aggregate `resume.dat`.
    Utorrent,
    /// BiglyBT/Vuze aggregate `downloads.config`.
    Biglybt,
}

impl ExportFormat {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.to_ascii_lowercase().as_str() {
            "generic" => Self::Generic,
            "libtorrent" | "qbittorrent" | "qbit" | "qb" | "deluge" => Self::Libtorrent,
            "transmission" => Self::Transmission,
            "rtorrent" => Self::Rtorrent,
            "utorrent" | "bittorrent" => Self::Utorrent,
            "biglybt" | "vuze" => Self::Biglybt,
            _ => return None,
        })
    }

    /// Aggregate formats accumulate every torrent into one file.
    fn is_aggregate(self) -> bool {
        matches!(self, Self::Utorrent | Self::Biglybt)
    }
}

/// How faithfully a torrent will transfer to the chosen target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExportFidelity {
    /// Piece state emitted in a form the target trusts → no full recheck.
    RecheckFree,
    /// Recheck-free only because the torrent is complete (rTorrent partials).
    CompleteOnly,
    /// Metadata/paths/counters carried, but the target will recheck content.
    MetadataOnly,
    /// Only the `.torrent` file is exported (no native resume state found).
    TorrentOnly,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportFidelitySummary {
    pub recheck_free: usize,
    pub complete_only: usize,
    pub metadata_only: usize,
    pub torrent_only: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedExport {
    pub info_hash: String,
    pub reason: String,
}

/// One torrent's native state, gathered read-only for export.
#[derive(Debug, Clone)]
pub struct ExportTorrent {
    pub info_hash: String,
    pub raw_torrent: Vec<u8>,
    pub row: TorrentRow,
    pub files: Vec<TorrentFileRow>,
    pub trackers: Vec<TorrentTrackerRow>,
    /// Per-piece have map. `None` when no fastresume state was found.
    pub have: Option<Vec<bool>>,
    pub partials: Vec<(u32, Vec<u32>)>,
    pub uploaded: u64,
    pub downloaded: u64,
}

impl ExportTorrent {
    fn is_complete(&self) -> bool {
        self.have.as_ref().is_some_and(|h| h.iter().all(|&b| b))
    }

    fn raw_info_hash(&self) -> Option<Vec<u8>> {
        decode_hex(&self.info_hash)
    }

    fn fidelity(&self, format: ExportFormat) -> ExportFidelity {
        match format {
            ExportFormat::Generic => ExportFidelity::TorrentOnly,
            _ if self.have.is_none() => ExportFidelity::TorrentOnly,
            ExportFormat::Libtorrent | ExportFormat::Transmission => ExportFidelity::RecheckFree,
            ExportFormat::Utorrent | ExportFormat::Biglybt => ExportFidelity::RecheckFree,
            ExportFormat::Rtorrent => {
                if self.is_complete() {
                    ExportFidelity::CompleteOnly
                } else {
                    ExportFidelity::MetadataOnly
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExportPlan {
    pub format: ExportFormat,
    pub torrents: Vec<ExportTorrent>,
    pub skipped: Vec<SkippedExport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportSummary {
    pub torrents: usize,
    pub files_written: usize,
    pub fidelity: ExportFidelitySummary,
}

/// Read native state read-only. `blob_dir` is `session_dir/torrents`,
/// `fastresume_dir` is `session_dir/fastresume`.
pub fn gather(
    db_path: &Path,
    blob_dir: &Path,
    fastresume_dir: &Path,
) -> Result<(Vec<ExportTorrent>, Vec<SkippedExport>), ExportError> {
    let conn = rusqlite::Connection::open(db_path)?;
    let rows = rt_db::list_all(&conn)?;
    let store = FastresumeStore::new(fastresume_dir.to_path_buf());
    let mut torrents = Vec::new();
    let mut skipped = Vec::new();

    for row in rows {
        let blob_path = blob_dir.join(format!("{}.torrent", row.info_hash));
        let raw_torrent = match std::fs::read(&blob_path) {
            Ok(bytes) => bytes,
            Err(_) => {
                skipped.push(SkippedExport {
                    info_hash: row.info_hash.clone(),
                    reason: format!("missing .torrent blob at {}", blob_path.display()),
                });
                continue;
            }
        };
        let files = rt_db::list_torrent_files(&conn, &row.info_hash)?;
        let trackers = rt_db::list_torrent_trackers(&conn, &row.info_hash)?;

        let (have, partials, uploaded, downloaded) = match store.load(&row.info_hash) {
            Ok(state) => {
                let have = state
                    .pieces
                    .iter()
                    .map(|p| *p == PieceState::Valid)
                    .collect::<Vec<_>>();
                let partials = state
                    .partial_pieces
                    .iter()
                    .map(|p| (p.piece, p.received_blocks.clone()))
                    .collect();
                (
                    Some(have),
                    partials,
                    state.uploaded_bytes,
                    state.downloaded_bytes,
                )
            }
            Err(_) => (
                None,
                Vec::new(),
                row.uploaded.max(0) as u64,
                row.downloaded.max(0) as u64,
            ),
        };

        torrents.push(ExportTorrent {
            info_hash: row.info_hash.clone(),
            raw_torrent,
            row,
            files,
            trackers,
            have,
            partials,
            uploaded,
            downloaded,
        });
    }

    torrents.sort_by(|a, b| a.info_hash.cmp(&b.info_hash));
    skipped.sort_by(|a, b| a.info_hash.cmp(&b.info_hash));
    Ok((torrents, skipped))
}

impl ExportPlan {
    pub fn new(
        format: ExportFormat,
        db_path: &Path,
        blob_dir: &Path,
        fastresume_dir: &Path,
    ) -> Result<Self, ExportError> {
        let (torrents, skipped) = gather(db_path, blob_dir, fastresume_dir)?;
        Ok(Self {
            format,
            torrents,
            skipped,
        })
    }

    pub fn torrent_count(&self) -> usize {
        self.torrents.len()
    }

    pub fn fidelity_summary(&self) -> ExportFidelitySummary {
        let mut s = ExportFidelitySummary::default();
        for t in &self.torrents {
            match t.fidelity(self.format) {
                ExportFidelity::RecheckFree => s.recheck_free += 1,
                ExportFidelity::CompleteOnly => s.complete_only += 1,
                ExportFidelity::MetadataOnly => s.metadata_only += 1,
                ExportFidelity::TorrentOnly => s.torrent_only += 1,
            }
        }
        s
    }

    pub fn to_markdown(&self) -> String {
        let s = self.fidelity_summary();
        let mut out = String::new();
        out.push_str("# TorrentNG Reverse Export (dry run)\n\n");
        out.push_str(&format!("- Target format: {:?}\n", self.format));
        out.push_str(&format!("- Torrents: {}\n", self.torrents.len()));
        out.push_str(&format!(
            "- Fidelity: {} recheck-free, {} complete-only, {} metadata-only, {} torrent-only\n",
            s.recheck_free, s.complete_only, s.metadata_only, s.torrent_only
        ));
        out.push_str(&format!("- Skipped: {}\n\n", self.skipped.len()));
        out.push_str("| Info hash | Name | Fidelity |\n| --- | --- | --- |\n");
        for t in &self.torrents {
            out.push_str(&format!(
                "| `{}` | {} | {:?} |\n",
                t.info_hash,
                t.row.name.replace('|', "\\|"),
                t.fidelity(self.format)
            ));
        }
        if !self.skipped.is_empty() {
            out.push_str("\n## Skipped\n\n");
            for sk in &self.skipped {
                out.push_str(&format!("- `{}`: {}\n", sk.info_hash, sk.reason));
            }
        }
        out
    }

    /// Write the export under `out_dir`. The native state is never modified.
    pub fn write(&self, out_dir: &Path) -> Result<ExportSummary, ExportError> {
        std::fs::create_dir_all(out_dir)?;
        let mut files_written = 0usize;
        let mut aggregate: Vec<(Vec<u8>, Ben)> = Vec::new();

        for t in &self.torrents {
            files_written += self.write_one(t, out_dir, &mut aggregate)?;
        }

        if self.format.is_aggregate() && !aggregate.is_empty() {
            aggregate.sort_by(|a, b| a.0.cmp(&b.0));
            let name = match self.format {
                ExportFormat::Utorrent => "resume.dat",
                ExportFormat::Biglybt => "downloads.config",
                _ => unreachable!(),
            };
            std::fs::write(out_dir.join(name), encode_ben(&Ben::D(aggregate)))?;
            files_written += 1;
        }

        if self.format == ExportFormat::Generic {
            let manifest = self
                .torrents
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "info_hash": t.info_hash,
                        "name": t.row.name,
                        "save_path": t.row.save_path,
                        "category": t.row.category,
                        "tags": t.row.tags,
                        "trackers": t.row.trackers,
                        "uploaded": t.uploaded,
                        "downloaded": t.downloaded,
                        "complete": t.is_complete(),
                        "have_pieces": t.have.as_ref().map(|h| h.iter().filter(|b| **b).count()),
                        "total_pieces": t.row.piece_count,
                    })
                })
                .collect::<Vec<_>>();
            std::fs::write(
                out_dir.join("manifest.json"),
                serde_json::to_vec_pretty(&manifest)?,
            )?;
            files_written += 1;
        }

        Ok(ExportSummary {
            torrents: self.torrents.len(),
            files_written,
            fidelity: self.fidelity_summary(),
        })
    }

    fn write_one(
        &self,
        t: &ExportTorrent,
        out_dir: &Path,
        aggregate: &mut Vec<(Vec<u8>, Ben)>,
    ) -> Result<usize, ExportError> {
        let hash = &t.info_hash;
        match self.format {
            ExportFormat::Generic => {
                std::fs::write(out_dir.join(format!("{hash}.torrent")), &t.raw_torrent)?;
                Ok(1)
            }
            ExportFormat::Libtorrent => {
                std::fs::write(out_dir.join(format!("{hash}.torrent")), &t.raw_torrent)?;
                std::fs::write(
                    out_dir.join(format!("{hash}.fastresume")),
                    encode_ben(&self.libtorrent_resume(t)),
                )?;
                Ok(2)
            }
            ExportFormat::Transmission => {
                let torrents = out_dir.join("torrents");
                let resume = out_dir.join("resume");
                std::fs::create_dir_all(&torrents)?;
                std::fs::create_dir_all(&resume)?;
                std::fs::write(torrents.join(format!("{hash}.torrent")), &t.raw_torrent)?;
                std::fs::write(
                    resume.join(format!("{hash}.resume")),
                    encode_ben(&self.transmission_resume(t)),
                )?;
                Ok(2)
            }
            ExportFormat::Rtorrent => {
                std::fs::write(out_dir.join(format!("{hash}.torrent")), &t.raw_torrent)?;
                std::fs::write(
                    out_dir.join(format!("{hash}.rtorrent")),
                    encode_ben(&self.rtorrent_resume(t)),
                )?;
                Ok(2)
            }
            ExportFormat::Utorrent => {
                std::fs::write(out_dir.join(format!("{hash}.torrent")), &t.raw_torrent)?;
                aggregate.push((hash.clone().into_bytes(), self.utorrent_entry(t)));
                Ok(1)
            }
            ExportFormat::Biglybt => {
                std::fs::write(out_dir.join(format!("{hash}.torrent")), &t.raw_torrent)?;
                aggregate.push((hash.clone().into_bytes(), self.biglybt_entry(t)));
                Ok(1)
            }
        }
    }

    fn tracker_tiers(&self, t: &ExportTorrent) -> Ben {
        let mut by_tier: BTreeMap<i64, Vec<String>> = BTreeMap::new();
        if t.trackers.is_empty() {
            for url in &t.row.trackers {
                by_tier.entry(0).or_default().push(url.clone());
            }
        } else {
            for tr in &t.trackers {
                by_tier.entry(tr.tier).or_default().push(tr.url.clone());
            }
        }
        Ben::L(
            by_tier
                .into_values()
                .map(|tier| Ben::L(tier.into_iter().map(Ben::s).collect()))
                .collect(),
        )
    }

    fn libtorrent_resume(&self, t: &ExportTorrent) -> Ben {
        let mut d: Vec<(Vec<u8>, Ben)> = vec![
            (b"file-format".to_vec(), Ben::s("libtorrent resume file")),
            (b"file-version".to_vec(), Ben::I(1)),
            (b"libtorrent-version".to_vec(), Ben::s("2.0.0")),
            (b"paused".to_vec(), Ben::I(0)),
            (b"save_path".to_vec(), Ben::s(&t.row.save_path)),
            (b"qBt-savePath".to_vec(), Ben::s(&t.row.save_path)),
            (b"total_uploaded".to_vec(), Ben::I(t.uploaded as i64)),
            (b"total_downloaded".to_vec(), Ben::I(t.downloaded as i64)),
            (b"trackers".to_vec(), self.tracker_tiers(t)),
        ];
        if let Some(raw) = t.raw_info_hash() {
            d.push((b"info-hash".to_vec(), Ben::B(raw)));
        }
        if let Some(cat) = &t.row.category {
            d.push((b"qBt-category".to_vec(), Ben::s(cat)));
        }
        if !t.row.tags.is_empty() {
            d.push((
                b"qBt-tags".to_vec(),
                Ben::L(t.row.tags.iter().map(Ben::s).collect()),
            ));
        }
        if let Some(have) = &t.have {
            d.push((b"pieces".to_vec(), Ben::B(have_to_piece_bytes(have))));
            if !t.partials.is_empty() {
                d.push((
                    b"unfinished".to_vec(),
                    Ben::D(
                        t.partials
                            .iter()
                            .map(|(piece, blocks)| {
                                (
                                    piece.to_string().into_bytes(),
                                    Ben::L(blocks.iter().map(|b| Ben::I(*b as i64)).collect()),
                                )
                            })
                            .collect(),
                    ),
                ));
            }
        }
        Ben::D(d)
    }

    fn transmission_resume(&self, t: &ExportTorrent) -> Ben {
        let mut progress: Vec<(Vec<u8>, Ben)> = Vec::new();
        if let Some(have) = &t.have {
            progress.push((b"have".to_vec(), Ben::B(have_to_bitfield(have))));
        }
        Ben::D(vec![
            (b"corrupt".to_vec(), Ben::I(0)),
            (b"destination".to_vec(), Ben::s(&t.row.save_path)),
            (b"name".to_vec(), Ben::s(&t.row.name)),
            (b"uploaded".to_vec(), Ben::I(t.uploaded as i64)),
            (b"downloaded".to_vec(), Ben::I(t.downloaded as i64)),
            (b"progress".to_vec(), Ben::D(progress)),
        ])
    }

    fn rtorrent_resume(&self, t: &ExportTorrent) -> Ben {
        let mut d: Vec<(Vec<u8>, Ben)> = vec![
            (
                b"complete".to_vec(),
                Ben::I(if t.is_complete() { 1 } else { 0 }),
            ),
            (b"directory".to_vec(), Ben::s(&t.row.save_path)),
            (b"uploaded".to_vec(), Ben::I(t.uploaded as i64)),
            (b"downloaded".to_vec(), Ben::I(t.downloaded as i64)),
        ];
        if let Some(cat) = &t.row.category {
            d.push((b"d.custom1".to_vec(), Ben::s(cat)));
        }
        if let Some(finished) = t.row.completed_at {
            d.push((b"timestamp.finished".to_vec(), Ben::I(finished)));
        }
        Ben::D(d)
    }

    fn utorrent_entry(&self, t: &ExportTorrent) -> Ben {
        let mut d: Vec<(Vec<u8>, Ben)> = vec![
            (b"caption".to_vec(), Ben::s(&t.row.name)),
            (b"path".to_vec(), Ben::s(&t.row.save_path)),
            (b"uploaded".to_vec(), Ben::I(t.uploaded as i64)),
            (b"downloaded".to_vec(), Ben::I(t.downloaded as i64)),
        ];
        if let Some(cat) = &t.row.category {
            d.push((b"label".to_vec(), Ben::s(cat)));
        }
        if let Some(have) = &t.have {
            d.push((b"have".to_vec(), Ben::B(have_to_bitfield(have))));
        }
        Ben::D(d)
    }

    fn biglybt_entry(&self, t: &ExportTorrent) -> Ben {
        let mut d: Vec<(Vec<u8>, Ben)> = vec![
            (b"save_dir".to_vec(), Ben::s(&t.row.save_path)),
            (b"uploadedEver".to_vec(), Ben::I(t.uploaded as i64)),
            (b"downloadedEver".to_vec(), Ben::I(t.downloaded as i64)),
        ];
        if let Some(have) = &t.have {
            d.push((
                b"resume data".to_vec(),
                Ben::D(vec![(b"valid".to_vec(), Ben::B(have_to_bitfield(have)))]),
            ));
        }
        Ben::D(d)
    }
}

// --- minimal owned bencode encoder (sorts dict keys; canonical) ---

#[derive(Debug, Clone)]
enum Ben {
    I(i64),
    B(Vec<u8>),
    L(Vec<Ben>),
    D(Vec<(Vec<u8>, Ben)>),
}

impl Ben {
    fn s(text: impl AsRef<str>) -> Ben {
        Ben::B(text.as_ref().as_bytes().to_vec())
    }
}

fn encode_ben(value: &Ben) -> Vec<u8> {
    let mut out = Vec::new();
    enc(value, &mut out);
    out
}

fn enc(value: &Ben, out: &mut Vec<u8>) {
    match value {
        Ben::I(n) => {
            out.push(b'i');
            out.extend_from_slice(n.to_string().as_bytes());
            out.push(b'e');
        }
        Ben::B(bytes) => {
            out.extend_from_slice(bytes.len().to_string().as_bytes());
            out.push(b':');
            out.extend_from_slice(bytes);
        }
        Ben::L(items) => {
            out.push(b'l');
            for item in items {
                enc(item, out);
            }
            out.push(b'e');
        }
        Ben::D(pairs) => {
            let mut pairs = pairs.clone();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            out.push(b'd');
            for (key, val) in &pairs {
                out.extend_from_slice(key.len().to_string().as_bytes());
                out.push(b':');
                out.extend_from_slice(key);
                enc(val, out);
            }
            out.push(b'e');
        }
    }
}

/// libtorrent `pieces`: one byte per piece, 1 = have, 0 = missing.
fn have_to_piece_bytes(have: &[bool]) -> Vec<u8> {
    have.iter().map(|&b| u8::from(b)).collect()
}

/// MSB-first piece bitfield (Transmission/uTorrent/BiglyBT).
fn have_to_bitfield(have: &[bool]) -> Vec<u8> {
    let mut out = vec![0u8; have.len().div_ceil(8)];
    for (i, &b) in have.iter().enumerate() {
        if b {
            out[i / 8] |= 0x80 >> (i % 8);
        }
    }
    out
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        dry_run_biglybt_config, dry_run_generic_torrent_directory, dry_run_qbittorrent_backup,
        dry_run_rtorrent_session, dry_run_transmission_session, dry_run_utorrent_config,
        ResumeConfidence,
    };
    use rt_bencode::{encode, BValue};
    use rt_fastresume::{FastresumeState, ImportPolicy};
    use std::path::PathBuf;

    fn fixture_torrent() -> (Vec<u8>, [u8; 20]) {
        let pieces = [9u8; 20];
        let mut info = vec![
            (b"length".as_slice(), BValue::Int(12)),
            (b"name".as_slice(), BValue::Bytes(b"sample.bin")),
            (b"piece length".as_slice(), BValue::Int(16_384)),
            (b"pieces".as_slice(), BValue::Bytes(&pieces)),
        ];
        info.sort_by(|a, b| a.0.cmp(b.0));
        let raw = encode(&BValue::Dict(vec![(
            b"info".as_slice(),
            BValue::Dict(info),
        )]));
        let hash = match rt_metainfo::parse_torrent(&raw).unwrap() {
            rt_metainfo::TorrentMeta::V1(m) => m.info_hash,
            rt_metainfo::TorrentMeta::Hybrid(m, _) => m.info_hash,
            rt_metainfo::TorrentMeta::V2(_) => unreachable!(),
        };
        (raw, hash)
    }

    fn hex_lower(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// Build a one-torrent native state on disk: returns (session_dir, db_path,
    /// blob_dir, fastresume_dir, info_hash_hex, save_path).
    fn native_fixture(tmp: &Path, complete: bool) -> (PathBuf, PathBuf, PathBuf, String, PathBuf) {
        let (raw, hash) = fixture_torrent();
        let hash_hex = hex_lower(&hash);
        let db_path = tmp.join("state.db");
        let blob_dir = tmp.join("torrents");
        let fr_dir = tmp.join("fastresume");
        let save_path = tmp.join("data");
        std::fs::create_dir_all(&blob_dir).unwrap();
        std::fs::create_dir_all(&save_path).unwrap();
        // Real data file so rTorrent file-hint synthesis can validate.
        std::fs::write(save_path.join("sample.bin"), [1u8; 12]).unwrap();
        std::fs::write(blob_dir.join(format!("{hash_hex}.torrent")), &raw).unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        rt_db::migrate(&conn).unwrap();
        let row = TorrentRow {
            info_hash: hash_hex.clone(),
            name: "sample.bin".into(),
            total_length: 12,
            piece_length: 16_384,
            piece_count: 1,
            is_private: false,
            save_path: save_path.to_string_lossy().into_owned(),
            category: Some("linux".into()),
            tags: vec!["iso".into()],
            state: "seeding".into(),
            added_at: 1_700_000_000,
            completed_at: Some(1_700_000_500),
            uploaded: 4096,
            downloaded: 12,
            ratio: 1.0,
            trackers: vec!["https://tracker.example/announce".into()],
        };
        rt_db::upsert(&conn, &row).unwrap();
        drop(conn);

        let store = FastresumeStore::new(fr_dir.clone());
        let mut state = FastresumeState::new_empty(&hash, 1, ImportPolicy::TrustHints);
        state.pieces = vec![if complete {
            PieceState::Valid
        } else {
            PieceState::Unknown
        }];
        state.uploaded_bytes = 4096;
        state.downloaded_bytes = 12;
        store.save(&state).unwrap();

        (db_path, blob_dir, fr_dir, hash_hex, save_path)
    }

    #[test]
    fn libtorrent_export_round_trips_through_qbittorrent_importer() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, blob, fr, hash, save) = native_fixture(tmp.path(), true);
        let plan = ExportPlan::new(ExportFormat::Libtorrent, &db, &blob, &fr).unwrap();
        assert_eq!(plan.fidelity_summary().recheck_free, 1);
        let out = tmp.path().join("out");
        plan.write(&out).unwrap();

        let imported = dry_run_qbittorrent_backup(&out).unwrap();
        let it = &imported.torrents[0];
        assert_eq!(it.info_hash, hash);
        assert_eq!(it.save_path.as_deref(), Some(save.as_path()));
        assert_eq!(it.category.as_deref(), Some("linux"));
        assert_eq!(it.uploaded, Some(4096));
        let st = it
            .to_fastresume_state(ImportPolicy::TrustHints)
            .expect("piece state");
        assert_eq!(st.pieces, vec![PieceState::Valid]);
    }

    #[test]
    fn transmission_export_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, blob, fr, _h, save) = native_fixture(tmp.path(), true);
        let out = tmp.path().join("out");
        ExportPlan::new(ExportFormat::Transmission, &db, &blob, &fr)
            .unwrap()
            .write(&out)
            .unwrap();

        let imported = dry_run_transmission_session(&out).unwrap();
        let it = &imported.torrents[0];
        assert_eq!(it.save_path.as_deref(), Some(save.as_path()));
        assert_eq!(it.uploaded, Some(4096));
        assert_eq!(
            it.to_fastresume_state(ImportPolicy::TrustHints)
                .unwrap()
                .pieces,
            vec![PieceState::Valid]
        );
    }

    #[test]
    fn utorrent_and_biglybt_aggregates_round_trip() {
        fn run_utorrent(path: &Path) -> Result<crate::MigrationPlan, crate::MigrationError> {
            dry_run_utorrent_config(path)
        }
        fn run_biglybt(path: &Path) -> Result<crate::MigrationPlan, crate::MigrationError> {
            dry_run_biglybt_config(path)
        }
        type ExportPlanParser = fn(&Path) -> Result<crate::MigrationPlan, crate::MigrationError>;
        let cases: [(ExportFormat, ExportPlanParser); 2] = [
            (ExportFormat::Utorrent, run_utorrent),
            (ExportFormat::Biglybt, run_biglybt),
        ];
        for (fmt, run) in cases {
            let tmp = tempfile::tempdir().unwrap();
            let (db, blob, fr, _h, save) = native_fixture(tmp.path(), true);
            let out = tmp.path().join("out");
            ExportPlan::new(fmt, &db, &blob, &fr)
                .unwrap()
                .write(&out)
                .unwrap();
            let imported = run(&out).unwrap();
            let it = &imported.torrents[0];
            assert_eq!(it.save_path.as_deref(), Some(save.as_path()));
            assert_eq!(
                it.to_fastresume_state(ImportPolicy::TrustHints)
                    .unwrap()
                    .pieces,
                vec![PieceState::Valid],
                "{fmt:?}"
            );
        }
    }

    #[test]
    fn rtorrent_export_complete_is_recheck_free() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, blob, fr, _h, _save) = native_fixture(tmp.path(), true);
        let plan = ExportPlan::new(ExportFormat::Rtorrent, &db, &blob, &fr).unwrap();
        assert_eq!(plan.fidelity_summary().complete_only, 1);
        let out = tmp.path().join("out");
        plan.write(&out).unwrap();
        let imported = dry_run_rtorrent_session(&out).unwrap();
        let it = &imported.torrents[0];
        assert_eq!(it.category.as_deref(), Some("linux"));
        assert_eq!(it.completed, Some(true));
        // rTorrent synthesises seed state from complete + present files.
        assert_eq!(it.resume_confidence, ResumeConfidence::Trusted);
    }

    #[test]
    fn rtorrent_export_partial_is_metadata_only() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, blob, fr, _h, _s) = native_fixture(tmp.path(), false);
        let plan = ExportPlan::new(ExportFormat::Rtorrent, &db, &blob, &fr).unwrap();
        assert_eq!(plan.fidelity_summary().metadata_only, 1);
    }

    #[test]
    fn generic_export_copies_torrent_and_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, blob, fr, hash, _s) = native_fixture(tmp.path(), true);
        let out = tmp.path().join("out");
        let summary = ExportPlan::new(ExportFormat::Generic, &db, &blob, &fr)
            .unwrap()
            .write(&out)
            .unwrap();
        assert_eq!(summary.torrents, 1);
        assert!(out.join(format!("{hash}.torrent")).exists());
        let manifest = std::fs::read_to_string(out.join("manifest.json")).unwrap();
        assert!(manifest.contains(&hash));
        assert!(manifest.contains("\"complete\": true"));
        // Generic stays scannable by the generic importer.
        assert_eq!(
            dry_run_generic_torrent_directory(&out)
                .unwrap()
                .torrent_count(),
            1
        );
    }

    #[test]
    fn missing_blob_is_skipped_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, blob, fr, hash, _s) = native_fixture(tmp.path(), true);
        std::fs::remove_file(blob.join(format!("{hash}.torrent"))).unwrap();
        let plan = ExportPlan::new(ExportFormat::Libtorrent, &db, &blob, &fr).unwrap();
        assert_eq!(plan.torrent_count(), 0);
        assert_eq!(plan.skipped.len(), 1);
        assert!(plan.skipped[0].reason.contains("missing .torrent blob"));
    }

    #[test]
    fn format_parsing_accepts_aliases() {
        assert_eq!(ExportFormat::parse("qb"), Some(ExportFormat::Libtorrent));
        assert_eq!(
            ExportFormat::parse("deluge"),
            Some(ExportFormat::Libtorrent)
        );
        assert_eq!(ExportFormat::parse("vuze"), Some(ExportFormat::Biglybt));
        assert_eq!(ExportFormat::parse("generic"), Some(ExportFormat::Generic));
        assert_eq!(ExportFormat::parse("nope"), None);
    }
}
