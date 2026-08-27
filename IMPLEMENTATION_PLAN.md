# Implementation Plan — VideoEditorFree

Status: implementation baseline and remaining work  
Date: 2026-08-27  
Target: Windows x64 desktop, Rust core, Tauri 2, React + TypeScript

## 1. Baseline and rules

Source read before planning:

- `AGENTS.md`
- `PRD.md`
- `DESIGN.md`
- `C:\Users\HP OMEN\.codex\attachments\1954acb4-2495-4c1e-adf3-e2a1270e7a67\pasted-text.txt`

Current checkout contains implemented Rust domain, persistence, media, jobs, and AI-policy crates; a Tauri host with typed IPC and native dialogs; a React/TypeScript UI; an explicit checksum-verified runtime bundle path; and evidence validators. Local cargo/npm static and unit gates pass. Large runtime/model files are not committed. Real-media execution, AI inference, native preview, installer, clean-machine/accessibility acceptance, and release acceptance remain unclaimed until the downloaded artifacts and target evidence pass. This file preserves planned work and does not claim release or runtime acceptance.

Decision precedence: `PRD.md` and `DESIGN.md` are the current product and architecture contracts. The attachment is reference input only. Therefore:

- V1 is Windows desktop. Android/UniFFI is post-core, not a V1 dependency.
- CPU FFmpeg is the correctness baseline. GPU, WGPU/WebGPU, Vulkan, and GStreamer require measured bottleneck evidence first.
- Canonical state is versioned `ProjectDocument` JSON owned by Rust. Zustand is UI projection only.
- Core works without internet. AI model download is explicit, resumable, checksum-verified, and optional.
- No fabricated source code, specification edit, fake fixture, fake model, hidden cloud fallback, raw frontend shell, or raw LLM filtergraph is allowed.
- A missing required prerequisite is `BLOCKED`. A missing optional provisioned capability is `UNAVAILABLE`. Neither may be reported as success.

## 2. Delivery gates

Release gates run in this order:

1. Scope gate: Windows target, minimum Windows/WebView2, baseline export profile, path policy, FFmpeg build/license, and fixture policy are decided.
2. Media gate: bundled `ffprobe`/`ffmpeg` and at least one licensed real fixture are available and reproducible.
3. State/IR gate: integer ticks, rational timebase/speed, half-open intervals, typed operations, validation, and revisioning pass.
4. Persistence gate: canonical JSON, atomic save, recovery, migration, and missing-asset behavior pass.
5. Optional capability gates: STT, LLM, TTS, hardware acceleration, generative video, MCP, and Android each pass their own readiness and security evidence.
6. Release gate: packaged clean-machine install, offline smoke, hashes, licenses, and known limitations are recorded.

Preliminary domain types may be created before the media spike because media contracts depend on them; acceptance still follows the gate order above.

## 3. Dependency graph

```text
G0 scope/toolchain/license decisions
  |
  +--> W1 editor-domain: ProjectDocument, IR, validation, typed operations
  |       |
  |       +--> W2 editor-project: JSON, paths, atomic save, recovery
  |       |       |
  |       |       +--> F1 persistence slice
  |       |
  |       +--> W3 editor-media: probe, render plan, bundled FFmpeg adapter
  |       |       |
  |       |       +--> F2 media/preview --> F3 edit/export
  |       |
  |       +--> W4 editor-jobs/editor-app: JobId, snapshots, progress, cancel
  |               |
  |               +--> W5 Tauri commands/capabilities/IPC facade
  |
  +--> W7 fixture and evidence harness
          |
          +--> validates F2/F3/F4 and every later media/AI claim

 W5 Tauri shell + W6 React UI + W1/W2/W3/W4
  |
  +--> F0 launch/create/open/probe/single-asset preview
  +--> F1 two-clip edit/export
  +--> F2 reliability/security/accessibility hardening
  +--> F3 deterministic feature expansion
          |
          +--> A1 STT/subtitles --> A2 transcript/LLM edit plans --> A3 TTS
          +--> A4 experimental capabilities (isolated; never core blocker)
          |
          +--> R Windows packaging/release
```

Dependency direction is one-way: React components call the typed frontend facade; the facade calls Tauri commands; commands call `editor-app`; application use cases compose domain, project, media, jobs, and optional AI ports. `editor-domain` has no Tauri, filesystem, FFmpeg, SQLite, GPU, model, or UI dependency.

