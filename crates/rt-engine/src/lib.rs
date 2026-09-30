pub mod command;
mod db_worker;
mod dht_task;
pub mod egress_policy;
pub mod engine;
mod metadata_task;
pub(crate) mod network_budget;
pub mod peer_id;
pub mod peer_ingress;
mod peer_listener;
pub mod storage_authority;
pub(crate) mod storage_jobs;
pub mod tier;
pub mod torrent_task;
pub(crate) mod torrent_task_v2;
pub(crate) mod tracker_runtime;

pub use command::{
    EngineCategory, EngineDatabaseWorkerStats, EngineGlobalLimits, EngineJob,
    EngineNetworkFeatures, EnginePeerSnapshot, EnginePieceState, EngineStats, EngineStorageRoot,
    EngineSubsystemHealth, EngineTorrentFile, EngineTorrentLimits, EngineTorrentMetadata,
    EngineTrackerHealth, EngineTrackerSnapshot, EngineWebseedSnapshot, HotTorrentMemoryStats,
    QueueMove, StorageDeviceLatencyStats, TorrentDiagnostic, TorrentLiveStats, TorrentRuntimeStats,
};
pub use egress_policy::{
    egress_policy_metrics, AddressClass, EgressPolicyError, EgressPolicyMetricsSnapshot,
    OutboundEgressPolicy, OutboundTargetKind,
};
pub use engine::{Engine, EngineHandle, MAX_MANUAL_PEER_ADDRESSES};
pub use peer_ingress::{
    PeerIngressBudget, PeerIngressConfig, PeerIngressPermit, PeerIngressReject, PeerIngressStats,
};
pub use storage_authority::{ServerStorageRoots, StorageAuthorityError};
pub use tier::{
    ActivityTimerWheel, CompactPieceBitmap, DormantTorrentSnapshot, TierController, TierDecision,
    TierEvent, TierInput, TierPolicy, TierScaleBudget, TierScaleSnapshot, TorrentActivityTier,
};

/// Summarize a Tokio task failure without formatting a panic payload, which
/// may contain request-controlled or otherwise sensitive values.
pub fn task_join_error_summary(task: &'static str, error: &tokio::task::JoinError) -> String {
    let outcome = if error.is_panic() {
        "panicked"
    } else {
        "was cancelled"
    };
    format!("{task} {outcome}")
}

pub(crate) fn log_task_join_error(
    component: &'static str,
    operation: &'static str,
    task: &'static str,
    error: &tokio::task::JoinError,
) {
    tracing::warn!(
        component,
        operation,
        result = "join_error",
        error = %task_join_error_summary(task, error),
        "supervised task failed while joining"
    );
}

pub(crate) async fn shutdown_join_task<T, F>(
    task: &mut tokio::task::JoinHandle<T>,
    wait_budget: std::time::Duration,
    abort_grace: std::time::Duration,
    component: &'static str,
    operation: &'static str,
    task_name: &'static str,
    on_timeout: F,
) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce(),
{
    match tokio::time::timeout(wait_budget, &mut *task).await {
        Ok(Ok(result)) => Some(result),
        Ok(Err(error)) => {
            log_task_join_error(component, operation, task_name, &error);
            None
        }
        Err(_) => {
            tracing::warn!(
                component,
                operation,
                task = task_name,
                result = "timeout",
                timeout_ms = wait_budget.as_millis().min(u128::from(u64::MAX)) as u64,
                "supervised task did not stop before shutdown deadline; aborting it"
            );
            on_timeout();
            task.abort();
            reap_aborted_task(task, abort_grace, component, operation, task_name).await
        }
    }
}

pub(crate) async fn reap_aborted_task<T>(
    task: &mut tokio::task::JoinHandle<T>,
    abort_grace: std::time::Duration,
    component: &'static str,
    operation: &'static str,
    task_name: &'static str,
) -> Option<T>
where
    T: Send + 'static,
{
    match tokio::time::timeout(abort_grace, &mut *task).await {
        Ok(Ok(result)) => Some(result),
        Ok(Err(error)) if error.is_cancelled() => None,
        Ok(Err(error)) => {
            log_task_join_error(component, operation, task_name, &error);
            None
        }
        Err(_) => {
            tracing::warn!(
                component,
                operation,
                task = task_name,
                result = "abort_grace_timeout",
                grace_ms = abort_grace.as_millis().min(u128::from(u64::MAX)) as u64,
                "supervised task did not finish during abort grace"
            );
            None
        }
    }
}

#[cfg(test)]
mod task_join_error_tests {
    use super::task_join_error_summary;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn join_error_summary_does_not_include_panic_payload() {
        let task = tokio::spawn(async {
            panic!("join-payload-canary");
        });
        let error = task.await.unwrap_err();
        let summary = task_join_error_summary("storage worker", &error);

        assert!(error.is_panic());
        assert_eq!(summary, "storage worker panicked");
        assert!(!summary.contains("join-payload-canary"));
    }

    #[tokio::test]
    async fn shutdown_join_helper_observes_panics_and_success() {
        let mut panicked = tokio::spawn(async { std::panic::panic_any(()) });
        assert!(super::shutdown_join_task(
            &mut panicked,
            Duration::from_secs(1),
            Duration::from_millis(10),
            "test",
            "join",
            "panic canary task",
            || {},
        )
        .await
        .is_none());

        let mut completed = tokio::spawn(async {});
        assert_eq!(
            super::shutdown_join_task(
                &mut completed,
                Duration::from_secs(1),
                Duration::from_millis(10),
                "test",
                "join",
                "completed task",
                || {},
            )
            .await,
            Some(())
        );
    }

    #[tokio::test]
    async fn shutdown_join_helper_aborts_and_reaps_after_deadline() {
        let mut task = tokio::spawn(std::future::pending::<()>());
        let abort_called = Arc::new(AtomicBool::new(false));
        let abort_called_by_callback = Arc::clone(&abort_called);

        let outcome = super::shutdown_join_task(
            &mut task,
            Duration::ZERO,
            Duration::from_secs(1),
            "test",
            "join",
            "pending task",
            move || abort_called_by_callback.store(true, Ordering::Release),
        )
        .await;

        assert!(outcome.is_none());
        assert!(abort_called.load(Ordering::Acquire));
        assert!(task.is_finished());
    }
}
