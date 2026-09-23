#!/bin/bash
# Install what a render needs on a fresh Deep Learning Base GPU AMI: Chrome's
# runtime libraries, ffmpeg, Node, chrome-headless-shell and hyperframes — each
# at a pinned version, so a render does not change because a registry did.
#
# Run by worker.py, which passes HF_VERSION and CHROME_VERSION. ~100 s.
set -euxo pipefail

export DEBIAN_FRONTEND=noninteractive HOME=/root
# On first boot Ubuntu's own updater takes the dpkg lock at a moment of its
# choosing — after a wait loop has seen it free, on 2026-09-23. A machine that
# lives for ten minutes has no use for it: stop it, and have apt wait for the
# lock rather than fail on it in case it was already mid-run.
systemctl stop unattended-upgrades.service apt-daily.service apt-daily-upgrade.service \
  apt-daily.timer apt-daily-upgrade.timer 2>/dev/null || true
APT=(apt-get -o DPkg::Lock::Timeout=600 -o Acquire::Retries=3)
"${APT[@]}" update
"${APT[@]}" install -y --no-install-recommends ca-certificates curl unzip xz-utils ffmpeg \
  libgbm1 libnss3 libatk-bridge2.0-0 libdrm2 libxcomposite1 libxdamage1 libxrandr2 \
  libcups2 libpangocairo-1.0-0 libxshmfence1 libgtk-3-0 libegl1 libasound2 \
  fonts-liberation fonts-noto-color-emoji fonts-noto-core fonts-dejavu-core fontconfig

NODE=v22.19.0
curl -fsSL "https://nodejs.org/dist/$NODE/node-$NODE-linux-x64.tar.xz" | tar -xJ -C /opt
ln -sf "/opt/node-$NODE-linux-x64/bin/"* /usr/local/bin/

export NPM_CONFIG_REGISTRY=https://registry.npmjs.org
npx --yes @puppeteer/browsers install "chrome-headless-shell@${CHROME_VERSION}" --path /opt/chrome
# Into /usr/local, so `hyperframes` is on every PATH — the worker's included —
# rather than only inside Node's own folder.
npm install -g --prefix /usr/local "hyperframes@${HF_VERSION}"
command -v hyperframes
hyperframes telemetry disable || true

# The GPU is the whole point: fail here, in seconds, rather than render in
# software for hours.
nvidia-smi --query-gpu=name,driver_version --format=csv,noheader
