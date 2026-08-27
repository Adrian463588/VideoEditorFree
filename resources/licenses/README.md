# License inventory

This inventory covers the artifacts declared by
`resources/runtime/bundle-manifest.json`. They are not committed to Git. The
downloader verifies the exact byte count and SHA-256 before installation.

| Artifact | Source/version | License | Target | Size | SHA-256 |
| --- | --- | --- | ---: | ---: | --- |
| BtbN FFmpeg Windows x64 GPL archive | FFmpeg download page → BtbN `latest` | GPL-3.0-or-later | Windows x64 | 170,676,191 | `06496188114b93337cdb10e38e5d3c8d7ffe467af2bc912268b601fa868d02cc` |
| llama.cpp CPU runtime | ggml-org `b10642` | MIT | Windows x64 | 18,076,036 | `b90c4b018de11961a25a2555427fa1576267e6499b3e2f873433d9188ec929e2` |
| Piper Windows x64 runtime | rhasspy Piper `2023.11.14-2` | MIT | Windows x64 | 22,477,236 | `f3c58906402b24f3a96d92145f58acba6d86c9b5db896d207f78dc80811efcea` |
| whisper.cpp `ggml-tiny.en` | ggerganov model repository | MIT | CPU model | 77,704,715 | `921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f` |
| Qwen2.5 0.5B Instruct Q4_K_M | Qwen model repository | Apache-2.0 | GGUF model | 491,400,032 | `74a4da8c9fdbcd15bd1f6d01d621410d31c6fc00986f5eb687824e7b93d7a9db` |
| Piper `lessac` medium voice | rhasspy voice repository `v1.0.0` | MIT model repository; dataset terms in `MODEL_CARD` | ONNX voice | 63,201,294 | `5efe09e69902187827af646e1a6e9d269dee769f9877d17b16b1b46eeaaf019f` |
| Piper `lessac` config | rhasspy voice repository `v1.0.0` | MIT model repository; dataset terms in `MODEL_CARD` | ONNX config | 4,885 | `efe19c417bed055f2d69908248c6ba650fa135bc868b0e6abb3da181dab690a0` |

FFmpeg itself publishes source code and points to Windows build providers; the
selected build is therefore recorded as a separately sourced third-party
archive. The Piper release API did not publish a digest for the selected
Windows archive, so its hash is explicitly marked as recomputed from the
immutable upstream release asset in the bundle manifest. Before publishing a
release asset, include the applicable upstream notices/source offer and
re-review the GPL and voice-dataset obligations. A missing or unreviewed notice
keeps distribution acceptance open.
