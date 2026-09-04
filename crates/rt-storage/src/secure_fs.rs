//! Descriptor-anchored filesystem operations for root-confined plans.
//!
//! Path validation is not enough for a storage sandbox: an ancestor can be
//! replaced between canonicalization and the eventual open/rename/unlink.
//! These helpers walk every ancestor from an already-opened root directory
//! with `O_NOFOLLOW`, then perform the operation relative to the resulting
//! directory descriptors. The descriptors remain anchored if an attacker
//! renames or replaces a path component after the walk.

use std::ffi::{CStr, CString, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

use sha1::{Digest, Sha1};

use crate::{PlannedStorageAction, StorageError, StoragePlan, StoragePlanStep};

pub(crate) fn execute_step(step: &StoragePlanStep, roots: &[PathBuf]) -> Result<(), StorageError> {
    let no_control = || Ok(());
    execute_step_with_control(step, roots, &no_control)
}

pub(crate) fn execute_step_with_control(
    step: &StoragePlanStep,
    roots: &[PathBuf],
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    check_control()?;
    match step.action {
        PlannedStorageAction::ImportExisting => {
            let source = required_path(step.source.as_ref(), "import-source")?;
            let destination = required_path(step.destination.as_ref(), "import-destination")?;
            secure_import(source, destination, step.bytes, roots, check_control)
        }
        PlannedStorageAction::Rename => {
            let source = required_path(step.source.as_ref(), "rename-source")?;
            let destination = required_path(step.destination.as_ref(), "rename-destination")?;
            secure_rename(source, destination, step.bytes, roots, check_control)
        }
        PlannedStorageAction::CopyVerifyRename => {
            let source = required_path(step.source.as_ref(), "copy-source")?;
            let destination = required_path(step.destination.as_ref(), "copy-destination")?;
            secure_copy(source, destination, step.bytes, roots, check_control)
        }
        PlannedStorageAction::SafeDelete => {
            let source = required_path(step.source.as_ref(), "delete-source")?;
            secure_delete(source, roots, false, check_control)
        }
        PlannedStorageAction::SafeDeleteIfPresent => {
            let source = required_path(step.source.as_ref(), "delete-source")?;
            secure_delete(source, roots, true, check_control)
        }
        PlannedStorageAction::PruneEmptyDirs => {
            let source = required_path(step.source.as_ref(), "prune-source")?;
            let root = required_path(step.destination.as_ref(), "prune-root")?;
            secure_prune(source, root, roots, check_control)
        }
    }
}

pub(crate) fn step_is_applied(
    step: &StoragePlanStep,
    roots: &[PathBuf],
) -> Result<bool, StorageError> {
    match step.action {
        PlannedStorageAction::Rename => {
            let source = required_path(step.source.as_ref(), "reconcile-source")?;
            let destination = required_path(step.destination.as_ref(), "reconcile-destination")?;
            let source_state = entry_state(source, roots)?;
            let destination_state = entry_state(destination, roots)?;
            if source_state.is_some() && destination_state.is_some() {
                return Err(staged(
                    "reconcile",
                    format!(
                        "rename has both source and destination present: {} -> {}",
                        source.display(),
                        destination.display()
                    ),
                ));
            }
            Ok(source_state.is_none() && destination_state.is_some())
        }
        PlannedStorageAction::CopyVerifyRename | PlannedStorageAction::ImportExisting => {
            let source = required_path(step.source.as_ref(), "reconcile-source")?;
            let destination = required_path(step.destination.as_ref(), "reconcile-destination")?;
            if entry_state(destination, roots)?.is_none() {
                return Ok(false);
            }
            // If the source is still available, compare it while both paths
            // are descriptor-anchored. If the source was removed after the
            // final rename, the destination remains valid evidence for the
            // following plan step.
            if entry_state(source, roots)?.is_some() {
                verify_content(source, destination, roots)?;
            }
            Ok(true)
        }
        PlannedStorageAction::SafeDelete
        | PlannedStorageAction::SafeDeleteIfPresent
        | PlannedStorageAction::PruneEmptyDirs => Ok(entry_state(
            required_path(step.source.as_ref(), "reconcile-source")?,
            roots,
        )?
        .is_none()),
    }
}

pub(crate) fn rollback_plan(
    plan: &StoragePlan,
    roots: &[PathBuf],
) -> (Vec<StoragePlanStep>, Vec<(StoragePlanStep, String)>) {
    let mut rolled_back = Vec::new();
    let mut failures = Vec::new();
    for step in &plan.rollback_steps {
        match execute_step(step, roots) {
            Ok(()) => rolled_back.push(step.clone()),
            Err(error) => failures.push((step.clone(), error.to_string())),
        }
    }
    (rolled_back, failures)
}

fn secure_import(
    source: &Path,
    destination: &Path,
    expected_bytes: u64,
    roots: &[PathBuf],
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    check_control()?;
    let (source_parent, source_name) = open_parent(source, roots, false, "import-source")?;
    let (destination_parent, destination_name) =
        open_parent(destination, roots, true, "import-destination")?;
    ensure_destination_available(&destination_parent, &destination_name, destination)?;
    let source_type = entry_type(&source_parent, &source_name, source)?;
    if source_type.is_symlink() {
        return Err(unsafe_symlink(source, "import-source"));
    }

    // Hard links are the cheap and atomic import path for regular files. A
    // directory cannot be linked, so it falls through to the same secure
    // recursive copy implementation used by cross-filesystem imports.
    let link_result = if source_type.is_file() {
        let source_c = c_name(&source_name, source)?;
        let destination_c = c_name(&destination_name, destination)?;
        unsafe {
            libc::linkat(
                source_parent.as_raw_fd(),
                source_c.as_ptr(),
                destination_parent.as_raw_fd(),
                destination_c.as_ptr(),
                0,
            )
        }
    } else {
        -1
    };
    if link_result == 0 {
        if entry_type(&destination_parent, &destination_name, destination)?.is_symlink() {
            let _ = unlink_at(&destination_parent, &destination_name, destination, 0);
            return Err(unsafe_symlink(source, "import-source"));
        }
        verify_expected_length(
            &destination_parent,
            &destination_name,
            expected_bytes,
            destination,
            check_control,
        )
    } else {
        secure_copy_entry(
            &source_parent,
            &source_name,
            &destination_parent,
            &destination_name,
            (source, destination),
            expected_bytes,
            check_control,
        )
    }
}

fn secure_copy(
    source: &Path,
    destination: &Path,
    expected_bytes: u64,
    roots: &[PathBuf],
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    check_control()?;
    let (source_parent, source_name) = open_parent(source, roots, false, "copy-source")?;
    let (destination_parent, destination_name) =
        open_parent(destination, roots, true, "copy-destination")?;
    ensure_destination_available(&destination_parent, &destination_name, destination)?;
    secure_copy_entry(
        &source_parent,
        &source_name,
        &destination_parent,
        &destination_name,
        (source, destination),
        expected_bytes,
        check_control,
    )
}

fn secure_copy_entry(
    source_parent: &File,
    source_name: &OsString,
    destination_parent: &File,
    destination_name: &OsString,
    paths: (&Path, &Path),
    expected_bytes: u64,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    let (source_path, destination_path) = paths;
    check_control()?;
    let source_type = entry_type(source_parent, source_name, source_path)?;
    if source_type.is_symlink() {
        return Err(unsafe_symlink(source_path, "copy-source"));
    }
    ensure_destination_available(destination_parent, destination_name, destination_path)?;

    if source_type.is_dir() {
        mkdir_at(destination_parent, destination_name, destination_path)?;
        let source_dir = open_dir_at(source_parent, source_name, source_path)
            .map_err(|error| StorageError::io(source_path.display().to_string(), error))?;
        let destination_dir =
            open_dir_at(destination_parent, destination_name, destination_path)
                .map_err(|error| StorageError::io(destination_path.display().to_string(), error))?;
        copy_directory_contents(
            &source_dir,
            &destination_dir,
            source_path,
            destination_path,
            check_control,
        )?;
    } else if source_type.is_file() {
        copy_file_at(
            source_parent,
            source_name,
            destination_parent,
            destination_name,
            source_path,
            destination_path,
            check_control,
        )?;
    } else {
        return Err(staged(
            "copy-source",
            format!("unsupported file type: {}", source_path.display()),
        ));
    }

    verify_expected_length(
        destination_parent,
        destination_name,
        expected_bytes,
        destination_path,
        check_control,
    )?;
    verify_content_at(
        source_parent,
        source_name,
        destination_parent,
        destination_name,
        source_path,
        destination_path,
        check_control,
    )
}

fn copy_directory_contents(
    source_dir: &File,
    destination_dir: &File,
    source_path: &Path,
    destination_path: &Path,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    for name in directory_names(source_dir, source_path)? {
        check_control()?;
        let child_source = source_path.join(&name);
        let child_destination = destination_path.join(&name);
        secure_copy_entry(
            source_dir,
            &name,
            destination_dir,
            &name,
            (&child_source, &child_destination),
            u64::MAX,
            check_control,
        )?;
    }
    Ok(())
}

fn copy_file_at(
    source_parent: &File,
    source_name: &OsString,
    destination_parent: &File,
    destination_name: &OsString,
    source_path: &Path,
    destination_path: &Path,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    check_control()?;
    let mut source = open_file_at(source_parent, source_name, source_path, libc::O_RDONLY)?;
    let mut destination = create_file_at(destination_parent, destination_name, destination_path)?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check_control()?;
        let read = source
            .read(&mut buffer)
            .map_err(|error| StorageError::io(source_path.display().to_string(), error))?;
        if read == 0 {
            break;
        }
        destination
            .write_all(&buffer[..read])
            .map_err(|error| StorageError::io(destination_path.display().to_string(), error))?;
    }
    destination
        .sync_all()
        .map_err(|error| StorageError::io(destination_path.display().to_string(), error))
}

