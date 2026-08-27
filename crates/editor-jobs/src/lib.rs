//! Bounded, in-memory orchestration contracts for long-running editor work.
//!
//! This crate owns job lifecycle state only. It does not spawn processes, touch
//! files, or know about Tauri, React, or a media provider.

use editor_domain::RelativePath;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
};

pub const DEFAULT_MAX_JOBS: usize = 128;
pub const DEFAULT_MAX_RUNNING_JOBS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(u64);

impl JobId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Probe,
    Proxy,
    Render,
    Export,
    Stt,
    LlmPlan,
    Tts,
    Generation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    CancelRequested,
    Cancelled,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Queued, Self::Running)
                | (Self::Queued, Self::CancelRequested)
                | (Self::Running, Self::Succeeded)
                | (Self::Running, Self::Failed)
                | (Self::Running, Self::CancelRequested)
                | (Self::CancelRequested, Self::Cancelled)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressStage {
    Queued,
    Snapshotting,
    Preparing,
    Executing,
    Validating,
    Finalizing,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotMetadata {
    pub base_revision: u64,
    pub snapshot_hash: String,
}

impl SnapshotMetadata {
    pub fn new(base_revision: u64, snapshot_hash: impl Into<String>) -> Result<Self, AppError> {
        let snapshot_hash = snapshot_hash.into();
        if snapshot_hash.trim().is_empty() {
            return Err(AppError::invalid_request("snapshot_hash must not be empty"));
        }
        Ok(Self {
            base_revision,
            snapshot_hash,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub details: Option<BTreeMap<String, String>>,
}

impl AppError {
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        retryable: bool,
        details: Option<BTreeMap<String, String>>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
            details,
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new("INVALID_REQUEST", message, false, None)
    }

    pub fn not_found(entity: &str, id: impl Into<String>) -> Self {
        let mut details = BTreeMap::new();
        details.insert("entity".to_owned(), entity.to_owned());
        details.insert("id".to_owned(), id.into());
        Self::new(
            "NOT_FOUND",
            format!("{entity} was not found"),
            false,
            Some(details),
        )
    }

    pub fn revision_conflict(expected: u64, actual: u64) -> Self {
        let mut details = BTreeMap::new();
        details.insert("expected".to_owned(), expected.to_string());
        details.insert("actual".to_owned(), actual.to_string());
        Self::new(
            "REVISION_CONFLICT",
            "The project changed after this operation was prepared.",
            true,
            Some(details),
        )
    }

    pub fn unavailable(code: &str, message: impl Into<String>) -> Self {
        Self::new(code, message, false, None)
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AppError {}

impl From<editor_domain::DomainError> for AppError {
    fn from(error: editor_domain::DomainError) -> Self {
        match error {
            editor_domain::DomainError::RevisionConflict { expected, actual } => {
                Self::revision_conflict(expected, actual)
            }
            other => Self::new("DOMAIN_VALIDATION_FAILED", other.to_string(), false, None),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: JobId,
    pub kind: JobKind,
    pub state: JobState,
    pub snapshot: SnapshotMetadata,
    pub stage: ProgressStage,
    pub progress: Option<f32>,
    pub message: String,
    pub output_path: Option<RelativePath>,
    pub error: Option<AppError>,
}

impl JobRecord {
    pub fn new(id: JobId, kind: JobKind, snapshot: SnapshotMetadata) -> Self {
        Self {
            id,
            kind,
            state: JobState::Queued,
            snapshot,
            stage: ProgressStage::Queued,
            progress: Some(0.0),
            message: "Queued".to_owned(),
            output_path: None,
            error: None,
        }
    }

    pub fn transition(&mut self, next: JobState) -> Result<(), AppError> {
        if !self.state.can_transition_to(next) {
            return Err(AppError::new(
                "JOB_INVALID_TRANSITION",
                format!(
                    "cannot transition job {} from {:?} to {:?}",
                    self.id, self.state, next
                ),
                false,
                None,
            ));
        }
        self.state = next;
        Ok(())
    }

    pub fn update_progress(
        &mut self,
        stage: ProgressStage,
        progress: Option<f32>,
        message: impl Into<String>,
    ) -> Result<(), AppError> {
        if self.state != JobState::Running {
            return Err(AppError::new(
                "JOB_NOT_RUNNING",
                format!("job {} is not running", self.id),
                false,
                None,
            ));
        }
        if let Some(value) = progress {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(AppError::invalid_request(
                    "job progress must be finite and between 0.0 and 1.0",
                ));
            }
        }
        self.stage = stage;
        self.progress = progress;
        self.message = message.into();
        Ok(())
    }

    fn succeed(&mut self, output_path: Option<RelativePath>) -> Result<(), AppError> {
        if matches!(
            self.kind,
            JobKind::Proxy | JobKind::Render | JobKind::Export | JobKind::Tts | JobKind::Generation
        ) && output_path.is_none()
        {
            return Err(AppError::new(
                "JOB_OUTPUT_REQUIRED",
                "Output-producing jobs require a validated output path.",
                false,
                None,
            ));
        }
        self.transition(JobState::Succeeded)?;
        self.stage = ProgressStage::Completed;
        self.progress = Some(1.0);
        self.message = "Completed".to_owned();
        self.output_path = output_path;
        self.error = None;
        Ok(())
    }

    fn fail(&mut self, error: AppError) -> Result<(), AppError> {
        self.transition(JobState::Failed)?;
        self.stage = ProgressStage::Failed;
        self.progress = None;
        self.message = error.message.clone();
        self.error = Some(error);
        Ok(())
    }

    fn cancel(&mut self) -> Result<(), AppError> {
        self.transition(JobState::Cancelled)?;
        self.stage = ProgressStage::Cancelled;
        self.progress = None;
        self.message = "Cancelled".to_owned();
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancellationError;

impl fmt::Display for CancellationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("job cancellation requested")
    }
}

impl std::error::Error for CancellationError {}

#[derive(Clone, Debug)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn check(&self) -> Result<(), CancellationError> {
        if self.is_cancelled() {
            Err(CancellationError)
        } else {
            Ok(())
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
pub struct CancellationRequest {
    token: CancellationToken,
}

impl CancellationRequest {
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    pub fn cancel(&self) {
        self.token.cancel();
    }
}

pub fn cancellation_pair() -> (CancellationRequest, CancellationToken) {
    let token = CancellationToken::new();
    (
        CancellationRequest {
            token: token.clone(),
        },
        token,
    )
}

#[derive(Clone, Debug)]
pub struct JobHandle {
    pub id: JobId,
    pub cancellation: CancellationRequest,
    pub token: CancellationToken,
}

struct JobEntry {
    record: JobRecord,
    cancellation: CancellationRequest,
}

struct RegistryInner {
    next_id: u64,
    active_jobs: usize,
    jobs: BTreeMap<JobId, JobEntry>,
}

#[derive(Clone)]
pub struct JobRegistry {
    max_jobs: usize,
    max_running_jobs: usize,
    inner: Arc<Mutex<RegistryInner>>,
}

impl JobRegistry {
    pub fn new(max_jobs: usize, max_running_jobs: usize) -> Result<Self, AppError> {
        if max_jobs == 0 || max_running_jobs == 0 || max_running_jobs > max_jobs {
            return Err(AppError::invalid_request(
                "job registry limits must be positive and running capacity cannot exceed total capacity",
            ));
        }
        Ok(Self {
            max_jobs,
            max_running_jobs,
            inner: Arc::new(Mutex::new(RegistryInner {
                next_id: 1,
                active_jobs: 0,
                jobs: BTreeMap::new(),
            })),
        })
    }

    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_MAX_JOBS, DEFAULT_MAX_RUNNING_JOBS)
            .expect("default job registry limits are valid")
    }

    pub fn submit(&self, kind: JobKind, snapshot: SnapshotMetadata) -> Result<JobHandle, AppError> {
        let mut inner = self.lock();
        if inner.jobs.len() >= self.max_jobs {
            return Err(AppError::new(
                "JOB_CAPACITY_EXCEEDED",
                "The in-memory job registry is full.",
                true,
                None,
            ));
        }
        let id = JobId::new(inner.next_id);
        inner.next_id = inner
            .next_id
            .checked_add(1)
            .ok_or_else(|| AppError::new("JOB_ID_EXHAUSTED", "No job IDs remain.", false, None))?;
        let (cancellation, token) = cancellation_pair();
        inner.jobs.insert(
            id,
            JobEntry {
                record: JobRecord::new(id, kind, snapshot),
                cancellation: cancellation.clone(),
            },
        );
        Ok(JobHandle {
            id,
            cancellation,
            token,
        })
    }

    pub fn start(&self, id: JobId) -> Result<JobRecord, AppError> {
        let mut inner = self.lock();
        if inner.active_jobs >= self.max_running_jobs {
            return Err(AppError::new(
                "JOB_RUNNING_CAPACITY_EXCEEDED",
                "The maximum number of running jobs has been reached.",
                true,
                None,
            ));
        }
        let record = {
            let entry = inner
                .jobs
                .get_mut(&id)
                .ok_or_else(|| AppError::not_found("job", id.to_string()))?;
            entry.record.transition(JobState::Running)?;
            entry.record.stage = ProgressStage::Preparing;
            entry.record.message = "Preparing".to_owned();
            entry.record.clone()
        };
        inner.active_jobs += 1;
        Ok(record)
    }

    pub fn update_progress(
        &self,
        id: JobId,
        stage: ProgressStage,
        progress: Option<f32>,
        message: impl Into<String>,
    ) -> Result<JobRecord, AppError> {
        let mut inner = self.lock();
        let entry = inner
            .jobs
            .get_mut(&id)
            .ok_or_else(|| AppError::not_found("job", id.to_string()))?;
        entry.record.update_progress(stage, progress, message)?;
        Ok(entry.record.clone())
    }

    pub fn mark_succeeded(
        &self,
        id: JobId,
        output_path: Option<RelativePath>,
    ) -> Result<JobRecord, AppError> {
        let mut inner = self.lock();
        {
            let entry = inner
                .jobs
                .get_mut(&id)
                .ok_or_else(|| AppError::not_found("job", id.to_string()))?;
            entry.record.succeed(output_path)?;
        }
        inner.active_jobs = inner.active_jobs.saturating_sub(1);
        Ok(inner
            .jobs
            .get(&id)
            .expect("job remains registered")
            .record
            .clone())
    }

    pub fn mark_failed(&self, id: JobId, error: AppError) -> Result<JobRecord, AppError> {
        let mut inner = self.lock();
        {
            let entry = inner
                .jobs
                .get_mut(&id)
                .ok_or_else(|| AppError::not_found("job", id.to_string()))?;
            entry.record.fail(error)?;
        }
        inner.active_jobs = inner.active_jobs.saturating_sub(1);
        Ok(inner
            .jobs
            .get(&id)
            .expect("job remains registered")
            .record
            .clone())
    }

    pub fn request_cancel(&self, id: JobId) -> Result<JobRecord, AppError> {
        let mut inner = self.lock();
        let entry = inner
            .jobs
            .get_mut(&id)
            .ok_or_else(|| AppError::not_found("job", id.to_string()))?;
        match entry.record.state {
            JobState::Queued => {
                entry.record.transition(JobState::CancelRequested)?;
                entry.cancellation.cancel();
                entry.record.cancel()?;
            }
            JobState::Running => {
                entry.record.transition(JobState::CancelRequested)?;
                entry.record.stage = ProgressStage::Cancelling;
                entry.record.message = "Cancellation requested".to_owned();
                entry.cancellation.cancel();
            }
            JobState::CancelRequested => entry.cancellation.cancel(),
            JobState::Succeeded | JobState::Failed | JobState::Cancelled => {
                return Err(AppError::new(
                    "JOB_NOT_CANCELLABLE",
                    format!("job {id} is already {:?}", entry.record.state),
                    false,
                    None,
                ));
            }
        }
        Ok(entry.record.clone())
    }

    pub fn mark_cancelled(&self, id: JobId) -> Result<JobRecord, AppError> {
        let mut inner = self.lock();
        let was_active = {
            let entry = inner
                .jobs
                .get_mut(&id)
                .ok_or_else(|| AppError::not_found("job", id.to_string()))?;
            let was_active = matches!(
                entry.record.state,
                JobState::Running | JobState::CancelRequested
            );
            if entry.record.state == JobState::Running {
                entry.record.transition(JobState::CancelRequested)?;
            }
            entry.record.cancel()?;
            entry.cancellation.cancel();
            was_active
        };
        if was_active {
            inner.active_jobs = inner.active_jobs.saturating_sub(1);
        }
        Ok(inner
            .jobs
            .get(&id)
            .expect("job remains registered")
            .record
            .clone())
    }

    pub fn get(&self, id: JobId) -> Result<JobRecord, AppError> {
        let inner = self.lock();
        inner
            .jobs
            .get(&id)
            .map(|entry| entry.record.clone())
            .ok_or_else(|| AppError::not_found("job", id.to_string()))
    }

    pub fn list(&self) -> Vec<JobRecord> {
        self.lock()
            .jobs
            .values()
            .map(|entry| entry.record.clone())
            .collect()
    }

    fn lock(&self) -> MutexGuard<'_, RegistryInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn snapshot() -> SnapshotMetadata {
        SnapshotMetadata::new(7, "snapshot-7").unwrap()
    }

    #[test]
    fn state_machine_accepts_only_valid_transitions() {
        let mut record = JobRecord::new(JobId::new(1), JobKind::Export, snapshot());
        assert!(record.transition(JobState::Running).is_ok());
        assert!(record.transition(JobState::Succeeded).is_ok());
        assert!(record.transition(JobState::Cancelled).is_err());
        assert!(record.state.is_terminal());
    }

    #[test]
    fn cancellation_signals_token_and_finishes_running_job() {
        let registry = JobRegistry::new(4, 1).unwrap();
        let handle = registry.submit(JobKind::Export, snapshot()).unwrap();
        registry.start(handle.id).unwrap();
        let requested = registry.request_cancel(handle.id).unwrap();
        assert_eq!(requested.state, JobState::CancelRequested);
        assert!(handle.token.is_cancelled());
        assert!(handle.token.check().is_err());
        let cancelled = registry.mark_cancelled(handle.id).unwrap();
        assert_eq!(cancelled.state, JobState::Cancelled);
        assert!(registry.mark_cancelled(handle.id).is_err());
    }

    #[test]
    fn registry_enforces_total_and_running_bounds() {
        let registry = JobRegistry::new(1, 1).unwrap();
        let first = registry.submit(JobKind::Probe, snapshot()).unwrap();
        assert!(registry.submit(JobKind::Proxy, snapshot()).is_err());
        registry.start(first.id).unwrap();
        assert!(registry.start(first.id).is_err());
    }

    #[test]
    fn app_error_serializes_as_stable_envelope() {
        let error = AppError::revision_conflict(4, 5);
        let json = serde_json::to_value(error).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "code": "REVISION_CONFLICT",
                "message": "The project changed after this operation was prepared.",
                "retryable": true,
                "details": { "actual": "5", "expected": "4" }
            })
        );
    }

    #[test]
    fn snapshot_metadata_rejects_empty_hash() {
        assert!(SnapshotMetadata::new(0, " ").is_err());
    }

    #[test]
    fn list_is_deterministic_by_job_id() {
        let registry = JobRegistry::new(3, 1).unwrap();
        registry.submit(JobKind::Probe, snapshot()).unwrap();
        registry.submit(JobKind::Render, snapshot()).unwrap();
        let ids: BTreeSet<_> = registry.list().into_iter().map(|job| job.id).collect();
        assert_eq!(
            ids.into_iter().collect::<Vec<_>>(),
            vec![JobId::new(1), JobId::new(2)]
        );
    }
}
