use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rt_bencode::{decode, BValue, Decoder};
use rt_db::{TorrentFileRow, TorrentRow, TorrentTrackerRow};
use rt_fastresume::{
    FastresumeState, FastresumeStore, FileHint, ImportPolicy, PartialPieceState, PieceState,
};
use rt_metainfo::{parse_torrent, TorrentMeta};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_TORRENT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RESUME_BYTES: u64 = 64 * 1024 * 1024;
const MAX_BLOCKS_PER_PARTIAL_PIECE: usize = 16_384;

#[derive(Debug, Error)]
pub enum MigrationError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Db(#[from] rt_db::DbError),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MigrationSource {
    RTorrent,
    QBittorrent,
    Transmission,
    Deluge,
    UTorrent,
    BiglyBT,
    Tixati,
    Generic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationPlan {
    pub source: MigrationSource,
    pub root: PathBuf,
    pub torrents: Vec<MigrationTorrent>,
    pub skipped: Vec<SkippedEntry>,
}

impl MigrationPlan {
    pub fn torrent_count(&self) -> usize {
        self.torrents.len()
    }

    pub fn warning_count(&self) -> usize {
        self.torrents
            .iter()
            .map(|torrent| torrent.warnings.len())
            .sum::<usize>()
            + self.skipped.len()
    }

    pub fn resume_confidence_summary(&self) -> ResumeConfidenceSummary {
        let mut summary = ResumeConfidenceSummary::default();
        for torrent in &self.torrents {
            match torrent.resume_confidence {
                ResumeConfidence::None => summary.none += 1,
                ResumeConfidence::MetadataOnly => summary.metadata_only += 1,
                ResumeConfidence::Hints => summary.hints += 1,
                ResumeConfidence::Trusted => summary.trusted += 1,
            }
        }
        summary
    }

    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# rtorrentNG Migration Dry Run\n\n");
        out.push_str(&format!("- Source: {:?}\n", self.source));
        out.push_str(&format!("- Root: `{}`\n", self.root.display()));
        out.push_str(&format!("- Importable torrents: {}\n", self.torrents.len()));
        out.push_str(&format!("- Warnings/skipped: {}\n", self.warning_count()));
        let confidence = self.resume_confidence_summary();
        out.push_str(&format!(
            "- Fast-resume: {} trusted, {} hints, {} metadata-only, {} none\n\n",
            confidence.trusted, confidence.hints, confidence.metadata_only, confidence.none
        ));
        out.push_str("| Info hash | Name | Save path | Resume | Tags | Warnings |\n");
        out.push_str("| --- | --- | --- | --- | --- | --- |\n");
        for torrent in &self.torrents {
            out.push_str(&format!(
                "| `{}` | {} | {} | {:?} | {} | {} |\n",
                torrent.info_hash,
                escape_table(&torrent.name),
                torrent
                    .save_path
                    .as_ref()
                    .map(|path| format!("`{}`", path.display()))
                    .unwrap_or_else(|| "-".to_owned()),
                torrent.resume_confidence,
                if torrent.tags.is_empty() {
                    "-".to_owned()
                } else {
                    escape_table(&torrent.tags.join(", "))
                },
                if torrent.warnings.is_empty() {
                    "-".to_owned()
                } else {
                    escape_table(&torrent.warnings.join("; "))
                }
            ));
        }
        if !self.skipped.is_empty() {
            out.push_str("\n## Skipped\n\n");
            for skipped in &self.skipped {
                out.push_str(&format!(
                    "- `{}`: {}\n",
                    skipped.path.display(),
                    skipped.reason
                ));
            }
        }
        out
    }

    pub fn to_db_import(&self, options: &ImportOptions) -> DbImportPlan {
        DbImportPlan {
            torrents: self
                .torrents
                .iter()
                .map(|torrent| torrent.to_db_rows(options))
                .collect(),
        }
    }

    pub fn apply_to_db(
        &self,
        conn: &mut rusqlite::Connection,
        options: &ImportOptions,
    ) -> Result<DbImportSummary, MigrationError> {
        let import = self.to_db_import(options);
        import.apply(conn)
    }

    pub fn apply_native_import(
        &self,
        conn: &mut rusqlite::Connection,
        fastresume_dir: impl AsRef<Path>,
        options: &ImportOptions,
        policy: ImportPolicy,
    ) -> Result<NativeImportSummary, MigrationError> {
        let db = self.apply_to_db(conn, options)?;
        let fastresume = self.apply_fastresume(fastresume_dir, policy)?;
        Ok(NativeImportSummary { db, fastresume })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationTorrent {
    pub info_hash: String,
    pub name: String,
    pub total_length: u64,
    pub piece_length: u64,
    pub piece_count: u64,
    pub is_private: bool,
    pub files: Vec<MigrationFile>,
    pub torrent_path: PathBuf,
    pub resume_path: Option<PathBuf>,
    pub save_path: Option<PathBuf>,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub uploaded: Option<u64>,
    pub downloaded: Option<u64>,
    pub completed: Option<bool>,
    pub added_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub paused: Option<bool>,
    pub tracker_activity: TrackerActivity,
    pub resume_confidence: ResumeConfidence,
    pub fastresume: Option<ImportedFastresume>,
    pub trackers: Vec<String>,
    pub warnings: Vec<String>,
}

impl MigrationTorrent {
    pub fn to_db_rows(&self, options: &ImportOptions) -> DbTorrentImport {
        let save_path = self
            .save_path
            .as_ref()
            .map(|path| options.remap_path(path))
            .or_else(|| options.default_save_path.clone())
            .unwrap_or_else(|| PathBuf::from("."))
            .to_string_lossy()
            .to_string();
        self.to_db_rows_with_save_path(save_path, options)
    }

    fn to_db_rows_with_save_path(
        &self,
        save_path: String,
        options: &ImportOptions,
    ) -> DbTorrentImport {
        let uploaded = self.uploaded.unwrap_or_default();
        let downloaded = self.downloaded.unwrap_or_default();
        let ratio = if downloaded == 0 {
            0.0
        } else {
            uploaded as f64 / downloaded as f64
        };
        let completed = self.completed.unwrap_or(false);
        let added_at = self.added_at.unwrap_or(options.added_at);
        let completed_at = if completed {
            Some(self.completed_at.unwrap_or(added_at))
        } else {
            None
        };
        DbTorrentImport {
            torrent: TorrentRow {
                info_hash: self.info_hash.clone(),
                name: self.name.clone(),
                total_length: i64_saturating(self.total_length),
                piece_length: i64_saturating(self.piece_length),
                piece_count: i64_saturating(self.piece_count),
                is_private: self.is_private,
                save_path,
                category: normalize_optional_label(self.category.clone()),
                tags: normalize_tags(self.tags.clone()),
                state: self.imported_state(completed),
                added_at,
                completed_at,
                uploaded: i64_saturating(uploaded),
                downloaded: i64_saturating(downloaded),
                ratio,
                trackers: self.trackers.clone(),
            },
            files: self
                .files
                .iter()
                .map(|file| TorrentFileRow {
                    info_hash: self.info_hash.clone(),
                    file_index: i64::from(file.index),
                    path: file.path.clone(),
                    length: i64_saturating(file.length),
                    offset: i64_saturating(file.offset),
                    priority: i64::from(file.priority),
                    wanted: file.wanted,
                    completed_bytes: if completed {
                        i64_saturating(file.length)
                    } else {
                        i64_saturating(file.completed_bytes.unwrap_or_default())
                    },
                })
                .collect(),
            trackers: self
                .trackers
                .iter()
                .enumerate()
                .map(|(index, url)| TorrentTrackerRow {
                    info_hash: self.info_hash.clone(),
                    tracker_index: i64_saturating(index as u64),
                    tier: i64_saturating(index as u64),
                    url: url.clone(),
                    status: self.tracker_activity.status(),
                    last_announce_at: self.tracker_activity.last_announce_at,
                    next_announce_at: self.tracker_activity.next_announce_at,
                    last_success_at: self.tracker_activity.last_success_at,
                    failure_reason: self.tracker_activity.failure_reason.clone(),
                    warning_message: self.tracker_activity.warning_message.clone(),
                    seeders: self.tracker_activity.seeders.map(i64_saturating),
                    leechers: self.tracker_activity.leechers.map(i64_saturating),
                    completed: self.tracker_activity.completed.map(i64_saturating),
                    uploaded: i64_saturating(uploaded),
                    downloaded: i64_saturating(downloaded),
                    left_bytes: if completed {
                        0
                    } else {
                        i64_saturating(self.total_length)
                    },
                })
                .collect(),
        }
    }

    fn imported_state(&self, completed: bool) -> String {
        if completed {
            "completed".to_owned()
        } else if self.paused.unwrap_or(true) {
            "stopped".to_owned()
        } else {
            "downloading".to_owned()
        }
    }

    pub fn to_fastresume_state(&self, policy: ImportPolicy) -> Option<FastresumeState> {
        let imported = self.fastresume.as_ref()?;
        let mut state = FastresumeState {
            version: rt_fastresume::state::FASTRESUME_VERSION,
            info_hash: self.info_hash.clone(),
            session_generation: 1,
            pieces: imported.pieces.clone(),
            partial_pieces: imported.partial_pieces.clone(),
            file_hints: imported.file_hints.clone(),
            last_full_verify: 0,
            clean_shutdown: imported.clean_shutdown,
            uploaded_bytes: self.uploaded.unwrap_or_default(),
            downloaded_bytes: self.downloaded.unwrap_or_default(),
            import_policy: policy,
        };
        if state.pieces.len() != self.piece_count as usize {
            return None;
        }
        match policy {
            ImportPolicy::RequireVerification => {
                for piece in &mut state.pieces {
                    if *piece == PieceState::Valid {
                        *piece = PieceState::Unknown;
                    }
                }
            }
            ImportPolicy::TrustHints | ImportPolicy::TrustAll => {}
        }
        Some(state)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationFile {
    pub index: u32,
    pub path: String,
    pub length: u64,
    pub offset: u64,
    pub priority: i32,
    pub wanted: bool,
    pub completed_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedEntry {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResumeConfidence {
    None,
    MetadataOnly,
    Hints,
    Trusted,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeConfidenceSummary {
    pub none: usize,
    pub metadata_only: usize,
    pub hints: usize,
    pub trusted: usize,
}

impl Default for ResumeConfidence {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedFastresume {
    pub pieces: Vec<PieceState>,
    pub partial_pieces: Vec<PartialPieceState>,
    pub file_hints: Vec<FileHint>,
    pub clean_shutdown: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrackerActivity {
    pub last_announce_at: Option<i64>,
    pub next_announce_at: Option<i64>,
    pub last_success_at: Option<i64>,
    pub failure_reason: Option<String>,
    pub warning_message: Option<String>,
    pub seeders: Option<u64>,
    pub leechers: Option<u64>,
    pub completed: Option<u64>,
}

impl TrackerActivity {
    fn status(&self) -> String {
        if self.failure_reason.is_some() {
            "error".to_owned()
        } else if self.warning_message.is_some() {
            "warning".to_owned()
        } else if self.last_success_at.is_some() {
            "working".to_owned()
        } else if self.last_announce_at.is_some() {
            "announced".to_owned()
        } else {
            "never_announced".to_owned()
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub default_save_path: Option<PathBuf>,
    pub path_remaps: Vec<PathRemap>,
    pub added_at: i64,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            default_save_path: None,
            path_remaps: Vec::new(),
            added_at: 0,
        }
    }
}

impl ImportOptions {
    pub fn remap_path(&self, path: &Path) -> PathBuf {
        self.path_remaps
            .iter()
            .filter_map(|remap| remap.apply(path))
            .max_by_key(|mapped| mapped.matched_components)
            .map(|mapped| mapped.path)
            .unwrap_or_else(|| path.to_path_buf())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRemap {
    pub from: PathBuf,
    pub to: PathBuf,
}

impl PathRemap {
    pub fn new(from: impl Into<PathBuf>, to: impl Into<PathBuf>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
        }
    }

    fn apply(&self, path: &Path) -> Option<MappedPath> {
        let suffix = path.strip_prefix(&self.from).ok()?;
        let mapped = if suffix.as_os_str().is_empty() {
            self.to.clone()
        } else {
            self.to.join(suffix)
        };
        Some(MappedPath {
            matched_components: self.from.components().count(),
            path: mapped,
        })
    }
}

#[derive(Debug, Clone)]
struct MappedPath {
    matched_components: usize,
    path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct DbImportPlan {
    pub torrents: Vec<DbTorrentImport>,
}

#[derive(Debug, Clone)]
pub struct FastresumeImportPlan {
    pub states: Vec<(String, FastresumeState)>,
    pub skipped: Vec<SkippedEntry>,
}

impl MigrationPlan {
    pub fn to_fastresume_import(&self, policy: ImportPolicy) -> FastresumeImportPlan {
        let mut states = Vec::new();
        let mut skipped = Vec::new();
        for torrent in &self.torrents {
            match torrent.to_fastresume_state(policy) {
                Some(state) => states.push((torrent.info_hash.clone(), state)),
                None => skipped.push(SkippedEntry {
                    path: torrent
                        .resume_path
                        .clone()
                        .unwrap_or_else(|| torrent.torrent_path.clone()),
                    reason: "no compatible fast-resume piece state".to_owned(),
                }),
            }
        }
        FastresumeImportPlan { states, skipped }
    }

    pub fn apply_fastresume(
        &self,
        dir: impl AsRef<Path>,
        policy: ImportPolicy,
    ) -> Result<FastresumeImportSummary, MigrationError> {
        let store = FastresumeStore::new(dir.as_ref().to_path_buf());
        let import = self.to_fastresume_import(policy);
        for (_, state) in &import.states {
            store.save(state).map_err(|e| {
                MigrationError::Io(std::io::Error::other(format!(
                    "fastresume save failed: {e}"
                )))
            })?;
        }
        Ok(FastresumeImportSummary {
            states: import.states.len(),
            skipped: import.skipped.len(),
            confidence: self.resume_confidence_summary(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FastresumeImportSummary {
    pub states: usize,
    pub skipped: usize,
    pub confidence: ResumeConfidenceSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeImportSummary {
    pub db: DbImportSummary,
    pub fastresume: FastresumeImportSummary,
}

impl DbImportPlan {
    pub fn apply(
        &self,
        conn: &mut rusqlite::Connection,
    ) -> Result<DbImportSummary, MigrationError> {
        let tx = conn.transaction()?;
        for import in &self.torrents {
            rt_db::upsert_in_tx(&tx, &import.torrent)?;
            rt_db::replace_torrent_files_in_tx(&tx, &import.torrent.info_hash, &import.files)?;
            rt_db::replace_torrent_trackers_in_tx(
                &tx,
                &import.torrent.info_hash,
                &import.trackers,
            )?;
        }
        tx.commit()?;
        Ok(DbImportSummary {
            torrents: self.torrents.len(),
            files: self.torrents.iter().map(|import| import.files.len()).sum(),
            trackers: self
                .torrents
                .iter()
                .map(|import| import.trackers.len())
                .sum(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct DbTorrentImport {
    pub torrent: TorrentRow,
    pub files: Vec<TorrentFileRow>,
    pub trackers: Vec<TorrentTrackerRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbImportSummary {
    pub torrents: usize,
    pub files: usize,
    pub trackers: usize,
}

#[derive(Debug, Clone, Default)]
struct ResumeData {
    save_path: Option<PathBuf>,
    category: Option<String>,
    tags: Vec<String>,
    uploaded: Option<u64>,
    downloaded: Option<u64>,
    completed: Option<bool>,
    added_at: Option<i64>,
    completed_at: Option<i64>,
    paused: Option<bool>,
    file_priorities: Vec<i32>,
    file_wanted: Vec<bool>,
    file_completed_bytes: Vec<u64>,
    tracker_activity: TrackerActivity,
    confidence: ResumeConfidence,
    pieces: Option<Vec<PieceState>>,
    partial_pieces: Vec<PartialPieceState>,
    clean_shutdown: bool,
}

impl ResumeData {
    fn has_importable_data(&self) -> bool {
        self.save_path.is_some()
            || self.category.is_some()
            || !self.tags.is_empty()
            || self.uploaded.is_some()
            || self.downloaded.is_some()
            || self.completed.is_some()
            || self.pieces.is_some()
            || !self.partial_pieces.is_empty()
    }
}

pub fn dry_run_rtorrent_session(root: impl AsRef<Path>) -> Result<MigrationPlan, MigrationError> {
    dry_run_rtorrent_session_with_options(root, &ImportOptions::default())
}

pub fn dry_run_rtorrent_session_with_options(
    root: impl AsRef<Path>,
    options: &ImportOptions,
) -> Result<MigrationPlan, MigrationError> {
    dry_run_session(
        root.as_ref(),
        MigrationSource::RTorrent,
        &["rtorrent"],
        options,
    )
}

pub fn dry_run_qbittorrent_backup(root: impl AsRef<Path>) -> Result<MigrationPlan, MigrationError> {
    dry_run_qbittorrent_backup_with_options(root, &ImportOptions::default())
}

pub fn dry_run_qbittorrent_backup_with_options(
    root: impl AsRef<Path>,
    options: &ImportOptions,
) -> Result<MigrationPlan, MigrationError> {
    dry_run_session(
        root.as_ref(),
        MigrationSource::QBittorrent,
        &["fastresume"],
        options,
    )
}

pub fn dry_run_transmission_session(
    root: impl AsRef<Path>,
) -> Result<MigrationPlan, MigrationError> {
    dry_run_transmission_session_with_options(root, &ImportOptions::default())
}

pub fn dry_run_transmission_session_with_options(
    root: impl AsRef<Path>,
    options: &ImportOptions,
) -> Result<MigrationPlan, MigrationError> {
    dry_run_session(
        root.as_ref(),
        MigrationSource::Transmission,
        &["resume"],
        options,
    )
}

pub fn dry_run_deluge_state(root: impl AsRef<Path>) -> Result<MigrationPlan, MigrationError> {
    dry_run_deluge_state_with_options(root, &ImportOptions::default())
}

pub fn dry_run_deluge_state_with_options(
    root: impl AsRef<Path>,
    options: &ImportOptions,
) -> Result<MigrationPlan, MigrationError> {
    dry_run_session(
        root.as_ref(),
        MigrationSource::Deluge,
        &["fastresume", "resume"],
        options,
    )
}

pub fn dry_run_utorrent_config(root: impl AsRef<Path>) -> Result<MigrationPlan, MigrationError> {
    dry_run_utorrent_config_with_options(root, &ImportOptions::default())
}

pub fn dry_run_utorrent_config_with_options(
    root: impl AsRef<Path>,
    options: &ImportOptions,
) -> Result<MigrationPlan, MigrationError> {
    dry_run_session(
        root.as_ref(),
        MigrationSource::UTorrent,
        &["dat", "resume"],
        options,
    )
}

pub fn dry_run_biglybt_config(root: impl AsRef<Path>) -> Result<MigrationPlan, MigrationError> {
    dry_run_biglybt_config_with_options(root, &ImportOptions::default())
}

pub fn dry_run_biglybt_config_with_options(
    root: impl AsRef<Path>,
    options: &ImportOptions,
) -> Result<MigrationPlan, MigrationError> {
    dry_run_session(
        root.as_ref(),
        MigrationSource::BiglyBT,
        &["dat", "config"],
        options,
    )
}

pub fn dry_run_tixati_config(root: impl AsRef<Path>) -> Result<MigrationPlan, MigrationError> {
    dry_run_tixati_config_with_options(root, &ImportOptions::default())
}

pub fn dry_run_tixati_config_with_options(
    root: impl AsRef<Path>,
    options: &ImportOptions,
) -> Result<MigrationPlan, MigrationError> {
    dry_run_session(root.as_ref(), MigrationSource::Tixati, &["dat"], options)
}

pub fn dry_run_generic_torrent_directory(
    root: impl AsRef<Path>,
) -> Result<MigrationPlan, MigrationError> {
    dry_run_generic_torrent_directory_with_options(root, &ImportOptions::default())
}

pub fn dry_run_generic_torrent_directory_with_options(
    root: impl AsRef<Path>,
    options: &ImportOptions,
) -> Result<MigrationPlan, MigrationError> {
    dry_run_session(root.as_ref(), MigrationSource::Generic, &[], options)
}

fn dry_run_session(
    root: &Path,
    source: MigrationSource,
    resume_extensions: &[&str],
    options: &ImportOptions,
) -> Result<MigrationPlan, MigrationError> {
    let mut torrents = Vec::new();
    let mut skipped = Vec::new();
    let mut resume_by_stem = BTreeMap::new();
    let mut aggregate_resume_paths = Vec::new();
    let files = collect_files(root)?;

    for path in &files {
        if extension_is(&path, resume_extensions) {
            if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
                resume_by_stem.insert(stem.to_owned(), path.clone());
            }
            if looks_like_aggregate_resume(path) {
                aggregate_resume_paths.push(path.clone());
            }
        }
    }

    for path in files {
        if !extension_is(&path, &["torrent"]) {
            continue;
        }
        match migration_torrent_from_path(&path, &resume_by_stem, &aggregate_resume_paths, options)
        {
            Ok(torrent) => torrents.push(torrent),
            Err(reason) => skipped.push(SkippedEntry { path, reason }),
        }
    }

    torrents.sort_by(|a, b| a.info_hash.cmp(&b.info_hash));
    skipped.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(MigrationPlan {
        source,
        root: root.to_path_buf(),
        torrents,
        skipped,
    })
}

fn migration_torrent_from_path(
    path: &Path,
    resume_by_stem: &BTreeMap<String, PathBuf>,
    aggregate_resume_paths: &[PathBuf],
    options: &ImportOptions,
) -> Result<MigrationTorrent, String> {
    let raw = read_limited(path, MAX_TORRENT_BYTES).map_err(|e| e.to_string())?;
    let meta = parse_torrent(&raw).map_err(|e| e.to_string())?;
    let info_hash = migration_info_hash(&meta);
    let file_stem = path.file_stem().and_then(|stem| stem.to_str());
    let resume_path = resume_by_stem
        .get(&info_hash)
        .or_else(|| file_stem.and_then(|stem| resume_by_stem.get(stem)))
        .cloned();
    let mut warnings = Vec::new();
    let mut resume = resume_path
        .as_ref()
        .and_then(
            |path| match parse_resume_file(path, &info_hash, file_stem) {
                Ok(resume) => Some(resume),
                Err(error) => {
                    warnings.push(format!("resume parse failed: {error}"));
                    None
                }
            },
        )
        .unwrap_or_default();
    let mut resolved_resume_path = resume_path.clone();
    if resume_path.is_none() {
        let mut aggregate_parse_errors = BTreeSet::new();
        for path in aggregate_resume_paths {
            match parse_resume_file(path, &info_hash, file_stem) {
                Ok(candidate) if candidate.has_importable_data() => {
                    resume = candidate;
                    resolved_resume_path = Some(path.clone());
                    break;
                }
                Ok(_) => {}
                Err(error) => {
                    aggregate_parse_errors.insert(format!(
                        "aggregate resume parse failed for `{}`: {error}",
                        path.display()
                    ));
                }
            }
        }
        warnings.extend(aggregate_parse_errors);
    }
    if resolved_resume_path.is_none() {
        warnings.push("missing resume sidecar; import will require verification".to_owned());
    }

    let trackers = migration_trackers(&meta);
    let mut files = migration_files(&meta);
    apply_file_resume_state(
        &mut files,
        &resume.file_priorities,
        &resume.file_wanted,
        &resume.file_completed_bytes,
    );
    let file_hints = resume
        .save_path
        .as_ref()
        .map(|save_path| collect_file_hints(&options.remap_path(save_path), &files, meta.name()))
        .unwrap_or_default();
    if resume.pieces.is_none() && resume.completed == Some(true) && !file_hints.is_empty() {
        resume.pieces = Some(vec![
            PieceState::Valid;
            migration_piece_count(&meta) as usize
        ]);
    }
    let confidence = if resume.pieces.is_some() {
        if file_hints.is_empty() {
            ResumeConfidence::Hints
        } else {
            ResumeConfidence::Trusted
        }
    } else {
        resume.confidence
    };
    let fastresume = resume.pieces.take().map(|pieces| {
        let (pieces, adjustment) =
            normalize_piece_states(pieces, migration_piece_count(&meta) as usize);
        if let Some(adjustment) = adjustment {
            warnings.push(adjustment);
        }
        ImportedFastresume {
            pieces,
            partial_pieces: resume.partial_pieces,
            file_hints,
            clean_shutdown: resume.clean_shutdown,
        }
    });
    if fastresume.is_some() && confidence != ResumeConfidence::Trusted {
        warnings.push(
            "resume has piece hints but file mtimes/sizes could not be fully validated".to_owned(),
        );
    }

    Ok(MigrationTorrent {
        info_hash,
        name: meta.name().to_owned(),
        total_length: migration_total_length(&meta),
        piece_length: migration_piece_length(&meta),
        piece_count: migration_piece_count(&meta),
        is_private: meta.is_private(),
        files,
        torrent_path: path.to_path_buf(),
        resume_path: resolved_resume_path,
        save_path: resume.save_path,
        category: resume.category,
        tags: resume.tags,
        uploaded: resume.uploaded,
        downloaded: resume.downloaded,
        completed: resume.completed,
        added_at: resume.added_at,
        completed_at: resume.completed_at,
        paused: resume.paused,
        tracker_activity: resume.tracker_activity,
        resume_confidence: confidence,
        fastresume,
        trackers,
        warnings,
    })
}

fn migration_info_hash(meta: &TorrentMeta) -> String {
    match meta {
        TorrentMeta::V1(meta) | TorrentMeta::Hybrid(meta, _) => hex_lower(&meta.info_hash),
        TorrentMeta::V2(meta) => hex_lower(&meta.info_hash_v2),
    }
}

fn migration_total_length(meta: &TorrentMeta) -> u64 {
    match meta {
        TorrentMeta::V1(meta) | TorrentMeta::Hybrid(meta, _) => meta.total_length(),
        TorrentMeta::V2(meta) => meta.total_length(),
    }
}

fn migration_piece_length(meta: &TorrentMeta) -> u64 {
    match meta {
        TorrentMeta::V1(meta) | TorrentMeta::Hybrid(meta, _) => meta.piece_length,
        TorrentMeta::V2(meta) => meta.piece_length,
    }
}

fn migration_piece_count(meta: &TorrentMeta) -> u64 {
    match meta {
        TorrentMeta::V1(meta) | TorrentMeta::Hybrid(meta, _) => meta.pieces.len() as u64,
        TorrentMeta::V2(meta) => meta.total_length().div_ceil(meta.piece_length),
    }
}

fn migration_trackers(meta: &TorrentMeta) -> Vec<String> {
    match meta {
        TorrentMeta::V1(meta) | TorrentMeta::Hybrid(meta, _) => meta.all_trackers(),
        TorrentMeta::V2(meta) => {
            let mut out = Vec::new();
            if let Some(announce) = &meta.announce {
                out.push(announce.clone());
            }
            for tier in &meta.announce_list {
                for url in tier {
                    if !out.contains(url) {
                        out.push(url.clone());
                    }
                }
            }
            out
        }
    }
}

fn migration_files(meta: &TorrentMeta) -> Vec<MigrationFile> {
    match meta {
        TorrentMeta::V1(meta) | TorrentMeta::Hybrid(meta, _) => meta
            .files
            .iter()
            .map(|file| MigrationFile {
                index: file.index,
                path: file.path.as_display(),
                length: file.length,
                offset: file.offset,
                priority: 1,
                wanted: true,
                completed_bytes: None,
            })
            .collect(),
        TorrentMeta::V2(meta) => meta
            .files
            .iter()
            .map(|file| MigrationFile {
                index: file.index,
                path: file.path.as_display(),
                length: file.length,
                offset: file.offset,
                priority: 1,
                wanted: true,
                completed_bytes: None,
            })
            .collect(),
    }
}

fn apply_file_resume_state(
    files: &mut [MigrationFile],
    priorities: &[i32],
    wanted: &[bool],
    completed_bytes: &[u64],
) {
    for file in files {
        if let Some(priority) = priorities.get(file.index as usize).copied() {
            file.priority = priority.clamp(0, 2);
            file.wanted = priority > 0;
        }
        if let Some(wanted) = wanted.get(file.index as usize).copied() {
            file.wanted = wanted;
            if !wanted {
                file.priority = 0;
            } else if file.priority == 0 {
                file.priority = 1;
            }
        }
        if let Some(bytes) = completed_bytes.get(file.index as usize).copied() {
            file.completed_bytes = Some(bytes.min(file.length));
        }
    }
}

fn parse_resume_file(
    path: &Path,
    info_hash: &str,
    file_stem: Option<&str>,
) -> Result<ResumeData, String> {
    let bytes = read_limited(path, MAX_RESUME_BYTES).map_err(|e| e.to_string())?;
    if bytes.is_empty() {
        return Ok(ResumeData::default());
    }
    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) {
        return Ok(resume_from_json(select_json_resume_entry(
            &json, info_hash, file_stem,
        )));
    }
    let value = decode(&bytes)
        .or_else(|_| Decoder::new(&bytes).with_strict_dict_keys(false).decode())
        .map_err(|e| e.to_string())?;
    Ok(resume_from_bencode(select_bencode_resume_entry(
        &value, info_hash, file_stem,
    )))
}

fn select_json_resume_entry<'a>(
    value: &'a serde_json::Value,
    info_hash: &str,
    file_stem: Option<&str>,
) -> &'a serde_json::Value {
    let Some(object) = value.as_object() else {
        return value;
    };
    let candidates = json_resume_keys(info_hash, file_stem);
    for key in &candidates {
        if let Some(entry) = object.get(key) {
            return entry;
        }
    }
    for container in ["torrents", "resume", "downloads"] {
        if let Some(entry) = object.get(container) {
            if let Some(nested) = select_json_resume_entry_optional(entry, &candidates) {
                return nested;
            }
        }
    }
    value
}

fn select_json_resume_entry_optional<'a>(
    value: &'a serde_json::Value,
    candidates: &[String],
) -> Option<&'a serde_json::Value> {
    let object = value.as_object()?;
    candidates.iter().find_map(|key| object.get(key))
}

fn json_resume_keys(info_hash: &str, file_stem: Option<&str>) -> Vec<String> {
    let mut keys = vec![info_hash.to_owned(), info_hash.to_ascii_uppercase()];
    if let Some(raw_hash) = decode_hex(info_hash) {
        keys.push(base32_upper(&raw_hash));
    }
    if let Some(stem) = file_stem {
        keys.push(stem.to_owned());
    }
    keys
}

fn select_bencode_resume_entry<'a>(
    value: &'a BValue<'a>,
    info_hash: &str,
    file_stem: Option<&str>,
) -> &'a BValue<'a> {
    let keys = bencode_resume_keys(info_hash, file_stem);
    if let Some(entry) = bencode_entry_for_keys(value, &keys) {
        return entry;
    }
    for container in [
        b"torrents".as_slice(),
        b"resume".as_slice(),
        b"downloads".as_slice(),
        b"state".as_slice(),
    ] {
        if let Some(entry) = value
            .get(container)
            .and_then(|nested| bencode_entry_for_keys(nested, &keys))
        {
            return entry;
        }
    }
    value
}

fn bencode_entry_for_keys<'a>(value: &'a BValue<'a>, keys: &[Vec<u8>]) -> Option<&'a BValue<'a>> {
    let BValue::Dict(dict) = value else {
        return None;
    };
    dict.iter()
        .find(|(key, _)| keys.iter().any(|candidate| candidate.as_slice() == *key))
        .map(|(_, value)| value)
}

fn bencode_resume_keys(info_hash: &str, file_stem: Option<&str>) -> Vec<Vec<u8>> {
    let mut keys = vec![
        info_hash.as_bytes().to_vec(),
        info_hash.to_ascii_uppercase().into_bytes(),
    ];
    if let Some(raw_hash) = decode_hex(info_hash) {
        keys.push(base32_upper(&raw_hash).into_bytes());
        keys.push(raw_hash);
    }
    if let Some(stem) = file_stem {
        keys.push(stem.as_bytes().to_vec());
    }
    keys
}

fn resume_from_json(value: &serde_json::Value) -> ResumeData {
    let mut resume = ResumeData::default();
    resume.save_path = first_json_string(
        value,
        &[
            "save_path",
            "savePath",
            "downloadDir",
            "destination",
            "path",
            "rootdir",
            "download_path",
            "directory",
            "directory_base",
            "save_dir",
        ],
    )
    .map(PathBuf::from);
    resume.category = first_json_string(value, &["category", "label"]).map(str::to_owned);
    resume.tags = first_json_list(value, &["tags", "labels"]);
    resume.uploaded = first_json_u64(
        value,
        &[
            "uploaded",
            "uploaded_bytes",
            "uploadedBytes",
            "uploadedEver",
            "up_total",
        ],
    );
    resume.downloaded = first_json_u64(
        value,
        &[
            "downloaded",
            "downloaded_bytes",
            "downloadedBytes",
            "downloadedBytesData",
            "downloadedEver",
            "down_total",
            "bytes_done",
        ],
    );
    resume.completed = first_json_bool(
        value,
        &[
            "completed",
            "complete",
            "is_complete",
            "isComplete",
            "finished",
        ],
    );
    resume.added_at = first_json_i64(
        value,
        &["added_at", "addedAt", "added_on", "addedOn", "time_added"],
    );
    resume.completed_at = first_json_i64(
        value,
        &[
            "completed_at",
            "completedAt",
            "completion_on",
            "completed_on",
            "time_completed",
        ],
    );
    resume.paused = first_json_bool(value, &["paused", "is_paused", "isPaused"])
        .or_else(|| json_paused_from_state(value));
    resume.file_priorities = first_json_i32_list(
        value,
        &[
            "file_priorities",
            "filePriorities",
            "priorities",
            "priority",
            "qBt-filePriority",
        ],
    );
    resume.file_wanted = first_json_bool_list(value, &["wanted", "file_wanted", "fileWanted"]);
    resume.file_completed_bytes = first_json_u64_list(
        value,
        &[
            "file_completed",
            "fileCompleted",
            "file_completed_bytes",
            "fileCompletedBytes",
            "file_progress",
            "fileProgress",
        ],
    );
    resume.tracker_activity = tracker_activity_from_json(value);
    if let Some(pieces) = json_piece_states(value) {
        resume.pieces = Some(pieces);
        resume.confidence = ResumeConfidence::Hints;
    } else if resume.save_path.is_some()
        || resume.category.is_some()
        || !resume.tags.is_empty()
        || resume.uploaded.is_some()
        || resume.downloaded.is_some()
        || resume.completed.is_some()
    {
        resume.confidence = ResumeConfidence::MetadataOnly;
    }
    resume.clean_shutdown =
        first_json_bool(value, &["clean_shutdown", "cleanShutdown"]).unwrap_or(true);
    resume
}

fn resume_from_bencode(value: &BValue<'_>) -> ResumeData {
    let mut resume = ResumeData::default();
    resume.save_path = first_bencode_string(
        value,
        &[
            b"save_path".as_slice(),
            b"save path".as_slice(),
            b"qBt-savePath".as_slice(),
            b"destination".as_slice(),
            b"path".as_slice(),
            b"rootdir".as_slice(),
            b"download_path".as_slice(),
            b"directory".as_slice(),
            b"directory_base".as_slice(),
            b"save_dir".as_slice(),
        ],
    )
    .map(PathBuf::from);
    resume.category = first_bencode_string(
        value,
        &[
            b"category".as_slice(),
            b"label".as_slice(),
            b"qBt-category".as_slice(),
            b"custom1".as_slice(),
            b"d.custom1".as_slice(),
        ],
    )
    .map(str::to_owned);
    resume.tags = first_bencode_list(
        value,
        &[
            b"tags".as_slice(),
            b"labels".as_slice(),
            b"qBt-tags".as_slice(),
        ],
    );
    resume.uploaded = first_bencode_u64(
        value,
        &[
            b"uploaded".as_slice(),
            b"uploaded_bytes".as_slice(),
            b"total_uploaded".as_slice(),
            b"total_uploaded_bytes".as_slice(),
            b"uploadedEver".as_slice(),
            b"up_total".as_slice(),
        ],
    );
    resume.downloaded = first_bencode_u64(
        value,
        &[
            b"downloaded".as_slice(),
            b"downloaded_bytes".as_slice(),
            b"total_downloaded".as_slice(),
            b"total_download".as_slice(),
            b"downloadedBytesData".as_slice(),
            b"total_downloaded_bytes".as_slice(),
            b"downloadedEver".as_slice(),
            b"down_total".as_slice(),
            b"bytes_done".as_slice(),
        ],
    );
    resume.completed = first_bencode_bool(
        value,
        &[
            b"completed".as_slice(),
            b"complete".as_slice(),
            b"is_complete".as_slice(),
            b"finished".as_slice(),
        ],
    )
    .or_else(|| bencode_completed_from_pieces(value));
    resume.added_at = first_bencode_i64(
        value,
        &[
            b"added_at".as_slice(),
            b"addedAt".as_slice(),
            b"added_on".as_slice(),
            b"time_added".as_slice(),
        ],
    );
    resume.completed_at = first_bencode_i64(
        value,
        &[
            b"completed_at".as_slice(),
            b"completedAt".as_slice(),
            b"completion_on".as_slice(),
            b"completed_on".as_slice(),
            b"time_completed".as_slice(),
        ],
    );
    resume.paused = first_bencode_bool(
        value,
        &[
            b"paused".as_slice(),
            b"is_paused".as_slice(),
            b"isPaused".as_slice(),
        ],
    )
    .or_else(|| bencode_paused_from_state(value));
    resume.file_priorities = first_bencode_i32_list(
        value,
        &[
            b"file_priorities".as_slice(),
            b"filePriorities".as_slice(),
            b"priorities".as_slice(),
            b"priority".as_slice(),
            b"qBt-filePriority".as_slice(),
            b"file_priority".as_slice(),
        ],
    );
    resume.file_wanted = first_bencode_bool_list(
        value,
        &[
            b"wanted".as_slice(),
            b"file_wanted".as_slice(),
            b"fileWanted".as_slice(),
        ],
    );
    resume.file_completed_bytes = first_bencode_u64_list(
        value,
        &[
            b"file_completed".as_slice(),
            b"fileCompleted".as_slice(),
            b"file_completed_bytes".as_slice(),
            b"fileCompletedBytes".as_slice(),
            b"file_progress".as_slice(),
            b"fileProgress".as_slice(),
        ],
    );
    resume.tracker_activity = tracker_activity_from_bencode(value);
    resume.pieces = libtorrent_piece_states(value)
        .or_else(|| transmission_piece_states(value))
        .or_else(|| nested_piece_states(value));
    resume.partial_pieces = libtorrent_partial_pieces(value);
    resume.clean_shutdown =
        first_bencode_bool(value, &[b"clean_shutdown".as_slice(), b"clean".as_slice()])
            .unwrap_or(true);
    resume.confidence = if resume.pieces.is_some() {
        ResumeConfidence::Hints
    } else if resume.save_path.is_some()
        || resume.category.is_some()
        || !resume.tags.is_empty()
        || resume.uploaded.is_some()
        || resume.downloaded.is_some()
        || resume.completed.is_some()
    {
        ResumeConfidence::MetadataOnly
    } else {
        ResumeConfidence::None
    };
    resume
}

fn libtorrent_piece_states(value: &BValue<'_>) -> Option<Vec<PieceState>> {
    let bytes = value.get(b"pieces")?.as_bytes()?;
    if bytes.is_empty() {
        return None;
    }
    Some(
        bytes
            .iter()
            .map(|byte| {
                if byte & 0x01 != 0 || byte & 0x02 != 0 {
                    PieceState::Valid
                } else {
                    PieceState::Unknown
                }
            })
            .collect(),
    )
}

fn libtorrent_partial_pieces(value: &BValue<'_>) -> Vec<PartialPieceState> {
    let Some(BValue::Dict(dict)) = value.get(b"unfinished") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, value) in dict {
        let Ok(piece) = std::str::from_utf8(key)
            .ok()
            .and_then(|text| text.parse::<u32>().ok())
            .ok_or(())
        else {
            continue;
        };
        let mut received_blocks = match value {
            BValue::List(items) => items
                .iter()
                .filter_map(BValue::as_int)
                .filter_map(|value| u32::try_from(value).ok())
                .collect(),
            BValue::Bytes(bytes) => bytes
                .iter()
                .enumerate()
                .filter_map(|(index, byte)| (*byte != 0).then_some(index as u32))
                .collect(),
            _ => Vec::new(),
        };
        if !received_blocks.is_empty() {
            received_blocks.sort_unstable();
            received_blocks.dedup();
            received_blocks.truncate(MAX_BLOCKS_PER_PARTIAL_PIECE);
            out.push(PartialPieceState {
                piece,
                received_blocks,
            });
        }
    }
    out.sort_by_key(|state| state.piece);
    out
}

fn transmission_piece_states(value: &BValue<'_>) -> Option<Vec<PieceState>> {
    let progress = value.get(b"progress").unwrap_or(value);
    for key in [
        b"bitfield".as_slice(),
        b"have".as_slice(),
        b"completed".as_slice(),
        b"complete".as_slice(),
        b"blocks".as_slice(),
        b"valid".as_slice(),
    ] {
        if let Some(bitfield) = progress.get(key).and_then(BValue::as_bytes) {
            return Some(bitfield_to_piece_states(bitfield));
        }
    }
    None
}

fn nested_piece_states(value: &BValue<'_>) -> Option<Vec<PieceState>> {
    for key in [
        b"resume data".as_slice(),
        b"resume_data".as_slice(),
        b"resume".as_slice(),
        b"progress".as_slice(),
    ] {
        if let Some(pieces) = value.get(key).and_then(transmission_piece_states) {
            return Some(pieces);
        }
    }
    None
}

fn bitfield_to_piece_states(bitfield: &[u8]) -> Vec<PieceState> {
    let mut pieces = Vec::with_capacity(bitfield.len() * 8);
    for byte in bitfield {
        for bit in 0..8 {
            let mask = 0x80 >> bit;
            pieces.push(if byte & mask != 0 {
                PieceState::Valid
            } else {
                PieceState::Unknown
            });
        }
    }
    pieces
}

fn bencode_completed_from_pieces(value: &BValue<'_>) -> Option<bool> {
    let pieces = libtorrent_piece_states(value)
        .or_else(|| transmission_piece_states(value))
        .or_else(|| nested_piece_states(value))?;
    Some(!pieces.is_empty() && pieces.iter().all(|piece| *piece == PieceState::Valid))
}

fn tracker_activity_from_bencode(value: &BValue<'_>) -> TrackerActivity {
    TrackerActivity {
        last_announce_at: first_bencode_i64(
            value,
            &[
                b"last_announce".as_slice(),
                b"lastAnnounce".as_slice(),
                b"last_announce_at".as_slice(),
            ],
        ),
        next_announce_at: first_bencode_i64(
            value,
            &[
                b"next_announce".as_slice(),
                b"nextAnnounce".as_slice(),
                b"next_announce_at".as_slice(),
            ],
        ),
        last_success_at: first_bencode_i64(
            value,
            &[
                b"last_success".as_slice(),
                b"lastSuccess".as_slice(),
                b"last_success_at".as_slice(),
            ],
        ),
        failure_reason: first_bencode_string(
            value,
            &[
                b"failure_reason".as_slice(),
                b"failureReason".as_slice(),
                b"error".as_slice(),
            ],
        )
        .map(str::to_owned),
        warning_message: first_bencode_string(
            value,
            &[
                b"warning_message".as_slice(),
                b"warningMessage".as_slice(),
                b"warning".as_slice(),
            ],
        )
        .map(str::to_owned),
        seeders: first_bencode_u64(value, &[b"seeders".as_slice(), b"seeds".as_slice()]),
        leechers: first_bencode_u64(value, &[b"leechers".as_slice(), b"peers".as_slice()]),
        completed: first_bencode_u64(
            value,
            &[
                b"completed_count".as_slice(),
                b"completedCount".as_slice(),
                b"scrape_completed".as_slice(),
            ],
        ),
    }
}

fn bencode_paused_from_state(value: &BValue<'_>) -> Option<bool> {
    first_bencode_string(
        value,
        &[
            b"state".as_slice(),
            b"status".as_slice(),
            b"qBt-state".as_slice(),
        ],
    )
    .and_then(paused_from_state_text)
}

fn json_piece_states(value: &serde_json::Value) -> Option<Vec<PieceState>> {
    let pieces = value
        .get("pieces")
        .or_else(|| value.get("pieceStates"))
        .or_else(|| value.get("bitfield"))
        .or_else(|| value.get("have"))
        .or_else(|| value.get("valid"))?;
    if let Some(text) = pieces.as_str() {
        if text.chars().all(|ch| ch == '0' || ch == '1') {
            return Some(
                text.chars()
                    .map(|ch| {
                        if ch == '1' {
                            PieceState::Valid
                        } else {
                            PieceState::Unknown
                        }
                    })
                    .collect(),
            );
        }
    }
    pieces.as_array().map(|items| {
        items
            .iter()
            .map(|item| match item {
                serde_json::Value::Bool(true) => PieceState::Valid,
                serde_json::Value::Number(number) if number.as_u64().unwrap_or(0) > 0 => {
                    PieceState::Valid
                }
                serde_json::Value::String(text)
                    if text.eq_ignore_ascii_case("valid")
                        || text.eq_ignore_ascii_case("complete")
                        || text == "1" =>
                {
                    PieceState::Valid
                }
                _ => PieceState::Unknown,
            })
            .collect()
    })
}

fn tracker_activity_from_json(value: &serde_json::Value) -> TrackerActivity {
    TrackerActivity {
        last_announce_at: first_json_i64(
            value,
            &["last_announce", "lastAnnounce", "last_announce_at"],
        ),
        next_announce_at: first_json_i64(
            value,
            &["next_announce", "nextAnnounce", "next_announce_at"],
        ),
        last_success_at: first_json_i64(value, &["last_success", "lastSuccess", "last_success_at"]),
        failure_reason: first_json_string(value, &["failure_reason", "failureReason", "error"])
            .map(str::to_owned),
        warning_message: first_json_string(
            value,
            &["warning_message", "warningMessage", "warning"],
        )
        .map(str::to_owned),
        seeders: first_json_u64(value, &["seeders", "seeds"]),
        leechers: first_json_u64(value, &["leechers", "peers"]),
        completed: first_json_u64(
            value,
            &["completed_count", "completedCount", "scrape_completed"],
        ),
    }
}

fn json_paused_from_state(value: &serde_json::Value) -> Option<bool> {
    first_json_string(value, &["state", "status", "qBt-state"]).and_then(paused_from_state_text)
}

fn paused_from_state_text(text: &str) -> Option<bool> {
    if text.eq_ignore_ascii_case("paused")
        || text.eq_ignore_ascii_case("stopped")
        || text.eq_ignore_ascii_case("queued")
    {
        Some(true)
    } else if text.eq_ignore_ascii_case("downloading")
        || text.eq_ignore_ascii_case("uploading")
        || text.eq_ignore_ascii_case("seeding")
        || text.eq_ignore_ascii_case("active")
    {
        Some(false)
    } else {
        None
    }
}

fn normalize_piece_states(
    mut pieces: Vec<PieceState>,
    piece_count: usize,
) -> (Vec<PieceState>, Option<String>) {
    let original_len = pieces.len();
    let warning = match original_len.cmp(&piece_count) {
        std::cmp::Ordering::Greater => Some(format!(
            "resume piece state had {original_len} entries; truncated to torrent piece count {piece_count}"
        )),
        std::cmp::Ordering::Less => Some(format!(
            "resume piece state had {original_len} entries; padded to torrent piece count {piece_count}"
        )),
        std::cmp::Ordering::Equal => None,
    };
    pieces.truncate(piece_count);
    pieces.resize(piece_count, PieceState::Unknown);
    (pieces, warning)
}

fn collect_file_hints(
    save_path: &Path,
    files: &[MigrationFile],
    torrent_name: &str,
) -> Vec<FileHint> {
    let mut hints = Vec::new();
    for file in files {
        let candidates = [
            save_path.join(&file.path),
            save_path.join(torrent_name).join(&file.path),
        ];
        let Some(metadata) = candidates
            .iter()
            .find_map(|path| std::fs::metadata(path).ok())
        else {
            continue;
        };
        if metadata.len() != file.length {
            continue;
        }
        hints.push(FileHint {
            file_index: file.index,
            size: metadata.len(),
            mtime_secs: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs())
                .unwrap_or_default(),
            inode: file_inode(&metadata),
        });
    }
    hints
}

#[cfg(unix)]
fn file_inode(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ino()
}

#[cfg(not(unix))]
fn file_inode(_: &std::fs::Metadata) -> u64 {
    0
}

fn collect_files(root: &Path) -> Result<Vec<PathBuf>, MigrationError> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
    collect_files_inner(root, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect_files_inner(path: &Path, out: &mut Vec<PathBuf>) -> Result<(), MigrationError> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files_inner(&path, out)?;
        } else if file_type.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn read_limited(path: &Path, max_bytes: u64) -> Result<Vec<u8>, std::io::Error> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} is {} bytes, larger than the {} byte import limit",
                path.display(),
                metadata.len(),
                max_bytes
            ),
        ));
    }
    std::fs::read(path)
}

fn extension_is(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|ext| {
            extensions
                .iter()
                .any(|expected| ext.eq_ignore_ascii_case(expected))
        })
}

fn first_json_string<'a>(value: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(serde_json::Value::as_str))
}

fn first_json_list(value: &serde_json::Value, keys: &[&str]) -> Vec<String> {
    keys.iter()
        .find_map(|key| {
            let value = value.get(*key)?;
            if let Some(items) = value.as_array() {
                return Some(
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_owned))
                        .collect(),
                );
            }
            value.as_str().map(split_list)
        })
        .unwrap_or_default()
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

fn first_json_u64(value: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(serde_json::Value::as_u64))
}

fn first_json_i64(value: &serde_json::Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(serde_json::Value::as_i64))
}

fn first_json_u64_list(value: &serde_json::Value, keys: &[&str]) -> Vec<u64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(serde_json::Value::as_array))
        .map(|items| items.iter().filter_map(serde_json::Value::as_u64).collect())
        .unwrap_or_default()
}

