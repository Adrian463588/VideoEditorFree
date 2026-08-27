export type CapabilityState = "READY" | "BLOCKED" | "UNAVAILABLE";

export type AssetKind = "Video" | "Audio" | "Image" | "Subtitle" | "Text";
export type AssetStatus = "Available" | "Missing" | "Unsupported" | "Invalid";
export type TrackKind = "Video" | "Audio" | "Subtitle" | "Text" | "Overlay";
export type ExportProfile = "youtube" | "instagram" | "tiktok";

export interface Rational {
  numerator: number;
  denominator: number;
}

export interface AssetFingerprint {
  size_bytes: number;
  modified_time: string;
  sha256: string | null;
}

export interface ProbeSummary {
  duration_ticks: number;
  stream_timebase: Rational;
  video: {
    codec: string;
    width: number;
    height: number;
    frame_rate: Rational | null;
  } | null;
  audio: {
    codec: string;
    sample_rate: number;
    channels: number;
  } | null;
  rotation_degrees: number | null;
  raw_tool_version: string;
}

export interface ProjectAsset {
  id: string;
  relative_path: string;
  kind: AssetKind;
  fingerprint: AssetFingerprint;
  probe: ProbeSummary | null;
  status: AssetStatus;
}

export interface ProjectClip {
  id: string;
  asset_id: string;
  timeline_start: number;
  timeline_duration: number;
  source_start: number;
  source_duration: number;
  speed: Rational;
  opacity: number;
  transform: Transform;
  effects: Effect[];
  keyframes: Keyframe[];
  text_overlay: TextOverlay | null;
}

export interface Transform {
  position_x: number;
  position_y: number;
  scale_x: number;
  scale_y: number;
  rotation_degrees: number;
  anchor_x: number;
  anchor_y: number;
}

export type Effect =
  | { Brightness: { value: number } }
  | { Contrast: { value: number } }
  | { Saturation: { value: number } }
  | { Exposure: { value: number } }
  | { Gamma: { value: number } }
  | { Temperature: { kelvin: number } }
  | { Tint: { value: number } }
  | { ColorBalance: { shadows: RgbDelta; midtones: RgbDelta; highlights: RgbDelta } }
  | { Crop: { left: number; top: number; right: number; bottom: number } }
  | { Rotate: { degrees: number } }
  | { Blur: { radius: number } }
  | { Sharpen: { amount: number } }
  | { Vignette: { amount: number } }
  | { Duotone: { shadows: RgbColor; highlights: RgbColor } }
  | { Lut: { relative_path: string } }
  | { Speed: { factor: Rational; preserve_pitch: boolean } }
  | { Volume: { gain_db: number } }
  | { Fade: { kind: "In" | "Out"; duration_ticks: number } };

export interface RgbDelta { red: number; green: number; blue: number }
export interface RgbColor { red: number; green: number; blue: number }
export interface TextOverlay {
  text: string;
  font_size: number;
  color: string;
  position_x: number;
  position_y: number;
}
export type KeyframeProperty = "Opacity" | "PositionX" | "PositionY" | "ScaleX" | "ScaleY" | "Rotation";
export type KeyframeValue = { Scalar: { value: number } } | { Point: { x: number; y: number } };
export interface Keyframe { at_tick: number; property: KeyframeProperty; value: KeyframeValue }

export interface ProjectMarker {
  id: string;
  position_ticks: number;
  name: string;
  comment: string | null;
  color_tag: string | null;
  clip_id: string | null;
}

export interface ProjectTrack {
  id: string;
  kind: TrackKind;
  name: string;
  enabled: boolean;
  locked: boolean;
  clips: ProjectClip[];
  ducking?: TrackDucking | null;
}

export interface TrackDucking {
  source_track_id: string;
  threshold_db: number;
  ratio: number;
  attack_ms: number;
  release_ms: number;
}

export interface ProjectDocument {
  schema_version: number;
  revision: number;
  project_id: string;
  name: string;
  project_root: string;
  assets: ProjectAsset[];
  sequence: {
    timebase: Rational;
    width: number;
    height: number;
    pixel_aspect: Rational;
    audio_sample_rate: number;
    audio_channels: number;
    tracks: ProjectTrack[];
    markers: ProjectMarker[];
  };
}

export interface JobRecord {
  id: string;
  kind: "probe" | "proxy" | "render" | "export" | "stt" | "llm_plan" | "tts" | "generation";
  state: "queued" | "running" | "succeeded" | "failed" | "cancel_requested" | "cancelled";
  snapshot: {
    base_revision: number;
    snapshot_hash: string;
  };
  stage: string;
  progress: number | null;
  message: string;
  output_path: string | null;
  error: AppError | null;
}

export interface AppError {
  code: string;
  message: string;
  retryable: boolean;
  details: Record<string, string> | null;
}

export interface ModelCapability {
  id: string;
  label: string;
  state: CapabilityState;
  reason: string;
}

export interface EditorSnapshot {
  project: ProjectDocument | null;
  jobs: JobRecord[];
  capabilities: {
    mediaRuntime: CapabilityState;
    assistant: ModelCapability;
    subtitles: ModelCapability;
    tts: ModelCapability;
    effects: HostCapabilityStatus;
    audioDucking: HostCapabilityStatus;
    exportProfiles: HostCapabilityStatus;
  };
  connection: CapabilityState;
  connectionMessage: string;
}

export interface HostCapabilityStatus {
  state: CapabilityState;
  reason: string;
}

export interface HostStatus {
  core: HostCapabilityStatus;
  media: HostCapabilityStatus;
  ai: HostCapabilityStatus;
  subtitles?: HostCapabilityStatus;
  tts?: HostCapabilityStatus;
  effects?: HostCapabilityStatus;
  audioDucking?: HostCapabilityStatus;
  exportProfiles?: HostCapabilityStatus;
  projectLoaded: boolean;
}

export const emptyEditorSnapshot: EditorSnapshot = {
  project: null,
  jobs: [],
  capabilities: {
    mediaRuntime: "BLOCKED",
    assistant: {
      id: "local-llm",
      label: "Local edit assistant",
      state: "UNAVAILABLE",
      reason: "No verified local LLM runtime or model is provisioned.",
    },
    subtitles: {
      id: "local-stt",
      label: "Local subtitle generation",
      state: "UNAVAILABLE",
      reason: "The host does not expose a verified subtitle generation capability.",
    },
    tts: {
      id: "local-tts",
      label: "Local voiceover generation",
      state: "UNAVAILABLE",
      reason: "The host does not expose a verified Piper voice runtime.",
    },
    effects: {
      state: "UNAVAILABLE",
      reason: "The host does not expose typed visual effects.",
    },
    audioDucking: {
      state: "UNAVAILABLE",
      reason: "The host does not expose typed audio ducking.",
    },
    exportProfiles: {
      state: "UNAVAILABLE",
      reason: "The host does not expose platform export profiles.",
    },
  },
  connection: "UNAVAILABLE",
  connectionMessage: "UNAVAILABLE — Tauri host is not running. No canonical project is connected.",
};
