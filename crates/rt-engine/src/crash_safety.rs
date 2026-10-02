//! Engine-wide crash-safety state.
//!
//! One [`CrashSafetyRuntime`] is shared by the engine actor, every torrent task
//! and the API layer. It owns:
//!
//! * the live, operator-toggleable [`CrashSafetyConfig`] (config-file defaults,
//!   optionally overridden at runtime and persisted by the engine);
//! * the run marker and the verdict on how the previous run ended;
//! * a cache of per-save-path mount durability classifications;
//! * counters and the report the API and UI display.
//!
//! The decision logic itself is pure and lives in `rt_fastresume::recovery`;
//! this module only supplies its inputs and records its outcomes. See
//! `docs/CRASH_SAFETY.md` for the operator-facing description.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use rt_config::{
    CompletionVerifyMode, CrashSafetyConfig, HostCrashRecovery, ResolvedCrashSafety,
    StructuralAuditMode,
};
use rt_fastresume::{
    assess_previous_run, AuditPolicy, MarkerRead, PreviousRun, PreviousRunAssessment,
    RecheckReason, RecoveryPolicy, RunMarker, RunMarkerStore,
};
use rt_storage::{DurabilityTrust, MountDurability};
use serde::Serialize;
use tracing::{info, warn};

/// Seconds between run-marker heartbeats.
pub const HEARTBEAT_INTERVAL_SECS: u64 = 60;
/// Most save roots whose classification is retained for the report.
const MAX_TRACKED_MOUNTS: usize = 256;

pub fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn unix_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64
}

/// The reverse of [`recovery_policy`], for reporting.
pub fn host_crash_recovery(policy: RecoveryPolicy) -> HostCrashRecovery {
    match policy {
        RecoveryPolicy::Watermark => HostCrashRecovery::Watermark,
        RecoveryPolicy::Recent => HostCrashRecovery::Recent,
        RecoveryPolicy::Full => HostCrashRecovery::Full,
    }
}

pub fn recovery_policy(mode: HostCrashRecovery) -> RecoveryPolicy {
    match mode {
        HostCrashRecovery::Watermark => RecoveryPolicy::Watermark,
        HostCrashRecovery::Recent => RecoveryPolicy::Recent,
        HostCrashRecovery::Full => RecoveryPolicy::Full,
    }
}

pub fn audit_policy(mode: StructuralAuditMode) -> AuditPolicy {
    match mode {
        StructuralAuditMode::Off => AuditPolicy::Off,
        StructuralAuditMode::OnUnclean => AuditPolicy::OnUnclean,
        StructuralAuditMode::Always => AuditPolicy::Always,
    }
}

struct LiveMarker {
    store: RunMarkerStore,
    marker: RunMarker,
}

