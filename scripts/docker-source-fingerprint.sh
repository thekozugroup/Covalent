#!/usr/bin/env bash
set -euo pipefail

if (( $# != 1 )); then
  echo "usage: docker-source-fingerprint.sh <repo-root>" >&2
  exit 2
fi

# These are every Docker build input that can affect the bytes produced by
# packaging/docker/Dockerfile. The shared helper records tracked and untracked
# nonignored paths by mode and content, and double-reads the tree so a manifest
# is never accepted while its source is moving.
repo_root=$1
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
exec "$script_dir/android-source-fingerprint.sh" "$repo_root" \
  .dockerignore \
  Cargo.toml \
  Cargo.lock \
  rust-toolchain.toml \
  crates \
  packaging/web \
  packaging/docker/Dockerfile \
  packaging/docker/Caddyfile \
  packaging/docker/entrypoint.sh \
  packaging/docker/caddy
