//! Handle-relative storage-plan execution for Windows.
//!
//! All paths are reduced to a configured storage root and then traversed with
//! capability directories. Recursive enumeration, file opens, creation,
//! deletion, and rename operate relative to opened directory handles. Windows
//! directory handles are held without `FILE_SHARE_DELETE`, and final opens
//! use `FILE_FLAG_OPEN_REPARSE_POINT` so reparse points are rejected instead
//! of followed.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Path, PathBuf};
use std::ptr;

use cap_std::fs::{Dir, File, Metadata, MetadataExt, OpenOptions, OpenOptionsExt};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::{
    FileRenameInfo, SetFileInformationByHandle, DELETE, FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_INFO_BY_HANDLE_CLASS, FILE_READ_ATTRIBUTES,
    FILE_RENAME_INFO, FILE_RENAME_INFO_0, FILE_SHARE_READ, FILE_SHARE_WRITE, SYNCHRONIZE,
};

use crate::plan::{
    ensure_storage_tree_depth, is_cancellation_error, reject_symlink_ancestors, required_path,
    resolve_confined_path, unsafe_symlink_error, PlannedStorageAction, StoragePlan,
    StoragePlanStep,
};
use crate::StorageError;

struct Entry {
    parent: Dir,
    name: OsString,
    path: PathBuf,
}

