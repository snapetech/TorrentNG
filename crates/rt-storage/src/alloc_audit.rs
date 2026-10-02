//! Filesystem allocation audit.
//!
//! After an unclean shutdown the classic failure of preallocating clients is a
//! file that is already at full size but only partly written: the missing
//! regions read back as zeros. Most filesystems can say, without reading any
//! payload, which byte ranges of a file are **holes** (no backing blocks) or
//! **unwritten extents** (blocks reserved by `fallocate` but never written).
//! A piece the resume state calls `Valid` must not overlap either.
//!
//! This module reports those ranges. It is a *prefilter*, never a verdict:
//!
//! * A range flagged here means "re-hash before trusting", not "corrupt".
//!   Compressing and deduplicating filesystems (ZFS, btrfs) and sparse-aware
//!   copy tools legitimately store zero runs as holes, and BEP 47 pad files
//!   are all zeros by definition.
//! * The absence of a flagged range proves nothing. A filesystem that wrote
//!   real zero blocks (for example glibc's `posix_fallocate` emulation, which
//!   TorrentNG no longer uses) is indistinguishable from data.
//!
//! Platform support:
//!
//! * **Linux**: `FS_IOC_FIEMAP` (holes *and* unwritten extents), falling back to
//!   `SEEK_HOLE`/`SEEK_DATA` (holes only).
//! * **macOS and FreeBSD**: `SEEK_HOLE`/`SEEK_DATA` (holes only). Preallocation
//!   there is a sparse length, so a never-written region is a hole.
//! * **Windows**: `FSCTL_QUERY_ALLOCATED_RANGES` (holes only, and only for
//!   sparse files; NTFS keeps a "valid data length" for never-written tails but
//!   no documented API exposes it).
//! * Everything else returns [`AllocationAudit::Unsupported`]; callers must
//!   treat that as "no information" and continue.

use std::io;
use std::path::Path;

/// Cap on extents walked per file. A pathologically fragmented file is
/// reported as one whole-file gap (distrust it) instead of consuming
/// unbounded time or memory.
pub const MAX_AUDIT_EXTENTS_PER_FILE: usize = 1 << 20;

/// Half-open byte range `[start, end)` within one file.
pub type ByteRange = (u64, u64);

/// Byte ranges of one file that are not backed by written data.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AllocationGaps {
    /// Ranges with no backing blocks at all.
    pub holes: Vec<ByteRange>,
    /// Ranges backed by preallocated but never-written extents.
    pub unwritten: Vec<ByteRange>,
}

impl AllocationGaps {
    pub fn is_empty(&self) -> bool {
        self.holes.is_empty() && self.unwritten.is_empty()
    }

    /// Holes and unwritten extents merged into one sorted, non-overlapping
    /// list.
    pub fn merged(&self) -> Vec<ByteRange> {
        let mut all: Vec<ByteRange> = self
            .holes
            .iter()
            .chain(self.unwritten.iter())
            .copied()
            .filter(|(start, end)| end > start)
            .collect();
        all.sort_unstable();
        let mut merged: Vec<ByteRange> = Vec::with_capacity(all.len());
        for (start, end) in all {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        merged
    }
}

/// How the allocation map was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditMethod {
    /// `FS_IOC_FIEMAP`: reports holes and unwritten extents.
    Fiemap,
    /// `SEEK_HOLE`/`SEEK_DATA`: reports holes only.
    SeekHole,
    /// Windows `FSCTL_QUERY_ALLOCATED_RANGES`: reports holes of sparse files.
    AllocatedRanges,
}

impl AuditMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditMethod::Fiemap => "fiemap",
            AuditMethod::SeekHole => "seek_hole",
            AuditMethod::AllocatedRanges => "allocated_ranges",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllocationAudit {
    /// This platform or filesystem cannot report allocation.
    Unsupported,
    Gaps {
        gaps: AllocationGaps,
        method: AuditMethod,
    },
}

