//! Migration certification matrix: every supported client, every direction.
//!
//! Fixtures are synthetic but Linux-ISO shaped (real ISO names, realistic
//! piece counts, single-file and multi-file release layouts) so the matrix
//! exercises multi-piece complete *and* partial state, not just toy 1-piece
//! torrents. Each client is checked in three directions:
//!
//!   1. IMPORT     — an independently-built client state dir → `dry_run_*`
//!   2. EXPORT     — native DB + blob + fastresume → `rt_migrate::export`
//!   3. ROUND-TRIP — export, then re-import through that client's importer
//!
//! Everything is offline and deterministic (no network, no real ISOs).

use std::path::{Path, PathBuf};

use rt_db::TorrentRow;
use rt_fastresume::{FastresumeState, FastresumeStore, ImportPolicy, PieceState};
use rt_metainfo::{parse_torrent, TorrentMeta};
use rt_migrate::export::{ExportFormat, ExportPlan};
use rt_migrate::{
    dry_run_biglybt_config, dry_run_deluge_state, dry_run_generic_torrent_directory,
    dry_run_qbittorrent_backup, dry_run_rtorrent_session, dry_run_tixati_config,
    dry_run_transmission_session, dry_run_utorrent_config, ImportOptions, MigrationError,
    MigrationPlan, ResumeConfidence,
};

const PIECE_LEN: i64 = 262_144;

// --- minimal owned bencode (independent of the exporter) -------------------

enum B {
    I(i64),
    S(Vec<u8>),
    L(Vec<B>),
    D(Vec<(Vec<u8>, B)>),
}

fn bs(t: &str) -> B {
    B::S(t.as_bytes().to_vec())
}

fn benc(v: &B, out: &mut Vec<u8>) {
    match v {
        B::I(n) => {
            out.push(b'i');
            out.extend_from_slice(n.to_string().as_bytes());
            out.push(b'e');
        }
        B::S(b) => {
            out.extend_from_slice(b.len().to_string().as_bytes());
            out.push(b':');
            out.extend_from_slice(b);
        }
        B::L(items) => {
            out.push(b'l');
            for i in items {
                benc(i, out);
            }
            out.push(b'e');
        }
        B::D(pairs) => {
            let mut p: Vec<&(Vec<u8>, B)> = pairs.iter().collect();
            p.sort_by(|a, b| a.0.cmp(&b.0));
            out.push(b'd');
            for (k, val) in p {
                out.extend_from_slice(k.len().to_string().as_bytes());
                out.push(b':');
                out.extend_from_slice(k);
                benc(val, out);
            }
            out.push(b'e');
        }
    }
}

