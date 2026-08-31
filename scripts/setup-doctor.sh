#!/bin/sh
# Read-only prerequisite check for end-user and personal-use setup paths.
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
mode=${1:-}

case "$mode" in
  docker|macos|android|all) ;;
  *)
    echo "usage: $0 docker|macos|android|all" >&2
    exit 64
    ;;
esac

problems=0

ok() {
  printf '  ok: %s\n' "$1"
}

note() {
  printf '  note: %s\n' "$1"
}

missing() {
  printf '  missing: %s\n' "$1" >&2
  printf '           %s\n' "$2" >&2
  problems=$((problems + 1))
}

need_command() {
  command_name=$1
  install_hint=$2
  if command -v "$command_name" >/dev/null 2>&1; then
    ok "$command_name"
  else
    missing "$command_name" "$install_hint"
  fi
}

need_sha256() {
  if command -v shasum >/dev/null 2>&1 || command -v sha256sum >/dev/null 2>&1; then
    ok "SHA-256 tool"
  else
    missing "SHA-256 tool" "Install shasum or sha256sum from your operating-system package manager."
  fi
}

check_rust() {
  need_command rustc "Install rustup, then use the repository rust-toolchain.toml."
  need_command cargo "Install rustup, then reopen the shell."
  need_command rustup "Install rustup from https://rustup.rs without piping an unreviewed script into a privileged shell."
  if command -v rustc >/dev/null 2>&1; then
    rust_version=$(rustc --version | awk '{ print $2 }')
    if [ "$rust_version" = "1.97.1" ]; then
      ok "Rust 1.97.1"
    else
      missing "Rust 1.97.1 (found $rust_version)" "Run: rustup toolchain install 1.97.1"
    fi
  fi
}

check_docker() {
  echo "Docker server prerequisites"
  need_command docker "Install Docker Engine or Docker Desktop from the official Docker distribution."
  if command -v docker >/dev/null 2>&1; then
    if docker info >/dev/null 2>&1; then
      ok "Docker daemon is running"
    else
      missing "Docker daemon" "Start Docker, then rerun this doctor."
    fi
    if docker compose version >/dev/null 2>&1; then
      ok "Docker Compose plugin"
    else
      missing "Docker Compose plugin" "Install the Docker Compose v2 plugin."
    fi
    if docker buildx version >/dev/null 2>&1; then
      ok "Docker Buildx plugin"
    else
      missing "Docker Buildx plugin" "Install the Docker Buildx plugin."
    fi
  fi
  need_command git "Install Git from your operating-system package manager."
  need_command curl "Install curl; the source-built claim client uses it for bounded HTTPS requests."
  need_command cc "Install a C compiler/build-essential package for the Rust claim client."
  check_rust
  need_sha256
}

check_macos() {
  echo "Apple Silicon macOS prerequisites"
  if [ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = arm64 ]; then
    ok "Apple Silicon Mac"
  else
    missing "Apple Silicon Mac" "The macOS app supports arm64 only."
  fi
  check_rust
  need_command xcodebuild "Install Xcode 26, open it once, and accept its license."
  need_command swift "Install Xcode 26 with Swift 6.3."
  need_command codesign "Use the codesign tool included with macOS."
  need_command lipo "Install the Xcode command-line tools."
  need_command curl "Install curl. It is included with macOS."
  need_command unzip "Install unzip. It is included with macOS."
  need_sha256
  if command -v xcodebuild >/dev/null 2>&1; then
    xcode_version=$(xcodebuild -version | sed -n '1s/^Xcode //p')
    case "$xcode_version" in
      26|26.*) ok "Xcode $xcode_version" ;;
      *) missing "Xcode 26 (found ${xcode_version:-unknown})" "Select Xcode 26 with xcode-select." ;;
    esac
  fi
  if command -v xcodegen >/dev/null 2>&1; then
    xcodegen_version=$(xcodegen --version 2>/dev/null | sed 's/^Version: //')
    if [ "$xcodegen_version" = "2.46.0" ]; then
      ok "XcodeGen 2.46.0"
    else
      note "XcodeGen ${xcodegen_version:-unknown} is ignored; the personal builder installs pinned 2.46.0 privately."
    fi
  else
    note "The personal builder installs pinned XcodeGen 2.46.0 privately."
  fi
}