/// Whether the allocation audit can run at all on this platform. Surfaced in
/// the API so the UI can grey out the option honestly.
pub const fn allocation_audit_supported() -> bool {
    cfg!(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        windows
    ))
}

/// Report the holes and unwritten extents of the file at `path`, considering
/// only the first `len` bytes.
///
/// Errors are real I/O failures (missing file, permission). A filesystem that
/// simply cannot answer yields `Ok(AllocationAudit::Unsupported)`.
pub fn audit_file_allocation(path: &Path, len: u64) -> io::Result<AllocationAudit> {
    #[cfg(target_os = "linux")]
    {
        linux::audit(path, len)
    }
    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    {
        bsd::audit(path, len)
    }
    #[cfg(windows)]
    {
        windows::audit(path, len)
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        windows
    )))]
    {
        let _ = (path, len);
        Ok(AllocationAudit::Unsupported)
    }
}

/// One page of an allocated-range query, in the OS's own terms.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) struct RangePage {
    /// `(offset, length)` pairs as reported; malformed entries are ignored.
    pub ranges: Vec<(i64, i64)>,
    /// The answer did not fit and the query must continue past the last range.
    pub more: bool,
}

/// Most pages an allocated-range query may take. A file with more ranges than
/// this is reported as having no allocated data at all, which the caller turns
/// into one whole-file gap: distrust it rather than spend unbounded time.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
const MAX_RANGE_PAGES: usize = 4096;

/// Drive a paged "which byte ranges are allocated" query over a file of `len`
/// bytes. `query(offset, length)` returns one page, or `Ok(None)` when the
/// filesystem cannot answer. Split from the Windows syscall wrapper so the
/// paging, clamping and termination logic is testable anywhere.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) fn collect_allocated_ranges(
    len: u64,
    mut query: impl FnMut(u64, u64) -> io::Result<Option<RangePage>>,
) -> io::Result<Option<Vec<(u64, u64)>>> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    let mut cursor = 0u64;
    let mut pages = 0usize;
    while cursor < len {
        pages += 1;
        if pages > MAX_RANGE_PAGES {
            return Ok(Some(Vec::new()));
        }
        let Some(page) = query(cursor, len - cursor)? else {
            return Ok(None);
        };
        let before = cursor;
        let reported = page.ranges.len();
        for (offset, length) in page.ranges {
            if offset < 0 || length <= 0 {
                continue;
            }
            let start = offset as u64;
            let end = start.saturating_add(length as u64).min(len);
            if end > start {
                out.push((start, end));
                cursor = cursor.max(end);
            }
        }
        // Done when the OS says so, when it returned nothing, or when a page
        // made no progress (a misbehaving driver must not loop us forever).
        if !page.more || reported == 0 || cursor <= before {
            break;
        }
    }
    Ok(Some(out))
}

/// Holes from a sorted list of allocated ranges `[start, end)` covering a file
/// of `len` bytes. Shared by the platforms that report what *is* allocated.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) fn gaps_from_allocated(
    allocated: impl IntoIterator<Item = (u64, u64)>,
    len: u64,
) -> AllocationGaps {
    gaps_from_extents(
        allocated
            .into_iter()
            .map(|(start, end)| (start, end.saturating_sub(start), false)),
        len,
    )
}

/// Fold a sorted list of extents `(logical, length, unwritten)` covering a
/// file of `len` bytes into holes and unwritten ranges. Split from the ioctl
/// plumbing so the gap arithmetic is testable on any platform.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn gaps_from_extents(
    extents: impl IntoIterator<Item = (u64, u64, bool)>,
    len: u64,
) -> AllocationGaps {
    let mut gaps = AllocationGaps::default();
    let mut cursor = 0u64;
    for (logical, length, unwritten) in extents {
        if length == 0 {
            continue;
        }
        let end = logical.saturating_add(length).min(len);
        let start = logical.min(len);
        if start > cursor {
            gaps.holes.push((cursor, start));
        }
        if unwritten && end > start {
            gaps.unwritten.push((start, end));
        }
        cursor = cursor.max(end);
        if cursor >= len {
            break;
        }
    }
    if cursor < len {
        gaps.holes.push((cursor, len));
    }
    gaps
}