fn bencode(v: B) -> Vec<u8> {
    let mut out = Vec::new();
    benc(&v, &mut out);
    out
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn bitfield_msb(have: &[bool]) -> Vec<u8> {
    let mut out = vec![0u8; have.len().div_ceil(8)];
    for (i, &b) in have.iter().enumerate() {
        if b {
            out[i / 8] |= 0x80 >> (i % 8);
        }
    }
    out
}

fn piece_bytes(have: &[bool]) -> Vec<u8> {
    have.iter().map(|&b| u8::from(b)).collect()
}

// --- ISO-shaped fixtures ---------------------------------------------------

struct Iso {
    raw: Vec<u8>,
    info_hash: [u8; 20],
    hash_hex: String,
    name: String,
    /// (relative path, length) — single entry for single-file torrents.
    files: Vec<(String, i64)>,
    total_len: i64,
    piece_count: usize,
}

fn parse_hash(raw: &[u8]) -> [u8; 20] {
    match parse_torrent(raw).unwrap() {
        TorrentMeta::V1(m) | TorrentMeta::Hybrid(m, _) => m.info_hash,
        TorrentMeta::V2(_) => unreachable!(),
    }
}

/// Single-file ISO, `pieces` whole pieces (e.g. a netinst image).
fn iso_single(name: &str, pieces: usize) -> Iso {
    let total = PIECE_LEN * pieces as i64;
    let pieces_blob = vec![7u8; 20 * pieces];
    let info = B::D(vec![
        (b"length".to_vec(), B::I(total)),
        (b"name".to_vec(), bs(name)),
        (b"piece length".to_vec(), B::I(PIECE_LEN)),
        (b"pieces".to_vec(), B::S(pieces_blob)),
    ]);
    let raw = bencode(B::D(vec![(b"info".to_vec(), info)]));
    let info_hash = parse_hash(&raw);
    Iso {
        hash_hex: hex(&info_hash),
        name: name.to_string(),
        files: vec![(name.to_string(), total)],
        total_len: total,
        piece_count: pieces,
        raw,
        info_hash,
    }
}

/// Multi-file release directory: the ISO plus a checksum file.
fn iso_set(dir: &str, pieces: usize) -> Iso {
    let iso_len = PIECE_LEN * (pieces as i64 - 1) + 4096;
    let sums_len = 512i64;
    let total = iso_len + sums_len;
    let pieces_blob = vec![3u8; 20 * pieces];
    let files = B::L(vec![
        B::D(vec![
            (b"length".to_vec(), B::I(iso_len)),
            (
                b"path".to_vec(),
                B::L(vec![bs(&format!("{dir}-x86_64.iso"))]),
            ),
        ]),
        B::D(vec![
            (b"length".to_vec(), B::I(sums_len)),
            (b"path".to_vec(), B::L(vec![bs("sha256sums.txt")])),
        ]),
    ]);
    let info = B::D(vec![
        (b"files".to_vec(), files),
        (b"name".to_vec(), bs(dir)),
        (b"piece length".to_vec(), B::I(PIECE_LEN)),
        (b"pieces".to_vec(), B::S(pieces_blob)),
    ]);
    let raw = bencode(B::D(vec![(b"info".to_vec(), info)]));
    let info_hash = parse_hash(&raw);
    Iso {
        hash_hex: hex(&info_hash),
        name: dir.to_string(),
        files: vec![
            (format!("{dir}/{dir}-x86_64.iso"), iso_len),
            (format!("{dir}/sha256sums.txt"), sums_len),
        ],
        total_len: total,
        piece_count: pieces,
        raw,
        info_hash,
    }
}

#[derive(Clone)]
struct Meta {
    save_path: PathBuf,
    category: String,
    tags: Vec<String>,
    uploaded: u64,
    downloaded: u64,
}

fn meta(tmp: &Path) -> Meta {
    let save = tmp.join("downloads");
    std::fs::create_dir_all(&save).unwrap();
    Meta {
        save_path: save,
        category: "linux-isos".into(),
        tags: vec!["distro".into()],
        uploaded: 9_000_000,
        downloaded: 4_000_000,
    }
}

/// Materialise the data files so rTorrent file-hint synthesis can validate.
fn lay_down_data(iso: &Iso, m: &Meta) {
    for (rel, len) in &iso.files {
        let path = m.save_path.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vec![0u8; *len as usize]).unwrap();
    }
}

// --- native state builder (export source) ----------------------------------

fn native_state(tmp: &Path, iso: &Iso, have: &[bool], m: &Meta) -> (PathBuf, PathBuf, PathBuf) {
    let db = tmp.join("state.db");
    let blob = tmp.join("torrents");
    let fr = tmp.join("fastresume");
    std::fs::create_dir_all(&blob).unwrap();
    std::fs::write(blob.join(format!("{}.torrent", iso.hash_hex)), &iso.raw).unwrap();

    let conn = rusqlite::Connection::open(&db).unwrap();
    rt_db::migrate(&conn).unwrap();
    let complete = have.iter().all(|&b| b);
    let row = TorrentRow {
        info_hash: iso.hash_hex.clone(),
        name: iso.name.clone(),
        total_length: iso.total_len,
        piece_length: PIECE_LEN,
        piece_count: iso.piece_count as i64,
        is_private: false,
        save_path: m.save_path.to_string_lossy().into_owned(),
        category: Some(m.category.clone()),
        tags: m.tags.clone(),
        state: if complete { "seeding" } else { "downloading" }.into(),
        added_at: 1_700_000_000,
        completed_at: complete.then_some(1_700_000_500),
        uploaded: m.uploaded as i64,
        downloaded: m.downloaded as i64,
        ratio: 2.25,
        trackers: vec!["https://tracker.example/announce".into()],
    };
    rt_db::upsert(&conn, &row).unwrap();
    drop(conn);

    let store = FastresumeStore::new(fr.clone());
    let mut st = FastresumeState::new_empty(
        &iso.info_hash,
        iso.piece_count as u32,
        ImportPolicy::TrustHints,
    );
    st.pieces = have
        .iter()
        .map(|&b| {
            if b {
                PieceState::Valid
            } else {
                PieceState::Unknown
            }
        })
        .collect();
    st.uploaded_bytes = m.uploaded;
    st.downloaded_bytes = m.downloaded;
    store.save(&st).unwrap();

    (db, blob, fr)
}