fn first_json_i32_list(value: &serde_json::Value, keys: &[&str]) -> Vec<i32> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(serde_json::Value::as_array))
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_i64)
                .filter_map(|value| i32::try_from(value).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn first_json_bool(value: &serde_json::Value, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|key| {
        let value = value.get(*key)?;
        if let Some(bool_value) = value.as_bool() {
            return Some(bool_value);
        }
        if let Some(number) = value.as_u64() {
            return Some(number != 0);
        }
        value.as_str().and_then(|text| match text {
            "1" => Some(true),
            "0" => Some(false),
            _ if text.eq_ignore_ascii_case("true")
                || text.eq_ignore_ascii_case("complete")
                || text.eq_ignore_ascii_case("completed")
                || text.eq_ignore_ascii_case("finished") =>
            {
                Some(true)
            }
            _ if text.eq_ignore_ascii_case("false")
                || text.eq_ignore_ascii_case("incomplete")
                || text.eq_ignore_ascii_case("downloading") =>
            {
                Some(false)
            }
            _ => None,
        })
    })
}

fn first_json_bool_list(value: &serde_json::Value, keys: &[&str]) -> Vec<bool> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(serde_json::Value::as_array))
        .map(|items| items.iter().filter_map(json_bool_like).collect())
        .unwrap_or_default()
}

