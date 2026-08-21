#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
node_binary="$repo_root/target/release/covalent-node"
cli_binary="$repo_root/target/release/covalent"
image=""
image_only=false
platform=""

if [ "${1:-}" = "--image-only" ]; then
  image_only=true
  image="${2:-}"
  if [ -z "$image" ]; then
    echo "--image-only requires a Docker image reference" >&2
    exit 2
  fi
else
  image="${1:-}"
fi

if [ "${3:-}" = "--platform" ]; then
  platform="${4:-}"
elif [ "${2:-}" = "--platform" ]; then
  platform="${3:-}"
fi
if [ -n "$platform" ] && [ -z "$image" ]; then
  echo "--platform requires a Docker image reference" >&2
  exit 2
fi

max_node_bytes=$((16 * 1024 * 1024))
max_cli_bytes=$((8 * 1024 * 1024))
max_image_bytes=$((96 * 1024 * 1024))

check_file() {
  artifact=$1
  limit=$2
  label=$3
  if [ ! -f "$artifact" ]; then
    echo "missing $label artifact: $artifact" >&2
    exit 1
  fi
  size=$(wc -c < "$artifact" | tr -d '[:space:]')
  if [ "$size" -gt "$limit" ]; then
    echo "$label exceeds budget: $size bytes > $limit bytes" >&2
    exit 1
  fi
  echo "$label size: $size bytes (budget $limit)"
}

if [ "$image_only" = false ]; then
  check_file "$node_binary" "$max_node_bytes" "covalent-node"
  check_file "$cli_binary" "$max_cli_bytes" "covalent CLI"
else
  echo "host binary budgets skipped for explicit image-only validation"
fi

if [ -n "$image" ]; then
  if ! command -v docker >/dev/null 2>&1; then
    echo "Docker is required to measure image $image" >&2
    exit 1
  fi
  image_size=$(docker image inspect "$image" --format '{{.Size}}')
  if [ "$image_size" -gt "$max_image_bytes" ]; then
    echo "container image exceeds budget: $image_size bytes > $max_image_bytes bytes" >&2
    exit 1
  fi
  if [ -n "$platform" ]; then
    expected_arch=${platform#linux/}
    actual_arch=$(docker image inspect "$image" --format '{{.Architecture}}')
    if [ "$actual_arch" != "$expected_arch" ]; then
      echo "container image architecture mismatch: expected $expected_arch for $platform, got $actual_arch" >&2
      exit 1
    fi
    echo "container image size ($platform): $image_size bytes (budget $max_image_bytes)"
  else
    echo "container image size: $image_size bytes (budget $max_image_bytes)"
  fi
fi