#[derive(Default)]
struct Counters {
    recovery_rechecks: AtomicU64,
    rechecks_unsynced_state: AtomicU64,
    rechecks_full_policy: AtomicU64,
    rechecks_recent_write: AtomicU64,
    rechecks_weak_mount: AtomicU64,
    rechecks_unknown_mount: AtomicU64,
    audit_torrents: AtomicU64,
    audit_files_unsupported: AtomicU64,
    audit_pieces_downgraded: AtomicU64,
    completions_gated: AtomicU64,
    completions_released: AtomicU64,
    completion_gate_retries: AtomicU64,
    completion_sync_unsupported: AtomicU64,
    completion_verify_pieces: AtomicU64,
    completion_verify_failures: AtomicU64,
    integrity_regressions: AtomicU64,
    /// Gauge: torrents currently held by the completion gate.
    completions_pending: AtomicU64,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct CrashSafetyCounters {
    pub recovery_rechecks: u64,
    pub rechecks_unsynced_state: u64,
    pub rechecks_full_policy: u64,
    pub rechecks_recent_write: u64,
    pub rechecks_weak_mount: u64,
    pub rechecks_unknown_mount: u64,
    pub audit_torrents: u64,
    pub audit_files_unsupported: u64,
    pub audit_pieces_downgraded: u64,
    pub completions_gated: u64,
    pub completions_released: u64,
    pub completion_gate_retries: u64,
    pub completion_sync_unsupported: u64,
    pub completion_verify_pieces: u64,
    pub completion_verify_failures: u64,
    pub integrity_regressions: u64,
    pub completions_pending: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PlatformSupport {
    /// The OS exposes a boot identity, so host crashes can be told apart from
    /// process crashes.
    pub boot_identity: bool,
    /// Holes and unwritten extents can be read from the filesystem.
    pub allocation_audit: bool,
    /// Mount type and options can be read for durability classification.
    pub mount_probe: bool,
    /// The OS accepts a page-cache drop hint for read-back verification.
    pub page_cache_drop: bool,
}

impl PlatformSupport {
    pub fn detect() -> Self {
        PlatformSupport {
            boot_identity: rt_storage::current_boot_identity().is_some(),
            allocation_audit: rt_storage::allocation_audit_supported(),
            mount_probe: rt_storage::mount_probe_supported(),
            page_cache_drop: rt_storage::MountScheduler::page_cache_drop_supported(),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MountReport {
    pub path: String,
    pub trust: DurabilityTrust,
    pub fs_type: Option<String>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CrashSafetyReport {
    pub run_id: u64,
    pub run_started_unix: u64,
    /// Whether a run marker is being maintained for this run.
    pub detection_active: bool,
    pub previous_run: PreviousRunAssessment,
    pub counters: CrashSafetyCounters,
    pub mounts: Vec<MountReport>,
    pub platform: PlatformSupport,
}

pub struct CrashSafetyRuntime {
    defaults: CrashSafetyConfig,
    settings: RwLock<Arc<CrashSafetyConfig>>,
    overridden: AtomicBool,
    session_dir: Option<PathBuf>,
    assessment: PreviousRunAssessment,
    boot_id: Option<String>,
    run_id: u64,
    started_unix: u64,
    marker: Mutex<Option<LiveMarker>>,
    /// Raw (pre-override) probe results keyed by save root.
    mounts: Mutex<HashMap<PathBuf, MountDurability>>,
    counters: Counters,
}

impl CrashSafetyRuntime {
    /// Read the previous run's marker, classify it, and (when detection is
    /// enabled) durably record the start of this run. Blocking: performs a
    /// small fsync'd write. Never fails startup; a marker that cannot be
    /// written disables detection for this run and is logged.
    pub fn start(
        session_dir: &Path,
        defaults: CrashSafetyConfig,
        effective: CrashSafetyConfig,
        overridden: bool,
    ) -> Arc<Self> {
        let boot_id = rt_storage::current_boot_identity();
        let store = RunMarkerStore::new(session_dir);
        let started_unix = unix_seconds();
        let run_id = unix_nanos();

        let (assessment, live) = if effective.host_crash_detection {
            let read = store.read();
            let assessment = assess_previous_run(&read, boot_id.as_deref());
            match store.begin_run(boot_id.as_deref(), std::process::id(), run_id, started_unix) {
                Ok(marker) => (assessment, Some(LiveMarker { store, marker })),
                Err(error) => {
                    warn!(
                        component = "crash_safety",
                        operation = "begin_run",
                        result = "error",
                        error = %error,
                        "could not write the run marker; host-crash detection is off for this run"
                    );
                    (assessment, None)
                }
            }
        } else {
            // Detection off: leave no marker behind, so a stale unclean one
            // cannot raise a false alarm if it is switched back on later.
            if let Err(error) = store.remove() {
                warn!(
                    component = "crash_safety",
                    operation = "remove_marker",
                    result = "error",
                    error = %error,
                    "could not remove a stale run marker"
                );
            }
            (
                assess_previous_run(&MarkerRead::Absent, boot_id.as_deref()),
                None,
            )
        };

        match assessment.verdict {
            PreviousRun::Clean | PreviousRun::NoRecord => info!(
                component = "crash_safety",
                operation = "assess_previous_run",
                verdict = assessment.verdict.as_str(),
                "previous run ended cleanly or left no record"
            ),
            verdict => warn!(
                component = "crash_safety",
                operation = "assess_previous_run",
                verdict = verdict.as_str(),
                previous_boot = ?assessment.previous_boot_id,
                current_boot = ?assessment.current_boot_id,
                crash_reference_unix = ?assessment.crash_reference_unix,
                "previous run did not shut down cleanly; applying crash recovery policy"
            ),
        }

        Arc::new(CrashSafetyRuntime {
            defaults,
            settings: RwLock::new(Arc::new(effective)),
            overridden: AtomicBool::new(overridden),
            session_dir: Some(session_dir.to_path_buf()),
            assessment,
            boot_id,
            run_id,
            started_unix,
            marker: Mutex::new(live),
            mounts: Mutex::new(HashMap::new()),
            counters: Counters::default(),
        })
    }

    /// A runtime with default settings and no marker. Used by unit tests and
    /// as the pre-attachment default of a `TorrentTask`.
    pub fn inert() -> Arc<Self> {
        let defaults = CrashSafetyConfig::default();
        Arc::new(CrashSafetyRuntime {
            settings: RwLock::new(Arc::new(defaults.clone())),
            defaults,
            overridden: AtomicBool::new(false),
            session_dir: None,
            assessment: assess_previous_run(&MarkerRead::Absent, None),
            boot_id: None,
            run_id: 0,
            started_unix: unix_seconds(),
            marker: Mutex::new(None),
            mounts: Mutex::new(HashMap::new()),
            counters: Counters::default(),
        })
    }

    /// Like [`Self::inert`] but with explicit settings and a chosen verdict for
    /// the previous run. Test support for exercising recovery paths.
    #[cfg(test)]
    pub fn for_test(
        settings: CrashSafetyConfig,
        previous_run: PreviousRun,
        crash_reference_unix: Option<u64>,
        run_id: u64,
    ) -> Arc<Self> {
        let mut assessment = assess_previous_run(&MarkerRead::Absent, None);
        assessment.verdict = previous_run;
        assessment.crash_reference_unix = crash_reference_unix;
        Arc::new(CrashSafetyRuntime {
            defaults: CrashSafetyConfig::default(),
            settings: RwLock::new(Arc::new(settings)),
            overridden: AtomicBool::new(false),
            session_dir: None,
            assessment,
            boot_id: Some("test-boot".to_owned()),
            run_id,
            started_unix: unix_seconds(),
            marker: Mutex::new(None),
            mounts: Mutex::new(HashMap::new()),
            counters: Counters::default(),
        })
    }

    /// Current effective settings (cheap `Arc` clone).
    pub fn settings(&self) -> Arc<CrashSafetyConfig> {
        Arc::clone(
            &self
                .settings
                .read()
                .expect("crash safety settings poisoned"),
        )
    }

    pub fn defaults(&self) -> &CrashSafetyConfig {
        &self.defaults
    }

    pub fn is_overridden(&self) -> bool {
        self.overridden.load(Ordering::Relaxed)
    }

    /// Replace the effective settings. `overridden` is true when the values
    /// come from a runtime change rather than the config file.
    ///
    /// Switching `host_crash_detection` on starts a marker for the rest of
    /// this run; switching it off removes the marker. The verdict on the
    /// *previous* run, computed at startup, is unaffected either way.
    pub fn apply_settings(&self, new: CrashSafetyConfig, overridden: bool) {
        let detection = new.host_crash_detection;
        *self
            .settings
            .write()
            .expect("crash safety settings poisoned") = Arc::new(new);
        self.overridden.store(overridden, Ordering::Relaxed);
        let Some(session_dir) = &self.session_dir else {
            return;
        };
        let mut live = self.marker.lock().expect("run marker poisoned");
        match (detection, live.is_some()) {
            (true, false) => {
                let store = RunMarkerStore::new(session_dir);
                match store.begin_run(
                    self.boot_id.as_deref(),
                    std::process::id(),
                    self.run_id,
                    self.started_unix,
                ) {
                    Ok(marker) => *live = Some(LiveMarker { store, marker }),
                    Err(error) => warn!(
                        component = "crash_safety",
                        operation = "begin_run",
                        result = "error",
                        error = %error,
                        "could not start the run marker after enabling detection"
                    ),
                }
            }
            (false, true) => {
                if let Some(old) = live.take() {
                    if let Err(error) = old.store.remove() {
                        warn!(
                            component = "crash_safety",
                            operation = "remove_marker",
                            result = "error",
                            error = %error,
                            "could not remove the run marker after disabling detection"
                        );
                    }
                }
            }
            _ => {}
        }
    }

    pub fn run_id(&self) -> u64 {
        self.run_id
    }

    pub fn boot_id(&self) -> Option<&str> {
        self.boot_id.as_deref()
    }

    pub fn assessment(&self) -> &PreviousRunAssessment {
        &self.assessment
    }

    /// How the previous run ended, as it should be used for decisions: with
    /// detection switched off the answer is always `NoRecord`.
    pub fn previous_run(&self) -> PreviousRun {
        if self.settings().host_crash_detection {
            self.assessment.verdict
        } else {
            PreviousRun::NoRecord
        }
    }

    /// Refresh the marker heartbeat. Blocking; call from a blocking context.
    pub fn heartbeat(&self) {
        let mut live = self.marker.lock().expect("run marker poisoned");
        if let Some(live) = live.as_mut() {
            if let Err(error) = live.store.heartbeat(&mut live.marker, unix_seconds()) {
                warn!(
                    component = "crash_safety",
                    operation = "heartbeat",
                    result = "error",
                    error = %error,
                    "could not refresh the run marker"
                );
            }
        }
    }

    /// Mark this run as cleanly finished. Call only after every torrent task
    /// has saved its final state within the shutdown deadline. Blocking.
    pub fn end_run_gracefully(&self) {
        let mut live = self.marker.lock().expect("run marker poisoned");
        if let Some(live) = live.as_mut() {
            match live
                .store
                .end_run_gracefully(&mut live.marker, unix_seconds())
            {
                Ok(()) => info!(
                    component = "crash_safety",
                    operation = "end_run",
                    result = "ok",
                    "run marker closed gracefully"
                ),
                Err(error) => warn!(
                    component = "crash_safety",
                    operation = "end_run",
                    result = "error",
                    error = %error,
                    "could not close the run marker; the next start will treat this run as unclean"
                ),
            }
        }
    }

    /// Effective durability classification of `save_root`: the probe result
    /// (cached), or `Strong` with an explanatory reason when probing is off,
    /// then the operator's per-path overrides.
    pub fn mount_durability(&self, save_root: &Path) -> MountDurability {
        self.classify_mount(&self.settings(), save_root, true)
    }

    /// `cache` controls whether a fresh probe result is remembered (and shown
    /// in the report). Real save roots are cached; paths merely being
    /// inspected through the API are not, so they cannot crowd them out.
    fn classify_mount(
        &self,
        settings: &CrashSafetyConfig,
        path: &Path,
        cache: bool,
    ) -> MountDurability {
        let probed = if !settings.mount_probe {
            MountDurability {
                trust: DurabilityTrust::Strong,
                fs_type: None,
                reasons: vec!["mount probing is disabled".to_owned()],
            }
        } else if cache {
            let mut mounts = self.mounts.lock().expect("mount cache poisoned");
            if let Some(hit) = mounts.get(path) {
                hit.clone()
            } else {
                let probed = rt_storage::probe_mount_durability(path);
                if mounts.len() < MAX_TRACKED_MOUNTS {
                    mounts.insert(path.to_path_buf(), probed.clone());
                }
                probed
            }
        } else {
            // An already-classified root keeps its cached rating, including a
            // downgrade learned from a failed fsync.
            let cached = self
                .mounts
                .lock()
                .expect("mount cache poisoned")
                .get(path)
                .cloned();
            cached.unwrap_or_else(|| rt_storage::probe_mount_durability(path))
        };
        rt_storage::apply_durability_overrides(
            probed,
            path,
            &settings.weak_mount_paths,
            &settings.strong_mount_paths,
        )
    }

    /// The settings that apply to torrents saved under `save_root`: the global
    /// values with any matching [`rt_config::PathPolicy`] applied, the mount's
    /// durability rating, and the read-back mode after weak-mount escalation.
    pub fn for_location(&self, save_root: &Path) -> LocationSettings {
        let settings = self.settings();
        let (resolved, matched) = settings.resolve_for(save_root);
        let trust = self.classify_mount(&settings, save_root, true).trust;
        LocationSettings {
            resolved,
            matched_policy: matched.map(|policy| policy.path.clone()),
            trust,
            completion_verify: effective_completion_verify(
                resolved.completion_verify,
                settings.weak_mount_escalation,
                trust,
            ),
        }
    }

    /// What `path` would get, for the "check a location" view. Does not add
    /// the path to the mount cache.
    pub fn describe_path(&self, path: &Path) -> PathReport {
        let settings = self.settings();
        let (resolved, matched) = settings.resolve_for(path);
        let mount = self.classify_mount(&settings, path, false);
        let (recovery, escalation) = rt_fastresume::escalate_policy(
            recovery_policy(resolved.host_crash_recovery),
            mount.trust,
            settings.weak_mount_escalation,
        );
        PathReport {
            path: path.display().to_string(),
            matched_policy: matched.map(|policy| policy.path.display().to_string()),
            completion_gate: resolved.completion_gate,
            host_crash_recovery: resolved.host_crash_recovery,
            host_crash_recovery_effective: host_crash_recovery(recovery),
            recovery_escalated_by: escalation.map(|reason| reason.as_str().to_owned()),
            structural_audit: resolved.structural_audit,
            completion_verify: resolved.completion_verify,
            completion_verify_effective: effective_completion_verify(
                resolved.completion_verify,
                settings.weak_mount_escalation,
                mount.trust,
            ),
            completion_verify_sample_percent: resolved.completion_verify_sample_percent,
            mount: MountReport {
                path: path.display().to_string(),
                trust: mount.trust,
                fs_type: mount.fs_type,
                reasons: mount.reasons,
            },
        }
    }

    /// A completion-gate sync failed in a way that shows this mount cannot
    /// honor fsync at all (`ENOSYS`, `EINVAL`, ...). Remember it so the report
    /// and future recovery treat the mount as weak.
    pub fn note_fsync_unsupported(&self, save_root: &Path) {
        self.counters
            .completion_sync_unsupported
            .fetch_add(1, Ordering::Relaxed);
        let mut mounts = self.mounts.lock().expect("mount cache poisoned");
        let entry = mounts
            .entry(save_root.to_path_buf())
            .or_insert_with(|| rt_storage::probe_mount_durability(save_root));
        const REASON: &str = "fsync failed with an \"unsupported\" error on this mount";
        entry.trust = DurabilityTrust::Weak;
        if !entry.reasons.iter().any(|reason| reason == REASON) {
            entry.reasons.insert(0, REASON.to_owned());
        }
    }

    pub fn record_recovery_recheck(&self, reason: RecheckReason) {
        self.counters
            .recovery_rechecks
            .fetch_add(1, Ordering::Relaxed);
        let counter = match reason {
            RecheckReason::UnsyncedStateAfterHostCrash => &self.counters.rechecks_unsynced_state,
            RecheckReason::FullPolicy => &self.counters.rechecks_full_policy,
            RecheckReason::RecentWrite => &self.counters.rechecks_recent_write,
            RecheckReason::WeakMount => &self.counters.rechecks_weak_mount,
            RecheckReason::UnknownMountRecentWrite => &self.counters.rechecks_unknown_mount,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_audit(&self, pieces_downgraded: u64, files_unsupported: u64) {
        self.counters.audit_torrents.fetch_add(1, Ordering::Relaxed);
        self.counters
            .audit_pieces_downgraded
            .fetch_add(pieces_downgraded, Ordering::Relaxed);
        self.counters
            .audit_files_unsupported
            .fetch_add(files_unsupported, Ordering::Relaxed);
    }

    pub fn record_gate_started(&self) {
        self.counters
            .completions_gated
            .fetch_add(1, Ordering::Relaxed);
        self.counters
            .completions_pending
            .fetch_add(1, Ordering::Relaxed);
    }

    /// The gate left `pending` without being released (torrent reverted to
    /// downloading, was rechecked, or was removed).
    pub fn record_gate_dropped(&self) {
        let pending = &self.counters.completions_pending;
        let mut current = pending.load(Ordering::Relaxed);
        while current > 0 {
            match pending.compare_exchange_weak(
                current,
                current - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }

    pub fn record_gate_released(&self) {
        self.counters
            .completions_released
            .fetch_add(1, Ordering::Relaxed);
        self.record_gate_dropped();
    }

    pub fn record_gate_retry(&self) {
        self.counters
            .completion_gate_retries
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_completion_verify(&self, pieces: u64, failures: u64) {
        self.counters
            .completion_verify_pieces
            .fetch_add(pieces, Ordering::Relaxed);
        self.counters
            .completion_verify_failures
            .fetch_add(failures, Ordering::Relaxed);
    }

    pub fn record_integrity_regression(&self) {
        self.counters
            .integrity_regressions
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn counters(&self) -> CrashSafetyCounters {
        let c = &self.counters;
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        CrashSafetyCounters {
            recovery_rechecks: get(&c.recovery_rechecks),
            rechecks_unsynced_state: get(&c.rechecks_unsynced_state),
            rechecks_full_policy: get(&c.rechecks_full_policy),
            rechecks_recent_write: get(&c.rechecks_recent_write),
            rechecks_weak_mount: get(&c.rechecks_weak_mount),
            rechecks_unknown_mount: get(&c.rechecks_unknown_mount),
            audit_torrents: get(&c.audit_torrents),
            audit_files_unsupported: get(&c.audit_files_unsupported),
            audit_pieces_downgraded: get(&c.audit_pieces_downgraded),
            completions_gated: get(&c.completions_gated),
            completions_released: get(&c.completions_released),
            completion_gate_retries: get(&c.completion_gate_retries),
            completion_sync_unsupported: get(&c.completion_sync_unsupported),
            completion_verify_pieces: get(&c.completion_verify_pieces),
            completion_verify_failures: get(&c.completion_verify_failures),
            integrity_regressions: get(&c.integrity_regressions),
            completions_pending: get(&c.completions_pending),
        }
    }

    pub fn report(&self) -> CrashSafetyReport {
        let settings = self.settings();
        let mut mounts: Vec<MountReport> = {
            let cache = self.mounts.lock().expect("mount cache poisoned");
            cache
                .keys()
                .map(|path| path.to_path_buf())
                .collect::<Vec<_>>()
        }
        .into_iter()
        .map(|path| {
            let effective = self.mount_durability(&path);
            MountReport {
                path: path.display().to_string(),
                trust: effective.trust,
                fs_type: effective.fs_type,
                reasons: effective.reasons,
            }
        })
        .collect();
        mounts.sort_by(|a, b| a.path.cmp(&b.path));
        CrashSafetyReport {
            run_id: self.run_id,
            run_started_unix: self.started_unix,
            detection_active: settings.host_crash_detection
                && self.marker.lock().expect("run marker poisoned").is_some(),
            previous_run: self.assessment.clone(),
            counters: self.counters(),
            mounts,
            platform: PlatformSupport::detect(),
        }
    }
}

/// Key under which a runtime override of the crash-safety settings is stored
/// in the `settings` table. Absent means "use the config file".
pub const SETTING_CRASH_SAFETY: &str = "crash_safety.v1";

/// Load a persisted runtime override, falling back to the config-file values.
/// A stored value that no longer parses or validates is ignored with a
/// warning: a bad row must never keep the daemon from starting.
pub fn load_persisted_settings(
    conn: &rusqlite::Connection,
    from_config: &CrashSafetyConfig,
) -> (CrashSafetyConfig, bool) {
    let stored = match rt_db::get_setting(conn, SETTING_CRASH_SAFETY) {
        Ok(value) => value,
        Err(rt_db::DbError::NotFound(_)) => return (from_config.clone(), false),
        Err(error) => {
            warn!(
                component = "crash_safety",
                operation = "load_settings",
                result = "ignored",
                error = %error,
                "could not read persisted crash-safety settings; using config-file values"
            );
            return (from_config.clone(), false);
        }
    };
    match serde_json::from_str::<CrashSafetyConfig>(&stored) {
        Ok(parsed) => match parsed.validate() {
            Ok(()) => (parsed, true),
            Err(error) => {
                warn!(
                    component = "crash_safety",
                    operation = "load_settings",
                    result = "ignored",
                    error = %error,
                    "persisted crash-safety settings are invalid; using config-file values"
                );
                (from_config.clone(), false)
            }
        },
        Err(error) => {
            warn!(
                component = "crash_safety",
                operation = "load_settings",
                result = "ignored",
                error = %error,
                "persisted crash-safety settings are malformed; using config-file values"
            );
            (from_config.clone(), false)
        }
    }
}

/// Everything the settings API and UI need in one response.
#[derive(Debug, Clone, Serialize)]
pub struct CrashSafetyView {
    /// The settings in effect right now.
    pub settings: CrashSafetyConfig,
    /// What the config file specifies (the target of "reset").
    pub defaults: CrashSafetyConfig,
    /// True when `settings` come from a runtime change rather than the file.
    pub overridden: bool,
    pub report: CrashSafetyReport,
}

impl CrashSafetyRuntime {
    pub fn view(&self) -> CrashSafetyView {
        CrashSafetyView {
            settings: (*self.settings()).clone(),
            defaults: self.defaults().clone(),
            overridden: self.is_overridden(),
            report: self.report(),
        }
    }
}

/// Settings in force for one save location. See
/// [`CrashSafetyRuntime::for_location`].
#[derive(Debug, Clone)]
pub struct LocationSettings {
    pub resolved: ResolvedCrashSafety,
    /// The path policy that matched, if any.
    pub matched_policy: Option<PathBuf>,
    pub trust: DurabilityTrust,
    /// Read-back mode after weak-mount escalation.
    pub completion_verify: CompletionVerifyMode,
}

/// What a location would get, for the API's "check a location" view.
#[derive(Debug, Clone, Serialize)]
pub struct PathReport {
    pub path: String,
    /// The path policy that matched, if any.
    pub matched_policy: Option<String>,
    pub completion_gate: bool,
    /// The configured recovery policy for this location.
    pub host_crash_recovery: HostCrashRecovery,
    /// The policy actually applied after weak/unknown-mount escalation.
    pub host_crash_recovery_effective: HostCrashRecovery,
    /// Why escalation raised it (`weak_mount`, `unknown_mount_recent_write`).
    pub recovery_escalated_by: Option<String>,
    pub structural_audit: StructuralAuditMode,
    pub completion_verify: CompletionVerifyMode,
    /// The read-back mode actually applied after weak-mount escalation.
    pub completion_verify_effective: CompletionVerifyMode,
    pub completion_verify_sample_percent: u8,
    pub mount: MountReport,
}

/// Effective completion-verify mode for a location: the configured mode,
/// raised to at least `Sample` on a mount classified weak when escalation is
/// enabled, so a mount that cannot honor fsync gets a read-back check even if
/// the operator never turned one on.
pub fn effective_completion_verify(
    configured: CompletionVerifyMode,
    escalation_enabled: bool,
    trust: DurabilityTrust,
) -> CompletionVerifyMode {
    if escalation_enabled
        && trust == DurabilityTrust::Weak
        && configured == CompletionVerifyMode::Off
    {
        CompletionVerifyMode::Sample
    } else {
        configured
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detection(on: bool) -> CrashSafetyConfig {
        CrashSafetyConfig {
            host_crash_detection: on,
            ..CrashSafetyConfig::default()
        }
    }

    #[test]
    fn first_start_records_a_marker_and_reports_no_record() {
        let dir = tempfile::tempdir().unwrap();
        let rt = CrashSafetyRuntime::start(
            dir.path(),
            CrashSafetyConfig::default(),
            CrashSafetyConfig::default(),
            false,
        );
        assert_eq!(rt.previous_run(), PreviousRun::NoRecord);
        assert!(rt.report().detection_active);
        assert!(dir.path().join(rt_fastresume::RUN_MARKER_FILE).exists());
    }

    #[test]
    fn ungraceful_exit_is_detected_on_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        // Run 1 starts and never ends (kill -9 / power cut).
        let first = CrashSafetyRuntime::start(
            dir.path(),
            CrashSafetyConfig::default(),
            CrashSafetyConfig::default(),
            false,
        );
        drop(first);
        let second = CrashSafetyRuntime::start(
            dir.path(),
            CrashSafetyConfig::default(),
            CrashSafetyConfig::default(),
            false,
        );
        // Same process, same boot on Linux => a process crash. On platforms
        // without a boot identity the cause is unknowable.
        let verdict = second.previous_run();
        if rt_storage::current_boot_identity().is_some() {
            assert_eq!(verdict, PreviousRun::ProcessCrash);
        } else {
            assert_eq!(verdict, PreviousRun::UncleanUnknownCause);
        }
        assert!(second.assessment().previous_run_id.is_some());
    }

    #[test]
    fn graceful_shutdown_is_clean_on_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let first = CrashSafetyRuntime::start(
            dir.path(),
            CrashSafetyConfig::default(),
            CrashSafetyConfig::default(),
            false,
        );
        first.heartbeat();
        first.end_run_gracefully();
        let second = CrashSafetyRuntime::start(
            dir.path(),
            CrashSafetyConfig::default(),
            CrashSafetyConfig::default(),
            false,
        );
        assert_eq!(second.previous_run(), PreviousRun::Clean);
    }

    #[test]
    fn a_marker_from_another_boot_is_a_host_crash() {
        let dir = tempfile::tempdir().unwrap();
        let store = RunMarkerStore::new(dir.path());
        // Simulate a previous run on a different boot that never finished.
        store
            .begin_run(Some("a-different-boot"), 1, 5, unix_seconds() - 100)
            .unwrap();
        let rt = CrashSafetyRuntime::start(
            dir.path(),
            CrashSafetyConfig::default(),
            CrashSafetyConfig::default(),
            false,
        );
        if rt_storage::current_boot_identity().is_some() {
            assert_eq!(rt.previous_run(), PreviousRun::HostCrash);
            assert!(rt.assessment().crash_reference_unix.is_some());
        }
        assert!(rt.previous_run().may_be_host_crash());
    }

    #[test]
    fn detection_off_leaves_no_marker_and_ignores_history() {
        let dir = tempfile::tempdir().unwrap();
        let store = RunMarkerStore::new(dir.path());
        store.begin_run(Some("old-boot"), 1, 5, 10).unwrap();
        let rt = CrashSafetyRuntime::start(
            dir.path(),
            CrashSafetyConfig::default(),
            detection(false),
            true,
        );
        assert_eq!(rt.previous_run(), PreviousRun::NoRecord);
        assert!(!rt.report().detection_active);
        assert!(!dir.path().join(rt_fastresume::RUN_MARKER_FILE).exists());
    }

    #[test]
    fn toggling_detection_at_runtime_starts_and_removes_the_marker() {
        let dir = tempfile::tempdir().unwrap();
        let rt = CrashSafetyRuntime::start(
            dir.path(),
            CrashSafetyConfig::default(),
            detection(false),
            false,
        );
        let marker = dir.path().join(rt_fastresume::RUN_MARKER_FILE);
        assert!(!marker.exists());
        rt.apply_settings(detection(true), true);
        assert!(marker.exists());
        assert!(rt.is_overridden());
        assert!(rt.report().detection_active);
        rt.apply_settings(detection(false), true);
        assert!(!marker.exists());
        assert_eq!(rt.previous_run(), PreviousRun::NoRecord);
    }

    #[test]
    fn mount_probe_off_reports_strong_but_overrides_still_apply() {
        let rt = CrashSafetyRuntime::inert();
        let settings = CrashSafetyConfig {
            mount_probe: false,
            weak_mount_paths: vec![PathBuf::from("/mnt/pool")],
            ..CrashSafetyConfig::default()
        };
        rt.apply_settings(settings, true);

        let normal = rt.mount_durability(Path::new("/srv/data"));
        assert_eq!(normal.trust, DurabilityTrust::Strong);
        assert!(normal.reasons[0].contains("disabled"));

        let overridden = rt.mount_durability(Path::new("/mnt/pool/x"));
        assert_eq!(overridden.trust, DurabilityTrust::Weak);
        assert!(overridden.reasons[0].contains("operator override"));
    }

    #[test]
    fn unsupported_fsync_downgrades_the_cached_mount_to_weak() {
        let dir = tempfile::tempdir().unwrap();
        let rt = CrashSafetyRuntime::inert();
        let before = rt.mount_durability(dir.path());
        rt.note_fsync_unsupported(dir.path());
        let after = rt.mount_durability(dir.path());
        assert_eq!(after.trust, DurabilityTrust::Weak);
        assert!(after.reasons[0].contains("fsync"));
        assert_eq!(rt.counters().completion_sync_unsupported, 1);
        // The report reflects the same downgrade.
        let report = rt.report();
        assert!(report.mounts.iter().any(
            |m| m.path == dir.path().display().to_string() && m.trust == DurabilityTrust::Weak
        ));
        let _ = before;
    }

    fn policy(path: &str) -> rt_config::PathPolicy {
        rt_config::PathPolicy {
            path: PathBuf::from(path),
            ..rt_config::PathPolicy::default()
        }
    }

    #[test]
    fn describe_path_reports_the_matching_policy_and_what_escalation_did() {
        let runtime = CrashSafetyRuntime::inert();
        let settings = CrashSafetyConfig {
            host_crash_recovery: HostCrashRecovery::Watermark,
            completion_verify: CompletionVerifyMode::Off,
            weak_mount_escalation: true,
            weak_mount_paths: vec![PathBuf::from("/mnt/nas")],
            path_policies: vec![
                rt_config::PathPolicy {
                    completion_gate: Some(false),
                    ..policy("/mnt")
                },
                rt_config::PathPolicy {
                    completion_verify_sample_percent: Some(40),
                    ..policy("/mnt/nas")
                },
            ],
            ..CrashSafetyConfig::default()
        };
        runtime.apply_settings(settings, true);

        let report = runtime.describe_path(Path::new("/mnt/nas/tv"));
        assert_eq!(report.matched_policy.as_deref(), Some("/mnt/nas"));
        assert_eq!(report.mount.trust, DurabilityTrust::Weak);
        assert!(
            report.completion_gate,
            "the longer policy wins outright and does not inherit its parent's overrides"
        );
        assert_eq!(report.completion_verify_sample_percent, 40);
        assert_eq!(report.host_crash_recovery, HostCrashRecovery::Watermark);
        assert_eq!(
            report.host_crash_recovery_effective,
            HostCrashRecovery::Full
        );
        assert_eq!(report.recovery_escalated_by.as_deref(), Some("weak_mount"));
        assert_eq!(report.completion_verify, CompletionVerifyMode::Off);
        assert_eq!(
            report.completion_verify_effective,
            CompletionVerifyMode::Sample
        );

        let parent = runtime.describe_path(Path::new("/mnt/other"));
        assert_eq!(parent.matched_policy.as_deref(), Some("/mnt"));
        assert!(!parent.completion_gate);

        let none = runtime.describe_path(Path::new("/srv/library"));
        assert_eq!(none.matched_policy, None);
        assert!(none.completion_gate);
    }

    #[test]
    fn describe_path_does_not_fill_the_tracked_mount_cache() {
        let runtime = CrashSafetyRuntime::inert();
        let dir = tempfile::tempdir().unwrap();
        for i in 0..8 {
            let _ = runtime.describe_path(&dir.path().join(format!("probe{i}")));
        }
        assert!(
            runtime.report().mounts.is_empty(),
            "only real save roots are tracked"
        );
        // Real save roots still are.
        let _ = runtime.for_location(dir.path());
        assert_eq!(runtime.report().mounts.len(), 1);
    }

    #[test]
    fn describe_path_honors_a_downgrade_learned_from_a_failed_fsync() {
        let runtime = CrashSafetyRuntime::inert();
        let dir = tempfile::tempdir().unwrap();
        runtime.note_fsync_unsupported(dir.path());
        let report = runtime.describe_path(dir.path());
        assert_eq!(report.mount.trust, DurabilityTrust::Weak);
        assert!(report.mount.reasons[0].contains("fsync"));
    }

    #[test]
    fn for_location_applies_the_path_policy_and_weak_mount_escalation() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = CrashSafetyRuntime::inert();
        let settings = CrashSafetyConfig {
            completion_verify: CompletionVerifyMode::Off,
            weak_mount_escalation: true,
            weak_mount_paths: vec![dir.path().to_path_buf()],
            path_policies: vec![rt_config::PathPolicy {
                completion_verify: Some(CompletionVerifyMode::Full),
                ..policy(dir.path().to_str().unwrap())
            }],
            ..CrashSafetyConfig::default()
        };
        runtime.apply_settings(settings, true);
        let location = runtime.for_location(dir.path());
        assert_eq!(location.matched_policy.as_deref(), Some(dir.path()));
        assert_eq!(location.trust, DurabilityTrust::Weak);
        assert_eq!(
            location.completion_verify,
            CompletionVerifyMode::Full,
            "an explicit choice is never lowered by escalation"
        );
    }

    #[test]
    fn persisted_override_wins_and_bad_rows_are_ignored() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let file = CrashSafetyConfig::default();

        // No row: config file values, not overridden.
        let (settings, overridden) = load_persisted_settings(&conn, &file);
        assert_eq!(settings, file);
        assert!(!overridden);

        // A valid row wins.
        let mut chosen = file.clone();
        chosen.host_crash_recovery = HostCrashRecovery::Full;
        rt_db::set_setting(
            &conn,
            SETTING_CRASH_SAFETY,
            &serde_json::to_string(&chosen).unwrap(),
            1,
        )
        .unwrap();
        let (settings, overridden) = load_persisted_settings(&conn, &file);
        assert_eq!(settings.host_crash_recovery, HostCrashRecovery::Full);
        assert!(overridden);

        // Garbage and out-of-range rows fall back rather than blocking start.
        rt_db::set_setting(&conn, SETTING_CRASH_SAFETY, "{not json", 2).unwrap();
        assert_eq!(load_persisted_settings(&conn, &file), (file.clone(), false));
        let mut invalid = file.clone();
        invalid.completion_verify_sample_percent = 0;
        rt_db::set_setting(
            &conn,
            SETTING_CRASH_SAFETY,
            &serde_json::to_string(&invalid).unwrap(),
            3,
        )
        .unwrap();
        assert_eq!(load_persisted_settings(&conn, &file), (file, false));
    }

    #[test]
    fn view_reports_defaults_separately_from_effective_settings() {
        let rt = CrashSafetyRuntime::inert();
        let changed = CrashSafetyConfig {
            completion_verify: CompletionVerifyMode::Sample,
            ..CrashSafetyConfig::default()
        };
        rt.apply_settings(changed.clone(), true);
        let view = rt.view();
        assert_eq!(view.settings, changed);
        assert_eq!(view.defaults, CrashSafetyConfig::default());
        assert!(view.overridden);
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["settings"]["completion_verify"], "sample");
        assert_eq!(json["defaults"]["completion_verify"], "off");
        assert!(json["report"]["platform"].is_object());
    }

    #[test]
    fn gate_gauge_tracks_started_dropped_and_released() {
        let rt = CrashSafetyRuntime::inert();
        rt.record_gate_started();
        rt.record_gate_started();
        assert_eq!(rt.counters().completions_pending, 2);
        rt.record_gate_released();
        rt.record_gate_dropped();
        let c = rt.counters();
        assert_eq!(c.completions_pending, 0);
        assert_eq!(c.completions_gated, 2);
        assert_eq!(c.completions_released, 1);
        // The gauge never underflows.
        rt.record_gate_dropped();
        assert_eq!(rt.counters().completions_pending, 0);
    }

    #[test]
    fn recovery_recheck_counters_split_by_reason() {
        let rt = CrashSafetyRuntime::inert();
        rt.record_recovery_recheck(RecheckReason::RecentWrite);
        rt.record_recovery_recheck(RecheckReason::RecentWrite);
        rt.record_recovery_recheck(RecheckReason::WeakMount);
        let c = rt.counters();
        assert_eq!(c.recovery_rechecks, 3);
        assert_eq!(c.rechecks_recent_write, 2);
        assert_eq!(c.rechecks_weak_mount, 1);
        assert_eq!(c.rechecks_full_policy, 0);
    }

    #[test]
    fn weak_mounts_get_at_least_a_sample_verify_when_escalation_is_on() {
        use CompletionVerifyMode::{Full, Off, Sample};
        assert_eq!(
            effective_completion_verify(Off, true, DurabilityTrust::Weak),
            Sample
        );
        assert_eq!(
            effective_completion_verify(Off, true, DurabilityTrust::Strong),
            Off
        );
        assert_eq!(
            effective_completion_verify(Off, true, DurabilityTrust::Unknown),
            Off
        );
        assert_eq!(
            effective_completion_verify(Full, true, DurabilityTrust::Weak),
            Full,
            "an explicit choice is never lowered"
        );
        assert_eq!(
            effective_completion_verify(Off, false, DurabilityTrust::Weak),
            Off
        );
    }
}