## 4. Worker write-scope

Workers may read the whole contract and neighboring interfaces, but write only their owned paths. No worker edits `AGENTS.md`, `PRD.md`, `DESIGN.md`, or this plan. No worker stages all files; integration uses explicit allowlists.

| Worker | Allowed write scope | Owns | Must not write | Dependencies |
|---|---|---|---|---|
| W0 — bootstrap/integration | `Cargo.toml`, workspace manifest files, `package.json`, `package-lock.json`, `scripts/` integration files, approved lockfiles | Toolchain pinning, package scripts, dependency/license review, merge sequencing | Feature internals, model/media payloads, frozen specs | G0 |
| W1 — domain | `crates/editor-domain/**` and its tests | IDs, `Rational`, ticks, `ProjectDocument`, timeline IR, typed operations, validation, revision rules | I/O, Tauri, FFmpeg, SQLite, UI, model code | G0 |
| W2 — project/persistence | `crates/editor-project/**` and its tests | `.vdeproj`, relative paths, canonicalization ports, atomic save, backup, migration, recovery | Timeline business rules duplicated from W1, UI, media execution | W1 |
| W3 — media | `crates/editor-media/**`, approved `resources/ffmpeg/**` manifests, media tests | Fixed-argv bundled FFmpeg/ffprobe adapter, probe parsing, typed render plan, output validation, CPU baseline | PATH discovery, shell strings, frontend filtergraphs, GPU-first path | W1, G0 media decision |
| W4 — jobs/application | `crates/editor-jobs/**`, `crates/editor-app/**` and tests | Job state machine, snapshots, cancellation, progress, error envelope, use cases | Tauri transport details, React state, provider-specific model code | W1, W2, W3 |
| W5 — desktop host | `apps/desktop/src-tauri/**` | Tauri commands, capabilities, scoped filesystem/dialog, CSP, typed IPC transport, sidecar declaration | Business logic, broad permissions, raw shell exposure | W2, W3, W4 |
| W6 — frontend | `apps/desktop/src/**` | Media bin, preview, timeline, inspector, jobs, accessibility, Zustand projection, typed API facade | Direct raw `invoke`, canonical state ownership, fake media/transcript/model output | W5 and corresponding use cases |
| W7 — fixtures/evidence | `fixtures/**`, `docs/release/**`, approved `resources/licenses/**` | Licensed fixture manifest, hashes, expected metadata, golden assertions, evidence reports, license inventory | User media/model payloads without redistribution permission, product source internals | G0 and each feature slice |
| WA1 — STT/subtitles | `crates/editor-ai/**` STT adapter, model manifests, AI UI additions under assistant-owned paths | Whisper-compatible provider, transcript/subtitle IR mapping, readiness states, provenance | Empty/fabricated transcript, direct project mutation, hidden download | F2/F3/F4, real audio/model |
| WA2 — transcript/LLM | `crates/editor-ai/**` planner/executor boundary and assistant UI additions | Typed `EditPlan`, allowlist, revision check, dry-run, confirmation, negative tests | Shell/path/filtergraph execution, silent mutation, free-form `EDIT:` protocol | WA1 or transcript fixture, F4 |
| WA3 — TTS | `crates/editor-ai/**` TTS adapter and model manifest additions | Real audio asset creation, voice provenance, placement job, failure path | Placeholder audio, unverified voice/model, hidden network | F4, model/voice fixture |
| WA4 — experimental | Isolated capability modules and manifests only | Generative video/GPU/MCP/Android spikes after explicit approval | Core dependency, release gate, broad permissions, speculative crates | F4 plus measured trigger and capability review |
| WR — release | Packaging config, installer scripts, release evidence only | Windows installer, bundled resources, clean-machine matrix, hashes, notices, limitations | Source feature changes, undocumented dependency upgrades | F0–F4 and selected AI gates |

Integration owns cross-worker conflict resolution. `Cargo.lock` and `package-lock.json` change only after dependency review; workers do not add competing FFmpeg, Whisper, LLM, timeline, GPU, or TTS stacks speculatively.

## 5. Phases and definition of done

### G0 — Scope and bootstrap

Owners: W0, W1, W5, W6, W7.  
Depends on: none.

Work:

