//! Typed, local-only AI boundary.
//!
//! This crate owns contracts and validation only. It does not load models,
//! infer transcripts, execute commands, access paths, or mutate projects.

use editor_domain::{
    AssetId, ClipId, DomainError, ProjectDocument, ProjectId, TickRange, TimelineOperation, TrackId,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::BTreeSet,
    error::Error,
    fmt,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

pub const MAX_SERIALIZED_PROMPT_CONTEXT_BYTES: usize = 64 * 1024;
pub const MAX_PROVIDER_INSTRUCTION_BYTES: usize = 8 * 1024;
pub const MAX_CONTEXT_CLIPS: usize = 256;
pub const MAX_TRANSCRIPT_CUES: usize = 4_096;
pub const MAX_PLAN_OPERATIONS: usize = 256;
pub const MAX_PLAN_WARNINGS: usize = 64;
pub const MAX_SERIALIZED_PLAN_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    SerializedPromptContextBytes,
    ProviderInstructionBytes,
    Clips,
    Cues,
    Operations,
    Warnings,
    SerializedPlanBytes,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceLimitExceeded {
    pub resource: ResourceKind,
    pub limit: usize,
    pub actual: usize,
}

impl ResourceLimitExceeded {
    fn new(resource: ResourceKind, limit: usize, actual: usize) -> Self {
        Self {
            resource,
            limit,
            actual,
        }
    }
}

impl fmt::Display for ResourceLimitExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} exceeds limit {} (actual {})",
            self.resource, self.limit, self.actual
        )
    }
}

impl fmt::Display for ResourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::SerializedPromptContextBytes => "serialized prompt/context bytes",
            Self::ProviderInstructionBytes => "provider instruction bytes",
            Self::Clips => "clips",
            Self::Cues => "transcript cues",
            Self::Operations => "plan operations",
            Self::Warnings => "plan warnings",
            Self::SerializedPlanBytes => "serialized plan bytes",
        };
        f.write_str(name)
    }
}

enum SerializationSizeError {
    LimitExceeded,
    Serialization(String),
}

struct SizeLimitedWriter {
    size: usize,
    limit: usize,
    exceeded: bool,
}

impl SizeLimitedWriter {
    fn new(limit: usize) -> Self {
        Self {
            size: 0,
            limit,
            exceeded: false,
        }
    }
}

