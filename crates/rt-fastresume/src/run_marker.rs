//! Run marker: tells the next start how the previous run ended.
//!
//! A small durable file in the session directory records, for the current run,
//! the OS boot identity, a start time and a heartbeat. A graceful shutdown
//! flips it to `graceful = true`. On the next start the marker classifies the
//! previous run (see [`PreviousRun`]):
//!
//! * `graceful` set: a clean shutdown.
//! * not graceful, **same** boot: only the process died. The page cache
//!   survived, so written data is intact.
//! * not graceful, **different** boot: the host crashed or lost power. Data
//!   that was never fsynced may be missing or zero-filled.
//! * not graceful, boot unknown (unsupported platform, or a corrupt marker):
//!   cannot tell, so callers must treat it as a possible host crash.
//! * no marker: first start, or an upgrade from a build without one. Handled
//!   exactly as before the marker existed.
//!
//! The marker is *not* trusted for anything positive. It only ever adds
//! scrutiny; a lost or stale marker degrades to the pre-existing per-torrent
//! fastresume checks.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const RUN_MARKER_FILE: &str = "run_marker.json";
const RUN_MARKER_VERSION: u32 = 1;
const MAX_RUN_MARKER_BYTES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunMarker {
    pub version: u32,
    /// Boot identity of the run that wrote this marker, when the platform
    /// provides one.
    pub boot_id: Option<String>,
    pub pid: u32,
    /// Unique per run (nanoseconds since the epoch at start). Stamped into
    /// every fastresume record the run writes so a record saved during the
    /// current run is never judged against the previous run's crash.
    pub run_id: u64,
    pub started_unix: u64,
    /// Refreshed periodically while the daemon runs; the best available
    /// estimate of when a crashed run died.
    pub heartbeat_unix: u64,
    /// Set only by an orderly shutdown after every torrent saved its state.
    pub graceful: bool,
}

/// Result of reading the marker from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerRead {
    Absent,
    Present(RunMarker),
    /// Unreadable or malformed. A torn write is what a crash during marker
    /// update looks like, so this is treated as unclean with unknown cause.
    Corrupt,
}

/// How the previous run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviousRun {
    /// No marker existed. Behaves as before this feature.
    NoRecord,
    Clean,
    /// Unclean, same boot: the OS (and its page cache) stayed up.
    ProcessCrash,
    /// Unclean, different boot: power loss, kernel panic or hard reset.
    HostCrash,
    /// Unclean, but the boot could not be compared.
    UncleanUnknownCause,
}

impl PreviousRun {
    pub fn was_unclean(self) -> bool {
        matches!(
            self,
            PreviousRun::ProcessCrash | PreviousRun::HostCrash | PreviousRun::UncleanUnknownCause
        )
    }