- Confirm Windows x64 and minimum Windows/WebView2 support.
- Select one FFmpeg/ffprobe binary build, sidecar naming, codec/license profile, and one baseline MP4 profile; record source, version, SHA-256, license, architecture, and packaging decision.
- Create only manifests and boundaries required by the first vertical slice. Do not create empty crates for later.
- Establish npm/cargo scripts, typed error envelope, capability manifest, strict CSP, scoped paths, and the smallest launchable Tauri/React shell.
- Define fixture manifest schema, evidence directory policy, and `PASS`/`FAIL`/`BLOCKED`/`UNAVAILABLE` reporting.

DoD:

- Scope decisions are recorded in implementation/release records without changing frozen specifications.
- Workspace and frontend manifest exist, dependencies are pinned after review, and no speculative stack is present.
- Empty app launches through the planned Tauri command. No invented media or model appears.
- Fixture and binary provenance format is executable by W7.
- F0 toolchain checks below pass, or each missing tool is explicitly `BLOCKED`.

Verification:

```text
rtk cargo fmt --all -- --check
rtk cargo test -p editor-domain
rtk npm exec tsc -- --noEmit
rtk npm run tauri dev
```

`rtk` is tooling, not an application dependency. If it is not installed, record that fact and run the same underlying project command only with explicit tool/version evidence.

### F0 — Canonical state, persistence, probe, and single-asset preview

Owners: W1, W2, W3, W4, W5, W6, W7.  
Depends on: G0.

Work:

- Implement validated `ProjectDocument`, normalized sequence/tracks/clips, stable IDs, integer timeline ticks, rational timebase/speed, and `[in, out)` intervals.
- Implement project create/open/save/close/recover with versioned JSON, atomic temporary sibling, reopen validation, one backup/autosave policy, migration boundary, and preserved missing assets.
- Implement file-dialog import, canonicalized/scoped paths, fingerprint, bundled `ffprobe` JSON parsing, required metadata, explicit asset status, and real errors.
- Implement direct single-asset preview; preview state must identify unavailable/missing/proxy status.
- Implement typed Tauri API facade and minimal accessible media browser/preview/project flows.

DoD:

- M-01, M-02, M-03, and M-05 have real fixture evidence.
- Project JSON round-trips without timeline drift; invalid JSON, duplicate IDs, bad rational values, negative durations, unknown assets, and missing files fail closed.
- Real files with spaces and Unicode are imported or rejected with a diagnostic; no placeholder clip is created.
- Probe output records tool version and required stream/timebase metadata.
- Single available asset plays/seeks in the packaged or declared desktop runtime without internet.

Verification:

```text
rtk cargo fmt --all -- --check
rtk cargo test -p editor-domain
rtk cargo test -p editor-project
rtk cargo clippy -p editor-media --all-targets -- -D warnings
rtk npm exec vitest -- run src/features/project
rtk npm exec vitest -- run src/features/media
rtk npm exec tsc -- --noEmit
rtk npm run tauri dev
```

### F1 — Two-clip deterministic edit/export slice

Owners: W1, W3, W4, W5, W6, W7.  
Depends on: F0.

Work:

- Add trim, split, reorder, delete, replace/relink as typed non-destructive operations.
- Snapshot validated revision, resolve `AssetId` to allowed paths, build deterministic CPU `RenderPlan`, and invoke bundled FFmpeg with argv arrays, `-progress`, and `-nostdin`.
- Write output to a temporary sibling, validate existence/non-zero size/probe/decode/expected stream order and duration, then atomically rename.
- Keep last valid output when a later export fails or is cancelled.

DoD:

- M-04, M-06, M-07, M-08, and M-09 pass with two licensed real video fixtures.
- Save/reopen preserves clip order, boundaries, IDs, and revision semantics.
- Export has output SHA-256, `ffprobe` evidence, decode/playback evidence, duration/order assertions, and no input/output aliasing.
- Unsupported, corrupt, missing, unreadable, cancelled, incomplete, and failed exports are explicit failures; no false success or partial output.
- CPU path is reference. No GPU dependency is introduced.

Verification:

```text
rtk cargo test -p editor-domain
rtk cargo clippy -p editor-media --all-targets -- -D warnings
rtk cargo test -p editor-media
rtk npm exec vitest -- run src/features/timeline
rtk npm exec vitest -- run src/features/preview
rtk npm exec tsc -- --noEmit
rtk npm run tauri dev
```