impl Write for SizeLimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.size) {
            self.exceeded = true;
            self.size = self.limit.saturating_add(1);
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "serialized value exceeds limit",
            ));
        }
        self.size += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialized_size<T: Serialize>(value: &T, limit: usize) -> Result<usize, SerializationSizeError> {
    let mut writer = SizeLimitedWriter::new(limit);
    match serde_json::to_writer(&mut writer, value) {
        Ok(()) => Ok(writer.size),
        Err(_error) if writer.exceeded => Err(SerializationSizeError::LimitExceeded),
        Err(error) => Err(SerializationSizeError::Serialization(error.to_string())),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum ModelStatus {
    Missing,
    Downloading,
    Verifying,
    Ready,
    Failed,
    Incompatible,
    #[serde(rename = "UNAVAILABLE")]
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    SpeechToText,
    LanguageModel,
    TextToSpeech,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRequirements {
    pub ram_mb: u64,
    pub vram_mb: u64,
    pub architecture: String,
}

impl ResourceRequirements {
    pub fn validate(&self) -> Result<(), ReadinessError> {
        if self.ram_mb == 0 {
            return Err(ReadinessError::InvalidManifest(
                "ram_mb must be positive".into(),
            ));
        }
        if self.architecture.trim().is_empty() {
            return Err(ReadinessError::InvalidManifest(
                "architecture must not be empty".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelManifest {
    pub id: String,
    pub version: String,
    pub format: String,
    pub runtime: String,
    pub artifact: String,
    pub source: String,
    pub license: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub capabilities: Vec<ModelCapability>,
    pub requirements: ResourceRequirements,
}

impl ModelManifest {
    pub fn validate(&self) -> Result<(), ReadinessError> {
        for (name, value) in [
            ("id", self.id.as_str()),
            ("version", self.version.as_str()),
            ("format", self.format.as_str()),
            ("runtime", self.runtime.as_str()),
            ("artifact", self.artifact.as_str()),
            ("source", self.source.as_str()),
            ("license", self.license.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ReadinessError::InvalidManifest(format!(
                    "{name} must not be empty"
                )));
            }
        }
        validate_relative_model_path(&self.artifact)?;
        if self.size_bytes == 0 {
            return Err(ReadinessError::InvalidManifest(
                "size_bytes must be positive".into(),
            ));
        }
        validate_sha256(&self.sha256).map_err(ReadinessError::InvalidManifest)?;
        if self.capabilities.is_empty() {
            return Err(ReadinessError::InvalidManifest(
                "capabilities must not be empty".into(),
            ));
        }
        let mut seen = BTreeSet::new();
        for capability in &self.capabilities {
            if !seen.insert(format!("{capability:?}")) {
                return Err(ReadinessError::InvalidManifest(
                    "capabilities must not contain duplicates".into(),
                ));
            }
        }
        self.requirements.validate()
    }
}

pub fn parse_model_manifest_json(input: &str) -> Result<ModelManifest, ReadinessError> {
    let manifest: ModelManifest = serde_json::from_str(input)
        .map_err(|error| ReadinessError::InvalidManifest(error.to_string()))?;
    manifest.validate()?;
    Ok(manifest)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactMetadata {
    #[serde(skip)]
    pub path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
}

impl ArtifactMetadata {
    pub fn validate(&self) -> Result<(), ReadinessError> {
        validate_artifact_path(&self.path)?;
        if self.size_bytes == 0 {
            return Err(ReadinessError::InvalidArtifact(
                "size_bytes must be positive".into(),
            ));
        }
        validate_sha256(&self.sha256).map_err(ReadinessError::InvalidArtifact)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEnvironment {
    pub runtime: String,
    pub architecture: String,
    pub available_ram_mb: u64,
    pub available_vram_mb: u64,
    pub runtime_available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelReadiness {
    pub status: ModelStatus,
    pub reason: Option<String>,
}

pub fn validate_model_readiness(
    manifest: &ModelManifest,
    artifact: Option<&ArtifactMetadata>,
    environment: &RuntimeEnvironment,
) -> Result<(), ReadinessError> {
    manifest.validate()?;
    let artifact = artifact.ok_or(ReadinessError::MissingArtifact)?;
    artifact.validate()?;
    if !environment.runtime_available {
        return Err(ReadinessError::RuntimeUnavailable);
    }
    if environment.runtime != manifest.runtime
        || environment.architecture != manifest.requirements.architecture
    {
        return Err(ReadinessError::IncompatibleRuntime);
    }
    if environment.available_ram_mb < manifest.requirements.ram_mb {
        return Err(ReadinessError::InsufficientRam);
    }
    if environment.available_vram_mb < manifest.requirements.vram_mb {
        return Err(ReadinessError::InsufficientVram);
    }
    if artifact.size_bytes != manifest.size_bytes {
        return Err(ReadinessError::SizeMismatch {
            expected: manifest.size_bytes,
            actual: artifact.size_bytes,
        });
    }
    let (actual_size, actual_sha256) = read_artifact_digest(&artifact.path)?;
    if actual_size != artifact.size_bytes {
        return Err(ReadinessError::SizeMismatch {
            expected: artifact.size_bytes,
            actual: actual_size,
        });
    }
    if !actual_sha256.eq_ignore_ascii_case(&artifact.sha256)
        || !actual_sha256.eq_ignore_ascii_case(&manifest.sha256)
    {
        return Err(ReadinessError::ChecksumMismatch);
    }
    Ok(())
}

pub fn evaluate_model_readiness(
    manifest: &ModelManifest,
    artifact: Option<&ArtifactMetadata>,
    environment: &RuntimeEnvironment,
) -> ModelReadiness {
    match validate_model_readiness(manifest, artifact, environment) {
        Ok(()) => ModelReadiness {
            status: ModelStatus::Ready,
            reason: None,
        },
        Err(error) => ModelReadiness {
            status: error.status(),
            reason: Some(error.to_string()),
        },
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadinessError {
    InvalidManifest(String),
    InvalidArtifact(String),
    InvalidArtifactPath(String),
    ArtifactIo(String),
    MissingArtifact,
    RuntimeUnavailable,
    IncompatibleRuntime,
    InsufficientRam,
    InsufficientVram,
    SizeMismatch { expected: u64, actual: u64 },
    ChecksumMismatch,
}

impl ReadinessError {
    fn status(&self) -> ModelStatus {
        match self {
            Self::MissingArtifact => ModelStatus::Missing,
            Self::RuntimeUnavailable => ModelStatus::Unavailable,
            Self::IncompatibleRuntime | Self::InsufficientRam | Self::InsufficientVram => {
                ModelStatus::Incompatible
            }
            Self::InvalidManifest(_)
            | Self::InvalidArtifact(_)
            | Self::InvalidArtifactPath(_)
            | Self::ArtifactIo(_)
            | Self::SizeMismatch { .. }
            | Self::ChecksumMismatch => ModelStatus::Failed,
        }
    }
}

impl fmt::Display for ReadinessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidManifest(reason) => write!(f, "invalid model manifest: {reason}"),
            Self::InvalidArtifact(reason) => write!(f, "invalid artifact metadata: {reason}"),
            Self::InvalidArtifactPath(reason) => write!(f, "invalid artifact path: {reason}"),
            Self::ArtifactIo(reason) => write!(f, "unable to read model artifact: {reason}"),
            Self::MissingArtifact => f.write_str("model artifact is missing"),
            Self::RuntimeUnavailable => f.write_str("model runtime is unavailable"),
            Self::IncompatibleRuntime => {
                f.write_str("model runtime or architecture is incompatible")
            }
            Self::InsufficientRam => f.write_str("model requires more RAM"),
            Self::InsufficientVram => f.write_str("model requires more VRAM"),
            Self::SizeMismatch { expected, actual } => {
                write!(
                    f,
                    "model size mismatch: expected {expected}, actual {actual}"
                )
            }
            Self::ChecksumMismatch => f.write_str("model SHA-256 mismatch"),
        }
    }
}

impl Error for ReadinessError {}

fn validate_sha256(value: &str) -> Result<(), String> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("SHA-256 must be 64 hexadecimal characters".into())
    }
}

fn validate_relative_model_path(value: &str) -> Result<(), ReadinessError> {
    let path = Path::new(value);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || value.starts_with('/')
        || value.starts_with('\\')
        || value.as_bytes().get(1) == Some(&b':')
        || path.components().any(|component| {
            matches!(
                component,
                Component::Prefix(_)
                    | Component::RootDir
                    | Component::CurDir
                    | Component::ParentDir
            )
        })
    {
        return Err(ReadinessError::InvalidManifest(
            "artifact must be a relative path without traversal".into(),
        ));
    }
    Ok(())
}

fn validate_artifact_path(path: &Path) -> Result<(), ReadinessError> {
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return Err(ReadinessError::InvalidArtifactPath(
            "path must be an absolute local path".into(),
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(ReadinessError::InvalidArtifactPath(
            "path must not contain traversal components".into(),
        ));
    }
    if path.file_name().is_none() {
        return Err(ReadinessError::InvalidArtifactPath(
            "path must identify a file".into(),
        ));
    }
    Ok(())
}

fn read_artifact_digest(path: &Path) -> Result<(u64, String), ReadinessError> {
    validate_artifact_path(path)?;
    let link_metadata = fs::symlink_metadata(path)
        .map_err(|error| ReadinessError::ArtifactIo(error.to_string()))?;
    if link_metadata.file_type().is_symlink() {
        return Err(ReadinessError::InvalidArtifactPath(
            "symbolic links are not accepted".into(),
        ));
    }
    let canonical_path =
        fs::canonicalize(path).map_err(|error| ReadinessError::ArtifactIo(error.to_string()))?;
    let metadata = fs::metadata(&canonical_path)
        .map_err(|error| ReadinessError::ArtifactIo(error.to_string()))?;
    if !metadata.is_file() {
        return Err(ReadinessError::InvalidArtifactPath(
            "path must identify a regular file".into(),
        ));
    }
    let mut file = File::open(&canonical_path)
        .map_err(|error| ReadinessError::ArtifactIo(error.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut size = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| ReadinessError::ArtifactIo(error.to_string()))?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(read as u64)
            .ok_or_else(|| ReadinessError::ArtifactIo("artifact is too large".into()))?;
        hasher.update(&buffer[..read])?;
    }
    Ok((size, hasher.finish()))
}

struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffer_len: usize,
    bit_len: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: [0; 64],
            buffer_len: 0,
            bit_len: 0,
        }
    }

    fn update(&mut self, mut input: &[u8]) -> Result<(), ReadinessError> {
        let input_bits = (input.len() as u64)
            .checked_mul(8)
            .ok_or_else(|| ReadinessError::ArtifactIo("artifact is too large".into()))?;
        self.bit_len = self
            .bit_len
            .checked_add(input_bits)
            .ok_or_else(|| ReadinessError::ArtifactIo("artifact is too large".into()))?;
        while !input.is_empty() {
            let copy_len = (self.buffer.len() - self.buffer_len).min(input.len());
            self.buffer[self.buffer_len..self.buffer_len + copy_len]
                .copy_from_slice(&input[..copy_len]);
            self.buffer_len += copy_len;
            input = &input[copy_len..];
            if self.buffer_len == self.buffer.len() {
                let block = self.buffer;
                self.process_block(&block);
                self.buffer_len = 0;
            }
        }
        Ok(())
    }

    fn finish(mut self) -> String {
        let bit_len = self.bit_len;
        self.buffer[self.buffer_len] = 0x80;
        self.buffer_len += 1;
        if self.buffer_len > 56 {
            self.buffer[self.buffer_len..].fill(0);
            let block = self.buffer;
            self.process_block(&block);
            self.buffer_len = 0;
        }
        self.buffer[self.buffer_len..56].fill(0);
        self.buffer[56..].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buffer;
        self.process_block(&block);

        let mut output = String::with_capacity(64);
        for word in self.state {
            use fmt::Write as _;
            write!(&mut output, "{word:08x}").expect("writing to String cannot fail");
        }
        output
    }

    fn process_block(&mut self, block: &[u8; 64]) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut words = [0_u32; 64];
        let (chunks, _) = block.as_chunks::<4>();
        for (index, chunk) in chunks.iter().take(16).enumerate() {
            words[index] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }

        let mut working = self.state;
        for index in 0..64 {
            let s1 = working[4].rotate_right(6)
                ^ working[4].rotate_right(11)
                ^ working[4].rotate_right(25);
            let choice = (working[4] & working[5]) ^ ((!working[4]) & working[6]);
            let temp1 = working[7]
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let s0 = working[0].rotate_right(2)
                ^ working[0].rotate_right(13)
                ^ working[0].rotate_right(22);
            let majority =
                (working[0] & working[1]) ^ (working[0] & working[2]) ^ (working[1] & working[2]);
            let temp2 = s0.wrapping_add(majority);
            working[7] = working[6];
            working[6] = working[5];
            working[5] = working[4];
            working[4] = working[3].wrapping_add(temp1);
            working[3] = working[2];
            working[2] = working[1];
            working[1] = working[0];
            working[0] = temp1.wrapping_add(temp2);
        }
        for (state, value) in self.state.iter_mut().zip(working) {
            *state = state.wrapping_add(value);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProvenance {
    pub provider: String,
    pub model_id: String,
    pub model_version: String,
}

impl ModelProvenance {
    fn validate(&self) -> Result<(), ProviderError> {
        if self.provider.trim().is_empty()
            || self.model_id.trim().is_empty()
            || self.model_version.trim().is_empty()
        {
            return Err(ProviderError::InvalidRequest(
                "model provenance is incomplete".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cue {
    pub range: TickRange,
    pub text: String,
    pub confidence: Option<f32>,
    pub speaker: Option<String>,
}

impl Cue {
    pub fn validate(&self) -> Result<(), TranscriptError> {
        self.range
            .validate()
            .map_err(TranscriptError::InvalidDomain)?;
        if self.text.trim().is_empty() {
            return Err(TranscriptError::EmptyCueText);
        }
        if self
            .confidence
            .is_some_and(|confidence| !confidence.is_finite() || !(0.0..=1.0).contains(&confidence))
        {
            return Err(TranscriptError::InvalidConfidence);
        }
        if self
            .speaker
            .as_ref()
            .is_some_and(|speaker| speaker.trim().is_empty())
        {
            return Err(TranscriptError::InvalidSpeaker);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transcript {
    pub provenance: ModelProvenance,
    pub cues: Vec<Cue>,
}

impl Transcript {
    pub fn validate(&self) -> Result<(), TranscriptError> {
        self.provenance
            .validate()
            .map_err(TranscriptError::InvalidProvider)?;
        if self.cues.is_empty() {
            return Err(TranscriptError::EmptyTranscript);
        }
        if self.cues.len() > MAX_TRANSCRIPT_CUES {
            return Err(TranscriptError::ResourceLimit(ResourceLimitExceeded::new(
                ResourceKind::Cues,
                MAX_TRANSCRIPT_CUES,
                self.cues.len(),
            )));
        }
        for cue in &self.cues {
            cue.validate()?;
        }
        Ok(())
    }
}

pub fn parse_transcript_json(input: &str) -> Result<Transcript, TranscriptError> {
    let value: Value =
        serde_json::from_str(input).map_err(|error| TranscriptError::Json(error.to_string()))?;
    validate_transcript_shape(&value)?;
    let transcript: Transcript =
        serde_json::from_value(value).map_err(|error| TranscriptError::Json(error.to_string()))?;
    transcript.validate()?;
    Ok(transcript)
}

/// Parse the JSON emitted by the official whisper.cpp CLI (`-oj`).
///
/// Whisper reports offsets in milliseconds. The AI contract deliberately keeps
/// those offsets as integer ticks with a documented 1 kHz timebase; the host
/// converts them to the sequence timebase before storing editable captions.
pub fn parse_whisper_json(
    input: &str,
    language: &str,
    provenance: ModelProvenance,
) -> Result<Transcript, TranscriptError> {
    if language.trim().is_empty() {
        return Err(TranscriptError::Json("language must not be empty".into()));
    }
    let root: Value =
        serde_json::from_str(input).map_err(|error| TranscriptError::Json(error.to_string()))?;
    let segments = root
        .get("transcription")
        .and_then(Value::as_array)
        .ok_or_else(|| TranscriptError::Json("whisper JSON missing transcription array".into()))?;
    if segments.len() > MAX_TRANSCRIPT_CUES {
        return Err(TranscriptError::ResourceLimit(ResourceLimitExceeded::new(
            ResourceKind::Cues,
            MAX_TRANSCRIPT_CUES,
            segments.len(),
        )));
    }
    let mut cues = Vec::with_capacity(segments.len());
    for segment in segments {
        let object = segment
            .as_object()
            .ok_or_else(|| TranscriptError::Json("whisper segment must be an object".into()))?;
        let text = object
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or(TranscriptError::EmptyCueText)?
            .to_owned();
        let offsets = object
            .get("offsets")
            .and_then(Value::as_object)
            .ok_or_else(|| TranscriptError::Json("whisper segment offsets are required".into()))?;
        let start = parse_whisper_millis(offsets.get("from"))?;
        let end = parse_whisper_millis(offsets.get("to"))?;
        cues.push(Cue {
            range: TickRange::new(start, end).map_err(TranscriptError::InvalidDomain)?,
            text,
            confidence: None,
            speaker: None,
        });
    }
    let transcript = Transcript { provenance, cues };
    transcript.validate()?;
    Ok(transcript)
}

fn parse_whisper_millis(value: Option<&Value>) -> Result<i64, TranscriptError> {
    let value = value.ok_or_else(|| TranscriptError::Json("whisper offset is required".into()))?;
    let millis = value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_str()?.parse::<i64>().ok())
        .ok_or_else(|| TranscriptError::Json("whisper offset must be an integer".into()))?;
    if millis < 0 {
        return Err(TranscriptError::Json(
            "whisper offset must not be negative".into(),
        ));
    }
    Ok(millis)
}

fn validate_transcript_shape(value: &Value) -> Result<(), TranscriptError> {
    let object = value
        .as_object()
        .ok_or_else(|| TranscriptError::Json("transcript root must be an object".into()))?;
    reject_json_keys(object, &["provenance", "cues"])?;
    let provenance = object
        .get("provenance")
        .and_then(Value::as_object)
        .ok_or_else(|| TranscriptError::Json("provenance must be an object".into()))?;
    reject_json_keys(provenance, &["provider", "model_id", "model_version"])?;
    let cues = object
        .get("cues")
        .and_then(Value::as_array)
        .ok_or_else(|| TranscriptError::Json("cues must be an array".into()))?;
    if cues.len() > MAX_TRANSCRIPT_CUES {
        return Err(TranscriptError::ResourceLimit(ResourceLimitExceeded::new(
            ResourceKind::Cues,
            MAX_TRANSCRIPT_CUES,
            cues.len(),
        )));
    }
    for cue in cues {
        let cue = cue
            .as_object()
            .ok_or_else(|| TranscriptError::Json("cue must be an object".into()))?;
        reject_json_keys(cue, &["range", "text", "confidence", "speaker"])?;
        let range = cue
            .get("range")
            .and_then(Value::as_object)
            .ok_or_else(|| TranscriptError::Json("cue range must be an object".into()))?;
        reject_json_keys(range, &["start", "end"])?;
    }
    Ok(())
}

fn reject_json_keys(object: &Map<String, Value>, allowed: &[&str]) -> Result<(), TranscriptError> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(TranscriptError::Json(format!(
            "unknown transcript field: {key}"
        )));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptError {
    Json(String),
    InvalidDomain(DomainError),
    InvalidProvider(ProviderError),
    ResourceLimit(ResourceLimitExceeded),
    EmptyTranscript,
    EmptyCueText,
    InvalidConfidence,
    InvalidSpeaker,
}

impl fmt::Display for TranscriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(f, "invalid transcript JSON: {error}"),
            Self::InvalidDomain(error) => error.fmt(f),
            Self::InvalidProvider(error) => error.fmt(f),
            Self::ResourceLimit(error) => error.fmt(f),
            Self::EmptyTranscript => f.write_str("transcript must contain at least one cue"),
            Self::EmptyCueText => f.write_str("cue text must not be empty"),
            Self::InvalidConfidence => f.write_str("cue confidence must be finite and in [0, 1]"),
            Self::InvalidSpeaker => f.write_str("speaker must not be empty"),
        }
    }
}

impl Error for TranscriptError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum PlanOperation {
    Trim {
        clip_id: ClipId,
        source_range: TickRange,
    },
    Split {
        clip_id: ClipId,
        at_timeline_tick: i64,
    },
    Delete {
        clip_id: ClipId,
    },
    RippleDelete {
        track_id: TrackId,
        clip_id: ClipId,
    },
    Reorder {
        track_id: TrackId,
        clip_id: ClipId,
        new_index: usize,
    },
}

impl PlanOperation {
    fn validate(&self) -> Result<(), PlanError> {
        match self {
            Self::Trim {
                clip_id,
                source_range,
            } => {
                clip_id.validate().map_err(PlanError::Domain)?;
                source_range.validate().map_err(PlanError::Domain)
            }
            Self::Split {
                clip_id,
                at_timeline_tick,
            } => {
                clip_id.validate().map_err(PlanError::Domain)?;
                if *at_timeline_tick < 0 {
                    return Err(PlanError::InvalidOperation(
                        "split tick must not be negative".into(),
                    ));
                }
                Ok(())
            }
            Self::Delete { clip_id } => clip_id.validate().map_err(PlanError::Domain),
            Self::RippleDelete { track_id, clip_id } => {
                track_id.validate().map_err(PlanError::Domain)?;
                clip_id.validate().map_err(PlanError::Domain)
            }
            Self::Reorder {
                track_id, clip_id, ..
            } => {
                track_id.validate().map_err(PlanError::Domain)?;
                clip_id.validate().map_err(PlanError::Domain)
            }
        }
    }

    fn domain_operation(&self) -> TimelineOperation {
        match self {
            Self::Trim {
                clip_id,
                source_range,
            } => TimelineOperation::TrimClip {
                clip_id: clip_id.clone(),
                source_start: source_range.start,
                source_end: source_range.end,
            },
            Self::Split {
                clip_id,
                at_timeline_tick,
            } => TimelineOperation::SplitClip {
                clip_id: clip_id.clone(),
                at_timeline_tick: *at_timeline_tick,
            },
            Self::Delete { clip_id } => TimelineOperation::DeleteClip {
                clip_id: clip_id.clone(),
            },
            Self::RippleDelete { track_id, clip_id } => TimelineOperation::RippleDelete {
                track_id: track_id.clone(),
                clip_id: clip_id.clone(),
            },
            Self::Reorder {
                track_id,
                clip_id,
                new_index,
            } => TimelineOperation::ReorderClip {
                track_id: track_id.clone(),
                clip_id: clip_id.clone(),
                new_index: *new_index,
            },
        }
    }

    fn clip_id(&self) -> &ClipId {
        match self {
            Self::Trim { clip_id, .. }
            | Self::Split { clip_id, .. }
            | Self::Delete { clip_id }
            | Self::RippleDelete { clip_id, .. }
            | Self::Reorder { clip_id, .. } => clip_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EditPlan {
    pub base_revision: u64,
    pub operations: Vec<PlanOperation>,
    pub warnings: Vec<String>,
    pub affected_clips: Vec<ClipId>,
    pub requires_confirmation: bool,
}

impl EditPlan {
    pub fn validate(&self) -> Result<(), PlanError> {
        self.validate_resource_limits()?;
        if self.operations.is_empty() {
            return Err(PlanError::EmptyOperations);
        }
        if !self.requires_confirmation {
            return Err(PlanError::ConfirmationRequired);
        }
        if self
            .warnings
            .iter()
            .any(|warning| warning.trim().is_empty())
        {
            return Err(PlanError::InvalidOperation(
                "warnings must not be empty".into(),
            ));
        }
        for clip_id in &self.affected_clips {
            clip_id.validate().map_err(PlanError::Domain)?;
        }
        let referenced: BTreeSet<String> = self
            .operations
            .iter()
            .map(|operation| operation.clip_id().to_string())
            .collect();
        let affected: BTreeSet<String> = self
            .affected_clips
            .iter()
            .map(ToString::to_string)
            .collect();
        if referenced != affected {
            return Err(PlanError::AffectedClipsMismatch);
        }
        for operation in &self.operations {
            operation.validate()?;
        }
        Ok(())
    }

    fn validate_resource_limits(&self) -> Result<(), PlanError> {
        if self.operations.len() > MAX_PLAN_OPERATIONS {
            return Err(PlanError::ResourceLimit(ResourceLimitExceeded::new(
                ResourceKind::Operations,
                MAX_PLAN_OPERATIONS,
                self.operations.len(),
            )));
        }
        if self.warnings.len() > MAX_PLAN_WARNINGS {
            return Err(PlanError::ResourceLimit(ResourceLimitExceeded::new(
                ResourceKind::Warnings,
                MAX_PLAN_WARNINGS,
                self.warnings.len(),
            )));
        }
        if self.affected_clips.len() > MAX_CONTEXT_CLIPS {
            return Err(PlanError::ResourceLimit(ResourceLimitExceeded::new(
                ResourceKind::Clips,
                MAX_CONTEXT_CLIPS,
                self.affected_clips.len(),
            )));
        }
        match serialized_size(self, MAX_SERIALIZED_PLAN_BYTES) {
            Ok(_) => Ok(()),
            Err(SerializationSizeError::LimitExceeded) => {
                Err(PlanError::ResourceLimit(ResourceLimitExceeded::new(
                    ResourceKind::SerializedPlanBytes,
                    MAX_SERIALIZED_PLAN_BYTES,
                    MAX_SERIALIZED_PLAN_BYTES.saturating_add(1),
                )))
            }
            Err(SerializationSizeError::Serialization(error)) => Err(PlanError::Json(error)),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedEditPlan {
    base_revision: u64,
    operations: Vec<TimelineOperation>,
}

impl ValidatedEditPlan {
    pub fn base_revision(&self) -> u64 {
        self.base_revision
    }
    pub fn operations(&self) -> &[TimelineOperation] {
        &self.operations
    }
}

pub fn validate_plan_for_apply(
    project: &ProjectDocument,
    plan: &EditPlan,
    user_confirmed: bool,
) -> Result<ValidatedEditPlan, PlanError> {
    plan.validate()?;
    if !user_confirmed {
        return Err(PlanError::ConfirmationRequired);
    }
    if plan.base_revision != project.revision {
        return Err(PlanError::RevisionMismatch {
            expected: plan.base_revision,
            actual: project.revision,
        });
    }
    let mut snapshot = project.clone();
    let mut revision = plan.base_revision;
    let mut operations = Vec::with_capacity(plan.operations.len());
    for operation in &plan.operations {
        let domain_operation = operation.domain_operation();
        let result = snapshot
            .apply(revision, domain_operation.clone())
            .map_err(PlanError::Domain)?;
        revision = result.revision;
        snapshot = result.document;
        operations.push(domain_operation);
    }
    Ok(ValidatedEditPlan {
        base_revision: plan.base_revision,
        operations,
    })
}

pub fn validate_plan(
    project: &ProjectDocument,
    plan: &EditPlan,
    user_confirmed: bool,
) -> Result<ValidatedEditPlan, PlanError> {
    validate_plan_for_apply(project, plan, user_confirmed)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanError {
    Json(String),
    Malformed(String),
    UnknownField(String),
    ForbiddenField(String),
    UnknownOperation(String),
    ForbiddenOperation(String),
    UnsupportedOperation(String),
    InvalidOperation(String),
    EmptyOperations,
    ResourceLimit(ResourceLimitExceeded),
    ConfirmationRequired,
    AffectedClipsMismatch,
    RevisionMismatch { expected: u64, actual: u64 },
    Domain(DomainError),
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(f, "invalid edit plan JSON: {error}"),
            Self::Malformed(reason) => write!(f, "malformed edit plan: {reason}"),
            Self::UnknownField(field) => write!(f, "unknown edit plan field: {field}"),
            Self::ForbiddenField(field) => write!(f, "forbidden edit plan field: {field}"),
            Self::UnknownOperation(operation) => write!(f, "unknown edit operation: {operation}"),
            Self::ForbiddenOperation(operation) => {
                write!(f, "forbidden edit operation: {operation}")
            }
            Self::UnsupportedOperation(operation) => {
                write!(f, "unsupported edit operation: {operation}")
            }
            Self::InvalidOperation(reason) => write!(f, "invalid edit operation: {reason}"),
            Self::EmptyOperations => f.write_str("edit plan must contain an operation"),
            Self::ResourceLimit(error) => error.fmt(f),
            Self::ConfirmationRequired => f.write_str("user confirmation is required"),
            Self::AffectedClipsMismatch => {
                f.write_str("affected_clips must exactly match operation clip IDs")
            }
            Self::RevisionMismatch { expected, actual } => {
                write!(
                    f,
                    "plan revision mismatch: expected {expected}, actual {actual}"
                )
            }
            Self::Domain(error) => error.fmt(f),
        }
    }
}

impl Error for PlanError {}

pub fn parse_edit_plan_json(input: &str) -> Result<EditPlan, PlanError> {
    if input.len() > MAX_SERIALIZED_PLAN_BYTES {
        return Err(PlanError::ResourceLimit(ResourceLimitExceeded::new(
            ResourceKind::SerializedPlanBytes,
            MAX_SERIALIZED_PLAN_BYTES,
            input.len(),
        )));
    }
    if input.to_ascii_lowercase().contains("edit:") {
        return Err(PlanError::ForbiddenField("EDIT: marker".into()));
    }
    let value: Value =
        serde_json::from_str(input).map_err(|error| PlanError::Json(error.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| PlanError::Malformed("root must be an object".into()))?;
    reject_forbidden_keys(object)?;
    reject_unknown_keys(
        object,
        &[
            "base_revision",
            "operations",
            "warnings",
            "affected_clips",
            "requires_confirmation",
        ],
    )?;
    let base_revision = required_u64(object, "base_revision")?;
    let operations_value = object
        .get("operations")
        .and_then(Value::as_array)
        .ok_or_else(|| PlanError::Malformed("operations must be an array".into()))?;
    let mut operations = Vec::with_capacity(operations_value.len());
    for operation in operations_value {
        operations.push(parse_operation(operation)?);
    }
    let warnings = required_string_array(object, "warnings")?;
    let affected_clips = required_string_array(object, "affected_clips")?
        .into_iter()
        .map(|value| ClipId::new(value).map_err(PlanError::Domain))
        .collect::<Result<Vec<_>, _>>()?;
    let requires_confirmation = object
        .get("requires_confirmation")
        .and_then(Value::as_bool)
        .ok_or_else(|| PlanError::Malformed("requires_confirmation must be boolean".into()))?;
    let plan = EditPlan {
        base_revision,
        operations,
        warnings,
        affected_clips,
        requires_confirmation,
    };
    plan.validate()?;
    Ok(plan)
}

fn parse_operation(value: &Value) -> Result<PlanOperation, PlanError> {
    let object = value
        .as_object()
        .ok_or_else(|| PlanError::Malformed("operation must be an object".into()))?;
    reject_forbidden_keys(object)?;
    let name = object
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| PlanError::Malformed("operation op must be a string".into()))?;
    match name {
        "trim" => {
            reject_unknown_keys(
                object,
                &[
                    "op",
                    "clip_id",
                    "source_range",
                    "source_start",
                    "source_end",
                ],
            )?;
            let clip_id = parse_id(object, "clip_id", ClipId::new)?;
            let source_range = if let Some(range) = object.get("source_range") {
                parse_tick_range(range)?
            } else {
                TickRange::new(
                    required_i64(object, "source_start")?,
                    required_i64(object, "source_end")?,
                )
                .map_err(PlanError::Domain)?
            };
            Ok(PlanOperation::Trim {
                clip_id,
                source_range,
            })
        }
        "split" => {
            reject_unknown_keys(object, &["op", "clip_id", "at_timeline_tick"])?;
            Ok(PlanOperation::Split {
                clip_id: parse_id(object, "clip_id", ClipId::new)?,
                at_timeline_tick: required_i64(object, "at_timeline_tick")?,
            })
        }
        "delete" => {
            reject_unknown_keys(object, &["op", "clip_id"])?;
            Ok(PlanOperation::Delete {
                clip_id: parse_id(object, "clip_id", ClipId::new)?,
            })
        }
        "ripple_delete" => {
            reject_unknown_keys(object, &["op", "track_id", "clip_id"])?;
            Ok(PlanOperation::RippleDelete {
                track_id: parse_id(object, "track_id", TrackId::new)?,
                clip_id: parse_id(object, "clip_id", ClipId::new)?,
            })
        }
        "reorder" => {
            reject_unknown_keys(object, &["op", "track_id", "clip_id", "new_index"])?;
            let index = required_u64(object, "new_index")?;
            let new_index = usize::try_from(index)
                .map_err(|_| PlanError::InvalidOperation("new_index exceeds usize".into()))?;
            Ok(PlanOperation::Reorder {
                track_id: parse_id(object, "track_id", TrackId::new)?,
                clip_id: parse_id(object, "clip_id", ClipId::new)?,
                new_index,
            })
        }
        "shell" | "execute" | "run_command" | "filtergraph" | "path" | "download" | "network" => {
            Err(PlanError::ForbiddenOperation(name.into()))
        }
        "" => Err(PlanError::Malformed(
            "operation name must not be empty".into(),
        )),
        other => Err(PlanError::UnknownOperation(other.into())),
    }
}

fn parse_id<T>(
    object: &Map<String, Value>,
    field: &str,
    constructor: fn(String) -> Result<T, DomainError>,
) -> Result<T, PlanError> {
    let value = object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| PlanError::Malformed(format!("{field} must be a string")))?;
    constructor(value.to_owned()).map_err(PlanError::Domain)
}

fn parse_tick_range(value: &Value) -> Result<TickRange, PlanError> {
    let object = value
        .as_object()
        .ok_or_else(|| PlanError::Malformed("source_range must be an object".into()))?;
    reject_unknown_keys(object, &["start", "end"])?;
    TickRange::new(required_i64(object, "start")?, required_i64(object, "end")?)
        .map_err(PlanError::Domain)
}

fn required_i64(object: &Map<String, Value>, field: &str) -> Result<i64, PlanError> {
    object
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| PlanError::Malformed(format!("{field} must be an integer")))
}

fn required_u64(object: &Map<String, Value>, field: &str) -> Result<u64, PlanError> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| PlanError::Malformed(format!("{field} must be a non-negative integer")))
}

fn required_string_array(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Vec<String>, PlanError> {
    object
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| PlanError::Malformed(format!("{field} must be an array")))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| PlanError::Malformed(format!("{field} must contain strings")))
        })
        .collect()
}

fn reject_unknown_keys(object: &Map<String, Value>, allowed: &[&str]) -> Result<(), PlanError> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(PlanError::UnknownField(key.clone()));
        }
    }
    Ok(())
}

fn reject_forbidden_keys(object: &Map<String, Value>) -> Result<(), PlanError> {
    for key in object.keys() {
        if matches!(
            key.as_str(),
            "path"
                | "paths"
                | "filtergraph"
                | "filtergraphs"
                | "shell"
                | "command"
                | "executable"
                | "url"
        ) {
            return Err(PlanError::ForbiddenField(key.clone()));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderError {
    NoModel { capability: ModelCapability },
    Unsupported { operation: String },
    InvalidRequest(String),
    ResourceLimit(ResourceLimitExceeded),
    InvalidPlan(PlanError),
    Failed(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoModel { capability } => write!(f, "no ready model for {capability:?}"),
            Self::Unsupported { operation } => write!(f, "unsupported AI operation: {operation}"),
            Self::InvalidRequest(reason) => write!(f, "invalid AI request: {reason}"),
            Self::ResourceLimit(error) => error.fmt(f),
            Self::InvalidPlan(error) => write!(f, "invalid AI edit plan: {error}"),
            Self::Failed(reason) => write!(f, "AI provider failed: {reason}"),
        }
    }
}

impl Error for ProviderError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioInput {
    pub asset_id: AssetId,
    pub range: Option<TickRange>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SttOptions {
    pub language: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ClipContext {
    pub clip_id: ClipId,
    pub asset_id: AssetId,
    pub timeline_range: TickRange,
    pub source_range: TickRange,
    pub locked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProjectContext {
    pub project_id: ProjectId,
    pub base_revision: u64,
    pub clips: Vec<ClipContext>,
}

#[derive(Serialize)]
struct SerializedPromptContext<'a> {
    context: &'a ProjectContext,
    instruction: &'a str,
}

pub fn validate_provider_request(
    context: &ProjectContext,
    instruction: &str,
) -> Result<(), ProviderError> {
    if instruction.len() > MAX_PROVIDER_INSTRUCTION_BYTES {
        return Err(ProviderError::ResourceLimit(ResourceLimitExceeded::new(
            ResourceKind::ProviderInstructionBytes,
            MAX_PROVIDER_INSTRUCTION_BYTES,
            instruction.len(),
        )));
    }
    if context.clips.len() > MAX_CONTEXT_CLIPS {
        return Err(ProviderError::ResourceLimit(ResourceLimitExceeded::new(
            ResourceKind::Clips,
            MAX_CONTEXT_CLIPS,
            context.clips.len(),
        )));
    }
    let prompt_context = SerializedPromptContext {
        context,
        instruction,
    };
    match serialized_size(&prompt_context, MAX_SERIALIZED_PROMPT_CONTEXT_BYTES) {
        Ok(_) => Ok(()),
        Err(SerializationSizeError::LimitExceeded) => {
            Err(ProviderError::ResourceLimit(ResourceLimitExceeded::new(
                ResourceKind::SerializedPromptContextBytes,
                MAX_SERIALIZED_PROMPT_CONTEXT_BYTES,
                MAX_SERIALIZED_PROMPT_CONTEXT_BYTES.saturating_add(1),
            )))
        }
        Err(SerializationSizeError::Serialization(error)) => {
            Err(ProviderError::InvalidRequest(error))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TtsRequest {
    pub text: String,
    pub voice_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioAsset {
    pub asset_id: AssetId,
    pub duration: TickRange,
    pub provenance: ModelProvenance,
}

pub trait SpeechToText {
    fn transcribe(
        &self,
        audio: AudioInput,
        options: SttOptions,
    ) -> Result<Transcript, ProviderError>;
}

mod private {
    use super::{EditPlan, ProjectContext, ProviderError};

    pub trait LanguageModelProvider {
        fn provide_plan(
            &self,
            context: ProjectContext,
            instruction: String,
        ) -> Result<EditPlan, ProviderError>;
    }
}

#[allow(private_bounds)]
pub trait LanguageModel: private::LanguageModelProvider {
    fn plan_edits(
        &self,
        context: ProjectContext,
        instruction: String,
    ) -> Result<EditPlan, ProviderError>
    where
        Self: Sized,
    {
        self.plan_edits_bounded(context, instruction)
    }

    fn plan_edits_bounded(
        &self,
        context: ProjectContext,
        instruction: String,
    ) -> Result<EditPlan, ProviderError>
    where
        Self: Sized,
    {
        validate_provider_request(&context, &instruction)?;
        let plan = self.provide_plan(context, instruction)?;
        plan.validate().map_err(ProviderError::InvalidPlan)?;
        Ok(plan)
    }
}

impl<T: private::LanguageModelProvider> LanguageModel for T {}

pub trait TextToSpeech {
    fn synthesize(&self, request: TtsRequest) -> Result<AudioAsset, ProviderError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_domain::{
        Asset, AssetKind, Fingerprint, Rational, Sequence, Track, TrackKind, Transform,
    };
    use std::{
        cell::Cell,
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicUsize, Ordering},
    };

    struct TestArtifact {
        path: PathBuf,
    }

    impl Drop for TestArtifact {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn test_artifact(bytes: &[u8]) -> TestArtifact {
        static NEXT_ARTIFACT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "video-editor-free-editor-ai-{}-{}.model",
            std::process::id(),
            NEXT_ARTIFACT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, bytes).unwrap();
        TestArtifact { path }
    }

    fn sha256(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes).unwrap();
        hasher.finish()
    }

    fn artifact_metadata(path: &Path, bytes: &[u8], declared_sha256: String) -> ArtifactMetadata {
        ArtifactMetadata {
            path: path.to_path_buf(),
            size_bytes: bytes.len() as u64,
            sha256: declared_sha256,
        }
    }

    fn hash(ch: char) -> String {
        std::iter::repeat_n(ch, 64).collect()
    }

    fn manifest() -> ModelManifest {
        ModelManifest {
            id: "local-model".into(),
            version: "1".into(),
            format: "gguf".into(),
            runtime: "llama.cpp".into(),
            artifact: "models/model.gguf".into(),
            source: "manual-import".into(),
            license: "MIT".into(),
            size_bytes: 10,
            sha256: hash('a'),
            capabilities: vec![ModelCapability::LanguageModel],
            requirements: ResourceRequirements {
                ram_mb: 100,
                vram_mb: 0,
                architecture: "x86_64".into(),
            },
        }
    }

    fn environment() -> RuntimeEnvironment {
        RuntimeEnvironment {
            runtime: "llama.cpp".into(),
            architecture: "x86_64".into(),
            available_ram_mb: 512,
            available_vram_mb: 0,
            runtime_available: true,
        }
    }

    fn plan_json(operation: &str) -> String {
        format!(
            r#"{{"base_revision":0,"operations":[{operation}],"warnings":[],"affected_clips":["clip-1"],"requires_confirmation":true}}"#
        )
    }

    fn context() -> ProjectContext {
        let project = project();
        let clip = &project.sequence.tracks[0].clips[0];
        ProjectContext {
            project_id: ProjectId::new("project-1").unwrap(),
            base_revision: project.revision,
            clips: vec![ClipContext {
                clip_id: clip.id.clone(),
                asset_id: clip.asset_id.clone(),
                timeline_range: TickRange::new(0, 20).unwrap(),
                source_range: TickRange::new(0, 20).unwrap(),
                locked: false,
            }],
        }
    }

    fn oversized_context() -> ProjectContext {
        let id = "x".repeat(128);
        ProjectContext {
            project_id: ProjectId::new(id.clone()).unwrap(),
            base_revision: 0,
            clips: (0..(MAX_CONTEXT_CLIPS - 1))
                .map(|_| ClipContext {
                    clip_id: ClipId::new(id.clone()).unwrap(),
                    asset_id: AssetId::new(id.clone()).unwrap(),
                    timeline_range: TickRange::new(0, 20).unwrap(),
                    source_range: TickRange::new(0, 20).unwrap(),
                    locked: false,
                })
                .collect(),
        }
    }

    fn valid_plan() -> EditPlan {
        parse_edit_plan_json(&plan_json(r#"{"op":"delete","clip_id":"clip-1"}"#)).unwrap()
    }

    struct CountingProvider<'a>(&'a Cell<usize>);

    impl private::LanguageModelProvider for CountingProvider<'_> {
        fn provide_plan(
            &self,
            _context: ProjectContext,
            _instruction: String,
        ) -> Result<EditPlan, ProviderError> {
            self.0.set(self.0.get() + 1);
            Err(ProviderError::Failed("provider must not be called".into()))
        }
    }

    fn project() -> ProjectDocument {
        let project_id = ProjectId::new("project-1").unwrap();
        let asset_id = AssetId::new("asset-1").unwrap();
        let track_id = TrackId::new("track-1").unwrap();
        let mut project = ProjectDocument::create(project_id, "Test").unwrap();
        project.assets.push(Asset {
            id: asset_id.clone(),
            relative_path: editor_domain::RelativePath::new("media/a.mp4").unwrap(),
            kind: AssetKind::Video,
            fingerprint: Fingerprint {
                size_bytes: 1,
                modified_time: "now".into(),
                sha256: None,
            },
            probe: None,
            status: editor_domain::AssetStatus::Available,
        });
        let mut track = Track::new(track_id, TrackKind::Video, "Video").unwrap();
        track.clips.push(editor_domain::Clip {
            id: ClipId::new("clip-1").unwrap(),
            asset_id,
            timeline_start: 0,
            timeline_duration: 20,
            source_start: 0,
            source_duration: 20,
            speed: Rational::new(1, 1).unwrap(),
            opacity: 1.0,
            transform: Transform::default(),
            effects: Vec::new(),
            keyframes: Vec::new(),
        });
        project.sequence = Sequence {
            tracks: vec![track],
            ..Sequence::default()
        };
        project.validate().unwrap();
        project
    }

    #[test]
    fn readiness_requires_metadata_and_matching_checksum() {
        let missing = evaluate_model_readiness(&manifest(), None, &environment());
        assert_eq!(missing.status, ModelStatus::Missing);
        let bytes = b"0123456789";
        let file = test_artifact(bytes);
        let mut matching_manifest = manifest();
        matching_manifest.sha256 = sha256(bytes);
        let bad = artifact_metadata(&file.path, bytes, hash('b'));
        assert_eq!(
            evaluate_model_readiness(&matching_manifest, Some(&bad), &environment()).status,
            ModelStatus::Failed
        );
        let good = artifact_metadata(&file.path, bytes, sha256(bytes));
        assert_eq!(
            evaluate_model_readiness(&matching_manifest, Some(&good), &environment()).status,
            ModelStatus::Ready
        );
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn readiness_rejects_declared_checksum_when_file_bytes_do_not_match() {
        let bytes = b"0123456789";
        let file = test_artifact(bytes);
        let manifest = manifest();
        let artifact = artifact_metadata(&file.path, bytes, manifest.sha256.clone());
        assert_eq!(
            evaluate_model_readiness(&manifest, Some(&artifact), &environment()).status,
            ModelStatus::Failed
        );
    }

    #[test]
    fn malformed_unknown_and_forbidden_plans_fail_closed() {
        assert!(matches!(
            parse_edit_plan_json("{}"),
            Err(PlanError::Malformed(_))
        ));
        assert!(matches!(
            parse_edit_plan_json(&plan_json(r#"{"op":"unknown","clip_id":"clip-1"}"#)),
            Err(PlanError::UnknownOperation(_))
        ));
        assert!(matches!(
            parse_edit_plan_json(&plan_json(r#"{"op":"shell","clip_id":"clip-1"}"#)),
            Err(PlanError::ForbiddenOperation(_))
        ));
        assert!(matches!(
            parse_edit_plan_json(&plan_json(
                r#"{"op":"delete","clip_id":"clip-1","path":"x"}"#
            )),
            Err(PlanError::ForbiddenField(_))
        ));
        assert!(matches!(
            parse_edit_plan_json("EDIT: trim clip-1"),
            Err(PlanError::ForbiddenField(_))
        ));
    }

    #[test]
    fn cue_range_is_half_open_and_validated() {
        let invalid = r#"{"provenance":{"provider":"p","model_id":"m","model_version":"1"},"cues":[{"range":{"start":4,"end":4},"text":"x","confidence":null,"speaker":null}]}"#;
        assert!(matches!(
            parse_transcript_json(invalid),
            Err(TranscriptError::InvalidDomain(_))
        ));
        let unknown = r#"{"provenance":{"provider":"p","model_id":"m","model_version":"1"},"cues":[{"range":{"start":0,"end":4,"extra":true},"text":"x","confidence":null,"speaker":null}]}"#;
        assert!(matches!(
            parse_transcript_json(unknown),
            Err(TranscriptError::Json(_))
        ));
        let valid = r#"{"provenance":{"provider":"p","model_id":"m","model_version":"1"},"cues":[{"range":{"start":0,"end":4},"text":"x","confidence":1.0,"speaker":null}]}"#;
        assert!(parse_transcript_json(valid).is_ok());
    }

    #[test]
    fn whisper_json_becomes_millisecond_cues() {
        let json = r#"{"transcription":[{"offsets":{"from":0,"to":1250},"text":" Hello "}]}"#;
        let transcript = parse_whisper_json(
            json,
            "id",
            ModelProvenance {
                provider: "whisper.cpp".into(),
                model_id: "ggml-tiny".into(),
                model_version: "1.9.0".into(),
            },
        )
        .unwrap();
        assert_eq!(transcript.cues[0].range, TickRange::new(0, 1250).unwrap());
        assert_eq!(transcript.cues[0].text, "Hello");
    }

    #[test]
    fn plan_requires_confirmation_and_revision_match_without_mutating_project() {
        let plan =
            parse_edit_plan_json(&plan_json(r#"{"op":"delete","clip_id":"clip-1"}"#)).unwrap();
        let project = project();
        let before = project.clone();
        assert!(validate_plan_for_apply(&project, &plan, false).is_err());
        assert_eq!(project, before);
        let mut stale = plan.clone();
        stale.base_revision = 1;
        assert!(matches!(
            validate_plan_for_apply(&project, &stale, true),
            Err(PlanError::RevisionMismatch { .. })
        ));
    }

    #[test]
    fn valid_plan_returns_typed_operations_only() {
        let plan = parse_edit_plan_json(&plan_json(
            r#"{"op":"trim","clip_id":"clip-1","source_start":2,"source_end":8}"#,
        ))
        .unwrap();
        let validated = validate_plan_for_apply(&project(), &plan, true).unwrap();
        assert_eq!(validated.base_revision(), 0);
        assert!(matches!(
            validated.operations()[0],
            TimelineOperation::TrimClip {
                source_start: 2,
                source_end: 8,
                ..
            }
        ));
    }

    #[test]
    fn provider_instruction_limit_is_checked_before_provider_call() {
        let calls = Cell::new(0);
        let provider = CountingProvider(&calls);
        let result =
            provider.plan_edits_bounded(context(), "x".repeat(MAX_PROVIDER_INSTRUCTION_BYTES + 1));
        assert!(matches!(
            result,
            Err(ProviderError::ResourceLimit(ResourceLimitExceeded {
                resource: ResourceKind::ProviderInstructionBytes,
                ..
            }))
        ));
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn direct_public_plan_entrypoint_enforces_limits_before_provider_call() {
        let calls = Cell::new(0);
        let provider = CountingProvider(&calls);
        let result = provider.plan_edits(context(), "x".repeat(MAX_PROVIDER_INSTRUCTION_BYTES + 1));
        assert!(matches!(
            result,
            Err(ProviderError::ResourceLimit(ResourceLimitExceeded {
                resource: ResourceKind::ProviderInstructionBytes,
                ..
            }))
        ));
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn context_clip_limit_is_checked_before_provider_call() {
        let calls = Cell::new(0);
        let provider = CountingProvider(&calls);
        let mut context = context();
        context.clips = vec![context.clips[0].clone(); MAX_CONTEXT_CLIPS + 1];
        let result = provider.plan_edits_bounded(context, "trim".into());
        assert!(matches!(
            result,
            Err(ProviderError::ResourceLimit(ResourceLimitExceeded {
                resource: ResourceKind::Clips,
                ..
            }))
        ));
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn serialized_prompt_context_limit_is_checked_before_provider_call() {
        let calls = Cell::new(0);
        let provider = CountingProvider(&calls);
        let result = provider.plan_edits_bounded(oversized_context(), "trim".into());
        assert!(matches!(
            result,
            Err(ProviderError::ResourceLimit(ResourceLimitExceeded {
                resource: ResourceKind::SerializedPromptContextBytes,
                ..
            }))
        ));
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn plan_operation_limit_is_checked_before_apply() {
        let mut plan = valid_plan();
        plan.operations = vec![plan.operations[0].clone(); MAX_PLAN_OPERATIONS + 1];
        assert!(matches!(
            validate_plan_for_apply(&project(), &plan, true),
            Err(PlanError::ResourceLimit(ResourceLimitExceeded {
                resource: ResourceKind::Operations,
                ..
            }))
        ));
    }

    #[test]
    fn plan_warning_limit_is_checked_before_apply() {
        let mut plan = valid_plan();
        plan.warnings = vec!["warning".into(); MAX_PLAN_WARNINGS + 1];
        assert!(matches!(
            validate_plan_for_apply(&project(), &plan, true),
            Err(PlanError::ResourceLimit(ResourceLimitExceeded {
                resource: ResourceKind::Warnings,
                ..
            }))
        ));
    }

    #[test]
    fn transcript_cue_limit_is_checked() {
        let cue = Cue {
            range: TickRange::new(0, 1).unwrap(),
            text: "cue".into(),
            confidence: None,
            speaker: None,
        };
        let transcript = Transcript {
            provenance: ModelProvenance {
                provider: "p".into(),
                model_id: "m".into(),
                model_version: "1".into(),
            },
            cues: vec![cue; MAX_TRANSCRIPT_CUES + 1],
        };
        assert!(matches!(
            transcript.validate(),
            Err(TranscriptError::ResourceLimit(ResourceLimitExceeded {
                resource: ResourceKind::Cues,
                ..
            }))
        ));
    }

    #[test]
    fn serialized_plan_limit_is_checked_before_apply() {
        let mut plan = valid_plan();
        plan.warnings = vec!["x".repeat(MAX_SERIALIZED_PLAN_BYTES)];
        assert!(matches!(
            validate_plan_for_apply(&project(), &plan, true),
            Err(PlanError::ResourceLimit(ResourceLimitExceeded {
                resource: ResourceKind::SerializedPlanBytes,
                ..
            }))
        ));
    }
}
