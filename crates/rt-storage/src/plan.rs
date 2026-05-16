use std::path::{Path, PathBuf};

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
        .unwrap_or("rtorrentng-staged");
    staged.set_file_name(format!(".{file_name}.rtng-copying"));
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
}
