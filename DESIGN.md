# DESIGN — VideoEditorFree

Status: Architecture baseline  
Date: 2026-08-27  
Related contract: `PRD.md`  
Current state: Rust crates, Tauri 2/React baseline, typed host IPC, native dialogs, explicit checksum-verified runtime provisioning, and evidence validators are implemented; local static/unit gates pass. Large runtime/model files are intentionally outside Git. Real export, AI inference, installer, clean-machine/runtime/accessibility evidence remain unclaimed until the installed artifacts and target evidence pass.

## 1. Design goals

- Keep Rust responsible for canonical state, validation, media orchestration, jobs, and persistence.
- Keep React responsible for presentation and transient UI state.
- Make one versioned project IR drive preview planning, render planning, export, undo, and recovery.
- Make optional runtimes replaceable without infecting the domain model.
- Fail closed on missing files, models, permissions, codecs, and cancellation.
- Prefer the smallest implementation that proves a vertical slice. Add GPU, GStreamer, WebGPU, SQLite, or extra bindings only after evidence requires them.

## 2. System shape

```text
┌──────────────────────────────────────────────────────────────────┐
│ React + TypeScript UI                                            │
│ features / timeline / preview / inspector / jobs / assistant     │
│ Zustand = UI projection; typed API facade = only IPC entry       │
└───────────────────────────────┬──────────────────────────────────┘
                                │ Tauri commands + typed job channel
┌───────────────────────────────▼──────────────────────────────────┐
│ Tauri 2 host                                                     │
│ capabilities / dialogs / scoped filesystem / window lifecycle   │
└───────────────────────────────┬──────────────────────────────────┘
                                │
┌───────────────────────────────▼──────────────────────────────────┐
│ Rust application orchestration                                   │
│ use cases / AppState / JobManager / error envelope              │
└───────────────┬────────────────┬────────────────┬────────────────┘
                │                │                │
┌───────────────▼─────┐ ┌────────▼─────────┐ ┌────▼───────────────┐
│ editor-domain        │ │ editor-media     │ │ editor-ai          │
│ IR / validation      │ │ probe / render   │ │ manifests/adapters │
│ pure Rust, no I/O    │ │ FFmpeg process   │ │ optional workers   │
└───────────────┬─────┘ └────────┬─────────┘ └────┬───────────────┘
                │                │                │
         JSON project       ffmpeg/ffprobe       whisper/llama/TTS
         + optional DB      bundled binaries      optional model packs
```

Use a modular monolith. The desktop is single-user and local; microservices add deployment and failure modes without solving a V1 problem.

## 3. Planned repository layout

```text
VideoEditorFree/
├── AGENTS.md
├── PRD.md
├── DESIGN.md
├── Cargo.toml                         # workspace
├── package.json                       # frontend/Tauri scripts
├── package-lock.json
├── apps/
│   └── desktop/
│       ├── src/                       # React/TypeScript
│       │   ├── api/                   # typed invoke/channel facade
│       │   ├── components/
│       │   ├── features/
│       │   │   ├── project/
│       │   │   ├── media/
│       │   │   ├── timeline/
│       │   │   ├── preview/
│       │   │   ├── jobs/
│       │   │   └── assistant/
│       │   ├── stores/                # UI state only
│       │   └── types/                 # generated/shared IPC types
│       └── src-tauri/
│           ├── src/
│           │   ├── commands.rs
│           │   ├── capabilities/
│           │   └── lib.rs
│           ├── binaries/              # target-suffixed sidecars
│           └── tauri.conf.json
├── crates/
│   ├── editor-domain/                 # pure IR and validation
│   ├── editor-project/                # JSON, migration, paths
│   ├── editor-media/                 # probe, render plan, FFmpeg adapter
│   ├── editor-jobs/                  # queue, progress, cancellation
│   ├── editor-ai/                    # model manifest and adapters
│   └── editor-app/                   # use cases and ports
├── resources/
│   ├── ffmpeg/
│   ├── models/
│   └── licenses/
├── fixtures/
│   ├── media-manifest.json
│   └── expected/
├── scripts/
└── docs/
    ├── adr/
    └── release/
```