// --- per-client IMPORT fixture writers (independent of the exporter) --------

fn libtorrent_fixture(dir: &Path, iso: &Iso, have: &[bool], m: &Meta) {
    std::fs::write(dir.join(format!("{}.torrent", iso.hash_hex)), &iso.raw).unwrap();
    let entry = B::D(vec![
        (b"pieces".to_vec(), B::S(piece_bytes(have))),
        (b"save_path".to_vec(), bs(&m.save_path.to_string_lossy())),
        (b"qBt-category".to_vec(), bs(&m.category)),
        (
            b"qBt-tags".to_vec(),
            B::L(m.tags.iter().map(|t| bs(t)).collect()),
        ),
        (b"total_uploaded".to_vec(), B::I(m.uploaded as i64)),
        (b"total_downloaded".to_vec(), B::I(m.downloaded as i64)),
    ]);
    std::fs::write(
        dir.join(format!("{}.fastresume", iso.hash_hex)),
        bencode(entry),
    )
    .unwrap();
}

fn transmission_fixture(dir: &Path, iso: &Iso, have: &[bool], m: &Meta) {
    let t = dir.join("torrents");
    let r = dir.join("resume");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::create_dir_all(&r).unwrap();
    std::fs::write(t.join(format!("{}.torrent", iso.hash_hex)), &iso.raw).unwrap();
    let entry = B::D(vec![
        (b"destination".to_vec(), bs(&m.save_path.to_string_lossy())),
        (b"uploaded".to_vec(), B::I(m.uploaded as i64)),
        (b"downloaded".to_vec(), B::I(m.downloaded as i64)),
        (
            b"progress".to_vec(),
            B::D(vec![(b"have".to_vec(), B::S(bitfield_msb(have)))]),
        ),
    ]);
    std::fs::write(r.join(format!("{}.resume", iso.hash_hex)), bencode(entry)).unwrap();
}

fn deluge_fixture(dir: &Path, iso: &Iso, have: &[bool], m: &Meta) {
    // Deluge keeps libtorrent .fastresume per torrent.
    libtorrent_fixture(dir, iso, have, m);
}

fn utorrent_fixture(dir: &Path, iso: &Iso, have: &[bool], m: &Meta) {
    std::fs::write(dir.join(format!("{}.torrent", iso.hash_hex)), &iso.raw).unwrap();
    let entry = B::D(vec![
        (b"path".to_vec(), bs(&m.save_path.to_string_lossy())),
        (b"label".to_vec(), bs(&m.category)),
        (b"uploaded".to_vec(), B::I(m.uploaded as i64)),
        (b"downloaded".to_vec(), B::I(m.downloaded as i64)),
        (b"have".to_vec(), B::S(bitfield_msb(have))),
    ]);
    std::fs::write(
        dir.join("resume.dat"),
        bencode(B::D(vec![(iso.hash_hex.clone().into_bytes(), entry)])),
    )
    .unwrap();
}

fn biglybt_fixture(dir: &Path, iso: &Iso, have: &[bool], m: &Meta) {
    std::fs::write(dir.join(format!("{}.torrent", iso.hash_hex)), &iso.raw).unwrap();
    let entry = B::D(vec![
        (b"save_dir".to_vec(), bs(&m.save_path.to_string_lossy())),
        (b"uploadedEver".to_vec(), B::I(m.uploaded as i64)),
        (b"downloadedEver".to_vec(), B::I(m.downloaded as i64)),
        (
            b"resume data".to_vec(),
            B::D(vec![(b"valid".to_vec(), B::S(bitfield_msb(have)))]),
        ),
    ]);
    std::fs::write(
        dir.join("downloads.config"),
        bencode(B::D(vec![(iso.hash_hex.clone().into_bytes(), entry)])),
    )
    .unwrap();
}

