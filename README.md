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

### New machine

```bash
aws sso login --profile dev
cargo run -- credentials      # shows what resolved, and from where
```

That is the whole setup. The team's shared API keys live in `dev.sops.env`,
committed to this repo with every value encrypted under the saaga dev KMS key —
the same arrangement `saaga`, `strapi-cms` and the other repos use. The app
decrypts it at startup, so there is no key to be handed to you and nothing to
paste.

Access is an IAM question, not a file-sharing one: granting someone `kms:Decrypt`
on the dev key lets them run the app, and removing it takes their access away
without anyone re-encrypting or redistributing anything.

`cargo run -- credentials` prints every key, whether it is set, and which layer
supplied it — without printing any secret. It is the first thing to run when
something is not working.

### Editing a shared key

```bash
sops dev.sops.env             # opens decrypted in $EDITOR, re-encrypts on save
```

Values are encrypted individually and variable names stay in plaintext, so
`git diff` shows *which* key changed rather than one unreadable blob. Never
`git add` the file without confirming it still contains `ENC[`.

### Personal overrides

The **Settings** tab in the app writes a local `.env` that takes precedence over
the team file, for anything you want to differ on your machine — your own
OpenRouter key, a local Strapi. The tab labels each field with where its current
value came from (`from the team`, `set here`, `from your shell`) so a value you
already have is never one you retype.

Where that local file lives:

| where you are | file the Settings tab edits |
|---|---|
| working in a checkout | that checkout's `.env` |
| running an installed release | `~/.stream-recorder/.env` (created on first save) |

Precedence, first wins: **shell export → local `.env` → `dev.sops.env`**.

YouTube is the one credential that stays personal in all cases — the OAuth
client is shared, but each person signs in as themselves with **Connect** on the
YouTube tab, and the token is stored per-account in `~/.saaga/auth.db`.

Without AWS access the app still runs; it prints one line saying the team
credentials could not be read and falls back to whatever is set locally.

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