The exact folder split may shrink during F0. Do not create empty crates “for later”; create a boundary when a vertical slice needs it.

## 4. Dependency direction

```text
React components
  └── frontend API facade
        └── Tauri commands
              └── editor-app use cases
                    ├── editor-domain
                    ├── editor-project
                    ├── editor-media
                    ├── editor-jobs
                    └── editor-ai

editor-domain ──> no project/media/AI/UI dependency
editor-media  ──> domain + process/filesystem port
editor-ai     ──> domain + worker/model ports
editor-project ─> domain
```

Rules:

- Domain types must compile and test without Tauri, FFmpeg, SQLite, GPU, or model files.
- Tauri commands translate transport DTOs into use-case inputs; business logic does not live in command functions.
- Components must not call raw `invoke` directly; the typed facade centralizes command names and errors.
- `editor-media` owns command construction and output validation; no frontend-supplied filtergraph or executable path.
- `editor-ai` owns model readiness and provider adapters; it cannot mutate a project without an application use case.

## 5. Canonical project and timeline IR

The project JSON is the portable source of truth. SQLite, if added, stores catalog/cache/job metadata only and never duplicates the timeline.

### 5.1 Core model

```rust
ProjectDocument {
    schema_version: u32,
    revision: u64,
    project_id: ProjectId,
    name: String,
    project_root: RelativePath,
    assets: Vec<Asset>,
    sequence: Sequence,
}

Sequence {
    timebase: Rational,              // sequence ticks per second
    width: u32,
    height: u32,
    pixel_aspect: Rational,
    audio_sample_rate: u32,
    audio_channels: u16,
    tracks: Vec<Track>,
}

Track {
    id: TrackId,
    kind: Video | Audio | Subtitle,
    name: String,
    enabled: bool,
    locked: bool,
    clips: Vec<Clip>,
}

Clip {
    id: ClipId,
    asset_id: AssetId,
    timeline_start: i64,             // sequence ticks
    timeline_duration: i64,          // positive; [start, start + duration)
    source_start: i64,               // asset stream ticks
    source_duration: i64,
    speed: Rational,
    opacity: f32,
    transform: Transform,
    effects: Vec<Effect>,
    keyframes: Vec<Keyframe>,
}
```

`Time` is never persisted as a floating-point second or as a millisecond-only value. Sequence positions use integer ticks; source positions retain the asset stream timebase; speed and ratios are reduced positive rationals. All intervals are half-open `[in, out)`.

Color/opacity/transform values may use bounded finite numeric values. Validate NaN, infinity, out-of-range values, negative durations, zero denominators, duplicate IDs, overlaps where forbidden, and references to unknown assets.

### 5.2 Asset metadata

```rust
Asset {
    id: AssetId,
    relative_path: String,
    kind: Video | Audio | Image | Subtitle,
    fingerprint: {
        size_bytes: u64,
        modified_time: String,
        sha256: Option<String>,
    },
    probe: Option<ProbeSummary>,
    status: Available | Missing | Unsupported | Invalid,
}

ProbeSummary {
    duration_ticks: i64,
    stream_timebase: Rational,
    video: Option<VideoStream>,
    audio: Option<AudioStream>,
    rotation_degrees: Option<i16>,
    raw_tool_version: String,
}
```

Paths are relative to the project root when possible. Import may reference an external file, copy it into a controlled project asset directory, or ask the user to choose; the policy must be explicit. Asset IDs, not arbitrary paths, appear in edit operations.

### 5.3 Typed effects

```rust
enum Effect {
    Brightness { value: f32 },
    Contrast { value: f32 },
    Saturation { value: f32 },
    ColorBalance { shadows: RgbDelta, midtones: RgbDelta, highlights: RgbDelta },
    Crop { left: f32, top: f32, right: f32, bottom: f32 },
    Rotate { degrees: i16 },
    Speed { factor: Rational, preserve_pitch: bool },
    Volume { gain_db: f32 },
    Fade { kind: FadeKind, duration_ticks: i64 },
}
```

The IR never accepts raw FFmpeg syntax. A typed render builder is the only translation path to a filtergraph.

## 6. Media pipeline

### 6.1 Import and probe

