//! Post-crash recovery policy.
//!
//! Pure decision functions that combine what is known about the previous run
//! (see [`crate::run_marker`]), the mount's durability trust, and one
//! torrent's own resume state into a single answer: trust the resume state, or
//! discard it and re-hash. Keeping this free of I/O makes every branch
//! directly testable.
//!
//! The rules, in order:
//!
//! 1. If the previous run could not have lost the page cache (clean shutdown,
//!    process-only crash, or no record), trust the state. The existing
//!    dirty-piece watermark still applies separately.
//! 2. A record saved *without* a data fsync (`fast` durability mode) cannot be
//!    trusted after a possible host crash. Discard it.
//! 3. Otherwise apply the configured policy, escalated by the mount's trust:
//!    weak mounts recheck everything, unknown mounts recheck at least recent
//!    writes.

use rt_storage::DurabilityTrust;
use serde::{Deserialize, Serialize};

use crate::run_marker::PreviousRun;

/// Mirror of `rt_config::HostCrashRecovery`, kept local so this crate does
/// not depend on the daemon's configuration crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryPolicy {
    Watermark,
    Recent,
    Full,
}

impl RecoveryPolicy {
    fn at_least_recent(self) -> Self {
        match self {
            RecoveryPolicy::Watermark => RecoveryPolicy::Recent,
            other => other,
        }
    }
}

/// Mirror of `rt_config::StructuralAuditMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditPolicy {
    Off,
    OnUnclean,
    Always,
}

/// Why a torrent's resume state was discarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecheckReason {
    /// Saved by the `fast` durability mode and the host may have crashed.
    UnsyncedStateAfterHostCrash,
    /// `host_crash_recovery = "full"`.
    FullPolicy,
    /// The torrent's payload was written inside the recent-write window.
    RecentWrite,
    /// The save path is on a mount classified weak.
    WeakMount,
    /// The save path is on an unknown-trust mount and was written recently.
    UnknownMountRecentWrite,
}

