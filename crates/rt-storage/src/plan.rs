use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use crate::StorageError;
use rt_path::SafeRelPath;

#[cfg(unix)]
use crate::secure_fs;

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
    let issues = common_issues(
        &req.source,
        &req.destination,
        req.bytes,
        req.available_bytes,
    );
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
        action: PlannedStorageAction::SafeDelete,
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
    let issues = common_issues(
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
                action: PlannedStorageAction::SafeDelete,
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
    if plan.can_apply {
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

    let mut completed = checkpointed_steps.iter().copied().collect::<HashSet<_>>();
    if let Some(index) = completed
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

    // First classify each step from its own source/destination state. Then
    // repeat the inference pass because a plan can contain more than one
    // copy/rename pair.
    loop {
        let mut changed = false;
        for (index, step) in plan.steps.iter().enumerate() {
            if completed.contains(&index) {
                continue;
            }
            if storage_step_is_applied(step)? {
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

fn storage_step_is_applied(step: &StoragePlanStep) -> Result<bool, StorageError> {
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
                return Err(StorageError::StagedMoveFailed {
                    step: "reconcile",
                    reason: format!(
                        "rename has both source and destination present: {} -> {}",
                        source.display(),
                        destination.display()
                    ),
                });
            }
            Ok(!source_exists && destination_exists)
        }
        PlannedStorageAction::CopyVerifyRename => {
            let destination = destination.ok_or_else(|| StorageError::StagedMoveFailed {
                step: "reconcile-destination",
                reason: "missing path".to_owned(),
            })?;
            if !path_exists_no_follow(destination) {
                return Ok(false);
            }
            // A source that is still present lets us detect a torn or
            // corrupted staging copy. If the source disappeared, the
            // destination is still recoverable evidence and the following
            // plan step determines whether it was a completed rename.
            if path_exists_no_follow(source) {
                verify_content_matches(source, destination)?;
            }
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
            if path_exists_no_follow(source) {
                verify_content_matches(source, destination)?;
            }
            Ok(true)
        }
        PlannedStorageAction::SafeDelete | PlannedStorageAction::SafeDeleteIfPresent => {
            Ok(!path_exists_no_follow(source))
        }
        PlannedStorageAction::PruneEmptyDirs => Ok(!path_exists_no_follow(source)),
    }
}

#[cfg(any(not(unix), test))]
pub(crate) fn execute_storage_plan(
    plan: &StoragePlan,
) -> Result<StoragePlanExecution, StorageError> {
    execute_storage_plan_with_checkpoints(plan, &[], |_, _| Ok(()))
}

#[cfg(any(not(unix), test))]
pub(crate) fn execute_storage_plan_with_checkpoints<F>(
    plan: &StoragePlan,
    completed_steps: &[usize],
    checkpoint_step: F,
) -> Result<StoragePlanExecution, StorageError>
where
    F: FnMut(usize, &StoragePlanStep) -> Result<(), StorageError>,
{
    let no_control = || Ok(());
    execute_storage_plan_with_executor(
        plan,
        completed_steps,
        checkpoint_step,
        execute_step,
        rollback_plan,
        storage_step_is_applied,
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
    A: Fn(&StoragePlanStep) -> Result<bool, StorageError>,
{
    ensure_plan_can_apply(plan)?;
    if plan.dry_run {
        return Ok(StoragePlanExecution::default());
    }

    let completed = completed_steps.iter().copied().collect::<HashSet<_>>();
    let mut completed = completed;
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
            check_control()?;
            if step_is_applied_fn(step)? {
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
                checkpoint_step(index, step)?;
            }
            execution.applied_steps.push(step.clone());
            continue;
        }
        check_control()?;
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
        checkpoint_step(index, step)?;
        execution.applied_steps.push(step.clone());
    }
    Ok(execution)
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
    #[cfg(unix)]
    {
        let check_control_ref: &dyn Fn() -> Result<(), StorageError> = &check_control;
        execute_storage_plan_with_executor(
            plan,
            completed_steps,
            checkpoint_step,
            |step| secure_fs::execute_step_with_control(step, &roots, check_control_ref),
            |plan| secure_fs::rollback_plan(plan, &roots),
            |step| secure_fs::step_is_applied(step, &roots),
            check_control_ref,
        )
    }
    #[cfg(not(unix))]
    {
        // The non-Unix executor has a weaker path-authority implementation,
        // but it still must honor the worker control plane. Ignoring pause,
        // cancel, or shutdown here would turn a portability fallback into an
        // uninterruptible storage job.
        execute_storage_plan_with_executor(
            plan,
            completed_steps,
            checkpoint_step,
            execute_step,
            rollback_plan,
            storage_step_is_applied,
            &check_control,
        )
    }
}

#[cfg(any(not(unix), test))]
fn execute_step(step: &StoragePlanStep) -> Result<(), StorageError> {
    match step.action {
        PlannedStorageAction::ImportExisting => {
            let source = required_path(step.source.as_ref(), "import-source")?;
            let destination = required_path(step.destination.as_ref(), "import-destination")?;
            ensure_destination_available(destination)?;
            create_parent(destination)?;
            reject_symlink(source, "import-source")?;
            match std::fs::hard_link(source, destination) {
                Ok(()) => verify_path_len(destination, step.bytes),
                Err(_) => copy_verify(source, destination, step.bytes),
            }
        }
        PlannedStorageAction::Rename => {
            let source = required_path(step.source.as_ref(), "rename-source")?;
            let destination = required_path(step.destination.as_ref(), "rename-destination")?;
            ensure_destination_available(destination)?;
            create_parent(destination)?;
            reject_symlink(source, "rename-source")?;
            std::fs::rename(source, destination)
                .map_err(|e| StorageError::io(destination.display().to_string(), e))?;
            verify_path_len(destination, step.bytes)
        }
        PlannedStorageAction::CopyVerifyRename => {
            let source = required_path(step.source.as_ref(), "copy-source")?;
            let destination = required_path(step.destination.as_ref(), "copy-destination")?;
            ensure_destination_available(destination)?;
            create_parent(destination)?;
            copy_verify(source, destination, step.bytes)
        }
        PlannedStorageAction::SafeDelete => {
            let source = required_path(step.source.as_ref(), "delete-source")?;
            let metadata = safe_symlink_metadata(source, "delete-source")?;
            let file_type = metadata.file_type();
            if file_type.is_dir() {
                std::fs::remove_dir_all(source)
                    .map_err(|e| StorageError::io(source.display().to_string(), e))
            } else if file_type.is_file() || file_type.is_symlink() {
                std::fs::remove_file(source)
                    .map_err(|e| StorageError::io(source.display().to_string(), e))
            } else {
                Err(StorageError::StagedMoveFailed {
                    step: "delete-source",
                    reason: format!("unsupported file type: {}", source.display()),
                })
            }
        }
        PlannedStorageAction::SafeDeleteIfPresent => {
            let source = required_path(step.source.as_ref(), "delete-source")?;
            let metadata = match std::fs::symlink_metadata(source) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => {
                    return Err(StorageError::io(source.display().to_string(), error));
                }
            };
            let file_type = metadata.file_type();
            if file_type.is_dir() {
                std::fs::remove_dir_all(source)
                    .map_err(|e| StorageError::io(source.display().to_string(), e))
            } else if file_type.is_file() || file_type.is_symlink() {
                match std::fs::remove_file(source) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(StorageError::io(source.display().to_string(), error)),
                }
            } else {
                Err(StorageError::StagedMoveFailed {
                    step: "delete-source",
                    reason: format!("unsupported file type: {}", source.display()),
                })
            }
        }
        PlannedStorageAction::PruneEmptyDirs => {
            let start = required_path(step.source.as_ref(), "prune-source")?;
            let root = required_path(step.destination.as_ref(), "prune-root")?;
            prune_empty_dirs(start, root)
        }
    }
}