1. Frontend opens a Tauri dialog and receives a selected path.
2. Backend canonicalizes and validates the path against the allowed import/project scope.
3. Backend assigns `AssetId`, fingerprints the file, and runs bundled `ffprobe` with a fixed argument list.
4. Parsed metadata is validated and stored in the project document.
5. UI receives `AssetStatus` and a `JobId`/result; failures retain the project and show recovery.

Do not assume every FFmpeg-readable format is product-supported. Add a format to the advertised matrix only after a real fixture passes probe, preview, render, and export/decode checks.

### 6.2 Preview strategy

- F0: play a single available asset through a controlled Tauri asset URL or project-scoped path.
- F1: compose multi-clip previews by generating a low-resolution proxy from the same IR.
- F2/F3: cache thumbnails, filmstrips, waveforms, and proxy segments by asset fingerprint plus render settings.
- Use the export/render plan as correctness source of truth. Preview may be approximate and must display its proxy/quality state.
- Consider WebCodecs/WebGPU, GStreamer, WGPU, or Vulkan only after profiling identifies a preview bottleneck.

### 6.3 Render and export

```text
ProjectDocument
    │ validate + snapshot revision
    ▼
RenderPlan (deterministic, typed)
    │ resolve AssetId -> allowed path
    ▼
FFmpeg argv + typed filtergraph + stream map
    │ spawn bundled process, -progress pipe:1, -nostdin
    ▼
temporary sibling output
    │ probe + decode check + cancellation check
    ▼
atomic rename to user-selected output
```

`MediaExecutor` requirements:

- Resolve executable from bundled/resource manifest, not `PATH`.
- Use `std::process::Command` or `tokio::process::Command` with separate arguments.
- Keep input/output paths distinct and reject output inside protected source paths if policy requires.
- Parse machine-readable progress; throttle UI updates to a bounded rate.
- Capture stdout/stderr with job ID and redaction policy.
- On cancellation, terminate the child, wait for it, remove temporary output, and report `Cancelled`.
- Verify exit status, output existence, non-zero size, probe metadata, and decodability before `Succeeded`.
- Preserve the last valid output if the current job fails.

FFmpeg sidecar packaging may use Tauri `externalBin`, but automatic runtime download is forbidden in the core path. If a wrapper crate is selected, it must not bypass the bundled-binary manifest, argument validation, cancellation, or license inventory.

## 7. Job system

```text
Queued -> Running -> Succeeded
                 ├-> Failed
                 └-> CancelRequested -> Cancelled
```

```rust
JobRecord {
    id: JobId,
    kind: Probe | Proxy | Render | Export | STT | LLMPlan | TTS | Generation,
    project_revision: u64,
    snapshot_hash: String,
    stage: String,
    progress: Option<f32>,          // None = indeterminate
    message: String,
    output_path: Option<RelativePath>,
    error: Option<AppError>,
}
```

Rules:

- Snapshot the validated IR and base revision when a job starts.
- Reject or re-plan a mutation when the current project revision differs from the plan revision.
- Use bounded concurrency; media and model workers must not load every model simultaneously.
- A process worker is preferred for runtimes that cannot interrupt inference safely.
- Persist enough job state to explain failure/recovery, not raw private media content.

## 8. IPC contract

Components call a generated/centralized API facade. Names may be snake_case in Rust and camelCase in TypeScript, but mapping is explicit.

### Commands

| Command | Input | Result |
|---|---|---|
| `project_create` | name, sequence settings | `ProjectSummary` |
| `project_open` | selected project path | `ProjectDocument` or `AppError` |
| `project_save` | project revision/document | save result + hash |
| `asset_import` | selected paths | asset IDs + probe job IDs |
| `asset_relink` | asset ID + selected path | updated asset status |
| `timeline_apply` | base revision + typed operation | new project revision |
| `preview_build` | project revision + quality | `JobId` |
| `export_start` | project revision + export profile + destination | `JobId` |
| `job_get` | job ID | `JobRecord` |
| `job_cancel` | job ID | cancellation result |
| `model_list` | none | manifest/status list |
| `model_import` | selected model path | model status |
| `assistant_plan` | base revision + user text | validated `EditPlan` or error |
| `assistant_apply` | confirmed plan ID | new project revision |

