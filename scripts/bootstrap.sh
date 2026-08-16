#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

mode="${1:-core}"
case "$mode" in
  core|apple|android|container|all) ;;
  *) echo "usage: $0 [core|apple|android|container|all]" >&2; exit 64 ;;
esac

require_commands() {
  for command_name in "$@"; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
      echo "missing required command for $mode mode: $command_name" >&2
      exit 1
    fi
  done
}

case "$mode" in
  core) required="cargo rustc rustfmt" ;;
  apple) required="cargo rustc rustfmt swift xcodebuild xcodegen" ;;
  android) required="java adb" ;;
  container) required="docker" ;;
  all) required="cargo rustc rustfmt swift xcodebuild xcodegen java adb docker" ;;
esac
require_commands $required

if [ "$mode" = core ] || [ "$mode" = apple ] || [ "$mode" = all ]; then
  rustc_version=$(rustc --version)
  case "$rustc_version" in
    "rustc 1.97.1 "*) ;;
    *)
      echo "expected Rust 1.97.1, found: $rustc_version" >&2
      exit 1
      ;;
  esac
  cargo fetch --locked
fi

if [ "$mode" = apple ] || [ "$mode" = all ]; then
  if [ "$(uname -s)" != "Darwin" ] || [ "$(uname -m)" != "arm64" ]; then
    echo "Apple bootstrap requires an Apple Silicon Mac." >&2
    exit 1
  fi
  (cd apps/apple && xcodegen generate --quiet)
fi

if [ "$mode" = android ] || [ "$mode" = all ]; then
  if [ ! -x apps/android/gradlew ]; then
    echo "Android Gradle wrapper is missing or not executable" >&2
    exit 1
  fi
  java -version >/dev/null 2>&1
fi

if [ "$mode" = container ] || [ "$mode" = all ]; then
  if ! docker info >/dev/null 2>&1; then
    echo "Docker daemon is unavailable" >&2
    exit 1
  fi
fi

echo "Covalent $mode bootstrap complete. No secrets or hosted account used."
