#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"
mode="${1:-all}"

check_core() {
  ./scripts/validate-foundation.sh
  cargo fmt --all -- --check
  cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
  cargo test --locked --workspace --all-features
  cargo run --quiet -p covalent-cli -- doctor >/dev/null
  ./scripts/smoke.sh
  cargo bench --locked -p covalent-core --bench engine_smoke
  cargo build --locked --release -p covalent-node -p covalent-cli
  ./scripts/check-artifact-budgets.sh
  node --test packaging/web/tests/*.test.mjs
  node scripts/check-openapi-routes.mjs
}

check_apple() {
  if [ "$(uname -s)" != "Darwin" ] || [ "$(uname -m)" != "arm64" ]; then
    echo "Apple checks require an Apple Silicon Mac." >&2
    exit 1
  fi
  swift test --package-path apps/apple
  (cd apps/apple && xcodegen generate --quiet)
  xcodebuild -project apps/apple/Covalent.xcodeproj -scheme CovalentMac -configuration Debug -destination 'platform=macOS,arch=arm64' ARCHS=arm64 EXCLUDED_ARCHS=x86_64 CODE_SIGNING_ALLOWED=NO build
  if [ "${COVALENT_INCLUDE_IOS:-0}" = "1" ]; then
    xcodebuild -project apps/apple/Covalent.xcodeproj -scheme CovalentIOS -configuration Debug -sdk iphonesimulator CODE_SIGNING_ALLOWED=NO build
  fi
}

check_android() {
  ./scripts/check-android.sh
}

check_container() {
  if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    echo "Docker daemon unavailable; container validation cannot pass." >&2
    exit 1
  fi
  docker compose -f packaging/docker/compose.yaml config --quiet
  docker build -f packaging/docker/Dockerfile -t covalent:foundation .
  ./scripts/check-container-runtime.sh covalent:foundation
  ./scripts/check-artifact-budgets.sh covalent:foundation
}

case "$mode" in
  core) check_core ;;
  apple) check_apple ;;
  android) check_android ;;
  container) check_container ;;
  all)
    check_core
    check_apple
    check_android
    check_container
    ;;
  *) echo "usage: $0 [core|apple|android|container|all]" >&2; exit 64 ;;
esac

echo "$mode checks passed"