Progress uses a typed Tauri channel or a small event facade. Large media, transcripts, and logs are fetched by ID/page rather than pushed as unbounded event payloads. Every event carries `job_id`, stage, status, and timestamp. An error envelope is stable:

```json
{
  "code": "MEDIA_PROBE_FAILED",
  "message": "The selected file could not be read.",
  "retryable": false,
  "details": null
}
```

Do not leak full local paths, model prompts, or command lines into user-facing errors unless the user explicitly opens diagnostics.

## 9. Persistence and recovery

### Canonical JSON

- `.vdeproj` contains `schema_version`, `revision`, assets, sequence, and user-visible settings.
- Save to `project.tmp`, flush/close, validate by reopening, then atomically replace the target.
- Keep one recoverable backup/autosave according to documented retention.
- On open, migrate older schemas in memory, validate, and write only after user-confirmed save.
- Missing assets remain in the document with status; opening does not silently delete clips.

### Optional SQLite

Use SQLite only for recent-project catalog, derived-cache index, or job history after a measured need. If used, enable transactions/WAL deliberately and keep the JSON document authoritative. Never maintain timeline state independently in JSON and SQLite.

## 10. AI and model architecture

### 10.1 Provider boundary

```rust
trait SpeechToText {
    fn transcribe(&self, audio: AudioInput, options: SttOptions) -> JobResult<Transcript>;
}

trait LanguageModel {
    fn plan_edits(&self, context: ProjectContext, text: String) -> JobResult<EditPlan>;
}

trait TextToSpeech {
    fn synthesize(&self, text: String, voice: VoiceId) -> JobResult<AudioAsset>
}
```

Adapters may use a bundled child process (`whisper-cli`, `llama`/`llama-server`, Piper-compatible runtime) or validated Rust bindings. Do not commit to `rsmpeg` + `ez-ffmpeg` + `ffmpeg-sidecar`, or to several competing LLM/Whisper crates, before a Windows spike proves one path.

### 10.2 Model manifest

```json
{
  "id": "whisper-base",
  "version": "provider-version",
  "format": "ggml|gguf|onnx",
  "runtime": "whisper.cpp|llama.cpp|onnxruntime|piper",
  "artifact": "relative/path/model.bin",
  "source": "https://verified-source.example/model",
  "license": "license-id-and-notice-path",
  "size_bytes": 0,
  "sha256": "hex",
  "capabilities": ["stt"],
  "requirements": { "ram_mb": 0, "vram_mb": 0, "architecture": "x86_64" }
}
```

Required states: `Missing`, `Downloading`, `Verifying`, `Ready`, `Failed`, `Incompatible`, `UNAVAILABLE`.

Provisioning rules:

- Manual import is always available for supported formats.
- Optional download is explicit, resumable, written to `.part`, checksum-verified, and atomically renamed.
- Load models lazily; unload or isolate heavy runtimes.
- No model is “ready” from filename alone.
- A model output must carry provider/model version in diagnostics and provenance.

### 10.3 LLM safety

The LLM receives bounded project context and may return only a schema-validated `EditPlan`:

```json
{
  "base_revision": 12,
  "operations": [
    { "op": "trim", "clip_id": "clip-1", "source_start": 120, "source_end": 480 }
  ],
  "warnings": [],
  "requires_confirmation": true
}
```

The executor validates IDs, ranges, permissions, resource cost, and operation allowlist. Natural-language response markers such as `EDIT:` are not a protocol. LLMs never execute shell, choose arbitrary paths, emit raw filtergraphs, or bypass confirmation.

## 11. Tauri security model

- One main capability for the editor window; add permissions deliberately per feature.
- Scope filesystem access to app data, project root, cache, selected imports, and selected export destination.
- Do not grant broad `fs:read-all`, `fs:write-all`, or frontend shell execution by default.
- If a sidecar is needed, allow only its declared binary and use backend-owned argv construction.
- Use a strict CSP and release configuration without development-only tooling.
- Validate all paths after canonicalization; defend against traversal, symlink surprises, output aliasing, and path injection.
- Treat project JSON, subtitle text, transcript content, and imported media metadata as untrusted input.
- Redact secrets and unnecessary local paths from logs.

