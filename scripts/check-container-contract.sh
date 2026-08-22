#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
dockerfile="$repo_root/packaging/docker/Dockerfile"
documentation="$repo_root/packaging/docker/README.md"
runtime_digest="sha256:fd791d74b68913cbb027c6546007b3f0d3bc45125f797758156952bc2d6daf40"

require_text() {
  expected=$1
  path=$2
  if ! grep -Fq "$expected" "$path"; then
    echo "container contract is missing '$expected' in $path" >&2
    exit 1
  fi
}

require_text "FROM rust:1.97.1-alpine3.23@sha256:" "$dockerfile"
require_text "FROM caddy:2.11.4-alpine@sha256:" "$dockerfile"
require_text "FROM alpine:3.23@$runtime_digest" "$dockerfile"
require_text "org.opencontainers.image.base.name=\"docker.io/library/alpine:3.23\"" "$dockerfile"
require_text "org.opencontainers.image.base.digest=\"$runtime_digest\"" "$dockerfile"
require_text 'Alpine 3.23 runtime base' "$documentation"
# Markdown backticks are literal documentation text.
# shellcheck disable=SC2016
require_text '`linux/amd64` and `linux/arm64`' "$documentation"

if grep -Eiq 'Debian|Bookworm|glibc runtime' "$documentation"; then
  echo "container documentation describes an obsolete non-Alpine runtime" >&2
  exit 1
fi

image=${1:-}
expected_revision=${2:-}
if [ -n "$image" ]; then
  command -v docker >/dev/null 2>&1 || {
    echo "Docker is required to inspect $image" >&2
    exit 1
  }
  test "$(docker image inspect "$image" --format '{{.Os}}')" = linux
  test "$(docker image inspect "$image" --format '{{.Config.User}}')" = 65532:65532
  test "$(docker image inspect "$image" --format '{{index .Config.Labels "org.opencontainers.image.base.name"}}')" = docker.io/library/alpine:3.23
  test "$(docker image inspect "$image" --format '{{index .Config.Labels "org.opencontainers.image.base.digest"}}')" = "$runtime_digest"
  test "$(docker image inspect "$image" --format '{{index .Config.Labels "org.opencontainers.image.licenses"}}')" = MIT
  if [ -n "$expected_revision" ]; then
    actual_revision=$(docker image inspect "$image" --format '{{index .Config.Labels "org.opencontainers.image.revision"}}')
    if [ "$actual_revision" != "$expected_revision" ]; then
      echo "container OCI revision mismatch: expected $expected_revision, got $actual_revision" >&2
      exit 1
    fi
  fi
fi

echo "container Alpine documentation and OCI contract: ok"
