#!/bin/sh
set -eu

host_arch=${1:?host architecture is required}
image_arch=${2:?image architecture is required}

normalize_arch() {
  case "$1" in
    aarch64|arm64) printf '%s\n' arm64 ;;
    x86_64|amd64) printf '%s\n' amd64 ;;
    *)
      echo "unsupported container runtime architecture: $1" >&2
      return 64
      ;;
  esac
}

normalized_host=$(normalize_arch "$host_arch")
normalized_image=$(normalize_arch "$image_arch")
if [ "$normalized_host" = "$normalized_image" ]; then
  printf '%s\n' native
else
  printf '%s\n' cross-arch
fi
