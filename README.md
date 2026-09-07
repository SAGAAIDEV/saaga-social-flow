# saaga-social-flow

A native macOS screen, camera, and microphone recorder built for social content creators. Captures multi-track video and audio with precise synchronization, generates procedural thumbnails with face tracking, and integrates with scheduling platforms.

## Features

- **Multi-track recording**: Screen capture, camera feed, and microphone all synced to a shared clock
- **Procedural thumbnails**: Generate procedural cards with offscreen WebView rendering and face-tracked overlays
- **Chapter markers**: Streamed take/chapter markers with global hotkey support (⌃⇧S)
- **Silent mic detection**: Automatic fallback device resolution when configured device goes silent
- **Frame colour management**: Per-frame colour annotation and visual frame tracking
- **Blog preview**: Integrated Strapi CMS preview and publishing
- **Social scheduling**: Buffer API integration for multi-platform post scheduling
- **Face tracking**: MediaPipe live face detection for thumbnail overlays
- **WebView UI**: Jinja2-templated native UI panes with web-based controls

## Requirements

- **macOS 12+** (uses ScreenCaptureKit, AVFoundation)
- **Xcode Command Line Tools** (for building)
- **Rust 1.70+** (install via [rustup](https://rustup.rs/))

## Installation

### Build from source

```bash
git clone https://github.com/SAGAAIDEV/saaga-social-flow
cd saaga-social-flow
cargo build --release
```

The binary will be at `target/release/saaga-social-flow`.

### Run

```bash
./target/release/saaga-social-flow
```

Or during development:

```bash
cargo run
```

## Configuration

Copy `.env.example` to `.env` and fill in your API keys:

```bash
cp .env.example .env
```

### Required API Keys

1. **OpenRouter API Key** (LLM for thumbnail blurbs)
   - Sign up: https://openrouter.ai/
   - Generate token in Settings → API Keys
   - Set `OPENROUTER_API_KEY=sk_...`

2. **Buffer API Key** (social scheduling)
   - Visit: https://publish.buffer.com/settings/api
   - Generate access token
   - Set `BUFFER_API_KEY=...`

3. **Strapi CMS** (blog publishing)
   - Local: `STRAPI_API_URL=http://localhost:1337`
   - Production: `STRAPI_API_URL=https://cms.saagasolve.com`
   - Generate API token in Strapi Admin → Settings → API Tokens
   - Set `STRAPI_API_TOKEN=...` and `BLOG_PUBLIC_BASE=http://localhost:3000`

### Optional Configuration

```bash
# Logging level (off, error, warn, info, debug, trace)
RUST_LOG=info

# Twitter handle for Buffer context
BUFFER_TWITTER_HANDLE=@your_handle

# Path to Node.js for content workflow
CONTENT_NODE_BIN=/usr/local/bin/node
```

See `.env.example` for complete reference.

## Usage

### Recording

1. Launch the app: `saaga-social-flow`
2. Use **⌃⇧S** to capture a screen region (drag to select, release to capture)
3. Select devices:
   - Screen: Choose display to record
   - Camera: Pick camera input (optional)
   - Microphone: Select audio device (auto-fallback on silence)
4. Press Start to begin recording
5. Use chapter markers (⌃⇧S) to split takes
6. Export when done

### Thumbnail Generation

Procedural cards are generated automatically:
- Built from TSX/HTML templates via offscreen WKWebView
- Rasterized as PNG/WebP with face tracking overlays
- Published to Strapi CMS for blog integration

### Scheduling

Generated content is queued in Buffer for posting to:
- LinkedIn
- Twitter/X
- Instagram
- Bluesky

## Architecture

### Core Modules

- **`src/capture/`**: Screen capture (ScreenCaptureKit), camera (AVFoundation), audio (Core Audio)
- **`src/ui/`**: Native macOS UI with Jinja2-templated WebView panes
- **`src/thumbnail/`**: Procedural card generation, face tracking, CMS integration
- **`src/schedule/`**: Buffer API integration, channel management, post planning
- **`src/card/`**: Card rendering, offscreen WebView rasterization
- **`src/figure/`**: Figure capture, WebP encoding, overlay composition

### Key Dependencies

- **objc2**: Safe Objective-C bindings (macOS APIs)
- **mediapipe**: Live face detection (Tasks C API)
- **minijinja**: Rust Jinja2 for template rendering
- **rusqlite**: Local SQLite for state persistence
- **rig**: LLM integration (Gemini)
- **winit**: Cross-platform windowing

## Performance

- **Release builds**: `opt-level = 3` for maximum performance
- **Face tracking**: Runs on background thread, never at chapter boundaries
- **MediaPipe**: Dynamically downloaded (~34 MB) on first use to `~/.cache/mediapipe-rs/`
- **Memory**: Efficient frame buffer pooling via Core Video

## Development

### Building

```bash
# Debug build
cargo build

# Release build (optimized)
cargo build --release

# Run tests
cargo test

# Check code
cargo check

# Format code
cargo fmt

# Lint
cargo clippy
```

### Project Structure

```
src/
├── main.rs              # Entry point, event loop
├── capture/             # Screen/camera/audio capture
├── ui/                  # Native UI, Jinja2 templates
├── thumbnail/           # Procedural card generation
├── schedule/            # Buffer scheduling integration
├── card/                # Card rendering via WKWebView
├── figure/              # Figure capture and encoding
└── av_delegate.rs       # AVAssetWriter delegate for audio/video
```

### UI Templates

HTML/Jinja2 templates in `src/ui/templates/`:
- Button rows, checkboxes, and form components via macros
- Data binding via `data-send` attributes
- CSS for native macOS appearance

## Troubleshooting

### Build Fails on macOS
- Ensure Xcode Command Line Tools are installed: `xcode-select --install`
- Update Rust: `rustup update`

### Silent Mic Issue
- The app auto-detects when the configured microphone goes silent
- Fallback device is resolved via `resolve_device`
- Check `RUST_LOG=debug` for device switching logs

### Thumbnail Generation Fails
- Ensure `GEMINI_API_KEY` is set
- Check Strapi CMS connectivity: `STRAPI_URL` and `STRAPI_TOKEN`

### Face Tracking Slow
- Face detection runs on a background thread
- First run downloads MediaPipe (~34 MB) — network-dependent

## Contributing

Contributions welcome! Please:
1. Fork the repository
2. Create a feature branch (`git checkout -b feat/your-feature`)
3. Commit with conventional messages (`feat:`, `fix:`, `docs:`)
4. Push and open a PR against `master`

## License

MIT

## Credits

Built by [SAGAAIDEV](https://github.com/SAGAAIDEV) for social content creators.

---

**Links:**
- [GitHub](https://github.com/SAGAAIDEV/saaga-social-flow)
- [Issues](https://github.com/SAGAAIDEV/saaga-social-flow/issues)
- [Releases](https://github.com/SAGAAIDEV/saaga-social-flow/releases)
