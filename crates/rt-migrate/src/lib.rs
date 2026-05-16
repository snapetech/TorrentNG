use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rt_bencode::{decode, BValue};
use rt_db::{TorrentFileRow, TorrentRow, TorrentTrackerRow};
use rt_metainfo::{parse_torrent, TorrentMeta};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MigrationError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Db(#[from] rt_db::DbError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MigrationSource {
    RTorrent,
    QBittorrent,
    Transmission,
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

    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# rtorrentNG Migration Dry Run\n\n");
        out.push_str(&format!("- Source: {:?}\n", self.source));
        out.push_str(&format!("- Root: `{}`\n", self.root.display()));
        out.push_str(&format!("- Importable torrents: {}\n", self.torrents.len()));
        out.push_str(&format!("- Warnings/skipped: {}\n\n", self.warning_count()));
        out.push_str("| Info hash | Name | Save path | Tags | Warnings |\n");
        out.push_str("| --- | --- | --- | --- | --- |\n");
        for torrent in &self.torrents {
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} |\n",
                torrent.info_hash,
                escape_table(&torrent.name),
                torrent
                    .save_path
                    .as_ref()
                    .map(|path| format!("`{}`", path.display()))
                    .unwrap_or_else(|| "-".to_owned()),
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
    pub trackers: Vec<String>,
    pub warnings: Vec<String>,
}

