# PRD — VideoEditorFree

Status: Draft product contract / implementation baseline  
Date: 2026-08-27  
Target: Windows x64 desktop  
Primary stack: Rust + Tauri 2 + React/TypeScript  
Repository state at authoring: specification-only; root contained `docs/` and no source manifest. Current checkout: Rust crates, Tauri 2/React baseline, typed host IPC, native dialogs, an explicit checksum-verified runtime bundle manifest/downloader, and evidence validators are implemented; local static/unit gates pass. No large runtime/model is committed. Local binary/fixture/model execution and clean-machine/runtime/accessibility evidence remain `BLOCKED` or `UNAVAILABLE` until the downloaded files and target evidence are verified.

## 0. Evidence boundary

This document converts two user-provided attachments into an executable product contract. The attachments are design guidance and repository suggestions, not proof that any feature already works.

Use these labels throughout delivery:

- `Decision`: accepted product or architecture rule.
- `Target`: intended outcome that still needs measurement.
- `BLOCKED`: cannot be evaluated because a prerequisite or artifact is missing.
- `UNAVAILABLE`: optional capability is not provisioned on this machine.
- `Inference`: conclusion derived from references, not directly verified in this checkout.

Implementation is present in the current checkout, but no real export, AI, installer, or release success is claimed. The explicit download path is implemented; real runtime use still requires successful artifact verification, a licensed fixture, and target evidence.

## 1. Product intent

VideoEditorFree is a local-first, non-linear video editor for Windows. Users import local media, arrange clips on a multi-track timeline, preview edits, apply deterministic media operations, save a portable project, and export a playable result. Optional local AI assists transcription, subtitles, and edit planning without silently sending footage or prompts to a cloud service.

“Lengkap” means the product has a complete path from import to export plus separately gated AI and experimental capabilities. It does not mean every advanced capability is a V1 dependency.

## 2. Goals

- Deliver a trustworthy core editor that works without internet access.
- Keep originals immutable and edits non-destructive.
- Make preview, render, and export derive from one versioned timeline document.
- Make long media and AI operations observable, cancellable, and recoverable.
- Package required media/runtime assets explicitly; never depend on an installed `ffmpeg` in `PATH`.
- Add local STT, LLM, TTS, and generative video only after each has a real model, license, resource check, and acceptance evidence.
- Keep the UI keyboard-usable, screen-reader understandable, and clear about missing capabilities.

## 3. Non-goals for V1

- Cloud rendering, telemetry, remote AI APIs, accounts, subscriptions, or social publishing.
- Collaborative editing or multi-user synchronization.
- Android parity; a future mobile target may reuse domain crates after desktop contracts stabilize.
- Generative video as a release gate.
- Vulkan/WebGPU/GStreamer custom compositor before profiling proves the simpler pipeline insufficient.
- Multiple interchangeable FFmpeg, Whisper, LLM, or timeline libraries in one release.

## 4. Users and primary jobs

### Persona P1 — Local creator

Windows creator, beginner to intermediate, needs reliable trimming, ordering, audio, captions, and export without uploading footage.

### Persona P2 — Technical creator

Power user who wants proxy preview, keyboard editing, reproducible projects, hardware acceleration when available, and relinkable media.

### Persona P3 — AI-assisted editor

User with locally provisioned STT/LLM/TTS models who wants transcript search, subtitle generation, or a reviewable natural-language edit plan. AI must remain assistive; user retains control of mutations.

## 5. Functional requirements

### 5.1 P0 — Core editor and safety

