#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use editor_app::{ConfiguredMediaPort, EditorApplication, ProjectPort};
use editor_domain::{
    ApplyResult, Asset, AssetId, AssetKind, AssetStatus, AudioStream, DomainError, Fingerprint,
    ProbeSummary, ProjectDocument, ProjectId, RelativePath, TimelineOperation, VideoStream,
};
use editor_jobs::{AppError, JobId, JobKind, JobRecord};
use editor_media::{BinaryContract, BinaryManifest, ExecutableConfig, ProbeMetadata};
use editor_project::{load_project, save_project, save_project_if_revision, validate_project_path};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, OnceLock},
    time::UNIX_EPOCH,
};
use tauri::{path::BaseDirectory, AppHandle, Manager, State};

const MAX_PATH_INPUT_BYTES: usize = 32 * 1024;
const MAX_IMPORT_PATHS: usize = 256;
type Application = EditorApplication<SharedProjectPort, ConfiguredMediaPort>;

#[derive(Clone)]
struct SharedProjectPort(Arc<Mutex<Option<ProjectDocument>>>);
impl SharedProjectPort {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(None)))
    }
    fn replace(&self, document: ProjectDocument) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(document);
    }
}
impl ProjectPort for SharedProjectPort {
    fn current_document(&self) -> Result<ProjectDocument, AppError> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .ok_or_else(|| {
                AppError::new("PROJECT_NOT_LOADED", "No project is loaded.", false, None)
            })
    }
    fn commit_document(&mut self, document: &ProjectDocument) -> Result<(), AppError> {
        self.replace(document.clone());
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CapabilityStatus {
    state: &'static str,
    reason: &'static str,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HostStatus {
    core: CapabilityStatus,
    media: CapabilityStatus,
    ai: CapabilityStatus,
    project_loaded: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HostError {
    code: String,
    message: String,
    retryable: bool,
    details: Option<BTreeMap<String, String>>,
}
impl HostError {
    fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
            details: None,
        }
    }
    fn unavailable(code: &str, message: &str) -> Self {
        Self::new(code, message)
    }
}
impl From<AppError> for HostError {
    fn from(error: AppError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            retryable: error.retryable,
            details: error.details,
        }
    }
}
impl From<DomainError> for HostError {
    fn from(error: DomainError) -> Self {
        match error {
            DomainError::RevisionConflict { expected, actual } => {
                let mut details = BTreeMap::new();
                details.insert("expected".into(), expected.to_string());
                details.insert("actual".into(), actual.to_string());
                Self {
                    code: "REVISION_CONFLICT".into(),
                    message: "The project changed before this operation completed.".into(),
                    retryable: true,
                    details: Some(details),
                }
            }
            DomainError::InvalidId { .. } | DomainError::InvalidValue { .. } => {
                Self::new("INVALID_PROJECT", "The project data is invalid.")
            }
            DomainError::NotFound { .. } => Self::new(
                "ENTITY_NOT_FOUND",
                "The requested project entity was not found.",
            ),
            _ => Self::new(
                "DOMAIN_OPERATION_REJECTED",
                "The timeline operation was rejected.",
            ),
        }
    }
}
impl From<editor_project::ProjectError> for HostError {
    fn from(error: editor_project::ProjectError) -> Self {
        match error {
            editor_project::ProjectError::InvalidProjectPath { .. }
            | editor_project::ProjectError::InvalidAssetReference { .. }
            | editor_project::ProjectError::PathOutsideProject { .. } => Self::new(
                "INVALID_PROJECT_PATH",
                "The project path or asset reference is not allowed.",
            ),
            editor_project::ProjectError::Io { .. }
            | editor_project::ProjectError::RecoveryFailed { .. } => Self::new(
                "PROJECT_IO_FAILED",
                "The project could not be read or written.",
            ),
            _ => Self::new(
                "PROJECT_INVALID",
                "The project document is invalid or unsupported.",
            ),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateProjectRequest {
    name: String,
    project_id: Option<String>,
    project_path: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectPathRequest {
    project_path: String,
}
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveProjectRequest {
    project_path: Option<String>,
    expected_revision: Option<u64>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AssetImportRequest {
    paths: Vec<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TimelineApplyRequest {
    base_revision: u64,
    operation: TimelineOperation,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExportRequest {
    output_path: String,
    base_revision: Option<u64>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewSeekRequest {
    timeline_ticks: i64,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JobRequest {
    job_id: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AssistantPlanRequest {
    base_revision: u64,
    text: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BundleDownloadRequest {
    profile: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SaveProjectResponse {
    bytes_written: usize,
    backup_created: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BundleDownloadResponse {
    profile: String,
    install_root: String,
    media_ready: bool,
    message: String,
}
#[derive(Clone, Debug, Serialize)]
struct JobResponse {
    id: String,
    kind: JobKind,
    state: editor_app::JobState,
    snapshot: editor_jobs::SnapshotMetadata,
    stage: editor_app::ProgressStage,
    progress: Option<f32>,
    message: String,
    output_path: Option<String>,
    error: Option<AppError>,
}
impl From<JobRecord> for JobResponse {
    fn from(job: JobRecord) -> Self {
        Self {
            id: job.id.to_string(),
            kind: job.kind,
            state: job.state,
            snapshot: job.snapshot,
            stage: job.stage,
            progress: job.progress,
            message: job.message,
            output_path: job.output_path.map(|path| path.to_string()),
            error: job.error,
        }
    }
}

struct HostState {
    shared: SharedProjectPort,
    media: ConfiguredMediaPort,
    app: Application,
    project_path: Option<PathBuf>,
    saved_revision: Option<u64>,
}
struct AppState(Mutex<HostState>);
fn invalid_request() -> HostError {
    HostError::new("INVALID_REQUEST", "The request is invalid.")
}

fn default_bundle_root() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("VideoEditorFree").join("runtime"))
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .map(|path| path.join("VideoEditorFree").join("runtime"))
        .unwrap_or_else(|| PathBuf::from(".videoeditorfree").join("runtime"))
}

fn media_executables_from_manifest(
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    manifest_path: PathBuf,
) -> Option<ExecutableConfig> {
    let bytes = std::fs::read(manifest_path).ok()?;
    let manifest = serde_json::from_slice::<BinaryManifest>(&bytes).ok()?;
    let config = ExecutableConfig::new(ffmpeg, ffprobe)
        .with_binary_contract(BinaryContract::reviewed(manifest));
    config.validate().ok().map(|_| config)
}

fn media_executables_from_bundle(root: &Path) -> Option<ExecutableConfig> {
    media_executables_from_manifest(
        root.join("media").join("ffmpeg.exe"),
        root.join("media").join("ffprobe.exe"),
        root.join("media-manifest.json"),
    )
}

fn media_executables_from_environment() -> Option<ExecutableConfig> {
    let ffmpeg = std::env::var_os("VIDEOEDITORFREE_FFMPEG")?;
    let ffprobe = std::env::var_os("VIDEOEDITORFREE_FFPROBE")?;
    let manifest_path = std::env::var_os("VIDEOEDITORFREE_MEDIA_MANIFEST")?;
    media_executables_from_manifest(
        PathBuf::from(ffmpeg),
        PathBuf::from(ffprobe),
        PathBuf::from(manifest_path),
    )
}

fn configured_media_port(root: PathBuf) -> ConfiguredMediaPort {
    let media = ConfiguredMediaPort::unavailable(root);
    if let Some(config) = media_executables_from_environment()
        .or_else(|| media_executables_from_bundle(&default_bundle_root()))
    {
        media.set_executables(Some(config));
    }
    media
}

fn bundle_script_path(app: &AppHandle) -> Result<PathBuf, HostError> {
    let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../scripts/runtime/download-bundle.ps1");
    if development.is_file() {
        return Ok(development);
    }
    for relative in [
        "scripts/runtime/download-bundle.ps1",
        "runtime/download-bundle.ps1",
        "download-bundle.ps1",
    ] {
        if let Ok(path) = app.path().resolve(relative, BaseDirectory::Resource) {
            if path.is_file() {
                return Ok(path);
            }
        }
    }
    Err(HostError::new(
        "BUNDLE_SCRIPT_MISSING",
        "The runtime bundle downloader is not included in this build.",
    ))
}

fn bundle_manifest_path(app: &AppHandle) -> Result<PathBuf, HostError> {
    let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../resources/runtime/bundle-manifest.json");
    if development.is_file() {
        return Ok(development);
    }
    for relative in [
        "resources/runtime/bundle-manifest.json",
        "runtime/bundle-manifest.json",
        "bundle-manifest.json",
        "scripts/runtime/bundle-manifest.json",
    ] {
        if let Ok(path) = app.path().resolve(relative, BaseDirectory::Resource) {
            if path.is_file() {
                return Ok(path);
            }
        }
    }
    Err(HostError::new(
        "BUNDLE_MANIFEST_MISSING",
        "The runtime bundle manifest is not included in this build.",
    ))
}

fn project_root(host: &HostState) -> PathBuf {
    host.project_path
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn asset_kind(path: &Path) -> Result<AssetKind, HostError> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| {
            HostError::new("MEDIA_UNSUPPORTED", "The asset has no supported extension.")
        })?;
    let kind = match extension.as_str() {
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" => AssetKind::Video,
        "wav" | "mp3" | "m4a" | "flac" | "ogg" => AssetKind::Audio,
        "png" | "jpg" | "jpeg" | "webp" | "bmp" => AssetKind::Image,
        "srt" | "vtt" => AssetKind::Subtitle,
        _ => {
            return Err(HostError::new(
                "MEDIA_UNSUPPORTED",
                "The asset type is not supported.",
            ))
        }
    };
    Ok(kind)
}

fn probe_summary(probe: ProbeMetadata) -> Result<ProbeSummary, HostError> {
    probe
        .validate()
        .map_err(|_| HostError::new("MEDIA_PROBE_FAILED", "The media probe was invalid."))?;
    let duration = probe
        .duration_seconds
        .ok_or_else(|| HostError::new("MEDIA_PROBE_FAILED", "The media probe has no duration."))?;
    let reference = probe
        .streams
        .iter()
        .find(|stream| stream.codec_type == "video")
        .or_else(|| {
            probe
                .streams
                .iter()
                .find(|stream| stream.codec_type == "audio")
        })
        .ok_or_else(|| {
            HostError::new("MEDIA_PROBE_FAILED", "The media probe has no media stream.")
        })?;
    let timebase = reference
        .time_base
        .ok_or_else(|| HostError::new("MEDIA_PROBE_FAILED", "The media probe has no timebase."))?;
    let ticks = (duration * timebase.denominator as f64 / timebase.numerator as f64).round();
    if !ticks.is_finite() || ticks <= 0.0 || ticks > i64::MAX as f64 {
        return Err(HostError::new(
            "MEDIA_PROBE_FAILED",
            "The media duration is out of range.",
        ));
    }
    let video = probe
        .streams
        .iter()
        .find(|stream| stream.codec_type == "video")
        .map(|stream| -> Result<VideoStream, HostError> {
            Ok(VideoStream {
                codec: stream.codec_name.clone().ok_or_else(|| {
                    HostError::new("MEDIA_PROBE_FAILED", "The video codec is missing.")
                })?,
                width: stream.width.unwrap_or_default(),
                height: stream.height.unwrap_or_default(),
                frame_rate: None,
            })
        })
        .transpose()?;
    let audio = probe
        .streams
        .iter()
        .find(|stream| stream.codec_type == "audio")
        .map(|stream| -> Result<AudioStream, HostError> {
            Ok(AudioStream {
                codec: stream.codec_name.clone().ok_or_else(|| {
                    HostError::new("MEDIA_PROBE_FAILED", "The audio codec is missing.")
                })?,
                sample_rate: stream.sample_rate.unwrap_or_default(),
                channels: stream.channels.unwrap_or_default(),
            })
        })
        .transpose()?;
    Ok(ProbeSummary {
        duration_ticks: ticks as i64,
        stream_timebase: timebase,
        video,
        audio,
        rotation_degrees: probe
            .streams
            .iter()
            .find_map(|stream| stream.rotation_degrees),
        raw_tool_version: probe.tool_version.unwrap_or_else(|| "unknown".to_owned()),
    })
}

fn stable_asset_id(relative_path: &str) -> Result<AssetId, HostError> {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in relative_path.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    AssetId::new(format!("asset-{hash:016x}")).map_err(HostError::from)
}

fn import_asset(host: &HostState, raw_path: &str) -> Result<Asset, HostError> {
    let input = PathBuf::from(raw_path);
    if !input.is_absolute() || !input.is_file() {
        return Err(HostError::new(
            "MEDIA_PATH_INVALID",
            "Imported assets must be existing absolute files.",
        ));
    }
    let input = input.canonicalize().map_err(|_| {
        HostError::new(
            "MEDIA_PATH_INVALID",
            "The imported asset could not be resolved.",
        )
    })?;
    let root = project_root(host).canonicalize().map_err(|_| {
        HostError::new(
            "PROJECT_IO_FAILED",
            "The project root could not be resolved.",
        )
    })?;
    if !input.starts_with(&root) {
        return Err(HostError::new(
            "INVALID_MEDIA_PATH",
            "Imported assets must be inside the project root.",
        ));
    }
    let relative = input
        .strip_prefix(&root)
        .map_err(|_| {
            HostError::new(
                "INVALID_MEDIA_PATH",
                "The asset path is outside the project.",
            )
        })?
        .to_string_lossy()
        .replace('\\', "/");
    let relative_path = RelativePath::new(relative).map_err(HostError::from)?;
    let kind = asset_kind(&input)?;
    let metadata = std::fs::metadata(&input)
        .map_err(|_| HostError::new("MEDIA_PATH_INVALID", "The asset metadata is unavailable."))?;
    let modified_time = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_secs().to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    let probe = if matches!(kind, AssetKind::Subtitle) {
        None
    } else {
        Some(probe_summary(
            host.media.probe(&input).map_err(HostError::from)?,
        )?)
    };
    Ok(Asset {
        id: stable_asset_id(relative_path.as_str())?,
        relative_path,
        kind,
        fingerprint: Fingerprint {
            size_bytes: metadata.len(),
            modified_time,
            sha256: None,
        },
        probe,
        status: AssetStatus::Available,
    })
}
fn project_path(raw: &str, must_exist: bool) -> Result<PathBuf, HostError> {
    if raw.trim().is_empty() || raw.len() > MAX_PATH_INPUT_BYTES {
        return Err(invalid_request());
    }
    let input = PathBuf::from(raw);
    validate_project_path(&input).map_err(HostError::from)?;
    if !input.is_absolute() {
        return Err(invalid_request());
    }
    if must_exist {
        if !input.is_file() {
            return Err(HostError::new(
                "PROJECT_NOT_FOUND",
                "The selected project file does not exist.",
            ));
        }
        return input.canonicalize().map_err(|_| {
            HostError::new(
                "PROJECT_IO_FAILED",
                "The selected project file could not be resolved.",
            )
        });
    }
    let parent = input
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(invalid_request)?
        .canonicalize()
        .map_err(|_| {
            HostError::new(
                "PROJECT_IO_FAILED",
                "The project destination folder could not be resolved.",
            )
        })?;
    Ok(parent.join(input.file_name().ok_or_else(invalid_request)?))
}
fn generated_project_id() -> ProjectId {
    static NEXT_ID: OnceLock<Mutex<u64>> = OnceLock::new();
    let counter = NEXT_ID.get_or_init(|| Mutex::new(0));
    let mut value = counter
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *value += 1;
    ProjectId::new(format!("project-{}", *value)).expect("generated project ID is valid")
}
fn set_loaded_project(host: &mut HostState, document: ProjectDocument, path: Option<PathBuf>) {
    host.shared.replace(document);
    host.media.set_root(
        path.as_deref()
            .and_then(Path::parent)
            .unwrap_or_else(|| Path::new(".")),
    );
    host.project_path = path;
    host.saved_revision = host.shared.current_document().ok().map(|doc| doc.revision);
}
fn parse_job_id(raw: &str) -> Result<JobId, HostError> {
    raw.parse::<u64>()
        .map(JobId::new)
        .map_err(|_| HostError::new("JOB_NOT_FOUND", "The requested job was not found."))
}

#[tauri::command]
fn bundle_download(
    request: BundleDownloadRequest,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<BundleDownloadResponse, HostError> {
    let profile = request.profile.unwrap_or_else(|| "all".to_owned());
    if !matches!(profile.as_str(), "core" | "ai" | "all") {
        return Err(invalid_request());
    }
    let script = bundle_script_path(&app)?;
    let manifest = bundle_manifest_path(&app)?;
    let install_root = default_bundle_root();
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .arg("-Profile")
        .arg(&profile)
        .arg("-InstallRoot")
        .arg(&install_root)
        .arg("-ManifestPath")
        .arg(&manifest)
        .output()
        .map_err(|error| {
            HostError::new(
                "BUNDLE_DOWNLOAD_FAILED",
                &format!("The runtime bundle downloader could not start: {error}"),
            )
        })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.lines().last().unwrap_or("download failed");
        return Err(HostError::new(
            "BUNDLE_DOWNLOAD_FAILED",
            &format!("Runtime bundle download failed: {detail}"),
        ));
    }
    let media_ready = media_executables_from_bundle(&install_root).is_some();
    let host = state
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    host.media.set_executables(
        media_executables_from_environment()
            .or_else(|| media_executables_from_bundle(&install_root)),
    );
    let message = if media_ready {
        "Runtime bundle downloaded and FFmpeg/ffprobe verified."
    } else {
        "Bundle artifacts were verified; media runtime is not part of this profile."
    };
    Ok(BundleDownloadResponse {
        profile,
        install_root: install_root.to_string_lossy().into_owned(),
        media_ready,
        message: message.to_owned(),
    })
}

#[tauri::command]
fn host_status(state: State<'_, AppState>) -> HostStatus {
    let host = state
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    HostStatus {
        core: CapabilityStatus {
            state: "READY",
            reason: "Project domain and persistence APIs are available.",
        },
        media: if host.media.is_configured() {
            CapabilityStatus {
                state: "READY",
                reason: "A configured media runtime is available.",
            }
        } else {
            CapabilityStatus {
                state: "UNAVAILABLE",
                reason: "No verified media runtime is provisioned.",
            }
        },
        ai: CapabilityStatus {
            state: "UNAVAILABLE",
            reason: "No verified local AI runtime or model is provisioned.",
        },
        project_loaded: host.shared.current_document().is_ok(),
    }
}
#[tauri::command]
fn project_create(
    request: CreateProjectRequest,
    state: State<'_, AppState>,
) -> Result<ProjectDocument, HostError> {
    let project_id = request
        .project_id
        .map(ProjectId::new)
        .transpose()
        .map_err(HostError::from)?
        .unwrap_or_else(generated_project_id);
    let document = ProjectDocument::create(project_id, request.name).map_err(HostError::from)?;
    let path = request
        .project_path
        .as_deref()
        .map(|path| project_path(path, false))
        .transpose()?;
    if let Some(path) = &path {
        save_project(path, &document).map_err(HostError::from)?;
    }
    let mut host = state
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    set_loaded_project(&mut host, document.clone(), path);
    Ok(document)
}
#[tauri::command]
fn project_open(
    request: ProjectPathRequest,
    state: State<'_, AppState>,
) -> Result<ProjectDocument, HostError> {
    let path = project_path(&request.project_path, true)?;
    let document = load_project(&path).map_err(HostError::from)?;
    let mut host = state
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    set_loaded_project(&mut host, document.clone(), Some(path));
    Ok(document)
}
#[tauri::command]
fn project_save(
    request: SaveProjectRequest,
    state: State<'_, AppState>,
) -> Result<SaveProjectResponse, HostError> {
    let mut host = state
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let document = host.app.snapshot().map_err(HostError::from)?.document;
    let expected_revision = request
        .expected_revision
        .or(host.saved_revision)
        .unwrap_or(document.revision);
    let path = request
        .project_path
        .as_deref()
        .map(|path| project_path(path, false))
        .transpose()?
        .or(host.project_path.clone())
        .ok_or_else(|| {
            HostError::new(
                "PROJECT_PATH_REQUIRED",
                "A project path is required before saving.",
            )
        })?;
    let result =
        save_project_if_revision(&path, &document, expected_revision).map_err(HostError::from)?;
    host.media.set_root(
        result
            .project_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".")),
    );
    host.project_path = Some(result.project_path);
    host.saved_revision = Some(document.revision);
    Ok(SaveProjectResponse {
        bytes_written: result.bytes_written,
        backup_created: result.backup_path.is_some(),
    })
}
#[tauri::command]
fn asset_import(
    request: AssetImportRequest,
    state: State<'_, AppState>,
) -> Result<ProjectDocument, HostError> {
    if request.paths.is_empty()
        || request.paths.len() > MAX_IMPORT_PATHS
        || request
            .paths
            .iter()
            .any(|path| path.trim().is_empty() || path.len() > MAX_PATH_INPUT_BYTES)
    {
        return Err(invalid_request());
    }
    let mut host = state
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let current = host.app.snapshot().map_err(HostError::from)?.document;
    if !host.media.is_configured() {
        return Err(HostError::unavailable(
            "MEDIA_UNAVAILABLE",
            "No verified FFmpeg/ffprobe runtime is provisioned; no asset was imported.",
        ));
    }
    let assets = request
        .paths
        .iter()
        .map(|path| import_asset(&host, path))
        .collect::<Result<Vec<_>, _>>()?;
    if assets.iter().any(|asset| {
        current.assets.iter().any(|existing| {
            existing.id == asset.id || existing.relative_path == asset.relative_path
        })
    }) {
        return Err(HostError::new(
            "ASSET_ALREADY_IMPORTED",
            "One of the selected assets is already in the project.",
        ));
    }
    if assets
        .iter()
        .enumerate()
        .any(|(index, asset)| assets[index + 1..].iter().any(|other| other.id == asset.id))
    {
        return Err(HostError::new(
            "ASSET_DUPLICATE",
            "The import selection contains a duplicate asset.",
        ));
    }
    let mut document = current;
    for asset in assets {
        document = host
            .app
            .apply_timeline(document.revision, TimelineOperation::AddAsset { asset })
            .map_err(HostError::from)?
            .document;
    }
    Ok(document)
}
#[tauri::command]
fn timeline_apply(
    request: TimelineApplyRequest,
    state: State<'_, AppState>,
) -> Result<ApplyResult, HostError> {
    let mut host = state
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    host.app
        .apply_timeline(request.base_revision, request.operation)
        .map_err(HostError::from)
}
fn preview_unavailable() -> Result<(), HostError> {
    Err(HostError::unavailable(
        "PREVIEW_UNAVAILABLE",
        "No reviewed playback backend is provisioned.",
    ))
}
#[tauri::command]
fn preview_play() -> Result<(), HostError> {
    preview_unavailable()
}
#[tauri::command]
fn preview_pause() -> Result<(), HostError> {
    preview_unavailable()
}
#[tauri::command]
fn preview_seek(request: PreviewSeekRequest) -> Result<(), HostError> {
    if request.timeline_ticks < 0 {
        return Err(invalid_request());
    }
    preview_unavailable()
}
#[tauri::command]
fn export_start(
    request: ExportRequest,
    state: State<'_, AppState>,
) -> Result<JobResponse, HostError> {
    let host = state
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !host.media.is_configured() {
        return Err(HostError::unavailable(
            "MEDIA_UNAVAILABLE",
            "No reviewed FFmpeg/ffprobe runtime is provisioned; no export was created.",
        ));
    }
    let snapshot = host.app.snapshot().map_err(HostError::from)?;
    if let Some(expected) = request
        .base_revision
        .filter(|revision| *revision != snapshot.document.revision)
    {
        return Err(HostError::from(AppError::revision_conflict(
            expected,
            snapshot.document.revision,
        )));
    }
    let output = RelativePath::new(request.output_path).map_err(HostError::from)?;
    let id = host
        .app
        .start_media_job_async(JobKind::Export, Some(output))
        .map_err(HostError::from)?;
    host.app
        .job(id)
        .map(JobResponse::from)
        .map_err(HostError::from)
}
#[tauri::command]
fn export(request: ExportRequest, state: State<'_, AppState>) -> Result<JobResponse, HostError> {
    export_start(request, state)
}
#[tauri::command]
fn job_get(request: JobRequest, state: State<'_, AppState>) -> Result<JobResponse, HostError> {
    let host = state
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    host.app
        .job(parse_job_id(&request.job_id)?)
        .map(JobResponse::from)
        .map_err(HostError::from)
}
#[tauri::command]
fn job_list(state: State<'_, AppState>) -> Vec<JobResponse> {
    let host = state
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    host.app
        .jobs()
        .list()
        .into_iter()
        .map(JobResponse::from)
        .collect()
}
#[tauri::command]
fn job_cancel(request: JobRequest, state: State<'_, AppState>) -> Result<JobResponse, HostError> {
    let host = state
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    host.app
        .cancel_job(parse_job_id(&request.job_id)?)
        .map(JobResponse::from)
        .map_err(HostError::from)
}
#[tauri::command]
fn assistant_plan(
    request: AssistantPlanRequest,
    state: State<'_, AppState>,
) -> Result<(), HostError> {
    if request.text.trim().is_empty() || request.text.len() > 64 * 1024 {
        return Err(invalid_request());
    }
    let host = state
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let snapshot = host.app.snapshot().map_err(HostError::from)?;
    if request.base_revision != snapshot.document.revision {
        return Err(HostError::from(AppError::revision_conflict(
            request.base_revision,
            snapshot.document.revision,
        )));
    }
    Err(HostError::unavailable(
        "AI_UNAVAILABLE",
        "No configured local language-model provider or verified model is available.",
    ))
}

fn main() {
    let shared = SharedProjectPort::new();
    let media_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let media = configured_media_port(media_root);
    let app = EditorApplication::new(
        shared.clone(),
        media.clone(),
        editor_jobs::JobRegistry::with_defaults(),
    );
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState(Mutex::new(HostState {
            shared,
            media,
            app,
            project_path: None,
            saved_revision: None,
        })))
        .invoke_handler(tauri::generate_handler![
            bundle_download,
            host_status,
            project_create,
            project_open,
            project_save,
            asset_import,
            timeline_apply,
            preview_play,
            preview_pause,
            preview_seek,
            export,
            export_start,
            job_get,
            job_list,
            job_cancel,
            assistant_plan
        ])
        .run(tauri::generate_context!())
        .expect("error while running VideoEditorFree");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_job_id_is_not_found() {
        assert_eq!(parse_job_id("nope").unwrap_err().code, "JOB_NOT_FOUND");
    }
    #[test]
    fn relative_path_rejects_absolute_export() {
        assert!(RelativePath::new(r"C:\out.mp4").is_err());
    }
}
