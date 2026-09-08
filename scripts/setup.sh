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
set -euo pipefail

cd "$(dirname "$0")/.."
HF_VERSION="$(sed -n 's/^pub const HF_VERSION: &str = "\(.*\)";/\1/p' src/edit/render.rs)"
CACHE="$HOME/.screencast/cache/hyperframes/$HF_VERSION/cli"

ok()   { printf '  \033[32mok\033[0m    %s\n' "$1"; }
warn() { printf '  \033[33mwarn\033[0m  %s\n' "$1"; }
bad()  { printf '  \033[31mmiss\033[0m  %s\n' "$1"; }

missing=0
need() {
  if command -v "$1" >/dev/null 2>&1; then ok "$1"; else bad "$1 — $2"; missing=1; fi
}

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
if [ -x "$CACHE/node_modules/.bin/hyperframes" ]; then
  ok "cached at $CACHE"
elif command -v npm >/dev/null 2>&1; then
  echo "  fetching hyperframes@$HF_VERSION (~360 MB, one time)…"
  mkdir -p "$CACHE"
  # --prefix keeps it out of this repo: the same renderer serves every project
  # on the machine, and 360 MB per checkout is not a tradeoff worth making.
  npm install --silent --prefix "$CACHE" "hyperframes@$HF_VERSION"
  ok "installed at $CACHE"
else
  bad "no npm, cannot fetch the renderer"
  missing=1
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
