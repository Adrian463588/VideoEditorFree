//! Canonical, side-effect-free project and timeline domain model.
//!
//! This crate deliberately knows nothing about Tauri, media codecs, filesystems,
//! databases, UI state, models, or rendering. It owns the serializable project
//! IR and the mutations that can safely change it.

use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DomainError {
    InvalidId {
        kind: String,
        reason: String,
    },
    InvalidValue {
        field: String,
        reason: String,
    },
    RevisionConflict {
        expected: u64,
        actual: u64,
    },
    NotFound {
        entity: String,
        id: String,
    },
    Locked {
        entity: String,
        id: String,
    },
    Overlap {
        track_id: TrackId,
        first: ClipId,
        second: ClipId,
    },
    AssetKindMismatch {
        track: TrackKind,
        asset: AssetKind,
    },
    UnsafeOperation {
        operation: String,
        reason: String,
    },
}

impl fmt::Display for DomainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId { kind, reason } => write!(formatter, "invalid {kind} ID: {reason}"),
            Self::InvalidValue { field, reason } => write!(formatter, "invalid {field}: {reason}"),
            Self::RevisionConflict { expected, actual } => {
                write!(
                    formatter,
                    "revision conflict: expected {expected}, actual {actual}"
                )
            }
            Self::NotFound { entity, id } => write!(formatter, "{entity} not found: {id}"),
            Self::Locked { entity, id } => write!(formatter, "{entity} is locked: {id}"),
            Self::Overlap {
                track_id,
                first,
                second,
            } => {
                write!(
                    formatter,
                    "clips overlap on track {track_id}: {first}, {second}"
                )
            }
            Self::AssetKindMismatch { track, asset } => {
                write!(
                    formatter,
                    "asset kind {asset:?} is not valid on {track:?} track"
                )
            }
            Self::UnsafeOperation { operation, reason } => {
                write!(formatter, "unsafe {operation} operation: {reason}")
            }
        }
    }
}

impl Error for DomainError {}

