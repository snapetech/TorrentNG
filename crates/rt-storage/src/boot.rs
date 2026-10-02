//! Operating-system boot identity.
//!
//! A host crash (power loss, kernel panic, hard reset) discards the page cache;
//! a crash of only the TorrentNG process does not. The two need different
//! recovery, and the only durable way to tell them apart is to remember which
//! boot the previous run belonged to and compare it with the current boot.
//!
//! Platform support:
//!
//! * **Linux** reads `/proc/sys/kernel/random/boot_id`, a random UUID generated
//!   once per boot. Containers share the host's value.
//! * **macOS and FreeBSD** read `kern.boottime` (`sysctlbyname`).
//! * **Windows** derives the boot time from `GetTickCount64` (uptime, which
//!   includes time spent asleep) and the wall clock.
//! * Other platforms report `None`. Callers must treat an unknown identity as
//!   "cannot rule out a host crash" and fall back to the conservative
//!   unclean-restart policy; nothing here may fail startup.
//!
//! The last three platforms have no opaque boot id, only a boot *time*. That is
//! stored as `bt:<unix seconds>` and compared with a small tolerance
//! ([`BOOT_TIME_TOLERANCE_SECS`]) because a clock step within one boot moves it.
//! A step larger than the tolerance is read as a reboot, which is the safe
//! direction: it can only cause extra rechecking after an unclean shutdown.

use std::io::Read;
use std::path::Path;

/// Longest identity string accepted from the OS or from a persisted record.
pub const MAX_BOOT_IDENTITY_BYTES: usize = 128;

/// Prefix of a boot identity that is a boot *time* rather than an opaque id.
const BOOT_TIME_PREFIX: &str = "bt:";

/// Two boot times closer than this are the same boot. A reboot cannot complete
/// this quickly, while ordinary clock slew stays well under it.
pub const BOOT_TIME_TOLERANCE_SECS: u64 = 15;

/// The identity of the current boot, or `None` when the platform cannot
/// provide one.
pub fn current_boot_identity() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        read_boot_identity_from(Path::new("/proc/sys/kernel/random/boot_id"))
    }
    #[cfg(any(target_os = "macos", target_os = "freebsd", windows))]
    {
        platform_boot_time_secs().map(format_boot_time)
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        windows
    )))]
    {
        None
    }
}

#[cfg_attr(
    not(any(target_os = "macos", target_os = "freebsd", windows, test)),
    allow(dead_code)
)]
fn format_boot_time(secs: u64) -> String {
    format!("{BOOT_TIME_PREFIX}{secs}")
}

/// Parse a `bt:<seconds>` identity.
fn parse_boot_time(identity: &str) -> Option<u64> {
    identity.strip_prefix(BOOT_TIME_PREFIX)?.parse().ok()
}

/// Boot time from the current wall clock and an uptime. `None` for an uptime
/// longer than the clock allows.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn boot_time_from_uptime(now_unix_secs: u64, uptime_millis: u64) -> Option<u64> {
    now_unix_secs.checked_sub(uptime_millis / 1000)
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn platform_boot_time_secs() -> Option<u64> {
    let name = std::ffi::CString::new("kern.boottime").ok()?;
    // SAFETY: `timeval` is plain old data; zero is a valid bit pattern.
    let mut boot: libc::timeval = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::timeval>();
    // SAFETY: `name` is NUL terminated, `boot`/`len` describe a live buffer of
    // exactly `len` bytes, and no new value is being set.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&mut boot as *mut libc::timeval).cast::<libc::c_void>(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && len >= std::mem::size_of::<libc::timeval>() && boot.tv_sec > 0)
        .then_some(boot.tv_sec as u64)
}

#[cfg(windows)]
fn platform_boot_time_secs() -> Option<u64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    boot_time_from_uptime(now, crate::win32::uptime_millis())
}

/// Read and validate an identity file. Exposed so tests and alternative
/// platform back ends share one bounded parser.
pub fn read_boot_identity_from(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_BOOT_IDENTITY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let text = String::from_utf8(bytes).ok()?;
    normalize_boot_identity(&text)
}

/// Trim and validate a candidate identity. Rejects empty, oversized and
/// non-printable values so a corrupt marker cannot masquerade as a boot.
pub fn normalize_boot_identity(candidate: &str) -> Option<String> {
    let trimmed = candidate.trim();
    if trimmed.is_empty()
        || trimmed.len() > MAX_BOOT_IDENTITY_BYTES
        || !trimmed.chars().all(|c| c.is_ascii_graphic())
    {
        return None;
    }
    Some(trimmed.to_owned())
}

