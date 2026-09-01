//! Process file-descriptor limit management.
//!
//! Storage NG bounds the open-handle cache to a fraction of the process
//! `RLIMIT_NOFILE` so that "too many open files" — rTorrent's classic
//! failure at scale — is structurally impossible regardless of torrent
//! count. At startup we raise the soft limit toward the hard limit.

/// Fraction of the usable fd budget the handle cache may consume. The
/// remainder is reserved for sockets (peers, trackers, DHT) and misc fds.
const HANDLE_CACHE_FRACTION: f64 = 0.6;

/// Absolute floor for the handle-cache capacity, used when the rlimit is
/// unexpectedly small or cannot be queried.
const MIN_HANDLE_CACHE: usize = 64;

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
            rlim.rlim_cur
        }
    }
    #[cfg(not(unix))]
    {
        1024
    }
}

/// Compute the handle-cache capacity (in open fds) from a soft fd limit.
pub fn handle_cache_capacity(soft_nofile: u64) -> usize {
    let budget = (soft_nofile as f64 * HANDLE_CACHE_FRACTION) as usize;
    budget.max(MIN_HANDLE_CACHE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_respects_floor() {
        assert_eq!(handle_cache_capacity(10), MIN_HANDLE_CACHE);
    }

    #[test]
    fn capacity_scales_with_limit() {
        // 60% of 100_000 = 60_000
        assert_eq!(handle_cache_capacity(100_000), 60_000);
        assert!(handle_cache_capacity(1_048_576) > handle_cache_capacity(65_536));
    }

    #[test]
    fn raise_returns_nonzero() {
        // Best-effort; must always return a usable positive budget.
        assert!(raise_nofile_limit() >= 1);
    }
}