fn secure_rename(
    source: &Path,
    destination: &Path,
    expected_bytes: u64,
    roots: &[PathBuf],
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    check_control()?;
    let (source_parent, source_name) = open_parent(source, roots, false, "rename-source")?;
    let (destination_parent, destination_name) =
        open_parent(destination, roots, true, "rename-destination")?;
    ensure_destination_available(&destination_parent, &destination_name, destination)?;
    if entry_type(&source_parent, &source_name, source)?.is_symlink() {
        return Err(unsafe_symlink(source, "rename-source"));
    }
    check_control()?;
    let source_c = c_name(&source_name, source)?;
    let destination_c = c_name(&destination_name, destination)?;
    let result = unsafe {
        libc::renameat(
            source_parent.as_raw_fd(),
            source_c.as_ptr(),
            destination_parent.as_raw_fd(),
            destination_c.as_ptr(),
        )
    };
    if result != 0 {
        return Err(StorageError::io(
            destination.display().to_string(),
            io::Error::last_os_error(),
        ));
    }
    verify_expected_length(
        &destination_parent,
        &destination_name,
        expected_bytes,
        destination,
        check_control,
    )
}

fn secure_delete(
    path: &Path,
    roots: &[PathBuf],
    missing_ok: bool,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    check_control()?;
    let (parent, name) = match open_parent(path, roots, false, "delete-source") {
        Ok(value) => value,
        Err(error) if missing_ok && is_not_found(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    remove_entry_at(&parent, &name, path, missing_ok, check_control)
}

fn remove_entry_at(
    parent: &File,
    name: &OsString,
    path: &Path,
    missing_ok: bool,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    check_control()?;
    let kind = match entry_type(parent, name, path) {
        Ok(kind) => kind,
        Err(error) if missing_ok && is_not_found(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    if kind.is_dir() {
        let child_dir = open_dir_at(parent, name, path)
            .map_err(|error| StorageError::io(path.display().to_string(), error))?;
        for child in directory_names(&child_dir, path)? {
            remove_entry_at(
                &child_dir,
                &child,
                &path.join(&child),
                missing_ok,
                check_control,
            )?;
        }
        check_control()?;
        unlink_at(parent, name, path, libc::AT_REMOVEDIR)
    } else if kind.is_file() || kind.is_symlink() {
        check_control()?;
        unlink_at(parent, name, path, 0)
    } else {
        Err(staged(
            "delete-source",
            format!("unsupported file type: {}", path.display()),
        ))
    }
}

fn secure_prune(
    source: &Path,
    boundary: &Path,
    roots: &[PathBuf],
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    check_control()?;
    let (root_path, source_parts) = relative_parts(source, roots, "prune-source")?;
    let (boundary_root, boundary_parts) = relative_parts_allow_root(boundary, roots, "prune-root")?;
    if root_path != boundary_root
        || boundary_parts.len() > source_parts.len()
        || boundary_parts != source_parts[..boundary_parts.len()]
    {
        return Err(staged(
            "prune-root",
            format!(
                "directory {} is outside prune root {}",
                source.display(),
                boundary.display()
            ),
        ));
    }
    if source_parts.len() == boundary_parts.len() {
        return Ok(());
    }

    let root_dir = open_root(&root_path)
        .map_err(|error| StorageError::io(root_path.display().to_string(), error))?;
    let mut directories = vec![root_dir];
    for part in &source_parts {
        let parent = directories.last().expect("root directory exists");
        let directory = match open_dir_at(parent, part, source) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(StorageError::io(source.display().to_string(), error)),
        };
        directories.push(directory);
    }

    for index in (boundary_parts.len()..source_parts.len()).rev() {
        check_control()?;
        match unlink_at(
            &directories[index],
            &source_parts[index],
            source,
            libc::AT_REMOVEDIR,
        ) {
            Ok(()) => {}
            Err(error) if is_not_found(&error) => {}
            Err(StorageError::Io { source: error, .. })
                if error.kind() == io::ErrorKind::DirectoryNotEmpty =>
            {
                break
            }
            Err(StorageError::Io { source: error, .. })
                if error.raw_os_error() == Some(libc::ENOTEMPTY) =>
            {
                break
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn verify_content(
    source: &Path,
    destination: &Path,
    roots: &[PathBuf],
) -> Result<(), StorageError> {
    let no_control = || Ok(());
    verify_content_with_control(source, destination, roots, &no_control)
}

fn verify_content_with_control(
    source: &Path,
    destination: &Path,
    roots: &[PathBuf],
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    check_control()?;
    let (source_parent, source_name) = open_parent(source, roots, false, "verify-content-source")?;
    let (destination_parent, destination_name) =
        open_parent(destination, roots, false, "verify-content-destination")?;
    verify_content_at(
        &source_parent,
        &source_name,
        &destination_parent,
        &destination_name,
        source,
        destination,
        check_control,
    )
}

fn verify_content_at(
    source_parent: &File,
    source_name: &OsString,
    destination_parent: &File,
    destination_name: &OsString,
    source_path: &Path,
    destination_path: &Path,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    check_control()?;
    let source_type = entry_type(source_parent, source_name, source_path)?;
    let destination_type = entry_type(destination_parent, destination_name, destination_path)?;
    if source_type.is_symlink() {
        return Err(unsafe_symlink(source_path, "verify-content-source"));
    }
    if destination_type.is_symlink() {
        return Err(unsafe_symlink(
            destination_path,
            "verify-content-destination",
        ));
    }
    if source_type.is_dir() {
        if !destination_type.is_dir() {
            return Err(staged(
                "verify-content",
                format!(
                    "expected a directory at {} to match source directory {}",
                    destination_path.display(),
                    source_path.display()
                ),
            ));
        }
        let source_dir = open_dir_at(source_parent, source_name, source_path)
            .map_err(|error| StorageError::io(source_path.display().to_string(), error))?;
        let destination_dir =
            open_dir_at(destination_parent, destination_name, destination_path)
                .map_err(|error| StorageError::io(destination_path.display().to_string(), error))?;
        let source_names = directory_names(&source_dir, source_path)?;
        let destination_names = directory_names(&destination_dir, destination_path)?;
        for name in &source_names {
            check_control()?;
            let child_source = source_path.join(name);
            let child_destination = destination_path.join(name);
            verify_content_at(
                &source_dir,
                name,
                &destination_dir,
                name,
                &child_source,
                &child_destination,
                check_control,
            )?;
        }
        if destination_names
            .iter()
            .any(|name| !source_names.iter().any(|source_name| source_name == name))
        {
            return Err(staged(
                "verify-content",
                format!(
                    "destination directory has entries absent from source: {}",
                    destination_path.display()
                ),
            ));
        }
        Ok(())
    } else if source_type.is_file() {
        if !destination_type.is_file() {
            return Err(staged(
                "verify-content",
                format!(
                    "expected a regular file at {} to match source file {}",
                    destination_path.display(),
                    source_path.display()
                ),
            ));
        }
        let source_hash = hash_file_at(source_parent, source_name, source_path, check_control)?;
        let destination_hash = hash_file_at(
            destination_parent,
            destination_name,
            destination_path,
            check_control,
        )?;
        if source_hash == destination_hash {
            Ok(())
        } else {
            Err(staged(
                "verify-content",
                format!(
                    "content hash mismatch after copy: {} != {}",
                    source_path.display(),
                    destination_path.display()
                ),
            ))
        }
    } else {
        Err(staged(
            "verify-content",
            format!("unsupported file type: {}", source_path.display()),
        ))
    }
}

fn hash_file_at(
    parent: &File,
    name: &OsString,
    path: &Path,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<[u8; 20], StorageError> {
    check_control()?;
    let mut file = open_file_at(parent, name, path, libc::O_RDONLY)?;
    let mut hasher = Sha1::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check_control()?;
        let read = file
            .read(&mut buffer)
            .map_err(|error| StorageError::io(path.display().to_string(), error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

fn verify_expected_length(
    parent: &File,
    name: &OsString,
    expected_bytes: u64,
    path: &Path,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    if expected_bytes == u64::MAX {
        return Ok(());
    }
    check_control()?;
    let actual = content_len_at(parent, name, path, check_control)?;
    if actual == expected_bytes {
        Ok(())
    } else {
        Err(StorageError::ShortIo {
            path: path.display().to_string(),
            expected: expected_bytes.min(usize::MAX as u64) as usize,
            actual: actual.min(usize::MAX as u64) as usize,
        })
    }
}

fn content_len_at(
    parent: &File,
    name: &OsString,
    path: &Path,
    check_control: &dyn Fn() -> Result<(), StorageError>,
) -> Result<u64, StorageError> {
    check_control()?;
    let kind = entry_type(parent, name, path)?;
    if kind.is_file() {
        let file = open_file_at(parent, name, path, libc::O_RDONLY)?;
        return file
            .metadata()
            .map(|metadata| metadata.len())
            .map_err(|error| StorageError::io(path.display().to_string(), error));
    }
    if kind.is_dir() {
        let directory = open_dir_at(parent, name, path)
            .map_err(|error| StorageError::io(path.display().to_string(), error))?;
        let mut total = 0u64;
        for child in directory_names(&directory, path)? {
            check_control()?;
            total = total.saturating_add(content_len_at(
                &directory,
                &child,
                &path.join(&child),
                check_control,
            )?);
        }
        return Ok(total);
    }
    Err(staged(
        "verify",
        format!("unsupported file type: {}", path.display()),
    ))
}

fn relative_parts(
    path: &Path,
    roots: &[PathBuf],
    step: &'static str,
) -> Result<(PathBuf, Vec<OsString>), StorageError> {
    relative_parts_inner(path, roots, step, false)
}

fn relative_parts_allow_root(
    path: &Path,
    roots: &[PathBuf],
    step: &'static str,
) -> Result<(PathBuf, Vec<OsString>), StorageError> {
    relative_parts_inner(path, roots, step, true)
}

fn relative_parts_inner(
    path: &Path,
    roots: &[PathBuf],
    step: &'static str,
    allow_root: bool,
) -> Result<(PathBuf, Vec<OsString>), StorageError> {
    let root = roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
        .ok_or_else(|| {
            staged(
                step,
                format!("path outside configured roots: {}", path.display()),
            )
        })?;
    let relative = path.strip_prefix(root).map_err(|_| {
        staged(
            step,
            format!("path outside configured roots: {}", path.display()),
        )
    })?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => parts.push(value.to_os_string()),
            _ => {
                return Err(staged(
                    step,
                    format!("unsafe path component: {}", path.display()),
                ));
            }
        }
    }
    if parts.is_empty() && !allow_root {
        return Err(staged(
            step,
            format!("refusing to operate on storage root: {}", path.display()),
        ));
    }
    Ok((root.clone(), parts))
}

fn open_parent(
    path: &Path,
    roots: &[PathBuf],
    create: bool,
    step: &'static str,
) -> Result<(File, OsString), StorageError> {
    let (root_path, parts) = relative_parts(path, roots, step)?;
    let name = parts.last().expect("relative path is non-empty").clone();
    let root = open_root(&root_path)
        .map_err(|error| StorageError::io(root_path.display().to_string(), error))?;
    let mut parent = root;
    for part in &parts[..parts.len() - 1] {
        parent = open_or_create_dir(&parent, part, create, path)?;
    }
    Ok((parent, name))
}

fn open_root(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "storage root must be absolute",
        ));
    }
    let name = CString::new("/").expect("literal has no NUL");
    let fd = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut root = unsafe { File::from_raw_fd(fd) };
    for component in path.components() {
        let Component::Normal(value) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "storage root contains an unsafe component",
            ));
        };
        let name = value.to_os_string();
        root = open_dir_at(&root, &name, path)?;
    }
    Ok(root)
}

fn open_or_create_dir(
    parent: &File,
    name: &OsString,
    create: bool,
    path: &Path,
) -> Result<File, StorageError> {
    match open_dir_at(parent, name, path) {
        Ok(directory) => Ok(directory),
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
            let c_name = c_name(name, path)?;
            let result = unsafe { libc::mkdirat(parent.as_raw_fd(), c_name.as_ptr(), 0o755) };
            if result != 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(StorageError::io(path.display().to_string(), error));
                }
            }
            open_dir_at(parent, name, path)
                .map_err(|error| StorageError::io(path.display().to_string(), error))
        }
        Err(error) => Err(StorageError::io(path.display().to_string(), error)),
    }
}

