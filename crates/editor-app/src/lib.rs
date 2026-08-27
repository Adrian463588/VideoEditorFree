//! Application use cases and dependency-injected media execution.

use editor_domain::{ApplyResult, ProjectDocument, RelativePath, TimelineOperation};
use editor_jobs::{CancellationToken, JobId, JobKind, JobRecord, JobRegistry, SnapshotMetadata};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub use editor_jobs::{AppError, JobState, ProgressStage};

pub trait ProjectPort {
    fn current_document(&self) -> Result<ProjectDocument, AppError>;
    fn commit_document(&mut self, document: &ProjectDocument) -> Result<(), AppError>;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectSnapshot {
    pub document: ProjectDocument,
    pub metadata: SnapshotMetadata,
}
impl ProjectSnapshot {
    pub fn capture(document: ProjectDocument) -> Result<Self, AppError> {
        document.validate()?;
        let bytes = serde_json::to_vec(&document).map_err(|error| {
            AppError::new(
                "SNAPSHOT_SERIALIZATION_FAILED",
                error.to_string(),
                false,
                None,
            )
        })?;
        Ok(Self {
            metadata: SnapshotMetadata::new(document.revision, stable_hash(&bytes))?,
            document,
        })
    }
}
fn stable_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[derive(Clone, Debug)]
pub struct MediaJobRequest {
    pub id: JobId,
    pub kind: JobKind,
    pub snapshot: ProjectSnapshot,
    pub output_path: Option<RelativePath>,
    pub profile: editor_media::ExportProfile,
    pub cancellation: CancellationToken,
}

pub trait MediaPort: Send + Sync {
    fn start(&self, request: MediaJobRequest) -> Result<(), AppError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableMediaPort;
impl MediaPort for UnavailableMediaPort {
    fn start(&self, _request: MediaJobRequest) -> Result<(), AppError> {
        Err(AppError::unavailable(
            "MEDIA_UNAVAILABLE",
            "The verified media runtime is not provisioned.",
        ))
    }
}

#[derive(Clone)]
pub struct ConfiguredMediaPort {
    runtime: Arc<Mutex<MediaRuntime>>,
}
struct MediaRuntime {
    root: PathBuf,
    executables: Option<editor_media::ExecutableConfig>,
}
impl ConfiguredMediaPort {
    pub fn unavailable(root: impl Into<PathBuf>) -> Self {
        Self {
            runtime: Arc::new(Mutex::new(MediaRuntime {
                root: root.into(),
                executables: None,
            })),
        }
    }
    pub fn configured(
        root: impl Into<PathBuf>,
        executables: editor_media::ExecutableConfig,
    ) -> Self {
        Self {
            runtime: Arc::new(Mutex::new(MediaRuntime {
                root: root.into(),
                executables: Some(executables),
            })),
        }
    }
    pub fn set_root(&self, root: impl Into<PathBuf>) {
        self.runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .root = root.into();
    }
    pub fn set_executables(&self, executables: Option<editor_media::ExecutableConfig>) {
        self.runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .executables = executables;
    }
    pub fn is_configured(&self) -> bool {
        self.runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .executables
            .is_some()
    }
    pub fn probe(&self, input: &Path) -> Result<editor_media::ProbeMetadata, AppError> {
        let runtime = self
            .runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let config = runtime.executables.clone().ok_or_else(|| {
            AppError::unavailable(
                "MEDIA_UNAVAILABLE",
                "The verified media runtime is not provisioned.",
            )
        })?;
        editor_media::probe_media(&config, input, &editor_media::SystemChildProcessRunner)
            .map_err(media_error)
    }
    fn output_path(runtime: &MediaRuntime, relative: &RelativePath) -> Result<PathBuf, AppError> {
        editor_media::validate_relative_alias(relative.as_str()).map_err(media_error)?;
        let root = std::fs::canonicalize(&runtime.root).map_err(|_| {
            AppError::unavailable("MEDIA_UNAVAILABLE", "The project root is unavailable.")
        })?;
        let candidate = root.join(relative.as_str());
        let parent = candidate
            .parent()
            .ok_or_else(|| AppError::invalid_request("output path must name a file"))?;
        let parent = std::fs::canonicalize(parent)
            .map_err(|_| AppError::invalid_request("output directory is unavailable"))?;
        if !parent.starts_with(&root) {
            return Err(AppError::invalid_request(
                "output path is outside the project root",
            ));
        }
        Ok(parent.join(
            candidate
                .file_name()
                .ok_or_else(|| AppError::invalid_request("output path must name a file"))?,
        ))
    }
}
impl MediaPort for ConfiguredMediaPort {
    fn start(&self, request: MediaJobRequest) -> Result<(), AppError> {
        if !matches!(
            request.kind,
            JobKind::Proxy | JobKind::Render | JobKind::Export
        ) {
            return Err(AppError::unavailable(
                "MEDIA_UNAVAILABLE",
                "This job kind has no configured media executor.",
            ));
        }
        let runtime = self
            .runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let config = runtime.executables.clone().ok_or_else(|| {
            AppError::unavailable(
                "MEDIA_UNAVAILABLE",
                "The verified media runtime is not provisioned.",
            )
        })?;
        let relative = request
            .output_path
            .as_ref()
            .ok_or_else(|| AppError::invalid_request("media jobs require an output path"))?;
        let output = Self::output_path(&runtime, relative)?;
        let executor = editor_media::FfmpegExecutor::new(config);
        if request.kind == JobKind::Export
            && request.profile != editor_media::ExportProfile::Baseline
        {
            let plan = editor_media::build_render_plan_with_profile(
                &request.snapshot.document,
                &runtime.root,
                output,
                executor.config(),
                request.profile,
            )
            .map_err(media_error)?;
            executor
                .execute_profile(&plan, &|| request.cancellation.is_cancelled())
                .map(|_| ())
                .map_err(media_error)
        } else {
            let plan = editor_media::build_render_plan(
                &request.snapshot.document,
                &runtime.root,
                output,
                executor.config(),
            )
            .map_err(media_error)?;
            executor
                .execute(&plan, &|| request.cancellation.is_cancelled())
                .map(|_| ())
                .map_err(media_error)
        }
    }
}

pub struct EditorApplication<P, M> {
    project: P,
    media: M,
    jobs: JobRegistry,
}
impl<P, M> EditorApplication<P, M>
where
    P: ProjectPort,
    M: MediaPort,
{
    pub fn new(project: P, media: M, jobs: JobRegistry) -> Self {
        Self {
            project,
            media,
            jobs,
        }
    }
    pub fn snapshot(&self) -> Result<ProjectSnapshot, AppError> {
        ProjectSnapshot::capture(self.project.current_document()?)
    }
    pub fn apply_timeline(
        &mut self,
        base_revision: u64,
        operation: TimelineOperation,
    ) -> Result<ApplyResult, AppError> {
        let current = self.project.current_document()?;
        let result = current.apply(base_revision, operation)?;
        self.project.commit_document(&result.document)?;
        Ok(result)
    }
    pub fn start_media_job(
        &self,
        kind: JobKind,
        output_path: Option<RelativePath>,
    ) -> Result<JobId, AppError> {
        self.start_media_job_with_profile(kind, output_path, editor_media::ExportProfile::Baseline)
    }
    pub fn start_media_job_with_profile(
        &self,
        kind: JobKind,
        output_path: Option<RelativePath>,
        profile: editor_media::ExportProfile,
    ) -> Result<JobId, AppError> {
        let snapshot = self.snapshot()?;
        let handle = self.jobs.submit(kind, snapshot.metadata.clone())?;
        self.jobs.start(handle.id)?;
        let request = MediaJobRequest {
            id: handle.id,
            kind,
            snapshot,
            output_path: output_path.clone(),
            profile,
            cancellation: handle.token.clone(),
        };
        match self.media.start(request) {
            Ok(()) => {
                self.jobs.mark_succeeded(handle.id, output_path)?;
                Ok(handle.id)
            }
            Err(error) => {
                finish_error(&self.jobs, handle.id, &handle.token, error.clone());
                Err(error)
            }
        }
    }
    pub fn start_media_job_async(
        &self,
        kind: JobKind,
        output_path: Option<RelativePath>,
    ) -> Result<JobId, AppError>
    where
        M: Clone + Send + 'static,
    {
        self.start_media_job_async_with_profile(
            kind,
            output_path,
            editor_media::ExportProfile::Baseline,
        )
    }
    pub fn start_media_job_async_with_profile(
        &self,
        kind: JobKind,
        output_path: Option<RelativePath>,
        profile: editor_media::ExportProfile,
    ) -> Result<JobId, AppError>
    where
        M: Clone + Send + 'static,
    {
        let snapshot = self.snapshot()?;
        let handle = self.jobs.submit(kind, snapshot.metadata.clone())?;
        self.jobs.start(handle.id)?;
        let request = MediaJobRequest {
            id: handle.id,
            kind,
            snapshot,
            output_path: output_path.clone(),
            profile,
            cancellation: handle.token.clone(),
        };
        let jobs = self.jobs.clone();
        let media = self.media.clone();
        let token = handle.token.clone();
        let id = handle.id;
        std::thread::spawn(move || match media.start(request) {
            Ok(()) => {
                let _ = jobs.mark_succeeded(id, output_path);
            }
            Err(error) => finish_error(&jobs, id, &token, error),
        });
        Ok(handle.id)
    }
    pub fn cancel_job(&self, id: JobId) -> Result<JobRecord, AppError> {
        self.jobs.request_cancel(id)
    }
    pub fn job(&self, id: JobId) -> Result<JobRecord, AppError> {
        self.jobs.get(id)
    }
    pub fn jobs(&self) -> &JobRegistry {
        &self.jobs
    }
}
fn finish_error(jobs: &JobRegistry, id: JobId, token: &CancellationToken, error: AppError) {
    if token.is_cancelled() || error.code == "MEDIA_CANCELLED" {
        let _ = jobs.mark_cancelled(id);
    } else {
        let _ = jobs.mark_failed(id, error);
    }
}
pub type AppFacade<P, M> = EditorApplication<P, M>;

fn media_error(error: editor_media::MediaError) -> AppError {
    let (code, message, retryable) = match error {
        editor_media::MediaError::BinaryUnavailable { .. }
        | editor_media::MediaError::ExecutableUnavailable { .. } => (
            "MEDIA_UNAVAILABLE",
            "The verified media runtime is not provisioned.",
            false,
        ),
        editor_media::MediaError::Cancelled => (
            "MEDIA_CANCELLED",
            "The media operation was cancelled.",
            false,
        ),
        editor_media::MediaError::ProcessFailed { .. } => {
            ("MEDIA_PROCESS_FAILED", "The media process failed.", true)
        }
        editor_media::MediaError::ProbeParse(_) | editor_media::MediaError::ProbeValidation(_) => (
            "MEDIA_PROBE_FAILED",
            "The selected media probe was invalid.",
            false,
        ),
        editor_media::MediaError::InvalidPlan(_) | editor_media::MediaError::Unsupported(_) => (
            "MEDIA_UNSUPPORTED",
            "The timeline cannot be rendered by the configured baseline profile.",
            false,
        ),
        editor_media::MediaError::PathViolation(_) | editor_media::MediaError::InvalidOutput(_) => {
            (
                "INVALID_MEDIA_PATH",
                "The media path is not allowed.",
                false,
            )
        }
        editor_media::MediaError::InvalidConfiguration(_) => (
            "MEDIA_CONFIGURATION_INVALID",
            "The media runtime configuration is invalid.",
            false,
        ),
        editor_media::MediaError::Io(_) | editor_media::MediaError::Domain(_) => (
            "MEDIA_FAILED",
            "The media operation failed.",
            retryable_for_io(&error),
        ),
    };
    AppError::new(code, message, retryable, None)
}
fn retryable_for_io(error: &editor_media::MediaError) -> bool {
    matches!(error, editor_media::MediaError::Io(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_domain::{ProjectId, TimelineOperation};
    use std::sync::mpsc::{self, Sender};
    use std::time::Duration;
    struct MemoryProject(ProjectDocument);
    impl ProjectPort for MemoryProject {
        fn current_document(&self) -> Result<ProjectDocument, AppError> {
            Ok(self.0.clone())
        }
        fn commit_document(&mut self, document: &ProjectDocument) -> Result<(), AppError> {
            self.0 = document.clone();
            Ok(())
        }
    }
    #[derive(Clone)]
    struct RecordingMediaPort {
        sender: Sender<editor_media::ExportProfile>,
    }
    impl MediaPort for RecordingMediaPort {
        fn start(&self, request: MediaJobRequest) -> Result<(), AppError> {
            self.sender
                .send(request.profile)
                .expect("recording receiver remains available");
            Ok(())
        }
    }
    fn project() -> ProjectDocument {
        ProjectDocument::create(ProjectId::new("project-1").unwrap(), "Test project").unwrap()
    }
    #[test]
    fn snapshot_has_revision_identity() {
        let first = ProjectSnapshot::capture(project()).unwrap();
        assert_eq!(first.metadata.base_revision, 0);
    }
    #[test]
    fn unavailable_media_fails_job() {
        let app = EditorApplication::new(
            MemoryProject(project()),
            UnavailableMediaPort,
            JobRegistry::new(4, 1).unwrap(),
        );
        let error = app.start_media_job(JobKind::Export, None).unwrap_err();
        assert_eq!(error.code, "MEDIA_UNAVAILABLE");
        assert_eq!(app.jobs().list()[0].state, JobState::Failed);
    }
    #[test]
    fn async_media_job_carries_export_profile() {
        let (sender, receiver) = mpsc::channel();
        let app = EditorApplication::new(
            MemoryProject(project()),
            RecordingMediaPort { sender },
            JobRegistry::new(4, 1).unwrap(),
        );
        let id = app
            .start_media_job_async_with_profile(
                JobKind::Export,
                None,
                editor_media::ExportProfile::Instagram,
            )
            .unwrap();
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            editor_media::ExportProfile::Instagram
        );
        assert!(matches!(
            app.job(id).unwrap().state,
            JobState::Running | JobState::Succeeded
        ));
    }
    #[test]
    fn timeline_revision_conflict_is_preserved() {
        let mut app = EditorApplication::new(
            MemoryProject(project()),
            UnavailableMediaPort,
            JobRegistry::new(4, 1).unwrap(),
        );
        let operation = TimelineOperation::AddTrack {
            track: editor_domain::Track::new(
                editor_domain::TrackId::new("video").unwrap(),
                editor_domain::TrackKind::Video,
                "Video",
            )
            .unwrap(),
        };
        app.apply_timeline(0, operation.clone()).unwrap();
        assert_eq!(
            app.apply_timeline(0, operation).unwrap_err().code,
            "REVISION_CONFLICT"
        );
    }
}