#[cfg(any(not(unix), test))]
fn prune_empty_dirs(mut current: &Path, root: &Path) -> Result<(), StorageError> {
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
    while current != root {
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
            Err(error) => return Err(StorageError::io(current.display().to_string(), error)),
        };
        if metadata.file_type().is_symlink() {
            return Err(unsafe_symlink_error(current, "prune-source"));
        }
        if !metadata.is_dir() {
            return Err(StorageError::StagedMoveFailed {
                step: "prune-source",
                reason: format!("not a directory: {}", current.display()),
            });
        }
        match std::fs::remove_dir(current) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => break,
            Err(error) => return Err(StorageError::io(current.display().to_string(), error)),
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
        if let Some(source) = &step.source {
            ensure_path_under_roots(source, roots, "source-root")?;
        }
        if let Some(destination) = &step.destination {
            ensure_path_under_roots(destination, roots, "destination-root")?;
        }
    }
    Ok(())
}

fn ensure_path_under_roots(
    path: &Path,
    roots: &[PathBuf],
    step: &'static str,
) -> Result<(), StorageError> {
    let resolved = resolve_confined_path(path)?;
    if roots.iter().any(|root| resolved.starts_with(root)) {
        Ok(())
    } else {
        Err(StorageError::StagedMoveFailed {
            step,
            reason: format!("path outside configured storage roots: {}", path.display()),
        })
    }
}

