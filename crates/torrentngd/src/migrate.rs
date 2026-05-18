//! `torrentngd migrate` — import existing client state into the native engine.
//!
//! Dry-run is the default and is read-only against the source directory.
//! `--apply` writes native DB rows plus compatible fast-resume state so that
//! complete torrents resume seeding without a full recheck. The source client
//! state is never modified.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context};
use rt_config::Config;
use rt_fastresume::ImportPolicy;
use rt_migrate::{
    dry_run_biglybt_config_with_options, dry_run_deluge_state_with_options,
    dry_run_generic_torrent_directory_with_options, dry_run_qbittorrent_backup_with_options,
    dry_run_rtorrent_session_with_options, dry_run_tixati_config_with_options,
    dry_run_transmission_session_with_options, dry_run_utorrent_config_with_options, ImportOptions,
    MigrationPlan, MigrationSource, PathRemap,
};

const USAGE: &str = "\
torrentngd migrate — import existing client state into the native engine

USAGE:
    torrentngd migrate --source <SRC> --from <DIR> [OPTIONS]

REQUIRED:
    --source <SRC>     rtorrent | qbittorrent | transmission | deluge |
                       utorrent | biglybt | tixati | generic
    --from <DIR>       source client state directory (read only)

OPTIONS:
    --apply            perform the import (default: dry-run report only)
    --policy <P>       fast-resume trust policy: verify | trust-hints |
                       trust-all (default: trust-hints)
    --remap <OLD=NEW>  rewrite a save-path prefix; repeatable
                       (e.g. --remap /downloads=/data)
    --default-save-path <DIR>
                       fallback save path for torrents with none recorded
    --report <FILE>    also write the markdown dry-run report to FILE
    --config <FILE>    config file (else TORRENTNGD_CONFIG / defaults)
    --yes              skip the confirmation prompt with --apply
    -h, --help         show this help

Dry-run is read-only. With --apply, native DB rows and compatible
fast-resume state are written together; complete torrents whose data is
present resume without a full recheck.";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Migrate(MigrateArgs),
}

#[derive(Debug, PartialEq, Eq)]
pub struct MigrateArgs {
    pub source: MigrationSource,
    pub from: PathBuf,
    pub apply: bool,
    pub policy: ImportPolicy,
    pub remaps: Vec<PathRemap>,
    pub default_save_path: Option<PathBuf>,
    pub report: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub assume_yes: bool,
}

pub fn parse_source(value: &str) -> Option<MigrationSource> {
    Some(match value.to_ascii_lowercase().as_str() {
        "rtorrent" => MigrationSource::RTorrent,
        "qbittorrent" | "qbit" | "qb" => MigrationSource::QBittorrent,
        "transmission" => MigrationSource::Transmission,
        "deluge" => MigrationSource::Deluge,
        "utorrent" | "bittorrent" => MigrationSource::UTorrent,
        "biglybt" | "vuze" => MigrationSource::BiglyBT,
        "tixati" => MigrationSource::Tixati,
        "generic" => MigrationSource::Generic,
        _ => return None,
    })
}

pub fn parse_policy(value: &str) -> Option<ImportPolicy> {
    Some(match value.to_ascii_lowercase().as_str() {
        "verify" | "require-verification" => ImportPolicy::RequireVerification,
        "trust-hints" | "hints" => ImportPolicy::TrustHints,
        "trust-all" | "all" => ImportPolicy::TrustAll,
        _ => return None,
    })
}

pub fn parse_remap(value: &str) -> Result<PathRemap, String> {
    let (from, to) = value
        .split_once('=')
        .ok_or_else(|| format!("invalid --remap `{value}`; expected OLD=NEW"))?;
    if from.is_empty() || to.is_empty() {
        return Err(format!(
            "invalid --remap `{value}`; OLD and NEW must be set"
        ));
    }
    Ok(PathRemap::new(from, to))
}

