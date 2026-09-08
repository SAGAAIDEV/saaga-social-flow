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

Open the app and go to the **Settings** tab. Every key the app needs is listed
there, grouped by what it unlocks, with a link to where you get one and a
**Test** button that makes a real call to the service and reports what came
back. Saving applies straight away — no restart.

Settings writes an ordinary `.env` file and names the one it is editing at the
top of the tab:

| where you are | file it edits |
|---|---|
| working in a checkout | that checkout's `.env` |
| running an installed release | `~/.stream-recorder/.env` (created on first save) |

Keys already exported in the shell you launched from win over the file for that
session; the tab flags any field where that is happening, so an edit that looks
lost is labelled rather than silent.

Prefer to edit by hand? `cp .env.example .env` still works — the tab reads and
writes the same format, preserving comments and any unrelated keys in the file.

### Required keys

| key | what stops without it | where to get one |
|---|---|---|
| `OPENROUTER_API_KEY` | thumbnails, figure blurbs, notes, social copy | [openrouter.ai/keys](https://openrouter.ai/keys) |
| `BUFFER_API_KEY` | building and sending the social schedule | [Buffer → Settings → API](https://publish.buffer.com/settings/api) |
| `S3_BUCKET` | uploading renders so Buffer has a video URL to attach | your AWS account |
| `STRAPI_API_URL`, `STRAPI_API_TOKEN` | publishing the article to the CMS | Strapi Admin → Settings → API Tokens |
| `YOUTUBE_CLIENT_ID`, `YOUTUBE_CLIENT_SECRET` | uploading the render to YouTube | [Google Cloud Console](https://console.cloud.google.com/apis/credentials) — OAuth client of type *Desktop app* |

One OpenRouter key covers every model, image models included: thumbnails route
to `google/gemini-3.1-flash-image` **through OpenRouter**, so there is no
separate Gemini key.

AWS credentials are not among these — the S3 upload uses your AWS profile.
YouTube is per-person: the client id and secret are shared, but each person
signs in as themselves on the YouTube tab.

### Optional

| key | effect when unset |
|---|---|
| `ASSEMBLYAI_API_KEY` | chapters still record; transcripts are skipped |
| `BUFFER_ORG_ID` | resolved on first use — set it only if you belong to several Buffer workspaces |
| `YOUTUBE_CHANNEL_ID` | uploads go to whichever account is signed in, instead of aborting on the wrong channel |
| `BUFFER_TWITTER_HANDLE` | no handle in the context used to write posts |
| `BLOG_PUBLIC_BASE` | figure links in blog content have no site to point at |
| `RUST_LOG` | `info`. Set `debug` for device-switching logs |

See `.env.example` for the complete reference.

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
- Open **Settings** and press **Test** under Models — it reports whether the
  OpenRouter key is live and what credit is left
- Image generation goes through OpenRouter, so `OPENROUTER_API_KEY` is the key
  to check; there is no separate Gemini key
- For CMS problems press **Test** under Blog (Strapi), which checks
  `STRAPI_API_URL` and `STRAPI_API_TOKEN` together

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
