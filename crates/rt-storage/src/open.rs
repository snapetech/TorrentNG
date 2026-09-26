//! Safe file opening for runtime storage paths.
//!
//! Every runtime path is normalized to an absolute path before it reaches
//! this module. On Unix, each ancestor is opened from the previously opened
//! directory descriptor with `O_NOFOLLOW`, so an ancestor replacement cannot
//! redirect a peer read, recheck, or write outside the path that was
//! authorized by the server.

use std::fs::File;
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::io;
use std::io::{Read, Write};
use std::path::Path;

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(all(windows, test))]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(any(unix, windows))]
use std::path::{Component, PathBuf};
#[cfg(windows)]
use windows_sys::Win32::Foundation::HANDLE;
#[cfg(all(windows, test))]
use windows_sys::Win32::Storage::FileSystem::MoveFileW;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

pub(crate) fn open_path_no_follow(path: &Path, write: bool, create: bool) -> io::Result<File> {
    #[cfg(unix)]
    {
        open_path_no_follow_unix(path, write, create)
    }
    #[cfg(windows)]
    {
        open_path_no_follow_windows(path, write, create)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(write)
            .create(write && create)
            .truncate(false);
        let file = options.open(path)?;
        ensure_regular_file(file)
    }
}

/// Create a new runtime file without following the final component or an
/// ancestor reparse point/symlink. Existing destinations are never opened or
/// truncated.
#[cfg(any(all(not(unix), not(windows)), test))]
pub(crate) fn create_new_file_no_follow(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        let (parent, name) = open_parent_no_follow_unix(path)?;
        let name = CString::new(name.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "runtime path contains NUL")
        })?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY
                    | libc::O_CREAT
                    | libc::O_EXCL
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK,
                0o644,
            )
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            ensure_regular_file(unsafe { File::from_raw_fd(fd) })
        }
    }
    #[cfg(windows)]
    {
        let _parents = open_windows_parent_dirs(path)?;
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        ensure_regular_file(options.open(path)?)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        ensure_regular_file(options.open(path)?)
    }
}

fn ensure_regular_file(file: File) -> io::Result<File> {
    let metadata = file.metadata()?;
    #[cfg(windows)]
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime storage path is a reparse point",
        ));
    }
    if metadata.is_file() {
        Ok(file)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime storage path is not a regular file",
        ))
    }
}

/// Check whether an open descriptor still refers to the object currently
/// named by `path`.
///
/// A path-backed cache can retain a descriptor after a storage move, cleanup,
/// or external unlink/recreate. The descriptor remains usable in that case,
/// but it no longer represents the path the caller authorized. Final
/// symlinks are inspected without following them so replacing a regular file
/// with a symlink cannot make a cached entry look valid.
pub(crate) fn file_matches_path(file: &File, path: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let file_metadata = file.metadata()?;
        let path_metadata = std::fs::symlink_metadata(path)?;
        Ok(
            file_metadata.dev() == path_metadata.dev()
                && file_metadata.ino() == path_metadata.ino(),
        )
    }

    #[cfg(windows)]
    {
        let current = open_path_no_follow_windows(path, false, false)?;
        return Ok(windows_file_identity(file)? == windows_file_identity(&current)?);
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (file, path);
        Ok(false)
    }
}

#[cfg(windows)]
pub(crate) fn windows_file_identity(file: &File) -> io::Result<(u32, u64)> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    let result =
        unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut information) };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    let file_index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    Ok((information.dwVolumeSerialNumber, file_index))
}

#[cfg(windows)]
pub(crate) fn windows_path_volume_serial(path: &Path) -> io::Result<u32> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime storage path is a reparse point",
        ));
    }

    let volume_serial = if metadata.is_dir() {
        let probe = path.join(".tng-volume-probe");
        let parents = open_windows_parent_dirs(&probe)?;
        let directory = parents.last().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "runtime storage directory has no opened path components",
            )
        })?;
        windows_file_identity(directory)?.0
    } else if metadata.is_file() {
        let file = open_path_no_follow_windows(path, false, false)?;
        windows_file_identity(&file)?.0
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime storage path is not a regular file or directory",
        ));
    };

    Ok(volume_serial)
}