fn rtorrent_fixture(dir: &Path, iso: &Iso, have: &[bool], m: &Meta) {
    std::fs::write(dir.join(format!("{}.torrent", iso.hash_hex)), &iso.raw).unwrap();
    let complete = have.iter().all(|&b| b);
    let entry = B::D(vec![
        (b"complete".to_vec(), B::I(i64::from(complete))),
        (b"directory".to_vec(), bs(&m.save_path.to_string_lossy())),
        (b"d.custom1".to_vec(), bs(&m.category)),
        (b"uploaded".to_vec(), B::I(m.uploaded as i64)),
        (b"downloaded".to_vec(), B::I(m.downloaded as i64)),
    ]);
    std::fs::write(
        dir.join(format!("{}.rtorrent", iso.hash_hex)),
        bencode(entry),
    )
    .unwrap();
}

fn tixati_fixture(dir: &Path, iso: &Iso, _have: &[bool], _m: &Meta) {
    // Tixati progress is an undocumented proprietary binary blob: it must
    // never be guessed into trusted state. We still expect metadata via the
    // .torrent itself.
    std::fs::write(dir.join(format!("{}.torrent", iso.hash_hex)), &iso.raw).unwrap();
    std::fs::write(
        dir.join(format!("{}.dat", iso.hash_hex)),
        [0x00u8, 0xFF, b'T', b'I', b'X', 0x01],
    )
    .unwrap();
}

type ImporterFn = fn(&Path) -> Result<MigrationPlan, MigrationError>;
type FixtureFn = fn(&Path, &Iso, &[bool], &Meta);

struct Client {
    name: &'static str,
    write_fixture: FixtureFn,
    import: ImporterFn,
    /// Export target, or `None` (Tixati) — generic is the documented exit.
    export: Option<ExportFormat>,
    /// True if the format carries a per-piece map (partial state survives).
    carries_pieces: bool,
}

fn qb(p: &Path) -> Result<MigrationPlan, MigrationError> {
    dry_run_qbittorrent_backup(p)
}
fn tr(p: &Path) -> Result<MigrationPlan, MigrationError> {
    dry_run_transmission_session(p)
}
fn dl(p: &Path) -> Result<MigrationPlan, MigrationError> {
    dry_run_deluge_state(p)
}
fn ut(p: &Path) -> Result<MigrationPlan, MigrationError> {
    dry_run_utorrent_config(p)
}
fn bg(p: &Path) -> Result<MigrationPlan, MigrationError> {
    dry_run_biglybt_config(p)
}
fn rt(p: &Path) -> Result<MigrationPlan, MigrationError> {
    dry_run_rtorrent_session(p)
}
fn tx(p: &Path) -> Result<MigrationPlan, MigrationError> {
    dry_run_tixati_config(p)
}

fn matrix() -> Vec<Client> {
    vec![
        Client {
            name: "qbittorrent",
            write_fixture: libtorrent_fixture,
            import: qb,
            export: Some(ExportFormat::Libtorrent),
            carries_pieces: true,
        },
        Client {
            name: "deluge",
            write_fixture: deluge_fixture,
            import: dl,
            export: Some(ExportFormat::Libtorrent),
            carries_pieces: true,
        },
        Client {
            name: "transmission",
            write_fixture: transmission_fixture,
            import: tr,
            export: Some(ExportFormat::Transmission),
            carries_pieces: true,
        },
        Client {
            name: "utorrent",
            write_fixture: utorrent_fixture,
            import: ut,
            export: Some(ExportFormat::Utorrent),
            carries_pieces: true,
        },
        Client {
            name: "biglybt",
            write_fixture: biglybt_fixture,
            import: bg,
            export: Some(ExportFormat::Biglybt),
            carries_pieces: true,
        },
        Client {
            name: "rtorrent",
            write_fixture: rtorrent_fixture,
            import: rt,
            export: Some(ExportFormat::Rtorrent),
            carries_pieces: false, // complete-state only
        },
        Client {
            name: "tixati",
            write_fixture: tixati_fixture,
            import: tx,
            export: None,
            carries_pieces: false,
        },
    ]
}

fn one(plan: &MigrationPlan) -> &rt_migrate::MigrationTorrent {
    assert_eq!(plan.torrent_count(), 1, "expected exactly one torrent");
    &plan.torrents[0]
}

// --- direction 1: IMPORT ---------------------------------------------------

