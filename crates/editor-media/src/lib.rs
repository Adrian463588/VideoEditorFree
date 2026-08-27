//! Fixed-argv FFmpeg/ffprobe media boundary.
//!
//! This crate owns probe parsing, deterministic plan construction, process
//! cancellation, output verification, and atomic finalization. It never uses
//! `PATH`, shell command strings, or caller-provided output evidence.

use editor_domain::{
    AssetKind, AssetStatus, ClipId, DomainError, ProjectDocument, Rational, TrackKind,
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
pub enum RenderStep {
    TrimClip(TrimClipStep),
    ConcatMux(ConcatMuxStep),
}
impl RenderStep {
    pub fn executable<'a>(&self, config: &'a ExecutableConfig) -> &'a Path {
        &config.ffmpeg
    }
    pub fn argv(&self) -> &[String] {
        match self {
            Self::TrimClip(s) => &s.argv,
            Self::ConcatMux(s) => &s.argv,
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
        let output = Command::new(executable)
            .args(argv)
            .stdin(Stdio::null())
            .output()
            .map_err(MediaError::Io)?;
        Ok(ProcessOutput {
            status_code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
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
        let concat = match plan.steps.last() {
            Some(RenderStep::ConcatMux(step)) => step,
            _ => {
                return Err(MediaError::InvalidPlan(
                    "render plan must end with concat mux".into(),
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
        write_concat_list(list_path, &plan.concat_list.entries)?;
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
            &concat.output,
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
    let stdout = child.stdout.take().ok_or_else(|| {
        MediaError::InvalidConfiguration("FFmpeg stdout pipe was not created".into())
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        MediaError::InvalidConfiguration("FFmpeg stderr pipe was not created".into())
    })?;
    let stdout_thread = thread::spawn(move || read_pipe(stdout));
    let stderr_thread = thread::spawn(move || read_pipe(stderr));
    let status = wait_cancellable(&mut child, cancelled)?;
    let stdout = stdout_thread
        .join()
        .map_err(|_| MediaError::ProcessFailed {
            code: status.code(),
            stderr: "FFmpeg stdout reader failed".into(),
        })??;
    let stderr = stderr_thread
        .join()
        .map_err(|_| MediaError::ProcessFailed {
            code: status.code(),
            stderr: "FFmpeg stderr reader failed".into(),
        })??;
    Ok(ProcessOutput {
        status_code: status.code(),
        stdout,
        stderr,
    })
}
fn read_pipe(mut pipe: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
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
        if let Some(status) = child.try_wait().map_err(MediaError::Io)? {
            return Ok(status);
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
    expected.matches_probe(&probe)?;
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
        Asset, AudioStream, Clip, Fingerprint, ProbeSummary, ProjectId, Track, Transform,
        VideoStream,
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
