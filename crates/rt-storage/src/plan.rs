use std::collections::HashSet;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use crate::StorageError;
use rt_path::SafeRelPath;

/// Bound recursive storage-plan walks below the stack-exhaustion threshold.
/// Imported directories can be operator-provided and do not necessarily
/// originate from validated metainfo, so each recursive walker enforces this
/// independently. Raise this only after converting the walkers to iterative
/// traversal or proving the stack bound on every supported target.
pub(crate) const MAX_STORAGE_TREE_DEPTH: usize = 64;

pub(crate) fn ensure_storage_tree_depth(
    depth: usize,
    path: &Path,
    step: &'static str,
) -> Result<(), StorageError> {
    if depth <= MAX_STORAGE_TREE_DEPTH {
        Ok(())
    } else {
        Err(StorageError::StagedMoveFailed {
            step,
            reason: format!(
                "directory nesting depth {depth} exceeds the maximum of {MAX_STORAGE_TREE_DEPTH}: {}",
                path.display()
            ),
        })
    }
}

#[cfg(all(windows, test))]
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

#[cfg(unix)]
use crate::secure_fs;
#[cfg(windows)]
use crate::windows_secure_fs;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PlannedStorageAction {
    ImportExisting,
    Rename,
    CopyVerifyRename,
    SafeDelete,
    /// Delete a path when present. This is intentionally distinct from
    /// `SafeDelete`, whose missing-source error is useful for detecting a
    /// broken move rollback. Cleanup jobs need retry-safe idempotence.
    SafeDeleteIfPresent,
    /// Remove empty directories upward until the supplied root boundary.
    /// Non-empty directories are a normal stopping condition.
    PruneEmptyDirs,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PlanIssue {
    SourceMissing(PathBuf),
    DestinationExists(PathBuf),
    InsufficientCapacity { needed: u64, available: u64 },
    AtomicNoReplaceRenameUnavailable,
    DeleteRequiresDryRunApproval,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoragePlanStep {
    pub action: PlannedStorageAction,
    pub source: Option<PathBuf>,
    pub destination: Option<PathBuf>,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoragePlan {
    pub dry_run: bool,
    pub can_apply: bool,
    pub issues: Vec<PlanIssue>,
    pub steps: Vec<StoragePlanStep>,
    pub rollback_steps: Vec<StoragePlanStep>,
}

#[derive(Debug, Clone)]
pub struct MovePlanRequest {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub bytes: u64,
    pub available_bytes: Option<u64>,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct ImportPlanRequest {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub bytes: u64,
    pub available_bytes: Option<u64>,
    pub hardlink_or_copy: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct DeletePlanRequest {
    pub target: PathBuf,
    pub bytes: u64,
    pub dry_run: bool,
    pub dry_run_approved: bool,
}

pub fn plan_move(req: &MovePlanRequest) -> StoragePlan {
    plan_move_with_rename_support(req, atomic_no_replace_rename_supported())
}

fn plan_move_with_rename_support(
    req: &MovePlanRequest,
    atomic_rename_supported: bool,
) -> StoragePlan {
    let mut issues = common_issues(
        &req.source,
        &req.destination,
        req.bytes,
        req.available_bytes,
    );
    if !atomic_rename_supported {
        issues.push(PlanIssue::AtomicNoReplaceRenameUnavailable);
    }
    let same_device = same_filesystem(&req.source, &req.destination).unwrap_or(false);
    let action = if same_device {
        PlannedStorageAction::Rename
    } else {
        PlannedStorageAction::CopyVerifyRename
    };
    let steps = match action {
        PlannedStorageAction::Rename => vec![StoragePlanStep {
            action,
            source: Some(req.source.clone()),
            destination: Some(req.destination.clone()),
            bytes: req.bytes,
        }],
        PlannedStorageAction::CopyVerifyRename => vec![
            StoragePlanStep {
                action: PlannedStorageAction::CopyVerifyRename,
                source: Some(req.source.clone()),
                destination: Some(staging_path(&req.destination)),
                bytes: req.bytes,
            },
            StoragePlanStep {
                action: PlannedStorageAction::Rename,
                source: Some(staging_path(&req.destination)),
                destination: Some(req.destination.clone()),
                bytes: req.bytes,
            },
            StoragePlanStep {
                action: PlannedStorageAction::SafeDelete,
                source: Some(req.source.clone()),
                destination: None,
                bytes: req.bytes,
            },
        ],
        _ => unreachable!("move planner only emits move actions"),
    };
    let rollback_steps = vec![StoragePlanStep {
        // The copy path cleans a partial staging entry before returning an
        // error. Rollback therefore has to tolerate an already-clean staging
        // path; otherwise a successful cleanup is falsely reported as a
        // rollback failure and escalated to manual recovery.
        action: PlannedStorageAction::SafeDeleteIfPresent,
        source: Some(staging_path(&req.destination)),
        destination: None,
        bytes: req.bytes,
    }];
    let can_apply = issues.is_empty();
    StoragePlan {
        dry_run: req.dry_run,
        can_apply,
        issues,
        steps,
        rollback_steps,
    }
}

fn atomic_no_replace_rename_supported() -> bool {
    // Keep this list aligned with `secure_fs::rename_without_replace` and the
    // Windows `MoveFileW` implementation. Other targets reject that step.
    cfg!(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        windows
    ))
}

pub fn plan_move_under_roots(
    req: &MovePlanRequest,
    roots: &[PathBuf],
) -> Result<StoragePlan, StorageError> {
    let roots = canonical_roots(roots)?;
    let source = confine_plan_path(&req.source, &roots)?;
    let destination = confine_plan_path(&req.destination, &roots)?;
    Ok(plan_move(&MovePlanRequest {
        source,
        destination,
        bytes: req.bytes,
        available_bytes: req.available_bytes,
        dry_run: req.dry_run,
    }))
}

pub fn plan_import(req: &ImportPlanRequest) -> StoragePlan {
    plan_import_with_rename_support(req, atomic_no_replace_rename_supported())
}

fn plan_import_with_rename_support(
    req: &ImportPlanRequest,
    atomic_rename_supported: bool,
) -> StoragePlan {
    let mut issues = common_issues(
        &req.source,
        &req.destination,
        req.bytes,
        req.available_bytes,
    );
    let action = if req.hardlink_or_copy
        && same_filesystem(&req.source, &req.destination).unwrap_or(false)
    {
        PlannedStorageAction::ImportExisting
    } else {
        PlannedStorageAction::CopyVerifyRename
    };
    if matches!(action, PlannedStorageAction::CopyVerifyRename) && !atomic_rename_supported {
        issues.push(PlanIssue::AtomicNoReplaceRenameUnavailable);
    }
    let (steps, rollback_steps) = match action {
        PlannedStorageAction::ImportExisting => (
            vec![StoragePlanStep {
                action,
                source: Some(req.source.clone()),
                destination: Some(req.destination.clone()),
                bytes: req.bytes,
            }],
            Vec::new(),
        ),
        PlannedStorageAction::CopyVerifyRename => (
            vec![
                StoragePlanStep {
                    action: PlannedStorageAction::CopyVerifyRename,
                    source: Some(req.source.clone()),
                    destination: Some(staging_path(&req.destination)),
                    bytes: req.bytes,
                },
                StoragePlanStep {
                    action: PlannedStorageAction::Rename,
                    source: Some(staging_path(&req.destination)),
                    destination: Some(req.destination.clone()),
                    bytes: req.bytes,
                },
            ],
            vec![StoragePlanStep {
                // Copy cleanup is idempotent: the executor may already have
                // removed a partial staging entry before rollback begins.
                action: PlannedStorageAction::SafeDeleteIfPresent,
                source: Some(staging_path(&req.destination)),
                destination: None,
                bytes: req.bytes,
            }],
        ),
        _ => unreachable!("import planner only emits import or copy actions"),
    };
    StoragePlan {
        dry_run: req.dry_run,
        can_apply: issues.is_empty(),
        issues,
        steps,
        rollback_steps,
    }
}

pub fn plan_import_under_roots(
    req: &ImportPlanRequest,
    roots: &[PathBuf],
) -> Result<StoragePlan, StorageError> {
    let roots = canonical_roots(roots)?;
    let source = confine_plan_path(&req.source, &roots)?;
    let destination = confine_plan_path(&req.destination, &roots)?;
    Ok(plan_import(&ImportPlanRequest {
        source,
        destination,
        bytes: req.bytes,
        available_bytes: req.available_bytes,
        hardlink_or_copy: req.hardlink_or_copy,
        dry_run: req.dry_run,
    }))
}

pub fn plan_delete(req: &DeletePlanRequest) -> StoragePlan {
    let mut issues = Vec::new();
    if !path_exists_no_follow(&req.target) {
        issues.push(PlanIssue::SourceMissing(req.target.clone()));
    }
    if !req.dry_run_approved {
        issues.push(PlanIssue::DeleteRequiresDryRunApproval);
    }
    let steps = vec![StoragePlanStep {
        action: PlannedStorageAction::SafeDelete,
        source: Some(req.target.clone()),
        destination: None,
        bytes: req.bytes,
    }];
    StoragePlan {
        dry_run: req.dry_run,
        can_apply: issues.is_empty(),
        issues,
        steps,
        rollback_steps: Vec::new(),
    }
}

pub fn plan_delete_under_roots(
    req: &DeletePlanRequest,
    roots: &[PathBuf],
) -> Result<StoragePlan, StorageError> {
    let roots = canonical_roots(roots)?;
    let target = confine_plan_path(&req.target, &roots)?;
    Ok(plan_delete(&DeletePlanRequest {
        target,
        bytes: req.bytes,
        dry_run: req.dry_run,
        dry_run_approved: req.dry_run_approved,
    }))
}

pub fn ensure_plan_can_apply(plan: &StoragePlan) -> Result<(), StorageError> {
    if plan.can_apply && plan.issues.is_empty() {
        Ok(())
    } else {
        Err(StorageError::StagedMoveFailed {
            step: "plan",
            reason: format!("{:?}", plan.issues),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoragePlanExecution {
    pub applied_steps: Vec<StoragePlanStep>,
    pub rolled_back_steps: Vec<StoragePlanStep>,
    /// Rollback steps that were *attempted* but themselves failed, with the
    /// reason. TNG-003: a rollback step failing silently is worse than the
    /// original failure -- it means partial state was left behind with no
    /// record that it needs manual attention. Never populated on a fully
    /// successful rollback.
    pub rollback_failures: Vec<(StoragePlanStep, String)>,
}

impl StoragePlanExecution {
    /// True if no rollback step failed -- vacuously true when execution
    /// succeeded and no rollback was attempted at all. When execution did
    /// fail, callers must check this (not just that `execute_storage_plan`
    /// returned an error) before assuming the filesystem is back to its
    /// pre-execution state.
    pub fn rollback_fully_succeeded(&self) -> bool {
        self.rollback_failures.is_empty()
    }
}

/// Reconcile a durable storage plan with the filesystem before retrying it
/// after a crash or a database failure. The database checkpoint is an
/// ordering hint, not proof that the last filesystem syscall was durable:
/// filesystem state may be ahead of the checkpoint when the process dies
/// between the syscall and its checkpoint commit.
///
/// The returned indexes are safe to pass back to
/// `execute_storage_plan_under_roots_with_checkpoints`. Ambiguous states are
/// rejected instead of silently repeating or deleting a file. A completed
/// copy followed by a completed rename is inferred as a pair when only the
/// latter is visible in the filesystem.
pub fn reconcile_storage_plan_under_roots(
    plan: &StoragePlan,
    roots: &[PathBuf],
    checkpointed_steps: &[usize],
) -> Result<Vec<usize>, StorageError> {
    ensure_plan_can_apply(plan)?;
    let roots = canonical_roots(roots)?;
    validate_plan_paths_under_roots(plan, &roots)?;

    if let Some(index) = checkpointed_steps
        .iter()
        .copied()
        .find(|index| *index >= plan.steps.len())
    {
        return Err(StorageError::StagedMoveFailed {
            step: "reconcile",
            reason: format!(
                "checkpointed storage-plan step {index} is outside plan length {}",
                plan.steps.len()
            ),
        });
    }

    // These indexes come from the engine's durable worker checkpoint, not from
    // the public storage API. The API rejects caller-supplied indexes before a
    // plan reaches this boundary. A checkpoint is only an ordering hint: the
    // filesystem syscall may have been lost, overwritten, or never committed
    // before the row was durable. Validate every checkpoint against live state
    // and leave unapplied steps in the plan so execution can retry them.
    let checkpointed = checkpointed_steps.iter().copied().collect::<HashSet<_>>();
    let mut completed = HashSet::new();
    for index in checkpointed_steps {
        if storage_step_is_applied_for_roots(plan, *index, &roots, checkpointed.contains(index))? {
            completed.insert(*index);
        }
    }
    // First classify each step from its own source/destination state. Then
    // repeat the inference pass because a plan can contain more than one
    // copy/rename pair.
    loop {
        let mut changed = false;
        for (index, step) in plan.steps.iter().enumerate() {
            if completed.contains(&index) {
                continue;
            }
            if storage_step_is_applied_for_roots(
                plan,
                index,
                &roots,
                checkpointed.contains(&index),
            )? {
                completed.insert(index);
                changed = true;
                continue;
            }
            // A later rename can prove that its immediately preceding copy
            // already happened even when the staging path has disappeared.
            if let Some(next) = plan.steps.get(index + 1) {
                if completed.contains(&(index + 1))
                    && matches!(step.action, PlannedStorageAction::CopyVerifyRename)
                    && step.destination == next.source
                {
                    completed.insert(index);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    let mut completed = completed.into_iter().collect::<Vec<_>>();
    completed.sort_unstable();
    Ok(completed)
}

fn storage_step_is_applied_for_roots(
    plan: &StoragePlan,
    index: usize,
    roots: &[PathBuf],
    checkpointed: bool,
) -> Result<bool, StorageError> {
    #[cfg(unix)]
    {
        secure_fs::step_is_applied(plan, index, roots, checkpointed)
    }
    #[cfg(windows)]
    {
        windows_secure_fs::step_is_applied(plan, index, roots, checkpointed)
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        let _ = roots;
        storage_step_is_applied(plan, index, checkpointed)
    }
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn storage_step_is_applied(
    plan: &StoragePlan,
    index: usize,
    checkpointed: bool,
) -> Result<bool, StorageError> {
    let step = &plan.steps[index];
    let source = required_path(step.source.as_ref(), "reconcile-source")?;
    let destination = step.destination.as_deref();
    match step.action {
        PlannedStorageAction::Rename => {
            let destination = destination.ok_or_else(|| StorageError::StagedMoveFailed {
                step: "reconcile-destination",
                reason: "missing path".to_owned(),
            })?;
            let source_exists = path_exists_no_follow(source);
            let destination_exists = path_exists_no_follow(destination);
            if source_exists && destination_exists {
                return Err(StorageError::FilesystemStateUncertain {
                    step: "reconcile",
                    reason: format!(
                        "rename has both source and destination present: {} -> {}",
                        source.display(),
                        destination.display()
                    ),
                });
            }
            if !source_exists && destination_exists {
                if checkpointed {
                    // A durable checkpoint is written only after the rename
                    // syscall returns. Once the source is gone, the
                    // checkpoint is the only proof available that this
                    // destructive step was the operation that published the
                    // destination. Do not demand the now-deleted source or
                    // repeat the rename against a newly-created source.
                    verify_reconciled_length(destination, step.bytes)?;
                    return Ok(true);
                }
                let Some(previous) = index
                    .checked_sub(1)
                    .and_then(|previous| plan.steps.get(previous))
                else {
                    return Err(StorageError::FilesystemStateUncertain {
                        step: "reconcile",
                        reason: format!(
                            "cannot prove rename completion after source disappeared: {} -> {}",
                            source.display(),
                            destination.display()
                        ),
                    });
                };
                let previous_source =
                    required_path(previous.source.as_ref(), "reconcile-previous-source")?;
                if !matches!(previous.action, PlannedStorageAction::CopyVerifyRename)
                    || previous.destination.as_deref() != Some(source)
                    || !path_exists_no_follow(previous_source)
                {
                    return Err(StorageError::FilesystemStateUncertain {
                        step: "reconcile",
                        reason: format!(
                            "cannot prove rename completion after source disappeared: {} -> {}",
                            source.display(),
                            destination.display()
                        ),
                    });
                }
                reconcile_content_matches(previous_source, destination)?;
                return Ok(true);
            }
            if checkpointed {
                return Err(StorageError::FilesystemStateUncertain {
                    step: "reconcile",
                    reason: format!(
                        "checkpointed rename is not in its committed state: {} -> {}",
                        source.display(),
                        destination.display()
                    ),
                });
            }
            Ok(false)
        }
        PlannedStorageAction::CopyVerifyRename => {
            let destination = destination.ok_or_else(|| StorageError::StagedMoveFailed {
                step: "reconcile-destination",
                reason: "missing path".to_owned(),
            })?;
            if !path_exists_no_follow(destination) {
                return Ok(false);
            }
            if !path_exists_no_follow(source) {
                return Err(StorageError::FilesystemStateUncertain {
                    step: "reconcile",
                    reason: format!(
                        "cannot prove copy completion after source disappeared: {} -> {}",
                        source.display(),
                        destination.display()
                    ),
                });
            }
            reconcile_content_matches(source, destination)?;
            Ok(true)
        }
        PlannedStorageAction::ImportExisting => {
            let destination = destination.ok_or_else(|| StorageError::StagedMoveFailed {
                step: "reconcile-destination",
                reason: "missing path".to_owned(),
            })?;
            if !path_exists_no_follow(destination) {
                return Ok(false);
            }
            if !path_exists_no_follow(source) {
                return Err(StorageError::FilesystemStateUncertain {
                    step: "reconcile",
                    reason: format!(
                        "cannot prove import completion after source disappeared: {} -> {}",
                        source.display(),
                        destination.display()
                    ),
                });
            }
            reconcile_content_matches(source, destination)?;
            Ok(true)
        }
        PlannedStorageAction::SafeDelete | PlannedStorageAction::SafeDeleteIfPresent => {
            let source_exists = path_exists_no_follow(source);
            if checkpointed && source_exists {
                return Err(StorageError::FilesystemStateUncertain {
                    step: "reconcile",
                    reason: format!(
                        "checkpointed delete target reappeared and will not be deleted again: {}",
                        source.display()
                    ),
                });
            }
            Ok(!source_exists)
        }
        PlannedStorageAction::PruneEmptyDirs => Ok(!path_exists_no_follow(source)),
    }
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn reconcile_content_matches(source: &Path, destination: &Path) -> Result<(), StorageError> {
    verify_content_matches(source, destination).map_err(|error| match error {
        StorageError::FilesystemStateUncertain { .. } => error,
        error => StorageError::FilesystemStateUncertain {
            step: "reconcile",
            reason: error.to_string(),
        },
    })
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn verify_reconciled_length(path: &Path, expected_bytes: u64) -> Result<(), StorageError> {
    if expected_bytes == u64::MAX {
        return Ok(());
    }
    let actual = path_content_len(path).map_err(|error| match error {
        StorageError::FilesystemStateUncertain { .. } => error,
        error => StorageError::FilesystemStateUncertain {
            step: "reconcile",
            reason: error.to_string(),
        },
    })?;
    if actual == expected_bytes {
        Ok(())
    } else {
        Err(StorageError::FilesystemStateUncertain {
            step: "reconcile",
            reason: format!(
                "checkpointed destination has {actual} bytes, expected {expected_bytes}: {}",
                path.display()
            ),
        })
    }
}

#[cfg(test)]
pub(crate) fn execute_storage_plan(
    plan: &StoragePlan,
) -> Result<StoragePlanExecution, StorageError> {
    execute_storage_plan_with_checkpoints(plan, &[], |_, _| Ok(()))
}

#[cfg(test)]
pub(crate) fn execute_storage_plan_with_checkpoints<F>(
    plan: &StoragePlan,
    completed_steps: &[usize],
    checkpoint_step: F,
) -> Result<StoragePlanExecution, StorageError>
where
    F: FnMut(usize, &StoragePlanStep) -> Result<(), StorageError>,
{
    let no_control = || Ok(());
    let checkpointed = completed_steps.iter().copied().collect::<HashSet<_>>();
    execute_storage_plan_with_executor(
        plan,
        completed_steps,
        checkpoint_step,
        execute_step,
        rollback_plan,
        |index, _step| storage_step_is_applied(plan, index, checkpointed.contains(&index)),
        &no_control,
    )
}

fn execute_storage_plan_with_executor<F, E, R, A>(
    plan: &StoragePlan,
    completed_steps: &[usize],
    mut checkpoint_step: F,
    execute_step_fn: E,
    rollback_plan_fn: R,
    step_is_applied_fn: A,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<StoragePlanExecution, StorageError>
where
    F: FnMut(usize, &StoragePlanStep) -> Result<(), StorageError>,
    E: Fn(&StoragePlanStep) -> Result<(), StorageError>,
    R: Fn(&StoragePlan) -> (Vec<StoragePlanStep>, Vec<(StoragePlanStep, String)>),
    A: Fn(usize, &StoragePlanStep) -> Result<bool, StorageError>,
{
    ensure_plan_can_apply(plan)?;
    if plan.dry_run {
        return Ok(StoragePlanExecution::default());
    }

    if let Some(index) = completed_steps
        .iter()
        .copied()
        .find(|index| *index >= plan.steps.len())
    {
        return Err(StorageError::StagedMoveFailed {
            step: "checkpoint",
            reason: format!(
                "checkpointed storage-plan step {index} is outside plan length {}",
                plan.steps.len()
            ),
        });
    }

    // This list is populated from the engine's durable worker checkpoint. The
    // public API rejects caller-supplied indexes; a checkpoint is still only
    // an ordering hint and must be validated against live filesystem state
    // before a step is skipped.
    let mut completed = HashSet::new();
    for index in completed_steps {
        if let Err(error) = check_control() {
            if cancellation_can_rollback(plan, &completed, false) && is_cancellation_error(&error) {
                return Err(rollback_cancelled_progress(error, plan, &rollback_plan_fn));
            }
            return Err(error);
        }
        if step_is_applied_fn(*index, &plan.steps[*index])? {
            completed.insert(*index);
        }
    }
    let mut inferred_completed = HashSet::new();
    // Filesystem mutations can commit before their durable checkpoint. Treat
    // an already-applied step as complete and infer a preceding copy when its
    // following rename is already present. This makes a retry safe even when
    // the caller lost the checkpoint record entirely, while still rejecting
    // ambiguous states in the same way as restart reconciliation.
    loop {
        let mut changed = false;
        for (index, step) in plan.steps.iter().enumerate() {
            if completed.contains(&index) {
                continue;
            }
            if let Err(error) = check_control() {
                if cancellation_can_rollback(plan, &completed, false)
                    && is_cancellation_error(&error)
                {
                    return Err(rollback_cancelled_progress(error, plan, &rollback_plan_fn));
                }
                return Err(error);
            }
            if step_is_applied_fn(index, step)? {
                completed.insert(index);
                inferred_completed.insert(index);
                changed = true;
                continue;
            }
            if let Some(next) = plan.steps.get(index + 1) {
                if completed.contains(&(index + 1))
                    && matches!(step.action, PlannedStorageAction::CopyVerifyRename)
                    && step.destination == next.source
                {
                    completed.insert(index);
                    inferred_completed.insert(index);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    let mut execution = StoragePlanExecution::default();
    for (index, step) in plan.steps.iter().enumerate() {
        if completed.contains(&index) {
            if inferred_completed.contains(&index) {
                if let Err(error) = checkpoint_step(index, step) {
                    if cancellation_can_rollback(plan, &completed, false)
                        && is_cancellation_error(&error)
                    {
                        return Err(rollback_cancelled_progress(error, plan, &rollback_plan_fn));
                    }
                    return Err(error);
                }
            }
            execution.applied_steps.push(step.clone());
            continue;
        }
        if let Err(error) = check_control() {
            if cancellation_can_rollback(plan, &completed, false) && is_cancellation_error(&error) {
                return Err(rollback_cancelled_progress(error, plan, &rollback_plan_fn));
            }
            return Err(error);
        }
        if let Err(error) = execute_step_fn(step) {
            let (rolled_back, rollback_failures) = rollback_plan_fn(plan);
            execution.rolled_back_steps = rolled_back;
            execution.rollback_failures = rollback_failures;
            // TNG-003: a rollback step that itself fails is worse than the
            // original failure -- it means files were left in a partial
            // state with nothing recording that fact. Previously this was
            // silently dropped (rollback_plan only returned steps that
            // succeeded); now it's folded into the error message, since
            // that message is the only thing every current caller actually
            // reads (see engine.rs's execute_storage_plan_job, which
            // persists `error.to_string()` as the job's failure reason and
            // otherwise discards the returned StoragePlanExecution).
            let reason = if execution.rollback_failures.is_empty() {
                error.to_string()
            } else {
                let failures = execution
                    .rollback_failures
                    .iter()
                    .map(|(step, reason)| {
                        format!(
                            "{:?} {} -> {}: {reason}",
                            step.action,
                            step.source
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_default(),
                            step.destination
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_default(),
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                format!(
                    "{error}; ADDITIONALLY {} rollback step(s) failed and left the filesystem in a partial state requiring manual attention: {failures}",
                    execution.rollback_failures.len()
                )
            };
            // Once a destructive step has committed, a later ordinary error
            // is no longer a simple retry. For example, a cross-filesystem
            // move publishes the destination and only then deletes the
            // source; if that final delete fails, both paths remain live and
            // the engine must not mark the job as an ordinary failed move.
            // The production rollback plan only removes staging, so preserve
            // the ambiguous filesystem state for manual recovery even when
            // all configured rollback steps happened to succeed.
            let committed_destructive_step = has_committed_destructive_step(plan, &completed);
            if error.requires_manual_recovery()
                || !execution.rollback_failures.is_empty()
                || committed_destructive_step
            {
                let step = match &error {
                    StorageError::FilesystemStateUncertain { step, .. } => *step,
                    _ if !execution.rollback_failures.is_empty() => "rollback",
                    _ if committed_destructive_step => "execute_after_destructive_step",
                    _ => "execute",
                };
                return Err(StorageError::FilesystemStateUncertain { step, reason });
            }
            // Preserve the control-plane marker when a cooperative pause or
            // cancellation interrupts a large copy. The storage worker uses
            // these markers to distinguish a resumable pause from a real
            // filesystem failure; flattening every error to `execute` would
            // turn a user pause in the middle of a file into a terminal job
            // failure after rollback.
            let step = match &error {
                StorageError::StagedMoveFailed {
                    step: marker @ ("pause" | "cancel"),
                    ..
                } => *marker,
                _ => "execute",
            };
            return Err(StorageError::StagedMoveFailed { step, reason });
        }
        // Record the filesystem mutation before invoking the checkpoint
        // callback. The callback persists the checkpoint and then checks the
        // control plane, so a cancellation can arrive after this step is
        // live but before the callback returns.
        completed.insert(index);
        if let Err(error) = checkpoint_step(index, step) {
            if cancellation_can_rollback(plan, &completed, false) && is_cancellation_error(&error) {
                return Err(rollback_cancelled_progress(error, plan, &rollback_plan_fn));
            }
            return Err(error);
        }
        execution.applied_steps.push(step.clone());
    }
    Ok(execution)
}

fn has_committed_destructive_step(plan: &StoragePlan, completed: &HashSet<usize>) -> bool {
    completed.iter().any(|index| {
        plan.steps.get(*index).is_some_and(|step| {
            matches!(
                step.action,
                PlannedStorageAction::Rename
                    | PlannedStorageAction::SafeDelete
                    | PlannedStorageAction::SafeDeleteIfPresent
                    | PlannedStorageAction::PruneEmptyDirs
            )
        })
    })
}

pub(crate) fn is_cancellation_error(error: &StorageError) -> bool {
    matches!(
        error,
        StorageError::Cancelled | StorageError::StagedMoveFailed { step: "cancel", .. }
    )
}

fn cancellation_can_rollback(
    plan: &StoragePlan,
    completed: &HashSet<usize>,
    current_step_committed: bool,
) -> bool {
    (current_step_committed || !completed.is_empty())
        && completed.iter().all(|index| {
            plan.steps.get(*index).is_some_and(|step| {
                !matches!(
                    step.action,
                    PlannedStorageAction::Rename
                        | PlannedStorageAction::SafeDelete
                        | PlannedStorageAction::SafeDeleteIfPresent
                        | PlannedStorageAction::PruneEmptyDirs
                )
            })
        })
}

fn rollback_cancelled_progress<R>(
    error: StorageError,
    plan: &StoragePlan,
    rollback_plan_fn: &R,
) -> StorageError
where
    R: Fn(&StoragePlan) -> (Vec<StoragePlanStep>, Vec<(StoragePlanStep, String)>),
{
    let (_, rollback_failures) = rollback_plan_fn(plan);
    if rollback_failures.is_empty() {
        return error;
    }
    let failures = rollback_failures
        .iter()
        .map(|(step, reason)| {
            format!(
                "{:?} {} -> {}: {reason}",
                step.action,
                step.source
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                step.destination
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    StorageError::FilesystemStateUncertain {
        step: "rollback",
        reason: format!(
            "{error}; ADDITIONALLY {} rollback step(s) failed and left the filesystem in a partial state requiring manual attention: {failures}",
            rollback_failures.len()
        ),
    }
}

pub fn execute_storage_plan_under_roots(
    plan: &StoragePlan,
    roots: &[PathBuf],
) -> Result<StoragePlanExecution, StorageError> {
    execute_storage_plan_under_roots_with_checkpoints(plan, roots, &[], |_, _| Ok(()))
}

pub fn execute_storage_plan_under_roots_with_checkpoints<F>(
    plan: &StoragePlan,
    roots: &[PathBuf],
    completed_steps: &[usize],
    checkpoint_step: F,
) -> Result<StoragePlanExecution, StorageError>
where
    F: FnMut(usize, &StoragePlanStep) -> Result<(), StorageError>,
{
    let no_control = || Ok(());
    execute_storage_plan_under_roots_with_checkpoints_and_control(
        plan,
        roots,
        completed_steps,
        checkpoint_step,
        no_control,
    )
}

/// Execute a root-confined storage plan while cooperatively observing a
/// caller-owned control signal during large filesystem steps.
///
/// The callback must return `Ok(())` to continue or a storage error to abort
/// the current step. Aborting a copy/import step rolls back its staging path;
/// rollback itself is deliberately not interruptible so a partial filesystem
/// mutation is not abandoned without cleanup.
pub fn execute_storage_plan_under_roots_with_checkpoints_and_control<F, C>(
    plan: &StoragePlan,
    roots: &[PathBuf],
    completed_steps: &[usize],
    checkpoint_step: F,
    check_control: C,
) -> Result<StoragePlanExecution, StorageError>
where
    F: FnMut(usize, &StoragePlanStep) -> Result<(), StorageError>,
    C: Fn() -> Result<(), StorageError>,
{
    ensure_plan_can_apply(plan)?;
    let roots = canonical_roots(roots)?;
    validate_plan_paths_under_roots(plan, &roots)?;
    if plan.dry_run {
        return Ok(StoragePlanExecution::default());
    }
    let checkpointed = completed_steps.iter().copied().collect::<HashSet<_>>();
    #[cfg(unix)]
    {
        let check_control_ref: &dyn Fn() -> Result<(), StorageError> = &check_control;
        execute_storage_plan_with_executor(
            plan,
            completed_steps,
            checkpoint_step,
            |step| secure_fs::execute_step_with_control(step, &roots, check_control_ref),
            |plan| secure_fs::rollback_plan(plan, &roots),
            |index, _step| {
                secure_fs::step_is_applied(plan, index, &roots, checkpointed.contains(&index))
            },
            check_control_ref,
        )
    }
    #[cfg(windows)]
    {
        let check_control_ref: &dyn Fn() -> Result<(), StorageError> = &check_control;
        execute_storage_plan_with_executor(
            plan,
            completed_steps,
            checkpoint_step,
            |step| windows_secure_fs::execute_step_with_control(step, &roots, check_control_ref),
            |plan| windows_secure_fs::rollback_plan(plan, &roots),
            |index, _step| {
                windows_secure_fs::step_is_applied(
                    plan,
                    index,
                    &roots,
                    checkpointed.contains(&index),
                )
            },
            check_control_ref,
        )
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        // Other non-Unix platforms retain a portable path executor. Windows
        // uses the handle-relative implementation above.
        let check_control_ref: &dyn Fn() -> Result<(), StorageError> = &check_control;
        execute_storage_plan_with_executor(
            plan,
            completed_steps,
            checkpoint_step,
            |step| execute_step_with_control(step, check_control_ref),
            rollback_plan,
            |index, _step| storage_step_is_applied(plan, index, checkpointed.contains(&index)),
            &check_control,
        )
    }
}

/// Run the plan's idempotent rollback steps under the same configured roots
/// used for execution. This is used when a queued or paused job is cancelled
/// after it has already committed non-destructive staging work.
pub fn rollback_storage_plan_under_roots(
    plan: &StoragePlan,
    roots: &[PathBuf],
) -> Result<StoragePlanExecution, StorageError> {
    ensure_plan_can_apply(plan)?;
    let roots = canonical_roots(roots)?;
    validate_plan_paths_under_roots(plan, &roots)?;
    if plan.dry_run {
        return Ok(StoragePlanExecution::default());
    }
    #[cfg(unix)]
    let (rolled_back_steps, rollback_failures) = secure_fs::rollback_plan(plan, &roots);
    #[cfg(windows)]
    let (rolled_back_steps, rollback_failures) = windows_secure_fs::rollback_plan(plan, &roots);
    #[cfg(all(not(unix), not(windows)))]
    let (rolled_back_steps, rollback_failures) = rollback_plan(plan);
    Ok(StoragePlanExecution {
        applied_steps: Vec::new(),
        rolled_back_steps,
        rollback_failures,
    })
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn execute_step(step: &StoragePlanStep) -> Result<(), StorageError> {
    let no_control = || Ok(());
    execute_step_with_control(step, &no_control)
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn execute_step_with_control(
    step: &StoragePlanStep,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    check_control()?;
    if let Some(source) = &step.source {
        reject_symlink_ancestors(source, "source-ancestor")?;
    }
    if let Some(destination) = &step.destination {
        reject_symlink_ancestors(destination, "destination-ancestor")?;
    }
    match step.action {
        PlannedStorageAction::ImportExisting => {
            let source = required_path(step.source.as_ref(), "import-source")?;
            let destination = required_path(step.destination.as_ref(), "import-destination")?;
            ensure_destination_available(destination)?;
            create_parent(destination)?;
            reject_symlink(source, "import-source")?;
            // Check before creating a hard link so a stale expected size does
            // not leave a destination behind after the import is rejected.
            verify_path_len_with_control(source, step.bytes, check_control)?;
            #[cfg(windows)]
            {
                // CreateHardLink resolves the source by path and follows a
                // reparse point. Since Windows has no handle-relative
                // hard-link primitive here, a path swap after the no-follow
                // checks could link an unintended target. Copy from the
                // no-follow source handle instead.
                copy_verify_with_control(source, destination, step.bytes, check_control)
            }
            #[cfg(not(windows))]
            {
                match std::fs::hard_link(source, destination) {
                    Ok(()) => {
                        let verification =
                            verify_path_len_with_control(destination, step.bytes, check_control);
                        if let Err(error) = verification {
                            if let Err(cleanup_error) = std::fs::remove_file(destination) {
                                return Err(StorageError::FilesystemStateUncertain {
                                step: "import-cleanup",
                                reason: format!(
                                    "{error}; failed to remove hard-link destination {}: {cleanup_error}",
                                    destination.display()
                                ),
                            });
                            }
                            return Err(error);
                        }
                        Ok(())
                    }
                    Err(_) => {
                        copy_verify_with_control(source, destination, step.bytes, check_control)
                    }
                }
            }
        }
        PlannedStorageAction::Rename => {
            let source = required_path(step.source.as_ref(), "rename-source")?;
            let destination = required_path(step.destination.as_ref(), "rename-destination")?;
            ensure_destination_available(destination)?;
            create_parent(destination)?;
            reject_symlink(source, "rename-source")?;
            // Validate the source before the destructive rename. If the
            // caller supplied a stale size, moving first would leave the
            // source gone when the post-rename check failed.
            verify_path_len_with_control(source, step.bytes, check_control)?;
            rename_plan_no_replace(source, destination)
                .map_err(|e| StorageError::io(destination.display().to_string(), e))?;
            if let Err(error) = verify_path_len_with_control(destination, step.bytes, check_control)
            {
                // The source has disappeared by this point. Restore it
                // before returning a verification failure; never leave a
                // same-filesystem move stranded solely because the post-move
                // check observed a changed file.
                if !path_exists_no_follow(source)
                    && rename_plan_no_replace(destination, source).is_ok()
                {
                    return Err(error);
                }
                return Err(StorageError::FilesystemStateUncertain {
                    step: "rename-verify",
                    reason: format!(
                        "{error}; failed to restore {} after verification failure",
                        source.display()
                    ),
                });
            }
            Ok(())
        }
        PlannedStorageAction::CopyVerifyRename => {
            let source = required_path(step.source.as_ref(), "copy-source")?;
            let destination = required_path(step.destination.as_ref(), "copy-destination")?;
            ensure_destination_available(destination)?;
            create_parent(destination)?;
            copy_verify_with_control(source, destination, step.bytes, check_control)
        }
        PlannedStorageAction::SafeDelete => {
            let source = required_path(step.source.as_ref(), "delete-source")?;
            safe_delete_with_control(source, false, check_control)
        }
        PlannedStorageAction::SafeDeleteIfPresent => {
            let source = required_path(step.source.as_ref(), "delete-source")?;
            safe_delete_with_control(source, true, check_control)
        }
        PlannedStorageAction::PruneEmptyDirs => {
            let start = required_path(step.source.as_ref(), "prune-source")?;
            let root = required_path(step.destination.as_ref(), "prune-root")?;
            prune_empty_dirs_with_control(start, root, check_control)
        }
    }
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn safe_delete_with_control(
    path: &Path,
    missing_ok: bool,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    check_control()?;
    let _parent_guard = match crate::open::hold_parent_dirs_no_follow(path) {
        Ok(guard) => guard,
        Err(error) if missing_ok && error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(StorageError::io(path.display().to_string(), error)),
    };
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if missing_ok && error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(StorageError::StagedMoveFailed {
                step: "delete-source",
                reason: format!(
                    "path missing during move/import execution: {}",
                    path.display()
                ),
            });
        }
        Err(error) => return Err(StorageError::io(path.display().to_string(), error)),
    };
    let file_type = metadata.file_type();
    let root_is_reparse_point = metadata_is_reparse_point(&metadata);
    let root_is_directory = file_type.is_dir() && !root_is_reparse_point;
    let mut removed = false;
    let result = if root_is_reparse_point {
        check_control()?;
        remove_reparse_point_no_follow(path, &metadata)
            .map_err(|error| StorageError::io(path.display().to_string(), error))?;
        removed = true;
        Ok(())
    } else if root_is_directory {
        remove_directory_contents_with_control(path, 0, check_control, &mut removed).and_then(
            |()| {
                check_control()?;
                std::fs::remove_dir(path)
                    .map_err(|error| StorageError::io(path.display().to_string(), error))?;
                removed = true;
                Ok(())
            },
        )
    } else if file_type.is_file() {
        check_control()?;
        std::fs::remove_file(path)
            .map_err(|error| StorageError::io(path.display().to_string(), error))?;
        removed = true;
        Ok(())
    } else {
        Err(StorageError::StagedMoveFailed {
            step: "delete-source",
            reason: format!("unsupported file type: {}", path.display()),
        })
    };

    match result {
        Err(error) if removed || (root_is_directory && !is_cancellation_error(&error)) => {
            Err(StorageError::FilesystemStateUncertain {
                step: "delete",
                reason: format!(
                    "failed to remove {} after deletion may have partially applied: {error}",
                    path.display()
                ),
            })
        }
        result => result,
    }
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn remove_directory_contents_with_control(
    path: &Path,
    depth: usize,
    check_control: &dyn Fn() -> Result<(), StorageError>,
    removed: &mut bool,
) -> Result<(), StorageError> {
    check_control()?;
    ensure_storage_tree_depth(depth, path, "delete-tree-depth")?;
    let _directory_guard = crate::open::hold_directory_no_follow(path)
        .map_err(|error| StorageError::io(path.display().to_string(), error))?;
    for entry in std::fs::read_dir(path)
        .map_err(|error| StorageError::io(path.display().to_string(), error))?
    {
        check_control()?;
        let entry = entry.map_err(|error| StorageError::io(path.display().to_string(), error))?;
        let child = entry.path();
        let metadata = std::fs::symlink_metadata(&child)
            .map_err(|error| StorageError::io(child.display().to_string(), error))?;
        let file_type = metadata.file_type();
        if metadata_is_reparse_point(&metadata) {
            check_control()?;
            remove_reparse_point_no_follow(&child, &metadata)
                .map_err(|error| StorageError::io(child.display().to_string(), error))?;
            *removed = true;
        } else if file_type.is_dir() {
            remove_directory_contents_with_control(
                &child,
                depth.saturating_add(1),
                check_control,
                removed,
            )?;
            check_control()?;
            std::fs::remove_dir(&child)
                .map_err(|error| StorageError::io(child.display().to_string(), error))?;
            *removed = true;
        } else if file_type.is_file() {
            check_control()?;
            std::fs::remove_file(&child)
                .map_err(|error| StorageError::io(child.display().to_string(), error))?;
            *removed = true;
        } else {
            return Err(StorageError::StagedMoveFailed {
                step: "delete-source",
                reason: format!("unsupported file type: {}", child.display()),
            });
        }
    }
    Ok(())
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn prune_empty_dirs_with_control(
    mut current: &Path,
    root: &Path,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    if !current.starts_with(root) {
        return Err(StorageError::StagedMoveFailed {
            step: "prune-root",
            reason: format!(
                "directory {} is outside prune root {}",
                current.display(),
                root.display()
            ),
        });
    }
    let mut removed = false;
    while current != root {
        if let Err(error) = check_control() {
            if removed {
                return Err(StorageError::FilesystemStateUncertain {
                    step: "prune",
                    reason: format!(
                        "{error}; pruning of {} was only partially applied",
                        root.display()
                    ),
                });
            }
            return Err(error);
        }
        let _parent_guard = match crate::open::hold_parent_dirs_no_follow(current) {
            Ok(guard) => guard,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                current = current
                    .parent()
                    .ok_or_else(|| StorageError::StagedMoveFailed {
                        step: "prune-parent",
                        reason: format!("directory has no parent: {}", current.display()),
                    })?;
                continue;
            }
            Err(error) if removed => {
                return Err(StorageError::FilesystemStateUncertain {
                    step: "prune",
                    reason: format!(
                        "failed to hold {} after directory pruning partially applied: {error}",
                        current.display()
                    ),
                });
            }
            Err(error) => return Err(StorageError::io(current.display().to_string(), error)),
        };
        let metadata = match std::fs::symlink_metadata(current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                current = current
                    .parent()
                    .ok_or_else(|| StorageError::StagedMoveFailed {
                        step: "prune-parent",
                        reason: format!("directory has no parent: {}", current.display()),
                    })?;
                continue;
            }
            Err(error) => {
                if removed {
                    return Err(StorageError::FilesystemStateUncertain {
                        step: "prune",
                        reason: format!(
                            "failed to inspect {} after directory pruning partially applied: {error}",
                            current.display()
                        ),
                    });
                }
                return Err(StorageError::io(current.display().to_string(), error));
            }
        };
        if metadata_is_reparse_point(&metadata) {
            if removed {
                return Err(StorageError::FilesystemStateUncertain {
                    step: "prune",
                    reason: format!(
                        "encountered a symlink at {} after directory pruning partially applied",
                        current.display()
                    ),
                });
            }
            return Err(unsafe_symlink_error(current, "prune-source"));
        }
        if !metadata.is_dir() {
            if removed {
                return Err(StorageError::FilesystemStateUncertain {
                    step: "prune",
                    reason: format!(
                        "encountered a non-directory at {} after directory pruning partially applied",
                        current.display()
                    ),
                });
            }
            return Err(StorageError::StagedMoveFailed {
                step: "prune-source",
                reason: format!("not a directory: {}", current.display()),
            });
        }
        match std::fs::remove_dir(current) {
            Ok(()) => removed = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => break,
            Err(error) => {
                if removed {
                    return Err(StorageError::FilesystemStateUncertain {
                        step: "prune",
                        reason: format!(
                            "failed to remove {} after directory pruning partially applied: {error}",
                            current.display()
                        ),
                    });
                }
                return Err(StorageError::io(current.display().to_string(), error));
            }
        }
        current = current
            .parent()
            .ok_or_else(|| StorageError::StagedMoveFailed {
                step: "prune-parent",
                reason: format!("directory has no parent: {}", current.display()),
            })?;
    }
    Ok(())
}

fn canonical_roots(roots: &[PathBuf]) -> Result<Vec<PathBuf>, StorageError> {
    if roots.is_empty() {
        return Err(StorageError::StagedMoveFailed {
            step: "roots",
            reason: "at least one storage root is required".to_string(),
        });
    }
    roots
        .iter()
        .map(|root| {
            std::fs::canonicalize(root).map_err(|e| StorageError::io(root.display().to_string(), e))
        })
        .collect()
}

fn confine_plan_path(path: &Path, roots: &[PathBuf]) -> Result<PathBuf, StorageError> {
    let root = roots
        .iter()
        .find(|root| path.starts_with(root))
        .ok_or_else(|| StorageError::StagedMoveFailed {
            step: "plan-path",
            reason: format!("path outside configured storage roots: {}", path.display()),
        })?;
    let relative = path
        .strip_prefix(root)
        .map_err(|_| StorageError::StagedMoveFailed {
            step: "plan-path",
            reason: format!("path outside configured storage roots: {}", path.display()),
        })?;
    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(value) => {
                value
                    .to_str()
                    .ok_or_else(|| StorageError::StagedMoveFailed {
                        step: "plan-path",
                        reason: format!("path is not valid UTF-8: {}", path.display()),
                    })
            }
            _ => Err(StorageError::StagedMoveFailed {
                step: "plan-path",
                reason: format!("unsafe path component: {}", path.display()),
            }),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let relative = SafeRelPath::from_components(&components, cfg!(windows)).map_err(|error| {
        StorageError::StagedMoveFailed {
            step: "plan-path",
            reason: error.to_string(),
        }
    })?;
    Ok(relative.resolve(root))
}

fn validate_plan_paths_under_roots(
    plan: &StoragePlan,
    roots: &[PathBuf],
) -> Result<(), StorageError> {
    for step in plan.steps.iter().chain(plan.rollback_steps.iter()) {
        let allow_root_boundary = matches!(step.action, PlannedStorageAction::PruneEmptyDirs);
        if let Some(source) = &step.source {
            let resolved = ensure_path_under_roots(source, roots, "source-root")?;
            if !allow_root_boundary && roots.contains(&resolved) {
                return Err(StorageError::StagedMoveFailed {
                    step: "source-root",
                    reason: format!(
                        "refusing to operate on configured storage root: {}",
                        source.display()
                    ),
                });
            }
        }
        if let Some(destination) = &step.destination {
            let resolved = ensure_path_under_roots(destination, roots, "destination-root")?;
            if !allow_root_boundary && roots.contains(&resolved) {
                return Err(StorageError::StagedMoveFailed {
                    step: "destination-root",
                    reason: format!(
                        "refusing to operate on configured storage root: {}",
                        destination.display()
                    ),
                });
            }
        }
    }
    Ok(())
}

fn ensure_path_under_roots(
    path: &Path,
    roots: &[PathBuf],
    step: &'static str,
) -> Result<PathBuf, StorageError> {
    let resolved = resolve_confined_path(path)?;
    if roots.iter().any(|root| resolved.starts_with(root)) {
        Ok(resolved)
    } else {
        Err(StorageError::StagedMoveFailed {
            step,
            reason: format!("path outside configured storage roots: {}", path.display()),
        })
    }
}

pub(crate) fn resolve_confined_path(path: &Path) -> Result<PathBuf, StorageError> {
    let has_non_absolute_prefix = path
        .components()
        .any(|component| matches!(component, Component::Prefix(_)))
        && !path.is_absolute();
    if has_non_absolute_prefix
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(StorageError::StagedMoveFailed {
            step: "path",
            reason: format!("unsafe path component: {}", path.display()),
        });
    }
    if path.exists() {
        return std::fs::canonicalize(path)
            .map_err(|e| StorageError::io(path.display().to_string(), e));
    }

    let mut tail = Vec::new();
    let mut ancestor = path;
    while !ancestor.exists() {
        let name = ancestor
            .file_name()
            .ok_or_else(|| StorageError::StagedMoveFailed {
                step: "path",
                reason: format!("path has no existing ancestor: {}", path.display()),
            })?;
        tail.push(name.to_os_string());
        ancestor = ancestor
            .parent()
            .ok_or_else(|| StorageError::StagedMoveFailed {
                step: "path",
                reason: format!("path has no existing ancestor: {}", path.display()),
            })?;
    }

    let mut resolved = std::fs::canonicalize(ancestor)
        .map_err(|e| StorageError::io(ancestor.display().to_string(), e))?;
    for part in tail.iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}

/// Runs every rollback step, continuing past individual failures (a failed
/// rollback step must not prevent attempting the rest -- each one is
/// independent staging/cleanup). Returns which steps succeeded and, just as
/// importantly, which ones failed and why: TNG-003 explicitly calls out
/// that dropping failed rollback steps silently is not acceptable.
#[cfg(any(all(not(unix), not(windows)), test))]
fn rollback_plan(plan: &StoragePlan) -> (Vec<StoragePlanStep>, Vec<(StoragePlanStep, String)>) {
    let mut rolled_back = Vec::new();
    let mut failures = Vec::new();
    for step in &plan.rollback_steps {
        match execute_step(step) {
            Ok(()) => rolled_back.push(step.clone()),
            Err(error) => failures.push((step.clone(), error.to_string())),
        }
    }
    (rolled_back, failures)
}

#[cfg(any(not(unix), test))]
pub(crate) fn required_path<'a>(
    path: Option<&'a PathBuf>,
    step: &'static str,
) -> Result<&'a Path, StorageError> {
    path.map(PathBuf::as_path)
        .ok_or_else(|| StorageError::StagedMoveFailed {
            step,
            reason: "missing path".to_string(),
        })
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn create_parent(path: &Path) -> Result<(), StorageError> {
    if let Some(parent) = path.parent() {
        crate::open::create_dir_all_no_follow(parent)
            .map_err(|e| StorageError::io(parent.display().to_string(), e))?;
    }
    Ok(())
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn rename_plan_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        crate::open::rename_no_replace(source, destination)
    }
    #[cfg(unix)]
    {
        std::fs::rename(source, destination)
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        let _ = (source, destination);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "atomic no-replace rename is unavailable on this platform",
        ))
    }
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn ensure_destination_available(path: &Path) -> Result<(), StorageError> {
    if path_exists_no_follow(path) {
        return Err(StorageError::StagedMoveFailed {
            step: "destination",
            reason: format!("destination exists: {}", path.display()),
        });
    }
    Ok(())
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn copy_verify_with_control(
    source: &Path,
    destination: &Path,
    expected_bytes: u64,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    let mut destination_created = false;
    let result = copy_verify_inner(
        source,
        destination,
        expected_bytes,
        &mut destination_created,
        check_control,
    );
    if let Err(error) = result {
        if destination_created {
            if let Err(cleanup_error) = remove_partial_destination(destination) {
                return Err(StorageError::FilesystemStateUncertain {
                    step: "copy-cleanup",
                    reason: format!(
                        "{error}; failed to remove partial destination {}: {cleanup_error}",
                        destination.display()
                    ),
                });
            }
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn copy_verify_inner(
    source: &Path,
    destination: &Path,
    expected_bytes: u64,
    destination_created: &mut bool,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    check_control()?;
    let _source_parent_guard = crate::open::hold_parent_dirs_no_follow(source)
        .map_err(|error| StorageError::io(source.display().to_string(), error))?;
    let _destination_parent_guard = crate::open::hold_parent_dirs_no_follow(destination)
        .map_err(|error| StorageError::io(destination.display().to_string(), error))?;
    let metadata = safe_symlink_metadata(source, "copy-source")?;
    let file_type = metadata.file_type();
    if metadata_is_reparse_point(&metadata) {
        return Err(unsafe_symlink_error(source, "copy-source"));
    }
    if file_type.is_dir() {
        ensure_storage_tree_depth(0, source, "copy-tree-depth")?;
        std::fs::create_dir(destination)
            .map_err(|e| StorageError::io(destination.display().to_string(), e))?;
        *destination_created = true;
        copy_dir_contents(source, destination, 0, check_control)?;
    } else if file_type.is_file() {
        copy_file_with_control(source, destination, destination_created, check_control)?;
    } else {
        return Err(StorageError::StagedMoveFailed {
            step: "copy-source",
            reason: format!("unsupported file type: {}", source.display()),
        });
    }
    // TNG-003: length matching alone can't catch a bit-flip (same size,
    // wrong bytes) from a bad disk, a bus error during the copy, or a
    // torn/partial write that happens to land on the right length. Verify
    // actual content -- source is re-read here rather than hashed
    // incrementally during the copy above, so this also catches corruption
    // introduced by the destination write path itself, not just a bug in
    // the copy loop.
    verify_path_len_with_control(destination, expected_bytes, check_control)?;
    verify_content_matches_with_control(source, destination, check_control)
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn remove_partial_destination(destination: &Path) -> Result<(), StorageError> {
    let no_control = || Ok(());
    safe_delete_with_control(destination, true, &no_control)
}

/// Recursively verifies that every regular file under `source` has bytes
/// identical to its counterpart under `destination`, via a streaming SHA-1
/// content hash (never loads a whole file into memory). Never follows
/// symlinks on either side.
#[cfg(any(all(not(unix), not(windows)), test))]
fn verify_content_matches(source: &Path, destination: &Path) -> Result<(), StorageError> {
    let no_control = || Ok(());
    verify_content_matches_with_control(source, destination, &no_control)
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn verify_content_matches_with_control(
    source: &Path,
    destination: &Path,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    let _source_parent_guard = crate::open::hold_parent_dirs_no_follow(source)
        .map_err(|error| StorageError::io(source.display().to_string(), error))?;
    let _destination_parent_guard = crate::open::hold_parent_dirs_no_follow(destination)
        .map_err(|error| StorageError::io(destination.display().to_string(), error))?;
    verify_content_matches_inner_with_control(source, destination, 0, check_control)
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn verify_content_matches_inner_with_control(
    source: &Path,
    destination: &Path,
    depth: usize,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    check_control()?;
    let source_meta = safe_symlink_metadata(source, "verify-content-source")?;
    let destination_meta = safe_symlink_metadata(destination, "verify-content-destination")?;
    let source_type = source_meta.file_type();
    let destination_type = destination_meta.file_type();
    if metadata_is_reparse_point(&source_meta) {
        return Err(unsafe_symlink_error(source, "verify-content-source"));
    }
    if metadata_is_reparse_point(&destination_meta) {
        return Err(unsafe_symlink_error(
            destination,
            "verify-content-destination",
        ));
    }

    if source_type.is_dir() {
        ensure_storage_tree_depth(depth, source, "verify-tree-depth")?;
        if !destination_type.is_dir() {
            return Err(StorageError::StagedMoveFailed {
                step: "verify-content",
                reason: format!(
                    "expected a directory at {} to match source directory {}",
                    destination.display(),
                    source.display()
                ),
            });
        }
        let _source_directory_guard = crate::open::hold_directory_no_follow(source)
            .map_err(|error| StorageError::io(source.display().to_string(), error))?;
        let _destination_directory_guard = crate::open::hold_directory_no_follow(destination)
            .map_err(|error| StorageError::io(destination.display().to_string(), error))?;
        for entry in std::fs::read_dir(source)
            .map_err(|e| StorageError::io(source.display().to_string(), e))?
        {
            let entry = entry.map_err(|e| StorageError::io(source.display().to_string(), e))?;
            let destination_child = destination.join(entry.file_name());
            verify_content_matches_inner_with_control(
                &entry.path(),
                &destination_child,
                depth.saturating_add(1),
                check_control,
            )?;
        }
        for entry in std::fs::read_dir(destination)
            .map_err(|e| StorageError::io(destination.display().to_string(), e))?
        {
            check_control()?;
            let entry =
                entry.map_err(|e| StorageError::io(destination.display().to_string(), e))?;
            let destination_child = entry.path();
            let destination_child_metadata = std::fs::symlink_metadata(&destination_child)
                .map_err(|error| {
                    StorageError::io(destination_child.display().to_string(), error)
                })?;
            if metadata_is_reparse_point(&destination_child_metadata) {
                return Err(unsafe_symlink_error(
                    &destination_child,
                    "verify-content-destination",
                ));
            }
            let source_child = source.join(entry.file_name());
            let source_child_metadata = match std::fs::symlink_metadata(&source_child) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(StorageError::StagedMoveFailed {
                        step: "verify-content",
                        reason: format!(
                            "destination directory has entries absent from source: {}",
                            destination.display()
                        ),
                    });
                }
                Err(error) => {
                    return Err(StorageError::io(source_child.display().to_string(), error))
                }
            };
            if metadata_is_reparse_point(&source_child_metadata) {
                return Err(unsafe_symlink_error(&source_child, "verify-content-source"));
            }
            if !source_child_metadata.file_type().is_file()
                && !source_child_metadata.file_type().is_dir()
            {
                return Err(StorageError::StagedMoveFailed {
                    step: "verify-content",
                    reason: format!("unsupported source entry type: {}", source_child.display()),
                });
            }
        }
        Ok(())
    } else if source_type.is_file() {
        if !destination_type.is_file() {
            return Err(StorageError::StagedMoveFailed {
                step: "verify-content",
                reason: format!(
                    "expected a regular file at {} to match source file {}",
                    destination.display(),
                    source.display()
                ),
            });
        }
        let source_hash = hash_file_sha1_with_control(source, check_control)?;
        let destination_hash = hash_file_sha1_with_control(destination, check_control)?;
        if source_hash == destination_hash {
            Ok(())
        } else {
            Err(StorageError::StagedMoveFailed {
                step: "verify-content",
                reason: format!(
                    "content hash mismatch after copy: {} != {}",
                    source.display(),
                    destination.display()
                ),
            })
        }
    } else {
        Err(StorageError::StagedMoveFailed {
            step: "verify-content",
            reason: format!("unsupported file type: {}", source.display()),
        })
    }
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn hash_file_sha1_with_control(
    path: &Path,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<[u8; 20], StorageError> {
    use sha1::{Digest, Sha1};
    use std::io::Read;

    let mut file = crate::open::open_path_no_follow(path, false, false)
        .map_err(|e| StorageError::io(path.display().to_string(), e))?;
    let mut hasher = Sha1::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        check_control()?;
        let read = file
            .read(&mut buf)
            .map_err(|e| StorageError::io(path.display().to_string(), e))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hasher.finalize().into())
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn verify_path_len_with_control(
    path: &Path,
    expected_bytes: u64,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    let _parent_guard = crate::open::hold_parent_dirs_no_follow(path)
        .map_err(|error| StorageError::io(path.display().to_string(), error))?;
    let actual = path_content_len_with_control(path, 0, check_control)?;
    if actual == expected_bytes {
        Ok(())
    } else {
        Err(StorageError::ShortIo {
            path: path.display().to_string(),
            expected: expected_bytes as usize,
            actual: actual as usize,
        })
    }
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn copy_dir_recursive(
    source: &Path,
    destination: &Path,
    depth: usize,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    check_control()?;
    ensure_storage_tree_depth(depth, source, "copy-tree-depth")?;
    reject_symlink(source, "copy-source")?;
    std::fs::create_dir(destination)
        .map_err(|e| StorageError::io(destination.display().to_string(), e))?;
    copy_dir_contents(source, destination, depth, check_control)
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn copy_dir_contents(
    source: &Path,
    destination: &Path,
    depth: usize,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    let _source_directory_guard = crate::open::hold_directory_no_follow(source)
        .map_err(|error| StorageError::io(source.display().to_string(), error))?;
    let _destination_directory_guard = crate::open::hold_directory_no_follow(destination)
        .map_err(|error| StorageError::io(destination.display().to_string(), error))?;
    for entry in
        std::fs::read_dir(source).map_err(|e| StorageError::io(source.display().to_string(), e))?
    {
        check_control()?;
        let entry = entry.map_err(|e| StorageError::io(source.display().to_string(), e))?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = safe_symlink_metadata(&source_path, "copy-source")?;
        let file_type = metadata.file_type();
        if metadata_is_reparse_point(&metadata) {
            return Err(unsafe_symlink_error(&source_path, "copy-source"));
        }
        if file_type.is_dir() {
            copy_dir_recursive(
                &source_path,
                &destination_path,
                depth.saturating_add(1),
                check_control,
            )?;
        } else if file_type.is_file() {
            ensure_destination_available(&destination_path)?;
            let mut destination_created = false;
            copy_file_with_control(
                &source_path,
                &destination_path,
                &mut destination_created,
                check_control,
            )?;
        } else {
            return Err(StorageError::StagedMoveFailed {
                step: "copy-source",
                reason: format!("unsupported file type: {}", source_path.display()),
            });
        }
    }
    Ok(())
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn path_content_len(path: &Path) -> Result<u64, StorageError> {
    let no_control = || Ok(());
    let _parent_guard = crate::open::hold_parent_dirs_no_follow(path)
        .map_err(|error| StorageError::io(path.display().to_string(), error))?;
    path_content_len_with_control(path, 0, &no_control)
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn path_content_len_with_control(
    path: &Path,
    depth: usize,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<u64, StorageError> {
    check_control()?;
    let metadata = safe_symlink_metadata(path, "verify")?;
    let file_type = metadata.file_type();
    if metadata_is_reparse_point(&metadata) {
        return Err(unsafe_symlink_error(path, "verify"));
    }
    if file_type.is_dir() {
        ensure_storage_tree_depth(depth, path, "verify-tree-depth")?;
        let _directory_guard = crate::open::hold_directory_no_follow(path)
            .map_err(|error| StorageError::io(path.display().to_string(), error))?;
        let mut total = 0u64;
        for entry in
            std::fs::read_dir(path).map_err(|e| StorageError::io(path.display().to_string(), e))?
        {
            check_control()?;
            let entry = entry.map_err(|e| StorageError::io(path.display().to_string(), e))?;
            total = total.saturating_add(path_content_len_with_control(
                &entry.path(),
                depth.saturating_add(1),
                check_control,
            )?);
        }
        Ok(total)
    } else if file_type.is_file() {
        Ok(metadata.len())
    } else {
        Err(StorageError::StagedMoveFailed {
            step: "verify",
            reason: format!("unsupported file type: {}", path.display()),
        })
    }
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn copy_file_with_control(
    source: &Path,
    destination: &Path,
    destination_created: &mut bool,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    use std::io::{Read, Write};

    let mut source_file = crate::open::open_path_no_follow(source, false, false)
        .map_err(|e| StorageError::io(source.display().to_string(), e))?;
    let mut destination_file = crate::open::create_new_file_no_follow(destination)
        .map_err(|e| StorageError::io(destination.display().to_string(), e))?;
    *destination_created = true;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check_control()?;
        let bytes_read = source_file
            .read(&mut buffer)
            .map_err(|e| StorageError::io(source.display().to_string(), e))?;
        if bytes_read == 0 {
            break;
        }
        destination_file
            .write_all(&buffer[..bytes_read])
            .map_err(|e| StorageError::io(destination.display().to_string(), e))?;
    }
    destination_file
        .sync_all()
        .map_err(|e| StorageError::io(destination.display().to_string(), e))
}

fn safe_symlink_metadata(
    path: &Path,
    step: &'static str,
) -> Result<std::fs::Metadata, StorageError> {
    std::fs::symlink_metadata(path).map_err(|e| {
        let err = StorageError::io(path.display().to_string(), e);
        match err {
            StorageError::FileNotFound { .. } => StorageError::StagedMoveFailed {
                step,
                reason: format!(
                    "path missing during move/import execution: {}",
                    path.display()
                ),
            },
            other => other,
        }
    })
}

#[cfg(any(not(unix), test))]
fn metadata_is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn remove_reparse_point_no_follow(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        if metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0 {
            std::fs::remove_dir(path)
        } else {
            std::fs::remove_file(path)
        }
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        std::fs::remove_file(path)
    }
}

#[cfg(any(all(not(unix), not(windows)), test))]
fn reject_symlink(path: &Path, step: &'static str) -> Result<(), StorageError> {
    if metadata_is_reparse_point(&safe_symlink_metadata(path, step)?) {
        Err(unsafe_symlink_error(path, step))
    } else {
        Ok(())
    }
}

#[cfg(any(not(unix), test))]
pub(crate) fn unsafe_symlink_error(path: &Path, step: &'static str) -> StorageError {
    StorageError::StagedMoveFailed {
        step,
        reason: format!(
            "symlink/reparse-point entries are not allowed in move/import plans: {}",
            path.display()
        ),
    }
}

/// Rejects reparse-point ancestors before a storage-plan path is used.
///
/// Unix and Windows executors also anchor the actual filesystem operations to
/// opened root-relative directory handles; this path-based check is an early
/// rejection and does not authorize a later operation by itself.
#[cfg(any(not(unix), test))]
pub(crate) fn reject_symlink_ancestors(
    path: &Path,
    step: &'static str,
) -> Result<(), StorageError> {
    for ancestor in path.ancestors().skip(1) {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata_is_reparse_point(&metadata) => {
                return Err(unsafe_symlink_error(ancestor, step));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(StorageError::io(ancestor.display().to_string(), error));
            }
        }
    }
    Ok(())
}

fn common_issues(
    source: &Path,
    destination: &Path,
    bytes: u64,
    available_bytes: Option<u64>,
) -> Vec<PlanIssue> {
    let mut issues = Vec::new();
    if !path_exists_no_follow(source) {
        issues.push(PlanIssue::SourceMissing(source.to_path_buf()));
    }
    if path_exists_no_follow(destination) {
        issues.push(PlanIssue::DestinationExists(destination.to_path_buf()));
    }
    if let Some(available) = available_bytes {
        if available < bytes {
            issues.push(PlanIssue::InsufficientCapacity {
                needed: bytes,
                available,
            });
        }
    }
    issues
}

fn path_exists_no_follow(path: &Path) -> bool {
    let Ok(resolved) = resolve_confined_path(path) else {
        return false;
    };
    safe_symlink_metadata(&resolved, "plan-path").is_ok()
}

fn staging_path(destination: &Path) -> PathBuf {
    let mut staged = destination.to_path_buf();
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("torrentng-staged");
    staged.set_file_name(format!(".{file_name}.tng-copying"));
    staged
}

#[cfg(unix)]
fn same_filesystem(source: &Path, destination: &Path) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;

    let source_dev = safe_symlink_metadata(source, "filesystem-source")
        .ok()?
        .dev();
    let dest_parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let dest_dev = safe_symlink_metadata(dest_parent, "filesystem-destination")
        .ok()?
        .dev();
    Some(source_dev == dest_dev)
}

#[cfg(windows)]
fn same_filesystem(source: &Path, destination: &Path) -> Option<bool> {
    let source_volume = crate::open::windows_path_volume_serial(source).ok()?;
    let destination_parent = destination.parent()?;
    let destination_volume = crate::open::windows_path_volume_serial(destination_parent).ok()?;
    Some(source_volume == destination_volume)
}

#[cfg(not(any(unix, windows)))]
fn same_filesystem(_source: &Path, _destination: &Path) -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_plan_uses_rename_on_same_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("dest.bin");
        std::fs::write(&source, b"data").unwrap();

        let plan = plan_move(&MovePlanRequest {
            source,
            destination,
            bytes: 4,
            available_bytes: Some(100),
            dry_run: true,
        });

        assert_eq!(plan.can_apply, atomic_no_replace_rename_supported());
        assert_eq!(
            plan.issues
                .contains(&PlanIssue::AtomicNoReplaceRenameUnavailable),
            !atomic_no_replace_rename_supported()
        );
        assert_eq!(plan.steps[0].action, PlannedStorageAction::Rename);
    }

    #[test]
    fn move_plan_fails_closed_when_atomic_rename_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("destination.bin");
        std::fs::write(&source, b"data").unwrap();

        let plan = plan_move_with_rename_support(
            &MovePlanRequest {
                source,
                destination,
                bytes: 4,
                available_bytes: Some(100),
                dry_run: true,
            },
            false,
        );

        assert!(!plan.can_apply);
        assert!(plan
            .issues
            .contains(&PlanIssue::AtomicNoReplaceRenameUnavailable));
    }

    #[test]
    fn staged_import_copy_fails_closed_when_atomic_rename_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("destination.bin");
        std::fs::write(&source, b"data").unwrap();

        let plan = plan_import_with_rename_support(
            &ImportPlanRequest {
                source,
                destination,
                bytes: 4,
                available_bytes: Some(100),
                hardlink_or_copy: false,
                dry_run: true,
            },
            false,
        );

        assert!(!plan.can_apply);
        assert!(plan
            .issues
            .contains(&PlanIssue::AtomicNoReplaceRenameUnavailable));

        let direct_import = plan_import_with_rename_support(
            &ImportPlanRequest {
                source: dir.path().join("source.bin"),
                destination: dir.path().join("linked.bin"),
                bytes: 4,
                available_bytes: Some(100),
                hardlink_or_copy: true,
                dry_run: true,
            },
            false,
        );
        assert!(direct_import.can_apply);
        assert_eq!(
            direct_import.steps[0].action,
            PlannedStorageAction::ImportExisting
        );
        assert!(!direct_import
            .issues
            .contains(&PlanIssue::AtomicNoReplaceRenameUnavailable));
    }

    #[cfg(windows)]
    #[test]
    fn confined_path_accepts_absolute_drive_prefixes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stored.bin");
        std::fs::write(&path, b"data").unwrap();

        assert_eq!(
            resolve_confined_path(&path).unwrap(),
            std::fs::canonicalize(path).unwrap()
        );
    }

    #[test]
    fn move_plan_reports_conflict_and_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("dest.bin");
        std::fs::write(&source, b"data").unwrap();
        std::fs::write(&destination, b"old").unwrap();

        let plan = plan_move(&MovePlanRequest {
            source,
            destination: destination.clone(),
            bytes: 10,
            available_bytes: Some(5),
            dry_run: true,
        });

        assert!(!plan.can_apply);
        assert!(plan
            .issues
            .contains(&PlanIssue::DestinationExists(destination)));
        assert!(plan.issues.contains(&PlanIssue::InsufficientCapacity {
            needed: 10,
            available: 5
        }));
    }

    #[test]
    fn move_plan_copy_verify_path_deletes_source_after_verified_rename() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("missing-parent/dest.bin");
        std::fs::write(&source, b"data").unwrap();

        let plan = plan_move(&MovePlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 4,
            available_bytes: Some(100),
            dry_run: false,
        });

        assert!(plan.can_apply);
        assert_eq!(
            plan.steps
                .iter()
                .map(|step| &step.action)
                .collect::<Vec<_>>(),
            vec![
                &PlannedStorageAction::CopyVerifyRename,
                &PlannedStorageAction::Rename,
                &PlannedStorageAction::SafeDelete,
            ]
        );

        execute_storage_plan(&plan).unwrap();

        assert!(!source.exists());
        assert_eq!(std::fs::read(&destination).unwrap(), b"data");
    }

    #[test]
    fn delete_requires_prior_dry_run_approval() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("delete.bin");
        std::fs::write(&target, b"data").unwrap();

        let plan = plan_delete(&DeletePlanRequest {
            target,
            bytes: 4,
            dry_run: false,
            dry_run_approved: false,
        });

        assert!(!plan.can_apply);
        assert!(plan
            .issues
            .contains(&PlanIssue::DeleteRequiresDryRunApproval));
    }

    #[test]
    fn import_plan_detects_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("dest.bin");
        std::fs::write(&source, b"data").unwrap();
        std::fs::write(&destination, b"old").unwrap();

        let plan = plan_import(&ImportPlanRequest {
            source,
            destination: destination.clone(),
            bytes: 4,
            available_bytes: Some(100),
            hardlink_or_copy: true,
            dry_run: true,
        });

        assert!(!plan.can_apply);
        assert!(plan
            .issues
            .contains(&PlanIssue::DestinationExists(destination)));
    }

    #[test]
    fn import_copy_plan_stages_before_final_rename() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("dest.bin");
        let staged = staging_path(&destination);
        std::fs::write(&source, b"data").unwrap();

        let plan = plan_import(&ImportPlanRequest {
            source,
            destination: destination.clone(),
            bytes: 4,
            available_bytes: Some(100),
            hardlink_or_copy: false,
            dry_run: false,
        });

        assert_eq!(
            plan.steps
                .iter()
                .map(|step| (&step.action, step.destination.as_ref()))
                .collect::<Vec<_>>(),
            vec![
                (&PlannedStorageAction::CopyVerifyRename, Some(&staged)),
                (&PlannedStorageAction::Rename, Some(&destination)),
            ]
        );
        assert_eq!(
            plan.rollback_steps,
            vec![StoragePlanStep {
                action: PlannedStorageAction::SafeDeleteIfPresent,
                source: Some(staged),
                destination: None,
                bytes: 4,
            }]
        );
    }

    #[cfg(unix)]
    #[test]
    fn plan_import_treats_broken_destination_symlink_as_existing() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("dest-link.bin");
        std::fs::write(&source, b"data").unwrap();
        symlink(dir.path().join("missing-target.bin"), &destination).unwrap();

        let plan = plan_import(&ImportPlanRequest {
            source,
            destination: destination.clone(),
            bytes: 4,
            available_bytes: Some(100),
            hardlink_or_copy: false,
            dry_run: true,
        });

        assert!(!plan.can_apply);
        assert!(plan
            .issues
            .contains(&PlanIssue::DestinationExists(destination)));
    }

    #[cfg(unix)]
    #[test]
    fn plan_delete_treats_broken_symlink_as_existing_source() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("delete-link");
        symlink(dir.path().join("missing-target"), &target).unwrap();

        let plan = plan_delete(&DeletePlanRequest {
            target,
            bytes: 0,
            dry_run: true,
            dry_run_approved: true,
        });

        assert!(plan.can_apply);
        assert!(plan.issues.is_empty());
    }

    #[test]
    fn execute_move_plan_renames_without_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("dest.bin");
        std::fs::write(&source, b"data").unwrap();

        let plan = plan_move(&MovePlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 4,
            available_bytes: Some(100),
            dry_run: false,
        });

        let execution = execute_storage_plan(&plan).unwrap();
        assert_eq!(execution.applied_steps.len(), 1);
        assert!(!source.exists());
        assert_eq!(std::fs::read(&destination).unwrap(), b"data");

        let conflict = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: PlannedStorageAction::Rename,
                source: Some(destination.clone()),
                destination: Some(destination.clone()),
                bytes: 4,
            }],
            rollback_steps: Vec::new(),
        };
        assert!(matches!(
            execute_storage_plan(&conflict),
            Err(StorageError::FilesystemStateUncertain { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn execute_move_plan_rejects_symlink_source_before_rename() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.bin");
        let source = dir.path().join("source-link.bin");
        let destination = dir.path().join("dest.bin");
        std::fs::write(&outside, b"outside").unwrap();
        symlink(&outside, &source).unwrap();
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: PlannedStorageAction::Rename,
                source: Some(source.clone()),
                destination: Some(destination.clone()),
                bytes: 7,
            }],
            rollback_steps: Vec::new(),
        };

        assert!(matches!(
            execute_storage_plan(&plan),
            Err(StorageError::StagedMoveFailed {
                step: "execute",
                ..
            })
        ));
        assert!(source.exists());
        assert!(!destination.exists());
    }

    #[test]
    fn execute_copy_verify_plan_rolls_back_staged_file_on_short_copy() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("dest.bin");
        let staged = staging_path(&destination);
        std::fs::write(&source, b"data").unwrap();
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: PlannedStorageAction::CopyVerifyRename,
                source: Some(source),
                destination: Some(staged.clone()),
                bytes: 99,
            }],
            rollback_steps: vec![StoragePlanStep {
                action: PlannedStorageAction::SafeDeleteIfPresent,
                source: Some(staged.clone()),
                destination: None,
                bytes: 99,
            }],
        };

        assert!(matches!(
            execute_storage_plan(&plan),
            Err(StorageError::StagedMoveFailed { .. })
        ));
        assert!(!staged.exists());
    }

    #[test]
    fn verify_content_matches_detects_bit_flip_despite_matching_length() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("dest.bin");
        // Same length as source, single byte flipped -- a length-only check
        // (verify_path_len) would have accepted this as a successful copy.
        std::fs::write(&source, b"AAAAAAAAAA").unwrap();
        std::fs::write(&destination, b"AAAAAAAAAB").unwrap();
        assert_eq!(
            std::fs::metadata(&source).unwrap().len(),
            std::fs::metadata(&destination).unwrap().len()
        );

        let error = verify_content_matches(&source, &destination).unwrap_err();
        assert!(matches!(
            error,
            StorageError::StagedMoveFailed {
                step: "verify-content",
                ..
            }
        ));
        assert!(error.to_string().contains("content hash mismatch"));
    }

    #[test]
    fn verify_content_matches_rejects_extra_destination_entries() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let destination = dir.path().join("destination");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(source.join("payload.bin"), b"payload").unwrap();
        std::fs::write(destination.join("payload.bin"), b"payload").unwrap();
        std::fs::write(destination.join("unexpected.bin"), b"unexpected").unwrap();

        let error = verify_content_matches(&source, &destination).unwrap_err();

        assert!(error
            .to_string()
            .contains("destination directory has entries absent from source"));
    }

    #[cfg(unix)]
    #[test]
    fn portable_tree_walks_reject_overdeep_directories_and_clean_partial_copy() {
        fn create_overdeep_tree(root: &Path) {
            std::fs::create_dir(root).unwrap();
            let mut current = root.to_path_buf();
            for _ in 0..=MAX_STORAGE_TREE_DEPTH {
                current.push("nested");
                std::fs::create_dir(&current).unwrap();
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        create_overdeep_tree(&source);
        create_overdeep_tree(&destination);
        let no_control = || Ok(());

        let verify_error = verify_content_matches(&source, &destination).unwrap_err();
        assert!(verify_error.to_string().contains("directory nesting depth"));
        let length_error = path_content_len(&source).unwrap_err();
        assert!(length_error.to_string().contains("directory nesting depth"));

        let copy_destination = directory.path().join("copy-destination");
        let copy_error =
            copy_verify_with_control(&source, &copy_destination, 0, &no_control).unwrap_err();
        assert!(copy_error.to_string().contains("directory nesting depth"));
        assert!(!copy_destination.exists(), "partial copy must be removed");

        let delete_error = safe_delete_with_control(&source, false, &no_control).unwrap_err();
        assert!(delete_error.to_string().contains("directory nesting depth"));
        assert!(source.exists(), "rejected deep tree must remain intact");
    }

    #[cfg(unix)]
    #[test]
    fn verify_content_matches_rejects_source_symlink_swap_during_final_scan() {
        use std::cell::Cell;
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let destination = dir.path().join("destination");
        let source_child = source.join("payload.bin");
        let destination_child = destination.join("payload.bin");
        let outside = dir.path().join("outside.bin");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(&source_child, []).unwrap();
        std::fs::write(&destination_child, []).unwrap();
        std::fs::write(&outside, []).unwrap();

        let checks = Cell::new(0usize);
        let check_control = || {
            let count = checks.get() + 1;
            checks.set(count);
            if count == 5 {
                std::fs::remove_file(&source_child).unwrap();
                symlink(&outside, &source_child).unwrap();
            }
            Ok(())
        };

        let error =
            verify_content_matches_with_control(&source, &destination, &check_control).unwrap_err();

        assert_eq!(
            checks.get(),
            5,
            "the swap must happen during the final scan"
        );
        assert!(matches!(
            error,
            StorageError::StagedMoveFailed {
                step: "verify-content-source",
                ..
            }
        ));
    }

    #[test]
    fn execute_plan_reports_rollback_step_failure_in_error_message() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("dest.bin");
        let missing_rollback_target = dir.path().join("does-not-exist.bin");
        let cleanup_target = dir.path().join("cleanup.bin");
        std::fs::write(&cleanup_target, b"cleanup").unwrap();
        // The primary source is absent, so execution fails before touching
        // the filesystem. The rollback list still contains one successful
        // cleanup and one failing cleanup, exercising both result paths.

        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: PlannedStorageAction::Rename,
                source: Some(source),
                destination: Some(destination),
                bytes: 4,
            }],
            rollback_steps: vec![
                StoragePlanStep {
                    action: PlannedStorageAction::SafeDelete,
                    source: Some(cleanup_target.clone()),
                    destination: None,
                    bytes: 4,
                },
                StoragePlanStep {
                    action: PlannedStorageAction::SafeDelete,
                    source: Some(missing_rollback_target),
                    destination: None,
                    bytes: 0,
                },
            ],
        };

        let error = execute_storage_plan(&plan).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("rollback step(s) failed"),
            "expected a failed rollback step to be surfaced in the error, got: {message}"
        );
        assert!(
            message.contains("does-not-exist.bin"),
            "expected the failing rollback step's path to be named in the error, got: {message}"
        );
        // The rollback step that *could* succeed still ran and cleaned up its
        // target even though a later rollback step failed -- one rollback
        // step failing must not stop the rest of the rollback from being
        // attempted.
        assert!(!cleanup_target.exists());
    }

    #[test]
    fn failure_after_destructive_step_requires_manual_recovery() {
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![
                StoragePlanStep {
                    action: PlannedStorageAction::Rename,
                    source: Some(PathBuf::from("source.bin")),
                    destination: Some(PathBuf::from("destination.bin")),
                    bytes: 4,
                },
                StoragePlanStep {
                    action: PlannedStorageAction::SafeDelete,
                    source: Some(PathBuf::from("source.bin")),
                    destination: None,
                    bytes: 4,
                },
            ],
            rollback_steps: Vec::new(),
        };
        let no_control = || Ok(());

        let error = execute_storage_plan_with_executor(
            &plan,
            &[],
            |_, _| Ok(()),
            |step| match step.action {
                PlannedStorageAction::Rename => Ok(()),
                PlannedStorageAction::SafeDelete => Err(StorageError::Io {
                    path: "source.bin".to_owned(),
                    source: std::io::Error::other("simulated source deletion failure"),
                }),
                _ => unreachable!("test plan only contains the two expected actions"),
            },
            |_| (Vec::new(), Vec::new()),
            |_, _| Ok(false),
            &no_control,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            StorageError::FilesystemStateUncertain {
                step: "execute_after_destructive_step",
                ..
            }
        ));
    }

    #[test]
    fn checkpoint_failure_leaves_committed_step_resumable() {
        let dir = tempfile::tempdir().unwrap();
        let source_one = dir.path().join("source-one.bin");
        let source_two = dir.path().join("source-two.bin");
        let destination_one = dir.path().join("destination-one.bin");
        let destination_two = dir.path().join("destination-two.bin");
        let staging_one = staging_path(&destination_one);
        std::fs::write(&source_one, b"one").unwrap();
        std::fs::write(&source_two, b"two").unwrap();
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![
                StoragePlanStep {
                    action: PlannedStorageAction::CopyVerifyRename,
                    source: Some(source_one.clone()),
                    destination: Some(staging_one),
                    bytes: 3,
                },
                StoragePlanStep {
                    action: PlannedStorageAction::Rename,
                    source: Some(staging_path(&destination_one)),
                    destination: Some(destination_one.clone()),
                    bytes: 3,
                },
                StoragePlanStep {
                    action: PlannedStorageAction::Rename,
                    source: Some(source_two.clone()),
                    destination: Some(destination_two.clone()),
                    bytes: 3,
                },
            ],
            rollback_steps: Vec::new(),
        };

        let checkpoint_error = execute_storage_plan_with_checkpoints(&plan, &[], |index, _| {
            if index == 0 {
                Err(StorageError::StagedMoveFailed {
                    step: "checkpoint",
                    reason: "simulated durable-store interruption".to_owned(),
                })
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert!(checkpoint_error
            .to_string()
            .contains("durable-store interruption"));
        assert!(source_one.exists());
        assert!(staging_path(&destination_one).exists());
        assert!(source_two.exists());
        assert!(!destination_two.exists());

        let resumed = execute_storage_plan_with_checkpoints(&plan, &[0], |_, _| Ok(())).unwrap();
        assert_eq!(resumed.applied_steps.len(), 3);
        assert!(source_one.exists());
        assert!(!source_two.exists());
        assert!(destination_one.exists());
        assert!(destination_two.exists());
    }

    #[test]
    fn retrying_a_completed_copy_plan_validates_checkpoint_state() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("destination.bin");
        std::fs::write(&source, b"data").unwrap();
        let plan = plan_import(&ImportPlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 4,
            available_bytes: Some(100),
            hardlink_or_copy: false,
            dry_run: false,
        });

        execute_storage_plan(&plan).unwrap();
        let retry = execute_storage_plan_with_checkpoints(&plan, &[0], |_, _| Ok(())).unwrap();

        assert_eq!(retry.applied_steps.len(), plan.steps.len());
        assert!(source.exists());
        assert_eq!(std::fs::read(destination).unwrap(), b"data");
    }

    #[test]
    fn checkpointed_rename_without_live_proof_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("destination.bin");
        std::fs::write(&source, b"data").unwrap();
        let plan = plan_move(&MovePlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 4,
            available_bytes: None,
            dry_run: false,
        });

        execute_storage_plan(&plan).unwrap();
        let error = execute_storage_plan_with_checkpoints(&plan, &[], |_, _| Ok(())).unwrap_err();

        assert!(matches!(
            error,
            StorageError::FilesystemStateUncertain {
                step: "reconcile",
                ..
            }
        ));
        assert!(!source.exists());
        assert_eq!(std::fs::read(destination).unwrap(), b"data");
    }

    #[test]
    fn rename_rejects_a_stale_expected_size_before_moving() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("destination.bin");
        std::fs::write(&source, b"data").unwrap();
        let plan = plan_move(&MovePlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 99,
            available_bytes: None,
            dry_run: false,
        });

        let error = execute_storage_plan(&plan).unwrap_err();

        assert!(error.to_string().contains("99"));
        assert_eq!(std::fs::read(source).unwrap(), b"data");
        assert!(!destination.exists());
    }

    #[test]
    fn execute_import_copy_failure_does_not_leave_final_destination() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("dest.bin");
        let staged = staging_path(&destination);
        std::fs::write(&source, b"data").unwrap();
        let plan = plan_import(&ImportPlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 99,
            available_bytes: Some(100),
            hardlink_or_copy: false,
            dry_run: false,
        });

        assert!(matches!(
            execute_storage_plan(&plan),
            Err(StorageError::StagedMoveFailed { .. })
        ));
        assert_eq!(std::fs::read(&source).unwrap(), b"data");
        assert!(!destination.exists());
        assert!(!staged.exists());
    }

    #[test]
    fn execute_import_plan_links_or_copies_without_removing_source() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("dest.bin");
        std::fs::write(&source, b"data").unwrap();
        let plan = plan_import(&ImportPlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 4,
            available_bytes: Some(100),
            hardlink_or_copy: true,
            dry_run: false,
        });

        execute_storage_plan(&plan).unwrap();

        assert_eq!(std::fs::read(&source).unwrap(), b"data");
        assert_eq!(std::fs::read(&destination).unwrap(), b"data");
    }

    #[test]
    fn execute_import_plan_rejects_stale_expected_size_without_destination() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("dest.bin");
        std::fs::write(&source, b"data").unwrap();
        let plan = plan_import(&ImportPlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 99,
            available_bytes: Some(100),
            hardlink_or_copy: true,
            dry_run: false,
        });

        assert!(execute_storage_plan(&plan).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), b"data");
        assert!(!destination.exists());
    }

    #[test]
    fn copy_directory_verification_failure_removes_partial_destination() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let source = root.join("source");
        let destination = root.join("destination");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("payload.bin"), b"data").unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: PlannedStorageAction::CopyVerifyRename,
                source: Some(source.clone()),
                destination: Some(destination.clone()),
                bytes: 99,
            }],
            rollback_steps: Vec::new(),
        };

        assert!(execute_storage_plan_under_roots(&plan, std::slice::from_ref(&root)).is_err());
        assert!(source.exists());
        assert!(!destination.exists());
    }

    #[test]
    fn copy_verification_failure_removes_partial_file_destination() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let source = root.join("source.bin");
        let destination = root.join("destination.bin");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&source, b"data").unwrap();
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: PlannedStorageAction::CopyVerifyRename,
                source: Some(source.clone()),
                destination: Some(destination.clone()),
                bytes: 99,
            }],
            rollback_steps: Vec::new(),
        };

        assert!(execute_storage_plan_under_roots(&plan, std::slice::from_ref(&root)).is_err());
        assert!(source.exists());
        assert!(!destination.exists());
    }

    #[cfg(unix)]
    #[test]
    fn execute_import_plan_rejects_symlink_source_before_hardlink() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.bin");
        let source = dir.path().join("source-link.bin");
        let destination = dir.path().join("dest.bin");
        std::fs::write(&outside, b"outside").unwrap();
        symlink(&outside, &source).unwrap();
        let plan = plan_import(&ImportPlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 7,
            available_bytes: Some(100),
            hardlink_or_copy: true,
            dry_run: false,
        });

        assert!(matches!(
            execute_storage_plan(&plan),
            Err(StorageError::StagedMoveFailed {
                step: "execute",
                ..
            })
        ));
        assert!(source.exists());
        assert!(!destination.exists());
    }

    #[test]
    fn execute_copy_verify_plan_copies_directory_tree_and_verifies_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let nested = source.join("nested");
        let destination = dir.path().join("dest");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(source.join("a.bin"), b"abcd").unwrap();
        std::fs::write(nested.join("b.bin"), b"ef").unwrap();

        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: PlannedStorageAction::CopyVerifyRename,
                source: Some(source.clone()),
                destination: Some(destination.clone()),
                bytes: 6,
            }],
            rollback_steps: Vec::new(),
        };

        execute_storage_plan(&plan).unwrap();

        assert_eq!(std::fs::read(source.join("a.bin")).unwrap(), b"abcd");
        assert_eq!(std::fs::read(destination.join("a.bin")).unwrap(), b"abcd");
        assert_eq!(
            std::fs::read(destination.join("nested/b.bin")).unwrap(),
            b"ef"
        );
    }

    #[cfg(unix)]
    #[test]
    fn execute_copy_verify_plan_rejects_symlink_source_file() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.bin");
        let source = dir.path().join("source-link.bin");
        let destination = dir.path().join("dest.bin");
        std::fs::write(&outside, b"outside").unwrap();
        symlink(&outside, &source).unwrap();

        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: PlannedStorageAction::CopyVerifyRename,
                source: Some(source.clone()),
                destination: Some(destination.clone()),
                bytes: 7,
            }],
            rollback_steps: Vec::new(),
        };

        assert!(matches!(
            execute_storage_plan(&plan),
            Err(StorageError::StagedMoveFailed {
                step: "execute",
                ..
            })
        ));
        assert!(source.exists());
        assert!(!destination.exists());
    }

    #[cfg(windows)]
    #[test]
    fn portable_copy_opens_source_without_following_a_file_reparse_point() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.bin");
        let source = dir.path().join("source-link.bin");
        let destination = dir.path().join("destination.bin");
        std::fs::write(&target, b"outside").unwrap();
        if let Err(error) = std::os::windows::fs::symlink_file(&target, &source) {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                return;
            }
            panic!("failed to create file symlink for copy regression test: {error}");
        }

        let no_control = || Ok(());
        let mut destination_created = false;
        let error =
            copy_file_with_control(&source, &destination, &mut destination_created, &no_control)
                .unwrap_err();

        assert!(matches!(error, StorageError::Io { .. }));
        assert!(!destination_created);
        assert!(!destination.exists());
        assert_eq!(std::fs::read(target).unwrap(), b"outside");
    }

    #[cfg(unix)]
    #[test]
    fn execute_copy_verify_plan_rejects_nested_symlink_entry() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let nested = source.join("nested");
        let outside = dir.path().join("outside.bin");
        let destination = dir.path().join("dest");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(source.join("a.bin"), b"abcd").unwrap();
        std::fs::write(&outside, b"outside").unwrap();
        symlink(&outside, nested.join("escape.bin")).unwrap();

        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: PlannedStorageAction::CopyVerifyRename,
                source: Some(source.clone()),
                destination: Some(destination.clone()),
                bytes: 11,
            }],
            rollback_steps: vec![StoragePlanStep {
                action: PlannedStorageAction::SafeDeleteIfPresent,
                source: Some(destination.clone()),
                destination: None,
                bytes: 11,
            }],
        };

        assert!(matches!(
            execute_storage_plan(&plan),
            Err(StorageError::StagedMoveFailed {
                step: "execute",
                ..
            })
        ));
        assert!(source.exists());
        assert!(!destination.exists());
    }

    #[test]
    fn move_plan_copy_verify_path_removes_source_directory_after_verified_rename() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let nested = source.join("nested");
        let destination = dir.path().join("missing-parent/dest");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(source.join("a.bin"), b"abcd").unwrap();
        std::fs::write(nested.join("b.bin"), b"ef").unwrap();

        let plan = plan_move(&MovePlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 6,
            available_bytes: Some(100),
            dry_run: false,
        });

        execute_storage_plan(&plan).unwrap();

        assert!(!source.exists());
        assert_eq!(std::fs::read(destination.join("a.bin")).unwrap(), b"abcd");
        assert_eq!(
            std::fs::read(destination.join("nested/b.bin")).unwrap(),
            b"ef"
        );
    }

    #[test]
    fn execute_delete_plan_removes_directory_tree_after_approval() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("delete-me");
        std::fs::create_dir_all(target.join("nested")).unwrap();
        std::fs::write(target.join("nested/file.bin"), b"data").unwrap();
        let plan = plan_delete(&DeletePlanRequest {
            target: target.clone(),
            bytes: 4,
            dry_run: false,
            dry_run_approved: true,
        });

        execute_storage_plan(&plan).unwrap();

        assert!(!target.exists());
    }

    #[test]
    fn cleanup_plan_is_idempotent_and_prunes_only_empty_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let payload = root.join("torrent/nested/payload.bin");
        std::fs::create_dir_all(payload.parent().unwrap()).unwrap();
        std::fs::write(&payload, b"data").unwrap();
        let keep = root.join("torrent/keep.txt");
        std::fs::write(&keep, b"keep").unwrap();

        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![
                StoragePlanStep {
                    action: PlannedStorageAction::SafeDeleteIfPresent,
                    source: Some(payload.clone()),
                    destination: None,
                    bytes: 4,
                },
                StoragePlanStep {
                    action: PlannedStorageAction::PruneEmptyDirs,
                    source: Some(payload.parent().unwrap().to_path_buf()),
                    destination: Some(root.clone()),
                    bytes: 0,
                },
            ],
            rollback_steps: Vec::new(),
        };

        execute_storage_plan_under_roots(&plan, std::slice::from_ref(&root)).unwrap();
        assert!(!payload.exists());
        assert!(!root.join("torrent/nested").exists());
        assert!(root.join("torrent").exists());
        assert!(keep.exists());

        // A retry after a worker restart must not turn already-completed
        // cleanup into a failed job or remove unrelated sibling content.
        execute_storage_plan_under_roots(&plan, std::slice::from_ref(&root)).unwrap();
        assert!(keep.exists());
    }

    #[cfg(unix)]
    #[test]
    fn execute_delete_plan_removes_symlink_without_following_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target_dir = dir.path().join("target-dir");
        let target_file = target_dir.join("target.bin");
        let link = dir.path().join("delete-link");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(&target_file, b"data").unwrap();
        symlink(&target_dir, &link).unwrap();

        let plan = plan_delete(&DeletePlanRequest {
            target: link.clone(),
            bytes: 0,
            dry_run: false,
            dry_run_approved: true,
        });

        execute_storage_plan(&plan).unwrap();

        assert!(!link.exists());
        assert_eq!(std::fs::read(&target_file).unwrap(), b"data");
    }

    #[test]
    fn execute_plan_under_roots_allows_confined_move() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.bin");
        let destination = root.join("dest.bin");
        std::fs::write(&source, b"data").unwrap();

        let plan = plan_move(&MovePlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 4,
            available_bytes: Some(100),
            dry_run: false,
        });

        execute_storage_plan_under_roots(&plan, &[root]).unwrap();

        assert!(!source.exists());
        assert_eq!(std::fs::read(&destination).unwrap(), b"data");
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_anchored_rename_never_replaces_a_new_destination() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.bin");
        let destination = root.join("destination.bin");
        std::fs::write(&source, b"source").unwrap();
        std::fs::write(&destination, b"existing").unwrap();
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: PlannedStorageAction::Rename,
                source: Some(source.clone()),
                destination: Some(destination.clone()),
                bytes: 6,
            }],
            rollback_steps: Vec::new(),
        };

        let error =
            execute_storage_plan_under_roots(&plan, std::slice::from_ref(&root)).unwrap_err();

        assert!(matches!(
            error,
            StorageError::FilesystemStateUncertain {
                step: "reconcile",
                ..
            }
        ));
        assert_eq!(std::fs::read(source).unwrap(), b"source");
        assert_eq!(std::fs::read(destination).unwrap(), b"existing");
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_anchored_rename_checks_size_before_mutating() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.bin");
        let destination = root.join("destination.bin");
        std::fs::write(&source, b"data").unwrap();
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: PlannedStorageAction::Rename,
                source: Some(source.clone()),
                destination: Some(destination.clone()),
                bytes: 99,
            }],
            rollback_steps: Vec::new(),
        };

        let error =
            execute_storage_plan_under_roots(&plan, std::slice::from_ref(&root)).unwrap_err();

        assert!(error.to_string().contains("99"));
        assert_eq!(std::fs::read(source).unwrap(), b"data");
        assert!(!destination.exists());
    }

    #[test]
    fn execute_plan_under_roots_rejects_destination_escape_before_copy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let source = root.join("source.bin");
        let destination = outside.join("dest.bin");
        std::fs::write(&source, b"data").unwrap();

        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: PlannedStorageAction::CopyVerifyRename,
                source: Some(source.clone()),
                destination: Some(destination.clone()),
                bytes: 4,
            }],
            rollback_steps: Vec::new(),
        };

        assert!(matches!(
            execute_storage_plan_under_roots(&plan, &[root]),
            Err(StorageError::StagedMoveFailed {
                step: "destination-root",
                ..
            })
        ));
        assert!(source.exists());
        assert!(!destination.exists());
    }

    #[test]
    fn execute_plan_under_roots_rejects_delete_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let outside = dir.path().join("outside.bin");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&outside, b"data").unwrap();

        let plan = plan_delete(&DeletePlanRequest {
            target: outside.clone(),
            bytes: 4,
            dry_run: false,
            dry_run_approved: true,
        });

        assert!(matches!(
            execute_storage_plan_under_roots(&plan, &[root]),
            Err(StorageError::StagedMoveFailed {
                step: "source-root",
                ..
            })
        ));
        assert_eq!(std::fs::read(&outside).unwrap(), b"data");
    }

    #[test]
    fn execute_plan_under_roots_refuses_to_delete_the_storage_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let sentinel = root.join("keep.bin");
        std::fs::write(&sentinel, b"keep").unwrap();

        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: PlannedStorageAction::SafeDelete,
                source: Some(root.clone()),
                destination: None,
                bytes: 4,
            }],
            rollback_steps: Vec::new(),
        };

        assert!(matches!(
            execute_storage_plan_under_roots(&plan, std::slice::from_ref(&root)),
            Err(StorageError::StagedMoveFailed {
                step: "source-root",
                ..
            })
        ));
        assert!(sentinel.exists());
    }

    #[test]
    fn execute_plan_under_roots_rejects_parent_dir_components() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = root.join("nested/../dest.bin");

        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: PlannedStorageAction::CopyVerifyRename,
                source: Some(source),
                destination: Some(destination),
                bytes: 4,
            }],
            rollback_steps: Vec::new(),
        };

        assert!(matches!(
            execute_storage_plan_under_roots(&plan, &[root]),
            Err(StorageError::StagedMoveFailed { step: "path", .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_anchored_execution_rejects_an_ancestor_symlink() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let source = root.path().join("source.bin");
        let alias = root.path().join("alias");
        let destination = alias.join("destination.bin");
        std::fs::write(&source, b"data").unwrap();
        symlink(outside.path(), &alias).unwrap();

        let step = StoragePlanStep {
            action: PlannedStorageAction::CopyVerifyRename,
            source: Some(source),
            destination: Some(destination),
            bytes: 4,
        };
        let error =
            crate::secure_fs::execute_step(&step, &[root.path().to_path_buf()]).unwrap_err();
        assert!(
            matches!(
                &error,
                StorageError::Io { source, .. }
                    if matches!(source.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR))
            ),
            "unexpected secure ancestor error: {error:?}"
        );
        assert!(!outside.path().join("destination.bin").exists());
    }

    // The portable fallback executor (`execute_step`/`rollback_plan`/
    // `prune_empty_dirs`, compiled for non-Unix targets and, via
    // `cfg(any(not(unix), test))`, for every `cargo test` run) has weaker
    // path authority than `secure_fs`'s descriptor-anchored Unix executor
    // above -- see the comment on `reject_symlink_ancestors`. Before this
    // block, none of these functions were called by any test on any
    // platform: on Unix the runtime dispatch always takes the `secure_fs`
    // branch, so this logic ran only under a `cfg(not(unix))` build that no
    // CI job actually tests. These tests call the fallback functions
    // directly so the logic itself -- not just the Unix branch -- has real
    // coverage.

    #[test]
    fn portable_execute_step_renames_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("dest.bin");
        std::fs::write(&source, b"data").unwrap();

        let step = StoragePlanStep {
            action: PlannedStorageAction::Rename,
            source: Some(source.clone()),
            destination: Some(destination.clone()),
            bytes: 4,
        };
        execute_step(&step).unwrap();

        assert!(!source.exists());
        assert_eq!(std::fs::read(&destination).unwrap(), b"data");
    }

    #[test]
    fn portable_execute_step_import_hardlinks_or_copies_without_removing_source() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("dest.bin");
        std::fs::write(&source, b"data").unwrap();

        let step = StoragePlanStep {
            action: PlannedStorageAction::ImportExisting,
            source: Some(source.clone()),
            destination: Some(destination.clone()),
            bytes: 4,
        };
        execute_step(&step).unwrap();

        assert!(source.exists(), "import must not remove the source");
        assert_eq!(std::fs::read(&destination).unwrap(), b"data");
    }

    #[cfg(windows)]
    #[test]
    fn portable_windows_import_does_not_create_a_path_based_hard_link() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("dest.bin");
        std::fs::write(&source, b"data").unwrap();

        let step = StoragePlanStep {
            action: PlannedStorageAction::ImportExisting,
            source: Some(source.clone()),
            destination: Some(destination.clone()),
            bytes: 4,
        };
        execute_step(&step).unwrap();

        std::fs::write(&source, b"edit").unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"data");
    }

    #[test]
    fn portable_execute_step_copy_verify_rename_copies_and_verifies_content() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("dest.bin");
        std::fs::write(&source, b"payload").unwrap();

        let step = StoragePlanStep {
            action: PlannedStorageAction::CopyVerifyRename,
            source: Some(source.clone()),
            destination: Some(destination.clone()),
            bytes: 7,
        };
        execute_step(&step).unwrap();

        assert!(source.exists());
        assert_eq!(std::fs::read(&destination).unwrap(), b"payload");
    }

    #[test]
    fn portable_execute_step_safe_delete_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.bin");
        std::fs::write(&target, b"data").unwrap();

        let step = StoragePlanStep {
            action: PlannedStorageAction::SafeDelete,
            source: Some(target.clone()),
            destination: None,
            bytes: 4,
        };
        execute_step(&step).unwrap();

        assert!(!target.exists());
    }

    #[cfg(windows)]
    #[test]
    fn portable_delete_removes_directory_reparse_points_without_following_targets() {
        use std::os::windows::fs::symlink_dir;

        let dir = tempfile::tempdir().unwrap();
        let external_root = dir.path().join("external-root");
        std::fs::create_dir(&external_root).unwrap();
        let external_file = external_root.join("keep.bin");
        std::fs::write(&external_file, b"preserve").unwrap();

        let root_link = dir.path().join("root-link");
        symlink_dir(&external_root, &root_link).unwrap();
        let no_control = || Ok(());
        safe_delete_with_control(&root_link, false, &no_control).unwrap();
        assert!(
            external_file.exists(),
            "root link target must remain intact"
        );
        assert!(std::fs::symlink_metadata(&root_link).is_err());

        let tree = dir.path().join("tree");
        std::fs::create_dir(&tree).unwrap();
        let nested_link = tree.join("nested-link");
        symlink_dir(&external_root, &nested_link).unwrap();
        safe_delete_with_control(&tree, false, &no_control).unwrap();
        assert!(
            external_file.exists(),
            "nested link target must remain intact"
        );
        assert!(std::fs::symlink_metadata(&tree).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn portable_executor_rejects_directory_reparse_points_for_copy_and_ancestors() {
        use std::os::windows::fs::symlink_dir;

        let dir = tempfile::tempdir().unwrap();
        let external_root = dir.path().join("external-root");
        std::fs::create_dir(&external_root).unwrap();
        let external_file = external_root.join("source.bin");
        std::fs::write(&external_file, b"external").unwrap();
        let alias = dir.path().join("alias");
        symlink_dir(&external_root, &alias).unwrap();

        let metadata = std::fs::symlink_metadata(&alias).unwrap();
        assert!(metadata_is_reparse_point(&metadata));
        assert!(reject_symlink(&alias, "test").is_err());
        assert!(reject_symlink_ancestors(&alias.join("child.bin"), "test").is_err());

        let destination = dir.path().join("copied.bin");
        let no_control = || Ok(());
        assert!(copy_verify_with_control(&alias, &destination, 8, &no_control).is_err());
        assert!(!destination.exists());
        assert_eq!(std::fs::read(&external_file).unwrap(), b"external");
    }

    #[test]
    fn portable_execute_step_safe_delete_if_present_is_idempotent_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("does-not-exist.bin");

        let step = StoragePlanStep {
            action: PlannedStorageAction::SafeDeleteIfPresent,
            source: Some(target),
            destination: None,
            bytes: 0,
        };
        execute_step(&step).unwrap();

        let nested_missing_target = dir.path().join("missing-parent").join("payload.bin");
        let nested_step = StoragePlanStep {
            action: PlannedStorageAction::SafeDeleteIfPresent,
            source: Some(nested_missing_target),
            destination: None,
            bytes: 0,
        };
        execute_step(&nested_step).unwrap();
    }

    #[test]
    fn portable_execute_step_prune_empty_dirs_removes_up_to_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();

        let step = StoragePlanStep {
            action: PlannedStorageAction::PruneEmptyDirs,
            source: Some(nested),
            destination: Some(root.clone()),
            bytes: 0,
        };
        execute_step(&step).unwrap();

        assert!(root.exists(), "prune must stop at (and keep) the root");
        assert!(!root.join("a").exists());
    }

    // Windows symlink creation normally requires Developer Mode or elevation,
    // which a CI runner may not have -- gate this to Unix so the test
    // exercises reject_symlink()'s logic without depending on the runner's
    // symlink privileges. The ancestor-swap test below already covers the
    // portable executor's Unix symlink handling in more depth.
    #[cfg(unix)]
    #[test]
    fn portable_execute_step_rejects_symlink_source() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.bin");
        let link = dir.path().join("link.bin");
        let destination = dir.path().join("dest.bin");
        std::fs::write(&real, b"data").unwrap();
        symlink(&real, &link).unwrap();

        let step = StoragePlanStep {
            action: PlannedStorageAction::Rename,
            source: Some(link),
            destination: Some(destination.clone()),
            bytes: 4,
        };
        let error = execute_step(&step).unwrap_err();
        assert!(matches!(error, StorageError::StagedMoveFailed { .. }));
        assert!(!destination.exists());
    }

    #[cfg(unix)]
    #[test]
    fn portable_execute_step_rejects_an_ancestor_symlink_swapped_after_validation() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let source = root.path().join("source.bin");
        let alias = root.path().join("alias");
        let destination = alias.join("destination.bin");
        std::fs::write(&source, b"data").unwrap();
        // Simulates the race the up-front `validate_plan_paths_under_roots`
        // check cannot see: by the time this step actually runs, an ancestor
        // directory that validated cleanly earlier has been replaced with a
        // symlink pointing outside the confined root.
        symlink(outside.path(), &alias).unwrap();

        let step = StoragePlanStep {
            action: PlannedStorageAction::CopyVerifyRename,
            source: Some(source),
            destination: Some(destination),
            bytes: 4,
        };
        let error = execute_step(&step).unwrap_err();
        assert!(matches!(
            error,
            StorageError::StagedMoveFailed {
                step: "destination-ancestor",
                ..
            }
        ));
        assert!(!outside.path().join("destination.bin").exists());
    }

    #[test]
    fn portable_rollback_plan_runs_configured_rollback_steps() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged.bin");
        let restored = dir.path().join("restored.bin");
        std::fs::write(&staged, b"data").unwrap();

        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: Vec::new(),
            rollback_steps: vec![StoragePlanStep {
                action: PlannedStorageAction::Rename,
                source: Some(staged.clone()),
                destination: Some(restored.clone()),
                bytes: 4,
            }],
        };
        let (rolled_back, failures) = rollback_plan(&plan);

        assert_eq!(rolled_back.len(), 1);
        assert!(failures.is_empty());
        assert!(!staged.exists());
        assert_eq!(std::fs::read(&restored).unwrap(), b"data");
    }

    #[test]
    fn reconcile_rejects_unverifiable_rename_committed_before_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.bin");
        let destination = root.join("destination.bin");
        std::fs::write(&source, b"data").unwrap();
        let plan = plan_move(&MovePlanRequest {
            source,
            destination: destination.clone(),
            bytes: 4,
            available_bytes: None,
            dry_run: false,
        });

        execute_storage_plan_under_roots(&plan, std::slice::from_ref(&root)).unwrap();

        let error = reconcile_storage_plan_under_roots(&plan, std::slice::from_ref(&root), &[])
            .unwrap_err();
        assert!(error.to_string().contains("cannot prove rename completion"));
        assert_eq!(std::fs::read(destination).unwrap(), b"data");
    }

    #[test]
    fn durable_rename_checkpoint_reconciles_after_source_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.bin");
        let destination = root.join("destination.bin");
        std::fs::write(&source, b"data").unwrap();
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: PlannedStorageAction::Rename,
                source: Some(source.clone()),
                destination: Some(destination.clone()),
                bytes: 4,
            }],
            rollback_steps: Vec::new(),
        };

        execute_storage_plan_under_roots(&plan, std::slice::from_ref(&root)).unwrap();

        assert_eq!(
            reconcile_storage_plan_under_roots(&plan, std::slice::from_ref(&root), &[0]).unwrap(),
            vec![0]
        );
        assert!(!source.exists());
        assert_eq!(std::fs::read(destination).unwrap(), b"data");
    }

    #[test]
    fn checkpointed_delete_rejects_a_recreated_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("target.bin");
        std::fs::write(&target, b"old").unwrap();
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: PlannedStorageAction::SafeDelete,
                source: Some(target.clone()),
                destination: None,
                bytes: 3,
            }],
            rollback_steps: Vec::new(),
        };

        execute_storage_plan_under_roots(&plan, std::slice::from_ref(&root)).unwrap();
        std::fs::write(&target, b"new").unwrap();

        let error = reconcile_storage_plan_under_roots(&plan, std::slice::from_ref(&root), &[0])
            .unwrap_err();
        assert!(matches!(
            error,
            StorageError::FilesystemStateUncertain {
                step: "reconcile",
                ..
            }
        ));
        assert_eq!(std::fs::read(target).unwrap(), b"new");
    }

    #[test]
    fn fully_checkpointed_cross_filesystem_move_reconciles_after_source_delete() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.bin");
        let staging = root.join(".destination.bin.tng-copying");
        let destination = root.join("destination.bin");
        std::fs::write(&destination, b"data").unwrap();
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![
                StoragePlanStep {
                    action: PlannedStorageAction::CopyVerifyRename,
                    source: Some(source.clone()),
                    destination: Some(staging.clone()),
                    bytes: 4,
                },
                StoragePlanStep {
                    action: PlannedStorageAction::Rename,
                    source: Some(staging),
                    destination: Some(destination.clone()),
                    bytes: 4,
                },
                StoragePlanStep {
                    action: PlannedStorageAction::SafeDelete,
                    source: Some(source),
                    destination: None,
                    bytes: 4,
                },
            ],
            rollback_steps: Vec::new(),
        };

        assert_eq!(
            reconcile_storage_plan_under_roots(&plan, std::slice::from_ref(&root), &[0, 1, 2])
                .unwrap(),
            vec![0, 1, 2]
        );
        assert_eq!(std::fs::read(destination).unwrap(), b"data");
    }

    #[test]
    fn failed_checkpoint_can_resume_without_repeating_filesystem_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.bin");
        let destination = root.join("destination.bin");
        std::fs::write(&source, b"data").unwrap();
        let plan = plan_move(&MovePlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 4,
            available_bytes: None,
            dry_run: false,
        });

        // The filesystem syscall succeeds, but the durable checkpoint fails.
        // This is the crash window the worker must recover from on restart.
        let result = execute_storage_plan_under_roots_with_checkpoints(
            &plan,
            std::slice::from_ref(&root),
            &[],
            |_, _| {
                Err(StorageError::StagedMoveFailed {
                    step: "checkpoint",
                    reason: "injected database failure".to_owned(),
                })
            },
        );
        assert!(result.is_err());
        assert!(!source.exists());
        assert_eq!(std::fs::read(&destination).unwrap(), b"data");

        // The callback failed before a durable checkpoint was written. A
        // caller must not turn the in-memory step into proof that the rename
        // was committed by this plan; without that proof the disappeared
        // source remains ambiguous.
        let error = reconcile_storage_plan_under_roots(&plan, std::slice::from_ref(&root), &[])
            .unwrap_err();
        assert!(matches!(
            error,
            StorageError::FilesystemStateUncertain {
                step: "reconcile",
                ..
            }
        ));
        assert_eq!(std::fs::read(&destination).unwrap(), b"data");
    }

    #[test]
    fn execute_rejects_an_out_of_range_checkpoint_before_mutating() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("destination.bin");
        std::fs::write(&source, b"data").unwrap();
        let plan = plan_move(&MovePlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 4,
            available_bytes: None,
            dry_run: false,
        });

        let error = execute_storage_plan_with_checkpoints(&plan, &[1], |_, _| Ok(())).unwrap_err();

        assert!(error.to_string().contains("outside plan length 1"));
        assert!(source.exists());
        assert!(!destination.exists());
    }

    #[test]
    fn controlled_copy_aborts_mid_step_and_rolls_back_staging() {
        use std::cell::Cell;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.bin");
        let destination = root.join("destination.bin");
        std::fs::write(&source, vec![0x5a; 128 * 1024]).unwrap();
        let plan = plan_import(&ImportPlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 128 * 1024,
            available_bytes: None,
            hardlink_or_copy: false,
            dry_run: false,
        });
        let checks = Cell::new(0usize);

        let result = execute_storage_plan_under_roots_with_checkpoints_and_control(
            &plan,
            std::slice::from_ref(&root),
            &[],
            |_, _| Ok(()),
            || {
                let count = checks.get() + 1;
                checks.set(count);
                if count == 8 {
                    Err(StorageError::StagedMoveFailed {
                        step: "cancel",
                        reason: "injected cancellation".to_owned(),
                    })
                } else {
                    Ok(())
                }
            },
        );

        assert!(matches!(
            result,
            Err(StorageError::StagedMoveFailed { step: "cancel", .. })
        ));
        assert!(checks.get() >= 8);
        assert!(source.exists());
        assert!(!destination.exists());
        assert!(!root.join(".destination.bin.tng-copying").exists());
    }

    #[test]
    fn cancellation_after_checkpoint_rolls_back_committed_staging() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.bin");
        let destination = root.join("destination.bin");
        std::fs::write(&source, b"data").unwrap();
        let plan = plan_import(&ImportPlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 4,
            available_bytes: None,
            hardlink_or_copy: false,
            dry_run: false,
        });

        let result = execute_storage_plan_under_roots_with_checkpoints_and_control(
            &plan,
            std::slice::from_ref(&root),
            &[],
            |_, _| {
                Err(StorageError::StagedMoveFailed {
                    step: "cancel",
                    reason: "injected cancellation after filesystem commit".to_owned(),
                })
            },
            || Ok(()),
        );

        assert!(matches!(
            result,
            Err(StorageError::StagedMoveFailed { step: "cancel", .. })
        ));
        assert!(source.exists());
        assert!(!destination.exists());
        assert!(!staging_path(&destination).exists());
    }

    #[test]
    fn cancellation_before_next_step_rolls_back_prior_staging() {
        use std::cell::Cell;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.bin");
        let destination = root.join("destination.bin");
        let staging = staging_path(&destination);
        std::fs::write(&source, b"data").unwrap();
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![
                StoragePlanStep {
                    action: PlannedStorageAction::CopyVerifyRename,
                    source: Some(source.clone()),
                    destination: Some(staging.clone()),
                    bytes: 4,
                },
                StoragePlanStep {
                    action: PlannedStorageAction::Rename,
                    source: Some(staging.clone()),
                    destination: Some(destination.clone()),
                    bytes: 4,
                },
            ],
            rollback_steps: vec![StoragePlanStep {
                action: PlannedStorageAction::SafeDeleteIfPresent,
                source: Some(staging.clone()),
                destination: None,
                bytes: 4,
            }],
        };
        let checkpointed = Cell::new(false);

        let result = execute_storage_plan_under_roots_with_checkpoints_and_control(
            &plan,
            std::slice::from_ref(&root),
            &[],
            |_, _| {
                checkpointed.set(true);
                Ok(())
            },
            || {
                if checkpointed.get() {
                    Err(StorageError::StagedMoveFailed {
                        step: "cancel",
                        reason: "injected cancellation before rename".to_owned(),
                    })
                } else {
                    Ok(())
                }
            },
        );

        assert!(matches!(
            result,
            Err(StorageError::StagedMoveFailed { step: "cancel", .. })
        ));
        assert!(source.exists());
        assert!(!destination.exists());
        assert!(!staging.exists());
    }

    #[test]
    fn controlled_delete_after_partial_removal_requires_manual_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let target = root.join("payload");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("a.bin"), b"a").unwrap();
        std::fs::write(target.join("b.bin"), b"b").unwrap();
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: PlannedStorageAction::SafeDelete,
                source: Some(target.clone()),
                destination: None,
                bytes: 2,
            }],
            rollback_steps: Vec::new(),
        };

        let result = execute_storage_plan_under_roots_with_checkpoints_and_control(
            &plan,
            std::slice::from_ref(&root),
            &[],
            |_, _| Ok(()),
            || {
                if !target.join("a.bin").exists() || !target.join("b.bin").exists() {
                    Err(StorageError::StagedMoveFailed {
                        step: "cancel",
                        reason: "injected cancellation".to_owned(),
                    })
                } else {
                    Ok(())
                }
            },
        );

        assert!(matches!(
            result,
            Err(StorageError::FilesystemStateUncertain { step: "delete", .. })
        ));
        assert!(target.exists());
        assert!(target.join("a.bin").exists() ^ target.join("b.bin").exists());
    }

    #[test]
    fn repeating_a_completed_copy_plan_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("destination.bin");
        std::fs::write(&source, b"data").unwrap();
        let plan = plan_import(&ImportPlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 4,
            available_bytes: None,
            hardlink_or_copy: false,
            dry_run: false,
        });

        execute_storage_plan(&plan).unwrap();
        let retried = execute_storage_plan(&plan).unwrap();

        assert_eq!(retried.applied_steps.len(), plan.steps.len());
        assert!(retried.rolled_back_steps.is_empty());
        assert!(retried.rollback_fully_succeeded());
        assert_eq!(std::fs::read(&destination).unwrap(), b"data");
        assert!(source.exists(), "import must not remove the source");
    }

    #[test]
    fn reconcile_infers_copy_when_following_rename_is_already_committed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.bin");
        let staging = root.join(".destination.bin.tng-copying");
        let destination = root.join("destination.bin");
        std::fs::write(&source, b"data").unwrap();
        // Simulate a process dying after the staging file was renamed but
        // before either checkpoint event reached SQLite.
        std::fs::write(&destination, b"data").unwrap();
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![
                StoragePlanStep {
                    action: PlannedStorageAction::CopyVerifyRename,
                    source: Some(source),
                    destination: Some(staging.clone()),
                    bytes: 4,
                },
                StoragePlanStep {
                    action: PlannedStorageAction::Rename,
                    source: Some(staging),
                    destination: Some(destination),
                    bytes: 4,
                },
            ],
            rollback_steps: Vec::new(),
        };

        assert_eq!(
            reconcile_storage_plan_under_roots(&plan, std::slice::from_ref(&root), &[]).unwrap(),
            vec![0, 1]
        );
    }

    #[test]
    fn reconcile_rejects_corrupt_staging_copy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.bin");
        let staging = root.join(".destination.bin.tng-copying");
        std::fs::write(&source, b"source").unwrap();
        std::fs::write(&staging, b"corrupt").unwrap();
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: PlannedStorageAction::CopyVerifyRename,
                source: Some(source),
                destination: Some(staging),
                bytes: 6,
            }],
            rollback_steps: Vec::new(),
        };

        assert!(matches!(
            reconcile_storage_plan_under_roots(&plan, std::slice::from_ref(&root), &[]),
            Err(StorageError::FilesystemStateUncertain {
                step: "reconcile",
                ..
            })
        ));
    }

    #[test]
    fn reconcile_rejects_conflicting_rename_as_manual_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.bin");
        let destination = root.join("destination.bin");
        std::fs::write(&source, b"source").unwrap();
        std::fs::write(&destination, b"destination").unwrap();
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: PlannedStorageAction::Rename,
                source: Some(source),
                destination: Some(destination),
                bytes: 6,
            }],
            rollback_steps: Vec::new(),
        };

        assert!(matches!(
            reconcile_storage_plan_under_roots(&plan, std::slice::from_ref(&root), &[]),
            Err(StorageError::FilesystemStateUncertain {
                step: "reconcile",
                ..
            })
        ));
    }
}