Threats and controls:

| Threat | Control |
|---|---|
| Malicious project path | Canonicalize, scope-check, reject traversal, use asset IDs. |
| Malformed media/parser crash | Isolate FFmpeg process, timeout/cancel, validate output. |
| LLM prompt injection in transcript | Treat transcript as data; typed plan, allowlist, confirmation. |
| Arbitrary command execution | No frontend shell permission; backend fixed executable and argv. |
| Partial/corrupt save | temp file, reopen validation, atomic replace, backup. |
| Unwanted egress | Offline profile, explicit allowlist, network capture acceptance. |

## 12. Frontend design

- `stores/` holds selection, playhead, zoom, panel visibility, optimistic job display, and accessibility announcements.
- Rust/project state is reloaded after successful mutations; optimistic updates cannot become a second source of truth.
- Timeline renders from normalized `ProjectDocument` projection and keeps drag/keyboard interaction as commands.
- Preview component owns playback lifecycle but asks Rust for asset/proxy URLs and render jobs.
- Inspector edits typed properties with range validation before `timeline_apply`.
- Assistant shows model readiness, plan diff, affected clips, warnings, confirmation, and undo.
- Model panel shows `UNAVAILABLE` with manual import/download action and reason.

## 13. Accessibility contract

- Every actionable control has accessible name, role, state, keyboard path, and visible focus.
- Export/probe/model status is announced through a live region and is not conveyed by color alone.
- Timeline supports select, move, split, delete, seek, track lock, and undo without drag-only interaction.
- Dialogs trap/restore focus correctly and provide cancel/escape behavior.
- Respect reduced-motion preference; do not use animation as the only state cue.
- Verify with keyboard-only flow, Windows UI Automation/accessibility tree, screen reader, contrast, zoom, and resize evidence.

## 14. Performance and hardware strategy

Baseline order:

1. Correct CPU render/export on licensed fixtures.
2. Background jobs with bounded memory and proxy/thumbnail caches.
3. Measure packaged preview, seek, export, and memory behavior.
4. Add hardware decode/encode as capability with CPU fallback.
5. Add custom GPU compositor only if traces show the current render path is the bottleneck.

FFmpeg’s advertised hardware backends are not proof of runtime acceleration. Acceptance requires actual process logs plus GPU/driver evidence and CPU/GPU correctness comparison. A CPU fallback passing does not prove hardware support.

## 15. Verification design

### Static

- Rust format, compile, Clippy, unit tests; TypeScript lint/typecheck/build.
- Dependency lock and license inventory.
- Search for `TODO`, `unimplemented!`, empty success values, `NoOp`, fake executors, and hard-coded responses.
- Verify Tauri commands, capabilities, CSP, schemas, path validation, and generated API parity.

### Domain/media

- Parameterized IR validation: empty, boundary, negative, overlap, duplicate ID, missing asset, bad timebase.
- JSON round-trip and schema migration.
- Real FFmpeg probe/trim/split/merge/effect/audio/subtitle/export fixtures.
- `ffprobe` before/after, output SHA-256, decodability, duration/stream/frame assertions.
- Corrupt/unsupported/permission-denied/missing-input tests.

### Runtime

- Tauri dev launch and packaged clean-machine install.
- Unicode/spaces/UNC paths, project relocation, cancel/recovery, and previous-output preservation.
- Network-disabled core run and process/network capture.
- Model load/inference with real model identity; missing/corrupt model must show `UNAVAILABLE`.
- Hardware trace, CPU fallback, and output parity.
- Keyboard, screen reader/UIA, focus, contrast, zoom, and reduced-motion checks.

Mocks may test UI fault states and boundary failures. Final acceptance cannot rely only on fake FFmpeg, fake model, fake IPC, fake browser, or fake media.

## 16. Architecture decisions

