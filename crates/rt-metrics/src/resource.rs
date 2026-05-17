use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

pub const MEMORY_CLASS_COUNT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum MemoryClass {
    StorageFrame = 0,
    PieceAssembly = 1,
    PeerBuffer = 2,
    WebseedBody = 3,
    Metadata = 4,
    TrackerPeers = 5,
    DhtTable = 6,
    ApiSnapshot = 7,
}

impl MemoryClass {
    pub const ALL: [MemoryClass; MEMORY_CLASS_COUNT] = [
        MemoryClass::StorageFrame,
        MemoryClass::PieceAssembly,
        MemoryClass::PeerBuffer,
        MemoryClass::WebseedBody,
        MemoryClass::Metadata,
        MemoryClass::TrackerPeers,
        MemoryClass::DhtTable,
        MemoryClass::ApiSnapshot,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            MemoryClass::StorageFrame => "storage_frame",
            MemoryClass::PieceAssembly => "piece_assembly",
            MemoryClass::PeerBuffer => "peer_buffer",
            MemoryClass::WebseedBody => "webseed_body",
            MemoryClass::Metadata => "metadata",
            MemoryClass::TrackerPeers => "tracker_peers",
            MemoryClass::DhtTable => "dht_table",
            MemoryClass::ApiSnapshot => "api_snapshot",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryPressure {
    Normal,
    Constrained,
    Critical,
}

impl MemoryPressure {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryPressure::Normal => "normal",
            MemoryPressure::Constrained => "constrained",
            MemoryPressure::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceGovernorConfig {
    pub total_cap_bytes: u64,
    pub class_caps_bytes: [u64; MEMORY_CLASS_COUNT],
    pub pressure_constrained_pct: u8,
    pub pressure_critical_pct: u8,
}

impl Default for ResourceGovernorConfig {
    fn default() -> Self {
        let mib = 1024 * 1024;
        Self {
            total_cap_bytes: 512 * mib,
            class_caps_bytes: [
                128 * mib,
                128 * mib,
                128 * mib,
                32 * mib,
                32 * mib,
                32 * mib,
                32 * mib,
                16 * mib,
            ],
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        }
    }
}

#[derive(Debug)]
struct ResourceGovernorInner {
    config: ResourceGovernorConfig,
    total_used: AtomicU64,
    class_used: [AtomicU64; MEMORY_CLASS_COUNT],
    denied: [AtomicU64; MEMORY_CLASS_COUNT],
}

#[derive(Debug, Clone)]
pub struct ResourceGovernor {
    inner: Arc<ResourceGovernorInner>,
}

impl ResourceGovernor {
    pub fn new(config: ResourceGovernorConfig) -> Self {
        Self {
            inner: Arc::new(ResourceGovernorInner {
                config,
                total_used: AtomicU64::new(0),
                class_used: std::array::from_fn(|_| AtomicU64::new(0)),
                denied: std::array::from_fn(|_| AtomicU64::new(0)),
            }),
        }
    }

    pub fn try_acquire(&self, class: MemoryClass, bytes: u64) -> Option<MemoryLease> {
        if bytes == 0 {
            return Some(MemoryLease {
                governor: self.clone(),
                class,
                bytes,
            });
        }

        let class_idx = class.index();
        let class_cap = self.inner.config.class_caps_bytes[class_idx];
        if class_cap == 0 || bytes > class_cap || bytes > self.inner.config.total_cap_bytes {
            self.inner.denied[class_idx].fetch_add(1, Ordering::Relaxed);
            return None;
        }

        if !reserve(&self.inner.class_used[class_idx], bytes, class_cap) {
            self.inner.denied[class_idx].fetch_add(1, Ordering::Relaxed);
            return None;
        }

        if !reserve(
            &self.inner.total_used,
            bytes,
            self.inner.config.total_cap_bytes,
        ) {
            self.inner.class_used[class_idx].fetch_sub(bytes, Ordering::AcqRel);
            self.inner.denied[class_idx].fetch_add(1, Ordering::Relaxed);
            return None;
        }

        Some(MemoryLease {
            governor: self.clone(),
            class,
            bytes,
        })
    }

    pub fn pressure(&self) -> MemoryPressure {
        pressure_for(
            self.inner.total_used.load(Ordering::Relaxed),
            self.inner.config.total_cap_bytes,
            self.inner.config.pressure_constrained_pct,
            self.inner.config.pressure_critical_pct,
        )
    }

    pub fn snapshot(&self) -> ResourceSnapshot {
        ResourceSnapshot {
            total_cap_bytes: self.inner.config.total_cap_bytes,
            total_used_bytes: self.inner.total_used.load(Ordering::Relaxed),
            pressure: self.pressure(),
            classes: std::array::from_fn(|idx| {
                let class = MemoryClass::ALL[idx];
                MemoryClassSnapshot {
                    class,
                    cap_bytes: self.inner.config.class_caps_bytes[idx],
                    used_bytes: self.inner.class_used[idx].load(Ordering::Relaxed),
                    denied_allocations: self.inner.denied[idx].load(Ordering::Relaxed),
                }
            }),
        }
    }

    fn release(&self, class: MemoryClass, bytes: u64) {
        if bytes == 0 {
            return;
        }
        self.inner.class_used[class.index()].fetch_sub(bytes, Ordering::AcqRel);
        self.inner.total_used.fetch_sub(bytes, Ordering::AcqRel);
    }
}

fn reserve(counter: &AtomicU64, bytes: u64, cap: u64) -> bool {
    loop {
        let current = counter.load(Ordering::Acquire);
        let Some(next) = current.checked_add(bytes) else {
            return false;
        };
        if next > cap {
            return false;
        }
        if counter
            .compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return true;
        }
    }
}

fn pressure_for(used: u64, cap: u64, constrained_pct: u8, critical_pct: u8) -> MemoryPressure {
    if cap == 0 {
        return MemoryPressure::Critical;
    }
    let used_pct = used.saturating_mul(100) / cap;
    if used_pct >= critical_pct as u64 {
        MemoryPressure::Critical
    } else if used_pct >= constrained_pct as u64 {
        MemoryPressure::Constrained
    } else {
        MemoryPressure::Normal
    }
}

#[derive(Debug)]
pub struct MemoryLease {
    governor: ResourceGovernor,
    class: MemoryClass,
    bytes: u64,
}

impl MemoryLease {
    pub fn class(&self) -> MemoryClass {
        self.class
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for MemoryLease {
    fn drop(&mut self) {
        self.governor.release(self.class, self.bytes);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryClassSnapshot {
    pub class: MemoryClass,
    pub cap_bytes: u64,
    pub used_bytes: u64,
    pub denied_allocations: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceSnapshot {
    pub total_cap_bytes: u64,
    pub total_used_bytes: u64,
    pub pressure: MemoryPressure,
    pub classes: [MemoryClassSnapshot; MEMORY_CLASS_COUNT],
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ResourceGovernorConfig {
        ResourceGovernorConfig {
            total_cap_bytes: 100,
            class_caps_bytes: [50, 80, 80, 80, 80, 80, 80, 80],
            pressure_constrained_pct: 60,
            pressure_critical_pct: 90,
        }
    }

    #[test]
    fn leases_release_on_drop() {
        let governor = ResourceGovernor::new(config());
        let lease = governor.try_acquire(MemoryClass::StorageFrame, 40).unwrap();
        assert_eq!(lease.bytes(), 40);
        assert_eq!(governor.snapshot().total_used_bytes, 40);
        drop(lease);
        assert_eq!(governor.snapshot().total_used_bytes, 0);
    }

    #[test]
    fn class_and_global_caps_are_enforced() {
        let governor = ResourceGovernor::new(config());
        let _a = governor.try_acquire(MemoryClass::StorageFrame, 40).unwrap();
        assert!(governor
            .try_acquire(MemoryClass::StorageFrame, 11)
            .is_none());
        let _b = governor.try_acquire(MemoryClass::PeerBuffer, 60).unwrap();
        assert!(governor.try_acquire(MemoryClass::Metadata, 1).is_none());
        let snap = governor.snapshot();
        assert_eq!(
            snap.classes[MemoryClass::StorageFrame.index()].denied_allocations,
            1
        );
        assert_eq!(
            snap.classes[MemoryClass::Metadata.index()].denied_allocations,
            1
        );
    }

    #[test]
    fn pressure_transitions_are_deterministic() {
        let governor = ResourceGovernor::new(config());
        assert_eq!(governor.pressure(), MemoryPressure::Normal);
        let constrained = governor.try_acquire(MemoryClass::PeerBuffer, 60).unwrap();
        assert_eq!(governor.pressure(), MemoryPressure::Constrained);
        drop(constrained);
        let _critical = governor.try_acquire(MemoryClass::PeerBuffer, 80).unwrap();
        let _more = governor.try_acquire(MemoryClass::StorageFrame, 10).unwrap();
        assert_eq!(governor.pressure(), MemoryPressure::Critical);
    }
}
