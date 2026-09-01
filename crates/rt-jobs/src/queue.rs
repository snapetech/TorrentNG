use std::collections::HashMap;

use crate::{
    error::JobError,
    job::{Job, JobId, JobKind},
};

/// In-memory job registry.
///
/// Phase 2: in-memory only. Phase 5 (daemon + DB) persists jobs to SQLite.
#[derive(Debug, Default)]
pub struct JobQueue {
    jobs: HashMap<JobId, Job>,
}

impl JobQueue {
    pub fn new() -> Self {
        JobQueue::default()
    }

    /// Enqueue a new job. Returns the job ID.
    pub fn enqueue(&mut self, job: Job) -> JobId {
        let id = job.id.clone();
        tracing::info!(
            component = "jobs",
            operation = "enqueue",
            job_id = %id,
            kind = ?job.kind,
            dry_run = job.dry_run,
            result = "ok",
            "job enqueued"
        );
        self.jobs.insert(id.clone(), job);
        id
    }

    pub fn get(&self, id: &JobId) -> Option<&Job> {
        self.jobs.get(id)
    }

    pub fn get_mut(&mut self, id: &JobId) -> Option<&mut Job> {
        self.jobs.get_mut(id)
    }

    /// Cancel a job by ID.
    pub fn cancel(&mut self, id: &JobId) -> Result<(), JobError> {
        let job = self
            .jobs
            .get_mut(id)
            .ok_or_else(|| JobError::NotFound(id.to_string()))?;

        if job.state.is_terminal() {
            return Err(JobError::AlreadyTerminal(
                id.to_string(),
                job.state.to_string(),
            ));
        }
        if !job.kind.is_cancellable() {
            return Err(JobError::NotCancellable(
                id.to_string(),
                job.state.to_string(),
            ));
        }
        job.cancel();
        tracing::info!(
            component = "jobs",
            operation = "cancel",
            job_id = %id,
            result = "ok",
            "job cancelled"
        );
        Ok(())
    }

    /// All active (non-terminal) jobs.
    pub fn active_jobs(&self) -> Vec<&Job> {
        self.jobs.values().filter(|j| j.state.is_active()).collect()
    }

    /// All jobs, sorted by created_at descending.
    pub fn all_jobs(&self) -> Vec<&Job> {
        let mut jobs: Vec<&Job> = self.jobs.values().collect();
        jobs.sort_by_key(|job| std::cmp::Reverse(job.created_at));
        jobs
    }

    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Validate that a destructive job has a matching dry-run result before enqueuing.
    ///
    /// Returns error if kind requires dry-run and this is not the dry-run job.
    pub fn validate_destructive(kind: &JobKind, dry_run: bool) -> Result<(), JobError> {
        if kind.requires_dry_run_first() && !dry_run {
            return Err(JobError::DryRunRequired(format!("{kind:?}")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{Job, JobKind, JobState};

    fn recheck_job() -> Job {
        Job::new(JobKind::RecheckTorrent, vec!["abc".into()], false, 100)
    }

    fn move_job_dry() -> Job {
        Job::new(JobKind::MoveTorrent, vec!["abc".into()], true, 10)
    }

    #[test]
    fn enqueue_and_retrieve() {
        let mut q = JobQueue::new();
        let job = recheck_job();
        let id = q.enqueue(job);
        assert!(q.get(&id).is_some());
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn cancel_active_job() {
        let mut q = JobQueue::new();
        let mut job = recheck_job();
        job.start();
        let id = q.enqueue(job);
        q.cancel(&id).unwrap();
        assert_eq!(q.get(&id).unwrap().state, JobState::Cancelled);
    }

    #[test]
    fn cancel_already_terminal() {
        let mut q = JobQueue::new();
        let mut job = recheck_job();
        job.complete();
        let id = q.enqueue(job);
        assert!(matches!(
            q.cancel(&id),
            Err(JobError::AlreadyTerminal(_, _))
        ));
    }

    #[test]
    fn cancel_not_found() {
        let mut q = JobQueue::new();
        let fake_id = JobId::new();
        assert!(matches!(q.cancel(&fake_id), Err(JobError::NotFound(_))));
    }

    #[test]
    fn active_jobs_excludes_terminal() {
        let mut q = JobQueue::new();
        let mut j1 = recheck_job();
        j1.start();
        let id1 = q.enqueue(j1);

        let mut j2 = recheck_job();
        j2.complete();
        q.enqueue(j2);

        let active = q.active_jobs();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, id1);
    }

    #[test]
    fn destructive_requires_dry_run() {
        let result = JobQueue::validate_destructive(&JobKind::DeleteTorrent, false);
        assert!(matches!(result, Err(JobError::DryRunRequired(_))));

        let ok = JobQueue::validate_destructive(&JobKind::DeleteTorrent, true);
        assert!(ok.is_ok());

        let recheck_ok = JobQueue::validate_destructive(&JobKind::RecheckTorrent, false);
        assert!(recheck_ok.is_ok());
    }

    #[test]
    fn dry_run_job_enqueued_ok() {
        let mut q = JobQueue::new();
        let dry_job = move_job_dry();
        let id = q.enqueue(dry_job);
        assert!(q.get(&id).unwrap().dry_run);
    }
}