/// Whether two identities name different boots.
///
/// `None` means "cannot tell" (either side unknown) and is never collapsed
/// into `Some(false)`: an unknown previous boot must not be trusted as the
/// same boot.
pub fn boot_changed(previous: Option<&str>, current: Option<&str>) -> Option<bool> {
    match (previous, current) {
        (Some(previous), Some(current)) => Some(
            match (parse_boot_time(previous), parse_boot_time(current)) {
                (Some(before), Some(now)) => before.abs_diff(now) > BOOT_TIME_TOLERANCE_SECS,
                _ => previous != current,
            },
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_parser_trims_and_rejects_garbage() {
        assert_eq!(
            normalize_boot_identity("  6f1c2a9e-1111-4222-8333-444455556666\n").as_deref(),
            Some("6f1c2a9e-1111-4222-8333-444455556666")
        );
        assert_eq!(normalize_boot_identity(""), None);
        assert_eq!(normalize_boot_identity("   \n"), None);
        assert_eq!(normalize_boot_identity("has space inside"), None);
        assert_eq!(normalize_boot_identity("ctl\u{7}char"), None);
        assert_eq!(
            normalize_boot_identity(&"a".repeat(MAX_BOOT_IDENTITY_BYTES + 1)),
            None
        );
        assert!(normalize_boot_identity(&"a".repeat(MAX_BOOT_IDENTITY_BYTES)).is_some());
    }

    #[test]
    fn identity_file_read_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boot_id");
        std::fs::write(&path, "abc-123\n").unwrap();
        assert_eq!(read_boot_identity_from(&path).as_deref(), Some("abc-123"));

        std::fs::write(&path, "a".repeat(64 * 1024)).unwrap();
        assert_eq!(read_boot_identity_from(&path), None);

        assert_eq!(read_boot_identity_from(&dir.path().join("missing")), None);
    }

    #[test]
    fn boot_change_is_tri_state() {
        assert_eq!(boot_changed(Some("a"), Some("a")), Some(false));
        assert_eq!(boot_changed(Some("a"), Some("b")), Some(true));
        assert_eq!(boot_changed(None, Some("b")), None);
        assert_eq!(boot_changed(Some("a"), None), None);
        assert_eq!(boot_changed(None, None), None);
    }

    #[test]
    fn boot_times_within_the_tolerance_are_the_same_boot() {
        let a = format_boot_time(1_700_000_000);
        let close = format_boot_time(1_700_000_000 + BOOT_TIME_TOLERANCE_SECS);
        let far = format_boot_time(1_700_000_000 + BOOT_TIME_TOLERANCE_SECS + 1);
        assert_eq!(boot_changed(Some(&a), Some(&close)), Some(false));
        assert_eq!(boot_changed(Some(&close), Some(&a)), Some(false));
        assert_eq!(boot_changed(Some(&a), Some(&far)), Some(true));
        // A machine that boots minutes later is a different boot.
        let later = format_boot_time(1_700_000_000 + 180);
        assert_eq!(boot_changed(Some(&a), Some(&later)), Some(true));
    }

    #[test]
    fn mixed_or_opaque_identities_compare_exactly() {
        let a = format_boot_time(1_700_000_000);
        // An opaque id never matches a boot time, and equal opaque ids match.
        assert_eq!(boot_changed(Some("uuid-1"), Some(&a)), Some(true));
        assert_eq!(boot_changed(Some("uuid-1"), Some("uuid-1")), Some(false));
        assert_eq!(boot_changed(Some("uuid-1"), Some("uuid-2")), Some(true));
    }

    #[test]
    fn boot_time_identity_round_trips_and_validates() {
        let id = format_boot_time(1_700_000_123);
        assert_eq!(id, "bt:1700000123");
        assert_eq!(parse_boot_time(&id), Some(1_700_000_123));
        assert_eq!(
            normalize_boot_identity(&id).as_deref(),
            Some("bt:1700000123")
        );
        assert_eq!(parse_boot_time("bt:notanumber"), None);
        assert_eq!(parse_boot_time("uuid"), None);
    }

    #[test]
    fn boot_time_is_wall_clock_minus_uptime() {
        assert_eq!(boot_time_from_uptime(1_000_000, 3_600_000), Some(996_400));
        assert_eq!(boot_time_from_uptime(1_000_000, 999), Some(1_000_000));
        assert_eq!(
            boot_time_from_uptime(10, 60_000_000),
            None,
            "uptime beyond the clock"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "freebsd", windows))]
    #[test]
    fn time_based_platforms_report_a_plausible_stable_identity() {
        let first = current_boot_identity().expect("platform should report a boot time");
        let boot = parse_boot_time(&first).expect("bt: identity");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(boot > 946_684_800, "boot time after the year 2000: {boot}");
        assert!(
            boot <= now + BOOT_TIME_TOLERANCE_SECS,
            "boot time not in the future"
        );
        assert_eq!(
            boot_changed(Some(&first), current_boot_identity().as_deref()),
            Some(false)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_reports_a_stable_identity_within_one_boot() {
        let first = current_boot_identity();
        assert!(first.is_some(), "/proc boot_id should exist on Linux");
        assert_eq!(first, current_boot_identity());
    }
}
