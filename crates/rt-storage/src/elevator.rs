use std::cmp::Ordering;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rt_path::{StorageProfile, StorageRootId};

use crate::io_class::IoClass;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileKey {
    pub storage_root: StorageRootId,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoKind {
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoOp {
    pub file_key: FileKey,
    pub offset: u64,
    pub len: u32,
    pub class: IoClass,
    pub kind: IoKind,
    pub deadline: Instant,
    pub choke_critical: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElevatorDispatch {
    pub file_key: FileKey,
    pub offset: u64,
    pub len: u32,
    pub class: IoClass,
    pub kind: IoKind,
    pub choke_critical: bool,
    pub op_sequences: Vec<u64>,
}

#[derive(Debug, Clone)]
struct QueuedOp {
    op: IoOp,
    sequence: u64,
    queued_at: Instant,
}

#[derive(Debug)]
pub struct DeviceElevator {
    device: DeviceId,
    profile: StorageProfile,
    budget: Duration,
    next_sequence: u64,
    pending: VecDeque<QueuedOp>,
}

impl DeviceElevator {
    pub fn new(device: DeviceId, profile: StorageProfile, budget: Duration) -> Self {
        let budget = if matches!(profile, StorageProfile::Ssd | StorageProfile::Nvme) {
            Duration::ZERO
        } else {
            budget
        };
        Self {
            device,
            profile,
            budget,
            next_sequence: 1,
            pending: VecDeque::new(),
        }
    }

    pub fn device(&self) -> &DeviceId {
        &self.device
    }

    pub fn profile(&self) -> &StorageProfile {
        &self.profile
    }

    pub fn budget(&self) -> Duration {
        self.budget
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn submit(&mut self, now: Instant, op: IoOp) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.pending.push_back(QueuedOp {
            op,
            sequence,
            queued_at: now,
        });
        sequence
    }

    pub fn drain_ready(&mut self, now: Instant) -> Vec<ElevatorDispatch> {
        let mut ready = Vec::new();
        let mut pending = VecDeque::with_capacity(self.pending.len());
        while let Some(queued) = self.pending.pop_front() {
            if self.is_ready(&queued, now) {
                ready.push(queued);
            } else {
                pending.push_back(queued);
            }
        }
        self.pending = pending;
        ready.sort_by(|a, b| compare_queued_ops(a, b, now));
        coalesce_ready_ops(ready)
    }

    fn is_ready(&self, queued: &QueuedOp, now: Instant) -> bool {
        queued.op.choke_critical
            || queued.op.deadline <= now
            || queued.op.class == IoClass::Foreground
            || now
                .checked_duration_since(queued.queued_at)
                .is_some_and(|age| age >= self.budget)
    }
}

fn compare_queued_ops(a: &QueuedOp, b: &QueuedOp, now: Instant) -> Ordering {
    let a_promoted = promotion_rank(a, now);
    let b_promoted = promotion_rank(b, now);
    b_promoted
        .cmp(&a_promoted)
        .then_with(|| b.op.class.cmp(&a.op.class))
        .then_with(|| compare_file_key(&a.op.file_key, &b.op.file_key))
        .then_with(|| a.op.offset.cmp(&b.op.offset))
        .then_with(|| a.sequence.cmp(&b.sequence))
}

fn compare_file_key(a: &FileKey, b: &FileKey) -> Ordering {
    a.storage_root
        .0
        .as_bytes()
        .cmp(b.storage_root.0.as_bytes())
        .then_with(|| a.path.cmp(&b.path))
}

fn promotion_rank(op: &QueuedOp, now: Instant) -> u8 {
    if op.op.choke_critical {
        3
    } else if op.op.class == IoClass::Foreground {
        2
    } else if op.op.deadline <= now {
        1
    } else {
        0
    }
}

fn coalesce_ready_ops(ready: Vec<QueuedOp>) -> Vec<ElevatorDispatch> {
    let mut out: Vec<ElevatorDispatch> = Vec::with_capacity(ready.len());
    for queued in ready {
        if let Some(last) = out.last_mut() {
            if can_merge(last, &queued.op) {
                let end = last
                    .offset
                    .saturating_add(last.len as u64)
                    .max(queued.op.offset.saturating_add(queued.op.len as u64));
                last.len = end.saturating_sub(last.offset).min(u32::MAX as u64) as u32;
                last.choke_critical |= queued.op.choke_critical;
                last.op_sequences.push(queued.sequence);
                continue;
            }
        }
        out.push(ElevatorDispatch {
            file_key: queued.op.file_key,
            offset: queued.op.offset,
            len: queued.op.len,
            class: queued.op.class,
            kind: queued.op.kind,
            choke_critical: queued.op.choke_critical,
            op_sequences: vec![queued.sequence],
        });
    }
    out
}

fn can_merge(last: &ElevatorDispatch, next: &IoOp) -> bool {
    last.kind == IoKind::Read
        && next.kind == IoKind::Read
        && last.file_key == next.file_key
        && last.class == next.class
        && next.offset <= last.offset.saturating_add(last.len as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> FileKey {
        FileKey {
            storage_root: StorageRootId::new(),
            path: PathBuf::from(path),
        }
    }

    fn op(file_key: FileKey, offset: u64, len: u32, class: IoClass, kind: IoKind) -> IoOp {
        IoOp {
            file_key,
            offset,
            len,
            class,
            kind,
            deadline: Instant::now() + Duration::from_secs(60),
            choke_critical: false,
        }
    }

    #[test]
    fn hdd_budget_holds_nonurgent_ops_until_elapsed() {
        let now = Instant::now();
        let mut elevator = DeviceElevator::new(
            DeviceId("sda".to_owned()),
            StorageProfile::Hdd,
            Duration::from_millis(10),
        );
        elevator.submit(
            now,
            op(
                file("movie.bin"),
                0,
                16 * 1024,
                IoClass::PeerRead,
                IoKind::Read,
            ),
        );

        assert!(elevator
            .drain_ready(now + Duration::from_millis(9))
            .is_empty());
        assert_eq!(
            elevator.drain_ready(now + Duration::from_millis(10)).len(),
            1
        );
    }

    #[test]
    fn nvme_degenerates_to_zero_budget() {
        let now = Instant::now();
        let mut elevator = DeviceElevator::new(
            DeviceId("nvme0n1".to_owned()),
            StorageProfile::Nvme,
            Duration::from_millis(10),
        );
        elevator.submit(
            now,
            op(
                file("movie.bin"),
                0,
                16 * 1024,
                IoClass::PeerRead,
                IoKind::Read,
            ),
        );

        assert_eq!(elevator.budget(), Duration::ZERO);
        assert_eq!(elevator.drain_ready(now).len(), 1);
    }

    #[test]
    fn ready_reads_are_offset_sorted_and_coalesced_per_file() {
        let now = Instant::now();
        let mut elevator = DeviceElevator::new(
            DeviceId("sda".to_owned()),
            StorageProfile::Hdd,
            Duration::from_millis(5),
        );
        let key = file("movie.bin");
        elevator.submit(
            now,
            op(
                key.clone(),
                32 * 1024,
                16 * 1024,
                IoClass::PeerRead,
                IoKind::Read,
            ),
        );
        elevator.submit(
            now,
            op(key.clone(), 0, 16 * 1024, IoClass::PeerRead, IoKind::Read),
        );
        elevator.submit(
            now,
            op(key, 16 * 1024, 16 * 1024, IoClass::PeerRead, IoKind::Read),
        );

        let dispatch = elevator.drain_ready(now + Duration::from_millis(5));
        assert_eq!(dispatch.len(), 1);
        assert_eq!(dispatch[0].offset, 0);
        assert_eq!(dispatch[0].len, 48 * 1024);
        assert_eq!(dispatch[0].op_sequences.len(), 3);
    }

    #[test]
    fn writes_are_ordered_but_not_coalesced() {
        let now = Instant::now();
        let mut elevator = DeviceElevator::new(
            DeviceId("sda".to_owned()),
            StorageProfile::Hdd,
            Duration::ZERO,
        );
        let key = file("piece.bin");
        elevator.submit(
            now,
            op(
                key.clone(),
                16 * 1024,
                16 * 1024,
                IoClass::PeerWrite,
                IoKind::Write,
            ),
        );
        elevator.submit(
            now,
            op(key, 0, 16 * 1024, IoClass::PeerWrite, IoKind::Write),
        );

        let dispatch = elevator.drain_ready(now);
        assert_eq!(dispatch.len(), 2);
        assert_eq!(dispatch[0].offset, 0);
        assert_eq!(dispatch[1].offset, 16 * 1024);
    }

    #[test]
    fn choke_critical_and_foreground_bypass_budget() {
        let now = Instant::now();
        let mut elevator = DeviceElevator::new(
            DeviceId("sda".to_owned()),
            StorageProfile::Hdd,
            Duration::from_millis(20),
        );
        let mut normal = op(
            file("bulk.bin"),
            0,
            16 * 1024,
            IoClass::Recheck,
            IoKind::Read,
        );
        normal.deadline = now + Duration::from_secs(60);
        elevator.submit(now, normal);

        let mut foreground = op(
            file("status.bin"),
            0,
            1024,
            IoClass::Foreground,
            IoKind::Read,
        );
        foreground.deadline = now + Duration::from_secs(60);
        elevator.submit(now, foreground);

        let mut critical = op(
            file("peer.bin"),
            0,
            16 * 1024,
            IoClass::PeerRead,
            IoKind::Read,
        );
        critical.choke_critical = true;
        critical.deadline = now + Duration::from_secs(60);
        elevator.submit(now, critical);

        let dispatch = elevator.drain_ready(now);
        assert_eq!(dispatch.len(), 2);
        assert!(dispatch[0].choke_critical);
        assert_eq!(dispatch[1].class, IoClass::Foreground);
        assert_eq!(elevator.pending_len(), 1);
    }
}