fn resolve_confined_path(path: &Path) -> Result<PathBuf, StorageError> {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
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
#[cfg(any(not(unix), test))]
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

fn required_path<'a>(
    path: Option<&'a PathBuf>,
    step: &'static str,
) -> Result<&'a Path, StorageError> {
    path.map(PathBuf::as_path)
        .ok_or_else(|| StorageError::StagedMoveFailed {
            step,
            reason: "missing path".to_string(),
        })
}

#[cfg(any(not(unix), test))]
fn create_parent(path: &Path) -> Result<(), StorageError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| StorageError::io(parent.display().to_string(), e))?;
    }
    Ok(())
}

#[cfg(any(not(unix), test))]
fn ensure_destination_available(path: &Path) -> Result<(), StorageError> {
    if path_exists_no_follow(path) {
        return Err(StorageError::StagedMoveFailed {
            step: "destination",
            reason: format!("destination exists: {}", path.display()),
        });
    }
    Ok(())
}

#[cfg(any(not(unix), test))]
fn copy_verify(source: &Path, destination: &Path, expected_bytes: u64) -> Result<(), StorageError> {
    let metadata = safe_symlink_metadata(source, "copy-source")?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(unsafe_symlink_error(source, "copy-source"));
    }
    if file_type.is_dir() {
        copy_dir_recursive(source, destination)?;
    } else if file_type.is_file() {
        std::fs::copy(source, destination)
            .map_err(|e| StorageError::io(destination.display().to_string(), e))?;
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
    verify_path_len(destination, expected_bytes)?;
    verify_content_matches(source, destination)
}