fn json_bool_like(value: &serde_json::Value) -> Option<bool> {
    value
        .as_bool()
        .or_else(|| value.as_u64().map(|number| number != 0))
        .or_else(|| {
            value.as_str().and_then(|text| match text {
                "1" => Some(true),
                "0" => Some(false),
                _ if text.eq_ignore_ascii_case("true") || text.eq_ignore_ascii_case("wanted") => {
                    Some(true)
                }
                _ if text.eq_ignore_ascii_case("false") || text.eq_ignore_ascii_case("skip") => {
                    Some(false)
                }
                _ => None,
            })
        })
}

fn looks_like_aggregate_resume(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| {
            let name = name.to_ascii_lowercase();
            matches!(
                name.as_str(),
                "resume.dat"
                    | "resume.dat.old"
                    | "downloads.config"
                    | "downloads.config.bak"
                    | "torrents.config"
                    | "torrents.config.bak"
            )
        })
}

fn first_bencode_string<'a>(value: &'a BValue<'a>, keys: &[&[u8]]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(BValue::as_str))
}

fn first_bencode_list(value: &BValue<'_>, keys: &[&[u8]]) -> Vec<String> {
    keys.iter()
        .find_map(|key| match value.get(key) {
            Some(BValue::List(items)) => Some(
                items
                    .iter()
                    .filter_map(BValue::as_str)
                    .map(str::to_owned)
                    .collect(),
            ),
            Some(bytes) => bytes.as_str().map(|tags| split_list(tags)),
            None => None,
        })
        .unwrap_or_default()
}