/// Create a directory tree without following an ancestor symlink.
///
/// On Unix, each component is opened relative to the descriptor for the
/// component before it. Missing components are created with `mkdirat`, then
/// reopened with `O_NOFOLLOW` before the next component is traversed. This is
/// the directory equivalent of [`open_path_no_follow`] and closes the small
/// but real window where `create_dir_all` could create a configured download
/// path outside its authorized root.
///
/// On Windows, each path prefix is opened with `FILE_FLAG_OPEN_REPARSE_POINT`
/// and retained without delete-sharing while the next component is accessed,
/// preventing a reparse-point ancestor from being substituted during the
/// walk.
pub fn create_dir_all_no_follow(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        create_dir_all_no_follow_unix(path)
    }
    #[cfg(windows)]
    {
        create_dir_all_no_follow_windows(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::fs::create_dir_all(path)
    }
}

/// Read metadata through the same ancestor-safe open path used by runtime
/// I/O. On Unix this prevents a replaced ancestor from redirecting a
/// fast-resume hint lookup to an unrelated tree. The returned metadata is
/// for the opened object, not a second path lookup.
pub fn metadata_no_follow(path: &Path) -> io::Result<std::fs::Metadata> {
    let file = open_path_no_follow(path, false, false)?;
    file.metadata()
}

/// Read a runtime-owned file with a hard byte ceiling. Path safety does not
/// stop a corrupted or operator-planted file from forcing an unbounded
/// allocation before its parser gets a chance to reject it.
pub fn read_file_no_follow_limited(path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
    let file = open_path_no_follow(path, false, false)?;
    let file_len = file.metadata()?.len();
    if file_len > max_bytes as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "runtime file {} is {} bytes, maximum is {max_bytes}",
                path.display(),
                file_len
            ),
        ));
    }
    let capacity = usize::try_from(file_len)
        .unwrap_or(max_bytes)
        .min(max_bytes);
    let mut bytes = Vec::with_capacity(capacity);
    // Read only the length observed on the opened descriptor. Reading up to
    // `max_bytes + 1` allows a concurrently growing file to force an
    // allocation far beyond the size that callers preflighted. The final
    // descriptor check still rejects that growth, but the allocation must be
    // bounded before the check runs.
    let mut limited = (&file).take(file_len);
    limited.read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes || file.metadata()?.len() > file_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "runtime file {} grew while it was being read",
                path.display()
            ),
        ));
    }
    Ok(bytes)
}

/// Replace the contents of a runtime-owned file without following a symlink.
pub fn write_file_no_follow(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = open_path_no_follow(path, true, true)?;
    file.set_len(0)?;
    file.write_all(contents)
}

/// Replace the contents of a runtime-owned file without following a symlink,
/// and wait for the file data to reach stable storage before returning.
pub fn write_file_no_follow_sync(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = open_path_no_follow(path, true, true)?;
    file.set_len(0)?;
    file.write_all(contents)?;
    file.sync_all()
}

/// Sync a runtime-owned directory after an atomic rename or unlink.
///
/// On Unix the directory is opened by descriptor-relative, no-follow
/// traversal so the sync cannot be redirected through a replaced ancestor.
/// Other platforms receive the file-level durability already provided by the
/// caller; directory fsync is not exposed consistently there.
pub fn sync_dir_no_follow(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        open_directory_no_follow_unix(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Remove a runtime-owned file without following a final component or any
/// ancestor symlink. A symlink itself is removed; its target is never touched.
pub fn remove_file_no_follow(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let (parent, name) = open_parent_no_follow_unix(path)?;
        let name = CString::new(name.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "runtime path contains NUL")
        })?;
        let result = unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(windows)]
    {
        let _parents = open_windows_parent_dirs(path)?;
        std::fs::remove_file(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::fs::remove_file(path)
    }
}

/// Rename a runtime-owned file using descriptor-relative parents on Unix.
pub fn rename_no_follow(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let (source_parent, source_name) = open_parent_no_follow_unix(source)?;
        let (destination_parent, destination_name) = open_parent_no_follow_unix(destination)?;
        let source_name = CString::new(source_name.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "runtime source contains NUL")
        })?;
        let destination_name =
            CString::new(destination_name.as_os_str().as_bytes()).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "runtime destination contains NUL",
                )
            })?;
        let result = unsafe {
            libc::renameat(
                source_parent.as_raw_fd(),
                source_name.as_ptr(),
                destination_parent.as_raw_fd(),
                destination_name.as_ptr(),
            )
        };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(windows)]
    {
        let _source_parents = open_windows_parent_dirs(source)?;
        let _destination_parents = open_windows_parent_dirs(destination)?;
        std::fs::rename(source, destination)
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::fs::rename(source, destination)
    }
}