    /// Whether the page cache may have been lost. Unknown cause counts.
    pub fn may_be_host_crash(self) -> bool {
        matches!(
            self,
            PreviousRun::HostCrash | PreviousRun::UncleanUnknownCause
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PreviousRun::NoRecord => "no_record",
            PreviousRun::Clean => "clean",
            PreviousRun::ProcessCrash => "process_crash",
            PreviousRun::HostCrash => "host_crash",
            PreviousRun::UncleanUnknownCause => "unclean_unknown_cause",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviousRunAssessment {
    pub verdict: PreviousRun,
    /// Best estimate of when the previous run stopped (its last heartbeat),
    /// when known. `None` for clean or absent markers.
    pub crash_reference_unix: Option<u64>,
    pub previous_boot_id: Option<String>,
    pub current_boot_id: Option<String>,
    pub previous_pid: Option<u32>,
    pub previous_run_id: Option<u64>,
}

/// Classify the previous run from the marker and the current boot identity.
pub fn assess_previous_run(
    read: &MarkerRead,
    current_boot_id: Option<&str>,
) -> PreviousRunAssessment {
    let current_boot = current_boot_id.map(str::to_owned);
    match read {
        MarkerRead::Absent => PreviousRunAssessment {
            verdict: PreviousRun::NoRecord,
            crash_reference_unix: None,
            previous_boot_id: None,
            current_boot_id: current_boot,
            previous_pid: None,
            previous_run_id: None,
        },
        MarkerRead::Corrupt => PreviousRunAssessment {
            verdict: PreviousRun::UncleanUnknownCause,
            crash_reference_unix: None,
            previous_boot_id: None,
            current_boot_id: current_boot,
            previous_pid: None,
            previous_run_id: None,
        },
        MarkerRead::Present(marker) => {
            if marker.graceful {
                return PreviousRunAssessment {
                    verdict: PreviousRun::Clean,
                    crash_reference_unix: None,
                    previous_boot_id: marker.boot_id.clone(),
                    current_boot_id: current_boot,
                    previous_pid: Some(marker.pid),
                    previous_run_id: Some(marker.run_id),
                };
            }
            let verdict = match rt_storage::boot_changed(marker.boot_id.as_deref(), current_boot_id)
            {
                Some(true) => PreviousRun::HostCrash,
                Some(false) => PreviousRun::ProcessCrash,
                None => PreviousRun::UncleanUnknownCause,
            };
            PreviousRunAssessment {
                verdict,
                crash_reference_unix: Some(marker.heartbeat_unix.max(marker.started_unix)),
                previous_boot_id: marker.boot_id.clone(),
                current_boot_id: current_boot,
                previous_pid: Some(marker.pid),
                previous_run_id: Some(marker.run_id),
            }
        }
    }
}

/// Reads and durably writes the run marker in a session directory.
#[derive(Debug, Clone)]
pub struct RunMarkerStore {
    path: PathBuf,
}

impl RunMarkerStore {
    pub fn new(session_dir: impl AsRef<Path>) -> Self {
        RunMarkerStore {
            path: session_dir.as_ref().join(RUN_MARKER_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn read(&self) -> MarkerRead {
        let bytes = match rt_storage::read_file_no_follow_limited(&self.path, MAX_RUN_MARKER_BYTES)
        {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return MarkerRead::Absent,
            Err(_) => return MarkerRead::Corrupt,
        };
        match serde_json::from_slice::<RunMarker>(&bytes) {
            Ok(marker) if marker.version == RUN_MARKER_VERSION => MarkerRead::Present(marker),
            _ => MarkerRead::Corrupt,
        }
    }

    /// Record the start of a run. Must be durable before torrent state is
    /// touched, otherwise a crash right after startup would look like a clean
    /// shutdown of the *previous* run.
    pub fn begin_run(
        &self,
        boot_id: Option<&str>,
        pid: u32,
        run_id: u64,
        now_unix: u64,
    ) -> io::Result<RunMarker> {
        let marker = RunMarker {
            version: RUN_MARKER_VERSION,
            boot_id: boot_id.map(str::to_owned),
            pid,
            run_id,
            started_unix: now_unix,
            heartbeat_unix: now_unix,
            graceful: false,
        };
        self.write(&marker)?;
        Ok(marker)
    }

    /// Refresh the heartbeat of the current run.
    pub fn heartbeat(&self, marker: &mut RunMarker, now_unix: u64) -> io::Result<()> {
        marker.heartbeat_unix = now_unix.max(marker.heartbeat_unix);
        self.write(marker)
    }

    /// Mark the current run as cleanly finished. Call only after every torrent
    /// has saved its final state.
    pub fn end_run_gracefully(&self, marker: &mut RunMarker, now_unix: u64) -> io::Result<()> {
        marker.heartbeat_unix = now_unix.max(marker.heartbeat_unix);
        marker.graceful = true;
        self.write(marker)
    }

    /// Remove the marker. Used when crash detection is switched off so a stale
    /// unclean marker cannot raise a false alarm when it is switched back on.
    pub fn remove(&self) -> io::Result<()> {
        match rt_storage::remove_file_no_follow(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn write(&self, marker: &RunMarker) -> io::Result<()> {
        let bytes = serde_json::to_vec(marker).map_err(io::Error::other)?;
        if let Some(parent) = self.path.parent() {
            rt_storage::create_dir_all_no_follow(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        rt_storage::write_file_no_follow_sync(&tmp, &bytes)?;
        rt_storage::rename_no_follow(&tmp, &self.path)?;
        if let Some(parent) = self.path.parent() {
            rt_storage::sync_dir_no_follow(parent)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(boot: Option<&str>, graceful: bool) -> RunMarker {
        RunMarker {
            version: RUN_MARKER_VERSION,
            boot_id: boot.map(str::to_owned),
            pid: 42,
            run_id: 9,
            started_unix: 1_000,
            heartbeat_unix: 1_500,
            graceful,
        }
    }

    #[test]
    fn absent_marker_is_no_record_and_never_scrutinized() {
        let a = assess_previous_run(&MarkerRead::Absent, Some("boot-a"));
        assert_eq!(a.verdict, PreviousRun::NoRecord);
        assert!(!a.verdict.was_unclean());
        assert!(!a.verdict.may_be_host_crash());
        assert_eq!(a.crash_reference_unix, None);
    }

    #[test]
    fn graceful_marker_is_clean_even_across_a_reboot() {
        let read = MarkerRead::Present(marker(Some("boot-a"), true));
        let a = assess_previous_run(&read, Some("boot-b"));
        assert_eq!(a.verdict, PreviousRun::Clean);
        assert!(!a.verdict.may_be_host_crash());
    }

    #[test]
    fn unclean_same_boot_is_a_process_crash() {
        let read = MarkerRead::Present(marker(Some("boot-a"), false));
        let a = assess_previous_run(&read, Some("boot-a"));
        assert_eq!(a.verdict, PreviousRun::ProcessCrash);
        assert!(a.verdict.was_unclean());
        assert!(!a.verdict.may_be_host_crash());
        assert_eq!(a.crash_reference_unix, Some(1_500));
    }

    #[test]
    fn unclean_different_boot_is_a_host_crash() {
        let read = MarkerRead::Present(marker(Some("boot-a"), false));
        let a = assess_previous_run(&read, Some("boot-b"));
        assert_eq!(a.verdict, PreviousRun::HostCrash);
        assert!(a.verdict.may_be_host_crash());
        assert_eq!(a.previous_boot_id.as_deref(), Some("boot-a"));
        assert_eq!(a.current_boot_id.as_deref(), Some("boot-b"));
    }

    #[test]
    fn unclean_with_unknown_boot_on_either_side_cannot_rule_out_host_crash() {
        for (prev, cur) in [(None, Some("b")), (Some("a"), None), (None, None)] {
            let read = MarkerRead::Present(marker(prev, false));
            let a = assess_previous_run(&read, cur);
            assert_eq!(
                a.verdict,
                PreviousRun::UncleanUnknownCause,
                "{prev:?}/{cur:?}"
            );
            assert!(a.verdict.may_be_host_crash());
        }
    }

    #[test]
    fn corrupt_marker_is_unclean_with_unknown_cause() {
        let a = assess_previous_run(&MarkerRead::Corrupt, Some("boot-a"));
        assert_eq!(a.verdict, PreviousRun::UncleanUnknownCause);
        assert_eq!(a.crash_reference_unix, None);
    }

    #[test]
    fn crash_reference_prefers_the_later_of_heartbeat_and_start() {
        let mut m = marker(Some("a"), false);
        m.started_unix = 2_000;
        m.heartbeat_unix = 1_000; // clock stepped back
        let a = assess_previous_run(&MarkerRead::Present(m), Some("b"));
        assert_eq!(a.crash_reference_unix, Some(2_000));
    }

    #[test]
    fn store_round_trips_through_a_full_run_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let store = RunMarkerStore::new(dir.path());
        assert_eq!(store.read(), MarkerRead::Absent);

        let mut m = store.begin_run(Some("boot-a"), 7, 1, 100).unwrap();
        // A crash here leaves exactly this on disk.
        match store.read() {
            MarkerRead::Present(on_disk) => {
                assert!(!on_disk.graceful);
                assert_eq!(on_disk.boot_id.as_deref(), Some("boot-a"));
                assert_eq!(on_disk.started_unix, 100);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            assess_previous_run(&store.read(), Some("boot-b")).verdict,
            PreviousRun::HostCrash
        );

        store.heartbeat(&mut m, 160).unwrap();
        match store.read() {
            MarkerRead::Present(on_disk) => assert_eq!(on_disk.heartbeat_unix, 160),
            other => panic!("unexpected {other:?}"),
        }

        store.end_run_gracefully(&mut m, 200).unwrap();
        assert_eq!(
            assess_previous_run(&store.read(), Some("boot-b")).verdict,
            PreviousRun::Clean
        );
    }

    #[test]
    fn remove_deletes_the_marker_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = RunMarkerStore::new(dir.path());
        store.begin_run(Some("a"), 1, 3, 10).unwrap();
        assert!(matches!(store.read(), MarkerRead::Present(_)));
        store.remove().unwrap();
        assert_eq!(store.read(), MarkerRead::Absent);
        store.remove().unwrap();
    }

    #[test]
    fn assessment_reports_the_previous_runs_id() {
        let read = MarkerRead::Present(marker(Some("a"), false));
        let a = assess_previous_run(&read, Some("a"));
        assert_eq!(a.previous_run_id, Some(9));
    }

    #[test]
    fn heartbeat_never_moves_backwards() {
        let dir = tempfile::tempdir().unwrap();
        let store = RunMarkerStore::new(dir.path());
        let mut m = store.begin_run(None, 1, 2, 500).unwrap();
        store.heartbeat(&mut m, 100).unwrap();
        assert_eq!(m.heartbeat_unix, 500);
    }

    #[test]
    fn garbage_and_wrong_version_files_read_as_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let store = RunMarkerStore::new(dir.path());
        std::fs::write(store.path(), b"{ not json").unwrap();
        assert_eq!(store.read(), MarkerRead::Corrupt);

        let mut m = marker(Some("a"), true);
        m.version = 99;
        std::fs::write(store.path(), serde_json::to_vec(&m).unwrap()).unwrap();
        assert_eq!(store.read(), MarkerRead::Corrupt);

        std::fs::write(store.path(), vec![b'x'; MAX_RUN_MARKER_BYTES + 1]).unwrap();
        assert_eq!(store.read(), MarkerRead::Corrupt);
    }
}
