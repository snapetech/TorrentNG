use std::path::{Component, Path, PathBuf};

use crate::StorageError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannedStorageAction {
    ImportExisting,
    Rename,
    CopyVerifyRename,
    SafeDelete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanIssue {
    SourceMissing(PathBuf),
    DestinationExists(PathBuf),
    InsufficientCapacity { needed: u64, available: u64 },
    DeleteRequiresDryRunApproval,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoragePlanStep {
    pub action: PlannedStorageAction,
    pub source: Option<PathBuf>,
    pub destination: Option<PathBuf>,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    let steps = vec![StoragePlanStep {
        action,
        source: Some(req.source.clone()),
        destination: Some(req.destination.clone()),
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

pub fn plan_delete(req: &DeletePlanRequest) -> StoragePlan {
    let mut issues = Vec::new();
    if !req.target.exists() {
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
}

pub fn execute_storage_plan(plan: &StoragePlan) -> Result<StoragePlanExecution, StorageError> {
    ensure_plan_can_apply(plan)?;
    if plan.dry_run {
        return Ok(StoragePlanExecution::default());
    }

    let mut execution = StoragePlanExecution::default();
    for step in &plan.steps {
        if let Err(error) = execute_step(step) {
            execution.rolled_back_steps = rollback_plan(plan);
            return Err(StorageError::StagedMoveFailed {
                step: "execute",
                reason: error.to_string(),
            });
        }
        execution.applied_steps.push(step.clone());
    }
    Ok(execution)
}

pub fn execute_storage_plan_under_roots(
    plan: &StoragePlan,
    roots: &[PathBuf],
) -> Result<StoragePlanExecution, StorageError> {
    ensure_plan_can_apply(plan)?;
    let roots = canonical_roots(roots)?;
    validate_plan_paths_under_roots(plan, &roots)?;
    if plan.dry_run {
        return Ok(StoragePlanExecution::default());
    }
    execute_storage_plan(plan)
}

fn execute_step(step: &StoragePlanStep) -> Result<(), StorageError> {
    match step.action {
        PlannedStorageAction::ImportExisting => {
            let source = required_path(step.source.as_ref(), "import-source")?;
            let destination = required_path(step.destination.as_ref(), "import-destination")?;
            ensure_destination_available(destination)?;
            create_parent(destination)?;
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
            if source.is_dir() {
                std::fs::remove_dir_all(source)
                    .map_err(|e| StorageError::io(source.display().to_string(), e))
            } else {
                std::fs::remove_file(source)
                    .map_err(|e| StorageError::io(source.display().to_string(), e))
            }
        }
    }
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

fn rollback_plan(plan: &StoragePlan) -> Vec<StoragePlanStep> {
    let mut rolled_back = Vec::new();
    for step in &plan.rollback_steps {
        if execute_step(step).is_ok() {
            rolled_back.push(step.clone());
        }
    }
    rolled_back
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

fn create_parent(path: &Path) -> Result<(), StorageError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| StorageError::io(parent.display().to_string(), e))?;
    }
    Ok(())
}

fn ensure_destination_available(path: &Path) -> Result<(), StorageError> {
    if path.exists() {
        return Err(StorageError::StagedMoveFailed {
            step: "destination",
            reason: format!("destination exists: {}", path.display()),
        });
    }
    Ok(())
}

fn copy_verify(source: &Path, destination: &Path, expected_bytes: u64) -> Result<(), StorageError> {
    if source.is_dir() {
        copy_dir_recursive(source, destination)?;
    } else {
        std::fs::copy(source, destination)
            .map_err(|e| StorageError::io(destination.display().to_string(), e))?;
    }
    verify_path_len(destination, expected_bytes)
}

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

fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<(), StorageError> {
    std::fs::create_dir_all(destination)
        .map_err(|e| StorageError::io(destination.display().to_string(), e))?;
    for entry in
        std::fs::read_dir(source).map_err(|e| StorageError::io(source.display().to_string(), e))?
    {
        let entry = entry.map_err(|e| StorageError::io(source.display().to_string(), e))?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir_recursive(&source_path, &destination_path)?;
        } else {
            ensure_destination_available(&destination_path)?;
            std::fs::copy(&source_path, &destination_path)
                .map_err(|e| StorageError::io(destination_path.display().to_string(), e))?;
        }
    }
    Ok(())
}

fn path_content_len(path: &Path) -> Result<u64, StorageError> {
    let metadata =
        std::fs::metadata(path).map_err(|e| StorageError::io(path.display().to_string(), e))?;
    if metadata.is_dir() {
        let mut total = 0u64;
        for entry in
            std::fs::read_dir(path).map_err(|e| StorageError::io(path.display().to_string(), e))?
        {
            let entry = entry.map_err(|e| StorageError::io(path.display().to_string(), e))?;
            total = total.saturating_add(path_content_len(&entry.path())?);
        }
        Ok(total)
    } else {
        Ok(metadata.len())
    }
}

fn common_issues(
    source: &Path,
    destination: &Path,
    bytes: u64,
    available_bytes: Option<u64>,
) -> Vec<PlanIssue> {
    let mut issues = Vec::new();
    if !source.exists() {
        issues.push(PlanIssue::SourceMissing(source.to_path_buf()));
    }
    if destination.exists() {
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

    let source_dev = std::fs::metadata(source).ok()?.dev();
    let dest_parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let dest_dev = std::fs::metadata(dest_parent).ok()?.dev();
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
}
