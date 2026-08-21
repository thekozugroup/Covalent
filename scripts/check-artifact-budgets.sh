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
  if ! command -v python3 >/dev/null 2>&1; then
    echo "python3 is required to measure image $image deterministically" >&2
    exit 1
  fi
  if ! docker image inspect "$image" >/dev/null 2>&1; then
    echo "container image not present locally: $image" >&2
    exit 1
  fi
  # `docker image inspect --format '{{.Size}}'` is store-dependent: against a
  # containerd-backed store it reports the *compressed* content size, which
  # measured this image at 25,399,624 bytes when its real on-disk footprint is
  # 63,667,200 — a 2.5x under-measurement that would let a ~250 MiB image pass a
  # 96 MiB budget. That is a gate failing open. Measure the uncompressed layer
  # bytes out of `docker save` instead, which is what actually lands on a node.
  image_size=$(docker save "$image" | python3 -c '
import json
import sys
import tarfile
import zlib

CHUNK = 1 << 20


def uncompressed_bytes(handle):
    head = handle.read(2)
    if head == b"\x1f\x8b":
        decompressor = zlib.decompressobj(zlib.MAX_WBITS | 16)
        total = len(decompressor.decompress(head))
        while True:
            chunk = handle.read(CHUNK)
            if not chunk:
                break
            total += len(decompressor.decompress(chunk))
        return total + len(decompressor.flush())
    total = len(head)
    while True:
        chunk = handle.read(CHUNK)
        if not chunk:
            break
        total += len(chunk)
    return total


layer_sizes = {}
manifest = None
with tarfile.open(fileobj=sys.stdin.buffer, mode="r|") as archive:
    for member in archive:
        if not member.isfile():
            continue
        handle = archive.extractfile(member)
        if handle is None:
            continue
        name = member.name.lstrip("./")
        if name == "manifest.json":
            manifest = json.loads(handle.read())
        else:
            layer_sizes[name] = uncompressed_bytes(handle)

if not manifest:
    sys.exit("docker save archive did not contain manifest.json")
layers = []
for entry in manifest:
    layers.extend(entry.get("Layers", []))
if not layers:
    sys.exit("docker save manifest.json listed no layers")

total = 0
for layer in dict.fromkeys(layers):
    key = layer.lstrip("./")
    if key not in layer_sizes:
        sys.exit("docker save archive is missing layer blob " + key)
    total += layer_sizes[key]
print(total)
')
  case "$image_size" in
    ''|*[!0-9]*)
      echo "failed to measure uncompressed layer bytes for image $image" >&2
      exit 1
      ;;
  esac
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
