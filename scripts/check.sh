#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

./scripts/validate-foundation.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo run --quiet -p covalent-cli -- doctor >/dev/null

swift test --package-path apps/apple
(cd apps/apple && xcodegen generate --quiet)
xcodebuild -project apps/apple/Covalent.xcodeproj -scheme CovalentMac -configuration Debug -destination 'platform=macOS' CODE_SIGNING_ALLOWED=NO build

./scripts/check-android.sh

if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  docker compose -f packaging/docker/compose.yaml config --quiet
  docker build -f packaging/docker/Dockerfile -t covalent:foundation .
  ./scripts/check-container-runtime.sh covalent:foundation
else
  echo "Docker daemon unavailable; Tier 1 container validation cannot pass." >&2
  exit 1
fi

if [ "${COVALENT_INCLUDE_IOS:-0}" = "1" ]; then
  xcodebuild -project apps/apple/Covalent.xcodeproj -scheme CovalentIOS -configuration Debug -sdk iphonesimulator CODE_SIGNING_ALLOWED=NO build
fi

echo "all requested foundation checks passed"
