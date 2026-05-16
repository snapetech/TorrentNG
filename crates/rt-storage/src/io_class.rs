/// Priority class for I/O scheduling.
///
/// Higher-priority classes are served ahead of lower-priority classes
/// within the same mount queue. Peer reads must never be starved by
/// bulk recheck or background copy operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IoClass {
    /// Background metadata-only reads (low priority).
    Metadata = 0,
    /// Bulk recheck reads — throttled to avoid saturating HDD queues.
    Recheck = 1,
    /// Staged move/copy operations — pauseable, lower than active seeding.
    MoveCopy = 2,
    /// Writes for incoming piece data (downloading).
    PeerWrite = 3,
    /// Reads for outgoing piece data (seeding) — must not be starved.
    PeerRead = 4,
    /// User-initiated foreground operations (immediate response expected).
    Foreground = 5,
}

impl IoClass {
    /// Maximum concurrent I/O operations for this class on an HDD mount.
    pub fn hdd_concurrency(self) -> usize {
        match self {
            IoClass::Metadata => 1,
            IoClass::Recheck => 1,
            IoClass::MoveCopy => 1,
            IoClass::PeerWrite => 2,
            IoClass::PeerRead => 4,
            IoClass::Foreground => 4,
        }
    }

    /// Maximum concurrent I/O operations for this class on an SSD/NVMe mount.
    pub fn ssd_concurrency(self) -> usize {
        match self {
            IoClass::Metadata => 2,
            IoClass::Recheck => 4,
            IoClass::MoveCopy => 4,
            IoClass::PeerWrite => 8,
            IoClass::PeerRead => 16,
            IoClass::Foreground => 16,
        }
    }
}
