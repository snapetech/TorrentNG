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
#[cfg(unix)]
use std::path::{Component, PathBuf};

pub(crate) fn open_path_no_follow(path: &Path, write: bool, create: bool) -> io::Result<File> {
    #[cfg(unix)]
    {
        open_path_no_follow_unix(path, write, create)
    }
    #[cfg(not(unix))]
    {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(write)
            .create(write && create)
            .truncate(false);
        options.open(path)
    }
}

/// Create a directory tree without following an ancestor symlink.
///
/// On Unix, each component is opened relative to the descriptor for the
/// component before it. Missing components are created with `mkdirat`, then
/// reopened with `O_NOFOLLOW` before the next component is traversed. This is
/// the directory equivalent of [`open_path_no_follow`] and closes the small
/// but real window where `create_dir_all` could create a configured download
/// path outside its authorized root.
pub fn create_dir_all_no_follow(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        create_dir_all_no_follow_unix(path)
    }
    #[cfg(not(unix))]
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
    let mut limited = file.take(max_bytes.saturating_add(1) as u64);
    limited.read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "runtime file {} grew beyond the {max_bytes} byte limit",
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
    #[cfg(not(unix))]
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
    #[cfg(not(unix))]
    {
        std::fs::rename(source, destination)
    }
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
    flags |= libc::O_CLOEXEC | libc::O_NOFOLLOW;
    if write && create {
        flags |= libc::O_CREAT;
    }
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags, 0o644) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
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
}
