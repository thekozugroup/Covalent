#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
dockerfile="$repo_root/packaging/docker/Dockerfile"
documentation="$repo_root/packaging/docker/README.md"
caddy_gomod="$repo_root/packaging/docker/caddy/go.mod"
caddy_gosum="$repo_root/packaging/docker/caddy/go.sum"
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
require_text "FROM alpine:3.23@$runtime_digest" "$dockerfile"

# Caddy is built from source against a pinned Go toolchain rather than lifted
# out of caddy:2.11.4-alpine, whose published binary is linked against go1.26.3.
# The old contract asserted the upstream image tag; the equivalent assertions
# for a from-source build are the toolchain image, the Caddy source version, and
# the two module bumps that the vendored binary could not deliver. All four are
# pinned by digest or exact version, and go.sum must exist or -mod=readonly has
# nothing to verify the module graph against.
require_text "FROM golang:1.26.7-alpine3.23@sha256:" "$dockerfile"
require_text "GOTOOLCHAIN=local" "$dockerfile"
require_text "GOFLAGS=-mod=readonly" "$dockerfile"
require_text "github.com/caddyserver/caddy/v2 v2.11.4" "$caddy_gomod"
# Eight stdlib advisories are fixed in go1.26.6; GOTOOLCHAIN=local makes this
# directive the floor the compiler itself enforces. Lowering it re-opens them.
require_text "go 1.26.7" "$caddy_gomod"
require_text "google.golang.org/grpc v1.82.1" "$caddy_gomod"
require_text "golang.org/x/text v0.39.0" "$caddy_gomod"
if [ ! -s "$caddy_gosum" ]; then
  echo "container contract requires a non-empty $caddy_gosum for -mod=readonly" >&2
  exit 1
fi
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
  # The Dockerfile asserts the build inputs; this asserts the artefact. A
  # from-source Caddy that silently stopped being Caddy 2.11.4 would still
  # produce a green build without it.
  caddy_version=$(docker run --rm --entrypoint caddy "$image" version 2>/dev/null | awk '{print $1}')
  if [ "$caddy_version" != "v2.11.4" ]; then
    echo "packaged Caddy reports '$caddy_version', expected v2.11.4" >&2
    exit 1
  fi
  if [ -n "$expected_revision" ]; then
    actual_revision=$(docker image inspect "$image" --format '{{index .Config.Labels "org.opencontainers.image.revision"}}')
    if [ "$actual_revision" != "$expected_revision" ]; then
      echo "container OCI revision mismatch: expected $expected_revision, got $actual_revision" >&2
      exit 1
    fi
  fi
fi

echo "container Alpine documentation and OCI contract: ok"
