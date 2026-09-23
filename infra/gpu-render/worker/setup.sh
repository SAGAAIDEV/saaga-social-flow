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

# Every download below is pinned to exact bytes or an exact version: this runs
# as root on a machine with write access to its render's outputs.
NODE=v22.19.0
NODE_SHA256=c0649af18e6a24f6fe5535a3e86b341dd49a8e71117c8b68bde973ef834f16f2
curl -fsSL -o /tmp/node.tar.xz "https://nodejs.org/dist/$NODE/node-$NODE-linux-x64.tar.xz"
echo "$NODE_SHA256  /tmp/node.tar.xz" | sha256sum -c -
tar -xJf /tmp/node.tar.xz -C /opt
ln -sf "/opt/node-$NODE-linux-x64/bin/"* /usr/local/bin/

export NPM_CONFIG_REGISTRY=https://registry.npmjs.org
npx --yes @puppeteer/browsers@3.2.3 install "chrome-headless-shell@${CHROME_VERSION}" --path /opt/chrome

# The renderer exactly as the Mac installs it: renderer/package.json and its
# lockfile, which terraform ships beside this script, installed with `npm ci`
# so every transitive dependency is the version the lockfile pins — not
# whatever the registry calls newest this morning.
mkdir -p /opt/renderer
cp /render/package.json /render/package-lock.json /opt/renderer/
npm ci --prefix /opt/renderer --no-audit --no-fund
ln -sf /opt/renderer/node_modules/.bin/hyperframes /usr/local/bin/hyperframes
test "$(hyperframes --version)" = "${HF_VERSION}"
hyperframes telemetry disable || true

# The GPU is the whole point: fail here, in seconds, rather than render in
# software for hours.
nvidia-smi --query-gpu=name,driver_version --format=csv,noheader