pub(super) fn execute_step_with_control(
    step: &StoragePlanStep,
    roots: &[PathBuf],
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    check_control()?;
    match step.action {
        PlannedStorageAction::ImportExisting => {
            let source = required_path(step.source.as_ref(), "import-source")?;
            let destination = required_path(step.destination.as_ref(), "import-destination")?;
            let source_entry = required_entry(source, roots, false, "import-source")?;
            let destination_entry = required_entry(destination, roots, true, "import-destination")?;
            ensure_missing(&destination_entry, "destination")?;
            let metadata = entry_metadata(&source_entry, "import-source")?;
            ensure_supported_entry(&metadata, source, "import-source")?;
            verify_path_len(source, roots, step.bytes, check_control)?;
            copy_verify(source, destination, roots, step.bytes, check_control)
        }
        PlannedStorageAction::Rename => {
            let source = required_path(step.source.as_ref(), "rename-source")?;
            let destination = required_path(step.destination.as_ref(), "rename-destination")?;
            let source_entry = required_entry(source, roots, false, "rename-source")?;
            let destination_entry = required_entry(destination, roots, true, "rename-destination")?;
            ensure_missing(&destination_entry, "destination")?;
            let metadata = entry_metadata(&source_entry, "rename-source")?;
            ensure_supported_entry(&metadata, source, "rename-source")?;
            verify_path_len(source, roots, step.bytes, check_control)?;
            rename_entry_no_replace(&source_entry, &destination_entry)?;
            if let Err(error) = verify_path_len(destination, roots, step.bytes, check_control) {
                let source_absent = !path_exists(source, roots, "rename-rollback")?;
                let destination_now = required_entry(destination, roots, false, "rename-rollback")?;
                if source_absent && rename_entry_no_replace(&destination_now, &source_entry).is_ok()
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
            copy_verify(source, destination, roots, step.bytes, check_control)
        }
        PlannedStorageAction::SafeDelete => {
            let source = required_path(step.source.as_ref(), "delete-source")?;
            safe_delete(source, roots, false, check_control)
        }
        PlannedStorageAction::SafeDeleteIfPresent => {
            let source = required_path(step.source.as_ref(), "delete-source")?;
            safe_delete(source, roots, true, check_control)
        }
        PlannedStorageAction::PruneEmptyDirs => {
            let start = required_path(step.source.as_ref(), "prune-source")?;
            let root = required_path(step.destination.as_ref(), "prune-root")?;
            prune_empty_dirs(start, root, roots, check_control)
        }
    }
}

pub(super) fn rollback_plan(
    plan: &StoragePlan,
    roots: &[PathBuf],
) -> (Vec<StoragePlanStep>, Vec<(StoragePlanStep, String)>) {
    let mut rolled_back = Vec::new();
    let mut failures = Vec::new();
    let no_control = || Ok(());
    for step in &plan.rollback_steps {
        match execute_step_with_control(step, roots, &no_control) {
            Ok(()) => rolled_back.push(step.clone()),
            Err(error) => failures.push((step.clone(), error.to_string())),
        }
    }
    (rolled_back, failures)
}

pub(super) fn step_is_applied(
    plan: &StoragePlan,
    index: usize,
    roots: &[PathBuf],
    checkpointed: bool,
) -> Result<bool, StorageError> {
    let step = &plan.steps[index];
    let source = required_path(step.source.as_ref(), "reconcile-source")?;
    let destination = step.destination.as_deref();
    match step.action {
        PlannedStorageAction::Rename => {
            let destination =
                destination.ok_or_else(|| plan_error("reconcile-destination", "missing path"))?;
            let source_exists = path_exists(source, roots, "reconcile-source")?;
            let destination_exists = path_exists(destination, roots, "reconcile-destination")?;
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
                    verify_reconciled_length(destination, roots, step.bytes)?;
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
                    || !path_exists(previous_source, roots, "reconcile-previous-source")?
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
                reconcile_content_matches(previous_source, destination, roots)?;
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
        PlannedStorageAction::CopyVerifyRename | PlannedStorageAction::ImportExisting => {
            let destination =
                destination.ok_or_else(|| plan_error("reconcile-destination", "missing path"))?;
            if !path_exists(destination, roots, "reconcile-destination")? {
                return Ok(false);
            }
            if !path_exists(source, roots, "reconcile-source")? {
                return Err(StorageError::FilesystemStateUncertain {
                    step: "reconcile",
                    reason: format!(
                        "cannot prove copy completion after source disappeared: {} -> {}",
                        source.display(),
                        destination.display()
                    ),
                });
            }
            reconcile_content_matches(source, destination, roots)?;
            Ok(true)
        }
        PlannedStorageAction::SafeDelete | PlannedStorageAction::SafeDeleteIfPresent => {
            let source_exists = path_exists(source, roots, "reconcile-source")?;
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
        PlannedStorageAction::PruneEmptyDirs => {
            Ok(!path_exists(source, roots, "reconcile-source")?)
        }
    }
}

fn required_entry(
    path: &Path,
    roots: &[PathBuf],
    create_parents: bool,
    step: &'static str,
) -> Result<Entry, StorageError> {
    path_entry(path, roots, create_parents, step)?.ok_or_else(|| StorageError::StagedMoveFailed {
        step,
        reason: format!("path does not exist: {}", path.display()),
    })
}

fn path_entry(
    path: &Path,
    roots: &[PathBuf],
    create_parents: bool,
    step: &'static str,
) -> Result<Option<Entry>, StorageError> {
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(plan_error(
            step,
            &format!(
                "path must be an absolute file or directory path: {}",
                path.display()
            ),
        ));
    }
    reject_symlink_ancestors(path, step)?;
    let name = path.file_name().expect("checked above").to_os_string();
    let raw_parent = path
        .parent()
        .ok_or_else(|| plan_error(step, "path has no parent"))?;
    let resolved_parent = resolve_confined_path(raw_parent)?;
    let root = roots
        .iter()
        .find(|root| resolved_parent.starts_with(root))
        .ok_or_else(|| StorageError::StagedMoveFailed {
            step,
            reason: format!("path outside configured storage roots: {}", path.display()),
        })?;
    let relative_parent =
        resolved_parent
            .strip_prefix(root)
            .map_err(|_| StorageError::StagedMoveFailed {
                step,
                reason: format!("path outside configured storage roots: {}", path.display()),
            })?;
    let root_dir = crate::open::open_windows_directory_capability(root)
        .map_err(|error| StorageError::io(root.display().to_string(), error))?;
    let Some(parent) =
        open_relative_directory(&root_dir, relative_parent, create_parents, path, step)?
    else {
        return Ok(None);
    };
    Ok(Some(Entry {
        parent,
        name,
        path: path.to_path_buf(),
    }))
}

fn open_relative_directory(
    root: &Dir,
    relative: &Path,
    create_missing: bool,
    display_path: &Path,
    step: &'static str,
) -> Result<Option<Dir>, StorageError> {
    let mut current = crate::open::reopen_windows_directory(root)
        .map_err(|error| StorageError::io(display_path.display().to_string(), error))?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(plan_error(
                step,
                &format!("unsafe path component: {}", display_path.display()),
            ));
        };
        let component_path = Path::new(name);
        match current.symlink_metadata(component_path) {
            Ok(metadata) => {
                if is_reparse(&metadata) {
                    return Err(unsafe_symlink_error(display_path, step));
                }
                if !metadata.is_dir() {
                    return Err(StorageError::StagedMoveFailed {
                        step,
                        reason: format!(
                            "path component is not a directory: {}",
                            display_path.display()
                        ),
                    });
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && create_missing => {
                match current.create_dir(component_path) {
                    Ok(()) => {}
                    Err(create_error) if create_error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(create_error) => {
                        return Err(StorageError::io(
                            display_path.display().to_string(),
                            create_error,
                        ))
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::io(display_path.display().to_string(), error)),
        }
        current = open_child_directory(&current, name, display_path, step)?;
    }
    Ok(Some(current))
}

fn open_child_directory(
    parent: &Dir,
    name: &std::ffi::OsStr,
    display_path: &Path,
    step: &'static str,
) -> Result<Dir, StorageError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let file = parent
        .open_with(Path::new(name), &options)
        .map_err(|error| StorageError::io(display_path.display().to_string(), error))?;
    let metadata = file
        .metadata()
        .map_err(|error| StorageError::io(display_path.display().to_string(), error))?;
    if is_reparse(&metadata) {
        return Err(unsafe_symlink_error(display_path, step));
    }
    if !metadata.is_dir() {
        return Err(StorageError::StagedMoveFailed {
            step,
            reason: format!(
                "path component is not a directory: {}",
                display_path.display()
            ),
        });
    }
    Ok(Dir::from_std_file(file.into_std()))
}

fn entry_metadata(entry: &Entry, _step: &'static str) -> Result<Metadata, StorageError> {
    entry
        .parent
        .symlink_metadata(Path::new(&entry.name))
        .map_err(|error| StorageError::io(entry.path.display().to_string(), error))
}

fn is_reparse(metadata: &Metadata) -> bool {
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn ensure_supported_entry(
    metadata: &Metadata,
    path: &Path,
    step: &'static str,
) -> Result<(), StorageError> {
    if is_reparse(metadata) {
        return Err(unsafe_symlink_error(path, step));
    }
    if metadata.is_file() || metadata.is_dir() {
        Ok(())
    } else {
        Err(StorageError::StagedMoveFailed {
            step,
            reason: format!("unsupported file type: {}", path.display()),
        })
    }
}

fn ensure_missing(entry: &Entry, step: &'static str) -> Result<(), StorageError> {
    match entry.parent.symlink_metadata(Path::new(&entry.name)) {
        Ok(_) => Err(StorageError::StagedMoveFailed {
            step,
            reason: format!("destination exists: {}", entry.path.display()),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StorageError::io(entry.path.display().to_string(), error)),
    }
}

fn path_exists(path: &Path, roots: &[PathBuf], step: &'static str) -> Result<bool, StorageError> {
    let Some(entry) = path_entry(path, roots, false, step)? else {
        return Ok(false);
    };
    match entry.parent.symlink_metadata(Path::new(&entry.name)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(StorageError::io(path.display().to_string(), error)),
    }
}

fn copy_verify(
    source: &Path,
    destination: &Path,
    roots: &[PathBuf],
    expected_bytes: u64,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    let source_entry = required_entry(source, roots, false, "copy-source")?;
    let destination_entry = required_entry(destination, roots, true, "copy-destination")?;
    ensure_missing(&destination_entry, "destination")?;
    let mut destination_created = false;
    let result = (|| {
        let source_metadata = entry_metadata(&source_entry, "copy-source")?;
        ensure_supported_entry(&source_metadata, source, "copy-source")?;
        let mut copied = 0u64;
        copy_entry(
            &source_entry,
            &destination_entry,
            roots,
            0,
            &mut copied,
            expected_bytes,
            check_control,
            &mut destination_created,
        )?;
        verify_path_len(destination, roots, expected_bytes, check_control)?;
        verify_content_matches(source, destination, roots, check_control)
    })();
    if let Err(error) = result {
        if destination_created {
            if let Err(cleanup_error) = safe_delete(destination, roots, true, &|| Ok(())) {
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

fn copy_entry(
    source: &Entry,
    destination: &Entry,
    roots: &[PathBuf],
    depth: usize,
    copied: &mut u64,
    expected_bytes: u64,
    check_control: &dyn Fn() -> Result<(), StorageError>,
    destination_created: &mut bool,
) -> Result<(), StorageError> {
    check_control()?;
    let source_metadata = entry_metadata(source, "copy-source")?;
    ensure_supported_entry(&source_metadata, &source.path, "copy-source")?;
    if source_metadata.is_dir() {
        ensure_storage_tree_depth(depth, &source.path, "copy-tree-depth")?;
        destination
            .parent
            .create_dir(Path::new(&destination.name))
            .map_err(|error| StorageError::io(destination.path.display().to_string(), error))?;
        *destination_created = true;
        let source_dir =
            open_child_directory(&source.parent, &source.name, &source.path, "copy-source")?;
        let destination_dir = open_child_directory(
            &destination.parent,
            &destination.name,
            &destination.path,
            "copy-destination",
        )?;
        copy_directory_contents(
            &source_dir,
            &destination_dir,
            &source.path,
            &destination.path,
            roots,
            depth,
            copied,
            expected_bytes,
            check_control,
            destination_created,
        )?;
    } else {
        let mut source_options = OpenOptions::new();
        source_options
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let mut source_file = source
            .parent
            .open_with(Path::new(&source.name), &source_options)
            .map_err(|error| StorageError::io(source.path.display().to_string(), error))?;
        ensure_open_file_regular(&source_file, &source.path, "copy-source")?;
        let mut destination_options = OpenOptions::new();
        destination_options
            .write(true)
            .create_new(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let mut destination_file = destination
            .parent
            .open_with(Path::new(&destination.name), &destination_options)
            .map_err(|error| StorageError::io(destination.path.display().to_string(), error))?;
        *destination_created = true;
        ensure_open_file_regular(&destination_file, &destination.path, "copy-destination")?;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            check_control()?;
            let read = source_file
                .read(&mut buffer)
                .map_err(|error| StorageError::io(source.path.display().to_string(), error))?;
            if read == 0 {
                break;
            }
            let next = copied
                .checked_add(read as u64)
                .ok_or_else(|| plan_error("copy-size", "copied byte count overflow"))?;
            if next > expected_bytes {
                return Err(safe_path_short_io(&destination.path, expected_bytes, next));
            }
            destination_file
                .write_all(&buffer[..read])
                .map_err(|error| StorageError::io(destination.path.display().to_string(), error))?;
            *copied = next;
        }
        destination_file
            .sync_all()
            .map_err(|error| StorageError::io(destination.path.display().to_string(), error))?;
    }
    Ok(())
}

fn copy_directory_contents(
    source: &Dir,
    destination: &Dir,
    source_path: &Path,
    destination_path: &Path,
    roots: &[PathBuf],
    depth: usize,
    copied: &mut u64,
    expected_bytes: u64,
    check_control: &dyn Fn() -> Result<(), StorageError>,
    destination_created: &mut bool,
) -> Result<(), StorageError> {
    for item in source
        .entries()
        .map_err(|error| StorageError::io(source_path.display().to_string(), error))?
    {
        check_control()?;
        let item =
            item.map_err(|error| StorageError::io(source_path.display().to_string(), error))?;
        let name = item.file_name();
        let source_child_path = source_path.join(&name);
        let destination_child_path = destination_path.join(&name);
        let source_child = Entry {
            parent: crate::open::reopen_windows_directory(source)
                .map_err(|error| StorageError::io(source_path.display().to_string(), error))?,
            name: name.clone(),
            path: source_child_path,
        };
        let destination_child = Entry {
            parent: crate::open::reopen_windows_directory(destination)
                .map_err(|error| StorageError::io(destination_path.display().to_string(), error))?,
            name,
            path: destination_child_path,
        };
        copy_entry(
            &source_child,
            &destination_child,
            roots,
            depth.saturating_add(1),
            copied,
            expected_bytes,
            check_control,
            destination_created,
        )?;
    }
    Ok(())
}

fn ensure_open_file_regular(
    file: &File,
    path: &Path,
    step: &'static str,
) -> Result<(), StorageError> {
    let metadata = file
        .metadata()
        .map_err(|error| StorageError::io(path.display().to_string(), error))?;
    if is_reparse(&metadata) {
        return Err(unsafe_symlink_error(path, step));
    }
    if !metadata.is_file() {
        return Err(StorageError::StagedMoveFailed {
            step,
            reason: format!("not a regular file: {}", path.display()),
        });
    }
    Ok(())
}

fn verify_path_len(
    path: &Path,
    roots: &[PathBuf],
    expected_bytes: u64,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    let actual = content_len(path, roots, 0, check_control)?;
    if actual == expected_bytes {
        Ok(())
    } else {
        Err(safe_path_short_io(path, expected_bytes, actual))
    }
}

fn content_len(
    path: &Path,
    roots: &[PathBuf],
    depth: usize,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<u64, StorageError> {
    check_control()?;
    let entry = required_entry(path, roots, false, "verify")?;
    let metadata = entry_metadata(&entry, "verify")?;
    ensure_supported_entry(&metadata, path, "verify")?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    ensure_storage_tree_depth(depth, path, "verify-tree-depth")?;
    let directory = open_child_directory(&entry.parent, &entry.name, path, "verify")?;
    content_len_directory(&directory, path, roots, depth, check_control)
}

fn content_len_directory(
    directory: &Dir,
    path: &Path,
    roots: &[PathBuf],
    depth: usize,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<u64, StorageError> {
    let mut total = 0u64;
    for item in directory
        .entries()
        .map_err(|error| StorageError::io(path.display().to_string(), error))?
    {
        check_control()?;
        let item = item.map_err(|error| StorageError::io(path.display().to_string(), error))?;
        let name = item.file_name();
        let child_path = path.join(&name);
        let child = Entry {
            parent: crate::open::reopen_windows_directory(directory)
                .map_err(|error| StorageError::io(path.display().to_string(), error))?,
            name,
            path: child_path.clone(),
        };
        let metadata = entry_metadata(&child, "verify")?;
        ensure_supported_entry(&metadata, &child_path, "verify")?;
        let child_total = if metadata.is_dir() {
            ensure_storage_tree_depth(depth.saturating_add(1), &child_path, "verify-tree-depth")?;
            let nested = open_child_directory(&child.parent, &child.name, &child_path, "verify")?;
            content_len_directory(
                &nested,
                &child_path,
                roots,
                depth.saturating_add(1),
                check_control,
            )?
        } else {
            metadata.len()
        };
        total = total
            .checked_add(child_total)
            .ok_or_else(|| plan_error("verify-size", "content byte count overflow"))?;
    }
    Ok(total)
}

fn verify_content_matches(
    source: &Path,
    destination: &Path,
    roots: &[PathBuf],
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    let source_entry = required_entry(source, roots, false, "verify-content-source")?;
    let destination_entry =
        required_entry(destination, roots, false, "verify-content-destination")?;
    verify_entry_contents(&source_entry, &destination_entry, roots, 0, check_control)
}

fn verify_entry_contents(
    source: &Entry,
    destination: &Entry,
    roots: &[PathBuf],
    depth: usize,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    check_control()?;
    let source_metadata = entry_metadata(source, "verify-content-source")?;
    let destination_metadata = entry_metadata(destination, "verify-content-destination")?;
    ensure_supported_entry(&source_metadata, &source.path, "verify-content-source")?;
    ensure_supported_entry(
        &destination_metadata,
        &destination.path,
        "verify-content-destination",
    )?;
    if source_metadata.is_dir() {
        if !destination_metadata.is_dir() {
            return Err(plan_error(
                "verify-content",
                &format!(
                    "expected a directory at {} to match source directory {}",
                    destination.path.display(),
                    source.path.display()
                ),
            ));
        }
        ensure_storage_tree_depth(depth, &source.path, "verify-tree-depth")?;
        let source_dir = open_child_directory(
            &source.parent,
            &source.name,
            &source.path,
            "verify-content-source",
        )?;
        let destination_dir = open_child_directory(
            &destination.parent,
            &destination.name,
            &destination.path,
            "verify-content-destination",
        )?;
        verify_directory_contents(
            &source_dir,
            &destination_dir,
            &source.path,
            &destination.path,
            roots,
            depth,
            check_control,
        )
    } else if source_metadata.is_file() {
        if !destination_metadata.is_file() {
            return Err(plan_error(
                "verify-content",
                &format!(
                    "expected a regular file at {} to match source file {}",
                    destination.path.display(),
                    source.path.display()
                ),
            ));
        }
        let source_hash = hash_file(source, "verify-content-source", check_control)?;
        let destination_hash = hash_file(destination, "verify-content-destination", check_control)?;
        if source_hash == destination_hash {
            Ok(())
        } else {
            Err(plan_error(
                "verify-content",
                &format!(
                    "content hash mismatch after copy: {} != {}",
                    source.path.display(),
                    destination.path.display()
                ),
            ))
        }
    } else {
        Err(plan_error(
            "verify-content",
            &format!("unsupported file type: {}", source.path.display()),
        ))
    }
}

fn verify_directory_contents(
    source: &Dir,
    destination: &Dir,
    source_path: &Path,
    destination_path: &Path,
    roots: &[PathBuf],
    depth: usize,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    let source_names = directory_names(source, source_path)?;
    let destination_names = directory_names(destination, destination_path)?;
    let source_set = source_names.iter().cloned().collect::<HashSet<_>>();
    let destination_set = destination_names.iter().cloned().collect::<HashSet<_>>();
    if source_set != destination_set {
        return Err(plan_error(
            "verify-content",
            &format!(
                "directory entries differ between {} and {}",
                source_path.display(),
                destination_path.display()
            ),
        ));
    }
    for name in source_names {
        check_control()?;
        let source_child = Entry {
            parent: crate::open::reopen_windows_directory(source)
                .map_err(|error| StorageError::io(source_path.display().to_string(), error))?,
            name: name.clone(),
            path: source_path.join(&name),
        };
        let destination_child = Entry {
            parent: crate::open::reopen_windows_directory(destination)
                .map_err(|error| StorageError::io(destination_path.display().to_string(), error))?,
            name: name.clone(),
            path: destination_path.join(&name),
        };
        verify_entry_contents(
            &source_child,
            &destination_child,
            roots,
            depth.saturating_add(1),
            check_control,
        )?;
    }
    Ok(())
}

fn directory_names(directory: &Dir, path: &Path) -> Result<Vec<OsString>, StorageError> {
    directory
        .entries()
        .map_err(|error| StorageError::io(path.display().to_string(), error))?
        .map(|entry| {
            entry
                .map(|entry| entry.file_name())
                .map_err(|error| StorageError::io(path.display().to_string(), error))
        })
        .collect()
}

fn hash_file(
    entry: &Entry,
    step: &'static str,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<[u8; 20], StorageError> {
    use sha1::{Digest, Sha1};
    let mut options = OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let mut file = entry
        .parent
        .open_with(Path::new(&entry.name), &options)
        .map_err(|error| StorageError::io(entry.path.display().to_string(), error))?;
    ensure_open_file_regular(&file, &entry.path, step)?;
    let mut hasher = Sha1::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check_control()?;
        let read = file
            .read(&mut buffer)
            .map_err(|error| StorageError::io(entry.path.display().to_string(), error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

fn safe_delete(
    path: &Path,
    roots: &[PathBuf],
    missing_ok: bool,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    check_control()?;
    let Some(entry) = path_entry(path, roots, false, "delete-source")? else {
        if missing_ok {
            return Ok(());
        }
        return Err(StorageError::StagedMoveFailed {
            step: "delete-source",
            reason: format!(
                "path missing during move/import execution: {}",
                path.display()
            ),
        });
    };
    let metadata = match entry_metadata(&entry, "delete-source") {
        Ok(metadata) => metadata,
        Err(StorageError::FileNotFound { .. }) if missing_ok => return Ok(()),
        Err(StorageError::FileNotFound { .. }) => {
            return Err(StorageError::StagedMoveFailed {
                step: "delete-source",
                reason: format!(
                    "path missing during move/import execution: {}",
                    path.display()
                ),
            })
        }
        Err(error) => return Err(error),
    };
    let root_is_reparse = is_reparse(&metadata);
    let root_is_dir = metadata.is_dir() && !root_is_reparse;
    let mut removed = false;
    let result = if root_is_reparse {
        match remove_reparse_entry(&entry, &metadata) {
            Ok(()) => {
                removed = true;
                Ok(())
            }
            Err(error) => Err(StorageError::io(path.display().to_string(), error)),
        }
    } else if root_is_dir {
        let directory = open_child_directory(&entry.parent, &entry.name, path, "delete-source")?;
        remove_directory_contents(&directory, path, 0, check_control, &mut removed)?;
        drop(directory);
        check_control()?;
        entry
            .parent
            .remove_dir(Path::new(&entry.name))
            .map_err(|error| StorageError::io(path.display().to_string(), error))?;
        removed = true;
        Ok(())
    } else if metadata.is_file() {
        check_control()?;
        entry
            .parent
            .remove_file(Path::new(&entry.name))
            .map_err(|error| StorageError::io(path.display().to_string(), error))?;
        removed = true;
        Ok(())
    } else {
        Err(plan_error(
            "delete-source",
            &format!("unsupported file type: {}", path.display()),
        ))
    };
    match result {
        Err(error) if removed || (root_is_dir && !is_cancellation_error(&error)) => {
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

fn remove_directory_contents(
    directory: &Dir,
    path: &Path,
    depth: usize,
    check_control: &dyn Fn() -> Result<(), StorageError>,
    removed: &mut bool,
) -> Result<(), StorageError> {
    check_control()?;
    ensure_storage_tree_depth(depth, path, "delete-tree-depth")?;
    for item in directory
        .entries()
        .map_err(|error| StorageError::io(path.display().to_string(), error))?
    {
        check_control()?;
        let item = item.map_err(|error| StorageError::io(path.display().to_string(), error))?;
        let name = item.file_name();
        let child_path = path.join(&name);
        let parent = crate::open::reopen_windows_directory(directory)
            .map_err(|error| StorageError::io(path.display().to_string(), error))?;
        let metadata = parent
            .symlink_metadata(Path::new(&name))
            .map_err(|error| StorageError::io(child_path.display().to_string(), error))?;
        if is_reparse(&metadata) {
            let entry = Entry {
                parent,
                name,
                path: child_path,
            };
            remove_reparse_entry(&entry, &metadata)
                .map_err(|error| StorageError::io(entry.path.display().to_string(), error))?;
            *removed = true;
        } else if metadata.is_dir() {
            let child_directory =
                open_child_directory(&parent, &name, &child_path, "delete-source")?;
            remove_directory_contents(
                &child_directory,
                &child_path,
                depth.saturating_add(1),
                check_control,
                removed,
            )?;
            drop(child_directory);
            check_control()?;
            parent
                .remove_dir(Path::new(&name))
                .map_err(|error| StorageError::io(child_path.display().to_string(), error))?;
            *removed = true;
        } else if metadata.is_file() {
            check_control()?;
            parent
                .remove_file(Path::new(&name))
                .map_err(|error| StorageError::io(child_path.display().to_string(), error))?;
            *removed = true;
        } else {
            return Err(plan_error(
                "delete-source",
                &format!("unsupported file type: {}", child_path.display()),
            ));
        }
    }
    Ok(())
}

fn remove_reparse_entry(entry: &Entry, metadata: &Metadata) -> io::Result<()> {
    if metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0 {
        entry.parent.remove_dir(Path::new(&entry.name))
    } else {
        entry.parent.remove_file(Path::new(&entry.name))
    }
}

fn prune_empty_dirs(
    start: &Path,
    root: &Path,
    roots: &[PathBuf],
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    let resolved_start = resolve_confined_path(start)?;
    let resolved_root = resolve_confined_path(root)?;
    if !resolved_start.starts_with(&resolved_root) {
        return Err(plan_error(
            "prune-root",
            &format!(
                "directory {} is outside prune root {}",
                resolved_start.display(),
                resolved_root.display()
            ),
        ));
    }
    let mut current = resolved_start.as_path();
    let mut removed = false;
    while current != resolved_root {
        if let Err(error) = check_control() {
            if removed {
                return Err(StorageError::FilesystemStateUncertain {
                    step: "prune",
                    reason: format!(
                        "{error}; pruning of {} was only partially applied",
                        resolved_root.display()
                    ),
                });
            }
            return Err(error);
        }
        let Some(entry) = path_entry(current, roots, false, "prune-source")? else {
            current = current
                .parent()
                .ok_or_else(|| plan_error("prune-parent", "directory has no parent"))?;
            continue;
        };
        let metadata = match entry_metadata(&entry, "prune-source") {
            Ok(metadata) => metadata,
            Err(StorageError::FileNotFound { .. }) => {
                current = current
                    .parent()
                    .ok_or_else(|| plan_error("prune-parent", "directory has no parent"))?;
                continue;
            }
            Err(error) => return Err(error),
        };
        if is_reparse(&metadata) {
            return Err(unsafe_symlink_error(current, "prune-source"));
        }
        if !metadata.is_dir() {
            return Err(plan_error(
                "prune-source",
                &format!("not a directory: {}", current.display()),
            ));
        }
        match entry.parent.remove_dir(Path::new(&entry.name)) {
            Ok(()) => removed = true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => break,
            Err(error) if removed => {
                return Err(StorageError::FilesystemStateUncertain {
                    step: "prune",
                    reason: format!(
                        "failed to remove {} after pruning partially applied: {error}",
                        current.display()
                    ),
                })
            }
            Err(error) => return Err(StorageError::io(current.display().to_string(), error)),
        }
        current = current
            .parent()
            .ok_or_else(|| plan_error("prune-parent", "directory has no parent"))?;
    }
    Ok(())
}

fn rename_entry_no_replace(source: &Entry, destination: &Entry) -> Result<(), StorageError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .access_mode(DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let file = source
        .parent
        .open_with(Path::new(&source.name), &options)
        .map_err(|error| StorageError::io(source.path.display().to_string(), error))?;
    let metadata = file
        .metadata()
        .map_err(|error| StorageError::io(source.path.display().to_string(), error))?;
    ensure_supported_entry(&metadata, &source.path, "rename-source")?;
    let destination_parent = open_rename_destination_directory(
        &destination.parent,
        &destination.path,
        metadata.is_dir(),
    )?;
    let mut wide = destination.name.encode_wide().collect::<Vec<_>>();
    if wide.is_empty() || wide.iter().any(|value| *value == 0) {
        return Err(plan_error("rename-destination", "invalid Windows filename"));
    }
    let name_bytes = wide
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| plan_error("rename-destination", "filename is too long"))?;
    let offset = offset_of!(FILE_RENAME_INFO, FileName);
    let byte_len = offset
        .checked_add(name_bytes)
        .ok_or_else(|| plan_error("rename-destination", "filename is too long"))?;
    let word_count = byte_len
        .checked_add(size_of::<usize>() - 1)
        .ok_or_else(|| plan_error("rename-destination", "filename is too long"))?
        / size_of::<usize>();
    let mut storage = vec![0usize; word_count.max(1)];
    let info = FILE_RENAME_INFO {
        Anonymous: FILE_RENAME_INFO_0 {
            ReplaceIfExists: false,
        },
        RootDirectory: destination_parent.as_raw_handle() as HANDLE,
        FileNameLength: u32::try_from(name_bytes)
            .map_err(|_| plan_error("rename-destination", "filename is too long"))?,
        FileName: [0],
    };
    // FILE_RENAME_INFO is a C struct with a trailing variable-length UTF-16
    // filename. The aligned word buffer supplies its required alignment.
    unsafe {
        let buffer = storage.as_mut_ptr().cast::<u8>();
        ptr::write(buffer.cast::<FILE_RENAME_INFO>(), info);
        ptr::copy_nonoverlapping(wide.as_ptr(), buffer.add(offset).cast::<u16>(), wide.len());
        let result = SetFileInformationByHandle(
            file.as_raw_handle() as HANDLE,
            FileRenameInfo as FILE_INFO_BY_HANDLE_CLASS,
            buffer.cast(),
            u32::try_from(byte_len)
                .map_err(|_| plan_error("rename-destination", "filename is too long"))?,
        );
        wide.clear();
        if result == 0 {
            return Err(StorageError::io(
                destination.path.display().to_string(),
                io::Error::last_os_error(),
            ));
        }
    }
    Ok(())
}

fn open_rename_destination_directory(
    parent: &Dir,
    path: &Path,
    source_is_directory: bool,
) -> Result<Dir, StorageError> {
    // FILE_RENAME_INFO resolves FileName relative to RootDirectory. Windows
    // requires FILE_ADD_FILE or FILE_ADD_SUBDIRECTORY on that directory handle
    // according to the source type; the ordinary traversal handles are
    // intentionally opened with read-only access.
    let add_right = if source_is_directory {
        FILE_ADD_SUBDIRECTORY
    } else {
        FILE_ADD_FILE
    };
    let mut options = OpenOptions::new();
    options
        .access_mode(add_right | FILE_READ_ATTRIBUTES | SYNCHRONIZE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let file = parent
        .open_with(Path::new("."), &options)
        .map_err(|error| StorageError::io(path.display().to_string(), error))?;
    let metadata = file
        .metadata()
        .map_err(|error| StorageError::io(path.display().to_string(), error))?;
    if is_reparse(&metadata) {
        return Err(unsafe_symlink_error(path, "rename-destination"));
    }
    if !metadata.is_dir() {
        return Err(StorageError::StagedMoveFailed {
            step: "rename-destination",
            reason: format!("destination parent is not a directory: {}", path.display()),
        });
    }
    Ok(Dir::from_std_file(file.into_std()))
}

fn reconcile_content_matches(
    source: &Path,
    destination: &Path,
    roots: &[PathBuf],
) -> Result<(), StorageError> {
    verify_content_matches(source, destination, roots, &|| Ok(())).map_err(|error| match error {
        StorageError::FilesystemStateUncertain { .. } => error,
        error => StorageError::FilesystemStateUncertain {
            step: "reconcile",
            reason: error.to_string(),
        },
    })
}

fn verify_reconciled_length(
    path: &Path,
    roots: &[PathBuf],
    expected: u64,
) -> Result<(), StorageError> {
    if expected == u64::MAX {
        return Ok(());
    }
    let actual = content_len(path, roots, 0, &|| Ok(())).map_err(|error| {
        StorageError::FilesystemStateUncertain {
            step: "reconcile",
            reason: error.to_string(),
        }
    })?;
    if actual == expected {
        Ok(())
    } else {
        Err(StorageError::FilesystemStateUncertain {
            step: "reconcile",
            reason: format!(
                "checkpointed destination has {actual} bytes, expected {expected}: {}",
                path.display()
            ),
        })
    }
}

fn plan_error(step: &'static str, reason: &str) -> StorageError {
    StorageError::StagedMoveFailed {
        step,
        reason: reason.to_owned(),
    }
}

fn safe_path_short_io(path: &Path, expected: u64, actual: u64) -> StorageError {
    StorageError::ShortIo {
        path: path.display().to_string(),
        expected: usize::try_from(expected).unwrap_or(usize::MAX),
        actual: usize::try_from(actual).unwrap_or(usize::MAX),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured_root(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).expect("storage root should canonicalize")
    }

    fn step(
        action: PlannedStorageAction,
        source: &Path,
        destination: Option<&Path>,
        bytes: u64,
    ) -> StoragePlanStep {
        StoragePlanStep {
            action,
            source: Some(source.to_path_buf()),
            destination: destination.map(Path::to_path_buf),
            bytes,
        }
    }

    #[test]
    fn rooted_copy_and_rename_preserve_nested_file_content() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let source = root.join("source");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        std::fs::write(source.join("one.bin"), b"first").unwrap();
        std::fs::write(source.join("nested/two.bin"), b"second").unwrap();
        let staging = root.join("staging");
        let destination = root.join("destination");
        let roots = vec![configured_root(&root)];

        execute_step_with_control(
            &step(
                PlannedStorageAction::CopyVerifyRename,
                &source,
                Some(&staging),
                11,
            ),
            &roots,
            &|| Ok(()),
        )
        .unwrap();
        execute_step_with_control(
            &step(
                PlannedStorageAction::Rename,
                &staging,
                Some(&destination),
                11,
            ),
            &roots,
            &|| Ok(()),
        )
        .unwrap();

        assert_eq!(
            std::fs::read(destination.join("one.bin")).unwrap(),
            b"first"
        );
        assert_eq!(
            std::fs::read(destination.join("nested/two.bin")).unwrap(),
            b"second"
        );
        assert!(source.exists());
        assert!(!staging.exists());
    }

    #[test]
    fn rooted_rename_never_replaces_an_existing_destination() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let source = root.join("source.bin");
        let destination = root.join("destination.bin");
        std::fs::write(&source, b"source").unwrap();
        std::fs::write(&destination, b"keep").unwrap();
        let roots = vec![configured_root(&root)];

        assert!(execute_step_with_control(
            &step(PlannedStorageAction::Rename, &source, Some(&destination), 6,),
            &roots,
            &|| Ok(()),
        )
        .is_err());

        assert_eq!(std::fs::read(&source).unwrap(), b"source");
        assert_eq!(std::fs::read(&destination).unwrap(), b"keep");
    }

    #[test]
    fn rooted_delete_removes_a_directory_reparse_point_without_following_it() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let outside_file = outside.join("keep.bin");
        std::fs::write(&outside_file, b"preserve").unwrap();
        let link = root.join("link");
        if let Err(error) = std::os::windows::fs::symlink_dir(&outside, &link) {
            if error.kind() == io::ErrorKind::PermissionDenied {
                return;
            }
            panic!("failed to create directory symlink for regression test: {error}");
        }
        let roots = vec![configured_root(&root)];

        execute_step_with_control(
            &step(PlannedStorageAction::SafeDelete, &link, None, 0),
            &roots,
            &|| Ok(()),
        )
        .unwrap();

        assert!(outside_file.exists());
        assert!(std::fs::symlink_metadata(link).is_err());
    }

    #[test]
    fn rooted_executor_rejects_a_reparse_point_ancestor() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let source = root.join("source.bin");
        std::fs::write(&source, b"source").unwrap();
        let link = root.join("alias");
        if let Err(error) = std::os::windows::fs::symlink_dir(&outside, &link) {
            if error.kind() == io::ErrorKind::PermissionDenied {
                return;
            }
            panic!("failed to create directory symlink for regression test: {error}");
        }
        let roots = vec![configured_root(&root)];
        let destination = link.join("escaped.bin");

        assert!(execute_step_with_control(
            &step(
                PlannedStorageAction::CopyVerifyRename,
                &source,
                Some(&destination),
                6,
            ),
            &roots,
            &|| Ok(()),
        )
        .is_err());

        assert!(!outside.join("escaped.bin").exists());
        assert_eq!(std::fs::read(source).unwrap(), b"source");
    }
}