/// Rename a file without replacing an existing destination.
///
/// `MoveFileW` makes the no-replace condition atomic: if another process
/// creates the destination after the check, the move fails and leaves both
/// files intact. Held parent handles also prevent ancestor replacement while
/// the paths are resolved. Storage plans use their root-relative executor.
#[cfg(all(windows, test))]
pub(crate) fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    let _source_parents = open_windows_parent_dirs(source)?;
    let _destination_parents = open_windows_parent_dirs(destination)?;
    let source = windows_move_path(source)?;
    let destination = windows_move_path(destination)?;
    let result = unsafe { MoveFileW(source.as_ptr(), destination.as_ptr()) };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(all(windows, test))]
fn windows_move_path(path: &Path) -> io::Result<Vec<u16>> {
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime move path must identify a file",
        )
    })?;
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime move path must have a parent directory",
        )
    })?;
    // Canonicalize only the parent: the final destination may not exist.
    // Parent directories are already held open without delete-sharing by the
    // caller, so this cannot be redirected through an ancestor replacement.
    let mut absolute = std::fs::canonicalize(parent)?;
    absolute.push(file_name);
    let mut wide: Vec<u16> = absolute
        .as_os_str()
        .encode_wide()
        .map(|unit| {
            if unit == b'/' as u16 {
                b'\\' as u16
            } else {
                unit
            }
        })
        .collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime move path contains NUL",
        ));
    }

    let verbatim_prefix = [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    let unc_prefix = [b'\\' as u16, b'\\' as u16];
    if wide.starts_with(&verbatim_prefix) {
        // Already in the extended-length namespace.
    } else if wide.starts_with(&unc_prefix) {
        let mut extended = r"\\?\UNC\".encode_utf16().collect::<Vec<_>>();
        extended.extend_from_slice(&wide[2..]);
        wide = extended;
    } else if wide.len() >= 3
        && matches!(wide[0], 0x41..=0x5a | 0x61..=0x7a)
        && wide[1] == b':' as u16
        && wide[2] == b'\\' as u16
    {
        let mut extended = r"\\?\".encode_utf16().collect::<Vec<_>>();
        extended.append(&mut wide);
        wide = extended;
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime move path is not an absolute drive or UNC path",
        ));
    }
    if wide.len() >= 32_767 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime move path exceeds the Windows path limit",
        ));
    }
    wide.push(0);
    Ok(wide)
}

#[cfg(windows)]
fn open_path_no_follow_windows(path: &Path, write: bool, create: bool) -> io::Result<File> {
    let _parents = open_windows_parent_dirs(path)?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(write)
        .create(write && create)
        .truncate(false)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    ensure_regular_file(options.open(path)?)
}

#[cfg(windows)]
fn open_windows_parent_dirs(path: &Path) -> io::Result<Vec<File>> {
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime storage path must be an absolute file path",
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime storage path must identify a file",
        )
    })?;
    open_windows_directory_chain(parent)
}

#[cfg(windows)]
fn open_windows_directory_chain(path: &Path) -> io::Result<Vec<File>> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime storage directory must be absolute",
        ));
    }
    let mut current = PathBuf::new();
    let mut opened = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => {
                current.push(component.as_os_str());
                opened.push(open_windows_directory(&current)?);
            }
            Component::Normal(name) => {
                current.push(name);
                opened.push(open_windows_directory(&current)?);
            }
            Component::CurDir | Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "runtime storage path contains an unsafe component",
                ));
            }
        }
    }
    if opened.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime storage directory has no rooted components",
        ));
    }
    Ok(opened)
}

#[cfg(any(all(not(unix), not(windows)), test))]
pub(crate) struct ParentDirsGuard {
    #[cfg(windows)]
    _handles: Vec<File>,
}

#[cfg(any(all(not(unix), not(windows)), test))]
pub(crate) fn hold_parent_dirs_no_follow(path: &Path) -> io::Result<ParentDirsGuard> {
    #[cfg(windows)]
    {
        Ok(ParentDirsGuard {
            _handles: open_windows_parent_dirs(path)?,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(ParentDirsGuard {})
    }
}

#[cfg(any(all(not(unix), not(windows)), test))]
pub(crate) struct DirectoryGuard {
    #[cfg(windows)]
    _handle: File,
}

#[cfg(any(all(not(unix), not(windows)), test))]
pub(crate) fn hold_directory_no_follow(path: &Path) -> io::Result<DirectoryGuard> {
    #[cfg(windows)]
    {
        Ok(DirectoryGuard {
            _handle: open_windows_directory(path)?,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(DirectoryGuard {})
    }
}

#[cfg(windows)]
fn open_windows_directory(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        // Holding each traversed directory without FILE_SHARE_DELETE prevents
        // it from being renamed or replaced while a later full-path open is
        // resolved. FILE_FLAG_OPEN_REPARSE_POINT lets us reject junctions and
        // other reparse tags on the opened handle itself.
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
    let directory = options.open(path)?;
    let metadata = directory.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime storage ancestor is a reparse point",
        ));
    }
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "runtime storage ancestor is not a directory",
        ));
    }
    Ok(directory)
}

