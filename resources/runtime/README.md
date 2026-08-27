# Runtime bundle

The repository intentionally contains only this manifest. Large binaries and
models are downloaded after an explicit user action into
`%LOCALAPPDATA%\VideoEditorFree\runtime`; that directory is ignored by Git.

The app's **Download bundle** button invokes
`scripts/runtime/download-bundle.ps1`. The script only accepts HTTPS artifacts
from `github.com` and `huggingface.co`, resumes `.part` files with `curl.exe`,
checks the declared byte count and SHA-256, and extracts only the required
executables. A failed checksum never becomes an installed runtime.

Sources selected for the Windows x64 manifest:

- FFmpeg Windows builds listed by the [FFmpeg download page](https://ffmpeg.org/download.html), using the BtbN GPL archive and its published checksum list.
- [llama.cpp b10642 Windows CPU release](https://github.com/ggml-org/llama.cpp/releases/tag/b10642).
- [whisper.cpp tiny.en model](https://huggingface.co/ggerganov/whisper.cpp).
- [Qwen2.5 0.5B Instruct GGUF](https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF).
- [Piper Windows runtime](https://github.com/rhasspy/piper/releases/tag/2023.11.14-2)
  and [lessac medium voice](https://huggingface.co/rhasspy/piper-voices/tree/v1.0.0/en/en_US/lessac/medium).

Review each upstream license and the Piper `MODEL_CARD` before redistributing
the downloaded files. The manifest is a provisioning source of truth, not a
claim that every optional AI adapter has passed runtime acceptance.