fn first_bencode_u64(value: &BValue<'_>, keys: &[&[u8]]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(BValue::as_int))
        .and_then(|value| u64::try_from(value).ok())
}

fn first_bencode_i64(value: &BValue<'_>, keys: &[&[u8]]) -> Option<i64> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(BValue::as_int))
}

fn first_bencode_u64_list(value: &BValue<'_>, keys: &[&[u8]]) -> Vec<u64> {
    keys.iter()
        .find_map(|key| value.get(key))
        .map(|value| match value {
            BValue::List(items) => items
                .iter()
                .filter_map(BValue::as_int)
                .filter_map(|value| u64::try_from(value).ok())
                .collect(),
            _ => Vec::new(),
        })
        .unwrap_or_default()
}

fn first_bencode_i32_list(value: &BValue<'_>, keys: &[&[u8]]) -> Vec<i32> {
    keys.iter()
        .find_map(|key| value.get(key))
        .map(|value| match value {
            BValue::List(items) => items
                .iter()
                .filter_map(BValue::as_int)
                .filter_map(|value| i32::try_from(value).ok())
                .collect(),
            BValue::Bytes(bytes) => bytes.iter().map(|byte| i32::from(*byte)).collect(),
            _ => Vec::new(),
        })
        .unwrap_or_default()
}