impl RecheckReason {
    pub fn as_str(self) -> &'static str {
        match self {
            RecheckReason::UnsyncedStateAfterHostCrash => "unsynced_state_after_host_crash",
            RecheckReason::FullPolicy => "full_policy",
            RecheckReason::RecentWrite => "recent_write",
            RecheckReason::WeakMount => "weak_mount",
            RecheckReason::UnknownMountRecentWrite => "unknown_mount_recent_write",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", content = "reason", rename_all = "snake_case")]
pub enum RecoveryAction {
    /// Use the resume state (subject to the dirty-piece watermark).
    Trust,
    /// Discard the resume state and re-hash the torrent.
    Recheck(RecheckReason),
}

impl RecoveryAction {
    pub fn is_recheck(self) -> bool {
        matches!(self, RecoveryAction::Recheck(_))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RecoveryInputs {
    pub previous_run: PreviousRun,
    /// Estimated time the previous run stopped, when known.
    pub crash_reference_unix: Option<u64>,
    /// Fallback reference when the crash time is unknown.
    pub now_unix: u64,
    pub policy: RecoveryPolicy,
    pub recent_window_secs: u64,
    pub mount_trust: DurabilityTrust,
    /// Escalate policy on weak/unknown mounts.
    pub weak_mount_escalation: bool,
    /// `FastresumeState::durability.synced`.
    pub state_synced: bool,
    /// `FastresumeState::durability.last_data_write_unix`; 0 = unknown.
    pub last_data_write_unix: u64,
    /// When the torrent finished downloading, if it did.
    pub completed_unix: Option<u64>,
    /// The record was written by the current daemon run, so it post-dates the
    /// crash being recovered from.
    pub state_saved_this_run: bool,
}

/// Raise a recovery policy for a mount whose `fsync` cannot be fully trusted:
/// a weak mount is rechecked in full, an unknown mount at least for recent
/// writes. Returns the policy to apply and, when escalation raised it, the
/// reason. Shared by the recovery decision and by the API view of what a
/// location would get.
pub fn escalate_policy(
    policy: RecoveryPolicy,
    mount_trust: DurabilityTrust,
    escalation_enabled: bool,
) -> (RecoveryPolicy, Option<RecheckReason>) {
    if !escalation_enabled {
        return (policy, None);
    }
    match mount_trust {
        DurabilityTrust::Weak => (RecoveryPolicy::Full, Some(RecheckReason::WeakMount)),
        DurabilityTrust::Unknown => {
            let raised = policy.at_least_recent();
            let reason = (raised != policy).then_some(RecheckReason::UnknownMountRecentWrite);
            (raised, reason)
        }
        DurabilityTrust::Strong => (policy, None),
    }
}

/// Decide whether a torrent's resume state may be trusted after this start.
pub fn decide_recovery(input: &RecoveryInputs) -> RecoveryAction {
    if !input.previous_run.may_be_host_crash() || input.state_saved_this_run {
        return RecoveryAction::Trust;
    }
    if !input.state_synced {
        return RecoveryAction::Recheck(RecheckReason::UnsyncedStateAfterHostCrash);
    }

    let (policy, escalated_by) =
        escalate_policy(input.policy, input.mount_trust, input.weak_mount_escalation);

    match policy {
        RecoveryPolicy::Watermark => RecoveryAction::Trust,
        RecoveryPolicy::Full => RecoveryAction::Recheck(
            escalated_by
                .filter(|reason| *reason == RecheckReason::WeakMount)
                .unwrap_or(RecheckReason::FullPolicy),
        ),
        RecoveryPolicy::Recent => {
            if written_recently(input) {
                RecoveryAction::Recheck(escalated_by.unwrap_or(RecheckReason::RecentWrite))
            } else {
                RecoveryAction::Trust
            }
        }
    }
}

/// Whether the torrent's payload was written within the window before the
/// crash. A torrent whose write time is unknown (state from an older build
/// that never went through a write) is treated as not recent: it cannot be
/// dated, and rechecking every such torrent after the first crash following an
/// upgrade would be a library-wide penalty for no evidence.
fn written_recently(input: &RecoveryInputs) -> bool {
    let last_write = input
        .last_data_write_unix
        .max(input.completed_unix.unwrap_or(0));
    if last_write == 0 {
        return false;
    }
    let reference = input.crash_reference_unix.unwrap_or(input.now_unix);
    last_write.saturating_add(input.recent_window_secs) >= reference
}

/// Whether the allocation audit should run for this torrent start.
pub fn should_audit_allocation(
    policy: AuditPolicy,
    previous_run: PreviousRun,
    state_clean_shutdown: bool,
    state_saved_this_run: bool,
) -> bool {
    match policy {
        AuditPolicy::Off => false,
        AuditPolicy::Always => true,
        AuditPolicy::OnUnclean => {
            !state_saved_this_run && (previous_run.was_unclean() || !state_clean_shutdown)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> RecoveryInputs {
        RecoveryInputs {
            previous_run: PreviousRun::HostCrash,
            crash_reference_unix: Some(100_000),
            now_unix: 200_000,
            policy: RecoveryPolicy::Recent,
            recent_window_secs: 86_400,
            mount_trust: DurabilityTrust::Strong,
            weak_mount_escalation: true,
            state_synced: true,
            last_data_write_unix: 0,
            completed_unix: None,
            state_saved_this_run: false,
        }
    }

    #[test]
    fn nothing_is_scrutinized_unless_the_page_cache_may_be_lost() {
        for run in [
            PreviousRun::Clean,
            PreviousRun::NoRecord,
            PreviousRun::ProcessCrash,
        ] {
            let mut input = base();
            input.previous_run = run;
            input.policy = RecoveryPolicy::Full;
            input.mount_trust = DurabilityTrust::Weak;
            input.state_synced = false;
            assert_eq!(decide_recovery(&input), RecoveryAction::Trust, "{run:?}");
        }
    }

    #[test]
    fn state_saved_by_the_current_run_is_never_rejudged() {
        let mut input = base();
        input.policy = RecoveryPolicy::Full;
        input.mount_trust = DurabilityTrust::Weak;
        input.state_synced = false;
        input.state_saved_this_run = true;
        assert_eq!(decide_recovery(&input), RecoveryAction::Trust);
    }

    #[test]
    fn fast_mode_state_is_discarded_after_a_possible_host_crash() {
        for run in [PreviousRun::HostCrash, PreviousRun::UncleanUnknownCause] {
            let mut input = base();
            input.previous_run = run;
            input.policy = RecoveryPolicy::Watermark;
            input.state_synced = false;
            assert_eq!(
                decide_recovery(&input),
                RecoveryAction::Recheck(RecheckReason::UnsyncedStateAfterHostCrash),
                "{run:?}"
            );
        }
    }

    #[test]
    fn watermark_policy_trusts_synced_state_on_a_strong_mount() {
        let mut input = base();
        input.policy = RecoveryPolicy::Watermark;
        input.last_data_write_unix = 99_999;
        assert_eq!(decide_recovery(&input), RecoveryAction::Trust);
    }

    #[test]
    fn full_policy_rechecks_everything() {
        let mut input = base();
        input.policy = RecoveryPolicy::Full;
        assert_eq!(
            decide_recovery(&input),
            RecoveryAction::Recheck(RecheckReason::FullPolicy)
        );
    }

    #[test]
    fn recent_policy_rechecks_only_torrents_written_inside_the_window() {
        let mut input = base();
        // Crash at 100_000, window 86_400 => cutoff 13_600.
        input.last_data_write_unix = 13_600;
        assert_eq!(
            decide_recovery(&input),
            RecoveryAction::Recheck(RecheckReason::RecentWrite)
        );
        input.last_data_write_unix = 13_599;
        assert_eq!(decide_recovery(&input), RecoveryAction::Trust);
    }

    #[test]
    fn recent_policy_counts_completion_time_as_a_write() {
        let mut input = base();
        input.last_data_write_unix = 0;
        input.completed_unix = Some(99_000);
        assert_eq!(
            decide_recovery(&input),
            RecoveryAction::Recheck(RecheckReason::RecentWrite)
        );
    }

    #[test]
    fn undatable_torrents_are_not_treated_as_recent() {
        let input = base();
        assert_eq!(input.last_data_write_unix, 0);
        assert_eq!(decide_recovery(&input), RecoveryAction::Trust);
    }

    #[test]
    fn writes_after_the_crash_reference_are_recent() {
        let mut input = base();
        input.last_data_write_unix = 150_000; // clock skew: after the heartbeat
        assert_eq!(
            decide_recovery(&input),
            RecoveryAction::Recheck(RecheckReason::RecentWrite)
        );
    }

    #[test]
    fn unknown_crash_time_falls_back_to_now() {
        let mut input = base();
        input.crash_reference_unix = None;
        input.now_unix = 1_000_000;
        input.last_data_write_unix = 900_000; // outside 86_400 of now
        assert_eq!(decide_recovery(&input), RecoveryAction::Trust);
        input.last_data_write_unix = 950_000;
        assert_eq!(
            decide_recovery(&input),
            RecoveryAction::Recheck(RecheckReason::RecentWrite)
        );
    }

    #[test]
    fn weak_mount_escalates_any_policy_to_a_full_recheck() {
        for policy in [
            RecoveryPolicy::Watermark,
            RecoveryPolicy::Recent,
            RecoveryPolicy::Full,
        ] {
            let mut input = base();
            input.policy = policy;
            input.mount_trust = DurabilityTrust::Weak;
            assert_eq!(
                decide_recovery(&input),
                RecoveryAction::Recheck(RecheckReason::WeakMount),
                "{policy:?}"
            );
        }
    }

    #[test]
    fn unknown_mount_raises_watermark_to_recent_but_leaves_full_alone() {
        let mut input = base();
        input.policy = RecoveryPolicy::Watermark;
        input.mount_trust = DurabilityTrust::Unknown;
        input.last_data_write_unix = 99_000;
        assert_eq!(
            decide_recovery(&input),
            RecoveryAction::Recheck(RecheckReason::UnknownMountRecentWrite)
        );
        input.last_data_write_unix = 1;
        assert_eq!(decide_recovery(&input), RecoveryAction::Trust);

        input.policy = RecoveryPolicy::Full;
        assert_eq!(
            decide_recovery(&input),
            RecoveryAction::Recheck(RecheckReason::FullPolicy)
        );
    }

    #[test]
    fn escalation_can_be_switched_off() {
        let mut input = base();
        input.policy = RecoveryPolicy::Watermark;
        input.mount_trust = DurabilityTrust::Weak;
        input.weak_mount_escalation = false;
        assert_eq!(decide_recovery(&input), RecoveryAction::Trust);
    }

    #[test]
    fn escalation_raises_weak_to_full_and_unknown_to_at_least_recent() {
        use RecoveryPolicy::*;
        for policy in [Watermark, Recent, Full] {
            assert_eq!(
                escalate_policy(policy, DurabilityTrust::Weak, true),
                (Full, Some(RecheckReason::WeakMount)),
                "{policy:?}"
            );
            assert_eq!(
                escalate_policy(policy, DurabilityTrust::Strong, true),
                (policy, None)
            );
            // Disabled: nothing changes whatever the mount is.
            for trust in [
                DurabilityTrust::Weak,
                DurabilityTrust::Unknown,
                DurabilityTrust::Strong,
            ] {
                assert_eq!(escalate_policy(policy, trust, false), (policy, None));
            }
        }
        assert_eq!(
            escalate_policy(Watermark, DurabilityTrust::Unknown, true),
            (Recent, Some(RecheckReason::UnknownMountRecentWrite))
        );
        // Already at least recent: unchanged, and no reason to report.
        assert_eq!(
            escalate_policy(Recent, DurabilityTrust::Unknown, true),
            (Recent, None)
        );
        assert_eq!(
            escalate_policy(Full, DurabilityTrust::Unknown, true),
            (Full, None)
        );
    }

    #[test]
    fn audit_gate_follows_policy_and_uncleanness() {
        use AuditPolicy::*;
        let gate = should_audit_allocation;
        assert!(!gate(Off, PreviousRun::HostCrash, false, false));
        assert!(gate(Always, PreviousRun::Clean, true, false));
        assert!(gate(Always, PreviousRun::Clean, true, true));
        assert!(!gate(OnUnclean, PreviousRun::Clean, true, false));
        assert!(!gate(OnUnclean, PreviousRun::NoRecord, true, false));
        assert!(gate(OnUnclean, PreviousRun::ProcessCrash, true, false));
        assert!(gate(OnUnclean, PreviousRun::HostCrash, true, false));
        // A torrent whose own state was saved unclean is audited even when the
        // run marker looked clean (feature disabled or marker lost).
        assert!(gate(OnUnclean, PreviousRun::NoRecord, false, false));
        // ...but not again once this run has already re-saved it.
        assert!(!gate(OnUnclean, PreviousRun::HostCrash, false, true));
    }
}
