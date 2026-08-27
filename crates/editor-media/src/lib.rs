//! Fixed-argv FFmpeg/ffprobe media boundary.
//!
//! This crate owns probe parsing, deterministic plan construction, process
//! cancellation, output verification, and atomic finalization. It never uses
//! `PATH`, shell command strings, or caller-provided output evidence.

use editor_domain::{
    AssetKind, AssetStatus, Clip, ClipId, DomainError, Effect, FadeKind, ProjectDocument, Rational,
    RgbColor, TextOverlay, TrackKind, Transform,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    error::Error,
    fmt,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};

const MAX_PROCESS_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_PROGRESS_EVENTS: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinaryManifest {
    pub identity: String,
    pub version: String,
    pub license: String,
    pub sha256: String,
    pub architecture: String,
}
impl BinaryManifest {
    pub fn validate(&self) -> Result<(), MediaError> {
        for (field, value) in [
            ("identity", &self.identity),
            ("version", &self.version),
            ("license", &self.license),
            ("architecture", &self.architecture),
        ] {
            if value.trim().is_empty() {
                return Err(MediaError::InvalidConfiguration(format!(
                    "binary manifest {field} must not be empty"
                )));
            }
        }
        if self.sha256.len() != 64 || !self.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(MediaError::InvalidConfiguration(
                "binary manifest sha256 must be a 64-character hexadecimal hash".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryContract {
    Reviewed(BinaryManifest),
    Unavailable { reason: String },
}
impl BinaryContract {
    pub fn reviewed(manifest: BinaryManifest) -> Self {
        Self::Reviewed(manifest)
    }
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
        }
    }
    pub fn validate(&self) -> Result<(), MediaError> {
        match self {
            Self::Reviewed(manifest) => manifest.validate(),
            Self::Unavailable { reason } if reason.trim().is_empty() => {
                Err(MediaError::InvalidConfiguration(
                    "binary unavailable reason must not be empty".into(),
                ))
            }
            Self::Unavailable { .. } => Ok(()),
        }
    }
    fn is_reviewed(&self) -> bool {
        matches!(self, Self::Reviewed(_))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutableConfig {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub binary_contract: BinaryContract,
}
impl ExecutableConfig {
    pub fn new(ffmpeg: impl Into<PathBuf>, ffprobe: impl Into<PathBuf>) -> Self {
        Self {
            ffmpeg: ffmpeg.into(),
            ffprobe: ffprobe.into(),
            binary_contract: BinaryContract::unavailable(
                "no reviewed FFmpeg/ffprobe binary manifest is provisioned",
            ),
        }
    }
    pub fn with_binary_contract(mut self, contract: BinaryContract) -> Self {
        self.binary_contract = contract;
        self
    }
    pub fn validate(&self) -> Result<(), MediaError> {
        validate_executable(&self.ffmpeg, "ffmpeg")?;
        validate_executable(&self.ffprobe, "ffprobe")?;
        self.binary_contract.validate()
    }
    fn validate_for_plan(&self) -> Result<(), MediaError> {
        validate_absolute_path(&self.ffmpeg, "ffmpeg")?;
        validate_absolute_path(&self.ffprobe, "ffprobe")?;
        self.binary_contract.validate()
    }
    fn validate_for_execution(&self, name: &str, path: &Path) -> Result<(), MediaError> {
        self.binary_contract.validate()?;
        if !self.binary_contract.is_reviewed() {
            let BinaryContract::Unavailable { reason } = &self.binary_contract else {
                unreachable!()
            };
            return Err(MediaError::BinaryUnavailable {
                reason: reason.clone(),
            });
        }
        validate_executable(path, name)
    }
}
fn validate_absolute_path(path: &Path, name: &str) -> Result<(), MediaError> {
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return Err(MediaError::InvalidConfiguration(format!(
            "{name} path must be an absolute configured path"
        )));
    }
    Ok(())
}
fn validate_executable(path: &Path, name: &str) -> Result<(), MediaError> {
    validate_absolute_path(path, name)?;
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) | Err(_) => Err(MediaError::ExecutableUnavailable {
            name: name.into(),
            path: path.into(),
        }),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProbeMetadata {
    pub duration_seconds: Option<f64>,
    pub format_name: Option<String>,
    pub streams: Vec<StreamMetadata>,
    pub tool_version: Option<String>,
}
impl ProbeMetadata {
    pub fn validate(&self) -> Result<(), MediaError> {
        let duration = self
            .duration_seconds
            .ok_or_else(|| MediaError::ProbeValidation("format duration is required".into()))?;
        positive_finite("format duration", duration)?;
        required_text("format name", self.format_name.as_deref())?;
        required_text("ffprobe tool version", self.tool_version.as_deref())?;
        if self.streams.is_empty() {
            return Err(MediaError::ProbeValidation(
                "at least one stream is required".into(),
            ));
        }
        let mut indices = Vec::new();
        let mut media = false;
        for stream in &self.streams {
            if indices.contains(&stream.index) {
                return Err(MediaError::ProbeValidation(format!(
                    "duplicate stream index {}",
                    stream.index
                )));
            }
            indices.push(stream.index);
            if stream.codec_type.trim().is_empty() {
                return Err(MediaError::ProbeValidation(
                    "stream codec_type must not be empty".into(),
                ));
            }
            required_text("stream codec name", stream.codec_name.as_deref())?;
            stream
                .time_base
                .ok_or_else(|| {
                    MediaError::ProbeValidation(format!(
                        "stream {} time_base is required",
                        stream.index
                    ))
                })?
                .validate()
                .map_err(MediaError::Domain)?;
            if let Some(value) = stream.duration_seconds {
                positive_finite("stream duration", value)?;
                if value > duration + 0.1 {
                    return Err(MediaError::ProbeValidation(format!(
                        "stream {} duration exceeds format duration",
                        stream.index
                    )));
                }
            }
            match stream.codec_type.as_str() {
                "video" => {
                    media = true;
                    if stream.width.unwrap_or(0) == 0 || stream.height.unwrap_or(0) == 0 {
                        return Err(MediaError::ProbeValidation(format!(
                            "video stream {} requires positive width and height",
                            stream.index
                        )));
                    }
                }
                "audio" => {
                    media = true;
                    if stream.sample_rate.unwrap_or(0) == 0 || stream.channels.unwrap_or(0) == 0 {
                        return Err(MediaError::ProbeValidation(format!(
                            "audio stream {} requires positive sample rate and channels",
                            stream.index
                        )));
                    }
                }
                _ => {}
            }
        }
        if !media {
            return Err(MediaError::ProbeValidation(
                "at least one video or audio stream is required".into(),
            ));
        }
        Ok(())
    }
    fn media_stream_kinds(&self) -> Vec<StreamKind> {
        self.streams
            .iter()
            .filter_map(|stream| match stream.codec_type.as_str() {
                "video" => Some(StreamKind::Video),
                "audio" => Some(StreamKind::Audio),
                _ => None,
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamMetadata {
    pub index: u32,
    pub codec_type: String,
    pub codec_name: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub time_base: Option<Rational>,
    pub duration_seconds: Option<f64>,
    pub rotation_degrees: Option<i16>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AssetProbeStatus {
    Available(ProbeMetadata),
    Unavailable { reason: String },
    Failed { error: String },
}

pub fn probe_media<R: ChildProcessRunner>(
    config: &ExecutableConfig,
    input: &Path,
    runner: &R,
) -> Result<ProbeMetadata, MediaError> {
    config.validate_for_execution("ffprobe", &config.ffprobe)?;
    if !input.is_absolute() || !input.is_file() {
        return Err(MediaError::PathViolation(input.to_string_lossy().into()));
    }
    let argv = vec![
        "-nostdin".into(),
        "-v".into(),
        "error".into(),
        "-print_format".into(),
        "json".into(),
        "-show_program_version".into(),
        "-show_format".into(),
        "-show_streams".into(),
        input.to_string_lossy().into(),
    ];
    let output = runner.run(&config.ffprobe, &argv)?;
    if output.status_code != Some(0) {
        return Err(MediaError::ProcessFailed {
            code: output.status_code,
            stderr: String::from_utf8_lossy(&output.stderr).into(),
        });
    }
    parse_probe_json(
        std::str::from_utf8(&output.stdout)
            .map_err(|e| MediaError::ProbeParse(format!("ffprobe output is not UTF-8: {e}")))?,
    )
}
pub fn probe_asset<R: ChildProcessRunner>(
    config: &ExecutableConfig,
    input: &Path,
    runner: &R,
) -> AssetProbeStatus {
    match probe_media(config, input, runner) {
        Ok(metadata) => AssetProbeStatus::Available(metadata),
        Err(MediaError::ExecutableUnavailable { .. })
        | Err(MediaError::BinaryUnavailable { .. })
        | Err(MediaError::PathViolation(_)) => AssetProbeStatus::Unavailable {
            reason: "reviewed ffprobe binary or input is unavailable".into(),
        },
        Err(error) => AssetProbeStatus::Failed {
            error: error.to_string(),
        },
    }
}
pub fn parse_probe_json(json: &str) -> Result<ProbeMetadata, MediaError> {
    let root: Value =
        serde_json::from_str(json).map_err(|e| MediaError::ProbeParse(e.to_string()))?;
    let streams = root
        .get("streams")
        .and_then(Value::as_array)
        .ok_or_else(|| MediaError::ProbeParse("ffprobe JSON missing streams array".into()))?;
    let metadata = ProbeMetadata {
        duration_seconds: root
            .get("format")
            .and_then(|f| number_or_string(f.get("duration"))),
        format_name: root
            .get("format")
            .and_then(|f| f.get("format_name"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        streams: streams
            .iter()
            .map(parse_stream)
            .collect::<Result<Vec<_>, _>>()?,
        tool_version: root
            .get("program_version")
            .and_then(|v| {
                v.as_str()
                    .or_else(|| v.get("version").and_then(Value::as_str))
            })
            .map(str::to_owned),
    };
    metadata.validate()?;
    Ok(metadata)
}
fn parse_stream(value: &Value) -> Result<StreamMetadata, MediaError> {
    let index = value
        .get("index")
        .and_then(Value::as_u64)
        .ok_or_else(|| MediaError::ProbeParse("stream missing numeric index".into()))?;
    let codec_type = value
        .get("codec_type")
        .and_then(Value::as_str)
        .ok_or_else(|| MediaError::ProbeParse("stream missing codec_type".into()))?;
    let time_base = value
        .get("time_base")
        .and_then(Value::as_str)
        .map(parse_rational)
        .transpose()?;
    Ok(StreamMetadata {
        index: u32::try_from(index)
            .map_err(|_| MediaError::ProbeParse("stream index overflows u32".into()))?,
        codec_type: codec_type.into(),
        codec_name: value
            .get("codec_name")
            .and_then(Value::as_str)
            .map(str::to_owned),
        width: value
            .get("width")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
        height: value
            .get("height")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
        sample_rate: value
            .get("sample_rate")
            .and_then(|v| number_or_string(Some(v)))
            .and_then(|n| u32::try_from(n as u64).ok()),
        channels: value
            .get("channels")
            .and_then(Value::as_u64)
            .and_then(|n| u16::try_from(n).ok()),
        time_base,
        duration_seconds: value
            .get("duration")
            .and_then(|v| number_or_string(Some(v))),
        rotation_degrees: value
            .get("tags")
            .and_then(|t| t.get("rotate"))
            .and_then(|v| number_or_string(Some(v)))
            .and_then(|n| i16::try_from(n as i64).ok()),
    })
}
fn number_or_string(value: Option<&Value>) -> Option<f64> {
    value.and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok()))
}
fn parse_rational(value: &str) -> Result<Rational, MediaError> {
    let (n, d) = value
        .split_once('/')
        .ok_or_else(|| MediaError::ProbeParse(format!("invalid rational: {value}")))?;
    Rational::new(
        n.parse()
            .map_err(|_| MediaError::ProbeParse(format!("invalid rational: {value}")))?,
        d.parse()
            .map_err(|_| MediaError::ProbeParse(format!("invalid rational: {value}")))?,
    )
    .map_err(MediaError::Domain)
}
fn required_text(field: &str, value: Option<&str>) -> Result<(), MediaError> {
    if value.is_none_or(|v| v.trim().is_empty()) {
        Err(MediaError::ProbeValidation(format!("{field} is required")))
    } else {
        Ok(())
    }
}
fn positive_finite(field: &str, value: f64) -> Result<(), MediaError> {
    if !value.is_finite() || value <= 0.0 {
        Err(MediaError::ProbeValidation(format!(
            "{field} must be finite and positive"
        )))
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamKind {
    Video,
    Audio,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamMap {
    pub input_index: usize,
    pub stream_kind: StreamKind,
    pub stream_index: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrimClipStep {
    pub clip_id: ClipId,
    pub input: PathBuf,
    pub output: PathBuf,
    pub source_start_seconds: String,
    pub source_duration_seconds: String,
    pub stream_map: Vec<StreamMap>,
    pub argv: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConcatMuxStep {
    pub concat_list: PathBuf,
    pub inputs: Vec<PathBuf>,
    pub output: PathBuf,
    pub stream_map: Vec<StreamMap>,
    pub argv: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompositeStep {
    pub inputs: Vec<PathBuf>,
    pub output: PathBuf,
    pub filter_complex: String,
    pub argv: Vec<String>,
}
pub type FilterComplexStep = CompositeStep;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderStep {
    TrimClip(TrimClipStep),
    ConcatMux(ConcatMuxStep),
    Composite(CompositeStep),
}
impl RenderStep {
    pub fn executable<'a>(&self, config: &'a ExecutableConfig) -> &'a Path {
        &config.ffmpeg
    }
    pub fn argv(&self) -> &[String] {
        match self {
            Self::TrimClip(s) => &s.argv,
            Self::ConcatMux(s) => &s.argv,
            Self::Composite(s) => &s.argv,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct ExpectedOutput {
    pub stream_kinds: Vec<StreamKind>,
    pub duration_seconds: f64,
    pub duration_tolerance_seconds: f64,
}
impl ExpectedOutput {
    pub fn validate(&self) -> Result<(), MediaError> {
        positive_finite("expected output duration", self.duration_seconds)?;
        if !self.duration_tolerance_seconds.is_finite() || self.duration_tolerance_seconds < 0.0 {
            return Err(MediaError::InvalidOutput(
                "expected duration tolerance must be finite and non-negative".into(),
            ));
        }
        if self.stream_kinds.is_empty() {
            return Err(MediaError::InvalidOutput(
                "expected output must contain a media stream".into(),
            ));
        }
        Ok(())
    }
    fn matches_probe(&self, probe: &ProbeMetadata) -> Result<(), MediaError> {
        self.validate()?;
        if self.stream_kinds != probe.media_stream_kinds() {
            return Err(MediaError::InvalidOutput(
                "output stream order does not match expected stream order".into(),
            ));
        }
        if (probe.duration_seconds.unwrap_or_default() - self.duration_seconds).abs()
            > self.duration_tolerance_seconds
        {
            return Err(MediaError::InvalidOutput(
                "output duration is outside expected tolerance".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConcatList {
    pub path: PathBuf,
    pub entries: Vec<PathBuf>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct RenderPlan {
    pub steps: Vec<RenderStep>,
    pub concat_list: ConcatList,
    pub output: PathBuf,
    pub expected_output: ExpectedOutput,
    pub binary_contract: BinaryContract,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportProfile {
    #[default]
    Baseline,
    Youtube,
    Instagram,
    Tiktok,
}

impl ExportProfile {
    pub fn parse(value: &str) -> Result<Self, MediaError> {
        match value {
            "baseline" => Ok(Self::Baseline),
            "youtube" => Ok(Self::Youtube),
            "instagram" => Ok(Self::Instagram),
            "tiktok" => Ok(Self::Tiktok),
            _ => Err(MediaError::InvalidPlan(format!(
                "unsupported export profile: {value}"
            ))),
        }
    }

    fn dimensions(self, sequence_width: u32, sequence_height: u32) -> (u32, u32) {
        match self {
            Self::Baseline | Self::Youtube => (sequence_width, sequence_height),
            Self::Instagram | Self::Tiktok => (1080, 1920),
        }
    }

    fn max_duration_seconds(self) -> Option<f64> {
        match self {
            Self::Baseline | Self::Youtube => None,
            Self::Instagram => Some(900.0),
            Self::Tiktok => Some(600.0),
        }
    }

    fn video_bitrate(self) -> &'static str {
        match self {
            Self::Baseline | Self::Youtube => "8M",
            Self::Instagram => "12M",
            Self::Tiktok => "10M",
        }
    }

    fn audio_bitrate(self) -> &'static str {
        match self {
            Self::Baseline | Self::Youtube => "384k",
            Self::Instagram | Self::Tiktok => "128k",
        }
    }

    fn audio_sample_rate(self) -> u32 {
        48_000
    }

    fn audio_channels(self) -> u16 {
        2
    }

    fn frame_rate(self) -> Rational {
        Rational {
            numerator: 30,
            denominator: 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProfileExpectedOutput {
    pub profile: ExportProfile,
    pub stream_kinds: Vec<StreamKind>,
    pub duration_seconds: f64,
    pub duration_tolerance_seconds: f64,
    pub width: u32,
    pub height: u32,
    pub video_codec: String,
    pub audio_codec: Option<String>,
    pub audio_sample_rate: Option<u32>,
    pub audio_channels: Option<u16>,
    pub container: String,
}

impl ProfileExpectedOutput {
    pub fn validate(&self) -> Result<(), MediaError> {
        if self.profile == ExportProfile::Baseline {
            return Err(MediaError::InvalidOutput(
                "baseline must use the legacy render plan".into(),
            ));
        }
        positive_finite("expected output duration", self.duration_seconds)?;
        if !self.duration_tolerance_seconds.is_finite() || self.duration_tolerance_seconds < 0.0 {
            return Err(MediaError::InvalidOutput(
                "expected duration tolerance must be finite and non-negative".into(),
            ));
        }
        if self.width == 0 || self.height == 0 {
            return Err(MediaError::InvalidOutput(
                "expected output dimensions must be positive".into(),
            ));
        }
        let has_audio = match self.stream_kinds.as_slice() {
            [StreamKind::Video] => false,
            [StreamKind::Video, StreamKind::Audio] => true,
            _ => {
                return Err(MediaError::InvalidOutput(
                    "profile output must contain video followed by optional audio".into(),
                ))
            }
        };
        if self.video_codec.trim().is_empty() || self.container.trim().is_empty() {
            return Err(MediaError::InvalidOutput(
                "profile output codec and container must not be empty".into(),
            ));
        }
        if has_audio != self.audio_codec.is_some()
            || has_audio != self.audio_sample_rate.is_some()
            || has_audio != self.audio_channels.is_some()
        {
            return Err(MediaError::InvalidOutput(
                "profile audio expectation is incomplete".into(),
            ));
        }
        if self.container != "mp4" {
            return Err(MediaError::InvalidOutput(
                "profile output container must be mp4".into(),
            ));
        }
        Ok(())
    }

    fn matches_probe(&self, probe: &ProbeMetadata) -> Result<(), MediaError> {
        self.validate()?;
        if self.stream_kinds != probe.media_stream_kinds() {
            return Err(MediaError::InvalidOutput(
                "profile output stream order does not match the expected layout".into(),
            ));
        }
        let duration = probe.duration_seconds.ok_or_else(|| {
            MediaError::InvalidOutput("profile output duration is missing".into())
        })?;
        if (duration - self.duration_seconds).abs() > self.duration_tolerance_seconds {
            return Err(MediaError::InvalidOutput(
                "profile output duration is outside expected tolerance".into(),
            ));
        }
        let video = probe
            .streams
            .iter()
            .find(|stream| stream.codec_type == "video")
            .ok_or_else(|| {
                MediaError::InvalidOutput("profile output has no video stream".into())
            })?;
        if video.width != Some(self.width)
            || video.height != Some(self.height)
            || !video
                .codec_name
                .as_deref()
                .is_some_and(|codec| codec.eq_ignore_ascii_case(&self.video_codec))
        {
            return Err(MediaError::InvalidOutput(
                "profile output video codec or dimensions do not match".into(),
            ));
        }
        if let Some(audio_codec) = &self.audio_codec {
            let audio = probe
                .streams
                .iter()
                .find(|stream| stream.codec_type == "audio")
                .ok_or_else(|| {
                    MediaError::InvalidOutput("profile output has no audio stream".into())
                })?;
            if !audio
                .codec_name
                .as_deref()
                .is_some_and(|codec| codec.eq_ignore_ascii_case(audio_codec))
                || audio.sample_rate != self.audio_sample_rate
                || audio.channels != self.audio_channels
            {
                return Err(MediaError::InvalidOutput(
                    "profile output audio codec or layout does not match".into(),
                ));
            }
        }
        if !probe.format_name.as_deref().is_some_and(|format| {
            format
                .split(',')
                .any(|candidate| candidate.eq_ignore_ascii_case(&self.container))
        }) {
            return Err(MediaError::InvalidOutput(
                "profile output container does not match".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayeredRenderPlan {
    pub step: CompositeStep,
    pub output: PathBuf,
    pub expected_output: ProfileExpectedOutput,
    pub binary_contract: BinaryContract,
    pub profile: ExportProfile,
}

pub fn build_render_plan(
    document: &ProjectDocument,
    project_root: &Path,
    output: impl Into<PathBuf>,
    executables: &ExecutableConfig,
) -> Result<RenderPlan, MediaError> {
    document.validate().map_err(MediaError::Domain)?;
    executables.validate_for_plan()?;
    let output = validate_output_path(output.into())?;
    let video_tracks = document
        .sequence
        .tracks
        .iter()
        .filter(|t| t.enabled && matches!(t.kind, TrackKind::Video))
        .collect::<Vec<_>>();
    if video_tracks.len() != 1 {
        return Err(MediaError::InvalidPlan(
            "baseline export requires exactly one enabled video track".into(),
        ));
    }
    if document
        .sequence
        .tracks
        .iter()
        .any(|t| t.enabled && !matches!(t.kind, TrackKind::Video) && !t.clips.is_empty())
    {
        return Err(MediaError::InvalidPlan(
            "baseline export does not silently drop enabled non-video tracks".into(),
        ));
    }
    let mut clips = video_tracks[0].clips.iter().collect::<Vec<_>>();
    clips.sort_by_key(|c| (c.timeline_start, c.id.to_string()));
    if clips.is_empty() {
        return Err(MediaError::InvalidPlan(
            "at least one clip is required".into(),
        ));
    }
    let mut steps = Vec::with_capacity(clips.len() + 1);
    let mut segments = Vec::with_capacity(clips.len());
    let mut expected_streams = None;
    let mut reference_probe = None;
    let mut expected_duration = 0.0;
    for (ordinal, clip) in clips.iter().enumerate() {
        let asset = document
            .assets
            .iter()
            .find(|a| a.id == clip.asset_id)
            .ok_or_else(|| {
                MediaError::Domain(DomainError::NotFound {
                    entity: "asset".into(),
                    id: clip.asset_id.to_string(),
                })
            })?;
        if !matches!(asset.kind, AssetKind::Video)
            || !matches!(asset.status, AssetStatus::Available)
        {
            return Err(MediaError::InvalidPlan(format!(
                "clip {} requires an available video asset",
                clip.id
            )));
        }
        if clip.speed != Rational::new(1, 1).expect("constant rational is valid")
            || !clip.effects.is_empty()
            || clip.opacity != 1.0
        {
            return Err(MediaError::Unsupported(
                "clip uses an unsupported baseline operation".into(),
            ));
        }
        let probe = asset.probe.as_ref().ok_or_else(|| {
            MediaError::InvalidPlan(format!("clip {} has no validated probe metadata", clip.id))
        })?;
        let kinds = [
            probe.video.as_ref().map(|_| StreamKind::Video),
            probe.audio.as_ref().map(|_| StreamKind::Audio),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        if expected_streams
            .as_ref()
            .is_some_and(|expected| expected != &kinds)
        {
            return Err(MediaError::Unsupported(
                "concat copy requires identical stream layout".into(),
            ));
        }
        if let Some(reference) = reference_probe {
            ensure_concat_compatible(reference, probe)?;
        } else {
            reference_probe = Some(probe);
        }
        expected_streams = Some(kinds.clone());
        let input = resolve_safe_path(project_root, asset.relative_path.as_str())?;
        let segment = unique_temporary_output_path(&output, ordinal)?;
        if paths_alias(&input, &output) || paths_alias(&input, &segment) {
            return Err(MediaError::PathViolation(
                "input and output paths must be distinct".into(),
            ));
        }
        // Source ticks belong to asset probe stream timebase, never sequence timebase.
        let start = ticks_to_seconds(clip.source_start, probe.stream_timebase)?;
        let duration = ticks_to_seconds(clip.source_duration, probe.stream_timebase)?;
        expected_duration += duration;
        let stream_map = stream_maps_for_kinds(&kinds);
        let start_text = format_seconds(start);
        let duration_text = format_seconds(duration);
        let mut argv = vec![
            "-nostdin".into(),
            "-y".into(),
            "-i".into(),
            input.to_string_lossy().into(),
            "-ss".into(),
            start_text.clone(),
            "-t".into(),
            duration_text.clone(),
        ];
        for map in &stream_map {
            argv.extend([
                "-map".into(),
                format!("0:{}:0", map.stream_kind.as_ffmpeg()),
            ]);
        }
        argv.extend([
            "-c:v".into(),
            "libx264".into(),
            "-c:a".into(),
            "aac".into(),
            "-f".into(),
            "matroska".into(),
            "-progress".into(),
            "pipe:1".into(),
            segment.to_string_lossy().into(),
        ]);
        steps.push(RenderStep::TrimClip(TrimClipStep {
            clip_id: clip.id.clone(),
            input,
            output: segment.clone(),
            source_start_seconds: start_text,
            source_duration_seconds: duration_text,
            stream_map,
            argv,
        }));
        segments.push(segment);
    }
    let list_path = unique_sibling_path(&output, "concat-plan");
    let final_temp = unique_temporary_output_path(&output, clips.len())?;
    let kinds = expected_streams.expect("clips is non-empty");
    let stream_map = stream_maps_for_kinds(&kinds)
        .into_iter()
        .enumerate()
        .map(|(index, map)| StreamMap {
            input_index: 0,
            stream_kind: map.stream_kind,
            stream_index: index as u32,
        })
        .collect::<Vec<_>>();
    let mut argv = vec![
        "-nostdin".into(),
        "-y".into(),
        "-f".into(),
        "concat".into(),
        "-safe".into(),
        "0".into(),
        "-i".into(),
        list_path.to_string_lossy().into(),
    ];
    for map in &stream_map {
        argv.extend(["-map".into(), format!("0:{}", map.stream_index)]);
    }
    argv.extend([
        "-c".into(),
        "copy".into(),
        "-progress".into(),
        "pipe:1".into(),
        final_temp.to_string_lossy().into(),
    ]);
    steps.push(RenderStep::ConcatMux(ConcatMuxStep {
        concat_list: list_path.clone(),
        inputs: segments.clone(),
        output: final_temp,
        stream_map,
        argv,
    }));
    Ok(RenderPlan {
        steps,
        concat_list: ConcatList {
            path: list_path,
            entries: segments,
        },
        output,
        expected_output: ExpectedOutput {
            stream_kinds: kinds,
            duration_seconds: expected_duration,
            duration_tolerance_seconds: 0.1,
        },
        binary_contract: executables.binary_contract.clone(),
    })
}

#[derive(Clone, Debug)]
struct VideoLayer {
    input_index: usize,
    timeline_start: f64,
    timeline_end: f64,
    is_overlay: bool,
    filter: String,
    opacity: f32,
    transform: Transform,
}

#[derive(Clone, Debug)]
struct TextLayer {
    overlay: TextOverlay,
    timeline_start: f64,
    timeline_end: f64,
}

#[derive(Clone, Debug)]
struct AudioLayer {
    input_index: usize,
    track_id: String,
    timeline_start: f64,
    effects: String,
}

/// Build one fixed-argv composition for the multi-layer export path.
///
/// The graph is generated from typed project values only. Frontend strings
/// never become filter expressions or command fragments.
pub fn build_export_plan(
    document: &ProjectDocument,
    project_root: &Path,
    output: impl Into<PathBuf>,
    executables: &ExecutableConfig,
    profile: ExportProfile,
) -> Result<RenderPlan, MediaError> {
    if profile == ExportProfile::Baseline {
        return Err(MediaError::Unsupported(
            "baseline export uses the legacy render plan".into(),
        ));
    }
    document.validate().map_err(MediaError::Domain)?;
    executables.validate_for_plan()?;
    let output = validate_output_path(output.into())?;
    let (width, height) = profile.dimensions(document.sequence.width, document.sequence.height);
    let frame_rate = format_rational(profile.frame_rate());
    let mut argv = vec![
        "-nostdin".into(),
        "-y".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!("color=c=black:s={width}x{height}:r={frame_rate}:d=1"),
    ];
    let mut inputs = Vec::new();
    let mut video_layers = Vec::new();
    let mut text_layers = Vec::new();
    let mut audio_layers = Vec::new();
    let mut duration = 0.0_f64;
    let one = Rational::new(1, 1).expect("constant rational is valid");

    for track in document.sequence.tracks.iter().filter(|track| {
        track.enabled && matches!(track.kind, TrackKind::Video | TrackKind::Overlay)
    }) {
        for clip in &track.clips {
            let asset = asset_for_clip(document, clip)?;
            if !matches!(asset.kind, AssetKind::Video | AssetKind::Image)
                || !matches!(asset.status, AssetStatus::Available)
            {
                return Err(MediaError::InvalidPlan(format!(
                    "clip {} requires an available video or image asset",
                    clip.id
                )));
            }
            let probe = asset.probe.as_ref().ok_or_else(|| {
                MediaError::InvalidPlan(format!("clip {} has no validated probe metadata", clip.id))
            })?;
            if probe.video.is_none() {
                return Err(MediaError::InvalidPlan(format!(
                    "clip {} has no video stream",
                    clip.id
                )));
            }
            if clip.speed != one || !clip.keyframes.is_empty() {
                return Err(MediaError::Unsupported(
                    "layered export requires normal-speed clips without keyframes".into(),
                ));
            }
            let source_start = ticks_to_seconds(clip.source_start, probe.stream_timebase)?;
            let source_duration = ticks_to_seconds(clip.source_duration, probe.stream_timebase)?;
            let timeline_start = ticks_to_seconds(clip.timeline_start, document.sequence.timebase)?;
            let timeline_duration =
                ticks_to_seconds(clip.timeline_duration, document.sequence.timebase)?;
            if (source_duration - timeline_duration).abs() > 0.01 {
                return Err(MediaError::Unsupported(
                    "layered export requires clip source and timeline durations to match when speed is 1".into(),
                ));
            }
            let input = resolve_safe_path(project_root, asset.relative_path.as_str())?;
            if paths_alias(&input, &output) {
                return Err(MediaError::PathViolation(
                    "input and output paths must be distinct".into(),
                ));
            }
            let input_index = inputs.len() + 1;
            inputs.push(input.clone());
            if matches!(asset.kind, AssetKind::Image) {
                argv.extend(["-loop".into(), "1".into()]);
            }
            argv.extend([
                "-ss".into(),
                format_seconds(source_start),
                "-t".into(),
                format_seconds(source_duration),
                "-i".into(),
                input.to_string_lossy().into(),
            ]);
            let timeline_end = timeline_start + timeline_duration;
            duration = duration.max(timeline_end);
            video_layers.push(VideoLayer {
                input_index,
                timeline_start,
                timeline_end,
                is_overlay: matches!(track.kind, TrackKind::Overlay),
                filter: video_effects(clip, project_root, document.sequence.timebase)?,
                opacity: clip.opacity,
                transform: clip.transform.clone(),
            });
            if probe.audio.is_some() && matches!(track.kind, TrackKind::Video) {
                audio_layers.push(AudioLayer {
                    input_index,
                    track_id: track.id.to_string(),
                    timeline_start,
                    effects: audio_effects(clip, document.sequence.timebase, true)?,
                });
            }
        }
    }

    for track in document
        .sequence
        .tracks
        .iter()
        .filter(|track| track.enabled && matches!(track.kind, TrackKind::Text))
    {
        for clip in &track.clips {
            let asset = asset_for_clip(document, clip)?;
            if !matches!(asset.kind, AssetKind::Text) {
                return Err(MediaError::InvalidPlan(format!(
                    "text clip {} requires a text asset",
                    clip.id
                )));
            }
            let overlay = clip.text_overlay.clone().ok_or_else(|| {
                MediaError::InvalidPlan(format!("text clip {} has no overlay content", clip.id))
            })?;
            let timeline_start = ticks_to_seconds(clip.timeline_start, document.sequence.timebase)?;
            let timeline_duration =
                ticks_to_seconds(clip.timeline_duration, document.sequence.timebase)?;
            let timeline_end = timeline_start + timeline_duration;
            duration = duration.max(timeline_end);
            text_layers.push(TextLayer {
                overlay,
                timeline_start,
                timeline_end,
            });
        }
    }

    for track in document
        .sequence
        .tracks
        .iter()
        .filter(|track| track.enabled && matches!(track.kind, TrackKind::Audio))
    {
        for clip in &track.clips {
            let asset = asset_for_clip(document, clip)?;
            if !matches!(asset.kind, AssetKind::Video | AssetKind::Audio)
                || !matches!(asset.status, AssetStatus::Available)
            {
                return Err(MediaError::InvalidPlan(format!(
                    "clip {} requires an available audio or video asset",
                    clip.id
                )));
            }
            let probe = asset.probe.as_ref().ok_or_else(|| {
                MediaError::InvalidPlan(format!("clip {} has no validated probe metadata", clip.id))
            })?;
            if probe.audio.is_none() {
                return Err(MediaError::InvalidPlan(format!(
                    "clip {} has no audio stream",
                    clip.id
                )));
            }
            if clip.speed != one || clip.opacity != 1.0 {
                return Err(MediaError::Unsupported(
                    "layered export supports audio clips at normal speed only".into(),
                ));
            }
            let source_start = ticks_to_seconds(clip.source_start, probe.stream_timebase)?;
            let source_duration = ticks_to_seconds(clip.source_duration, probe.stream_timebase)?;
            let timeline_start = ticks_to_seconds(clip.timeline_start, document.sequence.timebase)?;
            let timeline_duration =
                ticks_to_seconds(clip.timeline_duration, document.sequence.timebase)?;
            if (source_duration - timeline_duration).abs() > 0.01 {
                return Err(MediaError::Unsupported(
                    "layered export requires clip source and timeline durations to match when speed is 1".into(),
                ));
            }
            let input = resolve_safe_path(project_root, asset.relative_path.as_str())?;
            if paths_alias(&input, &output) {
                return Err(MediaError::PathViolation(
                    "input and output paths must be distinct".into(),
                ));
            }
            let input_index = inputs.len() + 1;
            inputs.push(input.clone());
            argv.extend([
                "-ss".into(),
                format_seconds(source_start),
                "-t".into(),
                format_seconds(source_duration),
                "-i".into(),
                input.to_string_lossy().into(),
            ]);
            duration = duration.max(timeline_start + timeline_duration);
            audio_layers.push(AudioLayer {
                input_index,
                track_id: track.id.to_string(),
                timeline_start,
                effects: audio_effects(clip, document.sequence.timebase, false)?,
            });
        }
    }
    if video_layers.is_empty() {
        return Err(MediaError::InvalidPlan(
            "at least one enabled video clip is required for export".into(),
        ));
    }
    if !duration.is_finite() || duration <= 0.0 {
        return Err(MediaError::InvalidPlan(
            "export duration must be positive".into(),
        ));
    }
    if profile
        .max_duration_seconds()
        .is_some_and(|limit| duration > limit + 0.001)
    {
        return Err(MediaError::InvalidPlan(format!(
            "{} export duration exceeds the platform preset limit",
            profile_name(profile)
        )));
    }
    argv[5] = format!(
        "color=c=black:s={width}x{height}:r={frame_rate}:d={}",
        format_seconds(duration)
    );

    let mut graph = "[0:v]format=yuv420p[base];".to_owned();
    let mut current_video = "base".to_owned();
    for (index, layer) in video_layers.iter().enumerate() {
        let source_label = format!("v{index}");
        let next_label = format!("v{index}_mix");
        let effects = format!(
            "{}{}{}",
            layer.filter,
            transform_scale_filter(&layer.transform, layer.is_overlay),
            transform_rotate_filter(&layer.transform),
        );
        if layer.is_overlay {
            graph.push_str(&format!(
                "[{input}:v]setpts=PTS-STARTPTS{effects},format=rgba,colorchannelmixer=aa={opacity:.6},setpts=PTS+{start:.6}/TB[{source}];[{current}][{source}]overlay=x='(main_w-overlay_w)/2+({position_x:.6}*main_w/2)':y='(main_h-overlay_h)/2+({position_y:.6}*main_h/2)':eof_action=pass:shortest=0:enable='between(t\\,{start:.6}\\,{end:.6})'[{next}];",
                input = layer.input_index,
                effects = effects,
                opacity = layer.opacity,
                position_x = layer.transform.position_x,
                position_y = layer.transform.position_y,
                start = layer.timeline_start,
                end = layer.timeline_end,
                source = source_label,
                current = current_video,
                next = next_label,
            ));
        } else {
            graph.push_str(&format!(
                "[{input}:v]setpts=PTS-STARTPTS{effects},scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=black,format=rgba,colorchannelmixer=aa={opacity:.6},format=yuv420p,setpts=PTS+{start:.6}/TB[{source}];[{current}][{source}]overlay=eof_action=pass:shortest=0:enable='between(t\\,{start:.6}\\,{end:.6})'[{next}];",
                input = layer.input_index,
                effects = effects,
                opacity = layer.opacity,
                start = layer.timeline_start,
                end = layer.timeline_end,
                source = source_label,
                current = current_video,
                next = next_label,
            ));
        }
        current_video = next_label;
    }
    for (index, layer) in text_layers.iter().enumerate() {
        let next_label = format!("text_{index}");
        graph.push_str(&format!(
            "[{current}]drawtext={drawtext}:enable='between(t\\,{start:.6}\\,{end:.6})'[{next}];",
            current = current_video,
            drawtext = drawtext_filter(&layer.overlay)?,
            start = layer.timeline_start,
            end = layer.timeline_end,
            next = next_label,
        ));
        current_video = next_label;
    }
    graph.push_str(&format!("[{current_video}]format=yuv420p[vout];"));

    let mut audio_track_labels: Vec<(String, String)> = Vec::new();
    for (layer_index, layer) in audio_layers.iter().enumerate() {
        let label = format!("a{layer_index}");
        let delay_ms = (layer.timeline_start * 1_000.0).round() as i64;
        graph.push_str(&format!(
            "[{input}:a]aresample=async=1:first_pts=0,asetpts=PTS-STARTPTS{effects},adelay={delay}|{delay}[{label}];",
            input = layer.input_index,
            effects = layer.effects,
            delay = delay_ms.max(0),
            label = label,
        ));
        audio_track_labels.push((layer.track_id.clone(), label));
    }

    let mut mixed_tracks: Vec<(String, String)> = Vec::new();
    for track in
        document.sequence.tracks.iter().filter(|track| {
            track.enabled && matches!(track.kind, TrackKind::Audio | TrackKind::Video)
        })
    {
        let labels = audio_track_labels
            .iter()
            .filter(|(track_id, _)| track_id == track.id.as_str())
            .map(|(_, label)| label.clone())
            .collect::<Vec<_>>();
        if labels.is_empty() {
            continue;
        }
        let output_label = format!("track_{}", mixed_tracks.len());
        if labels.len() == 1 {
            graph.push_str(&format!("[{}]anull[{}];", labels[0], output_label));
        } else {
            for label in &labels {
                graph.push_str(&format!("[{label}]"));
            }
            graph.push_str(&format!(
                "amix=inputs={}:duration=longest:dropout_transition=0:normalize=0[{}];",
                labels.len(),
                output_label
            ));
        }
        mixed_tracks.push((track.id.to_string(), output_label));
    }

    let mut source_counts = vec![0_usize; mixed_tracks.len()];
    for track in document
        .sequence
        .tracks
        .iter()
        .filter(|track| track.enabled && matches!(track.kind, TrackKind::Audio))
    {
        let Some(ducking) = &track.ducking else {
            continue;
        };
        mixed_tracks
            .iter()
            .position(|(track_id, _)| track_id == track.id.as_str())
            .ok_or_else(|| MediaError::InvalidPlan("ducking target has no audio clips".into()))?;
        let source_index = mixed_tracks
            .iter()
            .position(|(track_id, _)| track_id == ducking.source_track_id.as_str())
            .ok_or_else(|| MediaError::InvalidPlan("ducking source has no audio clips".into()))?;
        source_counts[source_index] += 1;
    }

    let mut final_tracks = mixed_tracks.clone();
    let mut source_outputs = (0..mixed_tracks.len())
        .map(|_| Vec::<String>::new())
        .collect::<Vec<_>>();
    for (index, count) in source_counts.iter().enumerate() {
        if *count == 0 {
            continue;
        }
        let source_label = final_tracks[index].1.clone();
        let labels = (0..=*count)
            .map(|fanout| format!("{source_label}_fan{fanout}"))
            .collect::<Vec<_>>();
        graph.push_str(&format!(
            "[{source_label}]asplit={}[{}];",
            labels.len(),
            labels.join("][")
        ));
        final_tracks[index].1 = labels[0].clone();
        source_outputs[index] = labels[1..].to_vec();
    }

    let mut source_cursors = vec![0_usize; mixed_tracks.len()];
    for track in document
        .sequence
        .tracks
        .iter()
        .filter(|track| track.enabled && matches!(track.kind, TrackKind::Audio))
    {
        let Some(ducking) = &track.ducking else {
            continue;
        };
        let target_index = final_tracks
            .iter()
            .position(|(track_id, _)| track_id == track.id.as_str())
            .ok_or_else(|| MediaError::InvalidPlan("ducking target has no audio clips".into()))?;
        let source_index = mixed_tracks
            .iter()
            .position(|(track_id, _)| track_id == ducking.source_track_id.as_str())
            .ok_or_else(|| MediaError::InvalidPlan("ducking source has no audio clips".into()))?;
        let cursor = source_cursors[source_index];
        let source_label = source_outputs[source_index]
            .get(cursor)
            .cloned()
            .ok_or_else(|| MediaError::InvalidPlan("ducking source fan-out is invalid".into()))?;
        source_cursors[source_index] += 1;
        let ducked_label = format!("ducked_{target_index}");
        let threshold = 10_f64.powf(f64::from(ducking.threshold_db) / 20.0);
        graph.push_str(&format!(
            "[{}][{}]sidechaincompress=threshold={threshold:.8}:ratio={ratio:.4}:attack={attack:.4}:release={release:.4}:makeup=1[{}];",
            final_tracks[target_index].1,
            source_label,
            ducked_label,
            ratio = ducking.ratio,
            attack = ducking.attack_ms,
            release = ducking.release_ms,
        ));
        final_tracks[target_index].1 = ducked_label;
    }

    let has_audio = !final_tracks.is_empty();
    if has_audio {
        for (_, label) in &final_tracks {
            graph.push_str(&format!("[{label}]"));
        }
        if final_tracks.len() == 1 {
            graph.push_str("anull[aout];");
        } else {
            graph.push_str(&format!(
                "amix=inputs={}:duration=longest:dropout_transition=0:normalize=0[aout];",
                final_tracks.len()
            ));
        }
    }

    let output_temp = unique_temporary_output_path(&output, inputs.len())?;
    let mut filter_argv = vec![
        "-filter_complex".into(),
        graph.clone(),
        "-map".into(),
        "[vout]".into(),
    ];
    if has_audio {
        filter_argv.extend(["-map".into(), "[aout]".into()]);
    }
    filter_argv.extend([
        "-c:v".into(),
        "libx264".into(),
        "-profile:v".into(),
        "high".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-r".into(),
        frame_rate,
        "-b:v".into(),
        profile.video_bitrate().into(),
    ]);
    if has_audio {
        filter_argv.extend([
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            profile.audio_bitrate().into(),
            "-ar".into(),
            profile.audio_sample_rate().to_string(),
            "-ac".into(),
            profile.audio_channels().to_string(),
        ]);
    }
    filter_argv.extend([
        "-movflags".into(),
        "+faststart".into(),
        "-t".into(),
        format_seconds(duration),
        "-f".into(),
        "mp4".into(),
        "-progress".into(),
        "pipe:1".into(),
        output_temp.to_string_lossy().into(),
    ]);
    argv.extend(filter_argv);
    Ok(RenderPlan {
        steps: vec![RenderStep::Composite(CompositeStep {
            inputs,
            output: output_temp,
            filter_complex: graph,
            argv,
        })],
        concat_list: ConcatList {
            path: unique_sibling_path(&output, "composite-plan"),
            entries: Vec::new(),
        },
        output,
        expected_output: ExpectedOutput {
            stream_kinds: if has_audio {
                vec![StreamKind::Video, StreamKind::Audio]
            } else {
                vec![StreamKind::Video]
            },
            duration_seconds: duration,
            duration_tolerance_seconds: 0.25,
        },
        binary_contract: executables.binary_contract.clone(),
    })
}

pub fn build_render_plan_with_profile(
    document: &ProjectDocument,
    project_root: &Path,
    output: impl Into<PathBuf>,
    executables: &ExecutableConfig,
    profile: ExportProfile,
) -> Result<LayeredRenderPlan, MediaError> {
    if profile == ExportProfile::Baseline {
        return Err(MediaError::Unsupported(
            "baseline export uses the legacy render plan".into(),
        ));
    }
    let output = output.into();
    let render_plan = build_export_plan(document, project_root, output, executables, profile)?;
    let RenderPlan {
        steps,
        output,
        expected_output: legacy_expected,
        binary_contract,
        ..
    } = render_plan;
    let step = match steps.into_iter().next() {
        Some(RenderStep::Composite(step)) => step,
        _ => {
            return Err(MediaError::InvalidPlan(
                "profile export did not produce a composite step".into(),
            ))
        }
    };
    let (width, height) = profile.dimensions(document.sequence.width, document.sequence.height);
    let has_audio = legacy_expected.stream_kinds.contains(&StreamKind::Audio);
    let expected_output = ProfileExpectedOutput {
        profile,
        stream_kinds: legacy_expected.stream_kinds,
        duration_seconds: legacy_expected.duration_seconds,
        duration_tolerance_seconds: legacy_expected.duration_tolerance_seconds,
        width,
        height,
        video_codec: "h264".into(),
        audio_codec: has_audio.then(|| "aac".into()),
        audio_sample_rate: has_audio.then(|| profile.audio_sample_rate()),
        audio_channels: has_audio.then(|| profile.audio_channels()),
        container: "mp4".into(),
    };
    expected_output.validate()?;
    let plan = LayeredRenderPlan {
        step,
        output,
        expected_output,
        binary_contract,
        profile,
    };
    validate_layered_render_plan(&plan, executables)?;
    Ok(plan)
}

fn asset_for_clip<'a>(
    document: &'a ProjectDocument,
    clip: &Clip,
) -> Result<&'a editor_domain::Asset, MediaError> {
    document
        .assets
        .iter()
        .find(|asset| asset.id == clip.asset_id)
        .ok_or_else(|| {
            MediaError::Domain(DomainError::NotFound {
                entity: "asset".into(),
                id: clip.asset_id.to_string(),
            })
        })
}

fn video_effects(
    clip: &Clip,
    project_root: &Path,
    _timebase: Rational,
) -> Result<String, MediaError> {
    let mut filters = Vec::new();
    for effect in &clip.effects {
        match effect {
            Effect::Brightness { value } => {
                filters.push(format!("eq=brightness={value:.6}"));
            }
            Effect::Contrast { value } => {
                filters.push(format!("eq=contrast={value:.6}"));
            }
            Effect::Saturation { value } => {
                filters.push(format!("eq=saturation={value:.6}"));
            }
            Effect::Exposure { value } => {
                filters.push(format!("exposure=exposure={value:.6}"));
            }
            Effect::Gamma { value } => {
                filters.push(format!("eq=gamma={value:.6}"));
            }
            Effect::Temperature { kelvin } => {
                filters.push(format!("colortemperature=temperature={kelvin:.6}"));
            }
            Effect::Tint { value } => {
                filters.push(format!("colorbalance=rm={value:.6}:gm={value:.6}:bm={:.6}", -value));
            }
            Effect::ColorBalance {
                shadows,
                midtones,
                highlights,
            } => filters.push(format!(
                "colorbalance=rs={:.6}:gs={:.6}:bs={:.6}:rm={:.6}:gm={:.6}:bm={:.6}:rh={:.6}:gh={:.6}:bh={:.6}",
                shadows.red,
                shadows.green,
                shadows.blue,
                midtones.red,
                midtones.green,
                midtones.blue,
                highlights.red,
                highlights.green,
                highlights.blue,
            )),
            Effect::Crop {
                left,
                top,
                right,
                bottom,
            } => filters.push(format!(
                "crop=iw*(1-{left:.6}-{right:.6}):ih*(1-{top:.6}-{bottom:.6}):iw*{left:.6}:ih*{top:.6}"
            )),
            Effect::Rotate { degrees } => filters.push(format!(
                "rotate={:.6}:ow=rotw(iw):oh=roth(ih):c=none",
                f64::from(*degrees) * std::f64::consts::PI / 180.0
            )),
            Effect::Blur { radius } => filters.push(format!("gblur=sigma={radius:.6}")),
            Effect::Sharpen { amount } => {
                filters.push(format!("unsharp=5:5:{amount:.6}:5:5:0"));
            }
            Effect::Vignette { amount } => filters.push(format!(
                "vignette=angle={:.6}",
                0.2 + f64::from(*amount) * 1.2
            )),
            Effect::Duotone {
                shadows,
                highlights,
            } => filters.push(duotone_filter(*shadows, *highlights)),
            Effect::Lut { relative_path } => {
                if relative_path
                    .as_str()
                    .rsplit('.')
                    .next()
                    .is_none_or(|extension| !extension.eq_ignore_ascii_case("cube"))
                {
                    return Err(MediaError::Unsupported(
                        "LUT effects require a project-relative .cube file".into(),
                    ));
                }
                let path = resolve_safe_path(project_root, relative_path.as_str())?;
                filters.push(format!("lut3d='{}'", escape_filter_value(&path.to_string_lossy())));
            }
            Effect::Speed { .. } | Effect::Volume { .. } | Effect::Fade { .. } => {}
        }
    }
    if filters.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!(",{}", filters.join(",")))
    }
}

fn duotone_filter(shadows: RgbColor, highlights: RgbColor) -> String {
    let component = |shadow: u8, highlight: u8| {
        let slope = (f64::from(highlight) - f64::from(shadow)) / 255.0;
        format!("{shadow}+val*{slope:.9}")
    };
    format!(
        "hue=s=0,lutrgb=r='{}':g='{}':b='{}'",
        component(shadows.red, highlights.red),
        component(shadows.green, highlights.green),
        component(shadows.blue, highlights.blue),
    )
}

fn transform_scale_filter(transform: &Transform, overlay: bool) -> String {
    if (transform.scale_x - 1.0).abs() < f32::EPSILON
        && (transform.scale_y - 1.0).abs() < f32::EPSILON
    {
        return String::new();
    }
    let flags = if overlay { ":flags=lanczos" } else { "" };
    format!(
        ",scale=iw*{:.6}:ih*{:.6}{flags}",
        transform.scale_x.max(0.01),
        transform.scale_y.max(0.01),
    )
}

fn transform_rotate_filter(transform: &Transform) -> String {
    if transform.rotation_degrees.abs() < f32::EPSILON {
        return String::new();
    }
    format!(
        ",rotate={:.6}:ow=rotw(iw):oh=roth(ih):c=none",
        f64::from(transform.rotation_degrees) * std::f64::consts::PI / 180.0
    )
}

fn escape_filter_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(':', "\\:")
        .replace(',', "\\,")
        .replace(';', "\\;")
        .replace('\'', "\\'")
}

fn drawtext_filter(overlay: &TextOverlay) -> Result<String, MediaError> {
    let color = format!("0x{}", &overlay.color[1..]);
    let mut options = vec![
        format!("text='{}'", escape_filter_value(&overlay.text)),
        format!("fontcolor={color}"),
        format!("fontsize={:.2}", overlay.font_size),
        format!("x=(w-text_w)/2+({:.6}*w/2)", overlay.position_x),
        format!("y=(h-text_h)/2+({:.6}*h/2)", overlay.position_y),
    ];
    if let Some(font) = default_font_file() {
        options.insert(
            0,
            format!(
                "fontfile='{}'",
                escape_filter_value(&font.to_string_lossy())
            ),
        );
    }
    Ok(options.join(":"))
}

fn default_font_file() -> Option<PathBuf> {
    let root = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:/Windows"));
    let windows_font = root.join("Fonts").join("arial.ttf");
    if windows_font.is_file() {
        return Some(windows_font);
    }
    let linux_font = PathBuf::from("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf");
    linux_font.is_file().then_some(linux_font)
}

fn audio_effects(
    clip: &Clip,
    timebase: Rational,
    ignore_visual_effects: bool,
) -> Result<String, MediaError> {
    let mut filters = Vec::new();
    let clip_duration = ticks_to_seconds(clip.timeline_duration, timebase)?;
    for effect in &clip.effects {
        match effect {
            Effect::Volume { gain_db } => filters.push(format!("volume={gain_db:.4}dB")),
            Effect::Fade {
                kind: FadeKind::In,
                duration_ticks,
            } => filters.push(format!(
                "afade=t=in:st=0:d={:.6}",
                ticks_to_seconds(*duration_ticks, timebase)?
            )),
            Effect::Fade {
                kind: FadeKind::Out,
                duration_ticks,
            } => {
                let fade = ticks_to_seconds(*duration_ticks, timebase)?;
                filters.push(format!(
                    "afade=t=out:st={:.6}:d={fade:.6}",
                    (clip_duration - fade).max(0.0)
                ));
            }
            _ if ignore_visual_effects => {}
            _ => {
                return Err(MediaError::Unsupported(
                    "audio layer contains an unsupported effect".into(),
                ))
            }
        }
    }
    if filters.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!(",{}", filters.join(",")))
    }
}

fn profile_name(profile: ExportProfile) -> &'static str {
    match profile {
        ExportProfile::Baseline => "Baseline",
        ExportProfile::Youtube => "YouTube",
        ExportProfile::Instagram => "Instagram",
        ExportProfile::Tiktok => "TikTok",
    }
}

fn ensure_concat_compatible(
    reference: &editor_domain::ProbeSummary,
    candidate: &editor_domain::ProbeSummary,
) -> Result<(), MediaError> {
    let same_video = reference
        .video
        .as_ref()
        .map(|s| (&s.codec, s.width, s.height))
        == candidate
            .video
            .as_ref()
            .map(|s| (&s.codec, s.width, s.height));
    let same_audio = reference
        .audio
        .as_ref()
        .map(|s| (&s.codec, s.sample_rate, s.channels))
        == candidate
            .audio
            .as_ref()
            .map(|s| (&s.codec, s.sample_rate, s.channels));
    if !same_video || !same_audio || reference.stream_timebase != candidate.stream_timebase {
        return Err(MediaError::Unsupported(
            "concat copy requires matching codec, resolution, stream timebase, and audio layout"
                .into(),
        ));
    }
    Ok(())
}
impl StreamKind {
    fn as_ffmpeg(&self) -> &'static str {
        match self {
            Self::Video => "v",
            Self::Audio => "a",
        }
    }
}
fn stream_maps_for_kinds(kinds: &[StreamKind]) -> Vec<StreamMap> {
    kinds
        .iter()
        .enumerate()
        .map(|(index, kind)| StreamMap {
            input_index: 0,
            stream_kind: kind.clone(),
            stream_index: index as u32,
        })
        .collect()
}
fn ticks_to_seconds(ticks: i64, timebase: Rational) -> Result<f64, MediaError> {
    if ticks < 0 {
        return Err(MediaError::InvalidPlan(
            "timeline ticks must not be negative".into(),
        ));
    }
    let seconds = ticks as f64 * timebase.denominator as f64 / timebase.numerator as f64;
    if seconds.is_finite() {
        Ok(seconds)
    } else {
        Err(MediaError::InvalidPlan(
            "timeline duration is not finite".into(),
        ))
    }
}
fn format_seconds(seconds: f64) -> String {
    format!("{seconds:.6}")
}

fn format_rational(value: Rational) -> String {
    format!("{}/{}", value.numerator, value.denominator)
}

pub fn validate_relative_alias(alias: &str) -> Result<(), MediaError> {
    let path = Path::new(alias);
    if alias.is_empty()
        || path.is_absolute()
        || alias.starts_with('\\')
        || alias.as_bytes().get(1) == Some(&b':')
    {
        return Err(MediaError::PathViolation(alias.into()));
    }
    for component in path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(MediaError::PathViolation(alias.into()));
        }
    }
    Ok(())
}
pub fn resolve_safe_path(root: &Path, alias: &str) -> Result<PathBuf, MediaError> {
    validate_relative_alias(alias)?;
    let root = fs::canonicalize(root).map_err(MediaError::Io)?;
    let canonical = fs::canonicalize(root.join(alias)).map_err(MediaError::Io)?;
    if canonical.starts_with(&root) {
        Ok(canonical)
    } else {
        Err(MediaError::PathViolation(alias.into()))
    }
}
pub fn validate_output_path(output: PathBuf) -> Result<PathBuf, MediaError> {
    if output.as_os_str().is_empty() || !output.is_absolute() || output.file_name().is_none() {
        return Err(MediaError::PathViolation(output.to_string_lossy().into()));
    }
    if output.exists() && output.is_dir() {
        return Err(MediaError::InvalidOutput(
            "output path is a directory".into(),
        ));
    }
    Ok(output)
}
pub fn temporary_output_path(output: &Path, ordinal: usize) -> Result<PathBuf, MediaError> {
    let output = validate_output_path(output.to_owned())?;
    let name = output
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| MediaError::InvalidOutput("output filename is not valid UTF-8".into()))?;
    Ok(output.with_file_name(format!(".{name}.{ordinal}.part")))
}
fn unique_temporary_output_path(output: &Path, ordinal: usize) -> Result<PathBuf, MediaError> {
    Ok(unique_sibling_path(
        &validate_output_path(output.to_owned())?,
        &format!("segment-{ordinal}"),
    ))
}
fn unique_sibling_path(output: &Path, label: &str) -> PathBuf {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);
    let name = output
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("output");
    let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    output.with_file_name(format!(
        ".{name}.{label}.{}.part",
        (std::process::id() as u64).saturating_add(id)
    ))
}
fn paths_alias(left: &Path, right: &Path) -> bool {
    left == right
        || matches!((fs::canonicalize(left), fs::canonicalize(right)), (Ok(a), Ok(b)) if a == b)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessOutput {
    pub status_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
pub trait ChildProcessRunner {
    fn run(&self, executable: &Path, argv: &[String]) -> Result<ProcessOutput, MediaError>;
}
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemChildProcessRunner;
impl ChildProcessRunner for SystemChildProcessRunner {
    fn run(&self, executable: &Path, argv: &[String]) -> Result<ProcessOutput, MediaError> {
        run_cancellable(executable, argv, &|| false)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressEvent {
    pub key: String,
    pub value: String,
}
#[derive(Clone, Debug, PartialEq)]
pub struct VerifiedOutput {
    pub probe: ProbeMetadata,
    pub decode_succeeded: bool,
    pub progress: Vec<ProgressEvent>,
}

#[derive(Clone, Debug)]
pub struct FfmpegExecutor {
    config: ExecutableConfig,
}
impl FfmpegExecutor {
    pub fn new(config: ExecutableConfig) -> Self {
        Self { config }
    }
    pub fn config(&self) -> &ExecutableConfig {
        &self.config
    }
    pub fn execute(
        &self,
        plan: &RenderPlan,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<VerifiedOutput, MediaError> {
        let list_path = unique_sibling_path(&plan.output, "concat");
        let result = self.execute_inner(plan, &list_path, cancelled);
        cleanup_file(&list_path);
        if result.is_err() {
            cleanup_plan_temps(plan);
        }
        result
    }
    pub fn execute_profile(
        &self,
        plan: &LayeredRenderPlan,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<VerifiedOutput, MediaError> {
        let result = self.execute_profile_inner(plan, cancelled);
        if result.is_err() {
            cleanup_file(&plan.step.output);
        }
        result
    }
    fn execute_profile_inner(
        &self,
        plan: &LayeredRenderPlan,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<VerifiedOutput, MediaError> {
        self.config
            .validate_for_execution("ffmpeg", &self.config.ffmpeg)?;
        self.config
            .validate_for_execution("ffprobe", &self.config.ffprobe)?;
        validate_layered_render_plan(plan, &self.config)?;
        if let Some(parent) = plan.output.parent() {
            if !parent.is_dir() {
                return Err(MediaError::InvalidOutput(
                    "output parent directory does not exist".into(),
                ));
            }
        }
        if cancelled() {
            return Err(MediaError::Cancelled);
        }
        let output = run_cancellable(&self.config.ffmpeg, &plan.step.argv, cancelled)?;
        if output.status_code != Some(0) {
            return Err(MediaError::ProcessFailed {
                code: output.status_code,
                stderr: String::from_utf8_lossy(&output.stderr).into(),
            });
        }
        let progress = parse_progress(&output.stdout);
        let verified = finalize_temp_output_with(
            &plan.step.output,
            &plan.output,
            &output,
            &self.config,
            cancelled,
            |probe| plan.expected_output.matches_probe(probe),
        )?;
        Ok(VerifiedOutput {
            probe: verified.probe,
            decode_succeeded: verified.decode_succeeded,
            progress,
        })
    }
    fn execute_inner(
        &self,
        plan: &RenderPlan,
        list_path: &Path,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<VerifiedOutput, MediaError> {
        self.config
            .validate_for_execution("ffmpeg", &self.config.ffmpeg)?;
        self.config
            .validate_for_execution("ffprobe", &self.config.ffprobe)?;
        validate_render_plan(plan, &self.config)?;
        let final_temp = match plan.steps.last() {
            Some(RenderStep::ConcatMux(step)) => &step.output,
            Some(RenderStep::Composite(step)) => &step.output,
            _ => {
                return Err(MediaError::InvalidPlan(
                    "render plan must end with a final media step".into(),
                ))
            }
        };
        if let Some(parent) = plan.output.parent() {
            if !parent.is_dir() {
                return Err(MediaError::InvalidOutput(
                    "output parent directory does not exist".into(),
                ));
            }
        }
        if plan
            .steps
            .iter()
            .any(|step| matches!(step, RenderStep::ConcatMux(_)))
        {
            write_concat_list(list_path, &plan.concat_list.entries)?;
        }
        let mut progress = Vec::new();
        for step in &plan.steps {
            if cancelled() {
                return Err(MediaError::Cancelled);
            }
            let argv = if matches!(step, RenderStep::ConcatMux(_)) {
                replace_concat_input(step.argv(), list_path)?
            } else {
                step.argv().to_vec()
            };
            let output = run_cancellable(&self.config.ffmpeg, &argv, cancelled)?;
            if output.status_code != Some(0) {
                return Err(MediaError::ProcessFailed {
                    code: output.status_code,
                    stderr: String::from_utf8_lossy(&output.stderr).into(),
                });
            }
            progress.extend(parse_progress(&output.stdout));
        }
        let process = ProcessOutput {
            status_code: Some(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        let verified = finalize_temp_output(
            final_temp,
            &plan.output,
            &plan.expected_output,
            &process,
            &self.config,
            cancelled,
        )?;
        Ok(VerifiedOutput {
            probe: verified.probe,
            decode_succeeded: verified.decode_succeeded,
            progress,
        })
    }
}

fn validate_layered_render_plan(
    plan: &LayeredRenderPlan,
    config: &ExecutableConfig,
) -> Result<(), MediaError> {
    if plan.binary_contract != config.binary_contract
        || plan.profile != plan.expected_output.profile
    {
        return Err(MediaError::InvalidPlan(
            "profile render plan binary contract or profile is invalid".into(),
        ));
    }
    plan.expected_output.validate()?;
    validate_output_path(plan.output.clone())?;
    let step = &plan.step;
    if step.inputs.is_empty()
        || step.output == plan.output
        || !step.output.is_absolute()
        || step.filter_complex.trim().is_empty()
        || step.argv.is_empty()
    {
        return Err(MediaError::InvalidPlan(
            "profile composite input, filter, or output is invalid".into(),
        ));
    }
    if !step.argv.iter().any(|arg| arg == "-nostdin")
        || !step
            .argv
            .windows(2)
            .any(|pair| pair == ["-progress", "pipe:1"])
        || !step.argv.windows(2).any(|pair| pair == ["-f", "mp4"])
    {
        return Err(MediaError::InvalidPlan(
            "profile FFmpeg argv requires -nostdin, -f mp4, and progress output".into(),
        ));
    }
    let filter_index = step
        .argv
        .iter()
        .position(|arg| arg == "-filter_complex")
        .ok_or_else(|| {
            MediaError::InvalidPlan("profile argv has no filter_complex option".into())
        })?;
    if step.argv.get(filter_index + 1) != Some(&step.filter_complex)
        || step.argv.last() != Some(&step.output.to_string_lossy().into_owned())
    {
        return Err(MediaError::InvalidPlan(
            "profile argv does not match its typed composite step".into(),
        ));
    }
    for input in &step.inputs {
        if !input.is_absolute()
            || !input.is_file()
            || paths_alias(input, &plan.output)
            || paths_alias(input, &step.output)
        {
            return Err(MediaError::PathViolation(
                "profile input/output paths are invalid or aliased".into(),
            ));
        }
    }
    Ok(())
}

fn validate_render_plan(plan: &RenderPlan, config: &ExecutableConfig) -> Result<(), MediaError> {
    if plan.steps.is_empty() || plan.binary_contract != config.binary_contract {
        return Err(MediaError::InvalidPlan(
            "render plan binary contract or steps are invalid".into(),
        ));
    }
    plan.expected_output.validate()?;
    validate_output_path(plan.output.clone())?;
    for step in &plan.steps {
        let argv = step.argv();
        if !argv.windows(2).any(|pair| pair == ["-progress", "pipe:1"])
            || !argv.iter().any(|arg| arg == "-nostdin")
        {
            return Err(MediaError::InvalidPlan(
                "every FFmpeg step requires -nostdin and -progress pipe:1".into(),
            ));
        }
        match step {
            RenderStep::TrimClip(value) => {
                if !value.input.is_absolute()
                    || !value.input.is_file()
                    || !value.output.is_absolute()
                    || paths_alias(&value.input, &plan.output)
                {
                    return Err(MediaError::PathViolation(
                        "render input/output paths are invalid or aliased".into(),
                    ));
                }
            }
            RenderStep::ConcatMux(value) => {
                if value.inputs.is_empty() || value.output == plan.output {
                    return Err(MediaError::InvalidPlan("concat output is invalid".into()));
                }
                for input in &value.inputs {
                    if !input.is_absolute() {
                        return Err(MediaError::PathViolation(input.to_string_lossy().into()));
                    }
                }
            }
            RenderStep::Composite(value) => {
                if value.inputs.is_empty()
                    || !value.output.is_absolute()
                    || value.output == plan.output
                    || value.filter_complex.trim().is_empty()
                {
                    return Err(MediaError::InvalidPlan(
                        "composite input, filter, or output is invalid".into(),
                    ));
                }
                for input in &value.inputs {
                    if !input.is_absolute() || !input.is_file() || paths_alias(input, &plan.output)
                    {
                        return Err(MediaError::PathViolation(
                            "composite input/output paths are invalid or aliased".into(),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}
fn replace_concat_input(argv: &[String], list_path: &Path) -> Result<Vec<String>, MediaError> {
    let mut result = argv.to_vec();
    let Some(index) = result.iter().position(|arg| arg == "-i") else {
        return Err(MediaError::InvalidPlan(
            "concat step has no input argument".into(),
        ));
    };
    if index + 1 >= result.len() {
        return Err(MediaError::InvalidPlan(
            "concat step input argument is incomplete".into(),
        ));
    }
    result[index + 1] = list_path.to_string_lossy().into();
    Ok(result)
}
fn write_concat_list(path: &Path, entries: &[PathBuf]) -> Result<(), MediaError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(MediaError::Io)?;
    for entry in entries {
        if !entry.is_absolute() || !entry.is_file() {
            return Err(MediaError::PathViolation(entry.to_string_lossy().into()));
        }
        let value = entry
            .to_string_lossy()
            .replace('\\', "/")
            .replace('\'', "'\\''");
        writeln!(file, "file '{value}'").map_err(MediaError::Io)?;
    }
    file.flush().map_err(MediaError::Io)
}
fn parse_progress(stdout: &[u8]) -> Vec<ProgressEvent> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| {
            line.split_once('=').map(|(key, value)| ProgressEvent {
                key: key.to_owned(),
                value: value.to_owned(),
            })
        })
        .take(MAX_PROGRESS_EVENTS)
        .collect()
}
fn run_cancellable(
    executable: &Path,
    argv: &[String],
    cancelled: &dyn Fn() -> bool,
) -> Result<ProcessOutput, MediaError> {
    let mut child = Command::new(executable)
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(MediaError::Io)?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(MediaError::InvalidConfiguration(
                "FFmpeg stdout pipe was not created".into(),
            ));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(MediaError::InvalidConfiguration(
                "FFmpeg stderr pipe was not created".into(),
            ));
        }
    };
    let stdout_thread = thread::spawn(move || read_pipe(stdout));
    let stderr_thread = thread::spawn(move || read_pipe(stderr));
    let status = match wait_cancellable(&mut child, cancelled) {
        Ok(status) => status,
        Err(error) => {
            let _ = stdout_thread.join();
            let _ = stderr_thread.join();
            return Err(error);
        }
    };
    let stdout_result = match stdout_thread.join() {
        Ok(result) => result.map_err(MediaError::Io),
        Err(_) => Err(MediaError::ProcessFailed {
            code: status.code(),
            stderr: "FFmpeg stdout reader failed".into(),
        }),
    };
    let stderr_result = match stderr_thread.join() {
        Ok(result) => result.map_err(MediaError::Io),
        Err(_) => Err(MediaError::ProcessFailed {
            code: status.code(),
            stderr: "FFmpeg stderr reader failed".into(),
        }),
    };
    let stdout = stdout_result?;
    let stderr = stderr_result?;
    Ok(ProcessOutput {
        status_code: status.code(),
        stdout,
        stderr,
    })
}
fn read_pipe(mut pipe: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(8 * 1024);
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let count = pipe.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let remaining = MAX_PROCESS_OUTPUT_BYTES.saturating_sub(bytes.len());
        let keep = remaining.min(count);
        bytes.extend_from_slice(&buffer[..keep]);
    }
    Ok(bytes)
}
fn wait_cancellable(
    child: &mut Child,
    cancelled: &dyn Fn() -> bool,
) -> Result<std::process::ExitStatus, MediaError> {
    loop {
        if cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(MediaError::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(MediaError::Io(error));
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[derive(Clone, Debug, PartialEq)]
struct InternalFinalization {
    probe: ProbeMetadata,
    decode_succeeded: bool,
}
fn finalize_temp_output(
    temp: &Path,
    output: &Path,
    expected: &ExpectedOutput,
    process: &ProcessOutput,
    config: &ExecutableConfig,
    cancelled: &dyn Fn() -> bool,
) -> Result<InternalFinalization, MediaError> {
    finalize_temp_output_with(temp, output, process, config, cancelled, |probe| {
        expected.matches_probe(probe)
    })
}

fn finalize_temp_output_with<F>(
    temp: &Path,
    output: &Path,
    process: &ProcessOutput,
    config: &ExecutableConfig,
    cancelled: &dyn Fn() -> bool,
    validate_expected: F,
) -> Result<InternalFinalization, MediaError>
where
    F: FnOnce(&ProbeMetadata) -> Result<(), MediaError>,
{
    if process.status_code != Some(0) {
        return Err(MediaError::ProcessFailed {
            code: process.status_code,
            stderr: String::from_utf8_lossy(&process.stderr).into(),
        });
    }
    if cancelled() {
        return Err(MediaError::Cancelled);
    }
    let metadata = fs::symlink_metadata(temp).map_err(MediaError::Io)?;
    if !metadata.file_type().is_file() || metadata.len() == 0 {
        return Err(MediaError::InvalidOutput(
            "temporary output is missing or empty".into(),
        ));
    }
    let probe = probe_media(config, temp, &SystemChildProcessRunner)?;
    validate_expected(&probe)?;
    let decode_succeeded = decode_output(config, temp)?;
    if !decode_succeeded {
        return Err(MediaError::InvalidOutput(
            "output decode check failed".into(),
        ));
    }
    if cancelled() {
        return Err(MediaError::Cancelled);
    }
    atomic_replace(temp, output)?;
    Ok(InternalFinalization {
        probe,
        decode_succeeded,
    })
}
fn decode_output(config: &ExecutableConfig, input: &Path) -> Result<bool, MediaError> {
    let argv = vec![
        "-nostdin".into(),
        "-v".into(),
        "error".into(),
        "-xerror".into(),
        "-i".into(),
        input.to_string_lossy().into(),
        "-map".into(),
        "0:v?".into(),
        "-map".into(),
        "0:a?".into(),
        "-f".into(),
        "null".into(),
        "-".into(),
    ];
    let result = SystemChildProcessRunner.run(&config.ffmpeg, &argv)?;
    Ok(result.status_code == Some(0))
}
fn atomic_replace(temp: &Path, output: &Path) -> Result<(), MediaError> {
    validate_output_path(output.to_owned())?;
    if temp == output {
        return Err(MediaError::InvalidOutput(
            "temporary output must be distinct from final output".into(),
        ));
    }
    #[cfg(windows)]
    {
        windows_atomic_replace(temp, output)
    }
    #[cfg(not(windows))]
    {
        fs::rename(temp, output).map_err(MediaError::Io)
    }
}
#[cfg(windows)]
fn windows_atomic_replace(temp: &Path, output: &Path) -> Result<(), MediaError> {
    use std::{ffi::OsStr, os::windows::ffi::OsStrExt};
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    fn wide(path: &OsStr) -> Vec<u16> {
        path.encode_wide().chain(std::iter::once(0)).collect()
    }
    let temp = wide(temp.as_os_str());
    let output = wide(output.as_os_str());
    if unsafe {
        MoveFileExW(
            temp.as_ptr(),
            output.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(MediaError::Io(io::Error::last_os_error()));
    }
    Ok(())
}
fn cleanup_file(path: &Path) {
    let _ = fs::remove_file(path);
}
fn cleanup_plan_temps(plan: &RenderPlan) {
    for step in &plan.steps {
        match step {
            RenderStep::TrimClip(value) => cleanup_file(&value.output),
            RenderStep::ConcatMux(value) => cleanup_file(&value.output),
            RenderStep::Composite(value) => cleanup_file(&value.output),
        }
    }
}

#[derive(Debug)]
pub enum MediaError {
    Io(io::Error),
    Domain(DomainError),
    InvalidConfiguration(String),
    BinaryUnavailable { reason: String },
    ExecutableUnavailable { name: String, path: PathBuf },
    ProbeParse(String),
    ProbeValidation(String),
    InvalidPlan(String),
    PathViolation(String),
    InvalidOutput(String),
    Unsupported(String),
    ProcessFailed { code: Option<i32>, stderr: String },
    Cancelled,
}
impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "media I/O error: {e}"),
            Self::Domain(e) => write!(f, "domain error: {e}"),
            Self::InvalidConfiguration(e) => write!(f, "invalid media configuration: {e}"),
            Self::BinaryUnavailable { reason } => write!(f, "media binary unavailable: {reason}"),
            Self::ExecutableUnavailable { name, path } => {
                write!(f, "{name} unavailable at {}", path.display())
            }
            Self::ProbeParse(e) => write!(f, "ffprobe parse error: {e}"),
            Self::ProbeValidation(e) => write!(f, "invalid probe metadata: {e}"),
            Self::InvalidPlan(e) => write!(f, "invalid media plan: {e}"),
            Self::PathViolation(e) => write!(f, "unsafe media path: {e}"),
            Self::InvalidOutput(e) => write!(f, "invalid output: {e}"),
            Self::Unsupported(e) => write!(f, "unsupported media operation: {e}"),
            Self::ProcessFailed { code, stderr } => {
                write!(f, "media process failed ({code:?}): {stderr}")
            }
            Self::Cancelled => write!(f, "media operation cancelled"),
        }
    }
}
impl Error for MediaError {}
impl From<io::Error> for MediaError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_domain::{
        Asset, AssetId, AssetKind, AssetStatus, AudioStream, Clip, Effect, Fingerprint,
        ProbeSummary, ProjectId, RelativePath, RgbColor, RgbDelta, TextOverlay, Track, TrackId,
        TrackKind, Transform, VideoStream,
    };
    use std::time::{SystemTime, UNIX_EPOCH};
    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "editor-media-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
    fn summary(timebase: Rational, codec: &str) -> ProbeSummary {
        ProbeSummary {
            duration_ticks: 60,
            stream_timebase: timebase,
            video: Some(VideoStream {
                codec: codec.into(),
                width: 1920,
                height: 1080,
                frame_rate: Some(Rational::new(30, 1).unwrap()),
            }),
            audio: Some(AudioStream {
                codec: "aac".into(),
                sample_rate: 48_000,
                channels: 2,
            }),
            rotation_degrees: None,
            raw_tool_version: "ffprobe 6.1".into(),
        }
    }
    fn document(root: &Path, timebase: Rational, second_codec: &str) -> ProjectDocument {
        fs::create_dir_all(root.join("media")).unwrap();
        fs::write(root.join("media/one.mp4"), [1]).unwrap();
        fs::write(root.join("media/two.mp4"), [2]).unwrap();
        let asset = |id: &str, path: &str, probe: ProbeSummary| Asset {
            id: editor_domain::AssetId::new(id).unwrap(),
            relative_path: editor_domain::RelativePath::new(path).unwrap(),
            kind: AssetKind::Video,
            fingerprint: Fingerprint {
                size_bytes: 1,
                modified_time: "test".into(),
                sha256: None,
            },
            probe: Some(probe),
            status: AssetStatus::Available,
        };
        let clip = |id: &str, asset_id: &str, start| Clip {
            id: ClipId::new(id).unwrap(),
            asset_id: editor_domain::AssetId::new(asset_id).unwrap(),
            timeline_start: start,
            timeline_duration: 30,
            source_start: 0,
            source_duration: 30,
            speed: Rational::new(1, 1).unwrap(),
            opacity: 1.0,
            transform: Transform::default(),
            effects: vec![],
            keyframes: vec![],
            text_overlay: None,
        };
        let mut track = Track::new(
            editor_domain::TrackId::new("video").unwrap(),
            TrackKind::Video,
            "Video",
        )
        .unwrap();
        track.clips = vec![clip("clip-b", "asset-b", 30), clip("clip-a", "asset-a", 0)];
        let mut doc = ProjectDocument::create(ProjectId::new("project").unwrap(), "Test").unwrap();
        doc.assets = vec![
            asset("asset-a", "media/one.mp4", summary(timebase, "h264")),
            asset("asset-b", "media/two.mp4", summary(timebase, second_codec)),
        ];
        doc.sequence.tracks = vec![track];
        doc
    }
    #[test]
    fn parse_probe_metadata_rejects_malformed_probe() {
        let json = r#"{"program_version":{"version":"6.1"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"time_base":"1/90000","duration":"2.5"},{"index":1,"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2,"time_base":"1/48000","duration":"2.5"}],"format":{"duration":"2.5","format_name":"mov,mp4"}}"#;
        assert!(parse_probe_json(json).is_ok());
        assert!(
            parse_probe_json(&json.replace("\"duration\":\"2.5\"", "\"duration\":\"NaN\""))
                .is_err()
        );
    }
    #[test]
    fn process_output_is_bounded_without_stopping_pipe_drain() {
        let input = vec![7_u8; MAX_PROCESS_OUTPUT_BYTES + 1];
        let output = read_pipe(std::io::Cursor::new(input)).unwrap();
        assert_eq!(output.len(), MAX_PROCESS_OUTPUT_BYTES);
    }
    #[test]
    fn plan_uses_asset_timebase_and_requires_concat_compatibility() {
        let root = temp_dir();
        let doc = document(&root, Rational::new(1, 1_000).unwrap(), "h264");
        let plan = build_render_plan(
            &doc,
            &root,
            root.join("final.mp4"),
            &ExecutableConfig::new(root.join("ffmpeg.exe"), root.join("ffprobe.exe")),
        )
        .unwrap();
        let RenderStep::TrimClip(step) = &plan.steps[0] else {
            panic!("missing trim")
        };
        assert_eq!(step.source_start_seconds, "0.000000");
        fs::remove_dir_all(root).unwrap();
        let root = temp_dir();
        let doc = document(&root, Rational::new(30, 1).unwrap(), "vp9");
        assert!(matches!(
            build_render_plan(
                &doc,
                &root,
                root.join("final.mp4"),
                &ExecutableConfig::new(root.join("ffmpeg.exe"), root.join("ffprobe.exe"))
            ),
            Err(MediaError::Unsupported(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn export_profiles_build_fixed_layered_filter_plans() {
        let root = temp_dir();
        let doc = document(&root, Rational::new(30, 1).unwrap(), "h264");
        let plan = build_render_plan_with_profile(
            &doc,
            &root,
            root.join("youtube.mp4"),
            &ExecutableConfig::new(root.join("ffmpeg.exe"), root.join("ffprobe.exe")),
            ExportProfile::Youtube,
        )
        .unwrap();
        assert_eq!(plan.expected_output.width, 1920);
        assert_eq!(plan.expected_output.height, 1080);
        assert!(plan.step.filter_complex.contains("overlay="));
        assert!(plan.step.filter_complex.contains("amix="));
        assert!(plan.step.argv.windows(2).any(|pair| pair == ["-f", "mp4"]));
        assert!(plan
            .step
            .argv
            .windows(2)
            .any(|pair| pair == ["-progress", "pipe:1"]));
        assert_eq!(
            plan.step.argv.last(),
            Some(&plan.step.output.to_string_lossy().into_owned())
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn export_plan_connects_audio_layers_with_sidechain_ducking() {
        let root = temp_dir();
        let mut doc = document(&root, Rational::new(30, 1).unwrap(), "h264");
        fs::write(root.join("media/music.wav"), [3]).unwrap();
        let audio_probe = {
            let mut probe = summary(Rational::new(30, 1).unwrap(), "aac");
            probe.video = None;
            probe
        };
        doc.assets.push(Asset {
            id: editor_domain::AssetId::new("music-asset").unwrap(),
            relative_path: editor_domain::RelativePath::new("media/music.wav").unwrap(),
            kind: AssetKind::Audio,
            fingerprint: Fingerprint {
                size_bytes: 1,
                modified_time: "test".into(),
                sha256: None,
            },
            probe: Some(audio_probe),
            status: AssetStatus::Available,
        });
        let mut voice = Track::new(
            editor_domain::TrackId::new("voice").unwrap(),
            TrackKind::Audio,
            "Voice",
        )
        .unwrap();
        voice.clips.push(Clip {
            id: ClipId::new("voice-clip").unwrap(),
            asset_id: editor_domain::AssetId::new("asset-a").unwrap(),
            timeline_start: 0,
            timeline_duration: 30,
            source_start: 0,
            source_duration: 30,
            speed: Rational::new(1, 1).unwrap(),
            opacity: 1.0,
            transform: Transform::default(),
            effects: Vec::new(),
            keyframes: Vec::new(),
            text_overlay: None,
        });
        let mut music = Track::new(
            editor_domain::TrackId::new("music").unwrap(),
            TrackKind::Audio,
            "Music",
        )
        .unwrap();
        music.ducking = Some(editor_domain::DuckingConfig {
            source_track_id: editor_domain::TrackId::new("voice").unwrap(),
            threshold_db: -24.0,
            ratio: 4.0,
            attack_ms: 20.0,
            release_ms: 250.0,
        });
        music.clips.push(Clip {
            id: ClipId::new("music-clip").unwrap(),
            asset_id: editor_domain::AssetId::new("music-asset").unwrap(),
            timeline_start: 0,
            timeline_duration: 30,
            source_start: 0,
            source_duration: 30,
            speed: Rational::new(1, 1).unwrap(),
            opacity: 1.0,
            transform: Transform::default(),
            effects: Vec::new(),
            keyframes: Vec::new(),
            text_overlay: None,
        });
        doc.sequence.tracks.push(voice);
        doc.sequence.tracks.push(music);
        let plan = build_render_plan_with_profile(
            &doc,
            &root,
            root.join("tiktok.mp4"),
            &ExecutableConfig::new(root.join("ffmpeg.exe"), root.join("ffprobe.exe")),
            ExportProfile::Tiktok,
        )
        .unwrap();
        assert_eq!(plan.expected_output.width, 1080);
        assert_eq!(plan.expected_output.height, 1920);
        assert!(plan.step.filter_complex.contains("sidechaincompress="));
        assert!(plan.step.filter_complex.contains("asplit="));
        assert_eq!(
            plan.expected_output.stream_kinds,
            vec![StreamKind::Video, StreamKind::Audio]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn layered_plan_maps_effects_transforms_overlays_and_text_to_ffmpeg_filters() {
        let root = temp_dir();
        let mut doc = document(&root, Rational::new(30, 1).unwrap(), "h264");
        fs::create_dir_all(root.join("looks")).unwrap();
        fs::write(
            root.join("looks/warm.cube"),
            "TITLE \"warm\"\nLUT_3D_SIZE 2\n",
        )
        .unwrap();

        let clip = &mut doc.sequence.tracks[0].clips[0];
        clip.opacity = 0.8;
        clip.transform = Transform {
            position_x: 0.2,
            position_y: -0.1,
            scale_x: 1.1,
            scale_y: 0.9,
            rotation_degrees: 5.0,
            anchor_x: 0.5,
            anchor_y: 0.5,
        };
        clip.effects = vec![
            Effect::Exposure { value: 0.4 },
            Effect::Gamma { value: 1.1 },
            Effect::Temperature { kelvin: 6_500.0 },
            Effect::Tint { value: 0.1 },
            Effect::ColorBalance {
                shadows: RgbDelta {
                    red: 0.1,
                    green: 0.0,
                    blue: -0.1,
                },
                midtones: RgbDelta {
                    red: 0.0,
                    green: 0.1,
                    blue: 0.0,
                },
                highlights: RgbDelta {
                    red: 0.1,
                    green: 0.1,
                    blue: 0.0,
                },
            },
            Effect::Blur { radius: 2.0 },
            Effect::Sharpen { amount: 0.5 },
            Effect::Vignette { amount: 0.4 },
            Effect::Duotone {
                shadows: RgbColor {
                    red: 10,
                    green: 20,
                    blue: 40,
                },
                highlights: RgbColor {
                    red: 240,
                    green: 220,
                    blue: 180,
                },
            },
            Effect::Lut {
                relative_path: RelativePath::new("looks/warm.cube").unwrap(),
            },
        ];

        let mut overlay = Track::new(
            TrackId::new("overlay").unwrap(),
            TrackKind::Overlay,
            "Overlay",
        )
        .unwrap();
        let mut overlay_clip = doc.sequence.tracks[0].clips[1].clone();
        overlay_clip.id = editor_domain::ClipId::new("overlay-clip").unwrap();
        overlay_clip.timeline_start = 15;
        overlay_clip.opacity = 0.65;
        overlay.clips.push(overlay_clip);
        doc.sequence.tracks.push(overlay);

        let text_asset_id = AssetId::new("title-asset").unwrap();
        doc.assets.push(Asset {
            id: text_asset_id.clone(),
            relative_path: RelativePath::new("generated/title.title").unwrap(),
            kind: AssetKind::Text,
            fingerprint: Fingerprint {
                size_bytes: 11,
                modified_time: "generated".into(),
                sha256: None,
            },
            probe: None,
            status: AssetStatus::Available,
        });
        let mut text = Track::new(TrackId::new("text").unwrap(), TrackKind::Text, "Text").unwrap();
        text.clips.push(Clip {
            id: editor_domain::ClipId::new("title-clip").unwrap(),
            asset_id: text_asset_id,
            timeline_start: 15,
            timeline_duration: 30,
            source_start: 0,
            source_duration: 30,
            speed: Rational::new(1, 1).unwrap(),
            opacity: 1.0,
            transform: Transform::default(),
            effects: Vec::new(),
            keyframes: Vec::new(),
            text_overlay: Some(TextOverlay {
                text: "Hello: editor".into(),
                font_size: 44.0,
                color: "#FFCC00".into(),
                position_x: 0.0,
                position_y: 0.75,
            }),
        });
        doc.sequence.tracks.push(text);

        let plan = build_render_plan_with_profile(
            &doc,
            &root,
            root.join("effects.mp4"),
            &ExecutableConfig::new(root.join("ffmpeg.exe"), root.join("ffprobe.exe")),
            ExportProfile::Instagram,
        )
        .unwrap();
        let graph = &plan.step.filter_complex;
        for expected in [
            "exposure=",
            "gamma=",
            "colortemperature=",
            "colorbalance=",
            "gblur=",
            "unsharp=",
            "vignette=",
            "lutrgb=",
            "lut3d=",
            "colorchannelmixer=aa=",
            "drawtext=",
        ] {
            assert!(graph.contains(expected), "missing {expected} in {graph}");
        }
        assert!(graph.contains("overlay=x='"));
        assert!(graph.contains("scale=iw*1.100000:ih*0.900000"));
        assert_eq!(plan.expected_output.width, 1080);
        assert_eq!(plan.expected_output.height, 1920);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn export_profile_serialization_is_stable() {
        assert_eq!(
            ExportProfile::parse("youtube").unwrap(),
            ExportProfile::Youtube
        );
        assert_eq!(
            serde_json::to_string(&ExportProfile::Instagram).unwrap(),
            "\"instagram\""
        );
        assert_eq!(ExportProfile::default(), ExportProfile::Baseline);
    }

    #[test]
    fn cleanup_removes_all_plan_temps() {
        let root = temp_dir();
        fs::create_dir_all(&root).unwrap();
        let output = root.join("out.mp4");
        let temp = root.join(".out.part");
        fs::write(&temp, [1]).unwrap();
        let plan = RenderPlan {
            steps: vec![RenderStep::ConcatMux(ConcatMuxStep {
                concat_list: root.join("list"),
                inputs: vec![],
                output: temp.clone(),
                stream_map: vec![],
                argv: vec![],
            })],
            concat_list: ConcatList {
                path: root.join("list"),
                entries: vec![],
            },
            output,
            expected_output: ExpectedOutput {
                stream_kinds: vec![StreamKind::Video],
                duration_seconds: 1.0,
                duration_tolerance_seconds: 0.1,
            },
            binary_contract: BinaryContract::unavailable("test"),
        };
        cleanup_plan_temps(&plan);
        assert!(!temp.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
