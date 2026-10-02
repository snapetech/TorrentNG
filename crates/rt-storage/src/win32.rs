//! Minimal hand-declared Win32 bindings.
//!
//! `rt-storage` needs a handful of kernel32 calls (file identity, uptime,
//! allocated-range queries, volume information). They are declared here rather
//! than pulling in a binding crate; kernel32 is already linked by `std`.
//! Everything in this module is `cfg(windows)`.
//!
//! Every wrapper returns `io::Result` (or `Option`) and never panics: a
//! failing call must degrade a feature to "unsupported", not abort the daemon.

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

type Handle = *mut c_void;
type Bool = i32;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FileTime {
    low: u32,
    high: u32,
}

#[repr(C)]
#[derive(Default)]
struct ByHandleFileInformation {
    file_attributes: u32,
    creation_time: FileTime,
    last_access_time: FileTime,
    last_write_time: FileTime,
    volume_serial_number: u32,
    file_size_high: u32,
    file_size_low: u32,
    number_of_links: u32,
    file_index_high: u32,
    file_index_low: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetFileInformationByHandle(file: Handle, info: *mut ByHandleFileInformation) -> Bool;
    fn GetTickCount64() -> u64;
    fn DeviceIoControl(
        device: Handle,
        control_code: u32,
        in_buffer: *const c_void,
        in_size: u32,
        out_buffer: *mut c_void,
        out_size: u32,
        bytes_returned: *mut u32,
        overlapped: *mut c_void,
    ) -> Bool;
    fn GetVolumePathNameW(file_name: *const u16, volume_path: *mut u16, buffer_len: u32) -> Bool;
    fn GetVolumeInformationW(
        root: *const u16,
        volume_name: *mut u16,
        volume_name_len: u32,
        serial: *mut u32,
        max_component_len: *mut u32,
        flags: *mut u32,
        fs_name: *mut u16,
        fs_name_len: u32,
    ) -> Bool;
    fn GetDriveTypeW(root: *const u16) -> u32;
}

const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
const SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;

const DRIVE_REMOTE: u32 = 4;
const DRIVE_RAMDISK: u32 = 6;

/// `(volume serial, file index)`: the identity of the object behind a handle.
pub(crate) fn file_identity(file: &File) -> io::Result<(u32, u64)> {
    let mut info = ByHandleFileInformation::default();
    // SAFETY: `file` owns a live handle and `info` is a correctly laid out
    // BY_HANDLE_FILE_INFORMATION for the duration of the call.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut info) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        info.volume_serial_number,
        (u64::from(info.file_index_high) << 32) | u64::from(info.file_index_low),
    ))
}