pub fn parse_args(args: &[String]) -> Result<Command, String> {
    let mut source: Option<MigrationSource> = None;
    let mut from: Option<PathBuf> = None;
    let mut apply = false;
    let mut policy = ImportPolicy::TrustHints;
    let mut remaps = Vec::new();
    let mut default_save_path = None;
    let mut report = None;
    let mut config = None;
    let mut assume_yes = false;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = |flag: &str| {
            iter.next()
                .cloned()
                .ok_or_else(|| format!("{flag} requires a value"))
        };
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--source" => {
                let raw = value("--source")?;
                source =
                    Some(parse_source(&raw).ok_or_else(|| format!("unknown --source `{raw}`"))?);
            }
            "--from" => from = Some(PathBuf::from(value("--from")?)),
            "--apply" => apply = true,
            "--yes" | "-y" => assume_yes = true,
            "--policy" => {
                let raw = value("--policy")?;
                policy = parse_policy(&raw).ok_or_else(|| format!("unknown --policy `{raw}`"))?;
            }
            "--remap" => remaps.push(parse_remap(&value("--remap")?)?),
            "--default-save-path" => {
                default_save_path = Some(PathBuf::from(value("--default-save-path")?))
            }
            "--report" => report = Some(PathBuf::from(value("--report")?)),
            "--config" => config = Some(PathBuf::from(value("--config")?)),
            other => return Err(format!("unknown argument `{other}` (try --help)")),
        }
    }

    let source = source.ok_or_else(|| "--source is required (try --help)".to_owned())?;
    let from = from.ok_or_else(|| "--from is required (try --help)".to_owned())?;
    Ok(Command::Migrate(MigrateArgs {
        source,
        from,
        apply,
        policy,
        remaps,
        default_save_path,
        report,
        config,
        assume_yes,
    }))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn dry_run(
    source: MigrationSource,
    root: &Path,
    options: &ImportOptions,
) -> anyhow::Result<MigrationPlan> {
    let plan = match source {
        MigrationSource::RTorrent => dry_run_rtorrent_session_with_options(root, options),
        MigrationSource::QBittorrent => dry_run_qbittorrent_backup_with_options(root, options),
        MigrationSource::Transmission => dry_run_transmission_session_with_options(root, options),
        MigrationSource::Deluge => dry_run_deluge_state_with_options(root, options),
        MigrationSource::UTorrent => dry_run_utorrent_config_with_options(root, options),
        MigrationSource::BiglyBT => dry_run_biglybt_config_with_options(root, options),
        MigrationSource::Tixati => dry_run_tixati_config_with_options(root, options),
        MigrationSource::Generic => dry_run_generic_torrent_directory_with_options(root, options),
    }
    .with_context(|| format!("scanning {source:?} state at {}", root.display()))?;
    Ok(plan)
}

pub(crate) fn load_config(path: Option<&Path>) -> Config {
    if let Some(path) = path {
        match Config::load(path) {
            Ok(c) => return c,
            Err(e) => eprintln!("config error ({}): {e}", path.display()),
        }
    } else if let Ok(env_path) = std::env::var("TORRENTNGD_CONFIG") {
        match Config::load(Path::new(&env_path)) {
            Ok(c) => return c,
            Err(e) => eprintln!("config error ({env_path}): {e}"),
        }
    }
    Config::load_default()
}

pub(crate) fn confirm<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
    prompt: &str,
) -> io::Result<bool> {
    write!(writer, "{prompt}")?;
    writer.flush()?;
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(false);
    }
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "yes" | "y"
    ))
}