#[test]
fn import_matrix_complete_and_partial_isos() {
    for c in matrix() {
        for iso in [
            iso_single("debian-12.5.0-amd64-netinst.iso", 4),
            iso_set("archlinux-2024.05.01", 4),
        ] {
            // tixati can't pair an aggregate-free .dat to a multi-file stem
            // differently; both ISO shapes still exercise it.
            let complete = [true, true, true, true];
            let partial = [true, true, false, false];

            for have in [complete, partial] {
                let tmp = tempfile::tempdir().unwrap();
                let m = meta(tmp.path());
                lay_down_data(&iso, &m);
                (c.write_fixture)(tmp.path(), &iso, &have, &m);

                let plan = (c.import)(tmp.path()).unwrap();
                let t = one(&plan);
                assert_eq!(t.info_hash, iso.hash_hex, "{} {}", c.name, iso.name);

                if c.name == "tixati" {
                    // Never falsely trusted; metadata path still works.
                    assert_ne!(t.resume_confidence, ResumeConfidence::Trusted);
                    assert!(t
                        .to_fastresume_state(ImportPolicy::TrustAll)
                        .map(|s| s.pieces.iter().all(|p| *p != PieceState::Valid))
                        .unwrap_or(true));
                    continue;
                }

                assert_eq!(t.save_path.as_deref(), Some(m.save_path.as_path()));
                assert_eq!(t.uploaded, Some(m.uploaded));
                assert_eq!(t.downloaded, Some(m.downloaded));

                let all = have.iter().all(|&b| b);
                if c.carries_pieces {
                    let st = t
                        .to_fastresume_state(ImportPolicy::TrustHints)
                        .expect("piece state");
                    let want: Vec<PieceState> = have
                        .iter()
                        .map(|&b| {
                            if b {
                                PieceState::Valid
                            } else {
                                PieceState::Unknown
                            }
                        })
                        .collect();
                    assert_eq!(st.pieces, want, "{} {} complete={all}", c.name, iso.name);
                } else if all {
                    // rTorrent: complete + present files → synthesised seed.
                    assert_eq!(
                        t.resume_confidence,
                        ResumeConfidence::Trusted,
                        "{} {}",
                        c.name,
                        iso.name
                    );
                } else {
                    // rTorrent partial: no trusted piece state.
                    assert_ne!(t.resume_confidence, ResumeConfidence::Trusted);
                }
            }
        }
    }
}

// --- direction 2: EXPORT ---------------------------------------------------