| ADR | Decision | Reason / revisit trigger |
|---|---|---|
| ADR-001 | Modular monolith with Rust workspace crates. | Fits single-user desktop; revisit only for measured process isolation or mobile reuse. |
| ADR-002 | Canonical versioned JSON project document. | Portable, inspectable, easy backup; migrate schema as needed. |
| ADR-003 | Rust-owned `ProjectDocument` and typed operations. | Prevents React/DB/render drift. |
| ADR-004 | One bundled FFmpeg/ffprobe process adapter. | Windows packaging and cancellation are simpler than several FFI stacks. Revisit after a measured bottleneck. |
| ADR-005 | Direct single-asset preview, then proxy composition. | Proves core quickly; custom compositor requires evidence. |
| ADR-006 | Job IDs, snapshots, progress, cancel, atomic output. | Long media/AI work must not block UI or corrupt data. |
| ADR-007 | AI providers optional and schema-constrained. | Core works without models; LLM cannot mutate or execute arbitrary commands. |
| ADR-008 | CPU render is reference; hardware is optional. | Correctness and portability precede optimization. |
| ADR-009 | SQLite is cache/catalog only, if needed. | Avoids two sources of timeline truth. |
| ADR-010 | MCP, Android, and generative video are post-core capabilities. | Each requires separate security, packaging, model, and runtime gates. |

## 17. Dependency and license policy

- Pin exact versions in lockfiles after a clean Windows build; do not copy version numbers from unverified snippets.
- Prefer standard library/process APIs where they reduce native build risk.
- Review every crate, bundled binary, codec, model, voice, fixture, and reference repository license before distribution.
- FFmpeg builds may include LGPL and optional GPL/nonfree components; selected build flags, dynamic/static linking, notices, source offer, and codec obligations must be recorded. Do not claim legal compliance from a crate name.
- Reference repositories with GPL/AGPL licenses are pattern sources unless a separate compatibility decision approves code reuse.
- User media and model files are not committed to the repository; fixtures need explicit provenance and redistribution permission.

## 18. Research notes and source links

- [Tauri security](https://v2.tauri.app/security/) and [capabilities](https://v2.tauri.app/security/capabilities/) support least-privilege IPC and scoped permissions.
- [Tauri filesystem scopes](https://v2.tauri.app/plugin/file-system/) document path scopes and traversal protections; [dialog](https://v2.tauri.app/plugin/dialog/) provides file selection.
- [Tauri sidecars](https://v2.tauri.app/develop/sidecar/) document target-suffixed external binaries and explicit shell permissions.
- [FFmpeg documentation](https://ffmpeg.org/ffmpeg.html) and [filters](https://ffmpeg.org/ffmpeg-filters.html) support probe/filter/hardware planning; [legal guidance](https://ffmpeg.org/legal.html) controls packaging claims.
- [whisper.cpp](https://github.com/ggml-org/whisper.cpp) documents Windows support, local inference, model formats, quantization, VAD, and optional backends.
- [llama.cpp](https://github.com/ggml-org/llama.cpp) documents local GGUF inference, quantization, `llama serve`, and hardware backends.
- [GStreamer](https://gstreamer.freedesktop.org/documentation/) and [gstreamer-rs](https://gstreamer.freedesktop.org/documentation/rust/) remain future alternatives, not V1 dependencies.
- [Microsoft ONNX Runtime Rust bindings](https://github.com/microsoft/onnxruntime/tree/main/rust) are an optional reference; runtime packaging must be tested on Windows.
- [MCP specification](https://modelcontextprotocol.io/specification/latest) informs a future tool surface; its human-consent and capability rules apply before exposing mutations.
- Reference implementations: [Clypra](https://github.com/AIEraDev/Clypra), [Tazama](https://github.com/MacCracken/tazama), [RoughCut](https://github.com/PaulBratslavsky/roughcut-ai-local-first-editor), [OpenCut](https://github.com/OpenCut-app/OpenCut), [OpenReel](https://github.com/Augani/openreel-video), [VibeClip](https://github.com/oktaydbk54/vibeclip), [Pireel](https://github.com/pireel/pireel), [SlateCut](https://github.com/Zambrini/slatecut), [Gausian](https://github.com/gausian-AI/Gausian_native_editor), and [Turbo](https://github.com/MikuroXina/turbo).

Research snapshot: 2026-08-27. Recheck URLs, branches, licenses, versions, and runtime behavior during implementation; links are not proof of feature readiness.
