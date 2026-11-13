#!/usr/bin/env bash
# Build the linux aarch64 af binary natively on an arm64 host (docker,
# bookworm base) and pull it back to /tmp/af-dist on the controller.
# Binary requires glibc >= 2.32 (Ubuntu 24.04+, Debian bookworm+, glibc 2.39).
#
# This script assumes SSH access to an arm64 Linux host with Docker installed.
# Set ARM64_HOST (and optionally ARM64_JUMP) in your environment, e.g.:
#   ARM64_HOST=user@host ARM64_JUMP=user@jumphost scripts/dist/build-arm64.sh
set -euo pipefail
mkdir -p /tmp/af-dist
: "${ARM64_HOST:?ARM64_HOST is required (e.g. user@arm-host)}"
ARM64_JUMP="${ARM64_JUMP:-}"
SSH_OPTS=(-o IdentitiesOnly=yes -o StrictHostKeyChecking=no)
if [ -n "$ARM64_JUMP" ]; then
  SSH_OPTS+=(-J "$ARM64_JUMP")
fi
ssh -i ~/.ssh/id_ed25519_ansible "${SSH_OPTS[@]}" "$ARM64_HOST" '
    mkdir -p ~/af-out
    docker run --rm -v ~/af-out:/out rust:1-slim-bookworm bash -c "
      apt-get update -qq >/dev/null
      apt-get install -y -qq git ca-certificates >/dev/null 2>&1
      git clone --depth 1 https://github.com/tobias-weiss-ai-xr/agentflow /src -q
      cd /src
      cargo build --release 2>&1 | tail -1
      cp target/release/af /out/af-linux-arm64
      echo ARM64_OK
    "'
scp -i ~/.ssh/id_ed25519_ansible "${SSH_OPTS[@]}" "$ARM64_HOST":~/af-out/af-linux-arm64 /tmp/af-dist/
ls -la /tmp/af-dist/