#[test]
fn export_matrix_fidelity_and_layout() {
    let iso = iso_single("ubuntu-24.04-desktop-amd64.iso", 4);
    for c in matrix() {
        let Some(fmt) = c.export else { continue };
        for (have, label) in [
            ([true, true, true, true], "complete"),
            ([true, true, false, false], "partial"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let m = meta(tmp.path());
            let (db, blob, fr) = native_state(tmp.path(), &iso, &have, &m);
            let plan = ExportPlan::new(fmt, &db, &blob, &fr).unwrap();
            let s = plan.fidelity_summary();
            assert_eq!(plan.torrent_count(), 1, "{} {label}", c.name);

            match fmt {
                ExportFormat::Rtorrent if label == "complete" => {
                    assert_eq!(s.complete_only, 1, "{} {label}", c.name)
                }
                ExportFormat::Rtorrent => assert_eq!(s.metadata_only, 1, "{} {label}", c.name),
                _ => assert_eq!(s.recheck_free, 1, "{} {label}", c.name),
            }

            let out = tmp.path().join(format!("out-{label}"));
            let summary = plan.write(&out).unwrap();
            assert_eq!(summary.torrents, 1);
            // The .torrent is always exported alongside resume state.
            let has_blob = std::fs::read_dir(&out)
                .unwrap()
                .flatten()
                .any(|e| e.path().extension().is_some_and(|x| x == "torrent"))
                || out.join("torrents").is_dir();
            assert!(has_blob, "{} {label}: no .torrent written", c.name);
        }
    }
}

// --- direction 3: ROUND-TRIP (export → re-import) --------------------------

#[test]
fn round_trip_matrix_preserves_state() {
    for c in matrix() {
        let Some(fmt) = c.export else { continue };
        let iso = iso_single("fedora-40-workstation.iso", 4);

        // Complete: every exportable format must round-trip recheck-free.
        {
            let tmp = tempfile::tempdir().unwrap();
            let m = meta(tmp.path());
            lay_down_data(&iso, &m);
            let have = [true, true, true, true];
            let (db, blob, fr) = native_state(tmp.path(), &iso, &have, &m);
            let out = tmp.path().join("out");
            ExportPlan::new(fmt, &db, &blob, &fr)
                .unwrap()
                .write(&out)
                .unwrap();

            let plan = (c.import)(&out).unwrap();
            let t = one(&plan);
            assert_eq!(t.info_hash, iso.hash_hex, "{} complete", c.name);
            assert_eq!(t.save_path.as_deref(), Some(m.save_path.as_path()));
            assert_eq!(t.uploaded, Some(m.uploaded));

            if c.carries_pieces {
                assert_eq!(
                    t.to_fastresume_state(ImportPolicy::TrustHints)
                        .unwrap()
                        .pieces,
                    vec![PieceState::Valid; iso.piece_count],
                    "{} complete round-trip",
                    c.name
                );
            } else {
                assert_eq!(
                    t.resume_confidence,
                    ResumeConfidence::Trusted,
                    "{} complete round-trip",
                    c.name
                );
            }
        }

        // Partial: only piece-carrying formats keep the exact have-map.
        if c.carries_pieces {
            let tmp = tempfile::tempdir().unwrap();
            let m = meta(tmp.path());
            lay_down_data(&iso, &m);
            let have = [true, false, true, false];
            let (db, blob, fr) = native_state(tmp.path(), &iso, &have, &m);
            let out = tmp.path().join("out");
            ExportPlan::new(fmt, &db, &blob, &fr)
                .unwrap()
                .write(&out)
                .unwrap();

            let plan = (c.import)(&out).unwrap();
            let t = one(&plan);
            let want: Vec<PieceState> = have
                .iter()
                .map(|&b| {
                    if b {
                        PieceState::Valid
                    } else {
                        PieceState::Unknown
                    }
                })
                .collect();
            assert_eq!(
                t.to_fastresume_state(ImportPolicy::TrustHints)
                    .unwrap()
                    .pieces,
                want,
                "{} partial round-trip",
                c.name
            );
        }
    }
}

// --- generic is the universal exit valve (incl. Tixati) --------------------

#[test]
fn generic_export_is_universal_exit() {
    let iso = iso_set("linuxmint-21.3-cinnamon", 4);
    let tmp = tempfile::tempdir().unwrap();
    let m = meta(tmp.path());
    let have = [true, true, true, true];
    let (db, blob, fr) = native_state(tmp.path(), &iso, &have, &m);
    let out = tmp.path().join("out");
    let summary = ExportPlan::new(ExportFormat::Generic, &db, &blob, &fr)
        .unwrap()
        .write(&out)
        .unwrap();
    assert_eq!(summary.torrents, 1);
    assert!(out.join(format!("{}.torrent", iso.hash_hex)).exists());
    let manifest = std::fs::read_to_string(out.join("manifest.json")).unwrap();
    assert!(manifest.contains(&iso.hash_hex));
    assert!(manifest.contains("linux-isos"));
    // Re-scannable by the generic importer.
    assert_eq!(
        dry_run_generic_torrent_directory(&out)
            .unwrap()
            .torrent_count(),
        1
    );
}

// --- production-shape regression tests --------------------------------
//
// These reproduce exact real-world shapes found by importing live
// production torrents from a real rTorrent seedbox (not synthetic-only
// coverage): real non-zero file content, `directory` pointing directly at
// the content folder (rTorrent's own-subdirectory default, unlike
// qBittorrent's save_path-is-parent convention this file's
// `rtorrent_fixture` above deliberately uses), a BEP47 padding file, and
// a folder renamed by an external tool. Each of these caused a real
// import failure before being found and fixed this way; they must never
// regress silently.

/// Distinguishable, non-zero content so a byte-corruption bug (truncation,
/// wrong-file swap, zero-fill) is actually detectable - unlike this file's
/// `lay_down_data` above, which writes all-zero placeholders.
fn distinct_content(seed: u8, len: usize) -> Vec<u8> {
    (0..len).map(|i| seed.wrapping_add(i as u8)).collect()
}

struct MultiFileFixture {
    raw: Vec<u8>,
    hash_hex: String,
    name: String,
    /// (relative path within the content folder, content) for real files.
    files: Vec<(String, Vec<u8>)>,
}

/// A real multi-file v1 torrent with two real files and, optionally, a
/// BEP47 padding entry (`attr: "p"`) that is deliberately never written to
/// disk - exactly matching what every real client does.
fn multi_file_fixture(name: &str, include_padding: bool) -> MultiFileFixture {
    let file_a = distinct_content(0x11, 300);
    let file_b = distinct_content(0x77, 500);
    let mut file_entries = vec![
        B::D(vec![
            (b"length".to_vec(), B::I(file_a.len() as i64)),
            (b"path".to_vec(), B::L(vec![bs("01 - track.flac")])),
        ]),
        B::D(vec![
            (b"length".to_vec(), B::I(file_b.len() as i64)),
            (b"path".to_vec(), B::L(vec![bs("02 - track.flac")])),
        ]),
    ];
    if include_padding {
        file_entries.push(B::D(vec![
            (b"attr".to_vec(), bs("p")),
            (b"length".to_vec(), B::I(28)),
            (b"path".to_vec(), B::L(vec![bs(".pad"), bs("28")])),
        ]));
    }
    let total = file_a.len() as i64 + file_b.len() as i64 + if include_padding { 28 } else { 0 };
    let piece_count = (((total + PIECE_LEN - 1) / PIECE_LEN).max(1)) as usize;
    let pieces_blob = vec![9u8; 20 * piece_count];
    let info = B::D(vec![
        (b"files".to_vec(), B::L(file_entries)),
        (b"name".to_vec(), bs(name)),
        (b"piece length".to_vec(), B::I(PIECE_LEN)),
        (b"pieces".to_vec(), B::S(pieces_blob)),
    ]);
    let raw = bencode(B::D(vec![
        (
            b"announce".to_vec(),
            bs("https://tracker.example/announce"),
        ),
        (b"info".to_vec(), info),
    ]));
    let info_hash = parse_hash(&raw);
    MultiFileFixture {
        hash_hex: hex(&info_hash),
        name: name.to_string(),
        files: vec![
            ("01 - track.flac".to_string(), file_a),
            ("02 - track.flac".to_string(), file_b),
        ],
        raw,
    }
}

/// A real rTorrent-style `.rtorrent` resume sidecar whose `directory`
/// points directly at `content_dir` - rTorrent's actual multi-file
/// convention, not the qBittorrent-style parent this file's
/// `rtorrent_fixture` (above) uses.
fn write_real_rtorrent_sidecar(session_dir: &Path, fx: &MultiFileFixture, content_dir: &Path) {
    std::fs::write(
        session_dir.join(format!("{}.torrent", fx.hash_hex)),
        &fx.raw,
    )
    .unwrap();
    let entry = B::D(vec![
        (b"complete".to_vec(), B::I(1)),
        (b"directory".to_vec(), bs(&content_dir.to_string_lossy())),
    ]);
    std::fs::write(
        session_dir.join(format!("{}.rtorrent", fx.hash_hex)),
        bencode(entry),
    )
    .unwrap();
}

#[test]
fn production_shape_directory_equals_content_folder_bytes_preserved_and_trusted() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = tmp.path().join("session");
    std::fs::create_dir_all(&session_dir).unwrap();
    let fx = multi_file_fixture("Real Album", false);

    // rTorrent's own-subdirectory convention: directory == parent/name.
    let content_dir = tmp.path().join("downloads").join(&fx.name);
    std::fs::create_dir_all(&content_dir).unwrap();
    for (rel, content) in &fx.files {
        std::fs::write(content_dir.join(rel), content).unwrap();
    }
    write_real_rtorrent_sidecar(&session_dir, &fx, &content_dir);

    let plan = dry_run_rtorrent_session(&session_dir).unwrap();
    let torrent = one(&plan);
    assert_eq!(
        torrent.resume_confidence,
        ResumeConfidence::Trusted,
        "warnings: {:?}",
        torrent.warnings
    );

    let fastresume_dir = tempfile::tempdir().unwrap();
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    rt_db::migrate(&conn).unwrap();
    plan.apply_native_import(
        &mut conn,
        fastresume_dir.path(),
        &ImportOptions::default(),
        ImportPolicy::TrustHints,
    )
    .unwrap();

    // Prove what actually matters: reconstruct exactly what the daemon
    // does at runtime (re-parse the persisted .torrent fresh, resolve
    // each file against the DB's save_path) and verify the bytes are the
    // real, untouched, correct ones - not just that the dry-run report
    // claims success.
    let row = rt_db::get(&conn, &fx.hash_hex).unwrap();
    let save_root = PathBuf::from(&row.save_path);
    let TorrentMeta::V1(meta) = parse_torrent(&fx.raw).unwrap() else {
        panic!("expected V1");
    };
    for (file, (_, expected_content)) in meta.files.iter().zip(&fx.files) {
        let resolved = file.path.resolve(&save_root);
        let actual = std::fs::read(&resolved)
            .unwrap_or_else(|e| panic!("daemon would fail to find {resolved:?}: {e}"));
        assert_eq!(
            &actual, expected_content,
            "file at {resolved:?} must be byte-identical to what was on disk before import"
        );
    }
}

