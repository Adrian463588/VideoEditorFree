# VideoEditorFree

![Rust](https://img.shields.io/badge/Rust-workspace-orange)
![Tauri](https://img.shields.io/badge/Tauri-2-blue)
![React](https://img.shields.io/badge/React-TypeScript-61dafb)

**VideoEditorFree** is an offline-first Windows desktop video editor with a
Rust domain/application core, Tauri 2 host, and React/TypeScript interface.
The project keeps the canonical project document and timeline in Rust; the UI
is a typed projection over host commands.

<p>
  <a href="https://github.com/Adrian463588/VideoEditorFree/releases/latest"><img alt="Download runtime bundle" src="https://img.shields.io/badge/Download-runtime%20bundle-5dd4a4?style=for-the-badge"></a>
  <a href="https://github.com/Adrian463588/VideoEditorFree/blob/main/PRD.md"><img alt="Read PRD" src="https://img.shields.io/badge/Read-PRD-80a8ff?style=for-the-badge"></a>
</p>

## What is implemented

- Integer-tick timeline IR with rational timebases and half-open ranges.
- Multi-track video, audio, text, and overlay lanes with drag/drop placement,
  magnetic snapping, typed trim, split, move, reorder, delete, ripple-delete,
  relink, markers, track state, effects, and revision-conflict operations.
- CPU video effects for exposure, gamma, temperature/tint, contrast, saturation,
  three-way color balance, crop, rotate, blur, sharpen, vignette, duotone, and
  project-relative `.cube` LUTs, plus opacity, transforms, and audio fades.
- Versioned `.vdeproj` persistence with relative asset paths, atomic saves,
  backup/recovery, and path containment checks.
- Fixed-argv FFmpeg/ffprobe planning, probe validation, cancellation, output
  verification, and atomic export finalization.
- Layered video/audio export with deterministic sidechain ducking and
  YouTube, Instagram Reels, and TikTok presets.
- Local multilingual Whisper subtitle generation with editable timestamped
  cues when the explicit Subtitle AI bundle is installed.
- Local Piper voiceover generation with a verified `en_US-lessac-medium` voice,
  inserted as a real audio asset and timeline clip when the AI bundle is
  installed.
- Reviewable local LLM edit plans using the provisioned Qwen GGUF runtime;
  typed timeline operations and color-grade presets require explicit apply.
- Job registry with snapshots, progress, cancellation, and failure envelopes.
- Bounded subprocess lifecycle: one local AI process at a time, fixed llama
  context/threads, 90-second planner timeout, capped model/media diagnostics,
  drained pipes, and in-flight host-status polling.
- Typed local-AI model/readiness and edit-plan contracts; model output cannot
  mutate a project without validation and confirmation.
- Native Tauri open/import dialogs with least-privilege capabilities.
- Explicit runtime provisioning from the app's **Download bundle** button.

## Downloading the runtime without bloating Git

The repository tracks only
[`resources/runtime/bundle-manifest.json`](resources/runtime/bundle-manifest.json)
and the downloader. Large files are never committed. The app downloads the
allowlisted Windows x64 artifacts to
`%LOCALAPPDATA%\VideoEditorFree\runtime`, resumes `.part` files, verifies the
declared size and SHA-256, then extracts only the required runtime files.

In the desktop app, choose **Download bundle** to provision the `core` media
profile (FFmpeg/ffprobe) required for import and export. From a
Windows development checkout, the equivalent command is:

```powershell
npm install
npm run verify:bundle
npm run bundle:download
```

Use `npm run bundle:download -- -Profile subtitles` or choose the **Subtitle AI**
profile beside the **Download bundle** button to install whisper.cpp and the
multilingual tiny model. The `subtitles` profile is separate from the larger
`ai` profile. Automatic
captions transcribe the selected spoken language locally; this is not a cloud
translation service.

The script defaults to the `all` profile for explicit CLI provisioning and also
accepts `-Profile core`, `-Profile subtitles`, or `-Profile ai`. Use `npm run bundle:download --
-Profile core` for the same profile as the desktop button. No network request is
made until the user explicitly starts the download. A failed or incomplete
download never changes capability state to `READY`.

The latest GitHub release also contains a small bootstrap ZIP with the script
and manifest only. It does not contain the large third-party binaries/models;
those remain upstream downloads and are installed only after checksum
verification.

The current correctness path is CPU-based. A native GPU backend is not claimed
until it has a reviewed implementation and measured acceptance evidence. The
preview transport also remains fail-closed until a reviewed playback backend is
provisioned; import, timeline editing, local AI generation, and export do not
silently fabricate unavailable results.

## Development

```text
npm install
npm run typecheck
npm run build
npm run verify:bundle
npm run verify:manifest
npm run tauri -- dev
```

Rust gates:

```text
rtk cargo fmt --all -- --check
rtk cargo test --locked --workspace
rtk cargo clippy --locked --workspace --all-targets -- -D warnings
rtk cargo build --locked --workspace
```

## Runtime and licensing

The selected FFmpeg build is a GPL Windows build distributed by BtbN and
listed by the FFmpeg project as a Windows-build source. The model manifest
records upstream source, version, license, size, architecture, and SHA-256 for
each optional artifact. Review upstream notices and the Piper voice
`MODEL_CARD` before redistributing downloaded files. The project does not
silently install third-party assets or claim runtime acceptance from a filename.

Read the contracts and traceability in
[`PRD.md`](PRD.md), [`DESIGN.md`](DESIGN.md), and [`AGENTS.md`](AGENTS.md).

Implementation references: [FFmpeg filters](https://ffmpeg.org/ffmpeg-filters.html),
[whisper.cpp CLI](https://github.com/ggml-org/whisper.cpp/blob/master/examples/cli/README.md),
[Piper](https://github.com/rhasspy/piper/blob/master/README.md), and
[llama.cpp CLI](https://github.com/ggml-org/llama.cpp/blob/master/tools/cli/README.md).