### F2 — Reliability, jobs, security, and accessibility

Owners: W4, W5, W6, W7.  
Depends on: F1.

Work:

- Complete `JobId` lifecycle: queued, running, succeeded, failed, cancel requested, cancelled; persist diagnostic-safe job state.
- Add bounded concurrency, progress/indeterminate stages, cancellation that terminates child and cleans temporary output, crash recovery, autosave, relink, and previous-output preservation.
- Harden Tauri capabilities, path canonicalization/scope checks, symlink/traversal/output-alias defense, CSP, error redaction, and no frontend shell access.
- Add keyboard timeline flows, visible focus, semantic labels, live status announcements, dialog focus restore, reduced motion, contrast, resize, and UIA/screen-reader checks.
- Capture offline core run and process/network evidence.

DoD:

- M-10 and M-11 pass; US-005 fault-injection/recovery evidence passes.
- Cancelled/failed jobs never produce a success artifact and do not corrupt project or previous export.
- Security review finds no broad filesystem/shell capability and no raw frontend filtergraph/path execution.
- Keyboard-only critical flow and accessibility evidence are recorded; color is not the only state cue.
- Network-disabled create/open/save/edit/preview/export works with core assets.

Verification:

```text
rtk cargo fmt --all -- --check
rtk cargo clippy -p editor-media --all-targets -- -D warnings
rtk cargo test -p editor-jobs
rtk cargo test -p editor-app
rtk npm exec vitest -- run src/features/jobs
rtk npm exec tsc -- --noEmit
rtk npm run tauri dev
```

### F3 — Deterministic editing expansion

Owners: W1, W3, W4, W5, W6, W7.  
Depends on: F2.

Work, in small slices with real fixtures: bounded multi-track video/audio/subtitle, overlap validation, lock/visibility, volume/pan/fades/mute/background music/ deterministic ducking, waveform and filmstrip caches keyed by fingerprint, typed brightness/contrast/saturation/rotate/crop/speed/color controls, transitions/compositing, text/graphic overlays, SRT/VTT sidecar and burn-in, undo/redo, project backup, and optional hardware decode/encode fallback.

DoD:

- S-01 through S-10 are not marked complete as a group until every shipped operation is typed, previewed, exported, and covered by real-media assertions.
- Each cache invalidates on fingerprint/settings change and never becomes project source of truth.
- Raw FFmpeg filter syntax is unreachable from UI/LLM; typed render builder is the only translation path.
- CPU output remains the reference. Hardware is separately reported `PASS` only with process logs, GPU/driver evidence, and CPU/output parity.
- Every mutation is undoable or explicitly documented as non-mutating.

Verification:

```text
rtk cargo test -p editor-domain
rtk cargo clippy -p editor-media --all-targets -- -D warnings
rtk npm exec vitest -- run src/features/timeline
rtk npm exec vitest -- run src/features/preview
rtk npm exec vitest -- run src/features/media
rtk npm exec tsc -- --noEmit
```

### A1 — Local STT and editable subtitles

Owners: WA1, W3, W4, W5, W6, W7.  
Depends on: F3 and a real audio fixture.

Work:

- Choose one Windows-tested Whisper-compatible runtime only after a spike; record model/runtime/version/license/size/SHA-256/architecture/resource requirements.
- Add manifest/readiness states `Missing`, `Downloading`, `Verifying`, `Ready`, `Failed`, `Incompatible`, `UNAVAILABLE`.
- Run local STT as a cancellable job; convert verified timestamps into the same subtitle IR used by manual captions.
- Add editable cues, transcript provenance, error state, and optional explicit model import/download with `.part` recovery.

DoD:

- A-01 and A-02 pass on real audio with model identity, timestamps, transcript evidence, accuracy protocol, and output playback/render evidence.
- Missing/corrupt/incompatible model or runtime is visible as `UNAVAILABLE`; no empty transcript is treated as success.
- Network-disabled inference works when model/runtime is present; no hidden download or cloud fallback.

Verification:

```text
rtk cargo test -p editor-ai
rtk cargo clippy -p editor-media --all-targets -- -D warnings
rtk npm exec vitest -- run src/features/assistant
rtk npm exec tsc -- --noEmit
```