fn open_dir_at(parent: &File, name: &OsString, _path: &Path) -> io::Result<File> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn open_file_at(
    parent: &File,
    name: &OsString,
    path: &Path,
    flags: i32,
) -> Result<File, StorageError> {
    let name = c_name(name, path)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        Err(StorageError::io(
            path.display().to_string(),
            io::Error::last_os_error(),
        ))
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn create_file_at(parent: &File, name: &OsString, path: &Path) -> Result<File, StorageError> {
    let name = c_name(name, path)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o644,
        )
    };
    if fd < 0 {
        Err(StorageError::io(
            path.display().to_string(),
            io::Error::last_os_error(),
        ))
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn mkdir_at(parent: &File, name: &OsString, path: &Path) -> Result<(), StorageError> {
    let name = c_name(name, path)?;
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o755) };
    if result == 0 {
        Ok(())
    } else {
        Err(StorageError::io(
            path.display().to_string(),
            io::Error::last_os_error(),
        ))
    }
}

fn unlink_at(parent: &File, name: &OsString, path: &Path, flags: i32) -> Result<(), StorageError> {
    let name = c_name(name, path)?;
    let result = unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if result == 0 {
        Ok(())
    } else {
        Err(StorageError::io(
            path.display().to_string(),
            io::Error::last_os_error(),
        ))
    }
}