/// Identity of whatever `path` names, **without following a final reparse
/// point** (symlink or junction), so a link swapped in for a regular file is
/// seen as a different object.
pub(crate) fn path_identity(path: &Path) -> io::Result<(u32, u64)> {
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(SHARE_ALL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    file_identity(&file)
}

/// Milliseconds since the system started, including time spent asleep.
pub(crate) fn uptime_millis() -> u64 {
    // SAFETY: no arguments, no pointers.
    unsafe { GetTickCount64() }
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// The volume mount point that holds `path` (for example `C:\`).
pub(crate) fn volume_root(path: &Path) -> Option<Vec<u16>> {
    let input = wide(path);
    let mut out = vec![0u16; 1024];
    // SAFETY: `input` is NUL terminated; `out` has the advertised length.
    let ok = unsafe { GetVolumePathNameW(input.as_ptr(), out.as_mut_ptr(), out.len() as u32) };
    if ok == 0 {
        return None;
    }
    let end = out.iter().position(|c| *c == 0)?;
    out.truncate(end);
    out.push(0);
    (end > 0).then_some(out)
}

/// Filesystem name (`NTFS`, `ReFS`, `exFAT`, ...) of the volume at `root`.
pub(crate) fn filesystem_name(root: &[u16]) -> Option<String> {
    let mut name = vec![0u16; 64];
    // SAFETY: `root` is NUL terminated; every out pointer is either null
    // (allowed for the values we do not want) or a live buffer of the given size.
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            name.as_mut_ptr(),
            name.len() as u32,
        )
    };
    if ok == 0 {
        return None;
    }
    let end = name.iter().position(|c| *c == 0).unwrap_or(name.len());
    Some(
        std::ffi::OsString::from_wide(&name[..end])
            .to_string_lossy()
            .into_owned(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DriveKind {
    Remote,
    RamDisk,
    Other,
}

pub(crate) fn drive_kind(root: &[u16]) -> DriveKind {
    // SAFETY: `root` is NUL terminated.
    match unsafe { GetDriveTypeW(root.as_ptr()) } {
        DRIVE_REMOTE => DriveKind::Remote,
        DRIVE_RAMDISK => DriveKind::RamDisk,
        _ => DriveKind::Other,
    }
}

/// Mark `file` sparse (`FSCTL_SET_SPARSE`) so unwritten regions stop taking
/// clusters. Test support: production never creates sparse files this way.
#[cfg(test)]
pub(crate) fn set_sparse(file: &File) -> io::Result<()> {
    const FSCTL_SET_SPARSE: u32 = 0x0009_00C4;
    let mut returned = 0u32;
    // SAFETY: no input buffer is required for FSCTL_SET_SPARSE's default form.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle().cast(),
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// One `FILE_ALLOCATED_RANGE_BUFFER`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AllocatedRange {
    offset: i64,
    length: i64,
}

const FSCTL_QUERY_ALLOCATED_RANGES: u32 = 0x0009_40CF;
const ERROR_MORE_DATA: i32 = 234;
const RANGES_PER_CALL: usize = 256;

/// Byte ranges of `file` (within `[0, len)`) that have allocated clusters,
/// via `FSCTL_QUERY_ALLOCATED_RANGES`.
///
/// `Ok(None)` when the filesystem cannot answer (FAT, some network shares).
/// On a file that is not sparse the whole file is reported as allocated, so
/// only sparse files ever show gaps. Paging and termination live in
/// [`crate::alloc_audit::collect_allocated_ranges`].
pub(crate) fn allocated_ranges(file: &File, len: u64) -> io::Result<Option<Vec<(u64, u64)>>> {
    use crate::alloc_audit::{collect_allocated_ranges, RangePage};
    let handle: Handle = file.as_raw_handle().cast();
    collect_allocated_ranges(len, |offset, length| {
        let query = AllocatedRange {
            offset: offset as i64,
            length: length.min(i64::MAX as u64) as i64,
        };
        let mut buffer = [AllocatedRange::default(); RANGES_PER_CALL];
        let mut returned = 0u32;
        // SAFETY: `query` and `buffer` are live for the call and sized as
        // declared; `returned` receives the byte count.
        let ok = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_QUERY_ALLOCATED_RANGES,
                (&query as *const AllocatedRange).cast(),
                std::mem::size_of::<AllocatedRange>() as u32,
                buffer.as_mut_ptr().cast(),
                std::mem::size_of_val(&buffer) as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        let more = if ok == 0 {
            let err = io::Error::last_os_error();
            match err.raw_os_error() {
                Some(ERROR_MORE_DATA) => true,
                // ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, ERROR_INVALID_PARAMETER
                Some(1) | Some(50) | Some(87) => return Ok(None),
                _ => return Err(err),
            }
        } else {
            false
        };
        let count =
            (returned as usize / std::mem::size_of::<AllocatedRange>()).min(RANGES_PER_CALL);
        Ok(Some(RangePage {
            ranges: buffer[..count]
                .iter()
                .map(|range| (range.offset, range.length))
                .collect(),
            more,
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_and_its_path_have_the_same_identity() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("same.bin");
        std::fs::write(&p, b"payload").unwrap();
        let file = File::open(&p).unwrap();
        assert_eq!(file_identity(&file).unwrap(), path_identity(&p).unwrap());
    }

    #[test]
    fn different_files_have_different_identities() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        assert_ne!(path_identity(&a).unwrap(), path_identity(&b).unwrap());
    }

    #[test]
    fn a_missing_path_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = path_identity(&dir.path().join("nope")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn uptime_is_positive_and_monotonic() {
        let first = uptime_millis();
        assert!(first > 0);
        assert!(uptime_millis() >= first);
    }

    #[test]
    fn volume_information_names_a_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        let root = volume_root(dir.path()).expect("volume root");
        let name = filesystem_name(&root).expect("filesystem name");
        assert!(!name.is_empty());
        // Whatever the kind, classification must not panic.
        let _ = drive_kind(&root);
    }

    #[test]
    fn allocated_ranges_of_a_written_file_cover_the_data_or_are_unsupported() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("data.bin");
        let mut f = File::create(&p).unwrap();
        f.write_all(&vec![1u8; 8192]).unwrap();
        f.sync_all().unwrap();
        let f = File::open(&p).unwrap();
        match allocated_ranges(&f, 8192).unwrap() {
            None => {}
            Some(ranges) => {
                // A fully written file has no hole: every byte is allocated.
                let covered: u64 = ranges.iter().map(|(s, e)| e - s).sum();
                assert_eq!(covered, 8192, "{ranges:?}");
            }
        }
    }
}