| ID | Requirement | Acceptance evidence |
|---|---|---|
| M-01 | Create, open, save, close, and recover a local project. | Project file round-trip, recovery fixture, atomic-save log. |
| M-02 | Import local video, audio, image, and subtitle assets through a file dialog. | Real files with spaces/Unicode; asset fingerprint and status shown. |
| M-03 | Probe imported media and store duration, streams, codec, dimensions, frame/timebase, audio rate, channels, rotation, and errors. | `ffprobe` JSON plus UI result; invalid media has a real error. |
| M-04 | Arrange clips on video/audio/subtitle tracks with stable IDs, position, source range, and duration. | Timeline JSON and UI state match after save/reopen. |
| M-05 | Play, pause, seek, set playhead, and preview a single asset or generated proxy composition. | Real-media playback and seek evidence. |
| M-06 | Cut, trim, split, reorder, delete, and replace/relink clips non-destructively. | Before/after project documents and exported output prove the edit. |
| M-07 | Export a project to a selected local output path using one documented baseline profile. | Output hash, `ffprobe`, decode/playback check, and correct ordering/duration. |
| M-08 | Report failure for unsupported, missing, corrupt, unreadable, cancelled, or incomplete operations. | Negative fixtures; no false-success output. |
| M-09 | Preserve a valid previous export when a later export fails or is cancelled. | Atomic temporary-output test and filesystem evidence. |
| M-10 | Provide visible and accessible job status for probe, proxy, render, and export. | Progress/cancel/error UI plus structured job logs. |
| M-11 | Provide keyboard access for critical flows and a visible focus state. | Keyboard-only recording and accessibility tree evidence. |

### 5.2 P1 — Deterministic editing expansion

| ID | Requirement | Notes |
|---|---|---|
| S-01 | Multi-track video and audio with overlap validation and track lock/visibility. | Bounded track count first; no “unlimited” claim without benchmark. |
| S-02 | Audio volume, pan, fade in/out, mute, background music, and deterministic ducking/sidechain. | Speech-aware ducking is a later AI enhancement. |
| S-03 | Waveform and thumbnail/filmstrip generation with cache invalidation by asset fingerprint. | Derived cache is never project source of truth. |
| S-04 | Brightness, contrast, saturation, rotation, crop, speed, and basic color controls. | Parameters are typed and validated; raw FFmpeg filters are not user input. |
| S-05 | Color grading expansion: `eq`, `colorbalance`, curves/LUT spike after baseline effects. | Subject to output correctness and license review. |
| S-06 | Transitions such as fade/dissolve/wipe and basic compositing. | Each transition must map to a deterministic render operation. |
| S-07 | Text/graphic overlays, subtitle styling, and keyframe-ready property model. | Ship only after timeline IR supports stable property tracks. |
| S-08 | Import, edit, position, style, and burn-in/export SRT/VTT subtitles. | Sidecar and burned-in outputs must be distinct options. |
| S-09 | Undo/redo, autosave, crash recovery, relink media, and project backup. | Every mutation is an undoable operation or a documented non-mutating job. |
| S-10 | One baseline CPU render plus optional hardware decode/encode fallback. | GPU is an optimization, never correctness dependency. |

### 5.3 P2 — Local AI capabilities

| ID | Requirement | Readiness gate |
|---|---|---|
| A-01 | Local Whisper-compatible STT produces editable, timestamped cues. | Real audio, model manifest, checksum, transcript evidence, accuracy protocol. |
| A-02 | Auto-subtitle generation uses the same subtitle IR as manual captions. | No separate hidden caption model or fake empty result. |
| A-03 | Transcript search and transcript-based cuts select real timeline ranges. | Ground-truth fixture, before/after project, undo evidence. |
| A-04 | Local LLM converts natural language into a typed `EditPlan`. | Schema validation, unsupported-command tests, no shell/network access. |
| A-05 | User previews and confirms an `EditPlan`; every applied operation is undoable. | Project revision check and before/after evidence. |
| A-06 | Filler/silence detection and best-take suggestions remain reviewable proposals. | Detection provenance and user approval; no silent deletion. |
| A-07 | Local TTS creates a real audio asset with model/voice provenance. | Output `ffprobe`, playback, model checksum, failure path. |
| A-08 | Model manager supports local import and optional explicit download. | Size, version, SHA-256, license, resource check, `.part` recovery. |

### 5.4 P3 — Experimental capabilities

