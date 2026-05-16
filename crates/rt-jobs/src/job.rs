use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for a job.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct JobId(pub Uuid);

impl JobId {
    pub fn new() -> Self {
        JobId(Uuid::new_v4())
    }
}

impl Default for JobId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The type of long-running operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobKind {
    ImportTorrent,
    VerifyTorrent,
    RecheckTorrent,
    MoveTorrent,
    CopyTorrent,
    DeleteTorrent,
    BulkSetCategory,
    BulkSetTags,
    BulkEditTrackers,
    BulkReannounce,
    BulkPause,
    BulkResume,
}

impl JobKind {
    /// True if this job type requires explicit dry-run approval before execution.
    pub fn requires_dry_run_first(&self) -> bool {
        matches!(
            self,
            JobKind::MoveTorrent
                | JobKind::CopyTorrent
                | JobKind::DeleteTorrent
                | JobKind::BulkEditTrackers
        )
    }

    /// True if this job can be cancelled mid-execution.
    pub fn is_cancellable(&self) -> bool {
        matches!(
            self,
            JobKind::RecheckTorrent
                | JobKind::VerifyTorrent
                | JobKind::MoveTorrent
                | JobKind::CopyTorrent
                | JobKind::BulkReannounce
                | JobKind::BulkPause
                | JobKind::BulkResume
                | JobKind::BulkSetCategory
                | JobKind::BulkSetTags
                | JobKind::BulkEditTrackers
        )
    }
}

/// Lifecycle state of a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobState {
    Queued,
    Running,
    Paused,
    Cancelling,
    Cancelled,
    Completed,
    Failed,
}

impl JobState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobState::Cancelled | JobState::Completed | JobState::Failed
        )
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self,
            JobState::Queued | JobState::Running | JobState::Paused
        )
    }
}

impl std::fmt::Display for JobState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Paused => "paused",
            JobState::Cancelling => "cancelling",
            JobState::Cancelled => "cancelled",
            JobState::Completed => "completed",
            JobState::Failed => "failed",
        };
        write!(f, "{s}")
    }
}

/// Progress tracking for a job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobProgress {
    /// Total items to process (pieces, files, or torrents depending on kind).
    pub total: u64,
    /// Items processed so far.
    pub done: u64,
    /// Last resumable checkpoint (e.g., piece index for recheck).
    pub checkpoint: u64,
}

impl JobProgress {
    pub fn new(total: u64) -> Self {
        JobProgress {
            total,
            done: 0,
            checkpoint: 0,
        }
    }

    pub fn fraction(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.done as f64 / self.total as f64
        }
    }

    pub fn advance(&mut self, by: u64) {
        self.done = (self.done + by).min(self.total);
        self.checkpoint = self.done;
    }
}

/// A long-running operation tracked by the job queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub kind: JobKind,
    pub state: JobState,
    /// IDs of torrents this job operates on (hex infohashes).
    pub affected_torrents: Vec<String>,
    pub progress: JobProgress,
    /// If true, this is a simulation run — no changes are committed.
    pub dry_run: bool,
    pub error: Option<String>,
    /// Unix timestamp seconds.
    pub created_at: u64,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
}

impl Job {
    pub fn new(kind: JobKind, affected_torrents: Vec<String>, dry_run: bool, total: u64) -> Self {
        Job {
            id: JobId::new(),
            kind,
            state: JobState::Queued,
            affected_torrents,
            progress: JobProgress::new(total),
            dry_run,
            error: None,
            created_at: unix_now(),
            started_at: None,
            finished_at: None,
        }
    }

    pub fn start(&mut self) {
        self.state = JobState::Running;
        self.started_at = Some(unix_now());
    }

    pub fn complete(&mut self) {
        self.state = JobState::Completed;
        self.finished_at = Some(unix_now());
    }

    pub fn fail(&mut self, reason: impl Into<String>) {
        self.state = JobState::Failed;
        self.error = Some(reason.into());
        self.finished_at = Some(unix_now());
    }

    pub fn cancel(&mut self) {
        self.state = JobState::Cancelled;
        self.finished_at = Some(unix_now());
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_lifecycle() {
        let mut job = Job::new(JobKind::RecheckTorrent, vec!["abc".into()], false, 100);
        assert_eq!(job.state, JobState::Queued);
        assert!(!job.state.is_terminal());

        job.start();
        assert_eq!(job.state, JobState::Running);

        job.progress.advance(50);
        assert!((job.progress.fraction() - 0.5).abs() < f64::EPSILON);

        job.complete();
        assert_eq!(job.state, JobState::Completed);
        assert!(job.state.is_terminal());
        assert!(job.finished_at.is_some());
    }

    #[test]
    fn job_failure() {
        let mut job = Job::new(JobKind::MoveTorrent, vec![], false, 10);
        job.start();
        job.fail("disk full");
        assert_eq!(job.state, JobState::Failed);
        assert_eq!(job.error.as_deref(), Some("disk full"));
    }

    #[test]
    fn job_cancel() {
        let mut job = Job::new(JobKind::BulkPause, vec![], false, 5);
        job.start();
        job.cancel();
        assert_eq!(job.state, JobState::Cancelled);
        assert!(job.state.is_terminal());
    }

    #[test]
    fn dry_run_kinds() {
        assert!(JobKind::MoveTorrent.requires_dry_run_first());
        assert!(JobKind::DeleteTorrent.requires_dry_run_first());
        assert!(!JobKind::RecheckTorrent.requires_dry_run_first());
        assert!(!JobKind::BulkPause.requires_dry_run_first());
    }

    #[test]
    fn cancellable_kinds() {
        assert!(JobKind::RecheckTorrent.is_cancellable());
        assert!(JobKind::MoveTorrent.is_cancellable());
        assert!(!JobKind::ImportTorrent.is_cancellable());
    }

    #[test]
    fn progress_fraction() {
        let mut p = JobProgress::new(200);
        assert!((p.fraction() - 0.0).abs() < f64::EPSILON);
        p.advance(100);
        assert!((p.fraction() - 0.5).abs() < f64::EPSILON);
        p.advance(200); // clamps to total
        assert!((p.fraction() - 1.0).abs() < f64::EPSILON);
        assert_eq!(p.done, 200);
    }

    #[test]
    fn zero_total_fraction() {
        let p = JobProgress::new(0);
        assert!((p.fraction() - 0.0).abs() < f64::EPSILON);
    }
}
