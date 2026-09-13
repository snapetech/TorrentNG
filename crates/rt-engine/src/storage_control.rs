//! Engine-facing storage command protocol.
//!
//! The storage worker owns filesystem execution in `storage_jobs`. This
//! module owns the message choreography at the engine boundary: validating
//! and quiescing targets, submitting a durable plan, and translating worker
//! completion back into engine commands. Keeping that protocol out of the
//! general command dispatcher makes the storage failure surface independently
//! reviewable without creating a second source of torrent truth.

use rt_storage::StoragePlan;
use tokio::sync::oneshot;
use tracing::warn;

use super::{
    normalize_storage_plan_targets, CmdResult, Engine, EngineCmd, PureV2RecheckCompletion,
    StorageDeleteCompletion, StorageJobCompletion, JOB_STATE_QUEUED,
    STORAGE_JOB_STATE_COMMIT_PENDING,
};

/// Queue a validated storage plan and send the worker completion back through
/// the engine actor. The actor remains responsible for ordering state changes,
/// while this module owns the worker protocol and its rollback-on-submit
/// behavior.
pub(super) async fn execute_storage_plan(
    engine: &mut Engine,
    operation: String,
    affected_torrents: Vec<String>,
    plan: StoragePlan,
    completed_steps: Vec<usize>,
    reply: oneshot::Sender<CmdResult<String>>,
) -> bool {
    let operation = operation.trim().to_ascii_lowercase();
    if plan.dry_run {
        let _ = reply.send(Err(
            "storage execution received a dry-run plan; use the preview endpoint first".to_owned(),
        ));
        return true;
    }
    if let Err(error) = rt_storage::ensure_plan_can_apply(&plan) {
        let _ = reply.send(Err(format!("storage plan cannot apply: {error}")));
        return true;
    }
    let affected_torrents = match normalize_storage_plan_targets(affected_torrents) {
        Ok(targets) => targets,
        Err(error) => {
            let _ = reply.send(Err(error));
            return true;
        }
    };
    let manual_recovery_torrents = affected_torrents.clone();
    if let Err(error) = engine
        .validate_storage_plan_targets(&operation, &affected_torrents)
        .await
    {
        let _ = reply.send(Err(error));
        return true;
    }
    if let Err(error) = engine.ensure_torrents_jobs_idle(&affected_torrents).await {
        let _ = reply.send(Err(error));
        return true;
    }
    let move_context = if operation == "move" {
        match engine
            .storage_plan_move_context(&affected_torrents, &plan)
            .await
        {
            Ok(context) => Some(context),
            Err(error) => {
                let _ = reply.send(Err(error));
                return true;
            }
        }
    } else {
        None
    };
    let quiesced = match engine
        .quiesce_torrents_for_storage_plan(&affected_torrents)
        .await
    {
        Ok(quiesced) => quiesced,
        Err(error) => {
            let _ = reply.send(Err(error));
            return true;
        }
    };
    // Capture every target's session identity, including dormant torrents
    // that had no task to quiesce. A move completion can be delivered after
    // an info-hash was removed and re-added; the hash alone must not let that
    // stale completion publish the old torrent's destination onto the new
    // incarnation.
    let affected_torrent_handles = engine.torrent_handles_for_targets(&affected_torrents).await;
    let (completion, completion_rx) = oneshot::channel();
    let mut context = move_context.as_ref().map_or_else(
        || serde_json::json!({}),
        |(_, name, old_save_path, save_path)| {
            serde_json::json!({
                "old_save_path": old_save_path.display().to_string(),
                "save_path": save_path.display().to_string(),
                "name": name,
            })
        },
    );
    context["quiesced"] = super::storage_quiesced_context(&quiesced);
    let result = engine
        .queue_storage_plan_job_with_context(
            &operation,
            affected_torrents.clone(),
            &plan,
            completed_steps,
            context,
            completion,
        )
        .await;
    if let Ok(job_id) = &result {
        let cmd_tx = engine.cmd_tx.clone();
        let job_id = job_id.clone();
        tokio::spawn(async move {
            let completion = completion_rx.await.unwrap_or_else(|_| {
                StorageJobCompletion::failed_with_manual_recovery(
                    "storage worker completion channel closed",
                    Vec::new(),
                )
            });
            if let Some((info_hash, name, old_save_path, save_path)) = move_context {
                let quiesced = quiesced
                    .iter()
                    .find(|(hash, _)| hash == &info_hash)
                    .map(|(_, paused)| *paused);
                let torrent_handle = affected_torrent_handles
                    .iter()
                    .find(|(hash, _)| hash == &info_hash)
                    .map(|(_, handle)| *handle);
                super::send_engine_command_until_delivered(
                    cmd_tx,
                    EngineCmd::StorageMoveFinished {
                        job_id,
                        info_hash,
                        name,
                        old_save_path,
                        save_path,
                        quiesced,
                        torrent_handle,
                        succeeded: completion.succeeded,
                        terminal_state: completion.state,
                        error: completion.error,
                        completed_steps: completion.completed_steps,
                        completed_byte_offset: completion.completed_byte_offset,
                        requires_manual_recovery: completion.requires_manual_recovery,
                        retry_attempt: 0,
                    },
                    "storage_move_completion",
                )
                .await;
            } else {
                super::send_engine_command_until_delivered(
                    cmd_tx,
                    EngineCmd::StoragePlanFinished {
                        job_id,
                        affected_torrents: quiesced,
                        affected_torrent_handles,
                        manual_recovery_torrents,
                        succeeded: completion.succeeded,
                        terminal_state: completion.state,
                        error: completion.error,
                        completed_steps: completion.completed_steps,
                        completed_byte_offset: completion.completed_byte_offset,
                        requires_manual_recovery: completion.requires_manual_recovery,
                    },
                    "storage_plan_completion",
                )
                .await;
            }
        });
    } else {
        engine
            .resume_torrents_after_storage_plan(quiesced, Vec::new())
            .await;
    }
    let _ = reply.send(result);
    true
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn finish_storage_plan(
    engine: &mut Engine,
    job_id: String,
    affected_torrents: Vec<(String, bool)>,
    affected_torrent_handles: Vec<(String, rt_session::TorrentHandle)>,
    manual_recovery_torrents: Vec<String>,
    succeeded: bool,
    terminal_state: String,
    error: Option<String>,
    completed_steps: Vec<usize>,
    completed_byte_offset: Option<i64>,
    requires_manual_recovery: bool,
) {
    // The worker uses queued for shutdown reattachment. The engine is also
    // shutting down, so resuming quiesced torrent tasks here would briefly
    // reopen work that must remain frozen until restart recovery.
    if terminal_state == JOB_STATE_QUEUED {
        return;
    }
    if !succeeded && requires_manual_recovery {
        warn!(
            component = "storage_jobs",
            operation = "complete",
            job_id = %job_id,
            result = "manual_recovery_required",
            state = %terminal_state,
            checkpoint = ?completed_steps,
            error = ?error,
            "storage plan left filesystem state that cannot be resumed safely"
        );
        let reason = super::manual_recovery_reason(error.unwrap_or_else(|| {
            "storage plan left filesystem state that requires manual recovery".to_owned()
        }));
        if let Err(persist_error) = engine
            .persist_storage_job_manual_recovery(
                &job_id,
                &manual_recovery_torrents,
                reason.clone(),
                "storage plan completion required manual recovery",
            )
            .await
        {
            warn!(
                component = "storage_jobs",
                operation = "persist_manual_recovery",
                job_id = %job_id,
                result = "error",
                error = %persist_error,
                "failed to persist storage plan manual-recovery marker at completion"
            );
        }
        for info_hash in manual_recovery_torrents {
            if let Some(expected_handle) = affected_torrent_handles
                .iter()
                .find(|(hash, _)| hash == &info_hash)
                .map(|(_, handle)| *handle)
            {
                if engine.torrent_handle_for(&info_hash).await != Some(expected_handle) {
                    warn!(
                        component = "storage_jobs",
                        operation = "mark_manual_recovery",
                        job_id = %job_id,
                        torrent = %info_hash,
                        result = "stale",
                        "discarding manual-recovery completion for a replaced torrent"
                    );
                    continue;
                }
            }
            engine.stop_torrent_task(&info_hash).await;
            if let Err(mark_error) = engine
                .mark_torrent_manual_recovery(&info_hash, &reason)
                .await
            {
                warn!(
                    component = "storage_jobs",
                    operation = "mark_manual_recovery",
                    job_id = %job_id,
                    torrent = %info_hash,
                    result = "error",
                    error = %mark_error,
                    "failed to persist torrent manual-recovery state after stopping its task"
                );
            }
        }
        return;
    }
    if !succeeded {
        warn!(
            component = "storage_jobs",
            operation = "complete",
            job_id = %job_id,
            result = "failed",
            state = %terminal_state,
            checkpoint = ?completed_steps,
            error = ?error,
            "storage plan finished without a successful commit"
        );
    }
    if succeeded && terminal_state == STORAGE_JOB_STATE_COMMIT_PENDING {
        if let Err(error) = engine
            .complete_storage_plan_job_async(&job_id, &completed_steps, completed_byte_offset)
            .await
        {
            warn!(
                component = "storage_jobs",
                operation = "complete",
                job_id = %job_id,
                result = "error",
                error = %error,
                "storage plan filesystem commit completed but durable job completion failed"
            );
        }
    }
    engine
        .resume_torrents_after_storage_plan(affected_torrents, affected_torrent_handles)
        .await;
}

pub(super) async fn finish_storage_delete(
    engine: &mut Engine,
    completion: StorageDeleteCompletion,
) {
    let job_id = completion.job_id.clone();
    let info_hash = completion.info_hash.clone();
    if let Err(error) = engine.finish_storage_delete(completion).await {
        warn!(
            component = "storage_jobs",
            operation = "finish_storage_delete",
            job_id = %job_id,
            torrent = %info_hash,
            result = "error",
            error = %error,
            "failed to finalize asynchronous torrent deletion"
        );
    }
}

pub(super) struct StorageMoveCompletion {
    pub(super) job_id: String,
    pub(super) info_hash: String,
    pub(super) name: Option<String>,
    pub(super) old_save_path: std::path::PathBuf,
    pub(super) save_path: std::path::PathBuf,
    pub(super) quiesced: Option<bool>,
    pub(super) torrent_handle: Option<rt_session::TorrentHandle>,
    pub(super) succeeded: bool,
    pub(super) terminal_state: String,
    pub(super) error: Option<String>,
    pub(super) completed_steps: Vec<usize>,
    pub(super) completed_byte_offset: Option<i64>,
    pub(super) requires_manual_recovery: bool,
    pub(super) retry_attempt: u8,
}

pub(super) async fn finish_storage_move(engine: &mut Engine, completion: StorageMoveCompletion) {
    let StorageMoveCompletion {
        job_id,
        info_hash,
        name,
        old_save_path,
        save_path,
        quiesced,
        torrent_handle,
        succeeded,
        terminal_state,
        error,
        completed_steps,
        completed_byte_offset,
        requires_manual_recovery,
        retry_attempt,
    } = completion;
    if let Err(error) = engine
        .finish_storage_move(
            &job_id,
            &info_hash,
            name,
            old_save_path,
            save_path,
            quiesced,
            torrent_handle,
            succeeded,
            terminal_state,
            error,
            completed_steps,
            completed_byte_offset,
            requires_manual_recovery,
            retry_attempt,
        )
        .await
    {
        warn!(
            component = "storage_jobs",
            operation = "finish_storage_move",
            job_id = %job_id,
            torrent = %info_hash,
            result = "error",
            error = %error,
            "failed to finalize asynchronous storage move"
        );
    }
}

pub(super) async fn finish_pure_v2_recheck(
    engine: &mut Engine,
    completion: PureV2RecheckCompletion,
) {
    let info_hash = completion.info_hash.clone();
    if let Err(error) = engine.finish_pure_v2_recheck(completion).await {
        warn!(
            component = "storage",
            operation = "finish_pure_v2_recheck",
            torrent = %info_hash,
            result = "error",
            error = %error,
            "failed to finalize pure-v2 recheck"
        );
    }
}