fn entry_state(path: &Path, roots: &[PathBuf]) -> Result<Option<EntryType>, StorageError> {
    let (parent, name) = match open_parent(path, roots, false, "reconcile") {
        Ok(value) => value,
        Err(error) if is_not_found(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    match entry_type(&parent, &name, path) {
        Ok(kind) => Ok(Some(kind)),
        Err(error) if is_not_found(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn entry_type(parent: &File, name: &OsString, path: &Path) -> Result<EntryType, StorageError> {
    let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
    let name = c_name(name, path)?;
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        return Err(StorageError::io(
            path.display().to_string(),
            io::Error::last_os_error(),
        ));
    }
    Ok(EntryType::from_mode(stat.st_mode))
}

#[derive(Debug, Clone, Copy)]
struct EntryType(u32);

impl EntryType {
    fn from_mode(mode: libc::mode_t) -> Self {
        Self(mode & libc::S_IFMT)
    }

    fn is_dir(self) -> bool {
        self.0 == libc::S_IFDIR
    }

    fn is_file(self) -> bool {
        self.0 == libc::S_IFREG
    }

    fn is_symlink(self) -> bool {
        self.0 == libc::S_IFLNK
    }
}

fn directory_names(directory: &File, path: &Path) -> Result<Vec<OsString>, StorageError> {
    let duplicate = unsafe { libc::dup(directory.as_raw_fd()) };
    if duplicate < 0 {
        return Err(StorageError::io(
            path.display().to_string(),
            io::Error::last_os_error(),
        ));
    }
    let directory_stream = unsafe { libc::fdopendir(duplicate) };
    if directory_stream.is_null() {
        unsafe { libc::close(duplicate) };
        return Err(StorageError::io(
            path.display().to_string(),
            io::Error::last_os_error(),
        ));
    }
    let mut names = Vec::new();
    loop {
        let entry = unsafe { libc::readdir(directory_stream) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        names.push(OsString::from_vec(name.to_vec()));
    }
    let close_result = unsafe { libc::closedir(directory_stream) };
    if close_result != 0 {
        return Err(StorageError::io(
            path.display().to_string(),
            io::Error::last_os_error(),
        ));
    }
    Ok(names)
}

fn ensure_destination_available(
    parent: &File,
    name: &OsString,
    path: &Path,
) -> Result<(), StorageError> {
    match entry_type(parent, name, path) {
        Ok(_) => Err(staged(
            "destination",
            format!("destination exists: {}", path.display()),
        )),
        Err(error) if is_not_found(&error) => Ok(()),
        Err(error) => Err(error),
    }
}

fn required_path<'a>(
    path: Option<&'a PathBuf>,
    step: &'static str,
) -> Result<&'a Path, StorageError> {
    path.map(PathBuf::as_path)
        .ok_or_else(|| staged(step, "missing path"))
}

fn c_name(name: &OsString, path: &Path) -> Result<CString, StorageError> {
    CString::new(name.as_bytes())
        .map_err(|_| staged("path", format!("path contains NUL: {}", path.display())))
}

fn staged(step: &'static str, reason: impl Into<String>) -> StorageError {
    StorageError::StagedMoveFailed {
        step,
        reason: reason.into(),
    }
}

fn unsafe_symlink(path: &Path, step: &'static str) -> StorageError {
    staged(
        step,
        format!(
            "symlink entries are not allowed in move/import plans: {}",
            path.display()
        ),
    )
}

fn is_not_found(error: &StorageError) -> bool {
    match error {
        StorageError::FileNotFound { .. } => true,
        StorageError::Io { source, .. } => source.kind() == io::ErrorKind::NotFound,
        _ => false,
    }
}