#[test]
fn production_shape_bep47_padding_file_not_wanted_real_files_trusted() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = tmp.path().join("session");
    std::fs::create_dir_all(&session_dir).unwrap();
    let fx = multi_file_fixture("Padded Album", true);

    let content_dir = tmp.path().join("downloads").join(&fx.name);
    std::fs::create_dir_all(&content_dir).unwrap();
    for (rel, content) in &fx.files {
        std::fs::write(content_dir.join(rel), content).unwrap();
    }
    // Deliberately never write the padding file - matching every real
    // client (qBittorrent/rTorrent skip materializing it on disk).
    write_real_rtorrent_sidecar(&session_dir, &fx, &content_dir);

    let plan = dry_run_rtorrent_session(&session_dir).unwrap();
    let torrent = one(&plan);
    assert_eq!(
        torrent.resume_confidence,
        ResumeConfidence::Trusted,
        "a missing padding file must not prevent trusting the real files; warnings: {:?}",
        torrent.warnings
    );

    let fastresume_dir = tempfile::tempdir().unwrap();
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    rt_db::migrate(&conn).unwrap();
    plan.apply_native_import(
        &mut conn,
        fastresume_dir.path(),
        &ImportOptions::default(),
        ImportPolicy::TrustHints,
    )
    .unwrap();

    let files = rt_db::list_torrent_files(&conn, &fx.hash_hex).unwrap();
    assert_eq!(files.len(), 3, "2 real files + 1 padding entry");
    let pad_row = files
        .iter()
        .find(|f| f.path.ends_with(".pad/28"))
        .expect("padding file row present");
    assert!(
        !pad_row.wanted,
        "a BEP47 padding file must never be marked wanted"
    );
    let real_rows: Vec<_> = files.iter().filter(|f| f.path != pad_row.path).collect();
    assert_eq!(real_rows.len(), 2);
    assert!(real_rows.iter().all(|f| f.wanted));
}