### A2 — Transcript operations and local LLM edit planning

Owners: WA2, WA1, W4, W5, W6, W7.  
Depends on: F3, F4, and transcript ground truth; A1 is needed for transcript-derived features.

Work:

- Select one Windows-tested local LLM runtime/model only after a resource/license spike.
- Accept bounded project context and return only schema-validated `EditPlan` with base revision, typed allowlisted operations, warnings, affected clips, and confirmation requirement.
- Implement transcript search/cuts as real range selection; implement filler/silence/best-take as reviewable proposals only.
- Validate IDs, ranges, locks, resource cost, permissions, and revision before apply; require user confirmation and preserve undo.

DoD:

- A-03 through A-06 pass with ground-truth fixture, dry-run/plan diff, approval, revision mismatch test, before/after project, undo, and unsupported/prompt-injection negative tests.
- LLM cannot execute shell, choose arbitrary paths, emit filtergraphs, bypass confirmation, or mutate without application use-case validation.
- Missing model/runtime is `UNAVAILABLE`; malformed/unsupported intent is an explicit no-change error.

Verification:

```text
rtk cargo test -p editor-ai
rtk cargo test -p editor-app
rtk npm exec vitest -- run src/features/assistant
rtk npm exec tsc -- --noEmit
```

### A3 — Local TTS

Owners: WA3, W4, W5, W6, W7.  
Depends on: F4 and a real model/voice provision.

Work:

- Select one Windows-tested Piper/Sherpa-compatible runtime and voice after license/resource review.
- Generate a real audio asset in a job, fingerprint it, record model/voice provenance, place it through typed timeline operations, and validate playback/export.

DoD:

- A-07 and the relevant part of A-08 pass with `ffprobe`, playback, checksum, provenance, cancellation, and failure evidence.
- Missing model/voice/runtime is `UNAVAILABLE`; no placeholder audio or empty successful result.

Verification:

```text
rtk cargo test -p editor-ai
rtk npm exec vitest -- run src/features/assistant
rtk npm exec tsc -- --noEmit
```

### A4 — Experimental capability packs

Owners: WA4, W7.  
Depends on: F4; each capability also needs its own trigger, security review, and acceptance matrix.

Work:

- C-01: local generative video as an optional reviewed asset pack with resource gate, cancel, provenance, and no-cloud proof.
- C-02: GPU preview/compositing only after measured CPU bottleneck, platform matrix, runtime trace, and output parity.
- C-03: versioned least-privilege MCP/agent protocol with human confirmation for mutations.
- C-04: Android/shared Rust target only as a separate project and acceptance matrix after Windows contracts stabilize.

DoD:

- Experimental capability is isolated from core install and core release gate.
- No capability is advertised from a filename, static dependency, or reference repository alone.
- Unprovisioned capability is `UNAVAILABLE`; missing acceptance artifact/tool/fixture is `BLOCKED`.

Verification is capability-specific. Do not add a generic pass command. Reuse the mandated Rust/TypeScript checks for touched code, then require runtime evidence described above.

### R — Windows packaging and release evidence

Owners: WR, W0, W5, W6, W7.  
Depends on: F0–F4 and only the AI/capability phases intentionally included in the release.

Work:

- Package Tauri app, target-suffixed bundled FFmpeg/ffprobe, notices/licenses, and approved optional assets.
- Run clean-machine install, offline smoke, Unicode/spaces/UNC path matrix, project relocation, recovery/cancel, and previous-output preservation.
- Record artifact hashes, tool versions, build type, fixture/model provenance, network/process evidence, accessibility evidence, known limitations, and release status.

DoD:

- PRD R exit criteria pass: clean install, bundled resources, offline smoke, license inventory, hashes, and known limitations.
- Core release does not require AI models, GPU drivers, Android, MCP, or generative-video packs.
- Any omitted feature is listed with its exact `BLOCKED` or `UNAVAILABLE` reason.
- No release claim is made from a dev launch, one machine, one fixture, or one successful command alone.

Verification:

```text
rtk cargo fmt --all -- --check
rtk cargo test -p editor-domain
rtk cargo clippy -p editor-media --all-targets -- -D warnings
rtk npm exec tsc -- --noEmit
rtk npm run tauri dev
```

If `npm run build` or a frontend lint script is declared during bootstrap, run it with `rtk` and record the exact script/version. If the script or clean Windows machine is absent, release remains `BLOCKED`; do not substitute dev launch evidence.

