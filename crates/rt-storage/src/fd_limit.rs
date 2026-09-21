//! Process file-descriptor limit management.
//!
//! Storage NG bounds each cache's live file descriptors and also shares one
//! managed-storage lease quota across caches, derived from a fraction of the
//! process `RLIMIT_NOFILE`. Unrelated process descriptors remain outside this
//! quota, so it is a storage budget rather than a guarantee against all
//! process-wide exhaustion. At first use we raise the soft limit toward the
//! hard limit.

use once_cell::sync::Lazy;

/// Hard ceiling aligned with `rt-config`'s validation of `file_pool_size`.
/// This also keeps RLIM_INFINITY from turning the cache into an effectively
/// unbounded descriptor-retention policy.
const MAX_HANDLE_CACHE_CAPACITY: usize = 65_536;

/// Fraction of the soft fd limit the handle cache may consume. The
/// remainder is reserved for sockets (peers, trackers, DHT) and misc fds.
const HANDLE_CACHE_FRACTION_NUMERATOR: u64 = 3;
const HANDLE_CACHE_FRACTION_DENOMINATOR: u64 = 5;

static PROCESS_HANDLE_CACHE_CAPACITY: Lazy<usize> =
    Lazy::new(|| handle_cache_capacity(raise_nofile_limit()));

/// Per-cache descriptor ceiling, initialized once after best-effort soft-limit
/// raising. Storage cache implementations clamp their configured capacity to
/// this value; cache-plus-in-flight permits also draw from the shared managed
/// storage descriptor quota.
pub fn process_handle_cache_capacity() -> usize {
    *PROCESS_HANDLE_CACHE_CAPACITY
}

/// Current use of the shared managed-storage descriptor lease quota.
///
/// This does not count sockets or other descriptors opened outside `rt-storage`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessDescriptorBudgetStats {
    pub active_leases: usize,
    pub capacity: usize,
    /// Admission attempts that encountered the process-wide storage quota.
    pub admission_waits_total: u64,
}

pub fn process_descriptor_budget_stats() -> ProcessDescriptorBudgetStats {
    let (active_leases, capacity, admission_waits_total) =
        crate::file_handle::process_descriptor_budget_stats();
    ProcessDescriptorBudgetStats {
        active_leases,
        capacity,
        admission_waits_total,
    }
}

/// Raise the soft `RLIMIT_NOFILE` to the hard limit and return the soft
/// limit now in effect. Best-effort: on any failure the current soft limit
/// (or a conservative fallback) is returned and the daemon continues.
pub fn raise_nofile_limit() -> u64 {
    #[cfg(unix)]
    {
        // SAFETY: `getrlimit`/`setrlimit` with `RLIMIT_NOFILE` and a
        // properly initialised `rlimit` are well-defined POSIX calls; we
        // only read/write the local `rlim` struct.
        unsafe {
            let mut rlim = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) != 0 {
                tracing::warn!(
                    component = "storage",
                    operation = "get_nofile_limit",
                    result = "fallback",
                    fallback_soft = 1024_u64,
                    "getrlimit(RLIMIT_NOFILE) failed; using fallback fd budget"
                );
                return 1024;
            }
            if rlim.rlim_cur < rlim.rlim_max {
                let desired = libc::rlimit {
                    rlim_cur: rlim.rlim_max,
                    rlim_max: rlim.rlim_max,
                };
                if libc::setrlimit(libc::RLIMIT_NOFILE, &desired) == 0 {
                    rlim.rlim_cur = rlim.rlim_max;
                } else {
                    tracing::warn!(
                        component = "storage",
                        operation = "set_nofile_limit",
                        result = "error",
                        soft = rlim.rlim_cur,
                        hard = rlim.rlim_max,
                        "setrlimit(RLIMIT_NOFILE) failed; keeping soft limit"
                    );
                }
            }
            soft_limit_to_u64(rlim.rlim_cur)
        }
    }
    #[cfg(not(unix))]
    {
        1024
    }
}

#[cfg(unix)]
fn soft_limit_to_u64(limit: libc::rlim_t) -> u64 {
    // `rlim_t` is unsigned on Linux and signed on some BSD targets. On a
    // signed target, a negative RLIM_INFINITY sentinel must remain a very
    // large limit rather than wrapping to a small cache budget.
    u64::try_from(limit as i128).unwrap_or(u64::MAX)
}

/// Compute the handle-cache capacity (in open fds) from a soft fd limit.
pub fn handle_cache_capacity(soft_nofile: u64) -> usize {
    // Divide before multiplying so even RLIM_INFINITY's u64 representation
    // cannot overflow. A small soft limit must reduce the cache too; flooring
    // it to 64 can reserve more descriptors than the process is allowed to
    // open in total.
    let budget = (soft_nofile / HANDLE_CACHE_FRACTION_DENOMINATOR)
        .saturating_mul(HANDLE_CACHE_FRACTION_NUMERATOR)
        .saturating_add(
            (soft_nofile % HANDLE_CACHE_FRACTION_DENOMINATOR) * HANDLE_CACHE_FRACTION_NUMERATOR
                / HANDLE_CACHE_FRACTION_DENOMINATOR,
        );
    usize::try_from(budget)
        .unwrap_or(usize::MAX)
        .min(MAX_HANDLE_CACHE_CAPACITY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_respects_low_fd_limits_and_reserved_fraction() {
        assert_eq!(handle_cache_capacity(0), 0);
        assert_eq!(handle_cache_capacity(10), 6);
        assert_eq!(handle_cache_capacity(32), 19);
        assert_eq!(handle_cache_capacity(64), 38);
    }

    #[test]
    fn capacity_scales_with_limit() {
        // 60% of 100_000 = 60_000
        assert_eq!(handle_cache_capacity(100_000), 60_000);
        assert!(handle_cache_capacity(1_048_576) > handle_cache_capacity(65_536));
    }

    #[test]
    fn unlimited_limit_still_has_a_finite_cache_ceiling() {
        assert_eq!(handle_cache_capacity(u64::MAX), MAX_HANDLE_CACHE_CAPACITY);
    }

    #[test]
    fn raise_returns_nonzero() {
        // Best-effort; must always return a usable positive budget.
        assert!(raise_nofile_limit() >= 1);
    }

    #[cfg(unix)]
    #[test]
    fn soft_limit_conversion_handles_platform_rlim_t_width() {
        assert_eq!(soft_limit_to_u64(0), 0);
        assert_eq!(soft_limit_to_u64(1), 1);
    }
}
