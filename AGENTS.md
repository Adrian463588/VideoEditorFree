# Agent Instructions

## Scope and source of truth

- Read `PRD.md`, then `DESIGN.md`, before changing code or dependencies.
- Current checkout includes implemented Rust domain, persistence, media, jobs, application, and AI-policy crates plus a Tauri 2/React/TypeScript desktop baseline, an explicit runtime bundle manifest/downloader, and the native dialog plugin.
- Runtime files are provisioned outside Git by explicit user action. A missing local `ffmpeg.exe`/`ffprobe.exe`, licensed media fixture, model/provider, installer, or runtime evidence remains `BLOCKED`/`UNAVAILABLE` until verified; a URL or filename is never enough.
- Target V1: Windows x64 desktop, Rust core, Tauri 2 shell, React + TypeScript UI.
- Treat `ProjectDocument` as canonical; Zustand stores UI projection only.
- Keep AI, GPU acceleration, Android, MCP, and generative video behind capability gates.

## Package Manager

- Use `npm` for the frontend and `cargo` for the Rust workspace.
- Commit lockfiles after dependency review; never add overlapping media/AI stacks speculatively.
- Prefix shell commands with `rtk` when available; `rtk` is tooling, not a project dependency.

## File-Scoped Commands

| Task | Command |
|---|---|
| Rust format | `rtk cargo fmt --all -- --check` |
| Rust unit test | `rtk cargo test -p editor-domain` |
| Rust lint | `rtk cargo clippy -p editor-media --all-targets -- -D warnings` |
| Typecheck | `rtk npm exec tsc -- --noEmit` |
| Frontend test | `rtk npm exec vitest -- run src/features/<feature>` |
| Desktop dev | `rtk npm run tauri dev` |

## Architecture invariants

- `editor-domain` has no Tauri, filesystem, FFmpeg, SQLite, or UI imports.
- Use integer timeline ticks and rational speed/timebases; intervals are `[in, out)`.
- Spawn bundled `ffmpeg.exe`/`ffprobe.exe` with argv arrays; never use shell strings or PATH discovery.
- Long work returns `JobId`; progress, cancellation, failure, and temporary-output cleanup are mandatory.
- LLM output is an allowlisted typed edit plan requiring validation and user confirmation.
- Bundle downloads use the tracked allowlist, HTTPS host restriction, `.part` recovery, size/SHA-256 verification, and atomic installation under `%LOCALAPPDATA%\VideoEditorFree\runtime`.

## Safety and evidence

- Never fabricate media, transcripts, model readiness, benchmark results, or success responses.
- Missing dependency/model/media is `BLOCKED` or `UNAVAILABLE`, not a successful fallback.
- Preserve user files and unrelated changes; never use `git reset --hard`, `git clean`, or `git add .`.
- Stage explicit allowlists only. Record command, tool version, artifact hash, and runtime evidence.

## Commit Attribution

AI commits MUST include:

```text
Co-Authored-By: (the agent model's name and attribution byline)
```