/// `SEEK_HOLE`/`SEEK_DATA` walk shared by every Unix that supports it.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
mod unix_seek {
    use super::*;
    use std::fs::File;
    use std::os::fd::AsRawFd;

    pub(super) fn unsupported_errno(errno: i32) -> bool {
        matches!(
            errno,
            libc::ENOTTY | libc::EOPNOTSUPP | libc::ENOSYS | libc::EINVAL
        )
    }

    /// Holes via `SEEK_HOLE`/`SEEK_DATA`. `Ok(None)` when unsupported.
    pub(super) fn seek_hole_gaps(file: &File, len: u64) -> io::Result<Option<AllocationGaps>> {
        let fd = file.as_raw_fd();
        let mut gaps = AllocationGaps::default();
        let mut cursor = 0u64;
        let mut iterations = 0usize;
        while cursor < len {
            iterations += 1;
            if iterations > MAX_AUDIT_EXTENTS_PER_FILE {
                gaps.holes = vec![(0, len)];
                return Ok(Some(gaps));
            }
            // SAFETY: plain lseek on a live descriptor.
            let hole = unsafe { libc::lseek(fd, cursor as libc::off_t, libc::SEEK_HOLE) };
            if hole < 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error().is_some_and(unsupported_errno)
                    || err.raw_os_error() == Some(libc::ENXIO)
                {
                    return Ok(if iterations == 1 { None } else { Some(gaps) });
                }
                return Err(err);
            }
            let hole = (hole as u64).min(len);
            if hole >= len {
                break;
            }
            // SAFETY: as above.
            let data = unsafe { libc::lseek(fd, hole as libc::off_t, libc::SEEK_DATA) };
            if data < 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::ENXIO) {
                    // The hole extends to end of file.
                    gaps.holes.push((hole, len));
                    break;
                }
                if err.raw_os_error().is_some_and(unsupported_errno) {
                    return Ok(if iterations == 1 { None } else { Some(gaps) });
                }
                return Err(err);
            }
            let data = (data as u64).min(len);
            if data > hole {
                gaps.holes.push((hole, data));
            }
            if data <= cursor {
                break;
            }
            cursor = data;
        }
        Ok(Some(gaps))
    }
}

/// macOS and FreeBSD: holes only, via `SEEK_HOLE`/`SEEK_DATA`.
#[cfg(any(target_os = "macos", target_os = "freebsd"))]
mod bsd {
    use super::*;

    pub(super) fn audit(path: &Path, len: u64) -> io::Result<AllocationAudit> {
        let file = crate::open::open_path_no_follow(path, false, false)?;
        if len == 0 {
            return Ok(AllocationAudit::Gaps {
                gaps: AllocationGaps::default(),
                method: AuditMethod::SeekHole,
            });
        }
        Ok(match unix_seek::seek_hole_gaps(&file, len)? {
            Some(gaps) => AllocationAudit::Gaps {
                gaps,
                method: AuditMethod::SeekHole,
            },
            None => AllocationAudit::Unsupported,
        })
    }
}

/// Windows: sparse-file holes via `FSCTL_QUERY_ALLOCATED_RANGES`.
#[cfg(windows)]
mod windows {
    use super::*;

