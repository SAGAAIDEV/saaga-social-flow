#!/usr/bin/env bash
# One-time setup for a new machine.
#
# Everything here is idempotent — running it twice is not a mistake, and it is
# the right first move when something has drifted.
#
# What it does NOT do: install Rust, Node or the AWS CLI. Those are personal
# toolchain choices with real opinions attached (rustup vs brew, nvm vs system
# node), and a setup script that silently picks for you is one you cannot trust
# with anything larger. It tells you what is missing and how to get it.
#
# ffmpeg is the exception: it is not a toolchain choice, there is one way to
# get it on a Mac, and without it no chapter is ever transcribed or rendered.
# So with Homebrew present it is installed here (the app does the same at
# launch).
set -euo pipefail

cd "$(dirname "$0")/.."
HF_VERSION="$(sed -n 's/^pub const HF_VERSION: &str = "\(.*\)";/\1/p' src/edit/render.rs)"

ok()   { printf '  \033[32mok\033[0m    %s\n' "$1"; }
warn() { printf '  \033[33mwarn\033[0m  %s\n' "$1"; }
bad()  { printf '  \033[31mmiss\033[0m  %s\n' "$1"; }

missing=0
need() {
  if command -v "$1" >/dev/null 2>&1; then ok "$1"; else bad "$1 — $2"; missing=1; fi
}

echo "ffmpeg"
if command -v ffmpeg >/dev/null 2>&1 && command -v ffprobe >/dev/null 2>&1; then
  ok "ffmpeg and ffprobe"
elif command -v brew >/dev/null 2>&1; then
  echo "  installing ffmpeg with Homebrew (a few minutes, one time)…"
  NONINTERACTIVE=1 brew install ffmpeg
  if command -v ffmpeg >/dev/null 2>&1; then ok "ffmpeg installed"; else bad "brew install ffmpeg did not put ffmpeg on PATH"; missing=1; fi
else
  bad "ffmpeg — install Homebrew (https://brew.sh), then: brew install ffmpeg"
  missing=1
fi

echo
echo "Toolchain"
need cargo "install Rust: https://rustup.rs"
need node  "brew install node"
need npx   "ships with node"
need sops  "brew install sops"
need aws   "brew install awscli"
need uv    "brew install uv (only needed for S3 upload)"

echo
echo "Component library"
if [ -f components/compositions/chapter-title-card.html ]; then
  ok "components/ ($(find components -type f | wc -l | tr -d ' ') files)"
else
  bad "components/ is missing — run: git checkout components/"
  missing=1
fi

echo
echo "HyperFrames renderer (v$HF_VERSION)"
# A dependency of this repo like any other: renderer/package.json pins it and
# its lockfile pins everything under it. `npm ci` installs exactly that, and is
# a no-op-sized reinstall when nothing changed.
if command -v npm >/dev/null 2>&1; then
  if [ -x renderer/node_modules/.bin/hyperframes ] \
     && [ "$(renderer/node_modules/.bin/hyperframes --version 2>/dev/null)" = "$HF_VERSION" ]; then
    ok "installed in renderer/node_modules"
  else
    echo "  installing renderer/ (hyperframes@$HF_VERSION, ~130 MB, one time)…"
    # The public registry, whatever ~/.npmrc points at: the lockfile resolves
    # from it, and a private default registry would only fail the install.
    npm ci --silent --prefix renderer --registry https://registry.npmjs.org
    ok "installed in renderer/node_modules"
  fi
else
  bad "no npm, cannot install the renderer"
  missing=1
fi

echo
echo "Render on AWS GPU (optional)"
if command -v terraform >/dev/null 2>&1; then
  ok "terraform"
else
  warn "terraform — only needed to change the stack in infra/gpu-render"
fi

echo
echo "Credentials"
if aws sts get-caller-identity --profile dev >/dev/null 2>&1; then
  ok "AWS dev profile is live — shared keys will decrypt"
else
  warn "not signed in — run: aws sso login --profile dev"
fi

echo
if [ "$missing" -eq 0 ]; then
  echo "Ready. Verify with:"
else
  echo "Install what is marked 'miss' above, then re-run this. Meanwhile:"
fi
echo "  cargo run -- doctor        # render dependencies"
echo "  cargo run -- credentials   # API keys, and where each came from"
