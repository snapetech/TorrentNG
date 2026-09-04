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
use tokio::time::timeout;
use tracing::warn;

use super::{
    normalize_storage_plan_targets, CmdResult, Engine, EngineCmd, PureV2RecheckCompletion,
    StorageDeleteCompletion, StorageJobCompletion, ENGINE_COMMAND_SEND_TIMEOUT,
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
    let affected_torrents = match normalize_storage_plan_targets(affected_torrents) {
        Ok(targets) => targets,
        Err(error) => {
            let _ = reply.send(Err(error));
            return true;
        }
    };
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
    let (completion, completion_rx) = oneshot::channel();
    let context = move_context.as_ref().map_or_else(
        || serde_json::json!({}),
        |(_, name, old_save_path, save_path)| {
            serde_json::json!({
                "old_save_path": old_save_path.display().to_string(),
                "save_path": save_path.display().to_string(),
                "name": name,
            })
        },
    );
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
                StorageJobCompletion::failed("storage worker completion channel closed", Vec::new())
            });
            if let Some((info_hash, name, old_save_path, save_path)) = move_context {
                let quiesced = quiesced
                    .iter()
                    .find(|(hash, _)| hash == &info_hash)
                    .map(|(_, paused)| *paused);
                let _ = timeout(
                    ENGINE_COMMAND_SEND_TIMEOUT,
                    cmd_tx.send(EngineCmd::StorageMoveFinished {
                        job_id,
                        info_hash,
                        name,
                        old_save_path,
                        save_path,
                        quiesced,
                        succeeded: completion.succeeded,
                        terminal_state: completion.state,
                        error: completion.error,
                        completed_steps: completion.completed_steps,
                        retry_attempt: 0,
                    }),
                )
                .await;
            } else {
                let _ = timeout(
                    ENGINE_COMMAND_SEND_TIMEOUT,
                    cmd_tx.send(EngineCmd::StoragePlanFinished {
                        job_id,
                        affected_torrents: quiesced,
                        succeeded: completion.succeeded,
                        terminal_state: completion.state,
                        error: completion.error,
                        completed_steps: completion.completed_steps,
                    }),
                )
                .await;
            }
        });
    } else {
        engine.resume_torrents_after_storage_plan(quiesced).await;
    }
    let _ = reply.send(result);
    true
}

pub(super) async fn finish_storage_plan(
    engine: &mut Engine,
    job_id: String,
    affected_torrents: Vec<(String, bool)>,
    succeeded: bool,
    terminal_state: String,
    error: Option<String>,
    completed_steps: Vec<usize>,
) {
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
            .complete_storage_plan_job_async(&job_id, &completed_steps)
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
        .resume_torrents_after_storage_plan(affected_torrents)
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
    pub(super) succeeded: bool,
    pub(super) terminal_state: String,
    pub(super) error: Option<String>,
    pub(super) completed_steps: Vec<usize>,
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
        succeeded,
        terminal_state,
        error,
        completed_steps,
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
            succeeded,
            terminal_state,
            error,
            completed_steps,
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