/// Entry point for `torrentngd migrate <args>`.
pub fn run(args: &[String]) -> anyhow::Result<()> {
    let command = match parse_args(args) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("error: {message}");
            bail!("invalid migrate arguments");
        }
    };

    let args = match command {
        Command::Help => {
            println!("{USAGE}");
            return Ok(());
        }
        Command::Migrate(args) => args,
    };

    if !args.from.is_dir() {
        bail!("--from {} is not a directory", args.from.display());
    }

    let options = ImportOptions {
        default_save_path: args.default_save_path.clone(),
        path_remaps: args.remaps.clone(),
        added_at: now_unix(),
    };

    let plan = dry_run(args.source, &args.from, &options)?;
    let report = plan.to_markdown();
    println!("{report}");

    if let Some(path) = &args.report {
        std::fs::write(path, &report)
            .with_context(|| format!("writing report to {}", path.display()))?;
        println!("Report written to {}", path.display());
    }

    let summary = plan.resume_confidence_summary();
    println!(
        "\n{} torrent(s): {} trusted, {} hints, {} metadata-only, {} none; {} warning(s)/skipped.",
        plan.torrent_count(),
        summary.trusted,
        summary.hints,
        summary.metadata_only,
        summary.none,
        plan.warning_count(),
    );

    if !args.apply {
        println!("\nDry-run only. Re-run with --apply to write native state.");
        return Ok(());
    }

    if plan.torrent_count() == 0 {
        println!("\nNothing to import.");
        return Ok(());
    }

    let config = load_config(args.config.as_deref());
    let db_path = config.db_path();
    let fastresume_dir = config.daemon.session_dir.join("fastresume");

    println!(
        "\nApply target:\n  database:   {}\n  fastresume: {}\n  policy:     {:?}",
        db_path.display(),
        fastresume_dir.display(),
        args.policy,
    );

    if !args.assume_yes {
        let stdin = io::stdin();
        if !confirm(
            stdin.lock(),
            io::stdout(),
            "Type 'yes' to apply this import: ",
        )
        .context("reading confirmation")?
        {
            println!("Aborted. No native state was written.");
            return Ok(());
        }
    }

    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::create_dir_all(&fastresume_dir)
        .with_context(|| format!("creating {}", fastresume_dir.display()))?;

    let mut conn = rusqlite::Connection::open(&db_path)
        .with_context(|| format!("opening database {}", db_path.display()))?;
    rt_db::migrate(&conn).context("migrating database schema")?;

    let result = plan
        .apply_native_import(&mut conn, &fastresume_dir, &options, args.policy)
        .map_err(|e| anyhow!("native import failed: {e}"))?;

    // Persist the .torrent metainfo into the engine blob dir so the native
    // state is complete: the daemon can load it, and `torrentngd export` can
    // project it back out without first running the daemon.
    let blob_dir = config.daemon.session_dir.join("torrents");
    std::fs::create_dir_all(&blob_dir)
        .with_context(|| format!("creating {}", blob_dir.display()))?;
    let mut blobs = 0usize;
    for torrent in &plan.torrents {
        let dest = blob_dir.join(format!("{}.torrent", torrent.info_hash));
        if std::fs::copy(&torrent.torrent_path, &dest).is_ok() {
            blobs += 1;
        }
    }

    println!(
        "\nImported {} torrent(s), {} file(s), {} tracker(s); {blobs} .torrent blob(s) persisted.",
        result.db.torrents, result.db.files, result.db.trackers,
    );
    println!(
        "Fast-resume: {} state(s) written, {} skipped ({} trusted, {} hints, {} metadata-only, {} none).",
        result.fastresume.states,
        result.fastresume.skipped,
        result.fastresume.confidence.trusted,
        result.fastresume.confidence.hints,
        result.fastresume.confidence.metadata_only,
        result.fastresume.confidence.none,
    );
    println!("Start torrentngd to resume; complete torrents skip the full recheck.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_minimal_dry_run() {
        let cmd = parse_args(&args(&["--source", "rtorrent", "--from", "/sess"])).unwrap();
        let Command::Migrate(a) = cmd else {
            panic!("expected migrate command");
        };
        assert_eq!(a.source, MigrationSource::RTorrent);
        assert_eq!(a.from, PathBuf::from("/sess"));
        assert!(!a.apply);
        assert!(!a.assume_yes);
        assert_eq!(a.policy, ImportPolicy::TrustHints);
    }

    #[test]
    fn parses_full_apply_invocation() {
        let cmd = parse_args(&args(&[
            "--source",
            "qbittorrent",
            "--from",
            "/bt",
            "--apply",
            "--yes",
            "--policy",
            "trust-all",
            "--remap",
            "/downloads=/data",
            "--remap",
            "/old=/new",
            "--default-save-path",
            "/data",
            "--report",
            "/tmp/r.md",
            "--config",
            "/etc/torrentngd/config.toml",
        ]))
        .unwrap();
        let Command::Migrate(a) = cmd else {
            panic!("expected migrate command");
        };
        assert_eq!(a.source, MigrationSource::QBittorrent);
        assert!(a.apply);
        assert!(a.assume_yes);
        assert_eq!(a.policy, ImportPolicy::TrustAll);
        assert_eq!(a.remaps.len(), 2);
        assert_eq!(a.remaps[0], PathRemap::new("/downloads", "/data"));
        assert_eq!(a.default_save_path, Some(PathBuf::from("/data")));
        assert_eq!(a.report, Some(PathBuf::from("/tmp/r.md")));
        assert_eq!(a.config, Some(PathBuf::from("/etc/torrentngd/config.toml")));
    }

    #[test]
    fn help_short_circuits() {
        assert_eq!(parse_args(&args(&["--help"])).unwrap(), Command::Help);
        assert_eq!(
            parse_args(&args(&["--source", "rtorrent", "-h"])).unwrap(),
            Command::Help
        );
    }

    #[test]
    fn rejects_missing_required_and_unknowns() {
        assert!(parse_args(&args(&["--from", "/x"])).is_err());
        assert!(parse_args(&args(&["--source", "rtorrent"])).is_err());
        assert!(parse_args(&args(&["--source", "nope", "--from", "/x"])).is_err());
        assert!(parse_args(&args(&["--bogus"])).is_err());
        assert!(parse_args(&args(&["--source"])).is_err());
    }

    #[test]
    fn source_and_policy_aliases() {
        assert_eq!(parse_source("QB"), Some(MigrationSource::QBittorrent));
        assert_eq!(parse_source("vuze"), Some(MigrationSource::BiglyBT));
        assert_eq!(parse_source("weird"), None);
        assert_eq!(
            parse_policy("verify"),
            Some(ImportPolicy::RequireVerification)
        );
        assert_eq!(parse_policy("hints"), Some(ImportPolicy::TrustHints));
        assert_eq!(parse_policy("nope"), None);
    }

    #[test]
    fn remap_parsing_validates_format() {
        assert_eq!(parse_remap("/a=/b").unwrap(), PathRemap::new("/a", "/b"));
        assert!(parse_remap("/a").is_err());
        assert!(parse_remap("=/b").is_err());
        assert!(parse_remap("/a=").is_err());
    }

    #[test]
    fn confirm_accepts_yes_only() {
        let mut out = Vec::new();
        let p = "Type 'yes' to apply this import: ";
        assert!(confirm(io::Cursor::new(b"yes\n"), &mut out, p).unwrap());
        assert!(confirm(io::Cursor::new(b"  Y \n"), &mut io::sink(), p).unwrap());
        assert!(!confirm(io::Cursor::new(b"no\n"), &mut io::sink(), p).unwrap());
        assert!(!confirm(io::Cursor::new(b""), &mut io::sink(), p).unwrap());
        assert!(String::from_utf8(out).unwrap().contains("Type 'yes'"));
    }

    #[test]
    fn dry_run_and_apply_wire_to_native_db() {
        let src = tempfile::tempdir().unwrap();
        // Empty source: exercises the full scan + apply wiring with a zero plan.
        let options = ImportOptions::default();
        let plan = dry_run(MigrationSource::Generic, src.path(), &options).unwrap();
        assert_eq!(plan.torrent_count(), 0);
        assert!(plan.to_markdown().contains("Migration Dry Run"));

        let db_dir = tempfile::tempdir().unwrap();
        let db_path = db_dir.path().join("state.db");
        let mut conn = rusqlite::Connection::open(&db_path).unwrap();
        rt_db::migrate(&conn).unwrap();
        let summary = plan
            .apply_native_import(
                &mut conn,
                db_dir.path().join("fastresume"),
                &options,
                ImportPolicy::TrustHints,
            )
            .unwrap();
        assert_eq!(summary.db.torrents, 0);
        assert!(db_path.exists());
    }
}
