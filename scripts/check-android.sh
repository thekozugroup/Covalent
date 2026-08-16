#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
android_java=""
android_sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"

if [ "$(uname -s)" = "Darwin" ]; then
  android_java=$(/usr/libexec/java_home -v 21 2>/dev/null || true)
  if [ -z "$android_java" ]; then
    android_java=$(/usr/libexec/java_home -v 17 2>/dev/null || true)
  fi
fi

if [ -z "$android_sdk" ] && [ -d "${HOME}/Library/Android/sdk" ]; then
  android_sdk="${HOME}/Library/Android/sdk"
fi

if [ -z "$android_sdk" ]; then
  echo "Android SDK not found; set ANDROID_HOME or ANDROID_SDK_ROOT." >&2
  exit 1
fi

if [ -n "$android_java" ]; then
  env JAVA_HOME="$android_java" ANDROID_HOME="$android_sdk" ANDROID_SDK_ROOT="$android_sdk" "$repo_root/apps/android/gradlew" -p "$repo_root/apps/android" --no-daemon test lint assembleDebug
else
  env ANDROID_HOME="$android_sdk" ANDROID_SDK_ROOT="$android_sdk" "$repo_root/apps/android/gradlew" -p "$repo_root/apps/android" --no-daemon test lint assembleDebug
fi