fn first_bencode_bool(value: &BValue<'_>, keys: &[&[u8]]) -> Option<bool> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(BValue::as_int))
        .map(|value| value != 0)
}

fn first_bencode_bool_list(value: &BValue<'_>, keys: &[&[u8]]) -> Vec<bool> {
    keys.iter()
        .find_map(|key| value.get(key))
        .map(|value| match value {
            BValue::List(items) => items
                .iter()
                .filter_map(BValue::as_int)
                .map(|value| value != 0)
                .collect(),
            BValue::Bytes(bytes) => bytes.iter().map(|byte| *byte != 0).collect(),
            _ => Vec::new(),
        })
        .unwrap_or_default()
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = Vec::with_capacity(value.len() / 2);
    for chunk in value.as_bytes().chunks_exact(2) {
        let text = std::str::from_utf8(chunk).ok()?;
        out.push(u8::from_str_radix(text, 16).ok()?);
    }
    Some(out)
}

fn base32_upper(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut buffer = 0u16;
    let mut bits = 0u8;
    for byte in bytes {
        buffer = (buffer << 8) | u16::from(*byte);
        bits += 8;
        while bits >= 5 {
            let index = ((buffer >> (bits - 5)) & 0x1f) as usize;
            out.push(ALPHABET[index] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        let index = ((buffer << (5 - bits)) & 0x1f) as usize;
        out.push(ALPHABET[index] as char);
    }
    out
}

fn escape_table(value: &str) -> String {
    value.replace('|', "\\|")
}

fn normalize_optional_label(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for tag in tags {
        let tag = tag.trim().to_owned();
        if !tag.is_empty() && !out.contains(&tag) {
            out.push(tag);
        }
    }
    out
}

fn i64_saturating(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rt_bencode::{encode, BValue};

    fn write_fixture_torrent(path: &Path) -> [u8; 20] {
        let pieces = [7u8; 20];
        let mut info = vec![
            (b"length".as_slice(), BValue::Int(12)),
            (b"name".as_slice(), BValue::Bytes(b"sample.bin")),
            (b"piece length".as_slice(), BValue::Int(16_384)),
            (b"pieces".as_slice(), BValue::Bytes(&pieces)),
        ];
        info.sort_by(|a, b| a.0.cmp(b.0));
        let mut torrent = vec![
            (
                b"announce".as_slice(),
                BValue::Bytes(b"https://tracker/announce"),
            ),
            (b"info".as_slice(), BValue::Dict(info)),
        ];
        torrent.sort_by(|a, b| a.0.cmp(b.0));
        std::fs::write(path, encode(&BValue::Dict(torrent))).unwrap();
        match parse_torrent(&std::fs::read(path).unwrap()).unwrap() {
            TorrentMeta::V1(meta) | TorrentMeta::Hybrid(meta, _) => meta.info_hash,
            TorrentMeta::V2(_) => unreachable!(),
        }
    }

    fn write_two_file_fixture_torrent(path: &Path) -> [u8; 20] {
        let pieces = [7u8; 20];
        let mut file0 = vec![
            (b"length".as_slice(), BValue::Int(5)),
            (
                b"path".as_slice(),
                BValue::List(vec![BValue::Bytes(b"a.bin")]),
            ),
        ];
        file0.sort_by(|a, b| a.0.cmp(b.0));
        let mut file1 = vec![
            (b"length".as_slice(), BValue::Int(7)),
            (
                b"path".as_slice(),
                BValue::List(vec![BValue::Bytes(b"b.bin")]),
            ),
        ];
        file1.sort_by(|a, b| a.0.cmp(b.0));
        let mut info = vec![
            (
                b"files".as_slice(),
                BValue::List(vec![BValue::Dict(file0), BValue::Dict(file1)]),
            ),
            (b"name".as_slice(), BValue::Bytes(b"multi")),
            (b"piece length".as_slice(), BValue::Int(16_384)),
            (b"pieces".as_slice(), BValue::Bytes(&pieces)),
        ];
        info.sort_by(|a, b| a.0.cmp(b.0));
        let mut torrent = vec![
            (
                b"announce".as_slice(),
                BValue::Bytes(b"https://tracker/announce"),
            ),
            (b"info".as_slice(), BValue::Dict(info)),
        ];
        torrent.sort_by(|a, b| a.0.cmp(b.0));
        std::fs::write(path, encode(&BValue::Dict(torrent))).unwrap();
        match parse_torrent(&std::fs::read(path).unwrap()).unwrap() {
            TorrentMeta::V1(meta) | TorrentMeta::Hybrid(meta, _) => meta.info_hash,
            TorrentMeta::V2(_) => unreachable!(),
        }
    }

    #[test]
    fn qbit_dry_run_preserves_resume_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("sample.torrent");
        let info_hash = write_fixture_torrent(&torrent_path);
        let info_hash_hex = hex_lower(&info_hash);
        std::fs::rename(
            &torrent_path,
            dir.path().join(format!("{info_hash_hex}.torrent")),
        )
        .unwrap();
        let resume = serde_json::json!({
            "save_path": "/downloads/movies",
            "category": "movies",
            "tags": ["hd", "archive"],
            "uploaded": 99,
            "downloaded": 12,
            "completed": true
        });
        std::fs::write(
            dir.path().join(format!("{info_hash_hex}.fastresume")),
            serde_json::to_vec(&resume).unwrap(),
        )
        .unwrap();

        let plan = dry_run_qbittorrent_backup(dir.path()).unwrap();

        assert_eq!(plan.torrent_count(), 1);
        let torrent = &plan.torrents[0];
        assert_eq!(torrent.info_hash, info_hash_hex);
        assert_eq!(
            torrent.save_path.as_deref(),
            Some(Path::new("/downloads/movies"))
        );
        assert_eq!(torrent.category.as_deref(), Some("movies"));
        assert_eq!(torrent.tags, vec!["hd".to_owned(), "archive".to_owned()]);
        assert_eq!(torrent.uploaded, Some(99));
        assert_eq!(torrent.downloaded, Some(12));
        assert_eq!(torrent.completed, Some(true));
        assert_eq!(torrent.resume_confidence, ResumeConfidence::MetadataOnly);
        assert!(torrent.warnings.is_empty());
    }

    #[test]
    fn qbit_libtorrent_resume_imports_piece_state() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("sample.torrent");
        let info_hash = write_fixture_torrent(&torrent_path);
        let info_hash_hex = hex_lower(&info_hash);
        std::fs::rename(
            &torrent_path,
            dir.path().join(format!("{info_hash_hex}.torrent")),
        )
        .unwrap();
        std::fs::write(dir.path().join("sample.bin"), [1u8; 12]).unwrap();
        let mut resume = vec![
            (
                b"qBt-savePath".as_slice(),
                BValue::Bytes(dir.path().as_os_str().as_encoded_bytes()),
            ),
            (b"qBt-category".as_slice(), BValue::Bytes(b"books")),
            (b"qBt-tags".as_slice(), BValue::Bytes(b"imported,archive")),
            (b"total_uploaded".as_slice(), BValue::Int(10)),
            (b"total_downloaded".as_slice(), BValue::Int(12)),
            (b"pieces".as_slice(), BValue::Bytes(&[1])),
        ];
        resume.sort_by(|a, b| a.0.cmp(b.0));
        std::fs::write(
            dir.path().join(format!("{info_hash_hex}.fastresume")),
            encode(&BValue::Dict(resume)),
        )
        .unwrap();

        let plan = dry_run_qbittorrent_backup(dir.path()).unwrap();
        let torrent = &plan.torrents[0];

        assert_eq!(torrent.resume_confidence, ResumeConfidence::Trusted);
        assert_eq!(torrent.tags, vec!["imported", "archive"]);
        let state = torrent
            .to_fastresume_state(ImportPolicy::TrustHints)
            .expect("fastresume state");
        assert_eq!(state.pieces, vec![PieceState::Valid]);
        assert_eq!(state.file_hints.len(), 1);
        assert_eq!(state.uploaded_bytes, 10);
        assert_eq!(state.downloaded_bytes, 12);
    }

    #[test]
    fn fastresume_apply_persists_imported_state_and_summary() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("sample.torrent");
        let info_hash = write_fixture_torrent(&torrent_path);
        let info_hash_hex = hex_lower(&info_hash);
        std::fs::rename(
            &torrent_path,
            dir.path().join(format!("{info_hash_hex}.torrent")),
        )
        .unwrap();
        std::fs::write(dir.path().join("sample.bin"), [1u8; 12]).unwrap();
        let mut resume = vec![
            (
                b"qBt-savePath".as_slice(),
                BValue::Bytes(dir.path().as_os_str().as_encoded_bytes()),
            ),
            (b"total_uploaded".as_slice(), BValue::Int(10)),
            (b"total_downloaded".as_slice(), BValue::Int(12)),
            (b"pieces".as_slice(), BValue::Bytes(&[1])),
        ];
        resume.sort_by(|a, b| a.0.cmp(b.0));
        std::fs::write(
            dir.path().join(format!("{info_hash_hex}.fastresume")),
            encode(&BValue::Dict(resume)),
        )
        .unwrap();
        let plan = dry_run_qbittorrent_backup(dir.path()).unwrap();
        let target = tempfile::tempdir().unwrap();

        let summary = plan
            .apply_fastresume(target.path(), ImportPolicy::TrustHints)
            .unwrap();

        assert_eq!(summary.states, 1);
        assert_eq!(summary.skipped, 0);
        assert_eq!(
            summary.confidence,
            ResumeConfidenceSummary {
                trusted: 1,
                ..ResumeConfidenceSummary::default()
            }
        );
        let store = FastresumeStore::new(target.path());
        let state = store.load(&info_hash_hex).unwrap();
        assert_eq!(state.pieces, vec![PieceState::Valid]);
        assert_eq!(state.import_policy, ImportPolicy::TrustHints);
        assert_eq!(state.uploaded_bytes, 10);
        assert_eq!(state.downloaded_bytes, 12);
    }

    #[test]
    fn native_import_applies_db_and_fastresume_together() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("sample.torrent");
        let info_hash = write_fixture_torrent(&torrent_path);
        let info_hash_hex = hex_lower(&info_hash);
        std::fs::rename(
            &torrent_path,
            dir.path().join(format!("{info_hash_hex}.torrent")),
        )
        .unwrap();
        std::fs::write(dir.path().join("sample.bin"), [1u8; 12]).unwrap();
        let resume = serde_json::json!({
            "save_path": dir.path(),
            "uploaded": 42,
            "downloaded": 12,
            "pieces": [true],
            "completed": true
        });
        std::fs::write(
            dir.path().join(format!("{info_hash_hex}.fastresume")),
            serde_json::to_vec(&resume).unwrap(),
        )
        .unwrap();
        let plan = dry_run_qbittorrent_backup(dir.path()).unwrap();
        let fastresume_dir = tempfile::tempdir().unwrap();
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();

        let summary = plan
            .apply_native_import(
                &mut conn,
                fastresume_dir.path(),
                &ImportOptions::default(),
                ImportPolicy::TrustHints,
            )
            .unwrap();

        assert_eq!(summary.db.torrents, 1);
        assert_eq!(summary.fastresume.states, 1);
        assert_eq!(rt_db::get(&conn, &info_hash_hex).unwrap().uploaded, 42);
        let state = FastresumeStore::new(fastresume_dir.path())
            .load(&info_hash_hex)
            .unwrap();
        assert_eq!(state.pieces, vec![PieceState::Valid]);
    }

    #[test]
    fn json_file_selection_imports_to_native_file_rows() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("multi.torrent");
        let info_hash = write_two_file_fixture_torrent(&torrent_path);
        let info_hash_hex = hex_lower(&info_hash);
        std::fs::rename(
            &torrent_path,
            dir.path().join(format!("{info_hash_hex}.torrent")),
        )
        .unwrap();
        let resume = serde_json::json!({
            "save_path": "/downloads",
            "filePriorities": [0, 2],
            "wanted": [false, true]
        });
        std::fs::write(
            dir.path().join(format!("{info_hash_hex}.fastresume")),
            serde_json::to_vec(&resume).unwrap(),
        )
        .unwrap();

        let plan = dry_run_qbittorrent_backup(dir.path()).unwrap();
        let import = plan.to_db_import(&ImportOptions::default());
        let files = &import.torrents[0].files;

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].priority, 0);
        assert!(!files[0].wanted);
        assert_eq!(files[1].priority, 2);
        assert!(files[1].wanted);
    }

    #[test]
    fn json_file_progress_imports_to_native_file_rows() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("multi.torrent");
        let info_hash = write_two_file_fixture_torrent(&torrent_path);
        let info_hash_hex = hex_lower(&info_hash);
        std::fs::rename(
            &torrent_path,
            dir.path().join(format!("{info_hash_hex}.torrent")),
        )
        .unwrap();
        let resume = serde_json::json!({
            "save_path": "/downloads",
            "fileCompletedBytes": [3, 999]
        });
        std::fs::write(
            dir.path().join(format!("{info_hash_hex}.fastresume")),
            serde_json::to_vec(&resume).unwrap(),
        )
        .unwrap();

        let plan = dry_run_qbittorrent_backup(dir.path()).unwrap();
        let import = plan.to_db_import(&ImportOptions::default());
        let files = &import.torrents[0].files;

        assert_eq!(files[0].completed_bytes, 3);
        assert_eq!(files[1].completed_bytes, 7);
    }

    #[test]
    fn json_tracker_activity_imports_to_native_tracker_rows() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("sample.torrent");
        let info_hash = write_fixture_torrent(&torrent_path);
        let info_hash_hex = hex_lower(&info_hash);
        std::fs::rename(
            &torrent_path,
            dir.path().join(format!("{info_hash_hex}.torrent")),
        )
        .unwrap();
        let resume = serde_json::json!({
            "save_path": "/downloads",
            "lastAnnounce": 100,
            "nextAnnounce": 200,
            "lastSuccess": 90,
            "seeders": 11,
            "leechers": 3,
            "completedCount": 7
        });
        std::fs::write(
            dir.path().join(format!("{info_hash_hex}.fastresume")),
            serde_json::to_vec(&resume).unwrap(),
        )
        .unwrap();

        let plan = dry_run_qbittorrent_backup(dir.path()).unwrap();
        let import = plan.to_db_import(&ImportOptions::default());
        let tracker = &import.torrents[0].trackers[0];

        assert_eq!(tracker.status, "working");
        assert_eq!(tracker.last_announce_at, Some(100));
        assert_eq!(tracker.next_announce_at, Some(200));
        assert_eq!(tracker.last_success_at, Some(90));
        assert_eq!(tracker.seeders, Some(11));
        assert_eq!(tracker.leechers, Some(3));
        assert_eq!(tracker.completed, Some(7));
    }

    #[test]
    fn json_lifecycle_state_imports_to_native_torrent_row() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("sample.torrent");
        let info_hash = write_fixture_torrent(&torrent_path);
        let info_hash_hex = hex_lower(&info_hash);
        std::fs::rename(
            &torrent_path,
            dir.path().join(format!("{info_hash_hex}.torrent")),
        )
        .unwrap();
        let resume = serde_json::json!({
            "save_path": "/downloads",
            "addedAt": 111,
            "completedAt": 222,
            "completed": true,
            "state": "seeding"
        });
        std::fs::write(
            dir.path().join(format!("{info_hash_hex}.fastresume")),
            serde_json::to_vec(&resume).unwrap(),
        )
        .unwrap();

        let plan = dry_run_qbittorrent_backup(dir.path()).unwrap();
        let import = plan.to_db_import(&ImportOptions {
            added_at: 999,
            ..ImportOptions::default()
        });
        let row = &import.torrents[0].torrent;

        assert_eq!(row.state, "completed");
        assert_eq!(row.added_at, 111);
        assert_eq!(row.completed_at, Some(222));
    }

    #[test]
    fn bencoded_file_selection_imports_to_native_file_rows() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("multi.torrent");
        let info_hash = write_two_file_fixture_torrent(&torrent_path);
        let info_hash_hex = hex_lower(&info_hash);
        std::fs::rename(
            &torrent_path,
            dir.path().join(format!("{info_hash_hex}.torrent")),
        )
        .unwrap();
        let mut resume = vec![
            (b"qBt-filePriority".as_slice(), BValue::Bytes(&[2, 0])),
            (b"qBt-savePath".as_slice(), BValue::Bytes(b"/downloads")),
        ];
        resume.sort_by(|a, b| a.0.cmp(b.0));
        std::fs::write(
            dir.path().join(format!("{info_hash_hex}.fastresume")),
            encode(&BValue::Dict(resume)),
        )
        .unwrap();

        let plan = dry_run_qbittorrent_backup(dir.path()).unwrap();
        let import = plan.to_db_import(&ImportOptions::default());
        let files = &import.torrents[0].files;

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].priority, 2);
        assert!(files[0].wanted);
        assert_eq!(files[1].priority, 0);
        assert!(!files[1].wanted);
    }

    #[test]
    fn bencoded_file_progress_imports_to_native_file_rows() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("multi.torrent");
        let info_hash = write_two_file_fixture_torrent(&torrent_path);
        let info_hash_hex = hex_lower(&info_hash);
        std::fs::rename(
            &torrent_path,
            dir.path().join(format!("{info_hash_hex}.torrent")),
        )
        .unwrap();
        let mut resume = vec![
            (
                b"file_completed_bytes".as_slice(),
                BValue::List(vec![BValue::Int(2), BValue::Int(6)]),
            ),
            (b"qBt-savePath".as_slice(), BValue::Bytes(b"/downloads")),
        ];
        resume.sort_by(|a, b| a.0.cmp(b.0));
        std::fs::write(
            dir.path().join(format!("{info_hash_hex}.fastresume")),
            encode(&BValue::Dict(resume)),
        )
        .unwrap();

        let plan = dry_run_qbittorrent_backup(dir.path()).unwrap();
        let import = plan.to_db_import(&ImportOptions::default());
        let files = &import.torrents[0].files;

        assert_eq!(files[0].completed_bytes, 2);
        assert_eq!(files[1].completed_bytes, 6);
    }

    #[test]
    fn bencoded_tracker_activity_imports_to_native_tracker_rows() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("sample.torrent");
        let info_hash = write_fixture_torrent(&torrent_path);
        let info_hash_hex = hex_lower(&info_hash);
        std::fs::rename(
            &torrent_path,
            dir.path().join(format!("{info_hash_hex}.torrent")),
        )
        .unwrap();
        let mut resume = vec![
            (b"error".as_slice(), BValue::Bytes(b"timeout")),
            (b"last_announce".as_slice(), BValue::Int(100)),
            (b"next_announce".as_slice(), BValue::Int(200)),
            (b"peers".as_slice(), BValue::Int(4)),
            (b"qBt-savePath".as_slice(), BValue::Bytes(b"/downloads")),
            (b"seeds".as_slice(), BValue::Int(12)),
        ];
        resume.sort_by(|a, b| a.0.cmp(b.0));
        std::fs::write(
            dir.path().join(format!("{info_hash_hex}.fastresume")),
            encode(&BValue::Dict(resume)),
        )
        .unwrap();

        let plan = dry_run_qbittorrent_backup(dir.path()).unwrap();
        let import = plan.to_db_import(&ImportOptions::default());
        let tracker = &import.torrents[0].trackers[0];

        assert_eq!(tracker.status, "error");
        assert_eq!(tracker.failure_reason.as_deref(), Some("timeout"));
        assert_eq!(tracker.last_announce_at, Some(100));
        assert_eq!(tracker.next_announce_at, Some(200));
        assert_eq!(tracker.seeders, Some(12));
        assert_eq!(tracker.leechers, Some(4));
    }

    #[test]
    fn bencoded_lifecycle_state_imports_to_native_torrent_row() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("sample.torrent");
        let info_hash = write_fixture_torrent(&torrent_path);
        let info_hash_hex = hex_lower(&info_hash);
        std::fs::rename(
            &torrent_path,
            dir.path().join(format!("{info_hash_hex}.torrent")),
        )
        .unwrap();
        let mut resume = vec![
            (b"added_on".as_slice(), BValue::Int(111)),
            (b"qBt-savePath".as_slice(), BValue::Bytes(b"/downloads")),
            (b"state".as_slice(), BValue::Bytes(b"downloading")),
        ];
        resume.sort_by(|a, b| a.0.cmp(b.0));
        std::fs::write(
            dir.path().join(format!("{info_hash_hex}.fastresume")),
            encode(&BValue::Dict(resume)),
        )
        .unwrap();

        let plan = dry_run_qbittorrent_backup(dir.path()).unwrap();
        let import = plan.to_db_import(&ImportOptions {
            added_at: 999,
            ..ImportOptions::default()
        });
        let row = &import.torrents[0].torrent;

        assert_eq!(row.state, "downloading");
        assert_eq!(row.added_at, 111);
        assert_eq!(row.completed_at, None);
    }

    #[test]
    fn path_remap_updates_db_rows_and_file_hint_trust() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("sample.torrent");
        let info_hash = write_fixture_torrent(&torrent_path);
        let info_hash_hex = hex_lower(&info_hash);
        std::fs::rename(
            &torrent_path,
            dir.path().join(format!("{info_hash_hex}.torrent")),
        )
        .unwrap();
        let new_root = dir.path().join("new-root");
        std::fs::create_dir(&new_root).unwrap();
        std::fs::write(new_root.join("sample.bin"), [1u8; 12]).unwrap();
        let old_root = Path::new("/old/downloads");
        let resume = serde_json::json!({
            "save_path": old_root,
            "pieces": [true],
            "completed": true
        });
        std::fs::write(
            dir.path().join(format!("{info_hash_hex}.fastresume")),
            serde_json::to_vec(&resume).unwrap(),
        )
        .unwrap();
        let options = ImportOptions {
            default_save_path: None,
            path_remaps: vec![PathRemap::new(old_root, &new_root)],
            added_at: 7,
        };

        let plan = dry_run_qbittorrent_backup_with_options(dir.path(), &options).unwrap();
        let torrent = &plan.torrents[0];

        assert_eq!(torrent.resume_confidence, ResumeConfidence::Trusted);
        let state = torrent
            .to_fastresume_state(ImportPolicy::TrustHints)
            .expect("fastresume state");
        assert_eq!(state.file_hints.len(), 1);
        let import = plan.to_db_import(&options);
        assert_eq!(
            import.torrents[0].torrent.save_path,
            new_root.to_string_lossy()
        );
    }

    #[test]
    fn path_remap_uses_longest_matching_prefix() {
        let options = ImportOptions {
            path_remaps: vec![
                PathRemap::new("/downloads", "/mnt/data"),
                PathRemap::new("/downloads/movies", "/mnt/movies"),
            ],
            ..ImportOptions::default()
        };

        assert_eq!(
            options.remap_path(Path::new("/downloads/movies/a.mkv")),
            PathBuf::from("/mnt/movies/a.mkv")
        );
        assert_eq!(
            options.remap_path(Path::new("/downloads/music/a.flac")),
            PathBuf::from("/mnt/data/music/a.flac")
        );
        assert_eq!(
            options.remap_path(Path::new("/other/a.bin")),
            PathBuf::from("/other/a.bin")
        );
    }

    #[test]
    fn import_plan_applies_native_db_rows() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("sample.torrent");
        let info_hash = write_fixture_torrent(&torrent_path);
        let info_hash_hex = hex_lower(&info_hash);
        std::fs::rename(
            &torrent_path,
            dir.path().join(format!("{info_hash_hex}.torrent")),
        )
        .unwrap();
        let resume = serde_json::json!({
            "save_path": "/downloads/imported",
            "category": " linux ",
            "tags": [" iso ", "archive", "iso"],
            "uploaded": 200,
            "downloaded": 100,
            "completed": true
        });
        std::fs::write(
            dir.path().join(format!("{info_hash_hex}.fastresume")),
            serde_json::to_vec(&resume).unwrap(),
        )
        .unwrap();
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let plan = dry_run_qbittorrent_backup(dir.path()).unwrap();

        let summary = plan
            .apply_to_db(
                &mut conn,
                &ImportOptions {
                    default_save_path: Some(PathBuf::from("/fallback")),
                    added_at: 1234,
                    ..ImportOptions::default()
                },
            )
            .unwrap();

        assert_eq!(
            summary,
            DbImportSummary {
                torrents: 1,
                files: 1,
                trackers: 1,
            }
        );
        let row = rt_db::get(&conn, &info_hash_hex).unwrap();
        assert_eq!(row.name, "sample.bin");
        assert_eq!(row.total_length, 12);
        assert_eq!(row.piece_length, 16_384);
        assert_eq!(row.piece_count, 1);
        assert_eq!(row.save_path, "/downloads/imported");
        assert_eq!(row.category.as_deref(), Some("linux"));
        assert_eq!(row.tags, vec!["iso".to_owned(), "archive".to_owned()]);
        assert_eq!(row.state, "completed");
        assert_eq!(row.completed_at, Some(1234));
        assert_eq!(row.uploaded, 200);
        assert_eq!(row.downloaded, 100);
        assert_eq!(row.ratio, 2.0);
        assert_eq!(row.trackers, vec!["https://tracker/announce".to_owned()]);
        assert_eq!(
            rt_db::list_torrent_tags(&conn, &info_hash_hex).unwrap(),
            vec!["archive", "iso"]
        );
        assert_eq!(rt_db::list_categories(&conn).unwrap(), vec!["linux"]);

        let files = rt_db::list_torrent_files(&conn, &info_hash_hex).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "sample.bin");
        assert_eq!(files[0].completed_bytes, 12);

        let trackers = rt_db::list_torrent_trackers(&conn, &info_hash_hex).unwrap();
        assert_eq!(trackers.len(), 1);
        assert_eq!(trackers[0].url, "https://tracker/announce");
        assert_eq!(trackers[0].uploaded, 200);
        assert_eq!(trackers[0].downloaded, 100);
        assert_eq!(trackers[0].left_bytes, 0);
    }

    #[test]
    fn rtorrent_dry_run_reports_missing_resume() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture_torrent(&dir.path().join("sample.torrent"));

        let plan = dry_run_rtorrent_session(dir.path()).unwrap();

        assert_eq!(plan.torrent_count(), 1);
        assert!(plan.torrents[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("missing resume")));
        assert!(plan.to_markdown().contains("Migration Dry Run"));
    }

    #[test]
    fn rtorrent_complete_resume_synthesizes_seed_piece_state() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("sample.torrent");
        let info_hash = write_fixture_torrent(&torrent_path);
        let info_hash_hex = hex_lower(&info_hash);
        std::fs::rename(
            &torrent_path,
            dir.path().join(format!("{info_hash_hex}.torrent")),
        )
        .unwrap();
        std::fs::write(dir.path().join("sample.bin"), [1u8; 12]).unwrap();
        let mut resume = vec![
            (b"complete".as_slice(), BValue::Int(1)),
            (b"d.custom1".as_slice(), BValue::Bytes(b"linux")),
            (
                b"directory".as_slice(),
                BValue::Bytes(dir.path().as_os_str().as_encoded_bytes()),
            ),
            (b"downloaded".as_slice(), BValue::Int(12)),
            (b"uploaded".as_slice(), BValue::Int(120)),
        ];
        resume.sort_by(|a, b| a.0.cmp(b.0));
        std::fs::write(
            dir.path().join(format!("{info_hash_hex}.rtorrent")),
            encode(&BValue::Dict(resume)),
        )
        .unwrap();

        let plan = dry_run_rtorrent_session(dir.path()).unwrap();
        let torrent = &plan.torrents[0];

        assert_eq!(torrent.resume_confidence, ResumeConfidence::Trusted);
        assert_eq!(torrent.category.as_deref(), Some("linux"));
        assert_eq!(torrent.completed, Some(true));
        let trusted = torrent
            .to_fastresume_state(ImportPolicy::TrustHints)
            .expect("fastresume state");
        assert_eq!(trusted.pieces, vec![PieceState::Valid]);
        let verify = torrent
            .to_fastresume_state(ImportPolicy::RequireVerification)
            .expect("fastresume state");
        assert_eq!(verify.pieces, vec![PieceState::Unknown]);
    }

    #[test]
    fn transmission_dry_run_reads_bencoded_resume() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("sample.torrent");
        write_fixture_torrent(&torrent_path);
        let mut resume = vec![
            (b"destination".as_slice(), BValue::Bytes(b"/downloads/tv")),
            (b"downloaded".as_slice(), BValue::Int(123)),
            (b"uploaded".as_slice(), BValue::Int(456)),
            (
                b"progress".as_slice(),
                BValue::Dict(vec![(
                    b"bitfield".as_slice(),
                    BValue::Bytes(&[0b1000_0000]),
                )]),
            ),
        ];
        resume.sort_by(|a, b| a.0.cmp(b.0));
        std::fs::write(
            dir.path().join("sample.resume"),
            encode(&BValue::Dict(resume)),
        )
        .unwrap();

        let plan = dry_run_transmission_session(dir.path()).unwrap();

        assert_eq!(plan.torrent_count(), 1);
        assert_eq!(
            plan.torrents[0].save_path.as_deref(),
            Some(Path::new("/downloads/tv"))
        );
        assert_eq!(plan.torrents[0].uploaded, Some(456));
        assert_eq!(plan.torrents[0].downloaded, Some(123));
        assert_eq!(plan.torrents[0].resume_confidence, ResumeConfidence::Hints);
        let state = plan.torrents[0]
            .to_fastresume_state(ImportPolicy::TrustAll)
            .expect("fastresume state");
        assert_eq!(state.pieces, vec![PieceState::Valid]);
        assert!(plan.torrents[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("truncated to torrent piece count")));
    }

    #[test]
    fn short_piece_state_is_padded_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("sample.torrent");
        let info_hash = write_fixture_torrent(&torrent_path);
        let info_hash_hex = hex_lower(&info_hash);
        std::fs::rename(
            &torrent_path,
            dir.path().join(format!("{info_hash_hex}.torrent")),
        )
        .unwrap();
        let resume = serde_json::json!({
            "save_path": "/downloads",
            "pieces": []
        });
        std::fs::write(
            dir.path().join(format!("{info_hash_hex}.fastresume")),
            serde_json::to_vec(&resume).unwrap(),
        )
        .unwrap();

        let plan = dry_run_qbittorrent_backup(dir.path()).unwrap();
        let torrent = &plan.torrents[0];

        let state = torrent
            .to_fastresume_state(ImportPolicy::TrustAll)
            .expect("fastresume state");
        assert_eq!(state.pieces, vec![PieceState::Unknown]);
        assert!(torrent
            .warnings
            .iter()
            .any(|warning| warning.contains("padded to torrent piece count")));
    }

    #[test]
    fn partial_piece_blocks_are_sorted_deduped_and_bounded() {
        let mut blocks = Vec::new();
        blocks.extend([3, 2, 2, 1].into_iter().map(BValue::Int));
        blocks.extend((0..20_000).map(BValue::Int));
        let unfinished = BValue::Dict(vec![(b"0".as_slice(), BValue::List(blocks))]);
        let resume = BValue::Dict(vec![(b"unfinished".as_slice(), unfinished)]);

        let partial = libtorrent_partial_pieces(&resume);

        assert_eq!(partial.len(), 1);
        assert_eq!(partial[0].piece, 0);
        assert_eq!(
            partial[0].received_blocks.len(),
            MAX_BLOCKS_PER_PARTIAL_PIECE
        );
        assert_eq!(partial[0].received_blocks[0], 0);
        assert_eq!(partial[0].received_blocks[1], 1);
        assert_eq!(partial[0].received_blocks[2], 2);
    }

    #[test]
    fn require_verification_downgrades_imported_valid_pieces() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("sample.torrent");
        let info_hash = write_fixture_torrent(&torrent_path);
        let info_hash_hex = hex_lower(&info_hash);
        std::fs::rename(
            &torrent_path,
            dir.path().join(format!("{info_hash_hex}.torrent")),
        )
        .unwrap();
        let resume = serde_json::json!({
            "save_path": "/downloads",
            "pieces": [true]
        });
        std::fs::write(
            dir.path().join(format!("{info_hash_hex}.fastresume")),
            serde_json::to_vec(&resume).unwrap(),
        )
        .unwrap();
        let plan = dry_run_qbittorrent_backup(dir.path()).unwrap();

        let state = plan.torrents[0]
            .to_fastresume_state(ImportPolicy::RequireVerification)
            .expect("fastresume state");

        assert_eq!(state.pieces, vec![PieceState::Unknown]);
        assert_eq!(state.import_policy, ImportPolicy::RequireVerification);
    }

    #[test]
    fn utorrent_resume_dat_matches_raw_info_hash_entries() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("sample.torrent");
        let info_hash = write_fixture_torrent(&torrent_path);
        let entry = BValue::Dict(vec![
            (b"downloaded".as_slice(), BValue::Int(12)),
            (b"have".as_slice(), BValue::Bytes(&[0b1000_0000])),
            (b"label".as_slice(), BValue::Bytes(b"archive")),
            (b"path".as_slice(), BValue::Bytes(b"/legacy/downloads")),
            (b"uploaded".as_slice(), BValue::Int(34)),
        ]);
        std::fs::write(
            dir.path().join("resume.dat"),
            encode(&BValue::Dict(vec![(info_hash.as_slice(), entry)])),
        )
        .unwrap();

        let plan = dry_run_utorrent_config(dir.path()).unwrap();
        let torrent = &plan.torrents[0];

        assert_eq!(
            torrent.resume_path.as_deref(),
            Some(dir.path().join("resume.dat").as_path())
        );
        assert_eq!(
            torrent.save_path.as_deref(),
            Some(Path::new("/legacy/downloads"))
        );
        assert_eq!(torrent.category.as_deref(), Some("archive"));
        assert_eq!(torrent.uploaded, Some(34));
        assert_eq!(torrent.downloaded, Some(12));
        assert_eq!(torrent.resume_confidence, ResumeConfidence::Hints);
        let state = torrent
            .to_fastresume_state(ImportPolicy::TrustAll)
            .expect("fastresume state");
        assert_eq!(state.pieces, vec![PieceState::Valid]);
    }

    #[test]
    fn biglybt_downloads_config_matches_hex_entries() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("sample.torrent");
        let info_hash = write_fixture_torrent(&torrent_path);
        let info_hash_hex = hex_lower(&info_hash);
        let mut entry = vec![
            (b"downloadedEver".as_slice(), BValue::Int(12)),
            (
                b"resume data".as_slice(),
                BValue::Dict(vec![(b"valid".as_slice(), BValue::Bytes(&[0b1000_0000]))]),
            ),
            (b"save_dir".as_slice(), BValue::Bytes(b"/vuze/downloads")),
            (b"uploadedEver".as_slice(), BValue::Int(56)),
        ];
        entry.sort_by(|a, b| a.0.cmp(b.0));
        std::fs::write(
            dir.path().join("downloads.config"),
            encode(&BValue::Dict(vec![(
                info_hash_hex.as_bytes(),
                BValue::Dict(entry),
            )])),
        )
        .unwrap();

        let plan = dry_run_biglybt_config(dir.path()).unwrap();
        let torrent = &plan.torrents[0];

        assert_eq!(
            torrent.save_path.as_deref(),
            Some(Path::new("/vuze/downloads"))
        );
        assert_eq!(torrent.uploaded, Some(56));
        assert_eq!(torrent.downloaded, Some(12));
        assert_eq!(torrent.resume_confidence, ResumeConfidence::Hints);
        let state = torrent
            .to_fastresume_state(ImportPolicy::TrustAll)
            .expect("fastresume state");
        assert_eq!(state.pieces, vec![PieceState::Valid]);
    }

    #[test]
    fn aggregate_json_resume_matches_base32_info_hash_entries() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("sample.torrent");
        let info_hash = write_fixture_torrent(&torrent_path);
        let base32_hash = base32_upper(&info_hash);
        let resume = serde_json::json!({
            "torrents": {
                base32_hash: {
                    "downloadDir": "/json/downloads",
                    "label": "tv",
                    "uploadedEver": 90,
                    "downloadedEver": 12,
                    "bitfield": "1"
                }
            }
        });
        std::fs::write(
            dir.path().join("resume.dat"),
            serde_json::to_vec(&resume).unwrap(),
        )
        .unwrap();

        let plan = dry_run_utorrent_config(dir.path()).unwrap();
        let torrent = &plan.torrents[0];

        assert_eq!(
            torrent.save_path.as_deref(),
            Some(Path::new("/json/downloads"))
        );
        assert_eq!(torrent.category.as_deref(), Some("tv"));
        assert_eq!(torrent.uploaded, Some(90));
        assert_eq!(torrent.downloaded, Some(12));
        assert_eq!(torrent.resume_confidence, ResumeConfidence::Hints);
        let state = torrent
            .to_fastresume_state(ImportPolicy::TrustAll)
            .expect("fastresume state");
        assert_eq!(state.pieces, vec![PieceState::Valid]);
    }

    #[test]
    fn broad_sources_are_scannable_metadata_first() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture_torrent(&dir.path().join("sample.torrent"));

        assert_eq!(
            dry_run_deluge_state(dir.path()).unwrap().source,
            MigrationSource::Deluge
        );
        assert_eq!(
            dry_run_utorrent_config(dir.path()).unwrap().source,
            MigrationSource::UTorrent
        );
        assert_eq!(
            dry_run_biglybt_config(dir.path()).unwrap().source,
            MigrationSource::BiglyBT
        );
        assert_eq!(
            dry_run_tixati_config(dir.path()).unwrap().source,
            MigrationSource::Tixati
        );
        assert_eq!(
            dry_run_generic_torrent_directory(dir.path())
                .unwrap()
                .source,
            MigrationSource::Generic
        );
    }

    #[test]
    fn oversized_resume_sidecar_is_skipped_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("sample.torrent");
        let info_hash = write_fixture_torrent(&torrent_path);
        let info_hash_hex = hex_lower(&info_hash);
        std::fs::rename(
            &torrent_path,
            dir.path().join(format!("{info_hash_hex}.torrent")),
        )
        .unwrap();
        let resume =
            std::fs::File::create(dir.path().join(format!("{info_hash_hex}.fastresume"))).unwrap();
        resume.set_len(MAX_RESUME_BYTES + 1).unwrap();

        let plan = dry_run_qbittorrent_backup(dir.path()).unwrap();

        assert_eq!(plan.torrent_count(), 1);
        assert!(plan.torrents[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("larger than")));
        assert_eq!(plan.torrents[0].resume_confidence, ResumeConfidence::None);
    }

    #[cfg(unix)]
    #[test]
    fn recursive_scan_does_not_follow_directory_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        write_fixture_torrent(&nested.join("sample.torrent"));
        std::os::unix::fs::symlink(dir.path(), nested.join("loop")).unwrap();

        let plan = dry_run_generic_torrent_directory(dir.path()).unwrap();

        assert_eq!(plan.torrent_count(), 1);
        assert_eq!(plan.skipped.len(), 0);
    }
}