    pub(super) fn audit(path: &Path, len: u64) -> io::Result<AllocationAudit> {
        let file = crate::open::open_path_no_follow(path, false, false)?;
        if len == 0 {
            return Ok(AllocationAudit::Gaps {
                gaps: AllocationGaps::default(),
                method: AuditMethod::AllocatedRanges,
            });
        }
        Ok(match crate::win32::allocated_ranges(&file, len)? {
            Some(allocated) => AllocationAudit::Gaps {
                gaps: gaps_from_allocated(allocated, len),
                method: AuditMethod::AllocatedRanges,
            },
            None => AllocationAudit::Unsupported,
        })
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::fs::File;
    use std::os::fd::AsRawFd;
    use unix_seek::{seek_hole_gaps, unsupported_errno};

    const FS_IOC_FIEMAP: u64 = 0xC020_660B;
    const FIEMAP_EXTENT_LAST: u32 = 0x0000_0001;
    const FIEMAP_EXTENT_UNWRITTEN: u32 = 0x0000_0800;
    const EXTENTS_PER_CALL: usize = 256;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct FiemapHeader {
        fm_start: u64,
        fm_length: u64,
        fm_flags: u32,
        fm_mapped_extents: u32,
        fm_extent_count: u32,
        fm_reserved: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct FiemapExtent {
        fe_logical: u64,
        fe_physical: u64,
        fe_length: u64,
        fe_reserved64: [u64; 2],
        fe_flags: u32,
        fe_reserved: [u32; 3],
    }

    const HEADER_WORDS: usize = std::mem::size_of::<FiemapHeader>() / 8;
    const EXTENT_WORDS: usize = std::mem::size_of::<FiemapExtent>() / 8;

    pub(super) fn audit(path: &Path, len: u64) -> io::Result<AllocationAudit> {
        // Symlink-safe, read-only, regular-file-only open like every other
        // payload access; the audit never reads payload bytes.
        let file = crate::open::open_path_no_follow(path, false, false)?;
        if len == 0 {
            return Ok(AllocationAudit::Gaps {
                gaps: AllocationGaps::default(),
                method: AuditMethod::Fiemap,
            });
        }
        match fiemap_extents(&file, len)? {
            Some(extents) => Ok(AllocationAudit::Gaps {
                gaps: gaps_from_extents(extents, len),
                method: AuditMethod::Fiemap,
            }),
            None => match seek_hole_gaps(&file, len)? {
                Some(gaps) => Ok(AllocationAudit::Gaps {
                    gaps,
                    method: AuditMethod::SeekHole,
                }),
                None => Ok(AllocationAudit::Unsupported),
            },
        }
    }

    /// `Ok(None)` means the filesystem does not implement FIEMAP. A file with
    /// too many extents is reported as one fully-unwritten span so it is
    /// distrusted rather than silently skipped.
    fn fiemap_extents(file: &File, len: u64) -> io::Result<Option<Vec<(u64, u64, bool)>>> {
        let fd = file.as_raw_fd();
        let mut out: Vec<(u64, u64, bool)> = Vec::new();
        let mut start = 0u64;
        let mut buffer = vec![0u64; HEADER_WORDS + EXTENT_WORDS * EXTENTS_PER_CALL];
        loop {
            buffer.iter_mut().for_each(|word| *word = 0);
            let header = FiemapHeader {
                fm_start: start,
                fm_length: len.saturating_sub(start),
                fm_flags: 0,
                fm_mapped_extents: 0,
                fm_extent_count: EXTENTS_PER_CALL as u32,
                fm_reserved: 0,
            };
            // SAFETY: `buffer` is 8-byte aligned and at least
            // `size_of::<FiemapHeader>()` bytes, and `FiemapHeader` is a
            // plain-old-data `repr(C)` struct.
            unsafe {
                std::ptr::write(buffer.as_mut_ptr().cast::<FiemapHeader>(), header);
            }
            // SAFETY: `fd` is a live descriptor owned by `file`, and `buffer`
            // is large enough for the header plus `fm_extent_count` extents,
            // which is what the kernel is told it may write.
            let rc = unsafe {
                libc::ioctl(
                    fd,
                    FS_IOC_FIEMAP as _,
                    buffer.as_mut_ptr().cast::<libc::c_void>(),
                )
            };
            if rc < 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error().is_some_and(unsupported_errno) {
                    return Ok(None);
                }
                return Err(err);
            }
            // SAFETY: the kernel filled the header it was given; reading it
            // back as the same `repr(C)` type is sound.
            let mapped = unsafe { std::ptr::read(buffer.as_ptr().cast::<FiemapHeader>()) }
                .fm_mapped_extents as usize;
            if mapped == 0 {
                break;
            }
            let mapped = mapped.min(EXTENTS_PER_CALL);
            let mut last_end = start;
            let mut saw_last = false;
            for index in 0..mapped {
                // SAFETY: `index < mapped <= EXTENTS_PER_CALL`, so the extent
                // lies inside `buffer`; the region is 8-byte aligned.
                let extent = unsafe {
                    std::ptr::read(
                        buffer
                            .as_ptr()
                            .add(HEADER_WORDS + index * EXTENT_WORDS)
                            .cast::<FiemapExtent>(),
                    )
                };
                out.push((
                    extent.fe_logical,
                    extent.fe_length,
                    extent.fe_flags & FIEMAP_EXTENT_UNWRITTEN != 0,
                ));
                last_end = extent.fe_logical.saturating_add(extent.fe_length);
                saw_last |= extent.fe_flags & FIEMAP_EXTENT_LAST != 0;
                if out.len() > MAX_AUDIT_EXTENTS_PER_FILE {
                    return Ok(Some(vec![(0, len, true)]));
                }
            }
            if saw_last || last_end >= len || last_end <= start {
                break;
            }
            start = last_end;
        }
        Ok(Some(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extents_with_no_gaps_yield_no_ranges() {
        let gaps = gaps_from_extents([(0, 4096, false), (4096, 4096, false)], 8192);
        assert!(gaps.is_empty());
    }

    #[test]
    fn leading_middle_and_trailing_holes_are_reported() {
        let gaps = gaps_from_extents([(4096, 4096, false), (16384, 4096, false)], 24576);
        assert_eq!(gaps.holes, vec![(0, 4096), (8192, 16384), (20480, 24576)]);
        assert!(gaps.unwritten.is_empty());
    }

    #[test]
    fn unwritten_extents_are_separate_from_holes() {
        let gaps = gaps_from_extents([(0, 4096, false), (4096, 8192, true)], 12288);
        assert!(gaps.holes.is_empty());
        assert_eq!(gaps.unwritten, vec![(4096, 12288)]);
    }

    #[test]
    fn extents_past_end_of_file_are_clamped() {
        let gaps = gaps_from_extents([(0, 1 << 20, true)], 1000);
        assert_eq!(gaps.unwritten, vec![(0, 1000)]);
        assert!(gaps.holes.is_empty());
    }

    #[test]
    fn empty_extent_list_is_one_whole_file_hole() {
        let gaps = gaps_from_extents(std::iter::empty(), 4096);
        assert_eq!(gaps.holes, vec![(0, 4096)]);
    }

    #[test]
    fn merged_coalesces_adjacent_and_overlapping_ranges() {
        let gaps = AllocationGaps {
            holes: vec![(0, 10), (20, 30)],
            unwritten: vec![(10, 15), (25, 40)],
        };
        assert_eq!(gaps.merged(), vec![(0, 15), (20, 40)]);
    }

    /// A file of `len` bytes with `data_at` written into it. On Windows the
    /// file is marked sparse first, since NTFS files are otherwise fully
    /// allocated.
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        windows
    ))]
    fn file_with_layout(data_at: &[(u64, &[u8])], len: u64) -> tempfile::NamedTempFile {
        use std::io::{Seek, SeekFrom, Write};
        let mut file = tempfile::Builder::new()
            .prefix("alloc-audit-")
            .tempfile_in(std::env::current_dir().unwrap())
            .unwrap();
        #[cfg(windows)]
        crate::win32::set_sparse(file.as_file()).unwrap();
        file.as_file().set_len(len).unwrap();
        for (offset, bytes) in data_at {
            file.as_file_mut().seek(SeekFrom::Start(*offset)).unwrap();
            file.as_file_mut().write_all(bytes).unwrap();
        }
        file.as_file().sync_all().unwrap();
        file
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        windows
    ))]
    #[test]
    fn sparse_file_holes_are_detected_when_the_filesystem_reports_them() {
        let block = vec![0xABu8; 4096];
        let file = file_with_layout(&[(0, &block), (1 << 20, &block)], 2 << 20);
        match audit_file_allocation(file.path(), 2 << 20).unwrap() {
            AllocationAudit::Unsupported => {
                eprintln!("filesystem reports no allocation map; nothing to assert");
            }
            AllocationAudit::Gaps { gaps, .. } => {
                let merged = gaps.merged();
                // Some filesystems cannot represent holes and report the file
                // as fully allocated. When they can, the middle span and tail
                // must be flagged and the written blocks must not be.
                if !merged.is_empty() {
                    assert!(
                        merged
                            .iter()
                            .any(|(s, e)| *s <= (1 << 19) && *e >= (1 << 19)),
                        "middle of the sparse file should be a gap: {merged:?}"
                    );
                    assert!(
                        merged.iter().all(|(start, _)| *start >= 4096),
                        "the first written block must not be flagged: {merged:?}"
                    );
                }
            }
        }
    }

    /// Gap arithmetic for platforms that report what *is* allocated.
    fn page(ranges: &[(i64, i64)], more: bool) -> io::Result<Option<RangePage>> {
        Ok(Some(RangePage {
            ranges: ranges.to_vec(),
            more,
        }))
    }

    #[test]
    fn allocated_range_paging_continues_from_the_last_range_until_done() {
        let mut calls = Vec::new();
        let result = collect_allocated_ranges(1000, |offset, length| {
            calls.push((offset, length));
            match calls.len() {
                1 => page(&[(0, 100), (200, 100)], true),
                2 => page(&[(400, 100)], true),
                _ => page(&[(900, 100)], false),
            }
        })
        .unwrap()
        .unwrap();
        assert_eq!(result, vec![(0, 100), (200, 300), (400, 500), (900, 1000)]);
        // Each page resumes where the previous one ended.
        assert_eq!(calls, vec![(0, 1000), (300, 700), (500, 500)]);
        let gaps = gaps_from_allocated(result, 1000);
        assert_eq!(gaps.holes, vec![(100, 200), (300, 400), (500, 900)]);
    }

    #[test]
    fn allocated_range_query_reports_unsupported_as_none() {
        assert_eq!(collect_allocated_ranges(10, |_, _| Ok(None)).unwrap(), None);
        // Unsupported part-way through discards nothing useful: still None.
        let mut first = true;
        let r = collect_allocated_ranges(1000, |_, _| {
            if std::mem::take(&mut first) {
                page(&[(0, 10)], true)
            } else {
                Ok(None)
            }
        })
        .unwrap();
        assert_eq!(r, None);
    }

    #[test]
    fn allocated_range_query_propagates_real_errors() {
        let err = collect_allocated_ranges(10, |_, _| {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn allocated_range_query_clamps_to_the_file_and_ignores_garbage() {
        let r = collect_allocated_ranges(100, |_, _| {
            page(
                &[(-5, 10), (10, 0), (10, -3), (50, 10_000), (200, 5)],
                false,
            )
        })
        .unwrap()
        .unwrap();
        assert_eq!(
            r,
            vec![(50, 100)],
            "past-the-end and malformed entries dropped"
        );
    }

    #[test]
    fn allocated_range_query_terminates_when_a_page_makes_no_progress() {
        // `more` with no new ranges, or ranges that never advance, must not spin.
        let mut calls = 0;
        let r = collect_allocated_ranges(1000, |_, _| {
            calls += 1;
            page(&[(0, 100)], true)
        })
        .unwrap()
        .unwrap();
        assert_eq!(r.len(), 2, "one real page, then a stalled repeat");
        assert!(calls <= 2, "stopped after the stall, made {calls} calls");

        let mut empty_calls = 0;
        let r = collect_allocated_ranges(1000, |_, _| {
            empty_calls += 1;
            page(&[], true)
        })
        .unwrap()
        .unwrap();
        assert!(r.is_empty());
        assert_eq!(empty_calls, 1);
    }

    #[test]
    fn allocated_range_query_over_an_empty_file_asks_nothing() {
        let r = collect_allocated_ranges(0, |_, _| panic!("no query for an empty file"))
            .unwrap()
            .unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn allocated_ranges_become_holes_between_and_after_them() {
        let gaps = gaps_from_allocated([(0, 4096), (8192, 12288)], 16384);
        assert_eq!(gaps.holes, vec![(4096, 8192), (12288, 16384)]);
        assert!(gaps.unwritten.is_empty());
        assert!(gaps_from_allocated([(0, 16384)], 16384).is_empty());
        assert_eq!(gaps_from_allocated([], 100).holes, vec![(0, 100)]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_reports_a_gap_in_a_sparse_file_or_says_it_cannot() {
        let block = vec![0xEEu8; 4096];
        let len = 4u64 << 20;
        let file = file_with_layout(&[(0, &block), (len - 4096, &block)], len);
        match audit_file_allocation(file.path(), len).unwrap() {
            AllocationAudit::Unsupported => {}
            AllocationAudit::Gaps { gaps, method } => {
                assert_eq!(method, AuditMethod::AllocatedRanges);
                let merged = gaps.merged();
                // The written head and tail must never be flagged.
                assert!(
                    merged.iter().all(|(s, e)| *s >= 4096 && *e <= len - 4096),
                    "written blocks flagged: {merged:?}"
                );
                eprintln!("windows allocated-range audit found {merged:?}");
            }
        }
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        windows
    ))]
    #[test]
    fn fully_written_file_reports_no_gaps() {
        let block = vec![0x5Au8; 1 << 16];
        let file = file_with_layout(&[(0, &block)], block.len() as u64);
        match audit_file_allocation(file.path(), block.len() as u64).unwrap() {
            AllocationAudit::Unsupported => {}
            AllocationAudit::Gaps { gaps, .. } => {
                assert!(gaps.is_empty(), "fully written file flagged: {gaps:?}");
            }
        }
    }

    /// The property the whole audit exists for: a `fallocate`d file that was
    /// never written must be flagged on filesystems that track unwritten
    /// extents (ext4, xfs, btrfs). Other filesystems only get the weaker
    /// no-panic guarantee.
    #[cfg(target_os = "linux")]
    #[test]
    fn preallocated_but_unwritten_file_is_flagged_on_extent_filesystems() {
        use std::os::fd::AsRawFd;
        let len = 4u64 << 20;
        let file = file_with_layout(&[], 0);
        let rc = unsafe { libc::fallocate(file.as_file().as_raw_fd(), 0, 0, len as libc::off_t) };
        if rc != 0 {
            eprintln!("fallocate unsupported here; skipping");
            return;
        }
        let topology = crate::device::detect_storage_topology(file.path());
        let extent_fs = matches!(
            topology.fs_type.as_deref(),
            Some("ext4") | Some("xfs") | Some("btrfs")
        );
        match audit_file_allocation(file.path(), len).unwrap() {
            AllocationAudit::Gaps {
                gaps,
                method: AuditMethod::Fiemap,
            } if extent_fs => {
                assert_eq!(
                    gaps.merged(),
                    vec![(0, len)],
                    "never-written preallocation must be entirely flagged on {:?}",
                    topology.fs_type
                );
            }
            other => eprintln!(
                "fs {:?} does not expose unwritten extents ({other:?}); nothing to assert",
                topology.fs_type
            ),
        }
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        windows
    ))]
    #[test]
    fn missing_file_is_an_io_error_not_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let err = audit_file_allocation(&dir.path().join("nope"), 10).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        windows
    )))]
    #[test]
    fn other_platforms_report_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"x").unwrap();
        assert_eq!(
            audit_file_allocation(&path, 1).unwrap(),
            AllocationAudit::Unsupported
        );
    }
}