android_sdk_path() {
  if [ -n "${ANDROID_SDK_ROOT:-}" ]; then
    printf '%s\n' "$ANDROID_SDK_ROOT"
  elif [ -n "${ANDROID_HOME:-}" ]; then
    printf '%s\n' "$ANDROID_HOME"
  elif [ -d "${HOME}/Library/Android/sdk" ]; then
    printf '%s\n' "${HOME}/Library/Android/sdk"
  else
    printf '%s\n' ""
  fi
}

check_android() {
  echo "Android personal APK prerequisites"
  check_rust
  need_command java "Install JDK 17 through 25. CI uses JDK 17."
  if command -v java >/dev/null 2>&1; then
    java_version=$(java -version 2>&1 | sed -n '1s/.*version "\([0-9][0-9]*\).*/\1/p')
    case "$java_version" in
      17|18|19|20|21|22|23|24|25) ok "JDK $java_version" ;;
      *) missing "JDK 17 through 25 (found ${java_version:-unknown})" "Select a supported JDK with JAVA_HOME, then reopen the shell." ;;
    esac
  fi
  need_command adb "Install Android SDK Platform Tools and add them to PATH."
  need_command git "Install Git; the guarded builder verifies that install artifacts stay ignored."
  need_command jq "Install jq; the native Android build contract parses bounded metadata with it."
  need_command tar "Install tar; the native Android build contract inspects packaged inputs with it."
  need_command unzip "Install unzip; the guarded builder verifies both packaged JNI libraries."
  need_sha256
  sdk=$(android_sdk_path)
  if [ -z "$sdk" ]; then
    missing "Android SDK" "Set ANDROID_SDK_ROOT or ANDROID_HOME to the installed SDK."
  elif [ ! -d "$sdk" ]; then
    missing "Android SDK directory $sdk" "Correct ANDROID_SDK_ROOT or ANDROID_HOME."
  else
    ok "Android SDK at $sdk"
    for required_directory in \
      "platforms/android-37.0" \
      "build-tools/37.0.0" \
      "platform-tools" \
      "ndk/27.1.12297006"
    do
      if [ -d "$sdk/$required_directory" ]; then
        ok "$required_directory"
      else
        missing "$required_directory" "Install it with sdkmanager; see docs/platform/android.md."
      fi
    done
  fi
  if command -v cargo >/dev/null 2>&1 && cargo ndk --version >/dev/null 2>&1; then
    cargo_ndk_version=$(cargo ndk --version 2>/dev/null | awk '{ print $2 }')
    if [ "$cargo_ndk_version" = "4.1.2" ]; then
      ok "cargo-ndk 4.1.2"
    else
      missing "cargo-ndk 4.1.2 (found ${cargo_ndk_version:-unknown})" "Run: cargo install --locked cargo-ndk --version 4.1.2"
    fi
  else
    missing "cargo-ndk 4.1.2" "Run: cargo install --locked cargo-ndk --version 4.1.2"
  fi
  if command -v rustup >/dev/null 2>&1; then
    installed_targets=$(rustup target list --installed)
    for target in aarch64-linux-android x86_64-linux-android; do
      if printf '%s\n' "$installed_targets" | grep -Fx "$target" >/dev/null; then
        ok "Rust target $target"
      else
        missing "Rust target $target" "Run: rustup target add $target"
      fi
    done
  fi
  if [ -x "$repo_root/apps/android/gradlew" ]; then
    ok "Android Gradle wrapper"
  else
    missing "Android Gradle wrapper" "Use a complete Covalent source checkout."
  fi
}

case "$mode" in
  docker) check_docker ;;
  macos) check_macos ;;
  android) check_android ;;
  all)
    check_docker
    check_macos
    check_android
    ;;
esac

if [ "$problems" -ne 0 ]; then
  printf '\nSetup doctor found %s problem(s). Fix them, then rerun this command.\n' "$problems" >&2
  exit 1
fi

printf '\nSetup prerequisites are ready for %s.\n' "$mode"
printf 'Next: docs/getting-started.md\n'
