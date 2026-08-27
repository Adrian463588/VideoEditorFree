#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use editor_ai::{
    parse_edit_plan_json, parse_whisper_json, validate_plan_for_apply, ClipContext,
    ModelProvenance, ProjectContext,
};
use editor_app::{ConfiguredMediaPort, EditorApplication, ProjectPort};
use editor_domain::{
    ApplyResult, Asset, AssetId, AssetKind, AssetStatus, AudioStream, Clip, ClipId, DomainError,
    Fingerprint, ProbeSummary, ProjectDocument, ProjectId, Rational, RelativePath,
    TimelineOperation, Track, TrackId, TrackKind, Transform, VideoStream,
};
use editor_jobs::{AppError, JobId, JobKind, JobRecord};
use editor_media::{
    BinaryContract, BinaryManifest, ChildProcessRunner, ExecutableConfig, ProbeMetadata,
    SystemChildProcessRunner,
};
use editor_project::{load_project, save_project, save_project_if_revision, validate_project_path};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant, UNIX_EPOCH},
};
use tauri::{path::BaseDirectory, AppHandle, Manager, State};

const MAX_PATH_INPUT_BYTES: usize = 32 * 1024;
const MAX_IMPORT_PATHS: usize = 256;
const MAX_LLM_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_LLM_PROMPT_BYTES: usize = 24 * 1024;
const MAX_COMMAND_ERROR_BYTES: usize = 64 * 1024;
const MAX_TRANSCRIPT_JSON_BYTES: usize = 16 * 1024 * 1024;
const LOCAL_LLM_TIMEOUT: Duration = Duration::from_secs(90);
const LOCAL_LLM_JSON_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "base_revision": {"type": "integer", "minimum": 0},
    "operations": {
      "type": "array",
      "minItems": 1,
      "maxItems": 32,
      "items": {
        "oneOf": [
          {"type": "object", "properties": {"op": {"enum": ["trim"]}, "clip_id": {"type": "string", "minLength": 1}, "source_start": {"type": "integer", "minimum": 0}, "source_end": {"type": "integer", "minimum": 1}}, "required": ["op", "clip_id", "source_start", "source_end"], "additionalProperties": false},
          {"type": "object", "properties": {"op": {"enum": ["split"]}, "clip_id": {"type": "string", "minLength": 1}, "at_timeline_tick": {"type": "integer", "minimum": 0}}, "required": ["op", "clip_id", "at_timeline_tick"], "additionalProperties": false},
          {"type": "object", "properties": {"op": {"enum": ["delete"]}, "clip_id": {"type": "string", "minLength": 1}}, "required": ["op", "clip_id"], "additionalProperties": false},
          {"type": "object", "properties": {"op": {"enum": ["ripple_delete"]}, "clip_id": {"type": "string", "minLength": 1}, "track_id": {"type": "string", "minLength": 1}}, "required": ["op", "clip_id", "track_id"], "additionalProperties": false},
          {"type": "object", "properties": {"op": {"enum": ["reorder"]}, "clip_id": {"type": "string", "minLength": 1}, "track_id": {"type": "string", "minLength": 1}, "new_index": {"type": "integer", "minimum": 0}}, "required": ["op", "clip_id", "track_id", "new_index"], "additionalProperties": false},
          {"type": "object", "properties": {"op": {"enum": ["effect_preset"]}, "clip_id": {"type": "string", "minLength": 1}, "preset": {"enum": ["warm_color_grade", "cool_color_grade", "monochrome"]}}, "required": ["op", "clip_id", "preset"], "additionalProperties": false}
        ]
      }
    },
    "warnings": {"type": "array", "maxItems": 32, "items": {"type": "string"}},
    "affected_clips": {"type": "array", "maxItems": 32, "items": {"type": "string", "minLength": 1}},
    "requires_confirmation": {"type": "boolean", "enum": [true]}
  },
  "required": ["base_revision", "operations", "warnings", "affected_clips", "requires_confirmation"],
  "additionalProperties": false
}"#;
const STT_LANGUAGES: &[&str] = &[
    "auto", "en", "id", "es", "fr", "de", "it", "pt", "ja", "ko", "zh", "ru", "ar", "hi", "nl",
    "tr", "pl", "uk", "vi",
];
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
    subtitles: CapabilityStatus,
    tts: CapabilityStatus,
    effects: CapabilityStatus,
    audio_ducking: CapabilityStatus,
    export_profiles: CapabilityStatus,
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
    fn retryable(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: true,
            details: None,
        }
    }
}
impl From<HostError> for AppError {
    fn from(error: HostError) -> Self {
        Self::new(error.code, error.message, error.retryable, error.details)
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
            editor_project::ProjectError::SizeLimit { .. } => Self::new(
                "PROJECT_TOO_LARGE",
                "The project file exceeds the safe memory limit.",
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
    profile: String,
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
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubtitleGenerateRequest {
    asset_id: String,
    language: String,
    base_revision: u64,
    track_id: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TtsGenerateRequest {
    text: String,
    voice_id: String,
    base_revision: u64,
    track_id: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AssistantApplyRequest {
    plan: serde_json::Value,
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
#[serde(rename_all = "camelCase")]
struct SubtitleGenerationResponse {
    job: JobResponse,
    document: ProjectDocument,
    language: String,
    cue_count: usize,
    message: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TtsGenerationResponse {
    job: JobResponse,
    document: ProjectDocument,
    voice_id: String,
    relative_path: String,
    message: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AssistantPlanResponse {
    plan: editor_ai::EditPlan,
    provenance: ModelProvenance,
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

fn bundle_root_from_base(base: Option<PathBuf>) -> PathBuf {
    base.map(|path| path.join("VideoEditorFree").join("runtime"))
        .unwrap_or_else(|| PathBuf::from(".videoeditorfree").join("runtime"))
}

fn default_bundle_root() -> PathBuf {
    bundle_root_from_base(
        std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("APPDATA"))
            .map(PathBuf::from),
    )
}

fn media_executables_from_manifest(
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    manifest_path: PathBuf,
) -> Option<ExecutableConfig> {
    let bytes = std::fs::read(manifest_path).ok()?;
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    let manifest = serde_json::from_slice::<BinaryManifest>(bytes).ok()?;
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

fn subtitle_runtime_paths(root: &Path) -> (PathBuf, PathBuf) {
    (
        root.join("ai").join("whisper").join("whisper-cli.exe"),
        root.join("models").join("ggml-tiny.bin"),
    )
}

fn subtitle_runtime_ready(root: &Path) -> bool {
    let (executable, model) = subtitle_runtime_paths(root);
    executable.is_file() && model.is_file()
}

fn tts_runtime_paths(root: &Path, voice_id: &str) -> (PathBuf, PathBuf) {
    (
        root.join("ai").join("piper").join("piper.exe"),
        root.join("voices").join(format!("{voice_id}.onnx")),
    )
}

fn tts_runtime_ready(root: &Path, voice_id: &str) -> bool {
    let (executable, voice) = tts_runtime_paths(root, voice_id);
    executable.is_file() && voice.is_file() && voice.with_extension("onnx.json").is_file()
}

fn llm_runtime_paths(root: &Path) -> (PathBuf, PathBuf) {
    (
        root.join("ai").join("llama").join("llama-cli.exe"),
        root.join("models")
            .join("qwen2.5-0.5b-instruct-q4_k_m.gguf"),
    )
}

fn llm_runtime_ready(root: &Path) -> bool {
    let (executable, model) = llm_runtime_paths(root);
    executable.is_file() && model.is_file()
}

fn validate_tts_voice(voice_id: &str) -> Result<(), HostError> {
    if voice_id != "en_US-lessac-medium" {
        return Err(HostError::new(
            "INVALID_REQUEST",
            "The requested Piper voice is not present in the verified bundle.",
        ));
    }
    Ok(())
}

fn configured_media_executables() -> Option<ExecutableConfig> {
    media_executables_from_environment()
        .or_else(|| media_executables_from_bundle(&default_bundle_root()))
}

fn validate_stt_language(language: &str) -> Result<(), HostError> {
    if !STT_LANGUAGES.contains(&language) {
        return Err(HostError::new(
            "INVALID_REQUEST",
            "The subtitle language is not supported by the bundled Whisper model.",
        ));
    }
    Ok(())
}

fn finish_subtitle_job_error(jobs: &editor_jobs::JobRegistry, id: JobId, error: &HostError) {
    if error.code == "AI_TRANSCRIPTION_CANCELLED" {
        let _ = jobs.mark_cancelled(id);
    } else {
        let _ = jobs.mark_failed(id, AppError::from(error.clone()));
    }
}

fn bundle_script_path(app: &AppHandle) -> Result<PathBuf, HostError> {
    let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../scripts/runtime/download-bundle.ps1");
    if development.is_file() {
        return Ok(development);
    }
    for relative in [
        "runtime/download-bundle.ps1",
        "scripts/runtime/download-bundle.ps1",
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
        "runtime/bundle-manifest.json",
        "resources/runtime/bundle-manifest.json",
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
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" | "ts" | "mts" | "m2ts" | "3gp" | "flv"
        | "wmv" | "mpeg" | "mpg" | "ogv" => AssetKind::Video,
        "wav" | "mp3" | "m4a" | "flac" | "ogg" | "aac" | "opus" | "aiff" | "aif" | "mka"
        | "ac3" | "wma" | "amr" => AssetKind::Audio,
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

fn copy_external_asset(source: &Path, root: &Path) -> Result<PathBuf, HostError> {
    let root = root.canonicalize().map_err(|_| {
        HostError::new(
            "MEDIA_COPY_FAILED",
            "The project root could not be resolved for media import.",
        )
    })?;
    let file_name = source
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            HostError::new("MEDIA_PATH_INVALID", "The imported asset has no file name.")
        })?;
    let media_root = root.join("media");
    std::fs::create_dir_all(&media_root).map_err(|_| {
        HostError::new(
            "MEDIA_COPY_FAILED",
            "The project media directory could not be created.",
        )
    })?;
    let media_root = media_root.canonicalize().map_err(|_| {
        HostError::new(
            "MEDIA_COPY_FAILED",
            "The project media directory could not be resolved.",
        )
    })?;
    if !media_root.starts_with(root) {
        return Err(HostError::new(
            "INVALID_MEDIA_PATH",
            "The project media directory is outside the project root.",
        ));
    }
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("asset");
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    let target = (0..10_000)
        .map(|index| {
            let candidate = if index == 0 {
                file_name.to_owned()
            } else {
                format!("{stem}-{index}{extension}")
            };
            media_root.join(candidate)
        })
        .find(|candidate| !candidate.exists())
        .ok_or_else(|| {
            HostError::new("MEDIA_COPY_FAILED", "No safe media file name is available.")
        })?;
    if !target.starts_with(&media_root) {
        return Err(HostError::new(
            "INVALID_MEDIA_PATH",
            "The imported asset destination is outside the project.",
        ));
    }
    let temporary = target.with_file_name(format!(".{file_name}.importing"));
    if let Err(error) = std::fs::copy(source, &temporary) {
        let _ = std::fs::remove_file(&temporary);
        return Err(HostError::new(
            "MEDIA_COPY_FAILED",
            &format!("The selected asset could not be copied into the project: {error}"),
        ));
    }
    if let Err(error) = std::fs::rename(&temporary, &target) {
        let _ = std::fs::remove_file(&temporary);
        return Err(HostError::new(
            "MEDIA_COPY_FAILED",
            &format!("The copied asset could not be finalized: {error}"),
        ));
    }
    Ok(target)
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
    let input = if input.starts_with(&root) {
        input
    } else {
        copy_external_asset(&input, &root)?
    };
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

fn format_srt_timestamp(milliseconds: i64) -> String {
    let total = milliseconds.max(0);
    let hours = total / 3_600_000;
    let minutes = (total / 60_000) % 60;
    let seconds = (total / 1_000) % 60;
    let millis = total % 1_000;
    format!("{hours:02}:{minutes:02}:{seconds:02},{millis:03}")
}

fn transcript_to_srt(transcript: &editor_ai::Transcript) -> String {
    transcript
        .cues
        .iter()
        .enumerate()
        .map(|(index, cue)| {
            format!(
                "{}\n{} --> {}\n{}\n",
                index + 1,
                format_srt_timestamp(cue.range.start),
                format_srt_timestamp(cue.range.end),
                cue.text
            )
        })
        .collect()
}

fn milliseconds_to_ticks(milliseconds: i64, timebase: Rational) -> Result<i64, HostError> {
    if milliseconds <= 0 {
        return Err(HostError::new(
            "AI_TRANSCRIPTION_FAILED",
            "The transcript has no positive duration.",
        ));
    }
    let ticks = (milliseconds as f64 * timebase.numerator as f64
        / (timebase.denominator as f64 * 1_000.0))
        .ceil();
    if !ticks.is_finite() || ticks > i64::MAX as f64 || ticks <= 0.0 {
        return Err(HostError::new(
            "AI_TRANSCRIPTION_FAILED",
            "The transcript duration is outside the project timebase range.",
        ));
    }
    Ok(ticks as i64)
}

fn whisper_transcribe(
    source: &Path,
    config: &ExecutableConfig,
    whisper: &Path,
    model: &Path,
    language: &str,
    job_id: JobId,
    cancelled: &dyn Fn() -> bool,
) -> Result<editor_ai::Transcript, HostError> {
    if !source.is_file() || !whisper.is_file() || !model.is_file() {
        return Err(HostError::unavailable(
            "SUBTITLE_RUNTIME_UNAVAILABLE",
            "The verified whisper.cpp runtime or multilingual model is not provisioned.",
        ));
    }
    let temporary_root = std::env::temp_dir().join(format!(
        "videoeditorfree-stt-{}-{}",
        std::process::id(),
        job_id
    ));
    std::fs::create_dir_all(&temporary_root).map_err(|error| {
        HostError::new(
            "AI_TRANSCRIPTION_FAILED",
            &format!("The subtitle temporary directory could not be created: {error}"),
        )
    })?;
    let result = (|| {
        let wav = temporary_root.join("audio.wav");
        let output_prefix = temporary_root.join("transcript");
        let extract_args = vec![
            "-nostdin".into(),
            "-y".into(),
            "-v".into(),
            "error".into(),
            "-i".into(),
            source.to_string_lossy().into(),
            "-map".into(),
            "0:a:0".into(),
            "-vn".into(),
            "-sn".into(),
            "-dn".into(),
            "-ar".into(),
            "16000".into(),
            "-ac".into(),
            "1".into(),
            "-c:a".into(),
            "pcm_s16le".into(),
            "-f".into(),
            "wav".into(),
            wav.to_string_lossy().into(),
        ];
        let extracted = SystemChildProcessRunner
            .run(&config.ffmpeg, &extract_args)
            .map_err(|error| HostError::new("AI_TRANSCRIPTION_FAILED", &error.to_string()))?;
        if extracted.status_code != Some(0) {
            return Err(HostError::new(
                "AI_TRANSCRIPTION_FAILED",
                "FFmpeg could not extract an audio stream for subtitle generation.",
            ));
        }
        if cancelled() {
            return Err(HostError::unavailable(
                "AI_TRANSCRIPTION_CANCELLED",
                "Subtitle generation was cancelled.",
            ));
        }
        let whisper_args: Vec<String> = vec![
            "-m".into(),
            model.to_string_lossy().into(),
            "-f".into(),
            wav.to_string_lossy().into(),
            "-l".into(),
            language.into(),
            "-oj".into(),
            "-np".into(),
            "-of".into(),
            output_prefix.to_string_lossy().into(),
        ];
        let mut process = Command::new(whisper)
            .args(&whisper_args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|error| {
                HostError::new(
                    "AI_TRANSCRIPTION_FAILED",
                    &format!("The whisper.cpp process could not start: {error}"),
                )
            })?;
        let status = loop {
            if cancelled() {
                let _ = process.kill();
                let _ = process.wait();
                return Err(HostError::unavailable(
                    "AI_TRANSCRIPTION_CANCELLED",
                    "Subtitle generation was cancelled.",
                ));
            }
            match process.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(error) => {
                    let _ = process.kill();
                    let _ = process.wait();
                    return Err(HostError::new(
                        "AI_TRANSCRIPTION_FAILED",
                        &format!("The whisper.cpp process could not be monitored: {error}"),
                    ));
                }
            }
        };
        if !status.success() {
            return Err(HostError::new(
                "AI_TRANSCRIPTION_FAILED",
                "whisper.cpp could not transcribe the selected audio.",
            ));
        }
        let json_path = output_prefix.with_extension("json");
        let file = std::fs::File::open(&json_path).map_err(|error| {
            HostError::new(
                "AI_TRANSCRIPTION_FAILED",
                &format!("whisper.cpp did not produce a transcript JSON file: {error}"),
            )
        })?;
        let mut json_bytes = Vec::with_capacity(64 * 1024);
        file.take((MAX_TRANSCRIPT_JSON_BYTES + 1) as u64)
            .read_to_end(&mut json_bytes)
            .map_err(|error| {
                HostError::new(
                    "AI_TRANSCRIPTION_FAILED",
                    &format!("The transcript JSON file could not be read: {error}"),
                )
            })?;
        if json_bytes.len() > MAX_TRANSCRIPT_JSON_BYTES {
            return Err(HostError::new(
                "AI_TRANSCRIPTION_OUTPUT_LIMIT",
                "The whisper.cpp transcript exceeded the 16 MiB output limit.",
            ));
        }
        let json = String::from_utf8(json_bytes).map_err(|_| {
            HostError::new(
                "AI_TRANSCRIPTION_FAILED",
                "whisper.cpp returned transcript JSON that is not valid UTF-8.",
            )
        })?;
        parse_whisper_json(
            &json,
            language,
            ModelProvenance {
                provider: "whisper.cpp".into(),
                model_id: "ggml-tiny".into(),
                model_version: "v1.9.0".into(),
            },
        )
        .map_err(|error| HostError::new("AI_TRANSCRIPTION_FAILED", &error.to_string()))
    })();
    let _ = std::fs::remove_dir_all(&temporary_root);
    result
}

fn write_generated_srt(
    root: &Path,
    job_id: JobId,
    language: &str,
    transcript: &editor_ai::Transcript,
) -> Result<(RelativePath, Asset), HostError> {
    let media_root = root.join("media");
    std::fs::create_dir_all(&media_root).map_err(|error| {
        HostError::new(
            "AI_TRANSCRIPTION_FAILED",
            &format!("The project media directory could not be created: {error}"),
        )
    })?;
    let file_name = generated_subtitle_file_name(job_id, language);
    let target = media_root.join(&file_name);
    let temporary = media_root.join(format!(".{file_name}.part"));
    let content = transcript_to_srt(transcript);
    std::fs::write(&temporary, content.as_bytes()).map_err(|error| {
        HostError::new(
            "AI_TRANSCRIPTION_FAILED",
            &format!("The generated subtitle file could not be written: {error}"),
        )
    })?;
    if let Err(error) = std::fs::rename(&temporary, &target) {
        let _ = std::fs::remove_file(&temporary);
        return Err(HostError::new(
            "AI_TRANSCRIPTION_FAILED",
            &format!("The generated subtitle file could not be finalized: {error}"),
        ));
    }
    let relative = RelativePath::new(format!("media/{file_name}")).map_err(HostError::from)?;
    let metadata = std::fs::metadata(&target).map_err(|error| {
        HostError::new(
            "AI_TRANSCRIPTION_FAILED",
            &format!("The generated subtitle metadata is unavailable: {error}"),
        )
    })?;
    let modified_time = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_secs().to_string())
        .unwrap_or_else(|| "unknown".into());
    let asset = Asset {
        id: stable_asset_id(relative.as_str())?,
        relative_path: relative.clone(),
        kind: AssetKind::Subtitle,
        fingerprint: Fingerprint {
            size_bytes: metadata.len(),
            modified_time,
            sha256: None,
        },
        probe: None,
        status: AssetStatus::Available,
    };
    Ok((relative, asset))
}

fn generated_subtitle_file_name(job_id: JobId, language: &str) -> String {
    format!("auto-subtitles-{job_id}-{language}.srt")
}

fn cleanup_generated_srt(root: &Path, job_id: JobId, language: &str) {
    let file_name = generated_subtitle_file_name(job_id, language);
    let target = root.join("media").join(&file_name);
    let _ = std::fs::remove_file(target);
    let _ = std::fs::remove_file(root.join("media").join(format!(".{file_name}.part")));
}

fn generated_tts_file_name(job_id: JobId) -> String {
    format!("tts-voiceover-{job_id}.wav")
}

fn cleanup_generated_tts(root: &Path, job_id: JobId) {
    let file_name = generated_tts_file_name(job_id);
    let media = root.join("media");
    let _ = std::fs::remove_file(media.join(&file_name));
    let _ = std::fs::remove_file(media.join(format!(".{file_name}.part")));
}

fn piper_synthesize(
    text: &str,
    piper: &Path,
    voice: &Path,
    output: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), HostError> {
    if !piper.is_file() || !voice.is_file() || !voice.with_extension("onnx.json").is_file() {
        return Err(HostError::unavailable(
            "TTS_RUNTIME_UNAVAILABLE",
            "The verified Piper runtime or voice is not provisioned.",
        ));
    }
    let mut process = Command::new(piper)
        .arg("--model")
        .arg(voice)
        .arg("--output_file")
        .arg(output)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            HostError::new(
                "TTS_GENERATION_FAILED",
                &format!("The Piper process could not start: {error}"),
            )
        })?;
    if let Some(mut stdin) = process.stdin.take() {
        if let Err(error) = stdin.write_all(text.as_bytes()) {
            let _ = process.kill();
            let _ = process.wait();
            return Err(HostError::new(
                "TTS_GENERATION_FAILED",
                &format!("Text could not be sent to Piper: {error}"),
            ));
        }
    }
    let status = loop {
        if cancelled() {
            let _ = process.kill();
            let _ = process.wait();
            return Err(HostError::unavailable(
                "TTS_GENERATION_CANCELLED",
                "Voiceover generation was cancelled.",
            ));
        }
        match process.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(error) => {
                let _ = process.kill();
                let _ = process.wait();
                return Err(HostError::new(
                    "TTS_GENERATION_FAILED",
                    &format!("The Piper process could not be monitored: {error}"),
                ));
            }
        }
    };
    if !status.success() {
        return Err(HostError::new(
            "TTS_GENERATION_FAILED",
            "Piper could not synthesize the requested voiceover.",
        ));
    }
    if !output.is_file()
        || std::fs::metadata(output)
            .map(|metadata| metadata.len() == 0)
            .unwrap_or(true)
    {
        return Err(HostError::new(
            "TTS_GENERATION_FAILED",
            "Piper completed without producing a non-empty WAV file.",
        ));
    }
    Ok(())
}

fn generated_tts_asset(
    root: &Path,
    job_id: JobId,
    probe: ProbeSummary,
) -> Result<(RelativePath, Asset), HostError> {
    let file_name = generated_tts_file_name(job_id);
    let target = root.join("media").join(&file_name);
    let relative = RelativePath::new(format!("media/{file_name}")).map_err(HostError::from)?;
    let metadata = std::fs::metadata(&target).map_err(|error| {
        HostError::new(
            "TTS_GENERATION_FAILED",
            &format!("The generated voiceover metadata is unavailable: {error}"),
        )
    })?;
    let modified_time = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_secs().to_string())
        .unwrap_or_else(|| "unknown".into());
    let asset = Asset {
        id: stable_asset_id(relative.as_str())?,
        relative_path: relative.clone(),
        kind: AssetKind::Audio,
        fingerprint: Fingerprint {
            size_bytes: metadata.len(),
            modified_time,
            sha256: None,
        },
        probe: Some(probe),
        status: AssetStatus::Available,
    };
    Ok((relative, asset))
}

fn generated_audio_track(
    document: &ProjectDocument,
    requested_track: Option<&str>,
    job_id: JobId,
) -> Result<(TrackId, Option<Track>), HostError> {
    if let Some(raw_id) = requested_track {
        let track_id = TrackId::new(raw_id.to_owned()).map_err(HostError::from)?;
        let track = document
            .sequence
            .tracks
            .iter()
            .find(|track| track.id == track_id)
            .ok_or_else(|| HostError::new("ENTITY_NOT_FOUND", "The audio track was not found."))?;
        if !matches!(track.kind, TrackKind::Audio) {
            return Err(HostError::new(
                "INVALID_REQUEST",
                "Voiceover generation requires an audio track.",
            ));
        }
        if track.locked {
            return Err(HostError::new(
                "TRACK_LOCKED",
                "The selected audio track is locked.",
            ));
        }
        return Ok((track_id, None));
    }
    let track_id = TrackId::new(format!("voiceover-layer-{job_id}")).map_err(HostError::from)?;
    let track = Track::new(
        track_id.clone(),
        TrackKind::Audio,
        format!("Voiceover {job_id}"),
    )
    .map_err(HostError::from)?;
    Ok((track_id, Some(track)))
}

fn seconds_to_ticks(seconds: f64, timebase: Rational, code: &str) -> Result<i64, HostError> {
    let ticks = (seconds * timebase.numerator as f64 / timebase.denominator as f64).round();
    if !ticks.is_finite() || ticks <= 0.0 || ticks > i64::MAX as f64 {
        return Err(HostError::new(
            code,
            "The generated media duration is out of range.",
        ));
    }
    Ok(ticks as i64)
}

fn generated_subtitle_track(
    document: &ProjectDocument,
    requested_track: Option<&str>,
    job_id: JobId,
) -> Result<(TrackId, Option<Track>), HostError> {
    if let Some(raw_id) = requested_track {
        let track_id = TrackId::new(raw_id.to_owned()).map_err(HostError::from)?;
        let track = document
            .sequence
            .tracks
            .iter()
            .find(|track| track.id == track_id)
            .ok_or_else(|| {
                HostError::new("ENTITY_NOT_FOUND", "The subtitle track was not found.")
            })?;
        if !matches!(track.kind, TrackKind::Subtitle) {
            return Err(HostError::new(
                "INVALID_REQUEST",
                "Subtitle generation requires a subtitle track.",
            ));
        }
        if track.locked {
            return Err(HostError::new(
                "TRACK_LOCKED",
                "The selected subtitle track is locked.",
            ));
        }
        return Ok((track_id, None));
    }
    let track_id = TrackId::new(format!("subtitle-layer-{job_id}")).map_err(HostError::from)?;
    let track = Track::new(
        track_id.clone(),
        TrackKind::Subtitle,
        format!("Subtitles {job_id}"),
    )
    .map_err(HostError::from)?;
    Ok((track_id, Some(track)))
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
    let revision = document.revision;
    host.shared.replace(document);
    host.app.invalidate_snapshot_cache();
    host.media.set_root(
        path.as_deref()
            .and_then(Path::parent)
            .unwrap_or_else(|| Path::new(".")),
    );
    host.project_path = path;
    host.saved_revision = Some(revision);
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
    if !matches!(profile.as_str(), "core" | "subtitles" | "ai" | "all") {
        return Err(invalid_request());
    }
    let script = bundle_script_path(&app)?;
    let manifest = bundle_manifest_path(&app)?;
    let install_root = default_bundle_root();
    let mut process = Command::new("powershell.exe")
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
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            HostError::new(
                "BUNDLE_DOWNLOAD_FAILED",
                &format!("The runtime bundle downloader could not start: {error}"),
            )
        })?;
    let stderr = process.stderr.take().ok_or_else(|| {
        let _ = process.kill();
        let _ = process.wait();
        HostError::new(
            "BUNDLE_DOWNLOAD_FAILED",
            "The runtime bundle downloader did not expose an error pipe.",
        )
    })?;
    let stderr_thread = thread::spawn(move || {
        read_bounded_pipe(
            stderr,
            MAX_COMMAND_ERROR_BYTES,
            Arc::new(AtomicBool::new(false)),
        )
    });
    let status = match process.wait() {
        Ok(status) => status,
        Err(error) => {
            let _ = process.kill();
            let _ = process.wait();
            let _ = stderr_thread.join();
            return Err(HostError::new(
                "BUNDLE_DOWNLOAD_FAILED",
                &format!("The runtime bundle downloader failed: {error}"),
            ));
        }
    };
    let stderr = stderr_thread
        .join()
        .map_err(|_| {
            HostError::new(
                "BUNDLE_DOWNLOAD_FAILED",
                "The runtime bundle downloader error reader failed.",
            )
        })?
        .map_err(|error| {
            HostError::new(
                "BUNDLE_DOWNLOAD_FAILED",
                &format!("The runtime bundle downloader error pipe failed: {error}"),
            )
        })?;
    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr);
        let detail = detail.lines().last().unwrap_or("download failed");
        return Err(HostError::new(
            "BUNDLE_DOWNLOAD_FAILED",
            &format!("Runtime bundle download failed: {detail}"),
        ));
    }
    let media_ready = media_executables_from_bundle(&install_root).is_some();
    let subtitle_ready = subtitle_runtime_ready(&install_root);
    let host = state
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    host.media.set_executables(
        media_executables_from_environment()
            .or_else(|| media_executables_from_bundle(&install_root)),
    );
    let message = if media_ready {
        if subtitle_ready {
            "Runtime bundle downloaded; FFmpeg/ffprobe and subtitle runtime verified."
        } else {
            "Runtime bundle downloaded and FFmpeg/ffprobe verified."
        }
    } else {
        if subtitle_ready {
            "Subtitle AI bundle downloaded and verified."
        } else {
            "Bundle artifacts were verified; media runtime is not part of this profile."
        }
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
    let bundle_root = default_bundle_root();
    let media_ready = host.media.is_configured();
    let ai_ready = llm_runtime_ready(&bundle_root);
    let subtitle_ready = subtitle_runtime_ready(&bundle_root);
    let tts_ready = tts_runtime_ready(&bundle_root, "en_US-lessac-medium");
    HostStatus {
        core: CapabilityStatus {
            state: "READY",
            reason: "Project domain and persistence APIs are available.",
        },
        media: if media_ready {
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
            state: if ai_ready {
                "READY"
            } else {
                "UNAVAILABLE"
            },
            reason: if ai_ready {
                "The local llama.cpp planner and Qwen model are available."
            } else {
                "Download and verify the AI bundle to enable the local edit planner."
            },
        },
        subtitles: if subtitle_ready {
            CapabilityStatus {
                state: "READY",
                reason: "The local multilingual Whisper subtitle runtime is available.",
            }
        } else {
            CapabilityStatus {
                state: "UNAVAILABLE",
                reason: "Download and verify the Subtitle AI bundle to generate local captions.",
            }
        },
        tts: if tts_ready {
            CapabilityStatus {
                state: "READY",
                reason: "The local Piper runtime and verified en_US voice are available.",
            }
        } else {
            CapabilityStatus {
                state: "UNAVAILABLE",
                reason: "Download and verify the AI bundle to generate local voiceovers.",
            }
        },
        effects: CapabilityStatus {
            state: "READY",
            reason: "Typed CPU video and audio effects are available; no measured GPU backend is claimed.",
        },
        audio_ducking: CapabilityStatus {
            state: "READY",
            reason: "Typed sidechain ducking is available in the timeline and export planner.",
        },
        export_profiles: if media_ready {
            CapabilityStatus {
                state: "READY",
                reason: "YouTube, Instagram Reels, and TikTok MP4 presets are available.",
            }
        } else {
            CapabilityStatus {
                state: "UNAVAILABLE",
                reason: "Download and verify the Core bundle before exporting.",
            }
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
    if host.project_path.is_none() {
        return Err(HostError::new(
            "PROJECT_PATH_REQUIRED",
            "Create or save a project before importing media.",
        ));
    }
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
fn subtitle_generate(
    request: SubtitleGenerateRequest,
    state: State<'_, AppState>,
) -> Result<SubtitleGenerationResponse, HostError> {
    validate_stt_language(&request.language)?;
    if request.asset_id.trim().is_empty() {
        return Err(invalid_request());
    }
    let _ai_slot = acquire_local_ai_slot()?;
    let (shared, jobs, snapshot, source, config, whisper, model, root, handle) = {
        let host = state
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if host.project_path.is_none() {
            return Err(HostError::new(
                "PROJECT_PATH_REQUIRED",
                "Create or save a project before generating subtitles.",
            ));
        }
        let snapshot = host.app.snapshot().map_err(HostError::from)?;
        if request.base_revision != snapshot.document.revision {
            return Err(HostError::from(AppError::revision_conflict(
                request.base_revision,
                snapshot.document.revision,
            )));
        }
        let asset = snapshot
            .document
            .assets
            .iter()
            .find(|asset| asset.id.as_str() == request.asset_id)
            .ok_or_else(|| {
                HostError::new(
                    "ENTITY_NOT_FOUND",
                    "The selected media asset was not found.",
                )
            })?;
        if !matches!(asset.kind, AssetKind::Audio | AssetKind::Video) {
            return Err(HostError::new(
                "INVALID_REQUEST",
                "Subtitle generation requires a video or audio asset.",
            ));
        }
        let root = project_root(&host)
            .canonicalize()
            .map_err(|_| HostError::new("PROJECT_IO_FAILED", "The project root is unavailable."))?;
        let source = editor_media::resolve_safe_path(&root, asset.relative_path.as_str())
            .map_err(|error| HostError::new("MEDIA_PATH_INVALID", &error.to_string()))?;
        let config = configured_media_executables().ok_or_else(|| {
            HostError::unavailable(
                "MEDIA_UNAVAILABLE",
                "No verified FFmpeg/ffprobe runtime is provisioned.",
            )
        })?;
        let (whisper, model) = subtitle_runtime_paths(&default_bundle_root());
        if !whisper.is_file() || !model.is_file() {
            return Err(HostError::unavailable(
                "SUBTITLE_RUNTIME_UNAVAILABLE",
                "Download and verify the Subtitle AI bundle before generating subtitles.",
            ));
        }
        let handle = host
            .app
            .jobs()
            .submit(JobKind::Stt, snapshot.metadata.clone())
            .map_err(HostError::from)?;
        host.app.jobs().start(handle.id).map_err(HostError::from)?;
        (
            host.shared.clone(),
            host.app.jobs().clone(),
            snapshot,
            source,
            config,
            whisper,
            model,
            root,
            handle,
        )
    };

    let transcript = match whisper_transcribe(
        &source,
        &config,
        &whisper,
        &model,
        &request.language,
        handle.id,
        &|| handle.token.is_cancelled(),
    ) {
        Ok(transcript) => transcript,
        Err(error) => {
            finish_subtitle_job_error(&jobs, handle.id, &error);
            return Err(error);
        }
    };
    if handle.token.is_cancelled() {
        let _ = jobs.mark_cancelled(handle.id);
        return Err(HostError::unavailable(
            "AI_TRANSCRIPTION_CANCELLED",
            "Subtitle generation was cancelled.",
        ));
    }
    let postprocess = (|| -> Result<ProjectDocument, HostError> {
        if handle.token.is_cancelled() {
            return Err(HostError::unavailable(
                "AI_TRANSCRIPTION_CANCELLED",
                "Subtitle generation was cancelled.",
            ));
        }
        let (_, subtitle_asset) =
            write_generated_srt(&root, handle.id, &request.language, &transcript)?;
        let max_end_millis = transcript
            .cues
            .iter()
            .map(|cue| cue.range.end)
            .max()
            .ok_or_else(|| {
                HostError::new(
                    "AI_TRANSCRIPTION_FAILED",
                    "Whisper returned no subtitle cues.",
                )
            })?;
        let duration_ticks =
            milliseconds_to_ticks(max_end_millis, snapshot.document.sequence.timebase)?;
        let (track_id, new_track) =
            generated_subtitle_track(&snapshot.document, request.track_id.as_deref(), handle.id)?;
        let clip = Clip {
            id: ClipId::new(format!("subtitle-clip-{}", handle.id)).map_err(HostError::from)?,
            asset_id: subtitle_asset.id.clone(),
            timeline_start: 0,
            timeline_duration: duration_ticks,
            source_start: 0,
            source_duration: duration_ticks,
            speed: Rational::new(1, 1).expect("constant rational is valid"),
            opacity: 1.0,
            transform: Transform::default(),
            effects: Vec::new(),
            keyframes: Vec::new(),
            text_overlay: None,
        };
        let host = state
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = shared.current_document().map_err(HostError::from)?;
        if current.revision != snapshot.document.revision {
            return Err(HostError::from(AppError::revision_conflict(
                snapshot.document.revision,
                current.revision,
            )));
        }
        if handle.token.is_cancelled() {
            return Err(HostError::unavailable(
                "AI_TRANSCRIPTION_CANCELLED",
                "Subtitle generation was cancelled.",
            ));
        }
        let mut document = current
            .apply(
                current.revision,
                TimelineOperation::AddAsset {
                    asset: subtitle_asset,
                },
            )
            .map_err(HostError::from)?
            .document;
        if let Some(track) = new_track {
            document = document
                .apply(document.revision, TimelineOperation::AddTrack { track })
                .map_err(HostError::from)?
                .document;
        }
        document = document
            .apply(
                document.revision,
                TimelineOperation::AddClip { track_id, clip },
            )
            .map_err(HostError::from)?
            .document;
        host.shared.replace(document.clone());
        host.app.invalidate_snapshot_cache();
        Ok(document)
    })();
    let document = match postprocess {
        Ok(document) => document,
        Err(error) => {
            cleanup_generated_srt(&root, handle.id, &request.language);
            finish_subtitle_job_error(&jobs, handle.id, &error);
            return Err(error);
        }
    };
    let job = jobs
        .mark_succeeded(handle.id, None)
        .map(JobResponse::from)
        .map_err(HostError::from)?;
    Ok(SubtitleGenerationResponse {
        job,
        document,
        language: request.language,
        cue_count: transcript.cues.len(),
        message: "Local subtitles generated and added to a subtitle layer.".into(),
    })
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
    let profile = editor_media::ExportProfile::parse(if request.profile.trim().is_empty() {
        "baseline"
    } else {
        request.profile.trim()
    })
    .map_err(|error| HostError::new("INVALID_EXPORT_PROFILE", &error.to_string()))?;
    let output = RelativePath::new(request.output_path).map_err(HostError::from)?;
    let id = host
        .app
        .start_media_job_async_with_profile(JobKind::Export, Some(output), profile)
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
fn tts_generate(
    request: TtsGenerateRequest,
    state: State<'_, AppState>,
) -> Result<TtsGenerationResponse, HostError> {
    validate_tts_voice(&request.voice_id)?;
    if request.text.trim().is_empty() || request.text.len() > 8 * 1024 {
        return Err(HostError::new(
            "INVALID_REQUEST",
            "Voiceover text must contain 1..=8192 bytes.",
        ));
    }
    let _ai_slot = acquire_local_ai_slot()?;
    let (shared, jobs, snapshot, media, piper, voice, root, handle) = {
        let host = state
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if host.project_path.is_none() {
            return Err(HostError::new(
                "PROJECT_PATH_REQUIRED",
                "Create or save a project before generating a voiceover.",
            ));
        }
        let snapshot = host.app.snapshot().map_err(HostError::from)?;
        if request.base_revision != snapshot.document.revision {
            return Err(HostError::from(AppError::revision_conflict(
                request.base_revision,
                snapshot.document.revision,
            )));
        }
        let (piper, voice) = tts_runtime_paths(&default_bundle_root(), &request.voice_id);
        let jobs = host.app.jobs().clone();
        let handle = jobs
            .submit(JobKind::Tts, snapshot.metadata.clone())
            .map_err(HostError::from)?;
        jobs.start(handle.id).map_err(HostError::from)?;
        (
            host.shared.clone(),
            jobs,
            snapshot,
            host.media.clone(),
            piper,
            voice,
            project_root(&host),
            handle,
        )
    };
    let target = root.join("media").join(generated_tts_file_name(handle.id));
    let temporary = root
        .join("media")
        .join(format!(".{}.part", generated_tts_file_name(handle.id)));
    let postprocess = (|| {
        std::fs::create_dir_all(root.join("media")).map_err(|error| {
            HostError::new(
                "TTS_GENERATION_FAILED",
                &format!("The project media directory could not be created: {error}"),
            )
        })?;
        piper_synthesize(request.text.trim(), &piper, &voice, &temporary, &|| {
            handle.token.is_cancelled()
        })?;
        std::fs::rename(&temporary, &target).map_err(|error| {
            HostError::new(
                "TTS_GENERATION_FAILED",
                &format!("The generated voiceover could not be finalized: {error}"),
            )
        })?;
        let probe = probe_summary(media.probe(&target).map_err(HostError::from)?)?;
        let source_duration = probe.duration_ticks;
        let seconds = source_duration as f64 * probe.stream_timebase.denominator as f64
            / probe.stream_timebase.numerator as f64;
        let timeline_duration = seconds_to_ticks(
            seconds,
            snapshot.document.sequence.timebase,
            "TTS_GENERATION_FAILED",
        )?;
        let (relative, asset) = generated_tts_asset(&root, handle.id, probe)?;
        let (track_id, new_track) =
            generated_audio_track(&snapshot.document, request.track_id.as_deref(), handle.id)?;
        let timeline_start = snapshot
            .document
            .sequence
            .tracks
            .iter()
            .find(|track| track.id == track_id)
            .and_then(|track| {
                track
                    .clips
                    .iter()
                    .filter_map(|clip| clip.timeline_start.checked_add(clip.timeline_duration))
                    .max()
            })
            .unwrap_or(0);
        let clip = Clip {
            id: ClipId::new(format!("tts-clip-{}", handle.id)).map_err(HostError::from)?,
            asset_id: asset.id.clone(),
            timeline_start,
            timeline_duration,
            source_start: 0,
            source_duration,
            speed: Rational::new(1, 1).expect("constant rational is valid"),
            opacity: 1.0,
            transform: Transform::default(),
            effects: Vec::new(),
            keyframes: Vec::new(),
            text_overlay: None,
        };
        let host = state
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = shared.current_document().map_err(HostError::from)?;
        if current.revision != snapshot.document.revision {
            return Err(HostError::from(AppError::revision_conflict(
                snapshot.document.revision,
                current.revision,
            )));
        }
        if handle.token.is_cancelled() {
            return Err(HostError::unavailable(
                "TTS_GENERATION_CANCELLED",
                "Voiceover generation was cancelled.",
            ));
        }
        let mut document = current
            .apply(current.revision, TimelineOperation::AddAsset { asset })
            .map_err(HostError::from)?
            .document;
        if let Some(track) = new_track {
            document = document
                .apply(document.revision, TimelineOperation::AddTrack { track })
                .map_err(HostError::from)?
                .document;
        }
        document = document
            .apply(
                document.revision,
                TimelineOperation::AddClip { track_id, clip },
            )
            .map_err(HostError::from)?
            .document;
        host.shared.replace(document.clone());
        host.app.invalidate_snapshot_cache();
        Ok((relative, document))
    })();
    let (relative, document) = match postprocess {
        Ok(value) => value,
        Err(error) => {
            cleanup_generated_tts(&root, handle.id);
            if error.code == "TTS_GENERATION_CANCELLED" {
                let _ = jobs.mark_cancelled(handle.id);
            } else {
                let _ = jobs.mark_failed(handle.id, AppError::from(error.clone()));
            }
            return Err(error);
        }
    };
    let job = jobs
        .mark_succeeded(handle.id, Some(relative.clone()))
        .map(JobResponse::from)
        .map_err(HostError::from)?;
    Ok(TtsGenerationResponse {
        job,
        document,
        voice_id: request.voice_id,
        relative_path: relative.to_string(),
        message: "Local Piper voiceover generated and added to an audio layer.".into(),
    })
}

fn project_context(document: &ProjectDocument) -> Result<ProjectContext, HostError> {
    let clips = document
        .sequence
        .tracks
        .iter()
        .flat_map(|track| {
            track.clips.iter().map(|clip| {
                Ok(ClipContext {
                    clip_id: clip.id.clone(),
                    asset_id: clip.asset_id.clone(),
                    timeline_range: clip.timeline_range().map_err(HostError::from)?,
                    source_range: clip.source_range().map_err(HostError::from)?,
                    locked: track.locked,
                })
            })
        })
        .collect::<Result<Vec<_>, HostError>>()?;
    Ok(ProjectContext {
        project_id: document.project_id.clone(),
        base_revision: document.revision,
        clips,
    })
}

fn local_plan_prompt(context: &ProjectContext, instruction: &str) -> Result<String, HostError> {
    let context_json = serde_json::to_string(context).map_err(|error| {
        HostError::new(
            "AI_PLAN_FAILED",
            &format!("The local planner context could not be serialized: {error}"),
        )
    })?;
    let prompt = [
        "You are an offline NLE edit planner. Return ONLY one JSON object and no markdown. ",
        "Use only existing clip IDs from context. Allowed operations are trim with source_start/source_end, split with at_timeline_tick, delete, ripple_delete with track_id, reorder with track_id/new_index, or effect_preset with preset warm_color_grade, cool_color_grade, or monochrome. ",
        "The JSON schema is {base_revision:number,operations:[{op:string,clip_id:string,...}],warnings:string[],affected_clips:string[],requires_confirmation:true}. ",
        "Numbers must be JSON integers, never quoted strings. Emit at least one operation. Do not emit paths, commands, URLs, filtergraphs, or network actions.\nContext: ",
        context_json.as_str(),
        "\nInstruction: ",
        instruction,
    ]
    .concat();
    if prompt.len() > MAX_LLM_PROMPT_BYTES {
        return Err(HostError::new(
            "AI_PLAN_FAILED",
            "The local planner prompt exceeds the bounded context limit.",
        ));
    }
    Ok(prompt)
}

fn extract_json_object(output: &str) -> Option<&str> {
    let start = output.find('{')?;
    let end = output.rfind('}')?;
    (start < end).then(|| &output[start..=end])
}

fn read_bounded_pipe(
    mut pipe: impl Read,
    max_bytes: usize,
    exceeded: Arc<AtomicBool>,
) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(max_bytes.min(8 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let count = pipe.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let remaining = max_bytes.saturating_sub(bytes.len());
        let keep = remaining.min(count);
        bytes.extend_from_slice(&buffer[..keep]);
        if keep < count {
            exceeded.store(true, Ordering::Release);
        }
    }
    Ok(bytes)
}

fn acquire_local_ai_slot() -> Result<std::sync::MutexGuard<'static, ()>, HostError> {
    static SLOT: OnceLock<Mutex<()>> = OnceLock::new();
    match SLOT.get_or_init(|| Mutex::new(())).try_lock() {
        Ok(guard) => Ok(guard),
        Err(std::sync::TryLockError::Poisoned(poisoned)) => Ok(poisoned.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => Err(HostError::retryable(
            "AI_BUSY",
            "A local AI operation is already running; try again after it completes.",
        )),
    }
}

fn run_local_llm(llama: &Path, model: &Path, prompt: &str) -> Result<Vec<u8>, HostError> {
    let model_path = model.to_string_lossy().into_owned();
    let arguments = vec![
        "-m".to_owned(),
        model_path,
        "--ctx-size".to_owned(),
        "4096".to_owned(),
        "--threads".to_owned(),
        "2".to_owned(),
        "-n".to_owned(),
        "256".to_owned(),
        "--temp".to_owned(),
        "0".to_owned(),
        "--seed".to_owned(),
        "42".to_owned(),
        "--single-turn".to_owned(),
        "--simple-io".to_owned(),
        "--no-display-prompt".to_owned(),
        "--no-jinja".to_owned(),
        "--no-perf".to_owned(),
        "--json-schema".to_owned(),
        LOCAL_LLM_JSON_SCHEMA.to_owned(),
        "-p".to_owned(),
        prompt.to_owned(),
    ];
    let mut process = Command::new(llama)
        .args(&arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            HostError::new(
                "AI_PLAN_FAILED",
                &format!("The llama.cpp process could not start: {error}"),
            )
        })?;
    let stdout = process.stdout.take().ok_or_else(|| {
        let _ = process.kill();
        let _ = process.wait();
        HostError::new(
            "AI_PLAN_FAILED",
            "The llama.cpp process did not expose an output pipe.",
        )
    })?;
    let output_limited = Arc::new(AtomicBool::new(false));
    let reader_flag = Arc::clone(&output_limited);
    let stdout_thread =
        thread::spawn(move || read_bounded_pipe(stdout, MAX_LLM_OUTPUT_BYTES, reader_flag));
    let started = Instant::now();
    let mut status = None;
    let mut wait_error = None;
    let mut timed_out = false;
    loop {
        if output_limited.load(Ordering::Acquire) {
            let _ = process.kill();
            status = process.wait().ok();
            break;
        }
        if started.elapsed() >= LOCAL_LLM_TIMEOUT {
            timed_out = true;
            let _ = process.kill();
            status = process.wait().ok();
            break;
        }
        match process.try_wait() {
            Ok(Some(exit_status)) => {
                status = Some(exit_status);
                break;
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(error) => {
                wait_error = Some(error);
                let _ = process.kill();
                let _ = process.wait();
                break;
            }
        }
    }
    let stdout = stdout_thread
        .join()
        .map_err(|_| HostError::new("AI_PLAN_FAILED", "The llama.cpp output reader failed."))?
        .map_err(|error| {
            HostError::new(
                "AI_PLAN_FAILED",
                &format!("The llama.cpp output pipe failed: {error}"),
            )
        })?;
    if let Some(error) = wait_error {
        return Err(HostError::new(
            "AI_PLAN_FAILED",
            &format!("The llama.cpp process could not be monitored: {error}"),
        ));
    }
    if timed_out {
        return Err(HostError::retryable(
            "AI_PLAN_TIMEOUT",
            "The local language model exceeded the 90 second resource limit.",
        ));
    }
    if output_limited.load(Ordering::Acquire) {
        return Err(HostError::new(
            "AI_PLAN_OUTPUT_LIMIT",
            "The local language model exceeded the 64 KiB output limit.",
        ));
    }
    let status = status.ok_or_else(|| {
        HostError::new(
            "AI_PLAN_FAILED",
            "The local language model exited without a process status.",
        )
    })?;
    if !status.success() {
        return Err(HostError::new(
            "AI_PLAN_FAILED",
            "The local language model returned a failed process status.",
        ));
    }
    Ok(stdout)
}

fn local_llm_plan(
    document: &ProjectDocument,
    instruction: &str,
) -> Result<AssistantPlanResponse, HostError> {
    let root = default_bundle_root();
    let (llama, model) = llm_runtime_paths(&root);
    if !llm_runtime_ready(&root) {
        return Err(HostError::unavailable(
            "AI_UNAVAILABLE",
            "The verified llama.cpp runtime or Qwen model is not provisioned.",
        ));
    }
    let context = project_context(document)?;
    let prompt = local_plan_prompt(&context, instruction)?;
    let _ai_slot = acquire_local_ai_slot()?;
    let output = run_local_llm(&llama, &model, &prompt)?;
    let stdout = String::from_utf8_lossy(&output);
    let json = extract_json_object(&stdout).ok_or_else(|| {
        HostError::new(
            "AI_PLAN_FAILED",
            "The local language model did not return a JSON edit plan.",
        )
    })?;
    let plan = parse_edit_plan_json(json).map_err(|error| {
        HostError::new(
            "AI_PLAN_INVALID",
            &format!("The local model returned an invalid edit plan: {error}"),
        )
    })?;
    if plan.base_revision != document.revision {
        return Err(HostError::new(
            "AI_PLAN_INVALID",
            "The local model returned a stale base revision.",
        ));
    }
    validate_plan_for_apply(document, &plan, true).map_err(|error| {
        HostError::new(
            "AI_PLAN_INVALID",
            &format!("The edit plan does not apply to the current project: {error}"),
        )
    })?;
    Ok(AssistantPlanResponse {
        plan,
        provenance: ModelProvenance {
            provider: "llama.cpp".into(),
            model_id: "Qwen2.5-0.5B-Instruct-Q4_K_M".into(),
            model_version: "Qwen2.5".into(),
        },
        message: "Local edit plan generated; review and confirm before applying.".into(),
    })
}

#[tauri::command]
fn assistant_plan(
    request: AssistantPlanRequest,
    state: State<'_, AppState>,
) -> Result<AssistantPlanResponse, HostError> {
    if request.text.trim().is_empty() || request.text.len() > 8 * 1024 {
        return Err(invalid_request());
    }
    let snapshot = {
        let host = state
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        host.app.snapshot().map_err(HostError::from)?
    };
    if request.base_revision != snapshot.document.revision {
        return Err(HostError::from(AppError::revision_conflict(
            request.base_revision,
            snapshot.document.revision,
        )));
    }
    local_llm_plan(&snapshot.document, request.text.trim())
}

#[tauri::command]
fn assistant_apply(
    request: AssistantApplyRequest,
    state: State<'_, AppState>,
) -> Result<ApplyResult, HostError> {
    let json = serde_json::to_string(&request.plan).map_err(|_| invalid_request())?;
    let plan = parse_edit_plan_json(&json).map_err(|error| {
        HostError::new(
            "AI_PLAN_INVALID",
            &format!("The edit plan could not be parsed: {error}"),
        )
    })?;
    let host = state
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let current = host.app.snapshot().map_err(HostError::from)?.document;
    let validated = validate_plan_for_apply(&current, &plan, true).map_err(|error| {
        HostError::new(
            "AI_PLAN_INVALID",
            &format!("The edit plan no longer applies: {error}"),
        )
    })?;
    let mut document = current;
    let mut last = None;
    for operation in validated.operations() {
        let result = document
            .apply(document.revision, operation.clone())
            .map_err(HostError::from)?;
        document = result.document.clone();
        last = Some(result);
    }
    let mut result =
        last.ok_or_else(|| HostError::new("AI_PLAN_INVALID", "The edit plan is empty."))?;
    result.document = document.clone();
    host.shared.replace(document);
    host.app.invalidate_snapshot_cache();
    Ok(result)
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
            subtitle_generate,
            tts_generate,
            timeline_apply,
            preview_play,
            preview_pause,
            preview_seek,
            export,
            export_start,
            job_get,
            job_list,
            job_cancel,
            assistant_plan,
            assistant_apply
        ])
        .run(tauri::generate_context!())
        .expect("error while running VideoEditorFree");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn temporary_test_root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("video-editor-import-{nonce}"))
    }

    #[test]
    fn external_media_is_copied_into_project_media_directory() {
        let base = temporary_test_root();
        let project_root = base.join("project");
        let source = base.join("outside").join("clip.mp4");
        fs::create_dir_all(&project_root).unwrap();
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, b"real test bytes").unwrap();

        let first = copy_external_asset(&source, &project_root).unwrap();
        let second = copy_external_asset(&source, &project_root).unwrap();

        assert_eq!(first.file_name().unwrap(), "clip.mp4");
        assert_eq!(second.file_name().unwrap(), "clip-1.mp4");
        assert_eq!(fs::read(first).unwrap(), b"real test bytes");
        assert_eq!(fs::read(second).unwrap(), b"real test bytes");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn bundle_root_appends_runtime_directory_once() {
        let base = PathBuf::from(r"C:\Users\Test\AppData\Local");
        let expected = base.join("VideoEditorFree").join("runtime");
        let actual = bundle_root_from_base(Some(base));
        assert_eq!(actual, expected);
        assert!(!actual.ends_with(Path::new(
            r"VideoEditorFree\runtime\VideoEditorFree\runtime",
        )));
    }

    #[test]
    fn bom_prefixed_media_manifest_is_accepted() {
        let base = temporary_test_root();
        let media = base.join("media");
        fs::create_dir_all(&media).unwrap();
        let ffmpeg = media.join("ffmpeg.exe");
        let ffprobe = media.join("ffprobe.exe");
        fs::write(&ffmpeg, b"ffmpeg").unwrap();
        fs::write(&ffprobe, b"ffprobe").unwrap();
        let manifest = base.join("media-manifest.json");
        let json = br#"{"identity":"FFmpeg","version":"test","license":"GPL","sha256":"0000000000000000000000000000000000000000000000000000000000000000","architecture":"x86_64"}"#;
        let mut content = vec![0xEF, 0xBB, 0xBF];
        content.extend_from_slice(json);
        fs::write(&manifest, content).unwrap();

        assert!(media_executables_from_manifest(ffmpeg, ffprobe, manifest).is_some());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn malformed_job_id_is_not_found() {
        assert_eq!(parse_job_id("nope").unwrap_err().code, "JOB_NOT_FOUND");
    }
    #[test]
    fn local_llm_json_schema_is_valid() {
        serde_json::from_str::<serde_json::Value>(LOCAL_LLM_JSON_SCHEMA)
            .expect("the local planner schema must be valid JSON");
    }
    #[test]
    fn local_process_output_is_bounded() {
        let exceeded = Arc::new(AtomicBool::new(false));
        let output = read_bounded_pipe(
            std::io::Cursor::new(vec![1_u8; MAX_LLM_OUTPUT_BYTES + 1]),
            MAX_LLM_OUTPUT_BYTES,
            Arc::clone(&exceeded),
        )
        .expect("bounded reader should accept an in-memory pipe");
        assert_eq!(output.len(), MAX_LLM_OUTPUT_BYTES);
        assert!(exceeded.load(Ordering::Acquire));
    }
    #[test]
    fn relative_path_rejects_absolute_export() {
        assert!(RelativePath::new(r"C:\out.mp4").is_err());
    }

    #[test]
    fn subtitle_timestamps_use_srt_millisecond_format() {
        assert_eq!(format_srt_timestamp(3_723_045), "01:02:03,045");
        assert_eq!(format_srt_timestamp(-1), "00:00:00,000");
    }

    #[test]
    fn subtitle_duration_converts_from_milliseconds_to_sequence_ticks() {
        assert_eq!(
            milliseconds_to_ticks(1_500, Rational::new(30, 1).unwrap()).unwrap(),
            45
        );
    }
}