| ID | Requirement | Release rule |
|---|---|---|
| C-01 | Local generative video creates a reviewed asset from a prompt. | Separate optional pack; never blocks core editor; no cloud fallback. |
| C-02 | Hardware-accelerated preview/compositing using GStreamer, WGPU, Vulkan, or WebGPU. | Add only after a measured bottleneck and platform matrix. |
| C-03 | MCP/agent control exposes safe project operations. | Versioned protocol, least privilege, human confirmation for mutations. |
| C-04 | Android or shared mobile Rust core. | Separate target and acceptance matrix after Windows core. |

### 5.5 Explicitly out of scope

- Any operation that claims success while returning `Ok(())`, `Ok(vec![])`, an empty string, or `NoOp` without a real artifact or explicit no-op result.
- Automatic hidden model downloads, hidden cloud fallback, or API-key requirements.
- Raw shell/filtergraph execution from frontend or LLM output.
- Copying code from reference repositories without license and provenance review.

## 6. User stories and acceptance criteria

### US-001 — Import and build a project

Given a valid local media file, when the user imports it, the backend records a stable asset ID, fingerprint, probe metadata, and availability status. The UI renders a usable asset entry. If the file is moved or unreadable, the status is `Missing` or an explicit error; no placeholder clip is created.

### US-002 — Edit and export

Given two real video clips, when the user trims, splits, reorders, saves, closes, and reopens the project, the timeline remains equivalent. Export produces a playable file with the expected stream order and duration. A failed export cannot replace the last valid export.

### US-003 — Work offline

With network disabled, the user can create/open/save/edit/preview/export the core project. AI is usable offline only when its runtime and model are already bundled or manually imported. A missing model is `UNAVAILABLE`, not a fabricated transcript or response.

### US-004 — Use local AI safely

Given a ready local model, when the user asks for a supported edit, the app displays a structured plan, warnings, affected clips, and base revision. Only after confirmation does Rust validate and apply the allowlisted operations. Ambiguous or unsupported input changes nothing.

### US-005 — Cancel and recover

Given a running export or AI job, when the user cancels or the process is interrupted, the job becomes `Cancelled` or `Failed`, temporary output is removed, the project remains readable, and the UI never presents partial output as completed.

## 7. UX requirements

- Main workspace: media browser, preview monitor, inspector, assistant/model status panel, and timeline.
- Empty states explain the next action; they do not show invented clips, waveform, transcript, or benchmark values.
- Every long task shows stage, progress or indeterminate status, cancel action, and final result.
- Unsupported media, missing model, permission failure, and missing asset states are visible in plain language with recovery action.
- Timeline operations are available by keyboard; drag is an enhancement, not the only interaction.
- Use semantic labels, accessible names/roles/states, focus management, live status announcements, sufficient contrast, and reduced-motion behavior.

## 8. Offline, privacy, and data policy

### Profiles

1. `CORE_OFFLINE`: no network is required or used; deterministic editing works with bundled runtime assets.
2. `AI_OFFLINE`: models are bundled or manually imported; inference and media stay local.
3. `MODEL_PROVISIONING`: explicit user action may download a declared artifact over an allowlisted HTTPS endpoint; the app shows consent, progress, checksum verification, and failure. This is not a hidden first-run requirement.

### Rules

- No telemetry, cloud render, remote inference, or silent network fallback in the core product.
- Store project metadata, caches, logs, and models under documented app/project directories.
- Do not modify original media; write derived files to controlled temporary/cache/output paths.
- Model and binary provenance includes source, version, license, size, SHA-256, target architecture, and runtime requirements.
- Network acceptance requires firewall/DNS/process evidence, not only a static URL scan.

## 9. Media and export contract

- Windows x64 is the first target. Exact minimum Windows/WebView2 version is a F0 decision.
- Initial acceptance uses licensed real fixtures whose source, license, SHA-256, and `ffprobe` JSON are recorded.
- First export profile is one documented MP4 profile, targeted as H.264/AAC subject to FFmpeg codec/license review. WebM, ProRes, image sequences, and additional codecs follow separate gates.
- FFmpeg support breadth is not inferred from its existence. Every advertised input/output format needs a fixture and decode/export test.
- Output is written to a temporary sibling file, probed/validated, then atomically renamed. Input and output paths must not alias.
- Hardware acceleration may select NVENC, AMF, QSV, D3D11VA, or another supported backend only after runtime evidence; CPU remains the reference path.