impl MigrationTorrent {
    pub fn to_db_rows(&self, options: &ImportOptions) -> DbTorrentImport {
        let save_path = self
            .save_path
            .clone()
            .or_else(|| options.default_save_path.clone())
            .unwrap_or_else(|| PathBuf::from("."))
            .to_string_lossy()
            .to_string();
        let uploaded = self.uploaded.unwrap_or_default();
        let downloaded = self.downloaded.unwrap_or_default();
        let ratio = if downloaded == 0 {
            0.0
        } else {
            uploaded as f64 / downloaded as f64
        };
        let completed = self.completed.unwrap_or(false);
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
                state: if completed { "completed" } else { "stopped" }.to_owned(),
                added_at: options.added_at,
                completed_at: completed.then_some(options.added_at),
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
                    priority: 1,
                    wanted: true,
                    completed_bytes: if completed {
                        i64_saturating(file.length)
                    } else {
                        0
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
                    status: "never_announced".to_owned(),
                    last_announce_at: None,
                    next_announce_at: None,
                    last_success_at: None,
                    failure_reason: None,
                    warning_message: None,
                    seeders: None,
                    leechers: None,
                    completed: None,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationFile {
    pub index: u32,
    pub path: String,
    pub length: u64,
    pub offset: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedEntry {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub default_save_path: Option<PathBuf>,
    pub added_at: i64,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            default_save_path: None,
            added_at: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DbImportPlan {
    pub torrents: Vec<DbTorrentImport>,
}

impl DbImportPlan {
    pub fn apply(
        &self,
        conn: &mut rusqlite::Connection,
    ) -> Result<DbImportSummary, MigrationError> {
        for import in &self.torrents {
            rt_db::upsert(conn, &import.torrent)?;
            rt_db::replace_torrent_files(conn, &import.torrent.info_hash, &import.files)?;
            rt_db::replace_torrent_trackers(conn, &import.torrent.info_hash, &import.trackers)?;
        }
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
}

pub fn dry_run_rtorrent_session(root: impl AsRef<Path>) -> Result<MigrationPlan, MigrationError> {
    dry_run_session(root.as_ref(), MigrationSource::RTorrent, &["rtorrent"])
}

pub fn dry_run_qbittorrent_backup(root: impl AsRef<Path>) -> Result<MigrationPlan, MigrationError> {
    dry_run_session(root.as_ref(), MigrationSource::QBittorrent, &["fastresume"])
}

pub fn dry_run_transmission_session(
    root: impl AsRef<Path>,
) -> Result<MigrationPlan, MigrationError> {
    dry_run_session(root.as_ref(), MigrationSource::Transmission, &["resume"])
}

fn dry_run_session(
    root: &Path,
    source: MigrationSource,
    resume_extensions: &[&str],
) -> Result<MigrationPlan, MigrationError> {
    let mut torrents = Vec::new();
    let mut skipped = Vec::new();
    let mut resume_by_stem = BTreeMap::new();

    for path in collect_files(root)? {
        if extension_is(&path, resume_extensions) {
            if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
                resume_by_stem.insert(stem.to_owned(), path);
            }
        }
    }

    for path in collect_files(root)? {
        if !extension_is(&path, &["torrent"]) {
            continue;
        }
        match migration_torrent_from_path(&path, &resume_by_stem) {
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
) -> Result<MigrationTorrent, String> {
    let raw = std::fs::read(path).map_err(|e| e.to_string())?;
    let meta = match parse_torrent(&raw).map_err(|e| e.to_string())? {
        TorrentMeta::V1(meta) | TorrentMeta::Hybrid(meta, _) => meta,
        TorrentMeta::V2(_) => return Err("pure v2 torrents are not yet importable".to_owned()),
    };
    let info_hash = hex_lower(&meta.info_hash);
    let resume_path = resume_by_stem
        .get(&info_hash)
        .or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| resume_by_stem.get(stem))
        })
        .cloned();
    let mut warnings = Vec::new();
    let resume = resume_path
        .as_ref()
        .and_then(|path| match parse_resume_file(path) {
            Ok(resume) => Some(resume),
            Err(error) => {
                warnings.push(format!("resume parse failed: {error}"));
                None
            }
        })
        .unwrap_or_default();
    if resume_path.is_none() {
        warnings.push("missing resume sidecar; import will require verification".to_owned());
    }

    let trackers = meta.all_trackers();
    let files = meta
        .files
        .iter()
        .map(|file| MigrationFile {
            index: file.index,
            path: file.path.as_display(),
            length: file.length,
            offset: file.offset,
        })
        .collect();

    Ok(MigrationTorrent {
        info_hash,
        name: meta.name.clone(),
        total_length: meta.total_length(),
        piece_length: meta.piece_length,
        piece_count: meta.pieces.len() as u64,
        is_private: meta.private,
        files,
        torrent_path: path.to_path_buf(),
        resume_path,
        save_path: resume.save_path,
        category: resume.category,
        tags: resume.tags,
        uploaded: resume.uploaded,
        downloaded: resume.downloaded,
        completed: resume.completed,
        trackers,
        warnings,
    })
}

fn parse_resume_file(path: &Path) -> Result<ResumeData, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.is_empty() {
        return Ok(ResumeData::default());
    }
    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) {
        return Ok(resume_from_json(&json));
    }
    let value = decode(&bytes).map_err(|e| e.to_string())?;
    Ok(resume_from_bencode(&value))
}

fn resume_from_json(value: &serde_json::Value) -> ResumeData {
    let mut resume = ResumeData::default();
    resume.save_path = first_json_string(
        value,
        &["save_path", "savePath", "downloadDir", "destination"],
    )
    .map(PathBuf::from);
    resume.category = first_json_string(value, &["category", "label"]).map(str::to_owned);
    resume.tags = first_json_array(value, &["tags", "labels"]);
    resume.uploaded = first_json_u64(value, &["uploaded", "uploaded_bytes", "uploadedBytes"]);
    resume.downloaded = first_json_u64(
        value,
        &["downloaded", "downloaded_bytes", "downloadedBytes"],
    );
    resume.completed = first_json_bool(value, &["completed", "complete"]);
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
        ],
    )
    .map(PathBuf::from);
    resume.category = first_bencode_string(
        value,
        &[
            b"category".as_slice(),
            b"label".as_slice(),
            b"qBt-category".as_slice(),
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
        ],
    );
    resume.downloaded = first_bencode_u64(
        value,
        &[
            b"downloaded".as_slice(),
            b"downloaded_bytes".as_slice(),
            b"total_downloaded".as_slice(),
        ],
    );
    resume.completed =
        first_bencode_bool(value, &[b"completed".as_slice(), b"complete".as_slice()]);
    resume
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
        if path.is_dir() {
            collect_files_inner(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
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

fn first_json_array(value: &serde_json::Value, keys: &[&str]) -> Vec<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(serde_json::Value::as_array))
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn first_json_u64(value: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(serde_json::Value::as_u64))
}

fn first_json_bool(value: &serde_json::Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(serde_json::Value::as_bool))
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
            Some(bytes) => bytes.as_str().map(|tags| {
                tags.split(',')
                    .map(str::trim)
                    .filter(|tag| !tag.is_empty())
                    .map(str::to_owned)
                    .collect()
            }),
            None => None,
        })
        .unwrap_or_default()
}

fn first_bencode_u64(value: &BValue<'_>, keys: &[&[u8]]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(BValue::as_int))
        .and_then(|value| u64::try_from(value).ok())
}

fn first_bencode_bool(value: &BValue<'_>, keys: &[&[u8]]) -> Option<bool> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(BValue::as_int))
        .map(|value| value != 0)
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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
        assert!(torrent.warnings.is_empty());
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
    fn transmission_dry_run_reads_bencoded_resume() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_path = dir.path().join("sample.torrent");
        write_fixture_torrent(&torrent_path);
        let mut resume = vec![
            (b"destination".as_slice(), BValue::Bytes(b"/downloads/tv")),
            (b"downloaded".as_slice(), BValue::Int(123)),
            (b"uploaded".as_slice(), BValue::Int(456)),
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
    }
}