/// Open a Windows directory as a capability root after walking every path
/// component without following reparse points. The returned handle omits
/// `FILE_SHARE_DELETE`, so the directory cannot be renamed out from under
/// handle-relative storage-plan operations.
#[cfg(windows)]
pub(crate) fn open_windows_directory_capability(path: &Path) -> io::Result<cap_std::fs::Dir> {
    let mut handles = open_windows_directory_chain(path)?;
    let directory = handles.pop().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime storage directory has no rooted components",
        )
    })?;
    Ok(cap_std::fs::Dir::from_std_file(directory))
}

#[cfg(windows)]
pub(crate) fn reopen_windows_directory(
    directory: &cap_std::fs::Dir,
) -> io::Result<cap_std::fs::Dir> {
    cap_std::fs::Dir::reopen_dir(directory)
}

#[cfg(windows)]
fn create_dir_all_no_follow_windows(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime storage path must be absolute",
        ));
    }

    let mut current = PathBuf::new();
    let mut opened = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => {
                current.push(component.as_os_str());
                opened.push(open_windows_directory(&current)?);
            }
            Component::Normal(name) => {
                current.push(name);
                match open_windows_directory(&current) {
                    Ok(directory) => opened.push(directory),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        match std::fs::create_dir(&current) {
                            Ok(()) => {}
                            Err(create_error)
                                if create_error.kind() == io::ErrorKind::AlreadyExists => {}
                            Err(create_error) => return Err(create_error),
                        }
                        opened.push(open_windows_directory(&current)?);
                    }
                    Err(error) => return Err(error),
                }
            }
            Component::CurDir | Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "runtime storage path contains an unsafe component",
                ));
            }
        }
    }
    if opened.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime storage path must be absolute",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn open_path_no_follow_unix(path: &Path, write: bool, create: bool) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime storage path must be absolute",
        ));
    }

    let mut parts = Vec::<PathBuf>::new();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(value) => parts.push(PathBuf::from(value)),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "runtime storage path contains an unsafe component",
                ));
            }
        }
    }
    let Some(final_name) = parts.pop() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime storage path must identify a file",
        ));
    };

    let parent = open_parent_from_parts(&parts)?;

    let name = std::ffi::CString::new(final_name.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime storage path contains NUL",
        )
    })?;
    let mut flags = if write { libc::O_RDWR } else { libc::O_RDONLY };
    // A runtime file may be replaced by a FIFO between validation and use.
    // Nonblocking open prevents that special file from stalling a storage
    // worker before the descriptor can be rejected as non-regular.
    flags |= libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
    if write && create {
        flags |= libc::O_CREAT;
    }
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags, 0o644) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        ensure_regular_file(unsafe { File::from_raw_fd(fd) })
    }
}

#[cfg(unix)]
fn open_parent_no_follow_unix(path: &Path) -> io::Result<(File, OsString)> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime storage path must be absolute",
        ));
    }
    let mut parts = Vec::<PathBuf>::new();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(value) => parts.push(PathBuf::from(value)),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "runtime storage path contains an unsafe component",
                ));
            }
        }
    }
    let Some(final_name) = parts.pop() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime storage path must identify a file",
        ));
    };
    Ok((open_parent_from_parts(&parts)?, final_name.into_os_string()))
}

#[cfg(unix)]
fn open_directory_no_follow_unix(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime storage path must be absolute",
        ));
    }
    let mut parts = Vec::<PathBuf>::new();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(value) => parts.push(PathBuf::from(value)),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "runtime storage path contains an unsafe component",
                ));
            }
        }
    }
    open_parent_from_parts(&parts)
}

#[cfg(unix)]
fn open_parent_from_parts(parts: &[PathBuf]) -> io::Result<File> {
    let root_name = CString::new("/").expect("literal has no NUL");
    let root_fd = unsafe {
        libc::open(
            root_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if root_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut parent = unsafe { File::from_raw_fd(root_fd) };
    for component in parts {
        let name = CString::new(component.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "runtime storage path contains NUL",
            )
        })?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        parent = unsafe { File::from_raw_fd(fd) };
    }
    Ok(parent)
}