## 6. Required status handling

| Missing prerequisite | Affected feature/capability | Required status | Unblock evidence |
|---|---|---|---|
| No Rust/npm/Tauri/WebView2 toolchain or manifest | All implementation phases | `BLOCKED` | Versioned toolchain and successful scoped checks |
| No bundled `ffmpeg.exe`/`ffprobe.exe` with provenance | M-03, M-05, M-07, M-08, M-09 and media work | `BLOCKED` | Selected binary manifest, SHA-256, license record, fixed-argv probe/render evidence |
| No licensed real media fixture or expected metadata | Import/probe/preview/edit/export acceptance; M-02–M-09 and S-* | `BLOCKED` | Fixture source/license/SHA-256/`ffprobe` JSON and real-media assertions |
| Missing implementation or package for a planned feature | Affected feature/phase | `BLOCKED` | Corresponding source manifests, build, tests, and runtime evidence |
| Missing/corrupt/unverified STT runtime or model | A-01/A-02 | Product capability `UNAVAILABLE`; phase gate `BLOCKED` | Runtime/model manifest, checksum, license, resource check, real transcript |
| Missing/corrupt/unverified LLM runtime or model | A-03–A-06 | Product capability `UNAVAILABLE`; phase gate `BLOCKED` | Typed plan evidence, negative tests, model provenance, local inference |
| Missing/corrupt/unverified TTS runtime or voice | A-07/A-08 | Product capability `UNAVAILABLE`; phase gate `BLOCKED` | Real audio output, `ffprobe`, playback, provenance |
| GPU backend/driver/device absent | C-02 and optional hardware path | `UNAVAILABLE`; CPU core remains eligible | Runtime GPU trace, driver evidence, CPU/output parity |
| Generative model/resource pack absent | C-01 | `UNAVAILABLE`; core release unaffected | Model/resource manifest, cancel/review/no-cloud evidence |
| MCP implementation or protocol/security evidence absent | C-03 | `UNAVAILABLE` or `BLOCKED` for its phase; never core success | Versioned protocol, least privilege, confirmation tests |
| Android toolchain/device/acceptance matrix absent | C-04 | `UNAVAILABLE` and out of V1 scope | Separate Android plan and real target evidence |
| Clean Windows machine/install target absent | R | `BLOCKED` | Clean-machine install and offline smoke logs |
| Accessibility inspection tool/screen reader/UIA evidence absent | M-11 and F2 | `BLOCKED` | Keyboard, focus, UIA/screen-reader, contrast, zoom, resize evidence |
| License/provenance record absent for dependency, binary, model, voice, or fixture | Affected feature/release | `BLOCKED` | Reviewed inventory, notices, source/version/hash/license |
| Optional download endpoint unavailable | Model provisioning only | AI remains `UNAVAILABLE`; core is not blocked | Manual import or later verified download; no hidden fallback |

Current state for this checkout: implementation exists for Rust domain, persistence, media, jobs, AI policy, Tauri host IPC, React UI, and evidence validators; local cargo/npm static and unit gates pass. Real FFmpeg/ffprobe binaries, licensed fixtures, AI models/providers, native file dialog/runtime preview, installer, clean-machine/accessibility acceptance, and real-media acceptance remain `BLOCKED` or `UNAVAILABLE`. Optional AI/GPU/Android capabilities may be displayed as `UNAVAILABLE` only after the corresponding capability status surface exists; until then their acceptance phases remain `BLOCKED`.

## 7. Evidence record required for every completed phase

Each phase stores, at minimum:

- exact command and exit code;
- Rust/npm/Tauri/FFmpeg/model tool versions;
- source revision or explicit non-Git checkout identity;
- fixture/model/binary source, license, size, architecture, and SHA-256;
- project revision and snapshot hash for jobs;
- output hash, `ffprobe` JSON, decode/playback result, and expected assertions;
- runtime machine/build profile, repetitions and percentile for performance claims;
- cancellation/recovery, network-disabled, path, security, and accessibility evidence where applicable;
- unresolved limitations and exact `BLOCKED`/`UNAVAILABLE` states.

One dev launch, one device/machine, one static dependency scan, a fake executor, an empty result, or a filename is never sufficient acceptance evidence.