/// Recursively verifies that every regular file under `source` has bytes
/// identical to its counterpart under `destination`, via a streaming SHA-1
/// content hash (never loads a whole file into memory). Never follows
/// symlinks on either side.
fn verify_content_matches(source: &Path, destination: &Path) -> Result<(), StorageError> {
    let source_meta = safe_symlink_metadata(source, "verify-content-source")?;
    let destination_meta = safe_symlink_metadata(destination, "verify-content-destination")?;
    let source_type = source_meta.file_type();
    let destination_type = destination_meta.file_type();
    if source_type.is_symlink() {
        return Err(unsafe_symlink_error(source, "verify-content-source"));
    }
    if destination_type.is_symlink() {
        return Err(unsafe_symlink_error(
            destination,
            "verify-content-destination",
        ));
    }

    if source_type.is_dir() {
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
        for entry in std::fs::read_dir(source)
            .map_err(|e| StorageError::io(source.display().to_string(), e))?
        {
            let entry = entry.map_err(|e| StorageError::io(source.display().to_string(), e))?;
            let destination_child = destination.join(entry.file_name());
            verify_content_matches(&entry.path(), &destination_child)?;
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
        let source_hash = hash_file_sha1(source)?;
        let destination_hash = hash_file_sha1(destination)?;
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

fn hash_file_sha1(path: &Path) -> Result<[u8; 20], StorageError> {
    use sha1::{Digest, Sha1};
    use std::fs::OpenOptions;
    use std::io::Read;

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .map_err(|e| StorageError::io(path.display().to_string(), e))?;
    let mut hasher = Sha1::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
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

#[cfg(any(not(unix), test))]
fn verify_path_len(path: &Path, expected_bytes: u64) -> Result<(), StorageError> {
    let actual = path_content_len(path)?;
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

#[cfg(any(not(unix), test))]
fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<(), StorageError> {
    reject_symlink(source, "copy-source")?;
    std::fs::create_dir_all(destination)
        .map_err(|e| StorageError::io(destination.display().to_string(), e))?;
    for entry in
        std::fs::read_dir(source).map_err(|e| StorageError::io(source.display().to_string(), e))?
    {
        let entry = entry.map_err(|e| StorageError::io(source.display().to_string(), e))?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = safe_symlink_metadata(&source_path, "copy-source")?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            return Err(unsafe_symlink_error(&source_path, "copy-source"));
        }
        if file_type.is_dir() {
            copy_dir_recursive(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            ensure_destination_available(&destination_path)?;
            std::fs::copy(&source_path, &destination_path)
                .map_err(|e| StorageError::io(destination_path.display().to_string(), e))?;
        } else {
            return Err(StorageError::StagedMoveFailed {
                step: "copy-source",
                reason: format!("unsupported file type: {}", source_path.display()),
            });
        }
    }
    Ok(())
}

#[cfg(any(not(unix), test))]
fn path_content_len(path: &Path) -> Result<u64, StorageError> {
    let metadata = safe_symlink_metadata(path, "verify")?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(unsafe_symlink_error(path, "verify"));
    }
    if file_type.is_dir() {
        let mut total = 0u64;
        for entry in
            std::fs::read_dir(path).map_err(|e| StorageError::io(path.display().to_string(), e))?
        {
            let entry = entry.map_err(|e| StorageError::io(path.display().to_string(), e))?;
            total = total.saturating_add(path_content_len(&entry.path())?);
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
fn reject_symlink(path: &Path, step: &'static str) -> Result<(), StorageError> {
    if safe_symlink_metadata(path, step)?.file_type().is_symlink() {
        Err(unsafe_symlink_error(path, step))
    } else {
        Ok(())
    }
}

fn unsafe_symlink_error(path: &Path, step: &'static str) -> StorageError {
    StorageError::StagedMoveFailed {
        step,
        reason: format!(
            "symlink entries are not allowed in move/import plans: {}",
            path.display()
        ),
    }
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

#[cfg(not(unix))]
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

        assert!(plan.can_apply);
        assert_eq!(plan.steps[0].action, PlannedStorageAction::Rename);
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
                action: PlannedStorageAction::SafeDelete,
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
            Err(StorageError::StagedMoveFailed { .. })
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
                action: PlannedStorageAction::SafeDelete,
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
    fn checkpoint_failure_leaves_committed_step_resumable() {
        let dir = tempfile::tempdir().unwrap();
        let source_one = dir.path().join("source-one.bin");
        let source_two = dir.path().join("source-two.bin");
        let destination_one = dir.path().join("destination-one.bin");
        let destination_two = dir.path().join("destination-two.bin");
        std::fs::write(&source_one, b"one").unwrap();
        std::fs::write(&source_two, b"two").unwrap();
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![
                StoragePlanStep {
                    action: PlannedStorageAction::Rename,
                    source: Some(source_one.clone()),
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
        assert!(!source_one.exists());
        assert!(destination_one.exists());
        assert!(source_two.exists());
        assert!(!destination_two.exists());

        let resumed = execute_storage_plan_with_checkpoints(&plan, &[0], |_, _| Ok(())).unwrap();
        assert_eq!(resumed.applied_steps.len(), 2);
        assert!(!source_two.exists());
        assert!(destination_two.exists());
    }

    #[test]
    fn retrying_a_completed_plan_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let destination = dir.path().join("destination.bin");
        std::fs::write(&source, b"data").unwrap();
        let plan = plan_move(&MovePlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 4,
            available_bytes: Some(100),
            dry_run: false,
        });

        execute_storage_plan(&plan).unwrap();
        let retry = execute_storage_plan(&plan).unwrap();

        assert_eq!(retry.applied_steps.len(), plan.steps.len());
        assert!(!source.exists());
        assert_eq!(std::fs::read(destination).unwrap(), b"data");
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
                action: PlannedStorageAction::SafeDelete,
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

    #[test]
    fn reconcile_detects_rename_committed_before_checkpoint() {
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

        assert_eq!(
            reconcile_storage_plan_under_roots(&plan, std::slice::from_ref(&root), &[]).unwrap(),
            vec![0]
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

        let completed =
            reconcile_storage_plan_under_roots(&plan, std::slice::from_ref(&root), &[]).unwrap();
        assert_eq!(completed, vec![0]);

        let resumed = execute_storage_plan_under_roots_with_checkpoints(
            &plan,
            std::slice::from_ref(&root),
            &completed,
            |_, _| panic!("a reconciled step must not be executed again"),
        )
        .unwrap();
        assert_eq!(resumed.applied_steps.len(), 1);
        assert_eq!(std::fs::read(&destination).unwrap(), b"data");
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
            Err(StorageError::StagedMoveFailed {
                step: "verify-content",
                ..
            })
        ));
    }
}