#[cfg(unix)]
fn create_dir_all_no_follow_unix(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime storage path must be absolute",
        ));
    }

    let root_name = std::ffi::CString::new("/").expect("literal has no NUL");
    let root_fd = unsafe {
        libc::open(
            root_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if root_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut parent = unsafe { File::from_raw_fd(root_fd) };

    for component in path.components() {
        let Component::Normal(value) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "runtime storage path contains an unsafe component",
            ));
        };
        let name = std::ffi::CString::new(value.as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "runtime storage path contains NUL",
            )
        })?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
        let open_error = if fd < 0 {
            Some(io::Error::last_os_error())
        } else {
            None
        };
        let fd = if fd >= 0 {
            fd
        } else if open_error
            .as_ref()
            .is_some_and(|error| error.kind() == io::ErrorKind::NotFound)
        {
            let mkdir_result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o755) };
            if mkdir_result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
            let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            fd
        } else {
            return Err(open_error.expect("failed openat has an OS error"));
        };
        parent = unsafe { File::from_raw_fd(fd) };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limited_read_rejects_oversized_runtime_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state.bin");
        std::fs::write(&path, b"1234").unwrap();

        let error = read_file_no_follow_limited(&path, 3).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(read_file_no_follow_limited(&path, 4).unwrap(), b"1234");
    }

    #[test]
    fn create_new_no_follow_refuses_to_replace_an_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("existing.bin");
        std::fs::write(&path, b"existing").unwrap();

        let error = create_new_file_no_follow(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(path).unwrap(), b"existing");
    }

    #[cfg(windows)]
    #[test]
    fn runtime_open_rejects_a_final_file_reparse_point() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target.bin");
        let link = temp.path().join("link.bin");
        std::fs::write(&target, b"target").unwrap();
        if let Err(error) = std::os::windows::fs::symlink_file(&target, &link) {
            if error.kind() == io::ErrorKind::PermissionDenied {
                return;
            }
            panic!("failed to create file symlink for regression test: {error}");
        }

        let error = open_path_no_follow(&link, false, false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(windows)]
    #[test]
    fn runtime_open_rejects_a_reparse_point_ancestor() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let alias = root.path().join("alias");
        if let Err(error) = std::os::windows::fs::symlink_dir(outside.path(), &alias) {
            if error.kind() == io::ErrorKind::PermissionDenied {
                return;
            }
            panic!("failed to create directory symlink for regression test: {error}");
        }

        let error = open_path_no_follow(&alias.join("payload.bin"), false, false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(windows)]
    #[test]
    fn no_replace_rename_preserves_an_existing_destination() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        std::fs::write(&source, b"source").unwrap();
        std::fs::write(&destination, b"destination").unwrap();

        let error = rename_no_replace(&source, &destination).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(source).unwrap(), b"source");
        assert_eq!(std::fs::read(destination).unwrap(), b"destination");
    }

    #[cfg(windows)]
    #[test]
    fn held_directory_handle_prevents_path_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("protected");
        let replacement = temp.path().join("replacement");
        std::fs::create_dir(&directory).unwrap();

        let guard = hold_directory_no_follow(&directory).unwrap();
        assert!(std::fs::rename(&directory, &replacement).is_err());
        drop(guard);
        std::fs::rename(&directory, &replacement).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn directory_creation_does_not_traverse_a_reparse_point_ancestor() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let alias = root.path().join("alias");
        if let Err(error) = std::os::windows::fs::symlink_dir(outside.path(), &alias) {
            if error.kind() == io::ErrorKind::PermissionDenied {
                return;
            }
            panic!("failed to create directory symlink for regression test: {error}");
        }

        let error = create_dir_all_no_follow(&alias.join("created")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(!outside.path().join("created").exists());
    }

    #[cfg(unix)]
    #[test]
    fn sync_directory_uses_the_runtime_directory_boundary() {
        let temp = tempfile::tempdir().unwrap();
        sync_dir_no_follow(temp.path()).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn limited_read_rejects_fifo_without_blocking() {
        use std::os::unix::ffi::OsStrExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state.fifo");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let result = unsafe { libc::mkfifo(name.as_ptr(), 0o600) };
        assert_eq!(result, 0);

        let error = read_file_no_follow_limited(&path, 64).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
