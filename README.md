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
- Typed trim, split, reorder, delete, ripple-delete, relink, markers, track
  state, effects, and revision-conflict operations.
- Versioned `.vdeproj` persistence with relative asset paths, atomic saves,
  backup/recovery, and path containment checks.
- Fixed-argv FFmpeg/ffprobe planning, probe validation, cancellation, output
  verification, and atomic export finalization.
- Job registry with snapshots, progress, cancellation, and failure envelopes.
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
profile (FFmpeg/ffprobe) required for import, preview, and export. From a
Windows development checkout, the equivalent command is:

```powershell
npm install
npm run verify:bundle
npm run bundle:download
```

The script defaults to the `all` profile for explicit CLI provisioning and also
accepts `-Profile core` or `-Profile ai`. Use `npm run bundle:download --
-Profile core` for the same profile as the desktop button. No network request is
made until the user explicitly starts the download. A failed or incomplete
download never changes capability state to `READY`.

The latest GitHub release also contains a small bootstrap ZIP with the script
and manifest only. It does not contain the large third-party binaries/models;
those remain upstream downloads and are installed only after checksum
verification.

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