macro_rules! id_type {
    ($name:ident, $kind:literal) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
                let value = value.into();
                validate_id($kind, &value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }

            pub fn validate(&self) -> Result<(), DomainError> {
                validate_id($kind, &self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl TryFrom<String> for $name {
            type Error = DomainError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }
    };
}

id_type!(ProjectId, "project");
id_type!(AssetId, "asset");
id_type!(TrackId, "track");
id_type!(ClipId, "clip");
id_type!(MarkerId, "marker");

fn validate_id(kind: &str, value: &str) -> Result<(), DomainError> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value == value.trim()
        && !value
            .chars()
            .any(|character| character.is_control() || character == '/' || character == '\\');
    if valid {
        Ok(())
    } else {
        Err(DomainError::InvalidId {
            kind: kind.to_owned(),
            reason: "must be 1..=128 trimmed characters without separators or control characters"
                .to_owned(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RelativePath(String);

impl RelativePath {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.is_empty()
            || value == "/"
            || value.starts_with('/')
            || value.starts_with('\\')
            || value.as_bytes().get(1) == Some(&b':')
            || value.chars().any(char::is_control)
            || value.split(['/', '\\']).any(|component| component == "..")
        {
            return Err(DomainError::InvalidValue {
                field: "relative_path".to_owned(),
                reason: "must be non-empty, relative, and free of traversal components".to_owned(),
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        Self::new(self.0.clone()).map(|_| ())
    }
}

impl fmt::Display for RelativePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rational {
    pub numerator: i64,
    pub denominator: i64,
}

impl Rational {
    pub fn new(numerator: i64, denominator: i64) -> Result<Self, DomainError> {
        if numerator <= 0 || denominator <= 0 {
            return Err(DomainError::InvalidValue {
                field: "rational".to_owned(),
                reason: "numerator and denominator must be positive".to_owned(),
            });
        }
        let divisor = gcd(numerator as u64, denominator as u64) as i64;
        Ok(Self {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        })
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if self.numerator <= 0 || self.denominator <= 0 {
            return Err(DomainError::InvalidValue {
                field: "rational".to_owned(),
                reason: "numerator and denominator must be positive".to_owned(),
            });
        }
        if gcd(self.numerator as u64, self.denominator as u64) != 1 {
            return Err(DomainError::InvalidValue {
                field: "rational".to_owned(),
                reason: "must be reduced".to_owned(),
            });
        }
        Ok(())
    }

    pub fn as_f64(self) -> f64 {
        self.numerator as f64 / self.denominator as f64
    }
}

fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickRange {
    pub start: i64,
    pub end: i64,
}

impl TickRange {
    pub fn new(start: i64, end: i64) -> Result<Self, DomainError> {
        if start < 0 || end <= start {
            return Err(DomainError::InvalidValue {
                field: "tick_range".to_owned(),
                reason: "requires 0 <= start < end for half-open [start, end)".to_owned(),
            });
        }
        Ok(Self { start, end })
    }

    pub fn from_duration(start: i64, duration: i64) -> Result<Self, DomainError> {
        let end = start
            .checked_add(duration)
            .ok_or_else(|| DomainError::InvalidValue {
                field: "tick_range".to_owned(),
                reason: "end overflows i64".to_owned(),
            })?;
        Self::new(start, end)
    }

    pub fn duration(self) -> i64 {
        self.end - self.start
    }

    pub fn contains(self, tick: i64) -> bool {
        self.start <= tick && tick < self.end
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        Self::new(self.start, self.end).map(|_| ())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetKind {
    Video,
    Audio,
    Image,
    Subtitle,
    Text,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub size_bytes: u64,
    pub modified_time: String,
    pub sha256: Option<String>,
}

impl Fingerprint {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.modified_time.is_empty() {
            return Err(invalid("fingerprint.modified_time", "must not be empty"));
        }
        if let Some(hash) = &self.sha256 {
            let valid =
                hash.len() == 64 && hash.chars().all(|character| character.is_ascii_hexdigit());
            if !valid {
                return Err(invalid(
                    "fingerprint.sha256",
                    "must be a 64-character hexadecimal hash",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetStatus {
    Available,
    Missing,
    Unsupported,
    Invalid,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoStream {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub frame_rate: Option<Rational>,
}

impl VideoStream {
    fn validate(&self) -> Result<(), DomainError> {
        if self.codec.trim().is_empty() {
            return Err(invalid("probe.video.codec", "must not be empty"));
        }
        if self.width == 0 || self.height == 0 {
            return Err(invalid("probe.video", "width and height must be positive"));
        }
        if let Some(rate) = self.frame_rate {
            rate.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioStream {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
}

impl AudioStream {
    fn validate(&self) -> Result<(), DomainError> {
        if self.codec.trim().is_empty() {
            return Err(invalid("probe.audio.codec", "must not be empty"));
        }
        if self.sample_rate == 0 || self.channels == 0 {
            return Err(invalid(
                "probe.audio",
                "sample rate and channels must be positive",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeSummary {
    pub duration_ticks: i64,
    pub stream_timebase: Rational,
    pub video: Option<VideoStream>,
    pub audio: Option<AudioStream>,
    pub rotation_degrees: Option<i16>,
    pub raw_tool_version: String,
}

impl ProbeSummary {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.duration_ticks <= 0 {
            return Err(invalid("probe.duration_ticks", "must be positive"));
        }
        self.stream_timebase.validate()?;
        if self.video.is_none() && self.audio.is_none() {
            return Err(invalid("probe", "must contain a video or audio stream"));
        }
        if let Some(video) = &self.video {
            video.validate()?;
        }
        if let Some(audio) = &self.audio {
            audio.validate()?;
        }
        if let Some(rotation) = self.rotation_degrees {
            if !(-360..=360).contains(&rotation) {
                return Err(invalid(
                    "probe.rotation_degrees",
                    "must be between -360 and 360",
                ));
            }
        }
        if self.raw_tool_version.trim().is_empty() {
            return Err(invalid("probe.raw_tool_version", "must not be empty"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub id: AssetId,
    pub relative_path: RelativePath,
    pub kind: AssetKind,
    pub fingerprint: Fingerprint,
    pub probe: Option<ProbeSummary>,
    pub status: AssetStatus,
}

impl Asset {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.id.validate()?;
        self.relative_path.validate()?;
        self.fingerprint.validate()?;
        if let Some(probe) = &self.probe {
            probe.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
    Subtitle,
    Text,
    Overlay,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RgbDelta {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RgbColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextOverlay {
    pub text: String,
    pub font_size: f32,
    pub color: String,
    pub position_x: f32,
    pub position_y: f32,
}

impl TextOverlay {
    fn validate(&self) -> Result<(), DomainError> {
        if self.text.trim().is_empty() || self.text.len() > 4_096 || self.text.contains('\0') {
            return Err(invalid(
                "text_overlay.text",
                "must contain 1..=4096 non-NUL bytes",
            ));
        }
        finite_between("text_overlay.font_size", self.font_size, 8.0, 512.0)?;
        let valid_color = matches!(self.color.len(), 7 | 9)
            && self.color.starts_with('#')
            && self.color[1..]
                .chars()
                .all(|value| value.is_ascii_hexdigit());
        if !valid_color {
            return Err(invalid(
                "text_overlay.color",
                "must be #RRGGBB or #RRGGBBAA",
            ));
        }
        finite_between("text_overlay.position_x", self.position_x, -1.0, 1.0)?;
        finite_between("text_overlay.position_y", self.position_y, -1.0, 1.0)
    }
}

impl RgbDelta {
    pub fn validate(&self) -> Result<(), DomainError> {
        for (field, value) in [
            ("red", self.red),
            ("green", self.green),
            ("blue", self.blue),
        ] {
            finite_between(&format!("rgb_delta.{field}"), value, -1.0, 1.0)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FadeKind {
    In,
    Out,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DuckingConfig {
    pub source_track_id: TrackId,
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
}

impl DuckingConfig {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.source_track_id.validate()?;
        finite_between("track.ducking.threshold_db", self.threshold_db, -60.0, 0.0)?;
        finite_between("track.ducking.ratio", self.ratio, 1.0, 20.0)?;
        finite_between("track.ducking.attack_ms", self.attack_ms, 0.01, 2_000.0)?;
        finite_between("track.ducking.release_ms", self.release_ms, 0.01, 9_000.0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Effect {
    Brightness {
        value: f32,
    },
    Contrast {
        value: f32,
    },
    Saturation {
        value: f32,
    },
    Exposure {
        value: f32,
    },
    Gamma {
        value: f32,
    },
    Temperature {
        kelvin: f32,
    },
    Tint {
        value: f32,
    },
    ColorBalance {
        shadows: RgbDelta,
        midtones: RgbDelta,
        highlights: RgbDelta,
    },
    Crop {
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    },
    Rotate {
        degrees: i16,
    },
    Blur {
        radius: f32,
    },
    Sharpen {
        amount: f32,
    },
    Vignette {
        amount: f32,
    },
    Duotone {
        shadows: RgbColor,
        highlights: RgbColor,
    },
    Lut {
        relative_path: RelativePath,
    },
    Speed {
        factor: Rational,
        preserve_pitch: bool,
    },
    Volume {
        gain_db: f32,
    },
    Fade {
        kind: FadeKind,
        duration_ticks: i64,
    },
}

impl Effect {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Brightness { value } => finite_between("effect.brightness", *value, -1.0, 1.0),
            Self::Contrast { value } => finite_between("effect.contrast", *value, 0.0, 4.0),
            Self::Saturation { value } => finite_between("effect.saturation", *value, 0.0, 4.0),
            Self::Exposure { value } => finite_between("effect.exposure", *value, -10.0, 10.0),
            Self::Gamma { value } => finite_between("effect.gamma", *value, 0.1, 10.0),
            Self::Temperature { kelvin } => {
                finite_between("effect.temperature.kelvin", *kelvin, 1_000.0, 40_000.0)
            }
            Self::Tint { value } => finite_between("effect.tint", *value, -1.0, 1.0),
            Self::ColorBalance {
                shadows,
                midtones,
                highlights,
            } => {
                shadows.validate()?;
                midtones.validate()?;
                highlights.validate()
            }
            Self::Crop {
                left,
                top,
                right,
                bottom,
            } => {
                finite_between("effect.crop.left", *left, 0.0, 1.0)?;
                finite_between("effect.crop.top", *top, 0.0, 1.0)?;
                finite_between("effect.crop.right", *right, 0.0, 1.0)?;
                finite_between("effect.crop.bottom", *bottom, 0.0, 1.0)?;
                if left + right >= 1.0 || top + bottom >= 1.0 {
                    return Err(invalid(
                        "effect.crop",
                        "opposite margins must leave visible content",
                    ));
                }
                Ok(())
            }
            Self::Rotate { .. } => Ok(()),
            Self::Blur { radius } => finite_between("effect.blur.radius", *radius, 0.1, 100.0),
            Self::Sharpen { amount } => finite_between("effect.sharpen.amount", *amount, 0.0, 10.0),
            Self::Vignette { amount } => {
                finite_between("effect.vignette.amount", *amount, 0.0, 1.0)
            }
            Self::Duotone { .. } => Ok(()),
            Self::Lut { relative_path } => relative_path.validate(),
            Self::Speed { factor, .. } => factor.validate(),
            Self::Volume { gain_db } => {
                finite_between("effect.volume.gain_db", *gain_db, -120.0, 24.0)
            }
            Self::Fade { duration_ticks, .. } => {
                if *duration_ticks > 0 {
                    Ok(())
                } else {
                    Err(invalid("effect.fade.duration_ticks", "must be positive"))
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    pub position_x: f32,
    pub position_y: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub rotation_degrees: f32,
    pub anchor_x: f32,
    pub anchor_y: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            position_x: 0.0,
            position_y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            rotation_degrees: 0.0,
            anchor_x: 0.5,
            anchor_y: 0.5,
        }
    }
}

impl Transform {
    fn validate(&self) -> Result<(), DomainError> {
        finite_between(
            "transform.position_x",
            self.position_x,
            -1_000_000.0,
            1_000_000.0,
        )?;
        finite_between(
            "transform.position_y",
            self.position_y,
            -1_000_000.0,
            1_000_000.0,
        )?;
        finite_between("transform.scale_x", self.scale_x, 0.0, 1000.0)?;
        finite_between("transform.scale_y", self.scale_y, 0.0, 1000.0)?;
        finite_between(
            "transform.rotation_degrees",
            self.rotation_degrees,
            -36000.0,
            36000.0,
        )?;
        finite_between("transform.anchor_x", self.anchor_x, 0.0, 1.0)?;
        finite_between("transform.anchor_y", self.anchor_y, 0.0, 1.0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum KeyframeProperty {
    Opacity,
    PositionX,
    PositionY,
    ScaleX,
    ScaleY,
    Rotation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum KeyframeValue {
    Scalar { value: f32 },
    Point { x: f32, y: f32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    /// Tick offset from clip start, therefore in [0, clip.timeline_duration).
    pub at_tick: i64,
    pub property: KeyframeProperty,
    pub value: KeyframeValue,
}

impl Keyframe {
    fn validate(&self, clip_duration: i64) -> Result<(), DomainError> {
        if self.at_tick < 0 || self.at_tick >= clip_duration {
            return Err(invalid(
                "keyframe.at_tick",
                "must be inside the clip half-open range",
            ));
        }
        match self.value {
            KeyframeValue::Scalar { value } => {
                if !value.is_finite() {
                    return Err(invalid("keyframe.value", "must be finite"));
                }
            }
            KeyframeValue::Point { x, y } => {
                if !x.is_finite() || !y.is_finite() {
                    return Err(invalid("keyframe.value", "must be finite"));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub asset_id: AssetId,
    pub timeline_start: i64,
    pub timeline_duration: i64,
    pub source_start: i64,
    pub source_duration: i64,
    pub speed: Rational,
    pub opacity: f32,
    pub transform: Transform,
    pub effects: Vec<Effect>,
    pub keyframes: Vec<Keyframe>,
    #[serde(default)]
    pub text_overlay: Option<TextOverlay>,
}

impl Clip {
    pub fn timeline_range(&self) -> Result<TickRange, DomainError> {
        TickRange::from_duration(self.timeline_start, self.timeline_duration)
    }

    pub fn source_range(&self) -> Result<TickRange, DomainError> {
        TickRange::from_duration(self.source_start, self.source_duration)
    }

    fn validate(&self, asset: &Asset) -> Result<(), DomainError> {
        self.id.validate()?;
        if self.asset_id != asset.id {
            return Err(DomainError::NotFound {
                entity: "asset".to_owned(),
                id: self.asset_id.to_string(),
            });
        }
        let timeline = self.timeline_range()?;
        let source = self.source_range()?;
        self.speed.validate()?;
        if !self.opacity.is_finite() || !(0.0..=1.0).contains(&self.opacity) {
            return Err(invalid(
                "clip.opacity",
                "must be finite and between 0 and 1",
            ));
        }
        self.transform.validate()?;
        match (&asset.kind, &self.text_overlay) {
            (AssetKind::Text, Some(text_overlay)) => text_overlay.validate()?,
            (AssetKind::Text, None) => {
                return Err(invalid(
                    "clip.text_overlay",
                    "text assets require text overlay content",
                ))
            }
            (_, Some(_)) => {
                return Err(invalid(
                    "clip.text_overlay",
                    "text overlay content requires a text asset",
                ))
            }
            (_, None) => {}
        }
        for effect in &self.effects {
            effect.validate()?;
        }
        for keyframe in &self.keyframes {
            keyframe.validate(timeline.duration())?;
        }
        if let Some(probe) = &asset.probe {
            let source_end = source.end;
            if source_end > probe.duration_ticks {
                return Err(invalid(
                    "clip.source_range",
                    "exceeds probed asset duration",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub kind: TrackKind,
    pub name: String,
    pub enabled: bool,
    pub locked: bool,
    #[serde(default)]
    pub ducking: Option<DuckingConfig>,
    pub clips: Vec<Clip>,
}

impl Track {
    pub fn new(id: TrackId, kind: TrackKind, name: impl Into<String>) -> Result<Self, DomainError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(invalid("track.name", "must not be empty"));
        }
        Ok(Self {
            id,
            kind,
            name,
            enabled: true,
            locked: false,
            ducking: None,
            clips: Vec::new(),
        })
    }

    fn validate(&self, assets: &[Asset]) -> Result<(), DomainError> {
        self.id.validate()?;
        if self.name.trim().is_empty() {
            return Err(invalid("track.name", "must not be empty"));
        }
        if let Some(ducking) = &self.ducking {
            if !matches!(self.kind, TrackKind::Audio) {
                return Err(invalid(
                    "track.ducking",
                    "ducking is only supported on audio tracks",
                ));
            }
            ducking.validate()?;
        }
        let mut seen: Vec<(ClipId, TickRange)> = Vec::new();
        for clip in &self.clips {
            let asset = assets
                .iter()
                .find(|asset| asset.id == clip.asset_id)
                .ok_or_else(|| DomainError::NotFound {
                    entity: "asset".to_owned(),
                    id: clip.asset_id.to_string(),
                })?;
            if !track_accepts_asset(&self.kind, &asset.kind) {
                return Err(DomainError::AssetKindMismatch {
                    track: self.kind.clone(),
                    asset: asset.kind.clone(),
                });
            }
            clip.validate(asset)?;
            let range = clip.timeline_range()?;
            for (other_id, other_range) in &seen {
                if range.overlaps(*other_range) {
                    return Err(DomainError::Overlap {
                        track_id: self.id.clone(),
                        first: other_id.clone(),
                        second: clip.id.clone(),
                    });
                }
            }
            seen.push((clip.id.clone(), range));
        }
        Ok(())
    }
}

fn track_accepts_asset(track: &TrackKind, asset: &AssetKind) -> bool {
    match track {
        TrackKind::Video => matches!(asset, AssetKind::Video | AssetKind::Image),
        TrackKind::Audio => matches!(asset, AssetKind::Video | AssetKind::Audio),
        TrackKind::Subtitle => matches!(asset, AssetKind::Subtitle),
        TrackKind::Text => matches!(asset, AssetKind::Text),
        TrackKind::Overlay => matches!(asset, AssetKind::Video | AssetKind::Image),
    }
}

fn validate_ducking_references(tracks: &[Track]) -> Result<(), DomainError> {
    for track in tracks {
        let Some(ducking) = &track.ducking else {
            continue;
        };
        let source = tracks
            .iter()
            .find(|candidate| candidate.id == ducking.source_track_id)
            .ok_or_else(|| DomainError::NotFound {
                entity: "track".to_owned(),
                id: ducking.source_track_id.to_string(),
            })?;
        if !matches!(source.kind, TrackKind::Audio) {
            return Err(invalid(
                "track.ducking.source_track_id",
                "source track must be an audio track",
            ));
        }
        if source.id == track.id {
            return Err(invalid(
                "track.ducking.source_track_id",
                "source track must differ from the target track",
            ));
        }

        let mut visited = Vec::new();
        let mut current = track.id.clone();
        loop {
            if visited.contains(&current) {
                return Err(invalid(
                    "track.ducking",
                    "ducking references must not form a cycle",
                ));
            }
            visited.push(current.clone());
            let Some(node) = tracks.iter().find(|candidate| candidate.id == current) else {
                break;
            };
            let Some(next) = node.ducking.as_ref() else {
                break;
            };
            current = next.source_track_id.clone();
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub id: MarkerId,
    pub position_ticks: i64,
    pub name: String,
    pub comment: Option<String>,
    pub color_tag: Option<String>,
    pub clip_id: Option<ClipId>,
}

impl Marker {
    fn validate(&self) -> Result<(), DomainError> {
        self.id.validate()?;
        if self.position_ticks < 0 {
            return Err(invalid("marker.position_ticks", "must not be negative"));
        }
        if self.name.trim().is_empty() {
            return Err(invalid("marker.name", "must not be empty"));
        }
        if self
            .color_tag
            .as_ref()
            .is_some_and(|tag| tag.trim().is_empty())
        {
            return Err(invalid(
                "marker.color_tag",
                "must not be empty when present",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sequence {
    pub timebase: Rational,
    pub width: u32,
    pub height: u32,
    pub pixel_aspect: Rational,
    pub audio_sample_rate: u32,
    pub audio_channels: u16,
    pub tracks: Vec<Track>,
    pub markers: Vec<Marker>,
}

impl Default for Sequence {
    fn default() -> Self {
        Self {
            timebase: Rational::new(30, 1).expect("constant rational is valid"),
            width: 1920,
            height: 1080,
            pixel_aspect: Rational::new(1, 1).expect("constant rational is valid"),
            audio_sample_rate: 48_000,
            audio_channels: 2,
            tracks: Vec::new(),
            markers: Vec::new(),
        }
    }
}

impl Sequence {
    fn validate(&self, assets: &[Asset]) -> Result<(), DomainError> {
        self.timebase.validate()?;
        self.pixel_aspect.validate()?;
        if self.width == 0 || self.height == 0 {
            return Err(invalid("sequence", "width and height must be positive"));
        }
        if self.audio_sample_rate == 0 || self.audio_channels == 0 {
            return Err(invalid(
                "sequence",
                "audio sample rate and channels must be positive",
            ));
        }
        ensure_unique(self.tracks.iter().map(|track| &track.id), "track")?;
        ensure_unique(self.markers.iter().map(|marker| &marker.id), "marker")?;
        for track in &self.tracks {
            track.validate(assets)?;
        }
        validate_ducking_references(&self.tracks)?;
        for marker in &self.markers {
            marker.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectDocument {
    pub schema_version: u32,
    pub revision: u64,
    pub project_id: ProjectId,
    pub name: String,
    pub project_root: RelativePath,
    pub assets: Vec<Asset>,
    pub sequence: Sequence,
}

impl ProjectDocument {
    pub fn create(project_id: ProjectId, name: impl Into<String>) -> Result<Self, DomainError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(invalid("project.name", "must not be empty"));
        }
        Ok(Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            revision: 0,
            project_id,
            name,
            project_root: RelativePath::new(".")?,
            assets: Vec::new(),
            sequence: Sequence::default(),
        })
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(invalid(
                "schema_version",
                "unsupported project schema version",
            ));
        }
        self.project_id.validate()?;
        if self.name.trim().is_empty() {
            return Err(invalid("project.name", "must not be empty"));
        }
        self.project_root.validate()?;
        ensure_unique(self.assets.iter().map(|asset| &asset.id), "asset")?;
        for asset in &self.assets {
            asset.validate()?;
        }
        self.sequence.validate(&self.assets)?;

        let mut clip_ids = Vec::new();
        for track in &self.sequence.tracks {
            for clip in &track.clips {
                if clip_ids.iter().any(|seen: &ClipId| seen == &clip.id) {
                    return Err(invalid("clip.id", "duplicate clip ID"));
                }
                clip_ids.push(clip.id.clone());
            }
        }
        for marker in &self.sequence.markers {
            if let Some(clip_id) = &marker.clip_id {
                if !clip_ids.iter().any(|seen| seen == clip_id) {
                    return Err(DomainError::NotFound {
                        entity: "clip".to_owned(),
                        id: clip_id.to_string(),
                    });
                }
            }
        }
        Ok(())
    }

    pub fn apply(
        &self,
        base_revision: u64,
        operation: TimelineOperation,
    ) -> Result<ApplyResult, DomainError> {
        if base_revision != self.revision {
            return Err(DomainError::RevisionConflict {
                expected: base_revision,
                actual: self.revision,
            });
        }
        self.validate()?;
        let next_revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("revision", "cannot advance beyond u64::MAX"))?;
        let mut next = self.clone();
        apply_operation(&mut next, operation, next_revision)?;
        next.revision = next_revision;
        next.validate()?;
        Ok(ApplyResult {
            document: next,
            undo: UndoToken {
                previous: Box::new(self.clone()),
                applied_revision: next_revision,
            },
        })
    }

    pub fn apply_in_place(
        &mut self,
        base_revision: u64,
        operation: TimelineOperation,
    ) -> Result<ApplyResult, DomainError> {
        let result = self.apply(base_revision, operation)?;
        *self = result.document.clone();
        Ok(result)
    }

    pub fn undo(
        &self,
        expected_revision: u64,
        token: &UndoToken,
    ) -> Result<ApplyResult, DomainError> {
        if expected_revision != self.revision {
            return Err(DomainError::RevisionConflict {
                expected: expected_revision,
                actual: self.revision,
            });
        }
        if token.applied_revision != self.revision || token.previous.project_id != self.project_id {
            return Err(DomainError::UnsafeOperation {
                operation: "undo".to_owned(),
                reason: "undo token does not belong to current revision".to_owned(),
            });
        }
        let next_revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("revision", "cannot advance beyond u64::MAX"))?;
        let mut restored = (*token.previous).clone();
        restored.revision = next_revision;
        restored.validate()?;
        Ok(ApplyResult {
            document: restored,
            undo: UndoToken {
                previous: Box::new(self.clone()),
                applied_revision: next_revision,
            },
        })
    }

    pub fn clip(&self, clip_id: &ClipId) -> Option<&Clip> {
        self.sequence
            .tracks
            .iter()
            .flat_map(|track| &track.clips)
            .find(|clip| &clip.id == clip_id)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TimelineOperation {
    AddAsset {
        asset: Asset,
    },
    DeleteAsset {
        asset_id: AssetId,
    },
    AddTrack {
        track: Track,
    },
    DeleteTrack {
        track_id: TrackId,
    },
    AddClip {
        track_id: TrackId,
        clip: Clip,
    },
    ReplaceClipAsset {
        clip_id: ClipId,
        asset_id: AssetId,
    },
    RelinkAsset {
        asset_id: AssetId,
        relative_path: RelativePath,
        fingerprint: Fingerprint,
        probe: Option<ProbeSummary>,
        status: AssetStatus,
    },
    MoveClip {
        clip_id: ClipId,
        timeline_start: i64,
    },
    MoveClipToTrack {
        clip_id: ClipId,
        track_id: TrackId,
        timeline_start: i64,
    },
    TrimClip {
        clip_id: ClipId,
        source_start: i64,
        source_end: i64,
    },
    SplitClip {
        clip_id: ClipId,
        at_timeline_tick: i64,
    },
    ReorderClip {
        track_id: TrackId,
        clip_id: ClipId,
        new_index: usize,
    },
    DeleteClip {
        clip_id: ClipId,
    },
    RippleDelete {
        track_id: TrackId,
        clip_id: ClipId,
    },
    SetClipEffects {
        clip_id: ClipId,
        effects: Vec<Effect>,
    },
    SetClipVisuals {
        clip_id: ClipId,
        opacity: f32,
        transform: Transform,
    },
    AddMarker {
        marker: Marker,
    },
    DeleteMarker {
        marker_id: MarkerId,
    },
    SetTrackState {
        track_id: TrackId,
        enabled: Option<bool>,
        locked: Option<bool>,
    },
    SetTrackDucking {
        track_id: TrackId,
        ducking: Option<DuckingConfig>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApplyResult {
    pub document: ProjectDocument,
    pub undo: UndoToken,
}

impl std::ops::Deref for ApplyResult {
    type Target = ProjectDocument;

    fn deref(&self) -> &Self::Target {
        &self.document
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UndoToken {
    previous: Box<ProjectDocument>,
    pub applied_revision: u64,
}

fn apply_operation(
    project: &mut ProjectDocument,
    operation: TimelineOperation,
    next_revision: u64,
) -> Result<(), DomainError> {
    match operation {
        TimelineOperation::AddAsset { asset } => {
            if project
                .assets
                .iter()
                .any(|existing| existing.id == asset.id)
            {
                return Err(invalid("asset.id", "duplicate asset ID"));
            }
            asset.validate()?;
            project.assets.push(asset);
        }
        TimelineOperation::DeleteAsset { asset_id } => {
            if project
                .sequence
                .tracks
                .iter()
                .any(|track| track.clips.iter().any(|clip| clip.asset_id == asset_id))
            {
                return Err(DomainError::UnsafeOperation {
                    operation: "delete_asset".to_owned(),
                    reason: "asset is still referenced by a clip".to_owned(),
                });
            }
            let index = project
                .assets
                .iter()
                .position(|asset| asset.id == asset_id)
                .ok_or_else(|| DomainError::NotFound {
                    entity: "asset".to_owned(),
                    id: asset_id.to_string(),
                })?;
            project.assets.remove(index);
        }
        TimelineOperation::AddTrack { track } => {
            if project
                .sequence
                .tracks
                .iter()
                .any(|existing| existing.id == track.id)
            {
                return Err(invalid("track.id", "duplicate track ID"));
            }
            track.validate(&project.assets)?;
            project.sequence.tracks.push(track);
        }
        TimelineOperation::DeleteTrack { track_id } => {
            let index = project
                .sequence
                .tracks
                .iter()
                .position(|track| track.id == track_id)
                .ok_or_else(|| DomainError::NotFound {
                    entity: "track".to_owned(),
                    id: track_id.to_string(),
                })?;
            if project.sequence.tracks[index].locked {
                return Err(DomainError::Locked {
                    entity: "track".to_owned(),
                    id: track_id.to_string(),
                });
            }
            if !project.sequence.tracks[index].clips.is_empty() {
                return Err(DomainError::UnsafeOperation {
                    operation: "delete_track".to_owned(),
                    reason: "track must be empty".to_owned(),
                });
            }
            project.sequence.tracks.remove(index);
        }
        TimelineOperation::AddClip { track_id, clip } => {
            if project.clip(&clip.id).is_some() {
                return Err(invalid("clip.id", "duplicate clip ID"));
            }
            let asset = project
                .assets
                .iter()
                .find(|asset| asset.id == clip.asset_id)
                .ok_or_else(|| DomainError::NotFound {
                    entity: "asset".to_owned(),
                    id: clip.asset_id.to_string(),
                })?;
            let track = project
                .sequence
                .tracks
                .iter_mut()
                .find(|track| track.id == track_id)
                .ok_or_else(|| DomainError::NotFound {
                    entity: "track".to_owned(),
                    id: track_id.to_string(),
                })?;
            if track.locked {
                return Err(DomainError::Locked {
                    entity: "track".to_owned(),
                    id: track_id.to_string(),
                });
            }
            if !track_accepts_asset(&track.kind, &asset.kind) {
                return Err(DomainError::AssetKindMismatch {
                    track: track.kind.clone(),
                    asset: asset.kind.clone(),
                });
            }
            clip.validate(asset)?;
            let range = clip.timeline_range()?;
            if let Some(other) = track.clips.iter().find(|other| {
                other
                    .timeline_range()
                    .is_ok_and(|other_range| range.overlaps(other_range))
            }) {
                return Err(DomainError::Overlap {
                    track_id,
                    first: other.id.clone(),
                    second: clip.id,
                });
            }
            track.clips.push(clip);
        }
        TimelineOperation::ReplaceClipAsset { clip_id, asset_id } => {
            let replacement = project
                .assets
                .iter()
                .find(|asset| asset.id == asset_id)
                .ok_or_else(|| DomainError::NotFound {
                    entity: "asset".to_owned(),
                    id: asset_id.to_string(),
                })?
                .clone();
            let (track_index, clip_index) = find_clip_position(project, &clip_id)?;
            let track = &mut project.sequence.tracks[track_index];
            if track.locked {
                return Err(DomainError::Locked {
                    entity: "track".to_owned(),
                    id: track.id.to_string(),
                });
            }
            if !track_accepts_asset(&track.kind, &replacement.kind) {
                return Err(DomainError::AssetKindMismatch {
                    track: track.kind.clone(),
                    asset: replacement.kind,
                });
            }
            let mut candidate = track.clips[clip_index].clone();
            candidate.asset_id = asset_id;
            candidate.validate(&replacement)?;
            track.clips[clip_index] = candidate;
        }
        TimelineOperation::RelinkAsset {
            asset_id,
            relative_path,
            fingerprint,
            probe,
            status,
        } => {
            relative_path.validate()?;
            fingerprint.validate()?;
            if let Some(probe) = &probe {
                probe.validate()?;
            }
            let asset_index = project
                .assets
                .iter()
                .position(|asset| asset.id == asset_id)
                .ok_or_else(|| DomainError::NotFound {
                    entity: "asset".to_owned(),
                    id: asset_id.to_string(),
                })?;
            if let Some(track) = project.sequence.tracks.iter().find(|track| {
                track.locked && track.clips.iter().any(|clip| clip.asset_id == asset_id)
            }) {
                return Err(DomainError::Locked {
                    entity: "track".to_owned(),
                    id: track.id.to_string(),
                });
            }
            let mut candidate = project.assets[asset_index].clone();
            candidate.relative_path = relative_path;
            candidate.fingerprint = fingerprint;
            candidate.probe = probe;
            candidate.status = status;
            candidate.validate()?;
            project.assets[asset_index] = candidate;
        }
        TimelineOperation::MoveClip {
            clip_id,
            timeline_start,
        } => {
            if timeline_start < 0 {
                return Err(invalid("move.timeline_start", "must not be negative"));
            }
            let (track_index, clip_index) = find_clip_position(project, &clip_id)?;
            let track = &mut project.sequence.tracks[track_index];
            if track.locked {
                return Err(DomainError::Locked {
                    entity: "track".to_owned(),
                    id: track.id.to_string(),
                });
            }
            let clip_duration = track.clips[clip_index].timeline_duration;
            let new_range = TickRange::from_duration(timeline_start, clip_duration)?;
            if let Some(other) = track
                .clips
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != clip_index)
                .find_map(|(_, other)| {
                    other
                        .timeline_range()
                        .ok()
                        .filter(|other_range| new_range.overlaps(*other_range))
                        .map(|_| other)
                })
            {
                return Err(DomainError::Overlap {
                    track_id: track.id.clone(),
                    first: other.id.clone(),
                    second: clip_id,
                });
            }
            track.clips[clip_index].timeline_start = timeline_start;
        }
        TimelineOperation::MoveClipToTrack {
            clip_id,
            track_id,
            timeline_start,
        } => {
            if timeline_start < 0 {
                return Err(invalid("move.timeline_start", "must not be negative"));
            }
            let (source_track_index, clip_index) = find_clip_position(project, &clip_id)?;
            let target_track_index = project
                .sequence
                .tracks
                .iter()
                .position(|track| track.id == track_id)
                .ok_or_else(|| DomainError::NotFound {
                    entity: "track".to_owned(),
                    id: track_id.to_string(),
                })?;
            if source_track_index == target_track_index {
                return apply_operation(
                    project,
                    TimelineOperation::MoveClip {
                        clip_id,
                        timeline_start,
                    },
                    next_revision,
                );
            }
            if project.sequence.tracks[source_track_index].locked {
                return Err(DomainError::Locked {
                    entity: "track".to_owned(),
                    id: project.sequence.tracks[source_track_index].id.to_string(),
                });
            }
            if project.sequence.tracks[target_track_index].locked {
                return Err(DomainError::Locked {
                    entity: "track".to_owned(),
                    id: project.sequence.tracks[target_track_index].id.to_string(),
                });
            }
            let clip = project.sequence.tracks[source_track_index].clips[clip_index].clone();
            let asset = project
                .assets
                .iter()
                .find(|asset| asset.id == clip.asset_id)
                .ok_or_else(|| DomainError::NotFound {
                    entity: "asset".to_owned(),
                    id: clip.asset_id.to_string(),
                })?;
            let target = &project.sequence.tracks[target_track_index];
            if !track_accepts_asset(&target.kind, &asset.kind) {
                return Err(DomainError::AssetKindMismatch {
                    track: target.kind.clone(),
                    asset: asset.kind.clone(),
                });
            }
            let new_range = TickRange::from_duration(timeline_start, clip.timeline_duration)?;
            if let Some(overlap) = target.clips.iter().find(|other| {
                other
                    .timeline_range()
                    .is_ok_and(|other_range| new_range.overlaps(other_range))
            }) {
                return Err(DomainError::Overlap {
                    track_id,
                    first: overlap.id.clone(),
                    second: clip_id,
                });
            }
            let mut moved = project.sequence.tracks[source_track_index]
                .clips
                .remove(clip_index);
            moved.timeline_start = timeline_start;
            moved.validate(asset)?;
            project.sequence.tracks[target_track_index]
                .clips
                .push(moved);
        }
        TimelineOperation::TrimClip {
            clip_id,
            source_start,
            source_end,
        } => {
            if source_start < 0 || source_end <= source_start {
                return Err(invalid("trim.source_range", "requires 0 <= start < end"));
            }
            let (track_index, clip_index) = find_clip_position(project, &clip_id)?;
            let track = &mut project.sequence.tracks[track_index];
            if track.locked {
                return Err(DomainError::Locked {
                    entity: "track".to_owned(),
                    id: track.id.to_string(),
                });
            }
            let clip = &mut track.clips[clip_index];
            let old_source_duration = i128::from(clip.source_duration);
            let new_source_duration = i128::from(source_end - source_start);
            let new_timeline_duration =
                i128::from(clip.timeline_duration) * new_source_duration / old_source_duration;
            let new_timeline_duration = i64::try_from(new_timeline_duration)
                .map_err(|_| invalid("trim.timeline_duration", "result exceeds i64"))?;
            if new_timeline_duration <= 0 {
                return Err(invalid(
                    "trim",
                    "source range is too short for one timeline tick",
                ));
            }
            clip.source_start = source_start;
            clip.source_duration = source_end - source_start;
            clip.timeline_duration = new_timeline_duration;
        }
        TimelineOperation::SplitClip {
            clip_id,
            at_timeline_tick,
        } => {
            let (track_index, clip_index) = find_clip_position(project, &clip_id)?;
            let original = project.sequence.tracks[track_index].clips[clip_index].clone();
            let new_id = next_split_id(&original.id, next_revision, &project.sequence.tracks)?;
            let track = &mut project.sequence.tracks[track_index];
            if track.locked {
                return Err(DomainError::Locked {
                    entity: "track".to_owned(),
                    id: track.id.to_string(),
                });
            }
            let original_range = original.timeline_range()?;
            if !original_range.contains(at_timeline_tick) {
                return Err(invalid(
                    "split.at_timeline_tick",
                    "must be inside the clip range",
                ));
            }
            let left_duration = at_timeline_tick - original.timeline_start;
            let source_offset = (i128::from(original.source_duration) * i128::from(left_duration)
                / i128::from(original.timeline_duration)) as i64;
            if source_offset <= 0 || source_offset >= original.source_duration {
                return Err(invalid(
                    "split",
                    "split point cannot be represented by source ticks",
                ));
            }
            let mut left = original.clone();
            left.timeline_duration = left_duration;
            left.source_duration = source_offset;
            left.keyframes
                .retain(|keyframe| keyframe.at_tick < left_duration);
            let mut right = original;
            right.id = new_id;
            right.timeline_start = at_timeline_tick;
            right.timeline_duration -= left_duration;
            right.source_start += source_offset;
            right.source_duration -= source_offset;
            for keyframe in &mut right.keyframes {
                keyframe.at_tick -= left_duration;
            }
            right.keyframes.retain(|keyframe| keyframe.at_tick >= 0);
            track.clips[clip_index] = left;
            track.clips.insert(clip_index + 1, right);
        }
        TimelineOperation::ReorderClip {
            track_id,
            clip_id,
            new_index,
        } => {
            let track = project
                .sequence
                .tracks
                .iter_mut()
                .find(|track| track.id == track_id)
                .ok_or_else(|| DomainError::NotFound {
                    entity: "track".to_owned(),
                    id: track_id.to_string(),
                })?;
            if track.locked {
                return Err(DomainError::Locked {
                    entity: "track".to_owned(),
                    id: track_id.to_string(),
                });
            }
            let old_index = track
                .clips
                .iter()
                .position(|clip| clip.id == clip_id)
                .ok_or_else(|| DomainError::NotFound {
                    entity: "clip".to_owned(),
                    id: clip_id.to_string(),
                })?;
            if new_index >= track.clips.len() {
                return Err(invalid(
                    "reorder.new_index",
                    "must be within the track clip list",
                ));
            }
            let clip = track.clips.remove(old_index);
            track.clips.insert(new_index, clip);
        }
        TimelineOperation::DeleteClip { clip_id } => {
            let (track_index, clip_index) = find_clip_position(project, &clip_id)?;
            let track = &mut project.sequence.tracks[track_index];
            if track.locked {
                return Err(DomainError::Locked {
                    entity: "track".to_owned(),
                    id: track.id.to_string(),
                });
            }
            track.clips.remove(clip_index);
            for marker in &mut project.sequence.markers {
                if marker.clip_id.as_ref() == Some(&clip_id) {
                    marker.clip_id = None;
                }
            }
        }
        TimelineOperation::RippleDelete { track_id, clip_id } => {
            let track = project
                .sequence
                .tracks
                .iter_mut()
                .find(|track| track.id == track_id)
                .ok_or_else(|| DomainError::NotFound {
                    entity: "track".to_owned(),
                    id: track_id.to_string(),
                })?;
            if track.locked {
                return Err(DomainError::Locked {
                    entity: "track".to_owned(),
                    id: track_id.to_string(),
                });
            }
            let index = track
                .clips
                .iter()
                .position(|clip| clip.id == clip_id)
                .ok_or_else(|| DomainError::NotFound {
                    entity: "clip".to_owned(),
                    id: clip_id.to_string(),
                })?;
            let deleted = track.clips.remove(index);
            let deleted_range = deleted.timeline_range()?;
            let shift = deleted_range.duration();
            for clip in &mut track.clips {
                if clip.timeline_start >= deleted_range.end {
                    clip.timeline_start -= shift;
                }
            }
            for marker in &mut project.sequence.markers {
                if marker.clip_id.as_ref() == Some(&clip_id) {
                    marker.clip_id = None;
                }
            }
        }
        TimelineOperation::SetClipEffects { clip_id, effects } => {
            let (track_index, clip_index) = find_clip_position(project, &clip_id)?;
            let track = &mut project.sequence.tracks[track_index];
            if track.locked {
                return Err(DomainError::Locked {
                    entity: "track".to_owned(),
                    id: track.id.to_string(),
                });
            }
            for effect in &effects {
                effect.validate()?;
            }
            let asset = project
                .assets
                .iter()
                .find(|asset| asset.id == track.clips[clip_index].asset_id)
                .ok_or_else(|| DomainError::NotFound {
                    entity: "asset".to_owned(),
                    id: track.clips[clip_index].asset_id.to_string(),
                })?;
            let mut candidate = track.clips[clip_index].clone();
            candidate.effects = effects;
            candidate.validate(asset)?;
            track.clips[clip_index] = candidate;
        }
        TimelineOperation::SetClipVisuals {
            clip_id,
            opacity,
            transform,
        } => {
            let (track_index, clip_index) = find_clip_position(project, &clip_id)?;
            let track = &mut project.sequence.tracks[track_index];
            if track.locked {
                return Err(DomainError::Locked {
                    entity: "track".to_owned(),
                    id: track.id.to_string(),
                });
            }
            let asset = project
                .assets
                .iter()
                .find(|asset| asset.id == track.clips[clip_index].asset_id)
                .ok_or_else(|| DomainError::NotFound {
                    entity: "asset".to_owned(),
                    id: track.clips[clip_index].asset_id.to_string(),
                })?;
            let mut candidate = track.clips[clip_index].clone();
            candidate.opacity = opacity;
            candidate.transform = transform;
            candidate.validate(asset)?;
            track.clips[clip_index] = candidate;
        }
        TimelineOperation::AddMarker { marker } => {
            if project
                .sequence
                .markers
                .iter()
                .any(|existing| existing.id == marker.id)
            {
                return Err(invalid("marker.id", "duplicate marker ID"));
            }
            marker.validate()?;
            if marker
                .clip_id
                .as_ref()
                .is_some_and(|clip_id| project.clip(clip_id).is_none())
            {
                return Err(DomainError::NotFound {
                    entity: "clip".to_owned(),
                    id: marker.clip_id.unwrap().to_string(),
                });
            }
            project.sequence.markers.push(marker);
        }
        TimelineOperation::DeleteMarker { marker_id } => {
            let index = project
                .sequence
                .markers
                .iter()
                .position(|marker| marker.id == marker_id)
                .ok_or_else(|| DomainError::NotFound {
                    entity: "marker".to_owned(),
                    id: marker_id.to_string(),
                })?;
            project.sequence.markers.remove(index);
        }
        TimelineOperation::SetTrackState {
            track_id,
            enabled,
            locked,
        } => {
            let track = project
                .sequence
                .tracks
                .iter_mut()
                .find(|track| track.id == track_id)
                .ok_or_else(|| DomainError::NotFound {
                    entity: "track".to_owned(),
                    id: track_id.to_string(),
                })?;
            if let Some(enabled) = enabled {
                track.enabled = enabled;
            }
            if let Some(locked) = locked {
                track.locked = locked;
            }
        }
        TimelineOperation::SetTrackDucking { track_id, ducking } => {
            let target = project
                .sequence
                .tracks
                .iter()
                .find(|track| track.id == track_id)
                .ok_or_else(|| DomainError::NotFound {
                    entity: "track".to_owned(),
                    id: track_id.to_string(),
                })?;
            if target.locked {
                return Err(DomainError::Locked {
                    entity: "track".to_owned(),
                    id: track_id.to_string(),
                });
            }
            if !matches!(target.kind, TrackKind::Audio) {
                return Err(invalid(
                    "track.ducking",
                    "ducking is only supported on audio tracks",
                ));
            }
            if let Some(ducking) = &ducking {
                ducking.validate()?;
            }
            let mut candidate_tracks = project.sequence.tracks.clone();
            let candidate = candidate_tracks
                .iter_mut()
                .find(|track| track.id == track_id)
                .expect("target track was found above");
            candidate.ducking = ducking.clone();
            validate_ducking_references(&candidate_tracks)?;
            let target = project
                .sequence
                .tracks
                .iter_mut()
                .find(|track| track.id == track_id)
                .expect("target track was found above");
            target.ducking = ducking;
        }
    }
    Ok(())
}

fn find_clip_position(
    project: &ProjectDocument,
    clip_id: &ClipId,
) -> Result<(usize, usize), DomainError> {
    for (track_index, track) in project.sequence.tracks.iter().enumerate() {
        if let Some(clip_index) = track.clips.iter().position(|clip| &clip.id == clip_id) {
            return Ok((track_index, clip_index));
        }
    }
    Err(DomainError::NotFound {
        entity: "clip".to_owned(),
        id: clip_id.to_string(),
    })
}

fn next_split_id(
    original: &ClipId,
    revision: u64,
    tracks: &[Track],
) -> Result<ClipId, DomainError> {
    let suffix = format!("-split-{revision}");
    let prefix_limit = 128usize.saturating_sub(suffix.len());
    let prefix: String = original.as_str().chars().take(prefix_limit).collect();
    let base = format!("{prefix}{suffix}");
    let mut candidate = base.clone();
    let mut suffix = 2;
    while tracks
        .iter()
        .flat_map(|track| &track.clips)
        .any(|clip| clip.id.as_str() == candidate)
    {
        let extra = format!("-{suffix}");
        let prefix_limit = 128usize.saturating_sub(extra.len());
        candidate = format!(
            "{}{}",
            base.chars().take(prefix_limit).collect::<String>(),
            extra
        );
        suffix += 1;
    }
    ClipId::new(candidate)
}

fn ensure_unique<'a, I, T>(values: I, kind: &str) -> Result<(), DomainError>
where
    I: IntoIterator<Item = &'a T>,
    T: Eq + fmt::Display + 'a,
{
    let mut seen = Vec::new();
    for value in values {
        if seen.contains(&value) {
            return Err(invalid(&format!("{kind}.id"), "duplicate ID"));
        }
        seen.push(value);
    }
    Ok(())
}

fn invalid(field: &str, reason: &str) -> DomainError {
    DomainError::InvalidValue {
        field: field.to_owned(),
        reason: reason.to_owned(),
    }
}

fn finite_between(field: &str, value: f32, minimum: f32, maximum: f32) -> Result<(), DomainError> {
    if value.is_finite() && (minimum..=maximum).contains(&value) {
        Ok(())
    } else {
        Err(invalid(
            field,
            "must be finite and within the supported range",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id<T>(constructor: impl FnOnce(String) -> Result<T, DomainError>, value: &str) -> T {
        constructor(value.to_owned()).expect("test ID must be valid")
    }

    fn project_with_track() -> (ProjectDocument, TrackId, AssetId) {
        let project_id = id(ProjectId::new, "project-1");
        let asset_id = id(AssetId::new, "asset-1");
        let track_id = id(TrackId::new, "video-1");
        let mut project = ProjectDocument::create(project_id, "Test project").unwrap();
        project.assets.push(Asset {
            id: asset_id.clone(),
            relative_path: RelativePath::new("media/video.mp4").unwrap(),
            kind: AssetKind::Video,
            fingerprint: Fingerprint {
                size_bytes: 10,
                modified_time: "2026-08-27T00:00:00Z".to_owned(),
                sha256: None,
            },
            probe: None,
            status: AssetStatus::Available,
        });
        project
            .sequence
            .tracks
            .push(Track::new(track_id.clone(), TrackKind::Video, "Video").unwrap());
        project.validate().unwrap();
        (project, track_id, asset_id)
    }

    fn clip(id_value: &str, asset_id: AssetId, start: i64) -> Clip {
        Clip {
            id: id(ClipId::new, id_value),
            asset_id,
            timeline_start: start,
            timeline_duration: 30,
            source_start: 0,
            source_duration: 30,
            speed: Rational::new(1, 1).unwrap(),
            opacity: 1.0,
            transform: Transform::default(),
            effects: Vec::new(),
            keyframes: Vec::new(),
            text_overlay: None,
        }
    }

    #[test]
    fn rational_is_reduced_and_invalid_deserialized_values_fail_validation() {
        assert_eq!(
            Rational::new(30, 60).unwrap(),
            Rational {
                numerator: 1,
                denominator: 2
            }
        );
        assert!(Rational::new(0, 1).is_err());
        let invalid: Rational = serde_json::from_str(r#"{"numerator":2,"denominator":4}"#).unwrap();
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn half_open_ranges_do_not_overlap_at_boundary() {
        let left = TickRange::new(0, 10).unwrap();
        let right = TickRange::new(10, 20).unwrap();
        assert!(!left.overlaps(right));
        assert!(left.contains(9));
        assert!(!left.contains(10));
    }

    #[test]
    fn project_json_round_trip_preserves_canonical_ir() {
        let (project, _, _) = project_with_track();
        let json = serde_json::to_string(&project).unwrap();
        let decoded: ProjectDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, project);
        decoded.validate().unwrap();
    }

    #[test]
    fn validation_rejects_duplicate_ids_unknown_assets_and_overlaps() {
        let (mut project, track_id, asset_id) = project_with_track();
        let first = clip("clip-1", asset_id.clone(), 0);
        let second = clip("clip-2", asset_id, 20);
        project.sequence.tracks[0].clips = vec![first, second];
        assert!(matches!(
            project.validate(),
            Err(DomainError::Overlap { .. })
        ));

        project.sequence.tracks[0].clips[1].asset_id = id(AssetId::new, "missing");
        assert!(
            matches!(project.validate(), Err(DomainError::NotFound { entity, .. }) if entity == "asset")
        );

        let mut duplicate = project_with_track().0;
        let asset = duplicate.assets[0].clone();
        duplicate.assets.push(asset);
        assert!(duplicate.validate().is_err());
        assert_eq!(track_id.as_str(), "video-1");
    }

    #[test]
    fn apply_increments_revision_and_rejects_stale_mutations() {
        let (mut project, track_id, asset_id) = project_with_track();
        let result = project
            .apply_in_place(
                0,
                TimelineOperation::AddClip {
                    track_id: track_id.clone(),
                    clip: clip("clip-1", asset_id, 0),
                },
            )
            .unwrap();
        assert_eq!(project.revision, 1);
        assert_eq!(result.revision, 1);
        assert!(matches!(
            project.apply(
                0,
                TimelineOperation::DeleteClip {
                    clip_id: id(ClipId::new, "clip-1")
                }
            ),
            Err(DomainError::RevisionConflict {
                expected: 0,
                actual: 1
            })
        ));
    }

    #[test]
    fn split_is_deterministic_and_undo_restores_previous_document() {
        let (mut project, track_id, asset_id) = project_with_track();
        project
            .apply_in_place(
                0,
                TimelineOperation::AddClip {
                    track_id: track_id.clone(),
                    clip: clip("clip-1", asset_id, 0),
                },
            )
            .unwrap();
        let result = project
            .apply_in_place(
                1,
                TimelineOperation::SplitClip {
                    clip_id: id(ClipId::new, "clip-1"),
                    at_timeline_tick: 10,
                },
            )
            .unwrap();
        assert_eq!(project.sequence.tracks[0].clips.len(), 2);
        assert_eq!(project.sequence.tracks[0].clips[0].timeline_duration, 10);
        assert_eq!(project.sequence.tracks[0].clips[1].timeline_start, 10);
        let before_undo = project.clone();
        let undone = project.undo(project.revision, &result.undo).unwrap();
        assert_eq!(undone.sequence.tracks[0].clips.len(), 1);
        assert_eq!(undone.sequence.tracks[0].clips[0].id.as_str(), "clip-1");
        assert_eq!(undone.revision, before_undo.revision + 1);
    }

    #[test]
    fn replace_clip_asset_preserves_clip_id_and_is_undoable() {
        let (mut project, track_id, asset_id) = project_with_track();
        let replacement_id = id(AssetId::new, "asset-2");
        project.assets.push(Asset {
            id: replacement_id.clone(),
            relative_path: RelativePath::new("media/replacement.mp4").unwrap(),
            kind: AssetKind::Video,
            fingerprint: project.assets[0].fingerprint.clone(),
            probe: None,
            status: AssetStatus::Missing,
        });
        project
            .apply_in_place(
                0,
                TimelineOperation::AddClip {
                    track_id,
                    clip: clip("clip-1", asset_id, 0),
                },
            )
            .unwrap();
        let result = project
            .apply_in_place(
                1,
                TimelineOperation::ReplaceClipAsset {
                    clip_id: id(ClipId::new, "clip-1"),
                    asset_id: replacement_id.clone(),
                },
            )
            .unwrap();
        assert_eq!(
            project.clip(&id(ClipId::new, "clip-1")).unwrap().asset_id,
            replacement_id
        );
        assert_eq!(project.revision, 2);
        let undone = project.undo(project.revision, &result.undo).unwrap();
        assert_eq!(
            undone
                .clip(&id(ClipId::new, "clip-1"))
                .unwrap()
                .asset_id
                .as_str(),
            "asset-1"
        );
    }

    #[test]
    fn replacement_rejects_locked_track_and_incompatible_asset() {
        let (mut project, track_id, asset_id) = project_with_track();
        let audio_id = id(AssetId::new, "audio-1");
        project.assets.push(Asset {
            id: audio_id.clone(),
            relative_path: RelativePath::new("media/audio.wav").unwrap(),
            kind: AssetKind::Audio,
            fingerprint: project.assets[0].fingerprint.clone(),
            probe: None,
            status: AssetStatus::Available,
        });
        project
            .apply_in_place(
                0,
                TimelineOperation::AddClip {
                    track_id: track_id.clone(),
                    clip: clip("clip-1", asset_id, 0),
                },
            )
            .unwrap();
        assert!(matches!(
            project.apply(
                1,
                TimelineOperation::ReplaceClipAsset {
                    clip_id: id(ClipId::new, "clip-1"),
                    asset_id: audio_id,
                }
            ),
            Err(DomainError::AssetKindMismatch { .. })
        ));
        project.sequence.tracks[0].locked = true;
        assert!(matches!(
            project.apply(
                1,
                TimelineOperation::ReplaceClipAsset {
                    clip_id: id(ClipId::new, "clip-1"),
                    asset_id: id(AssetId::new, "asset-1"),
                }
            ),
            Err(DomainError::Locked { entity, .. }) if entity == "track"
        ));
    }

    #[test]
    fn relink_updates_asset_metadata_without_changing_asset_id() {
        let (mut project, _, asset_id) = project_with_track();
        let result = project
            .apply_in_place(
                0,
                TimelineOperation::RelinkAsset {
                    asset_id: asset_id.clone(),
                    relative_path: RelativePath::new("relocated/video.mp4").unwrap(),
                    fingerprint: Fingerprint {
                        size_bytes: 20,
                        modified_time: "2026-08-27T01:00:00Z".to_owned(),
                        sha256: None,
                    },
                    probe: None,
                    status: AssetStatus::Missing,
                },
            )
            .unwrap();
        assert_eq!(project.assets[0].id, asset_id);
        assert_eq!(
            project.assets[0].relative_path.as_str(),
            "relocated/video.mp4"
        );
        assert_eq!(project.assets[0].status, AssetStatus::Missing);
        let undone = project.undo(project.revision, &result.undo).unwrap();
        assert_eq!(undone.assets[0].relative_path.as_str(), "media/video.mp4");
    }

    #[test]
    fn relink_rejects_locked_references_and_invalid_metadata() {
        let (mut project, track_id, asset_id) = project_with_track();
        project
            .apply_in_place(
                0,
                TimelineOperation::AddClip {
                    track_id,
                    clip: clip("clip-1", asset_id.clone(), 0),
                },
            )
            .unwrap();
        project.sequence.tracks[0].locked = true;
        let operation = TimelineOperation::RelinkAsset {
            asset_id,
            relative_path: RelativePath::new("relocated/video.mp4").unwrap(),
            fingerprint: Fingerprint {
                size_bytes: 10,
                modified_time: "2026-08-27T00:00:00Z".to_owned(),
                sha256: None,
            },
            probe: None,
            status: AssetStatus::Available,
        };
        assert!(matches!(
            project.apply(1, operation),
            Err(DomainError::Locked { entity, .. }) if entity == "track"
        ));
        assert!(RelativePath::new("../video.mp4").is_err());
    }

    #[test]
    fn move_clip_changes_position_preserves_id_and_rejects_overlap() {
        let (mut project, track_id, asset_id) = project_with_track();
        project
            .apply_in_place(
                0,
                TimelineOperation::AddClip {
                    track_id: track_id.clone(),
                    clip: clip("a", asset_id.clone(), 0),
                },
            )
            .unwrap();
        project
            .apply_in_place(
                1,
                TimelineOperation::AddClip {
                    track_id,
                    clip: clip("b", asset_id, 30),
                },
            )
            .unwrap();
        let result = project
            .apply_in_place(
                2,
                TimelineOperation::MoveClip {
                    clip_id: id(ClipId::new, "a"),
                    timeline_start: 60,
                },
            )
            .unwrap();
        assert_eq!(
            project.clip(&id(ClipId::new, "a")).unwrap().timeline_start,
            60
        );
        assert_eq!(
            result
                .document
                .clip(&id(ClipId::new, "a"))
                .unwrap()
                .id
                .as_str(),
            "a"
        );
        assert!(matches!(
            project.apply(
                3,
                TimelineOperation::MoveClip {
                    clip_id: id(ClipId::new, "a"),
                    timeline_start: 30,
                }
            ),
            Err(DomainError::Overlap { .. })
        ));
    }

    #[test]
    fn ripple_delete_shifts_only_later_clips_and_boundary_stays_valid() {
        let (mut project, track_id, asset_id) = project_with_track();
        project
            .apply_in_place(
                0,
                TimelineOperation::AddClip {
                    track_id: track_id.clone(),
                    clip: clip("a", asset_id.clone(), 0),
                },
            )
            .unwrap();
        project
            .apply_in_place(
                1,
                TimelineOperation::AddClip {
                    track_id: track_id.clone(),
                    clip: clip("b", asset_id.clone(), 30),
                },
            )
            .unwrap();
        project
            .apply_in_place(
                2,
                TimelineOperation::AddClip {
                    track_id: track_id.clone(),
                    clip: clip("c", asset_id, 60),
                },
            )
            .unwrap();
        project
            .apply_in_place(
                3,
                TimelineOperation::RippleDelete {
                    track_id,
                    clip_id: id(ClipId::new, "b"),
                },
            )
            .unwrap();
        assert_eq!(project.sequence.tracks[0].clips[0].timeline_start, 0);
        assert_eq!(project.sequence.tracks[0].clips[1].timeline_start, 30);
        project.validate().unwrap();
    }

    #[test]
    fn track_ducking_is_typed_validated_and_backward_compatible() {
        let (mut project, video_id, _) = project_with_track();
        let source_id = id(TrackId::new, "voice");
        let target_id = id(TrackId::new, "music");
        project
            .sequence
            .tracks
            .push(Track::new(source_id.clone(), TrackKind::Audio, "Voice").unwrap());
        project
            .sequence
            .tracks
            .push(Track::new(target_id.clone(), TrackKind::Audio, "Music").unwrap());

        let ducking = DuckingConfig {
            source_track_id: source_id,
            threshold_db: -24.0,
            ratio: 4.0,
            attack_ms: 20.0,
            release_ms: 250.0,
        };
        project
            .apply_in_place(
                0,
                TimelineOperation::SetTrackDucking {
                    track_id: target_id.clone(),
                    ducking: Some(ducking.clone()),
                },
            )
            .unwrap();
        assert_eq!(project.revision, 1);
        assert_eq!(project.sequence.tracks[2].ducking, Some(ducking));

        let mut old_json = serde_json::to_value(&project).unwrap();
        for track in old_json["sequence"]["tracks"]
            .as_array_mut()
            .expect("tracks are serialized as an array")
        {
            track
                .as_object_mut()
                .expect("track is serialized as an object")
                .remove("ducking");
        }
        let decoded: ProjectDocument = serde_json::from_value(old_json).unwrap();
        assert!(decoded
            .sequence
            .tracks
            .iter()
            .all(|track| track.ducking.is_none()));

        assert!(matches!(
            project.apply(
                project.revision,
                TimelineOperation::SetTrackDucking {
                    track_id: target_id.clone(),
                    ducking: Some(DuckingConfig {
                        source_track_id: video_id,
                        threshold_db: -24.0,
                        ratio: 4.0,
                        attack_ms: 20.0,
                        release_ms: 250.0,
                    }),
                },
            ),
            Err(DomainError::InvalidValue { field, .. }) if field == "track.ducking.source_track_id"
        ));
    }

    #[test]
    fn invalid_float_and_effect_parameters_fail_closed() {
        let (mut project, _, _) = project_with_track();
        let mut bad = clip("clip-1", project.assets[0].id.clone(), 0);
        bad.opacity = f32::NAN;
        project.sequence.tracks[0].clips.push(bad);
        assert!(project.validate().is_err());

        let effect = Effect::Crop {
            left: 0.6,
            top: 0.0,
            right: 0.4,
            bottom: 0.0,
        };
        assert!(effect.validate().is_err());
        let speed = Effect::Speed {
            factor: Rational {
                numerator: 0,
                denominator: 1,
            },
            preserve_pitch: false,
        };
        assert!(speed.validate().is_err());
    }

    #[test]
    fn layered_operations_support_overlay_effects_visuals_and_text_tracks() {
        let (mut project, video_track_id, asset_id) = project_with_track();
        let video_clip = clip("clip-1", asset_id.clone(), 0);
        project = project
            .apply(
                0,
                TimelineOperation::AddClip {
                    track_id: video_track_id,
                    clip: video_clip,
                },
            )
            .unwrap()
            .document;

        let overlay_track_id = id(TrackId::new, "overlay-1");
        project = project
            .apply(
                1,
                TimelineOperation::AddTrack {
                    track: Track::new(overlay_track_id.clone(), TrackKind::Overlay, "Overlay")
                        .unwrap(),
                },
            )
            .unwrap()
            .document;
        project = project
            .apply(
                2,
                TimelineOperation::MoveClipToTrack {
                    clip_id: id(ClipId::new, "clip-1"),
                    track_id: overlay_track_id.clone(),
                    timeline_start: 15,
                },
            )
            .unwrap()
            .document;
        project = project
            .apply(
                3,
                TimelineOperation::SetClipEffects {
                    clip_id: id(ClipId::new, "clip-1"),
                    effects: vec![
                        Effect::Exposure { value: 0.5 },
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
                                red: 12,
                                green: 20,
                                blue: 45,
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
                    ],
                },
            )
            .unwrap()
            .document;
        project = project
            .apply(
                4,
                TimelineOperation::SetClipVisuals {
                    clip_id: id(ClipId::new, "clip-1"),
                    opacity: 0.75,
                    transform: Transform {
                        position_x: 0.2,
                        position_y: -0.1,
                        scale_x: 1.1,
                        scale_y: 0.9,
                        rotation_degrees: 8.0,
                        anchor_x: 0.5,
                        anchor_y: 0.5,
                    },
                },
            )
            .unwrap()
            .document;

        let text_asset_id = id(AssetId::new, "title-asset");
        project = project
            .apply(
                5,
                TimelineOperation::AddAsset {
                    asset: Asset {
                        id: text_asset_id.clone(),
                        relative_path: RelativePath::new("generated/title.title").unwrap(),
                        kind: AssetKind::Text,
                        fingerprint: Fingerprint {
                            size_bytes: 5,
                            modified_time: "generated".to_owned(),
                            sha256: None,
                        },
                        probe: None,
                        status: AssetStatus::Available,
                    },
                },
            )
            .unwrap()
            .document;
        let text_track_id = id(TrackId::new, "text-1");
        project = project
            .apply(
                6,
                TimelineOperation::AddTrack {
                    track: Track::new(text_track_id.clone(), TrackKind::Text, "Text").unwrap(),
                },
            )
            .unwrap()
            .document;
        project = project
            .apply(
                7,
                TimelineOperation::AddClip {
                    track_id: text_track_id,
                    clip: Clip {
                        id: id(ClipId::new, "title-clip"),
                        asset_id: text_asset_id,
                        timeline_start: 20,
                        timeline_duration: 60,
                        source_start: 0,
                        source_duration: 60,
                        speed: Rational::new(1, 1).unwrap(),
                        opacity: 1.0,
                        transform: Transform::default(),
                        effects: Vec::new(),
                        keyframes: Vec::new(),
                        text_overlay: Some(TextOverlay {
                            text: "Hello world".to_owned(),
                            font_size: 48.0,
                            color: "#FFFFFF".to_owned(),
                            position_x: 0.0,
                            position_y: 0.8,
                        }),
                    },
                },
            )
            .unwrap()
            .document;

        assert_eq!(project.revision, 8);
        assert_eq!(project.sequence.tracks[1].kind, TrackKind::Overlay);
        assert_eq!(project.sequence.tracks[1].clips[0].opacity, 0.75);
        assert_eq!(project.sequence.tracks[1].clips[0].effects.len(), 10);
        assert!(project
            .sequence
            .tracks
            .iter()
            .find(|track| track.kind == TrackKind::Text)
            .is_some_and(|track| track.clips[0].text_overlay.is_some()));
        project.validate().unwrap();
    }
}
