//! `torrentngd export` — reverse migration: project native state back into
//! another client so a user can leave TorrentNG without losing seeding state.
//!
//! Dry-run is the default and is read-only against the native DB, persisted
//! `.torrent` blobs, and fast-resume state. `--apply` writes the target
//! client's layout under `--to`. The native state is never modified.

use std::io;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context};
use rt_migrate::export::{ExportFormat, ExportPlan};

use crate::migrate::{confirm, load_config};

const USAGE: &str = "\
torrentngd export — project native state back into another client

USAGE:
    torrentngd export --format <FMT> --to <DIR> [OPTIONS]

REQUIRED:
    --format <FMT>     generic | libtorrent (qbittorrent/deluge) |
                       transmission | rtorrent | utorrent | biglybt
    --to <DIR>         output directory for the exported layout

OPTIONS:
    --apply            write the export (default: dry-run report only)
    --report <FILE>    also write the markdown dry-run report to FILE
    --config <FILE>    config file (else TORRENTNGD_CONFIG / defaults);
                       locates the native DB, .torrent blobs, and fastresume
    --yes              skip the confirmation prompt with --apply
    -h, --help         show this help

The native state is read-only. The dry-run report and post-apply summary
break torrents into recheck-free / complete-only / metadata-only /
torrent-only so you can see how much seeding state survives the move.
Generic always works (copies .torrent files + a manifest; destination
rechecks).";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Export(ExportArgs),
}

#[derive(Debug, PartialEq, Eq)]
pub struct ExportArgs {
    pub format: ExportFormat,
    pub to: PathBuf,
    pub apply: bool,
    pub report: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub assume_yes: bool,
}

pub fn parse_args(args: &[String]) -> Result<Command, String> {
    let mut format: Option<ExportFormat> = None;
    let mut to: Option<PathBuf> = None;
    let mut apply = false;
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
            "--format" => {
                let raw = value("--format")?;
                format = Some(
                    ExportFormat::parse(&raw).ok_or_else(|| format!("unknown --format `{raw}`"))?,
                );
            }
            "--to" => to = Some(PathBuf::from(value("--to")?)),
            "--apply" => apply = true,
            "--yes" | "-y" => assume_yes = true,
            "--report" => report = Some(PathBuf::from(value("--report")?)),
            "--config" => config = Some(PathBuf::from(value("--config")?)),
            other => return Err(format!("unknown argument `{other}` (try --help)")),
        }
    }

    let format = format.ok_or_else(|| "--format is required (try --help)".to_owned())?;
    let to = to.ok_or_else(|| "--to is required (try --help)".to_owned())?;
    Ok(Command::Export(ExportArgs {
        format,
        to,
        apply,
        report,
        config,
        assume_yes,
    }))
}

/// Entry point for `torrentngd export <args>`.
pub fn run(args: &[String]) -> anyhow::Result<()> {
    let command = match parse_args(args) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("error: {message}");
            bail!("invalid export arguments");
        }
    };

    let args = match command {
        Command::Help => {
            println!("{USAGE}");
            return Ok(());
        }
        Command::Export(args) => args,
    };

    let config = load_config(args.config.as_deref())?;
    let db_path = config.db_path();
    let blob_dir = config.daemon.session_dir.join("torrents");
    let fastresume_dir = config.daemon.session_dir.join("fastresume");

    if !db_path.is_file() {
        bail!(
            "no native database at {} (is this the right --config?)",
            db_path.display()
        );
    }

    let plan = ExportPlan::new(args.format, &db_path, &blob_dir, &fastresume_dir)
        .map_err(|e| anyhow!("reading native state failed: {e}"))?;

    let report = plan.to_markdown();
    println!("{report}");
    if let Some(path) = &args.report {
        std::fs::write(path, &report)
            .with_context(|| format!("writing report to {}", path.display()))?;
        println!("Report written to {}", path.display());
    }

    let s = plan.fidelity_summary();
    println!(
        "\n{} torrent(s): {} recheck-free, {} complete-only, {} metadata-only, {} torrent-only; {} skipped.",
        plan.torrent_count(),
        s.recheck_free,
        s.complete_only,
        s.metadata_only,
        s.torrent_only,
        plan.skipped.len(),
    );

    if !args.apply {
        println!("\nDry-run only. Re-run with --apply to write the export.");
        return Ok(());
    }

    if plan.torrent_count() == 0 {
        println!("\nNothing to export.");
        return Ok(());
    }

    println!(
        "\nExport target:\n  format: {:?}\n  to:     {}\n  source: {} (read-only)",
        args.format,
        args.to.display(),
        db_path.display(),
    );

    if !args.assume_yes {
        let stdin = io::stdin();
        if !confirm(
            stdin.lock(),
            io::stdout(),
            "Type 'yes' to write this export: ",
        )
        .context("reading confirmation")?
        {
            println!("Aborted. Nothing was written.");
            return Ok(());
        }
    }

    let summary = plan
        .write(&args.to)
        .map_err(|e| anyhow!("writing export failed: {e}"))?;

    println!(
        "\nExported {} torrent(s), {} file(s) written to {}.",
        summary.torrents,
        summary.files_written,
        args.to.display(),
    );
    println!(
        "Fidelity: {} recheck-free, {} complete-only, {} metadata-only, {} torrent-only.",
        summary.fidelity.recheck_free,
        summary.fidelity.complete_only,
        summary.fidelity.metadata_only,
        summary.fidelity.torrent_only,
    );
    println!("Point the destination client at the exported layout to resume.");
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
        let cmd = parse_args(&args(&["--format", "qbittorrent", "--to", "/out"])).unwrap();
        let Command::Export(a) = cmd else {
            panic!("expected export command");
        };
        assert_eq!(a.format, ExportFormat::Libtorrent);
        assert_eq!(a.to, PathBuf::from("/out"));
        assert!(!a.apply);
        assert!(!a.assume_yes);
    }

    #[test]
    fn parses_full_apply_invocation() {
        let cmd = parse_args(&args(&[
            "--format",
            "transmission",
            "--to",
            "/out",
            "--apply",
            "--yes",
            "--report",
            "/tmp/r.md",
            "--config",
            "/etc/torrentngd/config.toml",
        ]))
        .unwrap();
        let Command::Export(a) = cmd else {
            panic!("expected export command");
        };
        assert_eq!(a.format, ExportFormat::Transmission);
        assert!(a.apply);
        assert!(a.assume_yes);
        assert_eq!(a.report, Some(PathBuf::from("/tmp/r.md")));
        assert_eq!(a.config, Some(PathBuf::from("/etc/torrentngd/config.toml")));
    }

    #[test]
    fn help_and_errors() {
        assert_eq!(parse_args(&args(&["--help"])).unwrap(), Command::Help);
        assert!(parse_args(&args(&["--to", "/x"])).is_err());
        assert!(parse_args(&args(&["--format", "qbit"])).is_err());
        assert!(parse_args(&args(&["--format", "nope", "--to", "/x"])).is_err());
        assert!(parse_args(&args(&["--bogus"])).is_err());
        assert!(parse_args(&args(&["--format"])).is_err());
    }
}