## 10. Non-functional requirements

| Area | Target / rule | Evidence |
|---|---|---|
| Correctness | Exported media must decode and match expected stream order, duration, and edit boundaries. | `ffprobe`, decode check, golden frames/audio assertions. |
| Responsiveness | UI remains responsive while jobs run; seek/preview targets are baselined on a declared fixture. | Packaged-build trace with repetitions and percentiles. |
| Reliability | Atomic save/export; cancel and crash recovery do not corrupt the last valid project/output. | Fault-injection and recovery logs. |
| Security | Least-privilege Tauri capabilities; no arbitrary frontend shell or path access. | Capability files, contract tests, threat review. |
| Privacy | Core and provisioned AI work without external data egress. | Network-disabled run and process/network capture. |
| Accessibility | Target WCAG 2.2 AA principles for critical flows plus Windows UIA/screen-reader checks. | Keyboard, focus, contrast, and UIA evidence. |
| Portability | Path Unicode/spaces/UNC, missing media, CPU-only fallback, and clean install are supported or clearly rejected. | Windows matrix and diagnostics. |
| Maintainability | Domain and render contracts are versioned; dependencies are pinned and license-reviewed. | Lockfiles, schema migrations, dependency inventory. |

Performance thresholds are `TBD/F0`: they must be agreed before benchmarking, then recorded with hardware, build type, fixture, repetitions, and percentile. One machine or one run is not a universal claim.

## 11. Delivery roadmap and definition of done

| Phase | Vertical slice | Exit criteria |
|---|---|---|
| F0 | Launch, file dialog, probe, single-asset preview, empty project save/load. | Packaged Windows app, real fixture, canonical JSON, no internet required. |
| F1 | Two clips: trim, split, reorder, delete, preview, save/reopen, baseline export. | Real output decodes; duration/order assertions pass; failed export preserves previous output. |
| F2 | Undo/redo, autosave/recovery, relink, progress, cancel, keyboard flow, capability hardening. | Fault-injection, accessibility, and cancellation evidence pass. |
| F3 | Bounded multi-track, audio controls, waveform/thumbnails, effects, transitions, SRT/VTT. | Each operation is typed, previewed/exported, and covered by real-media tests. |
| A1 | Local STT and editable subtitles. | Model gate, real audio, timestamps, provenance, and `UNAVAILABLE` path pass. |
| A2 | Local LLM edit planning and transcript operations. | Typed plan, dry-run, approval, revision check, allowlist, undo, negative tests pass. |
| A3 | Local TTS audio assets. | Model/voice gate, real audio output, placement, playback, export pass. |
| A4 | Experimental local generative video. | Separate capability, resource gate, provenance, cancel, review, and no-cloud proof pass. |
| R | Windows installer and release evidence. | Clean-machine install, bundled resources, offline smoke, license inventory, hashes, and known limitations are complete. |

Mandatory order: scope gate, media gate, state/IR gate, persistence gate, then optional AI gates. No AI milestone can mask an unproven deterministic core.

## 12. Acceptance status semantics

- `PASS`: gate ran with required evidence and all assertions passed.
- `FAIL`: gate ran and an assertion failed.
- `BLOCKED`: prerequisite/artifact/device/tool is missing; no success claim allowed.
- `UNAVAILABLE`: optional capability is not provisioned; core remains usable.

Current repository status: Rust crates, Tauri 2/React baseline, typed host IPC, and evidence validators are implemented; local static/unit gates pass. Real `ffmpeg.exe`/`ffprobe.exe` binaries, licensed media fixtures, AI models/providers, installer, clean-machine/runtime/accessibility evidence remain `BLOCKED` or `UNAVAILABLE`. No real export or AI success claim is made.

## 13. Risks and open decisions