#[test]
fn production_shape_directory_renamed_by_external_tool_stays_safe_metadata_only() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = tmp.path().join("session");
    std::fs::create_dir_all(&session_dir).unwrap();
    let fx = multi_file_fixture("Renamed Album", false);

    // An external tool (autobrr/cross-seed-style) renamed the content
    // folder without updating it to match the torrent's own name.
    let content_dir = tmp
        .path()
        .join("downloads")
        .join(format!("{}.__temp_owned_1", fx.name));
    std::fs::create_dir_all(&content_dir).unwrap();
    for (rel, content) in &fx.files {
        std::fs::write(content_dir.join(rel), content).unwrap();
    }
    write_real_rtorrent_sidecar(&session_dir, &fx, &content_dir);

    let plan = dry_run_rtorrent_session(&session_dir).unwrap();
    let torrent = one(&plan);
    assert_eq!(
        torrent.resume_confidence,
        ResumeConfidence::MetadataOnly,
        "must degrade safely (recheck needed) rather than claim Trusted with paths \
         that don't actually resolve"
    );
    assert_eq!(torrent.save_path.as_deref(), Some(content_dir.as_path()));

    let fastresume_dir = tempfile::tempdir().unwrap();
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    rt_db::migrate(&conn).unwrap();
    plan.apply_native_import(
        &mut conn,
        fastresume_dir.path(),
        &ImportOptions::default(),
        ImportPolicy::TrustHints,
    )
    .unwrap();

    // The real files must be completely untouched by the whole plan +
    // apply pipeline, even though this torrent's paths don't resolve.
    for (rel, expected_content) in &fx.files {
        let actual = std::fs::read(content_dir.join(rel)).unwrap();
        assert_eq!(
            &actual, expected_content,
            "apply must never touch real file bytes, even for a torrent it can't resolve"
        );
    }
}