| Risk / decision | Default used here | Required resolution |
|---|---|---|
| Meaning of offline | Core offline; explicit model provisioning. | Confirm whether bundled models are required for release. |
| FFmpeg integration | One Rust process adapter around bundled binaries. | Choose exact binary build, codecs, license profile, and packaging. |
| Project storage | Canonical versioned JSON; SQLite only for index/cache if needed. | Add SQLite only after a measured catalog/history need. |
| Preview | Direct single-asset playback, then generated proxy composition. | Profile before adopting GStreamer/WebGPU/Vulkan. |
| AI runtime | Adapter contracts; Whisper first, LLM/TTS later. | Select model/runtime by Windows build and license spike. |
| Media acceptance | Licensed fixtures + one initial export profile. | Define exact formats, duration/size/resource limits in F0. |
| Android | Future separate target. | Do not share release gates until desktop contracts stabilize. |
| MCP/agent control | Optional post-core capability. | Version protocol and confirmation policy before exposure. |

## 14. Source and reference register

### Official technical sources

- [Tauri security and trust boundaries](https://v2.tauri.app/security/)
- [Tauri capabilities](https://v2.tauri.app/security/capabilities/)
- [Tauri commands and frontend IPC](https://v2.tauri.app/develop/calling-rust/)
- [Tauri filesystem scopes](https://v2.tauri.app/plugin/file-system/)
- [Tauri dialog plugin](https://v2.tauri.app/plugin/dialog/)
- [Tauri sidecar packaging](https://v2.tauri.app/develop/sidecar/)
- [FFmpeg documentation](https://ffmpeg.org/ffmpeg.html)
- [FFmpeg filters](https://ffmpeg.org/ffmpeg-filters.html)
- [FFmpeg legal considerations](https://ffmpeg.org/legal.html)
- [Rust Edition Guide](https://doc.rust-lang.org/edition-guide/editions/)
- [React versions](https://react.dev/versions)
- [whisper.cpp](https://github.com/ggml-org/whisper.cpp)
- [llama.cpp](https://github.com/ggml-org/llama.cpp)
- [Microsoft ONNX Runtime Rust bindings](https://github.com/microsoft/onnxruntime/tree/main/rust)
- [GStreamer](https://gstreamer.freedesktop.org/documentation/)
- [Model Context Protocol specification](https://modelcontextprotocol.io/specification/latest)

### Verified reference repositories

These repositories inform patterns only. Their README claims, licenses, build health, and current branches must be rechecked before adopting code or dependencies.

- [Clypra](https://github.com/AIEraDev/Clypra) — Tauri/React/Rust editor patterns; MIT repository page. The first attachment used a different owner URL; use this verified URL.
- [Tazama](https://github.com/MacCracken/tazama) — Rust/GStreamer/Vulkan/editor decomposition; repository page lists AGPL-3.0.
- [RoughCut](https://github.com/PaulBratslavsky/roughcut-ai-local-first-editor) — local STT/transcript workflow; repository page lists GPL-3.0.
- [OpenCut](https://github.com/OpenCut-app/OpenCut) — Rust-core/plugin/headless direction; repository page describes the rewrite as in progress and lists MIT.
- [VibeClip](https://github.com/oktaydbk54/vibeclip) — chat-driven short-form workflow; repository page lists AGPL-3.0 and BYOK/cloud-compatible modes, so it is not an offline dependency.
- [Pireel](https://github.com/pireel/pireel) — canvas/chat/agent workflow; repository page lists AGPL-3.0-only.
- [Gausian Native Editor](https://github.com/gausian-AI/Gausian_native_editor) — local GPU/generative-video ideas; reference only pending license/build review.
- [Turbo](https://github.com/MikuroXina/turbo) — minimal Tauri/WebGL direction; reference only.
- [OpenReel Video](https://github.com/Augani/openreel-video) — browser WebCodecs/WebGPU patterns; not the Windows Rust runtime.
- [SlateCut](https://github.com/Zambrini/slatecut) — transcript/local-rendering ideas; reference only pending maturity review.
- [MobileFFmpeg](https://github.com/tanersener/mobile-ffmpeg) — historical mobile packaging reference; its repository says it is not maintained, so do not use as a new dependency.

Research snapshot date: 2026-08-27. URLs and repository status can drift; lock exact versions, commits, checksums, and licenses during implementation.
